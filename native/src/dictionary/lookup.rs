use crate::dictionary::customdict;
use crate::dictionary::deconjugator::{Deconjugator, Form};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::utils::latest_queue::{LatestValueQueue, PyLatestValueQueue};

pub const DEFAULT_FREQ: i64 = 999_999;
pub const CACHE_SIZE: usize = 500;
pub const JAPANESE_SEPARATORS: &str = concat!(
    "、。「」｛｝（）【】",
    "『』〈〉《》：・／",
    "…︙‥︰＋＝－÷？！",
    "．～―!?",
);

/// The typed equivalent of one upstream sense dictionary.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, IntoPyObject)]
pub struct Sense {
    pub glosses: Vec<String>,
    pub pos: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KanjiComponent {
    pub c: String,
    pub m: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, IntoPyObject)]
pub struct KanjiExample {
    pub w: String,
    pub r: String,
    pub m: String,
}

/// The typed equivalent of one upstream kanji entry dictionary.
fn components_to_python<'py>(
    components: Cow<'_, Vec<KanjiComponent>>,
    py: Python<'py>,
) -> PyResult<Bound<'py, PyAny>> {
    let result = PyList::empty(py);
    for component in components.iter() {
        let value = PyDict::new(py);
        value.set_item("c", &component.c)?;
        if let Some(meaning) = &component.m {
            value.set_item("m", meaning)?;
        }
        result.append(value)?;
    }
    Ok(result.into_any())
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, IntoPyObject)]
pub struct KanjiEntry {
    pub character: String,
    pub meanings: Vec<String>,
    pub readings: Vec<String>,
    #[pyo3(into_py_with = components_to_python)]
    pub components: Vec<KanjiComponent>,
    pub examples: Vec<KanjiExample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "RawMapEntry")]
pub struct MapEntry {
    pub written_form: Option<String>,
    pub reading: Option<String>,
    pub frequency: i64,
    pub entry_id: i64,
}

#[derive(Clone, Debug, PartialEq, IntoPyObject)]
pub struct DictionaryEntry {
    pub id: i64,
    pub written_form: Option<String>,
    pub reading: String,
    pub senses: Vec<Sense>,
    pub freq: i64,
    pub deconjugation_process: Vec<String>,
    pub priority: f64,
}

#[derive(Debug)]
struct MergedEntry {
    id: i64,
    written_form: Option<String>,
    reading: String,
    senses: Vec<Sense>,
    freq: i64,
    deconjugation_process: Vec<String>,
    priority: f64,
    match_len: usize,
}

#[derive(Clone, Debug)]
struct CachedLookup {
    entries: Vec<DictionaryEntry>,
    kanji_entry: Option<KanjiEntry>,
}

pub struct LookupEngine {
    entries: HashMap<i64, Vec<Sense>>,
    lookup_map: HashMap<String, Vec<MapEntry>>,
    kanji_entries: HashMap<String, KanjiEntry>,
    deconjugator: Deconjugator,
    max_dict_entries: usize,
    lookup_cache: HashMap<String, CachedLookup>,
    cache_order: VecDeque<String>,
    validation_issues: usize,
    validation_warnings: Vec<String>,
}

impl LookupEngine {
    pub fn new(
        entries: HashMap<i64, Vec<Sense>>,
        lookup_map: HashMap<String, Vec<MapEntry>>,
        kanji_entries: HashMap<String, KanjiEntry>,
        deconjugator: Deconjugator,
        max_dict_entries: usize,
    ) -> Self {
        let validation = customdict::validate(&entries, &lookup_map);
        Self {
            entries,
            lookup_map,
            kanji_entries,
            deconjugator,
            max_dict_entries,
            lookup_cache: HashMap::new(),
            cache_order: VecDeque::new(),
            validation_issues: validation.issues,
            validation_warnings: validation.warnings,
        }
    }

