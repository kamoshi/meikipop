mod wayland;
mod x11;

use std::error::Error;

use super::DesktopProviders;
use wayland::WaylandFrameProvider;

pub(super) fn create_desktop_providers(
    screencast_token: String,
) -> Result<DesktopProviders, Box<dyn Error>> {
    let frames = WaylandFrameProvider::new(screencast_token)?;
    let pointer = x11::LinuxPointerProvider::new(frames.pointer_provider());

    Ok(DesktopProviders {
        frames: Box::new(frames),
        pointer: Box::new(pointer),
    })
}
