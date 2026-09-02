// meikipop/ocr/providers/meikiocr/provider.rs

// Import the MeikiOCR library
use super::ocr::{MeikiOcr, OcrResult};

// Import the "contract" classes from your application's interface
use crate::ocr::interface::{BoundingBox, Mat, OcrContext, OcrProvider, Paragraph, Word};
use crate::ocr::providers::postprocessing::group_lines_into_paragraphs;

// --- pipeline configuration ---
// These thresholds are passed to the library's run_ocr method.
const DET_CONFIDENCE_THRESHOLD: f32 = 0.5;
const REC_CONFIDENCE_THRESHOLD: f32 = 0.1;

pub struct MeikiOcrProvider {
    ocr_client: MeikiOcr,
}

impl MeikiOcrProvider {
    /// Initializes the provider by creating an instance of the MeikiOCR client.
    /// The library handles the model downloading and session management internally.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        log::info!("initializing meikiocr (local) provider...");
        let ocr_client = MeikiOcr::new(None, 8)?;
        log::info!(
            "meikiocr (local) initialized successfully, running on: {}",
            ocr_client.active_provider
        );
        Ok(Self { ocr_client })
    }

    /// Performs OCR on the given image by calling the meikiocr library.
    pub fn scan(&mut self, image: &Mat) -> Result<Vec<Paragraph>, Box<dyn std::error::Error>> {
        let img_width = i32::try_from(image.width())?;
        let img_height = i32::try_from(image.height())?;

        if img_width == 0 || img_height == 0 {
            return Err("invalid image dimensions received".into());
        }

        // --- 1. Run the entire OCR pipeline with a single library call ---
        let ocr_results = self.ocr_client.run_ocr(
            image,
            DET_CONFIDENCE_THRESHOLD,
            REC_CONFIDENCE_THRESHOLD,
            0.2,
        )?;

        // --- 2. Transform the library's output to MeikiPop's format ---
        Ok(Self::_to_meikipop_paragraphs(
            ocr_results,
            img_width,
            img_height,
        ))
    }

    /// Converts an [x1, y1, x2, y2] pixel bbox to a normalized meikipop BoundingBox.
    fn _to_normalized_bbox(bbox_pixels: [i32; 4], img_width: i32, img_height: i32) -> BoundingBox {
        let [x1, y1, x2, y2] = bbox_pixels;
        let (box_w, box_h) = (x2 - x1, y2 - y1);

        let center_x = (x1 as f64 + box_w as f64 / 2.0) / img_width as f64;
        let center_y = (y1 as f64 + box_h as f64 / 2.0) / img_height as f64;
        let norm_w = box_w as f64 / img_width as f64;
        let norm_h = box_h as f64 / img_height as f64;

        BoundingBox::new(center_x, center_y, norm_w, norm_h)
    }

    /// Converts the final meikiocr result list into meikipop's Paragraph format.
    fn _to_meikipop_paragraphs(
        ocr_results: Vec<OcrResult>,
        img_width: i32,
        img_height: i32,
    ) -> Vec<Paragraph> {
        let mut lines = Vec::new();
        for line_result in ocr_results {
            let full_text = line_result.text.trim().to_owned();
            let chars = line_result.chars;
            if full_text.is_empty()
                || chars.is_empty()
                || !full_text.chars().any(is_japanese_character)
            {
                continue;
            }

            // create word objects for each character (best for precise lookups).
            let mut words_in_line = Vec::new();
            for char_info in &chars {
                let char_box = Self::_to_normalized_bbox(char_info.bbox, img_width, img_height);
                words_in_line.push(Word {
                    text: char_info.character.to_string(),
                    separator: String::new(),
                    r#box: char_box,
                });
            }

            // meikiocr doesn't provide a line-level box, so we must compute it
            // by finding the union of all character boxes in the line.
            let min_x = chars
                .iter()
                .map(|character| character.bbox[0])
                .min()
                .unwrap();
            let min_y = chars
                .iter()
                .map(|character| character.bbox[1])
                .min()
                .unwrap();
            let max_x = chars
                .iter()
                .map(|character| character.bbox[2])
                .max()
                .unwrap();
            let max_y = chars
                .iter()
                .map(|character| character.bbox[3])
                .max()
                .unwrap();
            let line_box =
                Self::_to_normalized_bbox([min_x, min_y, max_x, max_y], img_width, img_height);

            let is_vertical = line_box.width * 1.5 < line_box.height;
            let line = Paragraph {
                full_text,
                words: words_in_line,
                r#box: line_box,
                is_vertical,
            };
            lines.push(line);
        }

        group_lines_into_paragraphs(lines)
    }
}

impl OcrProvider for MeikiOcrProvider {
    fn name(&self) -> &'static str {
        "meikiocr (local)"
    }

    fn scan(
        &mut self,
        image: &Mat,
        _context: OcrContext,
    ) -> Result<Vec<Paragraph>, Box<dyn std::error::Error>> {
        MeikiOcrProvider::scan(self, image)
    }
}

fn is_japanese_character(character: char) -> bool {
    matches!(
        character,
        '\u{3040}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{4e00}'..='\u{9faf}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::providers::meikiocr::ocr::RecognizedChar;

    #[test]
    fn converts_meikiocr_results_to_normalized_paragraphs() {
        let results = vec![OcrResult {
            text: " 日本語 ".to_owned(),
            chars: vec![RecognizedChar {
                character: '日',
                bbox: [10, 20, 30, 60],
                conf: 0.9,
            }],
            is_vertical: false,
        }];

        let paragraphs = MeikiOcrProvider::_to_meikipop_paragraphs(results, 100, 100);

        assert_eq!(paragraphs.len(), 1);
        assert_eq!(paragraphs[0].full_text, "日本語");
        assert_eq!(paragraphs[0].words[0].text, "日");
        assert!((paragraphs[0].r#box.center_x - 0.2).abs() < f64::EPSILON);
        assert!((paragraphs[0].r#box.center_y - 0.4).abs() < f64::EPSILON);
        assert!((paragraphs[0].r#box.width - 0.2).abs() < f64::EPSILON);
        assert!((paragraphs[0].r#box.height - 0.4).abs() < f64::EPSILON);
        assert!(paragraphs[0].is_vertical);
    }

    #[test]
    fn filters_non_japanese_results() {
        let results = vec![OcrResult {
            text: "hello".to_owned(),
            chars: vec![RecognizedChar {
                character: 'h',
                bbox: [0, 0, 10, 10],
                conf: 0.9,
            }],
            is_vertical: false,
        }];

        assert!(MeikiOcrProvider::_to_meikipop_paragraphs(results, 100, 100).is_empty());
    }
}
