//! Optional automatic download of OSM data once the map stops moving.
//!
//! Off by default (Settings > Editing). When on, a pan or zoom that has been
//! still for [`SETTLE_DELAY`] while zoomed in to at least [`MIN_ZOOM`]
//! downloads the visible area, unless the layer already holds all of it. The
//! area is cut along a fixed grid of tiles ([`TILE_ZOOM`]) and only tiles the
//! layer does not already hold are downloaded, one at a time, nearest the view
//! centre first. Because the grid never moves, panning never fetches the same
//! ground twice, and neighbouring downloads abut instead of overlapping. Each
//! tile goes through the same path as File > Download from OSM, so it merges
//! into the active layer, keeps local edits, respects the server's area limit
//! and shows in the status bar's transfer indicator.
//!
//! This module is the decision logic, free of any UI: the debounce state
//! machine ([`AutoFetch`]) and the tile choice ([`tiles_to_fetch`]).

use crate::coordinates::GeoBounds;
use crate::coordinates::{lat_lon_to_mercator, LatLon};
use crate::download_bounds::{outside_region, MercRect};
use std::time::Duration;

/// Below this zoom nothing is fetched: the view would span too much data
/// (and usually more than the server allows in one request).
pub const MIN_ZOOM: f64 = 16.0;

/// The zoom of the tile grid downloads are cut along. A tile is about 600 m
/// across at the equator, small enough to stay under the server's node limit
/// even in dense cities.
pub const TILE_ZOOM: u32 = 16;

/// The most tiles queued for one view; the ones nearest its centre win.
pub const MAX_TILES: usize = 36;

/// How long the view must stay unchanged before a fetch starts.
pub const SETTLE_DELAY: Duration = Duration::from_millis(800);

/// What the map is looking at, compared to see whether it moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewKey {
    bounds: GeoBounds,
    zoom: f64,
}

impl ViewKey {
    pub fn new(bounds: GeoBounds, zoom: f64) -> Self {
        Self { bounds, zoom }
    }

    pub fn bounds(&self) -> GeoBounds {
        self.bounds
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }
}

/// The Mercator rectangle of a lat/lon one, `None` if degenerate.
fn merc_rect(b: &GeoBounds) -> Option<MercRect> {
    let min = lat_lon_to_mercator(LatLon::new(b.min_lat, b.min_lon));
    let max = lat_lon_to_mercator(LatLon::new(b.max_lat, b.max_lon));
    let ok = [min.x, min.y, max.x, max.y].iter().all(|v| v.is_finite())
        && min.x < max.x
        && min.y < max.y;
    ok.then_some(MercRect::new(min.x, max.x, min.y, max.y))
}

/// Whether `rects` together cover all of `view`.
pub fn covers(rects: &[MercRect], view: &GeoBounds) -> bool {
    let Some(view) = merc_rect(view) else {
        // Nothing sensible to fetch for a degenerate view.
        return true;
    };
    if rects.is_empty() {
        return false;
    }
    outside_region(view, rects).dim.is_empty()
}

/// The lat/lon bounds of slippy-map tile `x`, `y` at zoom `z`.
fn tile_bounds(z: u32, x: u32, y: u32) -> GeoBounds {
    let n = f64::from(1u32 << z);
    let lon = |x: f64| x / n * 360.0 - 180.0;
    let lat = |y: f64| {
        (std::f64::consts::PI * (1.0 - 2.0 * y / n))
            .sinh()
            .atan()
            .to_degrees()
    };
    GeoBounds::new(
        lat(f64::from(y) + 1.0),
        lat(f64::from(y)),
        lon(f64::from(x)),
        lon(f64::from(x) + 1.0),
    )
}

/// The tile column and row holding `lat`, `lon` at zoom `z`.
fn tile_of(z: u32, lat: f64, lon: f64) -> (i64, i64) {
    let n = f64::from(1u32 << z);
    let lat = lat.clamp(-85.05112878, 85.05112878).to_radians();
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n;
    (x.floor() as i64, y.floor() as i64)
}

