//! Translations of the bundled iD tagging schema's preset names, search terms,
//! field labels, placeholders and option labels.
//!
//! The files live in `assets/presets/translations/<locale>.json` (made by
//! `examples/update_preset_translations.rs` from the upstream
//! `dist/translations/<locale>.min.json`) and are embedded by `build.rs`, so
//! shipping another language is just adding a file. They are independent of
//! the UI message catalogs in `locales/`: upstream covers many more languages
//! than the app's own text, so the preset locale is resolved on its own,
//! from the same requested locale the UI uses (`i18n::requested_locale`).
//!
//! Every lookup returns `None` when a locale or string is missing, and the
//! caller falls back to the English text in the schema. Only presentation
//! text is translated; tag keys and values never are.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/preset_translations.rs"));

#[derive(serde::Deserialize, Default)]
struct RawPreset {
    name: Option<String>,
    #[serde(default)]
    terms: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawField {
    label: Option<String>,
    placeholder: Option<String>,
    #[serde(default)]
    options: HashMap<String, String>,
}

#[derive(serde::Deserialize)]
struct RawFile {
    #[serde(default)]
    presets: HashMap<String, RawPreset>,
    #[serde(default)]
    fields: HashMap<String, RawField>,
}

struct PresetText {
    name: Option<String>,
    /// Lowercased once at load so a search keystroke doesn't allocate.
    name_lower: Option<String>,
    terms_lower: Vec<String>,
}

/// One locale's preset and field strings, parsed once.
pub struct PresetTranslations {
    presets: HashMap<String, PresetText>,
    fields: HashMap<String, RawField>,
}

impl PresetTranslations {
    /// Parse a translation file (the shape `update_preset_translations` writes).
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        let raw: RawFile = serde_json::from_str(body)?;
        let presets = raw
            .presets
            .into_iter()
            .map(|(id, p)| {
                let text = PresetText {
                    name_lower: p.name.as_ref().map(|n| n.to_lowercase()),
                    name: p.name,
                    terms_lower: p.terms.iter().map(|t| t.to_lowercase()).collect(),
                };
                (id, text)
            })
            .collect();
        Ok(Self {
            presets,
            fields: raw.fields,
        })
    }

    pub fn preset_name(&self, preset_id: &str) -> Option<&str> {
        self.presets.get(preset_id)?.name.as_deref()
    }

    /// Whether the lowercase `query` is part of the preset's translated name
    /// or of one of its translated search terms.
    pub fn preset_matches(&self, preset_id: &str, query_lower: &str) -> bool {
        let Some(p) = self.presets.get(preset_id) else {
            return false;
        };
        p.name_lower
            .as_deref()
            .is_some_and(|n| n.contains(query_lower))
            || p.terms_lower.iter().any(|t| t.contains(query_lower))
    }

    pub fn field_label(&self, field_id: &str) -> Option<&str> {
        self.fields.get(field_id)?.label.as_deref()
    }

    pub fn field_placeholder(&self, field_id: &str) -> Option<&str> {
        self.fields.get(field_id)?.placeholder.as_deref()
    }

    pub fn option_label(&self, field_id: &str, value: &str) -> Option<&str> {
        self.fields
            .get(field_id)?
            .options
            .get(value)
            .map(String::as_str)
    }
}

/// Locale tags that have a bundled translation file, sorted.
pub fn bundled_locales() -> Vec<String> {
    BUNDLED.iter().map(|(tag, _)| tag.to_string()).collect()
}

/// The bundled locale a requested one resolves to (`de-AT` -> `de`), or the
/// English fallback when none matches. Same rules as the UI language.
pub fn resolve_locale(requested: &str) -> String {
    crate::i18n::resolve_locale_in(requested, &bundled_locales())
}

/// Parsed translations are leaked so lookups can hand out `'static`
/// references; there is at most one entry per bundled locale.
type Cache = Mutex<HashMap<String, Option<&'static PresetTranslations>>>;
static CACHE: OnceLock<Cache> = OnceLock::new();

/// The translations for `requested` (any locale string: `de`, `de-AT`,
/// `de_DE.UTF-8`), parsed on first use. `None` means English: nothing is
/// bundled for it.
pub fn translations_for(requested: &str) -> Option<&'static PresetTranslations> {
    let locale = resolve_locale(requested);
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(found) = cache.get(&locale) {
        return *found;
    }
    let parsed = BUNDLED
        .iter()
        .find(|(tag, _)| *tag == locale)
        .map(|(tag, body)| {
            let parsed = PresetTranslations::from_json(body)
                .unwrap_or_else(|e| panic!("bundled preset translations `{tag}` must parse: {e}"));
            &*Box::leak(Box::new(parsed))
        });
    cache.insert(locale, parsed);
    parsed
}

static ACTIVE: OnceLock<Option<&'static PresetTranslations>> = OnceLock::new();

/// The translations for the language this run uses, resolved once from the
/// requested UI locale (`--language`, else the setting, else the system).
/// `None` for English, and before `i18n::init` has run (as in unit tests,
/// which pass an explicit locale to [`translations_for`] instead).
pub fn active() -> Option<&'static PresetTranslations> {
    *ACTIVE.get_or_init(|| translations_for(crate::i18n::requested_locale()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn german_is_bundled_and_parses() {
        assert!(bundled_locales().iter().any(|l| l == "de"));
        assert!(translations_for("de").is_some());
    }

    #[test]
    fn regional_and_platform_spellings_resolve_to_german() {
        assert_eq!(resolve_locale("de-AT"), "de");
        assert_eq!(resolve_locale("de_DE.UTF-8"), "de");
        assert!(translations_for("de-AT").is_some());
    }

    #[test]
    fn unknown_locale_has_no_translations() {
        assert_eq!(resolve_locale("tlh"), "en");
        assert!(translations_for("tlh").is_none());
        assert!(translations_for("en").is_none());
    }

    #[test]
    fn german_preset_names_and_fields() {
        let de = translations_for("de").unwrap();
        assert_eq!(de.preset_name("leisure/park"), Some("Park"));
        assert_eq!(de.preset_name("building"), Some("Gebäude"));
        assert_eq!(de.field_label("surface"), Some("Oberfläche"));
        assert_eq!(de.field_label("oneway"), Some("Einbahnstraße"));
        assert_eq!(
            de.option_label("surface", "gravel"),
            Some("Grober Schotter")
        );
        assert_eq!(de.preset_name("no/such/preset"), None);
        assert_eq!(de.option_label("surface", "no-such-value"), None);
    }

    #[test]
    fn matches_translated_name_and_terms() {
        let de = translations_for("de").unwrap();
        assert!(de.preset_matches("leisure/park", "park"));
        // A term that is not part of the name.
        assert!(de.preset_matches("leisure/park", "grünanlage"));
        assert!(!de.preset_matches("leisure/park", "tankstelle"));
        assert!(!de.preset_matches("no/such/preset", "park"));
    }

    #[test]
    fn every_bundled_file_parses_and_only_names_vendored_ids() {
        let presets = crate::presets::preset_index();
        let fields = crate::fields::field_index();
        for (tag, body) in BUNDLED {
            let t = PresetTranslations::from_json(body).unwrap();
            assert!(!t.presets.is_empty(), "{tag}: no presets");
            assert!(
                t.presets.keys().all(|id| presets.get(id).is_some()),
                "{tag}: translation for a preset that is not vendored"
            );
            assert!(
                t.fields.keys().all(|id| fields.get(id).is_some()),
                "{tag}: translation for a field that is not vendored"
            );
        }
    }
}
