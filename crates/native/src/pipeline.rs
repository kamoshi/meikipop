use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, Sender};

use crate::dictionary::lookup::{DictionaryEntry, KanjiEntry, LookupEngine};
use crate::input::interface::{PointerProvider, PointerSnapshot};
use crate::ocr::hit_scan::hit_scan;
use crate::ocr::interface::{NormalizedPoint, OcrContext, Paragraph};
use crate::ocr::ocr::OcrProcessor;
use crate::platform::create_desktop_providers;
use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, FrameProvider};

pub struct PipelineConfig {
    pub dictionary_path: PathBuf,
    pub screencast_token_path: PathBuf,
    pub max_dict_entries: usize,
    pub max_lookup_length: usize,
    pub show_kanji: bool,
    pub capture_interval: Duration,
}

#[derive(Debug)]
pub enum PipelineEvent {
    CaptureReady,
    LookupResult {
        entries: Vec<DictionaryEntry>,
        kanji: Option<KanjiEntry>,
        mouse_x: i32,
        mouse_y: i32,
        capture_geometry: CaptureGeometry,
    },
    HidePopup {
        mouse_x: i32,
        mouse_y: i32,
    },
    Error(String),
}

pub struct Pipeline {
    event_receiver: Receiver<PipelineEvent>,
    running: Arc<AtomicBool>,
    popup_is_visible: Arc<AtomicBool>,
    coordinator: Option<JoinHandle<()>>,
}

impl Pipeline {
    pub fn start(config: PipelineConfig) -> Result<Self, std::io::Error> {
        Self::start_with_runner(move |running, popup_is_visible, event_sender| {
            let screencast_token = config.screencast_token_path.to_string_lossy().into_owned();
            let providers = match create_desktop_providers(screencast_token) {
                Ok(providers) => providers,
                Err(error) => {
                    send_error(
                        &event_sender,
                        "Failed to initialize desktop providers",
                        error,
                    );
                    return;
                }
            };
            run_pipeline(
                config,
                providers.frames,
                providers.pointer,
                running,
                popup_is_visible,
                event_sender,
            );
        })
    }

    pub fn start_with_providers(
        config: PipelineConfig,
        frame_provider: Box<dyn FrameProvider>,
        pointer_provider: Box<dyn PointerProvider>,
    ) -> Result<Self, std::io::Error> {
        Self::start_with_runner(move |running, popup_is_visible, event_sender| {
            run_pipeline(
                config,
                frame_provider,
                pointer_provider,
                running,
                popup_is_visible,
                event_sender,
            );
        })
    }

    fn start_with_runner(
        runner: impl FnOnce(Arc<AtomicBool>, Arc<AtomicBool>, Sender<PipelineEvent>) + Send + 'static,
    ) -> Result<Self, std::io::Error> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let running = Arc::new(AtomicBool::new(true));
        let popup_is_visible = Arc::new(AtomicBool::new(false));
        let coordinator = {
            let running = Arc::clone(&running);
            let popup_is_visible = Arc::clone(&popup_is_visible);
            thread::Builder::new()
                .name("PipelineInit".to_owned())
                .spawn(move || runner(running, popup_is_visible, event_sender))?
        };

