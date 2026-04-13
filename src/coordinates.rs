use gpui::{px, Pixels, Point as GpuiPoint, Size};
use std::f64::consts::PI;

/// Represents a geographic bounding box
#[derive(Debug, Clone, Copy)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn new(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> Self {
        Self {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lat >= self.min_lat && lat <= self.max_lat && lon >= self.min_lon && lon <= self.max_lon
    }

    pub fn width(&self) -> f64 {
        self.max_lon - self.min_lon
    }

    pub fn height(&self) -> f64 {
        self.max_lat - self.min_lat
    }

    pub fn center(&self) -> (f64, f64) {
        (
            (self.min_lat + self.max_lat) / 2.0,
            (self.min_lon + self.max_lon) / 2.0,
        )
    }
}

/// Web Mercator projection utilities
pub fn lat_lon_to_mercator(lat: f64, lon: f64) -> (f64, f64) {
    // Clamp latitude to avoid projection issues at poles
    let clamped_lat = lat.max(-85.051128779807).min(85.051128779807);

    // Standard Web Mercator projection (EPSG:3857)
    let x = lon * 20037508.34 / 180.0;
    let y = ((PI / 4.0) + (clamped_lat * PI / 360.0)).tan().ln() * 20037508.34 / PI;

    // Ensure finite values
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };

    (x, y)
}

fn mercator_to_lat_lon(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 180.0 / 20037508.34;
    let lat = (2.0 * (y * PI / 20037508.34).exp().atan() - PI / 2.0) * 180.0 / PI;

    // Ensure finite values and clamp to valid ranges
    let lat = if lat.is_finite() {
        lat.max(-85.051128779807).min(85.051128779807)
    } else {
        0.0
    };
    let lon = if lon.is_finite() {
        lon.max(-180.0).min(180.0)
    } else {
        0.0
    };

    (lat, lon)
}

/// Coordinate transformation system for converting between geographic and screen coordinates
/// Uses Web Mercator projection (EPSG:3857)
#[derive(Debug, Clone)]
pub struct CoordinateTransform {
    pub bounds: GeoBounds,
    pub screen_size: Size<Pixels>,
    pub zoom_level: f64,
    pub center_lat: f64,
    pub center_lon: f64,
    pub pixels_per_meter_x: f64,
    pub pixels_per_meter_y: f64,
    /// Visible extent in Web Mercator (EPSG:3857) meters. Cached so hot
    /// render paths can cull and project without recomputing lat/lon→mercator
    /// per node.
    pub mercator_min_x: f64,
    pub mercator_max_x: f64,
    pub mercator_min_y: f64,
    pub mercator_max_y: f64,
    pub mercator_center_x: f64,
    pub mercator_center_y: f64,
}

impl CoordinateTransform {
    pub fn new(
        center_lat: f64,
        center_lon: f64,
        zoom_level: f64,
        screen_size: Size<Pixels>,
    ) -> Self {
        // Convert center to Mercator coordinates
        let (center_x, center_y) = lat_lon_to_mercator(center_lat, center_lon);

        // Calculate the scale factor for this zoom level
        // At zoom level 0, the entire world (40075016.686 m) fits in 256 pixels
        let world_width_meters = 40075016.686; // Earth's circumference
        let tile_size = 256.0;
        let world_width_pixels = tile_size * 2.0_f64.powf(zoom_level);
        let meters_per_pixel = world_width_meters / world_width_pixels;

        let half_width_meters = (screen_size.width.0 as f64 / 2.0) * meters_per_pixel;
        let half_height_meters = (screen_size.height.0 as f64 / 2.0) * meters_per_pixel;

        let mercator_min_x = center_x - half_width_meters;
        let mercator_min_y = center_y - half_height_meters;
        let mercator_max_x = center_x + half_width_meters;
        let mercator_max_y = center_y + half_height_meters;

        let epsilon = 1e-6;
        let denom_x = (mercator_max_x - mercator_min_x).abs().max(epsilon);
        let denom_y = (mercator_max_y - mercator_min_y).abs().max(epsilon);
        let pixels_per_meter_x = screen_size.width.0 as f64 / denom_x;
        let pixels_per_meter_y = screen_size.height.0 as f64 / denom_y;

        // Calculate geographic bounds for compatibility
        let (min_lat, min_lon) = mercator_to_lat_lon(mercator_min_x, mercator_min_y);
        let (max_lat, max_lon) = mercator_to_lat_lon(mercator_max_x, mercator_max_y);
        let bounds = GeoBounds::new(
            min_lat.min(max_lat),
            max_lat.max(min_lat),
            min_lon.min(max_lon),
            max_lon.max(min_lon),
        );

        Self {
            bounds,
            screen_size,
            zoom_level,
            center_lat,
            center_lon,
            pixels_per_meter_x,
            pixels_per_meter_y,
            mercator_min_x,
            mercator_max_x,
            mercator_min_y,
            mercator_max_y,
            mercator_center_x: center_x,
            mercator_center_y: center_y,
        }
    }

