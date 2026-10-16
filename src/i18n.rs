//! Bundled desktop translations with named, non-recursive placeholders.

use std::{
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

use serde::{Deserialize, Serialize};

/// The persisted user preference; system language is resolved at startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    System,
    English,
    Russian,
}

type Catalog = HashMap<String, String>;
static ENGLISH: OnceLock<Result<Catalog, serde_json::Error>> = OnceLock::new();
static RUSSIAN: OnceLock<Result<Catalog, serde_json::Error>> = OnceLock::new();
static ACTIVE: AtomicU8 = AtomicU8::new(1);

fn language_for_locale(locale: Option<&str>) -> Language {
    if locale
        .and_then(|locale| locale.split(['-', '_', '.']).next())
        .is_some_and(|language| language.eq_ignore_ascii_case("ru"))
    {
        Language::Russian
    } else {
        Language::English
    }
}

fn catalog(language: Language) -> Result<&'static Catalog, &'static serde_json::Error> {
    let (storage, source) = if language == Language::Russian {
        (&RUSSIAN, include_str!("../locales/ru.json"))
    } else {
        (&ENGLISH, include_str!("../locales/en.json"))
    };
    storage
        .get_or_init(|| serde_json::from_str(source))
        .as_ref()
}

fn translate(language: Language, key: &str) -> &'static str {
    match catalog(language) {
        Ok(catalog) => catalog
            .get(key)
            .map_or("[Missing translation]", String::as_str),
        Err(_) => "[Invalid language catalog]",
    }
}

/// Changes the GUI language. This never changes filenames or report field names.
pub fn set_language(language: Language) {
    let language = if language == Language::System {
        language_for_locale(sys_locale::get_locale().as_deref())
    } else {
        language
    };
    ACTIVE.store(
        if language == Language::Russian { 2 } else { 1 },
        Ordering::Relaxed,
    );
}

/// Looks up a bundled GUI string, returning an explicit marker for missing keys.
pub fn tr(key: &str) -> &'static str {
    translate(
        if ACTIVE.load(Ordering::Relaxed) == 2 {
            Language::Russian
        } else {
            Language::English
        },
        key,
    )
}

fn format_in(language: Language, key: &str, values: &[(&str, String)]) -> String {
    let mut remaining = translate(language, key);
    let mut result = String::with_capacity(remaining.len());
    while let Some(start) = remaining.find('{') {
        let Some(end) = remaining[start..].find('}') else {
            break;
        };
        let end = start + end;
        result.push_str(&remaining[..start]);
        let name = &remaining[start + 1..end];
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push_str(&remaining[start..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    result.push_str(remaining);
    result
}

/// Formats named placeholders once; inserted filenames are never interpreted.
pub fn trf(key: &str, values: &[(&str, String)]) -> String {
    format_in(
        if ACTIVE.load(Ordering::Relaxed) == 2 {
            Language::Russian
        } else {
            Language::English
        },
        key,
        values,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn system_language_recognizes_russian_locale_variants() {
        for locale in [Some("ru"), Some("ru_RU.UTF-8"), Some("RU-ru")] {
            assert_eq!(language_for_locale(locale), Language::Russian);
        }
        for locale in [Some("en-US"), Some("uk-UA"), None] {
            assert_eq!(language_for_locale(locale), Language::English);
        }
    }

    #[test]
    fn russian_catalog_returns_a_real_translation() {
        assert_eq!(translate(Language::Russian, "System"), "Системная");
    }

    #[test]
    fn named_values_are_not_reinterpreted_as_placeholders() {
        assert_eq!(
            format_in(
                Language::Russian,
                "{files} files scanned in {seconds} s",
                &[
                    ("files", "name{seconds}".into()),
                    ("seconds", "1.25".into())
                ]
            ),
            "Просканировано файлов: name{seconds}; время: 1.25 с"
        );
    }

    #[test]
    fn catalogs_have_equal_keys_and_named_placeholders() {
        let english = catalog(Language::English).expect("English JSON");
        let russian = catalog(Language::Russian).expect("Russian JSON");
        assert_eq!(
            english.keys().collect::<BTreeSet<_>>(),
            russian.keys().collect::<BTreeSet<_>>()
        );
        let placeholders =
            regex::Regex::new(r"\{([a-z][a-z0-9_]*)\}").expect("placeholder pattern");
        for (key, english_value) in english {
            let names = |value: &str| {
                placeholders
                    .captures_iter(value)
                    .map(|capture| capture[1].to_owned())
                    .collect::<BTreeSet<_>>()
            };
            assert!(!russian[key].is_empty(), "Empty translation for {key}");
            assert_eq!(
                names(english_value),
                names(&russian[key]),
                "Placeholders differ for {key}"
            );
        }
    }

    #[test]
    fn every_literal_gui_translation_key_exists_in_both_catalogs() {
        let calls =
            regex::Regex::new(r#"\btrf?\(\s*("(?:[^"\\]|\\.)*")"#).expect("GUI call pattern");
        for capture in calls.captures_iter(include_str!("app.rs")) {
            let key: String =
                serde_json::from_str(&capture[1]).expect("Rust key is JSON-compatible");
            for language in [Language::English, Language::Russian] {
                assert!(
                    catalog(language).expect("catalog").contains_key(&key),
                    "Missing {language:?} GUI translation: {key}"
                );
            }
        }
    }
}