        Ok(Self {
            event_receiver,
            running,
            popup_is_visible,
            coordinator: Some(coordinator),
        })
    }

    pub fn try_recv(&self) -> Option<PipelineEvent> {
        self.event_receiver.try_recv().ok()
    }

    pub fn set_popup_visible(&self, visible: bool) {
        self.popup_is_visible.store(visible, Ordering::Release);
    }

    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(coordinator) = self.coordinator.take() {
            let _ = coordinator.join();
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct LookupRequest {
    lookup_string: Option<String>,
    mouse_x: i32,
    mouse_y: i32,
    capture_geometry: CaptureGeometry,
}

struct OcrWorkItem {
    frame: CapturedFrame,
    pointer: PointerSnapshot,
}

struct RecognizedFrame {
    sequence: u64,
    paragraphs: Vec<Paragraph>,
    capture_geometry: CaptureGeometry,
}

fn run_pipeline(
    config: PipelineConfig,
    mut frame_provider: Box<dyn FrameProvider>,
    pointer_provider: Box<dyn PointerProvider>,
    running: Arc<AtomicBool>,
    popup_is_visible: Arc<AtomicBool>,
    event_sender: Sender<PipelineEvent>,
) {
    log::debug!("Initializing native OCR pipeline");
    let capture_geometry = match frame_provider
        .capture_geometry()
        .map_err(|error| error.to_string())
    {
        Ok(geometry) => geometry,
        Err(error) => {
            send_error(&event_sender, "Failed to get capture geometry", error);
            return;
        }
    };
    log::info!(
        "Selected capture source: left={}, top={}, width={}, height={}",
        capture_geometry.left,
        capture_geometry.top,
        capture_geometry.width,
        capture_geometry.height
    );
    let _ = event_sender.send(PipelineEvent::CaptureReady);

    let ocr_processor = match OcrProcessor::new() {
        Ok(processor) => processor,
        Err(error) => {
            send_error(&event_sender, "Failed to initialize OCR", error);
            return;
        }
    };
    log::info!("OCR processor initialized");

    let lookup_engine =
        match LookupEngine::open_paths(&config.dictionary_path, config.max_dict_entries) {
            Ok(engine) => engine,
            Err(error) => {
                send_error(&event_sender, "Failed to load dictionary", error);
                return;
            }
        };
    let (validation_issues, validation_warnings) = lookup_engine.validation();
    for warning in validation_warnings {
        log::warn!("{warning}");
    }
    if validation_issues == 0 {
        log::info!("Dictionary validation passed with no issues.");
    } else {
        log::warn!(
            "Dictionary validation found {validation_issues} issue(s) — some entries may display incorrectly."
        );
    }
    log::info!("Dictionary loaded");

    // Each processing stage owns its mutable state. Capacity-one channels
    // provide backpressure without allowing stale work to accumulate.
    let (screenshot_sender, screenshot_receiver) = crossbeam_channel::bounded(1);
    let (ocr_sender, ocr_receiver) = crossbeam_channel::bounded(1);
    let (lookup_sender, lookup_receiver) = crossbeam_channel::bounded(1);
    let latest_pointer = Arc::new(Mutex::new(None));

    let workers = vec![
        spawn_hit_scan_worker(
            Arc::clone(&running),
            pointer_provider,
            capture_geometry.clone(),
            Arc::clone(&latest_pointer),
            ocr_receiver,
            lookup_sender,
        ),
        spawn_capture_worker(
            Arc::clone(&running),
            Arc::clone(&popup_is_visible),
            frame_provider,
            config.capture_interval,
            Arc::clone(&latest_pointer),
            screenshot_sender,
        ),
        spawn_ocr_worker(
            Arc::clone(&running),
            ocr_processor,
            screenshot_receiver,
            ocr_sender,
        ),
        spawn_lookup_worker(
            Arc::clone(&running),
            Arc::clone(&popup_is_visible),
            lookup_engine,
            config.max_lookup_length,
            config.show_kanji,
            lookup_receiver,
            event_sender,
        ),
    ];

    for worker in workers {
        let _ = worker.join();
    }
}

fn spawn_capture_worker(
    running: Arc<AtomicBool>,
    popup_is_visible: Arc<AtomicBool>,
    mut frame_provider: Box<dyn FrameProvider>,
    capture_interval: Duration,
    latest_pointer: Arc<Mutex<Option<PointerSnapshot>>>,
    screenshot_sender: Sender<OcrWorkItem>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ScreencastWorker".to_owned())
        .spawn(move || {
            log::debug!("Screencast worker started");
            while running.load(Ordering::Acquire) {
                if popup_is_visible.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }

                match frame_provider.capture_frame() {
                    Ok(frame) => {
                        let pointer = latest_pointer
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .clone();
                        let Some(pointer) = pointer else {
                            thread::sleep(capture_interval);
                            continue;
                        };
                        if !send_while_running(
                            &screenshot_sender,
                            OcrWorkItem { frame, pointer },
                            &running,
                        ) {
                            break;
                        }
                    }
                    Err(error) => log::error!("Screencast worker error: {error}"),
                }
                thread::sleep(capture_interval);
            }
            log::debug!("Screencast worker stopped");
        })
        .expect("failed to spawn ScreencastWorker")
}

