# iD-style Field-Based Tag Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "Fields" section to the side panel that renders typed, human-friendly widgets (text/checkbox/radio/combo/multi-combo) for the selected feature's matched preset, plus a minimal preset picker to deliberately change a feature's type — building on the existing preset-matching system (`src/presets.rs`, `feature_geometry`, `describe_selected_feature`).

**Architecture:** A new `src/fields.rs` module (mirroring `src/presets.rs`'s shape) owns `Field`/`FieldType`/`FieldIndex` types, JSON parsing, and field-list resolution — vendored the same way presets are, via an extended `examples/update_presets.rs`. A new `src/ui/fields_section.rs` (as `impl MapViewer` methods, matching how `src/side_panel.rs` already organizes its sections) renders the widgets, reusing gpui-component's existing `Checkbox`/`Radio`/`InputState` builders and a hand-rolled expand/collapse list (mirroring `src/ui/nsi_dialog.rs`'s pattern) for combo/multiCombo, since gpui-component's `Combobox` is generic delegate machinery not worth adopting for our small, static, vendored option lists. A new `src/ui/preset_picker_dialog.rs` mirrors `nsi_dialog.rs` exactly, searching the vendored `PresetIndex` instead of NSI. All tag mutations reuse the existing `apply_nsi_preset(&mut self, target: &FeatureRef, preset_tags: HashMap<String, String>)` (already fully generic despite its name) — no new mutation code.

**Tech Stack:** Rust, serde/serde_json, GPUI + gpui-component (`Checkbox`, `Radio`/`RadioGroup`, `input::{Input, InputState}`), `ureq`/`src/http.rs` (dev-only vendor-update tool, unchanged transport).

## Global Constraints

- **No multi-key fields.** Only fields with a single upstream `key` (not `keys`) are vendored; multi-key fields are dropped during trimming and never appear in a preset's `fields`/`more_fields`.
- **No dynamic/taginfo-driven combo suggestions.** Only fields with a static vendored `options` list get a combo/radio/multiCombo widget. A combo/radio/multiCombo field with no vendored `options`, or any upstream field type not in `{text, combo, check, radio, multiCombo}`, is dropped during trimming.
- **No multi-select field editing.** The Fields section only renders when exactly one feature is selected; multi-select continues to show the existing raw Tags table only.
- **No non-English localization.** Field labels/placeholders/option labels merge from `translations/en.json` only (same source/merge pattern already used for preset names).
- **No field-level validation** beyond what the widget itself constrains.
- **No changes to the Tags table's existing behavior.**
- `examples/update_presets.rs` remains a manual, non-CI, non-unit-tested dev tool — correctness verified by running it and reviewing the diff.
- Vendored data lives under `assets/presets/` (checked into git, embedded via `include_str!`, no runtime fetch) — same model as the presets/area-keys/icons data already there.
- All tag mutations for Fields-section widgets and the preset picker reuse `MapViewer::apply_nsi_preset(&mut self, target: &osm_gpui::selection::FeatureRef, preset_tags: std::collections::HashMap<String, String>)` at `src/main.rs:934` — it already diffs against current tags, applies only changed keys, and pushes one `UndoableAction::SetTags`. Do not write new tag-mutation logic.

---

### Task 1: Field/FieldType/FieldOption types + JSON parsing

**Files:**
- Create: `src/fields.rs`
- Modify: `src/lib.rs` (add `pub mod fields;` alongside the other `pub mod` lines, alphabetically after `pub mod custom_imagery_store;` and before `pub mod http;`)

**Interfaces:**
- Produces:
  - `pub enum FieldType { Text, Combo, Check, Radio, MultiCombo }`
  - `pub struct FieldOption { pub value: String, pub label: String }`
  - `pub struct Field { pub id: String, pub key: String, pub field_type: FieldType, pub label: String, pub placeholder: Option<String>, pub options: Vec<FieldOption> }`
  - `pub struct FieldIndex` with `pub fn from_json(body: &str) -> Result<Self, serde_json::Error>`, `pub fn get(&self, id: &str) -> Option<&Field>`, `pub fn len(&self) -> usize`

- [ ] **Step 1: Write the failing test**

Create `src/fields.rs`:

```rust
//! iD tagging schema field support: vendored field definitions (text/combo/
//! check/radio/multiCombo widgets) for the presets in `crate::presets`.
//! Read-only, vendored the same way presets are — see
//! `docs/superpowers/specs/2026-07-07-preset-fields-editor-design.md`.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    Text,
    Combo,
    Check,
    Radio,
    MultiCombo,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Field {
    pub id: String,
    pub key: String,
    pub field_type: FieldType,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<FieldOption>,
}

/// A parsed, in-memory collection of vendored `Field`s, keyed by id.
pub struct FieldIndex {
    fields: HashMap<String, Field>,
}

impl FieldIndex {
    /// Parse a JSON array of `Field` (the vendored `assets/presets/fields.json`
    /// shape, or a small fixture array in tests).
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        let list: Vec<Field> = serde_json::from_str(body)?;
        let fields = list.into_iter().map(|f| (f.id.clone(), f)).collect();
        Ok(Self { fields })
    }

    pub fn get(&self, id: &str) -> Option<&Field> {
        self.fields.get(id)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
    [
      {
        "id": "name",
        "key": "name",
        "field_type": "text",
        "label": "Name",
        "placeholder": "Common name"
      },
      {
        "id": "cuisine",
        "key": "cuisine",
        "field_type": "combo",
        "label": "Cuisine",
        "options": [
          {"value": "coffee_shop", "label": "Coffee Shop"},
          {"value": "pizza", "label": "Pizza"}
        ]
      }
    ]
    "#;

    #[test]
    fn from_json_parses_all_entries() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn get_finds_field_by_id() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let name = index.get("name").unwrap();
        assert_eq!(name.key, "name");
        assert_eq!(name.field_type, FieldType::Text);
        assert_eq!(name.placeholder.as_deref(), Some("Common name"));
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        assert!(index.get("not-a-real-field").is_none());
    }

    #[test]
    fn combo_field_parses_options() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let cuisine = index.get("cuisine").unwrap();
        assert_eq!(cuisine.field_type, FieldType::Combo);
        assert_eq!(cuisine.options.len(), 2);
        assert_eq!(cuisine.options[0].value, "coffee_shop");
        assert_eq!(cuisine.options[0].label, "Coffee Shop");
    }

    #[test]
    fn from_json_rejects_malformed_body() {
        assert!(FieldIndex::from_json("not json").is_err());
    }
}
```

Add `pub mod fields;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib fields::tests -- --nocapture`
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/fields.rs src/lib.rs
git commit -m "Add Field/FieldType types and JSON parsing"
```

---

### Task 2: Field-list resolution (default fields + more-fields)

**Files:**
- Modify: `src/fields.rs`

**Interfaces:**
- Consumes: `FieldIndex` (Task 1)
- Produces: `pub fn resolve_fields<'a>(index: &'a FieldIndex, field_ids: &[String]) -> Vec<&'a Field>`, `pub fn resolve_more_fields<'a>(index: &'a FieldIndex, more_field_ids: &[String], already_shown: &[String]) -> Vec<&'a Field>`

- [ ] **Step 1: Write the failing test**

Add to `src/fields.rs`, after the `FieldIndex` impl block:

```rust
/// Resolve a preset's default field-id list to `Field`s, in order, skipping
/// any id not present in `index` (shouldn't happen given vendor-time
/// filtering — see `examples/update_presets.rs` — but this stays
/// defensive rather than panicking).
pub fn resolve_fields<'a>(index: &'a FieldIndex, field_ids: &[String]) -> Vec<&'a Field> {
    field_ids.iter().filter_map(|id| index.get(id)).collect()
}

