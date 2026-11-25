//! Optional automatic download of OSM data once the map stops moving.
//!
//! Off by default (Settings > Editing). When on, a pan or zoom that has been
//! still for [`SETTLE_DELAY`] while zoomed in to at least [`MIN_ZOOM`]
//! downloads the visible area, unless the layer already holds all of it. The
//! download goes through the same path as File > Download from OSM, so it
//! merges into the active layer, keeps local edits, respects the server's
//! area limit and shows in the status bar's transfer indicator.
//!
//! This module is the decision logic, free of any UI: the debounce state
//! machine ([`AutoFetch`]) and the coverage test ([`should_fetch`]).

use crate::coordinates::GeoBounds;
use crate::coordinates::{lat_lon_to_mercator, LatLon};
use crate::download_bounds::{outside_region, MercRect};
use std::time::Duration;

/// Below this zoom nothing is fetched: the view would span too much data
/// (and usually more than the server allows in one request).
pub const MIN_ZOOM: f64 = 16.0;

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

/// Whether the settled view calls for a download. `known` is the downloaded
/// area of the layer a download would merge into: `None` when there is no
/// such layer yet (the first download creates it), `Some(&[])` for a layer
/// that reports no downloaded area (say a file with no `<bounds>`), which is
/// left alone rather than guessed at.
pub fn should_fetch(view: &ViewKey, known: Option<&[MercRect]>) -> bool {
    if view.zoom < MIN_ZOOM {
        return false;
    }
    match known {
        None => true,
        Some([]) => false,
        Some(rects) => !covers(rects, &view.bounds),
    }
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
        let around = rect(GeoBounds::new(49.99, 50.01, 9.99, 10.01));
        assert!(!should_fetch(&v, Some(&[around])));
    }

    #[test]
    fn a_partly_covered_view_is_fetched() {
        let v = view(50.0, 17.0);
        let half = rect(GeoBounds::new(49.99, 50.001, 9.99, 10.01));
        assert!(should_fetch(&v, Some(&[half])));
        let elsewhere = rect(GeoBounds::new(40.0, 40.1, 9.99, 10.01));
        assert!(should_fetch(&v, Some(&[elsewhere])));
    }

    #[test]
    fn two_rectangles_that_together_cover_the_view_count_as_covered() {
        let v = view(50.0, 17.0);
        let west = rect(GeoBounds::new(49.99, 50.01, 9.99, 10.0015));
        let east = rect(GeoBounds::new(49.99, 50.01, 10.0015, 10.01));
        assert!(!should_fetch(&v, Some(&[west, east])));
        // A gap between them is not covered.
        let east_far = rect(GeoBounds::new(49.99, 50.01, 10.002, 10.01));
        assert!(should_fetch(&v, Some(&[west, east_far])));
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
