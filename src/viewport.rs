use crate::coordinates::CoordinateTransform;
use gpui::{Pixels, Point, Size};

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

                self.transform.pan_by_pixels(-dx.0, -dy.0);
                self.last_mouse_position = Some(position);
                return true; // Indicate that the view should be redrawn
            }
        }

        self.last_mouse_position = Some(position);
        false
    }

    /// Handle scroll wheel event for zooming
    pub fn handle_scroll(&mut self, position: Point<Pixels>, delta: Point<Pixels>) -> bool {
        // Use vertical scroll for zooming
        let zoom_delta = -delta.y.0 as f64 * self.zoom_sensitivity;

        if zoom_delta.abs() > 0.0 {
            self.transform.zoom_at_point(position, zoom_delta);
            return true; // Indicate that the view should be redrawn
        }

        false
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

    /// Set zoom level directly
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

    /// Get current zoom level
    pub fn zoom_level(&self) -> f64 {
        self.transform.zoom_level
    }

    /// Get current center coordinates
    pub fn center(&self) -> (f64, f64) {
        (self.transform.center_lat, self.transform.center_lon)
    }

    /// Check if a geographic point is currently visible
    pub fn is_visible(&self, lat: f64, lon: f64) -> bool {
        self.transform.is_visible(lat, lon)
    }

    /// Convert geographic coordinates to screen coordinates
    pub fn geo_to_screen(&self, lat: f64, lon: f64) -> Point<Pixels> {
        self.transform.geo_to_screen(lat, lon)
    }

    /// Convert screen coordinates to geographic coordinates
    pub fn screen_to_geo(&self, point: Point<Pixels>) -> (f64, f64) {
        self.transform.screen_to_geo(point)
    }

    /// Get current visible bounds
    pub fn visible_bounds(&self) -> crate::coordinates::GeoBounds {
        self.transform.visible_bounds()
    }

    /// Get the zoom level as used by tile servers
    pub fn tile_zoom_level(&self) -> u32 {
        self.transform.tile_zoom_level()
    }

    /// Animate smooth zoom to a specific level
    pub fn animate_zoom_to(&mut self, target_zoom: f64, _duration_ms: u64) {
        // For now, just set the zoom directly
        // In a full implementation, you'd want to use gpui's animation system
        self.set_zoom(target_zoom);
    }

    /// Animate smooth pan to a location
    pub fn animate_pan_to(&mut self, lat: f64, lon: f64, _duration_ms: u64) {
        // For now, just pan directly
        // In a full implementation, you'd want to use gpui's animation system
        self.pan_to(lat, lon);
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

    /// Get viewport info for debugging
    pub fn debug_info(&self) -> String {
        format!(
            "Center: ({:.6}, {:.6}), Zoom: {:.2}, Bounds: ({:.6}, {:.6}) - ({:.6}, {:.6})",
            self.transform.center_lat,
            self.transform.center_lon,
            self.transform.zoom_level,
            self.transform.bounds.min_lat,
            self.transform.bounds.min_lon,
            self.transform.bounds.max_lat,
            self.transform.bounds.max_lon,
        )
    }
}

/// Viewport interaction handler trait for components that need to handle viewport events
pub trait ViewportInteraction {
    fn on_viewport_changed(&mut self, viewport: &Viewport);
    fn on_zoom_changed(&mut self, old_zoom: f64, new_zoom: f64);
    fn on_pan_changed(&mut self, old_center: (f64, f64), new_center: (f64, f64));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

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

    #[test]
    fn test_coordinate_conversion() {
        let screen_size = size(px(800.0), px(600.0));
        let viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        // Test round trip conversion
        let original_lat = 40.7500;
        let original_lon = -73.9000;
        let screen_point = viewport.geo_to_screen(original_lat, original_lon);
        let (converted_lat, converted_lon) = viewport.screen_to_geo(screen_point);

        assert!((converted_lat - original_lat).abs() < 0.001);
        assert!((converted_lon - original_lon).abs() < 0.001);
    }
}
