//! One read-only surface over the preset, field, translation and brand
//! (NSI) indexes, in the terms the UI asks its questions in. Every method
//! delegates to the existing index; nothing here caches or decides matching.

use std::collections::{HashMap, HashSet};

use crate::fields::{Field, FieldIndex, FieldOption, Widget};
use crate::nsi::NsiEntry;
use crate::preset_recents::Section;
use crate::preset_translations::PresetTranslations;
use crate::presets::{Geometry, Preset, PresetIndex};
use crate::selection::FeatureKind;
use crate::tag_suggest::SchemaIndex;

/// A field with the widget that edits it and the label to show for it.
#[derive(Debug, Clone, Copy)]
pub struct FieldView<'a> {
    pub field: &'a Field,
    pub widget: Widget,
    pub label: &'a str,
}

/// The fields of one feature: the ones to draw, then the ones the "Add field"
/// list offers, each in display order.
#[derive(Debug, Default)]
pub struct FeatureFields<'a> {
    pub shown: Vec<FieldView<'a>>,
    pub addable: Vec<FieldView<'a>>,
}

#[derive(Clone, Copy)]
pub struct Catalogue<'a> {
    presets: &'a PresetIndex,
    fields: &'a FieldIndex,
    translations: Option<&'a PresetTranslations>,
}

impl Catalogue<'static> {
    /// The bundled schema in the UI language this run uses.
    pub fn bundled() -> Self {
        Self::new(
            crate::presets::preset_index(),
            crate::fields::field_index(),
            crate::preset_translations::active(),
        )
    }
}

impl<'a> Catalogue<'a> {
    pub fn new(
        presets: &'a PresetIndex,
        fields: &'a FieldIndex,
        translations: Option<&'a PresetTranslations>,
    ) -> Self {
        Self {
            presets,
            fields,
            translations,
        }
    }

