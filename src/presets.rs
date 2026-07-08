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
