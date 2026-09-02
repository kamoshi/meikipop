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
use crate::ocr::ocr::{DEFAULT_PROVIDER_ID, OcrProcessor, OcrProviderInfo};
use crate::platform::create_desktop_providers;
use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, FrameProvider};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineRuntimeConfig {
    pub ocr_provider: String,
}

impl Default for PipelineRuntimeConfig {
    fn default() -> Self {
        Self {
            ocr_provider: DEFAULT_PROVIDER_ID.to_owned(),
        }
    }
}

pub struct PipelineConfig {
    pub dictionary_path: PathBuf,
    pub screencast_token_path: PathBuf,
    pub max_dict_entries: usize,
    pub max_lookup_length: usize,
    pub show_kanji: bool,
    pub capture_interval: Duration,
    pub runtime: PipelineRuntimeConfig,
}

#[derive(Debug)]
pub enum PipelineEvent {
    CaptureReady,
    OcrProvidersChanged {
        providers: Vec<OcrProviderInfo>,
        active_provider: String,
        error: Option<String>,
    },
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
    popup_state: SharedPopupState,
    command_sender: Sender<PipelineCommand>,
    coordinator: Option<JoinHandle<()>>,
}

type SharedPopupState = Arc<Mutex<PopupState>>;

#[derive(Default)]
struct PopupState {
    /// The pipeline has emitted content which the frontend has not dismissed.
    expected_visible: bool,
    /// Bounds last confirmed by the frontend. They remain authoritative during
    /// delayed dismissal so neither capture nor hit testing can see through it.
    bounds: Option<CaptureGeometry>,
}

enum PipelineCommand {
    UpdateConfig(PipelineRuntimeConfig),
}

impl PopupState {
    fn capture_is_paused(&self) -> bool {
        self.expected_visible || self.bounds.is_some()
    }
}

impl Pipeline {
    pub fn start(config: PipelineConfig) -> Result<Self, std::io::Error> {
        Self::start_with_runner(
            move |running, popup_state, command_receiver, event_sender| {
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
                    popup_state,
                    command_receiver,
                    event_sender,
                );
            },
        )
    }

    pub fn start_with_providers(
        config: PipelineConfig,
        frame_provider: Box<dyn FrameProvider>,
        pointer_provider: Box<dyn PointerProvider>,
    ) -> Result<Self, std::io::Error> {
        Self::start_with_runner(
            move |running, popup_state, command_receiver, event_sender| {
                run_pipeline(
                    config,
                    frame_provider,
                    pointer_provider,
                    running,
                    popup_state,
                    command_receiver,
                    event_sender,
                );
            },
        )
    }

    fn start_with_runner(
        runner: impl FnOnce(
            Arc<AtomicBool>,
            SharedPopupState,
            Receiver<PipelineCommand>,
            Sender<PipelineEvent>,
        ) + Send
        + 'static,
    ) -> Result<Self, std::io::Error> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let running = Arc::new(AtomicBool::new(true));
        let popup_state = Arc::new(Mutex::new(PopupState::default()));
        let coordinator = {
            let running = Arc::clone(&running);
            let popup_state = Arc::clone(&popup_state);
            thread::Builder::new()
                .name("PipelineInit".to_owned())
                .spawn(move || runner(running, popup_state, command_receiver, event_sender))?
        };

        Ok(Self {
            event_receiver,
            running,
            popup_state,
            command_sender,
            coordinator: Some(coordinator),
        })
    }

    pub fn try_recv(&self) -> Option<PipelineEvent> {
        self.event_receiver.try_recv().ok()
    }

    /// Reports the popup's global desktop bounds. While present, capture is
    /// paused and pointer hits inside the popup are excluded in the core.
    pub fn set_popup_bounds(&self, bounds: Option<CaptureGeometry>) {
        let mut state = self
            .popup_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.expected_visible = bounds.is_some();
        state.bounds = bounds;
    }

    /// Applies frontend-owned runtime configuration on the workers which own
    /// the affected components.
    pub fn update_config(&self, config: PipelineRuntimeConfig) -> Result<(), &'static str> {
        self.command_sender
            .send(PipelineCommand::UpdateConfig(config))
            .map_err(|_| "OCR worker is no longer running")
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
    capture_geometry: Option<CaptureGeometry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LookupInput {
    pointer: (i32, i32),
    capture_geometry: Option<CaptureGeometry>,
    target_available: bool,
    source_generation: u64,
    ocr_sequence: Option<u64>,
    pointer_blocked_by_popup: bool,
}

