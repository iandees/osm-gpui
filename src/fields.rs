//! iD tagging schema field support: the vendored field definitions behind the
//! presets in `crate::presets`, and the rules that decide which of them a
//! feature shows. Vendored the same way presets are.
//!
//! The schema describes fields with a few indirections, all resolved once
//! when the index loads (never per frame):
//! - a field's `label`/`placeholder`/options may be a reference `{other_field}`
//!   (or name one in `stringsCrossReference`), meaning "use that field's
//!   strings" — for the English text and for every translation alike;
//! - a field may be limited to some geometries, shown only when another tag is
//!   present or absent (`prerequisiteTag`), or be `universal` (offered for
//!   every feature in "Add field").
//!
//! Which fields a *preset* lists (`{preset_id}` references, inheritance from
//! the parent preset) is resolved in `crate::presets`.

use crate::preset_translations::PresetTranslations;
use crate::presets::{Geometry, Preset};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const FIELDS_JSON: &str = include_str!("../assets/presets/fields.json");

static FIELD_INDEX: OnceLock<FieldIndex> = OnceLock::new();

/// The vendored field index, parsed once on first access.
pub fn field_index() -> &'static FieldIndex {
    FIELD_INDEX.get_or_init(|| {
        FieldIndex::from_json(FIELDS_JSON).expect("vendored assets/presets/fields.json must parse")
    })
}

/// A field's type as the schema names it. `Other` is any type this version of
/// the app does not know; it is edited as plain text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    #[default]
    Text,
    Number,
    Url,
    Tel,
    Email,
    Textarea,
    Identifier,
    Colour,
    Date,
    Localized,
    Wikipedia,
    Wikidata,
    Roadspeed,
    Roadheight,
    Combo,
    TypeCombo,
    SemiCombo,
    MultiCombo,
    ManyCombo,
    NetworkCombo,
    DirectionalCombo,
    Check,
    DefaultCheck,
    OnewayCheck,
    Radio,
    StructureRadio,
    Access,
    Address,
    Restrictions,
    #[serde(other)]
    Other,
}

/// The editing control the Fields section uses for a field. Every
/// [`FieldType`] maps to one of these; types without a dedicated control of
/// their own degrade to [`Widget::Text`] (see [`Field::widget`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    /// One text input for the field's key.
    Text,
    /// A select box over the field's static options for one key.
    Combo,
    /// A checkbox: `yes` when ticked, tag removed when cleared.
    Check,
    /// A radio group over the static options for one key.
    Radio,
    /// A `;`-separated list of the field's static options in one key.
    SemiCombo,
    /// Chips of the options set as `<key><option>=yes` (key ends with `:`).
    PrefixMulti,
    /// Several keys, each edited by its own simple control
    /// ([`Field::sub_fields`]).
    MultiKey,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct FieldOption {
    pub value: String,
    #[serde(default)]
    pub label: String,
}

/// `prerequisiteTag`: the field is only available when this holds.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrerequisiteTag {
    pub key: Option<String>,
    pub key_not: Option<String>,
    pub value: Option<String>,
    pub value_not: Option<String>,
}

