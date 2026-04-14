use gpui::*;
use std::sync::{Arc, Mutex};

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::tile_cache::TileCache;
use crate::tiles::{get_tiles_for_bounds, url_from_template};

/// The built-in OpenStreetMap Carto tile URL template.
pub const OSM_CARTO_TEMPLATE: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

/// Layer for rendering raster map tiles
pub struct TileLayer {
    name: String,
    url_template: String,
    visible: bool,
    tile_cache: Arc<Mutex<TileCache>>,
    show_boundaries: bool,
}

impl TileLayer {
    pub fn new(tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self::new_with_template(
            "OpenStreetMap Carto".to_string(),
            OSM_CARTO_TEMPLATE.to_string(),
            tile_cache,
        )
    }

    pub fn new_with_name(name: String, tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self::new_with_template(name, OSM_CARTO_TEMPLATE.to_string(), tile_cache)
    }

    pub fn new_with_template(
        name: String,
        url_template: String,
        tile_cache: Arc<Mutex<TileCache>>,
    ) -> Self {
        Self {
            name,
            url_template,
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
            let tile_x = screen_top_left.x;
            let tile_y = screen_top_left.y;
            let tile_width = (screen_bottom_right.x - screen_top_left.x).abs();
            let tile_height = (screen_bottom_right.y - screen_top_left.y).abs();

            // Generate tile URL via the layer's URL template.
            let tile_url = url_from_template(&self.url_template, tile_coord);

            // Parent-tile fallback: while the child tile is loading, show the
            // already-cached parent (z-1) tile scaled 2× and clipped to this
            // child's quadrant. Prevents the dark "downloading" flash.
            let parent_fallback = tile_coord.parent().map(|parent_coord| {
                let (qx, qy) = tile_coord.quadrant_in_parent();
                let parent_url = url_from_template(&self.url_template, &parent_coord);
                div()
                    .absolute()
                    .left(-tile_width * qx as f32)
                    .top(-tile_height * qy as f32)
                    .w(tile_width * 2.0)
                    .h(tile_height * 2.0)
                    .child(
                        img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                            window.use_asset::<crate::tile_cache::TileAsset>(&parent_url, cx)
                        })
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .into_any_element()
            });

            // Create tile element using GPUI's img with asset loading
            let mut tile_element = div()
                .absolute()
                .left(tile_x)
                .top(tile_y)
                .w(tile_width)
                .h(tile_height)
                .overflow_hidden()
                .bg(rgb(0x2d3748)); // Ultimate fallback background

            if let Some(parent_el) = parent_fallback {
                tile_element = tile_element.child(parent_el);
            }

            let tile_element = tile_element
                .child(
                    // Use GPUI's img with asset loading system
                    img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        window.use_asset::<crate::tile_cache::TileAsset>(&tile_url, cx)
                    })
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
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
