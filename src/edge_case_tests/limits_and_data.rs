//! Further situations (batch C): the server's size limits (2,000 nodes per
//! way, 32,000 members per relation), geometry at the antimeridian and near
//! the poles, and Reverse on tag keys and values with unusual content.

use super::*;
use crate::{ExtrudeDrag, ExtrudeStyle};
use gpui::point;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

fn with_nodes(mut b: Build, ids: impl IntoIterator<Item = i64>) -> Build {
    for id in ids {
        b = b.node(id, &[]);
    }
    b
}

// ---------------------------------------------------------------------------
// 1. The 2,000 nodes per way limit
// ---------------------------------------------------------------------------

#[gpui::test]
fn merge_between_two_full_ways_keeps_both_at_the_limit(cx: &mut TestAppContext) {
    // Merging replaces one node by another, so no way grows: two ways of
    // exactly 2,000 nodes stay at 2,000 and nothing is refused.
    let b = with_nodes(Build::new(), 1..=4000);
    let data = b
        .way(10, &(1..=2000).collect::<Vec<_>>(), &road())
        .way(11, &(2001..=4000).collect::<Vec<_>>(), &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1000), (N, 3000)]);
        assert!(s.act("merge"), "{}", s.status());
        assert_eq!(s.way_nodes(10).unwrap().len(), 2000);
        assert_eq!(s.way_nodes(11).unwrap().len(), 2000);
        assert!(s.way_nodes(11).unwrap().contains(&1000));
        assert!(!s.has_node(3000));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merge_inside_a_full_way_never_grows_it(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=2000).collect();
    let data = with_nodes(Build::new(), 1..=2000)
        .way(10, &ids, &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Far apart: the way keeps its length (the node is simply visited
        // twice).
        s.select(&[(N, 100), (N, 1500)]);
        assert!(s.act("merge"), "{}", s.status());
        let after = s.way_nodes(10).unwrap();
        assert_eq!(after.len(), 2000);
        assert_eq!(after.iter().filter(|&&n| n == 100).count(), 2);
        undo_redo(s, &before);

        // Neighbours collapse into one: the way shrinks to 1,999.
        s.select(&[(N, 700), (N, 701)]);
        assert!(s.act("merge"), "{}", s.status());
        assert_eq!(s.way_nodes(10).unwrap().len(), 1999);
    });
}

#[gpui::test]
fn merging_the_ends_of_a_full_open_way_closes_it_without_growing(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=2000).collect();
    let data = with_nodes(Build::new(), 1..=2000)
        .way(10, &ids, &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2000)]);
        assert!(s.act("merge"), "{}", s.status());
        let after = s.way_nodes(10).unwrap();
        assert_eq!(after.len(), 2000);
        assert_eq!(after.first(), after.last());
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_vertex_of_a_way_over_the_limit_shortens_it(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=2001).collect();
    let data = with_nodes(Build::new(), 1..=2001)
        .way(10, &ids, &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1000)]);
        assert!(s.act("delete"));
        assert_eq!(s.way_nodes(10).unwrap().len(), 2000);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_right_at_the_limit_then_undo_redo_and_one_more_node_is_refused(cx: &mut TestAppContext) {
    // Way 10 has 1,000 nodes, way 11 has 1,001 and runs the other way; they
    // share node 1000, so the join is exactly 2,000 nodes and has to reverse
    // way 11.
    let data = with_nodes(Build::new(), (1..=2000).chain([5001]))
        .way(10, &(1..=1000).collect::<Vec<_>>(), &road())
        .way(11, &(1000..=2000).rev().collect::<Vec<_>>(), &road())
        .way(12, &[2000, 5001], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"), "{}", s.status());
        assert_eq!(s.way_nodes(10).unwrap().len(), 2000);
        assert!(!s.has_way(11));
        let joined = s.snap();
        // Way 10 is modified, way 11 deleted.
        assert_eq!(s.counts(), (0, 1, 1));

        s.undo();
        assert_eq!(s.snap(), before, "undo splits the joined way back apart");
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(s.way_nodes(10).unwrap().len(), 1000);
        assert_eq!(s.way_nodes(11).unwrap().len(), 1001);
        s.redo();
        assert_eq!(s.snap(), joined);

        // The joined way is full: one more node (way 12) is refused, and the
        // refusal records nothing.
        let steps = s.steps();
        s.select(&[(W, 10), (W, 12)]);
        assert!(!s.act("join"));
        assert!(s.status().contains("2,001"), "{}", s.status());
        assert_eq!(s.steps(), steps);
        assert_eq!(s.snap(), joined);
    });
}

