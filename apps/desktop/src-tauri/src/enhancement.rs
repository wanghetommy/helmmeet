//! Disk-driven microphone NS. Raw capture and ASR never call this processor.
use crate::{
    audio,
    db::{err, safe_relative, Runtime},
    lifecycle::Worker,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
#[cfg(feature = "audio-processing")]
use {crate::db::id, rusqlite::OptionalExtension};
#[cfg(feature = "audio-processing")]
pub const ENGINE: &str = "webrtc-apm-2.1/ns16k-delay96-v1";
pub const AVAILABLE: bool = cfg!(feature = "audio-processing");
fn valid_mode(mode: &str) -> Result<(), String> {
    if !["off", "low", "moderate"].contains(&mode) {
        return Err("降噪选项无效".into());
    }
    if mode != "off" && !AVAILABLE {
        return Err("此构建未包含语音增强组件".into());
    }
    Ok(())
}
pub fn default_mode(rt: &Runtime) -> Result<String, String> {
    rt.db()?
        .query_row(
            "SELECT value FROM app_preferences WHERE key='audio_enhancement_default'",
            [],
            |r| r.get(0),
        )
        .map_err(err)
}
pub fn set_default(rt: &Runtime, mode: &str) -> Result<Value, String> {
    valid_mode(mode)?;
    rt.db()?
        .execute(
            "UPDATE app_preferences SET value=? WHERE key='audio_enhancement_default'",
            [mode],
        )
        .map_err(err)?;
    Ok(json!({"saved":true}))
}
#[cfg(all(test, feature = "audio-processing"))]
pub fn configure(rt: &Runtime, meeting: &str, mode: &str) -> Result<Value, String> {
    configure_options(rt, meeting, mode, None)
}
pub fn validate_capture(rt: &Runtime, meeting: &str, mask: i32, state: &str) -> Result<(), String> {
    let echo: bool = rt
        .db()?
        .query_row(
            "SELECT echo_cancellation FROM audio_enhancement WHERE meeting_id=?",
            [meeting],
            |r| r.get(0),
        )
        .map_err(err)?;
    if echo && state == "ready" && mask != 3 {
        return Err("外放回声消除需要明确选择麦克风＋电脑声音；不会自动开启额外音源".into());
    }
    Ok(())
}
pub fn configure_options(
    rt: &Runtime,
    meeting: &str,
    mode: &str,
    echo: Option<bool>,
) -> Result<Value, String> {
    valid_mode(mode)?;
    rt.check_id(meeting)?;
    // Same lock order as audio::start: serialize the freeze against actual capture start.
    let _capture = rt.capture.lock().map_err(|_| "采集锁异常")?;
    let db = rt.db()?;
    let current: bool = db
        .query_row(
            "SELECT echo_cancellation FROM audio_enhancement WHERE meeting_id=?",
            [meeting],
            |r| r.get(0),
        )
        .map_err(err)?;
    let echo = echo.unwrap_or(current);
    if echo && !AVAILABLE {
        return Err("此构建未包含回声消除组件".into());
    }
    let engine = if echo {
        "webrtc-apm-2.1/aec3-16k-v1"
    } else {
        "webrtc-apm-2.1/ns16k-delay96-v1"
    };
    let changed=db.execute("UPDATE audio_enhancement SET mode=?1,echo_cancellation=?3,engine=?4,state='queued',error=NULL WHERE meeting_id=?2 AND EXISTS(SELECT 1 FROM meetings WHERE id=?2 AND state='ready' AND wall_start IS NULL)",params![mode,meeting,echo,engine]).map_err(err)?;
    if changed != 1 {
        return Err("开始录制后不能更改降噪强度；可随时切换原声回听".into());
    }
    Ok(json!({"saved":true}))
}
pub fn retry(rt: &Runtime, meeting: &str) -> Result<Value, String> {
    rt.check_id(meeting)?;
    if !AVAILABLE {
        return Err("此构建未包含语音增强组件".into());
    }
    let changed=rt.db()?.execute("UPDATE audio_enhancement SET generation=generation+1,state='queued',error=NULL WHERE meeting_id=? AND (mode!='off' OR echo_cancellation=1) AND state='failed'",[meeting]).map_err(err)?;
    if changed != 1 {
        return Err("当前没有可重试的增强任务".into());
    }
    Ok(json!({"queued":true}))
}
pub fn detail(rt: &Runtime, meeting: &str) -> Result<Value, String> {
    let db = rt.db()?;
    let mut value=db.query_row("SELECT mode,engine,state,error,generation,echo_cancellation FROM audio_enhancement WHERE meeting_id=?",[meeting],|r|Ok(json!({"available":AVAILABLE,"mode":r.get::<_,String>(0)?,"engine":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?,"generation":r.get::<_,u64>(4)?,"echo_cancellation":r.get::<_,bool>(5)?,"algorithm_delay_samples":if r.get::<_,bool>(5)? {128+if r.get::<_,String>(0)?=="off"{0}else{96}}else{96},"asr_source":"raw"}))).map_err(err)?;
    let (total,done,first_pending):(u64,u64,Option<u64>)=db.query_row("SELECT coalesce(sum(c.sample_count),0),coalesce(sum(CASE WHEN e.chunk_id IS NOT NULL THEN c.sample_count ELSE 0 END),0),min(CASE WHEN e.chunk_id IS NULL THEN c.start_ms END) FROM audio_chunks c LEFT JOIN enhanced_chunks e ON e.chunk_id=c.id WHERE c.meeting_id=? AND c.source='microphone'",[meeting],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(err)?;
    let (applied,bypass):(u64,u64)=db.query_row("SELECT coalesce(sum(CASE WHEN a.state='applied' THEN a.end_sample-a.start_sample ELSE 0 END),0),coalesce(sum(CASE WHEN a.state!='applied' THEN a.end_sample-a.start_sample ELSE 0 END),0) FROM aec_intervals a JOIN enhanced_chunks e ON e.chunk_id=a.chunk_id WHERE e.meeting_id=?",[meeting],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
    value["aec_applied_ms"] = json!(applied / 16);
    value["aec_bypass_ms"] = json!(bypass / 16);
    value["aec_status"]=db.query_row("SELECT a.state,a.reason,a.delay_ms FROM aec_intervals a JOIN enhanced_chunks e ON e.chunk_id=a.chunk_id WHERE e.meeting_id=? ORDER BY a.end_sample DESC LIMIT 1",[meeting],|r|Ok(json!({"state":r.get::<_,String>(0)?,"reason":r.get::<_,String>(1)?,"delay_ms":r.get::<_,Option<i32>>(2)?}))).unwrap_or(json!({"state":"waiting","reason":"waiting_for_audio","delay_ms":null}));
    value["total_ms"] = json!(total / 16);
    value["processed_ms"] = json!(done / 16);
    value["playable_through_ms"] = json!(first_pending.unwrap_or(
        db.query_row(
            "SELECT recording_duration_ms FROM meetings WHERE id=?",
            [meeting],
            |r| r.get::<_, u64>(0)
        )
        .map_err(err)?
    ));
    Ok(value)
}
/// Called after raw recovery. Missing derived files are regenerated; originals are untouched.
pub fn recover(rt: &Runtime) -> Result<(), String> {
    let rows = {
        let db = rt.db()?;
        let mut q = db
            .prepare("SELECT chunk_id,meeting_id,relative_path FROM enhanced_chunks")
            .map_err(err)?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for (chunk, meeting, path) in rows {
        if !safe_relative(&rt.root, &path)?.is_file() {
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            tx.execute("DELETE FROM enhanced_chunks WHERE chunk_id=?", [chunk])
                .map_err(err)?;
            tx.execute("UPDATE audio_enhancement SET state='queued',generation=generation+1,error=NULL WHERE meeting_id=?",[meeting]).map_err(err)?;
            tx.commit().map_err(err)?;
        }
    }
    // Only our private transient directory; never remove raw or registered enhanced files.
    let scratch = rt.root.join(".enhancement-tmp");
    if scratch.exists() {
        std::fs::remove_dir_all(scratch).map_err(err)?;
    }
    Ok(())
}
/// No substitution of raw mic samples under an enhanced label. System audio remains raw.
pub fn mix(
    rt: &Runtime,
    meeting: &str,
    start: u64,
    end: u64,
    microphone_only: bool,
) -> Result<Vec<f32>, String> {
    if end <= start || end - start > 30000 {
        return Err("回听区间无效".into());
    }
    rt.check_id(meeting)?;
    let rows = {
        let db = rt.db()?;
        let mode: String = db
            .query_row(
                "SELECT CASE WHEN echo_cancellation=1 THEN 'echo' ELSE mode END FROM audio_enhancement WHERE meeting_id=?",
                [meeting],
                |r| r.get(0),
            )
            .map_err(err)?;
        if mode == "off" {
            return Err("本场未开启降噪，请选择原声".into());
        }
        let count: u64 = db
            .query_row(
                "SELECT count(*) FROM audio_chunks WHERE meeting_id=? AND source='microphone'",
                [meeting],
                |r| r.get(0),
            )
            .map_err(err)?;
        if count == 0 {
            return Err("本场没有可增强的麦克风音轨".into());
        }
        let pending:u64=db.query_row("SELECT count(*) FROM audio_chunks c LEFT JOIN enhanced_chunks e ON e.chunk_id=c.id WHERE c.meeting_id=? AND c.source='microphone' AND c.start_ms<? AND coalesce(c.end_ms,9223372036854775807)>? AND (c.status!='sealed' OR e.chunk_id IS NULL)",params![meeting,end,start],|r|r.get(0)).map_err(err)?;
        if pending > 0 {
            return Err("此区间增强尚未完成；可先回听原声".into());
        }
        let mut q=db.prepare("SELECT e.relative_path,coalesce(t.start_sample,c.start_ms*16),CASE WHEN t.chunk_id IS NULL THEN (c.end_ms-c.start_ms)*16 ELSE e.sample_count END FROM enhanced_chunks e JOIN audio_chunks c ON c.id=e.chunk_id LEFT JOIN audio_chunk_timing t ON t.chunk_id=c.id WHERE c.meeting_id=? AND coalesce(t.start_sample,c.start_ms*16)<?*16 AND coalesce(t.start_sample,c.start_ms*16)+CASE WHEN t.chunk_id IS NULL THEN (c.end_ms-c.start_ms)*16 ELSE e.sample_count END>?*16 ORDER BY c.start_ms").map_err(err)?;
        let rows = q
            .query_map(params![meeting, end, start], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    let tracks:usize=rt.db()?.query_row("SELECT count(*) FROM (SELECT source FROM audio_chunks WHERE meeting_id=?1 UNION SELECT source FROM recording_sources WHERE meeting_id=?1)",[meeting],|r|r.get(0)).map_err(err)?;
    let gain = if microphone_only {
        1.0
    } else {
        meeting_core::audio_clock::mix_gain(tracks)
    };
    let mut out = if microphone_only {
        vec![0.0; ((end - start) * 16) as usize]
    } else {
        crate::asr::mix_source(rt, meeting, start, end, Some("system"))?
            .into_iter()
            .map(|s| s * gain)
            .collect()
    };
    for (path, from, count) in rows {
        let pcm = audio::read_wav(&safe_relative(&rt.root, &path)?)?;
        for t in (start * 16).max(from)..(end * 16).min(from + count) {
            if let Some(v) = pcm.get((t - from) as usize) {
                out[(t - start * 16) as usize] += v * gain;
            }
        }
    }
    Ok(out)
}
/// Export only a complete, stopped snapshot. Raw gaps remain gaps/silence in the mix.
pub fn require_export(rt: &Runtime, meeting: &str) -> Result<(), String> {
    let db = rt.db()?;
    let valid:bool=db.query_row("SELECT (e.mode!='off' OR e.echo_cancellation=1) AND e.state='complete' AND m.state NOT IN ('ready','recording','paused','interrupted','deleting') AND EXISTS(SELECT 1 FROM enhanced_chunks WHERE meeting_id=m.id) FROM audio_enhancement e JOIN meetings m ON m.id=e.meeting_id WHERE m.id=?",[meeting],|r|r.get(0)).map_err(err)?;
    if valid {
        Ok(())
    } else {
        Err("请结束录制并等待整场增强完成后导出；原声导出仍可使用".into())
    }
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    Worker::spawn(move |stop| {
        #[cfg(feature = "audio-processing")]
        {
            let mut session = None;
            while !stop.is_stopped() {
                if let Err((mid, generation, message)) = step(&rt, &mut session) {
                    if !stop.is_stopped()
                        && !rt.shutting_down.load(std::sync::atomic::Ordering::Acquire)
                    {
                        if let Ok(db) = rt.db() {
                            let _=db.execute("UPDATE audio_enhancement SET state='failed',error=? WHERE meeting_id=? AND generation=?",params![message,mid,generation]);
                        }
                    }
                    session = None;
                }
                if !stop.wait(Duration::from_millis(100)) {
                    break;
                }
            }
        }
        #[cfg(not(feature = "audio-processing"))]
        {
            let _ = rt;
            while stop.wait(Duration::from_secs(60)) {}
        }
    })
}
#[cfg(feature = "audio-processing")]
#[derive(Clone)]
struct Chunk {
    id: String,
    path: String,
    start: u64,
    end: u64,
    samples: usize,
    epoch: Option<String>,
    exists: bool,
    timing: Option<crate::echo::Timing>,
}
#[cfg(feature = "audio-processing")]
struct Pending {
    chunk: Chunk,
    output: Vec<f32>,
}
#[cfg(feature = "audio-processing")]
struct Session {
    meeting: String,
    generation: u64,
    processor: crate::processing::Processor,
    echo_enabled: bool,
    echo: Option<crate::echo::Echo>,
    noise: i32,
    frames: meeting_core::audio_processing::Frames,
    last: Option<Chunk>,
    pending: std::collections::VecDeque<Pending>,
    boundary: bool,
}
#[cfg(feature = "audio-processing")]
impl Session {
    fn distribute(&mut self, rt: &Runtime, output: &[f32]) -> Result<(), String> {
        let mut at = 0;
        while at < output.len() {
            let p = self.pending.front_mut().ok_or("增强分帧账目不匹配")?;
            let n = (p.chunk.samples - p.output.len()).min(output.len() - at);
            p.output.extend_from_slice(&output[at..at + n]);
            at += n;
            if p.output.len() == p.chunk.samples {
                let p = self.pending.pop_front().unwrap();
                if !p.chunk.exists {
                    self.publish(rt, p)?;
                } else if let Some(echo) = self.echo.as_mut() {
                    let start = p
                        .chunk
                        .timing
                        .as_ref()
                        .map_or(p.chunk.start * 16, |t| t.start);
                    let _ = echo.intervals(start, start + p.chunk.samples as u64);
                }
            }
        }
        Ok(())
    }
    fn flush(&mut self, rt: &Runtime) -> Result<(), String> {
        if let Some(echo) = self.echo.as_mut() {
            echo.drain();
        }
        let output = self.frames.finish(|frame| match self.echo.as_mut() {
            Some(echo) => echo.process(frame, &mut self.processor),
            None => self.processor.process(frame),
        })?;
        self.distribute(rt, &output)?;
        if !self.pending.is_empty() {
            return Err("增强尾段长度不匹配".into());
        }
        self.processor.reset()?;
        self.frames = if self.echo_enabled {
            meeting_core::audio_processing::Frames::with_delay(
                128 + if self.noise == 0 { 0 } else { 96 },
            )
        } else {
            Default::default()
        };
        self.echo = None;
        self.boundary = true;
        Ok(())
    }
    fn publish(&mut self, rt: &Runtime, p: Pending) -> Result<(), String> {
        if audio::available_bytes(&rt.root)? < 200_000_000 {
            return Err("增强已暂停：磁盘不足200MB，请释放空间后重试；原轨保留".into());
        }
        let scratch = rt.root.join(".enhancement-tmp");
        std::fs::create_dir_all(&scratch).map_err(err)?;
        let temp = scratch.join(format!("{}.wav", id()));
        let result = (|| {
            let mut wav = audio::WavFile::create(&temp)?;
            wav.write(&p.output)?;
            wav.sync()?;
            drop(wav);
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM audio_enhancement e JOIN meetings m ON m.id=e.meeting_id WHERE e.meeting_id=? AND e.generation=? AND e.state!='failed' AND m.state!='deleting')",params![self.meeting,self.generation],|r|r.get(0)).map_err(err)?;
            if !valid || rt.shutting_down.load(std::sync::atomic::Ordering::Acquire) {
                return Err("增强任务已取消".into());
            }
            let variant = if self.echo_enabled { "aec-v1" } else { "ns-v1" };
            let engine = if self.echo_enabled {
                crate::echo::ENGINE
            } else {
                ENGINE
            };
            let relative = format!(
                "meetings/{}/enhanced/{variant}/{}.wav",
                self.meeting, p.chunk.id
            );
            let path = safe_relative(&rt.root, &relative)?;
            std::fs::create_dir_all(path.parent().unwrap()).map_err(err)?;
            std::fs::rename(&temp, &path).map_err(err)?;
            tx.execute(
                "INSERT OR IGNORE INTO enhanced_chunks VALUES (?,?,?,?,?)",
                params![p.chunk.id, self.meeting, relative, p.chunk.samples, engine],
            )
            .map_err(err)?;
            if let Some(echo) = self.echo.as_mut() {
                let start = p
                    .chunk
                    .timing
                    .as_ref()
                    .map_or(p.chunk.start * 16, |t| t.start);
                let intervals = echo.intervals(start, start + p.chunk.samples as u64);
                if intervals.iter().map(|v| v.end - v.start).sum::<u64>() != p.chunk.samples as u64
                {
                    return Err("AEC区间账目不完整".into());
                }
                for v in intervals {
                    tx.execute(
                        "INSERT INTO aec_intervals VALUES (?,?,?,?,?,?)",
                        params![p.chunk.id, v.start, v.end, v.state, v.reason, v.delay],
                    )
                    .map_err(err)?;
                }
            }
            tx.commit().map_err(err)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(temp);
        result
    }
}
#[cfg(feature = "audio-processing")]
struct Job {
    meeting: String,
    generation: u64,
    mode: String,
    state: String,
    echo_enabled: bool,
}
#[cfg(feature = "audio-processing")]
fn step(rt: &Runtime, cache: &mut Option<Session>) -> Result<(), (String, u64, String)> {
    // One native instance, one raw chunk read at a time and bounded input/output buffers.
    let selected=(|| -> Result<Option<Job>,String> {
        rt.db()?.query_row("SELECT e.meeting_id,e.generation,e.mode,m.state,e.echo_cancellation FROM audio_enhancement e JOIN meetings m ON m.id=e.meeting_id WHERE (e.mode!='off' OR e.echo_cancellation=1) AND e.engine IN (?,?) AND e.state IN ('queued','processing') AND m.state NOT IN ('ready','deleting') ORDER BY (m.state='recording') DESC,m.created_at LIMIT 1",[ENGINE,crate::echo::ENGINE],|r|Ok(Job{meeting:r.get(0)?,generation:r.get(1)?,mode:r.get(2)?,state:r.get(3)?,echo_enabled:r.get(4)?})).optional().map_err(err)
    })().map_err(|e|(String::new(),0,e))?;
    let Some(Job {
        meeting: mid,
        generation,
        mode,
        state,
        echo_enabled,
    }) = selected
    else {
        *cache = None;
        return Ok(());
    };
    let run = (|| -> Result<(), String> {
        if cache
            .as_ref()
            .is_none_or(|s| s.meeting != mid || s.generation != generation)
        {
            *cache = Some(Session {
                meeting: mid.clone(),
                generation,
                processor: crate::processing::Processor::new(match mode.as_str() {
                    "off" => 0,
                    "low" => 1,
                    _ => 2,
                })?,
                echo_enabled,
                echo: None,
                noise: match mode.as_str() {
                    "off" => 0,
                    "low" => 1,
                    _ => 2,
                },
                frames: if echo_enabled {
                    meeting_core::audio_processing::Frames::with_delay(
                        128 + if mode == "off" { 0 } else { 96 },
                    )
                } else {
                    Default::default()
                },
                last: None,
                pending: Default::default(),
                boundary: true,
            });
        }
        let s = cache.as_mut().unwrap();
        let after = s.last.as_ref().map(|c| c.start as i64).unwrap_or(-1);
        let chunk=rt.db()?.query_row("SELECT c.id,c.relative_path,c.start_ms,c.end_ms,c.sample_count,(SELECT id FROM recording_epochs WHERE meeting_id=c.meeting_id AND recording_start_ms<=c.start_ms ORDER BY recording_start_ms DESC,rowid DESC LIMIT 1),EXISTS(SELECT 1 FROM enhanced_chunks WHERE chunk_id=c.id) FROM audio_chunks c WHERE c.meeting_id=? AND c.source='microphone' AND c.status='sealed' AND c.start_ms>? ORDER BY c.start_ms LIMIT 1",params![mid,after],|r|Ok(Chunk{id:r.get(0)?,path:r.get(1)?,start:r.get(2)?,end:r.get(3)?,samples:r.get(4)?,epoch:r.get(5)?,exists:r.get(6)?,timing:None})).optional().map_err(err)?;
        if let Some(mut chunk) = chunk {
            chunk.timing = crate::echo::timing(rt, &chunk.id)?;
            let start_sample = chunk.timing.as_ref().map_or(chunk.start * 16, |t| t.start);
            if echo_enabled
                && crate::echo::Echo::waiting(
                    rt,
                    &mid,
                    start_sample + chunk.samples as u64,
                    &state,
                )?
            {
                return Ok(());
            }
            if s.last.as_ref().is_some_and(|last| {
                last.epoch != chunk.epoch
                    || (!echo_enabled && last.end != chunk.start)
                    || (echo_enabled
                        && (last.timing.as_ref().map_or(last.start * 16, |t| t.start)
                            + last.samples as u64
                            != start_sample
                            || s.echo
                                .as_ref()
                                .is_some_and(|e| e.discontinuous(chunk.timing.as_ref()))))
            }) && !s.boundary
            {
                s.flush(rt)?;
            }
            let path = safe_relative(&rt.root, &chunk.path)?;
            if chunk.samples == 0
                || chunk.samples > 160000
                || std::fs::metadata(&path).map_err(err)?.len() > 324096
            {
                return Err("增强源片段大小无效，原轨未更改".into());
            }
            let pcm = audio::read_wav(&path)?;
            if pcm.len() != chunk.samples {
                return Err("增强源片段长度与索引不一致".into());
            }
            if echo_enabled {
                if s.echo.is_none() {
                    s.echo = Some(crate::echo::Echo::new(s.noise, start_sample)?);
                }
                s.echo.as_mut().unwrap().load(
                    rt,
                    &mid,
                    start_sample,
                    chunk.samples,
                    chunk.timing.as_ref(),
                )?;
            }
            s.pending.push_back(Pending {
                chunk: chunk.clone(),
                output: Vec::with_capacity(chunk.samples),
            });
            let output = s.frames.push(&pcm, |frame| match s.echo.as_mut() {
                Some(echo) => echo.process(frame, &mut s.processor),
                None => s.processor.process(frame),
            })?;
            s.distribute(rt, &output)?;
            s.last = Some(chunk);
            s.boundary = false;
            rt.db()?.execute("UPDATE audio_enhancement SET state='processing',error=NULL WHERE meeting_id=? AND generation=? AND state!='failed'",params![mid,generation]).map_err(err)?;
        } else if state != "recording" {
            if !s.boundary {
                s.flush(rt)?;
            }
            if !["paused", "interrupted"].contains(&state.as_str()) {
                let bad:u64=rt.db()?.query_row("SELECT count(*) FROM audio_chunks WHERE meeting_id=? AND source='microphone' AND status!='sealed'",[&mid],|r|r.get(0)).map_err(err)?;
                if bad > 0 {
                    return Err("麦克风原轨存在未恢复片段，请先处理原轨故障后重试增强".into());
                }
                rt.db()?.execute("UPDATE audio_enhancement SET state='complete',error=NULL WHERE meeting_id=? AND generation=? AND NOT EXISTS(SELECT 1 FROM audio_chunks c LEFT JOIN enhanced_chunks e ON e.chunk_id=c.id WHERE c.meeting_id=? AND c.source='microphone' AND (c.status!='sealed' OR e.chunk_id IS NULL))",params![mid,generation,mid]).map_err(err)?;
                *cache = None;
            }
        } else {
            let active:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM recording_sources WHERE meeting_id=? AND source='microphone' AND state='active')",[&mid],|r|r.get(0)).map_err(err)?;
            if !active && !s.boundary {
                s.flush(rt)?;
            }
        }
        Ok(())
    })();
    run.map_err(|e| (mid, generation, e))
}

#[cfg(all(test, feature = "audio-processing"))]
mod tests {
    use super::*;
    use crate::{models::tests::MemoryVault, processing::Processor};
    use meeting_core::audio_processing::Frames;
    struct Fixture {
        rt: Arc<Runtime>,
        mid: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("helmmeet-ns-{}", id()));
            let rt = Runtime::open_with_vault(root, Arc::new(MemoryVault::default())).unwrap();
            let mid = rt.create("合成降噪测试", "general").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            configure(&rt, &mid, "low").unwrap();
            Self { rt, mid }
        }
        fn chunk(&self, source: &str, start: u64, pcm: &[f32]) -> (String, Vec<u8>) {
            let cid = id();
            let relative = format!("meetings/{}/audio/{cid}.wav", self.mid);
            let path = self.rt.root.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut wav = audio::WavFile::create(&path).unwrap();
            wav.write(pcm).unwrap();
            wav.sync().unwrap();
            drop(wav);
            self.rt
                .db()
                .unwrap()
                .execute(
                    "INSERT INTO audio_chunks VALUES (?,?,?,?,?,?,?,'sealed')",
                    params![
                        cid,
                        self.mid,
                        source,
                        relative,
                        start,
                        start + pcm.len() as u64 / 16,
                        pcm.len()
                    ],
                )
                .unwrap();
            (cid, std::fs::read(path).unwrap())
        }
        fn epoch(&self, at: u64) -> String {
            let epoch = id();
            self.rt.db().unwrap().execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES (?,?,?,'synthetic','recording')",params![epoch,self.mid,at/16]).unwrap();
            epoch
        }
        fn timed(&self, source: &str, at: u64, pcm: &[f32], epoch: &str) -> String {
            let (cid, _) = self.chunk(source, at / 16, pcm);
            self.rt
                .db()
                .unwrap()
                .execute(
                    "INSERT INTO audio_chunk_timing VALUES (?,?,?,?,?)",
                    params![cid, self.mid, epoch, at, 1000000],
                )
                .unwrap();
            cid
        }
        fn route(&self, epoch: &str, start: u64, end: u64, supported: bool) {
            self.rt
                .db()
                .unwrap()
                .execute(
                    "INSERT INTO audio_output_routes VALUES (?,?,?,?,?,1,1,48000,?)",
                    params![id(), self.mid, epoch, start, end, supported],
                )
                .unwrap();
        }
        fn state(&self, state: &str, end: u64) {
            self.rt.db().unwrap().execute("UPDATE meetings SET state=?,recording_duration_ms=?,wall_start='2026-10-19T00:00:00Z' WHERE id=?",params![state,end,self.mid]).unwrap();
        }
        fn finish(&self, cache: &mut Option<Session>) {
            for _ in 0..30 {
                step(&self.rt, cache).unwrap();
                if detail(&self.rt, &self.mid).unwrap()["state"] == "complete" {
                    return;
                }
            }
            panic!("synthetic task did not finish");
        }
        fn enhanced(&self) -> Vec<f32> {
            let rows = {
                let db = self.rt.db().unwrap();
                let mut q=db.prepare("SELECT e.relative_path FROM enhanced_chunks e JOIN audio_chunks c ON c.id=e.chunk_id WHERE e.meeting_id=? ORDER BY c.start_ms").unwrap();
                q.query_map([&self.mid], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            rows.iter()
                .flat_map(|p| audio::read_wav(&self.rt.root.join(p)).unwrap())
                .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.rt.root);
        }
    }
    fn render(input: &[f32]) -> Vec<f32> {
        let mut p = Processor::new(1).unwrap();
        let mut f = Frames::default();
        let mut out = f.push(input, |x| p.process(x)).unwrap();
        out.extend(f.finish(|x| p.process(x)).unwrap());
        out.iter()
            .map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16 as f32 / 32768.0)
            .collect()
    }
    fn signal(count: usize) -> Vec<f32> {
        (0..count)
            .map(|i| 0.2 * (i as f32 * 0.11).sin() + 0.02 * (i as f32 * 2.13).sin())
            .collect()
    }
    #[test]
    fn cross_file_tail_and_restart_are_sample_exact_and_raw_asr_stays_unchanged() {
        let f = Fixture::new();
        let pcm = signal(64007);
        let (first, hash) = f.chunk("microphone", 0, &pcm[..32003]);
        let (second, _) = f.chunk("microphone", 2000, &pcm[32003..]);
        f.state("finalizing", 4000);
        let raw_before = crate::asr::mix(&f.rt, &f.mid, 0, 4000).unwrap();
        assert!(mix(&f.rt, &f.mid, 0, 4000, false).is_err());
        assert!(require_export(&f.rt, &f.mid).is_err());
        let mut cache = None;
        step(&f.rt, &mut cache).unwrap();
        step(&f.rt, &mut cache).unwrap();
        assert_eq!(
            f.rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM enhanced_chunks", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            1
        );
        drop(cache.take());
        recover(&f.rt).unwrap();
        f.finish(&mut cache);
        let input: Vec<f32> = pcm
            .iter()
            .map(|v| (v * 32767.0) as i16 as f32 / 32768.0)
            .collect();
        assert_eq!(f.enhanced(), render(&input));
        assert_eq!(f.enhanced().len(), 64007);
        assert_eq!(
            std::fs::read(f.rt.meeting_dir(&f.mid).join(format!("audio/{first}.wav"))).unwrap(),
            hash
        );
        assert_eq!(crate::asr::mix(&f.rt, &f.mid, 0, 4000).unwrap(), raw_before);
        require_export(&f.rt, &f.mid).unwrap();
        let path: String =
            f.rt.db()
                .unwrap()
                .query_row(
                    "SELECT relative_path FROM enhanced_chunks WHERE chunk_id=?",
                    [second],
                    |r| r.get(0),
                )
                .unwrap();
        std::fs::remove_file(f.rt.root.join(path)).unwrap();
        recover(&f.rt).unwrap();
        drop(cache.take());
        f.finish(&mut cache);
        assert_eq!(f.enhanced(), render(&input));
    }
    #[test]
    fn gap_and_pause_epoch_reset_filter_without_fabricating_archival_samples() {
        let f = Fixture::new();
        let pcm = signal(3217);
        f.chunk("microphone", 0, &pcm);
        f.chunk("microphone", 500, &pcm);
        f.state("finalizing", 701);
        f.finish(&mut None);
        let input: Vec<f32> = pcm
            .iter()
            .map(|v| (v * 32767.0) as i16 as f32 / 32768.0)
            .collect();
        let expected = render(&input);
        assert_eq!(f.enhanced(), [expected.clone(), expected].concat());
        assert!(mix(&f.rt, &f.mid, 201, 500, true)
            .unwrap()
            .iter()
            .all(|v| *v == 0.0));
        let g = Fixture::new();
        let pcm = signal(3200);
        g.chunk("microphone", 0, &pcm);
        g.chunk("microphone", 200, &pcm);
        for (start, epoch) in [(0, "e1"), (200, "e2")] {
            g.rt.db().unwrap().execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES (?,?,?,'x','recording')",params![epoch,g.mid,start]).unwrap();
        }
        g.state("finalizing", 400);
        g.finish(&mut None);
        let input: Vec<f32> = pcm
            .iter()
            .map(|v| (v * 32767.0) as i16 as f32 / 32768.0)
            .collect();
        let expected = render(&input);
        assert_eq!(g.enhanced(), [expected.clone(), expected].concat());
    }
    #[test]
    fn defaults_freeze_system_mix_and_late_publish_delete_fence() {
        let f = Fixture::new();
        set_default(&f.rt, "moderate").unwrap();
        assert_eq!(detail(&f.rt, &f.mid).unwrap()["mode"], "low");
        let other = f.rt.create("新默认", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(detail(&f.rt, &other).unwrap()["mode"], "moderate");
        let (mic_id, _) = f.chunk("microphone", 0, &signal(3200));
        let (_, system) = f.chunk("system", 0, &vec![0.1; 3200]);
        f.state("finalizing", 200);
        assert!(configure(&f.rt, &f.mid, "off").is_err());
        f.finish(&mut None);
        let mic = mix(&f.rt, &f.mid, 0, 200, true).unwrap();
        let mixed = mix(&f.rt, &f.mid, 0, 200, false).unwrap();
        let sys = crate::asr::mix_source(&f.rt, &f.mid, 0, 200, Some("system")).unwrap();
        assert!(mixed
            .iter()
            .zip(&mic)
            .zip(sys)
            .all(|((m, n), s)| (*m - (*n + s) * 0.5).abs() < 0.000001));
        let system_path: String =
            f.rt.db()
                .unwrap()
                .query_row(
                    "SELECT relative_path FROM audio_chunks WHERE meeting_id=? AND source='system'",
                    [&f.mid],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(std::fs::read(f.rt.root.join(system_path)).unwrap(), system);
        let mut stale = Session {
            meeting: f.mid.clone(),
            generation: 1,
            processor: Processor::new(1).unwrap(),
            echo_enabled: false,
            echo: None,
            noise: 1,
            frames: Default::default(),
            last: None,
            pending: Default::default(),
            boundary: true,
        };
        let c = Chunk {
            id: mic_id,
            path: String::new(),
            start: 0,
            end: 200,
            samples: 3200,
            epoch: None,
            exists: false,
            timing: None,
        };
        let path =
            f.rt.meeting_dir(&f.mid)
                .join(format!("enhanced/ns-v1/{}.wav", c.id));
        let before = std::fs::read(&path).unwrap();
        f.rt.db()
            .unwrap()
            .execute(
                "UPDATE audio_enhancement SET generation=2 WHERE meeting_id=?",
                [&f.mid],
            )
            .unwrap();
        assert!(stale
            .publish(
                &f.rt,
                Pending {
                    chunk: c.clone(),
                    output: vec![0.0; 3200]
                }
            )
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        stale.generation = 2;
        f.rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='deleting' WHERE id=?", [&f.mid])
            .unwrap();
        assert!(stale
            .publish(
                &f.rt,
                Pending {
                    chunk: c,
                    output: vec![0.0; 3200]
                }
            )
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        f.rt.db()
            .unwrap()
            .execute("DELETE FROM meetings WHERE id=?", [&f.mid])
            .unwrap();
        assert_eq!(
            f.rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM enhanced_chunks", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn native_delay_compensation_keeps_impulse_at_its_original_sample() {
        let mut input = vec![0.0; 16000];
        input[8000] = 0.8;
        let out = render(&input);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert_eq!(out.len(), input.len());
        assert_eq!(peak, 8000);
        assert!(out[8000] > 0.0);
    }
    #[test]
    fn invalid_source_fails_visibly_and_retry_does_not_touch_raw() {
        let f = Fixture::new();
        let (cid, bytes) = f.chunk("microphone", 0, &signal(3200));
        f.state("finalizing", 200);
        f.rt.db()
            .unwrap()
            .execute(
                "UPDATE audio_chunks SET sample_count=3199 WHERE id=?",
                [&cid],
            )
            .unwrap();
        let worker = background(f.rt.clone());
        for _ in 0..100 {
            if detail(&f.rt, &f.mid).unwrap()["state"] == "failed" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(detail(&f.rt, &f.mid).unwrap()["state"], "failed");
        drop(worker);
        f.rt.db()
            .unwrap()
            .execute(
                "UPDATE audio_chunks SET sample_count=3200 WHERE id=?",
                [&cid],
            )
            .unwrap();
        retry(&f.rt, &f.mid).unwrap();
        f.finish(&mut None);
        assert_eq!(
            std::fs::read(f.rt.meeting_dir(&f.mid).join(format!("audio/{cid}.wav"))).unwrap(),
            bytes
        );
    }
    #[test]
    fn ipc_defaults_to_raw_freezes_endpoint_and_rejects_partial_enhancement() {
        let f = Fixture::new();
        f.chunk("microphone", 0, &signal(6400));
        f.state("finalizing", 400);
        let operation = |fields: Value| {
            crate::commands::run(f.rt.clone(), serde_json::from_value(fields).unwrap())
        };
        let raw = operation(json!({"kind":"playback","meeting_id":f.mid,"start_ms":0})).unwrap();
        assert_eq!(raw["samples"].as_array().unwrap().len(), 6400);
        assert!(operation(
            json!({"kind":"playback","meeting_id":f.mid,"start_ms":0,"audio_variant":"enhanced"})
        )
        .is_err());
        f.finish(&mut None);
        let part=operation(json!({"kind":"playback","meeting_id":f.mid,"start_ms":0,"audio_variant":"enhanced","end_ms":200})).unwrap();
        assert_eq!(part["samples"].as_array().unwrap().len(), 3200);
        assert_eq!(part["end_ms"], 200);
        assert!(operation(
            json!({"kind":"playback","meeting_id":f.mid,"start_ms":0,"audio_variant":"unknown"})
        )
        .is_err());
    }
    #[test]
    fn graceful_shutdown_does_not_turn_pending_work_into_failure() {
        let f = Fixture::new();
        f.chunk("microphone", 0, &signal(6400));
        f.state("finalizing", 400);
        f.rt.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        let mut worker = background(f.rt.clone());
        std::thread::sleep(Duration::from_millis(250));
        worker.join().unwrap();
        assert_ne!(detail(&f.rt, &f.mid).unwrap()["state"], "failed");
        f.rt.shutting_down
            .store(false, std::sync::atomic::Ordering::Release);
        f.finish(&mut None);
        require_export(&f.rt, &f.mid).unwrap();
    }
    fn echo_signals(n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut seed = 39u32;
        let mut previous = 0.0;
        let far: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                previous =
                    0.6 * previous + 0.4 * (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                previous * 0.3
            })
            .collect();
        let mic=(0..n).map(|i|if i>320 {far[i-320]*0.5}else{0.0}+if i>=12*16000 {0.15*(i as f32*0.091).sin()}else{0.0}).collect();
        (far, mic)
    }
    fn echo_case(far: &[f32], mic: &[f32], packet: usize) -> Fixture {
        let f = Fixture::new();
        configure_options(&f.rt, &f.mid, "off", Some(true)).unwrap();
        let epoch = f.epoch(0);
        for (i, p) in far.chunks(64000).enumerate() {
            f.timed("system", (i * 64000) as u64, p, &epoch);
        }
        let mut at = 0;
        for p in mic.chunks(packet) {
            f.timed("microphone", at, p, &epoch);
            at += p.len() as u64;
        }
        f.route(&epoch, 0, mic.len() as u64, true);
        f.state("finalizing", mic.len() as u64 / 16);
        f
    }
    #[test]
    fn aec_disk_pipeline_arbitrary_chunks_restart_and_coverage_are_exact() {
        let (far, mic) = echo_signals(16 * 16000);
        let f = echo_case(&far, &mic, 64000);
        assert!(validate_capture(&f.rt, &f.mid, 1, "ready").is_err());
        let before = crate::asr::mix(&f.rt, &f.mid, 0, 16000).unwrap();
        let mut cache = None;
        step(&f.rt, &mut cache).unwrap();
        step(&f.rt, &mut cache).unwrap();
        drop(cache.take());
        recover(&f.rt).unwrap();
        f.finish(&mut cache);
        let g = echo_case(&far, &mic, 32003);
        g.finish(&mut None);
        let actual = f.enhanced();
        let wanted = g.enhanced();
        assert_eq!(actual.len(), wanted.len());
        let different = actual.iter().zip(&wanted).position(|(a, b)| a != b);
        assert!(different.is_none(), "first differing sample {different:?}");
        assert_eq!(f.enhanced().len(), mic.len());
        assert_eq!(crate::asr::mix(&f.rt, &f.mid, 0, 16000).unwrap(), before);
        assert!(
            detail(&f.rt, &f.mid).unwrap()["aec_applied_ms"]
                .as_u64()
                .unwrap()
                > 10000
        );
        let covered: u64 =
            f.rt.db()
                .unwrap()
                .query_row(
                    "SELECT sum(end_sample-start_sample) FROM aec_intervals",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(covered, mic.len() as u64);
        let output = f.enhanced();
        let energy = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        let raw_mic = crate::asr::mix_source(&f.rt, &f.mid, 0, 16000, Some("microphone")).unwrap();
        let erle = 10.0
            * (energy(&raw_mic[8 * 16000..10 * 16000]) / energy(&output[8 * 16000..10 * 16000]))
                .log10();
        println!("synthetic disk AEC echo-only reduction = {erle:.2}dB");
        assert!(erle > 10.0);
        require_export(&f.rt, &f.mid).unwrap();
        assert!(configure_options(&f.rt, &f.mid, "off", Some(false)).is_err());
    }
    #[test]
    fn reference_holes_unsupported_routes_and_future_samples_use_visible_fallback() {
        for scenario in [
            "hole",
            "future",
            "unsupported",
            "silence",
            "foreign_epoch",
            "unreadable",
        ] {
            let f = Fixture::new();
            configure_options(&f.rt, &f.mid, "off", Some(true)).unwrap();
            let epoch = f.epoch(0);
            let (far, mic) = echo_signals(64000);
            f.timed("microphone", 0, &mic, &epoch);
            match scenario {
                "hole" => {
                    f.timed("system", 0, &far[..16000], &epoch);
                    f.timed("system", 32000, &far[32000..], &epoch);
                }
                "future" => {
                    f.timed("system", 64000, &far, &epoch);
                }
                "foreign_epoch" => {
                    let other = f.epoch(100000);
                    f.timed("system", 0, &far, &other);
                }
                "silence" => {
                    f.timed("system", 0, &vec![0.0; 64000], &epoch);
                }
                _ => {
                    f.timed("system", 0, &far, &epoch);
                }
            }
            if scenario == "unreadable" {
                let path: String =
                    f.rt.db()
                        .unwrap()
                        .query_row(
                            "SELECT relative_path FROM audio_chunks WHERE source='system'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                std::fs::remove_file(f.rt.root.join(path)).unwrap();
            }
            f.route(&epoch, 0, 64000, scenario != "unsupported");
            f.state("finalizing", 4000);
            f.finish(&mut None);
            let d = detail(&f.rt, &f.mid).unwrap();
            assert_eq!(f.enhanced().len(), 64000);
            if scenario == "silence" {
                assert_eq!(d["aec_status"]["state"], "silent");
            } else {
                assert!(
                    d["aec_bypass_ms"].as_u64().unwrap() >= 990,
                    "{scenario}: {d}"
                );
            }
            if scenario != "hole" {
                let expected: Vec<f32> = mic
                    .iter()
                    .map(|v| {
                        let raw = (v * 32767.0) as i16 as f32 / 32768.0;
                        (raw * 32767.0) as i16 as f32 / 32768.0
                    })
                    .collect();
                assert_eq!(f.enhanced(), expected, "fallback {scenario}");
                assert_eq!(d["aec_applied_ms"], 0);
            }
        }
    }
    #[test]
    fn sealed_reference_waits_for_route_poll_without_false_boundary_resets() {
        let (far, mic) = echo_signals(8 * 16000);
        let f = echo_case(&far, &mic, 64000);
        f.rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'system','active',0)",[&f.mid]).unwrap();
        f.rt.db()
            .unwrap()
            .execute("UPDATE audio_output_routes SET end_sample=63000", [])
            .unwrap();
        f.state("recording", 8000);
        let mut cache = None;
        step(&f.rt, &mut cache).unwrap();
        assert_eq!(detail(&f.rt, &f.mid).unwrap()["state"], "queued");
        assert!(crate::echo::Echo::waiting(&f.rt, &f.mid, 64000, "recording").unwrap());
        // A stable later observation confirms the range; don't fabricate a route
        // in load(). Actual unknown switch gaps are covered by the adjacent test.
        f.rt.db()
            .unwrap()
            .execute("UPDATE audio_output_routes SET end_sample=129000", [])
            .unwrap();
        assert!(!crate::echo::Echo::waiting(&f.rt, &f.mid, 64000, "recording").unwrap());
        step(&f.rt, &mut cache).unwrap();
        step(&f.rt, &mut cache).unwrap();
        f.state("finalizing", 8000);
        f.finish(&mut cache);
        let false_gaps: u64 =
            f.rt.db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM aec_intervals WHERE reason='route_unobserved'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(false_gaps, 0);
        // A broken route observer has the same bounded wait as a late reference.
        f.rt.db()
            .unwrap()
            .execute("UPDATE audio_output_routes SET end_sample=63000", [])
            .unwrap();
        f.state("recording", 13000);
        assert!(!crate::echo::Echo::waiting(&f.rt, &f.mid, 64000, "recording").unwrap());
    }
    #[test]
    fn late_reference_wait_is_bounded_and_route_switch_resets_calibration() {
        let f = Fixture::new();
        configure_options(&f.rt, &f.mid, "off", Some(true)).unwrap();
        let epoch = f.epoch(0);
        let (_, mic) = echo_signals(64000);
        f.timed("microphone", 0, &mic, &epoch);
        f.rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'system','active',0)",[&f.mid]).unwrap();
        f.state("recording", 4000);
        let mut cache = None;
        step(&f.rt, &mut cache).unwrap();
        assert_eq!(detail(&f.rt, &f.mid).unwrap()["processed_ms"], 0);
        assert!(crate::echo::Echo::waiting(&f.rt, &f.mid, 64000, "recording").unwrap());
        f.state("recording", 13000);
        assert!(!crate::echo::Echo::waiting(&f.rt, &f.mid, 64000, "recording").unwrap());
        f.state("finalizing", 4000);
        f.finish(&mut cache);
        assert_eq!(detail(&f.rt, &f.mid).unwrap()["aec_applied_ms"], 0);
        let (far, mic) = echo_signals(8 * 16000);
        let g = echo_case(&far, &mic, 64000);
        g.rt.db()
            .unwrap()
            .execute("UPDATE audio_output_routes SET end_sample=64000", [])
            .unwrap();
        let epoch: String =
            g.rt.db()
                .unwrap()
                .query_row("SELECT id FROM recording_epochs", [], |r| r.get(0))
                .unwrap();
        g.route(&epoch, 65000, 128000, true);
        g.finish(&mut None);
        let reason:u64=g.rt.db().unwrap().query_row("SELECT count(*) FROM aec_intervals WHERE state='bypass' AND reason='route_unobserved'",[],|r|r.get(0)).unwrap();
        assert!(reason > 0);
        let warming:u64=g.rt.db().unwrap().query_row("SELECT sum(end_sample-max(start_sample,65000)) FROM aec_intervals WHERE state='warming' AND end_sample>65000",[],|r|r.get(0)).unwrap();
        assert!(warming >= 30000);
    }
}
