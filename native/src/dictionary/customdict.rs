use crate::dictionary::deconjugator::{Deconjugator, Rule};
use crate::dictionary::lookup::{KanjiEntry, LookupEngine, MapEntry, Sense, contains_kanji};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;
use std::process::Command;
use std::time::UNIX_EPOCH;

pub const DICT_URL: &str =
    "https://github.com/rtr46/meikipop/releases/download/dictionary-latest/dictionary.zip";
const DICTIONARY_FILENAME: &str = "dictionary.pkl";
const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;
pub const FORMAT_VERSION: u32 = 1;
pub const PYTHON_EXECUTABLE: &str = "python";
pub const CONVERTER_SCRIPT: &str = include_str!("convert_dictionary.py");

#[derive(Debug, Deserialize)]
pub struct DictionaryData {
    pub format_version: u32,
    pub source_size: u64,
    pub source_mtime_ns: u64,
    pub entries: HashMap<i64, Vec<Sense>>,
    pub lookup_map: HashMap<String, Vec<MapEntry>>,
    #[serde(default)]
    pub kanji_entries: HashMap<String, KanjiEntry>,
    #[serde(default)]
    pub deconjugator_rules: Vec<Rule>,
}

impl DictionaryData {
    pub fn from_json_path(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        Self::from_json_reader(BufReader::new(file))
    }

    pub fn from_json_reader<R: Read>(reader: R) -> Result<Self, String> {
        let dictionary: Self =
            serde_json::from_reader(reader).map_err(|error| error.to_string())?;
        if dictionary.format_version != FORMAT_VERSION {
            return Err(format!(
                "Unsupported dictionary format version {} (expected {FORMAT_VERSION})",
                dictionary.format_version
            ));
        }
        Ok(dictionary)
    }

    pub fn matches_source(&self, pickle_path: &Path) -> Result<bool, String> {
        let (source_size, source_mtime_ns) = source_metadata(pickle_path)?;
        Ok(self.source_size == source_size && self.source_mtime_ns == source_mtime_ns)
    }

    pub fn load_or_convert(
        python_executable: &Path,
        pickle_path: &Path,
        json_path: &Path,
    ) -> Result<Self, String> {
        if let Ok(dictionary) = Self::from_json_path(json_path) {
            if dictionary.matches_source(pickle_path)? {
                return Ok(dictionary);
            }
        }

        convert_dictionary(python_executable, pickle_path, json_path)?;
        let dictionary = Self::from_json_path(json_path)?;
        if !dictionary.matches_source(pickle_path)? {
            return Err("Converted dictionary JSON does not match its source pickle".into());
        }
        Ok(dictionary)
    }

    pub fn into_lookup_engine(self, max_dict_entries: usize) -> LookupEngine {
        LookupEngine::new(
            self.entries,
            self.lookup_map,
            self.kanji_entries,
            Deconjugator::new(self.deconjugator_rules),
            max_dict_entries,
        )
    }
}

fn source_metadata(pickle_path: &Path) -> Result<(u64, u64), String> {
    let metadata = fs::metadata(pickle_path).map_err(|error| error.to_string())?;
    let modified = metadata
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    let modified_ns = u64::try_from(modified.as_nanos())
        .map_err(|_| "Dictionary modification time is out of range".to_owned())?;
    Ok((metadata.len(), modified_ns))
}