// ---------------------------------------------------------------------------
// 2. Relations near the 32,000 member limit
// ---------------------------------------------------------------------------

/// A route relation 900 of `total` members: way 10, way 11 (if asked) and
/// then stops that are not loaded.
fn route_of(total: usize, ways: &[i64]) -> OsmData {
    let mut members: Vec<(MemberType, i64, &str)> = ways.iter().map(|&w| wm(w, "")).collect();
    while members.len() < total {
        members.push(nm(10_001 + members.len() as i64, "stop"));
    }
    assert_eq!(members.len(), total);
    Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &[("highway", "primary")])
        .way(11, &[3, 4, 5], &[("highway", "primary")])
        .relation(900, &[("type", "route"), ("route", "bus")], &members)
        .done()
}

#[gpui::test]
fn split_that_would_push_a_relation_past_32000_is_refused_and_changes_nothing(
    cx: &mut TestAppContext,
) {
    with_session(cx, route_of(32_000, &[10, 11]), |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("split"));
        assert!(s.status().contains("32,001"), "{}", s.status());
        assert!(s.status().contains("32,000"), "{}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn split_that_lands_exactly_on_32000_members_goes_through(cx: &mut TestAppContext) {
    with_session(cx, route_of(31_999, &[10, 11]), |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(s.act("split"), "{}", s.status());
        assert_eq!(s.members(900).len(), 32_000);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_way_listed_twice_needs_room_for_both_new_halves(cx: &mut TestAppContext) {
    // The way is a member twice (out and back), so a split adds two members:
    // 31,999 + 2 is over the limit although 31,999 + 1 would not be.
    with_session(cx, route_of(31_999, &[10, 11, 10]), |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("split"));
        assert!(s.status().contains("32,001"), "{}", s.status());
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn one_full_relation_blocks_the_split_for_every_relation_the_way_is_in(cx: &mut TestAppContext) {
    // Way 10 is in the full relation 900 and in the small relation 901.
    // Refusing for 900 must leave 901 untouched too (no half-done split).
    let mut data = route_of(32_000, &[10, 11]);
    data.relations.push(OsmRelation {
        id: 901,
        version: 1,
        tags: [("type", "route"), ("route", "bicycle")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        members: vec![OsmMember {
            member_type: MemberType::Way,
            reference: 10,
            role: String::new(),
        }],
    });
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.members(901), mem(&[(10, "")]));
        assert!(s.new_ways().is_empty());
    });
}

#[gpui::test]
fn a_relation_already_over_the_limit_still_allows_edits_that_do_not_grow_it(
    cx: &mut TestAppContext,
) {
    // 32,001 members (loaded from a server that allowed more once): Reverse
    // does not add members, so it must not be refused for the size.
    with_session(cx, route_of(32_001, &[10, 11]), |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"), "{}", s.status());
        assert_eq!(s.members(900).len(), 32_001);
    });
}

#[gpui::test]
fn join_inside_a_full_relation_keeps_it_within_the_limit(cx: &mut TestAppContext) {
    // Ways 10 and 11 are adjacent members: joining them removes one member.
    with_session(cx, route_of(32_000, &[10, 11]), |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"), "{}", s.status());
        assert_eq!(s.members(900).len(), 31_999);
        assert_eq!(s.members(900)[0], (10, String::new()));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merging_two_members_of_a_full_relation_does_not_grow_it(cx: &mut TestAppContext) {
    // Nodes 2 and 4 are members with different roles, so both memberships
    // are kept (M7): the count stays at the limit, not above it.
    let mut members: Vec<(MemberType, i64, &str)> = vec![nm(2, "a"), nm(4, "b")];
    while members.len() < 32_000 {
        members.push(nm(10_001 + members.len() as i64, "stop"));
    }
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .relation(900, &[("type", "site")], &members)
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4)]);
        assert!(s.act("merge"), "{}", s.status());
        assert!(s.members(900).len() <= 32_000);
        assert_eq!(s.members(900).len(), 32_000);
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 3. Antimeridian and polar latitudes
// ---------------------------------------------------------------------------

type Placed = Vec<(i64, (f64, f64))>;

const M_PER_DEG: f64 = 111_320.0;

/// Distance in metres, going the short way round the globe in longitude.
fn metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    let mut dlon = (b.1 - a.1) % 360.0;
    if dlon > 180.0 {
        dlon -= 360.0;
    } else if dlon < -180.0 {
        dlon += 360.0;
    }
    let mid_lat = ((a.0 + b.0) / 2.0).to_radians();
    let dx = dlon * mid_lat.cos() * M_PER_DEG;
    let dy = (b.0 - a.0) * M_PER_DEG;
    dx.hypot(dy)
}

