// upstream: https://github.com/rtr46/meikiocr/blob/main/meikiocr/ocr.py

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use hf_hub::{HFClientSync, split_id};
use ndarray::{Array2, Array3, Array4, Axis, Ix2, Ix3, s};
use opencv::core::{self, Mat, MatTraitConst, MatTraitManual, Rect, Size, Vec3b};
use opencv::imgproc;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use unicode_general_category::GeneralCategory;

type MeikiResult<T> = Result<T, Box<dyn Error>>;

// --- configuration ---
const DET_MODEL_REPO: &str = "rtr46/meiki.text.detect.v0";
const DET_MODEL_NAME: &str = "meiki.text.detect.v0.1.960x544.onnx";
const REC_MODEL_REPO: &str = "rtr46/meiki.txt.recognition.v0";
const REC_MODEL_NAME: &str = "meiki.text.rec.v0.960x32.onnx";
const VREC_MODEL_NAME: &str = "meiki.text.rec.v0.vertical.32x480.onnx";

const INPUT_DET_WIDTH: usize = 960;
const INPUT_DET_HEIGHT: usize = 544;

// Horizontal Recognition Dims
const INPUT_REC_HEIGHT: usize = 32;
const INPUT_REC_WIDTH: usize = 960;

// Vertical Recognition Dims
const INPUT_VREC_WIDTH: usize = 32;
const INPUT_VREC_HEIGHT: usize = 480;
const VREC_MAX_CONTENT_HEIGHT: usize = 420; // Height of segments when a split is forced
const VREC_OVERLAP_PX: usize = 64; // Overlap strictly by 64px in the scaled space

const X_OVERLAP_THRESHOLD: f32 = 0.3;
const Y_OVERLAP_THRESHOLD: f32 = 0.3;
const EPSILON: f32 = 1e-6;

const SWAPPED_PAIRS: [(&str, &str); 8] = [
    ("儡傀", "傀儡"),
    ("談冗", "冗談"),
    ("汰淘", "淘汰"),
    ("沱滂", "滂沱"),
    ("攣痙", "痙攣"),
    ("酊酩", "酩酊"),
    ("麭麺", "麺麭"),
    ("哭慟", "慟哭"),
];

