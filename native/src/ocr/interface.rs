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