fn wrap_lon(lon: f64) -> f64 {
    if lon > 180.0 {
        lon - 360.0
    } else if lon < -180.0 {
        lon + 360.0
    } else {
        lon
    }
}

/// `(east, north)` offsets in metres from `(lat, lon)` as positions.
fn offset(lat: f64, lon: f64, east: f64, north: f64) -> (f64, f64) {
    (
        lat + north / M_PER_DEG,
        wrap_lon(lon + east / (M_PER_DEG * lat.to_radians().cos())),
    )
}

fn put(mut b: Build, id: i64, pos: (f64, f64)) -> Build {
    b.data.as_mut().unwrap().nodes.insert(
        id,
        OsmNode {
            id,
            lat: pos.0,
            lon: pos.1,
            version: 3,
            tags: HashMap::new(),
        },
    );
    b
}

/// A closed way 10 of nodes 1-4 (a 40 m square, one corner pushed out by
/// 2 m so it is nearly but not exactly square) centred on `(lat, lon)`.
fn ring(lat: f64, lon: f64) -> (OsmData, Placed) {
    let pts = vec![
        (1, offset(lat, lon, -20.0, -20.0)),
        (2, offset(lat, lon, 20.0, -20.0)),
        (3, offset(lat, lon, 22.0, 20.0)),
        (4, offset(lat, lon, -20.0, 20.0)),
    ];
    let mut b = Build::new();
    for &(id, p) in &pts {
        b = put(b, id, p);
    }
    (
        b.way(10, &[1, 2, 3, 4, 1], &[("building", "yes")]).done(),
        pts,
    )
}

/// Four nodes 20 m apart on a roughly east-west line (wobbling by 1 m),
/// starting 30 m west of `(lat, lon)`. Way 10 holds them so they exist.
fn line(lat: f64, lon: f64, east: [f64; 4]) -> (OsmData, Placed) {
    let north = [0.0, 1.0, -1.0, 0.0];
    let pts: Vec<(i64, (f64, f64))> = (0..4)
        .map(|i| (i as i64 + 1, offset(lat, lon, east[i], north[i])))
        .collect();
    let mut b = Build::new();
    for &(id, p) in &pts {
        b = put(b, id, p);
    }
    (b.way(10, &[1, 2, 3, 4], &road()).done(), pts)
}

fn assert_sane(s: &Session, pts: &[(i64, (f64, f64))], max_move_m: f64, what: &str) {
    for &(id, before) in pts {
        let after = s.editable().node_lat_lon(id).unwrap();
        assert!(
            after.0.is_finite() && after.1.is_finite(),
            "{what}: node {id} became {after:?}"
        );
        assert!(
            (-90.0..=90.0).contains(&after.0) && (-180.0..=180.0).contains(&after.1),
            "{what}: node {id} left the valid coordinate range: {after:?}"
        );
        let moved = metres(before, after);
        assert!(
            moved <= max_move_m,
            "{what}: node {id} moved {moved:.1} m ({before:?} to {after:?}), more than {max_move_m} m"
        );
    }
}

fn ring_action(cx: &mut TestAppContext, action: &'static str, lat: f64, lon: f64, max: f64) {
    let (data, pts) = ring(lat, lon);
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act(action), "{action} at {lat},{lon}: {}", s.status());
        assert_sane(s, &pts, max, action);
        undo_redo(s, &before);
    });
}

fn line_action(
    cx: &mut TestAppContext,
    action: &'static str,
    lat: f64,
    lon: f64,
    east: [f64; 4],
    max: f64,
) {
    let (data, pts) = line(lat, lon, east);
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2), (N, 3), (N, 4)]);
        assert!(s.act(action), "{action} at {lat},{lon}: {}", s.status());
        assert_sane(s, &pts, max, action);
        undo_redo(s, &before);
    });
}