/// Resolve a preset's `more_fields` list to `Field`s, excluding any id
/// already present in `already_shown` (the preset's default `fields`,
/// already-rendered), so "Add field" never offers a duplicate.
pub fn resolve_more_fields<'a>(
    index: &'a FieldIndex,
    more_field_ids: &[String],
    already_shown: &[String],
) -> Vec<&'a Field> {
    more_field_ids
        .iter()
        .filter(|id| !already_shown.contains(id))
        .filter_map(|id| index.get(id))
        .collect()
}
```

Add tests inside `mod tests`:

```rust
    #[test]
    fn resolve_fields_returns_in_order() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let ids = vec!["cuisine".to_string(), "name".to_string()];
        let resolved = resolve_fields(&index, &ids);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].id, "cuisine");
        assert_eq!(resolved[1].id, "name");
    }

    #[test]
    fn resolve_fields_skips_missing_ids() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let ids = vec!["name".to_string(), "not-a-real-field".to_string()];
        let resolved = resolve_fields(&index, &ids);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "name");
    }

    #[test]
    fn resolve_more_fields_excludes_already_shown() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let more_ids = vec!["name".to_string(), "cuisine".to_string()];
        let already_shown = vec!["name".to_string()];
        let resolved = resolve_more_fields(&index, &more_ids, &already_shown);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "cuisine");
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib fields::tests -- --nocapture`
Expected: all previous tests plus 3 new resolution tests pass (8 total).

- [ ] **Step 3: Commit**

```bash
git add src/fields.rs
git commit -m "Add field-list resolution for default and more-fields"
```

---

### Task 3: Preset gains `fields`/`more_fields`

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Preset.fields: Vec<String>`, `Preset.more_fields: Vec<String>` (both `#[serde(default)]`).

- [ ] **Step 1: Write the failing test**

In `src/presets.rs`, find the `Preset` struct (currently: `id`, `name`, `icon`, `tags`, `geometry`, `match_score`) and add two fields:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    pub geometry: Vec<Geometry>,
    #[serde(default = "default_match_score")]
    pub match_score: f32,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub more_fields: Vec<String>,
}
```

Update the existing `MATCH_FIXTURE`/`FIXTURE` JSON constants in `src/presets.rs`'s test module: no change needed, since `#[serde(default)]` means presets without `fields`/`more_fields` in their JSON simply get empty `Vec`s — but add a new fixture entry with them populated, and a test asserting it parses:

```rust
    const FIELDS_FIXTURE: &str = r#"
    [
      {
        "id": "amenity/cafe",
        "name": "Cafe",
        "tags": {"amenity": "cafe"},
        "geometry": ["point"],
        "fields": ["name", "cuisine"],
        "more_fields": ["internet_access"]
      }
    ]
    "#;

    #[test]
    fn preset_parses_fields_and_more_fields() {
        let index = PresetIndex::from_json(FIELDS_FIXTURE).unwrap();
        let cafe = index.match_feature(
            &HashMap::from([("amenity".to_string(), "cafe".to_string())]),
            Geometry::Point,
        ).unwrap();
        assert_eq!(cafe.fields, vec!["name".to_string(), "cuisine".to_string()]);
        assert_eq!(cafe.more_fields, vec!["internet_access".to_string()]);
    }

    #[test]
    fn preset_without_fields_defaults_to_empty() {
        let index = PresetIndex::from_json(FIXTURE).unwrap();
        let point = index
            .match_feature(&HashMap::new(), Geometry::Point)
            .unwrap();
        assert!(point.fields.is_empty());
        assert!(point.more_fields.is_empty());
    }
```

