//! Imagery offset: shifting an imagery layer on screen so it lines up with
//! GPS traces or existing data before tracing.
//!
//! An offset is kept in ground metres east and north (not pixels or degrees),
//! so it means the same thing at every zoom. Everything here is pure maths,
//! the adjust session's state machine, and the small per-imagery-source store
//! that remembers offsets; drawing and input handling live elsewhere
//! (`layers::tile_layer`, and `imagery_offset_ui` in the binary).
//!
//! The offset is applied to where imagery pixels are painted only: OSM data,
//! the grid, hit-testing and every coordinate stay where they are.

use crate::coordinates::{LatLon, WORLD_WIDTH_M};
use crate::persist::{self, JsonStore, WriteOpts};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Edge of a tile in screen pixels at its own zoom.
const TILE_PX: f64 = 256.0;

/// A stored offset only auto-applies when the view is this close (metres) to
/// where it was made.
pub const NEAR_RADIUS_M: f64 = 10_000.0;

/// How far one plain arrow press moves the imagery, in screen pixels.
pub const NUDGE_PX: f64 = 1.0;
/// ... and with Shift held.
pub const NUDGE_PX_SHIFT: f64 = 10.0;

/// An imagery shift in ground metres. Positive `east_m` moves the imagery
/// east (right on screen), positive `north_m` moves it north (up).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ImageryOffset {
    pub east_m: f64,
    pub north_m: f64,
}

impl ImageryOffset {
    pub const ZERO: ImageryOffset = ImageryOffset {
        east_m: 0.0,
        north_m: 0.0,
    };

    pub fn new(east_m: f64, north_m: f64) -> Self {
        Self { east_m, north_m }
    }

    /// Whether the offset would show as `E 0.00 m, N 0.00 m`, i.e. there is
    /// nothing to warn the mapper about.
    pub fn is_zero(&self) -> bool {
        self.east_m.abs() < 0.005 && self.north_m.abs() < 0.005
    }

    pub fn plus(self, other: ImageryOffset) -> ImageryOffset {
        ImageryOffset::new(self.east_m + other.east_m, self.north_m + other.north_m)
    }
}

/// Ground metres covered by one screen pixel at `zoom` and `lat`.
pub fn metres_per_pixel(zoom: f64, lat: f64) -> f64 {
    WORLD_WIDTH_M * lat.to_radians().cos() / (TILE_PX * 2f64.powf(zoom))
}

/// The on-screen shift `(dx, dy)` in pixels (x right, y down) of imagery
/// carrying `offset`, at `zoom` and `lat`.
pub fn metres_to_pixels(offset: ImageryOffset, zoom: f64, lat: f64) -> (f64, f64) {
    let mpp = metres_per_pixel(zoom, lat);
    (offset.east_m / mpp, -offset.north_m / mpp)
}

/// The inverse of [`metres_to_pixels`]: the offset that shifts imagery by
/// `(dx, dy)` screen pixels.
pub fn pixels_to_metres(dx: f64, dy: f64, zoom: f64, lat: f64) -> ImageryOffset {
    let mpp = metres_per_pixel(zoom, lat);
    ImageryOffset::new(dx * mpp, -dy * mpp)
}

/// The offset in Web Mercator metres `(dx, dy)` at `lat`. Mercator stretches
/// ground distances by `1 / cos(lat)`, so the same ground shift is a bigger
/// Mercator shift away from the equator. Both the tile selection and the tile
/// painting go through this one function.
pub fn offset_to_mercator(offset: ImageryOffset, lat: f64) -> (f64, f64) {
    let scale = 1.0 / lat.to_radians().cos().max(1e-6);
    (offset.east_m * scale, offset.north_m * scale)
}

/// An arrow key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    Left,
    Right,
    Up,
    Down,
}

impl Nudge {
    pub fn from_key(key: &str) -> Option<Nudge> {
        match key {
            "left" => Some(Nudge::Left),
            "right" => Some(Nudge::Right),
            "up" => Some(Nudge::Up),
            "down" => Some(Nudge::Down),
            _ => None,
        }
    }
}

