//! Latest-wins, cancellable TTS preparation. Never owns capture or a player.
use crate::{db::Runtime, lifecycle::Worker};
use std::sync::{atomic::Ordering, mpsc, Arc, Mutex};
use std::time::Duration;

pub struct SpeechJob {
    pub generation: u64,
    pub capture_generation: u64,
    pub meeting: String,
    pub turn: String,
    pub text: String,
}
pub enum Prepared {
    Api { pcm: Vec<f32>, fallback: bool },
    Local { error: Option<String> },
}
pub struct Ready {
    pub job: SpeechJob,
    pub result: Result<Prepared, String>,
}
pub struct SpeechWorker {
    rt: Arc<Runtime>,
    pending: Arc<Mutex<Option<SpeechJob>>>,
    ready: mpsc::Receiver<Ready>,
    _worker: Worker,
}
impl SpeechWorker {
    pub fn new(rt: Arc<Runtime>) -> Self {
        Self::with_prepare(rt, prepare)
    }
    fn with_prepare(
        rt: Arc<Runtime>,
        prepare: impl Fn(&Runtime, &str, &dyn Fn() -> Result<(), String>) -> Result<Prepared, String>
            + Send
            + 'static,
    ) -> Self {
        let pending: Arc<Mutex<Option<SpeechJob>>> = Arc::new(Mutex::new(None));
        let jobs = pending.clone();
        let owner = rt.clone();
        let (tx, ready) = mpsc::sync_channel(2);
        let worker = Worker::spawn(move |stop| {
            while !stop.is_stopped() {
                let job = jobs.lock().ok().and_then(|mut j| j.take());
                let Some(job) = job else {
                    stop.wait(Duration::from_millis(20));
                    continue;
                };
                let check = || {
                    if stop.is_stopped()
                        || !current(&owner, &job)
                        || owner.shutting_down.load(Ordering::Acquire)
                    {
                        return Err("播报已取消".into());
                    }
                    Ok(())
                };
                let result = prepare(&owner, &job.text, &check);
                if check().is_ok() {
                    let _ = tx.try_send(Ready { job, result });
                }
            }
        });
        Self {
            rt,
            pending,
            ready,
            _worker: worker,
        }
    }
    pub fn cancel(&self) {
        self.rt.voice.speech_epoch.fetch_add(1, Ordering::AcqRel);
        while self.ready.try_recv().is_ok() {}
        if let Ok(mut pending) = self.pending.lock() {
            *pending = None;
        }
    }
    pub fn request(&self, meeting: &str, turn: &str, text: &str) -> Result<(), String> {
        let generation = self.rt.voice.speech_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        while self.ready.try_recv().is_ok() {}
        let job = SpeechJob {
            generation,
            capture_generation: self.rt.voice.epoch.load(Ordering::Acquire),
            meeting: meeting.into(),
            turn: turn.into(),
            text: text.into(),
        };
        *self.pending.lock().map_err(|_| "播报队列异常")? = Some(job);
        Ok(())
    }
    pub fn take_ready(&self) -> Option<Ready> {
        while let Ok(ready) = self.ready.try_recv() {
            if current(&self.rt, &ready.job) {
                return Some(ready);
            }
        }
        None
    }
}
fn current(rt: &Runtime, job: &SpeechJob) -> bool {
    rt.voice.speech_epoch.load(Ordering::Acquire) == job.generation
        && rt.voice.epoch.load(Ordering::Acquire) == job.capture_generation
}
fn prepare(
    rt: &Runtime,
    text: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Prepared, String> {
    check()?;
    let config = crate::tts::settings(rt)?;
    if config["enabled"] != true {
        return Ok(Prepared::Local { error: None });
    }
    match crate::tts::synthesize_cancellable(rt, text, check) {
        Ok(pcm) => {
            check()?;
            Ok(Prepared::Api {
                pcm,
                fallback: config["fallback_local"] != false,
            })
        }
        Err(error) => {
            check()?;
            if config["fallback_local"] == false {
                Err(error)
            } else {
                Ok(Prepared::Local { error: Some(error) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    #[test]
    fn superseding_a_slow_synthesis_releases_worker_without_stale_fallback_or_playback() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let (started, received) = mpsc::sync_channel(1);
        let output = SpeechWorker::with_prepare(rt, move |_, text, check| {
            if text == "old" {
                started.send(()).unwrap();
                loop {
                    check()?;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(Prepared::Api {
                pcm: vec![0.25; 160],
                fallback: true,
            })
        });
        output.request("m", "old-turn", "old").unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        output.request("m", "new-turn", "new").unwrap();
        let ready = loop {
            if let Some(ready) = output.take_ready() {
                break ready;
            }
            assert!(began.elapsed() < Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(ready.job.turn, "new-turn");
        assert!(matches!(ready.result, Ok(Prepared::Api { .. })));
        assert!(output.take_ready().is_none());
        output.request("m", "old-turn", "old").unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        drop(output); // stop token cancels a running synthesis, then joins it
        assert!(began.elapsed() < Duration::from_secs(1));
    }
}
