use std::cell::RefCell;
use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use fontdb::{Database, Family, Query};
use meikipop_native::dictionary::lookup::{DictionaryEntry, KanjiEntry};
use meikipop_native::pipeline::{Pipeline, PipelineConfig, PipelineEvent};
use meikipop_native::screenshot::interface::CaptureGeometry;
use slint::{ComponentHandle, ModelRc, VecModel};

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

#[derive(Clone, Copy, Debug, Default)]
struct PopupBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl PopupBounds {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x
            && x < self.x.saturating_add(self.width)
            && y >= self.y
            && y < self.y.saturating_add(self.height)
    }
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
    let pipeline = Rc::new(RefCell::new(Pipeline::start(pipeline_config())?));

    // Create the native X11 window before the first OCR hit. On XWayland/KDE,
    // properties such as always-on-top and the final position are only applied
    // reliably after the window has been mapped once.
    popup.show()?;
    popup.hide()?;

    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    let pipeline_for_screen_choice = Rc::clone(&pipeline);
    let popup_for_screen_choice = popup.as_weak();
    tray.on_choose_screen(move || {
        let token_path = screencast_token_path();
        if let Err(error) = std::fs::remove_file(&token_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %token_path.display(),
                %error,
                "Could not clear the saved screen selection"
            );
        }

        let mut pipeline = pipeline_for_screen_choice.borrow_mut();
        pipeline.shutdown();
        match Pipeline::start(pipeline_config()) {
            Ok(new_pipeline) => {
                *pipeline = new_pipeline;
                if let Some(popup) = popup_for_screen_choice.upgrade() {
                    let _ = popup.hide();
                }
                tracing::info!("Reopened the system screen chooser");
            }
            Err(error) => tracing::error!(%error, "Could not restart screen capture"),
        }
    });

    let popup_weak = popup.as_weak();
    let pipeline_for_updates = Rc::clone(&pipeline);
    let hide_timer = Rc::new(slint::Timer::default());
    let hide_timer_for_updates = Rc::clone(&hide_timer);
    let popup_bounds = Rc::new(RefCell::new(None));
    let popup_bounds_for_updates = Rc::clone(&popup_bounds);
    let update_timer = slint::Timer::default();
    update_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(20),
        move || {
            process_pipeline_events(
                &popup_weak,
                &pipeline_for_updates,
                &hide_timer_for_updates,
                &popup_bounds_for_updates,
            )
        },
    );

    tray.show()?;
    tracing::info!("MeikiPop is running in the background");
    slint::run_event_loop()?;

    pipeline.borrow_mut().shutdown();
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

