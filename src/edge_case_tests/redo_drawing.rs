//! Undo followed by redo must put back exactly what a drawing step created:
//! same ids, positions, node order and tags. Each test performs the edit
//! through the same handler the mouse uses, then walks undo / redo / undo /
//! redo and compares the layer's data with what it held at each point.

use super::*;
use crate::{AddProgress, ExtrudeDrag, ExtrudeStyle};
use gpui::{point, Pixels, Point, TestAppContext};

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

/// Way 10 runs east-west through the viewport centre from node 1 to node 2.
fn data() -> OsmData {
    let mut b = Build::new();
    b = b.way(10, &[1, 2], &[]);
    let mut d = b.done();
    for (id, lon) in [(1, LON - 0.001), (2, LON + 0.001)] {
        d.nodes.insert(
            id,
            OsmNode {
                id,
                lat: LAT,
                lon,
                version: 1,
                tags: HashMap::new(),
            },
        );
    }
    d
}

fn empty() -> OsmData {
    Build::new().done()
}

fn geo(s: &Session, lat: f64, lon: f64) -> Point<Pixels> {
    s.view.viewport.geo_to_screen(lat, lon)
}

/// A spot `(dx, dy)` pixels from the viewport centre.
fn off(s: &Session, dx: f32, dy: f32) -> Point<Pixels> {
    geo(s, LAT, LON) + point(px(dx), px(dy))
}

fn add_mode(s: &mut Session) {
    s.view.mode = EditMode::Add;
}

/// Run `edit`, then check one undo, redo, undo, redo each reproduce the data
/// from before / after it. Returns the data after the edit.
fn round_trip(s: &mut Session, edit: impl FnOnce(&mut Session)) -> Snap {
    let original = s.snap();
    let steps = s.steps();
    edit(s);
    let edited = s.snap();
    assert_ne!(edited, original, "the edit changed nothing");
    assert_eq!(s.steps(), steps + 1, "the edit is one undo step");
    s.undo();
    assert_eq!(s.snap(), original, "undo restores the original");
    s.redo();
    assert_eq!(s.snap(), edited, "redo reproduces the edit");
    s.undo();
    assert_eq!(s.snap(), original, "a second undo restores the original");
    s.redo();
    assert_eq!(s.snap(), edited, "a second redo reproduces the edit");
    edited
}

