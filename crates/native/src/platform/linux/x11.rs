use std::error::Error;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::wayland::WaylandPointerProvider;
use crate::platform::interface::{PointerProvider, PointerSnapshot};

fn preferred_position(
    pipewire: Option<((i32, i32), u64)>,
    last_pipewire_sequence: Option<u64>,
    x11: impl FnOnce() -> Result<(i32, i32), Box<dyn Error>>,
) -> Result<(i32, i32), Box<dyn Error>> {
    if let Some((position, sequence)) = pipewire {
        if Some(sequence) != last_pipewire_sequence {
            return Ok(position);
        }
        return Ok(x11().unwrap_or(position));
    }

    x11().map_err(|error| {
        format!("PipeWire has not provided cursor coordinates and X11 fallback failed: {error}")
            .into()
    })
}

/// Uses cursor metadata supplied by PipeWire and falls back to X11 (including
/// XWayland) coordinates. Capture geometry always comes from PipeWire so the
/// position and captured frame share desktop coordinates.
pub struct LinuxPointerProvider {
    x11: Option<RustConnection>,
    pipewire: WaylandPointerProvider,
    last_pipewire_sequence: Option<u64>,
}

impl LinuxPointerProvider {
    pub fn new(pipewire: WaylandPointerProvider) -> Self {
        let x11 = match x11rb::connect(None) {
            Ok((connection, _)) => {
                log::info!("Using PipeWire cursor coordinates with X11 fallback");
                Some(connection)
            }
            Err(error) => {
                log::info!("X11 cursor coordinates unavailable; using PipeWire: {error}");
                None
            }
        };
        Self {
            x11,
            pipewire,
            last_pipewire_sequence: None,
        }
    }

    fn x11_position(&self) -> Result<(i32, i32), Box<dyn Error>> {
        let connection = self.x11.as_ref().ok_or("X11 is unavailable")?;
        for screen in &connection.setup().roots {
            let reply = connection.query_pointer(screen.root)?.reply()?;
            if reply.same_screen {
                return Ok((i32::from(reply.root_x), i32::from(reply.root_y)));
            }
        }
        Err("X11 did not report the pointer on any screen".into())
    }
}

impl PointerProvider for LinuxPointerProvider {
    fn snapshot(&mut self) -> Result<PointerSnapshot, Box<dyn Error>> {
        let pipewire = self.pipewire.pipewire_observation();
        let position = preferred_position(pipewire, self.last_pipewire_sequence, || {
            self.x11_position()
        })?;
        if let Some((_, sequence)) = pipewire {
            self.last_pipewire_sequence = Some(sequence);
        }
        self.pipewire
            .snapshot_with_position_override(Some(position))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::preferred_position;

    #[test]
    fn pipewire_position_is_preferred_without_querying_x11() {
        let queried = Cell::new(false);
        let position = preferred_position(Some(((10, 20), 2)), Some(1), || {
            queried.set(true);
            Ok((30, 40))
        })
        .unwrap();

        assert_eq!(position, (10, 20));
        assert!(!queried.get());
    }

    #[test]
    fn x11_position_is_used_when_pipewire_has_none() {
        let position = preferred_position(None, None, || Ok((30, 40))).unwrap();
        assert_eq!(position, (30, 40));
    }

    #[test]
    fn x11_position_is_used_when_pipewire_observation_stalls() {
        let position = preferred_position(Some(((10, 20), 2)), Some(2), || Ok((30, 40))).unwrap();
        assert_eq!(position, (30, 40));
    }

    #[test]
    fn cached_pipewire_position_survives_an_x11_failure() {
        let position = preferred_position(Some(((10, 20), 2)), Some(2), || {
            Err("X11 query failed".into())
        })
        .unwrap();
        assert_eq!(position, (10, 20));
    }
}
