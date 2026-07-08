//! iD tagging schema field support: vendored field definitions (text/combo/
//! check/radio/multiCombo widgets) for the presets in `crate::presets`.
//! Read-only, vendored the same way presets are — see
//! `docs/superpowers/specs/2026-07-07-preset-fields-editor-design.md`.

use std::collections::HashMap;
use std::sync::OnceLock;

const FIELDS_JSON: &str = include_str!("../assets/presets/fields.json");

static FIELD_INDEX: OnceLock<FieldIndex> = OnceLock::new();

/// The vendored field index, parsed once on first access.
pub fn field_index() -> &'static FieldIndex {
    FIELD_INDEX.get_or_init(|| {
        FieldIndex::from_json(FIELDS_JSON).expect("vendored assets/presets/fields.json must parse")
    })
}

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

    #[test]
    fn vendored_field_index_loads_and_contains_website_field() {
        let index = field_index();
        assert!(
            index.len() > 5,
            "expected several vendored fields, got {}",
            index.len()
        );
        let website = index
            .get("website")
            .expect("a 'website' field should be vendored");
        assert_eq!(website.field_type, FieldType::Text);
    }
}
