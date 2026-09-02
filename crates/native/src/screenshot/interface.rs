use std::error::Error;

pub type RgbImage = image::RgbImage;

/// Logical desktop coordinates occupied by the source selected for capture.
///
/// A source may be a display, a window, or another platform-defined region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureGeometry {
    pub top: i32,
    pub left: i32,
    pub width: usize,
    pub height: usize,
}

impl CaptureGeometry {
    pub fn contains(&self, point: (i32, i32)) -> bool {
        let right = self
            .left
            .saturating_add_unsigned(self.width.min(u32::MAX as usize) as u32);
        let bottom = self
            .top
            .saturating_add_unsigned(self.height.min(u32::MAX as usize) as u32);
        point.0 >= self.left && point.0 < right && point.1 >= self.top && point.1 < bottom
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    pub raw: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// A frame and the logical desktop geometry it represented when captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedFrame {
    pub sequence: u64,
    pub screenshot: Screenshot,
    pub geometry: CaptureGeometry,
}

impl Screenshot {
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Matches Image.frombytes("RGB", screenshot.size, screenshot.bgra,
    /// "raw", "BGRX") from upstream.
    pub fn to_rgb(&self) -> Result<RgbImage, Box<dyn Error>> {
        let expected_len = self
            .width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("screenshot dimensions are too large")?;
        if self.raw.len() != expected_len {
            return Err(format!(
                "BGRA screenshot has {} bytes, expected {expected_len}",
                self.raw.len()
            )
            .into());
        }

        let mut rgb = Vec::with_capacity(self.width * self.height * 3);
        for pixel in self.raw.chunks_exact(4) {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        let width = u32::try_from(self.width).map_err(|_| "screenshot width exceeds u32")?;
        let height = u32::try_from(self.height).map_err(|_| "screenshot height exceeds u32")?;
        RgbImage::from_raw(width, height, rgb)
            .ok_or_else(|| "failed to construct RGB screenshot".into())
    }
}

pub trait FrameProvider: Send {
    /// Returns the current logical geometry of the system-selected source.
    fn capture_geometry(&mut self) -> Result<CaptureGeometry, Box<dyn Error>>;

    /// Returns a frame together with the geometry that produced it.
    /// Implementations must not pair a cached frame with newer geometry.
    fn capture_frame(&mut self) -> Result<CapturedFrame, Box<dyn Error>>;
}
