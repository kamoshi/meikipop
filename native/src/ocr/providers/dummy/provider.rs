// meikipop/ocr/providers/dummy/provider.rs

// The "contract" classes that a new provider MUST use for its return value.
use crate::ocr::interface::{BoundingBox, OcrProvider, Paragraph, Word};

/// A template for creating new OCR providers.
///
/// This class demonstrates the required structure and data transformations.
/// Developers can copy this file to start their own provider implementation.
/// When this provider is selected, it returns a fixed set of Japanese text
/// to allow for testing of the popup window without a real OCR backend.
pub struct DummyProvider;

impl DummyProvider {
    // The NAME is displayed in the settings and tray menu. Make it unique and descriptive.
    pub const NAME: &'static str = "Dummy OCR (Developer Template)";

    /// Performs OCR on the given image.
    ///
    /// This method must be implemented. Its main job is to:
    /// 1. Get OCR data from an external source (library, API, etc.).
    /// 2. Convert the proprietary data format into meikipop's standard format
    ///    (a list of Paragraph objects with normalized coordinates).
    /// 3. Return the list of Paragraphs.
    pub fn scan(img_width: usize, img_height: usize) -> Result<Vec<Paragraph>, &'static str> {
        log::info!(
            "{} received an image of size ({img_width}, {img_height}). Returning mock data.",
            Self::NAME
        );

        // --- Pro-Tip: Let an AI do the heavy lifting! ---
        // You can provide this file, the contents of `src/ocr/interface.py`,
        // and a sample JSON/text output from your chosen OCR engine to a
        // Large Language Model (like GPT-4, Claude, etc.) and ask it to
        // write the adapter code for you. This can get you 90% of the way there.

        // --- 1. OBTAIN OCR DATA ---
        // In a real provider, you would call your OCR engine here.
        // This could be a Python library, a REST API, or a command-line tool.
        // We will use a hardcoded, mock result for demonstration.

        // Example: Calling a Python library (if you had one)
        // import my_cool_ocr_library
        // client = my_cool_ocr_library.Client(api_key="...")
        // raw_ocr_results = client.recognize(image)

        // Example: Making a REST API call
        // import requests
        // _, buffer = cv2.imencode('.jpg', image)
        // response = requests.post("https://api.myocr.com/v1/scan", files={'image': buffer.tobytes()})
        // raw_ocr_results = response.json()

        // For this template, we'll define a mock result that simulates the output
        // from a fictional OCR engine. This engine gives us pixel coordinates.
        let mock_ocr_result = [
            MockLine {
                text: "これは横書きテキストです",
                bbox: PixelBox::new(100, 150, 400, 40), // A horizontal bounding box
                words: &[
                    MockWord::new("これは", 100, 150, 90, 40),
                    MockWord::new("横書き", 200, 150, 90, 40),
                    MockWord::new("テキストです", 300, 150, 200, 40),
                ],
            },
            MockLine {
                text: "縦書き",
                bbox: PixelBox::new(600, 200, 50, 300), // A vertical bounding box
                words: &[
                    // NOTE: A Word can contain multiple characters OR a single character.
                    // meikipop's hit-scanning works well with both approaches.
                    // Providing single-character boxes can lead to more precise lookups.
                    MockWord::new("縦", 600, 200, 50, 95),
                    MockWord::new("書", 600, 305, 50, 95),
                    MockWord::new("き", 600, 405, 50, 95),
                ],
            },
        ];

        // --- 2. PROCESS AND TRANSFORM THE DATA ---
        // This is the most important part. You must convert the raw results from your
        // OCR engine into the format meikipop understands (`List[Paragraph]`).
        let mut paragraphs = Vec::new();
        if img_width == 0 || img_height == 0 {
            return Err("Invalid image dimensions received.");
        }

