use gpui::SharedString;
use std::f64::consts::PI;

/// A tile coordinate in the Web Mercator projection
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

    /// Get the bounding box of this tile in lat/lon coordinates
    pub fn to_lat_lon_bounds(&self) -> (f64, f64, f64, f64) {
        let n = 2.0_f64.powi(self.z as i32);
        let lon_min = self.x as f64 / n * 360.0 - 180.0;
        let lon_max = (self.x + 1) as f64 / n * 360.0 - 180.0;

        let lat_rad_min = ((PI * (1.0 - 2.0 * (self.y + 1) as f64 / n)).sinh()).atan();
        let lat_rad_max = ((PI * (1.0 - 2.0 * self.y as f64 / n)).sinh()).atan();

        let lat_min = lat_rad_min * 180.0 / PI;
        let lat_max = lat_rad_max * 180.0 / PI;

        (lon_min, lat_min, lon_max, lat_max)
    }

    /// Generate the URL for this tile from the OSM tile server
    pub fn to_url(&self) -> String {
        format!(
            "https://tile.openstreetmap.org/{}/{}/{}.png",
            self.z, self.x, self.y
        )
    }

    /// The parent tile (one zoom level up), or `None` at z==0.
    pub fn parent(&self) -> Option<TileCoord> {
        if self.z == 0 {
            return None;
        }
        Some(TileCoord {
            x: self.x / 2,
            y: self.y / 2,
            z: self.z - 1,
        })
    }

    /// Position of this tile within its parent's 2×2 grid.
    /// Returns (column, row) where each is 0 or 1:
    /// (0, 0) = top-left, (1, 0) = top-right, (0, 1) = bottom-left, (1, 1) = bottom-right.
    pub fn quadrant_in_parent(&self) -> (u32, u32) {
        (self.x % 2, self.y % 2)
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

// Legacy functions kept for compatibility but no longer used with GPUI assets
pub fn screen_to_tile_coords(
    screen_x: f32,
    screen_y: f32,
    bounds_width: f32,
    bounds_height: f32,
    center_lat: f64,
    center_lon: f64,
    zoom_level: f64,
) -> (f64, f64) {
    let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
    let lon_span = 360.0 / (2.0_f64.powf(zoom_level));

    let pixels_per_degree_lat = bounds_height as f64 / lat_span;
    let pixels_per_degree_lon = bounds_width as f64 / lon_span;

    let lon = center_lon + ((screen_x - bounds_width / 2.0) as f64 / pixels_per_degree_lon);
    let lat = center_lat - ((screen_y - bounds_height / 2.0) as f64 / pixels_per_degree_lat);

    (lat, lon)
}

pub fn geo_to_screen(
    lat: f64,
    lon: f64,
    bounds_width: f32,
    bounds_height: f32,
    center_lat: f64,
    center_lon: f64,
    zoom_level: f64,
) -> (f32, f32) {
    let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
    let lon_span = 360.0 / (2.0_f64.powf(zoom_level));

    let pixels_per_degree_lat = bounds_height as f64 / lat_span;
    let pixels_per_degree_lon = bounds_width as f64 / lon_span;

    let x = (bounds_width / 2.0) + ((lon - center_lon) * pixels_per_degree_lon) as f32;
    let y = (bounds_height / 2.0) - ((lat - center_lat) * pixels_per_degree_lat) as f32;

    (x, y)
}

pub fn calculate_mercator_bounds(
    center_lat: f64,
    center_lon: f64,
    zoom_level: f64,
    _screen_width: f32,
    _screen_height: f32,
) -> (f64, f64, f64, f64) {
    let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
    let lon_span = 360.0 / (2.0_f64.powf(zoom_level));

    let min_lat = center_lat - lat_span / 2.0;
    let max_lat = center_lat + lat_span / 2.0;
    let min_lon = center_lon - lon_span / 2.0;
    let max_lon = center_lon + lon_span / 2.0;

    (min_lat, min_lon, max_lat, max_lon)
}

// Legacy types kept for compatibility
#[derive(Debug, Clone)]
pub enum TileLoadState {
    NotLoaded,
    Loading,
    Loaded(SharedString),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Tile {
    pub coord: TileCoord,
    pub state: TileLoadState,
    pub url: SharedString,
    pub loading_started: bool,
}

impl Tile {
    pub fn new(coord: TileCoord) -> Self {
        let url = coord.to_url().into();
        Self {
            coord,
            state: TileLoadState::NotLoaded,
            url,
            loading_started: false,
        }
    }
}

#[derive(Debug)]
pub enum TileMessage {
    RequestTile(TileCoord),
    TileLoaded(TileCoord),
    TileFailed(TileCoord, String),
}

// Legacy TileManager - no longer used with GPUI assets
pub struct TileManager;

impl TileManager {
    pub fn new() -> Self {
        Self
    }

    pub fn get_tile(&self, coord: TileCoord) -> Option<Tile> {
        Some(Tile::new(coord))
    }

    pub fn process_messages(&mut self) -> bool {
        false
    }

    pub fn init_loader(&mut self) {
        // No-op - GPUI handles loading now
    }

    pub fn get_stats(&self) -> (usize, usize, usize, usize) {
        (0, 0, 0, 0)
    }

    pub fn clear_cache(&self) {
        // No-op - GPUI handles caching now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lat_lon_to_tile() {
        let tile = lat_lon_to_tile(51.5074, -0.1278, 10); // London
        assert!(tile.x > 0 && tile.y > 0 && tile.z == 10);

        let tile = lat_lon_to_tile(0.0, 0.0, 1);
        assert_eq!(tile.x, 1);
        assert_eq!(tile.y, 1);
        assert_eq!(tile.z, 1);
    }

    #[test]
    fn test_tile_bounds() {
        let tile = TileCoord::new(1, 1, 1);
        let (lon_min, lat_min, lon_max, lat_max) = tile.to_lat_lon_bounds();
        assert!(lon_min >= 0.0 && lon_max <= 180.0);
        // Web Mercator bounds are ~±85.0511°, so -85.0 is too tight.
        assert!(lat_min >= -85.1 && lat_max <= 0.0);
    }

    #[test]
    fn test_get_tiles_for_bounds() {
        let tiles = get_tiles_for_bounds(51.0, -1.0, 52.0, 0.0, 5);
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|t| t.z == 5));
    }

    #[test]
    fn test_tile_url_generation() {
        let tile = TileCoord::new(123, 456, 10);
        let url = tile.to_url();
        assert_eq!(url, "https://tile.openstreetmap.org/10/123/456.png");
    }

    #[test]
    fn test_tile_parent() {
        assert_eq!(TileCoord::new(0, 0, 0).parent(), None);
        assert_eq!(TileCoord::new(2, 3, 2).parent(), Some(TileCoord::new(1, 1, 1)));
        assert_eq!(TileCoord::new(5, 7, 3).parent(), Some(TileCoord::new(2, 3, 2)));
    }

    #[test]
    fn test_tile_quadrant_in_parent() {
        assert_eq!(TileCoord::new(2, 2, 2).quadrant_in_parent(), (0, 0));
        assert_eq!(TileCoord::new(3, 2, 2).quadrant_in_parent(), (1, 0));
        assert_eq!(TileCoord::new(2, 3, 2).quadrant_in_parent(), (0, 1));
        assert_eq!(TileCoord::new(3, 3, 2).quadrant_in_parent(), (1, 1));
    }

    #[test]
    fn test_tile_creation() {
        let coord = TileCoord::new(1, 2, 3);
        let tile = Tile::new(coord);
        assert_eq!(tile.coord, coord);
        assert_eq!(
            tile.url,
            SharedString::from("https://tile.openstreetmap.org/3/1/2.png")
        );
        matches!(tile.state, TileLoadState::NotLoaded);
        assert!(!tile.loading_started);
    }
}