    pub fn open_paths(pickle_path: &Path, max_dict_entries: usize) -> Result<Self, String> {
        if !pickle_path.exists() {
            let data_dir = pickle_path
                .parent()
                .ok_or_else(|| "Dictionary path has no parent directory".to_owned())?;
            customdict::download_dictionary(data_dir)?;
        }

        let json_path = pickle_path.with_extension("json");
        customdict::DictionaryData::load_or_convert(
            Path::new(customdict::PYTHON_EXECUTABLE),
            pickle_path,
            &json_path,
        )
        .map(|dictionary| dictionary.into_lookup_engine(max_dict_entries))
    }

    pub fn lookup(&self, text: &str) -> Vec<DictionaryEntry> {
        self.do_lookup(text)
    }

    pub fn prepare_lookup_text(&self, lookup_string: &str, max_lookup_length: usize) -> String {
        let mut text: String = lookup_string
            .trim()
            .chars()
            .take(max_lookup_length)
            .collect();
        if let Some(index) = text.find(|ch| JAPANESE_SEPARATORS.contains(ch)) {
            text.truncate(index);
        }
        text
    }

    pub fn get_kanji_entry(&self, character: &str) -> Option<&KanjiEntry> {
        self.kanji_entries.get(character)
    }

    fn lookup_cached(
        &mut self,
        lookup_string: &str,
        max_lookup_length: usize,
        show_kanji: bool,
    ) -> CachedLookup {
        let text = self.prepare_lookup_text(lookup_string, max_lookup_length);
        if text.is_empty() {
            return CachedLookup {
                entries: Vec::new(),
                kanji_entry: None,
            };
        }

        if let Some(result) = self.lookup_cache.get(&text).cloned() {
            if let Some(index) = self.cache_order.iter().position(|key| key == &text) {
                self.cache_order.remove(index);
            }
            self.cache_order.push_back(text);
            return result;
        }

        let entries = self.lookup(&text);

        // Append kanji entry for the first character if applicable
        let kanji_entry = if show_kanji {
            text.chars()
                .next()
                .filter(|character| is_kanji(*character))
                .and_then(|character| self.get_kanji_entry(&character.to_string()))
                .cloned()
        } else {
            None
        };
        let result = CachedLookup {
            entries,
            kanji_entry,
        };

        self.lookup_cache.insert(text.clone(), result.clone());
        self.cache_order.push_back(text);
        if self.lookup_cache.len() > CACHE_SIZE {
            if let Some(oldest) = self.cache_order.pop_front() {
                self.lookup_cache.remove(&oldest);
            }
        }
        result
    }

    pub fn clear_cache(&mut self) {
        self.lookup_cache.clear();
        self.cache_order.clear();
    }

    /// Scan all prefixes of `text` (longest first), deconjugate each, then
    /// look up every resulting form in the kanji / kana maps.
    ///
    /// Collected results are keyed by (written_form, reading) to merge duplicate
    /// map entries that resolve to the same display pair. The final list is
    /// sorted by (match_length DESC, priority DESC).
    fn do_lookup(&self, text: &str) -> Vec<DictionaryEntry> {
        // entry_id -> (map_entry, form, match_len)
        let mut collected: HashMap<i64, (MapEntry, Form, usize)> = HashMap::new();
        let mut found_primary_match = false;

        let text_len = text.chars().count();
        for prefix_len in (1..=text_len).rev() {
            let prefix: String = text.chars().take(prefix_len).collect();

            let mut forms = self.deconjugator.deconjugate(&prefix);
            forms.insert(Form::new(&prefix));

            let mut prefix_hits = Vec::new();

            for form in forms {
                let map_entries = self.get_map_entries(&form.text);
                if map_entries.is_empty() {
                    continue;
                }

                for map_entry in map_entries {
                    let written = map_entry.written_form.as_deref();
                    let entry_id = map_entry.entry_id;

                    if written.is_none() && contains_kanji(&form.text) {
                        // logger.warning(f"Skipping malformed dictionary entry: kanji key '{form.text}'")
                        continue;
                    }

                    // POS validation: if the deconjugator tagged this form,
                    // the entry must contain that part-of-speech.
                    if let Some(required_pos) = form.tags.last() {
                        let entry_senses = self.entries.get(&entry_id);
                        let has_required_pos = entry_senses.is_some_and(|senses| {
                            senses
                                .iter()
                                .any(|sense| sense.pos.iter().any(|pos| pos == required_pos))
                        });
                        if !has_required_pos {
                            // logger.debug(
                            //     f"Pruning id={entry_id} ({written}): "
                            //     f"required POS '{required_pos}' not in {all_pos}"
                            // )
                            continue;
                        }
                    }

                    // Kana-only prefix filter: once a primary match with kanji
                    // exists, suppress kana-path entries that have a kanji form
                    if found_primary_match && !contains_kanji(&prefix) {
                        if written.is_some_and(contains_kanji) {
                            continue;
                        }
                    }

                    prefix_hits.push((map_entry.clone(), form.clone()));
                }
            }

            if !prefix_hits.is_empty() {
                if !found_primary_match {
                    found_primary_match = true;
                }

                for (map_entry, form) in prefix_hits {
                    collected
                        .entry(map_entry.entry_id)
                        .or_insert((map_entry, form, prefix_len));
                }
            }
        }

        self.format_and_sort(collected.into_values().collect(), text)
    }

