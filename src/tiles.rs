use gpui::*;
use log::{debug, error, info};
use reqwest;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

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

/// Convert screen coordinates to tile-based coordinates
/// This is useful for determining which tile contains a given screen point
pub fn screen_to_tile_coords(
    screen_x: f32,
    screen_y: f32,
    bounds_width: f32,
    bounds_height: f32,
    center_lat: f64,
    center_lon: f64,
    zoom_level: f64,
) -> (f64, f64) {
    // Calculate the geographic span of the current view
    let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
    let lon_span = 360.0 / (2.0_f64.powf(zoom_level));

    // Convert screen coordinates to geographic coordinates
    let pixels_per_degree_lat = bounds_height as f64 / lat_span;
    let pixels_per_degree_lon = bounds_width as f64 / lon_span;

    let lon = center_lon + ((screen_x - bounds_width / 2.0) as f64 / pixels_per_degree_lon);
    let lat = center_lat - ((screen_y - bounds_height / 2.0) as f64 / pixels_per_degree_lat);

    (lat, lon)
}

/// Convert geographic coordinates to screen coordinates
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

/// Calculate the Web Mercator projection bounds for a given geographic viewport
pub fn calculate_mercator_bounds(
    center_lat: f64,
    center_lon: f64,
    zoom_level: f64,
    _screen_width: f32,
    _screen_height: f32,
) -> (f64, f64, f64, f64) {
    // For Web Mercator tiles, we need to calculate the bounds differently
    // This is a simplified calculation for now
    let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
    let lon_span = 360.0 / (2.0_f64.powf(zoom_level));

    let min_lat = center_lat - lat_span / 2.0;
    let max_lat = center_lat + lat_span / 2.0;
    let min_lon = center_lon - lon_span / 2.0;
    let max_lon = center_lon + lon_span / 2.0;

    (min_lat, min_lon, max_lat, max_lon)
}

/// Represents the loading state of a tile
#[derive(Debug, Clone)]
pub enum TileLoadState {
    NotLoaded,
    Loading,
    Loaded(SharedString), // Store URL for GPUI img() component
    Failed(String),
}

/// A tile with its loading state and metadata
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

/// Message types for tile loading communication
#[derive(Debug)]
pub enum TileMessage {
    RequestTile(TileCoord),
    TileLoaded(TileCoord),
    TileFailed(TileCoord, String),
}

/// Tile cache and loader manager
pub struct TileManager {
    tiles: Arc<Mutex<HashMap<TileCoord, Tile>>>,
    sender: mpsc::UnboundedSender<TileMessage>,
    receiver: mpsc::UnboundedReceiver<TileMessage>,
    request_sender: Option<mpsc::UnboundedSender<TileCoord>>,
    client: reqwest::Client,
    background_spawned: bool,
}