impl PrerequisiteTag {
    /// Same rules as iD's `field.isAllowed`: `key` needs the tag present (and
    /// equal to `value`, or different from `valueNot`, when given); `keyNot`
    /// needs the tag absent.
    pub fn is_met(&self, tags: &HashMap<String, String>) -> bool {
        if let Some(key) = &self.key {
            let Some(actual) = tags.get(key).filter(|v| !v.is_empty()) else {
                return false;
            };
            if let Some(not) = &self.value_not {
                return actual != not;
            }
            if let Some(value) = &self.value {
                return actual == value;
            }
            return true;
        }
        if let Some(key_not) = &self.key_not {
            return tags.get(key_not).is_none_or(|v| v.is_empty());
        }
        true
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Field {
    pub id: String,
    /// The field's main tag key; empty for a field that only has `keys`.
    #[serde(default)]
    pub key: String,
    /// Every key of a multi-key field (access, address, cycleway, ...).
    #[serde(default)]
    pub keys: Vec<String>,
    pub field_type: FieldType,
    /// English label, with `{other_field}` references already resolved.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<FieldOption>,
    /// English labels of the sub-keys of a multi-key field.
    #[serde(default)]
    pub types: HashMap<String, String>,
    /// Geometries the field applies to; empty means all.
    #[serde(default)]
    pub geometry: Vec<Geometry>,
    #[serde(default)]
    pub prerequisite_tag: Option<PrerequisiteTag>,
    #[serde(default)]
    pub universal: bool,
    #[serde(default)]
    pub strings_cross_reference: Option<String>,
    /// The field whose translations supply this one's strings: the end of the
    /// `stringsCrossReference` chain, or the field itself. Set on load.
    #[serde(skip)]
    pub strings_id: String,
    /// Set on the synthetic sub-fields of a multi-key field, whose `label` is
    /// already final (not looked up in translations).
    #[serde(skip)]
    pub literal_label: bool,
}

/// `{name}` -> `name`.
fn reference_target(s: &str) -> Option<&str> {
    s.strip_prefix('{')?.strip_suffix('}')
}

/// Whether `s` is a `{reference}` rather than text to show.
pub fn is_reference(s: &str) -> bool {
    reference_target(s).is_some()
}

/// `addr:house_number` -> `House number`: the last key segment as words.
fn humanize_key(key: &str) -> String {
    let last = key.rsplit(':').next().unwrap_or(key).trim_end_matches('=');
    let words = last.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The address keys the fallback editor offers (plus any other `addr:*` key
/// the feature already has).
const COMMON_ADDRESS_KEYS: &[&str] = &[
    "addr:housenumber",
    "addr:street",
    "addr:city",
    "addr:postcode",
];

impl Field {
    /// The tag keys this field reads and writes.
    pub fn all_keys(&self) -> Vec<&str> {
        if !self.keys.is_empty() {
            self.keys.iter().map(String::as_str).collect()
        } else if !self.key.is_empty() {
            vec![self.key.as_str()]
        } else {
            Vec::new()
        }
    }

    /// Whether the app can edit the field at all: it needs a key. (The one
    /// keyless field is `restrictions`, which edits a relation's members.)
    pub fn is_editable(&self) -> bool {
        self.field_type != FieldType::Restrictions
            && (!self.key.is_empty() || !self.keys.is_empty())
    }

    /// The control to edit the field with. Anything without a dedicated
    /// control (and any combo with no static options to pick from) falls back
    /// to a plain text input for the field's main key, so no field is
    /// dropped for lacking a renderer.
    pub fn widget(&self) -> Widget {
        let has_options = !self.options.is_empty();
        let text_unless = |has: bool, widget: Widget| if has { widget } else { Widget::Text };
        match self.field_type {
            FieldType::Check | FieldType::DefaultCheck => Widget::Check,
            FieldType::Radio => text_unless(has_options, Widget::Radio),
            FieldType::Combo
            | FieldType::TypeCombo
            | FieldType::ManyCombo
            | FieldType::NetworkCombo
            | FieldType::OnewayCheck => text_unless(has_options, Widget::Combo),
            FieldType::SemiCombo => text_unless(has_options, Widget::SemiCombo),
            FieldType::MultiCombo if self.key.ends_with(':') => {
                text_unless(has_options, Widget::PrefixMulti)
            }
            FieldType::MultiCombo => text_unless(has_options, Widget::SemiCombo),
            FieldType::Access | FieldType::StructureRadio | FieldType::Address => Widget::MultiKey,
            FieldType::DirectionalCombo => text_unless(!self.keys.is_empty(), Widget::MultiKey),
            FieldType::Text
            | FieldType::Number
            | FieldType::Url
            | FieldType::Tel
            | FieldType::Email
            | FieldType::Textarea
            | FieldType::Identifier
            | FieldType::Colour
            | FieldType::Date
            | FieldType::Localized
            | FieldType::Wikipedia
            | FieldType::Wikidata
            | FieldType::Roadspeed
            | FieldType::Roadheight
            | FieldType::Restrictions
            | FieldType::Other => Widget::Text,
        }
    }

    /// Whether the feature has a value for any of this field's keys.
    pub fn has_value(&self, tags: &HashMap<String, String>) -> bool {
        match self.widget() {
            Widget::PrefixMulti => tags.keys().any(|k| k.starts_with(&self.key)),
            _ if self.field_type == FieldType::Address => {
                self.keys.iter().any(|k| tags.contains_key(k))
            }
            _ => self.all_keys().iter().any(|k| tags.contains_key(*k)),
        }
    }

    /// Whether the field may be shown for a feature of `geometry` with
    /// `tags`: editable, within its own geometry limit, prerequisite met.
    pub fn is_allowed(&self, geometry: Geometry, tags: &HashMap<String, String>) -> bool {
        self.is_editable()
            && (self.geometry.is_empty() || self.geometry.contains(&geometry))
            && self
                .prerequisite_tag
                .as_ref()
                .is_none_or(|p| p.is_met(tags))
    }

    /// The single-key fields a [`Widget::MultiKey`] field is edited as, one
    /// row per key. Not elaborate: `access` becomes a select per key,
    /// `structureRadio` a checkbox per key, `address` a text input per common
    /// key (plus any other `addr:*` key already set), a directional combo a
    /// select per side.
    pub fn sub_fields(
        &self,
        tags: &HashMap<String, String>,
        translations: Option<&PresetTranslations>,
    ) -> Vec<Field> {
        let keys: Vec<&String> = if self.field_type == FieldType::Address {
            self.keys
                .iter()
                .filter(|k| COMMON_ADDRESS_KEYS.contains(&k.as_str()) || tags.contains_key(*k))
                .collect()
        } else {
            self.keys.iter().collect()
        };
        keys.into_iter()
            .map(|key| {
                let (field_type, label) = match self.field_type {
                    FieldType::StructureRadio => {
                        let label = self
                            .options
                            .iter()
                            .find(|o| &o.value == key)
                            .map(|o| self.display_option_label(o, translations).to_string())
                            .unwrap_or_else(|| humanize_key(key));
                        (FieldType::Check, label)
                    }
                    FieldType::Address => (FieldType::Text, humanize_key(key)),
                    _ => {
                        let label = translations
                            .and_then(|t| t.field_type_label(&self.strings_id, key))
                            .or_else(|| self.types.get(key).map(String::as_str))
                            .map(str::to_string)
                            .unwrap_or_else(|| humanize_key(key));
                        let field_type = if self.options.is_empty() {
                            FieldType::Text
                        } else {
                            FieldType::Combo
                        };
                        (field_type, label)
                    }
                };
                Field {
                    id: format!("{}#{}", self.id, key),
                    key: key.clone(),
                    field_type,
                    label,
                    options: if field_type == FieldType::Combo {
                        self.options.clone()
                    } else {
                        Vec::new()
                    },
                    strings_id: self.strings_id.clone(),
                    literal_label: true,
                    ..Field::default()
                }
            })
            .collect()
    }

    /// The label to show: translated when `translations` has one, else the
    /// English schema label. The lookups borrow, so they don't allocate.
    pub fn display_label<'a>(&'a self, translations: Option<&'a PresetTranslations>) -> &'a str {
        if self.literal_label {
            return &self.label;
        }
        translations
            .and_then(|t| {
                t.field_label(&self.id)
                    .or_else(|| t.field_label(&self.strings_id))
            })
            .unwrap_or(&self.label)
    }

    /// The placeholder to show, translated where possible.
    pub fn display_placeholder<'a>(
        &'a self,
        translations: Option<&'a PresetTranslations>,
    ) -> Option<&'a str> {
        translations
            .and_then(|t| {
                t.field_placeholder(&self.id)
                    .or_else(|| t.field_placeholder(&self.strings_id))
            })
            .or(self.placeholder.as_deref())
    }

    /// The label of one of this field's options, translated where possible.
    /// The option's `value` (the tag value) is never changed.
    pub fn display_option_label<'a>(
        &'a self,
        option: &'a FieldOption,
        translations: Option<&'a PresetTranslations>,
    ) -> &'a str {
        translations
            .and_then(|t| {
                t.option_label(&self.id, &option.value)
                    .or_else(|| t.option_label(&self.strings_id, &option.value))
            })
            .unwrap_or(&option.label)
    }
}

