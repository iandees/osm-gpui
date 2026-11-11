//! Joining a node to a way: the "Join Node to Way" selection action, and
//! snapping while placing (Add mode) or dragging a node. The tag-compatibility
//! table itself is unit tested in `src/snap.rs`; these tests cover how it is
//! wired in, and the data effects (insertion index, closed and self-touching
//! ways, limits, relations, undo, upload order).

use super::*;
use gpui::{point, Modifiers, MouseMoveEvent, MouseUpEvent};

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

// Node positions in `Build` follow the id: ids that are multiples of 7 sit on
// latitude 40.0, with longitude -74 + 0.0003 * id. So a way over [7, 14, 21,
// 28] is a straight east-west line, and node 15 (40.0004, ...) lies just north
// of its second segment (14 - 21).
fn line() -> Build {
    Build::new()
        .nodes(&[7, 14, 15, 21, 28])
        .way(10, &[7, 14, 21, 28], &road())
}

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

// ---------------------------------------------------------------------------
// The action

#[gpui::test]
fn node_is_inserted_on_the_nearest_segment_and_moved_onto_the_line(cx: &mut TestAppContext) {
    with_session(cx, line().done(), |s| {
        let before = s.snap();
        let steps = s.steps();
        s.select(&[(N, 15), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![7, 14, 15, 21, 28]));
        let (lat, _) = s.snap().nodes[&15].0;
        assert!(
            (lat - 40.0).abs() < 1e-9,
            "node lies on the line, lat {lat}"
        );
        assert_eq!(s.steps(), steps + 1, "one undo step");
        // The node moved and the way changed; nothing else.
        assert_eq!(s.counts(), (0, 2, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn insertion_index_follows_the_segment_for_every_position(cx: &mut TestAppContext) {
    // Node 8 (40.0004, -73.9976) is nearest the first segment 7 - 14.
    let data = Build::new()
        .nodes(&[7, 8, 14, 21, 28])
        .way(10, &[7, 14, 21, 28], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 8), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![7, 8, 14, 21, 28]));
    });
}

#[gpui::test]
fn a_way_that_passes_the_node_twice_takes_the_first_pass(cx: &mut TestAppContext) {
    // 7 - 14 - 21 - 14 - 7 runs out and back over the same line. Node 15 is
    // equally near 14 - 21 (index 1) and 21 - 14 (index 2): the first wins.
    let data = Build::new()
        .nodes(&[7, 14, 15, 21])
        .way(10, &[7, 14, 21, 14, 7], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 15), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![7, 14, 15, 21, 14, 7]));
    });
}

#[gpui::test]
fn a_closed_way_can_be_joined_on_its_closing_segment(cx: &mut TestAppContext) {
    // Ring 7 (40.0, .0021), 14 (40.0, .0042), 16 (.0008, .0048), 9 (.0008,
    // .0027): the closing edge runs 9 - 7. Node 8 (.0004, .0024) is on it.
    let data = Build::new()
        .nodes(&[7, 8, 9, 14, 16])
        .way(10, &[7, 14, 16, 9, 7], &[("building", "yes")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 8), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes, vec![7, 14, 16, 9, 8, 7]);
        assert_eq!(nodes.first(), nodes.last(), "still closed");
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_full_way_refuses_one_more_node(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=2000).collect();
    let mut b = Build::new();
    for &id in &ids {
        b = b.node(id, &[]);
    }
    let data = b.node(5001, &[]).way(10, &ids, &road()).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 5001), (W, 10)]);
        assert!(!s.act("join_node_way"));
        assert!(s.status().contains("2,000"), "status: {}", s.status());
        assert_eq!(s.snap(), before, "nothing changed");
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_node_already_in_the_way_is_refused_without_a_duplicate(cx: &mut TestAppContext) {
    with_session(cx, line().done(), |s| {
        let before = s.snap();
        for node in [7, 14, 28] {
            s.select(&[(N, node), (W, 10)]);
            assert!(!s.act("join_node_way"));
            assert!(s.status().contains("already"), "status: {}", s.status());
            assert_eq!(s.snap(), before);
        }
    });
}

#[gpui::test]
fn it_needs_exactly_one_node_and_one_way(cx: &mut TestAppContext) {
    let data = line().nodes(&[8]).way(11, &[14, 15], &road()).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        for selection in [
            vec![(N, 15)],
            vec![(W, 10)],
            vec![(N, 15), (N, 8), (W, 10)],
            vec![(N, 8), (W, 10), (W, 11)],
        ] {
            s.select(&selection);
            assert!(!s.act("join_node_way"), "{selection:?}");
            assert_eq!(s.snap(), before);
        }
    });
}

