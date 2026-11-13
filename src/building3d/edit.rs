//! The 3D view's edits, planned as pure functions over the local metric
//! frame: push/pull of heights, splitting a footprint along a line or a part
//! at a height, drawing a new rectangular part, and the tag rewrites they
//! need. The app turns the plans into OSM changes and undo steps.

use std::collections::HashMap;

use glam::Vec3;

use super::camera::{OrbitCamera, Ray};
use super::model::Pt;
use super::tags::{self, Heights, MIN_THICKNESS_M};

pub type Tags = HashMap<String, String>;

// --- heights ----------------------------------------------------------------

/// Which height a vertical drag changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalEdit {
    /// The top (roof) of a part: `height`.
    Top,
    /// The underside of a raised part: `min_height`.
    Bottom,
    /// The whole part, keeping its thickness (Shift-drag of the roof).
    Lift,
}

/// Height of the point under the cursor on the vertical plane through
/// `anchor` that faces the camera — how far up or down a vertical drag
/// that grabbed `anchor` has gone.
pub fn drag_height(ray: &Ray, anchor: Vec3, camera: &OrbitCamera) -> Option<f32> {
    ray.hit_plane(anchor, camera.forward_flat())
        .map(|p| p.z)
        .filter(|z| z.is_finite())
}

/// Round `raw` to 0.1 m, or pull it onto the nearest of `candidates`
/// within `tolerance` metres (other parts' tops and bottoms), unless
/// snapping is off.
pub fn snap_height(raw: f64, candidates: &[f64], tolerance: f64, snap: bool) -> f64 {
    if snap {
        if let Some(c) = candidates
            .iter()
            .copied()
            .filter(|c| (c - raw).abs() <= tolerance)
            .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()))
        {
            return c;
        }
    }
    (raw * 10.0).round() / 10.0
}

/// `h` with the edited height moved to `value` (an absolute height in
/// metres), kept physically sensible: the bottom never goes below ground or
/// above the top.
pub fn apply_vertical(h: &Heights, edit: VerticalEdit, value: f64) -> Heights {
    let mut out = h.clone();
    match edit {
        VerticalEdit::Top => {
            out.height = value.max(h.min_height + h.roof_height.max(MIN_THICKNESS_M));
        }
        VerticalEdit::Bottom => {
            out.min_height = value.clamp(0.0, h.wall_top() - MIN_THICKNESS_M).max(0.0);
        }
        VerticalEdit::Lift => {
            let thickness = h.height - h.min_height;
            out.min_height = value.max(0.0);
            out.height = out.min_height + thickness;
        }
    }
    out
}

/// The part's tags rewritten for new heights: `height` and `min_height` in
/// metres, and — when the part counts storeys — `building:levels` /
/// `building:min_level` rescaled with the storey height they implied, so
/// the two descriptions keep agreeing.
pub fn retag_heights(tags_in: &Tags, old: &Heights, new: &Heights) -> Tags {
    let mut t = tags_in.clone();
    t.insert("height".into(), tags::format_height(new.height));
    if new.min_height > 0.005 {
        t.insert("min_height".into(), tags::format_height(new.min_height));
    } else {
        t.remove("min_height");
    }
    let level_height = level_height(tags_in, old);
    if t.contains_key("building:levels") {
        let levels = (new.wall_top() / level_height).round().max(1.0);
        t.insert("building:levels".into(), format!("{levels}"));
    }
    if t.contains_key("building:min_level")
        || (new.min_height > 0.005 && t.contains_key("building:levels"))
    {
        let min_level = (new.min_height / level_height).round();
        if min_level > 0.0 {
            t.insert("building:min_level".into(), format!("{min_level}"));
        } else {
            t.remove("building:min_level");
        }
    }
    t
}

/// The storey height a part's tags imply: its wall height over its
/// `building:levels`, else the default.
fn level_height(t: &Tags, h: &Heights) -> f64 {
    t.get("building:levels")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|l| *l >= 1.0)
        .map(|l| h.wall_top() / l)
        .filter(|lh| (1.5..=10.0).contains(lh))
        .unwrap_or(tags::LEVEL_HEIGHT_M)
}

/// The tags that describe a solid's shape and look, copied from a building
/// outline onto the parts made from it.
const PART_KEYS: &[&str] = &[
    "height",
    "min_height",
    "building:levels",
    "building:min_level",
    "building:colour",
    "building:material",
];