impl TileManager {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let client = reqwest::Client::builder()
            .user_agent("osm-gpui/0.1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            tiles: Arc::new(Mutex::new(HashMap::new())),
            sender,
            receiver,
            request_sender: None,
            client,
            background_spawned: false,
        }
    }

    /// Get a tile from the cache, initiating loading if necessary
    pub fn get_tile(&self, coord: TileCoord) -> Option<Tile> {
        let mut tiles = self.tiles.lock().unwrap();

        if let Some(tile) = tiles.get_mut(&coord) {
            match &tile.state {
                TileLoadState::NotLoaded => {
                    if !tile.loading_started {
                        // Mark as loading and request the tile
                        tile.state = TileLoadState::Loading;
                        tile.loading_started = true;

                        // Send request to background loader
                        if let Some(ref sender) = self.request_sender {
                            let _ = sender.send(coord);
                            debug!("🔄 Requesting tile: z{}/{}/{}", coord.z, coord.x, coord.y);
                        }
                    }
                    Some(tile.clone())
                }
                _ => Some(tile.clone()),
            }
        } else {
            // Create new tile and request loading
            let mut tile = Tile::new(coord);

            // Send request to background loader if available
            if let Some(ref sender) = self.request_sender {
                tile.state = TileLoadState::Loading;
                tile.loading_started = true;
                let _ = sender.send(coord);
                debug!(
                    "🆕 Creating new tile request: z{}/{}/{}",
                    coord.z, coord.x, coord.y
                );
            }

            tiles.insert(coord, tile.clone());
            Some(tile)
        }
    }

    /// Process incoming tile messages (call this regularly from main thread)
    pub fn process_messages(&mut self) -> bool {
        let mut processed_any = false;

        while let Ok(message) = self.receiver.try_recv() {
            processed_any = true;
            match message {
                TileMessage::TileLoaded(coord) => {
                    info!(
                        "✅ Tile loaded successfully: z{}/{}/{}",
                        coord.z, coord.x, coord.y
                    );
                    let mut tiles = self.tiles.lock().unwrap();
                    if let Some(tile) = tiles.get_mut(&coord) {
                        tile.state = TileLoadState::Loaded(tile.url.clone());
                    }
                }
                TileMessage::TileFailed(coord, error) => {
                    error!(
                        "❌ Tile failed to load: z{}/{}/{} - {}",
                        coord.z, coord.x, coord.y, error
                    );
                    let mut tiles = self.tiles.lock().unwrap();
                    if let Some(tile) = tiles.get_mut(&coord) {
                        tile.state = TileLoadState::Failed(error);
                    }
                }
                TileMessage::RequestTile(_) => {
                    // This shouldn't happen in normal operation
                    error!("Unexpected RequestTile message in process_messages");
                }
            }
        }

        processed_any
    }

    /// Initialize the background tile loader
    pub fn init_loader(&mut self) {
        if self.background_spawned {
            return;
        }

        let (request_sender, mut request_receiver) = mpsc::unbounded_channel::<TileCoord>();
        self.request_sender = Some(request_sender);

        let client = self.client.clone();
        let message_sender = self.sender.clone();

        // Spawn background task for tile loading
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");

            rt.block_on(async move {
                while let Some(coord) = request_receiver.recv().await {
                    let url = coord.to_url();
                    let client_clone = client.clone();
                    let sender_clone = message_sender.clone();

                    tokio::spawn(async move {
                        debug!("🌐 Downloading tile: {}", url);

                        match client_clone.get(&url).send().await {
                            Ok(response) => {
                                if response.status().is_success() {
                                    match response.bytes().await {
                                        Ok(_bytes) => {
                                            // Just mark as loaded - GPUI will handle the actual image loading
                                            let _ =
                                                sender_clone.send(TileMessage::TileLoaded(coord));
                                        }
                                        Err(e) => {
                                            let _ = sender_clone.send(TileMessage::TileFailed(
                                                coord,
                                                e.to_string(),
                                            ));
                                        }
                                    }
                                } else {
                                    let _ = sender_clone.send(TileMessage::TileFailed(
                                        coord,
                                        format!("HTTP {}", response.status()),
                                    ));
                                }
                            }
                            Err(e) => {
                                let _ = sender_clone
                                    .send(TileMessage::TileFailed(coord, e.to_string()));
                            }
                        }
                    });
                }
            });
        });

        self.background_spawned = true;
        debug!("🔧 TileManager background loader initialized");
    }

    /// Get statistics about the tile cache
    pub fn get_stats(&self) -> (usize, usize, usize, usize) {
        let tiles = self.tiles.lock().unwrap();
        let total = tiles.len();
        let mut loaded = 0;
        let mut loading = 0;
        let mut failed = 0;

        for tile in tiles.values() {
            match tile.state {
                TileLoadState::Loaded(_) => loaded += 1,
                TileLoadState::Loading => loading += 1,
                TileLoadState::Failed(_) => failed += 1,
                TileLoadState::NotLoaded => {}
            }
        }

        (total, loaded, loading, failed)
    }

    /// Clear the tile cache
    pub fn clear_cache(&self) {
        let mut tiles = self.tiles.lock().unwrap();
        tiles.clear();
        info!("🗑️ Tile cache cleared");
    }
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
