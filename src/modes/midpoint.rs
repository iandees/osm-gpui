//! Midpoint handles: a selected way shows a small handle halfway along each
//! segment, and dragging one inserts a new vertex there, so a way can be made
//! more accurate without leaving Select mode. The new node is only created on
//! release (one undo step); while dragging, a ghost of the two new segments
//! follows the pointer.

use gpui::{point, px, size, Bounds, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_ops::Recording;
use osm_gpui::ids::WayId;
use osm_gpui::interaction::DRAG_THRESHOLD;
use osm_gpui::layers::LayerId;
use osm_gpui::selection::FeatureKind;

use super::preview::{blue, line_width, stroke_polyline, PreviewFrame};
use crate::undo::{EditLabel, UndoableAction};
use crate::{to_pt, MapViewer};

/// Segments shorter than this on screen show no handle: it would sit on top
/// of the vertices at its ends.
const MIN_SEGMENT_PX: f32 = 24.0;
/// How close, in pixels, a press must be to a handle to grab it.
const GRAB_PX: f32 = 8.0;

/// The middle of one segment of a selected way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Handle {
    pub(crate) layer_id: LayerId,
    pub(crate) way: WayId,
    /// Index of the segment's first node; the new node goes at `segment + 1`.
    pub(crate) segment: usize,
    pub(crate) at: LatLon,
    /// The segment's end points, for drawing the ghost.
    pub(crate) a: LatLon,
    pub(crate) b: LatLon,
}

/// A handle that has been pressed.
pub(crate) struct MidpointDrag {
    pub(crate) handle: Handle,
    pub(crate) down: Point<Pixels>,
    pub(crate) current: Point<Pixels>,
}

impl MidpointDrag {
    fn moved(&self) -> bool {
        let (dx, dy) = (
            self.current.x.as_f32() - self.down.x.as_f32(),
            self.current.y.as_f32() - self.down.y.as_f32(),
        );
        dx.hypot(dy) >= DRAG_THRESHOLD
    }
}

/// Whether a segment from `a` to `b` (screen points) is long enough for a
/// handle.
fn long_enough(a: (f32, f32), b: (f32, f32)) -> bool {
    (b.0 - a.0).hypot(b.1 - a.1) >= MIN_SEGMENT_PX
}

impl MapViewer {
    /// The handles of every selected way, in screen order of the data.
    pub(crate) fn midpoint_handles(&self) -> Vec<Handle> {
        let mut out = Vec::new();
        for feat in &self.selected {
            if feat.kind != FeatureKind::Way {
                continue;
            }
            let Some(editable) = self
                .layer_manager
                .find_layer(feat.layer_id)
                .and_then(|l| l.as_editable())
            else {
                continue;
            };
            let way = WayId(feat.id);
            let Some(nodes) = editable.way_node_ids(way) else {
                continue;
            };
            let positions: Vec<Option<LatLon>> =
                nodes.iter().map(|&n| editable.node_lat_lon(n)).collect();
            for (segment, pair) in positions.windows(2).enumerate() {
                let (Some(a), Some(b)) = (pair[0], pair[1]) else {
                    continue;
                };
                let (sa, sb) = (
                    to_pt(self.viewport.geo_to_screen(a)),
                    to_pt(self.viewport.geo_to_screen(b)),
                );
                if !long_enough(sa, sb) {
                    continue;
                }
                let mid = point(px((sa.0 + sb.0) / 2.0), px((sa.1 + sb.1) / 2.0));
                out.push(Handle {
                    layer_id: feat.layer_id,
                    way,
                    segment,
                    at: self.viewport.screen_to_geo(mid),
                    a,
                    b,
                });
            }
        }
        out
    }

