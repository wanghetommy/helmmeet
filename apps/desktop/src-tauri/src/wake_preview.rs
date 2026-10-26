//! Explicit settings previews: one latest job, shared fixed response cache, no capture.
use crate::{db::Runtime, lifecycle::Worker};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

struct Job {
    id: u64,
    epoch: u64,
    config: Value,
}
pub struct Ready {
    pub id: u64,
    pub epoch: u64,
    pub pcm: Vec<f32>,
}
#[derive(Default)]
struct State {
    pending: Option<Job>,
    ready: Option<Ready>,
    status: Value,
    last_selection: u64,
}
#[derive(Default)]
pub struct Hub {
    generation: AtomicU64,
    state: Mutex<State>,
}
impl Hub {
    pub fn request(&self, config: Value, epoch: u64, selection: u64) -> Result<u64, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if selection == 0 || selection > 9_007_199_254_740_991 || selection <= state.last_selection
        {
            return Err("已被新的音色选择替代".into());
        }
        state.last_selection = selection;
        let id = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        state.status =
            json!({"id":id,"speaker":config["speaker"],"state":"preparing","cached":false});
        state.ready = None;
        state.pending = Some(Job { id, epoch, config });
        Ok(id)
    }
    pub fn current(&self, id: u64) -> bool {
        self.generation.load(Ordering::Acquire) == id
    }
    pub fn cancel(&self, expected: Option<u64>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if expected.is_some_and(|id| !self.current(id)) {
            return;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        state.pending = None;
        state.ready = None;
        if state.status.is_object() {
            state.status["state"] = json!("cancelled");
        }
    }
    pub fn status(&self) -> Value {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn take_ready(&self) -> Option<Ready> {
        self.state
            .lock()
            .ok()?
            .ready
            .take()
            .filter(|r| self.current(r.id))
    }
    pub fn phase(&self, id: u64, phase: &str, error: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.current(id) {
            return;
        }
        state.status["state"] = json!(phase);
        state.status["error"] = json!(error);
    }
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    background_with(rt, |rt, config, check| {
        crate::tts::synthesize_snapshot(rt, config, crate::wake_ack::TEXT, check)
    })
}
fn background_with(
    rt: Arc<Runtime>,
    synthesize: impl Fn(&Runtime, &Value, &dyn Fn() -> Result<(), String>) -> Result<Vec<f32>, String>
        + Send
        + 'static,
) -> Worker {
    Worker::spawn(move |stop| {
        while !stop.is_stopped() && !rt.shutting_down.load(Ordering::Acquire) {
            let job = rt
                .voice
                .preview
                .state
                .lock()
                .ok()
                .and_then(|mut s| s.pending.take());
            let Some(job) = job else {
                stop.wait(Duration::from_millis(20));
                continue;
            };
            let check = || {
                if stop.is_stopped()
                    || rt.shutting_down.load(Ordering::Acquire)
                    || !rt.voice.preview.current(job.id)
                    || rt.voice.epoch.load(Ordering::Acquire) != job.epoch
                {
                    Err("试听已取消".to_owned())
                } else {
                    Ok(())
                }
            };
            let key =
                crate::wake_ack::fingerprint(&job.config).expect("validated enabled TTS preview");
            let cached = crate::wake_ack::load(&rt.root, &key).ok();
            let mut hit = cached.is_some();
            let result = (|| {
                check()?;
                if let Some(pcm) = cached {
                    return Ok(pcm);
                }
                let _synthesis = crate::wake_ack::synthesis_guard(&rt.voice.ack, &check)?;
                if let Ok(pcm) = crate::wake_ack::load(&rt.root, &key) {
                    hit = true;
                    return Ok(pcm);
                }
                let pcm = synthesize(&rt, &job.config, &check)?;
                check()?;
                crate::wake_ack::valid(&pcm)?;
                crate::wake_ack::save(&rt.root, &key, &pcm)?;
                Ok(pcm)
            })();
            if check().is_err() {
                rt.voice.preview.phase(job.id, "cancelled", None);
                continue;
            }
            match result {
                Ok(pcm) => {
                    if crate::tts::saved(&rt)
                        .ok()
                        .and_then(|cfg| crate::wake_ack::fingerprint(&cfg))
                        == Some(key)
                        && rt.voice.ack.pcm(&job.config).is_none()
                    {
                        rt.voice.ack.invalidate();
                    }
                    let mut state = rt
                        .voice
                        .preview
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if rt.voice.preview.current(job.id) {
                        state.status["state"] = json!("ready");
                        state.status["cached"] = json!(hit);
                        state.ready = Some(Ready {
                            id: job.id,
                            epoch: job.epoch,
                            pcm,
                        });
                    }
                }
                Err(error) => rt.voice.preview.phase(job.id, "failed", Some(error)),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc};
    use std::time::Instant;
    fn fixture() -> (tempfile::TempDir, Arc<Runtime>) {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        (root, rt)
    }
    fn config(speaker: &str) -> Value {
        json!({"enabled":true,"speaker":speaker,"model":"seed-tts-2.0-standard","profile_id":"synthetic-profile","config_version":1})
    }
    fn wait(check: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        while !check() {
            assert!(Instant::now() < end, "preview timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn preview_cached_voices_reuses_exact_audio_without_changing_settings_or_capture() {
        let (_root, rt) = fixture();
        let before = crate::tts::saved(&rt).unwrap();
        let worker = background_with(rt.clone(), |_, _, _| panic!("Must use fixed cache"));
        for (i, speaker) in ["a", "b", "a"].into_iter().enumerate() {
            let cfg = config(speaker);
            let key = crate::wake_ack::fingerprint(&cfg).unwrap();
            crate::wake_ack::save(&rt.root, &key, &vec![0.25; 16000]).unwrap();
            let id = rt.voice.preview.request(cfg, 0, i as u64 + 1).unwrap();
            wait(|| rt.voice.preview.status()["state"] == "ready");
            let ready = rt.voice.preview.take_ready().unwrap();
            assert_eq!(ready.id, id);
            assert_eq!(ready.pcm, vec![0.25; 16000]);
            assert_eq!(rt.voice.preview.status()["cached"], true);
        }
        assert_eq!(crate::tts::saved(&rt).unwrap(), before);
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), 0);
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM meetings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(worker);
    }
    #[test]
    fn newer_selection_cancels_inflight_audio_and_out_of_order_ipc_cannot_replace_it() {
        let (_root, rt) = fixture();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = background_with(rt.clone(), move |_, cfg, check| {
            if cfg["speaker"] == "old" {
                tx.send(()).unwrap();
                while check().is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Ok(vec![0.75; 16000]);
            }
            Ok(vec![0.25; 16000])
        });
        let old = rt.voice.preview.request(config("old"), 0, 10).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let new = rt.voice.preview.request(config("new"), 0, 12).unwrap();
        assert!(rt.voice.preview.request(config("late-old"), 0, 11).is_err());
        rt.voice.preview.cancel(Some(old)); // Late cleanup must not cancel the new choice.
        wait(|| rt.voice.preview.status()["state"] == "ready");
        assert_eq!(rt.voice.preview.take_ready().unwrap().id, new);
        assert!(crate::wake_ack::load(
            &rt.root,
            &crate::wake_ack::fingerprint(&config("old")).unwrap()
        )
        .is_err());
        rt.voice.preview.request(config("old"), 0, 13).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn failed_preview_does_not_retry_and_cancelled_capture_never_publishes_audio() {
        let (_root, rt) = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let worker = background_with(rt.clone(), move |_, _, _| {
            count.fetch_add(1, Ordering::AcqRel);
            Err("synthetic unavailable".into())
        });
        rt.voice.preview.request(config("a"), 0, 1).unwrap();
        wait(|| rt.voice.preview.status()["state"] == "failed");
        assert!(rt.voice.preview.take_ready().is_none());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        rt.voice.preview.request(config("a"), 0, 2).unwrap();
        rt.voice.epoch.fetch_add(1, Ordering::AcqRel);
        wait(|| rt.voice.preview.status()["state"] == "cancelled");
        assert!(rt.voice.preview.take_ready().is_none());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        drop(worker);
    }
    #[test]
    fn shared_synthesis_gate_rechecks_cache_and_never_duplicates_a_finished_synthesis() {
        let (_root, rt) = fixture();
        let cfg = config("a");
        let key = crate::wake_ack::fingerprint(&cfg).unwrap();
        let guard = crate::wake_ack::synthesis_guard(&rt.voice.ack, &|| Ok(())).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let worker = background_with(rt.clone(), move |_, _, _| {
            count.fetch_add(1, Ordering::AcqRel);
            Ok(vec![0.75; 16000])
        });
        rt.voice.preview.request(cfg, 0, 1).unwrap();
        // Simulate the startup prewarmer completing while preview waits for it.
        crate::wake_ack::save(&rt.root, &key, &vec![0.25; 16000]).unwrap();
        drop(guard);
        wait(|| rt.voice.preview.status()["state"] == "ready");
        assert_eq!(
            rt.voice.preview.take_ready().unwrap().pcm,
            vec![0.25; 16000]
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);
        drop(worker);
    }
}
