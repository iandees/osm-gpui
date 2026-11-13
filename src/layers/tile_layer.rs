use gpui::*;
use std::sync::{Arc, Mutex};

use crate::coordinates::MAX_MAP_ZOOM;
use crate::download_bounds::MercRect;
use crate::imagery::AttributionInfo;
use crate::imagery_offset::{offset_to_mercator, ImageryOffset};
use crate::layers::{LayerId, MapLayer};
use crate::tile_cache::TileCache;
use crate::tiles::{tiles_covering, url_from_template, TileCoord};
use crate::viewport::Viewport;

/// The built-in OpenStreetMap Carto tile URL template.
pub const OSM_CARTO_TEMPLATE: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

/// Attribution text required for the built-in OpenStreetMap Carto base
/// layer. This is not sourced from the ELI index, so it's hardcoded here.
pub const OSM_CARTO_ATTRIBUTION: &str = "© OpenStreetMap contributors";

/// The imagery id under which the built-in OSM Carto layer's offset is kept.
pub const OSM_CARTO_IMAGERY_ID: &str = "osm-carto";

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
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
    attribution: Option<AttributionInfo>,
    /// Stable key of the imagery source, under which offsets are remembered.
    imagery_id: String,
    /// How far the imagery is painted from where its tiles say it is.
    offset: ImageryOffset,
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
        .with_imagery_id(OSM_CARTO_IMAGERY_ID)
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
        let imagery_id = format!("custom:{url_template}");
        Self {
            id,
            name,
            url_template,
            source_key,
            visible: true,
            tile_cache,
            min_zoom: None,
            max_zoom: None,
            attribution: None,
            imagery_id,
            offset: ImageryOffset::ZERO,
        }
    }

    /// Set the stable id of the imagery source (see `MapLayer::imagery_id`).
    pub fn with_imagery_id(mut self, id: impl Into<String>) -> Self {
        self.imagery_id = id.into();
        self
    }

    pub fn with_offset(mut self, offset: ImageryOffset) -> Self {
        self.offset = offset;
        self
    }

    /// The tiles to fetch and paint for `viewport` at `tile_zoom`, allowing
    /// for the layer's offset: these are the tiles that are visible *after*
    /// the shift, so no strip at the viewport edge is left empty.
    fn visible_tiles(&self, viewport: &Viewport, tile_zoom: u32) -> Vec<TileCoord> {
        tiles_covering(
            viewport.mercator_view_rect(),
            self.mercator_offset(viewport),
            tile_zoom,
        )
    }

    /// The offset as a Web Mercator shift, scaled for the view's latitude.
    fn mercator_offset(&self, viewport: &Viewport) -> (f64, f64) {
        offset_to_mercator(self.offset, viewport.center().0)
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

    /// Compute the tile zoom level to request for a given viewport zoom,
    /// honoring the layer's optional `min_zoom` / `max_zoom` ELI bounds.
    pub fn effective_tile_zoom(&self, viewport_z: f64) -> Option<u32> {
        compute_effective_tile_zoom(viewport_z, self.min_zoom, self.max_zoom)
    }

    /// Whether requesting tiles one zoom deeper on Hi-DPI displays is
    /// appropriate for this source. OSM Carto tiles are not designed as
    /// "retina" tiles: their labels and symbols are sized for 256px at 1x, so
    /// deeper tiles would render everything at half its intended logical
    /// size. Such sources are instead stretched (slightly soft, right size).
    fn hidpi_tiles(&self) -> bool {
        template_supports_hidpi(&self.url_template)
    }

    /// The tile zoom to actually request for a display with `scale_factor`
    /// device pixels per logical pixel: `effective_tile_zoom`, one level
    /// deeper on Hi-DPI displays when the source has tiles that deep.
    fn render_tile_zoom(&self, viewport_z: f64, scale_factor: f32) -> Option<u32> {
        self.effective_tile_zoom(viewport_z).map(|z| {
            if self.hidpi_tiles() {
                hidpi_tile_zoom(z, scale_factor, self.max_zoom)
            } else {
                z
            }
        })
    }
}

/// See `TileLayer::hidpi_tiles`.
fn template_supports_hidpi(url_template: &str) -> bool {
    url_template != OSM_CARTO_TEMPLATE
}

