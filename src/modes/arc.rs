//! Arc mode: three clicks (start, end, a point on the arc) make one open way
//! whose nodes lie on a circle.

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::edit_mode::EditMode;

use super::preview::{blue, fill_square, line_width, stroke_polyline, PreviewFrame};
use crate::MapViewer;

/// The arc being drawn: the start point is fixed after click 1, the end point
/// after click 2; the third click picks a point the arc passes through. Both
/// are geo (lat, lon) coordinates.
#[derive(Clone, Copy)]
pub(crate) struct ArcProgress {
    pub(crate) start: ArcPoint,
    pub(crate) end: Option<ArcPoint>,
}

/// What an Arc-mode click snapped onto.
#[derive(Clone, Copy)]
pub(crate) enum ArcSnap {
    /// An existing node, which the arc then uses as that endpoint.
    Node(i64),
    /// A way's segment (`way_id`, index of the segment's first node): the
    /// endpoint is a new node spliced into the way.
    Way(i64, usize),
}

/// A clicked Arc-mode point: its geo position (snapped onto a node or way
/// when the click was near one) and what it snapped onto.
#[derive(Clone, Copy)]
pub(crate) struct ArcPoint {
    pub(crate) geo: (f64, f64),
    pub(crate) snap: Option<ArcSnap>,
}

#[derive(Default)]
pub(crate) struct ArcState {
    pub(crate) progress: Option<ArcProgress>,
}

impl ArcState {
    pub(super) fn is_drawing(&self) -> bool {
        self.progress.is_some()
    }
}

/// Click 1 sets the arc's start, click 2 its end, click 3 a point the arc
/// passes through and commits it. Each click snaps onto a nearby way (unless
/// Ctrl/Alt is held).
pub(crate) fn click(view: &mut MapViewer, screen_pt: Point<Pixels>, no_snap: bool) {
    let Some(layer_id) = view.active_layer else {
        return;
    };
    let point = view.arc_point_at(layer_id, screen_pt, no_snap);

    match view.take_arc_progress() {
        None => {
            view.set_arc_progress(Some(ArcProgress {
                start: point,
                end: None,
            }));
        }
        Some(ArcProgress { start, end: None }) => {
            view.set_arc_progress(Some(ArcProgress {
                start,
                end: Some(point),
            }));
        }
        Some(ArcProgress {
            start,
            end: Some(end),
        }) => {
            // A third point in line with the first two defines no arc; keep
            // waiting for a better one.
            if view.arc_from_geo(start.geo, end.geo, point.geo).is_some() {
                view.commit_arc(layer_id, start, end, point.geo);
            } else {
                view.set_arc_progress(Some(ArcProgress {
                    start,
                    end: Some(end),
                }));
            }
        }
    }
}

/// Escape drops the arc being drawn, or else leaves the mode.
pub(super) fn escape(view: &mut MapViewer) {
    if view.take_arc_progress().is_none() {
        view.enter_mode(EditMode::Select);
    }
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Crosshair
}

/// The placed points, and the path to draw: the start..cursor rubber band
/// before the end is placed, then the arc through the cursor (a straight
/// chord while it is degenerate).
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    let Some(progress) = view.arc_progress().copied() else {
        return;
    };
    let half = gpui::px(4.0);
    fill_square(window, frame.geo(progress.start.geo), half, blue());
    // The cursor, snapped onto a way as a click would be.
    let cursor_geo = view.last_mouse_pos.map(|p| match view.active_layer {
        Some(layer_id) => view.arc_point_at(layer_id, p, false).geo,
        None => frame.viewport.screen_to_geo(p),
    });
    let path_points: Vec<(f64, f64)> = match (progress.end, cursor_geo) {
        (None, Some(cursor)) => vec![progress.start.geo, cursor],
        (None, None) => Vec::new(),
        (Some(end), cursor) => {
            fill_square(window, frame.geo(end.geo), half, blue());
            cursor
                .and_then(|c| view.arc_from_geo(progress.start.geo, end.geo, c))
                .unwrap_or_else(|| vec![progress.start.geo, end.geo])
        }
    };
    stroke_polyline(
        window,
        path_points.iter().map(|g| frame.geo(*g)),
        line_width(),
        blue(),
    );
}
