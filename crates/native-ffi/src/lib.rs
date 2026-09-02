//! Small C-compatible boundary around MeikiPop's native pipeline.

use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;
use std::ptr;
use std::sync::Once;
use std::time::Duration;

use meikipop_native::dictionary::lookup::{DictionaryEntry, KanjiEntry, Sense};
use meikipop_native::ocr::ocr::OcrProviderInfo;
use meikipop_native::pipeline::{Pipeline, PipelineConfig, PipelineEvent, PipelineRuntimeConfig};
use meikipop_native::screenshot::interface::CaptureGeometry;
use serde::{Deserialize, Serialize};

const MAX_DICT_ENTRIES: usize = 10;
const MAX_LOOKUP_LENGTH: usize = 25;
static LOGGING_INIT: Once = Once::new();

/// Installs a stderr logger for Rust code hosted by the Swift application.
///
/// Calling this function more than once is safe. `RUST_LOG` controls the
/// filter; when it is absent, MeikiPop logs at `info` level.
#[unsafe(no_mangle)]
pub extern "C" fn meikipop_logging_init() {
    LOGGING_INIT.call_once(|| {
        let environment = env_logger::Env::default()
            .filter_or("RUST_LOG", "meikipop_native=info,meikipop_native_ffi=info");
        let _ = env_logger::Builder::from_env(environment)
            .format_timestamp_millis()
            .try_init();
    });
}

/// Opaque to C and Swift. Rust remains the sole owner of the pipeline.
pub struct MeikiPopPipeline {
    pipeline: Pipeline,
}

#[derive(Deserialize)]
struct CoreConfiguration {
    ocr_provider: String,
}

impl From<CoreConfiguration> for PipelineRuntimeConfig {
    fn from(config: CoreConfiguration) -> Self {
        Self {
            ocr_provider: config.ocr_provider,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    CaptureReady,
    OcrProviders {
        providers: Vec<Provider>,
        active_provider: String,
        error: Option<String>,
    },
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
struct Provider {
    id: &'static str,
    name: &'static str,
}

impl From<OcrProviderInfo> for Provider {
    fn from(provider: OcrProviderInfo) -> Self {
        Self {
            id: provider.id,
            name: provider.name,
        }
    }
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
            PipelineEvent::OcrProvidersChanged {
                providers,
                active_provider,
                error,
            } => Self::OcrProviders {
                providers: providers.into_iter().map(Provider::from).collect(),
                active_provider,
                error,
            },
            PipelineEvent::LookupResult { entries, kanji, .. } => Self::Show {
                entries: entries.into_iter().map(Entry::from).collect(),
                kanji: kanji.map(Kanji::from),
            },
            PipelineEvent::HidePopup { .. } => Self::Hide,
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
/// `config_json` must point to a valid NUL-terminated UTF-8 string.
/// `error_out`, when non-null, must be valid for writing one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_start(
    dictionary_path: *const c_char,
    config_json: *const c_char,
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
    let runtime = match configuration_argument(config_json) {
        Ok(config) => config,
        Err(error) => {
            set_error(error_out, error);
            return ptr::null_mut();
        }
    };

    let config = PipelineConfig {
        dictionary_path,
        // The macOS provider does not use a screencast restoration token.
        screencast_token_path: PathBuf::new(),
        max_dict_entries: MAX_DICT_ENTRIES,
        max_lookup_length: MAX_LOOKUP_LENGTH,
        show_kanji: true,
        capture_interval: Duration::from_millis(300),
        runtime,
    };

    match Pipeline::start(config) {
        Ok(pipeline) => Box::into_raw(Box::new(MeikiPopPipeline { pipeline })),
        Err(error) => {
            set_error(error_out, error.to_string());
            ptr::null_mut()
        }
    }
}

/// Queues new frontend-owned runtime configuration. Applied state is reported
/// asynchronously through pipeline events. Returns false for invalid JSON,
/// invalid pointers, or a stopped pipeline.
///
/// # Safety
/// `pipeline` must be null or a live pointer returned by
/// `meikipop_pipeline_start`. `config_json` must point to valid UTF-8 C text.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_update_config(
    pipeline: *mut MeikiPopPipeline,
    config_json: *const c_char,
) -> bool {
    let Some(pipeline) = (unsafe { pipeline.as_ref() }) else {
        return false;
    };
    let Ok(config) = configuration_argument(config_json) else {
        return false;
    };
    pipeline.pipeline.update_config(config).is_ok()
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

/// Reports the popup's global desktop bounds to the native pipeline.
///
/// Passing `visible = false` clears the exclusion region; the remaining
/// arguments are ignored in that case.
///
/// # Safety
/// `pipeline` must be null or a live pointer returned by
/// `meikipop_pipeline_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meikipop_pipeline_set_popup_bounds(
    pipeline: *mut MeikiPopPipeline,
    visible: bool,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) {
    if let Some(pipeline) = unsafe { pipeline.as_ref() } {
        let bounds = visible.then_some(CaptureGeometry {
            left,
            top,
            width: width as usize,
            height: height as usize,
        });
        pipeline.pipeline.set_popup_bounds(bounds);
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

fn configuration_argument(pointer: *const c_char) -> Result<PipelineRuntimeConfig, String> {
    let json = c_string_argument(pointer, "config_json")?;
    serde_json::from_str::<CoreConfiguration>(&json)
        .map(PipelineRuntimeConfig::from)
        .map_err(|error| format!("Invalid pipeline configuration: {error}"))
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

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::configuration_argument;

    #[test]
    fn parses_frontend_runtime_configuration() {
        let json = CString::new(r#"{"ocr_provider":"apple_vision"}"#).unwrap();
        let config = configuration_argument(json.as_ptr()).unwrap();

        assert_eq!(config.ocr_provider, "apple_vision");
    }

    #[test]
    fn rejects_incomplete_runtime_configuration() {
        let json = CString::new("{}").unwrap();

        assert!(configuration_argument(json.as_ptr()).is_err());
    }
}
