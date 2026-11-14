//! Add mode: click to place nodes and grow a way, snapping onto nearby nodes
//! and ways.

use gpui::{CursorStyle, Window};

use super::preview::{blue, line_width, stroke_polyline, PreviewFrame};
use crate::MapViewer;

/// The last-placed node, and which way (if any) it belongs to. `None` in
/// `AddState` means "no node placed yet in this continuation": the next click
/// starts fresh.
pub(crate) struct AddProgress {
    pub(crate) way_id: Option<i64>,
    pub(crate) last_node_id: i64,
    /// Continuing an existing way from its first node (Extend Way on a way's
    /// start): new nodes are prepended to `way_id` instead of appended.
    pub(crate) at_start: bool,
}

#[derive(Default)]
pub(crate) struct AddState {
    pub(crate) progress: Option<AddProgress>,
}

impl AddState {
    pub(super) fn is_drawing(&self) -> bool {
        self.progress.is_some()
    }
}

pub(super) fn click(view: &mut MapViewer, at: gpui::Point<gpui::Pixels>, snapping_off: bool) {
    view.handle_add_click(at, snapping_off);
}

/// Escape drops the way being drawn, or else leaves the mode.
pub(super) fn escape(view: &mut MapViewer) {
    if view.take_add_progress().is_none() {
        view.enter_mode(osm_gpui::edit_mode::EditMode::Select);
    }
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Crosshair
}

/// A rubber band from the last placed node to the cursor.
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    let Some(progress) = view.add_progress() else {
        return;
    };
    let Some(mouse_pos) = view.last_mouse_pos else {
        return;
    };
    let Some(layer_id) = view.active_layer else {
        return;
    };
    let Some(editable) = view
        .layer_manager
        .find_layer(layer_id)
        .and_then(|layer| layer.as_editable())
    else {
        return;
    };
    let Some(last_geo) = editable.node_lat_lon(progress.last_node_id) else {
        return;
    };
    stroke_polyline(
        window,
        [frame.geo(last_geo), frame.local(mouse_pos)],
        line_width(),
        blue(),
    );
}
