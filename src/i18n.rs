//! Localization: message lookup, plural selection, and locale resolution.
//!
//! Catalogs are YAML files under `locales/` (one per language, named by its
//! locale tag, e.g. `locales/en.yml`), compiled into the binary by
//! `rust-i18n`. English is the fallback for any key a catalog lacks.
//!
//! UI code uses the [`tr!`](crate::tr) macro:
//!
//! ```ignore
//! tr!("menu.file.open")                       // plain message
//! tr!("status.cursor", lat = lat, lon = lon)  // named `%{lat}` placeholders
//! tr_count!("status.pending_edits", n)        // plural: picks `.one` / `.other` / ...
//! ```
//!
//! Never build a sentence out of translated fragments; give the translator
//! the whole sentence with named placeholders instead.
//!
//! Tests run in the `en` locale because nothing in a test process calls
//! [`init`] (the library default is `en`); tests that need another language
//! pass the locale explicitly to [`lookup_in`] and never touch the global.

use gpui::SharedString;
use std::borrow::Cow;
use std::fmt::Display;

/// Locale used when nothing better matches, and the source language of the
/// message keys.
pub const FALLBACK_LOCALE: &str = "en";

/// Extra in-memory catalogs registered next to the files in `locales/`. Empty
/// in release builds; unit tests register a throwaway locale here so locale
/// switching can be tested without shipping a fake language.
pub fn extra_backend() -> rust_i18n::SimpleBackend {
    #[allow(unused_mut)]
    let mut backend = rust_i18n::SimpleBackend::new();
    #[cfg(test)]
    {
        let mut zz = std::collections::HashMap::new();
        zz.insert(
            Cow::Borrowed("meta.language_name"),
            Cow::Borrowed("Test Language"),
        );
        zz.insert(
            Cow::Borrowed("test.greeting"),
            Cow::Borrowed("Hola %{name}"),
        );
        zz.insert(
            Cow::Borrowed("test.items.one"),
            Cow::Borrowed("%{count} uno"),
        );
        zz.insert(
            Cow::Borrowed("test.items.other"),
            Cow::Borrowed("%{count} muchos"),
        );
        backend.add_translations(Cow::Borrowed("zz"), zz);
    }
    backend
}

/// Locales that have a catalog, sorted.
pub fn available_locales() -> Vec<String> {
    crate::_rust_i18n_available_locales()
        .into_iter()
        .map(|l| l.into_owned())
        .collect()
}

/// The locale messages are currently looked up in.
pub fn current_locale() -> String {
    rust_i18n::locale().to_string()
}

/// Reduce a platform locale string (`en_US.UTF-8`, `fr-CA`, `de_DE@euro`) to a
/// BCP 47-style tag (`en-US`, `fr-CA`, `de-DE`).
pub fn normalize_locale(raw: &str) -> String {
    let base = raw.split(['.', '@']).next().unwrap_or(raw);
    base.replace('_', "-")
}

/// Pick the best available locale for `requested` (a platform or settings
/// string): an exact match, else the same language with fewer subtags
/// (`fr-CA` -> `fr`), else English. Case-insensitive.
pub fn resolve_locale_in(requested: &str, available: &[String]) -> String {
    let mut candidate = normalize_locale(requested);
    loop {
        if let Some(found) = available
            .iter()
            .find(|a| a.eq_ignore_ascii_case(&candidate))
        {
            return found.clone();
        }
        match candidate.rfind('-') {
            Some(i) => candidate.truncate(i),
            None => return FALLBACK_LOCALE.to_string(),
        }
    }
}

/// Choose the locale for this run: the user's `language` setting when set
/// (`Some("fr")`), else the system locale, else English.
pub fn choose_locale(setting: Option<&str>, system: Option<&str>, available: &[String]) -> String {
    match setting.or(system) {
        Some(requested) => resolve_locale_in(requested, available),
        None => FALLBACK_LOCALE.to_string(),
    }
}

/// Select the UI language for the process. Call once at startup, before any
/// menu or window is built; nothing in the test suite calls this.
pub fn init(setting: Option<&str>) {
    let system = sys_locale::get_locale();
    let chosen = choose_locale(setting, system.as_deref(), &available_locales());
    rust_i18n::set_locale(&chosen);
    let _ = REQUESTED.set(setting.map(str::to_string).or(system));
}

