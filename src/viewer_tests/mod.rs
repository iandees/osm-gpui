//! Tests that drive a `MapViewer` in a headless window, one file per area.
//! The fixtures several areas share live here.

mod add_mode;
mod autosave;
mod cli_and_window;
mod clipboard;
mod context_menu;
mod delete_layer;
mod download_merge;
mod find;
mod preset_fields;
mod relation_edits;
mod relation_members;
mod script_click_targets;
mod script_ops;
mod shape_actions;
mod undo_selection;
mod upload_conflicts;
mod upload_dialog;
mod upload_guards;
mod validation_section;
mod vertex_preview;

use crate::undo::UndoableAction;
use crate::{selection_actions, EditMode, MapViewer};
use gpui::prelude::*;
use gpui::{px, size, TestAppContext};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::osm::{OsmData, OsmNode, OsmWay};
use osm_gpui::viewport::Viewport;
use std::collections::HashMap;
use std::sync::Arc;

fn empty_tags() -> HashMap<String, String> {
    HashMap::new()
}

/// A single 2-node way `10 = [1, 2]`, both nodes at `center_lat`, lon
/// `center_lon - 0.001` / `+ 0.001` — a flat horizontal line through the
/// viewport's center at zoom 18 (matches the convention used by
/// `OsmLayer`'s own tests in `src/layers/osm_layer/`).
fn way_fixture(center_lat: f64, center_lon: f64) -> OsmData {
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon - 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let mut nodes = HashMap::new();
    nodes.insert(1, n1);
    nodes.insert(2, n2);
    let mut ways = HashMap::new();
    ways.insert(10, way);
    OsmData {
        nodes,
        ways,
        relations: Vec::new(),
        bounds: Vec::new(),
    }
}

const CENTER_LAT: f64 = 40.0;

const CENTER_LON: f64 = -74.0;

/// Builds a `MapViewer` in a headless test window, viewport centered on
/// `way_fixture`'s line (so screen `(400, 300)` lands exactly on it, per
/// the same convention as `OsmLayer`'s own tests), with one active OSM
/// layer containing that fixture, in Add mode.
fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, _cx| {
            view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_data(
                layer_id,
                "L".to_string(),
                Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
            );
            view.layer_manager.add_layer(Box::new(layer));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Add;
        })
        .unwrap();
    window
}

fn way_nodes(view: &MapViewer, layer_id: LayerId, way_id: i64) -> Vec<i64> {
    view.layer_manager
        .find_layer(layer_id)
        .and_then(|l| l.as_editable())
        .and_then(|e| e.way_node_ids(way_id))
        .unwrap_or_default()
}

fn feature(
    view: &MapViewer,
    kind: osm_gpui::selection::FeatureKind,
    id: i64,
) -> osm_gpui::selection::FeatureRef {
    osm_gpui::selection::FeatureRef {
        layer_id: view.active_layer.unwrap(),
        kind,
        id,
    }
}

/// The tags of way/node `id` in the view's active layer, sorted.
fn tags_of(
    view: &MapViewer,
    kind: osm_gpui::selection::FeatureKind,
    id: i64,
) -> Vec<(String, String)> {
    let layer_id = view.active_layer.unwrap();
    let mut tags = view
        .layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_editable()
        .unwrap()
        .feature_tags(&osm_gpui::selection::FeatureRef { layer_id, kind, id })
        .unwrap();
    tags.sort();
    tags
}

fn tag_pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node_position(view: &MapViewer, id: i64) -> (f64, f64) {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_editable()
        .unwrap()
        .node_lat_lon(id)
        .unwrap()
}

fn ways_at_node(view: &MapViewer, node: i64) -> Vec<i64> {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_editable()
        .unwrap()
        .ways_using_node(node)
}

fn status(view: &MapViewer) -> Option<&str> {
    view.status_message.as_ref().map(|(m, _)| m.as_str())
}

/// A view whose active layer holds `selection_actions::test_support`'s
/// road network, with a server baseline so the upload diff is meaningful.
fn setup_network(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
    setup_network_with(cx, |_| {})
}

