use gpui::*;
use std::sync::{Arc, Mutex};

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::tile_cache::TileCache;
use crate::tiles::get_tiles_for_bounds;

/// Layer for rendering raster map tiles
pub struct TileLayer {
    name: String,
    visible: bool,
    tile_cache: Arc<Mutex<TileCache>>,
    show_boundaries: bool,
}

impl TileLayer {
    pub fn new(tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self::new_with_name("OpenStreetMap Carto".to_string(), tile_cache)
    }

    pub fn new_with_name(name: String, tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self {
            name,
            visible: true,
            tile_cache,
            show_boundaries: false,
        }
    }

    pub fn set_show_boundaries(&mut self, show: bool) {
        self.show_boundaries = show;
    }
}

impl MapLayer for TileLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn render_elements(&self, viewport: &Viewport) -> Vec<AnyElement> {
        let mut elements = Vec::new();

        let zoom_level = viewport.zoom_level();
        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        for tile_coord in &visible_tiles {
            // Calculate tile position in screen coordinates
            let (tile_min_lon, tile_min_lat, tile_max_lon, tile_max_lat) =
                tile_coord.to_lat_lon_bounds();

            let screen_top_left = viewport.geo_to_screen(tile_max_lat, tile_min_lon);
            let screen_bottom_right = viewport.geo_to_screen(tile_min_lat, tile_max_lon);

            // Calculate tile screen position and size
            // Note: In screen coordinates, y increases downward
            // tile_max_lat (north) -> smaller y (top)
            // tile_min_lat (south) -> larger y (bottom)
            let tile_x = screen_top_left.x.0;
            let tile_y = screen_top_left.y.0;
            let tile_width = (screen_bottom_right.x.0 - screen_top_left.x.0).abs();
            let tile_height = (screen_bottom_right.y.0 - screen_top_left.y.0).abs();

            // Generate tile URL
            let tile_url = tile_coord.to_url();

            // Create tile element using GPUI's img with asset loading
            let tile_element = div()
                .absolute()
                .left(px(tile_x))
                .top(px(tile_y))
                .w(px(tile_width))
                .h(px(tile_height))
                .bg(rgb(0x2d3748)) // Default background
                .child(
                    // Use GPUI's img with asset loading system
                    img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        window.use_asset::<crate::tile_cache::TileAsset>(&tile_url, cx)
                    })
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                        .with_loading(|| {
                            div()
                                .size_full()
                                .bg(rgb(0x4a5568))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_color(rgb(0xffffff))
                                        .text_xs()
                                        .child("Downloading...")
                                )
                                .into_any_element()
                        })
                        .with_fallback(|| {
                            div()
                                .size_full()
                                .bg(rgb(0x9f1239))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_color(rgb(0xffffff))
                                        .text_xs()
                                        .child("Failed")
                                )
                                .into_any_element()
                        })
                        .into_any_element()
                )
                .into_any_element();

            elements.push(tile_element);
        }

        elements
    }

    fn render_canvas(&self, viewport: &Viewport, _bounds: Bounds<Pixels>, window: &mut Window) {
        if !self.show_boundaries {
            return;
        }

        // Render tile boundaries for debugging
        let zoom_level = viewport.zoom_level();
        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        let tile_color = rgb(0x4a5568);
        for tile_coord in &visible_tiles {
            let (tile_min_lon, tile_min_lat, tile_max_lon, tile_max_lat) =
                tile_coord.to_lat_lon_bounds();

            // Use the same coordinate logic as in render_elements for consistency
            let screen_top_left = viewport.geo_to_screen(tile_max_lat, tile_min_lon);
            let screen_bottom_right = viewport.geo_to_screen(tile_min_lat, tile_max_lon);

            // Validate coordinates before using in Lyon path
            use crate::coordinates::is_point_valid;
            if is_point_valid(screen_top_left) && is_point_valid(screen_bottom_right) {
                // Draw tile boundary rectangle
                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(screen_top_left.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_bottom_right.y));
                builder.line_to(point(screen_top_left.x, screen_bottom_right.y));
                builder.close();

                if let Ok(path) = builder.build() {
                    window.paint_path(path, tile_color);
                }
            }
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        let (cached_files, active_downloads) = if let Ok(tile_cache) = self.tile_cache.try_lock() {
            tile_cache.stats()
        } else {
            (0, 0)
        };

        vec![
            ("Cached Files".to_string(), cached_files.to_string()),
            ("Active Downloads".to_string(), active_downloads.to_string()),
            ("Show Boundaries".to_string(), self.show_boundaries.to_string()),
        ]
    }
}
