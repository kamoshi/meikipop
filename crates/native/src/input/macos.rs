use std::error::Error;
use std::time::{Duration, Instant};

use objc2_core_graphics::{CGEvent, CGEventSource, CGEventSourceStateID};

use crate::input::interface::{PointerProvider, PointerSnapshot};
use crate::platform::macos::window_server::{WindowListSnapshot, query_window_geometry};

const WINDOW_LIST_CACHE_DURATION: Duration = Duration::from_millis(50);

pub struct CoreGraphicsPointerProvider {
    window_id: Option<u32>,
    cached_window_list: Option<WindowListSnapshot>,
    last_window_list_check: Option<Instant>,
}

impl CoreGraphicsPointerProvider {
    pub fn new() -> Self {
        Self {
            window_id: None,
            cached_window_list: None,
            last_window_list_check: None,
        }
    }

    pub fn new_with_window_id(window_id: Option<u32>) -> Self {
        Self {
            window_id,
            cached_window_list: None,
            last_window_list_check: None,
        }
    }

    fn pointer_position() -> Result<(i32, i32), Box<dyn Error>> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or("Could not create a Core Graphics event source")?;
        let event = CGEvent::new(Some(&source)).ok_or("Could not create a Core Graphics event")?;
        let point = CGEvent::location(Some(&event));
        Ok((point.x.round() as i32, point.y.round() as i32))
    }

    fn refresh_window_list_if_needed(&mut self) {
        let now = Instant::now();
        let cache_is_fresh = self
            .last_window_list_check
            .is_some_and(|last| now.duration_since(last) < WINDOW_LIST_CACHE_DURATION);
        if cache_is_fresh {
            return;
        }

        self.last_window_list_check = Some(now);
        if let Some(snapshot) = WindowListSnapshot::on_screen() {
            self.cached_window_list = Some(snapshot);
        }
    }
}

impl Default for CoreGraphicsPointerProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PointerProvider for CoreGraphicsPointerProvider {
    fn snapshot(&mut self) -> Result<PointerSnapshot, Box<dyn Error>> {
        let position = Self::pointer_position()?;
        let Some(window_id) = self.window_id else {
            return Ok(PointerSnapshot {
                position,
                capture_geometry: None,
                target_available: true,
            });
        };

        self.refresh_window_list_if_needed();
        let capture_geometry = self
            .cached_window_list
            .as_ref()
            .and_then(|snapshot| snapshot.geometry(window_id))
            .or_else(|| query_window_geometry(window_id));
        let target_available = self
            .cached_window_list
            .as_ref()
            .is_some_and(|snapshot| snapshot.target_is_frontmost_at_point(window_id, position));

        Ok(PointerSnapshot {
            position,
            capture_geometry,
            target_available,
        })
    }
}
