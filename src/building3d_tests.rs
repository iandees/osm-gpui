//! 3D Building mode: opening the pane from a map click, and each 3D edit
//! (heights, wall push/pull, drawing a part, splitting along a line or at a
//! height) as one undo step that keeps the building's structure valid.

use super::*;
use gpui::{size, TestAppContext};
use osm_gpui::building3d::model::BuildingRoot;
use osm_gpui::building3d::tags::Heights;
use osm_gpui::extrude::ExtrudeKind;
use osm_gpui::geometry::LocalProjection;
use osm_gpui::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use osm_gpui::selection::{FeatureKind, FeatureRef};
use std::collections::HashMap;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

struct Fixture {
    data: OsmData,
    frame: LocalProjection,
    next: i64,
}

impl Fixture {
    fn new() -> Self {
        Self {
            data: OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: Vec::new(),
            },
            frame: LocalProjection::new((LAT, LON)),
            next: 1,
        }
    }

    fn node(&mut self, x: f64, y: f64) -> i64 {
        let id = self.next;
        self.next += 1;
        let (lat, lon) = self.frame.unproject((x, y));
        self.data.nodes.insert(
            id,
            OsmNode {
                id,
                lat,
                lon,
                version: 1,
                tags: HashMap::new(),
            },
        );
        id
    }

    fn way(&mut self, id: i64, nodes: Vec<i64>, tags: &[(&str, &str)]) {
        self.data.ways.insert(
            id,
            OsmWay {
                id,
                nodes,
                version: 1,
                tags: tags
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            },
        );
    }
}

/// A 20 x 10 m building (way 100, nodes 1-4 counter-clockwise from the
/// south-west corner), optionally with its west half as part 101 sharing
/// nodes 1 and 4, and optionally a `type=building` relation 500 over both.
fn fixture(with_part: bool, with_relation: bool) -> OsmData {
    let mut f = Fixture::new();
    let sw = f.node(0.0, 0.0);
    let se = f.node(20.0, 0.0);
    let ne = f.node(20.0, 10.0);
    let nw = f.node(0.0, 10.0);
    f.way(
        100,
        vec![sw, se, ne, nw, sw],
        &[("building", "yes"), ("height", "6"), ("name", "Hall")],
    );
    if with_part {
        let s = f.node(10.0, 0.0);
        let n = f.node(10.0, 10.0);
        // The outline passes through the part's east corners too.
        f.way(
            100,
            vec![sw, s, se, ne, n, nw, sw],
            &[("building", "yes"), ("height", "6")],
        );
        f.way(
            101,
            vec![sw, s, n, nw, sw],
            &[
                ("building:part", "yes"),
                ("height", "12"),
                ("building:levels", "4"),
            ],
        );
    }
    if with_relation {
        let mut members = vec![OsmMember {
            member_type: MemberType::Way,
            reference: 100,
            role: "outline".into(),
        }];
        if with_part {
            members.push(OsmMember {
                member_type: MemberType::Way,
                reference: 101,
                role: "part".into(),
            });
        }
        f.data.relations.push(OsmRelation {
            id: 500,
            members,
            version: 1,
            tags: [("type".to_string(), "building".to_string())]
                .into_iter()
                .collect(),
        });
    }
    f.data
}

fn setup(cx: &mut TestAppContext, data: OsmData) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    let mut id = LayerId(0);
    let id_ref = &mut id;
    window
        .update(cx, |view, _w, _cx| {
            view.viewport = Viewport::new(LAT, LON, 19.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    layer_id,
                    "data".to_string(),
                    Arc::new(data),
                )));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Building3d;
            *id_ref = layer_id;
        })
        .unwrap();
    (window, id)
}

fn editable(view: &MapViewer, layer: LayerId) -> &dyn osm_gpui::layers::EditableLayer {
    view.layer_manager
        .find_layer(layer)
        .and_then(|l| l.as_editable())
        .unwrap()
}

