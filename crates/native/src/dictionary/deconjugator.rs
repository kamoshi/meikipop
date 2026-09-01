use serde::Deserialize;
use std::collections::HashSet;

pub const MAX_DECONJ_ITERATIONS: usize = 10;

/// A JSON value that upstream permits to be either one string or a list.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn as_slice(&self) -> &[T] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

/// The typed equivalent of one upstream deconjugator rule dictionary.
#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    #[serde(rename = "type")]
    rule_type: Option<String>,
    dec_end: Option<OneOrMany<String>>,
    con_end: Option<OneOrMany<String>>,
    dec_tag: Option<OneOrMany<String>>,
    con_tag: Option<OneOrMany<String>>,
    #[serde(default)]
    detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Form {
    pub text: String,
    pub process: Vec<String>,
    pub tags: Vec<String>,
}

impl Form {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            process: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn with_details(text: impl Into<String>, process: Vec<String>, tags: Vec<String>) -> Self {
        Self {
            text: text.into(),
            process,
            tags,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Deconjugator {
    rules: Vec<Rule>,
}

impl Deconjugator {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// Parse the upstream JSON format, ignoring non-object entries just as the
    /// Python constructor ignores values that are not dictionaries.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        let values: Vec<serde_json::Value> = serde_json::from_str(json)?;
        let rules = values
            .into_iter()
            .filter(serde_json::Value::is_object)
            .map(serde_json::from_value)
            .collect::<serde_json::Result<Vec<_>>>()?;
        Ok(Self::new(rules))
    }

    pub fn deconjugate(&self, text: &str) -> HashSet<Form> {
        let clean_text = text.trim();
        if clean_text.is_empty() {
            return HashSet::new();
        }

        let initial_form = Form::new(clean_text);
        let mut processed = HashSet::new();
        let mut novel = HashSet::from([initial_form.clone()]);

        for _ in 0..MAX_DECONJ_ITERATIONS {
            if novel.is_empty() {
                break;
            }

            let mut new_novel = HashSet::new();
            for form in &novel {
                for rule in &self.rules {
                    let Some(rule_type) = rule.rule_type.as_deref() else {
                        continue;
                    };
                    if rule_type == "substitution" {
                        continue;
                    }

                    if rule_type == "onlyfinalrule" && !form.tags.is_empty() {
                        continue;
                    }
                    if rule_type == "neverfinalrule" && form.tags.is_empty() {
                        continue;
                    }

                    if let Some(new_forms) = self.apply_rule(form, rule) {
                        for new_form in new_forms {
                            if !processed.contains(&new_form) && !novel.contains(&new_form) {
                                new_novel.insert(new_form);
                            }
                        }
                    }
                }
            }

            processed.extend(novel);
            novel = new_novel;
        }

        processed.insert(initial_form);
        processed
    }

    fn apply_rule(&self, form: &Form, rule: &Rule) -> Option<HashSet<Form>> {
        let (Some(dec_ends), Some(con_ends)) = (&rule.dec_end, &rule.con_end) else {
            return None;
        };
        let dec_ends = dec_ends.as_slice();
        let con_ends = con_ends.as_slice();
        if dec_ends.is_empty() || con_ends.is_empty() {
            return None;
        }

        let dec_tags = rule.dec_tag.as_ref().map(OneOrMany::as_slice);
        let con_tags = rule.con_tag.as_ref().map(OneOrMany::as_slice);
        let max_len = dec_ends.len();
        let mut results = HashSet::new();

        for i in 0..max_len {
            let con_end = &con_ends[i % con_ends.len()];
            let con_tag = con_tags
                .filter(|tags| !tags.is_empty())
                .map(|tags| &tags[i % tags.len()]);
            let dec_end = &dec_ends[i % dec_ends.len()];
            let dec_tag = dec_tags
                .filter(|tags| !tags.is_empty())
                .map(|tags| &tags[i % tags.len()]);

            let Some(stem) = form.text.strip_suffix(con_end) else {
                continue;
            };

            let current_form_tag = form.tags.last();
            let is_starter_type = matches!(
                rule.rule_type.as_deref(),
                Some("stdrule" | "rewriterule" | "onlyfinalrule" | "contextrule")
            );
            let tag_matches = if form.tags.is_empty() && is_starter_type {
                true
            } else {
                current_form_tag == con_tag
            };

            if !tag_matches {
                continue;
            }
            if rule.rule_type.as_deref() == Some("rewriterule") && form.text != *con_end {
                continue;
            }

            let mut process = form.process.clone();
            process.push(rule.detail.clone());

            let mut tags = form.tags.clone();
            if !tags.is_empty() {
                tags.pop();
            }
            if let Some(dec_tag) = dec_tag {
                tags.push(dec_tag.clone());
            }

            results.insert(Form::with_details(
                format!("{stem}{dec_end}"),
                process,
                tags,
            ));
        }

        (!results.is_empty()).then_some(results)
    }
}

#[cfg(test)]
mod tests {
    use super::{Deconjugator, Form, Rule};
    use std::collections::HashSet;

    fn rules(json: &str) -> Vec<Rule> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn trims_and_returns_the_original_form() {
        let deconjugator = Deconjugator::new(Vec::new());

        assert_eq!(
            deconjugator.deconjugate("  食べる  "),
            HashSet::from([Form::new("食べる")])
        );
        assert!(deconjugator.deconjugate("  ").is_empty());
    }

    #[test]
    fn applies_a_starter_rule_and_a_followup_rule() {
        let deconjugator = Deconjugator::new(rules(
            r#"[
                {
                    "type": "stdrule",
                    "con_end": "ました",
                    "dec_end": "る",
                    "dec_tag": "v1",
                    "detail": "polite past"
                },
                {
                    "type": "neverfinalrule",
                    "con_end": "る",
                    "dec_end": "",
                    "con_tag": "v1",
                    "detail": "remove dictionary ending"
                }
            ]"#,
        ));

        let forms = deconjugator.deconjugate("食べました");

        assert!(forms.contains(&Form::new("食べました")));
        assert!(forms.contains(&Form::with_details(
            "食べる",
            vec!["polite past".into()],
            vec!["v1".into()],
        )));
        assert!(forms.contains(&Form::with_details(
            "食べ",
            vec!["polite past".into(), "remove dictionary ending".into()],
            Vec::new(),
        )));
    }

    #[test]
    fn supports_parallel_rule_arrays() {
        let deconjugator = Deconjugator::new(rules(
            r#"[{
                "type": "stdrule",
                "con_end": ["った", "んだ"],
                "dec_end": ["う", "む"],
                "dec_tag": ["v5u", "v5m"],
                "detail": "past"
            }]"#,
        ));

        assert!(
            deconjugator
                .deconjugate("読んだ")
                .contains(&Form::with_details(
                    "読む",
                    vec!["past".into()],
                    vec!["v5m".into()],
                ))
        );
    }

    #[test]
    fn parses_the_upstream_rule_file() {
        let json = include_str!("../../../../scripts/deconjugator.json");
        let deconjugator = Deconjugator::from_json(json).unwrap();

        assert!(!deconjugator.rules.is_empty());
    }
}
