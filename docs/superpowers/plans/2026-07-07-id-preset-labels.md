# iD Preset Friendly Labels/Icons Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a human-readable feature type (name + icon) for selected features in the side panel's Selection rows, matched from a vendored copy of the iD tagging schema presets.

**Architecture:** A new `src/presets.rs` module owns preset/area-key data types, JSON parsing, geometry classification, and geometry-aware tag matching — mirroring the shape of the existing `src/nsi.rs`. Preset and area-key data is vendored into `assets/presets/` (JSON + SVG icons) rather than fetched at runtime, refreshed by a manual dev tool (`examples/update_presets.rs`). `OsmLayer` gains a `feature_geometry` method (alongside its existing `feature_tags`) so the side panel can classify a selected feature's geometry; `render_selection_section` uses both to look up and render the matched preset's icon + name ahead of the existing `"Node 123456"` text.

**Tech Stack:** Rust, serde/serde_json (already a dependency), GPUI's `svg()` element with `.external_path(...)`, `ureq` via the existing `src/http.rs` retry client (used only by the dev-only vendor-update tool, not at app runtime).

## Global Constraints

- Vendored data lives under `assets/presets/` (JSON + `icons/*.svg`), checked into git — no runtime network fetch for presets/icons (unlike `nsi.rs`).
- v1 has **no** locationSet filtering, **no** preset search dialog, **no** NSI-dialog unification, **no** use of preset `terms`/`fields`/`addTags`/`removeTags`, **no** Tags-section display, **no** map-canvas POI icon rendering. Selection-row rows in the side panel only.
- The preset JSON schema kept is exactly: `id`, `name`, `icon` (optional), `tags` (map, `"*"` = key-present wildcard), `geometry` (list of `point`/`vertex`/`line`/`area`/`relation`), `match_score` (float, default `1.0`).
- `area_keys.json` schema: map of tag key → map of excluded values (value `true`); an empty inner map means every value of that key implies area on a closed way.
- The vendor-update tool (`examples/update_presets.rs`) is a manual dev tool, not part of CI or app runtime, and is not unit tested — its correctness is verified by running it and reviewing the resulting diff.
- Icon SVGs are resolved at runtime via `concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets/icons/")` + icon name + `.svg` (same simplification level as the project's current dev-only distribution model — no packaging story yet).

---

### Task 1: Preset and Geometry data types + JSON parsing

**Files:**
- Create: `src/presets.rs`
- Modify: `src/lib.rs` (add `pub mod presets;` alongside the other `pub mod` lines)

**Interfaces:**
- Produces:
  - `pub enum Geometry { Point, Vertex, Line, Area, Relation }` with `impl Geometry { pub fn fallback_name(&self) -> &'static str }`
  - `pub struct Preset { pub id: String, pub name: String, pub icon: Option<String>, pub tags: HashMap<String, String>, pub geometry: Vec<Geometry>, pub match_score: f32 }`
  - `pub struct PresetIndex` with `pub fn from_json(body: &str) -> Result<Self, serde_json::Error>` and `pub fn len(&self) -> usize`

- [ ] **Step 1: Write the failing test**

Create `src/presets.rs` with just enough scaffolding to compile the test module (types with `#[derive(serde::Deserialize)]` left as `todo!()`-free empty stubs won't compile with real assertions, so write the real types directly per the "no placeholders" rule — this step and Step 3 are combined into one file write since Rust requires the types to exist for the test to typecheck at all). Write:

```rust
//! iD tagging schema preset support: vendored preset/area-key data,
//! geometry classification, and geometry-aware tag matching. Read-only
//! (no editing UI) — mirrors the shape of `nsi.rs` but with vendored
//! rather than runtime-fetched data. See
//! `docs/superpowers/specs/2026-07-07-id-preset-labels-design.md`.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Geometry {
    Point,
    Vertex,
    Line,
    Area,
    Relation,
}

impl Geometry {
    /// Name used when no preset matches at all (should be rare in
    /// practice, since the vendored schema itself includes a
    /// geometry-only fallback preset per `Geometry` variant).
    pub fn fallback_name(&self) -> &'static str {
        match self {
            Geometry::Point => "Point",
            Geometry::Vertex => "Vertex",
            Geometry::Line => "Line",
            Geometry::Area => "Area",
            Geometry::Relation => "Relation",
        }
    }
}

fn default_match_score() -> f32 {
    1.0
}

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
}

/// A parsed, in-memory collection of vendored `Preset`s.
pub struct PresetIndex {
    presets: Vec<Preset>,
}

impl PresetIndex {
    /// Parse a JSON array of `Preset` (the vendored `assets/presets/presets.json`
    /// shape, or a small fixture array in tests).
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        let presets: Vec<Preset> = serde_json::from_str(body)?;
        Ok(Self { presets })
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
    [
      {
        "id": "amenity/cafe",
        "name": "Cafe",
        "icon": "maki-cafe",
        "tags": {"amenity": "cafe"},
        "geometry": ["point", "vertex", "area"],
        "match_score": 0.5
      },
      {
        "id": "point",
        "name": "Point",
        "tags": {},
        "geometry": ["point"]
      }
    ]
    "#;

    #[test]
    fn from_json_parses_all_entries() {
        let index = PresetIndex::from_json(FIXTURE).unwrap();
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn from_json_defaults_match_score_when_absent() {
        let index = PresetIndex::from_json(FIXTURE).unwrap();
        let point = index.presets.iter().find(|p| p.id == "point").unwrap();
        assert_eq!(point.match_score, 1.0);
    }

    #[test]
    fn from_json_rejects_malformed_body() {
        assert!(PresetIndex::from_json("not json").is_err());
    }
}
```

Add `pub mod presets;` to `src/lib.rs` next to the other `pub mod` declarations (alphabetical position: after `pub mod persist;`, before `pub mod script;`).

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: 3 tests pass (`from_json_parses_all_entries`, `from_json_defaults_match_score_when_absent`, `from_json_rejects_malformed_body`).

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs src/lib.rs
git commit -m "Add Preset/Geometry types and JSON parsing"
```

---

### Task 2: AreaKeys parsing

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: nothing new from Task 1 beyond the file itself.
- Produces:
  - `pub struct AreaKeys` with `pub fn from_json(body: &str) -> Result<Self, serde_json::Error>` and `pub fn closed_way_is_area(&self, tags: &HashMap<String, String>) -> bool`

- [ ] **Step 1: Write the failing test**

Add to `src/presets.rs` (below `PresetIndex`, above the `#[cfg(test)]` module):

```rust
/// Vendored copy of iD's `areaKeys.json`: which tag keys imply an area for
/// a *closed* way, and which specific values of those keys are excluded
/// (i.e. still imply a line even when the way is closed). A key mapped to
/// an empty inner map means every value of that key implies area.
pub struct AreaKeys(HashMap<String, HashMap<String, bool>>);

impl AreaKeys {
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        let map: HashMap<String, HashMap<String, bool>> = serde_json::from_str(body)?;
        Ok(Self(map))
    }

    /// Whether a closed way with these tags should be treated as an area
    /// rather than a line.
    pub fn closed_way_is_area(&self, tags: &HashMap<String, String>) -> bool {
        for (key, value) in tags {
            if let Some(excluded_values) = self.0.get(key) {
                if !excluded_values.contains_key(value) {
                    return true;
                }
            }
        }
        false
    }
}
```

Add tests inside the existing `mod tests` block:

```rust
    const AREA_KEYS_FIXTURE: &str = r#"
    {
      "building": {},
      "highway": {"residential": true, "footway": true}
    }
    "#;

    #[test]
    fn closed_way_is_area_true_when_key_has_no_exclusions() {
        let area_keys = AreaKeys::from_json(AREA_KEYS_FIXTURE).unwrap();
        let tags = HashMap::from([("building".to_string(), "yes".to_string())]);
        assert!(area_keys.closed_way_is_area(&tags));
    }

    #[test]
    fn closed_way_is_area_false_when_value_excluded() {
        let area_keys = AreaKeys::from_json(AREA_KEYS_FIXTURE).unwrap();
        let tags = HashMap::from([("highway".to_string(), "residential".to_string())]);
        assert!(!area_keys.closed_way_is_area(&tags));
    }

    #[test]
    fn closed_way_is_area_true_when_value_not_excluded() {
        let area_keys = AreaKeys::from_json(AREA_KEYS_FIXTURE).unwrap();
        let tags = HashMap::from([("highway".to_string(), "pedestrian".to_string())]);
        assert!(area_keys.closed_way_is_area(&tags));
    }

    #[test]
    fn closed_way_is_area_false_when_no_area_key_present() {
        let area_keys = AreaKeys::from_json(AREA_KEYS_FIXTURE).unwrap();
        let tags = HashMap::from([("natural".to_string(), "water".to_string())]);
        assert!(!area_keys.closed_way_is_area(&tags));
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus the 4 new `closed_way_is_area_*` tests pass (7 total).

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Add AreaKeys parsing and closed-way area heuristic"
```

---

### Task 3: Geometry classification from OsmData

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: `crate::osm::OsmData`, `crate::selection::FeatureKind` (existing types — `FeatureKind` is `{ Node, Way }`, `OsmData` has `pub nodes: HashMap<i64, OsmNode>` and `pub ways: HashMap<i64, OsmWay>`, where `OsmWay.nodes: Vec<i64>`).
- Produces: `pub fn classify_geometry(data: &crate::osm::OsmData, kind: crate::selection::FeatureKind, id: i64, area_keys: &AreaKeys) -> Option<Geometry>`

- [ ] **Step 1: Write the failing test**

Add near the top of `src/presets.rs` (after the `use` line):

```rust
use crate::osm::OsmData;
use crate::selection::FeatureKind;
```

Add the function after `AreaKeys`'s `impl` block:

```rust
/// Classify a feature's geometry for preset matching: a node not referenced
/// by any way is a `Point`; a node referenced by at least one way is a
/// `Vertex`; a way is a `Line` unless it's closed (first node id == last
/// node id) and its tags qualify as an area per `area_keys`, in which case
/// it's an `Area`. Returns `None` if the feature id isn't present in `data`.
pub fn classify_geometry(
    data: &OsmData,
    kind: FeatureKind,
    id: i64,
    area_keys: &AreaKeys,
) -> Option<Geometry> {
    match kind {
        FeatureKind::Node => {
            data.nodes.get(&id)?;
            let referenced = data.ways.values().any(|way| way.nodes.contains(&id));
            Some(if referenced { Geometry::Vertex } else { Geometry::Point })
        }
        FeatureKind::Way => {
            let way = data.ways.get(&id)?;
            let closed = way.nodes.len() >= 2 && way.nodes.first() == way.nodes.last();
            if closed && area_keys.closed_way_is_area(&way.tags) {
                Some(Geometry::Area)
            } else {
                Some(Geometry::Line)
            }
        }
    }
}
```

Add tests inside `mod tests`:

```rust
    use crate::osm::{OsmNode, OsmWay};

    fn node(id: i64, tags: HashMap<String, String>) -> OsmNode {
        OsmNode { id, lat: 0.0, lon: 0.0, version: 1, tags }
    }

    fn way(id: i64, nodes: Vec<i64>, tags: HashMap<String, String>) -> OsmWay {
        OsmWay { id, nodes, version: 1, tags }
    }

    fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
        let mut node_map = HashMap::new();
        for n in nodes {
            node_map.insert(n.id, n);
        }
        let mut way_map = HashMap::new();
        for w in ways {
            way_map.insert(w.id, w);
        }
        OsmData { nodes: node_map, ways: way_map, relations: Vec::new(), bounds: None }
    }

    #[test]
    fn unreferenced_node_is_point() {
        let data = data_with(vec![node(1, HashMap::new())], vec![]);
        let area_keys = AreaKeys::from_json("{}").unwrap();
        assert_eq!(
            classify_geometry(&data, FeatureKind::Node, 1, &area_keys),
            Some(Geometry::Point)
        );
    }

    #[test]
    fn referenced_node_is_vertex() {
        let data = data_with(
            vec![node(1, HashMap::new()), node(2, HashMap::new())],
            vec![way(10, vec![1, 2], HashMap::new())],
        );
        let area_keys = AreaKeys::from_json("{}").unwrap();
        assert_eq!(
            classify_geometry(&data, FeatureKind::Node, 1, &area_keys),
            Some(Geometry::Vertex)
        );
    }

    #[test]
    fn open_way_is_line() {
        let data = data_with(
            vec![node(1, HashMap::new()), node(2, HashMap::new())],
            vec![way(10, vec![1, 2], HashMap::new())],
        );
        let area_keys = AreaKeys::from_json("{}").unwrap();
        assert_eq!(
            classify_geometry(&data, FeatureKind::Way, 10, &area_keys),
            Some(Geometry::Line)
        );
    }

    #[test]
    fn closed_way_with_area_key_is_area() {
        let tags = HashMap::from([("building".to_string(), "yes".to_string())]);
        let data = data_with(
            vec![node(1, HashMap::new()), node(2, HashMap::new())],
            vec![way(10, vec![1, 2, 1], tags)],
        );
        let area_keys = AreaKeys::from_json(r#"{"building": {}}"#).unwrap();
        assert_eq!(
            classify_geometry(&data, FeatureKind::Way, 10, &area_keys),
            Some(Geometry::Area)
        );
    }

    #[test]
    fn closed_way_without_area_key_is_line() {
        let tags = HashMap::from([("highway".to_string(), "residential".to_string())]);
        let data = data_with(
            vec![node(1, HashMap::new()), node(2, HashMap::new())],
            vec![way(10, vec![1, 2, 1], tags)],
        );
        let area_keys = AreaKeys::from_json(r#"{"highway": {"residential": true}}"#).unwrap();
        assert_eq!(
            classify_geometry(&data, FeatureKind::Way, 10, &area_keys),
            Some(Geometry::Line)
        );
    }

    #[test]
    fn missing_feature_id_returns_none() {
        let data = data_with(vec![], vec![]);
        let area_keys = AreaKeys::from_json("{}").unwrap();
        assert_eq!(classify_geometry(&data, FeatureKind::Node, 999, &area_keys), None);
        assert_eq!(classify_geometry(&data, FeatureKind::Way, 999, &area_keys), None);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus 6 new geometry-classification tests pass (13 total).

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Add geometry classification for preset matching"
```

---

### Task 4: Geometry-aware preset matching

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: `PresetIndex` (Task 1), `Geometry` (Task 1)
- Produces:
  - `impl PresetIndex { pub fn match_feature(&self, tags: &HashMap<String, String>, geometry: Geometry) -> Option<&Preset> }`
  - `pub fn describe_feature(index: &PresetIndex, tags: &HashMap<String, String>, geometry: Geometry) -> (String, Option<String>)` — returns `(friendly_name, icon_name)`; `icon_name` is the raw preset `icon` field (e.g. `"maki-cafe"`), not a path.

- [ ] **Step 1: Write the failing test**

Add to the `impl PresetIndex` block in `src/presets.rs`:

```rust
    /// Find the best-matching preset for a feature's tags and geometry.
    /// A preset matches only if every one of its required `tags` entries
    /// is present on the feature (`"*"` means "key present, any value")
    /// and its `geometry` list includes the feature's geometry. Among
    /// matches, the preset with the most matched tag pairs wins; ties are
    /// broken by `match_score` descending.
    pub fn match_feature(&self, tags: &HashMap<String, String>, geometry: Geometry) -> Option<&Preset> {
        self.presets
            .iter()
            .filter(|p| p.geometry.contains(&geometry))
            .filter_map(|p| {
                let mut matched = 0usize;
                for (key, value) in &p.tags {
                    match tags.get(key) {
                        Some(actual) if value == "*" || actual == value => matched += 1,
                        _ => return None,
                    }
                }
                Some((matched, p))
            })
            .max_by(|(matched_a, preset_a), (matched_b, preset_b)| {
                matched_a
                    .cmp(matched_b)
                    .then(
                        preset_a
                            .match_score
                            .partial_cmp(&preset_b.match_score)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
            })
            .map(|(_, p)| p)
    }
```

Add the free function after the `impl PresetIndex` block:

```rust
/// Look up a friendly `(name, icon)` pair for a feature. Always returns a
/// name: falls back to the geometry's generic name (e.g. "Point") if
/// nothing in `index` matches at all, which should only happen if `index`
/// is missing the vendored schema's own geometry-only fallback presets
/// (e.g. in a minimal test fixture).
pub fn describe_feature(
    index: &PresetIndex,
    tags: &HashMap<String, String>,
    geometry: Geometry,
) -> (String, Option<String>) {
    match index.match_feature(tags, geometry) {
        Some(preset) => (preset.name.clone(), preset.icon.clone()),
        None => (geometry.fallback_name().to_string(), None),
    }
}
```

Add tests inside `mod tests`:

```rust
    const MATCH_FIXTURE: &str = r#"
    [
      {
        "id": "amenity/cafe",
        "name": "Cafe",
        "icon": "maki-cafe",
        "tags": {"amenity": "cafe"},
        "geometry": ["point", "vertex", "area"],
        "match_score": 1.0
      },
      {
        "id": "amenity/cafe/organic",
        "name": "Organic Cafe",
        "icon": "maki-cafe",
        "tags": {"amenity": "cafe", "organic": "only"},
        "geometry": ["point", "vertex", "area"],
        "match_score": 1.0
      },
      {
        "id": "building",
        "name": "Building",
        "icon": "maki-building",
        "tags": {"building": "*"},
        "geometry": ["area"],
        "match_score": 1.0
      },
      {
        "id": "point",
        "name": "Point",
        "tags": {},
        "geometry": ["point"],
        "match_score": 0.1
      },
      {
        "id": "area",
        "name": "Area",
        "tags": {},
        "geometry": ["area"],
        "match_score": 0.1
      }
    ]
    "#;

    #[test]
    fn match_feature_finds_exact_tag_match() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let tags = HashMap::from([("amenity".to_string(), "cafe".to_string())]);
        let preset = index.match_feature(&tags, Geometry::Point).unwrap();
        assert_eq!(preset.id, "amenity/cafe");
    }

    #[test]
    fn match_feature_prefers_more_specific_match() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let tags = HashMap::from([
            ("amenity".to_string(), "cafe".to_string()),
            ("organic".to_string(), "only".to_string()),
        ]);
        let preset = index.match_feature(&tags, Geometry::Point).unwrap();
        assert_eq!(preset.id, "amenity/cafe/organic");
    }

    #[test]
    fn match_feature_matches_wildcard_value() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let tags = HashMap::from([("building".to_string(), "house".to_string())]);
        let preset = index.match_feature(&tags, Geometry::Area).unwrap();
        assert_eq!(preset.id, "building");
    }

    #[test]
    fn match_feature_respects_geometry_filter() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        // "amenity/cafe" doesn't list "line" in its geometry, so a line
        // with the same tags should fall through to no match at all
        // (there's no generic "line" fallback in this fixture).
        let tags = HashMap::from([("amenity".to_string(), "cafe".to_string())]);
        assert!(index.match_feature(&tags, Geometry::Line).is_none());
    }

    #[test]
    fn match_feature_falls_back_to_generic_geometry_preset() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let tags = HashMap::from([("shop".to_string(), "bakery".to_string())]);
        let preset = index.match_feature(&tags, Geometry::Point).unwrap();
        assert_eq!(preset.id, "point");
    }

    #[test]
    fn describe_feature_returns_name_and_icon_on_match() {
        let index = PresetIndex::from_json(MATCH_FIXTURE).unwrap();
        let tags = HashMap::from([("amenity".to_string(), "cafe".to_string())]);
        let (name, icon) = describe_feature(&index, &tags, Geometry::Point);
        assert_eq!(name, "Cafe");
        assert_eq!(icon, Some("maki-cafe".to_string()));
    }

    #[test]
    fn describe_feature_falls_back_to_geometry_name_when_index_has_no_match() {
        let index = PresetIndex::from_json("[]").unwrap();
        let tags = HashMap::from([("shop".to_string(), "bakery".to_string())]);
        let (name, icon) = describe_feature(&index, &tags, Geometry::Vertex);
        assert_eq!(name, "Vertex");
        assert_eq!(icon, None);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus 7 new matching tests pass (20 total).

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Add geometry-aware preset matching and describe_feature"
```

---

### Task 5: `feature_geometry` on `EditableLayer` / `OsmLayer`

**Files:**
- Modify: `src/layers/mod.rs:111-136` (the `EditableLayer` trait, right after `feature_tags`)
- Modify: `src/layers/osm_layer.rs:1694` (add the impl right after `feature_tags`)

**Interfaces:**
- Consumes: `crate::presets::{classify_geometry, AreaKeys, Geometry}` (Task 3), `crate::selection::FeatureRef`
- Produces: `EditableLayer::feature_geometry(&self, feature: &FeatureRef, area_keys: &AreaKeys) -> Option<Geometry>`, implemented on `OsmLayer`.

- [ ] **Step 1: Write the failing test**

Add to the `EditableLayer` trait in `src/layers/mod.rs`, directly after the existing `feature_tags` method:

```rust
    /// Classify the geometry (point/vertex/line/area) of a feature this
    /// layer owns, for preset matching. Returns `None` if the feature
    /// doesn't belong to this layer or isn't found in its data.
    fn feature_geometry(
        &self,
        feature: &crate::selection::FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Option<crate::presets::Geometry>;
```

Add the implementation to `src/layers/osm_layer.rs`, directly after the existing `feature_tags` method (around line 1710):

```rust
    fn feature_geometry(
        &self,
        feature: &FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Option<crate::presets::Geometry> {
        if feature.layer_id != self.id {
            return None;
        }
        let data = self.osm_data.as_ref()?;
        crate::presets::classify_geometry(data, feature.kind, feature.id, area_keys)
    }
```

Add tests to the `mod tests` block in `src/layers/osm_layer.rs` (it already imports `EditableLayer`, `FeatureKind`, `OsmData`, `OsmNode`, `OsmWay`, and has a `data_with` helper — reuse them):

```rust
    #[test]
    fn feature_geometry_classifies_unreferenced_node_as_point() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 1,
        };
        assert_eq!(
            layer.feature_geometry(&feature, &area_keys),
            Some(crate::presets::Geometry::Point)
        );
    }

    #[test]
    fn feature_geometry_returns_none_for_wrong_layer() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(2),
            kind: FeatureKind::Node,
            id: 1,
        };
        assert_eq!(layer.feature_geometry(&feature, &area_keys), None);
    }

    #[test]
    fn feature_geometry_classifies_closed_area_way() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.0, version: 1, tags: empty_tags() };
        let mut tags = HashMap::new();
        tags.insert("building".to_string(), "yes".to_string());
        let way = OsmWay { id: 10, nodes: vec![1, 2, 1], version: 1, tags };
        let data = data_with(vec![n1, n2], vec![way]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json(r#"{"building": {}}"#).unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 10,
        };
        assert_eq!(
            layer.feature_geometry(&feature, &area_keys),
            Some(crate::presets::Geometry::Area)
        );
    }
```

- [ ] **Step 2: Run test to verify it fails first (trait method missing), then passes**

Run: `cargo test --lib layers::osm_layer::tests::feature_geometry -- --nocapture`
Expected first (before adding the trait method + impl): compile error `no method named feature_geometry`.
After adding both: 3 tests pass.

- [ ] **Step 3: Check the whole crate still builds** (the trait method is required on every `EditableLayer` implementor — `OsmLayer` is the only one per the trait's doc comment, so no other impls need updating)

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Add feature_geometry to EditableLayer/OsmLayer"
```

---

### Task 6: Vendor-update dev tool (`examples/update_presets.rs`)

**Files:**
- Create: `examples/update_presets.rs`

**Interfaces:**
- Consumes: `osm_gpui::http::{HttpRequest, UreqClient, fetch_with_retries, RetryPolicy}` (existing, used elsewhere for network calls), `osm_gpui::USER_AGENT`.
- Produces: on-disk `assets/presets/presets.json`, `assets/presets/area_keys.json`, `assets/presets/icons/*.svg`, `assets/presets/LICENSE` — consumed by Task 7 (running it) and Task 8 (embedding the output).

This is a manual dev tool (see Global Constraints) — not unit tested. Its steps are "write it, run it, inspect the output" rather than TDD.

- [ ] **Step 1: Write the tool**

```rust
//! Refreshes the vendored iD tagging schema data under `assets/presets/`.
//! Run manually with:
//!   cargo run --example update_presets
//!
//! Not part of CI or the app runtime — review the resulting git diff by hand
//! after running it.

use osm_gpui::http::{fetch_with_retries, HttpRequest, RetryPolicy, UreqClient};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

const PRESETS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/presets.json";
const AREA_KEYS_URL: &str =
    "https://cdn.jsdelivr.net/npm/@openstreetmap/id-tagging-schema/dist/areaKeys.json";
const MAKI_BASE: &str = "https://cdn.jsdelivr.net/npm/@mapbox/maki/icons";
const TEMAKI_BASE: &str = "https://cdn.jsdelivr.net/npm/@rapideditor/temaki/icons";

const OUT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets");

fn main() {
    let client = UreqClient::new();

    let presets_body = fetch(&client, PRESETS_URL);
    let area_keys_body = fetch(&client, AREA_KEYS_URL);

    let (trimmed_presets, icon_names) = trim_presets(&presets_body);

    fs::create_dir_all(OUT_DIR).expect("create assets/presets");
    fs::write(
        Path::new(OUT_DIR).join("presets.json"),
        serde_json::to_string_pretty(&trimmed_presets).unwrap(),
    )
    .expect("write presets.json");
    fs::write(Path::new(OUT_DIR).join("area_keys.json"), &area_keys_body)
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
/// build the jsDelivr URL for its SVG source.
fn icon_url(icon_name: &str) -> String {
    if let Some(name) = icon_name.strip_prefix("maki-") {
        format!("{}/{}.svg", MAKI_BASE, name)
    } else if let Some(name) = icon_name.strip_prefix("temaki-") {
        format!("{}/icon-{}.svg", TEMAKI_BASE, name)
    } else {
        // iD's own built-in icon names (used by some fallback/generic
        // presets) aren't fetchable from Maki/Temaki; skip them.
        format!("unsupported:{}", icon_name)
    }
}

/// Extract only the fields our `Preset` type keeps, from upstream's full
/// `presets.json` object-of-objects shape, and collect every referenced
/// icon name along the way.
fn trim_presets(body: &str) -> (Vec<Value>, HashSet<String>) {
    let root: Value = serde_json::from_str(body).expect("parse presets.json");
    let Some(obj) = root.as_object() else {
        panic!("presets.json root is not an object");
    };

    let mut out = Vec::new();
    let mut icon_names = HashSet::new();

    for (id, entry) in obj {
        let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
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
        trimmed.insert("name".to_string(), Value::String(name.to_string()));
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --example update_presets`
Expected: builds cleanly (no network call happens at build time — `main` only runs when executed).

- [ ] **Step 3: Commit**

```bash
git add examples/update_presets.rs
git commit -m "Add manual dev tool to refresh vendored preset/icon data"
```

---

### Task 7: Run the vendor-update tool and commit the vendored snapshot

**Files:**
- Create: `assets/presets/presets.json`, `assets/presets/area_keys.json`, `assets/presets/icons/*.svg`, `assets/presets/LICENSE` (generated by Task 6's tool, plus a hand-written LICENSE)

- [ ] **Step 1: Run the tool**

Run: `cargo run --example update_presets`
Expected output ends with a line like `done: <N> presets, <M> icons in <path>/assets/presets` where N is in the hundreds and M is in the low hundreds. If the network is unavailable in this environment, stop here and report that this task is blocked on network access — do not fabricate vendor content by hand.

- [ ] **Step 2: Spot-check the output**

Run: `python3 -c "import json; d=json.load(open('assets/presets/presets.json')); print(len(d)); print([p for p in d if p['id']=='amenity/cafe'])"`
Expected: prints a count in the hundreds, followed by a non-empty list containing the "Cafe" preset with `"tags": {"amenity": "cafe"}`.

Run: `ls assets/presets/icons | wc -l`
Expected: a nonzero count matching (or close to) the icon count printed by the tool.

- [ ] **Step 3: Add a LICENSE file for the vendored icon sets**

Create `assets/presets/LICENSE`:

```
This directory vendors data from two upstream projects:

- assets/presets/presets.json and assets/presets/area_keys.json are derived
  from the iD tagging schema (https://github.com/openstreetmap/id-tagging-schema),
  licensed under the ISC License.

- assets/presets/icons/maki-*.svg are from the Maki icon set
  (https://github.com/mapbox/maki), released under CC0 1.0 Universal.

- assets/presets/icons/temaki-*.svg are from the Temaki icon set
  (https://github.com/rapideditor/temaki), released under CC0 1.0 Universal.

Regenerate this directory with `cargo run --example update_presets`.
```

- [ ] **Step 4: Commit**

```bash
git add assets/presets/
git commit -m "Vendor iD tagging schema presets, area keys, and icons"
```

---

### Task 8: Global preset/area-key loader

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: `PresetIndex::from_json`, `AreaKeys::from_json` (Tasks 1, 2), the vendored files from Task 7.
- Produces: `pub fn preset_index() -> &'static PresetIndex`, `pub fn area_keys() -> &'static AreaKeys`

- [ ] **Step 1: Write the failing test**

Add near the top of `src/presets.rs`, after the existing `use` lines:

```rust
use std::sync::OnceLock;

const PRESETS_JSON: &str = include_str!("../assets/presets/presets.json");
const AREA_KEYS_JSON: &str = include_str!("../assets/presets/area_keys.json");

static PRESET_INDEX: OnceLock<PresetIndex> = OnceLock::new();
static AREA_KEYS: OnceLock<AreaKeys> = OnceLock::new();

/// The vendored preset index, parsed once on first access.
pub fn preset_index() -> &'static PresetIndex {
    PRESET_INDEX.get_or_init(|| {
        PresetIndex::from_json(PRESETS_JSON).expect("vendored assets/presets/presets.json must parse")
    })
}

/// The vendored area-key table, parsed once on first access.
pub fn area_keys() -> &'static AreaKeys {
    AREA_KEYS.get_or_init(|| {
        AreaKeys::from_json(AREA_KEYS_JSON).expect("vendored assets/presets/area_keys.json must parse")
    })
}
```

Add tests inside `mod tests`:

```rust
    #[test]
    fn vendored_preset_index_loads_and_contains_cafe() {
        let index = preset_index();
        assert!(index.len() > 100, "expected hundreds of vendored presets, got {}", index.len());
        let tags = HashMap::from([("amenity".to_string(), "cafe".to_string())]);
        let preset = index
            .match_feature(&tags, Geometry::Point)
            .expect("amenity=cafe should match a vendored preset");
        assert_eq!(preset.name, "Cafe");
    }

    #[test]
    fn vendored_area_keys_loads_and_treats_building_as_area() {
        let area_keys = area_keys();
        let tags = HashMap::from([("building".to_string(), "yes".to_string())]);
        assert!(area_keys.closed_way_is_area(&tags));
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus the 2 new loader tests pass (22 total). If `vendored_preset_index_loads_and_contains_cafe` fails because there's no `amenity/cafe`-equivalent preset in the vendored data, inspect `assets/presets/presets.json` for the actual matching preset id/name and adjust the assertion to match reality rather than changing the vendor data.

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Load vendored preset/area-key data via include_str!"
```

---

### Task 9: Icon path resolution

**Files:**
- Modify: `src/presets.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn icon_path(icon_name: &str) -> Option<std::path::PathBuf>` — returns `Some(path)` only if the SVG file actually exists on disk, so callers can skip rendering an icon element entirely rather than pointing GPUI at a missing file.

- [ ] **Step 1: Write the failing test**

Add to `src/presets.rs`:

```rust
const ICONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/presets/icons");

/// Resolve a preset's `icon` field (e.g. `"maki-cafe"`) to an absolute
/// filesystem path to its vendored SVG, if that file exists. Returns `None`
/// for icons we didn't vendor (e.g. iD's own built-in icon names, which
/// `examples/update_presets.rs` can't fetch from Maki/Temaki) so callers
/// render no icon rather than pointing GPUI at a missing path.
pub fn icon_path(icon_name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(ICONS_DIR).join(format!("{}.svg", icon_name));
    path.exists().then_some(path)
}
```

Add tests inside `mod tests`:

```rust
    #[test]
    fn icon_path_returns_some_for_vendored_icon() {
        // "maki-cafe" is the icon used by the vendored "Cafe" preset
        // (asserted to exist by vendored_preset_index_loads_and_contains_cafe
        // in the loader tests); its SVG should exist on disk too.
        assert!(icon_path("maki-cafe").is_some());
    }

    #[test]
    fn icon_path_returns_none_for_unknown_icon() {
        assert_eq!(icon_path("not-a-real-icon-xyz"), None);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib presets::tests -- --nocapture`
Expected: all previous tests plus 2 new icon-path tests pass (24 total). If `icon_path_returns_some_for_vendored_icon` fails, check `assets/presets/icons/` for the actual vendored file name for the Cafe preset's icon and adjust the test to use that real name.

- [ ] **Step 3: Commit**

```bash
git add src/presets.rs
git commit -m "Add icon path resolution for vendored preset icons"
```

---

### Task 10: Render friendly label + icon in the Selection section

**Files:**
- Modify: `src/side_panel.rs:175-221` (`render_selection_section`)

**Interfaces:**
- Consumes: `osm_gpui::presets::{preset_index, area_keys, describe_feature, icon_path}` (Tasks 4, 8, 9), `osm_gpui::layers::EditableLayer::{feature_tags, feature_geometry}` (Task 5, already-existing `feature_tags`).

- [ ] **Step 1: Write the failing test**

GPUI element trees aren't practically unit-testable in this codebase (no existing tests render `render_selection_section`'s output), so this task's "test" is the manual verification in Step 4. First, write a small pure-logic helper alongside the render function so at least the label/icon lookup itself is unit tested independent of GPUI. Add to `src/side_panel.rs`, above `render_selection_section`:

```rust
    /// Resolve the friendly `(name, icon_svg_path)` for a selected feature,
    /// or `None` if the feature's layer/tags/geometry can't be found (e.g.
    /// it was deleted since selection). `icon_svg_path` is `None` when the
    /// matched preset has no icon or the icon file isn't vendored.
    fn describe_selected_feature(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(String, Option<std::path::PathBuf>)> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let (name, icon_name) = osm_gpui::presets::describe_feature(
            osm_gpui::presets::preset_index(),
            &tags,
            geometry,
        );
        let icon_path = icon_name.and_then(|n| osm_gpui::presets::icon_path(&n));
        Some((name, icon_path))
    }
```

Add a test module at the bottom of `src/side_panel.rs` (create one if none exists yet — check the file first; if a `#[cfg(test)] mod tests` already exists, add to it instead of creating a second one):

```rust
#[cfg(test)]
mod preset_label_tests {
    use super::*;
    use osm_gpui::layers::{LayerId, LayerManager};
    use osm_gpui::layers::osm_layer::OsmLayer;
    use osm_gpui::osm::{OsmData, OsmNode};
    use osm_gpui::selection::{FeatureKind, FeatureRef};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn manager_with_cafe_node() -> (LayerManager, FeatureRef) {
        let mut manager = LayerManager::new();
        let layer_id = manager.alloc_id();
        let mut tags = HashMap::new();
        tags.insert("amenity".to_string(), "cafe".to_string());
        let node = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags };
        let data = Arc::new(OsmData {
            nodes: HashMap::from([(1, node)]),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: None,
        });
        let layer = OsmLayer::new_with_data(layer_id, "L", data);
        manager.add_layer(Box::new(layer));
        let feature = FeatureRef { layer_id, kind: FeatureKind::Node, id: 1 };
        (manager, feature)
    }

    // MapViewer::describe_selected_feature needs a full `MapViewer` (a GPUI
    // `Context`-bound struct), which isn't practical to construct outside a
    // running app. Test the same lookup path directly instead, exercising
    // exactly what describe_selected_feature does internally.
    #[test]
    fn cafe_node_resolves_to_cafe_label() {
        let (manager, feature) = manager_with_cafe_node();
        let layer = manager.find_layer(feature.layer_id).unwrap();
        let editable = layer.as_editable().unwrap();
        let tags: HashMap<String, String> = editable.feature_tags(&feature).unwrap().into_iter().collect();
        let geometry = editable
            .feature_geometry(&feature, osm_gpui::presets::area_keys())
            .unwrap();
        let (name, _icon) =
            osm_gpui::presets::describe_feature(osm_gpui::presets::preset_index(), &tags, geometry);
        assert_eq!(name, "Cafe");
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --lib side_panel::preset_label_tests -- --nocapture`
Expected: `cafe_node_resolves_to_cafe_label` passes. (If `LayerManager`/`LayerId`/`OsmLayer` aren't `pub` re-exported at those exact paths, check `src/lib.rs` and `src/layers/mod.rs` for their actual public paths and adjust the `use` lines — do not change visibility of unrelated items to make this compile.)

- [ ] **Step 3: Wire the render function**

Replace the body of `render_selection_section` in `src/side_panel.rs` (the `.children(self.selected.iter().enumerate().map(|(i, feat)| { ... }))` block) so each row includes the icon + friendly name ahead of the existing kind/id text:

```rust
            .children(self.selected.iter().enumerate().map(|(i, feat)| {
                let kind_label = match feat.kind {
                    FeatureKind::Node => "Node",
                    FeatureKind::Way => "Way",
                };
                let row_feat = *feat;
                let described = self.describe_selected_feature(feat);
                let row_text = match &described {
                    Some((name, _)) => format!("{} · {} {}", name, kind_label, feat.id),
                    None => format!("{} {}", kind_label, feat.id),
                };
                let icon_path = described.and_then(|(_, path)| path);

                let mut row = div()
                    .id(("selection-row", i))
                    .flex_shrink_0()
                    .h(px(Self::SELECTION_ROW_HEIGHT))
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|this| this.bg(cx.theme().accent));

                if let Some(path) = icon_path {
                    row = row.child(
                        gpui::svg()
                            .external_path(path.to_string_lossy().to_string())
                            .size(px(14.0))
                            .text_color(cx.theme().foreground),
                    );
                }

                row.child(row_text).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                        this.selected = vec![row_feat];
                        cx.notify();
                    }),
                )
            }))
```

- [ ] **Step 4: Manual verification**

Run: `cargo run --release`, open an OSM XML file containing a node tagged `amenity=cafe` (or create one — a minimal 2-line `<osm><node id="1" lat="40" lon="-74"><tag k="amenity" v="cafe"/></node></osm>` saved as a `.osm` file works via File > Open), click the node, and confirm the Selection section shows `Cafe · Node 1` (with an icon glyph to its left if `maki-cafe.svg` vendored correctly). Then select a plain untagged node and confirm it falls back to showing `Point · Node <id>` or just `Node <id>` per whichever the actual vendored fallback presets produce — note the exact wording seen in the manual test.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: all tests pass, no regressions in other modules.

- [ ] **Step 6: Commit**

```bash
git add src/side_panel.rs
git commit -m "Show matched preset name and icon in Selection rows"
```

---

## Self-Review Notes

- **Spec coverage:** vendored data layout (Tasks 6-7), update tooling (Task 6), geometry classification (Task 3), matching algorithm (Task 4), UI integration (Task 10), testing (unit tests in every task) — all covered. Non-goals from the spec (no search dialog, no NSI unification, no Tags-section display, no map-canvas icons, no locationSet filtering) are respected by construction — no task touches those areas.
- **Type consistency:** `Geometry`, `Preset`, `PresetIndex`, `AreaKeys` are defined once in Task 1/2 and referenced identically (same names, same method signatures) through Tasks 3-10. `FeatureKind`/`FeatureRef`/`OsmData`/`OsmNode`/`OsmWay` are all pre-existing types used as-is, not redefined.
- **Known risk:** Task 7 depends on live network access to jsDelivr; if that's unavailable in the execution environment, Task 7 (and everything downstream that needs real vendored content — Tasks 8, 9, 10's manual verification) is blocked. Tasks 1-6 do not depend on network access and can proceed independently.
