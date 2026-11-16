//! The Lanes side-panel section: the selected road drawn top-down as a strip
//! of lanes (as its `lanes*` and `turn:lanes*` tags describe them), with
//! buttons to add or remove lanes in each direction and to toggle the turn
//! arrows of the chosen lane. Each change is one undo step. The tag logic is
//! `osm_gpui::lanes`.

use gpui::{canvas, div, point, prelude::*, px, Context, MouseDownEvent, Pixels, Point, Window};
use gpui_component::ActiveTheme;

use osm_gpui::ids::{NodeId, WayId};
use osm_gpui::lanes::{is_road, Arrow, LaneConfig, LaneEdit};
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

/// A lane of the strip: its direction and index (left to right as that
/// direction's driver sees them), or a shared centre lane.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Row {
    Lane { forward: bool, lane: usize },
    BothWays,
}

/// The strip's rows top to bottom: backward lanes (their driver's leftmost
/// at the bottom, by the centre line), shared centre lanes, then forward
/// lanes (leftmost at the top). Forward traffic drives to the right.
fn rows(c: &LaneConfig) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(b) = &c.backward {
        rows.extend((0..b.count as usize).rev().map(|lane| Row::Lane {
            forward: false,
            lane,
        }));
    }
    rows.extend((0..c.both_ways).map(|_| Row::BothWays));
    if let Some(f) = &c.forward {
        rows.extend((0..f.count as usize).map(|lane| Row::Lane {
            forward: true,
            lane,
        }));
    }
    rows
}