/// Scale factor from which tiles are requested one zoom level deeper.
const HIDPI_MIN_SCALE: f32 = 1.5;

/// Global tile zoom cap used when a source advertises no max zoom (the same
/// cap `compute_effective_tile_zoom` applies).
const GLOBAL_MAX_TILE_ZOOM: u32 = 19;

/// Hi-DPI tile selection. A 256px tile spans 256 logical pixels, i.e. 512
/// device pixels at 2x, so it would be upscaled and look soft. Requesting the
/// tiles one zoom level deeper covers the same screen area with four sharp
/// 256px tiles per logical tile slot instead. Falls back to `tile_zoom`
/// (stretching) on standard-DPI displays and when `tile_zoom + 1` would
/// exceed the source's max zoom (or the global cap if it advertises none).
fn hidpi_tile_zoom(tile_zoom: u32, scale_factor: f32, max_zoom: Option<u32>) -> u32 {
    let limit = max_zoom.unwrap_or(GLOBAL_MAX_TILE_ZOOM);
    if scale_factor >= HIDPI_MIN_SCALE && tile_zoom < limit {
        tile_zoom + 1
    } else {
        tile_zoom
    }
}

/// Compute the tile zoom level to request for a given viewport zoom,
/// honoring optional `min_zoom` / `max_zoom` ELI bounds.
///
/// Returns `None` when the layer should not draw at all (viewport is
/// below `min_zoom`). Past the source's `max_zoom` (or the global cap of 19
/// when it advertises none) returns that max, so the deepest tiles are
/// over-zoomed (scaled up) however far the map is zoomed in.
fn compute_effective_tile_zoom(
    viewport_z: f64,
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
) -> Option<u32> {
    let rounded = viewport_z.round().clamp(0.0, MAX_MAP_ZOOM) as u32;
    if let Some(min_z) = min_zoom {
        if rounded < min_z {
            return None;
        }
    }
    Some(rounded.min(max_zoom.unwrap_or(GLOBAL_MAX_TILE_ZOOM)))
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

/// Source tile size in texels (standard XYZ raster tiles).
const TILE_TEXELS: f32 = 256.0;

/// How far (in screen pixels) to extend a tile's image past the tile's own
/// box on every side, so the box's clip removes the image's outermost
/// half-texel.
///
/// The GPU samples tile images from a texture atlas with bilinear
/// filtering. Within half a texel of the image edge a sample blends the
/// edge texel with the atlas padding (transparent black), giving a dark,
/// semi-transparent edge row. That row is half a texel thick, i.e.
/// `0.5 * tile_width / 256` screen pixels, so it is negligible at 1x but
/// grows when tiles are magnified (overzoomed) and shows as a dark seam
/// along every tile boundary. Extending the image by that much and clipping
/// it back to the tile box discards the blended row at the cost of
/// stretching the content by at most half a source texel at the edges.
fn edge_bleed_margin(tile_width_px: f32) -> f32 {
    (0.5 * tile_width_px / TILE_TEXELS).clamp(0.0, 16.0)
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
#[cfg(test)]
fn tile_screen_rect(viewport: &Viewport, tile: &TileCoord) -> (Pixels, Pixels, Pixels, Pixels) {
    tile_screen_rect_shifted(viewport, tile, (0.0, 0.0))
}

/// `tile_screen_rect` for imagery painted `(off_x, off_y)` Web Mercator metres
/// away from where the tile says it is (the layer's imagery offset).
fn tile_screen_rect_shifted(
    viewport: &Viewport,
    tile: &TileCoord,
    (off_x, off_y): (f64, f64),
) -> (Pixels, Pixels, Pixels, Pixels) {
    let MercRect {
        min_x,
        max_x,
        min_y,
        max_y,
    } = tile.mercator_bounds();
    let top_left = viewport.mercator_to_screen(min_x + off_x, max_y + off_y);
    let bottom_right = viewport.mercator_to_screen(max_x + off_x, min_y + off_y);

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

    fn imagery_offset(&self) -> Option<ImageryOffset> {
        Some(self.offset)
    }

    fn set_imagery_offset(&mut self, offset: ImageryOffset) {
        self.offset = offset;
    }

    fn imagery_id(&self) -> Option<&str> {
        Some(&self.imagery_id)
    }

    fn render_elements(
        &self,
        viewport: &Viewport,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let mut elements = Vec::new();

        let zoom_level = viewport.zoom_level();
        let Some(tile_zoom) = self.render_tile_zoom(zoom_level, window.scale_factor()) else {
            return elements;
        };
        let visible_tiles = self.visible_tiles(viewport, tile_zoom);
        let merc_offset = self.mercator_offset(viewport);

        // Tell the tile cache which tiles are needed now (visible tiles and
        // their parent fallbacks) so downloads for tiles that have scrolled
        // off the map are cancelled, and re-request any that were cancelled
        // but are visible again.
        let wanted: std::collections::HashSet<String> = visible_tiles
            .iter()
            .flat_map(|t| std::iter::once(*t).chain(t.parent()))
            .map(|t| url_from_template(&self.url_template, &t).to_string())
            .collect();
        for key in crate::tile_cache::set_wanted_tiles(&self.source_key, wanted) {
            cx.remove_asset::<crate::tile_cache::TileAsset>(&key);
        }

        for tile_coord in &visible_tiles {
            let (tile_x, tile_y, tile_width, tile_height) =
                tile_screen_rect_shifted(viewport, tile_coord, merc_offset);

            // Generate tile URL via the layer's URL template.
            let tile_url = url_from_template(&self.url_template, tile_coord);

            let asset_key = crate::tile_cache::TileAssetKey {
                url: tile_url.clone(),
                source_key: self.source_key.clone(),
            };

            // Probe the child tile first: the parent fallback is only built
            // (and its asset only requested) while the child is not ready.
            let child_ready = window
                .use_asset::<crate::tile_cache::TileAsset>(&asset_key, cx)
                .is_some();

            // Parent-tile fallback: while the child tile is loading, show the
            // already-cached parent (z-1) tile scaled 2x and clipped to this
            // child's quadrant. Prevents the dark "downloading" flash.
            let parent_fallback = if child_ready {
                None
            } else {
                tile_coord.parent().map(|parent_coord| {
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
                })
            };

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
            let bleed = px(edge_bleed_margin(f32::from(tile_width)));
            let char_budget = ((f32::from(tile_width) / 6.0) as usize).clamp(8, 40);
            let fallback_url = tile_url;

            let tile_element = tile_element
                .child(
                    // Use GPUI's img with asset loading system. Extended past
                    // the tile box by `edge_bleed_margin` (see its docs) and
                    // clipped by the box's overflow_hidden.
                    img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        window.use_asset::<crate::tile_cache::TileAsset>(&asset_key, cx)
                    })
                    .absolute()
                    .left(-bleed)
                    .top(-bleed)
                    .w(tile_width + bleed * 2.0)
                    .h(tile_height + bleed * 2.0)
                    .object_fit(gpui::ObjectFit::Fill)
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

    fn render_canvas(&self, _viewport: &Viewport, _bounds: Bounds<Pixels>, _window: &mut Window) {}

    fn stats(&self) -> Vec<(String, String)> {
        let (cached_files, active_downloads) = if let Ok(tile_cache) = self.tile_cache.try_lock() {
            tile_cache.stats()
        } else {
            (0, 0)
        };

        let mut stats = vec![
            ("Cached Files".to_string(), cached_files.to_string()),
            ("Active Downloads".to_string(), active_downloads.to_string()),
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
        compute_effective_tile_zoom, edge_bleed_margin, hidpi_tile_zoom, tile_screen_rect,
        TILE_OVERLAP_PX,
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
        let a = tiles[0].mercator_bounds();
        let corner = viewport.mercator_to_screen(a.max_x, a.min_y);

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

    /// An offset in metres moves a tile on screen by exactly the pixels the
    /// pure conversion says, at the view's zoom and latitude.
    #[test]
    fn offset_shifts_tiles_by_the_converted_pixels() {
        use super::tile_screen_rect_shifted;
        use crate::imagery_offset::{metres_to_pixels, offset_to_mercator, ImageryOffset};

        let viewport = Viewport::new(40.71277, -74.00591, 18.0, size(px(900.0), px(600.0)));
        let tile = TileCoord::new(77_000, 98_500, 18);
        let offset = ImageryOffset::new(3.2, -1.75);
        let (zero_x, zero_y, _, _) = tile_screen_rect(&viewport, &tile);
        let (x, y, _, _) = tile_screen_rect_shifted(
            &viewport,
            &tile,
            offset_to_mercator(offset, viewport.center().0),
        );
        let (dx, dy) = metres_to_pixels(offset, 18.0, viewport.center().0);
        assert!((x.as_f32() - zero_x.as_f32() - dx as f32).abs() < 0.01);
        assert!((y.as_f32() - zero_y.as_f32() - dy as f32).abs() < 0.01);
        assert!(dx > 0.0 && dy > 0.0, "east is right, south is down");
    }

    #[test]
    fn edge_bleed_margin_is_half_a_texel_and_bounded() {
        assert_eq!(edge_bleed_margin(256.0), 0.5);
        assert_eq!(edge_bleed_margin(1024.0), 2.0);
        assert_eq!(edge_bleed_margin(0.0), 0.0);
        assert_eq!(edge_bleed_margin(1.0e6), 16.0);
    }

    #[test]
    fn hidpi_goes_one_zoom_deeper() {
        assert_eq!(hidpi_tile_zoom(12, 2.0, Some(19)), 13);
        assert_eq!(hidpi_tile_zoom(12, 3.0, None), 13);
        assert_eq!(hidpi_tile_zoom(12, 1.5, None), 13);
    }

    #[test]
    fn osm_carto_is_not_treated_as_hidpi() {
        assert!(!super::template_supports_hidpi(super::OSM_CARTO_TEMPLATE));
        assert!(super::template_supports_hidpi(
            "https://example.com/{z}/{x}/{y}.png"
        ));
    }

    #[test]
    fn standard_dpi_is_unchanged() {
        assert_eq!(hidpi_tile_zoom(12, 1.0, Some(19)), 12);
        assert_eq!(hidpi_tile_zoom(12, 1.25, None), 12);
    }

    #[test]
    fn hidpi_respects_max_zoom() {
        // z+1 would exceed the source's max: stretch instead.
        assert_eq!(hidpi_tile_zoom(14, 2.0, Some(14)), 14);
        assert_eq!(hidpi_tile_zoom(13, 2.0, Some(14)), 14);
        // No advertised max: the global cap of 19 applies.
        assert_eq!(hidpi_tile_zoom(19, 2.0, None), 19);
        assert_eq!(hidpi_tile_zoom(18, 2.0, None), 19);
    }

    #[test]
    fn hidpi_slot_is_four_subtiles_of_same_area() {
        use crate::download_bounds::MercRect;
        use crate::tiles::tiles_covering;
        // One logical tile slot at z covers exactly 4 tiles at z+1.
        let slot = TileCoord::new(5, 7, 4).mercator_bounds();
        let eps = 1.0;
        let tiles = tiles_covering(
            MercRect::new(
                slot.min_x + eps,
                slot.max_x - eps,
                slot.min_y + eps,
                slot.max_y - eps,
            ),
            (0.0, 0.0),
            5,
        );
        assert_eq!(tiles.len(), 4);
        assert!(tiles
            .iter()
            .all(|t| t.parent() == Some(TileCoord::new(5, 7, 4))));
    }

    #[test]
    fn no_bounds_passthrough() {
        assert_eq!(compute_effective_tile_zoom(0.0, None, None), Some(0));
        assert_eq!(compute_effective_tile_zoom(5.4, None, None), Some(5));
        assert_eq!(compute_effective_tile_zoom(12.0, None, None), Some(12));
        // Global cap when the source advertises no max zoom.
        assert_eq!(compute_effective_tile_zoom(20.0, None, None), Some(19));
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
    fn far_above_max_overzooms() {
        assert_eq!(compute_effective_tile_zoom(16.0, None, Some(14)), Some(14));
        assert_eq!(compute_effective_tile_zoom(23.0, None, Some(14)), Some(14));
    }

    #[test]
    fn deep_source_fetches_to_its_own_max() {
        assert_eq!(compute_effective_tile_zoom(21.0, None, Some(23)), Some(21));
        assert_eq!(compute_effective_tile_zoom(23.0, None, Some(23)), Some(23));
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
        assert_eq!(
            compute_effective_tile_zoom(16.0, Some(5), Some(14)),
            Some(14)
        );
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
