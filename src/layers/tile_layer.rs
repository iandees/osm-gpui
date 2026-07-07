use gpui::*;
use std::sync::{Arc, Mutex};

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::tile_cache::TileCache;
use crate::tiles::{get_tiles_for_bounds, url_from_template, TileCoord};

/// The built-in OpenStreetMap Carto tile URL template.
pub const OSM_CARTO_TEMPLATE: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

/// Layer for rendering raster map tiles
pub struct TileLayer {
    name: String,
    url_template: String,
    visible: bool,
    tile_cache: Arc<Mutex<TileCache>>,
    show_boundaries: bool,
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
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
            min_zoom: None,
            max_zoom: None,
        }
    }

    pub fn with_min_zoom(mut self, min_zoom: Option<u32>) -> Self {
        self.min_zoom = min_zoom;
        self
    }

    pub fn with_max_zoom(mut self, max_zoom: Option<u32>) -> Self {
        self.max_zoom = max_zoom;
        self
    }

    pub fn set_show_boundaries(&mut self, show: bool) {
        self.show_boundaries = show;
    }

    /// Compute the tile zoom level to request for a given viewport zoom,
    /// honoring the layer's optional `min_zoom` / `max_zoom` ELI bounds.
    pub fn effective_tile_zoom(&self, viewport_z: f64) -> Option<u32> {
        compute_effective_tile_zoom(viewport_z, self.min_zoom, self.max_zoom)
    }
}

/// Compute the tile zoom level to request for a given viewport zoom,
/// honoring optional `min_zoom` / `max_zoom` ELI bounds.
///
/// Returns `None` when the layer should not draw at all (viewport is
/// below `min_zoom` or more than one level above `max_zoom`).
/// When viewport zoom rounds to exactly `max_zoom + 1`, returns
/// `Some(max_zoom)` so we overzoom by one level. Otherwise returns
/// the rounded viewport zoom, capped at the global limit of 18.
fn compute_effective_tile_zoom(
    viewport_z: f64,
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
) -> Option<u32> {
    let rounded = viewport_z.round().max(0.0).min(18.0) as u32;
    if let Some(min_z) = min_zoom {
        if rounded < min_z {
            return None;
        }
    }
    if let Some(max_z) = max_zoom {
        if rounded > max_z + 1 {
            return None;
        }
        if rounded > max_z {
            return Some(max_z);
        }
    }
    Some(rounded)
}

/// Shared per-frame geometry for a set of same-zoom tiles: one rounded
/// anchor position plus a rounded, uniform tile size. Every tile's screen
/// rect is derived from this by pure integer tile-index arithmetic (see
/// `tile_screen_rect`), rather than each tile independently projecting and
/// rounding its own corners.
///
/// This matters during panning: independently rounding each tile's own
/// (continuously changing) float position means different tiles cross
/// their own rounding boundary at different moments, so the grid visibly
/// jiggles — neighboring tiles pop by a pixel at different times relative
/// to the same drag motion. Deriving every tile from one shared anchor and
/// tile size means the whole grid snaps together at once: no per-tile
/// desync, and (since all tiles at a given zoom are the same size on the
/// Web Mercator grid) no antialiased seam either.
struct TileGridGeometry {
    anchor_x: Pixels,
    anchor_y: Pixels,
    tile_width: Pixels,
    tile_height: Pixels,
    min_x: u32,
    min_y: u32,
}

fn compute_tile_grid_geometry(
    viewport: &Viewport,
    visible_tiles: &[TileCoord],
    tile_zoom: u32,
) -> Option<TileGridGeometry> {
    let min_x = visible_tiles.iter().map(|t| t.x).min()?;
    let min_y = visible_tiles.iter().map(|t| t.y).min()?;

    // Web Mercator (EPSG:3857) half-width in meters, matching the constant
    // used by `coordinates::lat_lon_to_mercator`. The XYZ tile scheme is a
    // uniform grid directly in this projected space, so every tile at a
    // given zoom is exactly the same size — no trig needed to find it.
    let world_half = 20037508.34_f64;
    let n = 2.0_f64.powi(tile_zoom as i32);
    let tile_size_m = (world_half * 2.0) / n;

    let anchor_merc_x = -world_half + min_x as f64 * tile_size_m;
    let anchor_merc_y = world_half - min_y as f64 * tile_size_m;
    let anchor_raw = viewport.mercator_to_screen(anchor_merc_x, anchor_merc_y);

    let raw_tile_width = tile_size_m * viewport.transform.pixels_per_meter_x;
    let raw_tile_height = tile_size_m * viewport.transform.pixels_per_meter_y;

    Some(TileGridGeometry {
        anchor_x: anchor_raw.x.round(),
        anchor_y: anchor_raw.y.round(),
        tile_width: px(raw_tile_width.round() as f32),
        tile_height: px(raw_tile_height.round() as f32),
        min_x,
        min_y,
    })
}

