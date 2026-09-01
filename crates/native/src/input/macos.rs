use std::error::Error;

use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::input::interface::PointerProvider;

pub struct CoreGraphicsPointerProvider;

impl CoreGraphicsPointerProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CoreGraphicsPointerProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PointerProvider for CoreGraphicsPointerProvider {
    fn position(&mut self) -> Result<(i32, i32), Box<dyn Error>> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Could not create a Core Graphics event source")?;
        let event = CGEvent::new(source).map_err(|_| "Could not create a Core Graphics event")?;
        let point = event.location();
        Ok((point.x.round() as i32, point.y.round() as i32))
    }
}