/// `setup_network` with `edit` applied to the network data first (e.g. to
/// swap in different relations).
fn setup_network_with(
    cx: &mut TestAppContext,
    edit: impl FnOnce(&mut osm_gpui::osm::OsmData),
) -> gpui::WindowHandle<MapViewer> {
    let mut data = selection_actions::test_support::network_data();
    edit(&mut data);
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, _cx| {
            view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_data(layer_id, "Net".to_string(), Arc::new(data));
            view.layer_manager.add_layer(Box::new(layer));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Select;
        })
        .unwrap();
    window
}

/// Replace the network's relation 900 with a route over `members`
/// (`(way id, role)`).
fn route_over(ways: &'static [(i64, &'static str)]) -> impl FnOnce(&mut osm_gpui::osm::OsmData) {
    move |data| {
        data.relations = vec![osm_gpui::osm::OsmRelation {
            id: 900,
            version: 4,
            tags: [("type", "route"), ("name", "Test Route")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            members: ways
                .iter()
                .map(|&(reference, role)| osm_gpui::osm::OsmMember {
                    member_type: osm_gpui::osm::MemberType::Way,
                    reference,
                    role: role.to_string(),
                })
                .collect(),
        }];
    }
}

fn select(view: &mut MapViewer, kind: osm_gpui::selection::FeatureKind, ids: &[i64]) {
    let layer_id = view.active_layer.unwrap();
    view.set_selection(
        ids.iter()
            .map(|&id| osm_gpui::selection::FeatureRef { layer_id, kind, id })
            .collect(),
    );
}

fn pending(view: &MapViewer) -> (usize, usize, usize) {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_osm()
        .unwrap()
        .diff_for_upload()
        .counts()
}

fn undo_step(view: &mut MapViewer) {
    let action = view.undo_stack.undo().expect("something to undo");
    view.apply_undo_action(&action, false);
}

fn redo_step(view: &mut MapViewer) {
    let action = view.undo_stack.redo().expect("something to redo");
    view.apply_undo_action(&action, true);
}

fn relation_members(view: &MapViewer, relation_id: i64) -> Vec<(i64, String)> {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_editable()
        .unwrap()
        .relation(relation_id)
        .unwrap()
        .members
        .iter()
        .map(|m| (m.reference, m.role.clone()))
        .collect()
}

fn editable(view: &mut MapViewer) -> &mut dyn osm_gpui::layers::EditableLayer {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer_mut(layer_id)
        .unwrap()
        .as_editable_mut()
        .unwrap()
}

fn status_text(view: &MapViewer) -> Option<&str> {
    view.status_message.as_ref().map(|(m, _)| m.as_str())
}

/// Way 100 reshaped to `len` nodes, still from node 1 to node 3.
fn long_way_nodes(len: usize) -> Vec<i64> {
    let mut nodes = vec![1];
    nodes.extend(std::iter::repeat_n(2, len - 2));
    nodes.push(3);
    nodes
}

/// Give the test view's layer a pending change (one new node).
fn make_pending_change(view: &mut MapViewer) {
    let id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer_mut(id)
        .and_then(|l| l.as_editable_mut())
        .unwrap()
        .create_node(CENTER_LAT, CENTER_LON, None)
        .unwrap();
}

/// A signed-in view with the one-way fixture (way 10 untagged, an old
/// problem nobody touched) in a headless window.
fn validation_view(
    cx: &mut TestAppContext,
) -> (gpui::Entity<MapViewer>, &mut gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| MapViewer::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    let vcx = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
    let root = window.root(vcx).unwrap();
    let view: gpui::Entity<MapViewer> = root
        .read_with(vcx, |root, _| root.view().clone())
        .downcast::<MapViewer>()
        .unwrap();
    view.update_in(vcx, |view, _window, _cx| {
        view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
        let layer_id = view.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(
            layer_id,
            "L".to_string(),
            Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
        );
        view.layer_manager.add_layer(Box::new(layer));
        view.active_layer = Some(layer_id);
    });
    (view, vcx)
}

/// `setup`'s layer with node 1 tagged locally (and that edit on the undo
/// stack), as it would be when the server then refuses the upload.
fn tag_node_one(view: &mut MapViewer) {
    let layer_id = view.active_layer.unwrap();
    editable(view)
        .set_tag(osm_gpui::selection::FeatureKind::Node, 1, "amenity", "cafe")
        .unwrap();
    view.undo_stack.push(UndoableAction::SetTags {
        entries: vec![(
            osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Node,
                id: 1,
            },
            "amenity".to_string(),
            None,
            Some("cafe".to_string()),
        )],
    });
}
