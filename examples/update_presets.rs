//! Refreshes the vendored iD tagging schema data under `assets/presets/`.
//! Run manually with:
//!   cargo run --example update_presets
//!
//! Not part of CI or the app runtime — review the resulting git diff by hand
//! after running it.

use osm_gpui::http::{fetch_with_retries, HttpRequest, RetryPolicy, UreqClient};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

const PRESETS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/presets.json";
const TRANSLATIONS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/translations/en.json";
const MAKI_BASE: &str = "https://cdn.jsdelivr.net/npm/@mapbox/maki/icons";
const TEMAKI_BASE: &str = "https://cdn.jsdelivr.net/npm/@rapideditor/temaki/icons";

// Keys iD never treats as area-implying, regardless of what presets.json
// says — mirrors iD's own `areaKeys()` ignore list in
// modules/presets/index.js (these are "usually a line" keys).
const AREA_IGNORE_KEYS: &[&str] = &["barrier", "highway", "footway", "railway", "junction", "type"];

const OUT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets");

fn main() {
    let client = UreqClient::new();

    let presets_body = fetch(&client, PRESETS_URL);
    let presets_root: Value = serde_json::from_str(&presets_body).expect("parse presets.json");

    let translations_body = fetch(&client, TRANSLATIONS_URL);
    let names = preset_names(&translations_body);

    let (trimmed_presets, icon_names) = trim_presets(&presets_root, &names);
    let area_keys = compute_area_keys(&presets_root);

    fs::create_dir_all(OUT_DIR).expect("create assets/presets");
    fs::write(
        Path::new(OUT_DIR).join("presets.json"),
        serde_json::to_string_pretty(&trimmed_presets).unwrap(),
    )
    .expect("write presets.json");
    fs::write(
        Path::new(OUT_DIR).join("area_keys.json"),
        serde_json::to_string_pretty(&area_keys).unwrap(),
    )
    .expect("write area_keys.json");

    let icons_dir = Path::new(OUT_DIR).join("icons");
    fs::create_dir_all(&icons_dir).expect("create icons dir");

    // Delete vendored icons no longer referenced by any preset, so re-runs
    // don't accumulate stale files.
    if let Ok(entries) = fs::read_dir(&icons_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let icon_name = file_name.trim_end_matches(".svg");
            if !icon_names.contains(icon_name) {
                let _ = fs::remove_file(entry.path());
                println!("removed stale icon: {}", file_name);
            }
        }
    }

    for icon_name in &icon_names {
        let dest = icons_dir.join(format!("{}.svg", icon_name));
        if dest.exists() {
            continue;
        }
        if !icon_name.starts_with("maki-") && !icon_name.starts_with("temaki-") {
            // iD's own built-in icon names aren't fetchable from Maki/Temaki;
            // skip immediately rather than wasting retries on a bogus URL.
            eprintln!(
                "WARNING: no vendor source for icon '{}' (not maki-/temaki-), skipping",
                icon_name
            );
            continue;
        }
        let url = icon_url(icon_name);
        match fetch_optional(&client, &url) {
            Some(body) => {
                fs::write(&dest, body).unwrap_or_else(|e| {
                    panic!("write {}: {}", dest.display(), e);
                });
                println!("fetched icon: {}", icon_name);
            }
            None => {
                eprintln!("WARNING: could not fetch icon '{}' from {}", icon_name, url);
            }
        }
    }

    println!(
        "done: {} presets, {} icons in {}",
        trimmed_presets.len(),
        icon_names.len(),
        OUT_DIR
    );
}

fn fetch(client: &UreqClient, url: &str) -> String {
    fetch_optional(client, url).unwrap_or_else(|| panic!("failed to fetch {}", url))
}

fn fetch_optional(client: &UreqClient, url: &str) -> Option<String> {
    let req = HttpRequest::get(url).header("User-Agent", osm_gpui::USER_AGENT);
    let resp = fetch_with_retries(client, &req, &RetryPolicy::standard()).ok()?;
    resp.into_string().ok()
}

/// Given the icon name from a preset (e.g. "maki-cafe" or "temaki-lock"),
/// build the jsDelivr URL for its SVG source. Only called for names already
/// confirmed to start with "maki-" or "temaki-" (see the icon-fetch loop in
/// `main`, which skips anything else before calling this).
fn icon_url(icon_name: &str) -> String {
    if let Some(name) = icon_name.strip_prefix("maki-") {
        format!("{}/{}.svg", MAKI_BASE, name)
    } else if let Some(name) = icon_name.strip_prefix("temaki-") {
        format!("{}/icon-{}.svg", TEMAKI_BASE, name)
    } else {
        unreachable!("icon_url called with an unsupported icon name: {}", icon_name)
    }
}