/// The tiles to download for the settled view, nearest its centre first:
/// those that touch the view and are not already wholly held. `known` is the
/// downloaded area of the layer a download would merge into: `None` when
/// there is no such layer yet (the first download creates it), `Some(&[])`
/// for a layer that reports no downloaded area (say a file with no
/// `<bounds>`), which is left alone rather than guessed at.
pub fn tiles_to_fetch(view: &ViewKey, known: Option<&[MercRect]>) -> Vec<GeoBounds> {
    if view.zoom < MIN_ZOOM || matches!(known, Some([])) {
        return Vec::new();
    }
    let b = view.bounds;
    if !([b.min_lat, b.max_lat, b.min_lon, b.max_lon]
        .iter()
        .all(|v| v.is_finite())
        && b.min_lat < b.max_lat
        && b.min_lon < b.max_lon)
    {
        return Vec::new();
    }
    let (x0, y1) = tile_of(TILE_ZOOM, b.min_lat, b.min_lon);
    let (x1, y0) = tile_of(TILE_ZOOM, b.max_lat, b.max_lon);
    let max_index = (1i64 << TILE_ZOOM) - 1;
    let (x0, x1) = (x0.clamp(0, max_index), x1.clamp(0, max_index));
    let (y0, y1) = (y0.clamp(0, max_index), y1.clamp(0, max_index));
    // A view this large is not something to fetch tile by tile.
    if (x1 - x0 + 1) * (y1 - y0 + 1) > 4 * MAX_TILES as i64 {
        return Vec::new();
    }
    let centre = ((b.min_lat + b.max_lat) / 2.0, (b.min_lon + b.max_lon) / 2.0);
    let mut tiles: Vec<(f64, GeoBounds)> = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let tile = tile_bounds(TILE_ZOOM, x as u32, y as u32);
            if known.is_some_and(|rects| covers(rects, &tile)) {
                continue;
            }
            let dlat = (tile.min_lat + tile.max_lat) / 2.0 - centre.0;
            let dlon = (tile.min_lon + tile.max_lon) / 2.0 - centre.1;
            tiles.push((dlat * dlat + dlon * dlon, tile));
        }
    }
    tiles.sort_by(|a, b| a.0.total_cmp(&b.0));
    tiles.truncate(MAX_TILES);
    tiles.into_iter().map(|(_, t)| t).collect()
}

/// The debounce: tracks the last view seen and whether a settle timer is
/// running, and remembers the last view a fetch was started for so a failed
/// or empty fetch is not retried until the view changes.
#[derive(Debug, Default)]
pub struct AutoFetch {
    seen: Option<ViewKey>,
    timer_running: bool,
    attempted: Option<ViewKey>,
}

/// What a settle timer that just fired should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Settled {
    /// The view moved while waiting: start another timer for it.
    Wait(ViewKey),
    /// The view held still: decide whether to fetch it.
    Check(ViewKey),
    /// Already tried this view, or feature off: nothing to do.
    Nothing,
}

impl AutoFetch {
    /// Called every frame with the current view (`None` while the feature
    /// is off, which forgets everything so turning it on starts afresh).
    /// Returns the view to start a settle timer for, when one is needed.
    pub fn frame(&mut self, view: Option<ViewKey>) -> Option<ViewKey> {
        let Some(view) = view else {
            *self = Self::default();
            return None;
        };
        if self.seen == Some(view) {
            return None;
        }
        self.seen = Some(view);
        if self.timer_running {
            // The running timer will notice the move when it fires.
            return None;
        }
        self.timer_running = true;
        Some(view)
    }

    /// A settle timer that was started for `started` fired.
    pub fn timer_fired(&mut self, started: ViewKey) -> Settled {
        self.timer_running = false;
        let Some(now) = self.seen else {
            return Settled::Nothing;
        };
        if now != started {
            self.timer_running = true;
            return Settled::Wait(now);
        }
        if self.attempted == Some(now) {
            return Settled::Nothing;
        }
        Settled::Check(now)
    }

    /// A download was started for `view`.
    pub fn attempted(&mut self, view: ViewKey) {
        self.attempted = Some(view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(lat: f64, zoom: f64) -> ViewKey {
        ViewKey::new(GeoBounds::new(lat, lat + 0.002, 10.0, 10.003), zoom)
    }

    fn should_fetch(v: &ViewKey, known: Option<&[MercRect]>) -> bool {
        !tiles_to_fetch(v, known).is_empty()
    }

    fn rect(b: GeoBounds) -> MercRect {
        merc_rect(&b).unwrap()
    }

    #[test]
    fn nothing_is_fetched_below_the_minimum_zoom() {
        assert!(!should_fetch(&view(50.0, 15.9), None));
        assert!(should_fetch(&view(50.0, 16.0), None));
    }

    #[test]
    fn a_missing_layer_is_fetched_but_one_with_unknown_coverage_is_left_alone() {
        let v = view(50.0, 17.0);
        assert!(should_fetch(&v, None));
        assert!(!should_fetch(&v, Some(&[])));
    }

    #[test]
    fn a_fully_covered_view_is_not_fetched_again() {
        let v = view(50.0, 17.0);
        let around = rect(GeoBounds::new(49.9, 50.1, 9.9, 10.1));
        assert!(!should_fetch(&v, Some(&[around])));
    }

    #[test]
    fn a_partly_covered_view_is_fetched() {
        let v = view(50.0, 17.0);
        let half = rect(GeoBounds::new(49.9, 50.001, 9.9, 10.1));
        assert!(should_fetch(&v, Some(&[half])));
        let elsewhere = rect(GeoBounds::new(40.0, 40.1, 9.99, 10.01));
        assert!(should_fetch(&v, Some(&[elsewhere])));
    }

    #[test]
    fn two_rectangles_that_together_cover_the_view_count_as_covered() {
        let v = view(50.0, 17.0);
        let west = rect(GeoBounds::new(49.9, 50.1, 9.9, 10.0015));
        let east = rect(GeoBounds::new(49.9, 50.1, 10.0015, 10.1));
        assert!(!should_fetch(&v, Some(&[west, east])));
        // A gap between them is not covered.
        let east_far = rect(GeoBounds::new(49.9, 50.1, 10.002, 10.1));
        assert!(should_fetch(&v, Some(&[west, east_far])));
    }

    fn wide_view(lat: f64, lon: f64) -> ViewKey {
        ViewKey::new(GeoBounds::new(lat, lat + 0.008, lon, lon + 0.012), 17.0)
    }

    #[test]
    fn tiles_cover_the_view_without_overlapping() {
        let v = wide_view(50.0, 10.0);
        let tiles = tiles_to_fetch(&v, None);
        assert!(tiles.len() > 1 && tiles.len() <= MAX_TILES);
        let rects: Vec<MercRect> = tiles.iter().map(|t| rect(*t)).collect();
        assert!(covers(&rects, &v.bounds()));
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap_x = a.min_x.max(b.min_x) < a.max_x.min(b.max_x) - 1e-6;
                let overlap_y = a.min_y.max(b.min_y) < a.max_y.min(b.max_y) - 1e-6;
                assert!(!(overlap_x && overlap_y), "tiles overlap");
            }
        }
    }

