//! Adapters implement these boundaries in later milestones; no fake production success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    Microphone,
    System,
}

pub struct CaptureRequest {
    pub meeting_id: String,
    pub sources: Vec<AudioSource>,
}

#[derive(Debug)]
pub enum AdapterError {
    NotImplemented,
    PermissionDenied,
    Unavailable(String),
}

pub trait CaptureAdapter {
    /// Atomically starts every requested source, or rolls back all started sources.
    fn start(&mut self, request: CaptureRequest) -> Result<(), AdapterError>;
    /// Returns after callbacks stop and pending frames are handed off for persistence.
    fn stop(&mut self) -> Result<(), AdapterError>;
}

pub trait AsrAdapter {
    fn load_model(&mut self, path: &std::path::Path) -> Result<(), AdapterError>;
    fn cancel(&mut self);
}
