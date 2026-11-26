//! The Junction side-panel section: a top-down schematic of the junction at
//! the selected node (lanes, `turn:lanes` arrows, the movements out of one
//! approach coloured by what turn restrictions allow), and the problems the
//! junction model found. The model itself is `osm_gpui::junction`.

use gpui::{
    canvas, div, point, prelude::*, px, rgb, Context, Hsla, MouseDownEvent, Pixels, Point, Window,
};
use gpui_component::ActiveTheme;
use std::cell::Cell;
use std::rc::Rc;

use osm_gpui::ids::{NodeId, RelationId, WayId};
use osm_gpui::junction::{self, Junction, JunctionSource, MovementState, TurnKind};
use osm_gpui::lanes::{Arrow, CountSource};

use crate::road_paint::{
    arrow_head, fill, lane_divider, lane_glyph, stroke, ASPHALT, ASPHALT_UNKNOWN, CENTRE_LINE,
    LANE_LINE, V,
};
use osm_gpui::layers::{EditableLayer, LayerId};
use osm_gpui::osm::OsmRelation;
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::tags::Tags;
use osm_gpui::tr;
use osm_gpui::ui::style::{interactive_row, muted_text_size};

use crate::MapViewer;

const ALLOWED: u32 = 0x2E9E44;

/// Reads a live editable layer for the junction model.
struct LayerSource<'a> {
    layer: &'a dyn EditableLayer,
    layer_id: LayerId,
}

impl JunctionSource for LayerSource<'_> {
    fn node_pos(&self, id: NodeId) -> Option<osm_gpui::coordinates::LatLon> {
        self.layer.node_lat_lon(id)
    }
    fn ways_at(&self, id: NodeId) -> Vec<WayId> {
        self.layer.ways_using_node(id)
    }
    fn way_nodes(&self, id: WayId) -> Option<Vec<NodeId>> {
        self.layer.way_node_ids(id)
    }
    fn way_tags(&self, id: WayId) -> Option<Tags> {
        self.layer.feature_tags(&FeatureRef {
            layer_id: self.layer_id,
            kind: FeatureKind::Way,
            id: id.0,
        })
    }
    fn relations_touching(&self, node: NodeId, ways: &[WayId]) -> Vec<OsmRelation> {
        let mut ids: Vec<RelationId> = self.layer.relations_containing_node(node);
        for &w in ways {
            ids.extend(self.layer.relations_containing_way(w));
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| self.layer.relation(id))
            .collect()
    }
}

/// One arm laid out in the diagram.
#[derive(Clone, Debug)]
struct ArmLayout {
    /// Unit vector from the centre along the arm.
    u: V,
    /// Unit normal: the outbound (right-hand) side when driving away.
    n: V,
    /// Lane counts in and out (0 when that direction is closed).
    k_in: u32,
    k_out: u32,
    unknown: bool,
    /// Offset of the line between the directions, along `n`.
    centre: f32,
    /// Offsets of the road's edges along `n`: `left` on the inbound side.
    left: f32,
    right: f32,
    /// Distance from the centre to this arm's stop line, where its curbs end.
    stop: f32,
    arrows: Option<Vec<Vec<Arrow>>>,
}

impl ArmLayout {
    /// The point `along` the arm and `off` across it.
    fn at(&self, along: f32, off: f32) -> V {
        self.u.mul(along).add(self.n.mul(off))
    }
}

/// The whole diagram, in pixels relative to the canvas centre.
#[derive(Clone, Debug)]
struct Layout {
    lane_w: f32,
    len: f32,
    arms: Vec<ArmLayout>,
    /// The paved area: every road out to the canvas edge, joined to its
    /// neighbours by a small curb arc.
    outline: Vec<V>,
    /// Movement curves from the chosen arm: `(to arm, colour, dashed)`.
    curves: Vec<(usize, u32, bool)>,
    from: Option<usize>,
}