    /// Look up `text` in lookup_map with hira↔kata fallback.
    /// Kanji and kana strings never share keys so a single map suffices.
    fn get_map_entries(&self, text: &str) -> &[MapEntry] {
        let result = self.lookup_map.get(text).map(Vec::as_slice).unwrap_or(&[]);
        if !result.is_empty() {
            return result;
        }
        let kata = hira_to_kata(text);
        if kata != text {
            let result = self.lookup_map.get(&kata).map(Vec::as_slice).unwrap_or(&[]);
            if !result.is_empty() {
                return result;
            }
        }
        let hira = kata_to_hira(text);
        if hira != text {
            let result = self.lookup_map.get(&hira).map(Vec::as_slice).unwrap_or(&[]);
            if !result.is_empty() {
                return result;
            }
        }
        &[]
    }

    /// Merge map entries that share (written_form, reading) across different
    /// deconjugation paths, compute priority, then sort and return DictionaryEntry list.
    fn format_and_sort(
        &self,
        raw: Vec<(MapEntry, Form, usize)>,
        original_lookup: &str,
    ) -> Vec<DictionaryEntry> {
        // Key: (written_form, reading)  Value: accumulated data dict
        let mut merged: HashMap<(Option<String>, String), MergedEntry> = HashMap::new();

        for (map_entry, form, match_len) in raw {
            let written = map_entry.written_form;
            let reading = map_entry.reading.unwrap_or_default();
            let freq = map_entry.frequency;
            let entry_id = map_entry.entry_id;

            let entry_senses = self.entries.get(&entry_id).cloned().unwrap_or_default();
            let priority =
                calculate_priority(written.as_deref(), freq, &form, match_len, original_lookup);

            let key = (written.clone(), reading.clone());
            if let Some(cur) = merged.get_mut(&key) {
                // Same (written_form, reading) reached via a different deconjugation path
                // or from a different entry ID (genuine homograph with identical display forms).
                // Merge senses from the other entry and keep the best freq/priority/match_len.
                if entry_id != cur.id {
                    cur.senses.extend(entry_senses);
                }
                if priority > cur.priority {
                    cur.priority = priority;
                    cur.id = entry_id;
                    cur.deconjugation_process = form.process;
                }
                if freq < cur.freq {
                    cur.freq = freq;
                }
                if match_len > cur.match_len {
                    cur.match_len = match_len;
                }
            } else {
                merged.insert(
                    key,
                    MergedEntry {
                        id: entry_id,
                        written_form: written,
                        reading,
                        senses: entry_senses,
                        freq,
                        deconjugation_process: form.process,
                        priority,
                        match_len,
                    },
                );
            }
        }

        let mut sorted_entries: Vec<_> = merged.into_values().collect();
        sorted_entries.sort_by(|a, b| {
            b.match_len
                .cmp(&a.match_len)
                .then_with(|| b.priority.total_cmp(&a.priority))
        });

        let mut results = Vec::new();
        for entry in sorted_entries.into_iter().take(self.max_dict_entries) {
            results.push(DictionaryEntry {
                id: entry.id,
                written_form: entry.written_form,
                reading: entry.reading,
                senses: entry.senses,
                freq: entry.freq,
                deconjugation_process: entry.deconjugation_process,
                priority: entry.priority,
            });
        }
        results
    }
}