(`FIXTURE` here refers to whichever existing fixture constant in `src/presets.rs`'s test module already contains a geometry-only `"point"` fallback preset with empty `tags` — check the file for its exact name before writing this test, since earlier tasks may have named it `FIXTURE` or `MATCH_FIXTURE`; use the real constant name.)

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus the 2 new tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Add fields/more_fields to Preset"
```

---

### Task 4: Vendor fields.json in the update tool

**Files:**
- Modify: `examples/update_presets.rs`

**Interfaces:**
- Consumes: nothing new externally — extends the existing `main()`/`trim_presets()` in place.
- Produces: on-disk `assets/presets/fields.json` (consumed by Task 5's run and Task 6's loader); `Preset.fields`/`more_fields` in the trimmed `presets.json` output, filtered to only field IDs that survive field-trimming.

This is a manual dev tool (see Global Constraints) — not unit tested. Read the current `examples/update_presets.rs` first (it already has `PRESETS_URL`, `TRANSLATIONS_URL`, `preset_names()`, `trim_presets()`, `compute_area_keys()`, and the icon-fetch loop from prior work) before making these additions.

- [ ] **Step 1: Add the fields URL, field-name/label translation lookup, and trim function**

Add a new constant next to the existing URL constants:

```rust
const FIELDS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/fields.json";
```

Add a function that maps upstream field `type` strings to our `FieldType`, dropping anything unsupported:

```rust
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
```

Add a function that parses `dist/translations/en.json`'s field-label/placeholder/option-label data, mirroring the existing `preset_names` function's shape but nested at `en.presets.fields.<id>.*`:

```rust
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
```

Add the field-trimming function, producing both the vendored `fields.json` content and the set of field IDs that survived (needed to filter `Preset.fields`/`more_fields`):

```rust
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
```

- [ ] **Step 2: Wire it into `main()` and filter `Preset.fields`/`more_fields`**

In `main()`, after `let (trimmed_presets, icon_names) = trim_presets(&presets_root, &names);`, add:

```rust
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
```

Note: this assumes `trim_presets` already copies upstream's `fields`/`moreFields` arrays (as raw ID strings) into the trimmed preset object under those same key names — if it doesn't yet, add that copy to `trim_presets` itself (in its per-entry loop, alongside the existing `id`/`name`/`icon`/`tags`/`geometry`/`match_score` fields): `if let Some(f) = entry.get("fields") { trimmed.insert("fields".to_string(), f.clone()); }` and the same for `entry.get("moreFields")` inserted as `"more_fields"`.

Then write `fields.json` alongside the existing writes:

```rust
    fs::write(
        Path::new(OUT_DIR).join("fields.json"),
        serde_json::to_string_pretty(&trimmed_fields).unwrap(),
    )
    .expect("write fields.json");
```

Update the final summary `println!` to also report the field count:

```rust
    println!(
        "done: {} presets, {} fields, {} icons in {}",
        trimmed_presets.len(),
        trimmed_fields.len(),
        icon_names.len(),
        OUT_DIR
    );
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --example update_presets`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add examples/update_presets.rs
git commit -m "Vendor fields.json alongside presets.json and area_keys.json"
```

---

### Task 5: Run the vendor tool, commit the fields.json snapshot

**Files:**
- Modify: `assets/presets/presets.json` (now includes `fields`/`more_fields` per preset), `assets/presets/LICENSE` (add id-tagging-schema fields.json to the attribution list — it's already covered by the existing id-tagging-schema/ISC line, so likely no change needed; check first)
- Create: `assets/presets/fields.json`

- [ ] **Step 1: Run the tool**

Run: `cargo run --example update_presets`
Expected output ends with `done: <N> presets, <M> fields, <K> icons in <path>` — N should still be ~1731 (unchanged from before), M (field count) should be at least in the dozens to low hundreds, K unchanged (~543 referenced / 149 present, same as before). If the network is unavailable, stop and report BLOCKED — do not fabricate vendor content by hand.

- [ ] **Step 2: Spot-check the output**

Run: `python3 -c "
import json
fields = json.load(open('assets/presets/fields.json'))
print(len(fields))
name_field = [f for f in fields if f['id'] == 'name']
print(name_field)
presets = json.load(open('assets/presets/presets.json'))
cafe = [p for p in presets if p['id'] == 'amenity/cafe'][0]
print(cafe.get('fields'), cafe.get('more_fields'))
"`
Expected: a plausible field count, a "name" field present with `field_type: "text"`, and the Cafe preset's `fields`/`more_fields` populated with real field IDs (not empty, given Cafe is a well-fleshed-out real-world preset upstream).

- [ ] **Step 3: Check LICENSE still covers fields.json**

Read `assets/presets/LICENSE` — its existing line "`assets/presets/presets.json` and `assets/presets/area_keys.json` are derived from the iD tagging schema... ISC License" should be updated to also name `fields.json`:

```
- assets/presets/presets.json, assets/presets/area_keys.json, and
  assets/presets/fields.json are derived from the iD tagging schema
  (https://github.com/openstreetmap/id-tagging-schema), licensed under
  the ISC License.
```

- [ ] **Step 4: Commit**

```bash
git add assets/presets/
git commit -m "Vendor iD tagging schema field definitions"
```

---

### Task 6: Global field_index() loader

**Files:**
- Modify: `src/fields.rs`

**Interfaces:**
- Consumes: `FieldIndex::from_json` (Task 1), the vendored `assets/presets/fields.json` (Task 5).
- Produces: `pub fn field_index() -> &'static FieldIndex`

- [ ] **Step 1: Write the failing test**

Add near the top of `src/fields.rs`, after the `use` lines:

```rust
use std::sync::OnceLock;

const FIELDS_JSON: &str = include_str!("../assets/presets/fields.json");

static FIELD_INDEX: OnceLock<FieldIndex> = OnceLock::new();

/// The vendored field index, parsed once on first access.
pub fn field_index() -> &'static FieldIndex {
    FIELD_INDEX.get_or_init(|| {
        FieldIndex::from_json(FIELDS_JSON).expect("vendored assets/presets/fields.json must parse")
    })
}
```

Add a test inside `mod tests`:

```rust
    #[test]
    fn vendored_field_index_loads_and_contains_name_field() {
        let index = field_index();
        assert!(index.len() > 5, "expected several vendored fields, got {}", index.len());
        let name = index.get("name").expect("a 'name' field should be vendored");
        assert_eq!(name.field_type, FieldType::Text);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib fields::tests -- --nocapture`
Expected: all previous tests plus the new loader test pass. If `vendored_field_index_loads_and_contains_name_field` fails because there's no `"name"`-id field in the real vendored data, inspect `assets/presets/fields.json` for the actual field covering the `name` tag key and adjust the assertion to use its real id, per this project's established practice of matching assertions to real vendor content rather than changing the vendor data.

- [ ] **Step 3: Commit**

```bash
git add src/fields.rs
git commit -m "Load vendored field data via include_str!"
```

---

### Task 7: Fields section shell (side panel wiring, single-feature guard)

**Files:**
- Create: `src/ui/fields_section.rs`
- Modify: `src/main.rs:250` (`side_panel_open: [bool; 4]` → `[bool; 5]`), `src/main.rs:338` (`[true, true, true, false]` → adjust for 5 slots), `src/side_panel.rs` (add the Fields section between Selection and Tags), `src/ui/mod.rs` (add `pub mod fields_section;`)

**Interfaces:**
- Consumes: `describe_selected_feature`-style lookup already in `src/side_panel.rs` (reuse the same tags/geometry/match_feature pattern), `crate::MapViewer` (defined in `src/main.rs`).
- Produces: `impl MapViewer { pub(crate) fn render_fields_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement }` in `src/ui/fields_section.rs`, wired into `render_side_panel`.

This task only builds the section's shell — a guard clause for "not exactly one feature selected" and a placeholder for "matched preset has no fields" — real widget rendering is Tasks 8-10.

- [ ] **Step 1: Resize `side_panel_open` and its index reads**

In `src/main.rs`, change:
```rust
    side_panel_open: [bool; 4],
```
to:
```rust
    side_panel_open: [bool; 5],
```
And change the initializer (currently `side_panel_open: [true, true, true, false],`) to insert a slot for Fields between Selection (index 1) and Tags (now index 3):
```rust
            side_panel_open: [true, true, true, true, false],
```
(new index mapping: 0=Layers, 1=Selection, 2=Fields, 3=Tags, 4=History)

In `src/side_panel.rs`'s `render_side_panel` (around line 43-46), update the index reads and add a new one:

```rust
        let open_layers = self.side_panel_open[0];
        let open_selection = self.side_panel_open[1];
        let open_fields = self.side_panel_open[2];
        let open_tags = self.side_panel_open[3];
        let open_history = self.side_panel_open[4];
```

Add a `fields_section` local next to the existing section locals, and insert its `collapsible_section` call between Selection and Tags in the child list, updating the Tags/History `collapsible_section` calls' `index` arguments from `2`/`3` to `3`/`4`:

```rust
        let fields_section = self.render_fields_section(cx);
```

```rust
                    .child(self.collapsible_section(
                        selection_title,
                        1,
                        open_selection,
                        selection_section,
                        cx,
                    ))
                    .child(self.collapsible_section("Fields", 2, open_fields, fields_section, cx))
                    .child(self.collapsible_section("Tags", 3, open_tags, tags_section, cx))
                    .child(self.collapsible_section(
                        "History",
                        4,
                        open_history,
                        history_section,
                        cx,
                    )),
```

- [ ] **Step 2: Create `src/ui/fields_section.rs` with the guard-clause shell**

```rust
//! The Fields accordion section: typed widgets (text/combo/check/radio/
//! multi-combo) for the selected feature's matched preset, built from the
//! vendored `crate::fields::FieldIndex`. Only renders when exactly one
//! feature is selected — multi-select keeps using the raw Tags table.
//! See docs/superpowers/specs/2026-07-07-preset-fields-editor-design.md.

use gpui::{prelude::*, Context};
use gpui_component::{label::Label, ActiveTheme};

use crate::MapViewer;

impl MapViewer {
    /// The Fields accordion section body.
    pub(crate) fn render_fields_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.selected.len() != 1 {
            let message = if self.selected.is_empty() {
                "No selection."
            } else {
                "Select a single feature to edit fields."
            };
            return Label::new(message)
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let feature = self.selected[0];
        let Some((preset, _tags)) = self.matched_preset_for_field_editing(&feature) else {
            return Label::new("No matched preset.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        };

        if preset.fields.is_empty() {
            return Label::new("This feature type has no editable fields.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        // Real widget rendering lands in Tasks 8-10; for now, just list the
        // resolved field labels as plain text so the shell is independently
        // verifiable.
        let fields = crate::fields::resolve_fields(crate::fields::field_index(), &preset.fields);
        gpui::div()
            .flex()
            .flex_col()
            .gap_1()
            .children(fields.into_iter().map(|f| Label::new(f.label.clone()).text_sm()))
            .into_any_element()
    }

    /// Resolve the matched `Preset` and current tags for the single
    /// selected feature, or `None` if the feature/layer/tags/geometry
    /// can't be resolved (mirrors `describe_selected_feature`'s existing
    /// graceful-`None` pattern in `src/side_panel.rs`).
    fn matched_preset_for_field_editing(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(&'static osm_gpui::presets::Preset, std::collections::HashMap<String, String>)> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let preset = osm_gpui::presets::preset_index().match_feature(&tags, geometry)?;
        Some((preset, tags))
    }
}
```

Note: `matched_preset_for_field_editing` returning `&'static Preset` works because `preset_index()` returns `&'static PresetIndex` and `match_feature` borrows from it — this mirrors how `describe_selected_feature` already calls `preset_index()` in `src/side_panel.rs`. If `PresetIndex::match_feature`'s actual return type isn't `Option<&'static Preset>` in practice (e.g. it's `Option<&'a Preset>` tied to a passed-in reference lifetime rather than `'static`), adjust the signature to return an owned `Preset` clone instead, or restructure to avoid the mismatch — check `src/presets.rs`'s actual `match_feature` signature before writing this exactly, since the borrow-checker will reject a wrong assumption immediately and that's a fast, cheap thing to fix at implementation time.

Add `pub mod fields_section;` to `src/ui/mod.rs` (check its existing `pub mod` list first for alphabetical placement, e.g. after `pub mod custom_imagery_store;`-equivalent entries or near `pub mod nsi_dialog;`).

- [ ] **Step 3: Verify it compiles and the app runs**

Run: `cargo build`
Expected: builds cleanly. `side_panel.rs`'s existing tests (`preset_label_tests`) should be unaffected — run `cargo test --lib side_panel::` to confirm no regressions from the index renumbering.

- [ ] **Step 4: Manual verification**

Run: `cargo run --release -- --script docs/screenshots/fixtures/select.osm` is not itself a valid invocation — instead write a small script file (e.g. `docs/screenshots/verify_fields_shell.osmscript`, not committed — this is a manual check, delete it after) with:
```
window 1200 800
load_osm docs/screenshots/fixtures/select.osm
viewport 40.7120 -74.0060 18
wait_idle 5s
click 459,414
wait_idle 2s
capture out/fields-shell.png
```
Run: `cargo run --release -- --script docs/screenshots/verify_fields_shell.osmscript --window-size 1200x800`, then view `out/fields-shell.png` (e.g. via the Read tool) and confirm a new "Fields" section appears between Selection and Tags, showing plain-text labels for the Cafe preset's fields (exact labels depend on real vendored data — report what you actually see).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/side_panel.rs src/ui/fields_section.rs src/ui/mod.rs
git commit -m "Add Fields section shell to the side panel"
```

---

### Task 8: Text field widget (InputState, commit on blur/Enter)

**Files:**
- Modify: `src/main.rs` (add `fields_text_inputs: std::collections::HashMap<String, gpui::Entity<gpui_component::input::InputState>>` field + reset-on-selection-change wiring), `src/ui/fields_section.rs`

**Interfaces:**
- Consumes: `gpui_component::input::{Input, InputState, InputEvent}` (existing dependency, already used in `src/ui/tag_edit_dialog.rs`/`src/ui/nsi_dialog.rs`), `MapViewer::apply_nsi_preset` (existing, `src/main.rs:934`).
- Produces: text fields render as a live `InputState`-backed input, committing via `apply_nsi_preset` on blur or Enter.

- [ ] **Step 1: Add the per-field input-state map to `MapViewer`**

In `src/main.rs`, add a field next to `side_panel_open` (or another UI-state field):

```rust
    /// Live `InputState` entities for the Fields section's text widgets,
    /// keyed by field id. Rebuilt whenever the selected feature changes so
    /// stale entities from a previous feature never leak into a new one.
    fields_text_inputs: std::collections::HashMap<String, gpui::Entity<gpui_component::input::InputState>>,
```

Initialize it in the same constructor that sets `side_panel_open`:

```rust
            fields_text_inputs: std::collections::HashMap::new(),
```

Find wherever `self.selected` is assigned when a new feature is clicked/selected (the hit-test/selection-update code path already in `src/main.rs`) and add `self.fields_text_inputs.clear();` right after each place `self.selected` is reassigned to a *different* set of features — read the existing selection-update code first to find every assignment site (there may be more than one: click, box-select, programmatic selection from the Selection-row click-to-select in `src/side_panel.rs`) and add the clear consistently at each one, or better, add it inside a single shared helper if one already exists for "set selection and notify."

- [ ] **Step 2: Render text fields with a real `InputState`, committing on blur/Enter**

In `src/ui/fields_section.rs`, replace the placeholder `Label`-only rendering for fields with real per-type dispatch. Add a helper that gets-or-creates the input state entity for a field:

```rust
use gpui_component::input::{Input, InputEvent, InputState};

impl MapViewer {
    /// Get or create the `InputState` entity for a text field, seeded from
    /// `current_value` only on creation (an existing entity keeps whatever
    /// the user has typed, even if `current_value` hasn't changed — it's
    /// re-read from `self.fields_text_inputs`, not rebuilt every render).
    fn text_field_input(
        &mut self,
        field_id: &str,
        current_value: &str,
        placeholder: Option<&str>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<InputState> {
        if let Some(existing) = self.fields_text_inputs.get(field_id) {
            return existing.clone();
        }
        let placeholder = placeholder.unwrap_or("").to_string();
        let current_value = current_value.to_string();
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(placeholder);
            state.set_value(current_value, window, cx);
            state
        });
        self.fields_text_inputs.insert(field_id.to_string(), entity.clone());
        entity
    }
}
```

Note: `render_fields_section`'s current signature takes `&self` (from Task 7) but creating/mutating `self.fields_text_inputs` requires `&mut self`. Change `render_fields_section`'s signature to `&mut self` and update its call site in `src/side_panel.rs`'s `render_side_panel` accordingly (it's currently called as `self.render_fields_section(cx)` inside a method that itself takes `&self` — `render_side_panel` will need `&mut self` too, which cascades to `render_side_panel`'s own call site; trace this chain and update each signature, since GPUI's `Render::render` for `MapViewer` already takes `&mut self` at the top level, so this should be a mechanical propagation, not a redesign).

Also change `render_fields_section`'s parameter list to accept `window: &mut gpui::Window` (needed by `InputState::new`), propagating from wherever `render_side_panel` receives its own `window` parameter (check `Render::render`'s signature — it receives both `window` and `cx`).

Render a text field like this (add to the per-field dispatch in `render_fields_section`, once `window` is available):

```rust
            FieldType::Text => {
                let current = tags.get(&field.key).cloned().unwrap_or_default();
                let input = self.text_field_input(
                    &field.id,
                    &current,
                    field.placeholder.as_deref(),
                    window,
                    cx,
                );
                let field_key = field.key.clone();
                let feature = feat;
                cx.subscribe(&input, move |this: &mut Self, entity, event: &InputEvent, cx| {
                    let should_commit = matches!(event, InputEvent::Blur)
                        || matches!(event, InputEvent::PressEnter { .. });
                    if !should_commit {
                        return;
                    }
                    let value = entity.read(cx).value().to_string();
                    this.apply_nsi_preset(
                        &feature,
                        std::collections::HashMap::from([(field_key.clone(), value)]),
                    );
                    cx.notify();
                })
                .detach();

                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()).text_sm())
                    .child(Input::new(&input))
                    .into_any_element()
            }
