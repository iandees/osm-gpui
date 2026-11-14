use crate::coordinates::CoordinateTransform;
use gpui::{Pixels, Point, Size};

/// Wheel lines per pixel of precise scroll, for scaling a trackpad scroll
/// into the same zoom units as a notched wheel.
const PIXEL_SCROLL_ZOOM_SCALE: f64 = 0.1;

/// Viewport state for handling map navigation
#[derive(Debug, Clone)]
pub struct Viewport {
    pub transform: CoordinateTransform,
    pub is_dragging: bool,
    pub last_mouse_position: Option<Point<Pixels>>,
    pub drag_start_position: Option<Point<Pixels>>,
    pub zoom_sensitivity: f64,
    pub pan_sensitivity: f64,
}

impl Viewport {
    pub fn new(
        center_lat: f64,
        center_lon: f64,
        zoom_level: f64,
        screen_size: Size<Pixels>,
    ) -> Self {
        let transform = CoordinateTransform::new(center_lat, center_lon, zoom_level, screen_size);

        Self {
            transform,
            is_dragging: false,
            last_mouse_position: None,
            drag_start_position: None,
            zoom_sensitivity: 0.05,
            pan_sensitivity: 1.0,
        }
    }

    /// Handle mouse down event for starting drag operations
    pub fn handle_mouse_down(&mut self, position: Point<Pixels>) {
        self.is_dragging = true;
        self.last_mouse_position = Some(position);
        self.drag_start_position = Some(position);
    }

    /// Handle mouse up event for ending drag operations
    pub fn handle_mouse_up(&mut self) {
        self.is_dragging = false;
        self.last_mouse_position = None;
        self.drag_start_position = None;
    }

    /// Handle mouse move event for panning
    pub fn handle_mouse_move(&mut self, position: Point<Pixels>) -> bool {
        if let Some(last_pos) = self.last_mouse_position {
            if self.is_dragging {
                let dx = (position.x - last_pos.x) * self.pan_sensitivity as f32;
                let dy = (position.y - last_pos.y) * self.pan_sensitivity as f32;

                self.transform.pan_by_pixels(-dx, -dy);
                self.last_mouse_position = Some(position);
                return true; // Indicate that the view should be redrawn
            }
        }

        self.last_mouse_position = Some(position);
        false
    }

    /// Zoom around `position` by `zoom_delta` levels. Returns whether the
    /// view changed (and so needs a redraw).
    fn zoom_at(&mut self, position: Point<Pixels>, zoom_delta: f64) -> bool {
        if !zoom_delta.is_finite() || zoom_delta == 0.0 {
            return false;
        }
        let before = self.transform.zoom_level;
        self.transform.zoom_at_point(position, zoom_delta);
        self.transform.zoom_level != before
    }

    /// The scroll amount that drives zooming, given the raw vertical scroll
    /// amount (lines for a wheel, pixels for a trackpad) and the "invert
    /// scroll zoom" setting. Inverting flips the sign, so what zooms in zooms
    /// out. Only scroll zoom uses this: pinch, keys and menus do not.
    ///
    /// The base convention matches Leaflet (OSM website), iD and JOSM: GPUI
    /// reports positive vertical deltas when content is scrolled down (wheel
    /// rolled up/away on a non-natural setup), and a positive step zooms in.
    pub fn scroll_zoom_step(raw: f32, invert: bool) -> f32 {
        if invert {
            -raw
        } else {
            raw
        }
    }

    /// Zoom from a notched mouse wheel: `lines` is the vertical scroll
    /// amount in lines (`ScrollDelta::Lines`).
    pub fn zoom_by_wheel(&mut self, position: Point<Pixels>, lines: f32) -> bool {
        self.zoom_at(position, (lines as f64) * self.zoom_sensitivity)
    }

    /// Zoom from a precise (trackpad) vertical scroll of `pixels`, used
    /// when a modifier turns the two-finger scroll from a pan into a zoom.
    pub fn zoom_by_scroll_pixels(&mut self, position: Point<Pixels>, pixels: Pixels) -> bool {
        self.zoom_at(
            position,
            pixels.to_f64() * PIXEL_SCROLL_ZOOM_SCALE * self.zoom_sensitivity,
        )
    }

    /// Zoom from a trackpad pinch. `magnification` is the fractional scale
    /// change for this event (0.1 = 10% larger), as macOS reports it; a
    /// doubling of scale is one zoom level.
    pub fn zoom_by_pinch(&mut self, position: Point<Pixels>, magnification: f32) -> bool {
        let scale = 1.0 + magnification as f64;
        if scale <= 0.0 {
            return false;
        }
        self.zoom_at(position, scale.log2())
    }

    /// Pan from a precise (trackpad) two-finger scroll. `delta` is how far
    /// the content should move on screen, so the map follows the fingers.
    pub fn pan_by_scroll(&mut self, delta: Point<Pixels>) -> bool {
        if delta.x == Pixels::ZERO && delta.y == Pixels::ZERO {
            return false;
        }
        self.transform.pan_by_pixels(-delta.x, -delta.y);
        true
    }

