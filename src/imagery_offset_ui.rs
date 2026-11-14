//! The "Adjust imagery offset" mode: shifting the topmost visible imagery
//! layer by dragging the map, the arrow keys or typed metres, with a small
//! floating panel (value, East/North fields, Reset, Done). The maths and the
//! remembered offsets are in `osm_gpui::imagery_offset`.
//!
//! While the mode is on, a left-drag moves the imagery instead of selecting or
//! panning; Space-drag, right/middle-drag, scroll and pinch still move the
//! map. Arrow keys nudge by one screen pixel's worth of metres (ten with
//! Shift), Escape goes back to the offset the mode was entered with, and
//! Enter or Done keeps the offset.

use gpui::{
    div, prelude::*, px, Action, AnyElement, Context, Entity, Focusable, MouseButton, Pixels,
    Point, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme, Sizable,
};
use osm_gpui::imagery_offset::{self, ImageryOffset, Nudge, OffsetSession};
use osm_gpui::layers::{LayerId, LayerKind};
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{EditMode, MapViewer, SetMode};

gpui::actions!(osm_gpui, [AdjustImageryOffset, ResetImageryOffset]);

/// Adjust the imagery offset of the layer at `index` (its Layers-list
/// context menu).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdjustLayerImageryOffset {
    pub index: usize,
}

/// Reset the imagery offset of the layer at `index`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetLayerImageryOffset {
    pub index: usize,
}

/// Set the offset (metres) of the layer being adjusted, else the topmost
/// visible imagery layer. Shown in no menu: for the script harness
/// (`action SetImageryOffset {"east":3.2,"north":-1.75}`).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetImageryOffset {
    pub east: f64,
    pub north: f64,
}

/// The adjust mode's state, `Some` on `MapViewer` while the mode is on.
pub(crate) struct OffsetAdjust {
    layer: LayerId,
    session: OffsetSession,
    /// Last pointer position (map-local) of a left-drag in progress.
    drag_last: Option<Point<Pixels>>,
    east: Entity<InputState>,
    north: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl OffsetAdjust {
    pub(crate) fn dragging(&self) -> bool {
        self.drag_last.is_some()
    }
}

impl MapViewer {
    /// The imagery layer a request without a layer means: the topmost (drawn
    /// last) visible one.
    pub(crate) fn topmost_imagery_layer(&self) -> Option<LayerId> {
        self.layer_manager
            .layers()
            .iter()
            .rev()
            .find(|layer| layer.is_visible() && layer.kind() == LayerKind::Tile)
            .map(|layer| layer.id())
    }

    /// The offset being shown: the live one while adjusting, else the
    /// topmost visible imagery layer's.
    pub(crate) fn current_imagery_offset(&self) -> Option<ImageryOffset> {
        if let Some(adjust) = &self.offset_adjust {
            return Some(adjust.session.current());
        }
        let id = self.topmost_imagery_layer()?;
        Some(
            self.layer_manager
                .find_layer(id)?
                .as_imagery()?
                .imagery_offset(),
        )
    }

    fn set_layer_offset(&mut self, layer: LayerId, offset: ImageryOffset) {
        if let Some(layer) = self.layer_manager.find_layer_mut(layer) {
            if let Some(imagery) = layer.as_imagery_mut() {
                imagery.set_imagery_offset(offset);
            }
        }
    }

    /// The offset to start a new layer of `imagery_id` with: the remembered
    /// one if it was made within 10 km of the view centre, else none. (A
    /// remembered offset elsewhere is left in the store untouched.)
    pub(crate) fn saved_imagery_offset(&self, imagery_id: &str) -> ImageryOffset {
        let (lat, lon) = self.offset_anchor();
        imagery_offset::lookup(imagery_id, lat, lon).unwrap_or(ImageryOffset::ZERO)
    }

    /// Where the view is looking, for remembering an offset "here".
    fn offset_anchor(&self) -> (f64, f64) {
        self.viewport.center()
    }

    /// Remember `offset` as the one for `layer`'s imagery around the view.
    fn remember_layer_offset(&self, layer: LayerId, offset: ImageryOffset) {
        let Some(id) = self
            .layer_manager
            .find_layer(layer)
            .and_then(|l| l.as_imagery().map(|i| i.imagery_id().to_string()))
        else {
            return;
        };
        let (lat, lon) = self.offset_anchor();
        imagery_offset::remember(&id, offset, lat, lon);
    }

