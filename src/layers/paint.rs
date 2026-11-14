//! What the canvas-painting layers share: converting view-local coordinates
//! to window coordinates, batching triangles into one path, and the small
//! polygons notes and photos are drawn with.

use gpui::{point, px, rgba, size, Bounds, Path, PathBuilder, Pixels, Window};

use crate::layers::way_stroke::PathSink;
use crate::viewport::Viewport;

/// The viewport and where the canvas sits in the window. The viewport gives
/// view-local pixels (0,0 at the canvas's top-left); paint calls take window
/// pixels, so every layer's paint code converts through here.
pub struct PaintFrame<'a> {
    pub viewport: &'a Viewport,
    origin: (f32, f32),
    size: (f32, f32),
}

impl<'a> PaintFrame<'a> {
    pub fn new(viewport: &'a Viewport, bounds: Bounds<Pixels>) -> Self {
        Self {
            viewport,
            origin: (bounds.origin.x.as_f32(), bounds.origin.y.as_f32()),
            size: (bounds.size.width.as_f32(), bounds.size.height.as_f32()),
        }
    }

    /// The canvas's `(width, height)`.
    pub fn view_size(&self) -> (f32, f32) {
        self.size
    }

    /// View-local pixels to window pixels.
    pub fn to_window(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (x + self.origin.0, y + self.origin.1)
    }

    /// View-local pixels of a geographic position.
    pub fn local_geo(&self, lat: f64, lon: f64) -> (f32, f32) {
        let p = self.viewport.geo_to_screen(lat, lon);
        (p.x.as_f32(), p.y.as_f32())
    }

    /// Window pixels of a Web Mercator position.
    pub fn window_mercator(&self, mx: f64, my: f64) -> (f32, f32) {
        let p = self.viewport.mercator_to_screen(mx, my);
        self.to_window((p.x.as_f32(), p.y.as_f32()))
    }
}

/// Raw triangles painted in one colour, with the bounds accumulated as they
/// are pushed (a `Path` needs its bounds set before it is painted).
pub struct PathBatch {
    path: Option<Path<Pixels>>,
    /// `(min_x, max_x, min_y, max_y)`.
    bounds: (f32, f32, f32, f32),
}

impl PathBatch {
    pub fn new() -> Self {
        PathBatch {
            path: None,
            bounds: (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ),
        }
    }

    /// Where triangles are pushed.
    pub fn sink(&mut self) -> PathSink<'_> {
        PathSink {
            path: &mut self.path,
            bounds: &mut self.bounds,
        }
    }

    /// The path with its bounds set, or `None` if nothing was pushed.
    pub fn finish(self) -> Option<Path<Pixels>> {
        let mut path = self.path?;
        if path.vertices.is_empty() {
            return None;
        }
        let (min_x, max_x, min_y, max_y) = self.bounds;
        path.bounds = Bounds {
            origin: point(px(min_x), px(min_y)),
            size: size(px(max_x - min_x), px(max_y - min_y)),
        };
        Some(path)
    }
}

impl Default for PathBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a box intersects the view; both are `(min_x, max_x, min_y, max_y)`
/// in Web Mercator meters (the order of `Viewport::mercator_view_bounds`).
/// Touching counts.
pub fn intersects_view(bbox: (f64, f64, f64, f64), view: (f64, f64, f64, f64)) -> bool {
    let (min_x, max_x, min_y, max_y) = bbox;
    let (vmin_x, vmax_x, vmin_y, vmax_y) = view;
    !(max_x < vmin_x || min_x > vmax_x || max_y < vmin_y || min_y > vmax_y)
}

/// A 16-gon approximating a circle of radius `r` around `(x, y)`.
pub fn circle_outline(x: f32, y: f32, r: f32) -> Vec<(f32, f32)> {
    (0..16)
        .map(|i| {
            let a = (i as f32) * std::f32::consts::TAU / 16.0;
            (x + r * a.cos(), y + r * a.sin())
        })
        .collect()
}

/// Paint the closed polygon `pts` in window pixels, filled or stroked with
/// the given width.
pub fn paint_polygon(window: &mut Window, pts: &[(f32, f32)], color: u32, stroke: Option<f32>) {
    let mut builder = match stroke {
        Some(w) => PathBuilder::stroke(px(w)),
        None => PathBuilder::fill(),
    };
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 {
            builder.move_to(point(px(*x), px(*y)));
        } else {
            builder.line_to(point(px(*x), px(*y)));
        }
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, rgba(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::way_stroke::TriSink;

    fn frame_parts() -> (Viewport, Bounds<Pixels>) {
        (
            Viewport::new(40.0, -74.0, 15.0, size(px(800.0), px(600.0))),
            Bounds {
                origin: point(px(30.0), px(48.0)),
                size: size(px(800.0), px(600.0)),
            },
        )
    }

    #[test]
    fn window_coordinates_are_view_coordinates_plus_the_canvas_origin() {
        let (vp, bounds) = frame_parts();
        let frame = PaintFrame::new(&vp, bounds);
        // The view centre is the middle of the canvas.
        let (x, y) = frame.local_geo(40.0, -74.0);
        assert!((x - 400.0).abs() < 0.01 && (y - 300.0).abs() < 0.01);
        let (wx, wy) = frame.to_window((x, y));
        assert!((wx - 430.0).abs() < 0.01 && (wy - 348.0).abs() < 0.01);
        assert_eq!(frame.view_size(), (800.0, 600.0));
        let (mx, my) = vp.screen_to_mercator(point(px(400.0), px(300.0)));
        let (wx, wy) = frame.window_mercator(mx, my);
        assert!((wx - 430.0).abs() < 0.01 && (wy - 348.0).abs() < 0.01);
    }

    #[test]
    fn a_batch_reports_the_bounds_of_what_was_pushed() {
        let mut batch = PathBatch::new();
        assert!(PathBatch::new().finish().is_none());
        batch.sink().tri((1.0, 2.0), (5.0, 3.0), (2.0, 9.0));
        let path = batch.finish().expect("one triangle");
        assert_eq!(path.vertices.len(), 3);
        assert_eq!(path.bounds.origin, point(px(1.0), px(2.0)));
        assert_eq!(path.bounds.size, size(px(4.0), px(7.0)));
    }

    #[test]
    fn boxes_intersect_the_view_when_they_overlap_or_touch() {
        let view = (0.0, 10.0, 0.0, 10.0);
        assert!(intersects_view((2.0, 3.0, 2.0, 3.0), view));
        assert!(intersects_view((10.0, 12.0, 0.0, 1.0), view));
        assert!(!intersects_view((10.5, 12.0, 0.0, 1.0), view));
        assert!(!intersects_view((2.0, 3.0, -4.0, -1.0), view));
    }

    #[test]
    fn a_circle_outline_is_sixteen_points_on_the_circle() {
        let pts = circle_outline(10.0, 20.0, 4.0);
        assert_eq!(pts.len(), 16);
        for (x, y) in pts {
            let d = ((x - 10.0).powi(2) + (y - 20.0).powi(2)).sqrt();
            assert!((d - 4.0).abs() < 1e-4);
        }
    }
}