```

Note on the `cx.subscribe` inside a render method: subscribing every render would leak duplicate subscriptions. Guard this by only subscribing once per field id — e.g. track subscribed field ids in a `HashSet<String>` field on `MapViewer` (`fields_text_subscribed: std::collections::HashSet<String>`, cleared alongside `fields_text_inputs` in Step 1), and only call `cx.subscribe` the first time `text_field_input` creates a *new* entity for that field id (inside the `if let Some(existing) = ...` early-return branch, skip re-subscribing; only subscribe in the entity-creation branch). Fold this into `text_field_input` itself: since it already knows whether it just created a new entity, move the `cx.subscribe` call inside `text_field_input`'s creation branch rather than the caller.

- [ ] **Step 3: Manual verification**

Reuse/extend `docs/screenshots/verify_fields_shell.osmscript` from Task 7: after the existing `click`/`wait_idle`/`capture`, confirm the captured screenshot shows a real text input (with a visible border/box) for the Name field, pre-filled with "Fixture Cafe" (the fixture's `name` tag). Note: the `.osmscript` DSL has no op to type characters into a focused input (only single `key CHORD` dispatches, and `click`/`drag`/`scroll` — confirmed by reading `src/script/op.rs`), so this manual step can only verify the seeded initial value renders correctly, not a full edit-then-blur round trip; note that limitation honestly in your report rather than fabricating an edit test.

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all tests pass, no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/ui/fields_section.rs
git commit -m "Render text fields with commit-on-blur/Enter"
```

