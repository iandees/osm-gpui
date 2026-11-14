//! Arc mode: three clicks (start, end, a point on the arc) make one open way
//! whose nodes lie on a circle.

use gpui::{px, CursorStyle, Pixels, Point, Window};
use osm_gpui::edit_mode::EditMode;

use super::preview::{blue, fill_square, line_width, stroke_polyline, PreviewFrame};
use crate::undo::{roll_back_recording, EditLabel, UndoableAction};
use crate::{snap_node_ok, snap_way_ok, MapViewer, Tags};
use osm_gpui::edit_ops::Recording;
use osm_gpui::layers::LayerId;

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

impl MapViewer {
    /// The geo position for an Arc-mode click at `screen_pt`: snapped onto the
    /// nearest way in `layer_id` when one is within tolerance.
    pub(crate) fn arc_point_at(
        &self,
        layer_id: LayerId,
        screen_pt: gpui::Point<gpui::Pixels>,
        no_snap: bool,
    ) -> ArcPoint {
        let plain = ArcPoint {
            geo: self.viewport.screen_to_geo(screen_pt),
            snap: None,
        };
        if no_snap {
            return plain;
        }
        let sources: Vec<Tags> = Vec::new();
        // An existing node close by wins over a way.
        let node_hit = self.layer_manager.find_layer(layer_id).and_then(|layer| {
            let osm_layer = layer.as_osm()?;
            let editable = layer.as_editable()?;
            let id = osm_layer.snap_to_node_where(
                &self.viewport,
                screen_pt,
                osm_gpui::layers::node_markers::FULL_HIT_RADIUS,
                |id| snap_node_ok(editable, layer_id, &sources, None, id),
            )?;
            Some((id, editable.node_lat_lon(id)?))
        });
        if let Some((id, geo)) = node_hit {
            return ArcPoint {
                geo,
                snap: Some(ArcSnap::Node(id)),
            };
        }
        let hit = self.layer_manager.find_layer(layer_id).and_then(|layer| {
            let osm_layer = layer.as_osm()?;
            let editable = layer.as_editable()?;
            osm_layer.snap_to_way_where(
                &self.viewport,
                screen_pt,
                osm_gpui::snap::WAY_SNAP_TOLERANCE_PX,
                |w| snap_way_ok(editable, layer_id, &sources, None, w),
            )
        });
        match hit {
            Some((way_id, _, _, idx, lat, lon)) => ArcPoint {
                geo: (lat, lon),
                snap: Some(ArcSnap::Way(way_id, idx)),
            },
            None => plain,
        }
    }

    /// The arc from `start` to `end` bending through `through` (all geo
    /// coordinates), as geo points including both ends. The circle is worked
    /// out in screen-pixel space, which is isotropic, so it is a true circle
    /// on screen (see `rectangle_from_geo_edge`). `None` if the three points
    /// are in a line.
    pub(crate) fn arc_from_geo(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        through: (f64, f64),
    ) -> Option<Vec<(f64, f64)>> {
        let to_screen = |g: (f64, f64)| {
            let p = self.viewport.geo_to_screen(g.0, g.1);
            (p.x.to_f64(), p.y.to_f64())
        };
        let points = osm_gpui::arc::arc_through(
            to_screen(start),
            to_screen(through),
            to_screen(end),
            osm_gpui::arc::DEFAULT_STEP_DEGREES,
        )?;
        let last = points.len() - 1;
        Some(
            points
                .into_iter()
                .enumerate()
                .map(|(i, (x, y))| match i {
                    // Keep the endpoints exact rather than round-tripping them.
                    0 => start,
                    i if i == last => end,
                    _ => self
                        .viewport
                        .screen_to_geo(gpui::point(px(x as f32), px(y as f32))),
                })
                .collect(),
        )
    }

    /// Create the arc's nodes and one open, untagged way through them as one
    /// undo action, and select the new way.
    pub(crate) fn commit_arc(
        &mut self,
        layer_id: LayerId,
        start: ArcPoint,
        end: ArcPoint,
        through: (f64, f64),
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(points) = self.arc_from_geo(start.geo, end.geo, through) else {
            return;
        };
        // Endpoints snapped onto a node use that node; ones snapped onto a way
        // get a new node spliced into it (so the arc joins that way), which
        // grows the way.
        let way_snap = |p: &ArcPoint| match p.snap {
            Some(ArcSnap::Way(way_id, idx)) => Some((way_id, idx)),
            _ => None,
        };
        let mut splices: Vec<(usize, ArcPoint)> = [(0, start), (points.len() - 1, end)]
            .into_iter()
            .filter(|(_, p)| way_snap(p).is_some())
            .collect();
        for (_, p) in &splices {
            let (way_id, _) = way_snap(p).unwrap();
            let by = splices
                .iter()
                .filter(|(_, q)| way_snap(q).map(|s| s.0) == Some(way_id))
                .count();
            if self.refuse_way_growth(layer_id, way_id, by) {
                return;
            }
        }
        // Splice in descending segment order within a way so earlier indices
        // stay valid; on a shared segment, the point nearer its far end first.
        let seg_start = |p: &ArcPoint| {
            let (way_id, idx) = way_snap(p).unwrap();
            let editable = self
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())?;
            let id = *editable.way_node_ids(way_id)?.get(idx)?;
            editable.node_lat_lon(id)
        };
        let along = |p: &ArcPoint| {
            seg_start(p).map_or(0.0, |s| (p.geo.0 - s.0).powi(2) + (p.geo.1 - s.1).powi(2))
        };
        splices.sort_by(|(_, a), (_, b)| {
            let (aw, ai) = way_snap(a).unwrap();
            let (bw, bi) = way_snap(b).unwrap();
            aw.cmp(&bw)
                .then(bi.cmp(&ai))
                .then_with(|| along(b).total_cmp(&along(a)))
        });
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|layer| layer.as_editable_mut())
        else {
            return;
        };
        let mut rec = Recording::new(layer_id, editable);

        // Nodes the arc doesn't own: reused ones, and ones spliced into other
        // ways (undone by their own ops).
        let mut node_ids: Vec<Option<i64>> = vec![None; points.len()];
        let feature = |kind, id| osm_gpui::selection::FeatureRef { layer_id, kind, id };
        let mut selects = Vec::new();
        for (i, p) in [(0, start), (points.len() - 1, end)] {
            if let Some(ArcSnap::Node(id)) = p.snap {
                node_ids[i] = Some(id);
            }
        }
        for (i, p) in &splices {
            let (way_id, idx) = way_snap(p).unwrap();
            let Ok(id) = rec.insert_node_into_way(way_id, idx + 1, p.geo.0, p.geo.1) else {
                // Put back the splices already made, so nothing is half done.
                roll_back_recording(rec);
                return;
            };
            selects.push(feature(osm_gpui::selection::FeatureKind::Way, way_id));
            selects.push(feature(osm_gpui::selection::FeatureKind::Node, id));
            node_ids[*i] = Some(id);
        }
        let all_ids: Vec<i64> = node_ids
            .iter()
            .zip(&points)
            .map(|(id, &(lat, lon))| id.unwrap_or_else(|| rec.add_node(lat, lon)))
            .collect();
        let way_id = rec.add_way(all_ids, Vec::new());
        selects.push(feature(osm_gpui::selection::FeatureKind::Way, way_id));
        // The whole arc, splices included, is one undo step: undoing removes
        // the arc first, then the splices.
        self.undo_stack.push(
            UndoableAction::edit(layer_id, EditLabel::CreatedArc, rec.into_ops())
                .selecting(selects),
        );
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        }];
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }
}
