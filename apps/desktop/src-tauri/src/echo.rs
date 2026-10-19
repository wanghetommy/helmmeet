//! Causal, bounded disk reference reader and dual-stream AEC processor. No capture I/O.
use crate::{
    audio,
    db::{err, safe_relative, Runtime},
    processing::Processor,
};
use meeting_core::{
    audio_processing::FRAME,
    echo::{Gate, State},
};
use rusqlite::{params, OptionalExtension};
use std::collections::VecDeque;
pub const ENGINE: &str = "webrtc-apm-2.1/aec3-16k-v1";
#[derive(Clone)]
pub struct Timing {
    pub start: u64,
    pub epoch: String,
    pub origin: u64,
}
pub fn timing(rt: &Runtime, cid: &str) -> Result<Option<Timing>, String> {
    rt.db()?
        .query_row(
            "SELECT start_sample,epoch_id,host_origin_ns FROM audio_chunk_timing WHERE chunk_id=?",
            [cid],
            |r| {
                Ok(Timing {
                    start: r.get(0)?,
                    epoch: r.get(1)?,
                    origin: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(err)
}
struct Route {
    id: String,
    start: u64,
    end: u64,
    supported: bool,
}
struct Window {
    start: u64,
    values: Vec<Option<f32>>,
    routes: Vec<Route>,
    epoch: String,
    origin: u64,
    problem: Option<&'static str>,
}
#[derive(Clone, Debug)]
pub struct Interval {
    pub start: u64,
    pub end: u64,
    pub state: &'static str,
    pub reason: &'static str,
    pub delay: Option<i32>,
}
pub struct Echo {
    engine: Processor,
    gate: Gate,
    delay: usize,
    fallback_delay: VecDeque<f32>,
    cursor: u64,
    begin: u64,
    end: u64,
    window: Option<Window>,
    last_valid: bool,
    blend: f32,
    last: f32,
    estimate: Option<i32>,
    frames: usize,
    draining: bool,
    intervals: VecDeque<Interval>,
    last_status: (State, &'static str),
}
impl Echo {
    pub fn new(noise: i32, start: u64) -> Result<Self, String> {
        Ok(Self {
            engine: Processor::with_echo(noise, true)?,
            gate: Gate::default(),
            delay: 128 + if noise == 0 { 0 } else { 96 },
            fallback_delay: VecDeque::from(vec![0.0; 128]),
            cursor: start,
            begin: start,
            end: start,
            window: None,
            last_valid: false,
            blend: 0.0,
            last: 0.0,
            estimate: None,
            frames: 0,
            draining: false,
            intervals: Default::default(),
            last_status: (State::Bypass, "waiting_for_audio"),
        })
    }
    /// Wait for sealed reference AND an output-route observation through the end.
    /// Raw sealing may precede the next 250ms route poll. Reading that transient
    /// watermark as a permanent hole would reset AEC at every chunk boundary.
    /// A later observation only confirms a stable route; real switch gaps remain
    /// unknown. Never use future-timestamp audio as a reference.
    pub fn waiting(rt: &Runtime, meeting: &str, end: u64, state: &str) -> Result<bool, String> {
        if state != "recording" {
            return Ok(false);
        }
        let db = rt.db()?;
        let (active,duration):(bool,u64)=db.query_row("SELECT EXISTS(SELECT 1 FROM recording_sources WHERE meeting_id=m.id AND source='system' AND state='active'),recording_duration_ms FROM meetings m WHERE id=?",[meeting],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
        let sealed:u64=db.query_row("SELECT coalesce(max(t.start_sample+c.sample_count),0) FROM audio_chunks c JOIN audio_chunk_timing t ON t.chunk_id=c.id WHERE c.meeting_id=? AND c.source='system' AND c.status='sealed'",[meeting],|r|r.get(0)).map_err(err)?;
        let observed: u64 = db
            .query_row(
                "SELECT coalesce(max(end_sample),0) FROM audio_output_routes WHERE meeting_id=?",
                [meeting],
                |r| r.get(0),
            )
            .map_err(err)?;
        Ok(active && (sealed < end || observed < end) && duration * 16 < end + 128000)
    }
    pub fn load(
        &mut self,
        rt: &Runtime,
        meeting: &str,
        start: u64,
        count: usize,
        stamp: Option<&Timing>,
    ) -> Result<(), String> {
        if count > 160000 {
            return Err("AEC输入片段过大".into());
        }
        self.draining = false;
        self.end = start + count as u64;
        let Some(timing) = stamp else {
            self.window = None;
            return Ok(());
        };
        let from = start.saturating_sub(320);
        let to = self.end;
        let (sources, routes) = {
            let db = rt.db()?;
            let mut q=db.prepare("SELECT c.relative_path,t.start_sample,c.sample_count FROM audio_chunks c JOIN audio_chunk_timing t ON t.chunk_id=c.id WHERE c.meeting_id=? AND c.source='system' AND c.status='sealed' AND t.epoch_id=? AND t.host_origin_ns=? AND t.start_sample<? AND t.start_sample+c.sample_count>? ORDER BY t.start_sample").map_err(err)?;
            let sources = q
                .query_map(
                    params![meeting, timing.epoch, timing.origin, to, from],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, u64>(1)?,
                            r.get::<_, usize>(2)?,
                        ))
                    },
                )
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            let mut q=db.prepare("SELECT id,start_sample,end_sample,supported FROM audio_output_routes WHERE meeting_id=? AND epoch_id=? AND start_sample<? AND end_sample>? ORDER BY start_sample").map_err(err)?;
            let routes = q
                .query_map(params![meeting, timing.epoch, to, from], |r| {
                    Ok(Route {
                        id: r.get(0)?,
                        start: r.get(1)?,
                        end: r.get(2)?,
                        supported: r.get(3)?,
                    })
                })
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            (sources, routes)
        };
        let mut values = vec![None; (to - from) as usize];
        let mut problem = None;
        for (path, at, n) in sources {
            let path = safe_relative(&rt.root, &path)?;
            if n > 160000 || !std::fs::metadata(&path).is_ok_and(|m| m.len() <= 324096) {
                problem = Some("reference_unreadable");
                continue;
            }
            let Ok(pcm) = audio::read_wav(&path) else {
                problem = Some("reference_unreadable");
                continue;
            };
            if pcm.len() != n {
                problem = Some("reference_unreadable");
                continue;
            }
            for sample in from.max(at)..to.min(at + n as u64) {
                values[(sample - from) as usize] = Some(pcm[(sample - at) as usize]);
            }
        }
        self.window = Some(Window {
            start: from,
            values,
            routes,
            epoch: timing.epoch.clone(),
            origin: timing.origin,
            problem,
        });
        Ok(())
    }
    pub fn discontinuous(&self, stamp: Option<&Timing>) -> bool {
        match (&self.window, stamp) {
            (Some(w), Some(t)) => w.epoch != t.epoch || w.origin != t.origin,
            (None, None) => false,
            _ => true,
        }
    }
    pub fn drain(&mut self) {
        self.draining = true;
    }
    pub fn intervals(&mut self, start: u64, end: u64) -> Vec<Interval> {
        let result = self
            .intervals
            .iter()
            .filter_map(|v| {
                let a = v.start.max(start);
                let b = v.end.min(end);
                (b > a).then(|| Interval {
                    start: a,
                    end: b,
                    ..v.clone()
                })
            })
            .collect();
        while self.intervals.front().is_some_and(|v| v.end <= end) {
            self.intervals.pop_front();
        }
        result
    }
    pub fn process(
        &mut self,
        input: &[f32; FRAME],
        fallback_processor: &mut Processor,
    ) -> Result<[f32; FRAME], String> {
        let fallback = fallback_processor.process(input)?;
        let fallback: [f32; FRAME] = std::array::from_fn(|i| {
            self.fallback_delay.push_back(fallback[i]);
            self.fallback_delay.pop_front().unwrap()
        });
        let (mut render, mut route, mut reason) = ([0.0; FRAME], None, "timing_unavailable");
        let private_drain = self.draining && self.cursor >= self.end;
        if let Some(w) = &self.window {
            let real_end = (self.cursor + FRAME as u64).min(self.end);
            let candidate = w
                .routes
                .iter()
                .find(|r| r.start <= self.cursor && r.end >= real_end);
            reason = match candidate {
                Some(r) if r.supported => w.problem.unwrap_or("reference_missing"),
                Some(_) => "route_unsupported",
                None => "route_unobserved",
            };
            if let Some(r) = candidate.filter(|r| r.supported) {
                let valid = (self.cursor..real_end).all(|at| {
                    at >= w.start
                        && w.values
                            .get((at - w.start) as usize)
                            .is_some_and(Option::is_some)
                });
                if valid && real_end > self.cursor {
                    for at in self.cursor..real_end {
                        render[(at - self.cursor) as usize] =
                            w.values[(at - w.start) as usize].unwrap();
                    }
                    route = Some(r.id.as_str());
                    reason = "reference_valid";
                }
            }
        }
        let energy = render.iter().map(|x| x * x).sum::<f32>() / FRAME as f32;
        let decision = if private_drain {
            None
        } else {
            Some(
                self.gate
                    .observe(route, route.is_some(), energy, self.estimate),
            )
        };
        let reset = decision.as_ref().is_some_and(|d| d.reset);
        if reset {
            self.engine.reset()?;
            self.estimate = None;
            self.frames = 0;
        }
        let valid = if private_drain {
            self.last_valid
        } else {
            route.is_some()
        };
        let aec = if valid {
            self.engine.reverse(&render)?;
            let out = self.engine.process(input)?;
            self.frames += 1;
            if self.frames.is_multiple_of(100) {
                self.estimate = self.engine.delay_ms();
            }
            out
        } else {
            fallback
        };
        let use_echo = decision
            .as_ref()
            .map_or(self.blend > 0.5, |d| d.use_echo && !reset);
        let mut state = decision.as_ref().map_or(
            if use_echo {
                State::Applied
            } else {
                State::Bypass
            },
            |d| d.state,
        );
        if state == State::Warming {
            reason = if self.estimate.is_some_and(|d| d > 500) {
                "delay_out_of_range"
            } else {
                "calibrating"
            };
        }
        if state == State::Silent {
            if use_echo {
                state = State::Applied;
                reason = "render_tail";
            } else {
                reason = "no_playback_signal";
            }
        }
        if private_drain {
            state = self.last_status.0;
            reason = self.last_status.1;
        } else {
            self.last_status = (state, reason);
        }
        let target = if use_echo { 1.0 } else { 0.0 };
        let previous = self.blend;
        let transition = !valid && self.last_valid;
        let last = self.last;
        let out = std::array::from_fn(|i| {
            let t = (i + 1) as f32 / FRAME as f32;
            let mix = previous + (target - previous) * t;
            let value = fallback[i] * (1.0 - mix) + aec[i] * mix;
            if transition && i < 80 {
                let a = (i + 1) as f32 / 80.0;
                last * (1.0 - a) + value * a
            } else {
                value
            }
        });
        self.blend = target;
        self.last = out[FRAME - 1];
        self.last_valid = valid;
        let start = self
            .cursor
            .saturating_sub(self.delay as u64)
            .max(self.begin);
        let end = (self.cursor + FRAME as u64)
            .saturating_sub(self.delay as u64)
            .min(self.end);
        if end > start {
            let interval = Interval {
                start,
                end,
                state: state.as_str(),
                reason,
                delay: self.estimate,
            };
            if let Some(last) = self.intervals.back_mut().filter(|v| {
                v.end == start
                    && v.state == interval.state
                    && v.reason == reason
                    && v.delay == interval.delay
            }) {
                last.end = end;
            } else {
                self.intervals.push_back(interval);
            }
        }
        self.cursor += FRAME as u64;
        Ok(out)
    }
}