fn paint_strip(c: &LaneConfig, width: f32, o: Point<Pixels>, window: &mut Window) {
    let rows = rows(c);
    let (x0, x1) = (6.0, width - 6.0);
    let top = MARGIN;
    let bottom = MARGIN + rows.len() as f32 * LANE_H;
    fill(
        window,
        o,
        &[V(x0, top), V(x1, top), V(x1, bottom), V(x0, bottom)],
        ASPHALT,
    );
    let y = |i: usize| MARGIN + i as f32 * LANE_H;
    for i in 1..rows.len() {
        let (a, b) = (rows[i - 1], rows[i]);
        // Lanes of the same kind are divided by a dashed line.
        let same_kind = match (a, b) {
            (Row::Lane { forward: f1, .. }, Row::Lane { forward: f2, .. }) => f1 == f2,
            (Row::BothWays, Row::BothWays) => true,
            _ => false,
        };
        if same_kind {
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
        match *row {
            Row::Lane { forward, lane } => {
                let dir = if forward { &c.forward } else { &c.backward };
                let arrows = dir
                    .as_ref()
                    .and_then(|d| d.arrows.as_ref())
                    .and_then(|a| a.get(lane));
                let Some(arrows) = arrows else { continue };
                // Rotated so the glyph's "up" is the driving direction.
                let (h, base) = if forward {
                    (V(1.0, 0.0), V(x1 - 16.0 - len, cy))
                } else {
                    (V(-1.0, 0.0), V(x0 + 16.0 + len, cy))
                };
                lane_glyph(window, o, base, h, len, arrows);
            }
            Row::BothWays => {
                // Both-ways turn lane: left arrows facing each way.
                for (h, base) in [
                    (V(1.0, 0.0), V(width / 2.0 - 30.0, cy)),
                    (V(-1.0, 0.0), V(width / 2.0 + 30.0, cy)),
                ] {
                    lane_glyph(window, o, base, h, len, &[Arrow::Left]);
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
        let rows = rows(&config);
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
            .map(|(_, forward, lane)| Row::Lane { forward, lane })
            .filter(|r| rows.contains(r));

        let width = (self.side_panel_width - 1.0).max(200.0);
        let height = rows.len() as f32 * LANE_H + 2.0 * MARGIN;
        let lane_rows = rows.iter().enumerate().filter_map(|(i, row)| {
            let Row::Lane { forward, lane } = *row else {
                return None;
            };
            let selected = chosen == Some(*row);
            Some(
                div()
                    .id(("lane-row", i))
                    .absolute()
                    .left(px(6.0))
                    .w(px(width - 12.0))
                    .top(px(MARGIN + i as f32 * LANE_H))
                    .h(px(LANE_H))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.border_2().border_color(primary).bg(primary.opacity(0.25))
                    })
                    .hover(|d| d.bg(gpui::white().opacity(0.08)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.lane_selected = Some((way, forward, lane));
                            cx.notify();
                        }),
                    ),
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
                        paint_strip(&config, width, o, window);
                    },
                )
                .absolute()
                .inset_0()
                .size_full()
            })
            .children(lane_rows);

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

        // One row per direction: where it drives, its count and +/-.
        let direction_row = |forward: bool, cx: &mut Context<Self>| {
            let d = if forward {
                config.forward.as_ref()
            } else {
                config.backward.as_ref()
            }?;
            let towards = names_at(layer, layer_id, if forward { last } else { first }, way);
            let title = if forward {
                tr!("panel.lanes.forward", place = towards)
            } else {
                tr!("panel.lanes.backward", place = towards)
            };
            let k = if forward { 0 } else { 1 };
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
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
                            this.apply_lane_edit(LaneEdit::Count {
                                forward,
                                add: false,
                            });
                            cx.notify();
                        }),
                    ))
                    .child(small_button(("lanes-add", k), "+", false).on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.apply_lane_edit(LaneEdit::Count { forward, add: true });
                            cx.notify();
                        }),
                    )),
            )
        };
        let forward_row = direction_row(true, cx);
        let backward_row = direction_row(false, cx);

        // Arrow toggles for the chosen lane.
        let arrow_editor = match chosen {
            Some(Row::Lane { forward, lane }) => {
                let d = if forward {
                    config.forward.as_ref()
                } else {
                    config.backward.as_ref()
                };
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
                            this.apply_lane_edit(LaneEdit::Arrow {
                                forward,
                                lane,
                                arrow,
                            });
                            cx.notify();
                        }),
                    )
                });
                let title = if forward {
                    tr!("panel.lanes.arrows_forward", lane = lane + 1)
                } else {
                    tr!("panel.lanes.arrows_backward", lane = lane + 1)
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(title),
                    )
                    .child(div().flex().flex_wrap().gap_1().children(buttons))
                    .into_any_element()
            }
            _ => div()
                .text_size(muted_text_size())
                .text_color(muted)
                .child(tr!("panel.lanes.hint"))
                .into_any_element(),
        };

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
                        .children(forward_row)
                        .children(backward_row)
                        .when(config.both_ways > 0, |d| {
                            d.child(
                                div()
                                    .text_size(muted_text_size())
                                    .text_color(muted)
                                    .child(tr!("panel.lanes.both_ways", count = config.both_ways)),
                            )
                        })
                        .child(div().pt_1().child(arrow_editor)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_put_each_directions_leftmost_lane_by_the_centre_line() {
        let tags = Tags::from([
            ("highway", "primary"),
            ("lanes", "3"),
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
        ]);
        let r = rows(&LaneConfig::read(&tags));
        assert_eq!(
            r,
            [
                Row::Lane {
                    forward: false,
                    lane: 0
                },
                Row::Lane {
                    forward: true,
                    lane: 0
                },
                Row::Lane {
                    forward: true,
                    lane: 1
                },
            ]
        );
        let tags = Tags::from([
            ("highway", "primary"),
            ("lanes", "5"),
            ("lanes:forward", "2"),
            ("lanes:backward", "2"),
            ("lanes:both_ways", "1"),
        ]);
        let r = rows(&LaneConfig::read(&tags));
        assert_eq!(
            r[0],
            Row::Lane {
                forward: false,
                lane: 1
            }
        );
        assert_eq!(r[2], Row::BothWays);
        assert_eq!(
            r[3],
            Row::Lane {
                forward: true,
                lane: 0
            }
        );
    }
}