fn layout(j: &Junction, size: f32, from: Option<usize>) -> Layout {
    let half = size / 2.0;
    let len = half - 4.0;
    let max_lanes = j
        .arms
        .iter()
        .map(|a| {
            a.inbound.as_ref().map_or(0, |g| g.count) + a.outbound.as_ref().map_or(0, |g| g.count)
        })
        .max()
        .unwrap_or(2)
        .max(2);
    let lane_w = (size * 0.30 / max_lanes as f32).clamp(9.0, 22.0);

    let mut arms: Vec<ArmLayout> = j
        .arms
        .iter()
        .map(|a| {
            let b = a.bearing.to_radians();
            let u = V(b.sin(), -b.cos());
            let n = V(-u.1, u.0);
            let k_in = a.inbound.as_ref().map_or(0, |g| g.count);
            let k_out = a.outbound.as_ref().map_or(0, |g| g.count);
            let unknown = [&a.inbound, &a.outbound]
                .iter()
                .any(|g| g.as_ref().is_some_and(|g| g.source == CountSource::Unknown));
            // A oneway is centred on its axis; a two-way road has its
            // centre line on the axis.
            let centre = if k_in == 0 || k_out == 0 {
                (k_in as f32 - k_out as f32) * lane_w / 2.0
            } else {
                0.0
            };
            ArmLayout {
                u,
                n,
                k_in,
                k_out,
                unknown,
                centre,
                left: centre - k_in as f32 * lane_w,
                right: centre + k_out as f32 * lane_w,
                stop: 0.0,
                arrows: a.inbound.as_ref().and_then(|g| g.arrows.clone()),
            }
        })
        .collect();
    let bearings: Vec<f32> = j.arms.iter().map(|a| a.bearing).collect();
    let outline = curb_outline(&mut arms, &bearings, lane_w * 0.6, len, half * 0.6);

    let curves = from
        .map(|f| {
            j.movements_from(f)
                .map(|m| {
                    let (colour, dashed) = match m.state {
                        MovementState::Allowed => (ALLOWED, false),
                        MovementState::Banned { .. } => {
                            (osm_gpui::relation_overlay::RESTRICTION_NO_COLOUR, false)
                        }
                        MovementState::OnlyElsewhere { .. } => {
                            (osm_gpui::relation_overlay::RESTRICTION_ONLY_COLOUR, true)
                        }
                    };
                    (m.to, colour, dashed)
                })
                .collect()
        })
        .unwrap_or_default();

    Layout {
        lane_w,
        len,
        arms,
        outline,
        curves,
        from,
    }
}

/// Where the lines `p + u·t` and `q + v·s` cross: `(t, s)`, or `None` when
/// they are (nearly) parallel.
fn line_cross(p: V, u: V, q: V, v: V) -> Option<(f32, f32)> {
    let det = u.1 * v.0 - u.0 * v.1;
    if det.abs() < 1e-4 {
        return None;
    }
    let d = q.sub(p);
    let t = (d.1 * v.0 - d.0 * v.1) / det;
    let s = (u.0 * d.1 - u.1 * d.0) / det;
    Some((t, s))
}

