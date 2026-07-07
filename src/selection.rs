//! Selection types and pure hit-testing math.

use gpui::{Pixels, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Node,
    Way,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureRef {
    pub layer_name: String,
    pub kind: FeatureKind,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HitCandidate {
    pub feature: FeatureRef,
    pub kind: FeatureKind,
    pub dist_px: f32,
}

/// Enough information to fully restore a deleted node or way, produced by
/// `MapLayer::delete_feature` and consumed by `MapLayer::restore_feature`
/// (the undo path for a delete).
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedFeatureSnapshot {
    pub kind: FeatureKind,
    pub id: i64,
    /// The feature's tags at the time of deletion, as (key, value) pairs.
    pub tags: Vec<(String, String)>,
    /// For a way: its ordered member node ids. Empty/unused for a node.
    pub way_nodes: Vec<i64>,
    /// For a node: its (lat, lon) at the time of deletion. `None` for a way.
    pub node_lat_lon: Option<(f64, f64)>,
}

/// Shortest distance (in screen pixels) from point `p` to line segment `a`-`b`.
/// Handles zero-length segments by returning the distance to the single point.
pub fn point_to_segment_distance(
    p: Point<Pixels>,
    a: Point<Pixels>,
    b: Point<Pixels>,
) -> f32 {
    let px = p.x.as_f32();
    let py = p.y.as_f32();
    let ax = a.x.as_f32();
    let ay = a.y.as_f32();
    let bx = b.x.as_f32();
    let by = b.y.as_f32();

    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        let ex = px - ax;
        let ey = py - ay;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
    let qx = ax + t * dx;
    let qy = ay + t * dy;
    let ex = px - qx;
    let ey = py - qy;
    (ex * ex + ey * ey).sqrt()
}

/// Pick the winning feature across all visible OSM layers.
///
/// `per_layer` is expected in draw order (earliest-drawn first, topmost last).
/// Nearest candidate wins; on exact distance ties, later-drawn (topmost) wins.
pub fn resolve_hits(per_layer: Vec<Vec<HitCandidate>>) -> Option<FeatureRef> {
    let mut best: Option<(f32, usize, FeatureRef)> = None;
    for (layer_idx, candidates) in per_layer.into_iter().enumerate() {
        for c in candidates {
            match &best {
                None => best = Some((c.dist_px, layer_idx, c.feature)),
                Some((d, li, _)) => {
                    if c.dist_px < *d || (c.dist_px == *d && layer_idx >= *li) {
                        best = Some((c.dist_px, layer_idx, c.feature));
                    }
                }
            }
        }
    }
    best.map(|(_, _, f)| f)
}

/// A key's aggregated value across a set of features: either every feature
/// agrees (has the key with the same value), or they don't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue {
    Single(String),
    Multiple(usize),
}

/// Aggregate tags across multiple features' tag lists. Keys are the union
/// across all features. For each key, every feature contributes a state —
/// `Some(value)` if it has the key, `None` if it doesn't — and these states
/// are compared across *all* features, not just the ones that have the key:
/// exactly one distinct state (necessarily `Some(value)`, since the key
/// came from the union) yields `Single(value)`; more than one distinct
/// state yields `Multiple(distinct_count)`. A feature missing the key counts
/// as its own distinct state, so a key present on some features and absent
/// on others is `Multiple`, never `Single`. Sorted by key.
pub fn aggregate_tags(per_feature: &[Vec<(String, String)>]) -> Vec<(String, TagValue)> {
    use std::collections::BTreeSet;

    let mut keys: BTreeSet<String> = BTreeSet::new();
    for tags in per_feature {
        for (k, _) in tags {
            keys.insert(k.clone());
        }
    }

    keys.into_iter()
        .map(|k| {
            let states: BTreeSet<Option<String>> = per_feature
                .iter()
                .map(|tags| tags.iter().find(|(tk, _)| *tk == k).map(|(_, v)| v.clone()))
                .collect();

            let value = if states.len() == 1 {
                match states.into_iter().next().unwrap() {
                    Some(v) => TagValue::Single(v),
                    None => unreachable!("key came from the union of features' present tags"),
                }
            } else {
                TagValue::Multiple(states.len())
            };
            (k, value)
        })
        .collect()
}

/// Computes the tag mutations to apply across `features` for a tag-edit
/// dialog submission. Each feature's current tags are supplied as
/// `(FeatureRef, Vec<(String, String)>)`. Returns one entry per feature per
/// key actually touched, as `(feature, key, before, after)` (`before`/
/// `after` are `None` when the key is absent/removed); entries where
/// `before == after` are omitted since they're no-ops.
///
/// `is_add` is true for the "Add tag" flow (dialog opened with empty
/// key/value, targeting the whole selection): `new_key` is set to
/// `new_value` on every feature, overwriting any existing value.
///
/// Otherwise this is an edit or rename of an existing row:
/// - If `new_key != original_key` (rename): for each feature that already
///   has `original_key`, remove it and set `new_key` — to the feature's
///   own preserved value if `new_value == original_value` (value box left
///   untouched), else to `new_value` uniformly. Features that never had
///   `original_key` are left untouched entirely (nothing to rename).
/// - Otherwise (same key): set `original_key` to `new_value` on every
///   feature, unless `new_value == original_value` (untouched), in which
///   case each feature keeps its own current value (a no-op, omitted).
pub fn compute_tag_edit_entries(
    features: &[(FeatureRef, Vec<(String, String)>)],
    original_key: &str,
    original_value: &str,
    new_key: &str,
    new_value: &str,
    is_add: bool,
) -> Vec<(FeatureRef, String, Option<String>, Option<String>)> {
    let value_touched = new_value != original_value;
    let mut out = Vec::new();

    for (feature, tags) in features {
        let current = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

        if is_add {
            let before = current(new_key);
            if before.as_deref() != Some(new_value) {
                out.push((feature.clone(), new_key.to_string(), before, Some(new_value.to_string())));
            }
            continue;
        }

        if new_key != original_key {
            let Some(old_before) = current(original_key) else {
                continue; // never had the key being renamed — nothing to do
            };
            out.push((feature.clone(), original_key.to_string(), Some(old_before.clone()), None));

            let new_before = current(new_key);
            let after_value = if value_touched { new_value.to_string() } else { old_before };
            if new_before.as_deref() != Some(after_value.as_str()) {
                out.push((feature.clone(), new_key.to_string(), new_before, Some(after_value)));
            }
        } else {
            let before = current(original_key);
            let after = if value_touched { Some(new_value.to_string()) } else { before.clone() };
            if before != after {
                out.push((feature.clone(), original_key.to_string(), before, after));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn pt(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn fref(name: &str, kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef { layer_name: name.into(), kind, id }
    }

    #[test]
    fn orthogonal_midpoint_distance() {
        let d = point_to_segment_distance(pt(5.0, 3.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((d - 3.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn past_endpoint_falls_back_to_endpoint() {
        let d = point_to_segment_distance(pt(13.0, 4.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((d - 5.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn zero_length_segment_returns_point_distance() {
        let d = point_to_segment_distance(pt(3.0, 4.0), pt(0.0, 0.0), pt(0.0, 0.0));
        assert!((d - 5.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn resolve_returns_none_on_empty() {
        assert!(resolve_hits(vec![]).is_none());
        assert!(resolve_hits(vec![vec![], vec![]]).is_none());
    }

    #[test]
    fn resolve_picks_nearest() {
        let a = HitCandidate {
            feature: fref("L0", FeatureKind::Node, 1),
            kind: FeatureKind::Node,
            dist_px: 5.0,
        };
        let b = HitCandidate {
            feature: fref("L0", FeatureKind::Way, 2),
            kind: FeatureKind::Way,
            dist_px: 3.0,
        };
        let winner = resolve_hits(vec![vec![a, b]]).unwrap();
        assert_eq!(winner.id, 2);
    }

    #[test]
    fn resolve_tie_prefers_later_layer() {
        let a = HitCandidate {
            feature: fref("bottom", FeatureKind::Node, 1),
            kind: FeatureKind::Node,
            dist_px: 2.0,
        };
        let b = HitCandidate {
            feature: fref("top", FeatureKind::Node, 99),
            kind: FeatureKind::Node,
            dist_px: 2.0,
        };
        let winner = resolve_hits(vec![vec![a], vec![b]]).unwrap();
        assert_eq!(winner.layer_name, "top");
        assert_eq!(winner.id, 99);
    }

    #[test]
    fn aggregate_single_feature_single_value() {
        let per_feature = vec![vec![("highway".to_string(), "residential".to_string())]];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![("highway".to_string(), TagValue::Single("residential".to_string()))]
        );
    }

    #[test]
    fn aggregate_multiple_distinct_values_counts_distinct_only() {
        let per_feature = vec![
            vec![("name".to_string(), "Main St".to_string())],
            vec![("name".to_string(), "Elm St".to_string())],
            vec![("name".to_string(), "Main St".to_string())], // duplicate value
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(result, vec![("name".to_string(), TagValue::Multiple(2))]);
    }

    #[test]
    fn aggregate_missing_key_on_some_features_counts_as_distinct_value() {
        let per_feature = vec![
            vec![("name".to_string(), "Main St".to_string())],
            vec![], // no tags at all on this feature
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(result, vec![("name".to_string(), TagValue::Multiple(2))]);
    }

    #[test]
    fn aggregate_untagged_feature_makes_key_multiple_even_with_one_shared_value() {
        // A node with no tags plus a node with building=yes must not show as
        // "yes" — the untagged node's absence of the key counts as its own
        // distinct state, so this is Multiple(2), never Single.
        let per_feature = vec![
            vec![], // untagged node
            vec![("building".to_string(), "yes".to_string())],
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(result, vec![("building".to_string(), TagValue::Multiple(2))]);
    }

    #[test]
    fn aggregate_union_of_keys_across_features() {
        let per_feature = vec![
            vec![
                ("highway".to_string(), "residential".to_string()),
                ("surface".to_string(), "paved".to_string()),
            ],
            vec![("highway".to_string(), "residential".to_string())],
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![
                ("highway".to_string(), TagValue::Single("residential".to_string())),
                // Present on the first feature but absent on the second.
                ("surface".to_string(), TagValue::Multiple(2)),
            ]
        );
    }

    #[test]
    fn aggregate_empty_input_returns_empty() {
        assert!(aggregate_tags(&[]).is_empty());
    }

    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn edit_same_key_uniform_value_when_touched() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[("highway", "trunk")])),
        ];
        let entries = compute_tag_edit_entries(&features, "highway", "<2 values>", "highway", "service", false);
        assert_eq!(
            entries,
            vec![
                (a, "highway".to_string(), Some("residential".to_string()), Some("service".to_string())),
                (b, "highway".to_string(), Some("trunk".to_string()), Some("service".to_string())),
            ]
        );
    }

    #[test]
    fn edit_same_key_untouched_value_preserves_per_feature() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[("highway", "trunk")])),
        ];
        // Value box left showing the original "<2 values>" placeholder text.
        let entries = compute_tag_edit_entries(&features, "highway", "<2 values>", "highway", "<2 values>", false);
        assert!(entries.is_empty(), "no feature's value actually changes: {:?}", entries);
    }

    #[test]
    fn edit_same_key_noop_produces_no_entries() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a, tags(&[("highway", "residential")]))];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway", "residential", false);
        assert!(entries.is_empty());
    }

    #[test]
    fn rename_moves_value_and_skips_features_without_key() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[])), // b never had "highway"
        ];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway_type", "residential", false);
        assert_eq!(
            entries,
            vec![
                (a.clone(), "highway".to_string(), Some("residential".to_string()), None),
                (a, "highway_type".to_string(), None, Some("residential".to_string())),
            ]
        );
        // b is untouched entirely — it never had "highway" to rename.
    }

    #[test]
    fn rename_with_touched_value_uses_new_value() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a.clone(), tags(&[("highway", "residential")]))];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway_type", "trunk", false);
        assert_eq!(
            entries,
            vec![
                (a.clone(), "highway".to_string(), Some("residential".to_string()), None),
                (a, "highway_type".to_string(), None, Some("trunk".to_string())),
            ]
        );
    }

    #[test]
    fn add_sets_new_key_on_all_features_overwriting_existing() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[])),
            (b.clone(), tags(&[("surface", "gravel")])),
        ];
        let entries = compute_tag_edit_entries(&features, "", "", "surface", "paved", true);
        assert_eq!(
            entries,
            vec![
                (a, "surface".to_string(), None, Some("paved".to_string())),
                (b, "surface".to_string(), Some("gravel".to_string()), Some("paved".to_string())),
            ]
        );
    }

    #[test]
    fn add_noop_when_value_already_matches() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a, tags(&[("surface", "paved")]))];
        let entries = compute_tag_edit_entries(&features, "", "", "surface", "paved", true);
        assert!(entries.is_empty());
    }
}