/// Screen rect (x, y, width, height) for one tile, derived from the shared
/// grid geometry by integer tile-index arithmetic only.
fn tile_screen_rect(geom: &TileGridGeometry, tile: &TileCoord) -> (Pixels, Pixels, Pixels, Pixels) {
    let delta_col = (tile.x - geom.min_x) as f32;
    let delta_row = (tile.y - geom.min_y) as f32;
    let x = geom.anchor_x + geom.tile_width * delta_col;
    let y = geom.anchor_y + geom.tile_height * delta_row;
    (x, y, geom.tile_width, geom.tile_height)
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
        let Some(tile_zoom) = self.effective_tile_zoom(zoom_level) else {
            return elements;
        };
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);
        let Some(geom) = compute_tile_grid_geometry(viewport, &visible_tiles, tile_zoom) else {
            return elements;
        };

        for tile_coord in &visible_tiles {
            // Calculate tile screen position and size from the shared grid
            // geometry (integer tile-index arithmetic; see `TileGridGeometry`).
            let (tile_x, tile_y, tile_width, tile_height) = tile_screen_rect(&geom, tile_coord);

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

            // Pick a character budget for the fallback message based on
            // the on-screen tile width: roughly one char per 6 px, clamped
            // to a sensible range so very small tiles still get something.
            let char_budget = ((f32::from(tile_width) / 6.0) as usize).clamp(8, 40);
            let fallback_url = tile_url.clone();
            let asset_url = tile_url;

            let tile_element = tile_element
                .child(
                    // Use GPUI's img with asset loading system
                    img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        window.use_asset::<crate::tile_cache::TileAsset>(&asset_url, cx)
                    })
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                        .with_fallback(move || {
                            let reason = crate::tile_cache::last_error(&fallback_url)
                                .unwrap_or_else(|| "Failed".to_string());
                            let display = crate::tile_cache::truncate_middle(&reason, char_budget);
                            div()
                                .size_full()
                                .bg(rgb(0x9f1239))
                                .overflow_hidden()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_color(rgb(0xffffff))
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .child(display)
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
        let Some(tile_zoom) = self.effective_tile_zoom(zoom_level) else {
            return;
        };
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);
        let Some(geom) = compute_tile_grid_geometry(viewport, &visible_tiles, tile_zoom) else {
            return;
        };

        let tile_color = rgb(0x4a5568);
        for tile_coord in &visible_tiles {
            // Use the same grid geometry as render_elements for consistency
            let (tile_x, tile_y, tile_width, tile_height) = tile_screen_rect(&geom, tile_coord);
            let screen_top_left = point(tile_x, tile_y);
            let screen_bottom_right = point(tile_x + tile_width, tile_y + tile_height);

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

        let mut stats = vec![
            ("Cached Files".to_string(), cached_files.to_string()),
            ("Active Downloads".to_string(), active_downloads.to_string()),
            ("Show Boundaries".to_string(), self.show_boundaries.to_string()),
        ];
        if let Some(min_z) = self.min_zoom {
            stats.push(("Min Zoom".to_string(), min_z.to_string()));
        }
        if let Some(max_z) = self.max_zoom {
            stats.push(("Max Zoom".to_string(), max_z.to_string()));
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_effective_tile_zoom, compute_tile_grid_geometry, tile_screen_rect};
    use crate::tiles::TileCoord;
    use crate::viewport::Viewport;
    use gpui::{px, size};

    fn tiles_and_geom(viewport: &Viewport, z: u32) -> (Vec<TileCoord>, super::TileGridGeometry) {
        let tiles = vec![
            TileCoord::new(1206, 1539, z),
            TileCoord::new(1207, 1539, z),
            TileCoord::new(1206, 1540, z),
            TileCoord::new(1207, 1540, z),
        ];
        let geom = compute_tile_grid_geometry(viewport, &tiles, z).unwrap();
        (tiles, geom)
    }

    /// Adjacent tiles must land on identical shared pixel edges, or a
    /// hairline seam appears where the antialiased edge of each tile's
    /// quad exposes the background behind it. A center/zoom chosen to
    /// force a fractional (non-integer) pixel offset for the underlying
    /// projection is the regression case for that seam.
    #[test]
    fn adjacent_tiles_share_exact_pixel_edge() {
        let screen_size = size(px(801.0), px(600.0));
        let viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let (tiles, geom) = tiles_and_geom(&viewport, 12);

        let (x_a, _, width_a, _) = tile_screen_rect(&geom, &tiles[0]);
        let (x_b, _, _, _) = tile_screen_rect(&geom, &tiles[1]);

        assert_eq!(
            x_a.as_f32().fract(),
            0.0,
            "tile edge must land on a whole pixel, not a fractional one"
        );
        assert_eq!(
            x_a + width_a,
            x_b,
            "adjacent tiles must share the same pixel edge"
        );
    }

    /// During a drag, every visible tile must shift by the *same* amount
    /// each frame. If each tile independently rounds its own continuously
    /// changing screen position, different tiles cross their own rounding
    /// boundary at different moments — visible as tiles jiggling relative
    /// to each other mid-drag, even though they line up at any single rest
    /// frame. Simulate a slow drag and assert all tiles move in lockstep.
    #[test]
    fn tiles_pan_in_lockstep_without_relative_jiggle() {
        let screen_size = size(px(801.0), px(600.0));
        let mut viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let z = 12;

        let (tiles, geom0) = tiles_and_geom(&viewport, z);
        let mut prev_x: Vec<f32> = tiles
            .iter()
            .map(|t| tile_screen_rect(&geom0, t).0.as_f32())
            .collect();

        // Small sub-pixel steps, the exact case that exposed independent
        // per-tile rounding drift.
        for _ in 0..40 {
            viewport.transform.pan_by_pixels(px(0.3), px(0.0));
            let geom = compute_tile_grid_geometry(&viewport, &tiles, z).unwrap();
            let cur_x: Vec<f32> = tiles
                .iter()
                .map(|t| tile_screen_rect(&geom, t).0.as_f32())
                .collect();

            let deltas: Vec<f32> = cur_x
                .iter()
                .zip(prev_x.iter())
                .map(|(c, p)| c - p)
                .collect();
            let first = deltas[0];
            for (i, d) in deltas.iter().enumerate() {
                assert_eq!(
                    *d, first,
                    "tile {i} moved by {d}px but tile 0 moved by {first}px in the same frame \
                     (tiles desynced during pan)"
                );
            }
            prev_x = cur_x;
        }
    }

    #[test]
    fn no_bounds_passthrough() {
        assert_eq!(compute_effective_tile_zoom(0.0, None, None), Some(0));
        assert_eq!(compute_effective_tile_zoom(5.4, None, None), Some(5));
        assert_eq!(compute_effective_tile_zoom(12.0, None, None), Some(12));
        // Existing global cap of 18.
        assert_eq!(compute_effective_tile_zoom(20.0, None, None), Some(18));
    }

    #[test]
    fn below_min_returns_none() {
        assert_eq!(compute_effective_tile_zoom(3.0, Some(5), None), None);
        assert_eq!(compute_effective_tile_zoom(4.49, Some(5), None), None);
    }

    #[test]
    fn at_min_uses_viewport_z() {
        assert_eq!(compute_effective_tile_zoom(5.0, Some(5), None), Some(5));
        assert_eq!(compute_effective_tile_zoom(4.5, Some(5), None), Some(5));
    }

    #[test]
    fn at_max_uses_viewport_z() {
        assert_eq!(compute_effective_tile_zoom(14.0, None, Some(14)), Some(14));
        assert_eq!(compute_effective_tile_zoom(13.5, None, Some(14)), Some(14));
    }

    #[test]
    fn one_above_max_clamps() {
        assert_eq!(compute_effective_tile_zoom(15.0, None, Some(14)), Some(14));
    }

    #[test]
    fn two_above_max_returns_none() {
        assert_eq!(compute_effective_tile_zoom(16.0, None, Some(14)), None);
        assert_eq!(compute_effective_tile_zoom(17.0, None, Some(14)), None);
    }

    #[test]
    fn min_and_max_combined() {
        assert_eq!(compute_effective_tile_zoom(4.0, Some(5), Some(14)), None);
        assert_eq!(compute_effective_tile_zoom(5.0, Some(5), Some(14)), Some(5));
        assert_eq!(compute_effective_tile_zoom(10.0, Some(5), Some(14)), Some(10));
        assert_eq!(compute_effective_tile_zoom(14.0, Some(5), Some(14)), Some(14));
        assert_eq!(compute_effective_tile_zoom(15.0, Some(5), Some(14)), Some(14));
        assert_eq!(compute_effective_tile_zoom(16.0, Some(5), Some(14)), None);
    }
}