/// What the user asked for (the `language` setting or `--language`, else the
/// system locale), as given, before resolving it against the UI catalogs.
/// Other data with its own, wider set of languages (the preset translations)
/// resolves this against what it has. `None` before [`init`] has run.
static REQUESTED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

pub fn requested_locale() -> Option<&'static str> {
    REQUESTED.get()?.as_deref()
}

/// The message for `key` in `locale`, with the fallback chain applied
/// (`fr-CA` -> `fr` -> `en`). `None` when no catalog has the key at all.
pub fn lookup_in(locale: &str, key: &str) -> Option<Cow<'static, str>> {
    crate::_rust_i18n_try_translate(locale, key)
}

/// The message for `key` in the current locale. A key missing from every
/// catalog yields `"<locale>.<key>"` (and trips the key-coverage test), so a
/// missing message is visible rather than blank.
pub fn lookup(key: &str) -> Cow<'static, str> {
    let locale = rust_i18n::locale();
    match lookup_in(&locale, key) {
        Some(message) => message,
        None => Cow::Owned(format!("{}.{key}", &*locale)),
    }
}

/// The message for a key built at run time (`format!("shortcut.{id}")`). Such
/// keys can't be checked by the source scan in the tests, so callers add a
/// test that walks every id they can produce.
pub fn tr_dynamic(key: &str) -> SharedString {
    substitute(lookup(key), &[])
}

/// Replace `%{name}` placeholders in `message` with the matching argument.
pub fn substitute(message: Cow<'static, str>, args: &[(&str, &dyn Display)]) -> SharedString {
    if args.is_empty() || !message.contains("%{") {
        return SharedString::from(message);
    }
    let names: Vec<&str> = args.iter().map(|(name, _)| *name).collect();
    let values: Vec<String> = args.iter().map(|(_, value)| value.to_string()).collect();
    SharedString::from(rust_i18n::replace_patterns(&message, &names, &values))
}