fn spawn_ocr_worker(
    running: Arc<AtomicBool>,
    mut ocr_processor: OcrProcessor,
    screenshot_receiver: Receiver<OcrWorkItem>,
    ocr_sender: Sender<RecognizedFrame>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("OcrWorker".to_owned())
        .spawn(move || {
            let mut last_scan: Option<(Vec<u8>, NormalizedPoint)> = None;
            log::debug!("OCR worker started");
            while running.load(Ordering::Acquire) {
                let work = match screenshot_receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(work) => work,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                if !work.pointer.target_available {
                    continue;
                }
                let Some(focus_point) =
                    normalized_pointer_position(work.pointer.position, &work.frame.geometry)
                else {
                    continue;
                };
                let context = OcrContext {
                    focus_point: Some(focus_point),
                };
                if last_scan.as_ref().is_some_and(|(raw, previous_focus)| {
                    raw == &work.frame.screenshot.raw && *previous_focus == focus_point
                }) {
                    continue;
                }

                let ocr_start = std::time::Instant::now();
                let result = (|| -> Result<Vec<Paragraph>, Box<dyn Error>> {
                    let image = work.frame.screenshot.to_rgb()?;
                    ocr_processor.scan_rgb(&image, context)?;
                    Ok(ocr_processor.last_ocr_result.take().unwrap_or_default())
                })();

                match result {
                    Ok(paragraphs) => {
                        log::info!(
                            "OCR worker completed scan in {:?} (found {} text paragraph(s))",
                            ocr_start.elapsed(),
                            paragraphs.len()
                        );
                        // Only suppress an identical frame and focus point after OCR succeeded.
                        last_scan = Some((work.frame.screenshot.raw, focus_point));
                        if !send_while_running(
                            &ocr_sender,
                            RecognizedFrame {
                                sequence: work.frame.sequence,
                                paragraphs,
                                capture_geometry: work.frame.geometry,
                            },
                            &running,
                        ) {
                            break;
                        }
                    }
                    Err(error) => log::error!("OCR worker error: {error}"),
                }
            }
            log::debug!("OCR worker stopped");
        })
        .expect("failed to spawn OcrWorker")
}