/// A parsed, in-memory collection of vendored `Field`s, keyed by id, with all
/// cross-references between them resolved.
pub struct FieldIndex {
    fields: HashMap<String, Field>,
    /// Ids in vendored order.
    order: Vec<String>,
    /// Ids of the `universal` fields, in vendored order.
    universal: Vec<String>,
}

impl FieldIndex {
    /// Parse a JSON array of `Field` (the vendored `assets/presets/fields.json`
    /// shape, or a small fixture array in tests) and resolve the references
    /// between fields' strings.
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        let list: Vec<Field> = serde_json::from_str(body)?;
        let order: Vec<String> = list.iter().map(|f| f.id.clone()).collect();
        let mut fields: HashMap<String, Field> =
            list.into_iter().map(|f| (f.id.clone(), f)).collect();
        resolve_string_references(&mut fields, &order);
        let universal = order
            .iter()
            .filter(|id| fields[*id].universal)
            .cloned()
            .collect();
        Ok(Self {
            fields,
            order,
            universal,
        })
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

    pub fn iter(&self) -> impl Iterator<Item = &Field> {
        self.order.iter().filter_map(|id| self.fields.get(id))
    }

    /// The `universal` fields (name, note, website, ...), which every feature
    /// can add whatever its preset says.
    pub fn universal(&self) -> impl Iterator<Item = &Field> {
        self.universal.iter().filter_map(|id| self.fields.get(id))
    }

    /// The fields to show for a feature matched to `preset`, and the ones its
    /// "Add field" list offers.
    ///
    /// `shown`: the preset's `fields` that are allowed for this geometry and
    /// tag set, then any other allowed field (the preset's `more_fields` or a
    /// universal one) the feature already has a value for, then the
    /// `promoted` ones the user asked for. `addable`: the remaining allowed
    /// `more_fields` followed by the remaining universal fields. A field is
    /// never in both lists or twice in one.
    pub fn for_feature(
        &self,
        preset: &Preset,
        geometry: Geometry,
        tags: &HashMap<String, String>,
        promoted: &HashSet<String>,
    ) -> ResolvedFields<'_> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut shown: Vec<&Field> = Vec::new();
        for id in &preset.fields {
            if let Some(f) = self.get(id) {
                if f.is_allowed(geometry, tags) && seen.insert(f.id.as_str()) {
                    shown.push(f);
                }
            }
        }
        let optional: Vec<&Field> = preset
            .more_fields
            .iter()
            .filter_map(|id| self.get(id))
            .chain(self.universal())
            .collect();
        let mut addable: Vec<&Field> = Vec::new();
        for f in optional {
            if !f.is_allowed(geometry, tags) || seen.contains(f.id.as_str()) {
                continue;
            }
            seen.insert(f.id.as_str());
            if f.has_value(tags) || promoted.contains(&f.id) {
                shown.push(f);
            } else {
                addable.push(f);
            }
        }
        ResolvedFields { shown, addable }
    }
}

/// What [`FieldIndex::for_feature`] decided.
pub struct ResolvedFields<'a> {
    pub shown: Vec<&'a Field>,
    pub addable: Vec<&'a Field>,
}

/// Follow `stringsCrossReference` / `{field}` labels from `id` to the end:
/// the chain of fields whose strings may supply `id`'s, nearest first.
fn strings_chain<'a>(fields: &'a HashMap<String, Field>, id: &'a str) -> Vec<&'a Field> {
    let mut chain: Vec<&Field> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut current = id;
    while let Some(field) = fields.get(current) {
        if !seen.insert(field.id.as_str()) {
            break; // a reference cycle: stop where it closes
        }
        chain.push(field);
        let next = field
            .strings_cross_reference
            .as_deref()
            .and_then(reference_target)
            .or_else(|| reference_target(&field.label));
        match next {
            Some(next) => current = next,
            None => break,
        }
    }
    chain
}

