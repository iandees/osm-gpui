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
    let px = p.x.0;
    let py = p.y.0;
    let ax = a.x.0;
    let ay = a.y.0;
    let bx = b.x.0;
    let by = b.y.0;

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
}
