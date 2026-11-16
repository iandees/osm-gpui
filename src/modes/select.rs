//! Select mode: click to select, drag to box-select or move the selection,
//! hover to preview what a click would pick. The press, drag and release
//! gesture itself is shared by every mode that does not take over the left
//! button (see `press_gesture`).

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::interaction;

use super::preview::PreviewFrame;
use crate::{to_pt, MapViewer};

/// Select mode's gestures live in `MapViewer::interaction`, which also serves
/// the other modes' presses, so it holds nothing of its own.
#[derive(Default)]
pub(crate) struct SelectState;

/// A left press that is not owned by a mode's own gesture: starts a
/// move-drag if it lands on the selection, else a click or box-select.
/// `grab_vertices` lets a press on a visible, unselected vertex grab just
/// that node, as in iD and JOSM: select-and-move in one gesture (Shift keeps
/// the toggle-on-click meaning).
pub(super) fn press_gesture(
    view: &mut MapViewer,
    position: Point<Pixels>,
    modifiers: gpui::Modifiers,
    grab_vertices: bool,
) {
    view.pointer.vertex_grab = None;
    let mut hit_move_targets = if view.selected.is_empty()
        || view
            .layer_manager
            .hit_test_selection(&view.viewport, position, &view.selected)
            .is_none()
    {
        None
    } else {
        let per_layer = view.resolve_move_targets();
        if per_layer.is_empty() {
            None
        } else {
            Some(per_layer)
        }
    };
    if hit_move_targets.is_none() && grab_vertices && !modifiers.shift {
        if let Some((feature, targets)) = view.unselected_node_grab(position) {
            view.pointer.vertex_grab = Some(feature);
            hit_move_targets = Some(targets);
        }
    }
    view.pointer.interaction = interaction::on_left_mouse_down(to_pt(position), hit_move_targets);
}

/// Whether the hover highlight and the vertex preview follow the pointer: only
/// in Select mode, with no button down, no pan pending and no imagery offset
/// adjustment going on.
pub(super) fn hover_enabled(view: &MapViewer, button_down: bool) -> bool {
    !button_down && !view.pointer.space_held && view.offset_adjust.is_none()
}

/// Resolve a plain click into a selection change. `shift_held` toggles the
/// hit feature in/out of the existing selection (add if absent, remove if
/// already selected) instead of replacing it; a shift-click that hits nothing
/// is a no-op, leaving the existing selection intact.
pub(crate) fn click(
    view: &mut MapViewer,
    screen_pt: Point<Pixels>,
    shift_held: bool,
    click_count: usize,
) {
    let per_layer = view.layer_manager.hit_test_all(&view.viewport, screen_pt);
    let mut hit = osm_gpui::selection::resolve_hits(per_layer);
    // Double-click inside a closed way's interior (away from its outline,
    // which a plain click already selects) selects the way.
    if hit.is_none() && click_count == 2 {
        let interior = view
            .layer_manager
            .hit_test_interior_all(&view.viewport, screen_pt);
        hit = osm_gpui::selection::resolve_hits(interior);
    }
    // Data underneath always wins. A click on nothing but a read-only
    // reference feature (GPX, GeoJSON) shows what it is in the status bar.
    if hit.is_none() && !shift_held {
        if let Some(text) = view.reference_feature_at(screen_pt) {
            view.set_status(text);
        }
    }
    view.selected = osm_gpui::selection::apply_click_selection(&view.selected, hit, shift_held);
    view.fields_panel.fields_text_inputs.clear();
    view.fields_panel.fields_text_subscribed.clear();
    view.fields_panel.fields_open_combo = None;
    view.fields_panel.fields_promoted_more_fields.clear();
}

/// Escape clears the selection, unless it just cancelled a move-drag.
pub(super) fn escape(view: &mut MapViewer, cancelled_drag: bool) {
    if !cancelled_drag {
        view.set_selection(Vec::new());
    }
}

pub(super) fn cursor(hovering: bool) -> CursorStyle {
    if hovering {
        CursorStyle::PointingHand
    } else {
        CursorStyle::Arrow
    }
}

/// Nothing is previewed beyond the highlights every mode draws.
pub(super) fn paint_preview(_view: &MapViewer, _frame: &PreviewFrame<'_>, _window: &mut Window) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use gpui::TestAppContext;
    use osm_gpui::edit_mode::EditMode;
    use osm_gpui::interaction::Interaction;

    #[test]
    fn hover_points_a_hand_at_something_clickable() {
        assert_eq!(cursor(true), CursorStyle::PointingHand);
        assert_eq!(cursor(false), CursorStyle::Arrow);
    }

    #[gpui::test]
    fn a_press_on_nothing_starts_a_click_or_box_select(cx: &mut TestAppContext) {
        with_osm(cx, empty_data(), EditMode::Select, |view, _w, _cx, _l| {
            let at = gpui::point(gpui::px(10.0), gpui::px(10.0));
            press_gesture(view, at, gpui::Modifiers::none(), true);
            assert!(!matches!(view.pointer.interaction, Interaction::Idle));
            assert!(view.pointer.vertex_grab.is_none());
        });
    }

    #[gpui::test]
    fn hover_needs_no_button_pan_or_offset_adjustment(cx: &mut TestAppContext) {
        with_osm(cx, empty_data(), EditMode::Select, |view, _w, _cx, _l| {
            assert!(hover_enabled(view, false));
            assert!(!hover_enabled(view, true));
            view.pointer.space_held = true;
            assert!(!hover_enabled(view, false));
        });
    }
}