/// The change to the offset for one arrow press: one screen pixel's worth of
/// metres (ten with `big`), in the direction the imagery moves on screen.
pub fn nudge_step(dir: Nudge, big: bool, zoom: f64, lat: f64) -> ImageryOffset {
    let px = if big { NUDGE_PX_SHIFT } else { NUDGE_PX };
    let (dx, dy) = match dir {
        Nudge::Left => (-px, 0.0),
        Nudge::Right => (px, 0.0),
        Nudge::Up => (0.0, -px),
        Nudge::Down => (0.0, px),
    };
    pixels_to_metres(dx, dy, zoom, lat)
}

/// `value` with two decimals and a real minus sign (U+2212) for negatives.
/// A value that rounds to zero shows no sign.
pub fn format_metres(value: f64) -> String {
    let text = format!("{:.2}", value.abs());
    if value < 0.0 && text != "0.00" {
        format!("\u{2212}{text}")
    } else {
        text
    }
}

/// Parse what a person typed into a metres field: a plain hyphen, a real
/// minus sign and a decimal comma are all accepted.
pub fn parse_metres(text: &str) -> Option<f64> {
    let cleaned = text
        .trim()
        .trim_end_matches('m')
        .trim()
        .replace('\u{2212}', "-")
        .replace(',', ".");
    let value: f64 = cleaned.parse().ok()?;
    value.is_finite().then_some(value)
}

/// An offset being adjusted: the value at entry (what Escape goes back to)
/// and the live one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OffsetSession {
    entry: ImageryOffset,
    current: ImageryOffset,
}

impl OffsetSession {
    pub fn new(entry: ImageryOffset) -> Self {
        Self {
            entry,
            current: entry,
        }
    }

    pub fn entry(&self) -> ImageryOffset {
        self.entry
    }

    pub fn current(&self) -> ImageryOffset {
        self.current
    }

    /// The imagery was dragged `(dx, dy)` screen pixels.
    pub fn drag_by_pixels(&mut self, dx: f64, dy: f64, zoom: f64, lat: f64) {
        self.current = self.current.plus(pixels_to_metres(dx, dy, zoom, lat));
    }

    pub fn nudge(&mut self, dir: Nudge, big: bool, zoom: f64, lat: f64) {
        self.current = self.current.plus(nudge_step(dir, big, zoom, lat));
    }

    pub fn set(&mut self, offset: ImageryOffset) {
        self.current = offset;
    }

    pub fn reset(&mut self) {
        self.current = ImageryOffset::ZERO;
    }

    /// Escape: back to the offset at entry, which is returned.
    pub fn cancel(&mut self) -> ImageryOffset {
        self.current = self.entry;
        self.entry
    }
}

/// One remembered offset: for which imagery source, and the place it was made
/// (view centre) since an offset is only right near there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredOffset {
    pub imagery_id: String,
    pub east_m: f64,
    pub north_m: f64,
    pub lat: f64,
    pub lon: f64,
}

impl StoredOffset {
    pub fn offset(&self) -> ImageryOffset {
        ImageryOffset::new(self.east_m, self.north_m)
    }

    /// Where the offset was made.
    pub fn at(&self) -> LatLon {
        LatLon::new(self.lat, self.lon)
    }
}

/// The file's contents: a flat list so a source can have several offsets for
/// different places.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OffsetStore {
    #[serde(default)]
    pub entries: Vec<StoredOffset>,
}

/// Most entries kept; the oldest go first.
const MAX_ENTRIES: usize = 200;

/// Great-circle distance in metres.
pub fn distance_m(a: LatLon, b: LatLon) -> f64 {
    let (la, lb) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = lb - la;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la.cos() * lb.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().min(1.0).asin()
}

/// Whether `stored` should auto-apply to a view centred on `at`: the view is
/// within [`NEAR_RADIUS_M`] of where the offset was made.
pub fn is_near(stored: &StoredOffset, at: LatLon) -> bool {
    distance_m(stored.at(), at) <= NEAR_RADIUS_M
}

impl OffsetStore {
    /// The offset to auto-apply to `imagery_id` for a view centred on `at`:
    /// the nearest remembered one within [`NEAR_RADIUS_M`], or none (the layer
    /// then starts at zero and the stored ones are left alone).
    pub fn lookup(&self, imagery_id: &str, at: LatLon) -> Option<ImageryOffset> {
        self.entries
            .iter()
            .filter(|e| e.imagery_id == imagery_id && is_near(e, at))
            .min_by(|a, b| distance_m(a.at(), at).total_cmp(&distance_m(b.at(), at)))
            .map(StoredOffset::offset)
    }