/// The paved outline: each road's two edges out to `len`, and between each
/// pair of neighbouring roads a curb of radius `radius` where their facing
/// edges meet. Sets each arm's `stop` to where its curbs end. `arms` must be
/// in clockwise order (`bearings` ascending), as the junction model gives
/// them.
fn curb_outline(
    arms: &mut [ArmLayout],
    bearings: &[f32],
    radius: f32,
    len: f32,
    max_stop: f32,
) -> Vec<V> {
    let n = arms.len();
    // Per corner (arm i and its clockwise neighbour): the curb's tangent
    // point on each edge, as a distance along the arm, and the corner
    // point (the bezier control) if the edges meet in front of the centre.
    struct Corner {
        t_a: f32,
        t_b: f32,
        apex: Option<V>,
    }
    let corners: Vec<Corner> = (0..n)
        .map(|i| {
            let (a, b) = (&arms[i], &arms[(i + 1) % n]);
            let mut gap = bearings[(i + 1) % n] - bearings[i];
            if gap <= 0.0 {
                gap += 360.0;
            }
            // The edges facing each other: a's right side, b's left.
            let (p, q) = (a.n.mul(a.right), b.n.mul(b.left));
            match line_cross(p, a.u, q, b.u).filter(|_| gap < 170.0) {
                Some((t, s)) if t > -1.0 && s > -1.0 => {
                    let tangent = radius / (gap.to_radians() / 2.0).tan();
                    Corner {
                        t_a: (t.max(0.0) + tangent).min(max_stop),
                        t_b: (s.max(0.0) + tangent).min(max_stop),
                        apex: Some(p.add(a.u.mul(t.max(0.0)))),
                    }
                }
                // A straight run or a reflex angle: the edges join directly.
                _ => {
                    let reach = |x: &ArmLayout| x.left.abs().max(x.right.abs());
                    let t = reach(a).max(reach(b)) * 0.5;
                    Corner {
                        t_a: t,
                        t_b: t,
                        apex: None,
                    }
                }
            }
        })
        .collect();
    for i in 0..n {
        let before = &corners[(i + n - 1) % n];
        let after = &corners[i];
        arms[i].stop = before.t_b.max(after.t_a).max(2.0);
    }

    let mut out = Vec::new();
    for i in 0..n {
        let (a, b) = (&arms[i], &arms[(i + 1) % n]);
        let c = &corners[i];
        out.push(a.at(len, a.left));
        out.push(a.at(len, a.right));
        let from = a.at(c.t_a, a.right);
        let to = b.at(c.t_b, b.left);
        out.push(from);
        if let Some(apex) = c.apex {
            for k in 1..8 {
                let t = k as f32 / 8.0;
                let p = from
                    .mul((1.0 - t) * (1.0 - t))
                    .add(apex.mul(2.0 * (1.0 - t) * t))
                    .add(to.mul(t * t));
                out.push(p);
            }
        }
        out.push(to);
    }
    out
}

fn paint(l: &Layout, o: Point<Pixels>, window: &mut Window) {
    let w = l.lane_w;
    let edge = |a: &ArmLayout, along: f32, off: f32| a.at(along, off);

    fill(window, o, &l.outline, ASPHALT);

    for a in &l.arms {
        // A road whose lane count isn't known is drawn lighter.
        if a.unknown {
            fill(
                window,
                o,
                &[
                    edge(a, a.stop, a.left),
                    edge(a, l.len, a.left),
                    edge(a, l.len, a.right),
                    edge(a, a.stop, a.right),
                ],
                ASPHALT_UNKNOWN,
            );
        }
        // Lane dividers within each direction.
        let dividers = (1..a.k_in)
            .map(|j| a.centre - j as f32 * w)
            .chain((1..a.k_out).map(|j| a.centre + j as f32 * w));
        for off in dividers {
            lane_divider(window, o, edge(a, a.stop + 2.0, off), edge(a, l.len, off));
        }
        // Double centre line between the directions.
        if a.k_in > 0 && a.k_out > 0 {
            for d in [-1.2, 1.2] {
                let off = a.centre + d;
                stroke(
                    window,
                    o,
                    &[edge(a, a.stop, off), edge(a, l.len, off)],
                    1.2,
                    CENTRE_LINE,
                );
            }
        }
        // Stop line across the lanes coming in.
        if a.k_in > 0 {
            stroke(
                window,
                o,
                &[
                    edge(a, a.stop + 1.5, a.left),
                    edge(a, a.stop + 1.5, a.centre - 1.5),
                ],
                2.0,
                LANE_LINE,
            );
        }
        // One glyph per lane coming in, pointing into the junction.
        if let Some(arrows) = &a.arrows {
            let len = (w * 1.5).clamp(12.0, 24.0);
            let h = a.u.mul(-1.0);
            for (j, lane) in arrows.iter().enumerate().take(a.k_in as usize) {
                let off = a.centre - (j as f32 + 0.5) * w;
                let base = edge(a, a.stop + 6.0 + len, off);
                lane_glyph(window, o, base, h, len, lane);
            }
        }
    }

    // Movements from the chosen arm.
    let Some(f) = l.from else { return };
    let mut sorted = l.curves.clone();
    // Allowed under the others, so a ban is never hidden.
    sorted.sort_by_key(|&(_, c, _)| c != ALLOWED);
    for (to, colour, dash) in sorted {
        let (pts, tip) = curve(l, f, to);
        if dash {
            for seg in pts.chunks(3).step_by(2) {
                stroke(window, o, seg, 3.0, colour);
            }
        } else {
            stroke(window, o, &pts, 3.0, colour);
        }
        arrow_head(window, o, tip, l.arms[to].u, 8.0, colour);
    }
}

