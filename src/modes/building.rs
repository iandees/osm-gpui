//! Building mode: three clicks make a rectangle, or two corners when the mode
//! is aligned to a squared way.

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::interaction::{self, Interaction};
use osm_gpui::layers::LayerId;

use super::preview::{blue, fill_square, line_width, stroke_polyline, PreviewFrame};
use crate::undo::{EditLabel, UndoableAction};
use crate::MapViewer;
use osm_gpui::edit_ops::Recording;

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
        let corner = view.viewport.screen_to_geo(position);
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
    let opposite = view.viewport.screen_to_geo(up_pos);
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
    let (lat, lon) = view.viewport.screen_to_geo(screen_pt);

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

/// Entering Building mode with one squared way selected borrows its
/// orientation; staying in Building mode keeps the existing one.
pub(super) fn switch_to(view: &mut MapViewer) {
    let reference = if view.mode() == EditMode::Building {
        view.building_reference().copied()
    } else {
        view.squared_selection_reference()
    };
    view.enter_mode(EditMode::Building);
    view.set_building_reference(reference);
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
                        frame.viewport.screen_to_geo(mouse),
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
                .map(|p| frame.viewport.screen_to_geo(p))
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

impl MapViewer {
    /// Compute the final rectangle (corner_a, corner_b as one edge, offset
    /// by `cursor`'s perpendicular distance) and commit 4 new nodes + a
    /// closed `building=yes` way as one undo action.
    pub(crate) fn commit_building(
        &mut self,
        layer_id: LayerId,
        corner_a: (f64, f64),
        corner_b: (f64, f64),
        cursor: (f64, f64),
    ) {
        let (far_a, far_b) = self.rectangle_from_geo_edge(corner_a, corner_b, cursor);
        self.commit_building_corners(layer_id, [corner_a, corner_b, far_b, far_a]);
    }

    /// Create 4 nodes and a closed `building=yes` way through them (corners
    /// in ring order) as one undo action, and select the new way.
    pub(crate) fn commit_building_corners(&mut self, layer_id: LayerId, corners: [(f64, f64); 4]) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|layer| layer.as_editable_mut())
        else {
            return;
        };
        let mut rec = Recording::new(layer_id, editable);

        let [n0, n1, n2, n3] = corners.map(|(lat, lon)| rec.add_node(lat, lon));
        let tags = vec![("building".to_string(), "yes".to_string())];
        let way_id = rec.add_way(vec![n0, n1, n2, n3, n0], tags);

        let way = osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        };
        self.undo_stack.push(
            UndoableAction::edit(layer_id, EditLabel::CreatedBuilding, rec.into_ops())
                .selecting(vec![way]),
        );
        self.selected = vec![way];
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// If the selection is exactly one closed, squared way, the reference a
    /// Building-mode session should align to.
    pub(crate) fn squared_selection_reference(&self) -> Option<BuildingReference> {
        use osm_gpui::selection::FeatureKind;
        let [feature] = self.selected.as_slice() else {
            return None;
        };
        if feature.kind != FeatureKind::Way {
            return None;
        }
        let editable = self
            .layer_manager
            .find_layer(feature.layer_id)?
            .as_editable()?;
        let node_ids = editable.way_node_ids(feature.id)?;
        if node_ids.len() < 4 || node_ids.first() != node_ids.last() {
            return None;
        }
        let ring: Vec<(f64, f64)> = node_ids
            .iter()
            .map(|&id| editable.node_lat_lon(id))
            .collect::<Option<_>>()?;
        if !osm_gpui::building::is_squared(&ring, osm_gpui::building::SQUARED_TOLERANCE_DEG) {
            return None;
        }
        Some(BuildingReference {
            layer_id: feature.layer_id,
            way_id: feature.id,
            orientation: osm_gpui::building::dominant_orientation(&ring)?,
        })
    }
}