fn way_nodes(view: &MapViewer, layer: LayerId, id: i64) -> Option<Vec<i64>> {
    editable(view, layer).way_node_ids(id)
}

fn tag(view: &MapViewer, layer: LayerId, way: i64, key: &str) -> Option<String> {
    editable(view, layer)
        .feature_tags(&FeatureRef {
            layer_id: layer,
            kind: FeatureKind::Way,
            id: way,
        })?
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

fn all_ways(view: &MapViewer, layer: LayerId) -> Vec<i64> {
    let mut ids: Vec<i64> = editable(view, layer)
        .all_features()
        .into_iter()
        .filter(|f| f.kind == FeatureKind::Way)
        .map(|f| f.id)
        .collect();
    ids.sort();
    ids
}

fn new_ways(view: &MapViewer, layer: LayerId) -> Vec<i64> {
    all_ways(view, layer)
        .into_iter()
        .filter(|id| *id < 0)
        .collect()
}

fn undo_step(view: &mut MapViewer) {
    let action = view.undo_stack.undo().expect("something to undo");
    view.apply_undo_action(&action, false);
}

fn redo_step(view: &mut MapViewer) {
    let action = view.undo_stack.redo().expect("something to redo");
    view.apply_undo_action(&action, true);
}

fn open(view: &mut MapViewer, layer: LayerId, root: BuildingRoot) {
    view.open_building3d(layer, root);
    assert!(view.building3d.is_some(), "pane should open");
}

fn screen_of(view: &MapViewer, x: f64, y: f64) -> gpui::Point<gpui::Pixels> {
    let (lat, lon) = LocalProjection::new((LAT, LON)).unproject((x, y));
    view.viewport.geo_to_screen(lat, lon)
}

#[gpui::test]
fn map_click_opens_the_pane_on_the_building_relation(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(true, true));
    window
        .update(cx, |view, _w, _cx| {
            // Inside the part, away from any outline.
            let p = screen_of(view, 5.0, 5.0);
            view.handle_building3d_map_click(p, 1);
            let pane = view.building3d.as_ref().expect("pane opens");
            assert_eq!(pane_root(pane), BuildingRoot::Relation(500));
            assert_eq!(
                view.selected,
                vec![FeatureRef {
                    layer_id: layer,
                    kind: FeatureKind::Relation,
                    id: 500
                }]
            );
            // A second click on the part, now that its building is open,
            // selects the part.
            view.handle_building3d_map_click(p, 1);
            assert_eq!(view.selected[0].id, 101);
            assert_eq!(view.selected[0].kind, FeatureKind::Way);
        })
        .unwrap();
}

fn pane_root(pane: &building3d_pane::Building3dPane) -> BuildingRoot {
    pane.root_for_tests()
}