/// Resolve every `{field}` reference in labels, placeholders and option
/// labels, fill in options a field only borrows from the field it
/// cross-references, and set each field's `strings_id`.
fn resolve_string_references(fields: &mut HashMap<String, Field>, order: &[String]) {
    struct Resolved {
        id: String,
        strings_id: String,
        label: String,
        placeholder: Option<String>,
        options: Vec<FieldOption>,
        types: HashMap<String, String>,
    }
    let usable = |s: &str| !s.is_empty() && !is_reference(s);
    let mut resolved: Vec<Resolved> = Vec::with_capacity(order.len());
    for id in order {
        let chain = strings_chain(fields, id);
        let own = chain[0];
        let label = chain
            .iter()
            .map(|f| f.label.as_str())
            .find(|l| usable(l))
            .map(str::to_string)
            .unwrap_or_else(|| humanize_key(&own.id));
        let placeholder = chain
            .iter()
            .filter_map(|f| f.placeholder.as_deref())
            .find(|p| usable(p))
            .map(str::to_string);
        // A field with no options of its own offers its reference's.
        let option_source: &[FieldOption] = chain
            .iter()
            .map(|f| f.options.as_slice())
            .find(|o| !o.is_empty())
            .unwrap_or(&[]);
        let mut options: Vec<FieldOption> = option_source
            .iter()
            .map(|opt| {
                let label = chain
                    .iter()
                    .filter_map(|f| f.options.iter().find(|o| o.value == opt.value))
                    .map(|o| o.label.as_str())
                    .find(|l| usable(l))
                    .map(str::to_string)
                    .unwrap_or_else(|| opt.value.replace('_', " "));
                FieldOption {
                    value: opt.value.clone(),
                    label,
                }
            })
            .collect();
        if own.field_type == FieldType::OnewayCheck {
            options.retain(|o| o.value != "undefined");
        }
        let types = chain
            .iter()
            .map(|f| &f.types)
            .find(|t| !t.is_empty())
            .cloned()
            .unwrap_or_default();
        resolved.push(Resolved {
            id: id.clone(),
            strings_id: chain.last().map(|f| f.id.clone()).unwrap_or_default(),
            label,
            placeholder,
            options,
            types,
        });
    }
    for r in resolved {
        if let Some(f) = fields.get_mut(&r.id) {
            f.strings_id = r.strings_id;
            f.label = r.label;
            f.placeholder = r.placeholder;
            f.options = r.options;
            f.types = r.types;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::preset_index;

    /// Every `FieldType` with the widget it gets without options, with
    /// options, with a `:`-suffixed key, and with `keys` set (in that order).
    #[test]
    fn widget_table_pins_every_field_type() {
        use FieldType as T;
        use Widget as W;
        let table: &[(T, W, W, W, W)] = &[
            (T::Text, W::Text, W::Text, W::Text, W::Text),
            (T::Number, W::Text, W::Text, W::Text, W::Text),
            (T::Url, W::Text, W::Text, W::Text, W::Text),
            (T::Tel, W::Text, W::Text, W::Text, W::Text),
            (T::Email, W::Text, W::Text, W::Text, W::Text),
            (T::Textarea, W::Text, W::Text, W::Text, W::Text),
            (T::Identifier, W::Text, W::Text, W::Text, W::Text),
            (T::Colour, W::Text, W::Text, W::Text, W::Text),
            (T::Date, W::Text, W::Text, W::Text, W::Text),
            (T::Localized, W::Text, W::Text, W::Text, W::Text),
            (T::Wikipedia, W::Text, W::Text, W::Text, W::Text),
            (T::Wikidata, W::Text, W::Text, W::Text, W::Text),
            (T::Roadspeed, W::Text, W::Text, W::Text, W::Text),
            (T::Roadheight, W::Text, W::Text, W::Text, W::Text),
            (T::Combo, W::Text, W::Combo, W::Combo, W::Text),
            (T::TypeCombo, W::Text, W::Combo, W::Combo, W::Text),
            (T::SemiCombo, W::Text, W::SemiCombo, W::SemiCombo, W::Text),
            (
                T::MultiCombo,
                W::Text,
                W::SemiCombo,
                W::PrefixMulti,
                W::Text,
            ),
            (T::ManyCombo, W::Text, W::Combo, W::Combo, W::Text),
            (T::NetworkCombo, W::Text, W::Combo, W::Combo, W::Text),
            (T::DirectionalCombo, W::Text, W::Text, W::Text, W::MultiKey),
            (T::Check, W::Check, W::Check, W::Check, W::Check),
            (T::DefaultCheck, W::Check, W::Check, W::Check, W::Check),
            (T::OnewayCheck, W::Text, W::Combo, W::Combo, W::Text),
            (T::Radio, W::Text, W::Radio, W::Radio, W::Text),
            (
                T::StructureRadio,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
            ),
            (
                T::Access,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
            ),
            (
                T::Address,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
                W::MultiKey,
            ),
            (T::Restrictions, W::Text, W::Text, W::Text, W::Text),
            (T::Other, W::Text, W::Text, W::Text, W::Text),
        ];
        let make = |ft: T, options: bool, key: &str, keys: bool| Field {
            field_type: ft,
            key: key.to_string(),
            keys: if keys {
                vec!["a".to_string()]
            } else {
                Vec::new()
            },
            options: if options {
                vec![FieldOption {
                    value: "v".into(),
                    label: String::new(),
                }]
            } else {
                Vec::new()
            },
            ..Field::default()
        };
        for (ft, bare, opts, prefix, multi) in table {
            assert_eq!(make(*ft, false, "k", false).widget(), *bare, "{ft:?} bare");
            assert_eq!(
                make(*ft, true, "k", false).widget(),
                *opts,
                "{ft:?} options"
            );
            assert_eq!(
                make(*ft, true, "k:", false).widget(),
                *prefix,
                "{ft:?} prefix"
            );
            assert_eq!(make(*ft, false, "k", true).widget(), *multi, "{ft:?} keys");
        }
    }

    const FIXTURE: &str = r#"
    [
      {
        "id": "name",
        "key": "name",
        "field_type": "localized",
        "label": "Name",
        "placeholder": "Common name",
        "universal": true
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
      },
      {
        "id": "leaf_cycle",
        "key": "leaf_cycle",
        "field_type": "combo",
        "label": "Leaf Cycle",
        "placeholder": "Cycle",
        "options": [
          {"value": "evergreen", "label": "Evergreen"},
          {"value": "deciduous", "label": "Deciduous"},
          {"value": "mixed", "label": "Mixed"}
        ]
      },
      {
        "id": "leaf_cycle_singular",
        "key": "leaf_cycle",
        "field_type": "combo",
        "label": "{leaf_cycle}",
        "strings_cross_reference": "{leaf_cycle}",
        "options": [
          {"value": "evergreen", "label": ""},
          {"value": "deciduous", "label": ""}
        ]
      },
      {"id": "loop_a", "key": "a", "field_type": "text", "label": "{loop_b}"},
      {"id": "loop_b", "key": "b", "field_type": "text", "label": "{loop_a}"},
      {"id": "dangling", "key": "d", "field_type": "text", "label": "{no_such_field}"},
      {
        "id": "tunnel/name",
        "key": "tunnel:name",
        "field_type": "text",
        "label": "Tunnel Name",
        "prerequisite_tag": {"key": "tunnel"}
      },
      {
        "id": "not_building",
        "key": "x",
        "field_type": "text",
        "label": "Not a building",
        "prerequisite_tag": {"keyNot": "building"}
      },
      {
        "id": "toll",
        "key": "toll",
        "field_type": "check",
        "label": "Toll",
        "prerequisite_tag": {"key": "highway", "valueNot": "footway"}
      },
      {
        "id": "lines_only",
        "key": "l",
        "field_type": "text",
        "label": "Lines only",
        "geometry": ["line"]
      },
      {"id": "note", "key": "note", "field_type": "textarea", "label": "Note", "universal": true},
      {
        "id": "access",
        "keys": ["access", "foot"],
        "field_type": "access",
        "label": "Allowed Access",
        "types": {"access": "All", "foot": "Foot"},
        "options": [{"value": "yes", "label": "Allowed"}, {"value": "no", "label": "Prohibited"}]
      },
      {"id": "restrictions", "field_type": "restrictions", "label": "Turn Restrictions"},
      {"id": "future", "key": "f", "field_type": "somethingNew", "label": "Future"}
    ]
    "#;

    const PRESETS: &str = r#"
    [
      {"id": "amenity/cafe", "name": "Cafe", "tags": {"amenity": "cafe"}, "geometry": ["point"],
       "fields": ["name", "tunnel/name", "toll", "lines_only", "restrictions"],
       "more_fields": ["cuisine", "not_building"]}
    ]
    "#;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn ids(fields: &[&Field]) -> Vec<String> {
        fields.iter().map(|f| f.id.clone()).collect()
    }

    #[test]
    fn from_json_parses_all_entries() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        assert_eq!(index.len(), 15);
    }

    #[test]
    fn get_finds_field_by_id() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let name = index.get("name").unwrap();
        assert_eq!(name.key, "name");
        assert_eq!(name.field_type, FieldType::Localized);
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
    fn label_reference_takes_the_referenced_fields_strings() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let singular = index.get("leaf_cycle_singular").unwrap();
        assert_eq!(singular.label, "Leaf Cycle");
        assert_eq!(singular.placeholder.as_deref(), Some("Cycle"));
        assert_eq!(singular.strings_id, "leaf_cycle");
        // Its own, shorter option list, with the referenced field's labels.
        let labels: Vec<_> = singular.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Evergreen", "Deciduous"]);
    }

    #[test]
    fn reference_cycles_and_dangling_references_terminate_without_braces() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        for id in ["loop_a", "loop_b", "dangling"] {
            let label = &index.get(id).unwrap().label;
            assert!(!label.contains('{'), "{id}: {label}");
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn translated_reference_labels_follow_the_referenced_field() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let singular = index.get("leaf_cycle_singular").unwrap();
        // The translation of the referencing field is empty or itself a
        // reference; the referenced field's translation supplies the text.
        let tr = PresetTranslations::from_json(
            r#"{"fields": {
                "leaf_cycle_singular": {"label": "{leaf_cycle}"},
                "leaf_cycle": {"label": "Laubwechsel", "placeholder": "Zyklus",
                               "options": {"evergreen": "Immergrün"}}}}"#,
        )
        .unwrap();
        assert_eq!(singular.display_label(Some(&tr)), "Laubwechsel");
        assert_eq!(singular.display_placeholder(Some(&tr)), Some("Zyklus"));
        let evergreen = &singular.options[0];
        assert_eq!(
            singular.display_option_label(evergreen, Some(&tr)),
            "Immergrün"
        );
        // An option the translation lacks stays English.
        let deciduous = &singular.options[1];
        assert_eq!(
            singular.display_option_label(deciduous, Some(&tr)),
            "Deciduous"
        );
    }

    #[test]
    fn prerequisite_tags() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let g = Geometry::Line;
        let tunnel_name = index.get("tunnel/name").unwrap();
        assert!(!tunnel_name.is_allowed(g, &tags(&[])));
        assert!(tunnel_name.is_allowed(g, &tags(&[("tunnel", "yes")])));
        let not_building = index.get("not_building").unwrap();
        assert!(not_building.is_allowed(g, &tags(&[("amenity", "cafe")])));
        assert!(!not_building.is_allowed(g, &tags(&[("building", "yes")])));
        let toll = index.get("toll").unwrap();
        assert!(!toll.is_allowed(g, &tags(&[])));
        assert!(toll.is_allowed(g, &tags(&[("highway", "primary")])));
        assert!(!toll.is_allowed(g, &tags(&[("highway", "footway")])));
    }

    #[test]
    fn field_geometry_limits_where_it_is_allowed() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let lines_only = index.get("lines_only").unwrap();
        assert!(lines_only.is_allowed(Geometry::Line, &tags(&[])));
        assert!(!lines_only.is_allowed(Geometry::Area, &tags(&[])));
        assert!(!lines_only.is_allowed(Geometry::Point, &tags(&[])));
    }

    #[test]
    fn for_feature_filters_orders_and_offers_universal_fields() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let presets = crate::presets::PresetIndex::from_json(PRESETS).unwrap();
        let cafe = presets.get("amenity/cafe").unwrap();
        let none = HashSet::new();

        // tunnel/name needs a tunnel, toll a highway, lines_only a line, and
        // restrictions has no key to edit: only name remains. Add field:
        // the preset's more_fields, then the universal ones.
        let r = index.for_feature(cafe, Geometry::Point, &tags(&[]), &none);
        assert_eq!(ids(&r.shown), ["name"]);
        assert_eq!(ids(&r.addable), ["cuisine", "not_building", "note"]);

        // A more field with a value is shown without being asked for, and
        // leaves the Add field list; so does a promoted one.
        let with_note = tags(&[("note", "hi"), ("tunnel", "yes")]);
        let r = index.for_feature(cafe, Geometry::Point, &with_note, &none);
        assert_eq!(ids(&r.shown), ["name", "tunnel/name", "note"]);
        assert_eq!(ids(&r.addable), ["cuisine", "not_building"]);
        let promoted = HashSet::from(["cuisine".to_string()]);
        let r = index.for_feature(cafe, Geometry::Point, &tags(&[]), &promoted);
        assert_eq!(ids(&r.shown), ["name", "cuisine"]);

        // On a line, the line-only field appears.
        let r = index.for_feature(cafe, Geometry::Line, &tags(&[]), &none);
        assert_eq!(ids(&r.shown), ["name", "lines_only"]);
    }

    #[test]
    fn every_field_type_has_an_editing_control() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        // The name field is `localized`; with no dedicated control it is a
        // plain text input for `name`.
        assert_eq!(index.get("name").unwrap().widget(), Widget::Text);
        assert_eq!(index.get("cuisine").unwrap().widget(), Widget::Combo);
        assert_eq!(index.get("toll").unwrap().widget(), Widget::Check);
        assert_eq!(index.get("note").unwrap().widget(), Widget::Text);
        assert_eq!(index.get("access").unwrap().widget(), Widget::MultiKey);
        // An unknown type degrades to text, but is still editable.
        let future = index.get("future").unwrap();
        assert_eq!(future.field_type, FieldType::Other);
        assert_eq!(future.widget(), Widget::Text);
        assert!(future.is_editable());
        // A combo with nothing to pick from is text too.
        let mut bare = index.get("cuisine").unwrap().clone();
        bare.options.clear();
        assert_eq!(bare.widget(), Widget::Text);
        // Only `restrictions` has nothing to edit.
        assert!(!index.get("restrictions").unwrap().is_editable());
    }

    #[test]
    fn multi_key_fields_become_one_simple_row_per_key() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let access = index.get("access").unwrap();
        let rows = access.sub_fields(&tags(&[]), None);
        let summary: Vec<_> = rows
            .iter()
            .map(|r| (r.key.as_str(), r.label.as_str(), r.widget()))
            .collect();
        assert_eq!(
            summary,
            [
                ("access", "All", Widget::Combo),
                ("foot", "Foot", Widget::Combo)
            ]
        );
        assert_eq!(rows[0].options.len(), 2);
    }

    #[test]
    fn german_label_placeholder_and_options() {
        let de = crate::preset_translations::translations_for("de");
        let index = field_index();
        let surface = index.get("surface").unwrap();
        assert_eq!(surface.label, "Surface");
        assert_eq!(surface.display_label(de), "Oberfläche");
        let gravel = surface
            .options
            .iter()
            .find(|o| o.value == "gravel")
            .unwrap();
        assert_eq!(gravel.label, "Gravel");
        assert_eq!(surface.display_option_label(gravel, de), "Grober Schotter");
        // The tag value is untouched.
        assert_eq!(gravel.value, "gravel");
        let oneway = index.get("oneway").unwrap();
        assert_eq!(oneway.display_label(de), "Einbahnstraße");
        let image = index.get("image").unwrap();
        assert_eq!(
            image.placeholder.as_deref(),
            Some("https://example.com/photo.jpg")
        );
        assert_eq!(
            image.display_placeholder(de),
            Some("https://beispiel.net/foto.jpg")
        );
    }

    #[test]
    fn missing_translations_fall_back_to_english() {
        let index = FieldIndex::from_json(FIXTURE).unwrap();
        let cuisine = index.get("cuisine").unwrap();
        let name = index.get("name").unwrap();
        // No locale at all, an unknown locale, and a locale that lacks the field.
        let unknown = crate::preset_translations::translations_for("tlh");
        let sparse =
            PresetTranslations::from_json(r#"{"fields":{"pizza":{"label":"x"}}}"#).unwrap();
        for tr in [None, unknown, Some(&sparse)] {
            assert_eq!(cuisine.display_label(tr), "Cuisine");
            assert_eq!(name.display_placeholder(tr), Some("Common name"));
            assert_eq!(
                cuisine.display_option_label(&cuisine.options[0], tr),
                "Coffee Shop"
            );
        }
        // An English placeholder survives when only the label is translated.
        let label_only =
            PresetTranslations::from_json(r#"{"fields":{"name":{"label":"Nom"}}}"#).unwrap();
        assert_eq!(name.display_label(Some(&label_only)), "Nom");
        assert_eq!(
            name.display_placeholder(Some(&label_only)),
            Some("Common name")
        );
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
        assert_eq!(website.field_type, FieldType::Url);
        assert_eq!(website.widget(), Widget::Text);
    }

    /// The fields shown and offered for a feature, as the side panel would.
    struct Shown {
        preset: String,
        shown: Vec<String>,
        addable: Vec<String>,
    }

    fn resolve(pairs: &[(&str, &str)], geometry: Geometry) -> Shown {
        let tags = tags(pairs);
        let preset = preset_index()
            .match_feature(&tags, geometry)
            .unwrap_or_else(|| panic!("no preset for {pairs:?} as {geometry:?}"));
        let r = field_index().for_feature(preset, geometry, &tags, &HashSet::new());
        Shown {
            preset: preset.id.clone(),
            shown: ids(&r.shown),
            addable: ids(&r.addable),
        }
    }

    fn assert_shows(case: &str, pairs: &[(&str, &str)], geometry: Geometry, expected: &[&str]) {
        let got = resolve(pairs, geometry);
        for want in expected {
            assert!(
                got.shown.iter().any(|id| id == want),
                "{case}: preset {} should show `{want}`; shows {:?}",
                got.preset,
                got.shown
            );
        }
        // Every field of the list has a real label, in both languages.
        let de = crate::preset_translations::translations_for("de");
        for id in got.shown.iter().chain(&got.addable) {
            let f = field_index().get(id).unwrap();
            for tr in [None, de] {
                assert!(
                    !f.display_label(tr).is_empty() && !f.display_label(tr).contains('{'),
                    "{case}: field {id} label {:?}",
                    f.display_label(tr)
                );
            }
        }
    }

    #[test]
    fn roads_show_name_and_the_road_fields() {
        let line = Geometry::Line;
        let road = [
            "name",
            "oneway",
            "maxspeed",
            "surface",
            "structure",
            "access",
        ];
        for (value, extra) in [
            ("residential", &["lanes"][..]),
            ("tertiary", &["lanes", "ref_road_number"][..]),
            ("primary", &["lanes", "ref_road_number"][..]),
            ("service", &["service"][..]),
        ] {
            let mut expected = road.to_vec();
            expected.extend_from_slice(extra);
            assert_shows(
                value,
                &[("highway", value), ("name", "Fixture Street")],
                line,
                &expected,
            );
        }
        assert_shows(
            "footway",
            &[("highway", "footway")],
            line,
            &["name", "surface", "width", "structure", "access", "incline"],
        );
        assert_shows(
            "track",
            &[("highway", "track")],
            line,
            &[
                "name",
                "tracktype",
                "surface",
                "width",
                "structure",
                "access",
            ],
        );
        let tertiary = resolve(&[("highway", "tertiary")], line);
        assert_eq!(tertiary.preset, "highway/tertiary");
        assert!(!tertiary.shown.is_empty());
    }

    #[test]
    fn a_tunnel_name_appears_only_on_a_tunnel() {
        let plain = resolve(&[("highway", "tertiary")], Geometry::Line);
        assert!(!plain.shown.iter().any(|id| id == "tunnel/name"));
        let tunnel = resolve(
            &[("highway", "tertiary"), ("tunnel", "yes")],
            Geometry::Line,
        );
        assert!(tunnel.shown.iter().any(|id| id == "tunnel/name"));
    }

    #[test]
    fn buildings_and_places_show_their_fields() {
        let area = Geometry::Area;
        let point = Geometry::Point;
        assert_shows(
            "building=yes",
            &[("building", "yes")],
            area,
            &["name", "building", "building/levels", "height", "address"],
        );
        assert_shows(
            "building=house",
            &[("building", "house")],
            area,
            &["name", "building/levels", "address"],
        );
        let cafe = [
            "name",
            "cuisine",
            "address",
            "opening_hours",
            "phone",
            "website",
        ];
        assert_shows("cafe node", &[("amenity", "cafe")], point, &cafe);
        assert_shows("cafe area", &[("amenity", "cafe")], area, &cafe);
        let restaurant = ["name", "cuisine", "address", "opening_hours"];
        assert_shows(
            "restaurant node",
            &[("amenity", "restaurant")],
            point,
            &restaurant,
        );
        assert_shows(
            "restaurant area",
            &[("amenity", "restaurant")],
            area,
            &restaurant,
        );
        let shop = ["name", "address", "opening_hours"];
        assert_shows("supermarket node", &[("shop", "supermarket")], point, &shop);
        assert_shows("supermarket area", &[("shop", "supermarket")], area, &shop);
        assert_shows("city", &[("place", "city")], point, &["name", "population"]);
    }

    #[test]
    fn natural_and_landuse_features_show_name() {
        assert_shows("water", &[("natural", "water")], Geometry::Area, &["name"]);
        assert_shows(
            "residential landuse",
            &[("landuse", "residential")],
            Geometry::Area,
            &["name"],
        );
        assert_shows("rail", &[("railway", "rail")], Geometry::Line, &["name"]);
        assert_shows("river", &[("waterway", "river")], Geometry::Line, &["name"]);
        assert_shows(
            "fence",
            &[("barrier", "fence")],
            Geometry::Line,
            &["fence_type"],
        );
    }

    #[test]
    fn a_tree_shows_leaf_fields_with_real_labels() {
        let tree = resolve(&[("natural", "tree")], Geometry::Point);
        assert_eq!(tree.preset, "natural/tree");
        for want in ["leaf_type_singular", "leaf_cycle_singular", "height"] {
            assert!(tree.shown.iter().any(|id| id == want), "{:?}", tree.shown);
        }
        let index = field_index();
        let cycle = index.get("leaf_cycle_singular").unwrap();
        assert_eq!(cycle.label, "Leaf Cycle");
        let de = crate::preset_translations::translations_for("de");
        // Its own translation is empty; the referenced field's is used.
        let german = de.unwrap().field_label("leaf_cycle").unwrap();
        assert_ne!(german, "Leaf Cycle");
        assert_eq!(cycle.display_label(de), german);
        let evergreen = cycle.options.iter().find(|o| o.value == "evergreen");
        assert_eq!(evergreen.unwrap().label, "Evergreen");
    }

    #[test]
    fn relations_and_untagged_features_show_fields() {
        assert_shows(
            "bus route",
            &[("type", "route"), ("route", "bus")],
            Geometry::Relation,
            &["name", "ref_route", "operator", "network", "from", "to"],
        );
        assert_shows(
            "multipolygon",
            &[("type", "multipolygon")],
            Geometry::Relation,
            &[],
        );
        assert_shows("untagged way", &[], Geometry::Line, &["name"]);
        assert_shows("untagged node", &[], Geometry::Point, &["name"]);
    }

    #[test]
    fn name_is_offered_to_every_geometry() {
        for g in [
            Geometry::Point,
            Geometry::Vertex,
            Geometry::Line,
            Geometry::Area,
            Geometry::Relation,
        ] {
            // (An untagged closed way is not an area to iD without area=yes.)
            let pairs: &[(&str, &str)] = if g == Geometry::Area {
                &[("area", "yes")]
            } else {
                &[]
            };
            let r = resolve(pairs, g);
            assert!(
                r.shown.iter().chain(&r.addable).any(|id| id == "name"),
                "{g:?}: name neither shown nor offered ({})",
                r.preset
            );
        }
    }

    /// The two lists of every preset in the schema, resolved: nothing left
    /// unresolved, every id real, no preset empty for want of inheritance.
    #[test]
    fn every_bundled_preset_resolves_to_real_fields() {
        let presets = preset_index();
        let fields = field_index();
        let mut braces = Vec::new();
        let mut dangling = Vec::new();
        let mut no_key = std::collections::BTreeSet::new();
        let mut zero = Vec::new();
        let mut total = 0;
        for preset in presets.iter().chain(presets.templates()) {
            total += 1;
            for id in preset.fields.iter().chain(&preset.more_fields) {
                if id.contains('{') || id.contains('}') {
                    braces.push(format!("{}: {id}", preset.id));
                } else if let Some(f) = fields.get(id) {
                    if !f.is_editable() {
                        no_key.insert(id.clone());
                    }
                } else {
                    dangling.push(format!("{}: {id}", preset.id));
                }
            }
            if preset.fields.is_empty() {
                zero.push(preset.id.clone());
            }
        }
        assert!(braces.is_empty(), "unresolved references: {braces:?}");
        assert!(dangling.is_empty(), "unknown field ids: {dangling:?}");
        // Every field a preset lists can be edited (the one keyless field,
        // `restrictions`, is listed by no preset).
        assert!(no_key.is_empty(), "fields with no key: {no_key:?}");
        // Presets with no fields at all after resolution: those the schema
        // defines without any, and no ancestor to inherit from.
        let mut ancestorless_children = Vec::new();
        for id in &zero {
            let preset = presets.get(id).unwrap();
            let mut ancestor = id.as_str();
            while let Some(slash) = ancestor.rfind('/') {
                ancestor = &ancestor[..slash];
                if let Some(parent) = presets.get(ancestor) {
                    let shares = parent.geometry.iter().any(|g| preset.geometry.contains(g));
                    if shares && !parent.fields.is_empty() {
                        ancestorless_children.push(id.clone());
                    }
                }
            }
        }
        assert!(
            ancestorless_children.is_empty(),
            "empty despite a parent with fields: {ancestorless_children:?}"
        );
        println!(
            "{} of {total} presets resolve to zero fields: {zero:?}",
            zero.len()
        );
        // The schema defines these with neither `fields` nor an ancestor to
        // inherit from (the `@templates` are only ever referenced for their
        // `moreFields`). Any other preset appearing here is a regression.
        zero.retain(|id| !id.starts_with('@'));
        zero.sort();
        assert_eq!(
            zero,
            [
                "disc_golf/basket",
                "embankment",
                "ford_line",
                "golf/lateral_water_hazard",
                "golf/water_hazard",
                "man_made/geoglyph_multilinestring",
                "noexit/yes",
                "type/multipolygon"
            ],
            "presets with no fields (out of {total}): the schema's own, expected"
        );
    }

    /// `cargo test --release load_time -- --ignored --nocapture`
    #[test]
    #[ignore = "timing only"]
    fn load_time() {
        let presets = include_str!("../assets/presets/presets.json");
        let fields = include_str!("../assets/presets/fields.json");
        let t = std::time::Instant::now();
        let f = FieldIndex::from_json(fields).unwrap();
        let t_fields = t.elapsed();
        let t = std::time::Instant::now();
        let p = crate::presets::PresetIndex::from_json(presets).unwrap();
        println!(
            "fields {t_fields:?} ({} fields), presets {:?} ({} presets)",
            f.len(),
            t.elapsed(),
            p.len()
        );
    }

    #[test]
    fn every_bundled_field_has_resolved_strings_in_english_and_german() {
        let fields = field_index();
        let de = crate::preset_translations::translations_for("de");
        let mut offenders = Vec::new();
        for f in fields.iter() {
            if fields.get(&f.strings_id).is_none() {
                offenders.push(format!("{}: strings_id {}", f.id, f.strings_id));
            }
            for tr in [None, de] {
                let mut texts = vec![f.display_label(tr).to_string()];
                texts.extend(f.display_placeholder(tr).map(str::to_string));
                texts.extend(
                    f.options
                        .iter()
                        .map(|o| f.display_option_label(o, tr).to_string()),
                );
                texts.extend(f.types.values().cloned());
                for t in texts {
                    if t.is_empty() || t.contains('{') || t.contains('}') {
                        offenders.push(format!("{}: {t:?}", f.id));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "unresolved strings: {offenders:#?}");
    }
}
