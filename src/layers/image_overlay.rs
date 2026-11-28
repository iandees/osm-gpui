//! A photo or scanned map laid over the map, stretched to the four corner
//! pins the user placed (see `georeference`).
//!
//! gpui draws images axis-aligned only, so the layer warps the picture on the
//! CPU into a north-up raster covering the pins' bounding box and draws that
//! as one image element. While a pin is being dragged the raster is rebuilt
//! small (`PREVIEW_SIDE`) so the picture follows the pointer; the full-size
//! one (`FULL_SIDE`) is built when the drag ends.

use std::sync::{Arc, Mutex};

use gpui::{div, img, prelude::*, AnyElement, App, Bounds, ObjectFit, Pixels, RenderImage, Window};
use image::RgbaImage;

use crate::coordinates::Mercator;
use crate::download_bounds::MercRect;
use crate::georeference::{self, Corners};
use crate::layers::{LayerId, LayerKind, MapLayer};
use crate::viewport::Viewport;

/// Longest side of the raster while a pin is being dragged.
pub const PREVIEW_SIDE: u32 = 640;
/// Longest side of the settled raster.
pub const FULL_SIDE: u32 = 2048;
/// Pictures larger than this on their longest side are shrunk when loaded.
pub const MAX_SOURCE_SIDE: u32 = 4096;

/// How finely `ImageOverlayLayer::set_corners` rebuilds the raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Preview,
    Full,
}

impl Quality {
    fn side(self) -> u32 {
        match self {
            Quality::Preview => PREVIEW_SIDE,
            Quality::Full => FULL_SIDE,
        }
    }
}

/// Shrink `img` to at most `MAX_SOURCE_SIDE` on its longest side.
pub fn limit_source(img: RgbaImage) -> RgbaImage {
    let longest = img.width().max(img.height());
    if longest <= MAX_SOURCE_SIDE {
        return img;
    }
    let scale = MAX_SOURCE_SIDE as f64 / longest as f64;
    image::imageops::resize(
        &img,
        ((img.width() as f64 * scale).round() as u32).max(1),
        ((img.height() as f64 * scale).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    )
}

struct Rendered {
    image: Arc<RenderImage>,
    bounds: MercRect,
}

pub struct ImageOverlayLayer {
    id: LayerId,
    name: String,
    visible: bool,
    /// 0..=1.
    opacity: f32,
    source: Arc<RgbaImage>,
    corners: Corners,
    rendered: Option<Rendered>,
    /// Rasters this layer replaced: gpui keeps a texture per image until told
    /// to drop it, which `render_elements` (it has a window) does.
    retired: Mutex<Vec<Arc<RenderImage>>>,
}

impl ImageOverlayLayer {
    pub fn new(id: LayerId, name: String, source: RgbaImage, corners: Corners) -> Self {
        let mut layer = Self {
            id,
            name,
            visible: true,
            opacity: 0.6,
            source: Arc::new(source),
            corners,
            rendered: None,
            retired: Mutex::new(Vec::new()),
        };
        layer.rebuild(Quality::Full);
        layer
    }

    /// The picture's size in pixels.
    pub fn source_size(&self) -> (u32, u32) {
        (self.source.width(), self.source.height())
    }

    pub fn corners(&self) -> Corners {
        self.corners
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.05, 1.0);
    }

    /// Move the pins. A set that is not a proper quad is refused (the layer
    /// keeps its last good one); returns whether it was taken.
    pub fn set_corners(&mut self, corners: Corners, quality: Quality) -> bool {
        if !georeference::is_valid(&corners) {
            return false;
        }
        self.corners = corners;
        self.rebuild(quality);
        true
    }

    /// Rebuild the raster of the current pins at full quality.
    pub fn settle(&mut self) {
        self.rebuild(Quality::Full);
    }

    fn rebuild(&mut self, quality: Quality) {
        let Some(warped) = georeference::warp(&self.source, &self.corners, quality.side()) else {
            return;
        };
        let mut frames = smallvec::SmallVec::new();
        frames.push(image::Frame::new(warped.bgra));
        let next = Rendered {
            image: Arc::new(RenderImage::new(frames)),
            bounds: warped.bounds,
        };
        if let Some(old) = self.rendered.replace(next) {
            self.retired
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(old.image);
        }
    }
}

