//! A picture shown full size over the window, with zoom (wheel, Cmd or Ctrl +
//! trackpad scroll, pinch, double-click to reset) and pan (drag, or trackpad
//! scroll). Escape or the close button closes it. Extension panels open it
//! with a double-click on a picture.

use std::sync::Arc;

use gpui::{div, img, prelude::*, px, AnyElement, Context, RenderImage, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    IconName,
};
use osm_gpui::settings_store;
use osm_gpui::tr;

use crate::MapViewer;

const MAX_ZOOM: f32 = 12.0;

/// Decode downloaded image bytes into what gpui draws (BGRA).
pub(crate) fn decode(bytes: &[u8]) -> Result<RenderImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let mut rgba = img.to_rgba8();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let mut frames = smallvec::SmallVec::new();
    frames.push(image::Frame::new(rgba));
    Ok(RenderImage::new(frames))
}

/// Zoom and pan of the full-size picture. It is drawn in a box the size of
/// the window, scaled by `zoom` about the window's top-left and shifted by
/// `(x, y)`; at zoom 1 it is fitted whole and cannot be moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    zoom: f32,
    x: f32,
    y: f32,
    /// A drag in progress: where it started and the offset then.
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            x: 0.0,
            y: 0.0,
            drag: None,
        }
    }
}

impl View {
    /// Keep the scaled box covering the window.
    fn clamp(&mut self, w: f32, h: f32) {
        self.zoom = self.zoom.clamp(1.0, MAX_ZOOM);
        self.x = self.x.clamp(w - w * self.zoom, 0.0);
        self.y = self.y.clamp(h - h * self.zoom, 0.0);
    }

    /// Multiply the zoom by `factor`, keeping the point under `(px, py)`
    /// (window pixels) where it is.
    fn zoom_at(&mut self, px: f32, py: f32, factor: f32, w: f32, h: f32) {
        let old = self.zoom;
        let new = (old * factor).clamp(1.0, MAX_ZOOM);
        let k = new / old;
        self.x = px - (px - self.x) * k;
        self.y = py - (py - self.y) * k;
        self.zoom = new;
        self.clamp(w, h);
    }

    fn pan_by(&mut self, dx: f32, dy: f32, w: f32, h: f32) {
        self.x += dx;
        self.y += dy;
        self.clamp(w, h);
    }
}

/// The picture open full size, if any.
pub(crate) struct Open {
    image: Arc<RenderImage>,
    view: View,
}

impl MapViewer {
    pub(crate) fn open_image_viewer(&mut self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        self.image_viewer = Some(Open {
            image,
            view: View::default(),
        });
        cx.notify();
    }

    /// Escape closes the full-size view. Returns whether one was open.
    pub(crate) fn close_image_viewer(&mut self) -> bool {
        self.image_viewer.take().is_some()
    }

    fn viewer_view(&mut self) -> Option<&mut View> {
        self.image_viewer.as_mut().map(|o| &mut o.view)
    }

    pub(crate) fn render_image_viewer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.image_viewer.as_ref()?;
        let size = window.viewport_size();
        let (w, h) = (size.width.as_f32(), size.height.as_f32());
        let view = open.view;
        let dragging = view.drag.is_some();

        let picture = img(open.image.clone())
            .absolute()
            .left(px(view.x))
            .top(px(view.y))
            .w(px(w * view.zoom))
            .h(px(h * view.zoom))
            .object_fit(gpui::ObjectFit::Contain);

