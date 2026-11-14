//! Arc mode: three clicks (start, end, bulge) create one open way whose
//! nodes lie on a circle, as a single undo step.

use super::*;
use crate::test_support::*;
use gpui::{point, TestAppContext};

#[gpui::test]
fn three_clicks_make_one_arc_and_one_undo_removes_it(cx: &mut TestAppContext) {
    with_osm(cx, empty_data(), EditMode::Arc, |view, w, cx, layer| {
        let c = view.viewport.geo_to_screen(LAT, LON);
        // Chord along the horizontal, bulging upward: a shallow arc.
        let start = c + point(px(-100.0), px(0.0));
        let end = c + point(px(100.0), px(0.0));
        let bulge = c + point(px(0.0), px(-60.0));

        crate::modes::arc::click(view, start, false);
        assert!(view.arc_progress().is_some_and(|p| p.end.is_none()));
        crate::modes::arc::click(view, end, false);
        assert!(view.arc_progress().is_some_and(|p| p.end.is_some()));
        assert_eq!(osm_counts(view, layer), (0, 0), "nothing is created early");

        crate::modes::arc::click(view, bulge, false);
        assert!(view.arc_progress().is_none());
        let (nodes, ways) = osm_counts(view, layer);
        assert_eq!(ways, 1);
        assert!(nodes >= 3, "an arc has interior nodes, got {nodes}");
        assert_eq!(view.undo_stack.cursor, 1, "one undo step");

        // The new way is open, untagged and selected.
        let [sel] = view.selected.as_slice() else {
            panic!("the new way should be selected");
        };
        let editable = view
            .layer_manager
            .find_layer(layer)
            .unwrap()
            .as_editable()
            .unwrap();
        let ids = editable.way_node_ids(sel.id).unwrap();
        assert_eq!(ids.len(), nodes);
        assert_ne!(ids.first(), ids.last());

        // Every node sits on one circle (checked on screen, where the
        // arc was constructed) through the three clicks.
        let screen: Vec<(f64, f64)> = ids
            .iter()
            .map(|&id| {
                let (lat, lon) = editable.node_lat_lon(id).unwrap();
                let p = view.viewport.geo_to_screen(lat, lon);
                (p.x.to_f64(), p.y.to_f64())
            })
            .collect();
        let (a, b) = (screen[0], screen[screen.len() - 1]);
        assert!((a.0 - start.x.to_f64()).abs() < 0.5 && (a.1 - start.y.to_f64()).abs() < 0.5);
        assert!((b.0 - end.x.to_f64()).abs() < 0.5 && (b.1 - end.y.to_f64()).abs() < 0.5);
        // Chord 200 px, sagitta 60 px -> radius (100^2 + 60^2) / (2 * 60).
        let radius = (100.0_f64.powi(2) + 60.0_f64.powi(2)) / 120.0;
        let center = (c.x.to_f64(), c.y.to_f64() - 60.0 + radius);
        for p in &screen {
            let d = ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt();
            assert!((d - radius).abs() < 0.5, "off the circle by {}", d - radius);
        }

        view.on_undo(&Undo, w, cx);
        assert_eq!(
            osm_counts(view, layer),
            (0, 0),
            "undo removes the whole arc"
        );
    });
}

#[gpui::test]
fn a_collinear_third_click_is_ignored(cx: &mut TestAppContext) {
    with_osm(cx, empty_data(), EditMode::Arc, |view, _w, _cx, layer| {
        let c = view.viewport.geo_to_screen(LAT, LON);
        crate::modes::arc::click(view, c + point(px(-100.0), px(0.0)), false);
        crate::modes::arc::click(view, c + point(px(100.0), px(0.0)), false);
        // On the chord itself: no circle, so the arc stays pending.
        crate::modes::arc::click(view, c, false);
        assert!(view.arc_progress().is_some_and(|p| p.end.is_some()));
        assert_eq!(osm_counts(view, layer), (0, 0));
    });
}

