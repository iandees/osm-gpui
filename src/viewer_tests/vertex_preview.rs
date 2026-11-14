//! Hover previews of a way's vertices in Select mode, and press-drag on a previewed vertex.

use super::{feature, setup};
use crate::{menu, EditMode, MapViewer};
use gpui::{point, px, Context, MouseMoveEvent, MouseUpEvent, TestAppContext};
use osm_gpui::interaction::Interaction;

/// Window position of a map-local point (inverse of `window_to_map`).
fn win_pt(p: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
    point(
        p.x + px(MapViewer::MODE_PANEL_WIDTH),
        p.y + menu::top_chrome_height(),
    )
}

fn pointer_to(
    view: &mut MapViewer,
    p: gpui::Point<gpui::Pixels>,
    pressed: bool,
    cx: &mut Context<MapViewer>,
) {
    view.handle_mouse_move(
        &MouseMoveEvent {
            position: win_pt(p),
            pressed_button: pressed.then_some(gpui::MouseButton::Left),
            modifiers: gpui::Modifiers::none(),
        },
        cx,
    );
}

fn press(view: &mut MapViewer, p: gpui::Point<gpui::Pixels>) {
    view.handle_map_mouse_down(p, gpui::Modifiers::none());
}

fn release(view: &mut MapViewer, p: gpui::Point<gpui::Pixels>, cx: &mut Context<MapViewer>) {
    view.handle_mouse_up(
        &MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: win_pt(p),
            modifiers: gpui::Modifiers::none(),
            click_count: 1,
        },
        cx,
    );
}

fn preview_way_of(view: &MapViewer) -> Option<i64> {
    let id = view.active_layer?;
    view.layer_manager
        .find_layer(id)?
        .as_osm()?
        .vertex_preview_way()
}

fn node_screen(view: &MapViewer, id: i64) -> gpui::Point<gpui::Pixels> {
    let (lat, lon) = view
        .layer_manager
        .find_layer(view.active_layer.unwrap())
        .and_then(|l| l.as_editable())
        .and_then(|e| e.node_lat_lon(id))
        .unwrap();
    view.viewport.geo_to_screen(lat, lon)
}

/// `setup`, switched to Select mode.
fn select_setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
    let window = setup(cx);
    window
        .update(cx, |view, _w, _cx| view.mode = EditMode::Select)
        .unwrap();
    window
}

#[gpui::test]
fn hovering_a_way_previews_its_vertices_and_a_vertex_becomes_the_hover(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            // Far from everything: nothing hovered, nothing previewed.
            pointer_to(view, point(px(400.0), px(500.0)), false, cx);
            assert_eq!(view.hovered, None);
            assert_eq!(preview_way_of(view), None);
            // On the line: the way is hovered and previewed.
            pointer_to(view, point(px(400.0), px(300.0)), false, cx);
            assert_eq!(view.hovered, Some(feature(view, FeatureKind::Way, 10)));
            assert_eq!(preview_way_of(view), Some(10));
            // Onto a vertex: the node is hovered, the way stays previewed.
            let n1 = node_screen(view, 1);
            pointer_to(view, n1, false, cx);
            assert_eq!(view.hovered, Some(feature(view, FeatureKind::Node, 1)));
            assert_eq!(preview_way_of(view), Some(10));
            // A little off the line, past its tolerance: still previewed.
            pointer_to(view, point(px(400.0), px(310.0)), false, cx);
            assert_eq!(view.hovered, None);
            assert_eq!(preview_way_of(view), Some(10));
            // Away: gone.
            pointer_to(view, point(px(400.0), px(500.0)), false, cx);
            assert_eq!(preview_way_of(view), None);
        })
        .unwrap();
}

#[gpui::test]
fn a_pointer_landing_straight_on_a_vertex_hovers_the_node(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let n2 = node_screen(view, 2);
            pointer_to(view, n2, false, cx);
            assert_eq!(view.hovered, Some(feature(view, FeatureKind::Node, 2)));
        })
        .unwrap();
}

