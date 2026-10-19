use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone)]
pub struct StopToken(Arc<AtomicBool>);
impl StopToken {
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn wait(&self, duration: Duration) -> bool {
        if !self.is_stopped() {
            thread::park_timeout(duration);
        }
        !self.is_stopped()
    }
}

/// Owns the thread, including the native resources on its stack. Joining is the
/// release fence: a stop flag alone does not release a cached Whisper context.
pub struct Worker {
    stop: StopToken,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn spawn(run: impl FnOnce(StopToken) + Send + 'static) -> Self {
        let stop = StopToken(Arc::new(AtomicBool::new(false)));
        let token = stop.clone();
        Self {
            stop,
            handle: Some(thread::spawn(move || run(token))),
        }
    }
    pub fn request_stop(&self) {
        self.stop.0.store(true, Ordering::Release);
        if let Some(handle) = &self.handle {
            handle.thread().unpark();
        }
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        self.request_stop();
        self.handle
            .take()
            .map_or(Ok(()), |h| h.join().map_err(|_| "后台线程异常退出"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

pub struct Shutdown {
    // running -> draining -> ready; failed never grants permission to exit.
    phase: AtomicU8,
    workers: Mutex<Vec<Worker>>,
}
impl Shutdown {
    pub fn new(workers: Vec<Worker>) -> Self {
        Self {
            phase: AtomicU8::new(0),
            workers: Mutex::new(workers),
        }
    }
    pub fn ready(&self) -> bool {
        self.phase.load(Ordering::Acquire) == 2
    }
    pub fn begin(&self) -> Option<Vec<Worker>> {
        self.phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(std::mem::take(
            &mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()),
        ))
    }
    pub fn finish(&self, success: bool) {
        self.phase
            .store(if success { 2 } else { 3 }, Ordering::Release);
    }
}
pub fn stop_workers(mut workers: Vec<Worker>) -> Result<(), &'static str> {
    for worker in &workers {
        worker.request_stop();
    }
    let mut result = Ok(());
    for worker in &mut workers {
        if let Err(error) = worker.join() {
            result = Err(error);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn shutdown_wakes_idle_worker_and_drops_resources_before_ready() {
        struct Resource(std::sync::mpsc::SyncSender<()>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }
        let (started, ready) = sync_channel(1);
        let (released, freed) = sync_channel(1);
        let worker = Worker::spawn(move |stop| {
            let _resource = Resource(released);
            started.send(()).unwrap();
            while stop.wait(Duration::from_secs(60)) {}
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let shutdown = Shutdown::new(vec![worker]);
        let workers = shutdown.begin().unwrap();
        assert!(!shutdown.ready());
        assert!(shutdown.begin().is_none());
        stop_workers(workers).unwrap();
        freed.try_recv().unwrap();
        shutdown.finish(true);
        assert!(shutdown.ready());
    }

    #[test]
    fn failed_worker_does_not_skip_other_cleanup_or_allow_exit() {
        let (released, freed) = sync_channel(1);
        let failing = Worker::spawn(|_| panic!("synthetic worker failure"));
        let other = Worker::spawn(move |stop| {
            while stop.wait(Duration::from_secs(60)) {}
            released.send(()).unwrap();
        });
        let shutdown = Shutdown::new(vec![failing, other]);
        assert!(stop_workers(shutdown.begin().unwrap()).is_err());
        freed.try_recv().unwrap();
        shutdown.finish(false);
        assert!(!shutdown.ready());
        assert!(shutdown.begin().is_none());
    }
}