---

### Task 9: Check and Radio field widgets

**Files:**
- Modify: `src/ui/fields_section.rs`

**Interfaces:**
- Consumes: `gpui_component::checkbox::Checkbox`, `gpui_component::radio::{Radio, RadioGroup}` (existing dependency), `MapViewer::apply_nsi_preset`.
- Produces: `FieldType::Check` and `FieldType::Radio` render as immediate-commit widgets — no persistent entity state needed (unlike text fields), since both widgets are stateless per-render builders that read the current tag value directly.

- [ ] **Step 1: Render Check fields**

Add to the per-field dispatch in `render_fields_section`:

```rust
            FieldType::Check => {
                let current = tags.get(&field.key).map(String::as_str) == Some("yes");
                let field_key = field.key.clone();
                let feature = feat;
                gpui::div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        gpui_component::checkbox::Checkbox::new(("field-check", field.id.clone()))
                            .checked(current)
                            .on_click(cx.listener(move |this, checked: &bool, _window, cx| {
                                let value = if *checked { "yes" } else { "no" };
                                this.apply_nsi_preset(
                                    &feature,
                                    std::collections::HashMap::from([(
                                        field_key.clone(),
                                        value.to_string(),
                                    )]),
                                );
                                cx.notify();
                            })),
                    )
                    .child(Label::new(field.label.clone()).text_sm())
                    .into_any_element()
            }
```

Note: `Checkbox::new` takes `impl Into<ElementId>` — confirm `("field-check", field.id.clone())` (a tuple of a static str and a `String`) actually implements `Into<ElementId>` in this GPUI version (the existing layer-checkbox usage in `src/side_panel.rs` uses `("layer", index)` with a `usize`, not a `String` — check whether `ElementId` accepts a `String` second element or needs `SharedString`/`&str`; adjust to `field.id.as_str().to_string()` wrapped appropriately, or use `SharedString::from(field.id.clone())`, matching whatever the real trait bound requires).

- [ ] **Step 2: Render Radio fields**

```rust
            FieldType::Radio => {
                let current_value = tags.get(&field.key).cloned();
                let field_key = field.key.clone();
                let feature = feat;
                let options = field.options.clone();
                let selected_index = options
                    .iter()
                    .position(|opt| Some(&opt.value) == current_value.as_ref());

                let group = gpui_component::radio::RadioGroup::new(("field-radio", field.id.clone()))
                    .children(options.iter().enumerate().map(|(i, opt)| {
                        gpui_component::radio::Radio::new(("field-radio-option", i))
                            .checked(Some(i) == selected_index)
                            .label(opt.label.clone())
                    }))
                    .on_click(cx.listener(move |this, index: &usize, _window, cx| {
                        let Some(opt) = options.get(*index) else { return };
                        this.apply_nsi_preset(
                            &feature,
                            std::collections::HashMap::from([(
                                field_key.clone(),
                                opt.value.clone(),
                            )]),
                        );
                        cx.notify();
                    }));

                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()).text_sm())
                    .child(group)
                    .into_any_element()
            }
```

