//! Autocomplete for tag keys and values in the side panel's Tags table.
//!
//! Pure, UI-free logic. Suggestions come from three sources that are merged
//! and ranked here:
//!
//! 1. the loaded data (`DataIndex`: how often each key and each key/value
//!    pair is used in a layer, built once and kept up to date incrementally),
//! 2. the bundled iD tagging schema (`SchemaIndex`: keys and values the
//!    presets and fields know about, which works on an empty layer), and
//! 3. taginfo (`crate::taginfo`), which arrives later and is appended below
//!    everything local so the list never reshuffles under the user.
//!
//! Ranking, per list: prefix matches before substring matches; then values
//! present in the current selection, then the loaded data (most used first),
//! then the schema, then taginfo (in taginfo's own order, by usage).

use crate::fields::{FieldIndex, FieldType};
use crate::osm::OsmData;
use crate::presets::{Geometry, PresetIndex};
use crate::taginfo::TaginfoItem;
use std::collections::{HashMap, HashSet};

/// Use counts of the keys and the key/value pairs of one layer.
///
/// Counts are per element: a key's count is the number of nodes, ways and
/// relations that carry it, a value's the number that carry that exact pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataIndex {
    keys: HashMap<String, u32>,
    values: HashMap<String, HashMap<String, u32>>,
}

impl DataIndex {
    /// Count every tag of every element of `data`. One pass; meant to run
    /// once per layer load (and again after a bulk change), off the UI thread.
    pub fn from_osm(data: &OsmData) -> Self {
        let mut index = DataIndex::default();
        let tag_sets = data
            .nodes
            .values()
            .map(|n| &n.tags)
            .chain(data.ways.values().map(|w| &w.tags))
            .chain(data.relations.iter().map(|r| &r.tags));
        for tags in tag_sets {
            for (k, v) in tags {
                index.add_tag(k, v);
            }
        }
        index
    }

    /// Count one more element carrying `key=value`.
    pub fn add_tag(&mut self, key: &str, value: &str) {
        bump(&mut self.keys, key, 1);
        match self.values.get_mut(key) {
            Some(values) => bump(values, value, 1),
            None => {
                let mut values = HashMap::new();
                values.insert(value.to_string(), 1);
                self.values.insert(key.to_string(), values);
            }
        }
    }

    /// Count one fewer element carrying `key=value` (no-op if it is unknown).
    pub fn remove_tag(&mut self, key: &str, value: &str) {
        if !self.keys.contains_key(key) {
            return;
        }
        drop_one(&mut self.keys, key);
        if let Some(values) = self.values.get_mut(key) {
            drop_one(values, value);
            if values.is_empty() {
                self.values.remove(key);
            }
        }
    }

    /// Apply the edit of one element's tag `key` from `before` to `after`
    /// (`None` = absent), as recorded by an undo step.
    pub fn apply_change(&mut self, key: &str, before: Option<&str>, after: Option<&str>) {
        if before == after {
            return;
        }
        if let Some(v) = before {
            self.remove_tag(key, v);
        }
        if let Some(v) = after {
            self.add_tag(key, v);
        }
    }

    /// How many elements carry `key`.
    pub fn key_count(&self, key: &str) -> u32 {
        self.keys.get(key).copied().unwrap_or(0)
    }

    pub fn keys(&self) -> impl Iterator<Item = (&str, u32)> {
        self.keys.iter().map(|(k, c)| (k.as_str(), *c))
    }

