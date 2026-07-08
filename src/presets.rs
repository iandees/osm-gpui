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
}
