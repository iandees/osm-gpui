//! Simplify Way: redundant vertices of the
//! selected ways are dropped with a Douglas-Peucker pass (3 m tolerance), and
//! the dropped nodes are deleted. Vertices that are ends, junctions, tagged or
//! relation members stay. Positions here are given in metres from a base
//! point so the tolerance is easy to reason about.

use super::*;
use osm_gpui::osm::OsmNode;

const M_LAT: f64 = 1.0 / 111_195.0;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

/// Add node `id` at `x` metres east and `y` metres north of (40N, 74W).
fn nd(b: Build, id: i64, x: f64, y: f64, tags: &[(&str, &str)]) -> Build {
    at_lat_lon(
        b,
        id,
        40.0 + y * M_LAT,
        -74.0 + x * M_LAT / 40f64.to_radians().cos(),
        tags,
    )
}

fn at_lat_lon(mut b: Build, id: i64, lat: f64, lon: f64, tags: &[(&str, &str)]) -> Build {
    b.d().nodes.insert(
        id,
        OsmNode {
            id,
            lat,
            lon,
            version: 3,
            tags: tag_map(tags),
        },
    );
    b
}

/// Nodes 1..=5 along a line 200 m long, with ids `base + 0..5`, wobbling by
/// at most 1 m.
fn line(mut b: Build, base: i64, y: f64) -> Build {
    for (i, wobble) in [0.0, 0.8, -0.6, 1.0, 0.0].iter().enumerate() {
        b = nd(b, base + i as i64, i as f64 * 50.0, y + wobble, &[]);
    }
    b
}

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

fn assert_unchanged(s: &mut Session, before: &Snap, steps: usize) {
    assert_eq!(&s.snap(), before);
    assert_eq!(s.steps(), steps, "no undo step recorded");
    assert_eq!(s.status(), "Nothing to simplify (tolerance 3 m)");
}

#[gpui::test]
fn a_straight_line_with_midpoints_loses_them(cx: &mut TestAppContext) {
    let data = line(Build::new(), 1, 0.0)
        .way(10, &[1, 2, 3, 4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 5]));
        for gone in [2, 3, 4] {
            assert!(!s.has_node(gone), "node {gone} deleted");
        }
        assert_eq!(s.status(), "Simplified: removed 3 nodes (tolerance 3 m)");
        assert_eq!(s.steps(), 1, "one undo step for the whole action");
        assert_eq!(s.view.selected, vec![s.feature(W, 10)], "selection stays");
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_gentle_curve_is_kept(cx: &mut TestAppContext) {
    // A 400 m arc that bulges 25 m: every vertex is far off the chord.
    let mut b = Build::new();
    for (i, y) in [0.0, 18.0, 25.0, 18.0, 0.0].iter().enumerate() {
        b = nd(b, 1 + i as i64, i as f64 * 100.0, *y, &[]);
    }
    let data = b.way(10, &[1, 2, 3, 4, 5], &road()).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(!s.act("simplify"));
        assert_unchanged(s, &before, 0);
    });
}

#[gpui::test]
fn only_the_vertices_within_tolerance_go(cx: &mut TestAppContext) {
    // 1 - 2 - 3 along a line, then a 40 m kink at 4, then 5 on the line.
    let mut b = Build::new();
    for (id, x, y) in [
        (1, 0.0, 0.0),
        (2, 50.0, 1.0),
        (3, 100.0, 0.0),
        (4, 150.0, 40.0),
        (5, 200.0, 0.0),
    ] {
        b = nd(b, id, x, y, &[]);
    }
    let data = b.way(10, &[1, 2, 3, 4, 5], &road()).done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 3, 4, 5]));
        assert_eq!(s.status(), "Simplified: removed 1 node (tolerance 3 m)");
    });
}

