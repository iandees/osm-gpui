//! Turn-restriction edits from the Junction section.

use super::{editable, select, CENTER_LAT, CENTER_LON};
use crate::test_support::{osm_window, redo_step, undo_step};
use crate::{EditMode, MapViewer};
use gpui::TestAppContext;
use osm_gpui::ids::{NodeId, WayId};
use osm_gpui::osm::{OsmData, OsmNode, OsmRelation, OsmWay};
use osm_gpui::selection::FeatureKind;
use osm_gpui::tags::Tags;

const D: f64 = 0.001;

/// A T at node 1: way 10 comes from the south and ends there; way 20 runs
/// west (node 4) to east (node 5) through it.
fn tee() -> OsmData {
    let mut d = OsmData::default();
    for (id, dlat, dlon) in [(1, 0.0, 0.0), (2, -D, 0.0), (4, 0.0, -D), (5, 0.0, D)] {
        d.nodes.insert(
            NodeId(id),
            OsmNode {
                id: NodeId(id),
                lat: CENTER_LAT + dlat,
                lon: CENTER_LON + dlon,
                version: 1,
                tags: Tags::default(),
            },
        );
    }
    for (id, nodes, highway) in [
        (10, vec![2, 1], "primary"),
        (20, vec![4, 1, 5], "residential"),
    ] {
        d.ways.insert(
            WayId(id),
            OsmWay {
                id: WayId(id),
                nodes: nodes.into_iter().map(NodeId).collect(),
                version: 1,
                tags: Tags::from([("highway", highway)]),
            },
        );
    }
    d
}

/// The arm of the selected junction on way `way` towards `next`.
fn arm(view: &MapViewer, way: i64, next: i64) -> usize {
    let (_, j) = view.selected_junction().unwrap();
    j.arms
        .iter()
        .position(|a| a.way == WayId(way) && a.next_node == NodeId(next))
        .unwrap()
}

fn restrictions(view: &mut MapViewer) -> Vec<OsmRelation> {
    editable(view)
        .relations()
        .into_iter()
        .filter(|r| r.tags.get("type") == Some("restriction"))
        .collect()
}

/// `(role, way or node id)` of a relation's members.
fn members(r: &OsmRelation) -> Vec<(String, i64)> {
    r.members
        .iter()
        .map(|m| (m.role.clone(), m.reference))
        .collect()
}

#[gpui::test]
fn clicking_a_movement_steps_through_no_only_and_allowed(cx: &mut TestAppContext) {
    let window = osm_window(cx, "T", tee(), EditMode::Select);
    window
        .update(cx, |view, _window, _cx| {
            select(view, FeatureKind::Node, &[1]);
            let (south, west) = (arm(view, 10, 2), arm(view, 20, 4));

            // Allowed → no_left_turn. Way 20 runs through the junction, so it
            // is split there and the restriction names its west part.
            view.toggle_junction_movement(south, west);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let r = restrictions(view);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].tags.get("restriction"), Some("no_left_turn"));
            let m = members(&r[0]);
            assert_eq!(m[0], ("from".to_string(), 10));
            assert_eq!(m[1], ("via".to_string(), 1));
            assert_eq!(m[2].0, "to");
            let to_nodes = editable(view).way_node_ids(WayId(m[2].1)).unwrap();
            assert!(to_nodes.contains(&NodeId(4)) && to_nodes.contains(&NodeId(1)));
            assert!(!to_nodes.contains(&NodeId(5)));
            // The east part is a way of its own now.
            assert_eq!(editable(view).ways_using_node(NodeId(1)).len(), 3);

            // no_left_turn → only_left_turn.
            let west = arm(view, m[2].1, 4);
            view.toggle_junction_movement(south, west);
            assert_eq!(
                restrictions(view)[0].tags.get("restriction"),
                Some("only_left_turn")
            );

            // only_left_turn → allowed: the relation goes.
            view.toggle_junction_movement(south, west);
            assert!(restrictions(view).is_empty());
            assert_eq!(view.undo_stack.actions.len(), 3);

            // Each click is one undo step; undoing the first also un-splits.
            undo_step(view);
            undo_step(view);
            assert_eq!(
                restrictions(view)[0].tags.get("restriction"),
                Some("no_left_turn")
            );
            undo_step(view);
            assert!(restrictions(view).is_empty());
            assert_eq!(editable(view).ways_using_node(NodeId(1)).len(), 2);
            redo_step(view);
            assert_eq!(restrictions(view).len(), 1);
            assert_eq!(editable(view).ways_using_node(NodeId(1)).len(), 3);
        })
        .unwrap();
}

#[gpui::test]
fn a_movement_ruled_out_by_an_only_restriction_is_allowed_by_removing_it(cx: &mut TestAppContext) {
    let window = osm_window(cx, "T", tee(), EditMode::Select);
    window
        .update(cx, |view, _window, _cx| {
            select(view, FeatureKind::Node, &[1]);
            let south = arm(view, 10, 2);
            // A U-turn from the south needs no split: way 10 ends here.
            view.toggle_junction_movement(south, south);
            let r = restrictions(view);
            assert_eq!(r[0].tags.get("restriction"), Some("no_u_turn"));
            assert_eq!(
                members(&r[0]),
                [
                    ("from".to_string(), 10),
                    ("via".to_string(), 1),
                    ("to".to_string(), 10)
                ]
            );
            assert_eq!(editable(view).ways_using_node(NodeId(1)).len(), 2);
            view.toggle_junction_movement(south, south);
            // only_u_turn rules out the others; clicking one removes it.
            let east = arm(view, 20, 5);
            view.toggle_junction_movement(south, east);
            assert!(restrictions(view).is_empty());
        })
        .unwrap();
}