#[gpui::test]
fn endpoints_snap_onto_a_way_and_are_spliced_into_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .node_at(1, LAT, LON - 0.002)
        .node_at(2, LAT, LON + 0.002)
        .way(10, &[1, 2], &[])
        .done();
    with_osm(cx, data, EditMode::Arc, |view, w, cx, layer| {
        let c = view.viewport.geo_to_screen(LAT, LON);
        // Start and end a few pixels off the horizontal way; the bulge
        // is far from it.
        crate::modes::arc::click(view, c + point(px(-100.0), px(4.0)), false);
        crate::modes::arc::click(view, c + point(px(100.0), px(-4.0)), false);
        crate::modes::arc::click(view, c + point(px(0.0), px(-60.0)), false);

        let editable = view
            .layer_manager
            .find_layer(layer)
            .unwrap()
            .as_editable()
            .unwrap();
        let host = editable.way_node_ids(10).unwrap();
        assert_eq!(host.len(), 4, "both endpoints are spliced into the way");
        let [sel] = view.selected.as_slice() else {
            panic!("the new way should be selected");
        };
        let arc = editable.way_node_ids(sel.id).unwrap();
        assert!(host.contains(arc.first().unwrap()));
        assert!(host.contains(arc.last().unwrap()));
        // The spliced nodes lie exactly on the way.
        for id in [arc[0], arc[arc.len() - 1]] {
            assert!((editable.node_lat_lon(id).unwrap().0 - LAT).abs() < 1e-9);
        }

        // Ctrl/Alt turns snapping off: nothing is spliced.
        assert_eq!(view.undo_stack.cursor, 1, "the whole arc is one undo step");
        view.on_undo(&Undo, w, cx);
        let host_len = |view: &MapViewer| {
            view.layer_manager
                .find_layer(layer)
                .unwrap()
                .as_editable()
                .unwrap()
                .way_node_ids(10)
                .unwrap()
                .len()
        };
        assert_eq!(host_len(view), 2, "undo removes the splices too");
        crate::modes::arc::click(view, c + point(px(-100.0), px(4.0)), true);
        crate::modes::arc::click(view, c + point(px(100.0), px(-4.0)), true);
        crate::modes::arc::click(view, c + point(px(0.0), px(-60.0)), true);
        assert_eq!(host_len(view), 2);
    });
}

#[gpui::test]
fn endpoints_snap_onto_existing_vertices_and_reuse_them(cx: &mut TestAppContext) {
    let data = Build::new()
        .node_at(1, LAT, LON - 0.0005)
        .node_at(2, LAT, LON + 0.0005)
        .way(10, &[1, 2], &[])
        .done();
    with_osm(cx, data, EditMode::Arc, |view, w, cx, layer| {
        let a = view.viewport.geo_to_screen(LAT, LON - 0.0005);
        let b = view.viewport.geo_to_screen(LAT, LON + 0.0005);
        let c = view.viewport.geo_to_screen(LAT, LON);
        crate::modes::arc::click(view, a + point(px(2.0), px(2.0)), false);
        crate::modes::arc::click(view, b + point(px(-2.0), px(1.0)), false);
        crate::modes::arc::click(view, c + point(px(0.0), px(-60.0)), false);

        let editable = view
            .layer_manager
            .find_layer(layer)
            .unwrap()
            .as_editable()
            .unwrap();
        assert_eq!(editable.way_node_ids(10).unwrap(), vec![1, 2], "no splices");
        let [sel] = view.selected.as_slice() else {
            panic!("the new way should be selected");
        };
        let arc = editable.way_node_ids(sel.id).unwrap();
        assert_eq!(
            (arc[0], arc[arc.len() - 1]),
            (1, 2),
            "existing nodes reused"
        );

        view.on_undo(&Undo, w, cx);
        assert!(view
            .layer_manager
            .find_layer(layer)
            .unwrap()
            .as_editable()
            .unwrap()
            .node_lat_lon(1)
            .is_some());
        assert_eq!(
            osm_counts(view, layer),
            (2, 1),
            "only the arc's own nodes go"
        );
    });
}