#[derive(Deserialize)]
struct RawMapEntry(Option<String>, Option<String>, i64, i64);

impl From<RawMapEntry> for MapEntry {
    fn from(value: RawMapEntry) -> Self {
        let RawMapEntry(written_form, reading, frequency, entry_id) = value;
        Self {
            written_form,
            reading,
            frequency,
            entry_id,
        }
    }
}

#[pyclass(name = "LookupEngine", module = "meikipop_native.dictionary.lookup")]
pub struct PyLookupEngine {
    inner: Arc<Mutex<LookupEngine>>,
}

#[pymethods]
impl PyLookupEngine {
    #[staticmethod]
    #[pyo3(name = "open")]
    fn open(pickle_path: PathBuf, max_dict_entries: usize) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(
                LookupEngine::open_paths(&pickle_path, max_dict_entries)
                    .map_err(pyo3::exceptions::PyRuntimeError::new_err)?,
            )),
        })
    }

    fn validate(&self) -> PyResult<(usize, Vec<String>)> {
        let engine = self.lock()?;
        Ok((engine.validation_issues, engine.validation_warnings.clone()))
    }

    #[pyo3(name = "lookup")]
    fn py_lookup<'py>(
        &mut self,
        py: Python<'py>,
        lookup_string: &str,
        max_lookup_length: usize,
        show_kanji: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let results = PyList::empty(py);
        let result = self
            .lock()?
            .lookup_cached(lookup_string, max_lookup_length, show_kanji);
        for entry in result.entries {
            results.append(entry)?;
        }
        if let Some(entry) = result.kanji_entry {
            results.append(entry)?;
        }
        Ok(results)
    }

    #[pyo3(name = "clear_cache")]
    fn py_clear_cache(&self) -> PyResult<()> {
        self.lock()?.clear_cache();
        Ok(())
    }
}

impl PyLookupEngine {
    fn lock(&self) -> PyResult<std::sync::MutexGuard<'_, LookupEngine>> {
        self.inner.lock().map_err(|_| {
            pyo3::exceptions::PyRuntimeError::new_err("lookup engine lock was poisoned")
        })
    }
}

#[pyclass(name = "LookupWorker")]
pub struct PyLookupWorker {
    engine: Arc<Mutex<LookupEngine>>,
    shared_state: Py<PyAny>,
    popup_window: Py<PyAny>,
    config: Py<PyAny>,
    logger: Py<PyAny>,
    lookup_queue: Arc<LatestValueQueue<Py<PyAny>>>,
    worker_started: AtomicBool,
}

#[pymethods]
impl PyLookupWorker {
    #[new]
    fn new(
        py: Python<'_>,
        shared_state: Py<PyAny>,
        popup_window: Py<PyAny>,
        engine: PyRef<'_, PyLookupEngine>,
        config: Py<PyAny>,
        logger: Py<PyAny>,
    ) -> PyResult<Self> {
        let queue = shared_state.getattr(py, "lookup_queue")?;
        let queue: PyRef<'_, PyLatestValueQueue> = queue.extract(py)?;
        Ok(Self {
            engine: Arc::clone(&engine.inner),
            shared_state,
            popup_window,
            config,
            logger,
            lookup_queue: Arc::clone(&queue.inner),
            worker_started: AtomicBool::new(false),
        })
    }

    fn start(&self, py: Python<'_>) -> PyResult<()> {
        if self.worker_started.swap(true, Ordering::AcqRel) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "threads can only be started once",
            ));
        }

        let runtime = LookupWorkerRuntime {
            engine: Arc::clone(&self.engine),
            shared_state: self.shared_state.clone_ref(py),
            popup_window: self.popup_window.clone_ref(py),
            config: self.config.clone_ref(py),
            logger: self.logger.clone_ref(py),
            lookup_queue: Arc::clone(&self.lookup_queue),
        };
        thread::Builder::new()
            .name("Lookup".to_owned())
            .spawn(move || runtime.run())
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
        Ok(())
    }
}