    /// Update viewport size when window is resized
    pub fn update_size(&mut self, new_size: Size<Pixels>) {
        self.transform = CoordinateTransform::new(
            self.transform.center_lat,
            self.transform.center_lon,
            self.transform.zoom_level,
            new_size,
        );
    }

    pub fn set_zoom(&mut self, zoom_level: f64) {
        self.transform = CoordinateTransform::new(
            self.transform.center_lat,
            self.transform.center_lon,
            zoom_level,
            self.transform.screen_size,
        );
    }

    /// Pan to a specific geographic location
    pub fn pan_to(&mut self, lat: f64, lon: f64) {
        self.transform = CoordinateTransform::new(
            lat,
            lon,
            self.transform.zoom_level,
            self.transform.screen_size,
        );
    }

    pub fn zoom_level(&self) -> f64 {
        self.transform.zoom_level
    }

    pub fn center(&self) -> (f64, f64) {
        (self.transform.center_lat, self.transform.center_lon)
    }

    pub fn is_visible(&self, lat: f64, lon: f64) -> bool {
        self.transform.is_visible(lat, lon)
    }

    /// Convert geographic coordinates to screen coordinates
    pub fn geo_to_screen(&self, lat: f64, lon: f64) -> Point<Pixels> {
        self.transform.geo_to_screen(lat, lon)
    }

    /// Project a point given in Web Mercator meters directly to screen pixels
    /// (no trig). Prefer this in hot render paths where source coordinates
    /// have already been projected to Mercator and cached.
    #[inline]
    pub fn mercator_to_screen(&self, mx: f64, my: f64) -> Point<Pixels> {
        self.transform.mercator_to_screen(mx, my)
    }

    /// The trig-free Mercator-to-screen mapping as a copyable value for hot
    /// loops (see [`crate::coordinates::MercatorProjector`]).
    #[inline]
    pub fn projector(&self) -> crate::coordinates::MercatorProjector {
        self.transform.projector()
    }

    /// Mercator-space AABB of the current view (min_x, max_x, min_y, max_y).
    #[inline]
    pub fn mercator_view_bounds(&self) -> (f64, f64, f64, f64) {
        (
            self.transform.mercator_min_x,
            self.transform.mercator_max_x,
            self.transform.mercator_min_y,
            self.transform.mercator_max_y,
        )
    }

    /// `mercator_view_bounds` as a rectangle.
    #[inline]
    pub fn mercator_view_rect(&self) -> crate::download_bounds::MercRect {
        let (min_x, max_x, min_y, max_y) = self.mercator_view_bounds();
        crate::download_bounds::MercRect::new(min_x, max_x, min_y, max_y)
    }

    /// Convert screen coordinates to geographic coordinates
    pub fn screen_to_geo(&self, point: Point<Pixels>) -> (f64, f64) {
        self.transform.screen_to_geo(point)
    }

    /// Convert screen coordinates directly to Web Mercator meters (no lat/lon
    /// round trip). Used by box-select hit-testing.
    pub fn screen_to_mercator(&self, point: Point<Pixels>) -> (f64, f64) {
        self.transform.screen_to_mercator(point)
    }

    pub fn visible_bounds(&self) -> crate::coordinates::GeoBounds {
        self.transform.visible_bounds()
    }

    /// Get the zoom level as used by tile servers
    pub fn tile_zoom_level(&self) -> u32 {
        self.transform.tile_zoom_level()
    }