const EVEN: [f64; 4] = [-30.0, -10.0, 10.0, 30.0];
const UNEVEN: [f64; 4] = [-30.0, -25.0, 15.0, 30.0];

#[gpui::test]
fn square_and_circularize_stay_sane_just_west_of_the_antimeridian(cx: &mut TestAppContext) {
    // 179.9995 E: the ring's east edge is a few metres short of 180.
    ring_action(cx, "square", 10.0, 179.9995, 5.0);
    ring_action(cx, "circularize", 10.0, 179.9995, 8.0);
}

#[gpui::test]
fn square_and_circularize_stay_sane_just_east_of_the_antimeridian(cx: &mut TestAppContext) {
    ring_action(cx, "square", 10.0, -179.9995, 5.0);
    ring_action(cx, "circularize", 10.0, -179.9995, 8.0);
}

#[gpui::test]
fn square_and_circularize_stay_sane_for_a_ring_that_straddles_the_antimeridian(
    cx: &mut TestAppContext,
) {
    ring_action(cx, "square", 10.0, 180.0, 5.0);
    ring_action(cx, "circularize", 10.0, 180.0, 8.0);
}

#[gpui::test]
fn square_and_circularize_stay_sane_near_the_pole(cx: &mut TestAppContext) {
    ring_action(cx, "square", 85.0, 20.0, 5.0);
    ring_action(cx, "circularize", 85.0, 20.0, 8.0);
    ring_action(cx, "square", -85.0, -120.0, 5.0);
    ring_action(cx, "circularize", -85.0, -120.0, 8.0);
}

#[gpui::test]
fn square_and_circularize_stay_sane_at_the_pole_and_the_antimeridian_together(
    cx: &mut TestAppContext,
) {
    ring_action(cx, "square", 85.0, 180.0, 5.0);
    ring_action(cx, "circularize", 85.0, 180.0, 8.0);
}

#[gpui::test]
fn align_and_distribute_stay_sane_near_the_antimeridian_without_crossing_it(
    cx: &mut TestAppContext,
) {
    line_action(cx, "align", 10.0, 179.9990, UNEVEN, 3.0);
    line_action(cx, "distribute", 10.0, 179.9990, UNEVEN, 25.0);
    line_action(cx, "align", 10.0, -179.9990, UNEVEN, 3.0);
    line_action(cx, "distribute", 10.0, -179.9990, UNEVEN, 25.0);
}

#[gpui::test]
fn align_and_distribute_stay_sane_near_the_pole(cx: &mut TestAppContext) {
    line_action(cx, "align", 85.0, 20.0, UNEVEN, 3.0);
    line_action(cx, "distribute", 85.0, 20.0, UNEVEN, 25.0);
    line_action(cx, "align", -85.0, 20.0, EVEN, 3.0);
}

#[gpui::test]
fn align_stays_sane_for_a_line_that_straddles_the_antimeridian(cx: &mut TestAppContext) {
    line_action(cx, "align", 10.0, 180.0, UNEVEN, 3.0);
}

#[gpui::test]
#[ignore = "bug: distribute on nodes either side of the antimeridian averages longitudes (about 0) and spreads them along a planet-sized line, moving nodes thousands of km; geometry::LocalProjection does not wrap longitude"]
fn distribute_stays_sane_for_a_line_that_straddles_the_antimeridian(cx: &mut TestAppContext) {
    line_action(cx, "distribute", 10.0, 180.0, UNEVEN, 25.0);
}

