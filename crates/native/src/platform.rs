use std::error::Error;

use crate::input::interface::PointerProvider;
use crate::screenshot::interface::DisplayDescriptor;
use crate::screenshot::interface::FrameProvider;

pub struct DesktopProviders {
    pub frames: Box<dyn FrameProvider>,
    pub pointer: Box<dyn PointerProvider>,
}

#[cfg(target_os = "macos")]
pub fn discover_displays() -> Result<Vec<DisplayDescriptor>, Box<dyn Error>> {
    crate::screenshot::macos::discover_displays()
}

#[cfg(not(target_os = "macos"))]
pub fn discover_displays() -> Result<Vec<DisplayDescriptor>, Box<dyn Error>> {
    Err("Display discovery through this FFI is only supported on macOS".into())
}

#[cfg(target_os = "linux")]
pub fn create_desktop_providers(
    screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    use crate::input::x11::X11PointerProvider;
    use crate::screenshot::wayland_mss_shim::MssWaylandShim;

    Ok(DesktopProviders {
        frames: Box::new(MssWaylandShim::new(screencast_token)?),
        pointer: Box::new(X11PointerProvider::new()?),
    })
}

#[cfg(target_os = "macos")]
pub fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    use crate::input::macos::CoreGraphicsPointerProvider;
    use crate::screenshot::macos::ScreenCaptureKitFrameProvider;

    Ok(DesktopProviders {
        frames: Box::new(ScreenCaptureKitFrameProvider::new()?),
        pointer: Box::new(CoreGraphicsPointerProvider::new()),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    Err("No desktop providers are implemented for this platform".into())
}
