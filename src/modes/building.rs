//! Building mode: three clicks make a rectangle, or two corners when the mode
//! is aligned to a squared way.

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_mode::EditMode;
use osm_gpui::interaction::{self, Interaction};
use osm_gpui::layers::LayerId;

use super::preview::{blue, fill_square, line_width, stroke_polyline, PreviewFrame};
use crate::MapViewer;

/// The rectangle being drawn: corner A is fixed after click 1; corner B after
/// click 2. Both are geo (lat, lon) coordinates.
#[derive(Clone, Copy)]
pub(crate) struct BuildingProgress {
    pub(crate) corner_a: (f64, f64),
    pub(crate) corner_b: Option<(f64, f64)>,
}

/// The squared way whose orientation Building mode borrows: captured when
/// the mode is entered with exactly that way selected, and kept (even though
/// the selection moves on to each new building) until the mode is exited.
#[derive(Clone, Copy)]
pub(crate) struct BuildingReference {
    pub(crate) layer_id: LayerId,
    pub(crate) way_id: i64,
    /// Bearing of the way's longest edge in a local metric projection, in
    /// radians in `[0, pi)` (see `osm_gpui::building`).
    pub(crate) orientation: f64,
}

/// A left press in oriented Building mode, awaiting its release.
#[derive(Clone, Copy)]
pub(crate) struct BuildingPress {
    pub(crate) down: Point<Pixels>,
    /// True if this press placed the first corner (so releasing in place is
    /// just a click and the building waits for a second one).
    pub(crate) placed_first_corner: bool,
}

#[derive(Default)]
pub(crate) struct BuildingState {
    pub(crate) progress: Option<BuildingProgress>,
    /// The way the buildings are aligned to, if any. When set, a building
    /// takes two corners instead of three clicks. Not progress: it outlives
    /// each building and goes only with the session.
    pub(crate) reference: Option<BuildingReference>,
    pub(crate) press: Option<BuildingPress>,
}

impl BuildingState {
    pub(super) fn is_drawing(&self) -> bool {
        self.progress.is_some()
    }

    /// Drop the unfinished rectangle; the reference way stays.
    pub(super) fn reset_progress(&mut self) {
        self.progress = None;
        self.press = None;
    }
}

/// Oriented Building mode owns the left button entirely (no box-select, and
/// no move-drag of the selected reference way): the press places the first
/// corner, the release decides click vs drag. Returns whether it took the
/// press.
pub(super) fn press(view: &mut MapViewer, position: Point<Pixels>) -> bool {
    if view.building_reference().is_none() {
        return false;
    }
    let placed_first_corner = view.building_progress().is_none();
    if placed_first_corner {
        let corner = view.viewport.screen_to_geo(position).to_pair();
        view.set_building_progress(Some(BuildingProgress {
            corner_a: corner,
            corner_b: None,
        }));
    }
    view.set_building_press(Some(BuildingPress {
        down: position,
        placed_first_corner,
    }));
    view.interaction = Interaction::Idle;
    true
}

/// Release of an oriented-Building press. A release within a few pixels of
/// the press is a click: the first one leaves the corner placed and waits,
/// the second finishes the building at that point. A release farther away is
/// a drag and finishes the building at the release point. Returns whether
/// there was a press to finish.
pub(super) fn release(view: &mut MapViewer, up_pos: Point<Pixels>) -> bool {
    let Some(press) = view.take_building_press() else {
        return false;
    };
    let (Some(reference), Some(layer_id), Some(progress)) = (
        view.building_reference().copied(),
        view.active_layer,
        view.building_progress().copied(),
    ) else {
        return true;
    };
    let moved = (up_pos - press.down).magnitude() >= interaction::DRAG_THRESHOLD as f64;
    if press.placed_first_corner && !moved {
        return true;
    }
    let opposite = view.viewport.screen_to_geo(up_pos).to_pair();
    if let Some(corners) = osm_gpui::building::rectangle_from_opposite_corners(
        progress.corner_a,
        opposite,
        reference.orientation,
    ) {
        view.commit_building_corners(layer_id, corners);
        view.set_building_progress(None);
    }
    true
}