    #[test]
    fn the_tile_nearest_the_view_centre_comes_first() {
        let v = wide_view(50.0, 10.0);
        let tiles = tiles_to_fetch(&v, None);
        let first = tiles[0];
        assert!(first.min_lat <= 50.004 && 50.004 <= first.max_lat);
        assert!(first.min_lon <= 10.006 && 10.006 <= first.max_lon);
    }

    #[test]
    fn once_the_tiles_are_held_nothing_is_left_and_a_pan_fetches_only_the_new_edge() {
        let v = wide_view(50.0, 10.0);
        let held: Vec<MercRect> = tiles_to_fetch(&v, None).iter().map(|t| rect(*t)).collect();
        assert!(tiles_to_fetch(&v, Some(&held)).is_empty());
        // Panned east by a little: only tiles not already held are wanted.
        let panned = wide_view(50.0, 10.004);
        let more = tiles_to_fetch(&panned, Some(&held));
        assert!(!more.is_empty());
        for t in &more {
            let r = rect(*t);
            assert!(!held
                .iter()
                .any(|h| (h.min_x - r.min_x).abs() < 1e-6 && (h.min_y - r.min_y).abs() < 1e-6));
        }
    }

    #[test]
    fn a_huge_view_is_not_tiled() {
        let v = ViewKey::new(GeoBounds::new(40.0, 50.0, 0.0, 10.0), 17.0);
        assert!(tiles_to_fetch(&v, None).is_empty());
    }

    #[test]
    fn a_still_view_starts_one_timer_and_then_checks() {
        let mut a = AutoFetch::default();
        let v = view(50.0, 17.0);
        assert_eq!(a.frame(Some(v)), Some(v));
        // More frames of the same view do not start more timers.
        assert_eq!(a.frame(Some(v)), None);
        assert_eq!(a.timer_fired(v), Settled::Check(v));
    }

    #[test]
    fn a_view_that_moved_during_the_wait_restarts_it() {
        let mut a = AutoFetch::default();
        let (v1, v2, v3) = (view(50.0, 17.0), view(50.001, 17.0), view(50.002, 17.0));
        assert_eq!(a.frame(Some(v1)), Some(v1));
        assert_eq!(a.frame(Some(v2)), None);
        assert_eq!(a.frame(Some(v3)), None);
        // The first timer fires: the view is no longer v1, so wait for v3.
        assert_eq!(a.timer_fired(v1), Settled::Wait(v3));
        assert_eq!(a.frame(Some(v3)), None);
        assert_eq!(a.timer_fired(v3), Settled::Check(v3));
    }

    #[test]
    fn an_attempted_view_is_not_retried_until_it_changes() {
        let mut a = AutoFetch::default();
        let v = view(50.0, 17.0);
        a.frame(Some(v));
        assert_eq!(a.timer_fired(v), Settled::Check(v));
        a.attempted(v);
        // A frame of the same view starts nothing; if a timer ran anyway it
        // would do nothing.
        assert_eq!(a.frame(Some(v)), None);
        assert_eq!(a.timer_fired(v), Settled::Nothing);
        // Moving away and back to a fresh view checks again.
        let w = view(50.01, 17.0);
        assert_eq!(a.frame(Some(w)), Some(w));
        assert_eq!(a.timer_fired(w), Settled::Check(w));
    }

    #[test]
    fn turning_the_feature_off_forgets_the_view() {
        let mut a = AutoFetch::default();
        let v = view(50.0, 17.0);
        a.frame(Some(v));
        a.attempted(v);
        assert_eq!(a.frame(None), None);
        // Back on with the same view: it counts as new.
        assert_eq!(a.frame(Some(v)), Some(v));
        assert_eq!(a.timer_fired(v), Settled::Check(v));
    }
}
