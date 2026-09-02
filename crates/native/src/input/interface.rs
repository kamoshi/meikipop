use std::error::Error;

use crate::screenshot::interface::CaptureGeometry;

/// Pointer and selected-source state sampled as one logical observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerSnapshot {
    pub position: (i32, i32),
    pub capture_geometry: Option<CaptureGeometry>,
    pub target_available: bool,
}

pub trait PointerProvider: Send {
    fn snapshot(&mut self) -> Result<PointerSnapshot, Box<dyn Error>>;
}