#[gpui::test]
fn a_node_that_is_a_relation_member_stays_one_and_the_way_relations_are_unchanged(
    cx: &mut TestAppContext,
) {
    let data = line()
        .way(11, &[7, 14], &road())
        .relation(
            50,
            &[("type", "route"), ("route", "bus")],
            &[nm(15, "stop"), wm(10, "")],
        )
        .relation(
            51,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            &[wm(11, "from"), nm(14, "via"), wm(10, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 15), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        let after = s.snap();
        assert_eq!(after.relations, before.relations);
        assert_eq!(s.members(50), mem(&[(15, "stop"), (10, "")]));
        assert_eq!(s.members(51), mem(&[(11, "from"), (14, "via"), (10, "to")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_new_node_goes_up_before_the_modified_way(cx: &mut TestAppContext) {
    with_session(cx, line().done(), |s| {
        // A node created in this session, near the second segment.
        let id = s.editable_mut().add_node(40.0004, -74.0 + 0.0003 * 15.0);
        assert!(id < 0);
        s.select(&[(N, id), (W, 10)]);
        assert!(s.act("join_node_way"), "{}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes[2], id);
        // One created node (no separate move step needed upstream), one
        // modified way.
        assert_eq!(s.counts(), (1, 1, 0));
        let xml = s.xml();
        let node = at(&xml, &format!("<node id=\"{id}\""));
        let way = at(&xml, "<way id=\"10\"");
        assert!(node < way, "created node before the modified way: {xml}");
        assert!(xml.contains(&format!("<nd ref=\"{id}\"")), "{xml}");
        // The created node is uploaded once, at its final position on the line.
        assert_eq!(xml.matches(&format!("<node id=\"{id}\"")).count(), 1);
    });
}

// ---------------------------------------------------------------------------
// Snapping in the editor

/// A way 10 along latitude 40 through the viewport centre, with a free node 3
/// 0.0004 degrees north of it.
fn scene(way_tags: &[(&str, &str)], node_tags: &[(&str, &str)]) -> OsmData {
    let mut data = Build::new()
        .nodes(&[1, 2])
        .node(3, node_tags)
        .way(10, &[1, 2], way_tags)
        .done();
    let place = |data: &mut OsmData, id: i64, lat: f64, lon: f64| {
        let n = data.nodes.get_mut(&id).unwrap();
        n.lat = lat;
        n.lon = lon;
    };
    place(&mut data, 1, 40.0, -74.001);
    place(&mut data, 2, 40.0, -73.999);
    place(&mut data, 3, 40.0004, -74.0005);
    data
}

fn screen(s: &Session, lat: f64, lon: f64) -> gpui::Point<gpui::Pixels> {
    s.view.viewport.geo_to_screen(lat, lon)
}

fn win(p: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
    point(
        p.x + gpui::px(MapViewer::MODE_PANEL_WIDTH),
        p.y + crate::menu::top_chrome_height(),
    )
}

/// Press on node 3, drag to `to` and release, holding `mods` at the release.
fn drag_node_three(s: &mut Session, to: gpui::Point<gpui::Pixels>, mods: Modifiers) {
    let from = screen(s, 40.0004, -74.0005);
    s.view.set_selection(vec![s.feature(N, 3)]);
    s.view.handle_map_mouse_down(from, Modifiers::none());
    let layer_cx = &mut *s.cx;
    s.view.handle_mouse_move(
        &MouseMoveEvent {
            position: win(to),
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: mods,
        },
        layer_cx,
    );
    s.view.handle_mouse_up(
        &MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: win(to),
            modifiers: mods,
            click_count: 1,
        },
        layer_cx,
    );
}

#[gpui::test]
fn dragging_a_node_onto_a_way_inserts_it_as_one_undo_step(cx: &mut TestAppContext) {
    with_session(cx, scene(&road(), &[]), |s| {
        let before = s.snap();
        let steps = s.steps();
        // 2 px off the line, well inside the snap tolerance.
        let to = screen(s, 40.0, -74.0);
        drag_node_three(s, point(to.x, to.y + gpui::px(2.0)), Modifiers::none());
        assert_eq!(s.way_nodes(10), Some(vec![1, 3, 2]));
        let (lat, _) = s.snap().nodes[&3].0;
        assert!((lat - 40.0).abs() < 1e-9, "snapped onto the line: {lat}");
        assert_eq!(s.steps(), steps + 1);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn the_disable_modifier_leaves_a_dragged_node_free(cx: &mut TestAppContext) {
    for mods in [
        Modifiers {
            alt: true,
            ..Modifiers::none()
        },
        Modifiers {
            control: true,
            ..Modifiers::none()
        },
    ] {
        let mut app = cx.clone();
        with_session(&mut app, scene(&road(), &[]), move |s| {
            let to = screen(s, 40.0, -74.0);
            drag_node_three(s, point(to.x, to.y + gpui::px(2.0)), mods);
            assert_eq!(s.way_nodes(10), Some(vec![1, 2]), "{mods:?}");
            let (lat, _) = s.snap().nodes[&3].0;
            assert!(
                (lat - 40.0).abs() > 1e-7,
                "moved, but not put on the line: {lat}"
            );
            assert_eq!(s.steps(), 1, "just the plain move");
        });
    }
}

#[gpui::test]
fn a_node_does_not_snap_onto_a_way_of_an_incompatible_kind(cx: &mut TestAppContext) {
    // A crossing-style road node dragged over a building outline.
    with_session(
        cx,
        scene(&[("building", "yes")], &[("highway", "crossing")]),
        |s| {
            let to = screen(s, 40.0, -74.0);
            drag_node_three(s, point(to.x, to.y + gpui::px(2.0)), Modifiers::none());
            assert_eq!(s.way_nodes(10), Some(vec![1, 2]));
            assert!(s.view.snap_preview.is_none());
        },
    );
}

#[gpui::test]
fn the_snap_target_is_highlighted_while_dragging_and_cleared_after(cx: &mut TestAppContext) {
    with_session(cx, scene(&road(), &[]), |s| {
        let from = screen(s, 40.0004, -74.0005);
        let to = screen(s, 40.0, -74.0);
        s.view.set_selection(vec![s.feature(N, 3)]);
        s.view.handle_map_mouse_down(from, Modifiers::none());
        let cx = &mut *s.cx;
        s.view.handle_mouse_move(
            &MouseMoveEvent {
                position: win(to),
                pressed_button: Some(gpui::MouseButton::Left),
                modifiers: Modifiers::none(),
            },
            cx,
        );
        assert!(
            matches!(
                s.view.snap_preview,
                Some(crate::SnapTarget::Way {
                    way_id: 10,
                    index: 0,
                    ..
                })
            ),
            "{:?}",
            s.view.snap_preview
        );
        // Alt turns the highlight off.
        s.view.handle_mouse_move(
            &MouseMoveEvent {
                position: win(to),
                pressed_button: Some(gpui::MouseButton::Left),
                modifiers: Modifiers {
                    alt: true,
                    ..Modifiers::none()
                },
            },
            cx,
        );
        assert!(s.view.snap_preview.is_none());
    });
}

#[gpui::test]
fn dragging_a_node_onto_another_node_merges_them(cx: &mut TestAppContext) {
    with_session(cx, scene(&road(), &[]), |s| {
        let before = s.snap();
        let to = screen(s, 40.0, -74.001);
        drag_node_three(s, to, Modifiers::none());
        // One of the two survives, the way still has two nodes, and one undo
        // restores everything.
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes.len(), 2, "{nodes:?}");
        assert!(!(s.has_node(1) && s.has_node(3)), "merged into one node");
        s.assert_no_dangling();
        assert_eq!(s.steps(), 1);
        s.undo();
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn add_mode_snaps_a_way_being_drawn_only_onto_compatible_ways(cx: &mut TestAppContext) {
    // Way 10 is a building outline. A drawn way that is untagged snaps to
    // anything, but once it is a footway it must not glue onto the building.
    with_session(cx, scene(&[("building", "yes")], &[]), |s| {
        s.view.mode = EditMode::Add;
        s.view
            .handle_add_click(point(gpui::px(50.0), gpui::px(50.0)), false);
        s.view
            .handle_add_click(point(gpui::px(60.0), gpui::px(80.0)), false);
        let drawn = s.view.add_progress.as_ref().unwrap().way_id.unwrap();
        s.set_tag(W, drawn, "highway", "footway");
        let on_building = screen(s, 40.0, -74.0);
        s.view.handle_add_click(on_building, false);
        assert_eq!(
            s.way_nodes(10),
            Some(vec![1, 2]),
            "no snap onto the building"
        );

        // Make the drawn way a building too: now the same click snaps.
        s.editable_mut().remove_tag(W, drawn, "highway");
        s.set_tag(W, drawn, "building", "yes");
        let elsewhere = point(on_building.x + gpui::px(40.0), on_building.y);
        s.view.handle_add_click(elsewhere, false);
        assert_eq!(s.way_nodes(10).map(|n| n.len()), Some(3));
    });
}

#[gpui::test]
fn add_mode_with_an_untagged_way_snaps_and_the_modifier_prevents_it(cx: &mut TestAppContext) {
    with_session(cx, scene(&[("building", "yes")], &[]), |s| {
        s.view.mode = EditMode::Add;
        let on_line = screen(s, 40.0, -74.0);
        // Held Alt (the click handler receives "snapping disabled"): free node.
        s.view
            .handle_add_click(point(gpui::px(50.0), gpui::px(50.0)), false);
        let off = point(on_line.x - gpui::px(40.0), on_line.y);
        s.view
            .handle_add_click(off, osm_gpui::snap::snapping_disabled(true, false));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2]));
        // Without it, the untagged drawing way snaps onto anything.
        s.view.handle_add_click(on_line, false);
        assert_eq!(s.way_nodes(10).map(|n| n.len()), Some(3));
    });
}