Note: `Radio::label(...)` is assumed from the existing layer-checkbox's `.label(label)` builder call on `Checkbox` in `src/side_panel.rs` — confirm `Radio` has an equivalent `.label()` builder method (read `~/.cargo/git/checkouts/gpui-component-*/*/crates/ui/src/radio.rs` if it doesn't; if missing, render the label as a separate `Label` child next to each `Radio` instead).

- [ ] **Step 3: Manual verification**

Extend the manual-verification script from Task 8 (or use a new fixture with a preset that has a check/radio field — e.g. add an `internet_access=wlan` tag to a test node and confirm the Fixture Cafe's relevant fields, if any, render as checkbox/radio rather than plain text; the exact fields present depend on real vendored data, so inspect what the Cafe preset's real `fields`/`more_fields` resolve to first via the Task 5 spot-check output before writing this step's exact assertions).

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/ui/fields_section.rs
git commit -m "Render check and radio fields with immediate commit"
```

---

### Task 10: Combo and MultiCombo field widgets, "Add field"

**Files:**
- Modify: `src/ui/fields_section.rs`

**Interfaces:**
- Consumes: `MapViewer::apply_nsi_preset`, `crate::fields::resolve_more_fields` (Task 2).
- Produces: `FieldType::Combo` and `FieldType::MultiCombo` render as a hand-rolled expand/collapse option list (mirroring `src/ui/nsi_dialog.rs`'s result-list pattern, but without a search box since option lists are small and static per Global Constraints). An "Add field" control lists `preset.more_fields` (via `resolve_more_fields`, excluding `preset.fields`), promoting a chosen one into the rendered list for the current editing session.

- [ ] **Step 1: Add per-feature UI state for open dropdowns and promoted more-fields**

In `src/main.rs`, add two more fields next to `fields_text_inputs` (cleared at the same selection-change site):

```rust
    /// Which field's combo/multiCombo option list is currently expanded,
    /// if any (`None` = all collapsed). Only one at a time.
    fields_open_combo: Option<String>,
    /// Field ids promoted from a preset's `more_fields` into the visible
    /// list for the current editing session (cleared on selection change).
    fields_promoted_more_fields: std::collections::HashSet<String>,
```

Initialize both alongside `fields_text_inputs` and clear both at the same selection-change site from Task 8 Step 1.

- [ ] **Step 2: Render Combo fields (single-select)**

```rust
            FieldType::Combo => {
                let current_value = tags.get(&field.key).cloned();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());
                let current_label = current_value
                    .as_ref()
                    .and_then(|v| field.options.iter().find(|o| &o.value == v))
                    .map(|o| o.label.clone())
                    .unwrap_or_else(|| "(none)".to_string());

                let field_id_for_toggle = field.id.clone();
                let header = gpui::div()
                    .id(("field-combo-header", field.id.clone()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(Label::new(current_label).text_sm())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.fields_open_combo = if this.fields_open_combo.as_deref()
                                == Some(field_id_for_toggle.as_str())
                            {
                                None
                            } else {
                                Some(field_id_for_toggle.clone())
                            };
                            cx.notify();
                        }),
                    );

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()).text_sm())
                    .child(header);

                if is_open {
                    let field_key = field.key.clone();
                    let feature = feat;
                    let field_id_for_close = field.id.clone();
                    column = column.child(
                        gpui::div()
                            .id(("field-combo-options", field.id.clone()))
                            .flex()
                            .flex_col()
                            .max_h(gpui::px(160.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(cx.theme().border)
                            .children(field.options.iter().enumerate().map(|(i, opt)| {
                                let value = opt.value.clone();
                                let field_key = field_key.clone();
                                let field_id_for_close = field_id_for_close.clone();
                                gpui::div()
                                    .id(("field-combo-option", i))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(cx.theme().accent))
                                    .child(Label::new(opt.label.clone()).text_sm())
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _ev, _window, cx| {
                                            this.apply_nsi_preset(
                                                &feature,
                                                std::collections::HashMap::from([(
                                                    field_key.clone(),
                                                    value.clone(),
                                                )]),
                                            );
                                            this.fields_open_combo = None;
                                            let _ = &field_id_for_close;
                                            cx.notify();
                                        }),
                                    )
                            })),
                    );
                }

                column.into_any_element()
            }
```

- [ ] **Step 3: Render MultiCombo fields (list of chips + add-dropdown)**

```rust
            FieldType::MultiCombo => {
                let current_values: Vec<String> = tags
                    .get(&field.key)
                    .map(|v| v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
                    .unwrap_or_default();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()).text_sm());

                // Chips for already-selected values, each removable.
                let chips = gpui::div().flex().flex_row().flex_wrap().gap_1().children(
                    current_values.iter().map(|value| {
                        let field_key = field.key.clone();
                        let feature = feat;
                        let value_to_remove = value.clone();
                        let remaining: Vec<String> = current_values
                            .iter()
                            .filter(|v| *v != &value_to_remove)
                            .cloned()
                            .collect();
                        let label = field
                            .options
                            .iter()
                            .find(|o| &o.value == value)
                            .map(|o| o.label.clone())
                            .unwrap_or_else(|| value.clone());
                        gpui::div()
                            .id(("field-multicombo-chip", value.clone()))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(cx.theme().accent)
                            .cursor_pointer()
                            .child(Label::new(format!("{} ×", label)).text_xs())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.apply_nsi_preset(
                                        &feature,
                                        std::collections::HashMap::from([(
                                            field_key.clone(),
                                            remaining.join(";"),
                                        )]),
                                    );
                                    cx.notify();
                                }),
                            )
                    }),
                );
                column = column.child(chips);

                let field_id_for_toggle = field.id.clone();
                column = column.child(
                    gpui::div()
                        .id(("field-multicombo-add", field.id.clone()))
                        .cursor_pointer()
                        .child(Label::new("+ Add").text_xs().text_color(cx.theme().muted_foreground))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.fields_open_combo = if this.fields_open_combo.as_deref()
                                    == Some(field_id_for_toggle.as_str())
                                {
                                    None
                                } else {
                                    Some(field_id_for_toggle.clone())
                                };
                                cx.notify();
                            }),
                        ),
                );

                if is_open {
                    let field_key = field.key.clone();
                    let feature = feat;
                    let current_values = current_values.clone();
                    column = column.child(
                        gpui::div()
                            .id(("field-multicombo-options", field.id.clone()))
                            .flex()
                            .flex_col()
                            .max_h(gpui::px(160.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(cx.theme().border)
                            .children(
                                field
                                    .options
                                    .iter()
                                    .filter(|opt| !current_values.contains(&opt.value))
                                    .enumerate()
                                    .map(|(i, opt)| {
                                        let value = opt.value.clone();
                                        let field_key = field_key.clone();
                                        let mut updated = current_values.clone();
                                        gpui::div()
                                            .id(("field-multicombo-option", i))
                                            .px_2()
                                            .py_1()
                                            .cursor_pointer()
                                            .hover(|el| el.bg(cx.theme().accent))
                                            .child(Label::new(opt.label.clone()).text_sm())
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(move |this, _ev, _window, cx| {
                                                    updated.push(value.clone());
                                                    this.apply_nsi_preset(
                                                        &feature,
                                                        std::collections::HashMap::from([(
                                                            field_key.clone(),
                                                            updated.join(";"),
                                                        )]),
                                                    );
                                                    this.fields_open_combo = None;
                                                    cx.notify();
                                                }),
                                            )
                                    }),
                            ),
                    );
                }

                column.into_any_element()
            }