pub fn convert_dictionary(
    python_executable: &Path,
    pickle_path: &Path,
    json_path: &Path,
) -> Result<(), String> {
    let output = Command::new(python_executable)
        .arg("-c")
        .arg(CONVERTER_SCRIPT)
        .arg(pickle_path)
        .arg(json_path)
        .output()
        .map_err(|error| format!("Could not start dictionary converter: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Dictionary converter failed with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    Ok(())
}

pub fn download_dictionary(data_dir: &Path) -> Result<(), String> {
    let mut response = ureq::get(DICT_URL)
        .call()
        .map_err(|error| error.to_string())?;
    let data = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD_SIZE)
        .read_to_vec()
        .map_err(|error| error.to_string())?;
    extract_dictionary(io::Cursor::new(data), data_dir)
}

fn extract_dictionary<R: Read + Seek>(reader: R, data_dir: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(reader).map_err(|error| error.to_string())?;
    let mut dictionary = archive
        .by_name(DICTIONARY_FILENAME)
        .map_err(|error| error.to_string())?;
    if dictionary.size() > MAX_DOWNLOAD_SIZE {
        return Err(format!(
            "{DICTIONARY_FILENAME} exceeds the maximum supported size"
        ));
    }

    fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
    let destination = data_dir.join(DICTIONARY_FILENAME);
    let mut output = File::create(destination).map_err(|error| error.to_string())?;
    io::copy(&mut dictionary, &mut output).map_err(|error| error.to_string())?;
    Ok(())
}

pub struct ValidationResult {
    pub issues: usize,
    pub warnings: Vec<String>,
}

pub fn validate(
    entries: &HashMap<i64, Vec<Sense>>,
    lookup_map: &HashMap<String, Vec<MapEntry>>,
) -> ValidationResult {
    /*
    Scan the loaded dictionary for structural invariants and return warnings
    for any violations found. Never raises — validation is advisory only.

    Invariants checked:
      - Every map entry tuple has exactly 4 elements
      - written_form is a non-empty str or None (None is valid for kana-only)
      - reading is a str or None
      - freq is an int
      - entry_id exists in entries
      - A map entry reached via a kanji-containing key must not have
        written_form=None (that would render as an invisible entry)

    The first four invariants are enforced when deserializing each
    tuple into MapEntry, before LookupEngine can be constructed.
    */
    let mut issues = 0;
    let mut warnings = Vec::new();
    let mut missing_entry_ids = HashSet::new();

    for (surface, me_list) in lookup_map {
        let surface_has_kanji = contains_kanji(surface);
        for me in me_list {
            if surface_has_kanji && me.written_form.is_none() {
                warnings.push(format!(
                    concat!(
                        "Map entry under kanji key '{}' has written_form=None ",
                        "(entry will display incorrectly) — entry_id={}"
                    ),
                    surface, me.entry_id
                ));
                issues += 1;
            }

            if !entries.contains_key(&me.entry_id) {
                missing_entry_ids.insert(me.entry_id);
                issues += 1;
            }
        }
    }

    if !missing_entry_ids.is_empty() {
        let mut missing_entry_ids: Vec<_> = missing_entry_ids.into_iter().collect();
        missing_entry_ids.sort_unstable();
        warnings.push(format!(
            concat!(
                "{} entry ID(s) referenced in lookup_map ",
                "have no matching core entry — first few: {:?}"
            ),
            missing_entry_ids.len(),
            &missing_entry_ids[..missing_entry_ids.len().min(5)]
        ));
    }

    ValidationResult { issues, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn map_entry(written_form: Option<&str>, entry_id: i64) -> MapEntry {
        MapEntry {
            written_form: written_form.map(str::to_owned),
            reading: Some("かな".into()),
            frequency: 10,
            entry_id,
        }
    }

    #[test]
    fn valid_dictionary_has_no_issues() {
        let entries = HashMap::from([(1, Vec::new())]);
        let lookup_map = HashMap::from([("かな".into(), vec![map_entry(None, 1)])]);

        let result = validate(&entries, &lookup_map);

        assert_eq!(result.issues, 0);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn reports_invisible_kanji_entries_and_missing_ids() {
        let entries = HashMap::new();
        let lookup_map = HashMap::from([("猫".into(), vec![map_entry(None, 42)])]);

        let result = validate(&entries, &lookup_map);

        assert_eq!(result.issues, 2);
        assert_eq!(result.warnings.len(), 2);
        assert!(result.warnings[0].contains("written_form=None"));
        assert!(result.warnings[1].contains("42"));
    }

    #[test]
    fn loads_the_versioned_json_dictionary() {
        let json = r#"{
            "format_version": 1,
            "source_size": 123,
            "source_mtime_ns": 456,
            "entries": {
                "1": [{"glosses": ["cat"], "pos": ["n"], "tags": []}]
            },
            "lookup_map": {
                "猫": [["猫", "ねこ", 10, 1]]
            },
            "kanji_entries": {
                "猫": {
                    "character": "猫",
                    "meanings": ["cat"],
                    "readings": ["ねこ"],
                    "components": [{"c": "犭"}],
                    "examples": [{"w": "子猫", "r": "こねこ", "m": "kitten"}]
                }
            },
            "deconjugator_rules": []
        }"#;

        let dictionary = DictionaryData::from_json_reader(json.as_bytes()).unwrap();

        assert_eq!(dictionary.source_size, 123);
        assert_eq!(dictionary.source_mtime_ns, 456);
        assert_eq!(dictionary.entries.len(), 1);
        assert_eq!(dictionary.lookup_map["猫"][0].entry_id, 1);
        assert_eq!(dictionary.kanji_entries["猫"].components[0].m, None);

        let engine = dictionary.into_lookup_engine(10);
        assert_eq!(engine.lookup("猫")[0].written_form.as_deref(), Some("猫"));
        assert_eq!(engine.get_kanji_entry("猫").unwrap().character, "猫");
    }

    #[test]
    fn rejects_an_unknown_json_format_version() {
        let json = r#"{
            "format_version": 2,
            "source_size": 0,
            "source_mtime_ns": 0,
            "entries": {},
            "lookup_map": {}
        }"#;

        let error = DictionaryData::from_json_reader(json.as_bytes()).unwrap_err();

        assert!(error.contains("Unsupported dictionary format version 2"));
    }

    #[test]
    fn reuses_current_json_and_reconverts_stale_json() {
        let data_dir = std::env::temp_dir().join(format!(
            "meikipop-json-freshness-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&data_dir).unwrap();
        let pickle_path = data_dir.join(DICTIONARY_FILENAME);
        let json_path = data_dir.join("dictionary.json");
        fs::write(&pickle_path, b"pickle data").unwrap();
        let (source_size, source_mtime_ns) = source_metadata(&pickle_path).unwrap();
        fs::write(
            &json_path,
            format!(
                r#"{{
                    "format_version": 1,
                    "source_size": {source_size},
                    "source_mtime_ns": {source_mtime_ns},
                    "entries": {{}},
                    "lookup_map": {{}}
                }}"#
            ),
        )
        .unwrap();
        let missing_python = data_dir.join("missing-python");

        let dictionary =
            DictionaryData::load_or_convert(&missing_python, &pickle_path, &json_path).unwrap();
        assert!(dictionary.matches_source(&pickle_path).unwrap());

        fs::write(&pickle_path, b"changed pickle data").unwrap();
        let error =
            DictionaryData::load_or_convert(&missing_python, &pickle_path, &json_path).unwrap_err();
        assert!(error.contains("Could not start dictionary converter"));

        fs::remove_file(json_path).unwrap();
        fs::remove_file(pickle_path).unwrap();
        fs::remove_dir(data_dir).unwrap();
    }

    #[test]
    fn extracts_only_the_dictionary_file() {
        let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        archive
            .start_file("ignored.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"ignored").unwrap();
        archive
            .start_file(DICTIONARY_FILENAME, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"pickle data").unwrap();
        let archive = archive.finish().unwrap().into_inner();

        let data_dir =
            std::env::temp_dir().join(format!("meikipop-customdict-test-{}", std::process::id()));
        fs::create_dir_all(&data_dir).unwrap();

        extract_dictionary(io::Cursor::new(archive), &data_dir).unwrap();

        assert_eq!(
            fs::read(data_dir.join(DICTIONARY_FILENAME)).unwrap(),
            b"pickle data"
        );
        assert!(!data_dir.join("ignored.txt").exists());
        fs::remove_file(data_dir.join(DICTIONARY_FILENAME)).unwrap();
        fs::remove_dir(data_dir).unwrap();
    }
}
