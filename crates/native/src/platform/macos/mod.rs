use std::sync::{Arc, Mutex};

use crate::platform::interface::CaptureGeometry;

mod input;
mod screenshot;
mod window_server;

use super::DesktopProviders;
use input::CoreGraphicsPointerProvider;
use screenshot::ScreenCaptureKitFrameProvider;

pub(super) fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn std::error::Error>> {
    let source = new_shared_capture_source();
    let frames = ScreenCaptureKitFrameProvider::new(source.clone())?;
    let pointer = CoreGraphicsPointerProvider::new_with_source(source);

    Ok(DesktopProviders {
        frames: Box::new(frames),
        pointer: Box::new(pointer),
    })
}

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
