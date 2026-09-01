//! Small C-compatible boundary around MeikiPop's native pipeline.

use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;

use meikipop_native::dictionary::lookup::{DictionaryEntry, KanjiEntry, Sense};
use meikipop_native::pipeline::{Pipeline, PipelineConfig, PipelineEvent};
use serde::Serialize;

const MAX_DICT_ENTRIES: usize = 10;
const MAX_LOOKUP_LENGTH: usize = 25;

/// Opaque to C and Swift. Rust remains the sole owner of the pipeline.
pub struct MeikiPopPipeline {
    pipeline: Pipeline,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    CaptureReady,
    Show {
        entries: Vec<Entry>,
        kanji: Option<Kanji>,
    },
    Hide,
    Error {
        message: String,
    },
}

#[derive(Serialize)]
struct Entry {
    written_form: Option<String>,
    reading: String,
    senses: Vec<EntrySense>,
    freq: i64,
    deconjugation_process: Vec<String>,
}

impl From<DictionaryEntry> for Entry {
    fn from(entry: DictionaryEntry) -> Self {
        Self {
            written_form: entry.written_form,
            reading: entry.reading,
            senses: entry.senses.into_iter().map(EntrySense::from).collect(),
            freq: entry.freq,
            deconjugation_process: entry.deconjugation_process,
        }
    }
}

#[derive(Serialize)]
struct EntrySense {
    glosses: Vec<String>,
    pos: Vec<String>,
    tags: Vec<String>,
}

impl From<Sense> for EntrySense {
    fn from(sense: Sense) -> Self {
        Self {
            glosses: sense.glosses,
            pos: sense.pos,
            tags: sense.tags,
        }
    }
}

#[derive(Serialize)]
struct Kanji {
    character: String,
    meanings: Vec<String>,
    readings: Vec<String>,
}

impl From<KanjiEntry> for Kanji {
    fn from(kanji: KanjiEntry) -> Self {
        Self {
            character: kanji.character,
            meanings: kanji.meanings,
            readings: kanji.readings,
        }
    }
}

impl From<PipelineEvent> for Event {
    fn from(event: PipelineEvent) -> Self {
        match event {
            PipelineEvent::CaptureReady => Self::CaptureReady,
            PipelineEvent::LookupResult { entries, kanji, .. } => Self::Show {
                entries: entries.into_iter().map(Entry::from).collect(),
                kanji: kanji.map(Kanji::from),
            },
            PipelineEvent::HidePopup => Self::Hide,
            PipelineEvent::Error(message) => Self::Error { message },
        }
    }
}

/// Starts the native screen-capture, OCR, hit-scan, and lookup pipeline.
///
/// On failure, returns null and stores a Rust-owned error string in
/// `error_out`. The caller must release that string with
/// `meikipop_string_free`.
///
/// # Safety
/// `dictionary_path` must point to a valid NUL-terminated UTF-8 string.
/// `error_out`, when non-null, must be valid for writing one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_start(
    dictionary_path: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut MeikiPopPipeline {
    if !error_out.is_null() {
        // SAFETY: Guaranteed by the function contract.
        unsafe { *error_out = ptr::null_mut() };
    }

    let dictionary_path = match c_string_argument(dictionary_path, "dictionary_path") {
        Ok(path) => PathBuf::from(path),
        Err(error) => {
            set_error(error_out, error);
            return ptr::null_mut();
        }
    };

    let config = PipelineConfig {
        dictionary_path,
        // The macOS provider does not use a screencast restoration token.
        screencast_token_path: PathBuf::new(),
        monitor_index: 1,
        max_dict_entries: MAX_DICT_ENTRIES,
        max_lookup_length: MAX_LOOKUP_LENGTH,
        show_kanji: true,
        capture_interval: Duration::from_millis(300),
    };

    match Pipeline::start(config) {
        Ok(pipeline) => Box::into_raw(Box::new(MeikiPopPipeline { pipeline })),
        Err(error) => {
            set_error(error_out, error.to_string());
            ptr::null_mut()
        }
    }
}

/// Returns the next event as a Rust-owned JSON string, or null when the queue
/// is currently empty. Release a non-null result with `meikipop_string_free`.
///
/// # Safety
/// `pipeline` must be null or a live pointer returned by
/// `meikipop_pipeline_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_poll(pipeline: *mut MeikiPopPipeline) -> *mut c_char {
    // SAFETY: Guaranteed by the function contract.
    let Some(pipeline) = (unsafe { pipeline.as_ref() }) else {
        return ptr::null_mut();
    };
    let Some(event) = pipeline.pipeline.try_recv() else {
        return ptr::null_mut();
    };

    serde_json::to_string(&Event::from(event))
        .ok()
        .and_then(|json| CString::new(json).ok())
        .map_or(ptr::null_mut(), CString::into_raw)
}

/// Lets the frontend tell the capture loop whether its popup is visible.
///
/// # Safety
/// `pipeline` must be null or a live pointer returned by
/// `meikipop_pipeline_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_set_popup_visible(
    pipeline: *mut MeikiPopPipeline,
    visible: bool,
) {
    // SAFETY: Guaranteed by the function contract.
    if let Some(pipeline) = unsafe { pipeline.as_ref() } {
        pipeline.pipeline.set_popup_visible(visible);
    }
}

/// Stops and releases a pipeline. Passing null is allowed.
///
/// # Safety
/// `pipeline` must be null or a live pointer returned by
/// `meikipop_pipeline_start`, and may be destroyed only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_destroy(pipeline: *mut MeikiPopPipeline) {
    if !pipeline.is_null() {
        // SAFETY: Guaranteed by the function contract.
        unsafe { drop(Box::from_raw(pipeline)) };
    }
}

/// Releases a string returned through this API. Passing null is allowed.
///
/// # Safety
/// `string` must be null or a live pointer returned by this library, and may
/// be released only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_string_free(string: *mut c_char) {
    if !string.is_null() {
        // SAFETY: Guaranteed by the function contract.
        unsafe { drop(CString::from_raw(string)) };
    }
}

fn c_string_argument(pointer: *const c_char, name: &str) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!("{name} must not be null"));
    }
    // SAFETY: The caller guarantees that `pointer` is NUL-terminated.
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| format!("{name} must be valid UTF-8"))
}

fn set_error(error_out: *mut *mut c_char, message: String) {
    if error_out.is_null() {
        return;
    }
    if let Ok(message) = CString::new(message) {
        // SAFETY: The caller guarantees that `error_out` is writable.
        unsafe { *error_out = message.into_raw() };
    }
}