fn spawn_hit_scan_worker(
    running: Arc<AtomicBool>,
    mut pointer_provider: Box<dyn PointerProvider>,
    capture_geometry: CaptureGeometry,
    latest_pointer: Arc<Mutex<Option<PointerSnapshot>>>,
    ocr_receiver: Receiver<RecognizedFrame>,
    lookup_sender: Sender<LookupRequest>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("HitScannerWorker".to_owned())
        .spawn(move || {
            log::debug!("Mouse tracker and hit scanner worker started");
            let mut recognized_frame: Option<RecognizedFrame> = None;
            let mut last_mouse_pos = None;
            let mut last_capture_geometry = None;
            let mut last_target_available = None;
            let mut last_ocr_sequence = None;
            let mut last_pointer_error = None;

            while running.load(Ordering::Acquire) {
                while let Ok(latest_frame) = ocr_receiver.try_recv() {
                    recognized_frame = Some(latest_frame);
                }

                let pointer = match pointer_provider.snapshot() {
                    Ok(pointer) => {
                        last_pointer_error = None;
                        pointer
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if last_pointer_error.as_deref() != Some(message.as_str()) {
                            log::warn!("Failed to read pointer position: {message}");
                            last_pointer_error = Some(message);
                        }
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                };
                *latest_pointer
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(pointer.clone());

                let active_geometry = pointer
                    .capture_geometry
                    .clone()
                    .unwrap_or_else(|| capture_geometry.clone());
                let ocr_sequence = recognized_frame.as_ref().map(|frame| frame.sequence);
                let ocr_changed = last_ocr_sequence != ocr_sequence;
                if !lookup_input_changed(
                    last_mouse_pos,
                    last_capture_geometry.as_ref(),
                    last_target_available,
                    pointer.position,
                    &active_geometry,
                    pointer.target_available,
                    ocr_changed,
                ) {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                last_mouse_pos = Some(pointer.position);
                last_capture_geometry = Some(active_geometry.clone());
                last_target_available = Some(pointer.target_available);
                last_ocr_sequence = ocr_sequence;

                let geometry_matches_ocr = recognized_frame.as_ref().is_some_and(|frame| {
                    frame.capture_geometry.width == active_geometry.width
                        && frame.capture_geometry.height == active_geometry.height
                });
                let lookup_string = (pointer.target_available && geometry_matches_ocr)
                    .then(|| normalized_pointer_position(pointer.position, &active_geometry))
                    .flatten()
                    .and_then(|point| {
                        hit_scan(&recognized_frame.as_ref()?.paragraphs, point.x, point.y)
                    });

                if !send_while_running(
                    &lookup_sender,
                    LookupRequest {
                        lookup_string,
                        mouse_x: pointer.position.0,
                        mouse_y: pointer.position.1,
                        capture_geometry: active_geometry,
                    },
                    &running,
                ) {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            log::debug!("Mouse tracker and hit scanner worker stopped");
        })
        .expect("failed to spawn HitScannerWorker")
}

fn lookup_input_changed(
    last_pointer: Option<(i32, i32)>,
    last_capture_geometry: Option<&CaptureGeometry>,
    last_target_available: Option<bool>,
    pointer: (i32, i32),
    capture_geometry: &CaptureGeometry,
    target_available: bool,
    ocr_changed: bool,
) -> bool {
    ocr_changed
        || last_pointer != Some(pointer)
        || last_capture_geometry != Some(capture_geometry)
        || last_target_available != Some(target_available)
}

fn normalized_pointer_position(
    pointer: (i32, i32),
    capture_geometry: &CaptureGeometry,
) -> Option<NormalizedPoint> {
    if !capture_geometry.contains(pointer) {
        return None;
    }

    let x = (i64::from(pointer.0) - i64::from(capture_geometry.left)) as f64
        / capture_geometry.width as f64;
    let y = (i64::from(pointer.1) - i64::from(capture_geometry.top)) as f64
        / capture_geometry.height as f64;
    Some(NormalizedPoint { x, y })
}

fn spawn_lookup_worker(
    running: Arc<AtomicBool>,
    popup_is_visible: Arc<AtomicBool>,
    mut lookup_engine: LookupEngine,
    max_lookup_length: usize,
    show_kanji: bool,
    lookup_receiver: Receiver<LookupRequest>,
    event_sender: Sender<PipelineEvent>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("LookupWorker".to_owned())
        .spawn(move || {
            log::debug!("Lookup worker started");
            while running.load(Ordering::Acquire) {
                let request = match lookup_receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(request) => request,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                if let Some(lookup_string) = request.lookup_string {
                    let result =
                        lookup_engine.lookup_cached(&lookup_string, max_lookup_length, show_kanji);
                    if !result.entries.is_empty() || result.kanji_entry.is_some() {
                        log::debug!(
                            "Sending lookup result at mouse ({}, {}) in capture source left={}, top={}, width={}, height={}",
                            request.mouse_x,
                            request.mouse_y,
                            request.capture_geometry.left,
                            request.capture_geometry.top,
                            request.capture_geometry.width,
                            request.capture_geometry.height
                        );
                        if event_sender
                            .send(PipelineEvent::LookupResult {
                                entries: result.entries,
                                kanji: result.kanji_entry,
                                mouse_x: request.mouse_x,
                                mouse_y: request.mouse_y,
                                capture_geometry: request.capture_geometry,
                            })
                            .is_err()
                        {
                            break;
                        }
                        popup_is_visible.store(true, Ordering::Release);
                    } else {
                        hide_popup_if_visible(
                            &event_sender,
                            &popup_is_visible,
                            request.mouse_x,
                            request.mouse_y,
                        );
                    }
                } else {
                    hide_popup_if_visible(
                        &event_sender,
                        &popup_is_visible,
                        request.mouse_x,
                        request.mouse_y,
                    );
                }
            }
            log::debug!("Lookup worker stopped");
        })
        .expect("failed to spawn LookupWorker")
}

fn send_while_running<T>(sender: &Sender<T>, mut value: T, running: &AtomicBool) -> bool {
    while running.load(Ordering::Acquire) {
        match sender.send_timeout(value, Duration::from_millis(50)) {
            Ok(()) => return true,
            Err(SendTimeoutError::Timeout(returned_value)) => value = returned_value,
            Err(SendTimeoutError::Disconnected(_)) => return false,
        }
    }
    false
}

fn hide_popup_if_visible(
    sender: &Sender<PipelineEvent>,
    popup_is_visible: &AtomicBool,
    mouse_x: i32,
    mouse_y: i32,
) {
    if popup_is_visible.swap(false, Ordering::AcqRel) {
        let _ = sender.send(PipelineEvent::HidePopup { mouse_x, mouse_y });
    }
}

fn send_error(sender: &Sender<PipelineEvent>, context: &str, error: impl std::fmt::Display) {
    log::error!("{context}: {error}");
    let _ = sender.send(PipelineEvent::Error(format!("{context}: {error}")));
}

#[cfg(test)]
mod tests {
    use super::{lookup_input_changed, normalized_pointer_position};
    use crate::ocr::interface::NormalizedPoint;
    use crate::screenshot::interface::CaptureGeometry;

    fn geometry(left: i32, top: i32, width: usize, height: usize) -> CaptureGeometry {
        CaptureGeometry {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn capture_source_movement_invalidates_a_stationary_pointer_lookup() {
        let previous = geometry(0, 0, 800, 600);
        let moved = geometry(100, 50, 800, 600);

        assert!(lookup_input_changed(
            Some((200, 150)),
            Some(&previous),
            Some(true),
            (200, 150),
            &moved,
            true,
            false,
        ));
        assert!(!lookup_input_changed(
            Some((200, 150)),
            Some(&moved),
            Some(true),
            (200, 150),
            &moved,
            true,
            false,
        ));
    }

    #[test]
    fn target_occlusion_invalidates_a_stationary_pointer_lookup() {
        let target = geometry(0, 0, 800, 600);

        assert!(lookup_input_changed(
            Some((200, 150)),
            Some(&target),
            Some(true),
            (200, 150),
            &target,
            false,
            false,
        ));
    }

    #[test]
    fn pointer_normalization_rejects_invalid_or_outside_geometry() {
        assert_eq!(
            normalized_pointer_position((150, 100), &geometry(100, 50, 200, 100)),
            Some(NormalizedPoint { x: 0.25, y: 0.5 }),
        );
        assert_eq!(
            normalized_pointer_position((99, 100), &geometry(100, 50, 200, 100)),
            None,
        );
        assert_eq!(
            normalized_pointer_position((300, 100), &geometry(100, 50, 200, 100)),
            None,
        );
        assert_eq!(
            normalized_pointer_position((100, 50), &geometry(100, 50, 0, 100)),
            None,
        );
        assert_eq!(
            normalized_pointer_position(
                (i32::MAX - 1, 0),
                &geometry(i32::MIN, 0, u32::MAX as usize, 1),
            ),
            Some(NormalizedPoint {
                x: (u32::MAX as f64 - 1.0) / u32::MAX as f64,
                y: 0.0,
            }),
        );
    }
}