/// Tags for a new `building:part` cut from `source` (a part, copied whole,
/// or a building outline, of which only the shape/look keys carry over).
pub fn part_tags_from(source: &Tags, source_is_outline: bool) -> Tags {
    if !source_is_outline {
        return source.clone();
    }
    let mut t: Tags = source
        .iter()
        .filter(|(k, _)| PART_KEYS.contains(&k.as_str()) || k.starts_with("roof:"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    t.insert("building:part".into(), "yes".into());
    t
}

/// Split a part at height `at`: the tags of the lower piece (flat-roofed,
/// topping out at `at`) and of the upper piece (starting at `at`, keeping
/// the roof). `None` unless `at` is strictly inside the walls.
pub fn split_at_height(t: &Tags, h: &Heights, at: f64) -> Option<(Tags, Tags)> {
    if at <= h.min_height + MIN_THICKNESS_M || at >= h.wall_top() - MIN_THICKNESS_M {
        return None;
    }
    let lower_h = Heights {
        min_height: h.min_height,
        height: at,
        roof_height: 0.0,
    };
    let mut lower = retag_heights(t, h, &lower_h);
    for k in [
        "roof:shape",
        "roof:height",
        "roof:levels",
        "roof:orientation",
        "roof:direction",
        "roof:angle",
    ] {
        lower.remove(k);
    }
    let upper_h = Heights {
        min_height: at,
        height: h.height,
        roof_height: h.roof_height,
    };
    let upper = retag_heights(t, h, &upper_h);
    Some((lower, upper))
}

/// Per-key differences between two tag sets: (key, before, after), sorted
/// by key.
pub fn diff_tags(before: &Tags, after: &Tags) -> Vec<(String, Option<String>, Option<String>)> {
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter_map(|k| {
            let (b, a) = (before.get(k), after.get(k));
            (b != a).then(|| (k.clone(), b.cloned(), a.cloned()))
        })
        .collect()
}

// --- splitting a footprint along a line --------------------------------------

/// A vertex of a ring produced by [`split_ring`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingVertex {
    /// `ring[i]` of the input.
    Existing(usize),
    /// `RingSplit::new_points[i]`.
    New(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RingSplit {
    /// Points to create, each with the input segment it lies on (segment `i`
    /// runs from `ring[i]` to `ring[i + 1]`).
    pub new_points: Vec<(Pt, usize)>,
    /// The two pieces, open (no closing repeat), each starting at a cut.
    pub a: Vec<RingVertex>,
    pub b: Vec<RingVertex>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// The line misses the footprint or only touches it.
    NoCrossing,
    /// The line crosses the outline more than twice (a concave footprint);
    /// it would make more than two pieces.
    TooManyCrossings,
}

/// Split a simple polygon `ring` (open, no closing repeat) along the
/// infinite line through `p` and `q`. Vertices within `SNAP_M` of the line
/// are used as cut points instead of creating new ones next to them.
pub fn split_ring(ring: &[Pt], p: Pt, q: Pt) -> Result<RingSplit, SplitError> {
    const SNAP_M: f64 = 0.05;
    let n = ring.len();
    let (dx, dy) = (q.0 - p.0, q.1 - p.1);
    let len = dx.hypot(dy);
    if n < 3 || len < 1e-9 {
        return Err(SplitError::NoCrossing);
    }
    // Signed distance of each vertex from the line.
    let side: Vec<f64> = ring
        .iter()
        .map(|v| {
            let d = (dx * (v.1 - p.1) - dy * (v.0 - p.0)) / len;
            if d.abs() < SNAP_M {
                0.0
            } else {
                d
            }
        })
        .collect();

    // Cut points in ring order: on-line vertices and edge crossings.
    let mut cuts: Vec<RingVertex> = Vec::new();
    let mut new_points: Vec<(Pt, usize)> = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        if side[i] == 0.0 {
            cuts.push(RingVertex::Existing(i));
        }
        if side[i] * side[j] < 0.0 {
            let t = side[i] / (side[i] - side[j]);
            let (a, b) = (ring[i], ring[j]);
            new_points.push(((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t), i));
            cuts.push(RingVertex::New(new_points.len() - 1));
        }
    }
    // An on-line vertex where the polygon only touches the line (both
    // neighbours on the same side) is not a cut.
    cuts.retain(|c| match *c {
        RingVertex::Existing(i) => {
            let prev = side[(i + n - 1) % n];
            let next = side[(i + 1) % n];
            prev * next < 0.0 || (prev == 0.0) != (next == 0.0)
        }
        RingVertex::New(_) => true,
    });
    // Along an edge lying on the line, only its far end counts: drop a cut
    // vertex whose successor is also a cut on the line.
    let on_line_edge = |i: usize| side[i] == 0.0 && side[(i + 1) % n] == 0.0;
    cuts.retain(|c| match *c {
        RingVertex::Existing(i) => !on_line_edge(i),
        RingVertex::New(_) => true,
    });
    match cuts.len() {
        0 | 1 => return Err(SplitError::NoCrossing),
        2 => {}
        _ => return Err(SplitError::TooManyCrossings),
    }
    // Walk the ring from each cut to the other.
    let seq: Vec<RingVertex> = {
        let mut s = Vec::with_capacity(n + new_points.len());
        for i in 0..n {
            s.push(RingVertex::Existing(i));
            for (k, (_, seg)) in new_points.iter().enumerate() {
                if *seg == i {
                    s.push(RingVertex::New(k));
                }
            }
        }
        s
    };
    let pos = |c: RingVertex| seq.iter().position(|v| *v == c).unwrap();
    let (c0, c1) = (pos(cuts[0]), pos(cuts[1]));
    let walk = |from: usize, to: usize| -> Vec<RingVertex> {
        let mut out = Vec::new();
        let mut k = from;
        loop {
            out.push(seq[k]);
            if k == to {
                break;
            }
            k = (k + 1) % seq.len();
        }
        out
    };
    let a = walk(c0, c1);
    let b = walk(c1, c0);
    if a.len() < 3 || b.len() < 3 {
        return Err(SplitError::NoCrossing);
    }
    Ok(RingSplit { new_points, a, b })
}

// --- drawing a rectangle -----------------------------------------------------

/// Direction (radians from east) of the longest segment of `ring`: new
/// rectangles line up with the building.
pub fn dominant_angle(ring: &[Pt]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            ((b.0 - a.0).hypot(b.1 - a.1), (b.1 - a.1).atan2(b.0 - a.0))
        })
        .max_by(|x, y| x.0.total_cmp(&y.0))
        .map_or(0.0, |(_, ang)| ang)
}