#[gpui::test]
fn map_click_on_nothing_keeps_the_pane_closed(cx: &mut TestAppContext) {
    let (window, _layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, _w, _cx| {
            let p = screen_of(view, 200.0, 200.0);
            view.handle_building3d_map_click(p, 1);
            assert!(view.building3d.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn dragging_a_roof_sets_height_and_rescales_levels(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(true, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            let part = view.building3d_part_index_for_tests(101).unwrap();
            view.building3d_commit_heights(
                part,
                Heights {
                    min_height: 0.0,
                    height: 18.0,
                    roof_height: 0.0,
                },
            );
            assert_eq!(tag(view, layer, 101, "height").as_deref(), Some("18"));
            assert_eq!(
                tag(view, layer, 101, "building:levels").as_deref(),
                Some("6")
            );
            assert_eq!(view.undo_stack.actions.len(), 1);
            undo_step(view);
            assert_eq!(tag(view, layer, 101, "height").as_deref(), Some("12"));
            assert_eq!(
                tag(view, layer, 101, "building:levels").as_deref(),
                Some("4")
            );
        })
        .unwrap();
}

#[gpui::test]
fn pushing_a_wall_moves_free_nodes_and_keeps_shared_ones(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            let before = way_nodes(view, layer, 100).unwrap();
            // Segment 1 is the east wall (se -> ne); push it 5 m east.
            view.building3d_commit_wall(0, 1, (5.0, 0.0), ExtrudeKind::Reshape);
            assert_eq!(way_nodes(view, layer, 100).unwrap(), before);
            let (lat, lon) = editable(view, layer).node_lat_lon(2).unwrap();
            let (x, _) = LocalProjection::new((LAT, LON)).project((lat, lon));
            assert!((x - 25.0).abs() < 0.01, "{x}");
            undo_step(view);
            let (lat, lon) = editable(view, layer).node_lat_lon(2).unwrap();
            let (x, _) = LocalProjection::new((LAT, LON)).project((lat, lon));
            assert!((x - 20.0).abs() < 0.01, "{x}");
        })
        .unwrap();
}

#[gpui::test]
fn first_drawn_part_also_makes_the_rest_of_the_building_a_part(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            // A 4 x 4 m box on the roof (6 m up), in the scene's frame,
            // which is anchored at node 1 (0, 0).
            view.building3d_commit_new_part([(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)], 6.0);
            let made = new_ways(view, layer);
            assert_eq!(made.len(), 2, "{made:?}");
            // The base part reuses the outline's nodes and its height.
            let base = made
                .iter()
                .copied()
                .find(|&w| way_nodes(view, layer, w) == way_nodes(view, layer, 100))
                .expect("base part over the outline");
            assert_eq!(
                tag(view, layer, base, "building:part").as_deref(),
                Some("yes")
            );
            assert_eq!(tag(view, layer, base, "height").as_deref(), Some("6"));
            assert_eq!(tag(view, layer, base, "name"), None);
            let top = made.into_iter().find(|&w| w != base).unwrap();
            assert_eq!(tag(view, layer, top, "min_height").as_deref(), Some("6"));
            assert_eq!(tag(view, layer, top, "height").as_deref(), Some("9"));
            assert_eq!(view.selected[0].id, top);
            assert_eq!(view.undo_stack.actions.len(), 1);

            undo_step(view);
            assert!(new_ways(view, layer).is_empty());
            assert_eq!(pending(view, layer), 0);
            redo_step(view);
            assert_eq!(new_ways(view, layer).len(), 2);
        })
        .unwrap();
}

fn pending(view: &MapViewer, layer: LayerId) -> usize {
    let (a, b, c) = view
        .layer_manager
        .find_layer(layer)
        .and_then(|l| l.as_any().downcast_ref::<OsmLayer>())
        .unwrap()
        .diff_for_upload()
        .counts();
    a + b + c
}

#[gpui::test]
fn new_part_joins_the_building_relation(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(true, true));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Relation(500));
            view.building3d_commit_new_part(
                [(12.0, 2.0), (18.0, 2.0), (18.0, 8.0), (12.0, 8.0)],
                0.0,
            );
            let made = new_ways(view, layer);
            assert_eq!(made.len(), 1, "a building with parts gets no base part");
            assert_eq!(tag(view, layer, made[0], "min_height"), None);
            let rel = editable(view, layer).relation(500).unwrap();
            assert!(rel
                .members
                .iter()
                .any(|m| m.reference == made[0] && m.role == "part"));
            undo_step(view);
            let rel = editable(view, layer).relation(500).unwrap();
            assert_eq!(rel.members.len(), 2);
        })
        .unwrap();
}

