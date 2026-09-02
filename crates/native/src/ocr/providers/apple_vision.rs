use std::error::Error;
use std::mem;
use std::ptr;
use std::time::Instant;

use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::{CFData, CFRetained, CGRect};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_foundation::{NSArray, NSDictionary, NSRange, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::ocr::interface::{BoundingBox, Mat, OcrProvider, Paragraph, Word};
use crate::ocr::providers::postprocessing::contains_japanese_text;

/// Apple's documented default. Keeping it explicit makes the cost/accuracy
/// tradeoff visible without asking a full-screen Accurate request to inspect
/// near-pixel-sized text candidates. A configurable threshold can move into
/// generic scan policy together with the future ROI controls.
const MINIMUM_TEXT_HEIGHT: f32 = 1.0 / 32.0;
const BOX_EQUALITY_EPSILON: f64 = 1e-7;

pub struct AppleVisionOcrProvider;

fn cg_image_from_rgb(image: &Mat) -> Result<CFRetained<CGImage>, Box<dyn Error>> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return Err("cannot recognize an empty image".into());
    }

    let rgb_bytes_per_row = width
        .checked_mul(3)
        .ok_or("Apple Vision image row is too large")?;
    let expected_len = rgb_bytes_per_row
        .checked_mul(height)
        .ok_or("Apple Vision image is too large")?;
    if image.as_raw().len() != expected_len {
        return Err("Apple Vision received invalid RGB image storage".into());
    }

    // Core Graphics' broadly supported RGBX layout requires four-byte pixels.
    // CFData then owns the converted buffer, avoiding the former BMP encode and
    // Vision decode round trip.
    let mut rgbx = Vec::with_capacity(
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("Apple Vision image is too large")?,
    );
    for pixel in image.as_raw().chunks_exact(3) {
        rgbx.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0]);
    }
    let bytes_per_row = width
        .checked_mul(4)
        .ok_or("Apple Vision image row is too large")?;
    let data = CFData::from_bytes(&rgbx);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or("failed to create an Apple Vision data provider")?;
    let color_space =
        CGColorSpace::new_device_rgb().ok_or("failed to create an RGB color space")?;
    let bitmap_info = CGBitmapInfo(CGImageAlphaInfo::NoneSkipLast.0);

    // SAFETY: `data` owns exactly `height * bytes_per_row` bytes of packed RGBX,
    // the decode table is null, and the retained CGImage retains its provider.
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            bytes_per_row,
            Some(&color_space),
            bitmap_info,
            Some(&provider),
            ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or_else(|| "failed to create an Apple Vision CGImage".into())
}

fn normalized_box(rect: CGRect) -> BoundingBox {
    let width = rect.size.width;
    let height = rect.size.height;
    let center_x = rect.origin.x + width / 2.0;
    let center_y = 1.0 - (rect.origin.y + height / 2.0);
    BoundingBox::new(center_x, center_y, width, height)
}

fn boxes_are_equal(left: &BoundingBox, right: &BoundingBox) -> bool {
    (left.center_x - right.center_x).abs() <= BOX_EQUALITY_EPSILON
        && (left.center_y - right.center_y).abs() <= BOX_EQUALITY_EPSILON
        && (left.width - right.width).abs() <= BOX_EQUALITY_EPSILON
        && (left.height - right.height).abs() <= BOX_EQUALITY_EPSILON
}