/// CLDR-style plural category of `count` in `locale`, the suffix of the
/// message key (`status.pending_edits.one`). Only the categories English and
/// most European languages need are implemented; a language with more forms
/// (few, many, ...) adds an arm here and the matching keys to its catalog.
pub fn plural_category(locale: &str, count: u64) -> &'static str {
    let language = locale.split('-').next().unwrap_or(locale);
    match language {
        // No grammatical number.
        "ja" | "zh" | "ko" | "vi" | "th" | "id" => "other",
        // French treats 0 as singular too.
        "fr" => {
            if count <= 1 {
                "one"
            } else {
                "other"
            }
        }
        _ => {
            if count == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

/// The plural form of `key` for `count` in the current locale, falling back to
/// the `.other` form when a catalog lacks the specific category. `%{count}` is
/// filled in.
pub fn plural(key: &str, count: u64) -> SharedString {
    plural_in(&rust_i18n::locale(), key, count)
}

/// [`plural`] for an explicit locale.
pub fn plural_in(locale: &str, key: &str, count: u64) -> SharedString {
    plural_with_in(locale, key, count, &[])
}

/// [`plural`] with further `%{name}` placeholders besides `%{count}`, for a
/// message that also embeds another (already pluralised) piece of text.
pub fn plural_with(key: &str, count: u64, args: &[(&str, &dyn Display)]) -> SharedString {
    plural_with_in(&rust_i18n::locale(), key, count, args)
}

/// [`plural_with`] for an explicit locale.
pub fn plural_with_in(
    locale: &str,
    key: &str,
    count: u64,
    args: &[(&str, &dyn Display)],
) -> SharedString {
    let category = plural_category(locale, count);
    let message = lookup_in(locale, &format!("{key}.{category}"))
        .or_else(|| lookup_in(locale, &format!("{key}.other")))
        .unwrap_or_else(|| Cow::Owned(format!("{locale}.{key}.{category}")));
    let mut all: Vec<(&str, &dyn Display)> = vec![("count", &count)];
    all.extend_from_slice(args);
    substitute(message, &all)
}

/// Look up a UI message in the current locale: `tr!("key")` or
/// `tr!("key", name = value, ...)` for `%{name}` placeholders. Returns a
/// `SharedString`, which is cheap to clone and can be used as a GPUI child.
#[macro_export]
macro_rules! tr {
    ($key:expr) => {
        $crate::i18n::substitute($crate::i18n::lookup($key), &[])
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::substitute(
            $crate::i18n::lookup($key),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// Plural-aware lookup: `tr_count!("status.pending_edits", n)` picks the
/// `.one` / `.other` (or language-specific) message and fills in `%{count}`.
#[macro_export]
macro_rules! tr_count {
    ($key:expr, $count:expr) => {
        $crate::i18n::plural($key, ($count) as u64)
    };
    ($key:expr, $count:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::plural_with(
            $key,
            ($count) as u64,
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn avail(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn english_catalog_is_available() {
        assert!(available_locales().iter().any(|l| l == "en"));
        assert_eq!(
            lookup_in("en", "meta.language_name").as_deref(),
            Some("English")
        );
    }

    #[test]
    fn placeholder_substitution() {
        let greeting = substitute(
            lookup_in("zz", "test.greeting").unwrap(),
            &[("name", &"Ana")],
        );
        assert_eq!(greeting, "Hola Ana");
        // Unknown placeholders are left alone rather than dropped.
        let kept = substitute(Cow::Borrowed("a %{x} b"), &[("y", &1)]);
        assert_eq!(kept, "a %{x} b");
        // The macro form, against the real English catalog.
        assert_eq!(crate::tr!("meta.language_name"), "English");
    }

    #[test]
    fn plural_picks_the_right_form() {
        assert_eq!(plural_category("en", 0), "other");
        assert_eq!(plural_category("en", 1), "one");
        assert_eq!(plural_category("en", 2), "other");
        assert_eq!(plural_category("fr-CA", 0), "one");
        assert_eq!(plural_category("ja", 1), "other");
        assert_eq!(plural_in("zz", "test.items", 1), "1 uno");
        assert_eq!(plural_in("zz", "test.items", 5), "5 muchos");
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        let available = avail(&["en", "zz"]);
        assert_eq!(resolve_locale_in("tlh", &available), "en");
        assert_eq!(resolve_locale_in("xx-YY", &available), "en");
        // And messages in an unknown locale come from English.
        assert_eq!(
            lookup_in("tlh", "meta.language_name").as_deref(),
            Some("English")
        );
    }

    #[test]
    fn regional_variants_resolve_to_their_language() {
        let available = avail(&["en", "zz"]);
        assert_eq!(resolve_locale_in("zz_ZZ.UTF-8", &available), "zz");
        assert_eq!(resolve_locale_in("EN-us", &available), "en");
        assert_eq!(normalize_locale("de_DE@euro"), "de-DE");
    }

    #[test]
    fn setting_overrides_system_locale() {
        let available = avail(&["en", "zz"]);
        assert_eq!(choose_locale(None, Some("zz-ZZ"), &available), "zz");
        assert_eq!(choose_locale(Some("en"), Some("zz-ZZ"), &available), "en");
        assert_eq!(choose_locale(Some("zz"), Some("en-US"), &available), "zz");
        assert_eq!(choose_locale(None, None, &available), "en");
        // An override naming a language without a catalog falls back too.
        assert_eq!(choose_locale(Some("qq"), Some("zz"), &available), "en");
    }

    #[test]
    fn test_locale_is_english() {
        // Nothing in the test suite calls `init`, so lookups use English.
        assert_eq!(current_locale(), "en");
    }

    /// Flatten a catalog file into `(dotted.key, raw value)` pairs. The
    /// catalogs use a fixed subset of YAML (two-space indentation, `key:`
    /// parents, `key: "value"` leaves, `#` comment lines), so a line-based
    /// reader is enough and avoids a YAML dev-dependency.
    fn flatten_catalog(text: &str) -> Vec<(String, String)> {
        let mut path: Vec<(usize, String)> = Vec::new();
        let mut out = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.len() - trimmed.len();
            let Some((key, value)) = trimmed.split_once(':') else {
                panic!("unparseable catalog line: {line}");
            };
            while path.last().is_some_and(|(i, _)| *i >= indent) {
                path.pop();
            }
            let value = value.trim();
            if value.is_empty() {
                path.push((indent, key.to_string()));
            } else {
                let mut full: Vec<&str> = path.iter().map(|(_, k)| k.as_str()).collect();
                full.push(key);
                let value = value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(value);
                out.push((full.join("."), value.to_string()));
            }
        }
        out
    }

    /// The sorted, distinct `%{name}` placeholder names in `message`.
    fn placeholders(message: &str) -> Vec<String> {
        let mut names: Vec<String> = message
            .split("%{")
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name.to_string()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    const PLURAL_CATEGORIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

    /// A key with a trailing plural category removed (`a.b.one` -> `a.b`).
    /// Keys that are not plural forms are returned unchanged.
    fn plural_base(key: &str) -> &str {
        match key.rsplit_once('.') {
            Some((base, last)) if PLURAL_CATEGORIES.contains(&last) => base,
            _ => key,
        }
    }

    /// Every catalog in `locales/` has the same keys as English (plural forms
    /// may differ by category) and uses the same placeholders in each message.
    #[test]
    fn catalogs_match_english_keys_and_placeholders() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let read = |name: &str| flatten_catalog(&std::fs::read_to_string(dir.join(name)).unwrap());
        let english = read("en.yml");
        let english_bases: std::collections::BTreeSet<&str> =
            english.iter().map(|(k, _)| plural_base(k)).collect();

        assert!(english.len() > 100, "English catalog failed to parse");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if !name.ends_with(".yml") || name == "en.yml" {
                continue;
            }
            let catalog = read(&name);
            let bases: std::collections::BTreeSet<&str> =
                catalog.iter().map(|(k, _)| plural_base(k)).collect();
            let missing: Vec<_> = english_bases.difference(&bases).collect();
            let extra: Vec<_> = bases.difference(&english_bases).collect();
            assert!(missing.is_empty(), "{name} lacks keys: {missing:?}");
            assert!(extra.is_empty(), "{name} has keys not in en.yml: {extra:?}");

            for (key, message) in &catalog {
                // Plural forms are compared with English's `.other` when the
                // exact category does not exist in English.
                let is_plural = english_bases.contains(plural_base(key)) && plural_base(key) != key;
                let counterpart = english
                    .iter()
                    .find(|(k, _)| k == key)
                    .or_else(|| {
                        let other = format!("{}.other", plural_base(key));
                        english.iter().find(|(k, _)| *k == other)
                    })
                    .unwrap_or_else(|| panic!("{name}: `{key}` has no English counterpart"));
                let mut expected = placeholders(&counterpart.1);
                let mut actual = placeholders(message);
                if is_plural {
                    // A singular form may spell the number out ("ein Weg").
                    expected.retain(|p| p != "count");
                    actual.retain(|p| p != "count");
                }
                assert_eq!(
                    actual, expected,
                    "{name}: placeholders of `{key}` differ from English"
                );
            }
        }
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every literal key passed to `tr!` / `tr_count!` anywhere in `src/`
    /// exists in the English catalog (plural keys via their `.other` form),
    /// so no message can render as a raw key.
    #[test]
    fn every_key_used_in_source_exists_in_english() {
        let mut files = Vec::new();
        rust_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut checked = 0;
        for file in files {
            // This file's own tests mention the macros in string literals.
            if file.ends_with("i18n.rs") {
                continue;
            }
            // Comments and doc examples are not real uses.
            let source: String = std::fs::read_to_string(&file)
                .unwrap()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for (marker, plural) in [("tr!(", false), ("tr_count!(", true)] {
                for (start, _) in source.match_indices(marker) {
                    let preceded_by_ident = source[..start]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    let rest = source[start + marker.len()..].trim_start();
                    // Only literal keys; dynamic keys can't be scanned.
                    if preceded_by_ident || !rest.starts_with('"') {
                        continue;
                    }
                    let rest = &rest[1..];
                    let Some(end) = rest.find('"') else { continue };
                    let key = &rest[..end];
                    let lookup_key = if plural {
                        format!("{key}.other")
                    } else {
                        key.to_string()
                    };
                    assert!(
                        lookup_in("en", &lookup_key).is_some(),
                        "{}: key `{key}` is not in locales/en.yml",
                        file.display()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "found no tr! calls to check");
    }
}
