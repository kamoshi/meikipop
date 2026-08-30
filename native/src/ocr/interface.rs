use std::error::Error;

use opencv::core::Mat;
use pyo3::prelude::*;

/// Abstract interface for an OCR provider.
///
/// Any type that implements this interface can be used by the application's
/// OcrProcessor. This allows for easily swapping out different OCR backends.
pub trait OcrProvider: Send {
    /// A user-friendly name for this provider.
    fn name(&self) -> &'static str;

    /// Performs OCR on the given image.
    ///
    /// # Arguments
    ///
    /// * `image` - An OpenCV image to perform OCR on.
    ///
    /// # Returns
    ///
    /// A list of Paragraph objects found in the image, or an error if one
    /// occurred. Returns an empty list if no text is found.
    fn scan(&mut self, image: &Mat) -> Result<Vec<Paragraph>, Box<dyn Error>>;
}

/// A normalized bounding box. All coordinates are floats between 0.0 and 1.0.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundingBox {
    pub center_x: f64,
    pub center_y: f64,
    pub width: f64,
    pub height: f64,
}

impl BoundingBox {
    pub fn new(cx: f64, cy: f64, w: f64, h: f64) -> Self {
        Self {
            center_x: cx,
            center_y: cy,
            width: w,
            height: h,
        }
    }
}

/// Represents a single word recognized by the OCR.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub separator: String,
    pub r#box: BoundingBox,
}

/// Represents a block of text, composed of words.
#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub full_text: String,
    pub words: Vec<Word>,
    pub r#box: BoundingBox,
    pub is_vertical: bool,
}

impl Paragraph {
    pub fn new(r#box: BoundingBox, is_vertical: bool) -> Self {
        Self {
            full_text: String::new(),
            words: Vec::new(),
            r#box,
            is_vertical,
        }
    }
}

pub(crate) fn bounding_box_from_python(value: &Bound<'_, PyAny>) -> PyResult<BoundingBox> {
    Ok(BoundingBox {
        center_x: value.getattr("center_x")?.extract()?,
        center_y: value.getattr("center_y")?.extract()?,
        width: value.getattr("width")?.extract()?,
        height: value.getattr("height")?.extract()?,
    })
}

fn word_from_python(value: &Bound<'_, PyAny>) -> PyResult<Word> {
    Ok(Word {
        text: value.getattr("text")?.extract()?,
        separator: value.getattr("separator")?.extract()?,
        r#box: bounding_box_from_python(&value.getattr("box")?)?,
    })
}

pub(crate) fn paragraph_from_python(value: &Bound<'_, PyAny>) -> PyResult<Paragraph> {
    let mut words = Vec::new();
    for word in value.getattr("words")?.try_iter()? {
        words.push(word_from_python(&word?)?);
    }

    Ok(Paragraph {
        full_text: value.getattr("full_text")?.extract()?,
        words,
        r#box: bounding_box_from_python(&value.getattr("box")?)?,
        is_vertical: value.getattr("is_vertical")?.extract()?,
    })
}
