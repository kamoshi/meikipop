use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::dictionary::lookup::{DictionaryEntry, KanjiEntry, LookupEngine};
use crate::input::interface::{PointerProvider, PointerSnapshot};
use crate::ocr::hit_scan::hit_scan;
use crate::ocr::interface::{NormalizedPoint, Paragraph};
use crate::ocr::ocr::{DEFAULT_PROVIDER_ID, OcrProcessor, OcrProviderInfo};
use crate::platform::create_desktop_providers;
use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, FrameProvider};
use crate::utils::latest_mailbox::{
    LatestMailbox, RecvTimeoutError as MailboxRecvTimeoutError, TryRecvError as MailboxTryRecvError,
};

const OCR_MINIMUM_INTERVAL: Duration = Duration::from_secs(1);
const OCR_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    shared: SharedPipelineState,
    command_sender: Sender<PipelineCommand>,
    coordinator: Option<JoinHandle<()>>,
}

type SharedPopupState = Arc<Mutex<PopupState>>;

#[derive(Clone)]
struct SharedPipelineState {
    running: Arc<AtomicBool>,
    popup: SharedPopupState,
    freshness: SharedFreshness,
}

#[derive(Default)]
struct PopupState {
    /// The pipeline has emitted content which the frontend has not dismissed.
    expected_visible: bool,
    /// A dismissal request has been emitted and is awaiting a frontend decision.
    dismissal_pending: bool,
    /// Bounds last confirmed by the frontend. They remain authoritative during
    /// delayed dismissal so neither capture nor hit testing can see through it.
    bounds: Option<CaptureGeometry>,
}