    /// Remember `offset` for `imagery_id` made at `at`. It replaces any
    /// remembered offset for that source within range of the same place (a
    /// zero offset just removes it); offsets made elsewhere are kept.
    pub fn remember(&mut self, imagery_id: &str, offset: ImageryOffset, at: LatLon) {
        self.entries
            .retain(|e| !(e.imagery_id == imagery_id && is_near(e, at)));
        if !offset.is_zero() {
            self.entries.push(StoredOffset {
                imagery_id: imagery_id.to_string(),
                east_m: offset.east_m,
                north_m: offset.north_m,
                lat: at.lat,
                lon: at.lon,
            });
        }
        if self.entries.len() > MAX_ENTRIES {
            let extra = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..extra);
        }
    }
}

/// Load from `path`; the default (empty) store when the file is missing or
/// unreadable.
pub fn load_from(path: &Path) -> OffsetStore {
    persist::load_json(path, "imagery_offset_store")
}

pub fn save_to(path: &Path, store: &OffsetStore) -> std::io::Result<()> {
    persist::save_json(path, store, WriteOpts::default())
}

/// `<config_dir>/osm-gpui/imagery-offsets.json`, beside the custom-imagery
/// file. `None` where the persistence policy forbids touching real state (the
/// default, so no test can reach the real file).
pub fn default_path() -> Option<PathBuf> {
    persist::config_file("imagery-offsets.json")
}

static STORE: JsonStore<OffsetStore> = JsonStore::new();

/// The saved offsets, or an empty store where the persistence policy forbids
/// reading real state.
pub fn load() -> OffsetStore {
    default_path()
        .map(|path| load_from(&path))
        .unwrap_or_default()
}

/// Install the in-memory store (call once at startup). Whether `remember`
/// reaches the disk is the persistence policy's call, as for the other stores
/// (a scripted run is read-only, so its offsets live for the session only).
pub fn init_store(offsets: OffsetStore) {
    STORE.init(offsets);
}

/// The offset to auto-apply to a new layer of `imagery_id` viewed around
/// `at`, if one was remembered near there.
pub fn lookup(imagery_id: &str, at: LatLon) -> Option<ImageryOffset> {
    STORE
        .read("imagery_offset_store", |s| s.lookup(imagery_id, at))
        .flatten()
}