struct LookupWorkerRuntime {
    engine: Arc<Mutex<LookupEngine>>,
    shared_state: Py<PyAny>,
    popup_window: Py<PyAny>,
    config: Py<PyAny>,
    logger: Py<PyAny>,
    lookup_queue: Arc<LatestValueQueue<Py<PyAny>>>,
}

impl LookupWorkerRuntime {
    fn run(self) {
        python_log(&self.logger, "debug", "Lookup thread started.");
        let mut last_hit_result: Option<String> = None;
        while python_running(&self.shared_state) {
            self.lookup_queue.wait();
            let hit_result = Python::attach(|py| {
                self.lookup_queue
                    .get_with(|value| value.and_then(|value| value.extract(py).ok()))
            });
            if !python_running(&self.shared_state) {
                break;
            }
            python_log(&self.logger, "debug", "Lookup: Triggered");

            // skip lookup if hit_result didnt change
            if hit_result == last_hit_result {
                continue;
            }
            last_hit_result = hit_result;

            let result = self.lookup_and_update_popup(last_hit_result.as_deref());
            if let Err(error) = result {
                python_log(
                    &self.logger,
                    "error",
                    &format!(
                        "An unexpected error occurred in the lookup loop. Continuing: {error}"
                    ),
                );
            }
        }
        python_log(&self.logger, "debug", "Lookup thread stopped.");
    }

    fn lookup_and_update_popup(&self, lookup_string: Option<&str>) -> PyResult<()> {
        let lookup_result = if let Some(lookup_string) = lookup_string {
            python_log(
                &self.logger,
                "info",
                &format!("Looking up: {lookup_string}"),
            ); // keep at info level so people know whats up

            let (max_lookup_length, show_kanji) = Python::attach(|py| -> PyResult<_> {
                Ok((
                    self.config.getattr(py, "max_lookup_length")?.extract(py)?,
                    self.config.getattr(py, "show_kanji")?.extract(py)?,
                ))
            })?;
            let result = self
                .engine
                .lock()
                .map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("lookup engine lock was poisoned")
                })?
                .lookup_cached(lookup_string, max_lookup_length, show_kanji);
            Some(result)
        } else {
            None
        };

        Python::attach(|py| {
            let value = match lookup_result {
                Some(result) => lookup_result_to_python(py, result)?.into_any(),
                None => py.None().into_bound(py),
            };
            self.popup_window
                .call_method1(py, "set_latest_data", (value,))?;
            Ok(())
        })
    }
}

fn lookup_result_to_python<'py>(
    py: Python<'py>,
    result: CachedLookup,
) -> PyResult<Bound<'py, PyList>> {
    let module = py.import("meikipop.dictionary.lookup")?;
    let dictionary_entry = module.getattr("DictionaryEntry")?;
    let kanji_entry = module.getattr("KanjiEntry")?;
    let results = PyList::empty(py);

    for entry in result.entries {
        let process = PyTuple::new(py, &entry.deconjugation_process)?;
        let kwargs = entry.into_pyobject(py)?;
        kwargs.set_item("deconjugation_process", process)?;
        results.append(dictionary_entry.call((), Some(&kwargs))?)?;
    }
    if let Some(entry) = result.kanji_entry {
        let kwargs = entry.into_pyobject(py)?;
        results.append(kanji_entry.call((), Some(&kwargs))?)?;
    }
    Ok(results)
}

fn python_running(shared_state: &Py<PyAny>) -> bool {
    Python::attach(|py| {
        shared_state
            .getattr(py, "running")
            .and_then(|running| running.extract(py))
            .unwrap_or_else(|error| {
                log::error!("Could not read shared_state.running: {error}");
                false
            })
    })
}

fn python_log(logger: &Py<PyAny>, level: &str, message: &str) {
    Python::attach(|py| {
        if let Err(error) = logger.call_method1(py, level, (message,)) {
            log::error!("Could not forward lookup log message to Python: {error}");
        }
    });
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyLookupEngine>()?;
    module.add_class::<PyLookupWorker>()?;
    Ok(())
}

