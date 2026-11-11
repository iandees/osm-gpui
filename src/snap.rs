//! Snapping a node onto an existing way or node ("join node to way").
//!
//! # Design note
//!
//! What already existed: Add mode snaps a placed node onto the nearest way
//! segment (spliced in as a new vertex, so it becomes shared) and, once a way
//! is being drawn, onto an existing node (reused). `Ctrl` bypassed both. What
//! was missing, and what this module (with its callers in `main.rs` and
//! `selection_actions.rs`) adds:
//!
//! 1. **Dragging**: releasing a single dragged node within the pixel
//!    tolerance of a way segment inserts the node into that way at the right
//!    index (and moves it onto the line); releasing it on another node merges
//!    the two (the same planner as Merge Nodes, so tag conflicts and relation
//!    memberships are handled identically).
//! 2. **Tag compatibility** ([`may_snap`]): in dense areas an unrelated way
//!    must not be glued on by accident, so a feature only snaps to a way of a
//!    compatible kind (a building to a building, a waterway to a waterway,
//!    ...). The whole table is [`COMPAT`] below.
//! 3. **A modifier that turns snapping off**: [`snapping_disabled`]. `Alt` /
//!    `Option` is the documented key (it is free in Select and Add mode;
//!    Extrude uses it for a different purpose and never snaps). `Ctrl` keeps
//!    working for Add mode as before, although on macOS Ctrl-click is a
//!    secondary click, which is why `Alt` is the one the UI hints name.
//! 4. **Feedback**: while snapping would apply, the target segment (or node)
//!    and the snap point are highlighted; the click / release then does
//!    exactly what the highlight showed.
//! 5. **Join Node to Way** (selection action): select a node and a way to
//!    join them explicitly. It is deliberate, so it skips the compatibility
//!    check.
//!
//! Conservative choices (fewer accidental snaps): a way that already contains
//! the node is never a target (no node twice in a way, no consecutive
//! duplicates); a target with no recognisable tags only accepts an untagged
//! source; a tagged point of interest never snaps to a way. A way being drawn
//! in Add mode has no tags until the user gives it some, so it snaps to
//! anything (and only the disable modifier stops it).

use crate::geometry::{LatLon, LocalProjection};

/// Pixel distance within which a pointer snaps onto a way segment.
pub const WAY_SNAP_TOLERANCE_PX: f32 = 6.0;

/// Whether the held modifiers turn snapping off for this click / drag.
pub fn snapping_disabled(alt: bool, control: bool) -> bool {
    alt || control
}

/// What kind of thing a feature is, for deciding what may be glued to what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapClass {
    Highway,
    Railway,
    Waterway,
    Building,
    /// Land-use style areas: `landuse`, `natural`, `leisure`, `amenity`,
    /// `tourism`, `place`. They share borders in ordinary mapping.
    Area,
    Barrier,
    Power,
    Aeroway,
    Boundary,
    Coastline,
    /// Tagged with something we have no rule for.
    Other,
}

/// Which tag classifies a feature, first match wins. A `None` value list
/// matches any value of the key.
const CLASS_RULES: &[(&str, Option<&[&str]>, SnapClass)] = &[
    ("highway", None, SnapClass::Highway),
    ("railway", None, SnapClass::Railway),
    ("waterway", None, SnapClass::Waterway),
    ("building", None, SnapClass::Building),
    ("building:part", None, SnapClass::Building),
    ("entrance", None, SnapClass::Building),
    ("barrier", None, SnapClass::Barrier),
    ("power", None, SnapClass::Power),
    ("aeroway", None, SnapClass::Aeroway),
    ("boundary", None, SnapClass::Boundary),
    ("natural", Some(&["coastline"]), SnapClass::Coastline),
    ("landuse", None, SnapClass::Area),
    ("natural", None, SnapClass::Area),
    ("leisure", None, SnapClass::Area),
    ("amenity", None, SnapClass::Area),
    ("tourism", None, SnapClass::Area),
    ("place", None, SnapClass::Area),
];

/// Tags that say nothing about what a feature is.
const IGNORED_KEYS: &[&str] = &[
    "name",
    "ref",
    "alt_name",
    "old_name",
    "source",
    "note",
    "fixme",
    "FIXME",
    "comment",
    "description",
    "created_by",
    "layer",
    "level",
    "wikidata",
    "wikipedia",
    "check_date",
    "start_date",
    "area",
];