/// Remember `offset` for `imagery_id` as made around `at`, for this session
/// and (unless persistence is off) future launches.
pub fn remember(imagery_id: &str, offset: ImageryOffset, at: LatLon) {
    STORE.update_and_save(
        "imagery_offset_store",
        |s| s.remember(imagery_id, offset, at),
        |snapshot| {
            persist::save_reporting("imagery_offset_store", default_path().as_deref(), |p| {
                save_to(p, snapshot)
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-imagery-offset-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn metres_per_pixel_matches_known_values() {
        // Zoom 0 at the equator: the world's circumference over 256 px.
        assert!((metres_per_pixel(0.0, 0.0) - 156_543.03).abs() < 0.01);
        // Zoom 18 at the equator is about 0.597 m per pixel.
        assert!((metres_per_pixel(18.0, 0.0) - 0.5972).abs() < 1e-3);
        // At 60 degrees a pixel covers half the ground.
        let ratio = metres_per_pixel(10.0, 60.0) / metres_per_pixel(10.0, 0.0);
        assert!((ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn metres_to_pixels_signs_and_latitude_factor() {
        let (dx, dy) = metres_to_pixels(ImageryOffset::new(3.0, 2.0), 18.0, 0.0);
        assert!(dx > 0.0, "east is right");
        assert!(dy < 0.0, "north is up");
        let mpp = metres_per_pixel(18.0, 0.0);
        assert!((dx - 3.0 / mpp).abs() < 1e-9);
        assert!((dy + 2.0 / mpp).abs() < 1e-9);
        // The same metres are more pixels at 60 degrees (Mercator stretch).
        let (dx60, _) = metres_to_pixels(ImageryOffset::new(3.0, 0.0), 18.0, 60.0);
        assert!((dx60 / dx - 2.0).abs() < 1e-9);
    }

    #[test]
    fn pixels_and_metres_round_trip() {
        for &(zoom, lat) in &[(14.0, 0.0), (18.5, 40.7), (19.0, -33.9), (16.0, 64.1)] {
            let offset = ImageryOffset::new(12.34, -5.67);
            let (dx, dy) = metres_to_pixels(offset, zoom, lat);
            let back = pixels_to_metres(dx, dy, zoom, lat);
            assert!((back.east_m - offset.east_m).abs() < 1e-9);
            assert!((back.north_m - offset.north_m).abs() < 1e-9);
        }
    }

    #[test]
    fn mercator_offset_stretches_with_latitude() {
        let (x, y) = offset_to_mercator(ImageryOffset::new(10.0, -4.0), 0.0);
        assert!((x - 10.0).abs() < 1e-9 && (y + 4.0).abs() < 1e-9);
        let (x, y) = offset_to_mercator(ImageryOffset::new(10.0, -4.0), 60.0);
        assert!((x - 20.0).abs() < 1e-9 && (y + 8.0).abs() < 1e-9);
    }

    #[test]
    fn nudge_is_one_pixel_and_ten_with_shift() {
        let mpp = metres_per_pixel(18.0, 40.0);
        let right = nudge_step(Nudge::Right, false, 18.0, 40.0);
        assert!((right.east_m - mpp).abs() < 1e-9 && right.north_m == 0.0);
        let up = nudge_step(Nudge::Up, false, 18.0, 40.0);
        assert!((up.north_m - mpp).abs() < 1e-9 && up.east_m == 0.0);
        let down_big = nudge_step(Nudge::Down, true, 18.0, 40.0);
        assert!((down_big.north_m + 10.0 * mpp).abs() < 1e-9);
        let left_big = nudge_step(Nudge::Left, true, 18.0, 40.0);
        assert!((left_big.east_m + 10.0 * mpp).abs() < 1e-9);
        assert_eq!(Nudge::from_key("left"), Some(Nudge::Left));
        assert_eq!(Nudge::from_key("a"), None);
    }

    #[test]
    fn formatting_uses_two_decimals_and_a_real_minus() {
        assert_eq!(format_metres(3.2), "3.20");
        assert_eq!(format_metres(-1.75), "\u{2212}1.75");
        assert_eq!(format_metres(-0.001), "0.00");
        assert_eq!(format_metres(0.0), "0.00");
    }

    #[test]
    fn parsing_accepts_minus_forms_and_decimal_comma() {
        assert_eq!(parse_metres("3.2"), Some(3.2));
        assert_eq!(parse_metres("-1.75"), Some(-1.75));
        assert_eq!(parse_metres("\u{2212}1.75"), Some(-1.75));
        assert_eq!(parse_metres(" 2,5 m"), Some(2.5));
        assert_eq!(parse_metres("abc"), None);
        assert_eq!(parse_metres("NaN"), None);
        assert_eq!(parse_metres(""), None);
    }

    #[test]
    fn escape_restores_the_offset_at_entry() {
        let mut session = OffsetSession::new(ImageryOffset::new(1.5, -2.0));
        session.drag_by_pixels(30.0, -12.0, 18.0, 40.0);
        session.nudge(Nudge::Left, true, 18.0, 40.0);
        assert_ne!(session.current(), session.entry());
        assert_eq!(session.cancel(), ImageryOffset::new(1.5, -2.0));
        assert_eq!(session.current(), ImageryOffset::new(1.5, -2.0));
    }

    #[test]
    fn drag_and_reset_adjust_the_live_offset() {
        let mut session = OffsetSession::new(ImageryOffset::ZERO);
        session.drag_by_pixels(10.0, 0.0, 18.0, 0.0);
        let mpp = metres_per_pixel(18.0, 0.0);
        assert!((session.current().east_m - 10.0 * mpp).abs() < 1e-9);
        session.reset();
        assert!(session.current().is_zero());
        // Reset is not final: Escape still goes back to the entry value.
        let mut session = OffsetSession::new(ImageryOffset::new(4.0, 4.0));
        session.reset();
        assert_eq!(session.cancel(), ImageryOffset::new(4.0, 4.0));
    }

    #[test]
    fn near_rule_is_ten_kilometres() {
        let stored = StoredOffset {
            imagery_id: "x".into(),
            east_m: 3.0,
            north_m: 1.0,
            lat: 40.0,
            lon: -74.0,
        };
        assert!(is_near(&stored, LatLon::new(40.0, -74.0)));
        // About 5.5 km north.
        assert!(is_near(&stored, LatLon::new(40.05, -74.0)));
        // About 11 km north.
        assert!(!is_near(&stored, LatLon::new(40.1, -74.0)));
        // Across the world.
        assert!(!is_near(&stored, LatLon::new(-33.9, 151.2)));
    }

    #[test]
    fn two_stores_do_not_share_offsets() {
        let mut a = OffsetStore::default();
        let b = OffsetStore::default();
        a.remember(
            "eli-1",
            ImageryOffset::new(3.0, -1.0),
            LatLon::new(40.0, -74.0),
        );
        assert!(a.lookup("eli-1", LatLon::new(40.0, -74.0)).is_some());
        assert_eq!(b.lookup("eli-1", LatLon::new(40.0, -74.0)), None);
    }

    #[test]
    fn nothing_is_read_or_written_under_the_default_policy() {
        // Unit tests run with persistence disabled: no real file is named, so
        // `remember` has nowhere to write.
        assert!(default_path().is_none());
        assert_eq!(load(), OffsetStore::default());
    }

    #[test]
    fn store_lookup_applies_only_near_and_keeps_far_entries() {
        let mut store = OffsetStore::default();
        store.remember(
            "eli-1",
            ImageryOffset::new(3.0, -1.0),
            LatLon::new(40.0, -74.0),
        );
        store.remember(
            "eli-1",
            ImageryOffset::new(-2.0, 5.0),
            LatLon::new(51.5, -0.1),
        );
        store.remember(
            "other",
            ImageryOffset::new(9.0, 9.0),
            LatLon::new(40.0, -74.0),
        );
        assert_eq!(
            store.lookup("eli-1", LatLon::new(40.01, -74.01)),
            Some(ImageryOffset::new(3.0, -1.0))
        );
        assert_eq!(
            store.lookup("eli-1", LatLon::new(51.5, -0.1)),
            Some(ImageryOffset::new(-2.0, 5.0))
        );
        assert_eq!(store.lookup("eli-1", LatLon::new(0.0, 0.0)), None);
        assert_eq!(store.lookup("missing", LatLon::new(40.0, -74.0)), None);
        // Setting a new offset at the first place replaces only that one.
        store.remember(
            "eli-1",
            ImageryOffset::new(4.0, 4.0),
            LatLon::new(40.02, -74.0),
        );
        assert_eq!(store.entries.len(), 3);
        assert_eq!(
            store.lookup("eli-1", LatLon::new(40.0, -74.0)),
            Some(ImageryOffset::new(4.0, 4.0))
        );
        // Resetting to zero drops it and leaves the other place alone.
        store.remember("eli-1", ImageryOffset::ZERO, LatLon::new(40.0, -74.0));
        assert_eq!(store.lookup("eli-1", LatLon::new(40.0, -74.0)), None);
        assert!(store.lookup("eli-1", LatLon::new(51.5, -0.1)).is_some());
    }

    #[test]
    fn store_round_trips_through_a_file() {
        let path = tmp_dir("round-trip").join("imagery-offsets.json");
        let mut store = OffsetStore::default();
        store.remember(
            "eli-1",
            ImageryOffset::new(3.2, -1.75),
            LatLon::new(40.7, -74.0),
        );
        store.remember(
            "custom:https://t/{z}/{x}/{y}",
            ImageryOffset::new(0.5, 0.5),
            LatLon::new(1.0, 2.0),
        );
        save_to(&path, &store).unwrap();
        assert_eq!(load_from(&path), store);
    }

    #[test]
    fn missing_or_corrupt_file_is_an_empty_store() {
        let dir = tmp_dir("missing");
        assert_eq!(load_from(&dir.join("nope.json")), OffsetStore::default());
        let bad = dir.join("bad.json");
        std::fs::write(&bad, b"not json {{").unwrap();
        assert_eq!(load_from(&bad), OffsetStore::default());
    }

    #[test]
    fn the_real_path_is_never_used_under_test() {
        assert_eq!(default_path(), None);
    }

    #[test]
    fn oldest_entries_are_dropped_past_the_cap() {
        let mut store = OffsetStore::default();
        for i in 0..(MAX_ENTRIES + 5) {
            // 1 degree apart: never near each other.
            store.remember(
                "x",
                ImageryOffset::new(1.0, 1.0),
                LatLon::new((i % 80) as f64, (i / 80) as f64 * 3.0),
            );
        }
        assert!(store.entries.len() <= MAX_ENTRIES);
    }
}