    /// The values used with `key`, with their counts.
    pub fn values_of(&self, key: &str) -> impl Iterator<Item = (&str, u32)> {
        self.values
            .get(key)
            .into_iter()
            .flat_map(|m| m.iter().map(|(v, c)| (v.as_str(), *c)))
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn bump(map: &mut HashMap<String, u32>, key: &str, by: u32) {
    match map.get_mut(key) {
        Some(c) => *c += by,
        None => {
            map.insert(key.to_string(), by);
        }
    }
}

fn drop_one(map: &mut HashMap<String, u32>, key: &str) {
    if let Some(c) = map.get_mut(key) {
        if *c <= 1 {
            map.remove(key);
        } else {
            *c -= 1;
        }
    }
}

/// A value the tagging schema knows for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaValue {
    pub value: String,
    /// The English label of the field option, if the schema has one.
    pub label: Option<String>,
    /// The field the option comes from, so the UI can look up a translated
    /// label (`PresetTranslations::option_label`).
    pub field_id: Option<String>,
}

/// The keys and values the bundled presets and fields mention.
#[derive(Debug, Clone, Default)]
pub struct SchemaIndex {
    keys: Vec<String>,
    values: HashMap<String, Vec<SchemaValue>>,
    /// Per key used as a preset tag: the geometries of the presets that use
    /// it (`building` is only an area's tag, for example). Keys no preset
    /// uses as a tag (`name`, `addr:*`) are not listed and fit anything.
    key_geometries: HashMap<String, Vec<Geometry>>,
}

impl SchemaIndex {
    /// Collect every key of a preset's tags or a field's key, and per key
    /// the options of its fields and the values of the preset tags. Built
    /// once at startup.
    pub fn build(presets: &PresetIndex, fields: &FieldIndex) -> Self {
        let mut keys: HashSet<String> = HashSet::new();
        let mut values: HashMap<String, Vec<SchemaValue>> = HashMap::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut key_geometries: HashMap<String, Vec<Geometry>> = HashMap::new();

        let mut push =
            |key: &str, sv: SchemaValue, values: &mut HashMap<String, Vec<SchemaValue>>| {
                if seen.insert((key.to_string(), sv.value.clone())) {
                    values.entry(key.to_string()).or_default().push(sv);
                }
            };

        // Fields in id order so the result does not depend on hash order.
        let mut field_list: Vec<_> = fields.iter().collect();
        field_list.sort_by(|a, b| a.id.cmp(&b.id));
        for field in field_list {
            if !is_plain_key(&field.key) {
                continue;
            }
            keys.insert(field.key.clone());
            for opt in &field.options {
                if opt.value.is_empty() {
                    continue;
                }
                let label =
                    (!opt.label.is_empty() && opt.label != opt.value).then(|| opt.label.clone());
                push(
                    &field.key,
                    SchemaValue {
                        value: opt.value.clone(),
                        label,
                        field_id: Some(field.id.clone()),
                    },
                    &mut values,
                );
            }
            if field.field_type == FieldType::Check {
                for v in ["yes", "no"] {
                    push(
                        &field.key,
                        SchemaValue {
                            value: v.to_string(),
                            label: None,
                            field_id: None,
                        },
                        &mut values,
                    );
                }
            }
        }
        for preset in presets.iter() {
            for (k, v) in &preset.tags {
                if !is_plain_key(k) {
                    continue;
                }
                keys.insert(k.clone());
                let geometries = key_geometries.entry(k.clone()).or_default();
                for g in &preset.geometry {
                    if !geometries.contains(g) {
                        geometries.push(*g);
                    }
                }
                if v != "*" && !v.is_empty() {
                    push(
                        k,
                        SchemaValue {
                            value: v.clone(),
                            label: None,
                            field_id: None,
                        },
                        &mut values,
                    );
                }
            }
        }
        let mut keys: Vec<String> = keys.into_iter().collect();
        keys.sort();
        SchemaIndex {
            keys,
            values,
            key_geometries,
        }
    }

    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// Whether `key` makes sense on every one of the features whose
    /// possible geometries are `features` (a closed way can be a line or an
    /// area, so each feature lists several). True for a key no preset uses
    /// as a tag, and for an empty `features`.
    pub fn key_fits(&self, key: &str, features: &[Vec<Geometry>]) -> bool {
        let Some(allowed) = self.key_geometries.get(key) else {
            return true;
        };
        features.iter().all(|candidates| {
            candidates.is_empty() || candidates.iter().any(|g| allowed.contains(g))
        })
    }