    /// Enter the mode for `layer`.
    pub(crate) fn begin_offset_adjust(
        &mut self,
        layer: LayerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .layer_manager
            .find_layer(layer)
            .and_then(|l| l.as_imagery().map(|i| i.imagery_offset()))
        else {
            return;
        };
        // Leave any other mode (or an adjustment of another layer) first.
        self.on_set_mode(
            &SetMode {
                mode: EditMode::Select,
            },
            window,
            cx,
        );
        let make_input =
            |window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx));
        let east = make_input(window, cx);
        let north = make_input(window, cx);
        let subscriptions = [&east, &north]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, input, event, window, cx| {
                    this.on_offset_input_event(input, event, window, cx);
                })
            })
            .collect();
        self.offset_adjust = Some(OffsetAdjust {
            layer,
            session: OffsetSession::new(entry),
            drag_last: None,
            east,
            north,
            _subscriptions: subscriptions,
        });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Leave the mode. `keep` keeps the live offset and remembers it for the
    /// imagery source around the view; otherwise the layer goes back to the
    /// offset it had on entry.
    pub(crate) fn finish_offset_adjust(
        &mut self,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(adjust) = self.offset_adjust.take() else {
            return;
        };
        if keep {
            let offset = adjust.session.current();
            self.set_layer_offset(adjust.layer, offset);
            if offset != adjust.session.entry() {
                self.remember_layer_offset(adjust.layer, offset);
            }
        } else {
            let mut session = adjust.session;
            let entry = session.cancel();
            self.set_layer_offset(adjust.layer, entry);
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Forget a session whose layer has been deleted (called every render).
    pub(crate) fn drop_stale_offset_adjust(&mut self) {
        if let Some(adjust) = &self.offset_adjust {
            if self.layer_manager.find_layer(adjust.layer).is_none() {
                self.offset_adjust = None;
            }
        }
    }

    /// Apply the session's live offset to its layer.
    fn apply_session(&mut self) {
        if let Some(adjust) = &self.offset_adjust {
            let (layer, offset) = (adjust.layer, adjust.session.current());
            self.set_layer_offset(layer, offset);
        }
    }

    pub(crate) fn on_adjust_imagery_offset(
        &mut self,
        _: &AdjustImageryOffset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.topmost_imagery_layer() {
            Some(layer) => self.begin_offset_adjust(layer, window, cx),
            None => {
                self.set_status(tr!("imagery_offset.no_layer"));
                cx.notify();
            }
        }
    }

    pub(crate) fn on_adjust_layer_imagery_offset(
        &mut self,
        action: &AdjustLayerImageryOffset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layer = self
            .layer_manager
            .layers()
            .get(action.index)
            .filter(|l| l.kind() == LayerKind::Tile)
            .map(|l| l.id());
        if let Some(layer) = layer {
            self.begin_offset_adjust(layer, window, cx);
        }
    }

    /// Put `layer` back to no offset, and forget the remembered offset
    /// around here.
    fn reset_layer_offset(&mut self, layer: LayerId, cx: &mut Context<Self>) {
        if let Some(adjust) = self.offset_adjust.as_mut().filter(|a| a.layer == layer) {
            // Mid-adjustment a reset is part of the session (Escape undoes it).
            adjust.session.reset();
            self.apply_session();
        } else {
            self.set_layer_offset(layer, ImageryOffset::ZERO);
            self.remember_layer_offset(layer, ImageryOffset::ZERO);
            self.set_status(tr!("imagery_offset.reset_done"));
        }
        cx.notify();
    }

    pub(crate) fn on_reset_imagery_offset(
        &mut self,
        _: &ResetImageryOffset,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layer = self
            .offset_adjust
            .as_ref()
            .map(|a| a.layer)
            .or_else(|| self.topmost_imagery_layer());
        if let Some(layer) = layer {
            self.reset_layer_offset(layer, cx);
        }
    }

    pub(crate) fn on_reset_layer_imagery_offset(
        &mut self,
        action: &ResetLayerImageryOffset,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layer = self
            .layer_manager
            .layers()
            .get(action.index)
            .filter(|l| l.kind() == LayerKind::Tile)
            .map(|l| l.id());
        if let Some(layer) = layer {
            self.reset_layer_offset(layer, cx);
        }
    }

    /// Script/harness: set the offset directly.
    pub(crate) fn on_set_imagery_offset(
        &mut self,
        action: &SetImageryOffset,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = ImageryOffset::new(action.east, action.north);
        if let Some(adjust) = self.offset_adjust.as_mut() {
            adjust.session.set(offset);
            self.apply_session();
        } else if let Some(layer) = self.topmost_imagery_layer() {
            self.set_layer_offset(layer, offset);
            self.remember_layer_offset(layer, offset);
        }
        cx.notify();
    }

    /// Left press (map-local `position`) while adjusting: start moving the
    /// imagery. Returns whether the mode took the press.
    pub(crate) fn offset_adjust_mouse_down(&mut self, position: Point<Pixels>) -> bool {
        match self.offset_adjust.as_mut() {
            Some(adjust) => {
                adjust.drag_last = Some(position);
                true
            }
            None => false,
        }
    }

    /// Pointer move while adjusting. Returns whether it moved the imagery
    /// (and so is not also a map pan).
    pub(crate) fn offset_adjust_mouse_move(
        &mut self,
        position: Point<Pixels>,
        left_pressed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let (zoom, lat) = (self.viewport.zoom_level(), self.viewport.center().0);
        let Some(adjust) = self.offset_adjust.as_mut() else {
            return false;
        };
        let Some(last) = adjust.drag_last else {
            return false;
        };
        if !left_pressed {
            // The release happened outside the map.
            adjust.drag_last = None;
            return false;
        }
        let delta = position - last;
        adjust
            .session
            .drag_by_pixels(delta.x.as_f32() as f64, delta.y.as_f32() as f64, zoom, lat);
        adjust.drag_last = Some(position);
        self.apply_session();
        cx.notify();
        true
    }

    /// Left release while adjusting. Returns whether it ended an imagery drag.
    pub(crate) fn offset_adjust_mouse_up(&mut self) -> bool {
        self.offset_adjust
            .as_mut()
            .is_some_and(|adjust| adjust.drag_last.take().is_some())
    }

    /// Whether one of the panel's metre fields has keyboard focus, so the
    /// map must leave the key to it.
    pub(crate) fn offset_field_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.offset_adjust.as_ref().is_some_and(|adjust| {
            adjust.east.focus_handle(cx).is_focused(window)
                || adjust.north.focus_handle(cx).is_focused(window)
        })
    }

    /// A key pressed on the map while adjusting. Returns whether it was
    /// handled (arrows nudge, Escape cancels, Enter keeps).
    pub(crate) fn offset_adjust_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (zoom, lat) = (self.viewport.zoom_level(), self.viewport.center().0);
        let Some(adjust) = self.offset_adjust.as_mut() else {
            return false;
        };
        if let Some(dir) = Nudge::from_key(key) {
            adjust.session.nudge(dir, shift, zoom, lat);
            self.apply_session();
        } else if key == "escape" {
            self.finish_offset_adjust(false, window, cx);
        } else if key == "enter" {
            self.finish_offset_adjust(true, window, cx);
        } else {
            return false;
        }
        cx.notify();
        true
    }

    fn on_offset_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(adjust) = self.offset_adjust.as_ref() else {
            return;
        };
        let is_east = *input == adjust.east;
        if !is_east && *input != adjust.north {
            return;
        }
        match event {
            InputEvent::PressEnter { .. } => {
                self.commit_offset_field(input, is_east, cx);
                window.focus(&self.focus_handle, cx);
            }
            InputEvent::Blur => self.commit_offset_field(input, is_east, cx),
            _ => return,
        }
        cx.notify();
    }

    /// Take the typed metres of one field, if they parse. Text that does not
    /// parse is replaced by the current value on the next render.
    fn commit_offset_field(
        &mut self,
        input: &Entity<InputState>,
        is_east: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(value) = imagery_offset::parse_metres(input.read(cx).value().as_ref()) else {
            return;
        };
        let Some(adjust) = self.offset_adjust.as_mut() else {
            return;
        };
        let current = adjust.session.current();
        let next = if is_east {
            ImageryOffset::new(value, current.north_m)
        } else {
            ImageryOffset::new(current.east_m, value)
        };
        if next != current {
            adjust.session.set(next);
            self.apply_session();
        }
    }

    /// The floating panel at the top of the map while adjusting.
    pub(crate) fn render_imagery_offset_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let adjust = self.offset_adjust.as_ref()?;
        let layer_name = self
            .layer_manager
            .find_layer(adjust.layer)
            .map(|l| l.name().to_string())
            .unwrap_or_default();
        let current = adjust.session.current();

        // Keep the fields in step with drags and nudges, but never rewrite
        // one while it is being typed in.
        for (input, value) in [
            (&adjust.east, current.east_m),
            (&adjust.north, current.north_m),
        ] {
            let text = imagery_offset::format_metres(value);
            let focused = input.focus_handle(cx).is_focused(window);
            if !focused && input.read(cx).value().as_ref() != text.as_str() {
                input.update(cx, |state, cx| state.set_value(text, window, cx));
            }
        }

        let muted = cx.theme().muted_foreground;
        let value_line = tr!(
            "imagery_offset.value",
            east = imagery_offset::format_metres(current.east_m),
            north = imagery_offset::format_metres(current.north_m)
        );
        let field = |label, input: &Entity<InputState>| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(div().text_color(muted).child(label))
                .child(div().w(px(84.0)).child(Input::new(input).small()))
                .child(div().text_color(muted).child(tr!("imagery_offset.metres")))
        };

        let panel = v_flex()
            .id("imagery-offset-panel")
            .w(px(380.0))
            .p_3()
            .gap_2()
            .bg(cx.theme().popover.opacity(0.96))
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .text_color(cx.theme().popover_foreground)
            // Escape in a field cancels the mode. Other keys must keep
            // propagating (typed text is inserted after key-down handlers
            // run, and only if none stopped the event); the map's own key
            // handler skips them while a field has focus
            // (`offset_field_focused`).
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.finish_offset_adjust(false, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr!("imagery_offset.title")),
                    )
                    .child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(layer_name),
                    ),
            )
            .child(
                div()
                    .id("imagery-offset-value")
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(value_line),
            )
            .child(
                h_flex()
                    .gap_4()
                    .child(field(tr!("imagery_offset.east"), &adjust.east))
                    .child(field(tr!("imagery_offset.north"), &adjust.north)),
            )
            .child(
                div()
                    .text_size(muted_text_size())
                    .text_color(muted)
                    .line_height(gpui::relative(1.4))
                    .child(tr!("imagery_offset.hint")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("imagery-offset-reset")
                            .label(tr!("imagery_offset.reset"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let layer = this.offset_adjust.as_ref().map(|a| a.layer);
                                if let Some(layer) = layer {
                                    this.reset_layer_offset(layer, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("imagery-offset-done")
                            .primary()
                            .label(tr!("imagery_offset.done"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_offset_adjust(true, window, cx);
                            })),
                    ),
            );

        // A full-width, non-interactive row centres the panel without
        // covering the map around it.
        Some(
            div()
                .absolute()
                .top(px(12.0))
                .left_0()
                .right_0()
                .flex()
                .flex_row()
                .justify_center()
                .child(panel)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, TestAppContext};
    use osm_gpui::layers::tile_layer::TileLayer;

    /// A window whose view has one OSM Carto layer and a known viewport.
    fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                view.viewport.pan_to(40.7125, -74.0056);
                view.viewport.set_zoom(19.0);
                let id = view.layer_manager.alloc_id();
                let layer = TileLayer::new(id, view.tile_cache.clone());
                view.layer_manager.add_layer(Box::new(layer));
            })
            .unwrap();
        window
    }

    fn offset_of(view: &MapViewer) -> ImageryOffset {
        view.current_imagery_offset().unwrap()
    }

    fn layer_offset(view: &MapViewer) -> ImageryOffset {
        let id = view.topmost_imagery_layer().unwrap();
        view.layer_manager
            .find_layer(id)
            .unwrap()
            .as_imagery()
            .unwrap()
            .imagery_offset()
    }

    #[gpui::test]
    fn dragging_moves_the_imagery_by_the_dragged_pixels_in_metres(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                assert!(view.offset_adjust.is_some());
                assert!(view.offset_adjust_mouse_down(point(px(100.0), px(100.0))));
                assert!(view.offset_adjust_mouse_move(point(px(160.0), px(70.0)), true, cx));
                assert!(view.offset_adjust_mouse_up());

                let mpp = imagery_offset::metres_per_pixel(19.0, 40.7125);
                let got = layer_offset(view);
                assert!((got.east_m - 60.0 * mpp).abs() < 1e-6, "{got:?}");
                assert!((got.north_m - 30.0 * mpp).abs() < 1e-6, "{got:?}");
            })
            .unwrap();
    }

    #[gpui::test]
    fn escape_restores_the_entry_offset_and_enter_keeps(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                // Enter the mode with a non-zero offset already on the layer.
                let layer = view.topmost_imagery_layer().unwrap();
                view.set_layer_offset(layer, ImageryOffset::new(2.0, -1.0));
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);

                assert!(view.offset_adjust_key("right", true, window, cx));
                assert!(view.offset_adjust_key("up", false, window, cx));
                assert_ne!(layer_offset(view), ImageryOffset::new(2.0, -1.0));

                assert!(view.offset_adjust_key("escape", false, window, cx));
                assert!(view.offset_adjust.is_none());
                assert_eq!(layer_offset(view), ImageryOffset::new(2.0, -1.0));

                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                view.on_set_imagery_offset(
                    &SetImageryOffset {
                        east: 3.2,
                        north: -1.75,
                    },
                    window,
                    cx,
                );
                assert_eq!(offset_of(view), ImageryOffset::new(3.2, -1.75));
                assert!(view.offset_adjust_key("enter", false, window, cx));
                assert!(view.offset_adjust.is_none());
                assert_eq!(layer_offset(view), ImageryOffset::new(3.2, -1.75));
            })
            .unwrap();
    }

    #[gpui::test]
    fn arrow_keys_nudge_one_pixel_and_ten_with_shift(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                let mpp = imagery_offset::metres_per_pixel(19.0, 40.7125);
                view.offset_adjust_key("left", false, window, cx);
                assert!((layer_offset(view).east_m + mpp).abs() < 1e-9);
                view.offset_adjust_key("down", true, window, cx);
                assert!((layer_offset(view).north_m + 10.0 * mpp).abs() < 1e-9);
                // Keys that are not the mode's are left to the map.
                assert!(!view.offset_adjust_key("minus", false, window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn pointer_and_keys_do_nothing_when_not_adjusting(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                assert!(!view.offset_adjust_mouse_down(point(px(1.0), px(1.0))));
                assert!(!view.offset_adjust_mouse_move(point(px(9.0), px(9.0)), true, cx));
                assert!(!view.offset_adjust_mouse_up());
                assert!(!view.offset_adjust_key("right", false, window, cx));
                assert_eq!(layer_offset(view), ImageryOffset::ZERO);
            })
            .unwrap();
    }

    #[gpui::test]
    fn reset_clears_the_offset_and_without_imagery_nothing_starts(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                let layer = view.topmost_imagery_layer().unwrap();
                view.set_layer_offset(layer, ImageryOffset::new(5.0, 5.0));
                view.on_reset_imagery_offset(&ResetImageryOffset, window, cx);
                assert!(layer_offset(view).is_zero());

                // Hidden imagery is not a target.
                view.layer_manager
                    .find_layer_mut(layer)
                    .unwrap()
                    .set_visible(false);
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                assert!(view.offset_adjust.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_the_adjusted_layer_ends_the_mode(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                view.layer_manager.remove_at(0);
                view.drop_stale_offset_adjust();
                assert!(view.offset_adjust.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn picking_another_mode_keeps_the_offset_and_leaves_the_mode(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.on_adjust_imagery_offset(&AdjustImageryOffset, window, cx);
                view.on_set_imagery_offset(
                    &SetImageryOffset {
                        east: 1.0,
                        north: 2.0,
                    },
                    window,
                    cx,
                );
                view.on_set_mode(
                    &SetMode {
                        mode: EditMode::Select,
                    },
                    window,
                    cx,
                );
                assert!(view.offset_adjust.is_none());
                assert_eq!(layer_offset(view), ImageryOffset::new(1.0, 2.0));
            })
            .unwrap();
    }
}
