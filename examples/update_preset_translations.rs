//! Vendors preset translations from the iD tagging schema into
//! `assets/presets/translations/<locale>.json`. Run manually with:
//!   cargo run --example update_preset_translations -- <schema-version> <locale>...
//! for example `... -- 6.19.2 de fr`. Use the same schema version as the
//! vendored `presets.json` / `fields.json` (see `assets/presets/LICENSE`), so
//! the translated ids line up with the English ones.
//!
//! Each output file keeps only what the app reads, and only for presets and
//! fields that are actually vendored:
//!   { "presets": { "<id>": { "name": "...", "terms": ["..."] } },
//!     "fields":  { "<id>": { "label": "...", "placeholder": "...",
//!                            "options": { "<value>": "..." } } } }
//! A preset's `terms` are upstream's `terms` plus its `aliases`, both of which
//! are alternative search words.
//!
//! Not part of CI or the app runtime. Dropping the resulting file into
//! `assets/presets/translations/` is all it takes to ship a new locale (the
//! build script picks up every `*.json` there).

use osm_gpui::http::{fetch_with_retries, HttpRequest, RetryPolicy, UreqClient};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

const PRESETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets");

fn main() {
    let mut args = std::env::args().skip(1);
    let version = args
        .next()
        .expect("usage: update_preset_translations <schema-version> <locale>...");
    let locales: Vec<String> = args.collect();
    assert!(!locales.is_empty(), "name at least one locale, e.g. `de`");

    let preset_ids = vendored_ids("presets.json");
    let field_options = vendored_field_options();
    let client = UreqClient::new();

    let out_dir = Path::new(PRESETS_DIR).join("translations");
    fs::create_dir_all(&out_dir).expect("create translations dir");

    for locale in &locales {
        let url = format!(
            "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema@{version}/dist/translations/{locale}.min.json"
        );
        let req = HttpRequest::get(&url).header("User-Agent", osm_gpui::USER_AGENT);
        let body = fetch_with_retries(&client, &req, &RetryPolicy::standard())
            .unwrap_or_else(|e| panic!("fetch {url}: {e:?}"))
            .into_string()
            .expect("response body");
        let root: Value = serde_json::from_str(&body).expect("parse translations");
        let Some(section) = root.get(locale).and_then(|v| v.get("presets")) else {
            panic!("{url}: no `{locale}.presets` section");
        };

        let presets = trim_presets(section.get("presets"), &preset_ids);
        let fields = trim_fields(section.get("fields"), &field_options);
        println!(
            "{locale}: {} presets, {} fields",
            presets.len(),
            fields.len()
        );
        let mut out = Map::new();
        out.insert("presets".into(), Value::Object(presets));
        out.insert("fields".into(), Value::Object(fields));
        fs::write(
            out_dir.join(format!("{locale}.json")),
            serde_json::to_string(&Value::Object(out)).unwrap() + "\n",
        )
        .expect("write translation file");
    }
}

fn vendored(file: &str) -> Vec<Value> {
    let body = fs::read_to_string(Path::new(PRESETS_DIR).join(file)).expect("read vendored file");
    serde_json::from_str(&body).expect("parse vendored file")
}

fn vendored_ids(file: &str) -> HashSet<String> {
    vendored(file)
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect()
}

/// Vendored field id -> the option values the app can show for it.
fn vendored_field_options() -> BTreeMap<String, HashSet<String>> {
    vendored("fields.json")
        .iter()
        .filter_map(|f| {
            let id = f.get("id")?.as_str()?.to_string();
            let values = f
                .get("options")
                .and_then(|o| o.as_array())
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| o.get("value")?.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some((id, values))
        })
        .collect()
}

fn trim_presets(section: Option<&Value>, ids: &HashSet<String>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(entries) = section.and_then(|v| v.as_object()) else {
        return out;
    };
    for (id, entry) in entries {
        if !ids.contains(id) {
            continue;
        }
        let mut trimmed = Map::new();
        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
            trimmed.insert("name".into(), Value::String(name.to_string()));
        }
        // Older releases store `terms` as one comma-separated string and
        // `aliases` as one newline-separated string; newer ones use arrays.
        let mut terms: Vec<Value> = Vec::new();
        for (key, separator) in [("terms", ','), ("aliases", '\n')] {
            let words: Vec<String> = match entry.get(key) {
                Some(Value::String(s)) => {
                    s.split(separator).map(|w| w.trim().to_string()).collect()
                }
                Some(Value::Array(list)) => list
                    .iter()
                    .filter_map(|w| w.as_str().map(|w| w.trim().to_string()))
                    .collect(),
                _ => Vec::new(),
            };
            terms.extend(
                words
                    .into_iter()
                    .filter(|w| !w.is_empty())
                    .map(Value::String),
            );
        }
        if !terms.is_empty() {
            trimmed.insert("terms".into(), Value::Array(terms));
        }
        if !trimmed.is_empty() {
            out.insert(id.clone(), Value::Object(trimmed));
        }
    }
    out
}

fn trim_fields(
    section: Option<&Value>,
    vendored: &BTreeMap<String, HashSet<String>>,
) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(entries) = section.and_then(|v| v.as_object()) else {
        return out;
    };
    for (id, entry) in entries {
        let Some(values) = vendored.get(id) else {
            continue;
        };
        let mut trimmed = Map::new();
        for key in ["label", "placeholder"] {
            if let Some(text) = entry.get(key).and_then(|v| v.as_str()) {
                trimmed.insert(key.into(), Value::String(text.to_string()));
            }
        }
        // Options are either a plain string or `{ "title": ..., "description": ... }`.
        let mut options = Map::new();
        if let Some(opts) = entry.get("options").and_then(|v| v.as_object()) {
            for (value, opt) in opts {
                if !values.contains(value) {
                    continue;
                }
                let title = opt
                    .as_str()
                    .or_else(|| opt.get("title").and_then(|t| t.as_str()));
                if let Some(title) = title {
                    options.insert(value.clone(), Value::String(title.to_string()));
                }
            }
        }
        if !options.is_empty() {
            trimmed.insert("options".into(), Value::Object(options));
        }
        if !trimmed.is_empty() {
            out.insert(id.clone(), Value::Object(trimmed));
        }
    }
    out
}
