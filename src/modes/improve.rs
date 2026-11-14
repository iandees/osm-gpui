//! Improve Way mode: with a way chosen, a click moves its nearest node to the
//! cursor, inserts a node along it, or deletes the nearest node, by the
//! modifiers held.

use gpui::{px, Context, CursorStyle, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_mode::EditMode;
use osm_gpui::layers::LayerId;

use super::preview::{blue, fill_square, line_width, red, stroke_polyline, PreviewFrame};
use crate::undo::{EditLabel, UndoableAction};
use crate::MapViewer;
use osm_gpui::edit_ops::Recording;

/// What a click would do to Improve Way mode's target way: the way, its node
/// ids and their screen positions, and the planned edit (in screen space).
pub(crate) struct ImprovePlan {
    pub(crate) layer_id: LayerId,
    pub(crate) way_id: i64,
    pub(crate) ids: Vec<i64>,
    pub(crate) points: Vec<osm_gpui::improve_way::Pt>,
    pub(crate) plan: osm_gpui::improve_way::Plan,
}

/// The target is the selection and the operation the held modifiers, so there
/// is nothing to remember between events.
#[derive(Default)]
pub(crate) struct ImproveState;

impl MapViewer {
    /// Improve Way mode's target: the one way that is selected, if any.
    pub(crate) fn improve_target(&self) -> Option<(LayerId, i64)> {
        match self.selected.as_slice() {
            [f] if f.kind == osm_gpui::selection::FeatureKind::Way => Some((f.layer_id, f.id)),
            _ => None,
        }
    }

    /// The operation the held modifiers select in Improve Way mode: Shift
    /// deletes, Ctrl/Cmd/Alt insert, none moves.
    fn improve_op(&self) -> osm_gpui::improve_way::Op {
        let m = self.pointer.last_modifiers;
        osm_gpui::improve_way::Op::from_modifiers(m.shift, m.control || m.platform || m.alt)
    }

    /// What a click at `cursor` would do to the target way: its node ids,
    /// their screen positions and the planned edit (in screen space).
    pub(crate) fn improve_plan_at(&self, cursor: Point<Pixels>) -> Option<ImprovePlan> {
        let (layer_id, way_id) = self.improve_target()?;
        let editable = self.layer_manager.find_layer(layer_id)?.as_editable()?;
        let ids = editable.way_node_ids(way_id)?;
        let points: Vec<osm_gpui::improve_way::Pt> = ids
            .iter()
            .map(|&id| {
                let (lat, lon) = editable.node_lat_lon(id)?;
                let p = self.viewport.geo_to_screen(LatLon::new(lat, lon));
                Some((p.x.to_f64(), p.y.to_f64()))
            })
            .collect::<Option<_>>()?;
        let plan = osm_gpui::improve_way::plan(
            &points,
            &ids,
            (cursor.x.to_f64(), cursor.y.to_f64()),
            self.improve_op(),
        )?;
        Some(ImprovePlan {
            layer_id,
            way_id,
            ids,
            points,
            plan,
        })
    }
}

/// With no way chosen, a click picks the way under the cursor; with one
/// chosen, a click edits it (move the nearest node to the cursor, insert a
/// node along the way, or delete the nearest node).
pub(crate) fn click(view: &mut MapViewer, screen_pt: Point<Pixels>, cx: &mut Context<MapViewer>) {
    use osm_gpui::improve_way::Plan;
    use osm_gpui::selection::{FeatureKind, FeatureRef};

    if view.improve_target().is_none() {
        super::select::click(view, screen_pt, false, 1);
        if view.improve_target().is_none() {
            view.set_selection(Vec::new());
        }
        return;
    }
    if view.refuse_edit_during_upload() {
        return;
    }
    let Some(ImprovePlan {
        layer_id,
        way_id,
        plan,
        ..
    }) = view.improve_plan_at(screen_pt)
    else {
        return;
    };
    let viewport = view.viewport.clone();
    let to_geo = move |p: osm_gpui::improve_way::Pt| {
        viewport.screen_to_geo(gpui::point(px(p.0 as f32), px(p.1 as f32)))
    };
    let way_ref = FeatureRef {
        layer_id,
        kind: FeatureKind::Way,
        id: way_id,
    };
    match plan {
        Plan::Move { node, to } => {
            let LatLon { lat, lon } = to_geo(to);
            let Some(editable) = view
                .layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
            else {
                return;
            };
            let Some(before) = editable.node_lat_lon(node) else {
                return;
            };
            if editable.commit_node_moves(&[(node, lat, lon)]).is_err() {
                return;
            }
            view.undo_stack.push(UndoableAction::move_nodes(
                vec![(layer_id, vec![(node, before, (lat, lon))])],
                Some(vec![way_ref]),
            ));
        }
        Plan::Insert { index, at } => {
            if view.refuse_way_growth(layer_id, way_id, 1) {
                return;
            }
            let LatLon { lat, lon } = to_geo(at);
            let Some(editable) = view
                .layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
            else {
                return;
            };
            let mut rec = Recording::new(layer_id, editable);
            let Ok(node_id) = rec.insert_node_into_way(way_id, index, lat, lon) else {
                return;
            };
            let node_ref = FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: node_id,
            };
            view.undo_stack.push(
                UndoableAction::edit(layer_id, EditLabel::InsertedNode, rec.into_ops())
                    .selecting(vec![way_ref, node_ref]),
            );
        }
        Plan::Delete { node } => {
            // The ordinary Delete action does the bookkeeping (other ways,
            // relations, orphans) as one undo step.
            view.set_selection(vec![FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: node,
            }]);
            view.run_selection_action("delete", cx);
            // Keep working on the way if it survived.
            let still_there = view
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
                .is_some_and(|e| e.has_feature(FeatureKind::Way, way_id));
            view.set_selection(if still_there {
                vec![way_ref]
            } else {
                Vec::new()
            });
        }
    }
    cx.notify();
}

/// The first Escape lets go of the way (to pick another), the second leaves
/// the mode.
pub(super) fn escape(view: &mut MapViewer) {
    if view.improve_target().is_some() {
        view.set_selection(Vec::new());
    } else {
        view.enter_mode(EditMode::Select);
    }
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Crosshair
}

/// The target way as it would look after a click at the cursor.
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    use osm_gpui::improve_way::{marker, preview, Plan};

    let Some(ip) = view
        .pointer
        .last_mouse_pos
        .and_then(|c| view.improve_plan_at(c))
    else {
        return;
    };
    let color = if matches!(ip.plan, Plan::Delete { .. }) {
        red()
    } else {
        blue()
    };
    let outline = preview(&ip.points, &ip.ids, ip.plan);
    stroke_polyline(
        window,
        outline.iter().map(|p| frame.pixels(*p)),
        line_width(),
        color,
    );
    let c = frame.pixels(marker(&ip.points, &ip.ids, ip.plan));
    fill_square(window, c, px(5.0), color);
}
