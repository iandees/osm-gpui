//! Extrude mode: drag a way's segment to reshape the way, move the segment,
//! or pull a new way out of it; double-click a segment to add a node.

use gpui::{px, CursorStyle, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_mode::EditMode;
use osm_gpui::interaction;
use osm_gpui::layers::LayerId;

use super::preview::{blue, line_width, stroke_polyline, PreviewFrame};
use crate::undo::{roll_back_recording, EditLabel, UndoableAction};
use crate::MapViewer;
use osm_gpui::edit_ops::Recording;

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
    let Some(mouse_pos) = view.pointer.last_mouse_pos else {
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
    let cursor_geo = frame.viewport.screen_to_geo(mouse_pos).to_pair();
    let (far_a, far_b) = view.rectangle_from_geo_edge(a_geo, b_geo, cursor_geo);
    let a = frame.geo(a_geo);
    stroke_polyline(
        window,
        [a, frame.geo(b_geo), frame.geo(far_b), frame.geo(far_a), a],
        line_width(),
        blue(),
    );
}

impl MapViewer {
    /// Commit a NewWay extrude drag: compute the far 2 corners via
    /// `rectangle_from_geo_edge` (using `up_pos` for the perpendicular
    /// offset), create 2 new nodes and a closed way as one undo action. The
    /// way has no tags: extrude works on any way segment, not just buildings.
    pub(crate) fn commit_extrude(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(layer) = self.layer_manager.find_layer(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable() else {
            return;
        };
        let Some(a_geo) = editable.node_lat_lon(drag.node_a) else {
            return;
        };
        let Some(b_geo) = editable.node_lat_lon(drag.node_b) else {
            return;
        };
        let cursor_geo = self.viewport.screen_to_geo(up_pos).to_pair();

        let (far_a, far_b) = self.rectangle_from_geo_edge(a_geo, b_geo, cursor_geo);
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        let mut rec = Recording::new(drag.layer, editable);
        let new_a = rec.add_node(far_a.0, far_a.1);
        let new_b = rec.add_node(far_b.0, far_b.1);
        let way_nodes = vec![drag.node_a, drag.node_b, new_b, new_a, drag.node_a];
        let way_id = rec.add_way(way_nodes, Vec::new());

        let way = osm_gpui::selection::FeatureRef {
            layer_id: drag.layer,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        };
        self.undo_stack.push(
            UndoableAction::edit(drag.layer, EditLabel::ExtrudedWay, rec.into_ops())
                .selecting(vec![way]),
        );
        self.selected = vec![way];
        self.fields_panel.fields_text_inputs.clear();
        self.fields_panel.fields_text_subscribed.clear();
        self.fields_panel.fields_open_combo = None;
        self.fields_panel.fields_promoted_more_fields.clear();
    }

    /// Resolve a Reshape/Move extrude drag against the cursor at
    /// `cursor_screen` (map-local pixels): the plan to apply plus the way's
    /// resulting outline in screen pixels. Works in screen space, which is
    /// isotropic, so "perpendicular" means perpendicular on screen. `None`
    /// for anything that would not change the way.
    pub(crate) fn compute_extrude(
        &self,
        drag: &ExtrudeDrag,
        cursor_screen: gpui::Point<gpui::Pixels>,
    ) -> Option<ExtrudeResult> {
        use osm_gpui::extrude::{ExtrudeKind, PlanNode, SegmentNode};
        use osm_gpui::selection::{FeatureKind, FeatureRef};

        let editable = self.layer_manager.find_layer(drag.layer)?.as_editable()?;
        let ids = editable.way_node_ids(drag.way_id)?;
        let screen_of = |id: i64| -> Option<(f64, f64)> {
            let (lat, lon) = editable.node_lat_lon(id)?;
            let p = self.viewport.geo_to_screen(LatLon::new(lat, lon));
            Some((p.x.to_f64(), p.y.to_f64()))
        };
        let mut nodes = Vec::with_capacity(ids.len());
        for &id in &ids {
            // A node may be moved (rather than duplicated) only when nothing
            // else depends on it: no other way, no relation, no tags.
            let movable = editable
                .ways_using_node(id)
                .iter()
                .all(|&w| w == drag.way_id)
                && editable.relations_containing_node(id).is_empty()
                && editable
                    .feature_tags(&FeatureRef {
                        layer_id: drag.layer,
                        kind: FeatureKind::Node,
                        id,
                    })
                    .is_none_or(|t| t.is_empty());
            nodes.push(SegmentNode {
                id,
                pos: screen_of(id)?,
                movable,
            });
        }
        let down = (drag.down.x.to_f64(), drag.down.y.to_f64());
        let cursor = (cursor_screen.x.to_f64(), cursor_screen.y.to_f64());
        let kind = match drag.style {
            ExtrudeStyle::Move => ExtrudeKind::Move,
            _ => ExtrudeKind::Reshape,
        };
        let plan = osm_gpui::extrude::plan_extrude(
            &nodes,
            drag.segment,
            (cursor.0 - down.0, cursor.1 - down.1),
            kind,
        )?;
        let outline = plan
            .nodes
            .iter()
            .map(|n| match *n {
                PlanNode::New(i) => plan.new_nodes[i],
                PlanNode::Existing(id) => plan
                    .moved
                    .iter()
                    .find(|m| m.0 == id)
                    .map(|m| m.1)
                    .or_else(|| nodes.iter().find(|n| n.id == id).map(|n| n.pos))
                    .unwrap_or_default(),
            })
            .collect();
        Some(ExtrudeResult { plan, outline })
    }

    /// Commit a Reshape/Move extrude drag as one undo step: new nodes, the
    /// way's rewritten node list (`SetWayNodes`) and the moved nodes
    /// (`MoveNodes`), recorded in that order so undo runs them backwards.
    pub(crate) fn commit_extrude_reshape(
        &mut self,
        drag: &ExtrudeDrag,
        up_pos: gpui::Point<gpui::Pixels>,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        use osm_gpui::extrude::PlanNode;

        let Some(result) = self.compute_extrude(drag, up_pos) else {
            return;
        };
        let plan = result.plan;
        if !plan.new_nodes.is_empty()
            && self.refuse_way_growth(drag.layer, drag.way_id, plan.new_nodes.len())
        {
            return;
        }
        let to_geo = |p: (f64, f64)| {
            self.viewport
                .screen_to_geo(gpui::point(px(p.0 as f32), px(p.1 as f32)))
                .to_pair()
        };
        let new_geo: Vec<(f64, f64)> = plan.new_nodes.iter().map(|&p| to_geo(p)).collect();
        let moved_geo: Vec<(i64, (f64, f64))> =
            plan.moved.iter().map(|&(id, p)| (id, to_geo(p))).collect();

        let Some(editable) = self
            .layer_manager
            .find_layer_mut(drag.layer)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let Some(before) = editable.way_node_ids(drag.way_id) else {
            return;
        };
        let mut rec = Recording::new(drag.layer, editable);
        let mut new_ids = Vec::with_capacity(new_geo.len());
        for &(lat, lon) in &new_geo {
            let Ok(id) = rec.create_node(lat, lon, None) else {
                return;
            };
            new_ids.push(id);
        }
        let after: Vec<i64> = plan
            .nodes
            .iter()
            .map(|n| match *n {
                PlanNode::Existing(id) => id,
                PlanNode::New(i) => new_ids[i],
            })
            .collect();
        if after != before && rec.set_way_nodes(drag.way_id, after).is_err() {
            roll_back_recording(rec);
            return;
        }
        if !moved_geo.is_empty() {
            let moves: Vec<(i64, f64, f64)> = moved_geo
                .iter()
                .filter(|&&(id, _)| rec.layer().node_lat_lon(id).is_some())
                .map(|&(id, (lat, lon))| (id, lat, lon))
                .collect();
            if rec.commit_node_moves(&moves).is_err() {
                roll_back_recording(rec);
                return;
            }
        }
        if rec.is_empty() {
            return;
        }
        self.undo_stack.push(UndoableAction::edit(
            drag.layer,
            EditLabel::Text(osm_gpui::tr!("history.extruded_segment").to_string()),
            rec.into_ops(),
        ));
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id: drag.layer,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: drag.way_id,
        }];
        self.fields_panel.fields_text_inputs.clear();
        self.fields_panel.fields_text_subscribed.clear();
        self.fields_panel.fields_open_combo = None;
        self.fields_panel.fields_promoted_more_fields.clear();
    }

    /// Double-click on a segment (no drag): insert a new node on that
    /// segment, at the point nearest the double-click, splitting it.
    pub(crate) fn insert_node_on_segment(
        &mut self,
        drag: &ExtrudeDrag,
        up_pos: gpui::Point<gpui::Pixels>,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        if self.refuse_way_growth(drag.layer, drag.way_id, 1) {
            return;
        }
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        // Place the node on the segment itself (the point nearest the click),
        // not under the cursor, so the way's shape is unchanged.
        let (Some(a_geo), Some(b_geo)) = (
            editable.node_lat_lon(drag.node_a),
            editable.node_lat_lon(drag.node_b),
        ) else {
            return;
        };
        let LatLon { lat, lon } = {
            let a = self.viewport.geo_to_screen(LatLon::new(a_geo.0, a_geo.1));
            let b = self.viewport.geo_to_screen(LatLon::new(b_geo.0, b_geo.1));
            let (dx, dy) = ((b.x - a.x).to_f64(), (b.y - a.y).to_f64());
            let len_sq = dx * dx + dy * dy;
            let t = if len_sq <= f64::EPSILON {
                0.0
            } else {
                (((up_pos.x - a.x).to_f64() * dx + (up_pos.y - a.y).to_f64() * dy) / len_sq)
                    .clamp(0.0, 1.0)
            };
            self.viewport.screen_to_geo(gpui::point(
                px((a.x.to_f64() + t * dx) as f32),
                px((a.y.to_f64() + t * dy) as f32),
            ))
        };
        let Some(node_ids) = editable.way_node_ids(drag.way_id) else {
            return;
        };
        let Some(idx_a) = node_ids.iter().position(|&id| id == drag.node_a) else {
            return;
        };
        let insert_index = idx_a + 1;

        let mut rec = Recording::new(drag.layer, editable);
        let Ok(new_id) = rec.insert_node_into_way(drag.way_id, insert_index, lat, lon) else {
            return;
        };
        let feature = |kind, id| osm_gpui::selection::FeatureRef {
            layer_id: drag.layer,
            kind,
            id,
        };
        self.undo_stack.push(
            UndoableAction::edit(drag.layer, EditLabel::InsertedNode, rec.into_ops()).selecting(
                vec![
                    feature(osm_gpui::selection::FeatureKind::Way, drag.way_id),
                    feature(osm_gpui::selection::FeatureKind::Node, new_id),
                ],
            ),
        );
    }
}