/// Which classes may be glued together. Symmetric: a pair is allowed if it
/// appears in either order (a class always matches itself). Edit here to
/// change the rules.
const COMPAT: &[(SnapClass, SnapClass)] = &[
    // Water flows into and along water.
    (SnapClass::Waterway, SnapClass::Coastline),
    // Pairs involving `Area` and `Barrier`/`Building` are deliberately NOT
    // listed: a fence along a park or a shed in a garden are separate shapes.
    // (Highway, Railway) is handled by `is_crossing_node`.
];

fn ignored(key: &str) -> bool {
    IGNORED_KEYS.contains(&key)
        || key.starts_with("name:")
        || key.starts_with("source:")
        || key.starts_with("note:")
        || key.starts_with("fixme")
}

/// The class of a feature with these tags, or `None` if it has nothing but
/// ignorable tags (such as an untagged way).
pub fn classify(tags: &[(String, String)]) -> Option<SnapClass> {
    for &(key, values, class) in CLASS_RULES {
        if tags
            .iter()
            .any(|(k, v)| k == key && values.is_none_or(|vs| vs.contains(&v.as_str())))
        {
            return Some(class);
        }
    }
    tags.iter()
        .any(|(k, _)| !ignored(k))
        .then_some(SnapClass::Other)
}

/// A node that stands for a road / rail crossing, which may join the two.
fn is_crossing_node(tags: &[(String, String)]) -> bool {
    tags.iter()
        .any(|(k, v)| k == "railway" && (v == "level_crossing" || v == "crossing"))
}

fn classes_compatible(a: SnapClass, b: SnapClass, crossing: bool) -> bool {
    a == b
        || COMPAT
            .iter()
            .any(|&(x, y)| (x, y) == (a, b) || (y, x) == (a, b))
        || (crossing
            && matches!(
                (a, b),
                (SnapClass::Highway, SnapClass::Railway) | (SnapClass::Railway, SnapClass::Highway)
            ))
}

/// Whether a node whose own tags and whose ways' tags are `sources` may snap
/// onto a way with tags `target`.
///
/// Rules:
/// - If nothing in `sources` is recognisable (an untagged node on an untagged
///   or still-being-drawn way), anything may be snapped to.
/// - Otherwise every source class must be compatible with the target's
///   class, and a target with no class is refused (it could be anything).
/// - A tagged node with no way of its own (a point of interest) is a source
///   of class `Other` unless its tags are a recognised vertex kind, so it
///   only matches `Other` ways: it will not be glued to roads or buildings.
pub fn may_snap(sources: &[&[(String, String)]], target: &[(String, String)]) -> bool {
    let classes: Vec<SnapClass> = sources.iter().filter_map(|t| classify(t)).collect();
    if classes.is_empty() {
        return true;
    }
    let crossing = sources.iter().any(|t| is_crossing_node(t));
    match classify(target) {
        Some(tc) => classes
            .iter()
            .all(|&sc| classes_compatible(sc, tc, crossing)),
        None => false,
    }
}

/// Where a node would be inserted into a way, found from geometry alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JoinPlan {
    /// Index in the way's node list to insert at.
    pub index: usize,
    /// The point on the way's line the node is moved to.
    pub point: LatLon,
    /// How far the node had to move, in metres.
    pub distance_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// The way has fewer than two nodes, so no segment exists.
    NoSegment,
    /// The node is already one of the way's nodes.
    AlreadyInWay,
    /// The segment index is out of range.
    BadSegment,
}