```

- [ ] **Step 4: "Add field" control for `more_fields`**

At the end of `render_fields_section`'s main body (after rendering `preset.fields`), add a section listing promoted + addable `more_fields`:

```rust
        let already_shown: Vec<String> = preset
            .fields
            .iter()
            .cloned()
            .chain(self.fields_promoted_more_fields.iter().cloned())
            .collect();
        let addable = crate::fields::resolve_more_fields(
            crate::fields::field_index(),
            &preset.more_fields,
            &already_shown,
        );

        // Render already-promoted more_fields using the same per-type
        // dispatch as `preset.fields` (factor the per-field dispatch above
        // into a small helper `render_one_field(&mut self, field: &Field,
        // tags: &HashMap<String,String>, feat: FeatureRef, window: &mut
        // Window, cx: &mut Context<Self>) -> gpui::AnyElement` during this
        // task, so both `preset.fields` and promoted `more_fields` call the
        // same per-type code instead of duplicating it).

        if !addable.is_empty() {
            column = column.child(
                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(addable.into_iter().map(|f| {
                        let field_id = f.id.clone();
                        gpui::div()
                            .id(("field-add-more", field_id.clone()))
                            .cursor_pointer()
                            .child(Label::new(format!("+ {}", f.label)).text_xs())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.fields_promoted_more_fields.insert(field_id.clone());
                                    cx.notify();
                                }),
                            )
                    })),
            );
        }
```

(This step's exact integration point depends on how Tasks 7-9 structured `render_fields_section`'s body — refactor as needed to fold in the "already-shown" field list and the addable-more-fields control cleanly; the important behavioral contract is: promoted fields render with the same widget dispatch as default fields, and "Add field" never lists something already visible.)

- [ ] **Step 5: Manual verification**

Extend the manual-verification script: click a combo field's header to expand it, click an option, capture a screenshot showing the value committed and the dropdown collapsed. Report exactly what you observe against real vendored field data for the Cafe preset (or whichever preset the fixture node matches) — if the Cafe preset has no combo/multiCombo fields in the real vendored data, note that and verify with whatever combo-typed field the vendored `fields.json` does contain, adding a temporary tag to the fixture node if needed to exercise it, then reporting what preset/field you actually tested against.

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/ui/fields_section.rs
git commit -m "Render combo and multiCombo fields, add 'Add field' control"
```

---

### Task 11: Preset picker dialog

**Files:**
- Create: `src/ui/preset_picker_dialog.rs`
- Modify: `src/ui/mod.rs` (add `pub mod preset_picker_dialog;`), `src/main.rs` (new action, dialog field, open/subscribe handler, "change type" trigger)

**Interfaces:**
- Consumes: `osm_gpui::presets::{preset_index, PresetIndex, Preset}`, `MapViewer::apply_nsi_preset`, `EditableLayer::feature_geometry` (all existing).
- Produces: `pub struct PresetPickerDialog` with `pub enum DialogEvent { Submitted(std::collections::HashMap<String, String>), Cancelled }`, structurally identical to `NsiPresetDialog`.

- [ ] **Step 1: Add a search method to `PresetIndex` for the picker**

In `src/presets.rs`, add (this is new surface on the existing type, not a new file):

```rust
impl PresetIndex {
    /// Search presets by name (case-insensitive substring, same normalize
    /// approach as brand search elsewhere in this codebase), filtered to
    /// only presets whose `geometry` includes `geometry`. Capped at
    /// `limit` results, unordered beyond "substring matches first".
    pub fn search(&self, query: &str, geometry: Geometry, limit: usize) -> Vec<&Preset> {
        let q = query.to_lowercase();
        self.presets
            .iter()
            .filter(|p| p.geometry.contains(&geometry))
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
            .take(limit)
            .collect()
    }
}
```

Add a test in `src/presets.rs`'s `mod tests`:

```rust
    #[test]
    fn search_filters_by_geometry_and_name_substring() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let results = index.search("cafe", Geometry::Point, 10);
        assert!(results.iter().any(|p| p.id == "amenity/cafe"));
        let area_only_results = index.search("cafe", Geometry::Line, 10);
        assert!(!area_only_results.iter().any(|p| p.id == "amenity/cafe"));
    }
```

(`MATCH_FIXTURE` here refers to the fixture constant already used by `match_feature`'s tests — check `src/presets.rs` for its exact name.)

Run: `cargo test --lib presets::tests -- --nocapture` — confirm the new test passes alongside all existing ones.

Commit:
```bash
git add src/presets.rs
git commit -m "Add geometry-filtered preset search for the picker dialog"
```

- [ ] **Step 2: Create `src/ui/preset_picker_dialog.rs`**

Mirror `src/ui/nsi_dialog.rs` structurally exactly (same `InputState` query box, same keyboard up/down/Enter/Escape handling, same click-to-submit row pattern), swapping its NSI-specific parts:

```rust
//! Modal dialog to search vendored iD tagging schema presets and apply the
//! matched tags to the single currently-selected feature — lets a user
//! deliberately change a feature's type, not just accept whatever
//! PresetIndex::match_feature auto-matched.

use gpui::{
    div, prelude::*, rgba, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, Window,
};
use gpui_component::{
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};
use std::collections::HashMap;

use osm_gpui::presets::Geometry;

const MAX_RESULTS: usize = 30;

pub enum DialogEvent {
    Submitted(HashMap<String, String>),
    Cancelled,
}

pub struct PresetPickerDialog {
    query: Entity<InputState>,
    geometry: Geometry,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for PresetPickerDialog {}

impl PresetPickerDialog {
    pub fn new(geometry: Geometry, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search feature types…"));
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            geometry,
            selected_index: 0,
            focus_handle,
        }
    }

    fn results(&self, cx: &Context<Self>) -> Vec<(String, String)> {
        // (name, id) pairs — avoid borrowing `&'static Preset` across
        // renders; clone the small bits we need to display/apply.
        let text = self.query.read(cx).value().to_string();
        osm_gpui::presets::preset_index()
            .search(&text, self.geometry, MAX_RESULTS)
            .into_iter()
            .map(|p| (p.name.clone(), p.id.clone()))
            .collect()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some((_, id)) = results.get(self.selected_index) {
            if let Some(preset) = find_preset_by_id(id) {
                cx.emit(DialogEvent::Submitted(preset.tags.clone()));
            }
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.submit_selected(cx),
            "down" => {
                let count = self.results(cx).len();
                if count > 0 {
                    self.selected_index = (self.selected_index + 1).min(count - 1);
                    cx.notify();
                }
            }
            "up" => {
                self.selected_index = self.selected_index.saturating_sub(1);
                cx.notify();
            }
            _ => {}
        }
    }
}

