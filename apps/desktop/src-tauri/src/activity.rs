//! Bounded metadata projection after persistence. Never opens devices or gates recording.
use crate::{db::Runtime, lifecycle::Worker};
use std::{
    collections::HashMap,
    ffi::{c_void, CString},
    sync::{
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
extern "C" {
    fn hm_meeting_vad_create(directory: *const std::ffi::c_char) -> *mut c_void;
    fn hm_kws_accept(handle: *mut c_void, pcm: *const f32, count: i32, detect: i32) -> i32;
    fn hm_kws_destroy(handle: *mut c_void);
}
struct Frame {
    meeting: String,
    source: i32,
    epoch: String,
    recording_ms: u64,
    host: u64,
    pcm: Vec<f32>,
    queued: Instant,
}
#[derive(Default)]
pub struct Hub {
    feed: Mutex<Option<SyncSender<Frame>>>,
    states: Mutex<HashMap<(String, i32), State>>,
}
struct State {
    updated: Instant,
    quiet_since: Option<u64>,
    end: u64,
    recording_end: u64,
    recording_quiet: Option<u64>,
}
struct Vad(*mut c_void);
impl Drop for Vad {
    fn drop(&mut self) {
        unsafe { hm_kws_destroy(self.0) }
    }
}
pub fn feed(
    rt: &Runtime,
    meeting: &str,
    epoch: &str,
    source: i32,
    host: u64,
    recording_ms: u64,
    pcm: &[f32],
) {
    if pcm.is_empty() || pcm.len() > 8192 {
        return;
    }
    if let Ok(slot) = rt.activity.feed.lock() {
        if let Some(tx) = slot.as_ref() {
            if tx
                .try_send(Frame {
                    meeting: meeting.into(),
                    epoch: epoch.into(),
                    recording_ms,
                    source,
                    host,
                    pcm: pcm.into(),
                    queued: Instant::now(),
                })
                .is_err()
            {
                if let Ok(mut states) = rt.activity.states.lock() {
                    states.remove(&(meeting.into(), source));
                }
            }
        }
    }
}
pub fn quiet(rt: &Runtime, mid: &str, sources: &[(i32, String)]) -> bool {
    if sources.is_empty() {
        return false;
    }
    let Ok(states) = rt.activity.states.lock() else {
        return false;
    };
    sources.iter().all(|(source, state)| {
        state == "active"
            && states.get(&(mid.into(), *source)).is_some_and(|s| {
                s.updated.elapsed() < Duration::from_millis(600)
                    && s.quiet_since
                        .is_some_and(|start| s.end.saturating_sub(start) >= 2_000_000_000)
            })
    })
}
type Observation = (i32, Duration, Option<u64>, u64);
fn combined_pause(sources: &[(i32, String)], candidates: &[Observation]) -> Option<(u64, u64)> {
    let mut start = 0;
    let mut end = u64::MAX;
    for (source, state) in sources {
        if state == "disabled" {
            continue;
        }
        match candidates.iter().find(|(id, age, quiet, _)| {
            id == source && *age < Duration::from_millis(600) && quiet.is_some()
        }) {
            Some((_, _, Some(quiet), until)) if state == "active" => {
                start = start.max(*quiet);
                end = end.min(*until);
            }
            _ => return None,
        }
    }
    (end != u64::MAX && end.saturating_sub(start) >= 1500).then_some((start, end))
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    Worker::spawn(move |stop| {
        let (tx, rx) = mpsc::sync_channel(64);
        *rt.activity.feed.lock().unwrap() = Some(tx);
        let mut streams: HashMap<i32, (Vad, u64)> = HashMap::new();
        let mut active = String::new();
        let mut active_epoch = String::new();
        let mut last_pause_write = Instant::now() - Duration::from_secs(1);
        let mut marked = false;
        while !stop.is_stopped() {
            let f = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if active != f.meeting || active_epoch != f.epoch {
                streams.clear();
                rt.activity.states.lock().unwrap().clear();
                active = f.meeting.clone();
                active_epoch = f.epoch.clone();
                marked = false;
            }
            // Paragraph activity is independent of summary/wake settings.
            let enabled = !f.epoch.is_empty();
            if !enabled || f.queued.elapsed() > Duration::from_millis(500) {
                streams.remove(&f.source);
                rt.activity
                    .states
                    .lock()
                    .unwrap()
                    .remove(&(f.meeting, f.source));
                continue;
            }
            let discontinuous = streams
                .get(&f.source)
                .is_some_and(|(_, end)| f.host.abs_diff(*end) > 150_000_000);
            if discontinuous {
                streams.remove(&f.source);
                rt.activity
                    .states
                    .lock()
                    .unwrap()
                    .remove(&(f.meeting.clone(), f.source));
            }
            if let std::collections::hash_map::Entry::Vacant(e) = streams.entry(f.source) {
                let Ok(path) = CString::new(crate::voice::model_dir().to_string_lossy().as_bytes())
                else {
                    continue;
                };
                // This worker exclusively owns the model handle and releases it on join.
                let p = unsafe { hm_meeting_vad_create(path.as_ptr()) };
                if p.is_null() {
                    continue;
                }
                e.insert((Vad(p), f.host));
            }
            let (vad, end) = streams.get_mut(&f.source).unwrap();
            let flags = unsafe { hm_kws_accept(vad.0, f.pcm.as_ptr(), f.pcm.len() as i32, 0) };
            *end = f.host + f.pcm.len() as u64 * 62500;
            let mut states = rt.activity.states.lock().unwrap();
            if flags < 0 {
                states.remove(&(f.meeting, f.source));
                streams.remove(&f.source);
                continue;
            }
            let s = states
                .entry((f.meeting.clone(), f.source))
                .or_insert(State {
                    updated: Instant::now(),
                    quiet_since: None,
                    end: *end,
                    recording_end: f.recording_ms,
                    recording_quiet: None,
                });
            s.updated = Instant::now();
            s.end = *end;
            s.recording_end = f.recording_ms + f.pcm.len() as u64 / 16;
            if flags & 2 != 0 {
                s.quiet_since = None;
                s.recording_quiet = None;
            } else {
                s.quiet_since.get_or_insert(f.host);
                s.recording_quiet.get_or_insert(f.recording_ms);
            }
            // Coalesce sustained all-source silence at <=4 writes/sec. Never
            // hold the activity lock while opening SQLite (summary reads it too).
            let candidates = states
                .iter()
                .filter(|((mid, _), _)| mid == &f.meeting)
                .map(|((_, source), s)| {
                    (
                        *source,
                        s.updated.elapsed(),
                        s.recording_quiet,
                        s.recording_end,
                    )
                })
                .collect::<Vec<_>>();
            drop(states);
            if let Ok(db) = rt.db() {
                if !marked {
                    marked = db.execute("INSERT OR IGNORE INTO speech_activity_epochs(epoch_id,meeting_id) VALUES (?,?)",
                        rusqlite::params![f.epoch,f.meeting]).is_ok();
                }
                if last_pause_write.elapsed() >= Duration::from_millis(250) {
                    last_pause_write = Instant::now();
                    let sources = db.prepare("SELECT CASE source WHEN 'microphone' THEN 0 ELSE 1 END,state FROM recording_sources WHERE meeting_id=?")
                        .and_then(|mut q|q.query_map([&f.meeting],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>());
                    if let Ok(sources) = sources {
                        if let Some((start, end)) = combined_pause(&sources, &candidates) {
                            let _=db.execute("INSERT INTO speech_pauses VALUES (?,?,?,?) ON CONFLICT(meeting_id,epoch_id,start_ms) DO UPDATE SET end_ms=max(end_ms,excluded.end_ms)",rusqlite::params![f.meeting,f.epoch,start,end]);
                        }
                    }
                }
            }
        }
        *rt.activity.feed.lock().unwrap() = None;
        rt.activity.states.lock().unwrap().clear();
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_fresh_all_source_quiet_has_a_recording_time_boundary() {
        let sources = vec![(0, "active".into()), (1, "active".into())];
        let mut observations = vec![
            (0, Duration::ZERO, Some(1000), 5000),
            (1, Duration::ZERO, Some(3500), 5000),
        ];
        assert_eq!(combined_pause(&sources, &observations), Some((3500, 5000)));
        observations[1].2 = None;
        assert!(combined_pause(&sources, &observations).is_none());
        observations[1].2 = Some(3500);
        observations[1].1 = Duration::from_millis(600);
        assert!(combined_pause(&sources, &observations).is_none());
        assert!(combined_pause(&[], &[]).is_none());
        assert!(combined_pause(&[(0, "failed".into())], &observations).is_none());
    }
    #[test]
    fn standalone_meeting_vad_detects_public_fixture_without_keyword_or_capture() {
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs/zh_5.wav");
        let mut pcm = crate::audio::read_wav(&path).unwrap();
        pcm.extend(vec![0.; 40000]);
        let dir = CString::new(crate::voice::model_dir().to_string_lossy().as_bytes()).unwrap();
        let ptr = unsafe { hm_meeting_vad_create(dir.as_ptr()) };
        assert!(!ptr.is_null());
        let vad = Vad(ptr);
        let mut heard = false;
        let mut tail = 0;
        for packet in pcm.chunks(640) {
            let flags = unsafe { hm_kws_accept(vad.0, packet.as_ptr(), packet.len() as i32, 0) };
            assert!(flags >= 0);
            assert_eq!(flags & 1, 0);
            heard |= flags & 2 != 0;
            tail = flags;
        }
        assert!(heard);
        assert_eq!(tail & 2, 0);
    }
    #[test]
    fn silence_requires_fresh_valid_state_for_every_selected_source() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let sources = vec![(0, "active".into()), (1, "active".into())];
        let state = || State {
            recording_end: 0,
            recording_quiet: None,
            updated: Instant::now(),
            quiet_since: Some(0),
            end: 2_000_000_000,
        };
        rt.activity
            .states
            .lock()
            .unwrap()
            .insert(("m".into(), 0), state());
        assert!(!quiet(&rt, "m", &sources));
        rt.activity
            .states
            .lock()
            .unwrap()
            .insert(("m".into(), 1), state());
        assert!(quiet(&rt, "m", &sources));
        rt.activity
            .states
            .lock()
            .unwrap()
            .get_mut(&("m".into(), 1))
            .unwrap()
            .quiet_since = None;
        assert!(!quiet(&rt, "m", &sources));
        assert!(!quiet(&rt, "other", &sources));
    }
}
