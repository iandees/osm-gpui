//! Add mode: click to place nodes and grow a way, snapping onto nearby nodes
//! and ways.

use gpui::{CursorStyle, Window};

use super::preview::{blue, line_width, stroke_polyline, PreviewFrame};
use crate::undo::{roll_back_recording, EditLabel, UndoableAction};
use crate::{snap_node_ok, snap_way_ok, MapViewer, Tags};
use osm_gpui::edit_ops::{EditOp, Recording};
use osm_gpui::layers::LayerId;

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

/// What a hovering pointer would snap onto: a node or way near it, unless a
/// snapping-off modifier is held.
pub(super) fn hover_snap_target(
    view: &MapViewer,
    pos: gpui::Point<gpui::Pixels>,
    modifiers: gpui::Modifiers,
) -> Option<crate::SnapTarget> {
    if osm_gpui::snap::snapping_disabled(modifiers.alt, modifiers.control) {
        return None;
    }
    view.active_layer.and_then(|layer_id| {
        let sources = view.add_snap_sources(layer_id);
        view.find_snap_target(layer_id, pos, None, &sources, view.add_progress().is_some())
    })
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

impl MapViewer {
    /// Add mode: place a node, or extend/connect the in-progress way. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Add mode"
    /// and docs/superpowers/specs/2026-07-08-add-mode-snap-to-way-design.md
    /// for the snap-to-way behavior. `ctrl_held` (true when a snapping-off
    /// modifier, Alt or Ctrl, is held; see `osm_gpui::snap`) bypasses both
    /// snapping onto a nearby existing node and onto a nearby way's line
    /// geometry, always producing a fully independent node. Only targets
    /// `osm_gpui::snap::may_snap` allows for the way being drawn qualify.
    pub(crate) fn handle_add_click(
        &mut self,
        screen_pt: gpui::Point<gpui::Pixels>,
        ctrl_held: bool,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (mut lat, mut lon) = self.viewport.screen_to_geo(screen_pt);

        // Every click after the first grows the way being drawn by a node.
        let drawing_way = self.add_progress().and_then(|p| p.way_id);
        if let Some(way_id) = drawing_way {
            if self.refuse_way_growth(layer_id, way_id, 1) {
                return;
            }
        }

        // Clicking an existing node/way finishes the in-progress way by
        // connecting to it. Skipped entirely when Ctrl is held.
        if !ctrl_held && self.add_progress().is_some() {
            let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
            let sources = self.add_snap_sources(layer_id);
            let node_allowed = |id: i64| {
                self.layer_manager
                    .find_layer(layer_id)
                    .and_then(|l| l.as_editable())
                    .is_some_and(|e| snap_node_ok(e, layer_id, &sources, None, id))
            };
            let hit_node = osm_gpui::selection::resolve_hits(per_layer)
                .filter(|hit| {
                    hit.layer_id == layer_id && hit.kind == osm_gpui::selection::FeatureKind::Node
                })
                .map(|hit| hit.id)
                .filter(|&id| node_allowed(id))
                // Vertices that are not drawn (untagged ones, unless
                // selected) are not hit targets, but still snap targets.
                .or_else(|| {
                    self.layer_manager
                        .find_layer(layer_id)
                        .and_then(|layer| layer.as_osm())
                        .and_then(|osm_layer| {
                            osm_layer.snap_to_node_where(
                                &self.viewport,
                                screen_pt,
                                osm_gpui::layers::node_markers::FULL_HIT_RADIUS,
                                node_allowed,
                            )
                        })
                });
            if let Some(node_id) = hit_node {
                let way_id = self.add_extend_or_start_way(layer_id, node_id, Vec::new(), None);
                self.set_add_progress(None);
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                }];
                self.fields_text_inputs.clear();
                self.fields_text_subscribed.clear();
                self.fields_open_combo = None;
                self.fields_promoted_more_fields.clear();
                return;
            }
        }

        // Try to snap onto a nearby way's line geometry, unless Ctrl is
        // held. `snap` carries the snapped-onto way's id and splice index,
        // if any, threaded through to `add_extend_or_start_way` for the
        // 2nd+ click case.
        let snap_hit = if ctrl_held {
            None
        } else {
            let sources = self.add_snap_sources(layer_id);
            self.layer_manager.find_layer(layer_id).and_then(|layer| {
                let osm_layer = layer.as_osm()?;
                let editable = layer.as_editable()?;
                osm_layer.snap_to_way_where(
                    &self.viewport,
                    screen_pt,
                    osm_gpui::snap::WAY_SNAP_TOLERANCE_PX,
                    |w| snap_way_ok(editable, layer_id, &sources, None, w),
                )
            })
        };

        // With no OSM node or way to snap to (they win), a reference line or
        // point (GPX, GeoJSON) under the cursor pulls the new, ordinary node
        // onto itself: onto a vertex, else onto the nearest point of a segment.
        if snap_hit.is_none() && !ctrl_held {
            if let Some((snap_lat, snap_lon)) = self.reference_snap_at(screen_pt) {
                lat = snap_lat;
                lon = snap_lon;
            }
        }

        // Snapping splices a node into the way under the cursor.
        if let Some((snap_way, ..)) = snap_hit {
            let by = 1 + usize::from(drawing_way == Some(snap_way));
            if self.refuse_way_growth(layer_id, snap_way, by) {
                return;
            }
        }

        // Note: `find_layer_mut` is re-called in each arm below (rather than
        // binding `layer` once above the match) so its mutable borrow ends
        // before the arm needs `&mut self` again for `self.set_add_progress`/
        // `self.add_extend_or_start_way`/`self.undo_stack` — binding it once
        // outside the match would keep the borrow alive across those calls
        // and fail to compile.
        match self.take_add_progress() {
            None => {
                // First click of a fresh continuation: a lone node (or, if
                // snapped, a node spliced into the snapped-onto way), no way
                // of its own yet — the next click always starts a *new* way
                // from this node, whether or not this one landed on top of
                // an existing way.
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let mut rec = Recording::new(layer_id, editable);
                let new_id = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => {
                        let Ok(new_id) =
                            rec.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon)
                        else {
                            return;
                        };
                        let feature =
                            |kind, id| osm_gpui::selection::FeatureRef { layer_id, kind, id };
                        self.undo_stack.push(
                            UndoableAction::edit(layer_id, EditLabel::InsertedNode, rec.into_ops())
                                .selecting(vec![
                                    feature(osm_gpui::selection::FeatureKind::Way, way_id),
                                    feature(osm_gpui::selection::FeatureKind::Node, new_id),
                                ]),
                        );
                        new_id
                    }
                    None => {
                        let new_id = rec.add_node(lat, lon);
                        self.undo_stack.push(UndoableAction::edit(
                            layer_id,
                            EditLabel::CreatedNode,
                            rec.into_ops(),
                        ));
                        new_id
                    }
                };
                self.set_add_progress(Some(AddProgress {
                    way_id: None,
                    last_node_id: new_id,
                    at_start: false,
                }));
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: new_id,
                }];
            }
            Some(progress) => {
                // 2nd+ click: create the node (or snap it onto a way) and
                // fold it into the way being drawn in one step.
                // `add_extend_or_start_way` pushes the matching undo entry
                // that covers both the node creation and the way
                // mutation(s) (one click = one undo step).
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let mut rec = Recording::new(layer_id, editable);
                let (new_id, snap) = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => {
                        let Ok(id) = rec.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon)
                        else {
                            return;
                        };
                        (id, Some(way_id))
                    }
                    None => (rec.add_node(lat, lon), None),
                };
                let at_start = progress.at_start;
                let node_ops = rec.into_ops();
                self.set_add_progress(Some(progress));
                let way_id = self.add_extend_or_start_way(layer_id, new_id, node_ops, snap);
                self.set_add_progress(Some(AddProgress {
                    way_id: Some(way_id),
                    last_node_id: new_id,
                    at_start,
                }));
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                }];
            }
        }
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Shared by the "continue clicking" and "connect to existing feature"
    /// paths: start a new 2-node way if none exists yet, or extend the
    /// existing one, pushing one undo step for the whole click. Returns the
    /// way id (new or existing). `node_ops` are the ops that made `node_id`
    /// for this click (empty for an existing node the user clicked to
    /// connect, which undo must therefore leave alone); `snap` is the way the
    /// node was spliced into, if any.
    pub(crate) fn add_extend_or_start_way(
        &mut self,
        layer_id: LayerId,
        node_id: i64,
        node_ops: Vec<EditOp>,
        snap: Option<i64>,
    ) -> i64 {
        let progress_way_id = self.add_progress().and_then(|p| p.way_id);
        let at_start = self.add_progress().is_some_and(|p| p.at_start);
        let last_node_id = self
            .add_progress()
            .map(|p| p.last_node_id)
            .unwrap_or(node_id);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return progress_way_id.unwrap_or(0);
        };
        let Some(editable) = layer.as_editable_mut() else {
            return progress_way_id.unwrap_or(0);
        };
        let mut rec = Recording::resume(layer_id, editable, node_ops);

        let attached = match progress_way_id {
            Some(way_id) if at_start => {
                let mut nodes = rec.layer().way_node_ids(way_id).unwrap_or_default();
                nodes.insert(0, node_id);
                rec.set_way_nodes(way_id, nodes).map(|()| way_id)
            }
            Some(way_id) => rec.extend_way(way_id, node_id).map(|()| way_id),
            None => Ok(rec.add_way(vec![last_node_id, node_id], Vec::new())),
        };
        let way_id = match attached {
            Ok(way_id) => way_id,
            Err(e) => {
                eprintln!("adding to the way was refused: {e}");
                // Take back the node this click made, so none is left over.
                roll_back_recording(rec);
                return progress_way_id.unwrap_or(0);
            }
        };

        let feature = |kind, id| osm_gpui::selection::FeatureRef { layer_id, kind, id };
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let (label, select) = match snap {
            Some(snap_way) => (
                EditLabel::SnappedNode,
                vec![
                    feature(Way, way_id),
                    feature(Way, snap_way),
                    feature(Node, node_id),
                ],
            ),
            None => (
                EditLabel::ExtendedWay,
                vec![feature(Way, way_id), feature(Node, node_id)],
            ),
        };
        self.undo_stack
            .push(UndoableAction::edit(layer_id, label, rec.into_ops()).selecting(select));
        way_id
    }

    /// The tags that decide what Add mode may snap onto: those of the way
    /// being drawn, if it has any. A way with none yet (the usual case; tags
    /// come after drawing) snaps to anything, see `osm_gpui::snap`.
    pub(crate) fn add_snap_sources(&self, layer_id: LayerId) -> Vec<Tags> {
        let Some(way_id) = self.add_progress().and_then(|p| p.way_id) else {
            return Vec::new();
        };
        self.layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| {
                e.feature_tags(&osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                })
            })
            .map(|t| vec![t])
            .unwrap_or_default()
    }
}
