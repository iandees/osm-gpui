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
const FIELDS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/fields.json";
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

    let fields_body = fetch(&client, FIELDS_URL);
    let fields_root: Value = serde_json::from_str(&fields_body).expect("parse fields.json");
    let field_translations_map = field_translations(&translations_body);
    let (trimmed_fields, kept_field_ids) = trim_fields(&fields_root, &field_translations_map);

    // Filter each trimmed preset's fields/more_fields to only field ids that
    // survived trimming, so the Rust side never sees a dangling reference.
    let trimmed_presets: Vec<Value> = trimmed_presets
        .into_iter()
        .map(|mut preset| {
            for key in ["fields", "moreFields"] {
                if let Some(Value::Array(ids)) = preset.get(key).cloned() {
                    let filtered: Vec<Value> = ids
                        .into_iter()
                        .filter(|id| {
                            id.as_str()
                                .map(|s| kept_field_ids.contains(s))
                                .unwrap_or(false)
                        })
                        .collect();
                    let out_key = if key == "moreFields" { "more_fields" } else { key };
                    preset
                        .as_object_mut()
                        .unwrap()
                        .insert(out_key.to_string(), Value::Array(filtered));
                }
            }
            preset
        })
        .collect();

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
    fs::write(
        Path::new(OUT_DIR).join("fields.json"),
        serde_json::to_string_pretty(&trimmed_fields).unwrap(),
    )
    .expect("write fields.json");

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
        "done: {} presets, {} fields, {} icons in {}",
        trimmed_presets.len(),
        trimmed_fields.len(),
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
        // Carry the raw (unfiltered) field-id lists through under their
        // upstream key names; `main` filters them down to surviving field
        // ids (via `trim_fields`) and renames `moreFields` to `more_fields`
        // when it writes the final presets.json.
        if let Some(f) = entry.get("fields") {
            trimmed.insert("fields".to_string(), f.clone());
        }
        if let Some(f) = entry.get("moreFields") {
            trimmed.insert("moreFields".to_string(), f.clone());
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

/// Map upstream's field `type` string to our vendored `FieldType`, or
/// `None` if it's not one of the types this tool vendors (see Global
/// Constraints — no multi-key fields, no dynamic-suggestion types).
fn map_field_type(upstream_type: &str) -> Option<&'static str> {
    match upstream_type {
        "text" | "number" | "url" | "tel" | "email" => Some("text"),
        "combo" | "typeCombo" | "semiCombo" => Some("combo"),
        "check" | "onewayCheck" | "defaultCheck" => Some("check"),
        "radio" => Some("radio"),
        "multiCombo" => Some("multiCombo"),
        _ => None,
    }
}

struct FieldTranslation {
    label: Option<String>,
    placeholder: Option<String>,
    options: HashMap<String, String>, // option value -> translated label
}

/// Parse `dist/translations/en.json`'s field section into a flat
/// `field id -> FieldTranslation` map. Upstream nests this at
/// `en.presets.fields.<id>.{label,placeholder,options}`.
fn field_translations(body: &str) -> HashMap<String, FieldTranslation> {
    let root: Value = serde_json::from_str(body).expect("parse translations/en.json");
    let mut out = HashMap::new();
    let Some(fields) = root
        .get("en")
        .and_then(|v| v.get("presets"))
        .and_then(|v| v.get("fields"))
        .and_then(|v| v.as_object())
    else {
        return out;
    };
    for (id, entry) in fields {
        let label = entry.get("label").and_then(|v| v.as_str()).map(str::to_string);
        let placeholder = entry
            .get("placeholder")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let options = entry
            .get("options")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.insert(id.clone(), FieldTranslation { label, placeholder, options });
    }
    out
}

/// Extract only the fields our `Field` type keeps, from upstream's full
/// `fields.json` object-of-objects shape, merging in translated
/// label/placeholder/option-labels. Drops any field with an unsupported
/// `type`, a multi-key (`keys` instead of `key`) definition, or — for
/// combo/radio/multiCombo — no static `options` list.
fn trim_fields(
    root: &Value,
    translations: &HashMap<String, FieldTranslation>,
) -> (Vec<Value>, HashSet<String>) {
    let obj = root.as_object().expect("fields.json root is not an object");
    let mut out = Vec::new();
    let mut kept_ids = HashSet::new();

    for (id, entry) in obj {
        let Some(key) = entry.get("key").and_then(|v| v.as_str()) else {
            continue; // multi-key ("keys") or keyless field — not vendored
        };
        let Some(upstream_type) = entry.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(field_type) = map_field_type(upstream_type) else {
            continue;
        };

        let translation = translations.get(id);
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| translation.and_then(|t| t.label.clone()));
        let Some(label) = label else {
            continue;
        };
        let placeholder = entry
            .get("placeholder")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| translation.and_then(|t| t.placeholder.clone()));

        let needs_options = matches!(field_type, "combo" | "radio" | "multiCombo");
        let options: Vec<Value> = entry
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|value| {
                        let label = translation
                            .and_then(|t| t.options.get(value).cloned())
                            .unwrap_or_else(|| value.to_string());
                        let mut opt = serde_json::Map::new();
                        opt.insert("value".to_string(), Value::String(value.to_string()));
                        opt.insert("label".to_string(), Value::String(label));
                        Value::Object(opt)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if needs_options && options.is_empty() {
            continue; // no static options to offer — not vendored
        }

        let mut trimmed = serde_json::Map::new();
        trimmed.insert("id".to_string(), Value::String(id.clone()));
        trimmed.insert("key".to_string(), Value::String(key.to_string()));
        trimmed.insert("field_type".to_string(), Value::String(field_type.to_string()));
        trimmed.insert("label".to_string(), Value::String(label));
        if let Some(p) = placeholder {
            trimmed.insert("placeholder".to_string(), Value::String(p));
        }
        if !options.is_empty() {
            trimmed.insert("options".to_string(), Value::Array(options));
        }
        out.push(Value::Object(trimmed));
        kept_ids.insert(id.clone());
    }

    (out, kept_ids)
}