pub fn contains_kanji(text: &str) -> bool {
    text.chars().any(is_kanji)
}

pub fn is_kanji(character: char) -> bool {
    ('\u{4e00}'..='\u{9faf}').contains(&character)
}

pub fn calculate_priority(
    written_form: Option<&str>,
    freq: i64,
    form: &Form,
    match_len: usize,
    original_lookup: &str,
) -> f64 {
    let mut priority = match_len as f64;

    // Frequency: log scale maps rank 1..999_999 evenly to ~0..10
    // rank 1 → ~10, rank 1000 → ~5, rank 50000 → ~2.8, rank 999_999 → 0
    if freq < DEFAULT_FREQ {
        priority += 10.0 * (1.0 - (freq as f64).ln() / (DEFAULT_FREQ as f64).ln());
    }

    // Kana vs kanji preference
    let original_is_kana = !contains_kanji(original_lookup);
    let written_is_kana = written_form.is_none_or(|written| !contains_kanji(written));

    if original_is_kana {
        // Kana-only entry looked up via kana: small bonus
        if written_is_kana && form.process.is_empty() {
            priority += 3.0;
        }
    }

    // Deconjugation cost
    priority -= form.process.len() as f64;

    priority
}

pub fn hira_to_kata(text: &str) -> String {
    let mut res = vec![];

    for c in text.chars() {
        let code = c as u32;
        res.push(if (0x3041..=0x3096).contains(&code) {
            char::from_u32(code + 0x60).expect("hiragana conversion produces valid Unicode")
        } else {
            c
        });
    }

    res.into_iter().collect()
}

