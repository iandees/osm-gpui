//! Laying a photo or scanned map over the map and stretching it to fit the
//! existing features (georeferencing it): the "Add Photo or Scanned Map…"
//! command, and the placement mode with its four corner handles and floating
//! panel. The maths is in `osm_gpui::georeference` and the layer in
//! `osm_gpui::layers::image_overlay`.
//!
//! While the mode is on, a left-drag on a corner handle pulls that corner
//! (Shift keeps the picture's shape: it scales and rotates about the opposite
//! corner), a drag inside the picture moves it, and arrow keys nudge it by a
//! screen pixel (ten with Shift). Space-drag, right/middle-drag, scroll and
//! pinch still move the map. Escape puts the picture back as it was when the
//! mode was entered; Enter or Done keeps it.

use std::path::PathBuf;

use gpui::{
    canvas, div, point, prelude::*, px, rgba, Action, AnyElement, Context, Modifiers, MouseButton,
    PathBuilder, Pixels, Point, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme,
};
use image::RgbaImage;
use osm_gpui::coordinates::Mercator;
use osm_gpui::georeference::{self, Corners};
use osm_gpui::layers::image_overlay::{ImageOverlayLayer, Quality};
use osm_gpui::layers::{LayerId, LayerKind, MapLayer as _};
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{EditMode, MapViewer, SetMode};

gpui::actions!(osm_gpui, [AddPhotoOverlay]);

/// Adjust the placement of the photo layer at `index` (its Layers-list
/// context menu).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdjustLayerPhoto {
    pub index: usize,
}

/// Add the picture at `path` as a photo layer, as the file picker does after
/// a choice. Shown in no menu: for the script harness
/// (`action OpenPhotoOverlay {"path":"/tmp/scan.png"}`).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenPhotoOverlay {
    pub path: String,
}

/// How far (pixels) from a corner a press still grabs it.
const HANDLE_GRAB_PX: f32 = 14.0;
/// Side of the square drawn at a corner.
const HANDLE_SIZE_PX: f32 = 12.0;
/// How much of the view's shorter side a new picture covers.
const INITIAL_COVERAGE: f64 = 0.6;
/// Opacity step of the panel's − and + buttons.
const OPACITY_STEP: f32 = 0.1;

#[derive(Clone, Copy)]
enum Drag {
    /// Moving the whole picture; the pointer's last position.
    Move { last: Mercator },
    /// Pulling corner `index`; `grab` is the corner's offset from the pointer
    /// when it was pressed, so the corner does not jump to the pointer.
    Corner { index: usize, grab: (f64, f64) },
}

/// The placement mode's state, `Some` on `MapViewer` while the mode is on.
pub(crate) struct PhotoAdjust {
    layer: LayerId,
    entry_corners: Corners,
    entry_opacity: f32,
    drag: Option<Drag>,
}

/// Decode a picture file for the map, shrinking it when it is huge.
fn load_photo(path: &std::path::Path) -> Result<RgbaImage, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    Ok(osm_gpui::layers::image_overlay::limit_source(
        img.to_rgba8(),
    ))
}

impl MapViewer {
    fn photo_layer(&self, id: LayerId) -> Option<&ImageOverlayLayer> {
        self.layer_manager.find_layer(id)?.as_image_overlay()
    }

    fn photo_layer_mut(&mut self, id: LayerId) -> Option<&mut ImageOverlayLayer> {
        self.layer_manager
            .find_layer_mut(id)?
            .as_image_overlay_mut()
    }