/// `PresetIndex` doesn't expose lookup-by-id today — find the full
/// `Preset` (for its `tags`) by re-searching for its id among all presets.
/// Simple and correct at v1 scale (~1700 presets, one linear scan on
/// submit, not per-keystroke).
fn find_preset_by_id(id: &str) -> Option<&'static osm_gpui::presets::Preset> {
    osm_gpui::presets::preset_index()
        .search("", Geometry::Point, usize::MAX)
        .into_iter()
        .find(|p| p.id == id)
        .or_else(|| {
            // `search` filters by geometry; id lookup shouldn't care about
            // geometry, so fall back to checking every geometry variant.
            [Geometry::Vertex, Geometry::Line, Geometry::Area, Geometry::Relation]
                .into_iter()
                .find_map(|g| {
                    osm_gpui::presets::preset_index()
                        .search("", g, usize::MAX)
                        .into_iter()
                        .find(|p| p.id == id)
                })
        })
}
```

Note: `find_preset_by_id`'s geometry-juggling is awkward — a cleaner fix is adding a proper `PresetIndex::get(&self, id: &str) -> Option<&Preset>` (analogous to `FieldIndex::get`) in the same Step 1 as `search`, and using that directly instead. Prefer that: add `pub fn get(&self, id: &str) -> Option<&Preset> { self.presets.iter().find(|p| p.id == id) }` to `PresetIndex` in Step 1, and simplify `find_preset_by_id` (or drop it entirely and call `osm_gpui::presets::preset_index().get(id)` directly in `submit_selected`).

Copy the `Focusable` impl and `Render` impl bodies from `src/ui/nsi_dialog.rs` nearly verbatim, replacing:
- The dialog title `"Apply NSI Preset"` → `"Change Feature Type"`.
- `entry.display_name` / `format_tag_preview(entry)` → the preset's `name`, and a tag preview built from `Preset.tags` the same way `format_tag_preview` builds one from `NsiEntry.tags` (reuse or duplicate that small formatting function — it's generic over any `HashMap<String,String>` already, so it can likely be moved to a shared location or just called with `preset.tags` directly if its signature is `&HashMap<String,String>` rather than `&NsiEntry` specifically; check its exact signature first).
- `crate::nsi::current()`'s "downloading" empty-state branch — not needed here, since the vendored `PresetIndex` is always available synchronously (no network fetch); simplify to just "No matches." for an empty result set, no "downloading" state at all.

Add a small test module mirroring `nsi_dialog.rs`'s (adjusted for `Preset` instead of `NsiEntry`), if a tag-preview formatting function is duplicated rather than shared.

- [ ] **Step 3: Wire the dialog into `src/main.rs`**

Add `pub mod preset_picker_dialog;` to `src/ui/mod.rs`.

Add a field to `MapViewer` (next to `nsi_dialog: Option<Entity<NsiPresetDialog>>`):

```rust
    preset_picker_dialog: Option<gpui::Entity<osm_gpui::ui::preset_picker_dialog::PresetPickerDialog>>,
```

Initialize it as `None` alongside `nsi_dialog`.

Add a new action type (mirroring however `ApplyNsiPreset` is declared — check its exact declaration, likely a `#[derive(Action)]`-style struct or gpui `actions!` macro invocation near the top of `main.rs`) named e.g. `ChangeFeatureType`, and a handler mirroring `on_apply_nsi_preset` (`src/main.rs:893-925`) exactly, but:
- Computing the selected feature's `Geometry` via `feature_geometry` (same as `matched_preset_for_field_editing` in `src/ui/fields_section.rs` does) before constructing `PresetPickerDialog::new(geometry, window, cx)`.
- Subscribing to `DialogEvent::Submitted(preset_tags)` and calling `self.apply_nsi_preset(&target, preset_tags.clone())` — identical call, since `apply_nsi_preset` doesn't care which dialog produced the tags.

Add a "change type" trigger: a small button in the Fields section header (in `src/ui/fields_section.rs`, near the top of `render_fields_section`, before the per-field list) that dispatches the new action via `window.dispatch_action(Box::new(ChangeFeatureType), cx)` on click — mirroring how any existing menu/button in this codebase dispatches an action (check `src/menu.rs` or an existing button's `on_click` for the exact dispatch call shape used elsewhere).

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 5: Manual verification**

Extend the manual-verification script: select the fixture cafe node, trigger "change type", type a different preset name into the search box, click a result, capture a screenshot confirming the feature's tags changed (e.g. via the Tags section) and the Fields section now shows the new preset's fields.

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/ui/preset_picker_dialog.rs src/ui/mod.rs src/main.rs src/ui/fields_section.rs
git commit -m "Add preset picker dialog to deliberately change a feature's type"
```

## Self-Review Notes

- **Spec coverage:** vendoring extension (Tasks 4-5), Field/FieldIndex data model (Tasks 1-2, 6), Preset.fields/more_fields (Task 3), Fields section shell + single-feature guard (Task 7), all 5 widget types (Tasks 8-10), "Add field" (Task 10), preset picker (Task 11) — every section of the spec has a task. Non-goals (multi-key fields, dynamic combo suggestions, multi-select, non-English, field validation, Tags-table changes) are respected by construction — no task touches those areas.
- **Type consistency:** `Field`/`FieldType`/`FieldOption`/`FieldIndex` defined once (Task 1) and used identically through Tasks 2-11. `Preset.fields`/`more_fields` defined once (Task 3), consumed identically in Tasks 7, 10, 11. `apply_nsi_preset`'s existing signature is used unchanged throughout — no task redefines or wraps it.
- **Known risks flagged inline for the implementer to resolve against real code rather than guess:** `Checkbox`/`Radio`'s exact `ElementId`/`.label()` builder API (Task 9), `render_fields_section`'s `&self`→`&mut self` + `window` parameter propagation (Task 8), whether `match_feature`'s return lifetime is truly `'static` (Task 7), and the exact declaration shape of `ApplyNsiPreset`-style actions to mirror for `ChangeFeatureType` (Task 11) — these are called out explicitly rather than papered over with a guessed signature, since a wrong guess here is a compile error the implementer resolves in seconds by reading the real file, while a silently wrong guess baked into "complete code" would be worse.
- **Scope check:** this is one cohesive feature (field editor + picker) with a natural task order (data model → vendoring → UI shell → widgets by type → picker). 11 tasks is larger than the previous 10-task plan but each is still a single bite-sized, independently reviewable deliverable.