    /// Project a point given in Mercator (EPSG:3857) meters directly to
    /// screen pixels. This is the trig-free fast path used by layers that
    /// cache mercator coordinates up front; it avoids re-running
    /// `lat_lon_to_mercator` per vertex every frame.
    #[inline]
    pub fn mercator_to_screen(&self, mx: f64, my: f64) -> GpuiPoint<Pixels> {
        let half_w = self.screen_size.width.0 / 2.0;
        let half_h = self.screen_size.height.0 / 2.0;
        let x = half_w + ((mx - self.mercator_center_x) * self.pixels_per_meter_x) as f32;
        let y = half_h - ((my - self.mercator_center_y) * self.pixels_per_meter_y) as f32;
        let x = if x.is_finite() { x } else { half_w };
        let y = if y.is_finite() { y } else { half_h };
        GpuiPoint { x: px(x), y: px(y) }
    }

    /// Cheap visibility test in Mercator space (no trig, no GeoBounds conversion).
    #[inline]
    pub fn mercator_in_view(&self, mx: f64, my: f64) -> bool {
        mx >= self.mercator_min_x
            && mx <= self.mercator_max_x
            && my >= self.mercator_min_y
            && my <= self.mercator_max_y
    }

    /// Convert geographic coordinates (lat, lon) to screen coordinates (x, y)
    pub fn geo_to_screen(&self, lat: f64, lon: f64) -> GpuiPoint<Pixels> {
        let (merc_x, merc_y) = lat_lon_to_mercator(lat, lon);
        let (center_merc_x, center_merc_y) = lat_lon_to_mercator(self.center_lat, self.center_lon);

        let merc_y_diff = merc_y - center_merc_y;
        let y_offset = merc_y_diff * self.pixels_per_meter_y;
        let y = (self.screen_size.height.0 / 2.0) - y_offset as f32;

        // eprintln!("geo_to_screen: lat={:.6}, lon={:.6}, merc_y={:.2}, center_merc_y={:.2}, merc_y_diff={:.2}, ppm_y={:.6}, y_offset={:.2}, y={:.2}", lat, lon, merc_y, center_merc_y, merc_y_diff, self.pixels_per_meter_y, y_offset, y);

        // Ensure finite screen coordinates
        let x = (self.screen_size.width.0 / 2.0)
            + ((merc_x - center_merc_x) * self.pixels_per_meter_x) as f32;
        let x = if x.is_finite() { x } else { self.screen_size.width.0 / 2.0 };
        let y = if y.is_finite() { y } else { self.screen_size.height.0 / 2.0 };

        GpuiPoint { x: px(x), y: px(y) }
    }

    /// Convert screen coordinates (x, y) to geographic coordinates (lat, lon)
    pub fn screen_to_geo(&self, point: GpuiPoint<Pixels>) -> (f64, f64) {
        let (center_merc_x, center_merc_y) = lat_lon_to_mercator(self.center_lat, self.center_lon);

        let merc_x = center_merc_x
            + ((point.x.0 - self.screen_size.width.0 / 2.0) as f64 / self.pixels_per_meter_x);
        let merc_y = center_merc_y
            - ((point.y.0 - self.screen_size.height.0 / 2.0) as f64 / self.pixels_per_meter_y); // Flip Y axis

        // Ensure finite mercator coordinates before conversion
        let merc_x = if merc_x.is_finite() {
            merc_x
        } else {
            center_merc_x
        };
        let merc_y = if merc_y.is_finite() {
            merc_y
        } else {
            center_merc_y
        };

        mercator_to_lat_lon(merc_x, merc_y)
    }

    /// Update the transform for a new center point (for panning)
    pub fn pan_to(&mut self, new_center_lat: f64, new_center_lon: f64) {
        // Validate inputs before updating
        let lat = if new_center_lat.is_finite() {
            new_center_lat.max(-85.0).min(85.0)
        } else {
            self.center_lat
        };
        let lon = if new_center_lon.is_finite() {
            new_center_lon.max(-180.0).min(180.0)
        } else {
            self.center_lon
        };

        *self = Self::new(lat, lon, self.zoom_level, self.screen_size);
    }

    /// Update the transform for a new zoom level
    pub fn zoom_to(&mut self, new_zoom_level: f64) {
        // Validate zoom level
        let zoom = if new_zoom_level.is_finite() {
            new_zoom_level.max(1.0).min(20.0)
        } else {
            self.zoom_level
        };

        *self = Self::new(self.center_lat, self.center_lon, zoom, self.screen_size);
    }