    pub fn values_of(&self, key: &str) -> &[SchemaValue] {
        self.values.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Schema keys can be patterns (`addr:*`); those are not suggestable.
fn is_plain_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('*') && !key.contains(char::is_whitespace)
}

/// Where a suggestion came from; also its rank (lower is listed first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// A value present on the selected features.
    Selection,
    /// The loaded data.
    Data,
    /// The bundled tagging schema.
    Schema,
    /// taginfo.openstreetmap.org.
    Taginfo,
}

/// One entry of the dropdown.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// What accepting the suggestion puts in the input.
    pub text: String,
    /// Secondary text: the schema's label for a value.
    pub label: Option<String>,
    /// The schema field `label` comes from (for translation lookup).
    pub field_id: Option<String>,
    pub source: Source,
    /// Use count (data, taginfo); 0 when unknown.
    pub count: u64,
}

/// How `query` matches `text`: 0 for a prefix, 1 for a substring, `None`
/// for no match. Case-insensitive; an empty query is a prefix of everything.
fn match_class(query_lower: &str, text: &str) -> Option<u8> {
    if query_lower.is_empty() {
        return Some(0);
    }
    // Hot path (every distinct value of a key, per keystroke): plain ASCII
    // is compared in place without allocating.
    if query_lower.is_ascii() && text.is_ascii() {
        let (q, t) = (query_lower.as_bytes(), text.as_bytes());
        if t.len() < q.len() {
            return None;
        }
        if t[..q.len()].eq_ignore_ascii_case(q) {
            return Some(0);
        }
        return t
            .windows(q.len())
            .any(|w| w.eq_ignore_ascii_case(q))
            .then_some(1);
    }
    let t = text.to_lowercase();
    if t.starts_with(query_lower) {
        Some(0)
    } else if t.contains(query_lower) {
        Some(1)
    } else {
        None
    }
}

/// Sort the local (non-taginfo) candidates and append the taginfo ones.
fn finish(
    mut local: Vec<(u8, Suggestion)>,
    taginfo: &[TaginfoItem],
    query_lower: &str,
    exact: &str,
    mut accept_taginfo: impl FnMut(&str) -> bool,
    limit: usize,
) -> Vec<Suggestion> {
    local.sort_by(|(ca, a), (cb, b)| {
        ca.cmp(cb)
            .then(a.source.cmp(&b.source))
            .then(b.count.cmp(&a.count))
            .then(a.text.len().cmp(&b.text.len()))
            .then(a.text.cmp(&b.text))
    });
    let mut out: Vec<Suggestion> = local.into_iter().map(|(_, s)| s).collect();
    let mut present: HashSet<String> = out.iter().map(|s| s.text.clone()).collect();

    // taginfo goes strictly below the local entries, prefix matches first,
    // each group in taginfo's own (usage) order.
    let mut web: Vec<(u8, usize, &TaginfoItem)> = Vec::new();
    for (i, item) in taginfo.iter().enumerate() {
        if item.text == exact || present.contains(&item.text) || !accept_taginfo(&item.text) {
            continue;
        }
        if let Some(class) = match_class(query_lower, &item.text) {
            web.push((class, i, item));
        }
    }
    web.sort_by_key(|(class, i, _)| (*class, *i));
    for (_, _, item) in web {
        if present.insert(item.text.clone()) {
            out.push(Suggestion {
                text: item.text.clone(),
                label: None,
                field_id: None,
                source: Source::Taginfo,
                count: item.count,
            });
        }
    }
    out.truncate(limit);
    out
}

