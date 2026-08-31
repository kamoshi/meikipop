use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fontdb::{Database, Family, Query};
use meikipop_native::dictionary::lookup::{DictionaryEntry, KanjiEntry, LookupEngine};
use meikipop_native::ocr::ocr::OcrProcessor;
use meikipop_native::screenshot::interface::{Monitor, ScreenshotBackend};
use meikipop_native::screenshot::wayland_mss_shim::MssWaylandShim;
use slint::{ComponentHandle, ModelRc, VecModel};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

mod logger;

slint::include_modules!();

const MAX_DICT_ENTRIES: usize = 10;
const MAX_LOOKUP_LENGTH: usize = 25;

#[derive(Clone, Debug)]
struct FormattedEntryData {
    word: String,
    reading: String,
    deconjugation: String,
    freq: String,
    definitions: String,
}

#[derive(Clone, Debug)]
struct FormattedKanjiData {
    character: String,
    readings: String,
    meanings: String,
}

enum PopupUpdate {
    Show {
        entries: Vec<FormattedEntryData>,
        kanji: Option<FormattedKanjiData>,
        mouse_x: i32,
        mouse_y: i32,
        monitor: Monitor,
    },
    Hide,
    Error(String),
}

fn main() -> Result<(), Box<dyn Error>> {
    logger::setup_logging()?;
    tracing::info!("Starting MeikiPop");

    let popup = OcrPopup::new()?;
    let font_family = preferred_font_family();
    if font_family.is_empty() {
        tracing::debug!("Using the platform default font family");
    } else {
        tracing::debug!(font_family, "Selected popup font family");
    }
    popup.set_ui_font_family(font_family.into());
    let tray = MeikiPopTray::new()?;
    let (update_sender, update_receiver) = mpsc::channel();

    // Create the native X11 window before the first OCR hit. On XWayland/KDE,
    // properties such as always-on-top and the final position are only applied
    // reliably after the window has been mapped once.
    popup.show()?;
    popup.hide()?;

    let running = Arc::new(AtomicBool::new(true));
    start_continuous_pipeline(Arc::clone(&running), update_sender);

    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    let popup_weak = popup.as_weak();
    let update_timer = slint::Timer::default();
    update_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(20),
        move || process_popup_updates(&popup_weak, &update_receiver),
    );

    tray.show()?;
    tracing::info!("MeikiPop is running in the background");
    slint::run_event_loop()?;

    running.store(false, Ordering::SeqCst);
    tracing::info!("MeikiPop stopped");
    Ok(())
}

fn preferred_font_family() -> String {
    // Prefer Japanese-capable fonts commonly found on Linux, Windows, and macOS.
    // If none are installed, leave the family empty and let Slint use the
    // platform's default font and glyph fallback.
    const FONT_FAMILIES: &[&str] = &[
        "Noto Sans CJK JP",
        "Noto Sans JP",
        "Yu Gothic UI",
        "Yu Gothic",
        "Hiragino Sans",
        "Meiryo",
        "IPA Gothic",
    ];

    let mut database = Database::new();
    database.load_system_fonts();

    FONT_FAMILIES
        .iter()
        .find(|family| {
            database
                .query(&Query {
                    families: &[Family::Name(family)],
                    ..Query::default()
                })
                .is_some()
        })
        .copied()
        .unwrap_or_default()
        .to_owned()
}

