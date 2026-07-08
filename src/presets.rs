//! iD tagging schema preset support: vendored preset/area-key data,
//! geometry classification, and geometry-aware tag matching. Read-only
//! (no editing UI) — mirrors the shape of `nsi.rs` but with vendored
//! rather than runtime-fetched data. See
//! `docs/superpowers/specs/2026-07-07-id-preset-labels-design.md`.

use std::collections::HashMap;
use crate::osm::OsmData;
use crate::selection::FeatureKind;

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
}
