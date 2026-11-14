//! Right-click selection and the feature context menu.

use super::{setup, CENTER_LAT, CENTER_LON};
use crate::{EditMode, MapViewer};
use gpui::{point, px, size, TestAppContext};
use osm_gpui::coordinates::LatLon;

/// Window position of the fixture way's centre (map-local `(400, 300)`).
fn on_the_way() -> gpui::Point<gpui::Pixels> {
    point(px(400.0 + MapViewer::MODE_PANEL_WIDTH), px(300.0))
}

#[gpui::test]
fn right_click_selects_the_feature_and_opens_the_menu(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.enter_mode(EditMode::Select);
            view.right_press = Some(on_the_way());
            view.finish_right_press(on_the_way(), window, cx);
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 10);
            assert!(view.context_menu.is_some());
        })
        .unwrap();
}

/// The same click, but delivered as real mouse events through the
/// window's event dispatch, to cover the listeners in `render`.
#[gpui::test]
fn right_click_events_open_the_menu(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _, _| view.enter_mode(EditMode::Select))
        .unwrap();
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    // Events are only routed through a rendered frame.
    let root = window.root(vcx).unwrap();
    vcx.draw(
        point(px(0.0), px(0.0)),
        size(
            gpui::AvailableSpace::Definite(px(1200.0)),
            gpui::AvailableSpace::Definite(px(800.0)),
        ),
        |_, _| gpui::AnyView::from(root),
    );
    let pos = window
        .update(vcx, |view, _, _| {
            let p = view
                .viewport
                .geo_to_screen(LatLon::new(CENTER_LAT, CENTER_LON));
            point(p.x + px(MapViewer::MODE_PANEL_WIDTH), p.y)
        })
        .unwrap();
    vcx.simulate_mouse_down(pos, gpui::MouseButton::Right, gpui::Modifiers::none());
    vcx.simulate_mouse_up(pos, gpui::MouseButton::Right, gpui::Modifiers::none());
    window
        .update(vcx, |view, _, _| {
            assert_eq!(view.selected.len(), 1);
            assert!(view.context_menu.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn right_drag_pans_without_opening_the_menu(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.enter_mode(EditMode::Select);
            view.right_press = Some(on_the_way());
            let released = on_the_way() + point(px(30.0), px(0.0));
            view.finish_right_press(released, window, cx);
            assert!(view.selected.is_empty());
            assert!(view.context_menu.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn right_click_on_empty_map_opens_nothing(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.enter_mode(EditMode::Select);
            let empty = point(px(100.0), px(500.0));
            view.right_press = Some(empty);
            view.finish_right_press(empty, window, cx);
            assert!(view.context_menu.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn right_click_inside_the_selection_keeps_it(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.enter_mode(EditMode::Select);
            let layer_id = view.active_layer.unwrap();
            let way = osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Way,
                id: 10,
            };
            let node = osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Node,
                id: 1,
            };
            view.set_selection(vec![way, node]);
            view.right_press = Some(on_the_way());
            view.finish_right_press(on_the_way(), window, cx);
            assert_eq!(view.selected, vec![way, node]);
            assert!(view.context_menu.is_some());
        })
        .unwrap();
}