/// Key suggestions for the typed text `query`.
///
/// `exclude` holds keys that must not be offered (those the feature already
/// has). `features` lists each selected feature's possible geometries; keys
/// the presets only use on other geometries (`building` on an open way) are
/// left out. `data` is one index per visible layer, merged by summing counts.
pub fn suggest_keys(
    query: &str,
    data: &[&DataIndex],
    schema: &SchemaIndex,
    taginfo: &[TaginfoItem],
    exclude: &HashSet<String>,
    features: &[Vec<Geometry>],
    limit: usize,
) -> Vec<Suggestion> {
    let query = query.trim();
    let q = query.to_lowercase();

    // Nothing typed: only taginfo's (related or popular) keys are offered,
    // not every key there is.
    let (data, schema_keys): (&[&DataIndex], &[String]) = if query.is_empty() {
        (&[], &[])
    } else {
        (data, schema.keys())
    };
    let mut counts: HashMap<&str, u64> = HashMap::new();
    for index in data {
        for (k, c) in index.keys() {
            *counts.entry(k).or_insert(0) += c as u64;
        }
    }
    let mut local: Vec<(u8, Suggestion)> = Vec::new();
    for (k, c) in &counts {
        if *k == query || exclude.contains(*k) || !schema.key_fits(k, features) {
            continue;
        }
        if let Some(class) = match_class(&q, k) {
            local.push((
                class,
                Suggestion {
                    text: k.to_string(),
                    label: None,
                    field_id: None,
                    source: Source::Data,
                    count: *c,
                },
            ));
        }
    }
    for k in schema_keys {
        if k == query
            || exclude.contains(k)
            || counts.contains_key(k.as_str())
            || !schema.key_fits(k, features)
        {
            continue;
        }
        if let Some(class) = match_class(&q, k) {
            local.push((
                class,
                Suggestion {
                    text: k.clone(),
                    label: None,
                    field_id: None,
                    source: Source::Schema,
                    count: 0,
                },
            ));
        }
    }
    finish(
        local,
        taginfo,
        &q,
        query,
        |k| !exclude.contains(k) && schema.key_fits(k, features),
        limit,
    )
}

