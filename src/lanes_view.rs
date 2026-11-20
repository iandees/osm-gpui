//! The Lanes side-panel section: the selected road drawn top-down as a strip
//! of lanes (as its `lanes*` and `turn:lanes*` tags describe them), with
//! buttons to add or remove lanes in each direction and to toggle the turn
//! arrows of the chosen lane. Each change is one undo step. The tag logic is
//! `osm_gpui::lanes`.

use gpui::{canvas, div, point, prelude::*, px, Context, MouseDownEvent, Pixels, Point, Window};
use gpui_component::ActiveTheme;

use osm_gpui::ids::{NodeId, WayId};
use osm_gpui::lanes::{
    is_road, lane_tags_text, parking_is_physical, parse_lane_tags, paste_changes, Arrow, Category,
    Contraflow, ContraflowKind, DirLanes, Group, LaneConfig, LaneEdit, ParkingSide, Side,
    PARKING_KINDS, PARKING_ORIENTATIONS,
};
use osm_gpui::layers::{EditableLayer, LayerId};
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::tags::Tags;
use osm_gpui::ui::style::muted_text_size;
use osm_gpui::{tr, tr_count};

use crate::road_paint::{
    fill, lane_divider, lane_glyph, stroke, ASPHALT, CENTRE_LINE, LANE_LINE, V,
};
use crate::selection_actions::{record_edit, refused};
use crate::undo::{EditLabel, UndoableAction};
use crate::MapViewer;

/// Index of the Lanes section in `MapViewer::side_panel_open`.
pub(crate) const SECTION: usize = 9;

/// Height of one lane in the strip, in pixels.
const LANE_H: f32 = 28.0;
/// Space above and below the road.
const MARGIN: f32 = 10.0;
/// Space left and right of the road, where the end badges sit.
const INSET: f32 = 30.0;
/// Diameter of an end badge.
const BADGE: f32 = 20.0;

/// The way's first node ("A", the strip's left end) and last node ("B", its
/// right end): the same badges mark them on the map, so the strip's
/// directions can be matched to the road.
pub(crate) const END_A: u32 = 0xE8590C;
pub(crate) const END_B: u32 = 0x1E6FE8;

/// A round end badge with its letter.
fn end_badge(letter: &'static str, colour: u32) -> gpui::Div {
    div()
        .size(px(BADGE))
        .flex_shrink_0()
        .rounded_full()
        .border_2()
        .border_color(gpui::white())
        .bg(gpui::rgb(colour))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(gpui::white())
        .child(letter)
}

/// A lane of the strip: its group and index (left to right as that group's
/// driver sees them).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Row {
    group: Group,
    lane: usize,
}

/// The strip's rows top to bottom: backward lanes (their driver's leftmost
/// at the bottom, by the centre line), shared centre lanes, then forward
/// lanes (leftmost at the top). Forward traffic drives to the right.
///
/// Left-hand traffic swaps the directions: the forward lanes are on top.
fn rows(c: &LaneConfig, left_hand: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut push = |group, count: u32, rev: bool| {
        let lanes: Vec<usize> = (0..count as usize).collect();
        if rev {
            rows.extend(lanes.into_iter().rev().map(|lane| Row { group, lane }));
        } else {
            rows.extend(lanes.into_iter().map(|lane| Row { group, lane }));
        }
    };
    let backward = |push: &mut dyn FnMut(Group, u32, bool)| {
        if let Some(b) = &c.backward {
            push(Group::Backward, b.count, true);
        }
    };
    let forward = |push: &mut dyn FnMut(Group, u32, bool)| {
        if let Some(f) = &c.forward {
            push(Group::Forward, f.count, false);
        }
    };
    if left_hand {
        forward(&mut push);
        push(Group::BothWays, c.both_ways.count, false);
        backward(&mut push);
    } else {
        backward(&mut push);
        push(Group::BothWays, c.both_ways.count, false);
        forward(&mut push);
    }
    rows
}

/// Height of a parking band beside the road, and of a contraflow lane.
const PARK_H: f32 = 22.0;
const CONTRA_H: f32 = 16.0;

fn parking_band(p: &ParkingSide) -> bool {
    parking_is_physical(&p.kind)
}