        Some(
            div()
                .id("image-viewer")
                .absolute()
                .inset_0()
                .occlude()
                .overflow_hidden()
                .bg(gpui::rgba(0x000000f0))
                .when(dragging, |d| d.cursor_grabbing())
                .when(!dragging, |d| d.cursor_grab())
                .on_scroll_wheel(
                    cx.listener(|this, ev: &gpui::ScrollWheelEvent, window, cx| {
                        let size = window.viewport_size();
                        let (w, h) = (size.width.as_f32(), size.height.as_f32());
                        let (px_, py_) = (ev.position.x.as_f32(), ev.position.y.as_f32());
                        let sign = if settings_store::snapshot().invert_scroll_zoom {
                            -1.0
                        } else {
                            1.0
                        };
                        let zoom_modifier = ev.modifiers.platform || ev.modifiers.control;
                        let Some(view) = this.viewer_view() else {
                            return;
                        };
                        match ev.delta {
                            gpui::ScrollDelta::Lines(d) => {
                                view.zoom_at(px_, py_, (sign * d.y * 0.15).exp(), w, h)
                            }
                            gpui::ScrollDelta::Pixels(d) if zoom_modifier => {
                                view.zoom_at(px_, py_, (sign * d.y.as_f32() * 0.01).exp(), w, h)
                            }
                            gpui::ScrollDelta::Pixels(d) => {
                                view.pan_by(d.x.as_f32(), d.y.as_f32(), w, h)
                            }
                        }
                        cx.notify();
                    }),
                )
                .on_pinch(cx.listener(|this, ev: &gpui::PinchEvent, window, cx| {
                    let size = window.viewport_size();
                    if let Some(view) = this.viewer_view() {
                        view.zoom_at(
                            ev.position.x.as_f32(),
                            ev.position.y.as_f32(),
                            ev.delta.exp(),
                            size.width.as_f32(),
                            size.height.as_f32(),
                        );
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                        let Some(view) = this.viewer_view() else {
                            return;
                        };
                        if ev.click_count >= 2 {
                            *view = View::default();
                        } else {
                            view.drag = Some((
                                (ev.position.x.as_f32(), ev.position.y.as_f32()),
                                (view.x, view.y),
                            ));
                        }
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                    let size = window.viewport_size();
                    let Some(view) = this.viewer_view() else {
                        return;
                    };
                    let Some(((sx, sy), (ox, oy))) = view.drag else {
                        return;
                    };
                    view.x = ox + (ev.position.x.as_f32() - sx);
                    view.y = oy + (ev.position.y.as_f32() - sy);
                    view.clamp(size.width.as_f32(), size.height.as_f32());
                    cx.notify();
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if let Some(view) = this.viewer_view() {
                            view.drag = None;
                        }
                        cx.notify();
                    }),
                )
                .on_mouse_up_out(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if let Some(view) = this.viewer_view() {
                            view.drag = None;
                        }
                        cx.notify();
                    }),
                )
                .child(picture)
                .child(
                    div().absolute().top_3().right_3().child(
                        Button::new("image-viewer-close")
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(tr!("image_viewer.close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.image_viewer = None;
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_point_under_the_cursor_and_stays_covering_the_window() {
        let (w, h) = (800.0, 600.0);
        let mut v = View::default();
        // Zooming out at fit does nothing; no panning at fit either.
        v.zoom_at(400.0, 300.0, 0.5, w, h);
        assert_eq!(v, View::default());
        v.pan_by(50.0, 50.0, w, h);
        assert_eq!((v.x, v.y), (0.0, 0.0));
        // Zoom 2x about (600, 150): that point of the picture stays there.
        v.zoom_at(600.0, 150.0, 2.0, w, h);
        assert_eq!(v.zoom, 2.0);
        let picture_x = (600.0 - v.x) / v.zoom;
        assert!((picture_x - 600.0).abs() < 0.01);
        // Panning is limited to the picture's edges.
        v.pan_by(10_000.0, 10_000.0, w, h);
        assert_eq!((v.x, v.y), (0.0, 0.0));
        v.pan_by(-10_000.0, -10_000.0, w, h);
        assert_eq!((v.x, v.y), (-800.0, -600.0));
        // The zoom is capped.
        v.zoom_at(0.0, 0.0, 1000.0, w, h);
        assert_eq!(v.zoom, MAX_ZOOM);
    }

    #[test]
    fn decode_swaps_to_bgra() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = decode(&png).unwrap();
        assert_eq!(out.as_bytes(0).unwrap()[..3], [30, 20, 10]);
        assert!(decode(b"not an image").is_err());
    }
}