struct OcrWorkItem {
    frame: CapturedFrame,
    pointer: PointerSnapshot,
}

struct RecognizedFrame {
    source_generation: u64,
    scan_sequence: u64,
    paragraphs: Vec<Paragraph>,
    capture_geometry: CaptureGeometry,
}

enum OcrOutput {
    Reset,
    Frame(RecognizedFrame),
}

fn run_pipeline(
    config: PipelineConfig,
    mut frame_provider: Box<dyn FrameProvider>,
    pointer_provider: Box<dyn PointerProvider>,
    running: Arc<AtomicBool>,
    popup_state: SharedPopupState,
    command_receiver: Receiver<PipelineCommand>,
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
    if let Some(geometry) = &capture_geometry {
        log::info!(
            "Selected capture source: left={}, top={}, width={}, height={}",
            geometry.left,
            geometry.top,
            geometry.width,
            geometry.height
        );
    } else {
        log::info!("No capture source is currently shared; OCR is idle");
    }
    let _ = event_sender.send(PipelineEvent::CaptureReady);

    let ocr_processor = match OcrProcessor::new_with_provider(Some(&config.runtime.ocr_provider)) {
        Ok(processor) => processor,
        Err(error) => {
            send_error(&event_sender, "Failed to initialize OCR", error);
            return;
        }
    };
    log::info!("OCR processor initialized");
    send_ocr_provider_state(&event_sender, &ocr_processor, None);

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
            capture_geometry,
            Arc::clone(&popup_state),
            Arc::clone(&latest_pointer),
            ocr_receiver,
            lookup_sender,
        ),
        spawn_capture_worker(
            Arc::clone(&running),
            Arc::clone(&popup_state),
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
            command_receiver,
            event_sender.clone(),
        ),
        spawn_lookup_worker(
            Arc::clone(&running),
            Arc::clone(&popup_state),
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
    popup_state: SharedPopupState,
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
                if popup_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .capture_is_paused()
                {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }

                match frame_provider.capture_frame() {
                    Ok(Some(frame)) => {
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
                    Ok(None) => {}
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
    ocr_sender: Sender<OcrOutput>,
    command_receiver: Receiver<PipelineCommand>,
    event_sender: Sender<PipelineEvent>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("OcrWorker".to_owned())
        .spawn(move || {
            let mut last_scan: Option<(u64, Arc<[u8]>, NormalizedPoint)> = None;
            let mut ocr_sequence = 0_u64;
            log::debug!("OCR worker started");
            while running.load(Ordering::Acquire) {
                while let Ok(command) = command_receiver.try_recv() {
                    match command {
                        PipelineCommand::UpdateConfig(config) => {
                            let switch_result = ocr_processor.switch_provider(&config.ocr_provider);
                            if matches!(&switch_result, Ok(true)) {
                                last_scan = None;
                                if !send_while_running(&ocr_sender, OcrOutput::Reset, &running) {
                                    return;
                                }
                            }
                            let error = switch_result.err().map(|error| error.to_string());
                            send_ocr_provider_state(&event_sender, &ocr_processor, error);
                        }
                    }
                }
                let work = match screenshot_receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(work) => work,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                if !work.pointer.target_available {
                    continue;
                }
                if work.pointer.source_generation != work.frame.source_generation {
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
                if last_scan
                    .as_ref()
                    .is_some_and(|(generation, raw, previous_focus)| {
                        *generation == work.frame.source_generation
                            && (Arc::ptr_eq(raw, &work.frame.screenshot.raw)
                                || raw == &work.frame.screenshot.raw)
                            && *previous_focus == focus_point
                    })
                {
                    continue;
                }

                let ocr_start = std::time::Instant::now();
                let result = (|| -> Result<Vec<Paragraph>, Box<dyn Error>> {
                    let image = work.frame.screenshot.to_rgb()?;
                    ocr_processor.scan_rgb(&image, context)?;
                    Ok(ocr_processor.take_last_result())
                })();

                match result {
                    Ok(paragraphs) => {
                        log::info!(
                            "OCR worker completed scan in {:?} (found {} text paragraph(s))",
                            ocr_start.elapsed(),
                            paragraphs.len()
                        );
                        // Only suppress an identical frame and focus point after OCR succeeded.
                        last_scan = Some((
                            work.frame.source_generation,
                            work.frame.screenshot.raw,
                            focus_point,
                        ));
                        // A cached source frame can be rescanned around a new
                        // pointer position. Give every OCR result its own
                        // identity so hit scanning observes that new result.
                        ocr_sequence = ocr_sequence.wrapping_add(1);
                        if !send_while_running(
                            &ocr_sender,
                            OcrOutput::Frame(RecognizedFrame {
                                source_generation: work.frame.source_generation,
                                scan_sequence: ocr_sequence,
                                paragraphs,
                                capture_geometry: work.frame.geometry,
                            }),
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
    capture_geometry: Option<CaptureGeometry>,
    popup_state: SharedPopupState,
    latest_pointer: Arc<Mutex<Option<PointerSnapshot>>>,
    ocr_receiver: Receiver<OcrOutput>,
    lookup_sender: Sender<LookupRequest>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("HitScannerWorker".to_owned())
        .spawn(move || {
            log::debug!("Mouse tracker and hit scanner worker started");
            let mut recognized_frame: Option<RecognizedFrame> = None;
            let mut previous_input: Option<LookupInput> = None;
            let mut last_pointer_error = None;

            while running.load(Ordering::Acquire) {
                while let Ok(output) = ocr_receiver.try_recv() {
                    match output {
                        OcrOutput::Reset => recognized_frame = None,
                        OcrOutput::Frame(latest_frame) => recognized_frame = Some(latest_frame),
                    }
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
                    .or_else(|| capture_geometry.clone());
                let ocr_sequence = recognized_frame.as_ref().map(|frame| frame.scan_sequence);
                let pointer_blocked_by_popup = popup_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .bounds
                    .as_ref()
                    .is_some_and(|bounds| bounds.contains(pointer.position));
                let current_input = LookupInput {
                    pointer: pointer.position,
                    capture_geometry: active_geometry.clone(),
                    target_available: pointer.target_available,
                    source_generation: pointer.source_generation,
                    ocr_sequence,
                    pointer_blocked_by_popup,
                };
                if previous_input.as_ref() == Some(&current_input) {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                previous_input = Some(current_input);

                if pointer_blocked_by_popup {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }

                let geometry_matches_ocr = recognized_frame.as_ref().is_some_and(|frame| {
                    pointer.source_generation == frame.source_generation
                        && active_geometry.as_ref().is_some_and(|geometry| {
                            frame.capture_geometry.width == geometry.width
                                && frame.capture_geometry.height == geometry.height
                        })
                });
                let lookup_string = (pointer.target_available && geometry_matches_ocr)
                    .then(|| {
                        normalized_pointer_position(pointer.position, active_geometry.as_ref()?)
                    })
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
    popup_state: SharedPopupState,
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

                if let (Some(lookup_string), Some(capture_geometry)) =
                    (request.lookup_string, request.capture_geometry)
                {
                    let result =
                        lookup_engine.lookup_cached(&lookup_string, max_lookup_length, show_kanji);
                    if !result.entries.is_empty() || result.kanji_entry.is_some() {
                        log::debug!(
                            "Sending lookup result at mouse ({}, {}) in capture source left={}, top={}, width={}, height={}",
                            request.mouse_x,
                            request.mouse_y,
                            capture_geometry.left,
                            capture_geometry.top,
                            capture_geometry.width,
                            capture_geometry.height
                        );
                        if event_sender
                            .send(PipelineEvent::LookupResult {
                                entries: result.entries,
                                kanji: result.kanji_entry,
                                mouse_x: request.mouse_x,
                                mouse_y: request.mouse_y,
                                capture_geometry,
                            })
                            .is_err()
                        {
                            break;
                        }
                        popup_state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .expected_visible = true;
                    } else {
                        hide_popup_if_visible(
                            &event_sender,
                            &popup_state,
                            request.mouse_x,
                            request.mouse_y,
                        );
                    }
                } else {
                    hide_popup_if_visible(
                        &event_sender,
                        &popup_state,
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
    popup_state: &Mutex<PopupState>,
    mouse_x: i32,
    mouse_y: i32,
) {
    let should_hide = {
        let mut state = popup_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let was_expected_visible = state.expected_visible;
        state.expected_visible = false;
        was_expected_visible
    };
    if should_hide {
        let _ = sender.send(PipelineEvent::HidePopup { mouse_x, mouse_y });
    }
}

fn send_ocr_provider_state(
    sender: &Sender<PipelineEvent>,
    processor: &OcrProcessor,
    error: Option<String>,
) {
    let _ = sender.send(PipelineEvent::OcrProvidersChanged {
        providers: OcrProcessor::available_providers(),
        active_provider: processor.active_provider_id().to_owned(),
        error,
    });
}

fn send_error(sender: &Sender<PipelineEvent>, context: &str, error: impl std::fmt::Display) {
    log::error!("{context}: {error}");
    let _ = sender.send(PipelineEvent::Error(format!("{context}: {error}")));
}

#[cfg(test)]
mod tests {
    use super::{LookupInput, PopupState, normalized_pointer_position};
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
        let previous = lookup_input(previous, true);
        let moved = lookup_input(moved, true);

        assert_ne!(previous, moved);
        assert_eq!(moved, moved.clone());
    }

    #[test]
    fn target_occlusion_invalidates_a_stationary_pointer_lookup() {
        let target = geometry(0, 0, 800, 600);

        assert_ne!(
            lookup_input(target.clone(), true),
            lookup_input(target, false)
        );
    }

    #[test]
    fn source_replacement_invalidates_identical_ocr_input() {
        let mut replacement = lookup_input(geometry(0, 0, 800, 600), true);
        let original = replacement.clone();
        replacement.source_generation += 1;

        assert_ne!(original, replacement);
    }

    #[test]
    fn rescanning_a_cached_frame_invalidates_the_lookup_input() {
        let mut rescanned = lookup_input(geometry(0, 0, 800, 600), true);
        let original = rescanned.clone();
        rescanned.ocr_sequence = Some(original.ocr_sequence.unwrap() + 1);

        assert_ne!(original, rescanned);
    }

    #[test]
    fn popup_exclusion_change_invalidates_a_stationary_pointer_lookup() {
        let mut blocked = lookup_input(geometry(0, 0, 800, 600), true);
        let unblocked = blocked.clone();
        blocked.pointer_blocked_by_popup = true;

        assert_ne!(unblocked, blocked);
    }

    #[test]
    fn confirmed_popup_bounds_keep_capture_paused_during_delayed_dismissal() {
        let mut state = PopupState {
            expected_visible: true,
            bounds: Some(geometry(10, 20, 300, 200)),
        };

        state.expected_visible = false;
        assert!(state.capture_is_paused());

        state.bounds = None;
        assert!(!state.capture_is_paused());
    }

    fn lookup_input(capture_geometry: CaptureGeometry, target_available: bool) -> LookupInput {
        LookupInput {
            pointer: (200, 150),
            capture_geometry: Some(capture_geometry),
            target_available,
            source_generation: 1,
            ocr_sequence: Some(4),
            pointer_blocked_by_popup: false,
        }
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