/// The movement curve from arm `from` to arm `to`: its points and the tip of
/// its arrow head, relative to the canvas centre.
fn curve(l: &Layout, from: usize, to: usize) -> (Vec<V>, V) {
    let w = l.lane_w;
    let (fa, ta) = (&l.arms[from], &l.arms[to]);
    let start = fa.at(fa.stop, fa.centre - fa.k_in as f32 * w / 2.0);
    let out = ta.centre + ta.k_out as f32 * w / 2.0;
    let end = ta.at(ta.stop, out);
    let ctrl = if to == from {
        fa.u.mul(fa.stop * 0.2)
    } else {
        V(0.0, 0.0)
    };
    let pts = (0..=24)
        .map(|i| {
            let t = i as f32 / 24.0;
            start
                .mul((1.0 - t) * (1.0 - t))
                .add(ctrl.mul(2.0 * (1.0 - t) * t))
                .add(end.mul(t * t))
        })
        .chain([ta.at(ta.stop + 12.0, out)])
        .collect();
    (pts, ta.at(ta.stop + 18.0, out))
}

/// What a click at `p` (relative to the canvas centre) hits: the movement
/// curve to an arm (within a few pixels), else an arm's road.
#[derive(Debug, PartialEq)]
enum DiagramHit {
    Movement(usize),
    Arm(usize),
}

fn hit_test(l: &Layout, p: V) -> Option<DiagramHit> {
    const SLOP: f32 = 6.0;
    let dist = |a: V, b: V| (a.0 - b.0).hypot(a.1 - b.1);
    if let Some(f) = l.from {
        let near = l
            .curves
            .iter()
            .map(|&(to, _, _)| {
                let (pts, tip) = curve(l, f, to);
                let d = pts
                    .iter()
                    .chain([&tip])
                    .map(|&q| dist(p, q))
                    .fold(f32::MAX, f32::min);
                (to, d)
            })
            .filter(|&(_, d)| d <= SLOP)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((to, _)) = near {
            return Some(DiagramHit::Movement(to));
        }
    }
    l.arms
        .iter()
        .position(|a| {
            let along = p.0 * a.u.0 + p.1 * a.u.1;
            let across = p.0 * a.n.0 + p.1 * a.n.1;
            along >= a.stop && along <= l.len && across >= a.left && across <= a.right
        })
        .map(DiagramHit::Arm)
}

/// Where an arm's label sits, relative to the canvas centre.
fn label_pos(l: &Layout, arm: usize) -> V {
    l.arms[arm].u.mul(l.len - 22.0)
}

/// Label pill width in pixels.
const LABEL_W: f32 = 96.0;

fn turn_label(t: TurnKind) -> &'static str {
    match t {
        TurnKind::Straight => "straight on",
        TurnKind::SlightLeft => "slight left",
        TurnKind::Left => "left",
        TurnKind::SharpLeft => "sharp left",
        TurnKind::SlightRight => "slight right",
        TurnKind::Right => "right",
        TurnKind::SharpRight => "sharp right",
        TurnKind::UTurn => "U-turn",
    }
}

impl MapViewer {
    /// The junction at the single selected node, if two or more road arms
    /// meet there.
    pub(crate) fn selected_junction(&self) -> Option<(LayerId, Junction)> {
        let [feature] = self.selected.as_slice() else {
            return None;
        };
        if feature.kind != FeatureKind::Node {
            return None;
        }
        let layer = self
            .layer_manager
            .find_layer(feature.layer_id)?
            .as_editable()?;
        let src = LayerSource {
            layer,
            layer_id: feature.layer_id,
        };
        Some((feature.layer_id, junction::build(&src, NodeId(feature.id))?))
    }