/// The point on segment `a`-`b` nearest `p`, and its distance in metres.
fn nearest_on_segment(p: LatLon, a: LatLon, b: LatLon) -> (LatLon, f64) {
    let proj = LocalProjection::new(p);
    let (ax, ay) = proj.project(a);
    let (bx, by) = proj.project(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    // `p` is the projection origin, i.e. (0, 0).
    let t = if len2 == 0.0 {
        0.0
    } else {
        ((-ax * dx - ay * dy) / len2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (ax + t * dx, ay + t * dy);
    (proj.unproject((qx, qy)), qx.hypot(qy))
}

/// Plan inserting `node` into the way `way_nodes` / `points` (parallel
/// lists) on the segment starting at index `segment`.
pub fn plan_join_at_segment(
    node: i64,
    position: LatLon,
    way_nodes: &[i64],
    points: &[LatLon],
    segment: usize,
) -> Result<JoinPlan, JoinError> {
    if way_nodes.contains(&node) {
        return Err(JoinError::AlreadyInWay);
    }
    if way_nodes.len() < 2 || points.len() != way_nodes.len() {
        return Err(JoinError::NoSegment);
    }
    if segment + 1 >= points.len() {
        return Err(JoinError::BadSegment);
    }
    let (point, distance_m) = nearest_on_segment(position, points[segment], points[segment + 1]);
    Ok(JoinPlan {
        index: segment + 1,
        point,
        distance_m,
    })
}

/// Plan inserting `node` at the nearest segment of the way. A way that passes
/// the same distance from the node twice (a crossing or touching itself) is
/// resolved to the lowest-indexed segment, deterministically.
pub fn plan_join_nearest(
    node: i64,
    position: LatLon,
    way_nodes: &[i64],
    points: &[LatLon],
) -> Result<JoinPlan, JoinError> {
    if way_nodes.contains(&node) {
        return Err(JoinError::AlreadyInWay);
    }
    if way_nodes.len() < 2 || points.len() != way_nodes.len() {
        return Err(JoinError::NoSegment);
    }
    let mut best: Option<(usize, f64)> = None;
    for i in 0..points.len() - 1 {
        let (_, d) = nearest_on_segment(position, points[i], points[i + 1]);
        // A later segment must be clearly closer to win (1 nm slack).
        if best.is_none_or(|(_, bd)| d < bd - 1e-9) {
            best = Some((i, d));
        }
    }
    let (segment, _) = best.ok_or(JoinError::NoSegment)?;
    plan_join_at_segment(node, position, way_nodes, points, segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn ok(sources: &[&[(&str, &str)]], target: &[(&str, &str)]) -> bool {
        let owned: Vec<Vec<(String, String)>> = sources.iter().map(|s| t(s)).collect();
        let refs: Vec<&[(String, String)]> = owned.iter().map(|v| v.as_slice()).collect();
        may_snap(&refs, &t(target))
    }

    #[test]
    fn modifiers_disable_snapping() {
        assert!(snapping_disabled(true, false));
        assert!(snapping_disabled(false, true));
        assert!(!snapping_disabled(false, false));
    }

    #[test]
    fn classification_uses_the_first_matching_rule() {
        assert_eq!(
            classify(&t(&[("highway", "service")])),
            Some(SnapClass::Highway)
        );
        assert_eq!(
            classify(&t(&[("building", "yes"), ("amenity", "cafe")])),
            Some(SnapClass::Building)
        );
        assert_eq!(
            classify(&t(&[("natural", "coastline")])),
            Some(SnapClass::Coastline)
        );
        assert_eq!(classify(&t(&[("natural", "wood")])), Some(SnapClass::Area));
        assert_eq!(classify(&t(&[("shop", "bakery")])), Some(SnapClass::Other));
        assert_eq!(classify(&t(&[("name", "Elm"), ("source", "survey")])), None);
        assert_eq!(classify(&[]), None);
    }

    #[test]
    fn untagged_source_snaps_to_anything() {
        assert!(ok(&[], &[("highway", "residential")]));
        assert!(ok(&[&[], &[("name", "x")]], &[("landuse", "forest")]));
        assert!(ok(&[&[]], &[]));
    }

    #[test]
    fn like_snaps_to_like() {
        assert!(ok(
            &[&[("highway", "residential")]],
            &[("highway", "service")]
        ));
        assert!(ok(&[&[("building", "yes")]], &[("building", "house")]));
        assert!(ok(&[&[("waterway", "stream")]], &[("waterway", "river")]));
        assert!(ok(
            &[&[("landuse", "grass")]],
            &[("landuse", "residential")]
        ));
        assert!(ok(&[&[("barrier", "fence")]], &[("barrier", "wall")]));
    }

    #[test]
    fn landuse_snaps_to_natural_and_leisure_areas() {
        assert!(ok(&[&[("landuse", "forest")]], &[("natural", "water")]));
        assert!(ok(&[&[("leisure", "park")]], &[("landuse", "residential")]));
    }

    #[test]
    fn unlike_kinds_do_not_snap() {
        assert!(!ok(
            &[&[("landuse", "forest")]],
            &[("highway", "residential")]
        ));
        assert!(!ok(
            &[&[("highway", "residential")]],
            &[("building", "yes")]
        ));
        assert!(!ok(
            &[&[("building", "yes")]],
            &[("landuse", "residential")]
        ));
        assert!(!ok(&[&[("waterway", "stream")]], &[("highway", "path")]));
        assert!(!ok(&[&[("barrier", "fence")]], &[("landuse", "farmland")]));
        assert!(!ok(&[&[("power", "line")]], &[("highway", "primary")]));
    }

    #[test]
    fn highway_meets_railway_only_at_a_crossing_node() {
        let road = [("highway", "residential")];
        let rail = [("railway", "rail")];
        assert!(!ok(&[&road], &rail));
        assert!(ok(&[&road, &[("railway", "level_crossing")]], &rail));
        assert!(ok(&[&[("railway", "crossing")], &rail], &road));
    }

    #[test]
    fn waterway_may_end_on_the_coastline() {
        assert!(ok(&[&[("waterway", "river")]], &[("natural", "coastline")]));
        assert!(ok(&[&[("natural", "coastline")]], &[("waterway", "river")]));
    }

    #[test]
    fn a_node_on_several_ways_must_suit_all_of_them() {
        let road = [("highway", "residential")];
        let fence = [("barrier", "fence")];
        assert!(!ok(&[&road, &fence], &[("highway", "service")]));
        assert!(ok(&[&road, &[]], &[("highway", "service")]));
    }

    #[test]
    fn tagged_source_refuses_an_unclassified_target() {
        assert!(!ok(&[&[("highway", "path")]], &[]));
        assert!(!ok(&[&[("highway", "path")]], &[("name", "Back lane")]));
    }

    #[test]
    fn point_of_interest_is_not_glued_to_roads_or_buildings() {
        let shop = [("shop", "bakery")];
        assert!(!ok(&[&shop], &[("highway", "residential")]));
        assert!(!ok(&[&shop], &[("building", "yes")]));
    }

    #[test]
    fn entrance_snaps_to_a_building_wall_only() {
        let entrance = [("entrance", "yes")];
        assert!(ok(&[&entrance], &[("building", "house")]));
        assert!(!ok(&[&entrance], &[("highway", "footway")]));
    }

    // Geometry. Points near 0N 0E: 0.001 degree is about 111 m.
    fn line() -> (Vec<i64>, Vec<LatLon>) {
        (
            vec![1, 2, 3, 4],
            vec![(0.0, 0.0), (0.0, 0.002), (0.002, 0.002), (0.002, 0.0)],
        )
    }

    #[test]
    fn insertion_index_is_after_the_segments_start() {
        let (nodes, pts) = line();
        let plan = plan_join_nearest(9, (0.0011, 0.0021), &nodes, &pts).unwrap();
        assert_eq!(plan.index, 2);
        assert!((plan.point.1 - 0.002).abs() < 1e-9);
        assert!((plan.point.0 - 0.0011).abs() < 1e-6);
        let first = plan_join_nearest(9, (-0.0001, 0.001), &nodes, &pts).unwrap();
        assert_eq!(first.index, 1);
        let last = plan_join_nearest(9, (0.003, 0.001), &nodes, &pts).unwrap();
        assert_eq!(last.index, 3);
    }

    #[test]
    fn beyond_the_end_clamps_to_the_end_point() {
        let (nodes, pts) = line();
        let plan = plan_join_nearest(9, (0.003, 0.0), &nodes, &pts).unwrap();
        assert_eq!(plan.index, 3);
        assert!((plan.point.0 - 0.002).abs() < 1e-9 && plan.point.1.abs() < 1e-9);
    }

    #[test]
    fn a_way_passing_the_position_twice_resolves_to_the_first_segment() {
        // 1 -> 2 runs east, 3 -> 4 runs back west over the same line.
        let nodes = vec![1, 2, 3, 4];
        let pts = vec![(0.0, 0.0), (0.0, 0.002), (0.0, 0.002), (0.0, 0.0)];
        let plan = plan_join_nearest(9, (0.0001, 0.001), &nodes, &pts).unwrap();
        assert_eq!(plan.index, 1);
    }

    #[test]
    fn closed_way_includes_the_closing_segment() {
        let nodes = vec![1, 2, 3, 4, 1];
        let pts = vec![
            (0.0, 0.0),
            (0.0, 0.002),
            (0.002, 0.002),
            (0.002, 0.0),
            (0.0, 0.0),
        ];
        let plan = plan_join_nearest(9, (0.001, -0.0001), &nodes, &pts).unwrap();
        assert_eq!(plan.index, 4);
    }

    #[test]
    fn a_node_already_in_the_way_is_refused() {
        let (nodes, pts) = line();
        assert_eq!(
            plan_join_nearest(2, (0.0, 0.0), &nodes, &pts),
            Err(JoinError::AlreadyInWay)
        );
        assert_eq!(
            plan_join_at_segment(3, (0.0, 0.0), &nodes, &pts, 0),
            Err(JoinError::AlreadyInWay)
        );
    }

    #[test]
    fn degenerate_ways_are_refused() {
        assert_eq!(
            plan_join_nearest(9, (0.0, 0.0), &[1], &[(0.0, 0.0)]),
            Err(JoinError::NoSegment)
        );
        let (nodes, pts) = line();
        assert_eq!(
            plan_join_at_segment(9, (0.0, 0.0), &nodes, &pts, 3),
            Err(JoinError::BadSegment)
        );
    }
}