/// Value suggestions for `key` given the typed text `query`.
///
/// `selection_values` are the distinct values of `key` among the selected
/// features (pass them for a multi-selection); they are listed first.
pub fn suggest_values(
    key: &str,
    query: &str,
    selection_values: &[String],
    data: &[&DataIndex],
    schema: &SchemaIndex,
    taginfo: &[TaginfoItem],
    limit: usize,
) -> Vec<Suggestion> {
    let query = query.trim();
    let q = query.to_lowercase();

    let mut local: Vec<(u8, Suggestion)> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();

    for v in selection_values {
        if v == query || v.is_empty() {
            continue;
        }
        if let Some(class) = match_class(&q, v) {
            if taken.insert(v.clone()) {
                local.push((
                    class,
                    Suggestion {
                        text: v.clone(),
                        label: None,
                        field_id: None,
                        source: Source::Selection,
                        count: 0,
                    },
                ));
            }
        }
    }

    // Match first, merge only the matches: a key like `name` can have
    // hundreds of thousands of distinct values.
    let mut matched: Vec<(&str, u8, u64)> = Vec::new();
    for index in data {
        for (v, c) in index.values_of(key) {
            if v == query || v.is_empty() || taken.contains(v) {
                continue;
            }
            if let Some(class) = match_class(&q, v) {
                matched.push((v, class, c as u64));
            }
        }
    }
    if data.len() > 1 {
        let mut merged: HashMap<&str, (u8, u64)> = HashMap::new();
        for (v, class, c) in matched {
            merged.entry(v).or_insert((class, 0)).1 += c;
        }
        matched = merged.into_iter().map(|(v, (cl, c))| (v, cl, c)).collect();
    }
    // Only the best `limit` need to become suggestions; selecting them
    // first keeps a key with a huge number of matching values cheap.
    if matched.len() > limit {
        matched.select_nth_unstable_by(limit, |a, b| {
            a.1.cmp(&b.1)
                .then(b.2.cmp(&a.2))
                .then(a.0.len().cmp(&b.0.len()))
                .then(a.0.cmp(b.0))
        });
        matched.truncate(limit);
    }
    for (v, class, c) in matched {
        taken.insert(v.to_string());
        local.push((
            class,
            Suggestion {
                text: v.to_string(),
                label: None,
                field_id: None,
                source: Source::Data,
                count: c,
            },
        ));
    }
    for sv in schema.values_of(key) {
        if sv.value == query || taken.contains(&sv.value) {
            continue;
        }
        // The label counts as a (weaker) match too: "Asph" finds asphalt.
        let class = match_class(&q, &sv.value).or_else(|| {
            sv.label
                .as_deref()
                .and_then(|l| match_class(&q, l))
                .map(|_| 1)
        });
        if let Some(class) = class {
            taken.insert(sv.value.clone());
            local.push((
                class,
                Suggestion {
                    text: sv.value.clone(),
                    label: sv.label.clone(),
                    field_id: sv.field_id.clone(),
                    source: Source::Schema,
                    count: 0,
                },
            ));
        }
    }
    finish(local, taginfo, &q, query, |_| true, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{OsmNode, OsmWay};

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn data(nodes: &[&[(&str, &str)]], ways: &[&[(&str, &str)]]) -> OsmData {
        OsmData {
            nodes: nodes
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    (
                        i as i64 + 1,
                        OsmNode {
                            id: i as i64 + 1,
                            lat: 0.0,
                            lon: 0.0,
                            version: 1,
                            tags: tags(t),
                        },
                    )
                })
                .collect(),
            ways: ways
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    (
                        i as i64 + 1,
                        OsmWay {
                            id: i as i64 + 1,
                            nodes: vec![],
                            version: 1,
                            tags: tags(t),
                        },
                    )
                })
                .collect(),
            relations: vec![],
            bounds: vec![],
        }
    }

    fn schema() -> SchemaIndex {
        let presets = PresetIndex::from_json(
            r#"[
              {"id":"highway/residential","name":"Residential Road","tags":{"highway":"residential"},"geometry":["line"]},
              {"id":"highway/service","name":"Service Road","tags":{"highway":"service"},"geometry":["line"]},
              {"id":"amenity","name":"Amenity","tags":{"amenity":"*"},"geometry":["point"]}
            ]"#,
        )
        .unwrap();
        let fields = FieldIndex::from_json(
            r#"[
              {"id":"surface","key":"surface","field_type":"combo","label":"Surface",
               "options":[{"value":"asphalt","label":"Asphalt"},{"value":"gravel","label":"Gravel"}]},
              {"id":"oneway","key":"oneway","field_type":"check","label":"One Way"}
            ]"#,
        )
        .unwrap();
        SchemaIndex::build(&presets, &fields)
    }

    fn texts(v: &[Suggestion]) -> Vec<&str> {
        v.iter().map(|s| s.text.as_str()).collect()
    }

    fn ti(items: &[(&str, u64)]) -> Vec<TaginfoItem> {
        items
            .iter()
            .map(|(t, c)| TaginfoItem {
                text: t.to_string(),
                count: *c,
            })
            .collect()
    }

    #[test]
    fn empty_key_text_lists_only_taginfo_keys_the_feature_lacks() {
        // The Tags row's empty key input: no local keys (the caller passes
        // none), just taginfo's, in its order, minus keys already present.
        let exclude: HashSet<String> = ["name".to_string()].into();
        let got = suggest_keys(
            "",
            &[],
            &SchemaIndex::default(),
            &ti(&[("name", 9), ("atm", 8), ("operator", 5)]),
            &exclude,
            &[],
            10,
        );
        assert_eq!(texts(&got), ["atm", "operator"]);
        assert!(got.iter().all(|s| s.source == Source::Taginfo));
    }

    #[test]
    fn one_typed_character_filters_related_keys() {
        let got = suggest_keys(
            "o",
            &[],
            &SchemaIndex::default(),
            &ti(&[("atm", 8), ("operator", 5), ("opening_hours", 4)]),
            &HashSet::new(),
            &[],
            10,
        );
        assert_eq!(texts(&got), ["operator", "opening_hours"]);
    }

    fn geo_schema() -> SchemaIndex {
        let presets = PresetIndex::from_json(
            r#"[
              {"id":"building/house","name":"House","tags":{"building":"house"},"geometry":["area"]},
              {"id":"highway/service","name":"Service","tags":{"highway":"service"},"geometry":["line"]},
              {"id":"amenity/bank","name":"Bank","tags":{"amenity":"bank"},"geometry":["point","area"]}
            ]"#,
        )
        .unwrap();
        let fields = FieldIndex::from_json("[]").unwrap();
        SchemaIndex::build(&presets, &fields)
    }

    #[test]
    fn keys_for_other_geometries_are_left_out() {
        let schema = geo_schema();
        let taginfo = ti(&[("building", 9), ("highway", 8), ("name", 7), ("amenity", 6)]);
        let ask = |features: &[Vec<Geometry>]| {
            texts(&suggest_keys(
                "",
                &[],
                &SchemaIndex::default(),
                &taginfo,
                &HashSet::new(),
                features,
                10,
            ))
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        };
        // Without a schema, nothing is known and nothing is filtered.
        assert_eq!(ask(&[]), ["building", "highway", "name", "amenity"]);
        let filtered = |features: &[Vec<Geometry>]| {
            let got = suggest_keys("", &[], &schema, &taginfo, &HashSet::new(), features, 10);
            texts(&got)
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        };
        // An open way is only a line: no building or amenity, name is fine.
        assert_eq!(filtered(&[vec![Geometry::Line]]), ["highway", "name"]);
        // A closed way may be an area or a line.
        assert_eq!(
            filtered(&[vec![Geometry::Area, Geometry::Line]]),
            ["building", "highway", "name", "amenity"]
        );
        // Every selected feature must fit: a node and an open way.
        assert_eq!(
            filtered(&[vec![Geometry::Point], vec![Geometry::Line]]),
            ["name"]
        );
    }

    #[test]
    fn data_index_counts_elements() {
        let d = data(
            &[
                &[("amenity", "cafe")],
                &[("amenity", "cafe"), ("name", "A")],
            ],
            &[&[("highway", "service")]],
        );
        let idx = DataIndex::from_osm(&d);
        assert_eq!(idx.key_count("amenity"), 2);
        assert_eq!(idx.key_count("name"), 1);
        assert_eq!(idx.key_count("nope"), 0);
        assert_eq!(idx.values_of("amenity").collect::<Vec<_>>(), [("cafe", 2)]);
    }

    #[test]
    fn incremental_updates_match_a_rebuild() {
        let before = data(
            &[&[("amenity", "cafe")], &[("surface", "asphalt")]],
            &[&[("highway", "service"), ("surface", "gravel")]],
        );
        let mut idx = DataIndex::from_osm(&before);
        // Node 1: amenity=cafe -> shop=bakery; way 1: surface removed;
        // node 2: surface asphalt -> gravel; node 2 also gains name.
        let mut after = before.clone();
        {
            let n = after.nodes.get_mut(&1).unwrap();
            n.tags = tags(&[("shop", "bakery")]);
            idx.apply_change("amenity", Some("cafe"), None);
            idx.apply_change("shop", None, Some("bakery"));
            let w = after.ways.get_mut(&1).unwrap();
            w.tags.remove("surface");
            idx.apply_change("surface", Some("gravel"), None);
            let n2 = after.nodes.get_mut(&2).unwrap();
            n2.tags = tags(&[("surface", "gravel"), ("name", "X")]);
            idx.apply_change("surface", Some("asphalt"), Some("gravel"));
            idx.apply_change("name", None, Some("X"));
        }
        assert_eq!(idx, DataIndex::from_osm(&after));
    }

    #[test]
    fn removing_the_last_use_forgets_the_key() {
        let mut idx = DataIndex::default();
        idx.add_tag("a", "1");
        idx.remove_tag("a", "1");
        assert!(idx.is_empty());
        idx.remove_tag("a", "1"); // unknown: no-op, no underflow
        assert!(idx.is_empty());
    }

    #[test]
    fn schema_has_keys_and_values() {
        let s = schema();
        for k in ["highway", "amenity", "surface", "oneway"] {
            assert!(s.keys().contains(&k.to_string()), "{k}");
        }
        let surface: Vec<_> = s
            .values_of("surface")
            .iter()
            .map(|v| &v.value[..])
            .collect();
        assert_eq!(surface, ["asphalt", "gravel"]);
        assert_eq!(s.values_of("surface")[0].label.as_deref(), Some("Asphalt"));
        let hw: Vec<_> = s
            .values_of("highway")
            .iter()
            .map(|v| &v.value[..])
            .collect();
        assert!(hw.contains(&"residential") && hw.contains(&"service"));
        // "*" is not a value; check fields list yes/no.
        assert!(s.values_of("amenity").is_empty());
        assert_eq!(s.values_of("oneway").len(), 2);
    }

    #[test]
    fn keys_rank_prefix_before_substring_then_data_then_schema_then_taginfo() {
        let d = DataIndex::from_osm(&data(
            &[&[("sur_x", "1")], &[("sur_x", "1")], &[("subsurface", "1")]],
            &[],
        ));
        let s = schema();
        let web = ti(&[
            ("surveillance", 900),
            ("surface:note", 5),
            ("tunnel_surface", 4),
        ]);
        let got = suggest_keys("sur", &[&d], &s, &web, &HashSet::new(), &[], 20);
        assert_eq!(
            texts(&got),
            [
                "sur_x",        // prefix, data
                "surface",      // prefix, schema
                "subsurface",   // substring, data
                "surveillance", // taginfo prefix
                "surface:note",
                "tunnel_surface", // taginfo substring
            ]
        );
        assert_eq!(got[0].source, Source::Data);
        assert_eq!(got[1].source, Source::Schema);
        assert_eq!(got[3].source, Source::Taginfo);
    }

    #[test]
    fn data_keys_sort_by_count() {
        let d = DataIndex::from_osm(&data(
            &[&[("aa", "1")], &[("ab", "1")], &[("ab", "1")]],
            &[],
        ));
        let got = suggest_keys(
            "a",
            &[&d],
            &SchemaIndex::default(),
            &[],
            &HashSet::new(),
            &[],
            5,
        );
        assert_eq!(texts(&got), ["ab", "aa"]);
    }

    #[test]
    fn existing_keys_and_exact_text_are_not_suggested() {
        let d = DataIndex::from_osm(&data(&[&[("surface", "x"), ("surface:lanes", "y")]], &[]));
        let mut exclude = HashSet::new();
        exclude.insert("surface".to_string());
        let web = ti(&[("surface", 1), ("surfaced", 1)]);
        let got = suggest_keys("sur", &[&d], &schema(), &web, &exclude, &[], 20);
        assert_eq!(texts(&got), ["surface:lanes", "surfaced"]);
        // The text typed in full is not offered back.
        let got = suggest_keys(
            "surface:lanes",
            &[&d],
            &schema(),
            &[],
            &HashSet::new(),
            &[],
            20,
        );
        assert!(got.is_empty());
    }

    #[test]
    fn matching_ignores_case_and_merges_layers() {
        let a = DataIndex::from_osm(&data(&[&[("Highway", "1")]], &[]));
        let b = DataIndex::from_osm(&data(&[&[("Highway", "1")]], &[]));
        let got = suggest_keys(
            "high",
            &[&a, &b],
            &SchemaIndex::default(),
            &[],
            &HashSet::new(),
            &[],
            5,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].count, 2);
    }

    #[test]
    fn values_come_from_data_then_schema_with_labels() {
        let d = DataIndex::from_osm(&data(
            &[
                &[("surface", "asphalt")],
                &[("surface", "asphalt")],
                &[("surface", "paved")],
            ],
            &[],
        ));
        let got = suggest_values("surface", "", &[], &[&d], &schema(), &[], 20);
        assert_eq!(texts(&got), ["asphalt", "paved", "gravel"]);
        assert_eq!(got[0].source, Source::Data);
        // data wins over the schema entry, so no label on asphalt
        assert_eq!(got[2].label.as_deref(), Some("Gravel"));
        assert_eq!(got[2].source, Source::Schema);
    }

    #[test]
    fn schema_values_alone_work_on_an_empty_layer() {
        let got = suggest_values("surface", "as", &[], &[], &schema(), &[], 20);
        assert_eq!(texts(&got), ["asphalt"]);
        assert_eq!(got[0].label.as_deref(), Some("Asphalt"));
        let hw = suggest_values("highway", "res", &[], &[], &schema(), &[], 20);
        assert_eq!(texts(&hw), ["residential"]);
        let by_label = suggest_values("surface", "grav", &[], &[], &schema(), &[], 20);
        assert_eq!(texts(&by_label), ["gravel"]);
    }

    #[test]
    fn multi_selection_values_are_listed_first() {
        let d = DataIndex::from_osm(&data(&[&[("surface", "asphalt")]], &[]));
        let sel = vec!["gravel".to_string(), "dirt".to_string()];
        let got = suggest_values("surface", "", &sel, &[&d], &schema(), &[], 20);
        // equal rank, ordered by length then text
        assert_eq!(texts(&got)[..2], ["dirt", "gravel"]);
        assert_eq!(got[0].source, Source::Selection);
        assert_eq!(got[2].text, "asphalt");
    }

    #[test]
    fn taginfo_never_reorders_local_entries() {
        let d = DataIndex::from_osm(&data(&[&[("surface", "asphalt")]], &[]));
        let without = suggest_values("surface", "a", &[], &[&d], &schema(), &[], 20);
        let web = ti(&[("a", 1), ("abc", 9), ("asphalt", 5)]);
        let with = suggest_values("surface", "a", &[], &[&d], &schema(), &web, 20);
        assert_eq!(&with[..without.len()], &without[..]);
        assert_eq!(texts(&with)[without.len()..], ["abc"]);
    }

    /// Timing on a 300k-node layer. Run with
    /// `cargo test --release --lib -- --ignored --nocapture perf_300k`.
    #[test]
    #[ignore]
    fn perf_300k() {
        use std::time::Instant;
        let mut nodes = HashMap::new();
        for i in 0..300_000i64 {
            let mut t = HashMap::new();
            if i % 3 == 0 {
                t.insert("name".to_string(), format!("Place number {i}"));
            }
            if i % 5 == 0 {
                t.insert("addr:street".to_string(), format!("Street {}", i % 4000));
                t.insert("addr:housenumber".to_string(), format!("{}", i % 300));
            }
            if i % 7 == 0 {
                t.insert(
                    "amenity".to_string(),
                    ["cafe", "bench", "bar"][(i % 3) as usize].into(),
                );
                t.insert(format!("survey:{}", i % 50), "yes".to_string());
            }
            nodes.insert(
                i,
                OsmNode {
                    id: i,
                    lat: 0.0,
                    lon: 0.0,
                    version: 1,
                    tags: t,
                },
            );
        }
        let d = OsmData {
            nodes,
            ways: HashMap::new(),
            relations: vec![],
            bounds: vec![],
        };
        let t = Instant::now();
        let idx = DataIndex::from_osm(&d);
        println!("index build (300k nodes): {:?}", t.elapsed());
        let schema = {
            let t = Instant::now();
            let s =
                SchemaIndex::build(crate::presets::preset_index(), crate::fields::field_index());
            println!("schema build: {:?}, {} keys", t.elapsed(), s.keys().len());
            s
        };
        let t = Instant::now();
        let mut n = 0;
        for _ in 0..100 {
            n += suggest_keys("su", &[&idx], &schema, &[], &HashSet::new(), &[], 30).len();
        }
        println!("key lookup: {:?} per call ({n})", t.elapsed() / 100);
        let t = Instant::now();
        for _ in 0..100 {
            n += suggest_values("name", "pla", &[], &[&idx], &schema, &[], 30).len();
        }
        println!(
            "value lookup, 100k distinct names: {:?} per call",
            t.elapsed() / 100
        );
        let t = Instant::now();
        for _ in 0..100 {
            n += suggest_values("addr:street", "str", &[], &[&idx], &schema, &[], 30).len();
        }
        println!(
            "value lookup, 4k distinct streets: {:?} per call ({n})",
            t.elapsed() / 100
        );
    }

    #[test]
    fn limit_applies() {
        let d = DataIndex::from_osm(&data(
            &[&[("k1", "v")], &[("k2", "v")], &[("k3", "v")]],
            &[],
        ));
        let got = suggest_keys(
            "k",
            &[&d],
            &SchemaIndex::default(),
            &[],
            &HashSet::new(),
            &[],
            2,
        );
        assert_eq!(got.len(), 2);
    }
}