fn process_pipeline_events(
    popup_weak: &slint::Weak<OcrPopup>,
    pipeline: &Rc<RefCell<Pipeline>>,
    hide_timer: &Rc<slint::Timer>,
    popup_bounds: &Rc<RefCell<Option<PopupBounds>>>,
) {
    while let Some(event) = pipeline.borrow().try_recv() {
        let Some(popup) = popup_weak.upgrade() else {
            return;
        };
        match event {
            PipelineEvent::CaptureReady => {}
            PipelineEvent::LookupResult {
                entries,
                kanji,
                mouse_x,
                mouse_y,
                capture_geometry,
            } => {
                hide_timer.stop();
                // A screenshot may already be queued for OCR when the popup is
                // first shown. If that frame contains the popup, its lookup
                // result arrives while the pointer is over our own window.
                // Keep the existing entry visible instead of recursively
                // looking up text rendered by the popup itself.
                if popup_bounds
                    .borrow()
                    .is_some_and(|bounds| bounds.contains(mouse_x, mouse_y))
                {
                    pipeline.borrow().set_popup_visible(true);
                    tracing::debug!("Ignoring lookup result from inside the popup");
                    continue;
                }

                let formatted_entries: Vec<FormattedEntry> = entries
                    .iter()
                    .map(format_entry)
                    .map(|entry| FormattedEntry {
                        word: entry.word.into(),
                        reading: entry.reading.into(),
                        deconjugation: entry.deconjugation.into(),
                        freq: entry.freq.into(),
                        definitions: entry.definitions.into(),
                    })
                    .collect();
                let model = Rc::new(VecModel::from(formatted_entries));
                popup.set_entries(ModelRc::from(model));

                if let Some(kanji) = kanji.as_ref().map(format_kanji) {
                    popup.set_has_kanji(true);
                    popup.set_kanji_character(kanji.character.into());
                    popup.set_kanji_readings(kanji.readings.into());
                    popup.set_kanji_meanings(kanji.meanings.into());
                } else {
                    popup.set_has_kanji(false);
                }

                popup.set_has_error(false);
                popup.set_scroll_y(0.0);
                let _ = popup.show();
                pipeline.borrow().set_popup_visible(true);

                let scale_factor = popup.window().scale_factor();
                let physical_size = popup.window().size();
                let logical_width = (physical_size.width as f32 / scale_factor).round() as i32;
                let logical_height = (physical_size.height as f32 / scale_factor).round() as i32;

                let (x, y) = calculate_popup_position(
                    mouse_x,
                    mouse_y,
                    &capture_geometry,
                    logical_width,
                    logical_height,
                );

                tracing::debug!(
                    mouse_x,
                    mouse_y,
                    scale_factor,
                    physical_width = physical_size.width,
                    physical_height = physical_size.height,
                    logical_width,
                    logical_height,
                    target_x = x,
                    target_y = y,
                    "Positioning popup window"
                );

                popup
                    .window()
                    .set_position(slint::LogicalPosition::new(x as f32, y as f32));
                *popup_bounds.borrow_mut() = Some(PopupBounds {
                    x,
                    y,
                    width: logical_width,
                    height: logical_height,
                });
            }
            PipelineEvent::HidePopup { mouse_x, mouse_y } => {
                let popup_weak = popup.as_weak();
                let pipeline = Rc::clone(pipeline);
                let popup_bounds = Rc::clone(popup_bounds);
                hide_timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(200),
                    move || {
                        let Some(popup) = popup_weak.upgrade() else {
                            return;
                        };
                        if popup_bounds
                            .borrow()
                            .is_some_and(|bounds| bounds.contains(mouse_x, mouse_y))
                        {
                            pipeline.borrow().set_popup_visible(true);
                        } else {
                            let _ = popup.hide();
                            *popup_bounds.borrow_mut() = None;
                            pipeline.borrow().set_popup_visible(false);
                        }
                    },
                );
            }
            PipelineEvent::Error(error) => {
                hide_timer.stop();
                popup.set_has_error(true);
                popup.set_error_text(error.into());
                popup.set_scroll_y(0.0);
                let _ = popup.show();
                popup
                    .window()
                    .set_position(slint::LogicalPosition::new(80.0, 80.0));
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
            .filter(|process| !process.is_empty())
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

    let mut definition_lines = Vec::new();
    let multiple_senses = entry.senses.len() > 1;
    for (index, sense) in entry.senses.iter().enumerate() {
        let mut parts = Vec::new();
        if multiple_senses {
            parts.push(format!("({})", index + 1));
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
        definition_lines.push(parts.join(" "));
    }

    FormattedEntryData {
        word,
        reading,
        deconjugation,
        freq,
        definitions: definition_lines.join("\n"),
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
    capture_geometry: &CaptureGeometry,
    popup_width: i32,
    popup_height: i32,
) -> (i32, i32) {
    let offset = 16;
    let right_boundary = capture_geometry.left + capture_geometry.width as i32;
    let bottom_boundary = capture_geometry.top + capture_geometry.height as i32;

    let mut final_x = mouse_x + offset;
    let mut final_y = mouse_y + offset;

    if final_x + popup_width > right_boundary {
        final_x = mouse_x - popup_width - offset;
    }
    if final_y + popup_height > bottom_boundary {
        final_y = mouse_y - popup_height - offset;
    }

    let maximum_x = (right_boundary - popup_width).max(capture_geometry.left);
    let maximum_y = (bottom_boundary - popup_height).max(capture_geometry.top);
    final_x = final_x.clamp(capture_geometry.left, maximum_x);
    final_y = final_y.clamp(capture_geometry.top, maximum_y);

    (final_x, final_y)
}

fn dictionary_path() -> PathBuf {
    data_dir().join("meikipop").join("dictionary.pkl")
}

fn pipeline_config() -> PipelineConfig {
    PipelineConfig {
        dictionary_path: dictionary_path(),
        screencast_token_path: screencast_token_path(),
        max_dict_entries: MAX_DICT_ENTRIES,
        max_lookup_length: MAX_LOOKUP_LENGTH,
        show_kanji: true,
        capture_interval: Duration::from_millis(300),
    }
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
