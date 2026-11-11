//! Refreshes the vendored iD tagging schema data and the preset icons under
//! `assets/presets/`. Run manually with:
//!   cargo run --example update_presets [-- <schema-version>] [--data-only]
//!
//! `--data-only` skips the icon downloads and rewrites only the JSON data.
//!
//! `<schema-version>` is the `@openstreetmap/id-tagging-schema` release to
//! fetch (default: `DEFAULT_SCHEMA_VERSION`, the one the repo currently
//! bundles). Every icon set is fetched from a pinned upstream release (the
//! `*_VERSION` constants below); only icons some bundled preset references
//! (plus the per-geometry fallback icons) are written. Icons are made square
//! (see `osm_gpui::svg_normalize`) so GPUI does not squash them.
//!
//! Not part of CI or the app runtime — review the resulting git diff by hand
//! after running it, and keep `assets/presets/LICENSE` and
//! `assets/presets/icons/LICENSES.md` in step with the versions below.

use osm_gpui::http::{fetch_with_retries, HttpRequest, RetryPolicy, UreqClient};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

/// The schema release the repo currently bundles (see
/// `assets/presets/LICENSE`). Pass another version as the first argument.
const DEFAULT_SCHEMA_VERSION: &str = "6.19.2";

// Icon sources, each pinned to a tagged/published release. iD's preset
// icons and Font Awesome both come from the iD repository, which keeps the
// exact `iD-*` sprite sources and the Font Awesome Free glyphs under the
// (alias-resolved) names the schema uses, e.g. `fas-apple-alt`.
const MAKI_VERSION: &str = "8.2.0"; // npm @mapbox/maki (not git-tagged past 8.0.1)
const TEMAKI_VERSION: &str = "v5.13.0";
const ROENTGEN_VERSION: &str = "v0.16.0";
const ID_VERSION: &str = "v2.42.3";
// Font Awesome Free: fallback for names iD's copy no longer carries. iD
// v2.42.3 builds against @fortawesome/*-svg-icons ^7.2.0.
const FONT_AWESOME_VERSION: &str = "7.2.0";

fn schema_base(version: &str) -> String {
    format!(
        "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema@{}/dist",
        version
    )
}

/// Candidate upstream URLs for a preset icon name such as `temaki-lock`, in
/// order of preference; empty for a name with no known set prefix. Font
/// Awesome icons try iD's alias-resolved copy first (the schema uses names
/// like `fas-apple-alt`), then Font Awesome's own canonical file.
fn icon_urls(icon_name: &str) -> Vec<String> {
    let mut urls: Vec<String> = icon_url(icon_name).into_iter().collect();
    for (prefix, dir) in [("fas-", "solid"), ("far-", "regular"), ("fab-", "brands")] {
        if let Some(n) = icon_name.strip_prefix(prefix) {
            urls.push(format!(
                "https://raw.githubusercontent.com/FortAwesome/Font-Awesome/{}/svgs/{}/{}.svg",
                FONT_AWESOME_VERSION, dir, n
            ));
        }
    }
    urls
}

fn icon_url(icon_name: &str) -> Option<String> {
    let raw = |repo: &str, version: &str, path: &str| {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            repo, version, path
        )
    };
    if let Some(n) = icon_name.strip_prefix("maki-") {
        Some(format!(
            "https://cdn.jsdelivr.net/npm/@mapbox/maki@{}/icons/{}.svg",
            MAKI_VERSION, n
        ))
    } else if let Some(n) = icon_name.strip_prefix("temaki-") {
        Some(raw(
            "rapideditor/temaki",
            TEMAKI_VERSION,
            &format!("icons/{}.svg", n),
        ))
    } else if let Some(n) = icon_name.strip_prefix("roentgen-") {
        Some(raw(
            "enzet/Roentgen",
            ROENTGEN_VERSION,
            &format!("icons/{}.svg", n),
        ))
    } else if let Some(n) = icon_name.strip_prefix("iD-") {
        Some(raw(
            "openstreetmap/iD",
            ID_VERSION,
            &format!("svg/iD-sprite/presets/{}.svg", n),
        ))
    } else if ["fas-", "far-", "fab-"]
        .iter()
        .any(|p| icon_name.starts_with(p))
    {
        Some(raw(
            "openstreetmap/iD",
            ID_VERSION,
            &format!("svg/fontawesome/{}.svg", icon_name),
        ))
    } else {
        None
    }
}

// Keys iD never treats as area-implying, regardless of what presets.json
// says — mirrors iD's own `areaKeys()` ignore list in
// modules/presets/index.js (these are "usually a line" keys).
const AREA_IGNORE_KEYS: &[&str] = &[
    "barrier", "highway", "footway", "railway", "junction", "type",
];

const OUT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets");