#[gpui::test]
fn a_junction_vertex_is_kept(cx: &mut TestAppContext) {
    let data = line(Build::new(), 1, 0.0)
        .nodes(&[30])
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[3, 30], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 3, 5]));
        assert_eq!(
            s.way_nodes(11),
            Some(vec![3, 30]),
            "the other way is intact"
        );
        assert!(s.has_node(3));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_tagged_vertex_is_kept(cx: &mut TestAppContext) {
    let mut b = Build::new();
    for (i, x) in [0.0, 50.0, 100.0, 150.0, 200.0].iter().enumerate() {
        let tags: &[(&str, &str)] = if i == 3 {
            &[("highway", "crossing")]
        } else {
            &[]
        };
        b = nd(b, 1 + i as i64, *x, 0.0, tags);
    }
    let data = b.way(10, &[1, 2, 3, 4, 5], &road()).done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 5]));
        assert_eq!(s.tags(N, 4), pairs(&[("highway", "crossing")]));
        assert_eq!(s.status(), "Simplified: removed 2 nodes (tolerance 3 m)");
    });
}

#[gpui::test]
fn a_relation_member_vertex_is_kept(cx: &mut TestAppContext) {
    let data = line(Build::new(), 1, 0.0)
        .way(10, &[1, 2, 3, 4, 5], &road())
        .relation(900, &[("type", "site")], &[wm(10, ""), nm(2, "stop")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 5]));
        assert_eq!(s.typed_members(900).len(), 2);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn ends_and_a_way_that_cannot_shrink_are_left_alone(cx: &mut TestAppContext) {
    // Two-node way, and a three-node way whose middle is a junction.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3, 4], &road())
        .way(12, &[3, 5], &road())
        .nodes(&[5])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("simplify"));
        assert_unchanged(s, &before, 0);
    });
}

#[gpui::test]
fn a_closed_way_stays_a_valid_ring(cx: &mut TestAppContext) {
    // A 100 m square with a midpoint on each side.
    let mut b = Build::new();
    for (id, x, y) in [
        (1, 0.0, 0.0),
        (2, 50.0, 0.0),
        (3, 100.0, 0.0),
        (4, 100.0, 50.0),
        (5, 100.0, 100.0),
        (6, 50.0, 100.0),
        (7, 0.0, 100.0),
        (8, 0.0, 50.0),
    ] {
        b = nd(b, id, x, y, &[]);
    }
    let data = b
        .way(10, &[1, 2, 3, 4, 5, 6, 7, 8, 1], &[("building", "yes")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"), "{}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes, vec![1, 3, 5, 7, 1]);
        assert!(nodes.len() >= 4);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_tiny_closed_way_that_would_collapse_is_not_touched(cx: &mut TestAppContext) {
    // A 1 m square: everything is within tolerance, but a ring needs 4 entries.
    let mut b = Build::new();
    for (id, x, y) in [(1, 0.0, 0.0), (2, 1.0, 0.0), (3, 1.0, 1.0), (4, 0.0, 1.0)] {
        b = nd(b, id, x, y, &[]);
    }
    let data = b.way(10, &[1, 2, 3, 4, 1], &[("building", "yes")]).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(!s.act("simplify"));
        assert_unchanged(s, &before, 0);
    });
}

#[gpui::test]
fn a_node_visited_twice_by_one_way_is_kept(cx: &mut TestAppContext) {
    // Out along the line and back over the same nodes.
    let data = line(Build::new(), 1, 0.0)
        .way(10, &[1, 2, 3, 4, 5, 4, 3, 2, 1], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(!s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4, 5, 4, 3, 2, 1]));
    });
}

#[gpui::test]
fn several_ways_sharing_vertices_each_keep_the_shared_ones(cx: &mut TestAppContext) {
    // Way 10 runs 1..5 and way 11 runs 5..9 on the same line; way 12 crosses
    // at 3 (a middle vertex of 10), and 7 is a middle vertex of both 11 and 13.
    let mut b = Build::new();
    for i in 0..9 {
        b = nd(b, 1 + i, i as f64 * 50.0, 0.0, &[]);
    }
    b = nd(b, 30, 100.0, 50.0, &[]);
    b = nd(b, 31, 300.0, 50.0, &[]);
    let data = b
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[5, 6, 7, 8, 9], &road())
        .way(12, &[3, 30], &road())
        .way(13, &[31, 7], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 3, 5]));
        assert_eq!(s.way_nodes(11), Some(vec![5, 7, 9]));
        assert_eq!(s.way_nodes(12), Some(vec![3, 30]));
        assert_eq!(s.status(), "Simplified: removed 4 nodes (tolerance 3 m)");
        assert_eq!(s.steps(), 1);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_vertex_in_the_middle_of_two_selected_ways_stays_in_both(cx: &mut TestAppContext) {
    // Ways 10 and 11 cross at node 3, a middle vertex of each.
    let mut b = Build::new();
    for i in 0..5 {
        b = nd(b, 1 + i, i as f64 * 50.0, 0.0, &[]);
    }
    for (id, y) in [(21, -100.0), (22, -50.0), (24, 50.0), (25, 100.0)] {
        b = nd(b, id, 100.0, y, &[]);
    }
    let data = b
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[21, 22, 3, 24, 25], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 11), (W, 10)]);
        assert!(s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 3, 5]));
        assert_eq!(s.way_nodes(11), Some(vec![21, 3, 25]));
        assert!(s.has_node(3));
    });
}