    /// File > Imagery > Add Photo or Scanned Map…: pick a picture, then add
    /// it and start placing it.
    pub(crate) fn start_add_photo_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter(
                    tr!("menu.open_dialog.image_files").as_ref(),
                    &["png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp"],
                )
                .add_filter(tr!("menu.open_dialog.all_files").as_ref(), &["*"])
                .set_title(tr!("photo_overlay.open_title").as_ref())
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let _ = this.update_in(cx, |this, window, cx| {
                this.add_photo_overlay(path, window, cx)
            });
        })
        .detach();
    }

    pub(crate) fn on_add_photo_overlay(
        &mut self,
        _: &AddPhotoOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_add_photo_overlay(window, cx);
    }

    pub(crate) fn on_open_photo_overlay(
        &mut self,
        action: &OpenPhotoOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_photo_overlay(PathBuf::from(&action.path), window, cx);
    }

    /// Read the picture at `path` in the background, add it as a layer in
    /// the middle of the view and start placing it.
    pub(crate) fn add_photo_overlay(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!("photo_overlay.title").to_string());
        self.set_status(tr!("photo_overlay.loading", name = name));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load_photo(&path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(img) => this.finish_add_photo(name, img, window, cx),
                Err(error) => {
                    this.set_status(tr!("photo_overlay.load_failed", name = name, error = error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn finish_add_photo(
        &mut self,
        name: String,
        img: RgbaImage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.viewport.mercator_view_rect();
        let (view_w, view_h) = (view.max_x - view.min_x, view.max_y - view.min_y);
        let centre = Mercator::new(
            (view.min_x + view.max_x) / 2.0,
            (view.min_y + view.max_y) / 2.0,
        );
        let aspect = img.width() as f64 / img.height().max(1) as f64;
        let width = (INITIAL_COVERAGE * view_w).min(INITIAL_COVERAGE * view_h * aspect);
        let corners = georeference::initial_corners((img.width(), img.height()), centre, width);
        let name = self.layer_manager.unique_name(&name);
        let id = self.layer_manager.alloc_id();
        self.set_status(tr!("photo_overlay.added", name = name));
        self.layer_manager
            .add_layer(Box::new(ImageOverlayLayer::new(id, name, img, corners)));
        self.begin_photo_adjust(id, window, cx);
    }

    /// Enter the placement mode for `layer`.
    pub(crate) fn begin_photo_adjust(
        &mut self,
        layer: LayerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((entry_corners, entry_opacity)) =
            self.photo_layer(layer).map(|l| (l.corners(), l.opacity()))
        else {
            return;
        };
        // Leave any other mode (and a placement of another photo) first.
        self.on_set_mode(
            &SetMode {
                mode: EditMode::Select,
            },
            window,
            cx,
        );
        self.photo_adjust = Some(PhotoAdjust {
            layer,
            entry_corners,
            entry_opacity,
            drag: None,
        });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Leave the mode. `keep` keeps the picture where it is; otherwise it
    /// goes back to where (and how opaque) it was on entry.
    pub(crate) fn finish_photo_adjust(
        &mut self,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(adjust) = self.photo_adjust.take() else {
            return;
        };
        if let Some(layer) = self.photo_layer_mut(adjust.layer) {
            if keep {
                layer.settle();
            } else {
                layer.set_corners(adjust.entry_corners, Quality::Full);
                layer.set_opacity(adjust.entry_opacity);
            }
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Forget a session whose layer has been deleted (called every render).
    pub(crate) fn drop_stale_photo_adjust(&mut self) {
        if let Some(adjust) = &self.photo_adjust {
            if self.photo_layer(adjust.layer).is_none() {
                self.photo_adjust = None;
            }
        }
    }

    pub(crate) fn on_adjust_layer_photo(
        &mut self,
        action: &AdjustLayerPhoto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layer = self
            .layer_manager
            .layers()
            .get(action.index)
            .filter(|l| l.kind() == LayerKind::ImageOverlay)
            .map(|l| l.id());
        if let Some(layer) = layer {
            self.begin_photo_adjust(layer, window, cx);
        }
    }

    /// Left press (map-local `position`) while placing: grab the nearest
    /// corner handle, else the picture. Returns whether the mode took the
    /// press (it takes every press, so none selects or pans).
    pub(crate) fn photo_adjust_mouse_down(&mut self, position: Point<Pixels>) -> bool {
        let Some(adjust) = self.photo_adjust.as_ref() else {
            return false;
        };
        let Some(corners) = self.photo_layer(adjust.layer).map(|l| l.corners()) else {
            return true;
        };
        let here = self.viewport.screen_to_mercator(position);
        let nearest = (0..4)
            .map(|i| {
                let at = self.viewport.mercator_to_screen(corners[i]);
                let (dx, dy) = ((at.x - position.x).as_f32(), (at.y - position.y).as_f32());
                (i, dx.hypot(dy))
            })
            .filter(|(_, d)| *d <= HANDLE_GRAB_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let drag = match nearest {
            Some((index, _)) => Some(Drag::Corner {
                index,
                grab: (corners[index].x - here.x, corners[index].y - here.y),
            }),
            None if georeference::contains(&corners, here) => Some(Drag::Move { last: here }),
            None => None,
        };
        if let Some(adjust) = self.photo_adjust.as_mut() {
            adjust.drag = drag;
        }
        true
    }

    /// Pointer move while placing. Returns whether it moved the picture (and
    /// so is not also a map pan).
    pub(crate) fn photo_adjust_mouse_move(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        left_pressed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(adjust) = self.photo_adjust.as_mut() else {
            return false;
        };
        let Some(drag) = adjust.drag else {
            return false;
        };
        let layer_id = adjust.layer;
        if !left_pressed {
            // The release happened outside the map.
            adjust.drag = None;
            if let Some(layer) = self.photo_layer_mut(layer_id) {
                layer.settle();
            }
            cx.notify();
            return false;
        }
        let here = self.viewport.screen_to_mercator(position);
        let Some(corners) = self.photo_layer(layer_id).map(|l| l.corners()) else {
            return true;
        };
        let (next, drag) = match drag {
            Drag::Move { last } => (
                georeference::translated(&corners, here.x - last.x, here.y - last.y),
                Drag::Move { last: here },
            ),
            Drag::Corner { index, grab } => (
                georeference::drag_corner(
                    &corners,
                    index,
                    Mercator::new(here.x + grab.0, here.y + grab.1),
                    modifiers.shift,
                ),
                Drag::Corner { index, grab },
            ),
        };
        if let Some(layer) = self.photo_layer_mut(layer_id) {
            layer.set_corners(next, Quality::Preview);
        }
        if let Some(adjust) = self.photo_adjust.as_mut() {
            adjust.drag = Some(drag);
        }
        cx.notify();
        true
    }

    /// Left release while placing: the picture is rebuilt at full quality.
    /// Returns whether it ended a drag.
    pub(crate) fn photo_adjust_mouse_up(&mut self) -> bool {
        let Some(adjust) = self.photo_adjust.as_mut() else {
            return false;
        };
        if adjust.drag.take().is_none() {
            return false;
        }
        let layer = adjust.layer;
        if let Some(layer) = self.photo_layer_mut(layer) {
            layer.settle();
        }
        true
    }

    /// A key pressed on the map while placing. Returns whether it was handled
    /// (arrows nudge, Escape cancels, Enter keeps).
    pub(crate) fn photo_adjust_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(layer_id) = self.photo_adjust.as_ref().map(|a| a.layer) else {
            return false;
        };
        match key {
            "escape" => self.finish_photo_adjust(false, window, cx),
            "enter" => self.finish_photo_adjust(true, window, cx),
            "left" | "right" | "up" | "down" => {
                // One screen pixel in Mercator metres, ten with Shift.
                let origin = self.viewport.screen_to_mercator(point(px(0.0), px(0.0)));
                let one = self.viewport.screen_to_mercator(point(px(1.0), px(0.0)));
                let step = (one.x - origin.x) * if shift { 10.0 } else { 1.0 };
                let (dx, dy) = match key {
                    "left" => (-step, 0.0),
                    "right" => (step, 0.0),
                    "up" => (0.0, step),
                    _ => (0.0, -step),
                };
                if let Some(layer) = self.photo_layer_mut(layer_id) {
                    let moved = georeference::translated(&layer.corners(), dx, dy);
                    layer.set_corners(moved, Quality::Full);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn change_photo_opacity(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(layer_id) = self.photo_adjust.as_ref().map(|a| a.layer) else {
            return;
        };
        if let Some(layer) = self.photo_layer_mut(layer_id) {
            let next = layer.opacity() + delta;
            layer.set_opacity(next);
        }
        cx.notify();
    }

    fn reset_photo_adjust(&mut self, cx: &mut Context<Self>) {
        let Some((layer_id, corners, opacity)) = self
            .photo_adjust
            .as_ref()
            .map(|a| (a.layer, a.entry_corners, a.entry_opacity))
        else {
            return;
        };
        if let Some(layer) = self.photo_layer_mut(layer_id) {
            layer.set_corners(corners, Quality::Full);
            layer.set_opacity(opacity);
        }
        cx.notify();
    }

    /// The picture's outline and its four corner handles, over the map.
    /// Not interactive: presses reach the map's handler, which grabs them.
    pub(crate) fn render_photo_adjust_handles(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let adjust = self.photo_adjust.as_ref()?;
        let corners = self.photo_layer(adjust.layer)?.corners();
        let at: Vec<Point<Pixels>> = corners
            .iter()
            .map(|c| self.viewport.mercator_to_screen(*c))
            .collect();
        let accent = cx.theme().primary;
        let outline = {
            let at = at.clone();
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let mut path = PathBuilder::stroke(px(2.0));
                    path.move_to(bounds.origin + at[0]);
                    for p in &at[1..] {
                        path.line_to(bounds.origin + *p);
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, accent);
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let half = HANDLE_SIZE_PX / 2.0;
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(outline)
                .children(at.iter().enumerate().map(|(i, p)| {
                    div()
                        .id(("photo-handle", i))
                        .absolute()
                        .left(p.x - px(half))
                        .top(p.y - px(half))
                        .size(px(HANDLE_SIZE_PX))
                        .rounded_sm()
                        .bg(rgba(0xffffffff))
                        .border_2()
                        .border_color(accent)
                }))
                .into_any_element(),
        )
    }

    /// The floating panel at the top of the map while placing.
    pub(crate) fn render_photo_adjust_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let adjust = self.photo_adjust.as_ref()?;
        let layer = self.photo_layer(adjust.layer)?;
        let muted = cx.theme().muted_foreground;
        let percent = format!("{}%", (layer.opacity() * 100.0).round() as u32);

        let panel = v_flex()
            .id("photo-overlay-panel")
            .w(px(400.0))
            .p_3()
            .gap_2()
            .bg(cx.theme().popover.opacity(0.96))
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .text_color(cx.theme().popover_foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr!("photo_overlay.title")),
                    )
                    .child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(layer.name().to_string()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(muted).child(tr!("photo_overlay.opacity")))
                    .child(
                        Button::new("photo-opacity-less")
                            .label(tr!("photo_overlay.less"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_photo_opacity(-OPACITY_STEP, cx)
                            })),
                    )
                    .child(div().w(px(44.0)).child(percent))
                    .child(
                        Button::new("photo-opacity-more")
                            .label(tr!("photo_overlay.more"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_photo_opacity(OPACITY_STEP, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(muted_text_size())
                    .text_color(muted)
                    .line_height(gpui::relative(1.4))
                    .child(tr!("photo_overlay.hint")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("photo-overlay-reset")
                            .label(tr!("photo_overlay.reset"))
                            .on_click(cx.listener(|this, _, _, cx| this.reset_photo_adjust(cx))),
                    )
                    .child(
                        Button::new("photo-overlay-done")
                            .primary()
                            .label(tr!("photo_overlay.done"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_photo_adjust(true, window, cx);
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
