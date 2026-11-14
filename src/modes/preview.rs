//! Drawing helpers shared by the modes' live previews, so every rubber band,
//! outline and marker is built the same way and the colours live in one place.

use gpui::{fill, point, px, size, Bounds, PathBuilder, Pixels, Point, Rgba, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::viewport::Viewport;

/// The colour of every preview that shows what a click would make or keep.
pub(super) fn blue() -> Rgba {
    gpui::rgb(0x3b82f6)
}

/// An Improve Way preview that would delete a node.
pub(super) fn red() -> Rgba {
    gpui::rgb(0xef4444)
}

/// The snap target highlight.
pub(super) fn amber() -> Rgba {
    gpui::rgb(0xf59e0b)
}

/// Width of a preview line.
pub(super) fn line_width() -> Pixels {
    px(2.0)
}

/// Where and how the map is being painted: the viewport and the map area's
/// bounds, whose origin turns map-local points into window points.
pub(crate) struct PreviewFrame<'a> {
    pub(crate) viewport: &'a Viewport,
    pub(crate) bounds: Bounds<Pixels>,
}

impl PreviewFrame<'_> {
    /// The window position of a geo `(lat, lon)`.
    pub(super) fn geo(&self, (lat, lon): (f64, f64)) -> Point<Pixels> {
        let p = self.viewport.geo_to_screen(LatLon::new(lat, lon));
        self.local(p)
    }

    /// The window position of a map-local point.
    pub(super) fn local(&self, p: Point<Pixels>) -> Point<Pixels> {
        point(p.x + self.bounds.origin.x, p.y + self.bounds.origin.y)
    }

    /// The window position of a map-local `(x, y)` in plain pixels.
    pub(super) fn pixels(&self, p: (f64, f64)) -> Point<Pixels> {
        point(
            px(p.0 as f32) + self.bounds.origin.x,
            px(p.1 as f32) + self.bounds.origin.y,
        )
    }
}

/// Stroke the line through `points` (nothing is painted if the path cannot be
/// built, e.g. for no points).
pub(super) fn stroke_polyline(
    window: &mut Window,
    points: impl IntoIterator<Item = Point<Pixels>>,
    width: Pixels,
    color: Rgba,
) {
    let mut builder = PathBuilder::stroke(width);
    for (i, p) in points.into_iter().enumerate() {
        if i == 0 {
            builder.move_to(p);
        } else {
            builder.line_to(p);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Highlight a snap target: the segment it would join (amber) and a ring
/// around the point the node would land on.
pub(super) fn paint_snap_target(
    target: &crate::SnapTarget,
    frame: &PreviewFrame<'_>,
    window: &mut Window,
) {
    let (lat, lon) = match target {
        crate::SnapTarget::Node { lat, lon, .. } => (*lat, *lon),
        crate::SnapTarget::Way { a, b, lat, lon, .. } => {
            stroke_polyline(window, [frame.geo(*a), frame.geo(*b)], px(4.0), amber());
            (*lat, *lon)
        }
    };
    let c = frame.geo((lat, lon));
    let half = px(7.0);
    let bounds = Bounds {
        origin: point(c.x - half, c.y - half),
        size: size(half * 2.0, half * 2.0),
    };
    window.paint_quad(gpui::quad(
        bounds,
        half,
        gpui::transparent_black(),
        px(2.5),
        amber(),
        gpui::BorderStyle::default(),
    ));
}

/// A filled square `2 * half` wide centred on `center`.
pub(super) fn fill_square(window: &mut Window, center: Point<Pixels>, half: Pixels, color: Rgba) {
    window.paint_quad(fill(
        Bounds {
            origin: point(center.x - half, center.y - half),
            size: size(half * 2.0, half * 2.0),
        },
        color,
    ));
}