/// Queries each Unicode scalar using its corresponding UTF-16 `NSRange`.
/// Accurate Vision requests return word-precision boxes, so adjacent scalars
/// with the same box are folded back into one pipeline word.
fn words_from_ranges(
    text: &str,
    fallback_box: &BoundingBox,
    mut box_for_range: impl FnMut(NSRange) -> Option<BoundingBox>,
) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut leading_without_box = String::new();
    let mut utf16_offset = 0;

    for character in text.chars() {
        let utf16_len = character.len_utf16();
        let range = NSRange::new(utf16_offset, utf16_len);
        utf16_offset += utf16_len;

        let range_box = box_for_range(range);
        if character.is_whitespace()
            && let Some(word) = words.last_mut()
        {
            word.separator.push(character);
            continue;
        }

        match range_box {
            Some(r#box)
                if words
                    .last()
                    .is_some_and(|word| boxes_are_equal(&word.r#box, &r#box)) =>
            {
                words.last_mut().unwrap().text.push(character);
            }
            Some(r#box) => {
                let mut word_text = mem::take(&mut leading_without_box);
                word_text.push(character);
                words.push(Word {
                    text: word_text,
                    separator: String::new(),
                    r#box,
                });
            }
            None => {
                if let Some(word) = words.last_mut() {
                    word.text.push(character);
                } else {
                    leading_without_box.push(character);
                }
            }
        }
    }

    if words.is_empty() && !leading_without_box.is_empty() {
        words.push(Word {
            text: leading_without_box,
            separator: String::new(),
            r#box: fallback_box.clone(),
        });
    }
    words
}

fn candidate_words(
    candidate: &VNRecognizedText,
    text: &str,
    fallback_box: &BoundingBox,
) -> Vec<Word> {
    words_from_ranges(text, fallback_box, |range| {
        // SAFETY: `words_from_ranges` constructs in-bounds UTF-16 ranges for
        // the exact string returned by this candidate.
        unsafe { candidate.boundingBoxForRange_error(range) }
            .ok()
            .map(|observation| {
                // SAFETY: Vision returned a live rectangle observation.
                normalized_box(unsafe { observation.boundingBox() })
            })
    })
}

fn text_is_vertical(paragraph_box: &BoundingBox, words: &[Word]) -> bool {
    if let (Some(first), Some(last)) = (words.first(), words.last())
        && !boxes_are_equal(&first.r#box, &last.r#box)
    {
        let horizontal_travel = (last.r#box.center_x - first.r#box.center_x).abs();
        let vertical_travel = (last.r#box.center_y - first.r#box.center_y).abs();
        return vertical_travel > horizontal_travel;
    }

    paragraph_box.height > paragraph_box.width
}

fn configure_request(request: &VNRecognizeTextRequest) {
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    request.setAutomaticallyDetectsLanguage(false);
    request.setMinimumTextHeight(MINIMUM_TEXT_HEIGHT);

    let lang_ja = NSString::from_str("ja-JP");
    let langs = NSArray::from_retained_slice(&[lang_ja]);
    request.setRecognitionLanguages(&langs);
}

fn recognize(image: &Mat) -> Result<Vec<Paragraph>, Box<dyn Error>> {
    let start_time = Instant::now();
    let cg_image = cg_image_from_rgb(image)?;
    let image_prep_time = start_time.elapsed();

    let options = NSDictionary::new();
    let alloc = VNImageRequestHandler::alloc();
    // SAFETY: `options` is an empty dictionary, so it satisfies Vision's
    // VNImageOption key/value contract.
    let handler =
        unsafe { VNImageRequestHandler::initWithCGImage_options(alloc, &cg_image, &options) };

    let request = VNRecognizeTextRequest::new();
    configure_request(&request);

    let request_base: Retained<VNRequest> =
        Retained::into_super(Retained::into_super(request.clone()));
    let requests = NSArray::from_retained_slice(&[request_base]);

    let vision_start = Instant::now();
    handler.performRequests_error(&requests)?;
    let vision_time = vision_start.elapsed();

    let mut paragraphs = Vec::new();
    if let Some(results) = request.results() {
        for observation in results.iter() {
            let top_candidates = observation.topCandidates(1);
            let Some(top_candidate) = top_candidates.firstObject() else {
                continue;
            };
            let text = top_candidate.string().to_string();
            if !contains_japanese_text(&text) {
                continue;
            }

            // SAFETY: Vision returned a live recognized-text observation.
            let paragraph_box = normalized_box(unsafe { observation.boundingBox() });
            let words = candidate_words(&top_candidate, &text, &paragraph_box);
            if words.is_empty() {
                continue;
            }
            let is_vertical = text_is_vertical(&paragraph_box, &words);
            paragraphs.push(Paragraph {
                full_text: text,
                words,
                r#box: paragraph_box,
                is_vertical,
            });
        }
    }

    log::info!(
        "Apple Vision OCR total: {:?}, image_prep: {:?}, vision: {:?}, paragraphs: {}",
        start_time.elapsed(),
        image_prep_time,
        vision_time,
        paragraphs.len()
    );

    Ok(paragraphs)
}

impl AppleVisionOcrProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        log::info!("Initialized macOS Apple Vision OCR provider");
        Ok(Self)
    }
}

impl OcrProvider for AppleVisionOcrProvider {
    fn name(&self) -> &'static str {
        "apple_vision (macOS)"
    }

    fn scan(&mut self, image: &Mat) -> Result<Vec<Paragraph>, Box<dyn Error>> {
        autoreleasepool(|_| recognize(image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn creates_a_cg_image_from_rgb_pixels() {
        let source = Mat::from_pixel(2, 3, Rgb([10, 20, 30]));
        let image = cg_image_from_rgb(&source).unwrap();

        assert_eq!(CGImage::width(Some(&image)), 2);
        assert_eq!(CGImage::height(Some(&image)), 3);
        assert_eq!(CGImage::bits_per_pixel(Some(&image)), 32);
        assert_eq!(CGImage::bytes_per_row(Some(&image)), 8);
        let provider = CGImage::data_provider(Some(&image)).unwrap();
        let pixels = CGDataProvider::data(Some(&provider)).unwrap().to_vec();
        assert_eq!(&pixels[..4], &[10, 20, 30, 0]);
    }

    #[test]
    fn configures_accurate_japanese_recognition_with_an_explicit_text_height() {
        autoreleasepool(|_| {
            let request = VNRecognizeTextRequest::new();
            configure_request(&request);

            assert_eq!(
                request.recognitionLevel(),
                VNRequestTextRecognitionLevel::Accurate
            );
            let languages = unsafe { request.recognitionLanguages() };
            assert_eq!(languages.len(), 1);
            assert_eq!(languages.firstObject().unwrap().to_string(), "ja-JP");
            assert_eq!(request.minimumTextHeight(), MINIMUM_TEXT_HEIGHT);
        });
    }

    #[test]
    fn uses_utf16_ranges_and_groups_word_precision_boxes() {
        let fallback = BoundingBox::new(0.5, 0.5, 1.0, 0.2);
        let first = BoundingBox::new(0.2, 0.5, 0.3, 0.2);
        let second = BoundingBox::new(0.7, 0.5, 0.3, 0.2);
        let mut ranges = Vec::new();
        let words = words_from_ranges("日😀 本", &fallback, |range| {
            ranges.push(range);
            match range.location {
                0 | 1 => Some(first.clone()),
                3 => None,
                4 => Some(second.clone()),
                _ => unreachable!(),
            }
        });

        assert_eq!(
            ranges,
            [
                NSRange::new(0, 1),
                NSRange::new(1, 2),
                NSRange::new(3, 1),
                NSRange::new(4, 1),
            ]
        );
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "日😀");
        assert_eq!(words[0].separator, " ");
        assert_eq!(words[1].text, "本");
    }

    #[test]
    fn detects_vertical_text_from_word_progression() {
        let paragraph = BoundingBox::new(0.5, 0.5, 0.1, 0.8);
        let words = [
            Word {
                text: "日".to_owned(),
                separator: String::new(),
                r#box: BoundingBox::new(0.5, 0.2, 0.1, 0.1),
            },
            Word {
                text: "本".to_owned(),
                separator: String::new(),
                r#box: BoundingBox::new(0.5, 0.8, 0.1, 0.1),
            },
        ];

        assert!(text_is_vertical(&paragraph, &words));
    }
}