fn start_continuous_pipeline(running: Arc<AtomicBool>, sender: Sender<PopupUpdate>) {
    thread::Builder::new()
        .name("PipelineInit".to_owned())
        .spawn(move || {
            tracing::debug!("Initializing native OCR pipeline");
            let token_path = screencast_token_path();
            let mut screenshot_backend =
                match MssWaylandShim::new(token_path.to_string_lossy().into_owned()) {
                    Ok(backend) => backend,
                    Err(error) => {
                        tracing::error!(%error, "Failed to initialize screencast");
                        let _ = sender.send(PopupUpdate::Error(format!(
                            "Failed to initialize screencast: {error}"
                        )));
                        return;
                    }
                };

            let monitor = match screenshot_backend
                .monitors()
                .map_err(|e| e.to_string())
                .and_then(|m| {
                    m.get(1)
                        .cloned()
                        .ok_or_else(|| "No monitor found".to_owned())
                }) {
                Ok(mon) => mon,
                Err(error) => {
                    tracing::error!(%error, "Failed to get monitor");
                    let _ = sender.send(PopupUpdate::Error(format!(
                        "Failed to get monitor: {error}"
                    )));
                    return;
                }
            };
            tracing::info!(
                left = monitor.left,
                top = monitor.top,
                width = monitor.width,
                height = monitor.height,
                "Selected scan monitor"
            );

            let ocr_processor = match OcrProcessor::new() {
                Ok(proc) => Arc::new(Mutex::new(proc)),
                Err(error) => {
                    tracing::error!(%error, "Failed to initialize OCR");
                    let _ = sender.send(PopupUpdate::Error(format!(
                        "Failed to initialize OCR: {error}"
                    )));
                    return;
                }
            };
            tracing::info!("OCR processor initialized");

            let lookup_engine = match LookupEngine::open_paths(&dictionary_path(), MAX_DICT_ENTRIES)
            {
                Ok(engine) => Arc::new(Mutex::new(engine)),
                Err(error) => {
                    tracing::error!(%error, "Failed to load dictionary");
                    let _ = sender.send(PopupUpdate::Error(format!(
                        "Failed to load dictionary: {error}"
                    )));
                    return;
                }
            };
            tracing::info!("Dictionary loaded");

            // Thread 1: Screencast & OCR loop
            let ocr_running = Arc::clone(&running);
            let ocr_processor_for_capture = Arc::clone(&ocr_processor);
            let monitor_for_capture = monitor.clone();
            // Upstream triggers hit_scan_queue after every completed OCR pass.
            let ocr_generation = Arc::new(AtomicU64::new(0));
            let ocr_generation_for_capture = Arc::clone(&ocr_generation);
            // Match upstream's screen_lock: don't capture the popup itself.
            let popup_is_visible = Arc::new(AtomicBool::new(false));
            let popup_is_visible_for_capture = Arc::clone(&popup_is_visible);
            thread::Builder::new()
                .name("ScreencastOcrWorker".to_owned())
                .spawn(move || {
                    let mut last_raw_frame: Option<Vec<u8>> = None;
                    tracing::debug!("Screencast and OCR worker started");
                    while ocr_running.load(Ordering::Relaxed) {
                        if popup_is_visible_for_capture.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        let result = (|| -> Result<(), Box<dyn Error>> {
                            let screenshot = screenshot_backend.grab(&monitor_for_capture)?;
                            if let Some(prev) = &last_raw_frame {
                                if prev == &screenshot.raw {
                                    return Ok(());
                                }
                            }
                            let image = screenshot.to_rgb()?;
                            let mut processor = ocr_processor_for_capture
                                .lock()
                                .map_err(|_| "OCR processor lock was poisoned")?;
                            processor.scan_rgb(&image)?;
                            // Only suppress an identical frame after OCR succeeded.
                            last_raw_frame = Some(screenshot.raw);
                            ocr_generation_for_capture.fetch_add(1, Ordering::Release);
                            Ok(())
                        })();

                        if let Err(error) = result {
                            tracing::error!(%error, "OCR worker error");
                        }
                        thread::sleep(Duration::from_millis(300));
                    }
                    tracing::debug!("Screencast and OCR worker stopped");
                })
                .expect("failed to spawn ScreencastOcrWorker");

            // Thread 2: Mouse Tracker & Hit Scanner
            let mouse_running = Arc::clone(&running);
            let ocr_processor_for_mouse = Arc::clone(&ocr_processor);
            let lookup_engine_for_mouse = Arc::clone(&lookup_engine);
            let sender_for_mouse = sender;
            let popup_is_visible_for_mouse = Arc::clone(&popup_is_visible);
            let ocr_generation_for_mouse = Arc::clone(&ocr_generation);
            thread::Builder::new()
                .name("MouseTrackerWorker".to_owned())
                .spawn(move || {
                    tracing::debug!("Mouse tracker and hit scanner worker started");
                    let (connection, screen_number) = match x11rb::connect(None) {
                        Ok(conn) => conn,
                        Err(err) => {
                            tracing::error!(error = %err, "Failed to connect to X11");
                            let _ = sender_for_mouse.send(PopupUpdate::Error(format!(
                                "Failed to connect to X11: {err}"
                            )));
                            return;
                        }
                    };
                    let root = connection.setup().roots[screen_number].root;

                    let mut last_mouse_pos: Option<(i32, i32)> = None;
                    let mut last_ocr_generation = 0;
                    while mouse_running.load(Ordering::Relaxed) {
                        let pointer = match connection
                            .query_pointer(root)
                            .map_err(|e| e.to_string())
                            .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()))
                        {
                            Ok(p) => (i32::from(p.root_x), i32::from(p.root_y)),
                            Err(_) => {
                                thread::sleep(Duration::from_millis(25));
                                continue;
                            }
                        };

                        let mouse_x = pointer.0;
                        let mouse_y = pointer.1;

                        let current_ocr_generation =
                            ocr_generation_for_mouse.load(Ordering::Acquire);
                        if last_mouse_pos == Some(pointer)
                            && last_ocr_generation == current_ocr_generation
                        {
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        last_mouse_pos = Some(pointer);
                        last_ocr_generation = current_ocr_generation;

                        let norm_x = (mouse_x - monitor.left) as f64 / monitor.width as f64;
                        let norm_y = (mouse_y - monitor.top) as f64 / monitor.height as f64;

                        let hit_opt =
                            if (0.0..=1.0).contains(&norm_x) && (0.0..=1.0).contains(&norm_y) {
                                ocr_processor_for_mouse
                                    .lock()
                                    .ok()
                                    .and_then(|proc| proc.hit_scan(norm_x, norm_y))
                            } else {
                                None
                            };

                        if let Some(lookup_str) = hit_opt {
                            let (entries, kanji) =
                                if let Ok(mut engine) = lookup_engine_for_mouse.lock() {
                                    let result =
                                        engine.lookup_cached(&lookup_str, MAX_LOOKUP_LENGTH, true);
                                    let entries = result.entries.iter().map(format_entry).collect();
                                    let kanji = result.kanji_entry.as_ref().map(format_kanji);
                                    (entries, kanji)
                                } else {
                                    (Vec::new(), None)
                                };

                            if !entries.is_empty() || kanji.is_some() {
                                let _ = sender_for_mouse.send(PopupUpdate::Show {
                                    entries,
                                    kanji,
                                    mouse_x,
                                    mouse_y,
                                    monitor: monitor.clone(),
                                });
                                popup_is_visible_for_mouse.store(true, Ordering::Release);
                            } else if popup_is_visible_for_mouse.load(Ordering::Acquire) {
                                let _ = sender_for_mouse.send(PopupUpdate::Hide);
                                popup_is_visible_for_mouse.store(false, Ordering::Release);
                            }
                        } else if popup_is_visible_for_mouse.load(Ordering::Acquire) {
                            let _ = sender_for_mouse.send(PopupUpdate::Hide);
                            popup_is_visible_for_mouse.store(false, Ordering::Release);
                        }

                        thread::sleep(Duration::from_millis(20));
                    }
                    tracing::debug!("Mouse tracker and hit scanner worker stopped");
                })
                .expect("failed to spawn MouseTrackerWorker");
        })
        .expect("failed to spawn PipelineInit thread");
}

