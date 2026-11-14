//! Extrude mode: drag a way's segment to reshape the way, move the segment,
//! or pull a new way out of it; double-click a segment to add a node.

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::interaction;
use osm_gpui::layers::LayerId;

use super::preview::{blue, line_width, stroke_polyline, PreviewFrame};
use crate::MapViewer;

/// What an Extrude-mode drag does, fixed by the modifier keys held at
/// mouse-down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExtrudeStyle {
    /// Default: reshape the way, growing or notching it (JOSM's plain drag).
    Reshape,
    /// Cmd (Ctrl off macOS): translate the segment's endpoints, no new nodes.
    Move,
    /// Alt/Option: pull a new separate, untagged way out of the segment.
    NewWay,
}

impl ExtrudeStyle {
    fn from_modifiers(m: gpui::Modifiers) -> Self {
        if m.alt {
            ExtrudeStyle::NewWay
        } else if m.secondary() {
            ExtrudeStyle::Move
        } else {
            ExtrudeStyle::Reshape
        }
    }
}

/// The way segment being extruded from, and the two endpoint node ids used to
/// build the preview/final rectangle.
pub(crate) struct ExtrudeDrag {
    pub(crate) layer: LayerId,
    pub(crate) way_id: i64,
    pub(crate) node_a: i64,
    pub(crate) node_b: i64,
    /// Index of the segment (`node_a`) in the way's node list.
    pub(crate) segment: usize,
    pub(crate) style: ExtrudeStyle,
    /// Screen position of the mouse-down that started this drag, for the
    /// click/drag threshold check on release: unlike Select mode's move-drag
    /// this does not go through the shared `Interaction` state machine.
    pub(crate) down: Point<Pixels>,
}

/// An Extrude drag resolved against the current cursor: the plan to apply and
/// the resulting outline in screen coordinates, for the live preview.
pub(crate) struct ExtrudeResult {
    pub(crate) plan: osm_gpui::extrude::ExtrudePlan,
    /// The way's node list after the edit, as screen positions.
    pub(crate) outline: Vec<(f64, f64)>,
}

#[derive(Default)]
pub(crate) struct ExtrudeState {
    pub(crate) drag: Option<ExtrudeDrag>,
}

/// The index `i` such that `node_ids[i] == a && node_ids[i + 1] == b`,
/// preferring `hint` (the hit-test's index) when it is consistent.
pub(crate) fn segment_index_in_way(node_ids: &[i64], hint: usize, a: i64, b: i64) -> Option<usize> {
    let matches = |i: usize| node_ids.get(i) == Some(&a) && node_ids.get(i + 1) == Some(&b);
    if matches(hint) {
        return Some(hint);
    }
    (0..node_ids.len().saturating_sub(1)).find(|&i| matches(i))
}

/// Start a drag from the segment under the press, if any.
pub(super) fn press(view: &mut MapViewer, position: Point<Pixels>, modifiers: gpui::Modifiers) {
    let Some(layer_id) = view.active_layer else {
        return;
    };
    let Some(layer) = view.layer_manager.find_layer(layer_id) else {
        return;
    };
    let Some(osm_layer) = layer.as_osm() else {
        return;
    };
    // A segment of the selected way wins over a nearer segment of an
    // overlapping one.
    let selected_way = view
        .selected
        .iter()
        .find(|f| f.layer_id == layer_id && f.kind == osm_gpui::selection::FeatureKind::Way)
        .map(|f| f.id);
    let Some((way_id, node_a, node_b, idx)) =
        osm_layer.hit_test_segment_preferring(&view.viewport, position, 6.0, selected_way)
    else {
        return;
    };
    let segment = layer
        .as_editable()
        .and_then(|e| e.way_node_ids(way_id))
        .and_then(|ids| segment_index_in_way(&ids, idx, node_a, node_b));
    if let Some(segment) = segment {
        view.set_extrude_drag(Some(ExtrudeDrag {
            layer: layer_id,
            way_id,
            node_a,
            node_b,
            segment,
            style: ExtrudeStyle::from_modifiers(modifiers),
            down: position,
        }));
    }
}

/// Finish the drag: past the drag threshold it commits by style, a
/// double-click inserts a node on the segment, anything else does nothing.
/// Returns whether there was a drag.
pub(super) fn release(view: &mut MapViewer, click_count: usize, up_pos: Point<Pixels>) -> bool {
    let Some(drag) = view.take_extrude_drag() else {
        return false;
    };
    let moved = (up_pos - drag.down).magnitude() >= interaction::DRAG_THRESHOLD as f64;
    if moved {
        match drag.style {
            ExtrudeStyle::NewWay => view.commit_extrude(&drag, up_pos),
            ExtrudeStyle::Reshape | ExtrudeStyle::Move => {
                view.commit_extrude_reshape(&drag, up_pos)
            }
        }
    } else if click_count == 2 {
        view.insert_node_on_segment(&drag, up_pos);
    }
    true
}

/// Extrude works from mouse-down/move/up directly; a stray click here (a
/// zero-movement mouse-up) does nothing.
pub(super) fn click() {}

/// Escape drops the drag, or else leaves the mode.
pub(super) fn escape(view: &mut MapViewer) {
    if view.take_extrude_drag().is_none() {
        view.enter_mode(EditMode::Select);
    }
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Arrow
}

/// For Reshape/Move, the way's whole outline as it would be after the drop;
/// for NewWay, the rectangle that would be pulled out of the segment.
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    let Some(drag) = view.extrude_drag() else {
        return;
    };
    let Some(mouse_pos) = view.last_mouse_pos else {
        return;
    };
    if drag.style != ExtrudeStyle::NewWay {
        if let Some(result) = view.compute_extrude(drag, mouse_pos) {
            stroke_polyline(
                window,
                result.outline.iter().map(|p| frame.pixels(*p)),
                line_width(),
                blue(),
            );
        }
        return;
    }
    let Some(editable) = view
        .layer_manager
        .find_layer(drag.layer)
        .and_then(|layer| layer.as_editable())
    else {
        return;
    };
    let (Some(a_geo), Some(b_geo)) = (
        editable.node_lat_lon(drag.node_a),
        editable.node_lat_lon(drag.node_b),
    ) else {
        return;
    };
    let cursor_geo = frame.viewport.screen_to_geo(mouse_pos);
    let (far_a, far_b) = view.rectangle_from_geo_edge(a_geo, b_geo, cursor_geo);
    let a = frame.geo(a_geo);
    stroke_polyline(
        window,
        [a, frame.geo(b_geo), frame.geo(far_b), frame.geo(far_a), a],
        line_width(),
        blue(),
    );
}