/// The rectangle with opposite corners `start` and `end`, its sides along
/// `angle` and its perpendicular, counter-clockwise.
pub fn rectangle(start: Pt, end: Pt, angle: f64) -> [Pt; 4] {
    let (u, v) = ((angle.cos(), angle.sin()), (-angle.sin(), angle.cos()));
    let d = (end.0 - start.0, end.1 - start.1);
    let (du, dv) = (d.0 * u.0 + d.1 * u.1, d.0 * v.0 + d.1 * v.1);
    let at = |a: f64, b: f64| (start.0 + u.0 * a + v.0 * b, start.1 + u.1 * a + v.1 * b);
    let corners = [at(0.0, 0.0), at(du, 0.0), at(du, dv), at(0.0, dv)];
    if du * dv < 0.0 {
        [corners[0], corners[3], corners[2], corners[1]]
    } else {
        corners
    }
}

/// Snap `p` to the nearest of `candidates` within `tolerance`.
pub fn snap_point(p: Pt, candidates: &[Pt], tolerance: f64) -> Pt {
    candidates
        .iter()
        .copied()
        .map(|c| (c, (c.0 - p.0).hypot(c.1 - p.1)))
        .filter(|(_, d)| *d <= tolerance)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(p, |(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pairs: &[(&str, &str)]) -> Tags {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn h(min: f64, height: f64, roof: f64) -> Heights {
        Heights {
            min_height: min,
            height,
            roof_height: roof,
        }
    }

    #[test]
    fn drag_height_follows_the_cursor_up_the_wall() {
        let cam = OrbitCamera::framing(Vec3::ZERO, 10.0, 0.0);
        let size = glam::Vec2::new(800.0, 600.0);
        let anchor = Vec3::new(0.0, 0.0, 6.0);
        let p = cam.project(Vec3::new(0.0, 0.0, 10.0), size, 100.0).unwrap();
        let z = drag_height(&cam.ray(p, size, 100.0), anchor, &cam).unwrap();
        assert!((z - 10.0).abs() < 0.01, "{z}");
    }

    #[test]
    fn snapping() {
        assert_eq!(snap_height(12.34, &[], 0.5, true), 12.3);
        assert_eq!(snap_height(12.34, &[12.0, 20.0], 0.5, true), 12.0);
        assert_eq!(snap_height(12.34, &[12.0], 0.5, false), 12.3);
        assert_eq!(snap_height(12.34, &[13.0], 0.5, true), 12.3);
    }

    #[test]
    fn vertical_edits_are_clamped() {
        let base = h(3.0, 10.0, 0.0);
        assert_eq!(apply_vertical(&base, VerticalEdit::Top, 15.0).height, 15.0);
        assert_eq!(
            apply_vertical(&base, VerticalEdit::Top, 1.0).height,
            3.0 + MIN_THICKNESS_M
        );
        assert_eq!(
            apply_vertical(&base, VerticalEdit::Bottom, -2.0).min_height,
            0.0
        );
        assert_eq!(
            apply_vertical(&base, VerticalEdit::Bottom, 50.0).min_height,
            10.0 - MIN_THICKNESS_M
        );
        let lifted = apply_vertical(&base, VerticalEdit::Lift, 5.0);
        assert_eq!((lifted.min_height, lifted.height), (5.0, 12.0));
        let grounded = apply_vertical(&base, VerticalEdit::Lift, -5.0);
        assert_eq!((grounded.min_height, grounded.height), (0.0, 7.0));
    }

    #[test]
    fn retag_writes_metres_and_rescales_levels() {
        let before = t(&[
            ("building:part", "yes"),
            ("building:levels", "4"),
            ("height", "12"),
        ]);
        let old = h(0.0, 12.0, 0.0);
        let after = retag_heights(&before, &old, &h(0.0, 18.0, 0.0));
        assert_eq!(after["height"], "18");
        assert_eq!(after["building:levels"], "6");
        assert!(!after.contains_key("min_height"));
        let raised = retag_heights(&before, &old, &h(6.0, 18.0, 0.0));
        assert_eq!(raised["min_height"], "6");
        assert_eq!(raised["building:min_level"], "2");
        // Back to the ground drops the min tags.
        let lowered = retag_heights(&raised, &h(6.0, 18.0, 0.0), &h(0.0, 18.0, 0.0));
        assert!(!lowered.contains_key("min_height"));
        assert!(!lowered.contains_key("building:min_level"));
    }

    #[test]
    fn retag_leaves_levels_alone_when_absent() {
        let before = t(&[("building", "yes")]);
        let after = retag_heights(&before, &h(0.0, 6.0, 0.0), &h(0.0, 9.5, 0.0));
        assert_eq!(after["height"], "9.5");
        assert!(!after.contains_key("building:levels"));
    }

    #[test]
    fn part_tags_from_outline_keep_only_shape_keys() {
        let outline = t(&[
            ("building", "school"),
            ("name", "Main Hall"),
            ("height", "12"),
            ("roof:shape", "hipped"),
            ("building:colour", "red"),
        ]);
        let part = part_tags_from(&outline, true);
        assert_eq!(part["building:part"], "yes");
        assert_eq!(part["height"], "12");
        assert_eq!(part["roof:shape"], "hipped");
        assert_eq!(part["building:colour"], "red");
        assert!(!part.contains_key("name"));
        assert!(!part.contains_key("building"));
        let copy = part_tags_from(&outline, false);
        assert_eq!(copy, outline);
    }

    #[test]
    fn split_at_height_stacks_two_parts() {
        let tags_in = t(&[
            ("building:part", "yes"),
            ("height", "20"),
            ("roof:shape", "pyramidal"),
            ("roof:height", "4"),
        ]);
        let heights = h(0.0, 20.0, 4.0);
        let (lower, upper) = split_at_height(&tags_in, &heights, 10.0).unwrap();
        assert_eq!(lower["height"], "10");
        assert!(!lower.contains_key("roof:shape"));
        assert!(!lower.contains_key("min_height"));
        assert_eq!(upper["min_height"], "10");
        assert_eq!(upper["height"], "20");
        assert_eq!(upper["roof:shape"], "pyramidal");
        // Outside the walls (the roof starts at 16).
        assert!(split_at_height(&tags_in, &heights, 17.0).is_none());
        assert!(split_at_height(&tags_in, &heights, 0.0).is_none());
    }

    #[test]
    fn diff_lists_changed_keys_only() {
        let a = t(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let b = t(&[("a", "1"), ("b", "5"), ("d", "4")]);
        assert_eq!(
            diff_tags(&a, &b),
            vec![
                ("b".into(), Some("2".into()), Some("5".into())),
                ("c".into(), Some("3".into()), None),
                ("d".into(), None, Some("4".into())),
            ]
        );
    }

    fn square() -> Vec<Pt> {
        vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]
    }

    fn resolve(s: &RingSplit, ring: &[Pt], piece: &[RingVertex]) -> Vec<Pt> {
        piece
            .iter()
            .map(|v| match *v {
                RingVertex::Existing(i) => ring[i],
                RingVertex::New(k) => s.new_points[k].0,
            })
            .collect()
    }

    #[test]
    fn split_square_down_the_middle() {
        let ring = square();
        let s = split_ring(&ring, (5.0, -1.0), (5.0, 11.0)).unwrap();
        assert_eq!(s.new_points, vec![((5.0, 0.0), 0), ((5.0, 10.0), 2)]);
        let a = resolve(&s, &ring, &s.a);
        let b = resolve(&s, &ring, &s.b);
        assert_eq!(a, vec![(5.0, 0.0), (10.0, 0.0), (10.0, 10.0), (5.0, 10.0)]);
        assert_eq!(b, vec![(5.0, 10.0), (0.0, 10.0), (0.0, 0.0), (5.0, 0.0)]);
        let area = |r: &[Pt]| super::super::model::signed_area(r).abs();
        assert!((area(&a) + area(&b) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn split_through_corners_reuses_vertices() {
        let ring = square();
        let s = split_ring(&ring, (0.0, 0.0), (10.0, 10.0)).unwrap();
        assert!(s.new_points.is_empty());
        assert_eq!(
            s.a,
            vec![
                RingVertex::Existing(0),
                RingVertex::Existing(1),
                RingVertex::Existing(2)
            ]
        );
        assert_eq!(
            s.b,
            vec![
                RingVertex::Existing(2),
                RingVertex::Existing(3),
                RingVertex::Existing(0)
            ]
        );
    }

    #[test]
    fn split_that_misses_or_grazes_fails() {
        let ring = square();
        assert_eq!(
            split_ring(&ring, (20.0, 0.0), (20.0, 10.0)),
            Err(SplitError::NoCrossing)
        );
        // Along an edge.
        assert_eq!(
            split_ring(&ring, (0.0, 0.0), (10.0, 0.0)),
            Err(SplitError::NoCrossing)
        );
        // Touching one corner.
        assert_eq!(
            split_ring(&ring, (-5.0, 5.0), (5.0, -5.0)),
            Err(SplitError::NoCrossing)
        );
    }

    #[test]
    fn split_of_a_u_shape_across_both_arms_is_refused() {
        let u = vec![
            (0.0, 0.0),
            (30.0, 0.0),
            (30.0, 20.0),
            (20.0, 20.0),
            (20.0, 10.0),
            (10.0, 10.0),
            (10.0, 20.0),
            (0.0, 20.0),
        ];
        assert_eq!(
            split_ring(&u, (-1.0, 15.0), (31.0, 15.0)),
            Err(SplitError::TooManyCrossings)
        );
        // Across the base only: fine.
        assert!(split_ring(&u, (-1.0, 5.0), (31.0, 5.0)).is_ok());
    }

    #[test]
    fn rectangle_is_aligned_and_ccw() {
        let r = rectangle((0.0, 0.0), (4.0, 3.0), 0.0);
        assert_eq!(r, [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)]);
        let r = rectangle((0.0, 0.0), (4.0, -3.0), 0.0);
        assert!(super::super::model::signed_area(&r) > 0.0);
        let rot = rectangle((0.0, 0.0), (0.0, 2.0), std::f64::consts::FRAC_PI_4);
        let area = super::super::model::signed_area(&rot);
        assert!((area - 2.0).abs() < 1e-9, "{area}");
    }

    #[test]
    fn dominant_angle_and_point_snap() {
        let ring = vec![(0.0, 0.0), (10.0, 10.0), (9.0, 10.0)];
        assert!((dominant_angle(&ring) - std::f64::consts::FRAC_PI_4).abs() < 1e-9);
        assert_eq!(
            snap_point((0.2, 0.1), &[(0.0, 0.0), (5.0, 5.0)], 0.5),
            (0.0, 0.0)
        );
        assert_eq!(snap_point((2.0, 2.0), &[(0.0, 0.0)], 0.5), (2.0, 2.0));
    }
}