        for ocr_line in mock_ocr_result {
            // The typed mock data guarantees that line text and bbox data are present.

            // meikipop requires NORMALIZED coordinates (from 0.0 to 1.0).
            // Here we convert the absolute pixel BBox to a normalized BoundingBox.
            // Our mock 'bbox' has top-left corner (x,y) and width/height (w,h).
            let line_box = ocr_line.bbox.normalized(img_width, img_height);

            // For Japanese, it's crucial to know the writing direction.
            // If your OCR engine doesn't provide this, you can infer it from
            // the bounding box's aspect ratio.
            let is_vertical = ocr_line.bbox.h > ocr_line.bbox.w;

            // Now, process the words within the line.
            let mut words_in_para = Vec::new();
            for word_data in ocr_line.words {
                // The typed mock data guarantees that word text and bbox data are present.

                // Convert word coordinates, just like we did for the paragraph.
                let word_box = word_data.bbox.normalized(img_width, img_height);

                // The separator is important for reconstructing text. In Japanese, it's often empty.
                let separator = String::new();
                words_in_para.push(Word {
                    text: word_data.text.to_owned(),
                    separator,
                    r#box: word_box,
                });
            }

            // If your OCR only provides line-level data, you might need to
            // create a single `Word` object for the entire line text.
            if words_in_para.is_empty() {
                words_in_para.push(Word {
                    text: ocr_line.text.to_owned(),
                    separator: String::new(),
                    r#box: line_box.clone(),
                });
            }

            // Finally, assemble the Paragraph object.
            paragraphs.push(Paragraph {
                full_text: ocr_line.text.to_owned(),
                words: words_in_para,
                r#box: line_box,
                is_vertical,
            });
        }

        // --- 3. RETURN THE RESULT ---
        // The final result must be a list of Paragraph objects.
        // If no text was found, return an empty list `[]`.
        // If a critical error occurred, return `None`.
        Ok(paragraphs)
    }
}

impl OcrProvider for DummyProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn scan(
        &mut self,
        image: &crate::ocr::interface::Mat,
    ) -> Result<Vec<Paragraph>, Box<dyn std::error::Error>> {
        DummyProvider::scan(image.width() as usize, image.height() as usize)
            .map_err(|error| error.into())
    }
}

#[derive(Clone, Copy)]
struct PixelBox {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl PixelBox {
    const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    fn normalized(self, img_width: usize, img_height: usize) -> BoundingBox {
        BoundingBox {
            center_x: (self.x as f64 + self.w as f64 / 2.0) / img_width as f64,
            center_y: (self.y as f64 + self.h as f64 / 2.0) / img_height as f64,
            width: self.w as f64 / img_width as f64,
            height: self.h as f64 / img_height as f64,
        }
    }
}

struct MockWord {
    text: &'static str,
    bbox: PixelBox,
}

impl MockWord {
    const fn new(text: &'static str, x: usize, y: usize, w: usize, h: usize) -> Self {
        Self {
            text,
            bbox: PixelBox::new(x, y, w, h),
        }
    }
}

struct MockLine<'a> {
    text: &'static str,
    bbox: PixelBox,
    words: &'a [MockWord],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_upstream_mock_paragraphs() {
        let paragraphs = DummyProvider::scan(800, 600).unwrap();

        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].full_text, "これは横書きテキストです");
        assert_eq!(paragraphs[0].words.len(), 3);
        assert!(!paragraphs[0].is_vertical);
        assert_eq!(paragraphs[1].full_text, "縦書き");
        assert_eq!(paragraphs[1].words.len(), 3);
        assert!(paragraphs[1].is_vertical);
    }

    #[test]
    fn rejects_zero_sized_images() {
        assert!(DummyProvider::scan(0, 600).is_err());
        assert!(DummyProvider::scan(800, 0).is_err());
    }

    #[test]
    fn can_be_used_as_an_ocr_provider_trait_object() {
        use crate::ocr::interface::Mat;

        let mut provider: Box<dyn OcrProvider> = Box::new(DummyProvider);
        let image = Mat::new(800, 600);

        assert_eq!(provider.name(), DummyProvider::NAME);
        assert_eq!(provider.scan(&image).unwrap().len(), 2);
    }
}