/// Extrude segment 1-2 of ring 10 by `dist` pixels, with the viewport
/// centred on the ring, and check what comes out.
fn extrude_at(cx: &mut TestAppContext, style: ExtrudeStyle, lat: f64, lon: f64) {
    let (data, pts) = ring(lat, lon);
    with_session(cx, data, move |s| {
        // Zoom 20 so the 40 m square is a few hundred pixels wide.
        s.view.viewport = Viewport::new(lat, lon, 20.0, size(px(800.0), px(600.0)));
        let screen = |s: &Session, id: i64| {
            let (la, lo) = s.editable().node_lat_lon(id).unwrap();
            s.view.viewport.geo_to_screen(la, lo)
        };
        let (a, b) = (screen(s, 1), screen(s, 2));
        let mid = point((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
        let len = dx.hypot(dy);
        assert!(len > 10.0, "segment is {len}px on screen");
        // Pull the segment out by 40 px, away from the ring.
        let up = point(mid.x + px(dy / len * 40.0), mid.y + px(-dx / len * 40.0));
        let drag = ExtrudeDrag {
            layer: s.layer,
            way_id: 10,
            node_a: 1,
            node_b: 2,
            segment: 0,
            style,
            down: mid,
        };
        match style {
            ExtrudeStyle::NewWay => s.view.commit_extrude(&drag, up),
            _ => s.view.commit_extrude_reshape(&drag, up),
        }
        assert_eq!(s.steps(), 1, "extrude at {lat},{lon} did nothing");
        let new_nodes: Vec<i64> = s
            .snap()
            .nodes
            .keys()
            .copied()
            .filter(|&id| id < 0)
            .collect();
        let mut all = pts.clone();
        for id in &new_nodes {
            all.push((*id, s.editable().node_lat_lon(*id).unwrap()));
        }
        // Existing nodes may move up to the drag distance (about 40 px, which
        // at zoom 20 is a few metres); new nodes must end up near the ring.
        for &(id, p) in &all {
            let now = s.editable().node_lat_lon(id).unwrap();
            assert!(now.0.is_finite() && now.1.is_finite(), "node {id}: {now:?}");
            assert!(
                (-90.0..=90.0).contains(&now.0) && (-180.0..=180.0).contains(&now.1),
                "extrude at {lat},{lon} put node {id} outside the valid range: {now:?}"
            );
            let origin = (lat, lon);
            assert!(
                metres(origin, now) < 100.0,
                "extrude at {lat},{lon} put node {id} {:.0} m away ({p:?} to {now:?})",
                metres(origin, now)
            );
        }
    });
}

#[gpui::test]
fn extrude_stays_sane_near_the_antimeridian_and_the_poles(cx: &mut TestAppContext) {
    for style in [
        ExtrudeStyle::Reshape,
        ExtrudeStyle::Move,
        ExtrudeStyle::NewWay,
    ] {
        extrude_at(cx, style, 10.0, 179.9995);
        extrude_at(cx, style, 10.0, -179.9995);
        extrude_at(cx, style, 85.0, 20.0);
        extrude_at(cx, style, -85.0, 20.0);
    }
}

#[gpui::test]
fn extrude_stays_sane_across_the_antimeridian(cx: &mut TestAppContext) {
    // The ring is centred on 180, so pulling its east side out lands east of
    // the antimeridian: the new longitudes must wrap into -180..180.
    for style in [ExtrudeStyle::Reshape, ExtrudeStyle::NewWay] {
        extrude_at(cx, style, 10.0, 180.0);
    }
}

// ---------------------------------------------------------------------------
// 4. Reverse: unusual tag keys and values
// ---------------------------------------------------------------------------

fn reverse_tags(
    cx: &mut TestAppContext,
    tags: &'static [(&'static str, &'static str)],
) -> Vec<(String, String)> {
    let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = out.clone();
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], tags)
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        *sink.borrow_mut() = s.tags(W, 10);
        undo_redo(s, &before);
    });
    let v = out.borrow().clone();
    v
}

#[gpui::test]
fn keys_that_only_look_like_forward_or_backward_are_not_swapped(cx: &mut TestAppContext) {
    // `lane:forward_x`, `forwards`, trailing space, upper case and a
    // look-alike letter (Cyrillic o) are different keys from `:forward`.
    let tags: &[(&str, &str)] = &[
        ("lane:forward_x", "2"),
        ("maxspeed:forwards", "30"),
        ("maxspeed:forward ", "40"),
        ("maxspeed:Forward", "50"),
        ("maxspeed:BACKWARD", "60"),
        ("lanes:f\u{43e}rward", "3"),
        ("xforward:y", "1"),
        ("forward", "yes"),
        ("left:turn", "yes"),
    ];
    let mut expected = pairs(tags);
    expected.sort();
    assert_eq!(reverse_tags(cx, tags), expected);
}

#[gpui::test]
fn exact_forward_and_backward_parts_swap_in_any_position_after_the_first(cx: &mut TestAppContext) {
    assert_eq!(
        reverse_tags(
            cx,
            &[
                ("lanes:forward", "2"),
                ("lanes:backward", "1"),
                ("turn:lanes:forward:x", "2"),
                ("a:forward:b:backward", "1"),
                ("sidewalk:both", "yes"),
            ]
        ),
        pairs(&[
            ("a:backward:b:forward", "1"),
            ("lanes:backward", "2"),
            ("lanes:forward", "1"),
            ("sidewalk:both", "yes"),
            ("turn:lanes:backward:x", "2"),
        ])
    );
}