    /// The Junction section's content, or `None` when no junction is
    /// selected.
    pub(crate) fn render_junction_section(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (layer_id, j) = self.selected_junction()?;
        let theme = cx.theme();
        let (muted, ground, fg, warn): (Hsla, Hsla, Hsla, Hsla) = (
            theme.muted_foreground,
            theme.muted,
            theme.foreground,
            theme.warning,
        );

        let from = match self.junction_from {
            Some((node, arm)) if node == j.node && arm < j.arms.len() => Some(arm),
            _ => j.default_from_arm(),
        };
        // Edge to edge: the panel's width less its 1px left border.
        let size = (self.side_panel_width - 1.0).max(200.0);
        let l = layout(&j, size, from);
        let half = size / 2.0;
        let node = j.node;

        let labels = (0..j.arms.len()).map(|i| {
            let p = label_pos(&l, i);
            let chosen = Some(i) == from;
            // Kept inside the canvas.
            let x = (half + p.0 - LABEL_W / 2.0).clamp(2.0, size - LABEL_W - 2.0);
            let y = (half + p.1 - 10.0).clamp(2.0, size - 22.0);
            div()
                .id(("junction-arm", i))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(LABEL_W))
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(LABEL_W))
                        .px_1()
                        .rounded_sm()
                        .text_size(muted_text_size())
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .cursor_pointer()
                        .bg(if chosen {
                            theme.primary
                        } else {
                            theme.background.opacity(0.85)
                        })
                        .text_color(if chosen { theme.primary_foreground } else { fg })
                        .border_1()
                        .border_color(theme.border)
                        .child(j.arms[i].label.clone()),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.junction_from = Some((node, i));
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        });

        // Where the canvas was painted, for hit-testing clicks on it.
        let origin: Rc<Cell<Option<Point<Pixels>>>> = Rc::new(Cell::new(None));
        let diagram = div()
            .id("junction-diagram")
            .relative()
            .w(px(size))
            .h(px(size))
            .bg(ground)
            .overflow_hidden()
            .child({
                let l = l.clone();
                let origin = origin.clone();
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let o = point(bounds.origin.x + px(half), bounds.origin.y + px(half));
                        origin.set(Some(o));
                        paint(&l, o, window);
                    },
                )
                .absolute()
                .inset_0()
                .size_full()
            })
            .children(labels)
            .on_mouse_down(gpui::MouseButton::Left, {
                let l = l.clone();
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let Some(o) = origin.get() else {
                        return;
                    };
                    let p = V(
                        (ev.position.x - o.x).as_f32(),
                        (ev.position.y - o.y).as_f32(),
                    );
                    match (hit_test(&l, p), l.from) {
                        (Some(DiagramHit::Movement(to)), Some(f)) => {
                            this.toggle_junction_movement(f, to)
                        }
                        (Some(DiagramHit::Arm(arm)), _) => this.junction_from = Some((node, arm)),
                        _ => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                })
            });

        let movements = from.map(|f| {
            let rows = j.movements_from(f).enumerate().map(|(i, m)| {
                let to = m.to;
                let (dot, text, relation) = match &m.state {
                    MovementState::Allowed => {
                        (ALLOWED, tr!("panel.junction.allowed").to_string(), None)
                    }
                    MovementState::Banned { relation, value } => (
                        osm_gpui::relation_overlay::RESTRICTION_NO_COLOUR,
                        format!("r{} {value}", relation.0),
                        Some(*relation),
                    ),
                    MovementState::OnlyElsewhere { relation, value } => (
                        osm_gpui::relation_overlay::RESTRICTION_ONLY_COLOUR,
                        format!("r{} {value}", relation.0),
                        Some(*relation),
                    ),
                };
                // Clicking the row steps the movement's restriction; the
                // relation text selects the relation instead.
                let row = interactive_row(("junction-move", i), false, cx)
                    .gap_2()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.toggle_junction_movement(f, to);
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(rgb(dot))
                            .flex_shrink_0(),
                    )
                    .child(div().flex_1().min_w(px(0.)).truncate().child(format!(
                        "{} → {}",
                        turn_label(m.turn),
                        j.arms[m.to].label
                    )))
                    .child({
                        let label = div()
                            .id(("junction-move-relation", i))
                            .flex_shrink_0()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(text);
                        match relation {
                            Some(rel) => label
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        this.select_relation(layer_id, rel);
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                ),
                            None => label,
                        }
                    });
                row
            });
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .pt_1()
                        .child(tr!("panel.junction.from", arm = j.arms[f].label.clone())),
                )
                .children(rows)
        });

        let issue_rows = j.issues.iter().map(|issue| {
            div()
                .flex()
                .gap_1()
                .text_size(muted_text_size())
                .child(div().text_color(warn).flex_shrink_0().child("⚠"))
                .child(div().flex_1().min_w(px(0.)).child(issue.message.clone()))
        });
        let listed = j.listed.iter().map(|r| {
            div()
                .text_size(muted_text_size())
                .child(format!("r{} {} ({})", r.relation.0, r.value, r.note))
        });

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(diagram)
                // The section has no inset of its own (the diagram runs edge
                // to edge), so the text gets the usual 12px here.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .child(
                            div()
                                .text_size(muted_text_size())
                                .text_color(muted)
                                .child(tr!("panel.junction.hint")),
                        )
                        .children(movements)
                        .when(!j.listed.is_empty(), |d| {
                            d.child(
                                div()
                                    .pt_1()
                                    .text_size(muted_text_size())
                                    .text_color(muted)
                                    .child(tr!("panel.junction.via_way")),
                            )
                            .children(listed)
                        })
                        .child(
                            div()
                                .pt_1()
                                .text_size(muted_text_size())
                                .text_color(muted)
                                .child(if j.issues.is_empty() {
                                    tr!("panel.junction.no_issues")
                                } else {
                                    tr!("panel.junction.issues")
                                }),
                        )
                        .children(issue_rows),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-way road with one lane each way at `bearing`, lanes 10 px.
    fn arm(bearing: f32) -> ArmLayout {
        let b = bearing.to_radians();
        let u = V(b.sin(), -b.cos());
        ArmLayout {
            u,
            n: V(-u.1, u.0),
            k_in: 1,
            k_out: 1,
            unknown: false,
            centre: 0.0,
            left: -10.0,
            right: 10.0,
            stop: 0.0,
            arrows: None,
        }
    }

    #[test]
    fn a_square_crossing_is_only_as_big_as_its_roads_plus_the_curb() {
        let bearings = [0.0, 90.0, 180.0, 270.0];
        let mut arms: Vec<ArmLayout> = bearings.iter().map(|&b| arm(b)).collect();
        let outline = curb_outline(&mut arms, &bearings, 4.0, 100.0, 60.0);
        // Edges 10 px from each axis meet 10 px out; the curb adds its radius.
        for a in &arms {
            assert!((a.stop - 14.0).abs() < 0.01, "stop {}", a.stop);
        }
        // Nothing of the outline comes nearer the centre than the corners.
        let nearest = outline
            .iter()
            .map(|p| p.0.hypot(p.1))
            .fold(f32::MAX, f32::min);
        assert!(nearest > 14.0, "{nearest}");
    }

    #[test]
    fn a_straight_through_road_has_no_curb_on_its_straight_side() {
        // A T: north, south and east. West of the north-south road is
        // straight, so it adds no curb and those stops come from the east.
        let bearings = [0.0, 90.0, 180.0];
        let mut arms: Vec<ArmLayout> = bearings.iter().map(|&b| arm(b)).collect();
        curb_outline(&mut arms, &bearings, 4.0, 100.0, 60.0);
        assert!((arms[0].stop - 14.0).abs() < 0.01);
        assert!((arms[1].stop - 14.0).abs() < 0.01);
        assert!((arms[2].stop - 14.0).abs() < 0.01);
    }
}
