use crate::mercator::{get_tiles_for_bounds, TileCoord};
use crate::tiles::{Tile, TileManager};
use gpui::{
    px, AppContext, AsyncAppContext, Context, Pixels, Point, Render, Size, Task, WindowContext,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Background tile renderer that manages and draws map tiles
pub struct BackgroundRenderer {
    tile_manager: Arc<Mutex<TileManager>>,
    loaded_tiles: Arc<Mutex<HashMap<TileCoord, Tile>>>,
    loading_tasks: Vec<Task<()>>,
    tile_size: u32,
    enabled: bool,
}

impl BackgroundRenderer {
    /// Create a new background renderer with OSM tile server
    pub fn new() -> Self {
        let tile_manager = Arc::new(Mutex::new(TileManager::new(
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png".to_string(),
        )));

        Self {
            tile_manager,
            loaded_tiles: Arc::new(Mutex::new(HashMap::new())),
            loading_tasks: Vec::new(),
            tile_size: 256,
            enabled: true,
        }
    }

    /// Create a new background renderer with custom tile server
    pub fn new_with_server(base_url: String) -> Self {
        let tile_manager = Arc::new(Mutex::new(TileManager::new(base_url)));

        Self {
            tile_manager,
            loaded_tiles: Arc::new(Mutex::new(HashMap::new())),
            loading_tasks: Vec::new(),
            tile_size: 256,
            enabled: true,
        }
    }

    /// Enable or disable tile rendering
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.clear_tiles();
        }
    }

    /// Check if tile rendering is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Request tiles for the current viewport
    pub fn request_tiles_for_viewport(
        &mut self,
        center_lat: f64,
        center_lon: f64,
        zoom_level: f64,
        screen_size: Size<Pixels>,
        cx: &mut AsyncAppContext,
    ) {
        if !self.enabled {
            return;
        }

        // Calculate the geographic bounds with some padding for smooth panning
        let padding_factor = 1.5; // Load 50% more tiles around the viewport
        let lat_span = 180.0 / (2.0_f64.powf(zoom_level)) * padding_factor;
        let lon_span = 360.0 / (2.0_f64.powf(zoom_level)) * padding_factor;

        let min_lat = center_lat - lat_span / 2.0;
        let max_lat = center_lat + lat_span / 2.0;
        let min_lon = center_lon - lon_span / 2.0;
        let max_lon = center_lon + lon_span / 2.0;

        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;

        // Clean up old loading tasks
        self.loading_tasks.retain(|task| !task.is_finished());

        // Request tiles
        let tile_manager = self.tile_manager.clone();
        let loaded_tiles = self.loaded_tiles.clone();

        let task = cx.spawn(move |mut cx| async move {
            let tiles = {
                let mut manager = tile_manager.lock().unwrap();
                manager
                    .request_tiles_for_bounds(
                        min_lat, min_lon, max_lat, max_lon, tile_zoom, &mut cx,
                    )
                    .await
            };

            // Update loaded tiles
            {
                let mut loaded = loaded_tiles.lock().unwrap();
                for tile in tiles {
                    loaded.insert(tile.coord, tile);
                }
            }
        });

        self.loading_tasks.push(task);
    }

    /// Render tiles to the given canvas bounds
    pub fn render_tiles(
        &self,
        bounds: gpui::Bounds<Pixels>,
        center_lat: f64,
        center_lon: f64,
        zoom_level: f32,
        window: &mut gpui::Window,
    ) {
        if !self.enabled {
            return;
        }

        let loaded_tiles = self.loaded_tiles.lock().unwrap();
        if loaded_tiles.is_empty() {
            return;
        }

        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;

        // Calculate which tiles are visible in the current view
        let lat_span = 180.0 / (2.0_f64.powf(zoom_level as f64));
        let lon_span = 360.0 / (2.0_f64.powf(zoom_level as f64));

        let min_lat = center_lat - lat_span / 2.0;
        let max_lat = center_lat + lat_span / 2.0;
        let min_lon = center_lon - lon_span / 2.0;
        let max_lon = center_lon + lon_span / 2.0;

        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        // Calculate pixels per degree for positioning
        let pixels_per_degree_lat = bounds.size.height.to_f64() / lat_span;
        let pixels_per_degree_lon = bounds.size.width.to_f64() / lon_span;

        // Render each visible tile
        for tile_coord in visible_tiles {
            if let Some(tile) = loaded_tiles.get(&tile_coord) {
                self.render_single_tile(
                    tile,
                    bounds,
                    center_lat,
                    center_lon,
                    pixels_per_degree_lat,
                    pixels_per_degree_lon,
                    window,
                );
            }
        }
    }

    /// Render a single tile at the correct position
    fn render_single_tile(
        &self,
        tile: &Tile,
        bounds: gpui::Bounds<Pixels>,
        center_lat: f64,
        center_lon: f64,
        pixels_per_degree_lat: f64,
        pixels_per_degree_lon: f64,
        window: &mut gpui::Window,
    ) {
        // Get the geographic bounds of this tile
        let (tile_min_lon, tile_min_lat, tile_max_lon, tile_max_lat) =
            tile.coord.to_lat_lon_bounds();

        // Convert tile bounds to screen coordinates
        let tile_screen_min_x = ((tile_min_lon - center_lon) * pixels_per_degree_lon
            + bounds.size.width.to_f64() / 2.0) as f32;
        let tile_screen_max_x = ((tile_max_lon - center_lon) * pixels_per_degree_lon
            + bounds.size.width.to_f64() / 2.0) as f32;
        let tile_screen_min_y = ((center_lat - tile_max_lat) * pixels_per_degree_lat
            + bounds.size.height.to_f64() / 2.0) as f32;
        let tile_screen_max_y = ((center_lat - tile_min_lat) * pixels_per_degree_lat
            + bounds.size.height.to_f64() / 2.0) as f32;

        // Calculate tile screen bounds
        let tile_bounds = gpui::Bounds {
            origin: Point {
                x: px(tile_screen_min_x) + bounds.origin.x,
                y: px(tile_screen_min_y) + bounds.origin.y,
            },
            size: Size {
                width: px(tile_screen_max_x - tile_screen_min_x),
                height: px(tile_screen_max_y - tile_screen_min_y),
            },
        };

        // Only render if the tile is visible
        if tile_bounds.origin.x < bounds.size.width
            && tile_bounds.origin.x + tile_bounds.size.width > Pixels::ZERO
            && tile_bounds.origin.y < bounds.size.height
            && tile_bounds.origin.y + tile_bounds.size.height > Pixels::ZERO
        {
            // Convert image to GPUI format and paint it
            if let Ok(image_data) = self.convert_image_to_gpui(&tile.image) {
                window.paint_image(tile_bounds, image_data);
            }
        }
    }

    /// Convert a DynamicImage to GPUI image format
    fn convert_image_to_gpui(
        &self,
        image: &image::DynamicImage,
    ) -> Result<gpui::ImageData, Box<dyn std::error::Error>> {
        let rgba_image = image.to_rgba8();
        let (width, height) = rgba_image.dimensions();
        let raw_data = rgba_image.into_raw();

        Ok(gpui::ImageData::new(
            gpui::Size {
                width: px(width as f32),
                height: px(height as f32),
            },
            raw_data,
        ))
    }

    /// Get statistics about loaded tiles
    pub fn get_stats(&self) -> TileStats {
        let loaded_tiles = self.loaded_tiles.lock().unwrap();
        let tile_manager = self.tile_manager.lock().unwrap();
        let (cache_size, cache_capacity) = tile_manager.cache_stats();

        TileStats {
            loaded_tiles: loaded_tiles.len(),
            loading_tiles: tile_manager.loading_stats(),
            cache_size,
            cache_capacity,
            active_tasks: self.loading_tasks.len(),
        }
    }

    /// Clear all loaded tiles and cancel loading tasks
    pub fn clear_tiles(&mut self) {
        {
            let mut loaded_tiles = self.loaded_tiles.lock().unwrap();
            loaded_tiles.clear();
        }

        {
            let mut tile_manager = self.tile_manager.lock().unwrap();
            tile_manager.clear();
        }

        self.loading_tasks.clear();
    }

    /// Get the number of loaded tiles
    pub fn loaded_tile_count(&self) -> usize {
        let loaded_tiles = self.loaded_tiles.lock().unwrap();
        loaded_tiles.len()
    }

    /// Check if any tiles are currently loading
    pub fn is_loading(&self) -> bool {
        let tile_manager = self.tile_manager.lock().unwrap();
        tile_manager.loading_stats() > 0 || !self.loading_tasks.is_empty()
    }

    /// Set the tile size (default 256)
    pub fn set_tile_size(&mut self, size: u32) {
        self.tile_size = size;
    }

    /// Get the current tile size
    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }
}

