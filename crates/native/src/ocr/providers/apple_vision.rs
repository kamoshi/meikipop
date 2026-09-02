use std::error::Error;
use std::time::Instant;

use image::ExtendedColorType;
use image::codecs::bmp::BmpEncoder;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

use crate::ocr::interface::{BoundingBox, Mat, OcrContext, OcrProvider, Paragraph, Word};

pub struct AppleVisionOcrProvider;

fn region_around_focus(context: OcrContext) -> (f64, f64, f64, f64) {
    let Some(focus) = context.focus_point else {
        return (0.0, 0.0, 1.0, 1.0);
    };

    let roi_w = 0.35;
    let roi_h = 0.30;
    let roi_x = (focus.x.clamp(0.0, 1.0) - roi_w / 2.0).clamp(0.0, 1.0 - roi_w);
    // Vision uses a bottom-left origin while pipeline coordinates use top-left.
    let roi_y_bl = (1.0 - focus.y.clamp(0.0, 1.0) - roi_h / 2.0).clamp(0.0, 1.0 - roi_h);
    (roi_x, roi_y_bl, roi_w, roi_h)
}

fn encode_bmp(image: &Mat) -> image::ImageResult<Vec<u8>> {
    let mut buf = Vec::new();
    BmpEncoder::new(&mut buf).encode(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

impl AppleVisionOcrProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        log::info!("Initialized macOS Apple Vision OCR provider (Native RegionOfInterest mode)");
        Ok(Self)
    }
}

impl OcrProvider for AppleVisionOcrProvider {
    fn name(&self) -> &'static str {
        "apple_vision (macOS)"
    }

    fn scan(&mut self, image: &Mat, context: OcrContext) -> Result<Vec<Paragraph>, Box<dyn Error>> {
        let start_time = Instant::now();

        let buf = encode_bmp(image)?;
        let encode_time = start_time.elapsed();

        let ns_data = NSData::with_bytes(&buf);

        let options = NSDictionary::new();
        let alloc = VNImageRequestHandler::alloc();
        let handler = VNImageRequestHandler::initWithData_options(alloc, &ns_data, &options);

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        request.setAutomaticallyDetectsLanguage(false);

        let lang_ja = NSString::from_str("ja-JP");
        let langs = NSArray::from_retained_slice(&[lang_ja]);
        request.setRecognitionLanguages(&langs);

        // Dynamically restrict Vision's Region of Interest (ROI) around cursor.
        let (roi_x, roi_y_bottom_left, roi_w, roi_h) = region_around_focus(context);
        // SAFETY: The helper clamps the rectangle to finite unit image coordinates.
        unsafe {
            request.setRegionOfInterest(CGRect::new(
                CGPoint::new(roi_x, roi_y_bottom_left),
                CGSize::new(roi_w, roi_h),
            ));
        }

        let request_base: Retained<VNRequest> =
            Retained::into_super(Retained::into_super(request.clone()));
        let requests = NSArray::from_retained_slice(&[request_base]);

        let vision_start = Instant::now();
        handler.performRequests_error(&requests)?;
        let vision_time = vision_start.elapsed();

        let mut paragraphs = Vec::new();
        if let Some(results) = request.results() {
            for obs in results.iter() {
                let top_candidates = obs.topCandidates(1);
                if let Some(top_candidate) = top_candidates.firstObject() {
                    let text = top_candidate.string().to_string();
                    let bbox_cg = unsafe { obs.boundingBox() };

                    // Re-project ROI-relative coordinates to the full captured image.
                    let local_w = bbox_cg.size.width;
                    let local_h = bbox_cg.size.height;
                    let local_x = bbox_cg.origin.x;
                    let local_y_bl = bbox_cg.origin.y;

                    let global_w = local_w * roi_w;
                    let global_h = local_h * roi_h;
                    let global_left = roi_x + local_x * roi_w;
                    let global_bottom_left = roi_y_bottom_left + local_y_bl * roi_h;

                    let global_cx = global_left + global_w / 2.0;
                    let global_cy_bottom_left = global_bottom_left + global_h / 2.0;
                    let global_cy_top_left = 1.0 - global_cy_bottom_left;

                    let box_norm =
                        BoundingBox::new(global_cx, global_cy_top_left, global_w, global_h);
                    let mut paragraph = Paragraph::new(box_norm.clone(), false);
                    paragraph.full_text = text.clone();
                    paragraph.words.push(Word {
                        text,
                        separator: String::new(),
                        r#box: box_norm,
                    });
                    paragraphs.push(paragraph);
                }
            }
        }

        log::info!(
            "Apple Vision OCR total: {:?}, bmp_prep: {:?}, NPU_inference: {:?}, paragraphs: {}",
            start_time.elapsed(),
            encode_time,
            vision_time,
            paragraphs.len()
        );

        Ok(paragraphs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb};

    #[test]
    fn bmp_encoding_preserves_rgb_channels() {
        let source = Mat::from_pixel(1, 1, Rgb([10, 20, 30]));

        let decoded =
            image::load_from_memory_with_format(&encode_bmp(&source).unwrap(), ImageFormat::Bmp)
                .unwrap()
                .into_rgb8();

        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30]);
    }

    #[test]
    fn region_of_interest_uses_pipeline_relative_focus() {
        let context = OcrContext {
            focus_point: Some(crate::ocr::interface::NormalizedPoint { x: 0.5, y: 0.25 }),
        };

        assert_eq!(region_around_focus(context), (0.325, 0.6, 0.35, 0.3));
        assert_eq!(
            region_around_focus(OcrContext::default()),
            (0.0, 0.0, 1.0, 1.0)
        );
    }
}