#[derive(Debug, PartialEq, Eq)]
enum PipelineCommand {
    UpdateConfig {
        revision: ConfigRevision,
        config: PipelineRuntimeConfig,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ConfigRevision(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScanRevision {
    config: ConfigRevision,
    source_generation: u64,
    frame_sequence: u64,
    scan_sequence: u64,
}

#[derive(Clone)]
struct LatestFrameIdentity {
    source_generation: u64,
    frame_sequence: u64,
    raw: Arc<[u8]>,
    pixel_size: (usize, usize),
    geometry: CaptureGeometry,
}

impl LatestFrameIdentity {
    fn from_frame(frame: &CapturedFrame) -> Self {
        Self {
            source_generation: frame.source_generation,
            frame_sequence: frame.sequence,
            raw: Arc::clone(&frame.screenshot.raw),
            pixel_size: frame.screenshot.size(),
            geometry: frame.geometry.clone(),
        }
    }
}

struct FreshnessState {
    config_revision: ConfigRevision,
    desired_runtime: PipelineRuntimeConfig,
    latest_frame: Option<LatestFrameIdentity>,
    latest_pointer: Option<PointerSnapshot>,
    latest_lookup_revision: u64,
}

type SharedFreshness = Arc<Mutex<FreshnessState>>;

impl PopupState {
    fn capture_is_paused(&self) -> bool {
        self.expected_visible || self.bounds.is_some()
    }

    fn update_bounds(&mut self, bounds: Option<CaptureGeometry>) {
        self.expected_visible = bounds.is_some();
        if bounds.is_none() {
            self.dismissal_pending = false;
        }
        self.bounds = bounds;
    }

    fn mark_content_available(&mut self) {
        self.expected_visible = true;
        self.dismissal_pending = false;
    }

    fn request_dismissal(&mut self) -> bool {
        if !self.expected_visible || self.dismissal_pending {
            return false;
        }
        self.dismissal_pending = true;
        true
    }

    fn pointer_is_blocked(&mut self, pointer: (i32, i32)) -> bool {
        let blocked = self
            .bounds
            .as_ref()
            .is_some_and(|bounds| bounds.contains(pointer));
        if blocked {
            // Entering the popup rejects a pending dismissal. Once the pointer
            // leaves, the next lookup miss can request dismissal again.
            self.dismissal_pending = false;
        }
        blocked
    }
}

impl Pipeline {
    pub fn start(config: PipelineConfig) -> Result<Self, std::io::Error> {
        let initial_runtime = config.runtime.clone();
        Self::start_with_runner(
            initial_runtime,
            move |shared, command_receiver, event_sender| {
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
                    shared,
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
        let initial_runtime = config.runtime.clone();
        Self::start_with_runner(
            initial_runtime,
            move |shared, command_receiver, event_sender| {
                run_pipeline(
                    config,
                    frame_provider,
                    pointer_provider,
                    shared,
                    command_receiver,
                    event_sender,
                );
            },
        )
    }

    fn start_with_runner(
        initial_runtime: PipelineRuntimeConfig,
        runner: impl FnOnce(SharedPipelineState, Receiver<PipelineCommand>, Sender<PipelineEvent>)
        + Send
        + 'static,
    ) -> Result<Self, std::io::Error> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let shared = SharedPipelineState {
            running: Arc::new(AtomicBool::new(true)),
            popup: Arc::new(Mutex::new(PopupState::default())),
            freshness: Arc::new(Mutex::new(FreshnessState {
                config_revision: ConfigRevision::default(),
                desired_runtime: initial_runtime,
                latest_frame: None,
                latest_pointer: None,
                latest_lookup_revision: 0,
            })),
        };
        let coordinator = {
            let shared = shared.clone();
            let running = Arc::clone(&shared.running);
            thread::Builder::new()
                .name("PipelineInit".to_owned())
                .spawn(move || {
                    runner(shared, command_receiver, event_sender);
                    running.store(false, Ordering::Release);
                })?
        };

        Ok(Self {
            event_receiver,
            shared,
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
            .shared
            .popup
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.update_bounds(bounds);
    }

    /// Applies frontend-owned runtime configuration on the workers which own
    /// the affected components.
    pub fn update_config(&self, config: PipelineRuntimeConfig) -> Result<(), &'static str> {
        if !self.shared.running.load(Ordering::Acquire) {
            return Err("OCR worker is no longer running");
        }
        let revision = {
            let mut freshness = self
                .shared
                .freshness
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if freshness.desired_runtime == config {
                return Ok(());
            }
            freshness.config_revision.0 = freshness.config_revision.0.wrapping_add(1);
            freshness.desired_runtime = config.clone();
            freshness.config_revision
        };
        self.command_sender
            .send(PipelineCommand::UpdateConfig { revision, config })
            .map_err(|_| "OCR worker is no longer running")
    }

    pub fn shutdown(&mut self) {
        self.shared.running.store(false, Ordering::Release);
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
    revision: u64,
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

/// Limits expensive provider calls without slowing capture, cached-result reuse,
/// pointer tracking, or shutdown/configuration checks.
struct OcrRateLimiter {
    minimum_interval: Duration,
    last_started_at: Option<Instant>,
}

impl OcrRateLimiter {
    fn new(minimum_interval: Duration) -> Self {
        Self {
            minimum_interval,
            last_started_at: None,
        }
    }

    fn remaining_at(&self, now: Instant) -> Duration {
        self.last_started_at.map_or(Duration::ZERO, |started_at| {
            self.minimum_interval
                .saturating_sub(now.saturating_duration_since(started_at))
        })
    }

    fn record_start(&mut self, now: Instant) {
        self.last_started_at = Some(now);
    }
}

#[derive(Clone)]
struct RecognizedFrame {
    revision: ScanRevision,
    input_raw: Arc<[u8]>,
    pixel_size: (usize, usize),
    paragraphs: Vec<Paragraph>,
    capture_geometry: CaptureGeometry,
}

enum OcrOutput {
    Reset,
    Frame(RecognizedFrame),
}

/// Closes the next stage even when a worker returns early or unwinds.
struct CloseMailboxOnDrop<T>(Arc<LatestMailbox<T>>);

impl<T> Drop for CloseMailboxOnDrop<T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A pipeline cannot make progress safely after one of its workers exits.
/// Propagate unexpected termination instead of leaving sibling workers alive.
struct StopPipelineOnDrop(Arc<AtomicBool>);

impl Drop for StopPipelineOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn ocr_work_is_current(
    work: &OcrWorkItem,
    config_revision: ConfigRevision,
    freshness: &SharedFreshness,
) -> bool {
    let (config_matches, latest_frame, latest_pointer) = {
        let state = freshness.lock().unwrap_or_else(|error| error.into_inner());
        (
            state.config_revision == config_revision,
            state.latest_frame.clone(),
            state.latest_pointer.clone(),
        )
    };
    config_matches
        && latest_frame.is_some_and(|latest| {
            latest.source_generation == work.frame.source_generation
                && latest.pixel_size == work.frame.screenshot.size()
                && latest.geometry.width == work.frame.geometry.width
                && latest.geometry.height == work.frame.geometry.height
                && (latest.frame_sequence == work.frame.sequence
                    || Arc::ptr_eq(&latest.raw, &work.frame.screenshot.raw)
                    || latest.raw.as_ref() == work.frame.screenshot.raw.as_ref())
        })
        && latest_pointer.is_some_and(|pointer| {
            pointer.target_available && pointer.source_generation == work.frame.source_generation
        })
}

/// A newer sample or pointer position does not make an already completed OCR
/// result unusable. Hit scanning intentionally applies the current pointer to
/// the newest installed recognition, as the original pipeline did. Reject only
/// hard boundaries that cannot safely share OCR coordinates or provider state.
fn recognized_frame_is_publishable(frame: &RecognizedFrame, freshness: &SharedFreshness) -> bool {
    let state = freshness.lock().unwrap_or_else(|error| error.into_inner());
    state.config_revision == frame.revision.config
        && state.latest_frame.as_ref().is_some_and(|latest| {
            latest.source_generation == frame.revision.source_generation
                && latest.geometry.width == frame.capture_geometry.width
                && latest.geometry.height == frame.capture_geometry.height
        })
        && state.latest_pointer.as_ref().is_some_and(|pointer| {
            pointer.target_available
                && pointer.source_generation == frame.revision.source_generation
        })
}

fn lookup_request_is_current(revision: u64, freshness: &SharedFreshness) -> bool {
    freshness
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .latest_lookup_revision
        == revision
}

fn take_latest_command(receiver: &Receiver<PipelineCommand>) -> Option<PipelineCommand> {
    let mut latest = None;
    while let Ok(command) = receiver.try_recv() {
        latest = Some(command);
    }
    latest
}

fn run_pipeline(
    config: PipelineConfig,
    mut frame_provider: Box<dyn FrameProvider>,
    pointer_provider: Box<dyn PointerProvider>,
    shared: SharedPipelineState,
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

    // These are state streams, not event streams. Each stage only needs the
    // newest pending value, so producers replace obsolete work without waiting.
    let screenshot_mailbox = Arc::new(LatestMailbox::new());
    let ocr_mailbox = Arc::new(LatestMailbox::new());
    let lookup_mailbox = Arc::new(LatestMailbox::new());
    let workers = vec![
        spawn_hit_scan_worker(
            shared.clone(),
            pointer_provider,
            capture_geometry,
            Arc::clone(&ocr_mailbox),
            Arc::clone(&lookup_mailbox),
        ),
        spawn_capture_worker(
            shared.clone(),
            frame_provider,
            config.capture_interval,
            Arc::clone(&screenshot_mailbox),
        ),
        spawn_ocr_worker(
            shared.clone(),
            ocr_processor,
            screenshot_mailbox,
            ocr_mailbox,
            command_receiver,
            event_sender.clone(),
        ),
        spawn_lookup_worker(
            shared,
            lookup_engine,
            config.max_lookup_length,
            config.show_kanji,
            lookup_mailbox,
            event_sender,
        ),
    ];

    for worker in workers {
        let _ = worker.join();
    }
}

fn spawn_capture_worker(
    shared: SharedPipelineState,
    frame_provider: Box<dyn FrameProvider>,
    capture_interval: Duration,
    screenshot_mailbox: Arc<LatestMailbox<OcrWorkItem>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ScreencastWorker".to_owned())
        .spawn(move || {
            run_capture_worker(
                &shared,
                frame_provider,
                capture_interval,
                screenshot_mailbox,
            )
        })
        .expect("failed to spawn ScreencastWorker")
}

fn run_capture_worker(
    shared: &SharedPipelineState,
    mut frame_provider: Box<dyn FrameProvider>,
    capture_interval: Duration,
    screenshot_mailbox: Arc<LatestMailbox<OcrWorkItem>>,
) {
    let _stop_pipeline = StopPipelineOnDrop(Arc::clone(&shared.running));
    let _close_output = CloseMailboxOnDrop(Arc::clone(&screenshot_mailbox));
    log::debug!("Screencast worker started");
    while shared.running.load(Ordering::Acquire) {
        if shared
            .popup
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capture_is_paused()
        {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        match frame_provider.capture_frame() {
            Ok(Some(frame)) => {
                let pointer = {
                    let mut freshness = shared
                        .freshness
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    freshness.latest_frame = Some(LatestFrameIdentity::from_frame(&frame));
                    freshness.latest_pointer.clone()
                };
                if let Some(pointer) = pointer {
                    match screenshot_mailbox.send_replace(OcrWorkItem { frame, pointer }) {
                        Ok(Some(_)) => log::trace!("Replaced obsolete pending OCR input"),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            }
            Ok(None) => {}
            Err(error) => log::error!("Screencast worker error: {error}"),
        }
        thread::sleep(capture_interval);
    }
    log::debug!("Screencast worker stopped");
}

fn spawn_ocr_worker(
    shared: SharedPipelineState,
    ocr_processor: OcrProcessor,
    screenshot_mailbox: Arc<LatestMailbox<OcrWorkItem>>,
    ocr_mailbox: Arc<LatestMailbox<OcrOutput>>,
    command_receiver: Receiver<PipelineCommand>,
    event_sender: Sender<PipelineEvent>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("OcrWorker".to_owned())
        .spawn(move || {
            run_ocr_worker(
                &shared,
                ocr_processor,
                screenshot_mailbox,
                ocr_mailbox,
                &command_receiver,
                &event_sender,
            )
        })
        .expect("failed to spawn OcrWorker")
}

fn run_ocr_worker(
    shared: &SharedPipelineState,
    mut ocr_processor: OcrProcessor,
    screenshot_mailbox: Arc<LatestMailbox<OcrWorkItem>>,
    ocr_mailbox: Arc<LatestMailbox<OcrOutput>>,
    command_receiver: &Receiver<PipelineCommand>,
    event_sender: &Sender<PipelineEvent>,
) {
    let _stop_pipeline = StopPipelineOnDrop(Arc::clone(&shared.running));
    let _close_output = CloseMailboxOnDrop(Arc::clone(&ocr_mailbox));
    let mut cached_recognition: Option<RecognizedFrame> = None;
    let mut active_config_revision = ConfigRevision::default();
    let mut scan_sequence = 0_u64;
    let mut pending_work: Option<OcrWorkItem> = None;
    let mut rate_limiter = OcrRateLimiter::new(OCR_MINIMUM_INTERVAL);
    log::debug!("OCR worker started");

    while shared.running.load(Ordering::Acquire) {
        // Runtime configuration is desired state. Applying only the most recent
        // command avoids initializing providers that have already been superseded.
        if let Some(PipelineCommand::UpdateConfig { revision, config }) =
            take_latest_command(command_receiver)
        {
            let switch_result = ocr_processor.switch_provider(&config.ocr_provider);
            active_config_revision = revision;
            cached_recognition = None;
            if ocr_mailbox.send_replace(OcrOutput::Reset).is_err() {
                break;
            }
            let error = switch_result.err().map(|error| error.to_string());
            send_ocr_provider_state(event_sender, &ocr_processor, error);
        }

        let work = match pending_work.take() {
            Some(pending) => match screenshot_mailbox.try_recv_latest() {
                Ok(latest) => latest.value,
                Err(MailboxTryRecvError::Empty) => pending,
                Err(MailboxTryRecvError::Closed) => break,
            },
            None => match screenshot_mailbox.recv_latest_timeout(OCR_WORKER_POLL_INTERVAL) {
                Ok(item) => item.value,
                Err(MailboxRecvTimeoutError::Timeout) => continue,
                Err(MailboxRecvTimeoutError::Closed) => break,
            },
        };

        if !work.pointer.target_available
            || work.pointer.source_generation != work.frame.source_generation
        {
            continue;
        }
        if shared
            .freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .config_revision
            != active_config_revision
        {
            continue;
        }
        if let Some(cached) = cached_recognition.as_ref().filter(|cached| {
            cached.revision.config == active_config_revision
                && cached.revision.source_generation == work.frame.source_generation
                && cached.pixel_size == work.frame.screenshot.size()
                && (Arc::ptr_eq(&cached.input_raw, &work.frame.screenshot.raw)
                    || cached.input_raw.as_ref() == work.frame.screenshot.raw.as_ref())
        }) {
            let already_published = cached.revision.frame_sequence == work.frame.sequence
                && cached.capture_geometry == work.frame.geometry;
            let mut reused = cached.clone();
            reused.revision.frame_sequence = work.frame.sequence;
            reused.input_raw = Arc::clone(&work.frame.screenshot.raw);
            reused.capture_geometry = work.frame.geometry.clone();
            if !already_published
                && recognized_frame_is_publishable(&reused, &shared.freshness)
                && ocr_mailbox.send_replace(OcrOutput::Frame(reused)).is_err()
            {
                break;
            }
            continue;
        }

        if !ocr_work_is_current(&work, active_config_revision, &shared.freshness) {
            log::debug!("Skipped OCR input superseded before recognition started");
            continue;
        }

        let remaining = rate_limiter.remaining_at(Instant::now());
        if !remaining.is_zero() {
            pending_work = Some(work);
            match screenshot_mailbox.recv_latest_timeout(remaining.min(OCR_WORKER_POLL_INTERVAL)) {
                Ok(latest) => pending_work = Some(latest.value),
                Err(MailboxRecvTimeoutError::Timeout) => {}
                Err(MailboxRecvTimeoutError::Closed) => break,
            }
            continue;
        }

        scan_sequence = scan_sequence.wrapping_add(1);
        let revision = ScanRevision {
            config: active_config_revision,
            source_generation: work.frame.source_generation,
            frame_sequence: work.frame.sequence,
            scan_sequence,
        };
        let mut recognized = RecognizedFrame {
            revision,
            input_raw: Arc::clone(&work.frame.screenshot.raw),
            pixel_size: work.frame.screenshot.size(),
            paragraphs: Vec::new(),
            capture_geometry: work.frame.geometry.clone(),
        };
        let ocr_start = Instant::now();
        rate_limiter.record_start(ocr_start);
        let result = (|| -> Result<Vec<Paragraph>, Box<dyn Error>> {
            let image = work.frame.screenshot.to_rgb()?;
            ocr_processor.scan(&image)
        })();

        match result {
            Ok(paragraphs) => {
                log::info!(
                    "OCR worker completed scan in {:?} (found {} text paragraph(s))",
                    ocr_start.elapsed(),
                    paragraphs.len()
                );
                recognized.paragraphs = paragraphs;
                if !recognized_frame_is_publishable(&recognized, &shared.freshness) {
                    log::debug!("Discarded OCR result superseded while recognition was running");
                    continue;
                }
                cached_recognition = Some(recognized.clone());
                if ocr_mailbox
                    .send_replace(OcrOutput::Frame(recognized))
                    .is_err()
                {
                    break;
                }
            }
            Err(error) => log::error!("OCR worker error: {error}"),
        }
    }
    log::debug!("OCR worker stopped");
}

fn spawn_hit_scan_worker(
    shared: SharedPipelineState,
    pointer_provider: Box<dyn PointerProvider>,
    capture_geometry: Option<CaptureGeometry>,
    ocr_mailbox: Arc<LatestMailbox<OcrOutput>>,
    lookup_mailbox: Arc<LatestMailbox<LookupRequest>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("HitScannerWorker".to_owned())
        .spawn(move || {
            run_hit_scan_worker(
                &shared,
                pointer_provider,
                capture_geometry,
                ocr_mailbox,
                lookup_mailbox,
            )
        })
        .expect("failed to spawn HitScannerWorker")
}

fn run_hit_scan_worker(
    shared: &SharedPipelineState,
    mut pointer_provider: Box<dyn PointerProvider>,
    capture_geometry: Option<CaptureGeometry>,
    ocr_mailbox: Arc<LatestMailbox<OcrOutput>>,
    lookup_mailbox: Arc<LatestMailbox<LookupRequest>>,
) {
    let _stop_pipeline = StopPipelineOnDrop(Arc::clone(&shared.running));
    let _close_output = CloseMailboxOnDrop(Arc::clone(&lookup_mailbox));
    log::debug!("Mouse tracker and hit scanner worker started");
    let mut recognized_frame: Option<RecognizedFrame> = None;
    let mut previous_input: Option<LookupInput> = None;
    let mut last_pointer_error = None;
    let mut lookup_revision = 0_u64;

    while shared.running.load(Ordering::Acquire) {
        match ocr_mailbox.try_recv_latest() {
            Ok(item) => match item.value {
                OcrOutput::Reset => recognized_frame = None,
                OcrOutput::Frame(frame) => recognized_frame = Some(frame),
            },
            Err(MailboxTryRecvError::Empty) => {}
            Err(MailboxTryRecvError::Closed) => break,
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
        shared
            .freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_pointer = Some(pointer.clone());
        let recognition_is_current = recognized_frame
            .as_ref()
            .is_some_and(|frame| recognized_frame_is_publishable(frame, &shared.freshness));

        let active_geometry = pointer
            .capture_geometry
            .clone()
            .or_else(|| capture_geometry.clone());
        let ocr_sequence = if recognition_is_current {
            recognized_frame
                .as_ref()
                .map(|frame| frame.revision.scan_sequence)
        } else {
            None
        };
        let pointer_blocked_by_popup = shared
            .popup
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pointer_is_blocked(pointer.position);
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
        lookup_revision = lookup_revision.wrapping_add(1);
        shared
            .freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_lookup_revision = lookup_revision;

        if pointer_blocked_by_popup {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        let geometry_matches_ocr = recognition_is_current
            && recognized_frame.as_ref().is_some_and(|frame| {
                pointer.source_generation == frame.revision.source_generation
                    && active_geometry.as_ref().is_some_and(|geometry| {
                        frame.capture_geometry.width == geometry.width
                            && frame.capture_geometry.height == geometry.height
                    })
            });
        let lookup_string = (pointer.target_available && geometry_matches_ocr)
            .then(|| normalized_pointer_position(pointer.position, active_geometry.as_ref()?))
            .flatten()
            .and_then(|point| hit_scan(&recognized_frame.as_ref()?.paragraphs, point.x, point.y));

        if lookup_mailbox
            .send_replace(LookupRequest {
                revision: lookup_revision,
                lookup_string,
                mouse_x: pointer.position.0,
                mouse_y: pointer.position.1,
                capture_geometry: active_geometry,
            })
            .is_err()
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    log::debug!("Mouse tracker and hit scanner worker stopped");
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
    shared: SharedPipelineState,
    mut lookup_engine: LookupEngine,
    max_lookup_length: usize,
    show_kanji: bool,
    lookup_mailbox: Arc<LatestMailbox<LookupRequest>>,
    event_sender: Sender<PipelineEvent>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("LookupWorker".to_owned())
        .spawn(move || {
            run_lookup_worker(
                &shared,
                &mut lookup_engine,
                max_lookup_length,
                show_kanji,
                &lookup_mailbox,
                &event_sender,
            )
        })
        .expect("failed to spawn LookupWorker")
}

fn run_lookup_worker(
    shared: &SharedPipelineState,
    lookup_engine: &mut LookupEngine,
    max_lookup_length: usize,
    show_kanji: bool,
    lookup_mailbox: &LatestMailbox<LookupRequest>,
    event_sender: &Sender<PipelineEvent>,
) {
    let _stop_pipeline = StopPipelineOnDrop(Arc::clone(&shared.running));
    log::debug!("Lookup worker started");
    while shared.running.load(Ordering::Acquire) {
        let Some(item) = lookup_mailbox.recv_latest() else {
            break;
        };
        let request = item.value;
        if !lookup_request_is_current(request.revision, &shared.freshness) {
            continue;
        }

        let prepared_text = request
            .lookup_string
            .as_deref()
            .map(|text| lookup_engine.prepare_lookup_text(text, max_lookup_length))
            .filter(|text| !text.is_empty());

        if let (Some(lookup_string), Some(capture_geometry)) =
            (prepared_text, request.capture_geometry)
        {
            let result = lookup_engine.lookup_cached(&lookup_string, max_lookup_length, show_kanji);
            let current = shared
                .freshness
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if current.latest_lookup_revision != request.revision {
                log::debug!("Discarded dictionary result superseded during lookup");
                continue;
            }
            if !result.entries.is_empty() || result.kanji_entry.is_some() {
                shared
                    .popup
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .mark_content_available();
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
            } else {
                hide_popup_if_visible(
                    event_sender,
                    &shared.popup,
                    request.mouse_x,
                    request.mouse_y,
                );
            }
        } else {
            let current = shared
                .freshness
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if current.latest_lookup_revision != request.revision {
                continue;
            }
            hide_popup_if_visible(
                event_sender,
                &shared.popup,
                request.mouse_x,
                request.mouse_y,
            );
        }
    }
    log::debug!("Lookup worker stopped");
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
        state.request_dismissal()
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        ConfigRevision, FreshnessState, LatestFrameIdentity, LookupInput, OcrRateLimiter,
        PipelineCommand, PipelineRuntimeConfig, PopupState, RecognizedFrame, ScanRevision,
        StopPipelineOnDrop, normalized_pointer_position, ocr_work_is_current,
        recognized_frame_is_publishable, take_latest_command,
    };
    use crate::ocr::interface::NormalizedPoint;
    use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, Screenshot};

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
    fn pointer_movement_refreshes_popup_state_for_the_same_ocr_result() {
        let original = lookup_input(geometry(0, 0, 800, 600), true);
        let mut moved = original.clone();
        moved.pointer.0 += 1;

        assert_eq!(original.ocr_sequence, moved.ocr_sequence);
        assert_ne!(original, moved);
    }

    #[test]
    fn dismissal_stays_pending_until_the_frontend_confirms_the_popup_is_hidden() {
        let mut state = PopupState::default();
        state.mark_content_available();
        state.update_bounds(Some(geometry(10, 20, 300, 200)));

        assert!(state.request_dismissal());
        assert!(!state.request_dismissal());
        assert!(state.capture_is_paused());

        state.update_bounds(None);
        assert!(!state.capture_is_paused());
        assert!(!state.dismissal_pending);
    }

    #[test]
    fn entering_the_popup_cancels_a_pending_dismissal() {
        let mut state = PopupState::default();
        state.mark_content_available();
        state.update_bounds(Some(geometry(10, 20, 300, 200)));
        assert!(state.request_dismissal());

        assert!(state.pointer_is_blocked((20, 30)));
        assert!(!state.dismissal_pending);
        assert!(state.request_dismissal());
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

    fn recognized(raw: Arc<[u8]>, frame_sequence: u64) -> RecognizedFrame {
        RecognizedFrame {
            revision: ScanRevision {
                config: ConfigRevision(0),
                source_generation: 1,
                frame_sequence,
                scan_sequence: 1,
            },
            input_raw: raw,
            pixel_size: (1, 1),
            paragraphs: Vec::new(),
            capture_geometry: geometry(0, 0, 100, 100),
        }
    }

    fn pending_work(raw: Arc<[u8]>, frame_sequence: u64) -> super::OcrWorkItem {
        super::OcrWorkItem {
            frame: CapturedFrame {
                source_generation: 1,
                sequence: frame_sequence,
                screenshot: Screenshot {
                    raw,
                    width: 1,
                    height: 1,
                },
                geometry: geometry(10, 20, 100, 100),
            },
            pointer: crate::input::interface::PointerSnapshot {
                position: (60, 70),
                capture_geometry: Some(geometry(10, 20, 100, 100)),
                source_generation: 1,
                target_available: true,
            },
        }
    }

    fn freshness(raw: Arc<[u8]>, frame_sequence: u64) -> Arc<Mutex<FreshnessState>> {
        Arc::new(Mutex::new(FreshnessState {
            config_revision: ConfigRevision(0),
            desired_runtime: PipelineRuntimeConfig::default(),
            latest_frame: Some(LatestFrameIdentity {
                source_generation: 1,
                frame_sequence,
                raw,
                pixel_size: (1, 1),
                geometry: geometry(10, 20, 100, 100),
            }),
            latest_pointer: Some(crate::input::interface::PointerSnapshot {
                position: (60, 70),
                capture_geometry: Some(geometry(10, 20, 100, 100)),
                source_generation: 1,
                target_available: true,
            }),
            latest_lookup_revision: 0,
        }))
    }

    #[test]
    fn newer_equal_pixels_do_not_block_completed_ocr() {
        let pixels: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
        let frame = recognized(Arc::clone(&pixels), 1);
        let freshness = freshness(Arc::from([1, 2, 3, 4]), 2);

        assert!(recognized_frame_is_publishable(&frame, &freshness));
    }

    #[test]
    fn newer_changed_pixels_do_not_starve_completed_ocr() {
        let frame = recognized(Arc::from([1, 2, 3, 4]), 1);
        let freshness = freshness(Arc::from([4, 3, 2, 1]), 2);

        assert!(recognized_frame_is_publishable(&frame, &freshness));
    }

    #[test]
    fn configuration_change_invalidates_in_flight_ocr() {
        let pixels: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
        let frame = recognized(Arc::clone(&pixels), 1);
        let freshness = freshness(pixels, 1);
        freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .config_revision = ConfigRevision(1);

        assert!(!recognized_frame_is_publishable(&frame, &freshness));
    }

    #[test]
    fn pointer_movement_uses_completed_ocr_for_current_hit_testing() {
        let pixels: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
        let frame = recognized(Arc::clone(&pixels), 1);
        let freshness = freshness(pixels, 1);
        freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_pointer
            .as_mut()
            .unwrap()
            .position = (70, 70);

        assert!(recognized_frame_is_publishable(&frame, &freshness));
    }

    #[test]
    fn pointer_movement_does_not_invalidate_pending_full_frame_ocr() {
        let pixels: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
        let work = pending_work(Arc::clone(&pixels), 1);
        let freshness = freshness(pixels, 1);
        freshness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_pointer
            .as_mut()
            .unwrap()
            .position = (70, 70);

        assert!(ocr_work_is_current(&work, ConfigRevision(0), &freshness));
    }

    #[test]
    fn source_replacement_rejects_completed_ocr() {
        let pixels: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
        let frame = recognized(Arc::clone(&pixels), 1);
        let freshness = freshness(pixels, 1);
        let mut state = freshness.lock().unwrap_or_else(|error| error.into_inner());
        state.latest_frame.as_mut().unwrap().source_generation = 2;
        state.latest_pointer.as_mut().unwrap().source_generation = 2;
        drop(state);

        assert!(!recognized_frame_is_publishable(&frame, &freshness));
    }

    #[test]
    fn rapid_configuration_changes_apply_only_the_latest_desired_state() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        for revision in 1..=3 {
            sender
                .send(PipelineCommand::UpdateConfig {
                    revision: ConfigRevision(revision),
                    config: PipelineRuntimeConfig {
                        ocr_provider: format!("provider-{revision}"),
                    },
                })
                .unwrap();
        }

        assert_eq!(
            take_latest_command(&receiver),
            Some(PipelineCommand::UpdateConfig {
                revision: ConfigRevision(3),
                config: PipelineRuntimeConfig {
                    ocr_provider: "provider-3".to_owned(),
                },
            })
        );
        assert_eq!(take_latest_command(&receiver), None);
    }

    #[test]
    fn ocr_rate_limiter_allows_only_one_start_per_interval() {
        let interval = Duration::from_secs(1);
        let started_at = std::time::Instant::now();
        let mut limiter = OcrRateLimiter::new(interval);

        assert_eq!(limiter.remaining_at(started_at), Duration::ZERO);
        limiter.record_start(started_at);
        assert_eq!(
            limiter.remaining_at(started_at + Duration::from_millis(250)),
            Duration::from_millis(750)
        );
        assert_eq!(limiter.remaining_at(started_at + interval), Duration::ZERO);
    }

    #[test]
    fn a_worker_exit_stops_the_rest_of_the_pipeline() {
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        {
            let _guard = StopPipelineOnDrop(Arc::clone(&running));
        }

        assert!(!running.load(std::sync::atomic::Ordering::Acquire));
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