fn main() {
    // `--data-only` rewrites presets.json / area_keys.json / fields.json and
    // leaves the icons alone (no icon downloads).
    let data_only = std::env::args().any(|a| a == "--data-only");
    let schema_version = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .unwrap_or_else(|| DEFAULT_SCHEMA_VERSION.to_string());
    println!("id-tagging-schema {}", schema_version);
    let base = schema_base(&schema_version);
    let client = UreqClient::new();

    let presets_body = fetch(&client, &format!("{}/presets.json", base));
    let presets_root: Value = serde_json::from_str(&presets_body).expect("parse presets.json");

    let translations_body = fetch(&client, &format!("{}/translations/en.json", base));
    let names = preset_names(&translations_body);

    let (trimmed_presets, icon_names) = trim_presets(&presets_root, &names);
    let area_keys = compute_area_keys(&presets_root);

    let fields_body = fetch(&client, &format!("{}/fields.json", base));
    let fields_root: Value = serde_json::from_str(&fields_body).expect("parse fields.json");
    let field_translations_map = field_translations(&translations_body);
    let trimmed_fields = trim_fields(&fields_root, &field_translations_map);

    // The field-id lists are kept exactly as upstream wrote them: entries
    // like `{highway/primary}` (insert that preset's list here) and a missing
    // `fields` (inherit the parent preset's) are resolved by the app when it
    // loads the index (`PresetIndex::from_json`), not here. Only `moreFields`
    // is renamed to `more_fields`.
    let trimmed_presets: Vec<Value> = trimmed_presets
        .into_iter()
        .map(|mut preset| {
            let obj = preset.as_object_mut().unwrap();
            if let Some(more) = obj.remove("moreFields") {
                obj.insert("more_fields".to_string(), more);
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

    if data_only {
        println!(
            "done (data only): {} presets, {} fields in {}",
            trimmed_presets.len(),
            trimmed_fields.len(),
            OUT_DIR
        );
        return;
    }

    let icons_dir = Path::new(OUT_DIR).join("icons");
    fs::create_dir_all(&icons_dir).expect("create icons dir");

    // The geometry fallback icons are always vendored, even if no preset
    // references them.
    let icon_names: BTreeSet<String> = icon_names
        .into_iter()
        .chain(
            osm_gpui::presets::FALLBACK_ICON_NAMES
                .iter()
                .map(|s| s.to_string()),
        )
        .collect();

    // Delete vendored icons no longer referenced by any preset, so re-runs
    // don't accumulate stale files. Non-SVG files (LICENSES.md) stay.
    if let Ok(entries) = fs::read_dir(&icons_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let Some(icon_name) = file_name.strip_suffix(".svg") else {
                continue;
            };
            if !icon_names.contains(icon_name) {
                let _ = fs::remove_file(entry.path());
                println!("removed stale icon: {}", file_name);
            }
        }
    }

    // Fetch every referenced icon (always re-fetched, so a bumped version
    // constant takes effect), a few at a time.
    let queue: Vec<&String> = icon_names.iter().collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let missing = std::sync::Mutex::new(Vec::<(String, String)>::new());
    let fetched = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(icon_name) = queue.get(i) else {
                    break;
                };
                let urls = icon_urls(icon_name);
                if urls.is_empty() {
                    missing.lock().unwrap().push((
                        icon_name.to_string(),
                        "no vendor source for this icon-set prefix".to_string(),
                    ));
                    continue;
                }
                let url = urls.join(" or ");
                match urls.iter().find_map(|u| fetch_optional(&client, u)) {
                    Some(body) => {
                        let dest = icons_dir.join(format!("{}.svg", icon_name));
                        fs::write(&dest, osm_gpui::svg_normalize::square_viewbox(&body))
                            .unwrap_or_else(|e| panic!("write {}: {}", dest.display(), e));
                        fetched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    None => {
                        // Don't keep a stale copy of something upstream no
                        // longer ships at the pinned version.
                        let _ = fs::remove_file(icons_dir.join(format!("{}.svg", icon_name)));
                        missing.lock().unwrap().push((icon_name.to_string(), url));
                    }
                }
            });
        }
    });

    let mut missing = missing.into_inner().unwrap();
    missing.sort();
    for (icon_name, url) in &missing {
        eprintln!("WARNING: could not fetch icon '{}' from {}", icon_name, url);
    }

    println!(
        "done: {} presets, {} fields, {} icons fetched ({} referenced icons not found) in {}",
        trimmed_presets.len(),
        trimmed_fields.len(),
        fetched.load(std::sync::atomic::Ordering::SeqCst),
        missing.len(),
        OUT_DIR
    );
    if !missing.is_empty() {
        println!(
            "Add the missing names to KNOWN_MISSING_ICONS in src/presets.rs (with a reason), \
             or bump the pinned versions above."
        );
    }
}