#[gpui::test]
fn swapping_keys_never_loses_or_merges_tags(cx: &mut TestAppContext) {
    // Both halves of a pair, plus a lookalike that must stay a separate tag.
    let result = reverse_tags(
        cx,
        &[
            ("maxspeed:forward", "30"),
            ("maxspeed:backward", "50"),
            ("maxspeed:forwards", "70"),
            ("maxspeed", "40"),
        ],
    );
    assert_eq!(
        result,
        pairs(&[
            ("maxspeed", "40"),
            ("maxspeed:backward", "30"),
            ("maxspeed:forward", "50"),
            ("maxspeed:forwards", "70"),
        ])
    );
}

#[gpui::test]
fn values_with_other_case_spaces_or_an_s_are_not_taken_for_directions(cx: &mut TestAppContext) {
    // Pinned as is: only the exact lower-case words flip. iD also reads the
    // legacy `forwards` / `backwards` (R13); this app leaves them alone.
    let tags: &[(&str, &str)] = &[
        ("priority", "Forward"),
        ("trolley_wire", "forward "),
        ("railway:preferred_direction", "forwards"),
        ("sidewalk", "LEFT"),
        ("parking:lane", "right "),
        ("surface", "backwards"),
    ];
    let mut expected = pairs(tags);
    expected.sort();
    assert_eq!(reverse_tags(cx, tags), expected);
}

#[gpui::test]
fn directional_values_of_directional_keys_follow_the_same_exact_rule(cx: &mut TestAppContext) {
    let tags: &[(&str, &str)] = &[
        ("direction", "Forward"),
        ("direction", "forwards"),
        ("incline", "UP"),
        ("incline", "steep"),
        ("incline", "\u{ff11}\u{ff10}%"),
    ];
    // Same key twice is impossible in a tag map; test them one at a time.
    for tag in tags {
        let one: &'static [(&'static str, &'static str)] = Box::leak(Box::new([*tag]));
        assert_eq!(reverse_tags(cx, one), pairs(one), "{tag:?}");
    }
}

#[gpui::test]
fn unicode_keys_and_values_survive_reverse_untouched(cx: &mut TestAppContext) {
    let tags: &[(&str, &str)] = &[
        ("name", "Stra\u{df}e links"),
        ("name:ja", "\u{53f3}\u{5074}\u{901a}\u{884c}"),
        ("destination", "\u{4e0a}\u{6d77}"),
        ("note:forward", "\u{2192}"),
    ];
    assert_eq!(
        reverse_tags(cx, tags),
        pairs(&[
            ("destination", "\u{4e0a}\u{6d77}"),
            ("name", "Stra\u{df}e links"),
            ("name:ja", "\u{53f3}\u{5074}\u{901a}\u{884c}"),
            ("note:backward", "\u{2192}"),
        ])
    );
}

#[gpui::test]
fn numeric_compass_direction_on_a_way_is_not_negated(cx: &mut TestAppContext) {
    assert_eq!(
        reverse_tags(cx, &[("direction", "90")]),
        pairs(&[("direction", "90")])
    );
    assert_eq!(
        reverse_tags(cx, &[("direction", "270")]),
        pairs(&[("direction", "270")])
    );
}

#[gpui::test]
fn node_direction_tags_with_odd_keys_and_values_follow_the_exact_rule(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[])
        .node(
            2,
            &[
                ("highway", "stop"),
                ("direction", "forwards"),
                ("traffic_sign:forward ", "stop"),
                ("traffic_sign:forward_x", "stop"),
                ("traffic_sign:Backward", "stop"),
                ("traffic_sign:backward", "yield"),
            ],
        )
        .node(3, &[])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(
            s.tags(N, 2),
            pairs(&[
                ("direction", "forwards"),
                ("highway", "stop"),
                ("traffic_sign:Backward", "stop"),
                ("traffic_sign:forward", "yield"),
                ("traffic_sign:forward ", "stop"),
                ("traffic_sign:forward_x", "stop"),
            ])
        );
        undo_redo(s, &before);
    });
}
