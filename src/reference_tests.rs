//! `MapViewer` behaviour around read-only GPX / GeoJSON reference layers.

use super::*;
use gpui::{point, size, TestAppContext};
use osm_gpui::reference::{Builder, FeatureKind, Format};
use std::collections::HashMap;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

fn track() -> osm_gpui::reference::ReferenceData {
    let mut b = Builder::new(Format::Gpx);
    let f = b.feature(FeatureKind::Track, "", Some("Walk".into()), vec![]);
    // A line through the view centre, west to east.
    b.line(f, &[(LON - 0.001, LAT), (LON + 0.001, LAT)])
        .unwrap();
    b.finish().unwrap()
}

fn osm_layer(view: &mut MapViewer) -> LayerId {
    let id = view.layer_manager.alloc_id();
    let data = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    view.layer_manager
        .add_layer(Box::new(OsmLayer::new_with_data(
            id,
            "data".to_string(),
            Arc::new(data),
        )));
    id
}

fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _w, _cx| {
            view.viewport = Viewport::new(LAT, LON, 18.0, size(px(800.0), px(600.0)));
        })
        .unwrap();
    window
}

#[gpui::test]
fn a_reference_layer_is_inert_for_editing(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let data_id = osm_layer(view);
            view.active_layer = Some(data_id);
            view.add_reference_dataset("walk".into(), track(), cx);
            // Below the data layer, not the active layer.
            let names: Vec<_> = view
                .layer_manager
                .layers()
                .iter()
                .map(|l| l.name())
                .collect();
            assert_eq!(names, vec!["walk", "data"]);
            assert_eq!(view.active_layer, Some(data_id));
            assert!(!has_unsaved_changes_in(view));
            assert_eq!(view.active_layer_pending(), Some(0));
            // Deleting needs no confirmation.
            view.on_delete_layer(&DeleteLayer { index: 0 }, window, cx);
            assert!(view.delete_layer_dialog.is_none());
            assert_eq!(view.layer_manager.layers().len(), 1);
        })
        .unwrap();
}

fn has_unsaved_changes_in(view: &MapViewer) -> bool {
    view.layer_manager.layers().iter().any(|l| l.is_modified())
}

#[gpui::test]
fn clicking_a_reference_line_shows_it_in_the_status_bar(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            view.add_reference_dataset("walk".into(), track(), cx);
            view.handle_select_click(point(px(400.0), px(302.0)), false, 1);
            let status = view.status_message.as_ref().unwrap().0.to_string();
            assert!(status.contains("Walk"), "{status}");
            assert!(view.selected.is_empty());
            // Away from the line: no new message.
            view.status_message = None;
            view.handle_select_click(point(px(400.0), px(500.0)), false, 1);
            assert!(view.status_message.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn add_mode_snaps_a_new_node_onto_the_reference_line(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let data_id = osm_layer(view);
            view.active_layer = Some(data_id);
            view.add_reference_dataset("walk".into(), track(), cx);
            view.mode = EditMode::Add;
            // 4 px below the line, between its two vertices.
            view.handle_add_click(point(px(430.0), px(304.0)), false);
            let node = view.selected[0];
            let (lat, _lon) = view
                .layer_manager
                .find_layer(data_id)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.node_lat_lon(node.id))
                .unwrap();
            assert!((lat - LAT).abs() < 1e-7, "{lat}");
            assert_eq!(view.active_layer_pending(), Some(1));
            // Ctrl bypasses snapping.
            view.add_progress = None;
            view.handle_add_click(point(px(430.0), px(304.0)), true);
            let node = view.selected[0];
            let (lat, _) = view
                .layer_manager
                .find_layer(data_id)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.node_lat_lon(node.id))
                .unwrap();
            assert!((lat - LAT).abs() > 1e-6, "{lat}");
        })
        .unwrap();
}

#[gpui::test]
fn a_hidden_reference_layer_does_not_snap_or_report(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            view.add_reference_dataset("walk".into(), track(), cx);
            let id = view.layer_manager.layers()[0].id();
            view.toggle_layer_visibility(id);
            assert!(view
                .reference_feature_at(point(px(400.0), px(300.0)))
                .is_none());
            assert!(view
                .reference_snap_at(point(px(400.0), px(300.0)))
                .is_none());
        })
        .unwrap();
}

#[gpui::test]
fn a_single_point_is_centred_at_an_editing_zoom(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let mut b = Builder::new(Format::GeoJson);
            let f = b.feature(FeatureKind::Geo, "Point", None, vec![]);
            b.point(f, 8.5, 47.25).unwrap();
            view.add_reference_dataset("pt".into(), b.finish().unwrap(), cx);
            let (lat, lon) = view.viewport.center();
            assert!((lat - 47.25).abs() < 1e-6 && (lon - 8.5).abs() < 1e-6);
            assert!(view.viewport.zoom_level() >= 18.0);
        })
        .unwrap();
}
