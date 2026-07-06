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
/// that has the key agrees on one value, or they don't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue {
    Single(String),
    Multiple(usize),
}

/// Aggregate tags across multiple features' tag lists. Keys are the union
/// across all features. For each key, distinct values are counted only
/// among the features that *have* that key: exactly one distinct value
/// yields `Single(value)`; more than one yields `Multiple(distinct_count)`.
/// A feature missing a key does not affect that key's aggregation. Sorted
/// by key.
pub fn aggregate_tags(per_feature: &[Vec<(String, String)>]) -> Vec<(String, TagValue)> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut by_key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tags in per_feature {
        for (k, v) in tags {
            by_key.entry(k.clone()).or_default().insert(v.clone());
        }
    }

    by_key
        .into_iter()
        .map(|(k, values)| {
            let value = if values.len() == 1 {
                TagValue::Single(values.into_iter().next().unwrap())
            } else {
                TagValue::Multiple(values.len())
            };
            (k, value)
        })
        .collect()
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
    fn aggregate_missing_key_on_some_features_is_ignored() {
        let per_feature = vec![
            vec![("name".to_string(), "Main St".to_string())],
            vec![], // no tags at all on this feature
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![("name".to_string(), TagValue::Single("Main St".to_string()))]
        );
    }

    #[test]
    fn aggregate_union_of_keys_across_features() {
        let per_feature = vec![
            vec![("highway".to_string(), "residential".to_string())],
            vec![("surface".to_string(), "paved".to_string())],
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![
                ("highway".to_string(), TagValue::Single("residential".to_string())),
                ("surface".to_string(), TagValue::Single("paved".to_string())),
            ]
        );
    }

    #[test]
    fn aggregate_empty_input_returns_empty() {
        assert!(aggregate_tags(&[]).is_empty());
    }
}
