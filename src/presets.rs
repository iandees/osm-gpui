//! iD tagging schema preset support: vendored preset/area-key data,
//! geometry classification, and geometry-aware tag matching. Read-only
//! (no editing UI) — mirrors the shape of `nsi.rs` but with vendored
//! rather than runtime-fetched data. See
//! `docs/superpowers/specs/2026-07-07-id-preset-labels-design.md`.

use std::collections::HashMap;
use std::sync::OnceLock;
use crate::osm::OsmData;
use crate::selection::FeatureKind;

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
}

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

#[cfg(test)]
mod tests {
    use super::*;
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
}