#[gpui::test]
fn a_way_that_is_not_selected_is_not_simplified(cx: &mut TestAppContext) {
    let data = line(Build::new(), 1, 0.0)
        .way(10, &[1, 2, 3, 4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(!s.act("simplify"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn ways_across_the_antimeridian_simplify_by_ground_distance(cx: &mut TestAppContext) {
    // 0.0005 degrees of longitude at the equator is about 55 m. The line
    // runs from 179.999 east over the antimeridian to -179.999; way 11 does
    // the same but with its middle node 5 m north of the line.
    let lons = [179.999, 179.9995, 180.0, -179.9995, -179.999];
    let mut b = Build::new();
    for (i, lon) in lons.iter().enumerate() {
        b = at_lat_lon(b, 1 + i as i64, 0.0, *lon, &[]);
        let north = if i == 2 { 5.0 * M_LAT } else { 0.0 };
        b = at_lat_lon(b, 11 + i as i64, 0.01 + north, *lon, &[]);
    }
    let data = b
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[11, 12, 13, 14, 15], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 5]));
        undo_redo(s, &before);
        s.select(&[(W, 11)]);
        assert!(s.act("simplify"), "{}", s.status());
        // The 5 m kink is kept; its neighbours are within tolerance of the
        // two shorter chords.
        assert_eq!(s.way_nodes(11), Some(vec![11, 13, 15]));
    });
}

#[gpui::test]
fn upload_modifies_the_way_before_deleting_the_nodes(cx: &mut TestAppContext) {
    let data = line(Build::new(), 1, 0.0)
        .way(10, &[1, 2, 3, 4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("simplify"));
        assert_eq!(s.counts(), (0, 1, 3));
        let d = s.diff();
        assert_eq!(d.modified_ways.len(), 1);
        assert_eq!(d.deleted_node_ids.len(), 3);
        let xml = s.xml();
        let modify = at(&xml, "<modify>");
        let delete = at(&xml, "<delete>");
        assert!(modify < delete, "{xml}");
        assert!(xml[modify..delete].contains("<way id=\"10\""));
        assert!(xml[modify..delete].contains("<nd ref=\"5\""));
        assert!(!xml[modify..delete].contains("<nd ref=\"3\""));
        // Undo leaves nothing to upload.
        s.undo();
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_larger_tolerance_removes_more_vertices_on_the_same_input(cx: &mut TestAppContext) {
    // A gentle bulge of 8 m over 200 m.
    let mut b = Build::new();
    for (i, y) in [0.0, 5.0, 8.0, 5.0, 0.0].iter().enumerate() {
        b = nd(b, 1 + i as i64, i as f64 * 50.0, *y, &[]);
    }
    let data = b.way(10, &[1, 2, 3, 4, 5], &road()).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        let sel = s.view.selected.clone();
        let active = Some(s.layer);
        let small =
            crate::selection_actions::simplify_ways(&mut s.view.layer_manager, &sel, active, 0.5);
        assert!(small.is_err(), "0.5 m keeps the whole bulge");
        assert_eq!(s.snap(), before);
        let big =
            crate::selection_actions::simplify_ways(&mut s.view.layer_manager, &sel, active, 10.0)
                .ok()
                .unwrap();
        assert_eq!(big.status, "Simplified: removed 3 nodes (tolerance 10 m)");
        assert_eq!(s.way_nodes(10), Some(vec![1, 5]));
    });
}