/// Statistics about tile loading and caching
#[derive(Debug, Clone)]
pub struct TileStats {
    pub loaded_tiles: usize,
    pub loading_tiles: usize,
    pub cache_size: usize,
    pub cache_capacity: usize,
    pub active_tasks: usize,
}

impl TileStats {
    /// Get a formatted string representation of the stats
    pub fn format(&self) -> String {
        format!(
            "Tiles: {}/{} loaded, {} loading, {} tasks",
            self.cache_size, self.cache_capacity, self.loading_tiles, self.active_tasks
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    #[test]
    fn test_background_renderer_creation() {
        let renderer = BackgroundRenderer::new();
        assert!(renderer.is_enabled());
        assert_eq!(renderer.tile_size(), 256);
    }

    #[test]
    fn test_enable_disable() {
        let mut renderer = BackgroundRenderer::new();
        assert!(renderer.is_enabled());

        renderer.set_enabled(false);
        assert!(!renderer.is_enabled());

        renderer.set_enabled(true);
        assert!(renderer.is_enabled());
    }

    #[test]
    fn test_stats() {
        let renderer = BackgroundRenderer::new();
        let stats = renderer.get_stats();
        assert_eq!(stats.loaded_tiles, 0);
        assert_eq!(stats.active_tasks, 0);
    }
}