    /// A preset by its exact id.
    pub fn preset(&self, id: &str) -> Option<&'a Preset> {
        self.presets.get(id)
    }

    /// The preset that best matches a feature's tags and geometry kind.
    pub fn match_preset(
        &self,
        tags: &HashMap<String, String>,
        geometry: Geometry,
    ) -> Option<&'a Preset> {
        self.presets.match_feature(tags, geometry)
    }

    /// The preset's name in the UI language (English where untranslated).
    pub fn preset_name<'p>(&self, preset: &'p Preset) -> &'p str
    where
        'a: 'p,
    {
        preset.display_name(self.translations)
    }

    /// The `(name, icon)` of a feature's preset. A relation of a kind the
    /// schema has no preset for matches only the catch-all "Relation", whose
    /// name says less than the relation's own `type` tag, so that wins.
    pub fn describe(
        &self,
        tags: &HashMap<String, String>,
        geometry: Geometry,
        kind: FeatureKind,
    ) -> (String, Option<String>) {
        let (mut name, icon) =
            crate::presets::describe_feature_in(self.translations, self.presets, tags, geometry);
        if kind == FeatureKind::Relation
            && self
                .match_preset(tags, geometry)
                .is_none_or(|p| p.id == "relation")
        {
            if let Some(kind) = tags.get("type").filter(|t| !t.trim().is_empty()) {
                name = kind.clone();
            }
        }
        (name, icon)
    }

    /// The fields to draw for a feature matched to `preset`, and the ones its
    /// "Add field" list offers. The preset's field lists were resolved
    /// (references expanded, parent fields inherited) when the index loaded.
    pub fn fields_for(
        &self,
        preset: &Preset,
        geometry: Geometry,
        tags: &HashMap<String, String>,
        promoted: &HashSet<String>,
    ) -> FeatureFields<'a> {
        let resolved = self.fields.for_feature(preset, geometry, tags, promoted);
        let view = |field: &'a Field| FieldView {
            field,
            widget: field.widget(),
            label: self.field_label(field),
        };
        FeatureFields {
            shown: resolved.shown.into_iter().map(view).collect(),
            addable: resolved.addable.into_iter().map(view).collect(),
        }
    }

    /// A field's label: translated when there is a translation, else English.
    pub fn field_label<'f>(&self, field: &'f Field) -> &'f str
    where
        'a: 'f,
    {
        field.display_label(self.translations)
    }

    pub fn field_placeholder<'f>(&self, field: &'f Field) -> Option<&'f str>
    where
        'a: 'f,
    {
        field.display_placeholder(self.translations)
    }

    /// One option's label; its `value` (the tag value) is never translated.
    pub fn option_label<'f>(&self, field: &'f Field, option: &'f FieldOption) -> &'f str
    where
        'a: 'f,
    {
        field.display_option_label(option, self.translations)
    }

    /// The translated label of option `value` of field `field_id`, if any.
    pub fn translated_option_label(&self, field_id: &str, value: &str) -> Option<&'a str> {
        self.translations?.option_label(field_id, value)
    }

    /// The per-key rows of a [`Widget::MultiKey`] field.
    pub fn sub_fields(&self, field: &Field, tags: &HashMap<String, String>) -> Vec<Field> {
        field.sub_fields(tags, self.translations)
    }

    /// Presets for the picker's search box: every match of `geometries[0]`,
    /// then those of the later geometries not already listed (a closed way
    /// is offered area presets first, then line ones).
    pub fn search_presets(
        &self,
        query: &str,
        geometries: &[Geometry],
        limit: usize,
    ) -> Vec<&'a Preset> {
        self.presets
            .search_geometries_in(self.translations, query, geometries, limit)
    }

    /// The picker's list before anything is typed: recents, then the common
    /// presets of each geometry.
    pub fn default_presets(
        &self,
        recents: &[String],
        geometries: &[Geometry],
    ) -> Vec<(Section, &'a Preset)> {
        crate::preset_recents::default_list(self.presets, recents, geometries)
    }

    /// Whether the brand (NSI) index has been downloaded yet.
    pub fn brands_loaded(&self) -> bool {
        crate::nsi::current().is_some()
    }

    /// Brand entries matching `query`; empty until the index has loaded.
    pub fn search_brands(&self, query: &str, limit: usize) -> Vec<NsiEntry> {
        let Some(index) = crate::nsi::current() else {
            return Vec::new();
        };
        index.search(query, limit).into_iter().cloned().collect()
    }

    /// The keys and values the schema knows, for tag suggestions.
    pub fn schema_index(&self) -> SchemaIndex {
        SchemaIndex::build(self.presets, self.fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESETS: &str = r#"[
      {"id":"building","name":"Building","tags":{"building":"*"},"geometry":["area"],
       "fields":["name","levels"]},
      {"id":"building/house","name":"House","tags":{"building":"house"},"geometry":["area"]},
      {"id":"highway/residential","name":"Residential Road","tags":{"highway":"residential"},
       "geometry":["line"],"fields":["name"]},
      {"id":"area/highway","name":"Road Area","tags":{"highway":"residential","area":"yes"},
       "geometry":["area"]},
      {"id":"relation","name":"Relation","tags":{},"geometry":["relation"],"match_score":0.1},
      {"id":"type/route","name":"Route","tags":{"type":"route"},"geometry":["relation"]}
    ]"#;
    const FIELDS: &str = r#"[
      {"id":"name","key":"name","field_type":"localized","label":"Name","universal":true},
      {"id":"levels","key":"building:levels","field_type":"number","label":"Levels"},
      {"id":"surface","key":"surface","field_type":"combo","label":"Surface",
       "options":[{"value":"asphalt","label":"Asphalt"}]}
    ]"#;
    const DE: &str = r#"{"presets":{"building":{"name":"Gebäude"}},
      "fields":{"name":{"label":"Name (de)"}}}"#;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn fields_follow_the_preset_inherit_from_parent_and_carry_widget_and_label() {
        let presets = PresetIndex::from_json(PRESETS).unwrap();
        let fields = FieldIndex::from_json(FIELDS).unwrap();
        let cat = Catalogue::new(&presets, &fields, None);
        // `building/house` lists no fields, so it takes `building`'s.
        let house = cat.match_preset(&tags(&[("building", "house")]), Geometry::Area);
        assert_eq!(house.unwrap().id, "building/house");
        let got = cat.fields_for(
            house.unwrap(),
            Geometry::Area,
            &tags(&[("building", "house")]),
            &HashSet::new(),
        );
        let shown: Vec<_> = got
            .shown
            .iter()
            .map(|v| (v.field.id.as_str(), v.widget, v.label))
            .collect();
        assert_eq!(
            shown,
            vec![("name", Widget::Text, "Name"), ("levels", Widget::Text, "Levels")]
        );
        assert!(got.addable.is_empty());
    }

    #[test]
    fn labels_are_translated_with_an_english_fallback() {
        let presets = PresetIndex::from_json(PRESETS).unwrap();
        let fields = FieldIndex::from_json(FIELDS).unwrap();
        let de = PresetTranslations::from_json(DE).unwrap();
        let cat = Catalogue::new(&presets, &fields, Some(&de));
        let building = cat.preset("building").unwrap();
        assert_eq!(cat.preset_name(building), "Gebäude");
        let house = cat.preset("building/house").unwrap();
        assert_eq!(cat.preset_name(house), "House");
        let got = cat.fields_for(building, Geometry::Area, &HashMap::new(), &HashSet::new());
        let labels: Vec<_> = got.shown.iter().map(|v| v.label).collect();
        assert_eq!(labels, vec!["Name (de)", "Levels"]);
        let surface = fields.get("surface").unwrap();
        assert_eq!(cat.option_label(surface, &surface.options[0]), "Asphalt");
        assert_eq!(cat.translated_option_label("surface", "asphalt"), None);
    }

    #[test]
    fn a_closed_way_is_offered_area_presets_before_line_presets() {
        let presets = PresetIndex::from_json(PRESETS).unwrap();
        let fields = FieldIndex::from_json(FIELDS).unwrap();
        let cat = Catalogue::new(&presets, &fields, None);
        let ids = |geoms: &[Geometry]| -> Vec<String> {
            cat.search_presets("", geoms, 10)
                .iter()
                .map(|p| p.id.clone())
                .collect()
        };
        assert_eq!(
            ids(&[Geometry::Area, Geometry::Line]),
            ["building", "building/house", "area/highway", "highway/residential"]
        );
        assert_eq!(ids(&[Geometry::Line, Geometry::Area])[0], "highway/residential");
    }

    #[test]
    fn closed_way_preset_follows_the_chosen_geometry() {
        let presets = PresetIndex::from_json(PRESETS).unwrap();
        let fields = FieldIndex::from_json(FIELDS).unwrap();
        let cat = Catalogue::new(&presets, &fields, None);
        let road = tags(&[("highway", "residential")]);
        assert_eq!(
            cat.match_preset(&road, Geometry::Line).unwrap().id,
            "highway/residential"
        );
        let area = tags(&[("highway", "residential"), ("area", "yes")]);
        assert_eq!(
            cat.match_preset(&area, Geometry::Area).unwrap().id,
            "area/highway"
        );
    }

    #[test]
    fn describe_names_an_unknown_relation_by_its_type_tag() {
        let presets = PresetIndex::from_json(PRESETS).unwrap();
        let fields = FieldIndex::from_json(FIELDS).unwrap();
        let cat = Catalogue::new(&presets, &fields, None);
        let (name, _) = cat.describe(
            &tags(&[("type", "boundary")]),
            Geometry::Relation,
            FeatureKind::Relation,
        );
        assert_eq!(name, "boundary");
        let (name, _) = cat.describe(
            &tags(&[("type", "route")]),
            Geometry::Relation,
            FeatureKind::Relation,
        );
        assert_eq!(name, "Route");
        let (name, _) = cat.describe(
            &tags(&[("type", "boundary")]),
            Geometry::Relation,
            FeatureKind::Way,
        );
        assert_eq!(name, "Relation");
    }

    #[test]
    fn bundled_catalogue_describes_a_cafe() {
        let cat = Catalogue::bundled();
        let (name, icon) = cat.describe(
            &tags(&[("amenity", "cafe")]),
            Geometry::Point,
            FeatureKind::Node,
        );
        assert_eq!(name, "Cafe");
        assert!(icon.is_some());
        assert!(cat.schema_index().keys().contains(&"amenity".to_string()));
    }
}