fn process_popup_updates(popup_weak: &slint::Weak<OcrPopup>, receiver: &Receiver<PopupUpdate>) {
    while let Ok(update) = receiver.try_recv() {
        let Some(popup) = popup_weak.upgrade() else {
            return;
        };
        match update {
            PopupUpdate::Show {
                entries,
                kanji,
                mouse_x,
                mouse_y,
                monitor,
            } => {
                let formatted_entries: Vec<FormattedEntry> = entries
                    .into_iter()
                    .map(|e| FormattedEntry {
                        word: e.word.into(),
                        reading: e.reading.into(),
                        deconjugation: e.deconjugation.into(),
                        freq: e.freq.into(),
                        definitions: e.definitions.into(),
                    })
                    .collect();
                let model = Rc::new(VecModel::from(formatted_entries));
                popup.set_entries(ModelRc::from(model));

                if let Some(k) = kanji {
                    popup.set_has_kanji(true);
                    popup.set_kanji_character(k.character.into());
                    popup.set_kanji_readings(k.readings.into());
                    popup.set_kanji_meanings(k.meanings.into());
                } else {
                    popup.set_has_kanji(false);
                }

                popup.set_has_error(false);
                let _ = popup.show();
                let popup_size = popup.window().size();
                let (x, y) = calculate_popup_position(
                    mouse_x,
                    mouse_y,
                    &monitor,
                    popup_size.width as i32,
                    popup_size.height as i32,
                );
                popup
                    .window()
                    .set_position(slint::PhysicalPosition::new(x, y));
            }
            PopupUpdate::Hide => {
                let _ = popup.hide();
            }
            PopupUpdate::Error(error) => {
                popup.set_has_error(true);
                popup.set_error_text(error.into());
                let _ = popup.show();
                popup
                    .window()
                    .set_position(slint::PhysicalPosition::new(80, 80));
            }
        }
    }
}