pub fn kata_to_hira(text: &str) -> String {
    let mut res = vec![];

    for c in text.chars() {
        let code = c as u32;
        if (0x30A1..=0x30F6).contains(&code) {
            res.push(
                char::from_u32(code - 0x60).expect("katakana conversion produces valid Unicode"),
            );
        } else if code == 0x30FD {
            res.push('\u{309D}'); // ヽ → ゝ
        } else if code == 0x30FE {
            res.push('\u{309E}'); // ヾ → ゞ
        } else {
            res.push(c);
        }
    }

    res.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_entry(
        written_form: Option<&str>,
        reading: Option<&str>,
        frequency: i64,
        entry_id: i64,
    ) -> MapEntry {
        MapEntry {
            written_form: written_form.map(str::to_owned),
            reading: reading.map(str::to_owned),
            frequency,
            entry_id,
        }
    }

    fn engine(
        entries: HashMap<i64, Vec<Sense>>,
        lookup_map: HashMap<String, Vec<MapEntry>>,
        max_dict_entries: usize,
    ) -> LookupEngine {
        LookupEngine::new(
            entries,
            lookup_map,
            HashMap::new(),
            Deconjugator::from_json("[]").unwrap(),
            max_dict_entries,
        )
    }

    #[test]
    fn detects_kanji() {
        assert!(contains_kanji("食べる"));
        assert!(!contains_kanji("たべる"));
        assert!(!contains_kanji(""));
    }

    #[test]
    fn prepares_lookup_text_like_upstream() {
        let engine = engine(HashMap::new(), HashMap::new(), 10);

        assert_eq!(engine.prepare_lookup_text("  食べました  ", 4), "食べまし");
        assert_eq!(engine.prepare_lookup_text("猫。犬", 10), "猫");
        assert_eq!(engine.prepare_lookup_text("猫!犬", 10), "猫");
        assert_eq!(engine.prepare_lookup_text("  ", 10), "");
        assert_eq!(engine.prepare_lookup_text("猫", 0), "");
    }

    #[test]
    fn calculates_upstream_priority_components() {
        let plain = Form::new("たべる");
        assert_eq!(
            calculate_priority(Some("たべる"), DEFAULT_FREQ, &plain, 3, "たべる"),
            6.0
        );

        let deconjugated = Form::with_details(
            "食べる",
            vec!["past".into(), "polite".into()],
            vec!["v1".into()],
        );
        assert_eq!(
            calculate_priority(Some("食べる"), DEFAULT_FREQ, &deconjugated, 3, "食べた"),
            1.0
        );

        let frequency_priority = calculate_priority(Some("食べる"), 1, &plain, 3, "食べる");
        assert!((frequency_priority - 13.0).abs() < 1e-10);
    }

    #[test]
    fn gets_exact_and_kana_fallback_entries() {
        let entry = map_entry(Some("食べる"), Some("たべる"), 10, 1);
        let lookup_map = HashMap::from([("タベル".into(), vec![entry.clone()])]);
        let engine = engine(HashMap::new(), lookup_map, 10);

        assert_eq!(engine.get_map_entries("タベル"), [entry.clone()]);
        assert_eq!(engine.get_map_entries("たべる"), [entry]);
        assert!(engine.get_map_entries("ない").is_empty());
    }

    #[test]
    fn looks_up_prefixes_and_builds_dictionary_entries() {
        let entries = HashMap::from([(
            1,
            vec![Sense {
                glosses: vec!["to eat".into()],
                pos: vec!["v1".into()],
                tags: vec![],
            }],
        )]);
        let lookup_map = HashMap::from([(
            "食べる".into(),
            vec![map_entry(Some("食べる"), Some("たべる"), 100, 1)],
        )]);
        let engine = engine(entries, lookup_map, 10);

        let result = engine.lookup("食べるもの");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].written_form.as_deref(), Some("食べる"));
        assert_eq!(result[0].reading, "たべる");
        assert_eq!(result[0].id, 1);
    }

    #[test]
    fn gets_kanji_entries() {
        let kanji_entry = KanjiEntry {
            character: "猫".into(),
            meanings: vec!["cat".into()],
            readings: vec!["ビョウ".into(), "ねこ".into()],
            components: vec![KanjiComponent {
                c: "犭".into(),
                m: None,
            }],
            examples: vec![KanjiExample {
                w: "子猫".into(),
                r: "こねこ".into(),
                m: "kitten".into(),
            }],
        };
        let engine = LookupEngine::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::from([("猫".into(), kanji_entry.clone())]),
            Deconjugator::from_json("[]").unwrap(),
            10,
        );

        assert_eq!(engine.get_kanji_entry("猫"), Some(&kanji_entry));
        assert_eq!(engine.get_kanji_entry("犬"), None);
    }

    #[test]
    fn caches_by_preprocessed_text_and_clears_the_cache() {
        let mut engine = engine(HashMap::new(), HashMap::new(), 10);

        engine.lookup_cached("  猫。犬  ", 10, false);
        engine.lookup_cached("猫", 10, false);

        assert_eq!(engine.lookup_cache.len(), 1);
        assert_eq!(engine.cache_order, ["猫"]);

        engine.clear_cache();

        assert!(engine.lookup_cache.is_empty());
        assert!(engine.cache_order.is_empty());
    }

    #[test]
    fn limits_the_lookup_cache_to_the_upstream_size() {
        let mut engine = engine(HashMap::new(), HashMap::new(), 10);

        for index in 0..=CACHE_SIZE {
            engine.lookup_cached(&format!("word{index}"), 20, false);
        }

        assert_eq!(engine.lookup_cache.len(), CACHE_SIZE);
        assert!(!engine.lookup_cache.contains_key("word0"));
        assert!(
            engine
                .lookup_cache
                .contains_key(&format!("word{CACHE_SIZE}"))
        );
    }

    #[test]
    fn converts_hiragana_to_katakana() {
        assert_eq!(hira_to_kata("たべる"), "タベル");
    }

    #[test]
    fn preserves_non_hiragana_characters() {
        assert_eq!(hira_to_kata("食べる123"), "食ベル123");
    }

    #[test]
    fn converts_katakana_to_hiragana() {
        assert_eq!(kata_to_hira("タベル"), "たべる");
    }

    #[test]
    fn converts_katakana_iteration_marks() {
        assert_eq!(kata_to_hira("ヽヾ"), "ゝゞ");
    }

    #[test]
    fn preserves_non_katakana_characters() {
        assert_eq!(kata_to_hira("食ベル123"), "食べる123");
    }
}