#[gpui::test]
fn building_redo_recreates_the_ring_with_its_ids_and_tags(cx: &mut TestAppContext) {
    with_session(cx, empty(), |s| {
        let edited = round_trip(s, |s| {
            for p in [off(s, -60.0, 0.0), off(s, 60.0, 0.0), off(s, 60.0, 50.0)] {
                s.view.handle_building_click(p);
            }
        });
        let [way] = s.new_ways()[..] else {
            panic!("one new way");
        };
        let nodes = &edited.ways[&way].0;
        assert_eq!((nodes.len(), nodes[0] == nodes[4]), (5, true));
        assert_eq!(s.tags(W, way), pairs(&[("building", "yes")]));
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn arc_redo_recreates_the_way_and_its_nodes(cx: &mut TestAppContext) {
    with_session(cx, empty(), |s| {
        round_trip(s, |s| {
            let (a, b, c) = (off(s, -100.0, 0.0), off(s, 100.0, 0.0), off(s, 0.0, -60.0));
            for p in [a, b, c] {
                s.view.handle_arc_click(p, false);
            }
        });
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn arc_starting_on_a_way_redoes_the_splice_too(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        let before = s.way_nodes(10).unwrap();
        let edited = round_trip(s, |s| {
            let (a, b, c) = (
                geo(s, LAT, LON - 0.0004),
                off(s, 150.0, -100.0),
                off(s, 0.0, -120.0),
            );
            for p in [a, b, c] {
                s.view.handle_arc_click(p, false);
            }
        });
        assert_eq!(
            edited.ways[&10].0.len(),
            before.len() + 1,
            "the start was spliced into way 10"
        );
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn extrude_new_way_redo_recreates_the_nodes_and_way(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        round_trip(s, |s| {
            let drag = ExtrudeDrag {
                layer: s.layer,
                way_id: 10,
                node_a: 1,
                node_b: 2,
                segment: 0,
                style: ExtrudeStyle::NewWay,
                down: off(s, 0.0, 0.0),
            };
            let up = off(s, 0.0, -80.0);
            s.view.commit_extrude(&drag, up);
        });
        let [way] = s.new_ways()[..] else {
            panic!("one new way");
        };
        let ring = s.way_nodes(way).unwrap();
        assert_eq!((ring.len(), ring[0], ring[1]), (5, 1, 2));
        assert_eq!(
            s.way_nodes(10),
            Some(vec![1, 2]),
            "the source way is intact"
        );
    });
}

#[gpui::test]
fn extrude_reshape_redo_reproduces_new_nodes_and_the_way_edit(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        round_trip(s, |s| {
            let drag = ExtrudeDrag {
                layer: s.layer,
                way_id: 10,
                node_a: 1,
                node_b: 2,
                segment: 0,
                style: ExtrudeStyle::Reshape,
                down: off(s, 0.0, 0.0),
            };
            let up = off(s, 0.0, -80.0);
            s.view.commit_extrude_reshape(&drag, up);
        });
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn inserting_a_node_on_a_segment_redoes_at_the_same_id_and_index(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        let edited = round_trip(s, |s| {
            let drag = ExtrudeDrag {
                layer: s.layer,
                way_id: 10,
                node_a: 1,
                node_b: 2,
                segment: 0,
                style: ExtrudeStyle::Reshape,
                down: off(s, 0.0, 0.0),
            };
            let at = off(s, 0.0, 0.0);
            s.view.insert_node_on_segment(&drag, at);
        });
        let nodes = &edited.ways[&10].0;
        assert_eq!((nodes.len(), nodes[0], nodes[2]), (3, 1, 2));
        assert!(nodes[1] < 0);
    });
}

#[gpui::test]
fn three_add_clicks_undo_redo_and_undo_again(cx: &mut TestAppContext) {
    with_session(cx, empty(), |s| {
        add_mode(s);
        let original = s.snap();
        let points = [
            off(s, -100.0, -50.0),
            off(s, 0.0, -50.0),
            off(s, 100.0, 50.0),
        ];
        let mut states = vec![original];
        for p in points {
            s.view.handle_add_click(p, false);
            states.push(s.snap());
        }
        assert_eq!(s.steps(), 3);
        let way = s.new_ways()[0];
        assert_eq!(s.way_nodes(way).unwrap().len(), 3);

        for round in 0..2 {
            for i in (0..3).rev() {
                s.undo();
                assert_eq!(s.snap(), states[i], "round {round}: undo to state {i}");
            }
            for (i, state) in states.iter().enumerate().skip(1) {
                s.redo();
                assert_eq!(&s.snap(), state, "round {round}: redo to state {i}");
            }
        }
        assert_eq!(s.new_ways(), vec![way], "the way keeps its id throughout");
    });
}

#[gpui::test]
fn add_click_back_onto_the_first_node_redoes_without_duplicating_it(cx: &mut TestAppContext) {
    with_session(cx, empty(), |s| {
        add_mode(s);
        let first = off(s, -100.0, -50.0);
        for p in [first, off(s, 0.0, -50.0), off(s, 100.0, 50.0)] {
            s.view.handle_add_click(p, false);
        }
        let three_clicks = s.snap();
        let nodes_before = three_clicks.nodes.len();
        round_trip(s, |s| s.view.handle_add_click(first, false));
        let way = s.new_ways()[0];
        let ring = s.way_nodes(way).unwrap();
        assert_eq!((ring.len(), ring[0] == ring[3]), (4, true));
        assert_eq!(s.snap().nodes.len(), nodes_before, "no node was added");
        s.undo();
        assert_eq!(s.snap(), three_clicks);
        assert!(s.has_node(ring[0]), "undo keeps the pre-existing node");
    });
}

#[gpui::test]
fn continuing_a_way_from_its_start_or_its_end_redoes_on_the_right_side(cx: &mut TestAppContext) {
    for at_start in [true, false] {
        with_session(cx, data(), move |s| {
            add_mode(s);
            s.view.add_progress = Some(AddProgress {
                way_id: Some(10),
                last_node_id: if at_start { 1 } else { 2 },
                at_start,
            });
            let edited = round_trip(s, |s| {
                let p = off(s, 0.0, -100.0);
                s.view.handle_add_click(p, false);
            });
            let nodes = &edited.ways[&10].0;
            assert_eq!(nodes.len(), 3);
            let new = nodes.iter().position(|&n| n < 0).unwrap();
            assert_eq!(new, if at_start { 0 } else { 2 });
        });
    }
}

#[gpui::test]
fn add_clicks_that_snap_onto_a_way_redo_the_splices(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        add_mode(s);
        let steps = [
            geo(s, LAT, LON - 0.0005),
            off(s, 0.0, -120.0),
            geo(s, LAT, LON + 0.0005),
        ];
        let mut states = vec![s.snap()];
        for p in steps {
            s.view.handle_add_click(p, false);
            states.push(s.snap());
        }
        assert_eq!(s.steps(), 3);
        assert_eq!(s.way_nodes(10).unwrap().len(), 4, "both ends were spliced");
        for _ in 0..2 {
            for i in (0..3).rev() {
                s.undo();
                assert_eq!(s.snap(), states[i]);
            }
            for state in states.iter().skip(1) {
                s.redo();
                assert_eq!(&s.snap(), state);
            }
        }
        s.assert_no_dangling();
    });
}
