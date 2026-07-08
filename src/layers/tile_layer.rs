use gpui::*;
use std::sync::{Arc, Mutex};

use crate::imagery::AttributionInfo;
use crate::layers::{LayerId, MapLayer};
use crate::tile_cache::TileCache;
use crate::tiles::{get_tiles_for_bounds, url_from_template, TileCoord};
use crate::viewport::Viewport;

/// The built-in OpenStreetMap Carto tile URL template.
pub const OSM_CARTO_TEMPLATE: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

/// Attribution text required for the built-in OpenStreetMap Carto base
/// layer. This is not sourced from the ELI index, so it's hardcoded here.
pub const OSM_CARTO_ATTRIBUTION: &str = "© OpenStreetMap contributors";

/// Link target for the built-in OpenStreetMap Carto attribution.
pub const OSM_CARTO_ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// Layer for rendering raster map tiles
pub struct TileLayer {
    id: LayerId,
    name: String,
    url_template: String,
    source_key: String,
    visible: bool,
    tile_cache: Arc<Mutex<TileCache>>,
    show_boundaries: bool,
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
    attribution: Option<AttributionInfo>,
}

impl TileLayer {
    pub fn new(id: LayerId, tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self::new_with_template(
            id,
            "OpenStreetMap Carto".to_string(),
            OSM_CARTO_TEMPLATE.to_string(),
            tile_cache,
        )
        .with_attribution(Some(AttributionInfo {
            text: OSM_CARTO_ATTRIBUTION.to_string(),
            url: Some(OSM_CARTO_ATTRIBUTION_URL.to_string()),
        }))
    }

    pub fn new_with_name(id: LayerId, name: String, tile_cache: Arc<Mutex<TileCache>>) -> Self {
        Self::new_with_template(id, name, OSM_CARTO_TEMPLATE.to_string(), tile_cache)
    }