fn fetch(client: &UreqClient, url: &str) -> String {
    fetch_optional(client, url).unwrap_or_else(|| panic!("failed to fetch {}", url))
}

fn fetch_optional(client: &UreqClient, url: &str) -> Option<String> {
    let req = HttpRequest::get(url).header("User-Agent", osm_gpui::USER_AGENT);
    let resp = fetch_with_retries(client, &req, &RetryPolicy::standard()).ok()?;
    resp.into_string().ok()
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
        let icon = entry
            .get("icon")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(icon_name) = &icon {
            icon_names.insert(icon_name.clone());
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
            Value::Object(
                tags.into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect(),
            ),
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
    let obj = root
        .as_object()
        .expect("presets.json root is not an object");

    let mut area_keys: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();

    // Keeplist pass.
    for entry in obj.values() {
        if entry
            .get("suggestion")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
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
                Value::Object(v.into_keys().map(|vk| (vk, Value::Bool(true))).collect()),
            )
        })
        .collect()
}

struct FieldTranslation {
    label: Option<String>,
    placeholder: Option<String>,
    options: HashMap<String, String>, // option value -> translated label
    types: HashMap<String, String>,   // sub-key -> label (multi-key fields)
}

/// An option or type entry's English text: a plain string, or the `title` of
/// a `{ "title": ..., "description": ... }` object.
fn entry_title(v: &Value) -> Option<String> {
    v.as_str()
        .or_else(|| v.get("title").and_then(|t| t.as_str()))
        .map(str::to_string)
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
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let placeholder = entry
            .get("placeholder")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let titles = |key: &str| -> HashMap<String, String> {
            entry
                .get(key)
                .and_then(|v| v.as_object())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| entry_title(v).map(|s| (k.clone(), s)))
                        .collect()
                })
                .unwrap_or_default()
        };
        out.insert(
            id.clone(),
            FieldTranslation {
                label,
                placeholder,
                options: titles("options"),
                types: titles("types"),
            },
        );
    }
    out
}

/// Extract what the app reads of each field, from upstream's full
/// `fields.json` object-of-objects shape, merging in the English
/// label/placeholder/option-labels. Every field is kept, whatever its type:
/// what each type can do is decided by the app, not here. Strings that are
/// references to another field (`"{leaf_cycle}"`, `stringsCrossReference`) are
/// kept as written and resolved when the index loads; an option or label
/// with no English text of its own is written empty for the same reason.
fn trim_fields(root: &Value, translations: &HashMap<String, FieldTranslation>) -> Vec<Value> {
    let obj = root.as_object().expect("fields.json root is not an object");
    let mut out = Vec::new();

    for (id, entry) in obj {
        let Some(field_type) = entry.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let translation = translations.get(id);
        let text = |name: &str| entry.get(name).and_then(|v| v.as_str()).map(str::to_string);
        let label = text("label").or_else(|| translation.and_then(|t| t.label.clone()));
        let placeholder =
            text("placeholder").or_else(|| translation.and_then(|t| t.placeholder.clone()));
        let options: Vec<Value> = entry
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|value| {
                        let label = translation
                            .and_then(|t| t.options.get(value).cloned())
                            .unwrap_or_default();
                        serde_json::json!({ "value": value, "label": label })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut trimmed = serde_json::Map::new();
        trimmed.insert("id".to_string(), Value::String(id.clone()));
        if let Some(key) = entry.get("key").and_then(|v| v.as_str()) {
            trimmed.insert("key".to_string(), Value::String(key.to_string()));
        }
        if let Some(keys) = entry.get("keys").filter(|v| v.is_array()) {
            trimmed.insert("keys".to_string(), keys.clone());
        }
        trimmed.insert(
            "field_type".to_string(),
            Value::String(field_type.to_string()),
        );
        if let Some(label) = label {
            trimmed.insert("label".to_string(), Value::String(label));
        }
        if let Some(p) = placeholder {
            trimmed.insert("placeholder".to_string(), Value::String(p));
        }
        if !options.is_empty() {
            trimmed.insert("options".to_string(), Value::Array(options));
        }
        if let Some(types) = translation.map(|t| &t.types).filter(|t| !t.is_empty()) {
            trimmed.insert(
                "types".to_string(),
                Value::Object(
                    types
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect(),
                ),
            );
        }
        for (from, to) in [
            ("geometry", "geometry"),
            ("prerequisiteTag", "prerequisite_tag"),
            ("stringsCrossReference", "strings_cross_reference"),
            ("universal", "universal"),
        ] {
            if let Some(v) = entry.get(from) {
                trimmed.insert(to.to_string(), v.clone());
            }
        }
        out.push(Value::Object(trimmed));
    }

    out
}
