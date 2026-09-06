use crate::dictionary::deconjugator::{Deconjugator, Rule};
use crate::dictionary::lookup::{KanjiEntry, LookupEngine, MapEntry, Sense, contains_kanji};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

pub const DICT_URL: &str =
    "https://github.com/kamoshi/meikipop/releases/download/dictionary-latest/dictionary.zip";
const DICTIONARY_FILENAME: &str = "dictionary.cbor";
const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct DictionaryData {
    pub entries: HashMap<i64, Vec<Sense>>,
    pub lookup_map: HashMap<String, Vec<MapEntry>>,
    #[serde(default)]
    pub kanji_entries: HashMap<String, KanjiEntry>,
    #[serde(default)]
    pub deconjugator_rules: Vec<Rule>,
}

impl DictionaryData {
    pub fn from_cbor_path(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        Self::from_cbor_reader(BufReader::new(file))
    }

    pub fn from_cbor_reader<R: Read>(reader: R) -> Result<Self, String> {
        ciborium::from_reader(reader).map_err(|error| error.to_string())
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
    use serde::Serialize;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[derive(Serialize)]
    struct TestSense<'a> {
        glosses: Vec<&'a str>,
        pos: Vec<&'a str>,
        tags: Vec<&'a str>,
    }

    #[derive(Serialize)]
    struct TestKanjiComponent<'a> {
        c: &'a str,
    }

    #[derive(Serialize)]
    struct TestKanjiExample<'a> {
        w: &'a str,
        r: &'a str,
        m: &'a str,
    }

    #[derive(Serialize)]
    struct TestKanjiEntry<'a> {
        character: &'a str,
        meanings: Vec<&'a str>,
        readings: Vec<&'a str>,
        components: Vec<TestKanjiComponent<'a>>,
        examples: Vec<TestKanjiExample<'a>>,
    }

    #[derive(Serialize)]
    struct TestDictionary<'a> {
        entries: HashMap<i64, Vec<TestSense<'a>>>,
        lookup_map: HashMap<&'a str, Vec<(Option<&'a str>, Option<&'a str>, i64, i64)>>,
        kanji_entries: HashMap<&'a str, TestKanjiEntry<'a>>,
        deconjugator_rules: Vec<serde_json::Value>,
    }

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
    fn loads_cbor_dictionary() {
        let payload = TestDictionary {
            entries: HashMap::from([(
                1,
                vec![TestSense {
                    glosses: vec!["cat"],
                    pos: vec!["n"],
                    tags: Vec::new(),
                }],
            )]),
            lookup_map: HashMap::from([("猫", vec![(Some("猫"), Some("ねこ"), 10, 1)])]),
            kanji_entries: HashMap::from([(
                "猫",
                TestKanjiEntry {
                    character: "猫",
                    meanings: vec!["cat"],
                    readings: vec!["ねこ"],
                    components: vec![TestKanjiComponent { c: "犭" }],
                    examples: vec![TestKanjiExample {
                        w: "子猫",
                        r: "こねこ",
                        m: "kitten",
                    }],
                },
            )]),
            deconjugator_rules: Vec::new(),
        };
        let mut cbor = Vec::new();
        ciborium::into_writer(&payload, &mut cbor).unwrap();

        let dictionary = DictionaryData::from_cbor_reader(cbor.as_slice()).unwrap();

        assert_eq!(dictionary.entries.len(), 1);
        assert_eq!(dictionary.lookup_map["猫"][0].entry_id, 1);
        assert_eq!(dictionary.kanji_entries["猫"].components[0].m, None);

        let engine = dictionary.into_lookup_engine(10);
        assert_eq!(engine.lookup("猫")[0].written_form.as_deref(), Some("猫"));
        assert_eq!(engine.get_kanji_entry("猫").unwrap().character, "猫");
    }

    #[test]
    #[ignore = "requires a generated dictionary.cbor"]
    fn loads_generated_cbor_dictionary() {
        let path = std::env::var_os("MEIKIPOP_DICTIONARY_PATH")
            .map(std::path::PathBuf::from)
            .expect("MEIKIPOP_DICTIONARY_PATH must point to dictionary.cbor");

        let dictionary = DictionaryData::from_cbor_path(&path).unwrap();

        assert!(!dictionary.entries.is_empty());
        assert!(!dictionary.lookup_map.is_empty());
        assert!(!dictionary.kanji_entries.is_empty());
        assert!(!dictionary.deconjugator_rules.is_empty());
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
        archive.write_all(b"cbor data").unwrap();
        let archive = archive.finish().unwrap().into_inner();

        let data_dir =
            std::env::temp_dir().join(format!("meikipop-customdict-test-{}", std::process::id()));
        fs::create_dir_all(&data_dir).unwrap();

        extract_dictionary(io::Cursor::new(archive), &data_dir).unwrap();

        assert_eq!(
            fs::read(data_dir.join(DICTIONARY_FILENAME)).unwrap(),
            b"cbor data"
        );
        assert!(!data_dir.join("ignored.txt").exists());
        fs::remove_file(data_dir.join(DICTIONARY_FILENAME)).unwrap();
        fs::remove_dir(data_dir).unwrap();
    }
}
