use crate::dictionary::deconjugator::{Deconjugator, Form};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const DEFAULT_FREQ: i64 = 999_999;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    pub written_form: Option<String>,
    pub reading: Option<String>,
    pub frequency: i64,
    pub entry_id: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DictionaryEntry {
    pub id: i64,
    pub written_form: Option<String>,
    pub reading: String,
    pub senses: Vec<Value>,
    pub freq: i64,
    pub deconjugation_process: Vec<String>,
    pub priority: f64,
}

#[derive(Debug)]
struct MergedEntry {
    id: i64,
    written_form: Option<String>,
    reading: String,
    senses: Vec<Value>,
    freq: i64,
    deconjugation_process: Vec<String>,
    priority: f64,
    match_len: usize,
}

#[pyclass(module = "meikipop_native.dictionary.lookup")]
pub struct LookupEngine {
    entries: HashMap<i64, Vec<Value>>,
    lookup_map: HashMap<String, Vec<MapEntry>>,
    deconjugator: Deconjugator,
    max_dict_entries: usize,
}

impl LookupEngine {
    pub fn new(
        entries: HashMap<i64, Vec<Value>>,
        lookup_map: HashMap<String, Vec<MapEntry>>,
        deconjugator: Deconjugator,
        max_dict_entries: usize,
    ) -> Self {
        Self {
            entries,
            lookup_map,
            deconjugator,
            max_dict_entries,
        }
    }

    pub fn lookup(&self, text: &str) -> Vec<DictionaryEntry> {
        self.do_lookup(text)
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
                            senses.iter().any(|sense| {
                                sense
                                    .get("pos")
                                    .and_then(Value::as_array)
                                    .is_some_and(|pos| {
                                        pos.iter().any(|value| value.as_str() == Some(required_pos))
                                    })
                            })
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

#[pymethods]
impl LookupEngine {
    #[new]
    fn py_new(
        entries: &Bound<'_, PyAny>,
        lookup_map: &Bound<'_, PyAny>,
        rules: &Bound<'_, PyAny>,
        max_dict_entries: usize,
    ) -> PyResult<Self> {
        let json = entries.py().import("json")?;
        let entries_json = json
            .call_method1("dumps", (entries,))?
            .extract::<String>()?;
        let lookup_map_json = json
            .call_method1("dumps", (lookup_map,))?
            .extract::<String>()?;
        let rules_json = json.call_method1("dumps", (rules,))?.extract::<String>()?;

        let entries: HashMap<i64, Vec<Value>> = serde_json::from_str(&entries_json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        let raw_lookup_map: HashMap<String, Vec<RawMapEntry>> =
            serde_json::from_str(&lookup_map_json)
                .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
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
        let deconjugator = Deconjugator::from_json(&rules_json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;

        Ok(Self::new(
            entries,
            lookup_map,
            deconjugator,
            max_dict_entries,
        ))
    }

    #[pyo3(name = "lookup")]
    fn py_lookup(&self, py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
        let serialized = serde_json::to_string(&self.lookup(text))
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        Ok(py
            .import("json")?
            .call_method1("loads", (serialized,))?
            .unbind())
    }
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<LookupEngine>()?;
    Ok(())
}

pub fn contains_kanji(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9faf}').contains(&c))
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
    use serde_json::json;

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
        entries: HashMap<i64, Vec<Value>>,
        lookup_map: HashMap<String, Vec<MapEntry>>,
        max_dict_entries: usize,
    ) -> LookupEngine {
        LookupEngine::new(
            entries,
            lookup_map,
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
            vec![json!({"glosses": ["to eat"], "pos": ["v1"], "tags": []})],
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
