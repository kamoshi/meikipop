use std::error::Error;

use crate::input::interface::PointerProvider;
use crate::screenshot::interface::FrameProvider;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

pub struct DesktopProviders {
    pub frames: Box<dyn FrameProvider>,
    pub pointer: Box<dyn PointerProvider>,
}

#[cfg(target_os = "linux")]
pub fn create_desktop_providers(
    screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    use crate::screenshot::wayland::WaylandFrameProvider;

    let frames = WaylandFrameProvider::new(screencast_token)?;
    let pointer = frames.pointer_provider();
    Ok(DesktopProviders {
        frames: Box::new(frames),
        pointer: Box::new(pointer),
    })
}

#[cfg(target_os = "macos")]
pub fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    use crate::input::macos::CoreGraphicsPointerProvider;
    use crate::screenshot::macos::ScreenCaptureKitFrameProvider;

    let frames = ScreenCaptureKitFrameProvider::new()?;
    let window_id = frames.window_id();
    let pointer = CoreGraphicsPointerProvider::new_with_window_id(window_id);
    Ok(DesktopProviders {
        frames: Box::new(frames),
        pointer: Box::new(pointer),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    Err("No desktop providers are implemented for this platform".into())
}
