use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, Sender};

use crate::dictionary::lookup::{DictionaryEntry, KanjiEntry, LookupEngine};
use crate::input::interface::PointerProvider;
use crate::ocr::hit_scan::hit_scan;
use crate::ocr::interface::Paragraph;
use crate::ocr::ocr::OcrProcessor;
use crate::platform::create_desktop_providers;
use crate::screenshot::interface::{FrameProvider, Monitor, Screenshot};

pub struct PipelineConfig {
    pub dictionary_path: PathBuf,
    pub screencast_token_path: PathBuf,
    pub monitor_index: usize,
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
        monitor: Monitor,
    },
    HidePopup,
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
    let monitor = match frame_provider
        .monitors()
        .map_err(|error| error.to_string())
        .and_then(|monitors| {
            monitors
                .get(config.monitor_index)
                .cloned()
                .ok_or_else(|| "No monitor found".to_owned())
        }) {
        Ok(monitor) => monitor,
        Err(error) => {
            send_error(&event_sender, "Failed to get monitor", error);
            return;
        }
    };
    log::info!(
        "Selected scan monitor: left={}, top={}, width={}, height={}",
        monitor.left,
        monitor.top,
        monitor.width,
        monitor.height
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

    let workers = vec![
        spawn_capture_worker(
            Arc::clone(&running),
            Arc::clone(&popup_is_visible),
            frame_provider,
            monitor.clone(),
            config.capture_interval,
            screenshot_sender,
        ),
        spawn_ocr_worker(
            Arc::clone(&running),
            ocr_processor,
            screenshot_receiver,
            ocr_sender,
        ),
        spawn_hit_scan_worker(
            Arc::clone(&running),
            pointer_provider,
            monitor.clone(),
            ocr_receiver,
            lookup_sender,
        ),
        spawn_lookup_worker(
            Arc::clone(&running),
            Arc::clone(&popup_is_visible),
            lookup_engine,
            monitor,
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
    monitor: Monitor,
    capture_interval: Duration,
    screenshot_sender: Sender<Screenshot>,
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

                match frame_provider.frame(&monitor) {
                    Ok(screenshot) => {
                        if !send_while_running(&screenshot_sender, screenshot, &running) {
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
    screenshot_receiver: Receiver<Screenshot>,
    ocr_sender: Sender<Vec<Paragraph>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("OcrWorker".to_owned())
        .spawn(move || {
            let mut last_raw_frame: Option<Vec<u8>> = None;
            log::debug!("OCR worker started");
            while running.load(Ordering::Acquire) {
                let screenshot = match screenshot_receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(screenshot) => screenshot,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                if last_raw_frame.as_ref() == Some(&screenshot.raw) {
                    continue;
                }

                let ocr_start = std::time::Instant::now();
                let result = (|| -> Result<Vec<Paragraph>, Box<dyn Error>> {
                    let image = screenshot.to_rgb()?;
                    ocr_processor.scan_rgb(&image)?;
                    Ok(ocr_processor.last_ocr_result.take().unwrap_or_default())
                })();

                match result {
                    Ok(paragraphs) => {
                        log::info!(
                            "OCR worker completed scan in {:?} (found {} text paragraph(s))",
                            ocr_start.elapsed(),
                            paragraphs.len()
                        );
                        // Only suppress an identical frame after OCR succeeded.
                        last_raw_frame = Some(screenshot.raw);
                        if !send_while_running(&ocr_sender, paragraphs, &running) {
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
    monitor: Monitor,
    ocr_receiver: Receiver<Vec<Paragraph>>,
    lookup_sender: Sender<LookupRequest>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("HitScannerWorker".to_owned())
        .spawn(move || {
            log::debug!("Mouse tracker and hit scanner worker started");
            let mut paragraphs = Vec::new();
            let mut last_mouse_pos = None;

            while running.load(Ordering::Acquire) {
                let mut ocr_changed = false;
                while let Ok(latest_paragraphs) = ocr_receiver.try_recv() {
                    paragraphs = latest_paragraphs;
                    ocr_changed = true;
                }

                let pointer = match pointer_provider.position() {
                    Ok(pointer) => pointer,
                    Err(error) => {
                        log::warn!("Failed to read pointer position: {error}");
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                };

                if last_mouse_pos == Some(pointer) && !ocr_changed {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                last_mouse_pos = Some(pointer);

                let norm_x = (pointer.0 - monitor.left) as f64 / monitor.width as f64;
                let norm_y = (pointer.1 - monitor.top) as f64 / monitor.height as f64;
                let lookup_string =
                    if (0.0..=1.0).contains(&norm_x) && (0.0..=1.0).contains(&norm_y) {
                        hit_scan(&paragraphs, norm_x, norm_y)
                    } else {
                        None
                    };

                if !send_while_running(
                    &lookup_sender,
                    LookupRequest {
                        lookup_string,
                        mouse_x: pointer.0,
                        mouse_y: pointer.1,
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

fn spawn_lookup_worker(
    running: Arc<AtomicBool>,
    popup_is_visible: Arc<AtomicBool>,
    mut lookup_engine: LookupEngine,
    monitor: Monitor,
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
                            "Sending lookup result at mouse ({}, {}) on monitor left={}, top={}, width={}, height={}",
                            request.mouse_x,
                            request.mouse_y,
                            monitor.left,
                            monitor.top,
                            monitor.width,
                            monitor.height
                        );
                        if event_sender
                            .send(PipelineEvent::LookupResult {
                                entries: result.entries,
                                kanji: result.kanji_entry,
                                mouse_x: request.mouse_x,
                                mouse_y: request.mouse_y,
                                monitor: monitor.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                        popup_is_visible.store(true, Ordering::Release);
                    } else {
                        hide_popup_if_visible(&event_sender, &popup_is_visible);
                    }
                } else {
                    hide_popup_if_visible(&event_sender, &popup_is_visible);
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

fn hide_popup_if_visible(sender: &Sender<PipelineEvent>, popup_is_visible: &AtomicBool) {
    if popup_is_visible.swap(false, Ordering::AcqRel) {
        let _ = sender.send(PipelineEvent::HidePopup);
    }
}

fn send_error(sender: &Sender<PipelineEvent>, context: &str, error: impl std::fmt::Display) {
    log::error!("{context}: {error}");
    let _ = sender.send(PipelineEvent::Error(format!("{context}: {error}")));
}