/// Space above (`Side::Left`, the way's left) or below the lanes for the
/// contraflow lane and parking band on that side.
fn side_extra(c: &LaneConfig, side: Side) -> f32 {
    let p = if side == Side::Left {
        &c.parking_left
    } else {
        &c.parking_right
    };
    let contra = c
        .contraflow
        .is_some_and(|cf| cf.side == side && cf.kind != ContraflowKind::Shared);
    contra as u8 as f32 * CONTRA_H + parking_band(p) as u8 as f32 * PARK_H
}

/// Top of the first lane row.
fn road_top(c: &LaneConfig) -> f32 {
    MARGIN + side_extra(c, Side::Left)
}

/// Height of the whole strip.
fn strip_height(c: &LaneConfig) -> f32 {
    road_top(c)
        + (c.forward.as_ref().map_or(0, |d| d.count)
            + c.backward.as_ref().map_or(0, |d| d.count)
            + c.both_ways.count) as f32
            * LANE_H
        + side_extra(c, Side::Right)
        + MARGIN
}

/// A lane's fill when its tags give it a purpose, or `None` for ordinary
/// asphalt.
fn lane_tint(d: &DirLanes, lane: usize) -> Option<u32> {
    let a = |cat| d.attr(cat, lane);
    let no_cars = a(Category::Vehicle) == "no";
    if matches!(a(Category::Cycleway), "lane" | "track")
        || (no_cars && a(Category::Bicycle) == "designated")
    {
        Some(0x4E9B63)
    } else if a(Category::Bus) == "designated" {
        Some(0xB5604F)
    } else if matches!(a(Category::Vehicle), "hov" | "designated") {
        Some(0x9B7A2F)
    } else if no_cars {
        Some(0x7A4C4C)
    } else {
        None
    }
}