fn _get_model_path(repo_id: &str, filename: &str) -> MeikiResult<PathBuf> {
    let client = HFClientSync::new()?;
    let (owner, name) = split_id(repo_id);
    client
        .model(owner, name)
        .download_file()
        .filename(filename)
        .send()
        .map_err(|error| {
            log::error!("Error downloading model {filename}: {error}");
            error.into()
        })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextBox {
    pub bbox: [i32; 4],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecognizedChar {
    pub character: char,
    pub bbox: [i32; 4],
    pub conf: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcrResult {
    pub text: String,
    pub chars: Vec<RecognizedChar>,
    pub is_vertical: bool,
}

#[derive(Clone, Debug)]
struct CropMetadata {
    orig_bbox: [i32; 4],
    effective_w: usize,
    effective_h: usize,
    segment_idx: usize,
}

#[derive(Clone, Debug)]
struct Candidate {
    character: char,
    bbox: [i32; 4],
    conf: f32,
    interval: (i32, i32),
}

struct DetectionRaw {
    boxes: Array3<f32>,
    scores: Array2<f32>,
}

struct RecognitionRaw {
    labels: Array2<i32>,
    boxes: Array3<f32>,
    scores: Array2<f32>,
}

pub struct MeikiOcr {
    det_session: Session,
    rec_session: Session,
    vrec_session: Session,
    pub active_provider: String,
    max_batch_size: usize,
}

impl MeikiOcr {
    /// Initializes the meikiocr pipeline by loading detection and recognition models.
    ///
    /// `provider` corresponds to the ONNX Runtime execution provider used upstream.
    /// `max_batch_size` controls recognition-model memory usage and defaults to 8.
    pub fn new(provider: Option<&str>, max_batch_size: usize) -> MeikiResult<Self> {
        let det_model_path = _get_model_path(DET_MODEL_REPO, DET_MODEL_NAME)?;
        let rec_model_path = _get_model_path(REC_MODEL_REPO, REC_MODEL_NAME)?;
        let vrec_model_path = _get_model_path(REC_MODEL_REPO, VREC_MODEL_NAME)?;

        // CPU is the guaranteed baseline. Additional execution providers can be
        // registered here without changing the ported OCR pipeline below.
        let active_provider = provider.unwrap_or("CPUExecutionProvider").to_owned();

        let make_session = |path: &PathBuf| -> MeikiResult<Session> {
            Ok(Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_op_spinning(false)?
                .with_inter_op_spinning(false)?
                .commit_from_file(path)?)
        };

        let det_session = make_session(&det_model_path)?;
        let rec_session = make_session(&rec_model_path)?;
        let vrec_session = make_session(&vrec_model_path)?;

        log::info!("meikiocr initialized on: {active_provider}; max_batch_size = {max_batch_size}");

        Ok(Self {
            det_session,
            rec_session,
            vrec_session,
            active_provider,
            max_batch_size,
        })
    }

    /// Runs the full OCR pipeline on a given image.
    pub fn run_ocr(
        &mut self,
        image: &Mat,
        det_threshold: f32,
        rec_threshold: f32,
        punct_conf_factor: f32,
    ) -> MeikiResult<Vec<OcrResult>> {
        let text_boxes = self.run_detection(image, det_threshold)?;
        log::debug!("Detection found {} text boxes.", text_boxes.len());

        if text_boxes.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = vec![OcrResult::default(); text_boxes.len()];

        let mut h_indices = Vec::new();
        let mut v_indices = Vec::new();
        for (i, tb) in text_boxes.iter().enumerate() {
            let [x1, y1, x2, y2] = tb.bbox;
            let (w, h) = (x2 - x1, y2 - y1);
            if w <= 0 || h <= 0 {
                continue;
            }

            if h > w {
                v_indices.push(i);
            } else {
                h_indices.push(i);
            }
        }

        if !h_indices.is_empty() {
            log::debug!("Processing {} horizontal boxes.", h_indices.len());
            self._process_recognition_pipeline(
                image,
                &text_boxes,
                &h_indices,
                &mut results,
                rec_threshold,
                punct_conf_factor,
                false,
            )?;
        }

        if !v_indices.is_empty() {
            log::debug!("Processing {} vertical boxes.", v_indices.len());
            self._process_recognition_pipeline(
                image,
                &text_boxes,
                &v_indices,
                &mut results,
                rec_threshold,
                punct_conf_factor,
                true,
            )?;
        }

        Ok(results)
    }

    /// Runs only the text detection part of the pipeline.
    pub fn run_detection(&mut self, image: &Mat, conf_threshold: f32) -> MeikiResult<Vec<TextBox>> {
        let (det_input, scale) = self._preprocess_for_detection(image)?;
        let det_raw = self._run_detection_inference(det_input, scale)?;
        let text_boxes = self._postprocess_detection_results(det_raw, image, conf_threshold);
        Ok(text_boxes)
    }

    /// Runs only the text recognition part of the pipeline on a batch of text line images.
    /// Note: `run_ocr` is recommended for general use.
    pub fn run_recognition(
        &mut self,
        text_line_images: &[Mat],
        conf_threshold: f32,
        punct_conf_factor: f32,
    ) -> MeikiResult<Vec<OcrResult>> {
        if text_line_images.is_empty() {
            return Ok(Vec::new());
        }

        let text_boxes: Vec<_> = text_line_images
            .iter()
            .map(|image| TextBox {
                bbox: [0, 0, image.cols(), image.rows()],
            })
            .collect();
        let mut results = vec![OcrResult::default(); text_line_images.len()];

        for (i, image) in text_line_images.iter().enumerate() {
            let (h, w) = (image.rows(), image.cols());
            let is_vertical = h > w;

            let Some((rec_batch, valid_indices, crop_metadata)) =
                self._preprocess_for_recognition(image, &text_boxes[i..=i], &[0], is_vertical)?
            else {
                continue;
            };

            let rec_raw = self._run_recognition_inference(rec_batch, is_vertical)?;
            let mut temp_results = vec![OcrResult::default()];
            self._postprocess_recognition_results(
                rec_raw,
                &valid_indices,
                &crop_metadata,
                conf_threshold,
                &mut temp_results,
                punct_conf_factor,
                is_vertical,
            );
            results[i] = temp_results.remove(0);
        }

        Ok(results)
    }

    // --- Internal methods ---

    fn _preprocess_for_detection(&self, image: &Mat) -> MeikiResult<(Array4<f32>, f32)> {
        let (h_orig, w_orig) = (image.rows() as usize, image.cols() as usize);
        let scale =
            (INPUT_DET_WIDTH as f32 / w_orig as f32).min(INPUT_DET_HEIGHT as f32 / h_orig as f32);
        let (w_resized, h_resized) = (
            (w_orig as f32 * scale) as i32,
            (h_orig as f32 * scale) as i32,
        );
        let mut resized = Mat::default();
        imgproc::resize(
            image,
            &mut resized,
            Size::new(w_resized, h_resized),
            0.0,
            0.0,
            imgproc::INTER_LINEAR,
        )?;

        let mut tensor = Array4::<f32>::zeros((1, 3, INPUT_DET_HEIGHT, INPUT_DET_WIDTH));
        copy_mat_to_chw(&resized, tensor.index_axis_mut(Axis(0), 0))?;
        Ok((tensor, scale))
    }

    fn _run_detection_inference(
        &mut self,
        tensor: Array4<f32>,
        scale: f32,
    ) -> MeikiResult<DetectionRaw> {
        let orig_target_sizes = Array2::from_shape_vec(
            (1, 2),
            vec![
                (INPUT_DET_WIDTH as f32 / scale) as i64,
                (INPUT_DET_HEIGHT as f32 / scale) as i64,
            ],
        )?;
        let input_name = self.det_session.inputs()[0].name().to_owned();
        let size_input_name = self.det_session.inputs()[1].name().to_owned();
        let tensor = Tensor::from_array(tensor)?;
        let orig_target_sizes = Tensor::from_array(orig_target_sizes)?;
        let outputs = self.det_session.run(ort::inputs![
            input_name => tensor,
            size_input_name => orig_target_sizes,
        ])?;

        Ok(DetectionRaw {
            boxes: outputs[1]
                .try_extract_array::<f32>()?
                .to_owned()
                .into_dimensionality::<Ix3>()?,
            scores: outputs[2]
                .try_extract_array::<f32>()?
                .to_owned()
                .into_dimensionality::<Ix2>()?,
        })
    }

    fn _postprocess_detection_results(
        &self,
        raw_outputs: DetectionRaw,
        image: &Mat,
        conf_threshold: f32,
    ) -> Vec<TextBox> {
        let (h_orig, w_orig) = (image.rows(), image.cols());
        let boxes = raw_outputs.boxes.index_axis(Axis(0), 0);
        let scores = raw_outputs.scores.index_axis(Axis(0), 0);

        let mut text_boxes = Vec::new();
        for (bbox, &score) in boxes.outer_iter().zip(scores.iter()) {
            if score > conf_threshold {
                text_boxes.push(TextBox {
                    bbox: [
                        bbox[0].clamp(0.0, w_orig as f32) as i32,
                        bbox[1].clamp(0.0, h_orig as f32) as i32,
                        bbox[2].clamp(0.0, w_orig as f32) as i32,
                        bbox[3].clamp(0.0, h_orig as f32) as i32,
                    ],
                });
            }
        }
        text_boxes.sort_by_key(|tb| tb.bbox[1]);
        text_boxes
    }

    #[allow(clippy::too_many_arguments)]
    fn _process_recognition_pipeline(
        &mut self,
        image: &Mat,
        text_boxes: &[TextBox],
        indices: &[usize],
        results: &mut [OcrResult],
        rec_threshold: f32,
        punct_conf_factor: f32,
        is_vertical: bool,
    ) -> MeikiResult<()> {
        let Some((rec_batch, valid_indices, crop_metadata)) =
            self._preprocess_for_recognition(image, text_boxes, indices, is_vertical)?
        else {
            return Ok(());
        };

        let mut labels_chunks = Vec::new();
        let mut boxes_chunks = Vec::new();
        let mut scores_chunks = Vec::new();
        for i in (0..rec_batch.len_of(Axis(0))).step_by(self.max_batch_size) {
            let end = (i + self.max_batch_size).min(rec_batch.len_of(Axis(0)));
            let batch_chunk = rec_batch.slice(s![i..end, .., .., ..]).to_owned();
            let raw = self._run_recognition_inference(batch_chunk, is_vertical)?;
            labels_chunks.push(raw.labels);
            boxes_chunks.push(raw.boxes);
            scores_chunks.push(raw.scores);
        }

        let all_rec_raw = RecognitionRaw {
            labels: ndarray::concatenate(
                Axis(0),
                &labels_chunks
                    .iter()
                    .map(|array| array.view())
                    .collect::<Vec<_>>(),
            )?,
            boxes: ndarray::concatenate(
                Axis(0),
                &boxes_chunks
                    .iter()
                    .map(|array| array.view())
                    .collect::<Vec<_>>(),
            )?,
            scores: ndarray::concatenate(
                Axis(0),
                &scores_chunks
                    .iter()
                    .map(|array| array.view())
                    .collect::<Vec<_>>(),
            )?,
        };
        self._postprocess_recognition_results(
            all_rec_raw,
            &valid_indices,
            &crop_metadata,
            rec_threshold,
            results,
            punct_conf_factor,
            is_vertical,
        );
        Ok(())
    }

    fn _preprocess_for_recognition(
        &self,
        image: &Mat,
        text_boxes: &[TextBox],
        indices: &[usize],
        is_vertical: bool,
    ) -> MeikiResult<Option<(Array4<f32>, Vec<usize>, Vec<CropMetadata>)>> {
        let mut tensors = Vec::new();
        let mut valid_indices = Vec::new();
        let mut crop_metadata = Vec::new();

        for &i in indices {
            let [x1, y1, x2, y2] = text_boxes[i].bbox;
            if x2 <= x1 || y2 <= y1 {
                continue;
            }
            let crop = Mat::roi(image, Rect::new(x1, y1, x2 - x1, y2 - y1))?;
            if crop.empty() {
                continue;
            }

            let (h, w) = (crop.rows(), crop.cols());

            if !is_vertical {
                let mut new_h = INPUT_REC_HEIGHT;
                let scale = new_h as f32 / h as f32;
                let mut new_w = (w as f32 * scale).round_ties_even() as usize;

                if new_w > INPUT_REC_WIDTH {
                    let scale_w = INPUT_REC_WIDTH as f32 / new_w as f32;
                    new_w = INPUT_REC_WIDTH;
                    new_h = (new_h as f32 * scale_w).round_ties_even() as usize;
                }

                tensors.push(resize_pad_to_chw(
                    &crop,
                    new_w,
                    new_h,
                    INPUT_REC_WIDTH,
                    INPUT_REC_HEIGHT,
                )?);
                valid_indices.push(i);
                crop_metadata.push(CropMetadata {
                    orig_bbox: [x1, y1, x2, y2],
                    effective_w: new_w,
                    effective_h: new_h,
                    segment_idx: 0,
                });
            } else {
                // vertical
                let scale = INPUT_VREC_WIDTH as f32 / w as f32;
                let h_scaled_full = h as f32 * scale;

                log::debug!(
                    "[Box {i}] Vertical original dims: h={h}, w={w}, scale={scale:.3}, scaled_h={h_scaled_full:.1}"
                );

                // Only split if it absolutely exceeds the 480px model limit
                let (max_h_scaled, y_starts, segment_h_orig) = if h_scaled_full
                    > INPUT_VREC_HEIGHT as f32
                {
                    // When splitting, enforce the smaller VREC_MAX_CONTENT_HEIGHT to force padding
                    let segment_h_orig = VREC_MAX_CONTENT_HEIGHT as f32 / scale;
                    let stride_orig = (VREC_MAX_CONTENT_HEIGHT - VREC_OVERLAP_PX) as f32 / scale;
                    let mut y_starts = Vec::new();
                    let mut curr_y = y1 as f32;
                    while curr_y + segment_h_orig < y2 as f32 {
                        y_starts.push(curr_y);
                        curr_y += stride_orig;
                    }
                    let last_y = y2 as f32 - segment_h_orig;
                    if y_starts.is_empty() || last_y > y_starts.last().copied().unwrap() + 1.0 {
                        y_starts.push(last_y);
                    }
                    (VREC_MAX_CONTENT_HEIGHT, y_starts, segment_h_orig)
                } else {
                    // If organically smaller than 480px (e.g. 478px), do not split. Process natively.
                    (INPUT_VREC_HEIGHT, vec![y1 as f32], (y2 - y1) as f32)
                };

                for (seg_idx, sy1_f) in y_starts.into_iter().enumerate() {
                    let sy1 = sy1_f.round_ties_even() as i32;
                    let sy2 = ((sy1_f + segment_h_orig).round_ties_even() as i32).min(y2);
                    if sy2 <= sy1 {
                        continue;
                    }
                    let segment_crop = Mat::roi(image, Rect::new(x1, sy1, x2 - x1, sy2 - sy1))?;
                    let seg_h = segment_crop.rows();
                    if seg_h <= 0 {
                        continue;
                    }

                    let seg_new_h =
                        ((seg_h as f32 * scale).round_ties_even() as usize).min(max_h_scaled);
                    tensors.push(resize_pad_to_chw(
                        &segment_crop,
                        INPUT_VREC_WIDTH,
                        seg_new_h,
                        INPUT_VREC_WIDTH,
                        INPUT_VREC_HEIGHT,
                    )?);
                    valid_indices.push(i);

                    let pad_h = INPUT_VREC_HEIGHT - seg_new_h;
                    log::debug!(
                        "  -> Segment {seg_idx}: sy1={sy1}, sy2={sy2}, content_h={seg_new_h}, pad_bottom={pad_h}"
                    );
                    crop_metadata.push(CropMetadata {
                        orig_bbox: [x1, sy1, x2, sy2],
                        effective_w: INPUT_VREC_WIDTH,
                        effective_h: seg_new_h,
                        segment_idx: seg_idx,
                    });
                }
            }
        }

        if tensors.is_empty() {
            return Ok(None);
        }
        let tensor_views: Vec<_> = tensors.iter().map(|tensor| tensor.view()).collect();
        Ok(Some((
            ndarray::stack(Axis(0), &tensor_views)?,
            valid_indices,
            crop_metadata,
        )))
    }

    fn _run_recognition_inference(
        &mut self,
        batch_tensor: Array4<f32>,
        is_vertical: bool,
    ) -> MeikiResult<RecognitionRaw> {
        let (width, height) = if !is_vertical {
            (INPUT_REC_WIDTH, INPUT_REC_HEIGHT)
        } else {
            (INPUT_VREC_WIDTH, INPUT_VREC_HEIGHT)
        };
        let orig_size = Array2::from_shape_vec((1, 2), vec![width as i64, height as i64])?;
        let session = if !is_vertical {
            &mut self.rec_session
        } else {
            &mut self.vrec_session
        };
        let batch_tensor = Tensor::from_array(batch_tensor)?;
        let orig_size = Tensor::from_array(orig_size)?;
        let outputs = session.run(ort::inputs![
            "images" => batch_tensor,
            "orig_target_sizes" => orig_size,
        ])?;
        Ok(RecognitionRaw {
            labels: into_array2(&outputs[0])?,
            boxes: into_array3(&outputs[1])?,
            scores: into_array2(&outputs[2])?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn _postprocess_recognition_results(
        &self,
        raw_rec_outputs: RecognitionRaw,
        valid_indices: &[usize],
        crop_metadata: &[CropMetadata],
        rec_conf_threshold: f32,
        results: &mut [OcrResult],
        punct_conf_factor: f32,
        is_vertical: bool,
    ) {
        let mut candidates_by_idx: HashMap<usize, Vec<Candidate>> = HashMap::new();

        for i in 0..raw_rec_outputs.labels.len_of(Axis(0)) {
            let labels = raw_rec_outputs.labels.index_axis(Axis(0), i);
            let boxes = raw_rec_outputs.boxes.index_axis(Axis(0), i);
            let scores = raw_rec_outputs.scores.index_axis(Axis(0), i);
            let orig_idx = valid_indices[i];
            let meta = &crop_metadata[i];
            let [gx1, gy1, gx2, gy2] = meta.orig_bbox;
            let (crop_w, crop_h) = (gx2 - gx1, gy2 - gy1);
            let candidates = candidates_by_idx.entry(orig_idx).or_default();

            log::debug!(
                "--- Processing Raw Results for Box {orig_idx} | Seg {} ---",
                meta.segment_idx
            );

            for j in 0..labels.len_of(Axis(0)) {
                let scr = scores[j];
                if scr < rec_conf_threshold {
                    continue;
                }
                let Some(character) = char::from_u32(labels[j] as u32) else {
                    continue;
                };
                let box_row = boxes.index_axis(Axis(0), j);
                let (mut rx1, mut ry1, mut rx2, mut ry2) =
                    (box_row[0], box_row[1], box_row[2], box_row[3]);

                if !is_vertical {
                    let effective_w = meta.effective_w as f32;
                    if rx1 >= effective_w {
                        continue;
                    }
                    rx1 = rx1.min(effective_w);
                    rx2 = rx2.min(effective_w);
                    let (cx1, cx2) = (
                        (rx1 / effective_w) * crop_w as f32,
                        (rx2 / effective_w) * crop_w as f32,
                    );
                    let (cy1, cy2) = (
                        (ry1 / INPUT_REC_HEIGHT as f32) * crop_h as f32,
                        (ry2 / INPUT_REC_HEIGHT as f32) * crop_h as f32,
                    );
                    let bbox = [
                        gx1 + cx1 as i32,
                        gy1 + cy1 as i32,
                        gx1 + cx2 as i32,
                        gy1 + cy2 as i32,
                    ];
                    candidates.push(Candidate {
                        character,
                        bbox,
                        conf: scr,
                        interval: (bbox[0], bbox[2]),
                    });
                } else {
                    // vertical
                    let effective_h = meta.effective_h as f32;
                    if ry1 >= effective_h {
                        continue;
                    }
                    ry1 = ry1.min(effective_h);
                    ry2 = ry2.min(effective_h);
                    let (cx1, cx2) = (
                        (rx1 / INPUT_VREC_WIDTH as f32) * crop_w as f32,
                        (rx2 / INPUT_VREC_WIDTH as f32) * crop_w as f32,
                    );
                    let (cy1, cy2) = (
                        (ry1 / effective_h) * crop_h as f32,
                        (ry2 / effective_h) * crop_h as f32,
                    );
                    let bbox = [
                        gx1 + cx1 as i32,
                        gy1 + cy1 as i32,
                        gx1 + cx2 as i32,
                        gy1 + cy2 as i32,
                    ];
                    if bbox[3] <= bbox[1] {
                        continue;
                    }
                    log::debug!(
                        "  [KEPT]      '{}' (conf: {:.2}) mapping to global gy={}-{}",
                        character,
                        scr,
                        bbox[1],
                        bbox[3]
                    );
                    candidates.push(Candidate {
                        character,
                        bbox,
                        conf: scr,
                        interval: (bbox[1], bbox[3]),
                    });
                }
            }
        }

        let overlap_threshold = if !is_vertical {
            X_OVERLAP_THRESHOLD
        } else {
            Y_OVERLAP_THRESHOLD
        };

        for (orig_idx, mut candidates) in candidates_by_idx {
            log::debug!("--- Running NMS on Combined Candidates for Box {orig_idx} ---");

            if punct_conf_factor != 1.0 {
                for cand in &mut candidates {
                    if is_punctuation(cand.character) {
                        cand.conf *= punct_conf_factor;
                    }
                }
            }

            candidates.sort_by(|a, b| b.conf.total_cmp(&a.conf));
            let mut accepted: Vec<Candidate> = Vec::new();
            let mut accepted_intervals = Vec::new();

            for cand in candidates {
                let (i1_c, i2_c) = cand.interval;
                let len_c = (i2_c - i1_c) as f32 + EPSILON;
                let mut is_overlap = false;

                for &(i1_a, i2_a) in &accepted_intervals {
                    if i1_c >= i2_a || i1_a >= i2_c {
                        continue;
                    }
                    let inter_start = i1_c.max(i1_a);
                    let inter_end = i2_c.min(i2_a);
                    let inter_len = 0.max(inter_end - inter_start);
                    let len_a = (i2_a - i1_a) as f32 + EPSILON;
                    let min_len = len_c.min(len_a);
                    if inter_len as f32 / min_len > overlap_threshold {
                        is_overlap = true;
                        break;
                    }
                }

                if !is_overlap {
                    log::debug!(
                        "  [NMS ACCEPT] '{}' (conf: {:.2}) at interval {:?}",
                        cand.character,
                        cand.conf,
                        cand.interval
                    );
                    accepted_intervals.push(cand.interval);
                    accepted.push(cand);
                } else {
                    log::debug!(
                        "  [NMS SUPPRESS] '{}' (conf: {:.2}) at interval {:?} due to overlap",
                        cand.character,
                        cand.conf,
                        cand.interval
                    );
                }
            }

            accepted.sort_by_key(|cand| cand.interval.0);
            let mut result_chars: Vec<_> = accepted
                .into_iter()
                .map(|candidate| RecognizedChar {
                    character: candidate.character,
                    bbox: candidate.bbox,
                    conf: candidate.conf,
                })
                .collect();
            let mut text: String = result_chars.iter().map(|item| item.character).collect();
            Self::_fix_swapped_pairs(&mut text, &mut result_chars);
            log::debug!("--- FINAL TEXT BOX {orig_idx}: {text} ---");
            results[orig_idx] = OcrResult {
                text,
                chars: result_chars,
                is_vertical,
            };
        }
    }

    fn _fix_swapped_pairs(text: &mut String, chars: &mut [RecognizedChar]) {
        for (wrong, correct) in SWAPPED_PAIRS {
            if let Some(byte_idx) = text.find(wrong) {
                let char_idx = text[..byte_idx].chars().count();
                if char_idx + 1 < chars.len() {
                    text.replace_range(byte_idx..byte_idx + wrong.len(), correct);
                    chars.swap(char_idx, char_idx + 1);
                }
            }
        }
    }
}

fn is_punctuation(character: char) -> bool {
    matches!(
        unicode_general_category::get_general_category(character),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::OpenPunctuation
    )
}

pub(crate) fn mat_from_rgb_bytes(bytes: &[u8], width: usize, height: usize) -> PyResult<Mat> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| PyValueError::new_err("image dimensions are too large"))?;
    if width == 0 || height == 0 {
        return Err(PyValueError::new_err(
            "image dimensions must be greater than zero",
        ));
    }
    if bytes.len() != expected_len {
        return Err(PyValueError::new_err(format!(
            "RGB image buffer has {} bytes, expected {expected_len}",
            bytes.len()
        )));
    }

    let mut image = Mat::new_rows_cols_with_default(
        height as i32,
        width as i32,
        core::CV_8UC3,
        core::Scalar::all(0.0),
    )
    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
    image
        .data_bytes_mut()
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
        .copy_from_slice(bytes);
    Ok(image)
}

fn resize_pad_to_chw(
    source: &impl core::ToInputArray,
    new_w: usize,
    new_h: usize,
    target_w: usize,
    target_h: usize,
) -> MeikiResult<Array3<f32>> {
    let mut resized = Mat::default();
    imgproc::resize(
        source,
        &mut resized,
        Size::new(new_w as i32, new_h as i32),
        0.0,
        0.0,
        imgproc::INTER_LINEAR,
    )?;
    let mut tensor = Array3::<f32>::zeros((3, target_h, target_w));
    copy_mat_to_chw(&resized, tensor.view_mut())?;
    Ok(tensor)
}

fn copy_mat_to_chw(
    mat: &Mat,
    mut destination: ndarray::ArrayViewMut3<'_, f32>,
) -> opencv::Result<()> {
    for y in 0..mat.rows() {
        for x in 0..mat.cols() {
            let pixel = *mat.at_2d::<Vec3b>(y, x)?;
            for channel in 0..3 {
                destination[[channel, y as usize, x as usize]] = pixel[channel] as f32 / 255.0;
            }
        }
    }
    Ok(())
}

fn into_array3<T>(value: &ort::value::DynValue) -> MeikiResult<Array3<T>>
where
    T: ort::value::PrimitiveTensorElementType + Clone,
{
    Ok(value
        .try_extract_array::<T>()?
        .to_owned()
        .into_dimensionality::<Ix3>()?)
}

fn into_array2<T>(value: &ort::value::DynValue) -> MeikiResult<Array2<T>>
where
    T: ort::value::PrimitiveTensorElementType + Clone,
{
    Ok(value
        .try_extract_array::<T>()?
        .to_owned()
        .into_dimensionality::<Ix2>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_swapped_pairs_in_text_and_characters() {
        let mut text = "これは儡傀です".to_owned();
        let mut chars: Vec<_> = text
            .chars()
            .map(|character| RecognizedChar {
                character,
                ..RecognizedChar::default()
            })
            .collect();

        MeikiOcr::_fix_swapped_pairs(&mut text, &mut chars);

        assert_eq!(text, "これは傀儡です");
        assert_eq!(
            chars.iter().map(|item| item.character).collect::<String>(),
            text
        );
    }
}
