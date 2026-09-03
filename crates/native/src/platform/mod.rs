use self::interface::{FrameProvider, PointerProvider};

pub mod interface;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub struct DesktopProviders {
    pub frames: Box<dyn FrameProvider>,
    pub pointer: Box<dyn PointerProvider>,
}

#[cfg(target_os = "linux")]
pub fn create_desktop_providers(
    screencast_token: String,
) -> Result<DesktopProviders, Box<dyn std::error::Error>> {
    linux::create_desktop_providers(screencast_token)
}

#[cfg(target_os = "macos")]
pub fn create_desktop_providers(
    screencast_token: String,
) -> Result<DesktopProviders, Box<dyn std::error::Error>> {
    macos::create_desktop_providers(screencast_token)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn create_desktop_providers(
    _screencast_token: String,
) -> Result<DesktopProviders, Box<dyn std::error::Error>> {
    Err("No desktop providers are implemented for this platform".into())
}