/// Parse `dist/translations/en.json` into a flat `preset id -> display name`
/// map. Upstream nests this at `en.presets.presets.<id>.name` — most presets
/// carry their display name here rather than inline in `presets.json`.
fn preset_names(body: &str) -> HashMap<String, String> {
    let root: Value = serde_json::from_str(body).expect("parse translations/en.json");
    let mut names = HashMap::new();
    if let Some(presets) = root
        .get("en")
        .and_then(|v| v.get("presets"))
        .and_then(|v| v.get("presets"))
        .and_then(|v| v.as_object())
    {
        for (id, entry) in presets {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                names.insert(id.clone(), name.to_string());
            }
        }
    }
    names
}

/// Extract only the fields our `Preset` type keeps, from upstream's full
/// `presets.json` object-of-objects shape, and collect every referenced
/// icon name along the way. `names` is the translations lookup from
/// `preset_names`, used when a preset has no inline `name` field (the
/// common case — only a minority of upstream presets embed their name
/// directly).
fn trim_presets(root: &Value, names: &HashMap<String, String>) -> (Vec<Value>, HashSet<String>) {
    let Some(obj) = root.as_object() else {
        panic!("presets.json root is not an object");
    };

    let mut out = Vec::new();
    let mut icon_names = HashSet::new();

    for (id, entry) in obj {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| names.get(id).cloned());
        let Some(name) = name else {
            continue;
        };
        let Some(geometry) = entry.get("geometry").and_then(|v| v.as_array()) else {
            continue;
        };
        let tags: BTreeMap<String, String> = entry
            .get("tags")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let icon = entry.get("icon").and_then(|v| v.as_str()).map(str::to_string);
        if let Some(icon_name) = &icon {
            if icon_name.starts_with("maki-") || icon_name.starts_with("temaki-") {
                icon_names.insert(icon_name.clone());
            }
        }
        let match_score = entry.get("matchScore").and_then(|v| v.as_f64());

        let mut trimmed = serde_json::Map::new();
        trimmed.insert("id".to_string(), Value::String(id.clone()));
        trimmed.insert("name".to_string(), Value::String(name));
        if let Some(icon_name) = icon {
            trimmed.insert("icon".to_string(), Value::String(icon_name));
        }
        trimmed.insert(
            "tags".to_string(),
            Value::Object(tags.into_iter().map(|(k, v)| (k, Value::String(v))).collect()),
        );
        trimmed.insert("geometry".to_string(), Value::Array(geometry.clone()));
        if let Some(score) = match_score {
            trimmed.insert(
                "match_score".to_string(),
                Value::Number(serde_json::Number::from_f64(score).unwrap()),
            );
        }
        out.push(Value::Object(trimmed));
    }

    (out, icon_names)
}

/// Derive `area_keys.json`'s content directly from the full upstream
/// `presets.json`, replicating iD's own `areaKeys()` function
/// (`modules/presets/index.js`): a key becomes an area key if some
/// non-suggestion, non-replacement preset's *first* tag key is that key and
/// that preset's `geometry` includes `"area"` (and the key isn't in
/// `AREA_IGNORE_KEYS`). A specific value is then excluded from that key's
/// area-implying set if some preset also supporting `"line"` geometry sets
/// that exact key/value via `addTags`. Returns a JSON object shaped exactly
/// like `AreaKeys::from_json` expects: key -> { excluded_value: true, ... }.
fn compute_area_keys(root: &Value) -> serde_json::Map<String, Value> {
    let obj = root.as_object().expect("presets.json root is not an object");

    let mut area_keys: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();

    // Keeplist pass.
    for entry in obj.values() {
        if entry.get("suggestion").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        if entry.get("replacement").is_some() {
            continue;
        }
        let Some(tags) = entry.get("tags").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(first_key) = tags.keys().next() else {
            continue;
        };
        if AREA_IGNORE_KEYS.contains(&first_key.as_str()) {
            continue;
        }
        let Some(geometry) = entry.get("geometry").and_then(|v| v.as_array()) else {
            continue;
        };
        let supports_area = geometry.iter().any(|g| g.as_str() == Some("area"));
        if supports_area {
            area_keys.entry(first_key.clone()).or_default();
        }
    }

    // Discardlist pass: exclude specific values that a line-capable preset
    // also tags via addTags for an already-known area key.
    for entry in obj.values() {
        let Some(add_tags) = entry.get("addTags").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(geometry) = entry.get("geometry").and_then(|v| v.as_array()) else {
            continue;
        };
        let supports_line = geometry.iter().any(|g| g.as_str() == Some("line"));
        if !supports_line {
            continue;
        }
        for (key, value) in add_tags {
            let Some(value_str) = value.as_str() else {
                continue;
            };
            if value_str == "*" {
                continue;
            }
            if let Some(excluded) = area_keys.get_mut(key) {
                excluded.insert(value_str.to_string(), true);
            }
        }
    }

    area_keys
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                Value::Object(v.into_iter().map(|(vk, _)| (vk, Value::Bool(true))).collect()),
            )
        })
        .collect()
}