/// A short description of a lane's access tags, for its label in the strip.
fn lane_label(d: &DirLanes, lane: usize) -> String {
    [
        (Category::Vehicle, "vehicle"),
        (Category::Bicycle, "bicycle"),
        (Category::Cycleway, "cycleway"),
        (Category::Bus, "bus"),
    ]
    .iter()
    .filter_map(|&(cat, name)| {
        let v = d.attr(cat, lane);
        (!v.is_empty()).then(|| format!("{name}={v}"))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// Cars standing in a parking band between `y0` and `y1`.
fn paint_parking(
    p: &ParkingSide,
    (x0, x1): (f32, f32),
    (y0, y1): (f32, f32),
    window: &mut Window,
    o: Point<Pixels>,
) {
    fill(
        window,
        o,
        &[V(x0, y0), V(x1, y0), V(x1, y1), V(x0, y1)],
        0x464B52,
    );
    let h = y1 - y0;
    let (cw, pitch, slant) = match p.orientation.as_str() {
        "perpendicular" => (10.0, 16.0, 0.0),
        "diagonal" => (10.0, 16.0, 6.0),
        _ => (22.0, 30.0, 0.0),
    };
    let ch = if p.orientation == "perpendicular" || p.orientation == "diagonal" {
        h - 4.0
    } else {
        h - 8.0
    };
    let top = y0 + (h - ch) / 2.0;
    let mut x = x0 + 10.0;
    while x + cw + slant < x1 - 6.0 {
        fill(
            window,
            o,
            &[
                V(x + slant, top),
                V(x + slant + cw, top),
                V(x + cw, top + ch),
                V(x, top + ch),
            ],
            0xAEB3BB,
        );
        x += pitch;
    }
}

fn paint_strip(c: &LaneConfig, left_hand: bool, width: f32, o: Point<Pixels>, window: &mut Window) {
    let rows = rows(c, left_hand);
    let (x0, x1) = (INSET, width - INSET);
    let top = road_top(c);
    let bottom = top + rows.len() as f32 * LANE_H;
    let y = |i: usize| top + i as f32 * LANE_H;
    // Outer bands, outermost parking, then contraflow next to the lanes.
    for side in [Side::Left, Side::Right] {
        let (p, cf) = if side == Side::Left {
            (
                &c.parking_left,
                c.contraflow.filter(|cf| cf.side == Side::Left),
            )
        } else {
            (
                &c.parking_right,
                c.contraflow.filter(|cf| cf.side == Side::Right),
            )
        };
        let contra = cf.is_some_and(|cf| cf.kind != ContraflowKind::Shared);
        let (inner, dir) = if side == Side::Left {
            (top, -1.0)
        } else {
            (bottom, 1.0)
        };
        let mut edge = inner;
        if contra {
            let far = edge + dir * CONTRA_H;
            let (ya, yb) = (edge.min(far), edge.max(far));
            fill(
                window,
                o,
                &[V(x0, ya), V(x1, ya), V(x1, yb), V(x0, yb)],
                0x4E9B63,
            );
            // Against the oneway: pointing at A.
            lane_glyph(
                window,
                o,
                V(x1 - 20.0, (ya + yb) / 2.0),
                V(-1.0, 0.0),
                CONTRA_H + 4.0,
                &[Arrow::Through],
            );
            edge = far;
        }
        if parking_band(p) {
            let far = edge + dir * PARK_H;
            paint_parking(p, (x0, x1), (edge.min(far), edge.max(far)), window, o);
        }
    }
    fill(
        window,
        o,
        &[V(x0, top), V(x1, top), V(x1, bottom), V(x0, bottom)],
        ASPHALT,
    );
    for (i, row) in rows.iter().enumerate() {
        if let Some(t) = c.group(row.group).and_then(|d| lane_tint(d, row.lane)) {
            fill(
                window,
                o,
                &[V(x0, y(i)), V(x1, y(i)), V(x1, y(i + 1)), V(x0, y(i + 1))],
                t,
            );
        }
    }
    for i in 1..rows.len() {
        let (a, b) = (rows[i - 1], rows[i]);
        // Lanes of the same kind are divided by a dashed line.
        if a.group == b.group {
            lane_divider(window, o, V(x0, y(i)), V(x1, y(i)));
        } else {
            // Between the directions (or beside a shared centre lane).
            for d in [-1.2, 1.2] {
                stroke(
                    window,
                    o,
                    &[V(x0, y(i) + d), V(x1, y(i) + d)],
                    1.2,
                    CENTRE_LINE,
                );
            }
        }
    }
    // Arrows near each direction's far end, pointing the way it drives.
    let len = 24.0;
    for (i, row) in rows.iter().enumerate() {
        let cy = y(i) + LANE_H / 2.0;
        let arrows = c
            .group(row.group)
            .and_then(|d| d.arrows.as_ref())
            .and_then(|a| a.get(row.lane));
        match row.group {
            Group::Forward | Group::Backward => {
                let Some(arrows) = arrows else { continue };
                // Rotated so the glyph's "up" is the driving direction.
                let (h, base) = if row.group == Group::Forward {
                    (V(1.0, 0.0), V(x1 - 16.0 - len, cy))
                } else {
                    (V(-1.0, 0.0), V(x0 + 16.0 + len, cy))
                };
                lane_glyph(window, o, base, h, len, arrows);
            }
            Group::BothWays => {
                // Both-ways turn lane: left arrows facing each way unless
                // `turn:lanes:both_ways` says otherwise.
                let arrows: &[Arrow] = arrows.map_or(&[Arrow::Left], Vec::as_slice);
                for (h, base) in [
                    (V(1.0, 0.0), V(width / 2.0 - 30.0, cy)),
                    (V(-1.0, 0.0), V(width / 2.0 + 30.0, cy)),
                ] {
                    lane_glyph(window, o, base, h, len, arrows);
                }
            }
        }
    }
    // Road edges.
    for yy in [top, bottom] {
        stroke(window, o, &[V(x0, yy), V(x1, yy)], 1.0, LANE_LINE);
    }
}

/// The road names meeting `node`, other than `way`, for "towards ...".
fn names_at(layer: &dyn EditableLayer, layer_id: LayerId, node: NodeId, way: WayId) -> String {
    let mut names: Vec<String> = Vec::new();
    for w in layer.ways_using_node(node) {
        if w == way {
            continue;
        }
        let Some(tags) = layer.feature_tags(&FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: w.get(),
        }) else {
            continue;
        };
        if !is_road(&tags) {
            continue;
        }
        let name = tags
            .get("name")
            .or_else(|| tags.get("ref"))
            .map_or_else(|| format!("w{}", w.get()), str::to_string);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        tr!("panel.lanes.way_end").to_string()
    } else {
        names.join(" / ")
    }
}

/// The glyph a toggle button shows for an arrow, with the driving direction
/// up.
fn arrow_symbol(a: Arrow) -> &'static str {
    match a {
        Arrow::Reverse => "↶",
        Arrow::SharpLeft => "↙",
        Arrow::Left => "←",
        Arrow::SlightLeft => "↖",
        Arrow::Through => "↑",
        Arrow::SlightRight => "↗",
        Arrow::Right => "→",
        Arrow::SharpRight => "↘",
        Arrow::MergeToLeft => "⇖",
        Arrow::MergeToRight => "⇗",
    }
}

impl MapViewer {
    /// The single selected way, if it is a road: its layer, id and tags.
    fn selected_road(&self) -> Option<(LayerId, WayId, Tags)> {
        let [feature] = self.selected.as_slice() else {
            return None;
        };
        if feature.kind != FeatureKind::Way {
            return None;
        }
        let layer = self
            .layer_manager
            .find_layer(feature.layer_id)?
            .as_editable()?;
        let tags = layer.feature_tags(feature)?;
        is_road(&tags).then_some((feature.layer_id, WayId(feature.id), tags))
    }

    /// The A and B badges on the map at the selected road's first and last
    /// nodes, matching the Lanes strip's ends, while that section is open.
    /// Each sits just beyond its end of the way, so it doesn't hide the node.
    pub(crate) fn render_lane_end_markers(&self) -> Vec<gpui::AnyElement> {
        let mut out = Vec::new();
        if !self.side_panel_open[SECTION] {
            return out;
        }
        let Some((layer_id, way, _)) = self.selected_road() else {
            return out;
        };
        let Some(layer) = self
            .layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
        else {
            return out;
        };
        let Some(nodes) = layer.way_node_ids(way).filter(|n| n.len() >= 2) else {
            return out;
        };
        let screen = |n: NodeId| {
            layer.node_lat_lon(n).map(|at| {
                let p = self.viewport.geo_to_screen(at);
                V(p.x.as_f32(), p.y.as_f32())
            })
        };
        let n = nodes.len();
        for (end, inner, letter, colour) in [
            (nodes[0], nodes[1], "A", END_A),
            (nodes[n - 1], nodes[n - 2], "B", END_B),
        ] {
            let (Some(p), Some(q)) = (screen(end), screen(inner)) else {
                continue;
            };
            let d = p.sub(q);
            let len = d.0.hypot(d.1);
            let out_dir = if len > 0.0 {
                d.mul(1.0 / len)
            } else {
                V(0.0, -1.0)
            };
            let c = p.add(out_dir.mul(BADGE * 0.9));
            out.push(
                end_badge(letter, colour)
                    .absolute()
                    .left(px(c.0 - BADGE / 2.0))
                    .top(px(c.1 - BADGE / 2.0))
                    .into_any_element(),
            );
        }
        out
    }

    /// Apply a lane edit to the selected road as one undo step.
    pub(crate) fn apply_lane_edit(&mut self, edit: LaneEdit) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, way, tags)) = self.selected_road() else {
            return;
        };
        let mut config = LaneConfig::read(&tags);
        if !config.apply(edit) {
            return;
        }
        let changes = config.tag_changes(&tags);
        self.commit_lane_changes(layer_id, way, changes);
    }

    /// Paste lane tags from the clipboard (`key=value` per line) onto the
    /// selected road: its lane tags become exactly the pasted ones.
    pub(crate) fn paste_lane_tags(&mut self, cx: &mut Context<Self>) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, way, tags)) = self.selected_road() else {
            return;
        };
        let text = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default();
        let pasted = parse_lane_tags(&text);
        if pasted.is_empty() {
            self.set_status(tr!("panel.lanes.paste_none").to_string());
            return;
        }
        self.commit_lane_changes(layer_id, way, paste_changes(&tags, &pasted));
    }

    /// Copy the selected road's lane tags to the clipboard.
    pub(crate) fn copy_lane_tags(&mut self, cx: &mut Context<Self>) {
        let Some((_, _, tags)) = self.selected_road() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(lane_tags_text(&tags)));
        self.set_status(tr!("panel.lanes.copied").to_string());
    }

    /// Set and remove tags on a way as one undo step.
    fn commit_lane_changes(
        &mut self,
        layer_id: LayerId,
        way: WayId,
        changes: Vec<(String, Option<String>)>,
    ) {
        if changes.is_empty() {
            return;
        }
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|layer| layer.as_editable_mut())
        else {
            return;
        };
        let result = record_edit(layer_id, editable, |rec| {
            for (key, value) in &changes {
                match value {
                    Some(v) => rec.set_tag(FeatureKind::Way, way.get(), key, v),
                    None => rec.remove_tag(FeatureKind::Way, way.get(), key),
                }
                .map_err(refused)?;
            }
            Ok(())
        });
        match result {
            Ok(((), ops)) => {
                // The Fields inputs (Lanes among them) hold their text until
                // rebuilt; drop them so they show the new tags.
                self.fields_panel.fields_text_inputs.clear();
                self.fields_panel.fields_text_subscribed.clear();
                let label = tr!("action.lanes.changed").to_string();
                self.undo_stack.push(UndoableAction::edit(
                    layer_id,
                    EditLabel::Text(label.clone()),
                    ops,
                ));
                self.set_status(label);
            }
            Err(message) => self.set_status(message),
        }
    }

    /// The Lanes section's content, or `None` unless one road is selected.
    pub(crate) fn render_lanes_section(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (layer_id, way, tags) = self.selected_road()?;
        let layer = self.layer_manager.find_layer(layer_id)?.as_editable()?;
        let config = LaneConfig::read(&tags);
        let left_hand = self.lane_left_hand;
        let rows = rows(&config, left_hand);
        let nodes = layer.way_node_ids(way)?;
        let (first, last) = (*nodes.first()?, *nodes.last()?);

        let theme = cx.theme();
        let (muted, ground, border, primary, primary_fg, fg) = (
            theme.muted_foreground,
            theme.muted,
            theme.border,
            theme.primary,
            theme.primary_foreground,
            theme.foreground,
        );

        // The chosen lane, if it still exists.
        let chosen = self
            .lane_selected
            .filter(|(w, _, _)| *w == way)
            .map(|(_, group, lane)| Row { group, lane })
            .filter(|r| rows.contains(r));

        let width = (self.side_panel_width - 1.0).max(200.0);
        let height = strip_height(&config);
        let top = road_top(&config);
        let lane_rows = rows.iter().enumerate().map(|(i, row)| {
            let selected = chosen == Some(*row);
            let (group, lane) = (row.group, row.lane);
            let label = config
                .group(group)
                .map(|d| lane_label(d, lane))
                .unwrap_or_default();
            div()
                .id(("lane-row", i))
                .absolute()
                .left(px(INSET))
                .w(px(width - 2.0 * INSET))
                .top(px(top + i as f32 * LANE_H))
                .h(px(LANE_H))
                .cursor_pointer()
                .when(selected, |d| {
                    d.border_2().border_color(primary).bg(primary.opacity(0.25))
                })
                .hover(|d| d.bg(gpui::white().opacity(0.08)))
                .child(
                    div()
                        .px_1()
                        .text_size(px(10.))
                        .text_color(gpui::white())
                        .truncate()
                        .child(label),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.lane_selected = Some((way, group, lane));
                        cx.notify();
                    }),
                )
        });
        let strip = div()
            .relative()
            .w(px(width))
            .h(px(height))
            .bg(ground)
            .child({
                let config = config.clone();
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let o = point(bounds.origin.x, bounds.origin.y);
                        paint_strip(&config, left_hand, width, o, window);
                    },
                )
                .absolute()
                .inset_0()
                .size_full()
            })
            .children(lane_rows)
            // A at the left (the way's start), B at the right (its end).
            .child(
                end_badge("A", END_A)
                    .absolute()
                    .left(px((INSET - BADGE) / 2.0))
                    .top(px((height - BADGE) / 2.0)),
            )
            .child(
                end_badge("B", END_B)
                    .absolute()
                    .right(px((INSET - BADGE) / 2.0))
                    .top(px((height - BADGE) / 2.0)),
            );

        let small_button = |id: (&'static str, usize), label: &'static str, on: bool| {
            div()
                .id(id)
                .min_w(px(26.))
                .h(px(24.))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .when(on, |d| d.bg(primary).text_color(primary_fg))
                .when(!on, |d| d.text_color(fg).hover(|d| d.bg(ground)))
                .child(label)
        };
        let caption = |text: gpui::SharedString| {
            div()
                .text_size(muted_text_size())
                .text_color(muted)
                .child(text)
        };

        // One row per direction: where it drives, its count and +/-.
        let direction_row = |group: Group, cx: &mut Context<Self>| {
            let forward = group == Group::Forward;
            let d = config.group(group)?;
            let towards = names_at(layer, layer_id, if forward { last } else { first }, way);
            let title = tr!("panel.lanes.towards", place = towards);
            // A → B for forward traffic, B → A for backward.
            let (from_badge, to_badge) = if forward {
                (end_badge("A", END_A), end_badge("B", END_B))
            } else {
                (end_badge("B", END_B), end_badge("A", END_A))
            };
            let k = if forward { 0 } else { 1 };
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(from_badge)
                    .child("→")
                    .child(to_badge)
                    .child(div().flex_1().min_w(px(0.)).truncate().child(title))
                    .child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(tr_count!("panel.lanes.count", d.count)),
                    )
                    .child(small_button(("lanes-remove", k), "−", false).on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.apply_lane_edit(LaneEdit::Count { group, add: false });
                            cx.notify();
                        }),
                    ))
                    .child(small_button(("lanes-add", k), "+", false).on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            let defaults = Category::ALL
                                .into_iter()
                                .zip(this.lane_defaults.iter().cloned())
                                .collect::<Vec<_>>();
                            this.apply_lane_edit(LaneEdit::AddLane { group, defaults });
                            cx.notify();
                        }),
                    )),
            )
        };
        let forward_row = direction_row(Group::Forward, cx);
        let backward_row = direction_row(Group::Backward, cx);

        // Shared centre turn lanes, on a road that goes both ways.
        let both_ways_row = (config.forward.is_some() && config.backward.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(tr!("panel.lanes.both_ways", count = config.both_ways.count)),
                )
                .child(small_button(("lanes-remove", 2), "−", false).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.apply_lane_edit(LaneEdit::Count {
                            group: Group::BothWays,
                            add: false,
                        });
                        cx.notify();
                    }),
                ))
                .child(small_button(("lanes-add", 2), "+", false).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let defaults = Category::ALL
                            .into_iter()
                            .zip(this.lane_defaults.iter().cloned())
                            .collect::<Vec<_>>();
                        this.apply_lane_edit(LaneEdit::AddLane {
                            group: Group::BothWays,
                            defaults,
                        });
                        cx.notify();
                    }),
                ))
        });

        // Arrow and access toggles for the chosen lane.
        let lane_editor = match chosen {
            Some(Row { group, lane }) => {
                let d = config.group(group);
                let current: Vec<Arrow> = d
                    .and_then(|d| d.arrows.as_ref())
                    .and_then(|a| a.get(lane))
                    .cloned()
                    .unwrap_or_default();
                let buttons = Arrow::EDITABLE.iter().enumerate().map(|(i, &arrow)| {
                    small_button(
                        ("lane-arrow", i),
                        arrow_symbol(arrow),
                        current.contains(&arrow),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.apply_lane_edit(LaneEdit::Arrow { group, lane, arrow });
                            cx.notify();
                        }),
                    )
                });
                let title = match group {
                    Group::Forward => tr!("panel.lanes.arrows_forward", lane = lane + 1),
                    Group::Backward => tr!("panel.lanes.arrows_backward", lane = lane + 1),
                    Group::BothWays => tr!("panel.lanes.arrows_both_ways", lane = lane + 1),
                };
                let count = d.map_or(0, |d| d.count as usize);
                let move_button = |id: usize, label: &'static str, left: bool, ok: bool| {
                    small_button(("lane-move", id), label, false)
                        .when(!ok, |b| b.opacity(0.4))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                if ok {
                                    let to = if left { lane - 1 } else { lane + 1 };
                                    this.apply_lane_edit(LaneEdit::Move { group, lane, left });
                                    this.lane_selected = Some((way, group, to));
                                    cx.notify();
                                }
                            }),
                        )
                };
                let mut col = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().child(caption(title)))
                            .child(move_button(0, "◀", true, lane > 0))
                            .child(move_button(1, "▶", false, lane + 1 < count)),
                    )
                    .child(div().flex().flex_wrap().gap_1().children(buttons));
                for (ci, cat) in Category::ALL.into_iter().enumerate() {
                    let now = d.map_or("", |d| d.attr(cat, lane)).to_string();
                    let name = match cat {
                        Category::Bicycle => tr!("panel.lanes.cat_bicycle"),
                        Category::Vehicle => tr!("panel.lanes.cat_vehicle"),
                        Category::Cycleway => tr!("panel.lanes.cat_cycleway"),
                        Category::Bus => tr!("panel.lanes.cat_bus"),
                    };
                    let buttons = cat.values().iter().enumerate().map(|(vi, &value)| {
                        small_button(("lane-attr", ci * 10 + vi), value, now == value)
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.apply_lane_edit(LaneEdit::Attr {
                                        group,
                                        lane,
                                        cat,
                                        value: value.to_string(),
                                    });
                                    cx.notify();
                                }),
                            )
                    });
                    col = col
                        .child(caption(name))
                        .child(div().flex().flex_wrap().gap_1().children(buttons));
                }
                col.into_any_element()
            }
            None => caption(tr!("panel.lanes.hint")).into_any_element(),
        };

        // Contraflow cycling on a oneway.
        let contraflow = (config.forward.is_some() != config.backward.is_some()).then(|| {
            let now = config.contraflow;
            let side = now.map_or(Side::Left, |c| c.side);
            let kinds = [
                (None, "–"),
                (Some(ContraflowKind::Lane), "lane"),
                (Some(ContraflowKind::Track), "track"),
                (Some(ContraflowKind::Shared), "shared"),
            ];
            let kind_buttons = kinds.into_iter().enumerate().map(|(i, (kind, label))| {
                small_button(("contraflow-kind", i), label, now.map(|c| c.kind) == kind)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.apply_lane_edit(LaneEdit::Contraflow(
                                kind.map(|kind| Contraflow { side, kind }),
                            ));
                            cx.notify();
                        }),
                    )
            });
            let side_buttons = [(Side::Left, "left"), (Side::Right, "right")]
                .into_iter()
                .enumerate()
                .map(|(i, (s, label))| {
                    small_button(("contraflow-side", i), label, now.is_some() && side == s)
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                if let Some(c) = now {
                                    this.apply_lane_edit(LaneEdit::Contraflow(Some(Contraflow {
                                        side: s,
                                        ..c
                                    })));
                                    cx.notify();
                                }
                            }),
                        )
                });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(caption(tr!("panel.lanes.contraflow")))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(kind_buttons)
                        .children(side_buttons),
                )
        });

        // On-street parking, per side of the way.
        let mut parking = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(caption(tr!("panel.lanes.parking")));
        for (si, (side, now)) in [
            (Side::Left, &config.parking_left),
            (Side::Right, &config.parking_right),
        ]
        .into_iter()
        .enumerate()
        {
            let name = match side {
                Side::Left => tr!("panel.lanes.parking_left"),
                Side::Right => tr!("panel.lanes.parking_right"),
            };
            let kinds = PARKING_KINDS.iter().enumerate().map(|(i, &kind)| {
                small_button(("parking-kind", si * 20 + i), kind, now.kind == kind).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.apply_lane_edit(LaneEdit::Parking {
                            side,
                            kind: kind.to_string(),
                            orientation: String::new(),
                        });
                        cx.notify();
                    }),
                )
            });
            let physical = parking_is_physical(&now.kind);
            let orientations = PARKING_ORIENTATIONS
                .iter()
                .enumerate()
                .filter(|_| physical)
                .map(|(i, &o)| {
                    small_button(("parking-orient", si * 20 + i), o, now.orientation == o)
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.apply_lane_edit(LaneEdit::Parking {
                                    side,
                                    kind: String::new(),
                                    orientation: o.to_string(),
                                });
                                cx.notify();
                            }),
                        )
                });
            parking = parking
                .child(caption(name))
                .child(div().flex().flex_wrap().gap_1().children(kinds))
                .child(div().flex().flex_wrap().gap_1().children(orientations));
        }

        // Display and clipboard tools.
        let tool = |id: usize, label: gpui::SharedString, on: bool| {
            div()
                .id(("lane-tool", id))
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .rounded_sm()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .when(on, |d| d.bg(primary).text_color(primary_fg))
                .when(!on, |d| d.text_color(fg).hover(|d| d.bg(ground)))
                .child(label)
        };
        let toolbar = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                tool(0, tr!("panel.lanes.left_hand"), left_hand).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.lane_left_hand = !this.lane_left_hand;
                        cx.notify();
                    }),
                ),
            )
            .child(tool(1, tr!("panel.lanes.copy"), false).on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.copy_lane_tags(cx);
                    cx.notify();
                }),
            ))
            .child(tool(2, tr!("panel.lanes.paste"), false).on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.paste_lane_tags(cx);
                    cx.notify();
                }),
            ));

        // Values new lanes start with, and a button to give them to all lanes.
        let mut defaults = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(caption(tr!("panel.lanes.defaults")));
        for (ci, cat) in Category::ALL.into_iter().enumerate() {
            let now = self.lane_defaults[ci].clone();
            let buttons = cat.values().iter().enumerate().map(|(vi, &value)| {
                small_button(("lane-default", ci * 10 + vi), value, now == value).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        let d = &mut this.lane_defaults[ci];
                        *d = if d == value {
                            String::new()
                        } else {
                            value.to_string()
                        };
                        cx.notify();
                    }),
                )
            });
            defaults = defaults.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(70.)).child(caption(match cat {
                        Category::Bicycle => "bicycle".into(),
                        Category::Vehicle => "vehicle".into(),
                        Category::Cycleway => "cycleway".into(),
                        Category::Bus => "psv".into(),
                    })))
                    .children(buttons),
            );
        }
        defaults = defaults.child(div().flex().child(
            tool(3, tr!("panel.lanes.apply_all"), false).on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    let values = Category::ALL
                        .into_iter()
                        .zip(this.lane_defaults.iter().cloned())
                        .collect();
                    this.apply_lane_edit(LaneEdit::SetAll(values));
                    cx.notify();
                }),
            ),
        ));

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(strip)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .child(toolbar)
                        .children(forward_row)
                        .children(backward_row)
                        .children(both_ways_row)
                        .child(div().pt_1().child(lane_editor))
                        .child(defaults)
                        .children(contraflow)
                        .child(parking),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(group: Group, lane: usize) -> Row {
        Row { group, lane }
    }

    #[test]
    fn rows_put_each_directions_leftmost_lane_by_the_centre_line() {
        let tags = Tags::from([
            ("highway", "primary"),
            ("lanes", "3"),
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
        ]);
        assert_eq!(
            rows(&LaneConfig::read(&tags), false),
            [
                r(Group::Backward, 0),
                r(Group::Forward, 0),
                r(Group::Forward, 1)
            ]
        );
        let tags = Tags::from([
            ("highway", "primary"),
            ("lanes", "5"),
            ("lanes:forward", "2"),
            ("lanes:backward", "2"),
            ("lanes:both_ways", "1"),
        ]);
        let rows = rows(&LaneConfig::read(&tags), false);
        assert_eq!(rows[0], r(Group::Backward, 1));
        assert_eq!(rows[2], r(Group::BothWays, 0));
        assert_eq!(rows[3], r(Group::Forward, 0));
    }

    #[test]
    fn left_hand_traffic_puts_the_forward_lanes_on_top() {
        let tags = Tags::from([
            ("highway", "primary"),
            ("lanes", "3"),
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
        ]);
        assert_eq!(
            rows(&LaneConfig::read(&tags), true),
            [
                r(Group::Forward, 0),
                r(Group::Forward, 1),
                r(Group::Backward, 0)
            ]
        );
    }

    #[test]
    fn parking_and_contraflow_make_the_strip_taller() {
        let plain = Tags::from([("highway", "primary"), ("oneway", "yes"), ("lanes", "1")]);
        let h0 = strip_height(&LaneConfig::read(&plain));
        let mut t = plain.clone();
        t.insert("parking:left", "lane");
        t.insert("oneway:bicycle", "no");
        t.insert("cycleway:right", "lane");
        let c = LaneConfig::read(&t);
        assert_eq!(strip_height(&c), h0 + PARK_H + CONTRA_H);
        assert_eq!(road_top(&c), MARGIN + PARK_H);
    }
}
