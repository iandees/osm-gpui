use std::f64::consts::PI;

/// Web Mercator projection utilities for working with map tiles
/// Based on EPSG:3857 / Spherical Mercator projection used by most web mapping services

/// Convert latitude/longitude to Web Mercator coordinates
pub fn lat_lon_to_mercator(lat: f64, lon: f64) -> (f64, f64) {
    let x = lon * 20037508.34 / 180.0;
    let y = ((90.0 + lat) * PI / 360.0).tan().ln() / (PI / 180.0);
    let y = y * 20037508.34 / 180.0;
    (x, y)
}

/// Convert Web Mercator coordinates to latitude/longitude
pub fn mercator_to_lat_lon(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 180.0 / 20037508.34;
    let lat = (y * 180.0 / 20037508.34).to_radians().exp().atan() * 360.0 / PI - 90.0;
    (lat, lon)
}

/// Tile coordinate structure
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

impl TileCoord {
    pub fn new(x: u32, y: u32, z: u32) -> Self {
        Self { x, y, z }
    }

    /// Get the bounding box of this tile in Web Mercator coordinates
    pub fn to_mercator_bounds(&self) -> (f64, f64, f64, f64) {
        let n = 2.0_f64.powi(self.z as i32);
        let lon_min = self.x as f64 / n * 360.0 - 180.0;
        let lon_max = (self.x + 1) as f64 / n * 360.0 - 180.0;

        let lat_rad_min = ((PI * (1.0 - 2.0 * (self.y + 1) as f64 / n)).sinh()).atan();
        let lat_rad_max = ((PI * (1.0 - 2.0 * self.y as f64 / n)).sinh()).atan();

        let lat_min = lat_rad_min * 180.0 / PI;
        let lat_max = lat_rad_max * 180.0 / PI;

        (lon_min, lat_min, lon_max, lat_max)
    }

    /// Get the bounding box of this tile in latitude/longitude
    pub fn to_lat_lon_bounds(&self) -> (f64, f64, f64, f64) {
        self.to_mercator_bounds()
    }

    /// Convert tile coordinates to pixel coordinates within the tile
    pub fn to_pixel_bounds(&self, tile_size: u32) -> (u32, u32, u32, u32) {
        let x_min = self.x * tile_size;
        let y_min = self.y * tile_size;
        let x_max = x_min + tile_size;
        let y_max = y_min + tile_size;
        (x_min, y_min, x_max, y_max)
    }
}

/// Convert latitude/longitude to tile coordinates at a given zoom level
pub fn lat_lon_to_tile(lat: f64, lon: f64, zoom: u32) -> TileCoord {
    let lat_rad = lat.to_radians();
    let n = 2.0_f64.powi(zoom as i32);

    let x = ((lon + 180.0) / 360.0 * n).floor() as u32;
    let y = ((1.0 - lat_rad.tan().asinh() / PI) / 2.0 * n).floor() as u32;

    TileCoord::new(x, y, zoom)
}

/// Get all tile coordinates that are visible in the given geographic bounds at a zoom level
pub fn get_tiles_for_bounds(
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
    zoom: u32,
) -> Vec<TileCoord> {
    let min_tile = lat_lon_to_tile(max_lat, min_lon, zoom); // Note: max_lat for min_tile.y
    let max_tile = lat_lon_to_tile(min_lat, max_lon, zoom); // Note: min_lat for max_tile.y

    let mut tiles = Vec::new();

    for x in min_tile.x..=max_tile.x {
        for y in min_tile.y..=max_tile.y {
            tiles.push(TileCoord::new(x, y, zoom));
        }
    }

    tiles
}

/// Convert pixel coordinates to tile coordinates
pub fn pixel_to_tile(pixel_x: f64, pixel_y: f64, zoom: u32, tile_size: u32) -> TileCoord {
    let tile_x = (pixel_x / tile_size as f64).floor() as u32;
    let tile_y = (pixel_y / tile_size as f64).floor() as u32;
    TileCoord::new(tile_x, tile_y, zoom)
}

/// Convert tile coordinates to pixel coordinates (top-left corner of tile)
pub fn tile_to_pixel(tile: &TileCoord, tile_size: u32) -> (f64, f64) {
    let pixel_x = tile.x as f64 * tile_size as f64;
    let pixel_y = tile.y as f64 * tile_size as f64;
    (pixel_x, pixel_y)
}