fn format_entry(entry: &DictionaryEntry) -> FormattedEntryData {
    let word = entry
        .written_form
        .clone()
        .unwrap_or_else(|| entry.reading.clone());
    let reading = if entry.written_form.is_some() && !entry.reading.is_empty() {
        entry.reading.clone()
    } else {
        String::new()
    };
    let deconjugation = if !entry.deconjugation_process.is_empty() {
        entry
            .deconjugation_process
            .iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ← ")
    } else {
        String::new()
    };
    let freq = if entry.freq > 0 && entry.freq < 999_999 {
        entry.freq.to_string()
    } else {
        String::new()
    };

    let mut def_lines = Vec::new();
    let multiple_senses = entry.senses.len() > 1;
    for (idx, sense) in entry.senses.iter().enumerate() {
        let mut parts = Vec::new();
        if multiple_senses {
            parts.push(format!("({})", idx + 1));
        }
        if !sense.pos.is_empty() {
            parts.push(format!("({})", sense.pos.join(", ")));
        }
        if !sense.tags.is_empty() {
            parts.push(format!("[{}]", sense.tags.join(", ")));
        }
        if !sense.glosses.is_empty() {
            parts.push(sense.glosses.join("; "));
        }
        def_lines.push(parts.join(" "));
    }

    FormattedEntryData {
        word,
        reading,
        deconjugation,
        freq,
        definitions: def_lines.join("\n"),
    }
}

fn format_kanji(kanji: &KanjiEntry) -> FormattedKanjiData {
    FormattedKanjiData {
        character: kanji.character.clone(),
        readings: kanji.readings.join(", "),
        meanings: kanji.meanings.join("; "),
    }
}

fn calculate_popup_position(
    mouse_x: i32,
    mouse_y: i32,
    monitor: &Monitor,
    popup_width: i32,
    popup_height: i32,
) -> (i32, i32) {
    let offset = 16;
    let right_boundary = monitor.left + monitor.width as i32;
    let bottom_boundary = monitor.top + monitor.height as i32;

    let mut final_x = mouse_x + offset;
    let mut final_y = mouse_y + offset;

    if final_x + popup_width > right_boundary {
        final_x = mouse_x - popup_width - offset;
    }
    if final_y + popup_height > bottom_boundary {
        final_y = mouse_y - popup_height - offset;
    }

    let maximum_x = (right_boundary - popup_width).max(monitor.left);
    let maximum_y = (bottom_boundary - popup_height).max(monitor.top);
    final_x = final_x.clamp(monitor.left, maximum_x);
    final_y = final_y.clamp(monitor.top, maximum_y);

    (final_x, final_y)
}

fn dictionary_path() -> PathBuf {
    data_dir().join("meikipop").join("dictionary.pkl")
}

fn screencast_token_path() -> PathBuf {
    cache_dir()
        .join("meikipop")
        .join(".ocr_screencapture_token")
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}
