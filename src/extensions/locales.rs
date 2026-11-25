//! An extension's own translations: `locales/<tag>.toml` in its folder, one
//! file per language, with nested tables flattened to dotted keys:
//!
//! ```toml
//! # locales/de.toml
//! name = "MapRoulette"
//! [commands]
//! tasks = "MapRoulette-Aufgaben"
//! [panel]
//! load = "Daten laden"
//! tasks = "%{count} Aufgaben"
//! ```
//!
//! The app reads `name` and `commands.<id>` itself (for the menus);
//! every other key is the extension's, looked up through `translate`. A
//! lookup tries the app's language (`de-AT`), then its base language (`de`),
//! then `en`.

use std::collections::HashMap;
use std::path::Path;

/// Largest locale file read.
const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Locales {
    /// Lowercase tag -> dotted key -> message.
    catalogs: HashMap<String, HashMap<String, String>>,
}

impl Locales {
    /// The catalogs in `<dir>/locales`; none when there is no such folder.
    /// A file that does not parse is skipped with a message on stderr.
    pub fn load(dir: &Path) -> Locales {
        let mut catalogs = HashMap::new();
        let Ok(entries) = std::fs::read_dir(dir.join("locales")) else {
            return Locales::default();
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(tag) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                eprintln!("{}: too large, skipped", path.display());
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| Self::parse(&t))
            {
                Ok(messages) => {
                    catalogs.insert(tag.to_ascii_lowercase(), messages);
                }
                Err(e) => eprintln!("{}: {e}", path.display()),
            }
        }
        Locales { catalogs }
    }

    /// One catalog's messages, flattened.
    pub fn parse(text: &str) -> Result<HashMap<String, String>, String> {
        let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        flatten("", &table, &mut out);
        Ok(out)
    }

    #[cfg(test)]
    pub fn from_catalogs(catalogs: &[(&str, &str)]) -> Locales {
        Locales {
            catalogs: catalogs
                .iter()
                .map(|(tag, text)| (tag.to_ascii_lowercase(), Self::parse(text).unwrap()))
                .collect(),
        }
    }

    /// The tags to try for `locale`, best first.
    pub(crate) fn chain(locale: &str) -> Vec<String> {
        let mut tag = locale.replace('_', "-").to_ascii_lowercase();
        let mut chain = Vec::new();
        loop {
            chain.push(tag.clone());
            match tag.rfind('-') {
                Some(i) => tag.truncate(i),
                None => break,
            }
        }
        if !chain.iter().any(|t| t == "en") {
            chain.push("en".into());
        }
        chain
    }

    /// `key` in `locale`, if any catalog in the chain has it.
    pub fn lookup(&self, locale: &str, key: &str) -> Option<&str> {
        Self::chain(locale).iter().find_map(|tag| {
            self.catalogs
                .get(tag)
                .and_then(|c| c.get(key))
                .map(String::as_str)
        })
    }

    /// `key` in every catalog that has it, by language tag (for the index).
    pub fn all_of(&self, key: &str) -> std::collections::BTreeMap<String, String> {
        self.catalogs
            .iter()
            .filter_map(|(tag, c)| c.get(key).map(|m| (tag.clone(), m.clone())))
            .collect()
    }

    /// `key` in `locale` with `%{name}` replaced from `args`; the key itself
    /// when no catalog has it (so a missing message is visible).
    pub fn translate(&self, locale: &str, key: &str, args: &[(String, String)]) -> String {
        let Some(message) = self.lookup(locale, key) else {
            return key.to_string();
        };
        let mut out = message.to_string();
        for (name, value) in args {
            out = out.replace(&format!("%{{{name}}}"), value);
        }
        out
    }
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut HashMap<String, String>) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(key, s.clone());
            }
            toml::Value::Table(t) => flatten(&key, t, out),
            // Numbers and booleans are not messages; ignore them.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locales() -> Locales {
        Locales::from_catalogs(&[
            (
                "en",
                "name = \"Roulette\"\n[panel]\nload = \"Load Data\"\ntasks = \"%{count} tasks\"\nonly_en = \"x\"",
            ),
            ("de", "[panel]\nload = \"Daten laden\"\ntasks = \"%{count} Aufgaben\""),
            ("de-CH", "[panel]\nload = \"Date lade\""),
        ])
    }

    #[test]
    fn a_region_falls_back_to_its_language_then_english() {
        let l = locales();
        assert_eq!(l.lookup("de-CH", "panel.load"), Some("Date lade"));
        assert_eq!(l.lookup("de-CH", "panel.tasks"), Some("%{count} Aufgaben"));
        assert_eq!(l.lookup("de_AT", "panel.load"), Some("Daten laden"));
        assert_eq!(l.lookup("de", "panel.only_en"), Some("x"));
        assert_eq!(l.lookup("fr", "name"), Some("Roulette"));
        assert_eq!(l.lookup("fr", "nope"), None);
    }

    #[test]
    fn arguments_are_filled_in_and_a_missing_key_shows_itself() {
        let l = locales();
        let args = [("count".to_string(), "3".to_string())];
        assert_eq!(l.translate("de", "panel.tasks", &args), "3 Aufgaben");
        assert_eq!(l.translate("de", "panel.missing", &args), "panel.missing");
    }

    #[test]
    fn the_repository_extension_has_every_english_key_in_german() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/maproulette");
        let l = Locales::load(&dir);
        let en = l.catalogs.get("en").expect("en catalog");
        let de = l.catalogs.get("de").expect("de catalog");
        let mut missing: Vec<&String> = en.keys().filter(|k| !de.contains_key(*k)).collect();
        missing.sort();
        assert!(missing.is_empty(), "missing in de: {missing:?}");
    }
}