    /// Reset viewport to initial state
    pub fn reset(&mut self, center_lat: f64, center_lon: f64, zoom_level: f64) {
        self.is_dragging = false;
        self.last_mouse_position = None;
        self.drag_start_position = None;

        self.transform = CoordinateTransform::new(
            center_lat,
            center_lon,
            zoom_level,
            self.transform.screen_size,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn test_viewport_creation() {
        let screen_size = size(px(800.0), px(600.0));
        let viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        assert_eq!(viewport.zoom_level(), 10.0);
        assert_eq!(viewport.center(), (40.7128, -74.0060));
        assert!(!viewport.is_dragging);
    }

    #[test]
    fn test_viewport_zoom() {
        let screen_size = size(px(800.0), px(600.0));
        let mut viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        viewport.set_zoom(12.0);
        assert_eq!(viewport.zoom_level(), 12.0);
    }

    #[test]
    fn test_viewport_pan() {
        let screen_size = size(px(800.0), px(600.0));
        let mut viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        let new_lat = 41.0;
        let new_lon = -73.0;
        viewport.pan_to(new_lat, new_lon);

        assert_eq!(viewport.center(), (new_lat, new_lon));
    }

    fn test_viewport() -> Viewport {
        Viewport::new(40.7128, -74.0060, 10.0, size(px(800.0), px(600.0)))
    }

    #[test]
    fn scroll_pan_moves_the_map_with_the_fingers() {
        let mut viewport = test_viewport();
        let anchor = viewport.screen_to_geo(point(px(400.0), px(300.0)));

        // Content should move 50px right and 30px down.
        assert!(viewport.pan_by_scroll(point(px(50.0), px(30.0))));

        let moved = viewport.geo_to_screen(anchor.0, anchor.1);
        assert!((moved.x.as_f32() - 450.0).abs() < 0.5);
        assert!((moved.y.as_f32() - 330.0).abs() < 0.5);
        assert_eq!(viewport.zoom_level(), 10.0);
    }

    #[test]
    fn scroll_zoom_step_keeps_or_flips_the_sign() {
        assert_eq!(Viewport::scroll_zoom_step(2.0, false), 2.0);
        assert_eq!(Viewport::scroll_zoom_step(-2.0, false), -2.0);
        assert_eq!(Viewport::scroll_zoom_step(2.0, true), -2.0);
        assert_eq!(Viewport::scroll_zoom_step(-2.0, true), 2.0);
        assert_eq!(Viewport::scroll_zoom_step(0.0, true), 0.0);
    }

    #[test]
    fn inverting_reverses_wheel_zoom_for_line_deltas() {
        let at = point(px(400.0), px(300.0));
        for invert in [false, true] {
            let mut viewport = test_viewport();
            // Wheel up (positive lines) zooms in by default.
            assert!(viewport.zoom_by_wheel(at, Viewport::scroll_zoom_step(1.0, invert)));
            if invert {
                assert!(viewport.zoom_level() < 10.0);
            } else {
                assert!(viewport.zoom_level() > 10.0);
            }
        }
    }

    #[test]
    fn inverting_reverses_zoom_for_pixel_deltas() {
        let at = point(px(400.0), px(300.0));
        for invert in [false, true] {
            let mut viewport = test_viewport();
            let step = Viewport::scroll_zoom_step(20.0, invert);
            assert!(viewport.zoom_by_scroll_pixels(at, px(step)));
            if invert {
                assert!(viewport.zoom_level() < 10.0);
            } else {
                assert!(viewport.zoom_level() > 10.0);
            }
        }
    }

    #[test]
    fn zero_scroll_is_not_a_change() {
        let mut viewport = test_viewport();
        assert!(!viewport.pan_by_scroll(point(px(0.0), px(0.0))));
        assert!(!viewport.zoom_by_wheel(point(px(400.0), px(300.0)), 0.0));
        assert!(!viewport.zoom_by_pinch(point(px(400.0), px(300.0)), 0.0));
    }

    #[test]
    fn pinch_doubling_is_one_zoom_level_and_keeps_the_point_under_the_fingers() {
        let mut viewport = test_viewport();
        let fingers = point(px(600.0), px(200.0));
        let anchor = viewport.screen_to_geo(fingers);

        assert!(viewport.zoom_by_pinch(fingers, 1.0));

        assert!((viewport.zoom_level() - 11.0).abs() < 1e-9);
        let after = viewport.geo_to_screen(anchor.0, anchor.1);
        assert!((after.x.as_f32() - 600.0).abs() < 0.5);
        assert!((after.y.as_f32() - 200.0).abs() < 0.5);
    }

    #[test]
    fn pinch_ignores_a_collapse_to_zero_scale() {
        let mut viewport = test_viewport();
        assert!(!viewport.zoom_by_pinch(point(px(400.0), px(300.0)), -1.0));
        assert_eq!(viewport.zoom_level(), 10.0);
    }

    #[test]
    fn wheel_and_pixel_scroll_zoom_in_the_same_direction() {
        let mut wheel = test_viewport();
        let mut trackpad = test_viewport();
        let at = point(px(400.0), px(300.0));

        assert!(wheel.zoom_by_wheel(at, 2.0));
        assert!(trackpad.zoom_by_scroll_pixels(at, px(20.0)));

        assert!(wheel.zoom_level() > 10.0);
        assert!((wheel.zoom_level() - trackpad.zoom_level()).abs() < 1e-9);
    }

    #[test]
    fn zoom_at_the_limit_is_not_a_change() {
        let mut viewport = Viewport::new(40.7128, -74.0060, 23.0, size(px(800.0), px(600.0)));
        assert!(!viewport.zoom_by_pinch(point(px(400.0), px(300.0)), 0.5));
    }

    #[test]
    fn test_coordinate_conversion() {
        let screen_size = size(px(800.0), px(600.0));
        let viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        let original_lat = 40.7500;
        let original_lon = -73.9000;
        let screen_point = viewport.geo_to_screen(original_lat, original_lon);
        let (converted_lat, converted_lon) = viewport.screen_to_geo(screen_point);

        assert!((converted_lat - original_lat).abs() < 0.001);
        assert!((converted_lon - original_lon).abs() < 0.001);
    }
}