/// Click 1 sets corner A, click 2 sets corner B (fixing the first edge),
/// click 3 commits the rectangle. See
/// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Building mode".
pub(crate) fn click(view: &mut MapViewer, screen_pt: Point<Pixels>) {
    let Some(layer_id) = view.active_layer else {
        return;
    };
    let LatLon { lat, lon } = view.viewport.screen_to_geo(screen_pt);

    match view.take_building_progress() {
        None => {
            view.set_building_progress(Some(BuildingProgress {
                corner_a: (lat, lon),
                corner_b: None,
            }));
        }
        Some(BuildingProgress {
            corner_a,
            corner_b: None,
        }) => {
            view.set_building_progress(Some(BuildingProgress {
                corner_a,
                corner_b: Some((lat, lon)),
            }));
        }
        Some(BuildingProgress {
            corner_a,
            corner_b: Some(corner_b),
        }) => {
            view.commit_building(layer_id, corner_a, corner_b, (lat, lon));
            view.set_building_progress(None);
        }
    }
}

/// Escape drops the rectangle being drawn, or else leaves the mode (and with
/// it the reference way).
pub(super) fn escape(view: &mut MapViewer) {
    view.set_building_press(None);
    if view.take_building_progress().is_none() {
        view.enter_mode(EditMode::Select);
    }
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Crosshair
}

/// The reference way highlighted, then whatever of the rectangle is known.
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    if let Some(reference) = view.building_reference().copied() {
        view.layer_manager.render_highlight(
            &osm_gpui::selection::FeatureRef {
                layer_id: reference.layer_id,
                kind: osm_gpui::selection::FeatureKind::Way,
                id: reference.way_id,
            },
            osm_gpui::layers::HighlightKind::Hovered,
            frame.viewport,
            frame.bounds,
            window,
        );
    }
    let Some(progress) = view.building_progress().copied() else {
        return;
    };
    let a = frame.geo(progress.corner_a);
    match progress.corner_b {
        // Oriented: the cursor is the opposite corner, so preview the whole
        // rectangle.
        None if view.building_reference().is_some() => {
            fill_square(window, a, gpui::px(4.0), blue());
            let corners = view
                .building_reference()
                .copied()
                .zip(view.last_mouse_pos)
                .and_then(|(reference, mouse)| {
                    osm_gpui::building::rectangle_from_opposite_corners(
                        progress.corner_a,
                        frame.viewport.screen_to_geo(mouse).to_pair(),
                        reference.orientation,
                    )
                });
            if let Some(corners) = corners {
                stroke_polyline(
                    window,
                    corners
                        .iter()
                        .chain(Some(&corners[0]))
                        .map(|c| frame.geo(*c)),
                    line_width(),
                    blue(),
                );
            }
        }
        // Only corner A placed: a marker at the fixed corner plus a
        // rubber-band line to the cursor (no edge yet to offset a rectangle
        // from).
        None => {
            fill_square(window, a, gpui::px(4.0), blue());
            if let Some(mouse_pos) = view.last_mouse_pos {
                stroke_polyline(window, [a, frame.local(mouse_pos)], line_width(), blue());
            }
        }
        Some(corner_b) => {
            let cursor_geo = view
                .last_mouse_pos
                .map(|p| frame.viewport.screen_to_geo(p).to_pair())
                .unwrap_or(corner_b);
            let (far_a, far_b) =
                view.rectangle_from_geo_edge(progress.corner_a, corner_b, cursor_geo);
            stroke_polyline(
                window,
                [
                    a,
                    frame.geo(corner_b),
                    frame.geo(far_b),
                    frame.geo(far_a),
                    a,
                ],
                line_width(),
                blue(),
            );
        }
    }
}
