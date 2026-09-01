use std::error::Error;

pub type RgbImage = image::RgbImage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub top: i32,
    pub left: i32,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    pub raw: Vec<u8>,
    pub width: usize,
    pub height: usize,
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
    fn monitors(&mut self) -> Result<Vec<Monitor>, Box<dyn Error>>;
    fn frame(&mut self, monitor: &Monitor) -> Result<Screenshot, Box<dyn Error>>;
}