    pub fn new_with_template(
        id: LayerId,
        name: String,
        url_template: String,
        tile_cache: Arc<Mutex<TileCache>>,
    ) -> Self {
        let source_key = crate::tile_cache::source_key_for_template(&url_template);
        Self {
            id,
            name,
            url_template,
            source_key,
            visible: true,
            tile_cache,
            show_boundaries: false,
            min_zoom: None,
            max_zoom: None,
            attribution: None,
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

    /// Set the required source-credit (text and optional link) for this
    /// layer's tiles.
    pub fn with_attribution(mut self, attribution: Option<AttributionInfo>) -> Self {
        self.attribution = attribution;
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
    let rounded = viewport_z.round().clamp(0.0, 18.0) as u32;
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

/// A small, deliberate overlap added to every tile's rendered size so
/// adjacent tiles overlap slightly instead of exactly abutting. This is the
/// standard technique tile-based map renderers use (e.g. Leaflet sizes
/// tiles a pixel larger than their exact geometric size) to guarantee no
/// gap can ever appear between neighbors, regardless of where their
/// (necessarily continuous, sub-pixel) edges happen to fall. Since adjacent
/// map tiles show continuous, matching imagery at their shared edge, a
/// small overlap of nearly-identical content is imperceptible — unlike a
/// gap, which exposes the dark fallback background behind the tiles.
///
/// Deliberately a *fixed* pixel amount, not proportional to the tile's
/// current on-screen size: `object_fit: Cover` reacts to a larger
/// container by magnifying the same fixed-size source image to fill it,
/// so a bigger container doesn't extend a tile's true content into its
/// neighbor — it just zooms that tile in past its true scale, sacrificing
/// real edge content. At 3px this became visible as missing edge pixels
/// (over-magnified crop); keep this fixed and small — just enough to
/// survive whatever sub-pixel rounding happens downstream in rendering.
const TILE_OVERLAP_PX: f32 = 0.5;

/// Web Mercator (EPSG:3857) bounding box of a tile, in meters, computed
/// directly from its XYZ grid index (no trig — the XYZ scheme is a
/// uniform grid in this projected space, so every tile at a given zoom is
/// exactly the same size). Matches the constant used by
/// `coordinates::lat_lon_to_mercator`.
fn tile_mercator_bounds(tile: &TileCoord) -> (f64, f64, f64, f64) {
    let world_half = 20037508.34_f64;
    let n = 2.0_f64.powi(tile.z as i32);
    let tile_size_m = (world_half * 2.0) / n;

    let min_x = -world_half + tile.x as f64 * tile_size_m;
    let max_x = min_x + tile_size_m;
    let max_y = world_half - tile.y as f64 * tile_size_m;
    let min_y = max_y - tile_size_m;
    (min_x, min_y, max_x, max_y)
}

/// Screen rect (x, y, width, height) for one tile: each tile is projected
/// independently at its exact, continuous (sub-pixel) screen position —
/// deliberately *not* rounded, so tiles move perfectly smoothly and in
/// exact lockstep as the viewport pans or zooms, with no per-tile or
/// per-frame quantization to desync.
///
/// The box is then grown by `TILE_OVERLAP_PX` split evenly on *all four*
/// sides (not just grown rightward/downward), so it overlaps every
/// neighbor — including diagonally, at the corner where four tiles meet.
/// Growing one-sided only would let a tile overlap its right/bottom
/// neighbor but not its left/top one, leaving the shared corner of a 2×2
/// tile block uncovered from every direction — visible as a faint cross
/// where the dark fallback background shows through at that one point.
fn tile_screen_rect(viewport: &Viewport, tile: &TileCoord) -> (Pixels, Pixels, Pixels, Pixels) {
    let (min_x, min_y, max_x, max_y) = tile_mercator_bounds(tile);
    let top_left = viewport.mercator_to_screen(min_x, max_y);
    let bottom_right = viewport.mercator_to_screen(max_x, min_y);

    let half_overlap = px(TILE_OVERLAP_PX / 2.0);
    let width = (bottom_right.x - top_left.x).abs() + px(TILE_OVERLAP_PX);
    let height = (bottom_right.y - top_left.y).abs() + px(TILE_OVERLAP_PX);
    (
        top_left.x - half_overlap,
        top_left.y - half_overlap,
        width,
        height,
    )
}

impl MapLayer for TileLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn attribution(&self) -> Option<&AttributionInfo> {
        self.attribution.as_ref()
    }

    fn render_elements(&self, viewport: &Viewport) -> Vec<AnyElement> {
        let mut elements = Vec::new();

        let zoom_level = viewport.zoom_level();
        let Some(tile_zoom) = self.effective_tile_zoom(zoom_level) else {
            return elements;
        };
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat,
            bounds_geo.min_lon,
            bounds_geo.max_lat,
            bounds_geo.max_lon,
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        for tile_coord in &visible_tiles {
            let (tile_x, tile_y, tile_width, tile_height) = tile_screen_rect(viewport, tile_coord);

            // Generate tile URL via the layer's URL template.
            let tile_url = url_from_template(&self.url_template, tile_coord);

            // Parent-tile fallback: while the child tile is loading, show the
            // already-cached parent (z-1) tile scaled 2× and clipped to this
            // child's quadrant. Prevents the dark "downloading" flash.
            let parent_fallback = tile_coord.parent().map(|parent_coord| {
                let (qx, qy) = tile_coord.quadrant_in_parent();
                let parent_url = url_from_template(&self.url_template, &parent_coord);
                let parent_key = crate::tile_cache::TileAssetKey {
                    url: parent_url,
                    source_key: self.source_key.clone(),
                };
                div()
                    .absolute()
                    .left(-tile_width * qx as f32)
                    .top(-tile_height * qy as f32)
                    .w(tile_width * 2.0)
                    .h(tile_height * 2.0)
                    .child(
                        img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                            window.use_asset::<crate::tile_cache::TileAsset>(&parent_key, cx)
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
            let asset_key = crate::tile_cache::TileAssetKey {
                url: tile_url,
                source_key: self.source_key.clone(),
            };

            let tile_element = tile_element
                .child(
                    // Use GPUI's img with asset loading system
                    img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        window.use_asset::<crate::tile_cache::TileAsset>(&asset_key, cx)
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
                                    .child(display),
                            )
                            .into_any_element()
                    })
                    .into_any_element(),
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
            bounds_geo.min_lat,
            bounds_geo.min_lon,
            bounds_geo.max_lat,
            bounds_geo.max_lon,
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        let tile_color = rgb(0x4a5568);
        for tile_coord in &visible_tiles {
            // Use the same positioning as render_elements for consistency
            let (tile_x, tile_y, tile_width, tile_height) = tile_screen_rect(viewport, tile_coord);
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
            (
                "Show Boundaries".to_string(),
                self.show_boundaries.to_string(),
            ),
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
    use super::{
        compute_effective_tile_zoom, tile_mercator_bounds, tile_screen_rect, TILE_OVERLAP_PX,
    };
    use crate::tiles::TileCoord;
    use crate::viewport::Viewport;
    use gpui::{px, size};

    fn adjacent_tiles(z: u32) -> [TileCoord; 4] {
        [
            TileCoord::new(1206, 1539, z),
            TileCoord::new(1207, 1539, z),
            TileCoord::new(1206, 1540, z),
            TileCoord::new(1207, 1540, z),
        ]
    }

    /// The single point where four tiles meet (top-right corner of the
    /// top-left tile) must be covered by every one of the four tiles'
    /// rendered boxes, regardless of paint order. Growing a tile's box only
    /// rightward/downward covers its right and bottom neighbors but leaves
    /// this exact corner uncovered by the tiles above/left of it — visible
    /// as a faint cross where the dark fallback background shows through.
    #[test]
    fn shared_corner_of_four_tiles_is_covered_from_every_side() {
        let screen_size = size(px(801.0), px(600.0));
        let viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let tiles = adjacent_tiles(12);

        // Corner mercator coordinate shared by all four tiles: top-left
        // tile's right edge / bottom edge.
        let (_, min_y_a, max_x_a, _) = tile_mercator_bounds(&tiles[0]);
        let corner = viewport.mercator_to_screen(max_x_a, min_y_a);

        // The corner must be a comfortable *interior* point of each tile's
        // box, not merely on its boundary — an edge that only touches the
        // corner (margin == 0) is exactly the exact-abutment case that
        // causes an antialiased seam. Require a minimum real margin.
        let min_margin = 0.2;
        for (i, tile) in tiles.iter().enumerate() {
            let (x, y, w, h) = tile_screen_rect(&viewport, tile);
            let margin_left = corner.x.as_f32() - x.as_f32();
            let margin_right = (x + w).as_f32() - corner.x.as_f32();
            let margin_top = corner.y.as_f32() - y.as_f32();
            let margin_bottom = (y + h).as_f32() - corner.y.as_f32();
            assert!(
                margin_left >= min_margin
                    && margin_right >= min_margin
                    && margin_top >= min_margin
                    && margin_bottom >= min_margin,
                "tile {i} ({tile:?}) does not comfortably cover the shared 4-tile corner at \
                 {corner:?} (margins: left={margin_left}, right={margin_right}, \
                 top={margin_top}, bottom={margin_bottom}): tile rect = ({x:?}, {y:?}, {w:?}, {h:?})"
            );
        }
    }

    /// Adjacent tiles must overlap (or at minimum touch) at their shared
    /// edge — never leave a gap, or the dark fallback background behind
    /// the tiles shows through as a hairline seam.
    #[test]
    fn adjacent_tiles_overlap_with_no_gap() {
        let screen_size = size(px(801.0), px(600.0));
        let viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let tiles = adjacent_tiles(12);

        let (x_a, _, width_a, _) = tile_screen_rect(&viewport, &tiles[0]);
        let (x_b, _, _, _) = tile_screen_rect(&viewport, &tiles[1]);

        // tile_a's right edge must reach at least as far as tile_b's left
        // edge (overlap allowed, gap is not).
        assert!(
            (x_a + width_a).as_f32() >= x_b.as_f32(),
            "adjacent tiles must overlap or touch, not leave a gap: \
             tile_a right={:?} tile_b left={:?}",
            x_a + width_a,
            x_b
        );
        // ...and the overlap shouldn't be more than the deliberate margin
        // plus a small tolerance for floating point error.
        assert!(
            (x_a + width_a).as_f32() - x_b.as_f32() <= TILE_OVERLAP_PX + 0.01,
            "overlap is larger than the deliberate margin — likely a real position bug"
        );
    }

    /// During a pure pan, every visible tile must shift by the *same*
    /// amount each frame — no per-tile quantization to desync. Since tile
    /// positions are now projected independently at full precision (no
    /// rounding), this holds automatically; this test locks that in as a
    /// regression guard. Tiles here are multiple columns/rows apart, which
    /// would have caught the earlier (since-reverted) `anchor + index *
    /// rounded_width` design: that scheme jumped a tile by an amount
    /// proportional to its distance from the anchor whenever the rounded
    /// width ticked over.
    #[test]
    fn tiles_pan_in_lockstep_without_relative_jiggle() {
        let screen_size = size(px(801.0), px(600.0));
        let mut viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let tiles = adjacent_tiles(12);

        let mut prev_x: Vec<f32> = tiles
            .iter()
            .map(|t| tile_screen_rect(&viewport, t).0.as_f32())
            .collect();

        for _ in 0..40 {
            viewport.transform.pan_by_pixels(px(0.3), px(0.0));
            let cur_x: Vec<f32> = tiles
                .iter()
                .map(|t| tile_screen_rect(&viewport, t).0.as_f32())
                .collect();

            let deltas: Vec<f32> = cur_x
                .iter()
                .zip(prev_x.iter())
                .map(|(c, p)| c - p)
                .collect();
            let first = deltas[0];
            for (i, d) in deltas.iter().enumerate() {
                assert!(
                    (*d - first).abs() < 0.001,
                    "tile {i} moved by {d}px but tile 0 moved by {first}px in the same frame \
                     (tiles desynced during pan)"
                );
            }
            prev_x = cur_x;
        }
    }

    /// During a slow zoom, a tile's position must change *smoothly* — no
    /// disproportionate per-frame jump. Zooming legitimately moves
    /// different tiles by different amounts (it scales distances from the
    /// viewport center, so a lockstep-equal-delta check does not apply
    /// here, unlike panning). What must hold is that each small zoom step
    /// produces a correspondingly small position change; a quantization
    /// scheme that rounds tile size once and multiplies by tile-index
    /// distance from some anchor would instead show sudden jumps whenever
    /// the rounded size ticks over — bigger for tiles farther from the
    /// anchor. This tile is several tiles away from the viewport center,
    /// which is exactly the case that would expose that.
    #[test]
    fn tiles_zoom_smoothly_without_disproportionate_jumps() {
        let screen_size = size(px(801.0), px(600.0));
        let mut viewport = Viewport::new(40.71277, -74.00591, 12.37, screen_size);
        let tile = TileCoord::new(1206, 1539, 12);

        let mut prev_x = tile_screen_rect(&viewport, &tile).0.as_f32();
        for _ in 0..60 {
            let next_zoom = viewport.transform.zoom_level + 0.002;
            viewport.transform.zoom_to(next_zoom);
            let cur_x = tile_screen_rect(&viewport, &tile).0.as_f32();

            let delta = (cur_x - prev_x).abs();
            assert!(
                delta < 2.0,
                "tile position jumped {delta}px for a 0.002 zoom-level step — \
                 expected a smooth, small change"
            );
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
        assert_eq!(
            compute_effective_tile_zoom(10.0, Some(5), Some(14)),
            Some(10)
        );
        assert_eq!(
            compute_effective_tile_zoom(14.0, Some(5), Some(14)),
            Some(14)
        );
        assert_eq!(
            compute_effective_tile_zoom(15.0, Some(5), Some(14)),
            Some(14)
        );
        assert_eq!(compute_effective_tile_zoom(16.0, Some(5), Some(14)), None);
    }

    /// Guards against `TileLayer`'s `source_key` field and
    /// `source_key_for_template` silently desyncing under future refactors:
    /// a layer built via `TileLayer::new` (the built-in OSM Carto layer)
    /// must derive its `source_key` from the same template it uses to
    /// resolve tile URLs.
    #[gpui::test]
    fn built_in_layer_source_key_matches_template_derivation(cx: &mut gpui::TestAppContext) {
        use crate::idle_tracker::IdleTracker;
        use crate::layers::LayerId;
        use crate::tile_cache::{source_key_for_template, TileCache};
        use std::sync::{Arc, Mutex};

        let executor = cx.executor();
        let idle = IdleTracker::new();
        let cache = Arc::new(Mutex::new(TileCache::new(executor, idle)));
        let layer = super::TileLayer::new(LayerId(1), cache);
        assert_eq!(
            layer.source_key,
            source_key_for_template(super::OSM_CARTO_TEMPLATE)
        );
    }
}
