use std::sync::{Arc, Mutex};

use crate::screenshot::interface::CaptureGeometry;

pub(crate) mod window_server;

#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureSourceSnapshot {
    pub(crate) generation: u64,
    pub(crate) geometry: Option<CaptureGeometry>,
    pub(crate) window_id: Option<u32>,
    /// Whether the selected source is currently producing usable frames.
    ///
    /// A source can become temporarily unavailable without changing identity;
    /// for example, ScreenCaptureKit may suspend a stream without ending it.
    pub(crate) available: bool,
}

pub(crate) type SharedCaptureSource = Arc<Mutex<CaptureSourceSnapshot>>;

pub(crate) fn new_shared_capture_source() -> SharedCaptureSource {
    Arc::new(Mutex::new(CaptureSourceSnapshot::default()))
}