    /// Zoom in/out by a delta while keeping the zoom centered on a specific screen point
    pub fn zoom_at_point(&mut self, screen_point: GpuiPoint<Pixels>, zoom_delta: f64) {
        if !zoom_delta.is_finite() {
            return;
        }

        // Step 1: Convert screen point to Mercator coordinates at current zoom
        let (center_merc_x, center_merc_y) = lat_lon_to_mercator(self.center_lat, self.center_lon);
        let dx = (screen_point.x.0 - self.screen_size.width.0 / 2.0) as f64 / self.pixels_per_meter_x;
        let dy = -(screen_point.y.0 - self.screen_size.height.0 / 2.0) as f64 / self.pixels_per_meter_y;
        let mouse_merc_x = center_merc_x + dx;
        let mouse_merc_y = center_merc_y + dy;

        // Step 2: Update zoom level
        let new_zoom = (self.zoom_level + zoom_delta).max(1.0).min(20.0);
        let new_transform = Self::new(self.center_lat, self.center_lon, new_zoom, self.screen_size);

        // Step 3: Calculate new center so mouse_merc_x/y stays under the same screen pixel
        let new_dx = (screen_point.x.0 - self.screen_size.width.0 / 2.0) as f64 / new_transform.pixels_per_meter_x;
        let new_dy = -(screen_point.y.0 - self.screen_size.height.0 / 2.0) as f64 / new_transform.pixels_per_meter_y;
        let new_center_merc_x = mouse_merc_x - new_dx;
        let new_center_merc_y = mouse_merc_y - new_dy;
        let (new_center_lat, new_center_lon) = mercator_to_lat_lon(new_center_merc_x, new_center_merc_y);

        *self = Self::new(new_center_lat, new_center_lon, new_zoom, self.screen_size);
    }

    /// Pan by a screen pixel offset
    pub fn pan_by_pixels(&mut self, dx: f32, dy: f32) {
        // Validate inputs
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }

        let mercator_dx = dx as f64 / self.pixels_per_meter_x;
        let mercator_dy = -(dy as f64) / self.pixels_per_meter_y; // Negative because screen Y increases downward

        // Ensure finite mercator deltas
        let mercator_dx = if mercator_dx.is_finite() {
            mercator_dx
        } else {
            0.0
        };
        let mercator_dy = if mercator_dy.is_finite() {
            mercator_dy
        } else {
            0.0
        };

        let (center_merc_x, center_merc_y) = lat_lon_to_mercator(self.center_lat, self.center_lon);
        let new_center_merc_x = center_merc_x + mercator_dx;
        let new_center_merc_y = center_merc_y + mercator_dy;
        let (new_center_lat, new_center_lon) =
            mercator_to_lat_lon(new_center_merc_x, new_center_merc_y);

        self.pan_to(new_center_lat, new_center_lon);
    }

    /// Check if a geographic point is visible in the current view
    pub fn is_visible(&self, lat: f64, lon: f64) -> bool {
        self.bounds.contains(lat, lon)
    }

    /// Get the current visible geographic bounds
    pub fn visible_bounds(&self) -> GeoBounds {
        self.bounds
    }

    /// Get the current zoom level as used by tile servers (integer)
    pub fn tile_zoom_level(&self) -> u32 {
        self.zoom_level.round().max(0.0).min(20.0) as u32
    }
}

/// Validates that a point has finite coordinates
pub fn is_point_valid(point: GpuiPoint<Pixels>) -> bool {
    point.x.0.is_finite() && point.y.0.is_finite()
}

/// Creates a safe point with finite coordinates, falling back to defaults if needed
pub fn safe_point(x: f32, y: f32, default_x: f32, default_y: f32) -> GpuiPoint<Pixels> {
    let safe_x = if x.is_finite() { x } else { default_x };
    let safe_y = if y.is_finite() { y } else { default_y };
    GpuiPoint {
        x: px(safe_x),
        y: px(safe_y),
    }
}

/// Validates coordinates before using them in Lyon paths
pub fn validate_coords(lat: f64, lon: f64) -> Option<(f64, f64)> {
    if lat.is_finite()
        && lon.is_finite()
        && lat >= -90.0
        && lat <= 90.0
        && lon >= -180.0
        && lon <= 180.0
    {
        Some((lat, lon))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    #[test]
    fn test_coordinate_conversion() {
        let screen_size = size(px(800.0), px(600.0));
        let transform = CoordinateTransform::new(40.7128, -74.0060, 10.0, screen_size);

        // Test center point conversion
        let screen_center = transform.geo_to_screen(40.7128, -74.0060);
        assert!((screen_center.x.0 - 400.0).abs() < 1.0);
        assert!((screen_center.y.0 - 300.0).abs() < 1.0);

        // Test round trip conversion
        let original_lat = 40.7500;
        let original_lon = -73.9000;
        let screen_point = transform.geo_to_screen(original_lat, original_lon);
        let (converted_lat, converted_lon) = transform.screen_to_geo(screen_point);

        assert!((converted_lat - original_lat).abs() < 0.001);
        assert!((converted_lon - original_lon).abs() < 0.001);
    }
}
