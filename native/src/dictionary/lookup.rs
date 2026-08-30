use crate::dictionary::customdict;
use crate::dictionary::deconjugator::{Deconjugator, Form};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::borrow::Cow;
use std::collections::HashMap;

pub const DEFAULT_FREQ: i64 = 999_999;
pub const JAPANESE_SEPARATORS: &str = concat!(
    "、。「」｛｝（）【】",
    "『』〈〉《》：・／",
    "…︙‥︰＋＝－÷？！",
    "．～―!?",
);

/// The typed equivalent of one upstream sense dictionary.
#[derive(Clone, Debug, PartialEq, Eq, FromPyObject, IntoPyObject)]
pub struct Sense {
    #[pyo3(item)]
    pub glosses: Vec<String>,
    #[pyo3(item)]
    pub pos: Vec<String>,
    #[pyo3(item)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, FromPyObject)]
pub struct KanjiComponent {
    #[pyo3(item)]
    pub c: String,
    #[pyo3(item, default)]
    pub m: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, FromPyObject, IntoPyObject)]
pub struct KanjiExample {
    #[pyo3(item)]
    pub w: String,
    #[pyo3(item)]
    pub r: String,
    #[pyo3(item)]
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

#[derive(Clone, Debug, PartialEq, Eq, FromPyObject, IntoPyObject)]
pub struct KanjiEntry {
    #[pyo3(item)]
    pub character: String,
    #[pyo3(item)]
    pub meanings: Vec<String>,
    #[pyo3(item)]
    pub readings: Vec<String>,
    #[pyo3(item)]
    #[pyo3(into_py_with = components_to_python)]
    pub components: Vec<KanjiComponent>,
    #[pyo3(item)]
    pub examples: Vec<KanjiExample>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[pyclass(module = "meikipop_native.dictionary.lookup")]
pub struct LookupEngine {
    entries: HashMap<i64, Vec<Sense>>,
    lookup_map: HashMap<String, Vec<MapEntry>>,
    kanji_entries: HashMap<String, KanjiEntry>,
    deconjugator: Deconjugator,
    max_dict_entries: usize,
}

impl LookupEngine {
    pub fn new(
        entries: HashMap<i64, Vec<Sense>>,
        lookup_map: HashMap<String, Vec<MapEntry>>,
        kanji_entries: HashMap<String, KanjiEntry>,
        deconjugator: Deconjugator,
        max_dict_entries: usize,
    ) -> Self {
        Self {
            entries,
            lookup_map,
            kanji_entries,
            deconjugator,
            max_dict_entries,
        }
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

#[derive(FromPyObject)]
struct RawMapEntry(Option<String>, Option<String>, i64, i64);

#[pymethods]
impl LookupEngine {
    #[new]
    fn py_new(
        entries: &Bound<'_, PyAny>,
        lookup_map: &Bound<'_, PyAny>,
        kanji_entries: &Bound<'_, PyAny>,
        rules: &Bound<'_, PyAny>,
        max_dict_entries: usize,
    ) -> PyResult<Self> {
        let entries: HashMap<i64, Vec<Sense>> = entries.extract()?;
        let raw_lookup_map: HashMap<String, Vec<RawMapEntry>> = lookup_map.extract()?;
        let kanji_entries: HashMap<String, KanjiEntry> = kanji_entries.extract()?;
        let lookup_map = raw_lookup_map
            .into_iter()
            .map(|(surface, entries)| {
                let entries = entries
                    .into_iter()
                    .map(
                        |RawMapEntry(written_form, reading, frequency, entry_id)| MapEntry {
                            written_form,
                            reading,
                            frequency,
                            entry_id,
                        },
                    )
                    .collect();
                (surface, entries)
            })
            .collect();
        let deconjugator = Deconjugator::from_python(rules)?;

        Ok(Self::new(
            entries,
            lookup_map,
            kanji_entries,
            deconjugator,
            max_dict_entries,
        ))
    }

    #[pyo3(name = "prepare_lookup_text")]
    fn py_prepare_lookup_text(&self, lookup_string: &str, max_lookup_length: usize) -> String {
        self.prepare_lookup_text(lookup_string, max_lookup_length)
    }

    fn validate(&self) -> (usize, Vec<String>) {
        let result = customdict::validate(&self.entries, &self.lookup_map);
        (result.issues, result.warnings)
    }

    #[pyo3(name = "lookup", signature = (text, show_kanji=false))]
    fn py_lookup<'py>(
        &self,
        py: Python<'py>,
        text: &str,
        show_kanji: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let results = PyList::empty(py);
        for entry in self.lookup(text) {
            results.append(entry)?;
        }

        // Append kanji entry for the first character if applicable
        if show_kanji && text.chars().next().is_some_and(is_kanji) {
            if let Some(entry) = text
                .chars()
                .next()
                .and_then(|character| self.get_kanji_entry(&character.to_string()))
            {
                results.append(entry.clone())?;
            }
        }

        Ok(results)
    }

    #[pyo3(name = "get_kanji_entry")]
    fn py_get_kanji_entry(&self, character: &str) -> Option<KanjiEntry> {
        self.get_kanji_entry(character).cloned()
    }
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<LookupEngine>()?;
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
