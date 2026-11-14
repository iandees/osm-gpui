//! Slippy-map tile maths: the one tile coordinate type, the tiles covering a
//! view, a tile's bounds, and URL templates.

use std::f64::consts::PI;
use std::ops::Range;

use crate::coordinates::{GeoBounds, MERCATOR_HALF_WORLD_M};
use crate::download_bounds::MercRect;

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

    /// The tile's lat/lon box.
    pub fn geo_bounds(&self) -> GeoBounds {
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        let (max_lat, min_lon) = tile_position_to_lat_lon(self.z, x, y);
        let (min_lat, max_lon) = tile_position_to_lat_lon(self.z, x + 1.0, y + 1.0);
        GeoBounds::new(min_lat, max_lat, min_lon, max_lon)
    }

    /// The tile's Web Mercator box in metres, computed straight from its grid
    /// index (no trig: the grid is uniform in this space).
    pub fn mercator_bounds(&self) -> MercRect {
        let tile_size_m = MERCATOR_HALF_WORLD_M * 2.0 / 2.0_f64.powi(self.z as i32);
        let min_x = -MERCATOR_HALF_WORLD_M + self.x as f64 * tile_size_m;
        let max_y = MERCATOR_HALF_WORLD_M - self.y as f64 * tile_size_m;
        MercRect::new(min_x, min_x + tile_size_m, max_y - tile_size_m, max_y)
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

/// The `{switch:a,b,c}` placeholder of a URL template.
struct SwitchSpan<'a> {
    /// Byte range of the whole `{switch:...}` text in the template.
    range: Range<usize>,
    /// The subdomains listed, trimmed.
    options: Vec<&'a str>,
}

/// Find the `{switch:...}` placeholder: anchored on the literal prefix, then
/// the next `}`.
fn find_switch(template: &str) -> Option<SwitchSpan<'_>> {
    const PREFIX: &str = "{switch:";
    let start = template.find(PREFIX)?;
    let end = start + template[start..].find('}')?;
    Some(SwitchSpan {
        range: start..end + 1,
        options: template[start + PREFIX.len()..end]
            .split(',')
            .map(str::trim)
            .collect(),
    })
}

/// Replace the `{switch:a,b,c}` placeholder of `template` with `marker`, so
/// every subdomain of a source maps to the same text.
pub(crate) fn collapse_switch(template: &str, marker: &str) -> String {
    let mut out = template.to_string();
    if let Some(switch) = find_switch(template) {
        out.replace_range(switch.range, marker);
    }
    out
}

/// Substitute tile placeholders in a URL template.
///
/// Supports `{z}` / `{zoom}`, `{x}`, `{y}`, the TMS-flipped `{-y}`, and
/// `{switch:a,b,c}` / `{s}` subdomain placeholders. A `{s}` placeholder is
/// only substituted when a `{switch:...}` list also appears in the template
/// (that list becomes the subdomain pool); otherwise `{s}` is left as-is.
pub fn url_from_template(template: &str, tile: &TileCoord) -> String {
    let TileCoord { x, y, z } = *tile;
    let n = 1u32.checked_shl(z).unwrap_or(0);
    let flipped_y = n.saturating_sub(1).saturating_sub(y);

    let mut s = template.to_string();
    if let Some(switch) = find_switch(template) {
        // Pseudo-random pick based on tile coordinates (stable per-tile).
        let idx = ((x as u64).wrapping_mul(31) ^ (y as u64).wrapping_mul(17) ^ z as u64) as usize
            % switch.options.len();
        let subdomain = switch.options[idx].to_string();
        s.replace_range(switch.range, &subdomain);
        s = s.replace("{s}", &subdomain);
    }

    s = s.replace("{zoom}", &z.to_string());
    s = s.replace("{z}", &z.to_string());
    s = s.replace("{x}", &x.to_string());
    s = s.replace("{-y}", &flipped_y.to_string());
    s = s.replace("{y}", &y.to_string());
    s
}

