use std::error::Error;
use std::time::{Duration, Instant};

use objc2_core_graphics::{CGEvent, CGEventSource, CGEventSourceStateID};

use super::SharedCaptureSource;
use super::window_server::WindowListSnapshot;
use crate::platform::interface::{PointerProvider, PointerSnapshot};

const WINDOW_LIST_CACHE_DURATION: Duration = Duration::from_millis(50);

pub struct CoreGraphicsPointerProvider {
    source: Option<SharedCaptureSource>,
    cached_window_list: Option<WindowListSnapshot>,
    last_window_list_check: Option<Instant>,
}

impl CoreGraphicsPointerProvider {
    pub fn new() -> Self {
        Self {
            source: None,
            cached_window_list: None,
            last_window_list_check: None,
        }
    }

    pub(crate) fn new_with_source(source: SharedCaptureSource) -> Self {
        Self {
            source: Some(source),
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
        // Occlusion is a safety check: never retain stale z-order information
        // when WindowServer cannot provide a fresh observation.
        self.cached_window_list = WindowListSnapshot::on_screen();
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
        let Some(source) = self.source.clone() else {
            return Ok(PointerSnapshot {
                position,
                capture_geometry: None,
                source_generation: 0,
                target_available: true,
            });
        };

        let selected = source
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if !selected.available {
            return Ok(PointerSnapshot {
                position,
                capture_geometry: None,
                source_generation: selected.generation,
                target_available: false,
            });
        }

        let Some(window_id) = selected.window_id else {
            return Ok(PointerSnapshot {
                position,
                capture_geometry: selected.geometry,
                source_generation: selected.generation,
                target_available: true,
            });
        };

        self.refresh_window_list_if_needed();
        let target_available = self
            .cached_window_list
            .as_ref()
            .is_some_and(|snapshot| snapshot.target_is_frontmost_at_point(window_id, position));

        Ok(PointerSnapshot {
            position,
            // The captured frame owns the coordinate system used by OCR.
            // WindowServer bounds can include different border/shadow extents,
            // so they are used only for z-order and occlusion checks here.
            capture_geometry: selected.geometry,
            source_generation: selected.generation,
            target_available,
        })
    }
}
