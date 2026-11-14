//! Merging a download into the active layer, exporting a layer, and the downloaded-area overlay.

use super::{empty_tags, setup, setup_network, way_fixture, CENTER_LAT, CENTER_LON};
use crate::{MapViewer, ToggleDownloadedArea, DOWNLOAD_LAYER_NAME};
use gpui::{point, px, TestAppContext};
use osm_gpui::osm::{OsmData, OsmNode};

/// The way fixture's data plus one extra node (id 50), as a second
/// download overlapping the first would return.
fn overlapping_download() -> OsmData {
    let mut data = way_fixture(CENTER_LAT, CENTER_LON);
    data.nodes.insert(
        50,
        OsmNode {
            id: 50,
            lat: CENTER_LAT + 0.001,
            lon: CENTER_LON,
            version: 1,
            tags: empty_tags(),
        },
    );
    data
}

#[gpui::test]
fn download_merges_into_the_active_layer_and_keeps_local_edits(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            // A local edit to node 1 before the second download arrives.
            view.layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .commit_node_moves(&[(1, CENTER_LAT + 0.0005, CENTER_LON - 0.001)])
                .unwrap();

            view.apply_download(overlapping_download());

            assert_eq!(view.layer_manager.layers().len(), 1, "no second layer");
            let layer = view.layer_manager.find_layer(layer_id).unwrap();
            let editable = layer.as_editable().unwrap();
            assert!(editable.node_lat_lon(50).is_some(), "new node merged in");
            assert_eq!(
                editable.node_lat_lon(1),
                Some((CENTER_LAT + 0.0005, CENTER_LON - 0.001)),
                "local move kept"
            );
            // Still exactly one pending edit: the move.
            let osm = layer.as_osm().unwrap();
            assert_eq!(osm.diff_for_upload().counts(), (0, 1, 0));
            assert!(osm.is_modified());
            let (message, _) = view.status_message.clone().unwrap();
            assert_eq!(message, "Downloaded OSM data: 1 new, 1 local edit kept");
        })
        .unwrap();
}

#[gpui::test]
fn download_with_no_active_layer_creates_one(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.active_layer.is_none());

            view.apply_download(overlapping_download());

            let layers = view.layer_manager.layers();
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].name(), DOWNLOAD_LAYER_NAME);
            assert_eq!(view.active_layer, Some(layers[0].id()));
            assert!(!layers[0].as_osm().unwrap().is_modified());
        })
        .unwrap();
}

#[gpui::test]
fn exported_layer_reopens_with_the_same_pending_edits(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            // Two kinds of pending edit: a moved node and a created one.
            view.layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .commit_node_moves(&[(1, CENTER_LAT + 0.0005, CENTER_LON - 0.001)])
                .unwrap();
            view.handle_add_click(point(px(50.0), px(50.0)), false);
            let pending = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_osm()
                .unwrap()
                .diff_for_upload()
                .counts();
            assert_eq!(pending, (1, 1, 0));

            let (name, xml) = view.exportable_layer().expect("active layer exports");
            assert_eq!(name, "L");

            // Reopen the exported document as File > Open would.
            let (data, edits) = osm_gpui::osm::OsmParser::new()
                .parse_str_with_edits(&xml)
                .unwrap();
            view.add_osm_dataset("reopened".to_string(), data, &edits, cx);

            let reopened = view
                .layer_manager
                .layer_named("reopened")
                .unwrap()
                .as_osm()
                .unwrap();
            assert_eq!(reopened.diff_for_upload().counts(), pending);
            assert!(reopened.is_modified());
        })
        .unwrap();
}

#[gpui::test]
fn show_downloaded_area_toggles_and_defaults_on(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            assert!(view.shows_downloaded_area());
            view.set_show_downloaded_area(false, cx);
            assert!(!view.shows_downloaded_area());
            // Setting the current value again is a no-op.
            view.set_show_downloaded_area(false, cx);
            assert!(!view.shows_downloaded_area());
            view.on_toggle_downloaded_area(&ToggleDownloadedArea, _window, cx);
            assert!(view.shows_downloaded_area());
        })
        .unwrap();
}