/// `(lat, lon)` of the point `(x, y)` in tile units of zoom `z` (fractions
/// allowed; y grows southwards).
pub fn tile_position_to_lat_lon(z: u32, x: f64, y: f64) -> (f64, f64) {
    let n = 2.0_f64.powi(z as i32);
    let lat = (PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees();
    (lat, x / n * 360.0 - 180.0)
}

/// Number of tiles along one axis at `zoom` (saturating, so absurd zooms
/// do not overflow).
fn grid_size(zoom: u32) -> u32 {
    1u32.checked_shl(zoom).unwrap_or(u32::MAX)
}

/// An inclusive block of tiles at one zoom.
struct TileBlock {
    zoom: u32,
    x: (u32, u32),
    y: (u32, u32),
}

impl TileBlock {
    /// The block spanning tile-unit positions `x` and `y` (each `(low, high)`
    /// with y growing southwards), clamped to the grid.
    fn clamped(zoom: u32, x: (f64, f64), y: (f64, f64)) -> Self {
        let max_index = (grid_size(zoom) - 1) as f64;
        let index = |v: f64| v.floor().clamp(0.0, max_index) as u32;
        Self {
            zoom,
            x: (index(x.0), index(x.1)),
            y: (index(y.0), index(y.1)),
        }
    }

    /// Tiles column by column (all of a column's rows before the next column).
    fn columns_first(&self) -> Vec<TileCoord> {
        let mut out = Vec::new();
        for x in self.x.0..=self.x.1 {
            for y in self.y.0..=self.y.1 {
                out.push(TileCoord::new(x, y, self.zoom));
            }
        }
        out
    }

    /// Tiles row by row.
    fn rows_first(&self) -> Vec<TileCoord> {
        let mut out = Vec::new();
        for y in self.y.0..=self.y.1 {
            for x in self.x.0..=self.x.1 {
                out.push(TileCoord::new(x, y, self.zoom));
            }
        }
        out
    }
}

/// The tiles at `zoom` that must be fetched and painted so that every pixel of
/// a view covering the Web Mercator box `view` shows imagery, when the imagery
/// is painted shifted by `(off_x, off_y)` Mercator metres: the tile under a
/// view pixel is the one at that pixel *minus* the shift. Clamped to the tile
/// grid; listed column by column.
pub fn tiles_covering(view: MercRect, (off_x, off_y): (f64, f64), zoom: u32) -> Vec<TileCoord> {
    let tile_m = MERCATOR_HALF_WORLD_M * 2.0 / grid_size(zoom) as f64;
    let col = |mx: f64| (mx + MERCATOR_HALF_WORLD_M) / tile_m;
    // North is up: the largest y is the smallest tile row.
    let row = |my: f64| (MERCATOR_HALF_WORLD_M - my) / tile_m;
    TileBlock::clamped(
        zoom,
        (col(view.min_x - off_x), col(view.max_x - off_x)),
        (row(view.max_y - off_y), row(view.min_y - off_y)),
    )
    .columns_first()
}

/// The tiles at `zoom` that touch the lat/lon box `b`, clamped to the tile
/// grid and listed row by row. Latitudes beyond the projection's limit are
/// clamped to it.
pub fn tiles_covering_geo(b: &GeoBounds, zoom: u32) -> Vec<TileCoord> {
    TileBlock::clamped(
        zoom,
        (lon_to_x(b.min_lon, zoom), lon_to_x(b.max_lon, zoom)),
        (lat_to_y(b.max_lat, zoom), lat_to_y(b.min_lat, zoom)),
    )
    .rows_first()
}

fn lon_to_x(lon: f64, zoom: u32) -> f64 {
    (lon + 180.0) / 360.0 * 2.0_f64.powi(zoom as i32)
}

fn lat_to_y(lat: f64, zoom: u32) -> f64 {
    let lat = lat.clamp(-85.05112878, 85.05112878).to_radians();
    (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0 * 2.0_f64.powi(zoom as i32)
}

#[cfg(test)]
fn lat_lon_to_tile(lat: f64, lon: f64, zoom: u32) -> TileCoord {
    let lat_rad = lat.to_radians();
    let n = 2.0_f64.powi(zoom as i32);

    let x = ((lon + 180.0) / 360.0 * n).floor() as u32;
    let y = ((1.0 - lat_rad.tan().asinh() / PI) / 2.0 * n).floor() as u32;

    TileCoord::new(x, y, zoom)
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
        let b = TileCoord::new(1, 1, 1).geo_bounds();
        assert!(b.min_lon >= 0.0 && b.max_lon <= 180.0);
        // Web Mercator bounds are ~±85.0511°, so -85.0 is too tight.
        assert!(b.min_lat >= -85.1 && b.max_lat <= 0.0);
    }

    #[test]
    fn mercator_bounds_are_the_exact_grid_cell() {
        let h = MERCATOR_HALF_WORLD_M;
        assert_eq!(
            TileCoord::new(0, 0, 0).mercator_bounds(),
            MercRect::new(-h, h, -h, h)
        );
        // z=1, east column, north row: the north-east quadrant.
        assert_eq!(
            TileCoord::new(1, 0, 1).mercator_bounds(),
            MercRect::new(0.0, h, 0.0, h)
        );
    }

    #[test]
    fn geo_and_mercator_bounds_describe_the_same_tile() {
        use crate::coordinates::lat_lon_to_mercator;
        let t = TileCoord::new(7898, 11796, 15);
        let (g, m) = (t.geo_bounds(), t.mercator_bounds());
        let (min_x, min_y) = lat_lon_to_mercator(g.min_lat, g.min_lon);
        let (max_x, max_y) = lat_lon_to_mercator(g.max_lat, g.max_lon);
        for (a, b) in [
            (min_x, m.min_x),
            (max_x, m.max_x),
            (min_y, m.min_y),
            (max_y, m.max_y),
        ] {
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
        }
    }

    #[test]
    fn tile_math_round_trips() {
        let t = TileCoord::new(7898, 11796, 15);
        let b = t.geo_bounds();
        assert!(b.min_lat < b.max_lat && b.min_lon < b.max_lon);
        // The tile contains the point it was computed from, and only itself
        // (plus neighbours when a box straddles an edge).
        let (lat, lon) = (44.926, -93.2234);
        assert!(b.min_lat <= lat && lat <= b.max_lat && b.min_lon <= lon && lon <= b.max_lon);
        let point = GeoBounds::new(lat, lat, lon, lon);
        assert_eq!(tiles_covering_geo(&point, 15), vec![t]);
        // A box over a tile's own bounds, nudged inwards, is that tile.
        let inner = GeoBounds::new(
            b.min_lat + 1e-6,
            b.max_lat - 1e-6,
            b.min_lon + 1e-6,
            b.max_lon - 1e-6,
        );
        assert_eq!(tiles_covering_geo(&inner, 15), vec![t]);
        // Twice as wide: two tiles side by side.
        let wide = GeoBounds::new(
            b.min_lat + 1e-6,
            b.max_lat - 1e-6,
            b.min_lon + 1e-6,
            b.max_lon + (b.max_lon - b.min_lon) - 1e-6,
        );
        assert_eq!(tiles_covering_geo(&wide, 15).len(), 2);
    }

    #[test]
    fn geo_coverage_lists_rows_first_and_clamps_to_the_grid() {
        let world = GeoBounds::new(-90.0, 90.0, -400.0, 400.0);
        let coords: Vec<(u32, u32)> = tiles_covering_geo(&world, 1)
            .iter()
            .map(|t| (t.x, t.y))
            .collect();
        assert_eq!(coords, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn a_view_wider_than_the_world_counts_only_the_real_tiles() {
        let world = GeoBounds::new(-90.0, 90.0, -200.0, 200.0);
        let tiles = tiles_covering_geo(&world, 2);
        assert_eq!(tiles.len(), 16);
        assert!(tiles.iter().all(|t| t.x <= 3 && t.y <= 3 && t.z == 2));
    }

    #[test]
    fn an_ordinary_view_covers_the_tiles_under_it() {
        let tiles = tiles_covering_geo(&GeoBounds::new(51.0, 52.0, -1.0, 0.0), 5);
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|t| t.z == 5));
        let point = tiles_covering_geo(&GeoBounds::new(51.5, 51.5, -0.5, -0.5), 5);
        assert_eq!(point, vec![lat_lon_to_tile(51.5, -0.5, 5)]);
    }

    #[test]
    fn test_tile_parent() {
        assert_eq!(TileCoord::new(0, 0, 0).parent(), None);
        assert_eq!(
            TileCoord::new(2, 3, 2).parent(),
            Some(TileCoord::new(1, 1, 1))
        );
        assert_eq!(
            TileCoord::new(5, 7, 3).parent(),
            Some(TileCoord::new(2, 3, 2))
        );
    }

    #[test]
    fn test_tile_quadrant_in_parent() {
        assert_eq!(TileCoord::new(2, 2, 2).quadrant_in_parent(), (0, 0));
        assert_eq!(TileCoord::new(3, 2, 2).quadrant_in_parent(), (1, 0));
        assert_eq!(TileCoord::new(2, 3, 2).quadrant_in_parent(), (0, 1));
        assert_eq!(TileCoord::new(3, 3, 2).quadrant_in_parent(), (1, 1));
    }

    #[test]
    fn url_template_substitution() {
        let tile = TileCoord::new(3, 5, 4);
        // {z}/{x}/{y}
        let u = url_from_template("https://t.example/{z}/{x}/{y}.png", &tile);
        assert_eq!(u, "https://t.example/4/3/5.png");
        // {zoom} alias
        let u = url_from_template("https://t.example/{zoom}/{x}/{y}.png", &tile);
        assert_eq!(u, "https://t.example/4/3/5.png");
        // {-y} TMS flip: at z=4 n=16, flipped = 16-1-5 = 10
        let u = url_from_template("https://t.example/{z}/{x}/{-y}.png", &tile);
        assert_eq!(u, "https://t.example/4/3/10.png");
        // {switch:a,b,c} — ensure the output is one of the listed subdomains
        let u = url_from_template(
            "https://{switch:alpha,beta,gamma}.example/{z}/{x}/{y}.png",
            &tile,
        );
        let prefix_ok = [
            "https://alpha.example/",
            "https://beta.example/",
            "https://gamma.example/",
        ]
        .iter()
        .any(|p| u.starts_with(p));
        assert!(prefix_ok, "got {}", u);
        // {s} paired with {switch:...} is also substituted
        let u = url_from_template("https://{switch:a,b,c}.example/{s}/{z}/{x}/{y}.png", &tile);
        let inner = u.trim_start_matches("https://");
        let first = inner.split('.').next().unwrap();
        assert!(u.contains(&format!("/{}/", first)), "got {}", u);
    }

    #[test]
    fn a_lone_s_placeholder_and_an_unclosed_switch_are_left_alone() {
        let tile = TileCoord::new(1, 1, 1);
        assert_eq!(
            url_from_template("https://{s}.example/{z}/{x}/{y}", &tile),
            "https://{s}.example/1/1/1"
        );
        assert_eq!(
            url_from_template("https://{switch:a,b.example/z", &tile),
            "https://{switch:a,b.example/z"
        );
    }

    #[test]
    fn collapse_switch_replaces_only_the_switch_span() {
        assert_eq!(
            collapse_switch("https://{switch: a , b }.e/{z}", "s"),
            "https://s.e/{z}"
        );
        assert_eq!(collapse_switch("https://e/{z}", "s"), "https://e/{z}");
        assert_eq!(
            collapse_switch("https://{switch:a", "s"),
            "https://{switch:a"
        );
    }

    /// Every pixel of the view, when the imagery is painted shifted, must lie
    /// on one of the returned tiles.
    fn assert_covered(view: MercRect, off: (f64, f64), zoom: u32) {
        let tiles = tiles_covering(view, off, zoom);
        let n = 2f64.powi(zoom as i32);
        let half = MERCATOR_HALF_WORLD_M;
        let tile_m = half * 2.0 / n;
        let steps = 40;
        for i in 0..=steps {
            for j in 0..=steps {
                let px = view.min_x + (view.max_x - view.min_x) * i as f64 / steps as f64;
                let py = view.min_y + (view.max_y - view.min_y) * j as f64 / steps as f64;
                // The imagery point painted at this view point.
                let (sx, sy) = (px - off.0, py - off.1);
                let tx = ((sx + half) / tile_m).floor() as u32;
                let ty = ((half - sy) / tile_m).floor() as u32;
                assert!(
                    tiles.contains(&TileCoord::new(tx, ty, zoom)),
                    "view point ({px}, {py}) shifted by {off:?} needs tile {tx},{ty} which is missing"
                );
            }
        }
    }

    #[test]
    fn tile_coverage_holds_for_offsets_in_every_direction() {
        let zoom = 18;
        let tile_m = MERCATOR_HALF_WORLD_M * 2.0 / 2f64.powi(zoom as i32);
        // A 1000 x 700 px view somewhere in the world.
        let (cx, cy) = (-8_238_000.0, 4_970_000.0);
        let view = MercRect::new(
            cx - 500.0 * tile_m / 256.0,
            cx + 500.0 * tile_m / 256.0,
            cy - 350.0 * tile_m / 256.0,
            cy + 350.0 * tile_m / 256.0,
        );
        let steps = [0.0, 3.0, -3.0, 0.4 * tile_m, -0.9 * tile_m, 1.0 * tile_m];
        let big = [2.5 * tile_m, -3.2 * tile_m];
        for &ox in steps.iter().chain(big.iter()) {
            for &oy in steps.iter().chain(big.iter()) {
                assert_covered(view, (ox, oy), zoom);
            }
        }
    }

    #[test]
    fn zero_offset_selects_exactly_the_view_tiles() {
        let zoom = 5;
        let half = MERCATOR_HALF_WORLD_M;
        let tile_m = half * 2.0 / 32.0;
        let view = MercRect::new(tile_m * 0.1 - half, tile_m * 1.9 - half, 0.0, 1.0);
        let tiles = tiles_covering(view, (0.0, 0.0), zoom);
        let xs: std::collections::BTreeSet<u32> = tiles.iter().map(|t| t.x).collect();
        assert_eq!(xs.into_iter().collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn tile_coverage_lists_whole_columns_before_the_next_column() {
        let everything = MercRect::new(-1.0e9, 1.0e9, -1.0e9, 1.0e9);
        let tiles = tiles_covering(everything, (0.0, 0.0), 1);
        let coords: Vec<(u32, u32)> = tiles.iter().map(|t| (t.x, t.y)).collect();
        assert_eq!(coords, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn tile_coverage_is_clamped_to_the_grid() {
        let everything = MercRect::new(-1.0e9, 1.0e9, -1.0e9, 1.0e9);
        let tiles = tiles_covering(everything, (0.0, 0.0), 2);
        assert_eq!(tiles.len(), 16);
        assert!(tiles.iter().all(|t| t.x < 4 && t.y < 4));
    }

    #[test]
    fn shifting_east_fetches_tiles_to_the_west() {
        // Imagery shifted east: the view's west edge now shows imagery that
        // lies west of it.
        let zoom = 10;
        let tile_m = MERCATOR_HALF_WORLD_M * 2.0 / 1024.0;
        let view = MercRect::new(0.5 * tile_m, 1.5 * tile_m, 0.5 * tile_m, 0.6 * tile_m);
        let plain = tiles_covering(view, (0.0, 0.0), zoom);
        let shifted = tiles_covering(view, (tile_m, 0.0), zoom);
        let min_x = |t: &[TileCoord]| t.iter().map(|t| t.x).min().unwrap();
        assert_eq!(min_x(&shifted) + 1, min_x(&plain));
    }

    #[test]
    fn a_zoom_beyond_the_grid_width_does_not_overflow() {
        let tiles = tiles_covering_geo(&GeoBounds::new(0.0, 0.0, 0.0, 0.0), 40);
        assert_eq!(tiles.len(), 1);
    }
}