/// Calculate the resolution (meters per pixel) at a given latitude and zoom level
pub fn resolution_at_latitude(lat: f64, zoom: u32) -> f64 {
    let earth_circumference = 40075016.686; // Earth's circumference in meters
    let lat_rad = lat.to_radians();
    let tiles_at_zoom = 2.0_f64.powi(zoom as i32);
    let tile_size = 256.0; // Standard tile size in pixels

    (earth_circumference * lat_rad.cos()) / (tiles_at_zoom * tile_size)
}

/// Convert Web Mercator coordinates to tile pixel coordinates at a given zoom level
pub fn mercator_to_tile_pixel(
    mercator_x: f64,
    mercator_y: f64,
    zoom: u32,
    tile_size: u32,
) -> (f64, f64) {
    let world_size = tile_size as f64 * 2.0_f64.powi(zoom as i32);
    let pixel_x = (mercator_x + 20037508.34) / (2.0 * 20037508.34) * world_size;
    let pixel_y = (20037508.34 - mercator_y) / (2.0 * 20037508.34) * world_size;
    (pixel_x, pixel_y)
}

/// Convert tile pixel coordinates to Web Mercator coordinates
pub fn tile_pixel_to_mercator(pixel_x: f64, pixel_y: f64, zoom: u32, tile_size: u32) -> (f64, f64) {
    let world_size = tile_size as f64 * 2.0_f64.powi(zoom as i32);
    let mercator_x = (pixel_x / world_size) * (2.0 * 20037508.34) - 20037508.34;
    let mercator_y = 20037508.34 - (pixel_y / world_size) * (2.0 * 20037508.34);
    (mercator_x, mercator_y)
}

/// Calculate the best zoom level for a given geographic bounds and screen size
pub fn calculate_best_zoom(
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
    screen_width: f64,
    screen_height: f64,
) -> u32 {
    let lat_span = max_lat - min_lat;
    let lon_span = max_lon - min_lon;

    // Calculate zoom level based on longitude span (more reliable than latitude)
    let zoom_lon = (360.0 / lon_span).log2().floor();

    // Calculate zoom level based on latitude span (using Web Mercator)
    let lat_rad_min = min_lat.to_radians();
    let lat_rad_max = max_lat.to_radians();
    let mercator_height = lat_rad_max.tan().asinh() - lat_rad_min.tan().asinh();
    let zoom_lat = (2.0 * PI / mercator_height).log2().floor();

    // Take the minimum to ensure both dimensions fit
    let zoom = zoom_lon.min(zoom_lat).max(1.0).min(18.0) as u32;

    zoom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lat_lon_to_tile() {
        // Test known coordinates
        let tile = lat_lon_to_tile(51.5074, -0.1278, 10); // London
        assert!(tile.x > 0 && tile.y > 0 && tile.z == 10);

        // Test equator and prime meridian
        let tile = lat_lon_to_tile(0.0, 0.0, 1);
        assert_eq!(tile.x, 1);
        assert_eq!(tile.y, 1);
        assert_eq!(tile.z, 1);
    }

    #[test]
    fn test_tile_bounds() {
        let tile = TileCoord::new(1, 1, 1);
        let (lon_min, lat_min, lon_max, lat_max) = tile.to_lat_lon_bounds();

        // At zoom level 1, tile (1,1) should cover southeastern quadrant
        assert!(lon_min >= 0.0 && lon_max <= 180.0);
        assert!(lat_min >= -85.0 && lat_max <= 0.0);
    }

    #[test]
    fn test_mercator_conversion() {
        let lat = 51.5074;
        let lon = -0.1278;

        let (x, y) = lat_lon_to_mercator(lat, lon);
        let (lat2, lon2) = mercator_to_lat_lon(x, y);

        assert!((lat - lat2).abs() < 0.0001);
        assert!((lon - lon2).abs() < 0.0001);
    }

    #[test]
    fn test_get_tiles_for_bounds() {
        let tiles = get_tiles_for_bounds(51.0, -1.0, 52.0, 0.0, 5);
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|t| t.z == 5));
    }
}
