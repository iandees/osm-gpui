//! Every selection action that changes data must undo to exactly the data it
//! started from and redo to exactly the data it produced, ids, versions and
//! relation order included. The test walks `ACTIONS`, so an action added later
//! is checked as soon as some selection below applies it; one that applies to
//! none of them fails the test until a selection is added.

use super::*;
use crate::selection_actions::ACTIONS;
use std::collections::BTreeSet;

/// Actions that never record an undo step of their own, with why.
const NOT_STEPS: &[&str] = &[
    // Read the clipboard or the pointer position, which a test has none of.
    "copy_features",
    "paste_features",
    "paste_features_source",
    "duplicate_features",
    // Only copy tags to the clipboard.
    "copy_tags",
    // Only switch to Add mode.
    "extend",
    // Open a dialog or start a pointer-driven session; the answer is applied
    // by code with its own tests.
    "add_to_relation",
    "parallel",
    "rotate",
];

fn point(id: i64, lat: f64, lon: f64) -> (i64, f64, f64) {
    (id, lat, lon)
}

fn data() -> OsmData {
    let mut b = Build::new();
    for (id, lat, lon) in [
        // Way 10 runs east; way 15 crosses it at node 3; way 11 continues it.
        point(1, 40.0, -74.0010),
        point(2, 40.0, -74.0005),
        point(3, 40.0, -74.0000),
        point(4, 40.0, -73.9995),
        point(5, 40.0003, -73.9990),
        point(6, 40.0006, -73.9985),
        point(52, 40.0009, -73.9980),
        point(50, 40.0005, -74.0000),
        point(51, 40.0010, -74.0000),
        // A slightly skewed shop building.
        point(20, 40.0010, -74.0020),
        point(21, 40.0010, -74.0012),
        point(22, 40.0016, -74.00122),
        point(23, 40.0016, -74.0020),
        // A path with a vertex almost on the line.
        point(7, 40.0020, -74.0010),
        point(8, 40.00205, -74.0005),
        point(9, 40.0020, -74.0000),
        // An outer ring with an inner ring.
        point(60, 40.0030, -74.0020),
        point(61, 40.0030, -74.0010),
        point(62, 40.0040, -74.0010),
        point(63, 40.0040, -74.0020),
        point(64, 40.0033, -74.0017),
        point(65, 40.0033, -74.0013),
        point(66, 40.0037, -74.0013),
        point(67, 40.0037, -74.0017),
        // Loose points, one near the path.
        point(40, 40.0050, -74.0020),
        point(41, 40.0051, -74.0015),
        point(42, 40.0049, -74.0010),
        point(44, 40.00203, -74.0007),
    ] {
        b = b.node_at(id, lat, lon);
    }
    b = b.node(43, &[("amenity", "bench")]);
    let mut d = b
        .way(10, &[1, 2, 3, 4], &[("highway", "residential")])
        .way(11, &[4, 5, 6], &[("highway", "residential")])
        .way(15, &[3, 50, 51], &[("highway", "service")])
        .way(16, &[6, 52], &[("highway", "residential")])
        .way(
            12,
            &[20, 21, 22, 23, 20],
            &[("building", "yes"), ("shop", "bakery"), ("name", "Joe")],
        )
        .way(13, &[7, 8, 9], &[("highway", "path")])
        .way(30, &[60, 61, 62, 63, 60], &[])
        .way(31, &[64, 65, 66, 67, 64], &[])
        .relation(
            100,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, "forward"), wm(11, "backward"), nm(43, "stop")],
        )
        .relation(101, &[("type", "site")], &[wm(12, "")])
        .done();
    d.nodes.get_mut(&43).unwrap().lat = 40.0050;
    d.nodes.get_mut(&43).unwrap().lon = -74.0005;
    d
}

/// Selections that between them apply every editing action at least once.
fn selections() -> Vec<Vec<(FeatureKind, i64)>> {
    use FeatureKind::{Node as N, Way as W};
    vec![
        vec![(W, 12)],
        vec![(W, 10)],
        vec![(W, 11)],
        vec![(W, 13)],
        vec![(W, 15)],
        vec![(W, 10), (W, 11)],
        vec![(W, 11), (W, 16)],
        vec![(W, 10), (W, 15)],
        vec![(W, 30)],
        vec![(W, 30), (W, 31)],
        vec![(N, 2)],
        vec![(N, 3)],
        vec![(N, 3), (W, 10)],
        vec![(N, 3), (W, 15)],
        vec![(N, 4)],
        vec![(N, 2), (N, 3)],
        vec![(N, 1), (N, 2), (N, 3)],
        vec![(N, 40), (N, 41)],
        vec![(N, 40), (N, 41), (N, 42)],
        vec![(N, 44), (W, 13)],
        vec![(N, 43)],
        vec![(N, 40)],
    ]
}

fn layer_data(s: &Session) -> OsmData {
    let layer = s.view.layer_manager.find_layer(s.layer).unwrap();
    (*layer.as_osm().unwrap().get_osm_data().unwrap()).clone()
}

#[gpui::test]
fn every_editing_action_undoes_and_redoes_exactly(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        let mut exercised = BTreeSet::new();
        for action in ACTIONS.iter().filter(|a| !NOT_STEPS.contains(&a.id)) {
            for selection in selections() {
                s.select(&selection);
                if action.id == "paste_tags" {
                    s.select(&[(FeatureKind::Way, 12)]);
                    s.act("copy_tags");
                    s.select(&selection);
                }
                let (before_data, before) = (layer_data(s), s.snap());
                let done = s.view.undo_stack.cursor;
                if !s.act(action.id) {
                    continue;
                }
                let what = format!("{} on {selection:?}", action.id);
                assert_eq!(
                    s.view.undo_stack.cursor,
                    done + 1,
                    "{what} is one undo step"
                );
                assert!(
                    !s.view
                        .undo_stack
                        .actions
                        .last()
                        .unwrap()
                        .description()
                        .is_empty(),
                    "{what} has a history label"
                );
                let (after_data, after) = (layer_data(s), s.snap());
                assert_ne!(after_data, before_data, "{what} changed nothing");

                s.undo();
                assert_eq!(s.snap(), before, "{what}: undo restores the data");
                assert_eq!(
                    layer_data(s),
                    before_data,
                    "{what}: undo restores ids, versions and order"
                );
                s.redo();
                assert_eq!(s.snap(), after, "{what}: redo reproduces the data");
                assert_eq!(
                    layer_data(s),
                    after_data,
                    "{what}: redo reproduces ids, versions and order"
                );
                // Back to the start for the next selection.
                s.undo();
                assert_eq!(layer_data(s), before_data, "{what}: a second undo restores");
                exercised.insert(action.id);
            }
        }
        for action in ACTIONS.iter().filter(|a| !NOT_STEPS.contains(&a.id)) {
            assert!(
                exercised.contains(action.id),
                "no selection applies `{}`: add one to `selections`, or list the action in NOT_STEPS with the reason",
                action.id
            );
        }
    });
}