    /// A Select-mode press on a handle starts a midpoint drag. Returns
    /// whether it did.
    pub(crate) fn midpoint_press(
        &mut self,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) -> bool {
        self.pointer.midpoint_drag = None;
        if modifiers.shift {
            return false;
        }
        let p = to_pt(position);
        let grabbed = self.midpoint_handles().into_iter().find(|h| {
            let s = to_pt(self.viewport.geo_to_screen(h.at));
            (s.0 - p.0).hypot(s.1 - p.1) <= GRAB_PX
        });
        let Some(handle) = grabbed else {
            return false;
        };
        self.pointer.interaction = osm_gpui::interaction::Interaction::Idle;
        self.pointer.midpoint_drag = Some(MidpointDrag {
            handle,
            down: position,
            current: position,
        });
        true
    }

    /// Pointer movement during a midpoint drag. Returns whether one is in
    /// progress (the move is then fully handled).
    pub(crate) fn midpoint_move(&mut self, position: Point<Pixels>, left_pressed: bool) -> bool {
        if !left_pressed {
            self.pointer.midpoint_drag = None;
            return false;
        }
        match &mut self.pointer.midpoint_drag {
            Some(drag) => {
                drag.current = position;
                true
            }
            None => false,
        }
    }

    /// Abandon a midpoint drag (Escape). Returns whether there was one.
    pub(crate) fn cancel_midpoint_drag(&mut self) -> bool {
        self.pointer.midpoint_drag.take().is_some()
    }

    /// The left button came up. Returns whether a midpoint drag was in
    /// progress: released past the threshold it inserts the vertex, else the
    /// press was a plain click.
    pub(crate) fn midpoint_release(
        &mut self,
        event: &gpui::MouseUpEvent,
        up_pos: Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(mut drag) = self.pointer.midpoint_drag.take() else {
            return false;
        };
        drag.current = up_pos;
        if !drag.moved() {
            self.handle_map_click(
                up_pos,
                event.modifiers.shift,
                event.click_count,
                osm_gpui::snap::snapping_disabled(event.modifiers.alt, event.modifiers.control),
                cx,
            );
            cx.notify();
            return true;
        }
        if self.refuse_edit_during_upload() {
            cx.notify();
            return true;
        }
        let Handle {
            layer_id,
            way,
            segment,
            ..
        } = drag.handle;
        let at = self.viewport.screen_to_geo(up_pos);
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            return true;
        };
        let mut rec = Recording::new(layer_id, editable);
        match rec.insert_node_into_way(way, segment + 1, at) {
            Ok(_) => {
                self.undo_stack.push(UndoableAction::edit(
                    layer_id,
                    EditLabel::InsertedNode,
                    rec.into_ops(),
                ));
            }
            Err(e) => self.set_status(e.to_string()),
        }
        cx.notify();
        true
    }

    /// Paint the handles of the selected ways, or the ghost of the vertex
    /// being dragged out of one.
    pub(super) fn paint_midpoints(&self, frame: &PreviewFrame<'_>, window: &mut Window) {
        if let Some(drag) = &self.pointer.midpoint_drag {
            if drag.moved() {
                let tip = frame.local(drag.current);
                stroke_polyline(
                    window,
                    [frame.geo(drag.handle.a), tip, frame.geo(drag.handle.b)],
                    line_width(),
                    blue(),
                );
                paint_handle(window, tip, px(5.0));
                return;
            }
        }
        for h in self.midpoint_handles() {
            paint_handle(window, frame.geo(h.at), px(4.0));
        }
    }
}

fn paint_handle(window: &mut Window, center: Point<Pixels>, half: Pixels) {
    window.paint_quad(gpui::quad(
        Bounds {
            origin: point(center.x - half, center.y - half),
            size: size(half * 2.0, half * 2.0),
        },
        half,
        gpui::rgba(0x3b82f6cc),
        px(1.5),
        gpui::white(),
        gpui::BorderStyle::default(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_segments_get_no_handle() {
        assert!(!long_enough((0.0, 0.0), (10.0, 10.0)));
        assert!(long_enough((0.0, 0.0), (30.0, 0.0)));
    }
}