#[gpui::test]
fn no_preview_outside_select_mode_while_space_is_held_or_when_zoomed_out(cx: &mut TestAppContext) {
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let on_line = point(px(400.0), px(300.0));
            view.space_held = true;
            pointer_to(view, on_line, false, cx);
            assert_eq!(preview_way_of(view), None);
            view.space_held = false;
            pointer_to(view, on_line, false, cx);
            assert_eq!(preview_way_of(view), Some(10));
            // Switching to Add mode drops it and keeps it away.
            view.mode = EditMode::Add;
            view.clear_vertex_preview();
            pointer_to(view, on_line, false, cx);
            assert_eq!(preview_way_of(view), None);
            assert_eq!(view.hovered, None);
            // Select mode below the preview zoom.
            view.mode = EditMode::Select;
            view.viewport.set_zoom(17.0);
            pointer_to(view, on_line, false, cx);
            assert_eq!(preview_way_of(view), None);
        })
        .unwrap();
}

#[gpui::test]
fn clicking_a_previewed_vertex_selects_the_node_without_an_undo_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            pointer_to(view, point(px(400.0), px(300.0)), false, cx);
            let n1 = node_screen(view, 1);
            pointer_to(view, n1, false, cx);
            press(view, n1);
            release(view, n1, cx);
            assert_eq!(view.selected, vec![feature(view, FeatureKind::Node, 1)]);
            assert!(view.undo_stack.undo().is_none(), "a click is not an edit");
        })
        .unwrap();
}

#[gpui::test]
fn press_drag_on_a_previewed_vertex_selects_and_moves_it_in_one_undo_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            pointer_to(view, point(px(400.0), px(300.0)), false, cx);
            let n1 = node_screen(view, 1);
            let before2 = node_screen(view, 2);
            pointer_to(view, n1, false, cx);
            press(view, n1);
            let to = point(n1.x + px(30.0), n1.y + px(20.0));
            pointer_to(view, to, true, cx);
            // Selected as soon as the drag starts.
            assert_eq!(view.selected, vec![feature(view, FeatureKind::Node, 1)]);
            release(view, to, cx);
            assert_eq!(view.selected, vec![feature(view, FeatureKind::Node, 1)]);
            let moved = node_screen(view, 1);
            assert!((moved.x - to.x).abs() < px(0.5) && (moved.y - to.y).abs() < px(0.5));
            assert_eq!(node_screen(view, 2), before2, "only the grabbed node moves");
            let action = view.undo_stack.undo().expect("one undo step");
            assert_eq!(action.description(), "Moved 1 node");
            assert!(view.undo_stack.undo().is_none(), "exactly one step");
        })
        .unwrap();
}

#[gpui::test]
fn press_drag_on_a_vertex_of_a_selected_way_moves_just_that_node(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let way = feature(view, FeatureKind::Way, 10);
            view.selected = vec![way];
            let layer = view.active_layer.unwrap();
            view.layer_manager
                .find_layer_mut(layer)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .set_highlight(&[way]);
            let n1 = node_screen(view, 1);
            let before2 = node_screen(view, 2);
            press(view, n1);
            let to = point(n1.x + px(0.0), n1.y + px(40.0));
            pointer_to(view, to, true, cx);
            release(view, to, cx);
            assert_eq!(view.selected, vec![feature(view, FeatureKind::Node, 1)]);
            assert_eq!(node_screen(view, 2), before2);
            assert_eq!(
                view.undo_stack.undo().unwrap().description(),
                "Moved 1 node"
            );
            assert!(view.undo_stack.undo().is_none());
        })
        .unwrap();
}

#[gpui::test]
fn a_drag_that_starts_on_the_line_still_box_selects(cx: &mut TestAppContext) {
    let window = select_setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let n1_before = node_screen(view, 1);
            // Start between the vertices, on the line; drag out a box
            // that would enclose node 1.
            let from = point(px(300.0), px(305.0));
            pointer_to(view, from, false, cx);
            assert_eq!(preview_way_of(view), Some(10));
            press(view, from);
            let to = point(px(100.0), px(100.0));
            pointer_to(view, to, true, cx);
            release(view, to, cx);
            assert_eq!(node_screen(view, 1), n1_before, "nothing moved");
            assert!(view.undo_stack.undo().is_none());
            // The box catches only what is drawn: the preview is gone
            // once the button is down and moving, so not the vertex.
            assert!(view.selected.is_empty(), "{:?}", view.selected);
        })
        .unwrap();
}

#[gpui::test]
fn add_mode_press_on_a_vertex_does_not_grab_it(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            assert_eq!(view.mode, EditMode::Add);
            let n1 = node_screen(view, 1);
            pointer_to(view, n1, false, cx);
            assert_eq!(preview_way_of(view), None);
            press(view, n1);
            assert!(view.vertex_grab.is_none());
            assert!(matches!(view.interaction, Interaction::Pending { .. }));
        })
        .unwrap();
}