#[gpui::test]
fn splitting_a_part_keeps_neighbours_joined(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(true, true));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Relation(500));
            let part = view.building3d_part_index_for_tests(101).unwrap();
            let outline_before = way_nodes(view, layer, 100).unwrap();
            // Cut part 101 (x 0..10) north-south... across y = 5.
            view.building3d_commit_split_line(part, (-1.0, 5.0), (11.0, 5.0));
            let made = new_ways(view, layer);
            assert_eq!(made.len(), 1);
            let (kept, new) = (
                way_nodes(view, layer, 101).unwrap(),
                way_nodes(view, layer, made[0]).unwrap(),
            );
            assert_eq!(kept.len(), 5);
            assert_eq!(new.len(), 5);
            // Both halves carry the part's tags.
            assert_eq!(tag(view, layer, made[0], "height").as_deref(), Some("12"));
            // The cut nodes on the west wall are also on the outline, which
            // shares that wall.
            let outline_after = way_nodes(view, layer, 100).unwrap();
            assert_eq!(outline_after.len(), outline_before.len() + 1);
            // And the new half is a member of the relation.
            let rel = editable(view, layer).relation(500).unwrap();
            assert!(rel.members.iter().any(|m| m.reference == made[0]));
            assert_eq!(view.undo_stack.actions.len(), 1);

            undo_step(view);
            assert!(new_ways(view, layer).is_empty());
            assert_eq!(way_nodes(view, layer, 100).unwrap(), outline_before);
            assert_eq!(way_nodes(view, layer, 101).unwrap().len(), 5);
            assert_eq!(pending(view, layer), 0);
        })
        .unwrap();
}

#[gpui::test]
fn splitting_a_partless_building_makes_two_parts(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            view.building3d_commit_split_line(0, (8.0, -1.0), (8.0, 11.0));
            let made = new_ways(view, layer);
            assert_eq!(made.len(), 2);
            for w in &made {
                assert_eq!(
                    tag(view, layer, *w, "building:part").as_deref(),
                    Some("yes")
                );
                assert_eq!(tag(view, layer, *w, "height").as_deref(), Some("6"));
            }
            // The outline gained the two cut nodes and keeps its tags.
            assert_eq!(way_nodes(view, layer, 100).unwrap().len(), 7);
            assert_eq!(tag(view, layer, 100, "building").as_deref(), Some("yes"));
        })
        .unwrap();
}

#[gpui::test]
fn a_cut_that_misses_changes_nothing(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            view.building3d_commit_split_line(0, (50.0, -1.0), (50.0, 11.0));
            assert!(new_ways(view, layer).is_empty());
            assert!(view.undo_stack.actions.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn stacking_a_part_at_a_height(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(true, false));
    window
        .update(cx, |view, _w, _cx| {
            open(view, layer, BuildingRoot::Way(100));
            let part = view.building3d_part_index_for_tests(101).unwrap();
            view.building3d_commit_split_height(part, 6.0);
            let made = new_ways(view, layer);
            assert_eq!(made.len(), 1);
            assert_eq!(tag(view, layer, 101, "height").as_deref(), Some("6"));
            assert_eq!(
                tag(view, layer, 101, "building:levels").as_deref(),
                Some("2")
            );
            let upper = made[0];
            assert_eq!(way_nodes(view, layer, upper), way_nodes(view, layer, 101));
            assert_eq!(tag(view, layer, upper, "min_height").as_deref(), Some("6"));
            assert_eq!(tag(view, layer, upper, "height").as_deref(), Some("12"));
            assert_eq!(view.selected[0].id, upper);
            undo_step(view);
            assert!(new_ways(view, layer).is_empty());
            assert_eq!(tag(view, layer, 101, "height").as_deref(), Some("12"));
        })
        .unwrap();
}

#[gpui::test]
fn the_scene_follows_undo_and_closes_when_the_building_goes(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, fixture(false, false));
    window
        .update(cx, |view, window, cx| {
            open(view, layer, BuildingRoot::Way(100));
            view.building3d_commit_new_part([(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)], 6.0);
            view.building3d_frame(window, cx);
            assert_eq!(view.building3d_part_count_for_tests(), 2);
            undo_step(view);
            view.building3d_frame(window, cx);
            assert_eq!(view.building3d_part_count_for_tests(), 1);

            // Deleting the building closes the pane.
            view.set_selection(vec![FeatureRef {
                layer_id: layer,
                kind: FeatureKind::Way,
                id: 100,
            }]);
            view.delete_selected_features(cx);
            view.building3d_frame(window, cx);
            assert!(view.building3d.is_none());
        })
        .unwrap();
}