impl MapLayer for ImageOverlayLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn kind(&self) -> LayerKind {
        LayerKind::ImageOverlay
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn as_image_overlay(&self) -> Option<&ImageOverlayLayer> {
        Some(self)
    }

    fn as_image_overlay_mut(&mut self) -> Option<&mut ImageOverlayLayer> {
        Some(self)
    }

    fn render_elements(
        &self,
        viewport: &Viewport,
        window: &mut Window,
        _cx: &mut App,
    ) -> Vec<AnyElement> {
        let retired = std::mem::take(&mut *self.retired.lock().unwrap_or_else(|e| e.into_inner()));
        for image in retired {
            let _ = window.drop_image(image);
        }
        let Some(rendered) = &self.rendered else {
            return Vec::new();
        };
        let b = rendered.bounds;
        let top_left = viewport.mercator_to_screen(Mercator::new(b.min_x, b.max_y));
        let bottom_right = viewport.mercator_to_screen(Mercator::new(b.max_x, b.min_y));
        let image = rendered.image.clone();
        vec![div()
            .absolute()
            .left(top_left.x)
            .top(top_left.y)
            .w(bottom_right.x - top_left.x)
            .h(bottom_right.y - top_left.y)
            .opacity(self.opacity)
            .child(img(image).size_full().object_fit(ObjectFit::Fill))
            .into_any_element()]
    }

    fn render_canvas(&self, _viewport: &Viewport, _bounds: Bounds<Pixels>, _window: &mut Window) {}

    fn stats(&self) -> Vec<(String, String)> {
        vec![(
            "Size".to_string(),
            format!("{} × {} px", self.source.width(), self.source.height()),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> ImageOverlayLayer {
        let corners = georeference::initial_corners((10, 10), Mercator::new(0.0, 0.0), 100.0);
        ImageOverlayLayer::new(
            LayerId(1),
            "Scan".into(),
            RgbaImage::from_pixel(10, 10, image::Rgba([1, 2, 3, 255])),
            corners,
        )
    }

    #[test]
    fn a_new_layer_has_a_raster_covering_its_pins() {
        let l = layer();
        let b = l.rendered.as_ref().unwrap().bounds;
        assert_eq!((b.min_x, b.max_x), (-50.0, 50.0));
    }

    #[test]
    fn a_folded_set_of_pins_is_refused() {
        let mut l = layer();
        let before = l.corners();
        let mut bow = before;
        bow.swap(1, 2);
        assert!(!l.set_corners(bow, Quality::Preview));
        assert_eq!(l.corners(), before);
    }

    #[test]
    fn moving_the_pins_retires_the_old_raster() {
        let mut l = layer();
        let moved = georeference::translated(&l.corners(), 10.0, 0.0);
        assert!(l.set_corners(moved, Quality::Preview));
        assert_eq!(l.retired.lock().unwrap().len(), 1);
        assert_eq!(l.rendered.as_ref().unwrap().bounds.min_x, -40.0);
    }

    #[test]
    fn opacity_never_reaches_invisible() {
        let mut l = layer();
        l.set_opacity(0.0);
        assert!(l.opacity() > 0.0);
        l.set_opacity(3.0);
        assert_eq!(l.opacity(), 1.0);
    }

    #[test]
    fn big_pictures_are_shrunk_keeping_the_aspect() {
        let big = RgbaImage::new(8192, 4096);
        let small = limit_source(big);
        assert_eq!((small.width(), small.height()), (4096, 2048));
    }
}
