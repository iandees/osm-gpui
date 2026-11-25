//! What the Mapillary, Fotoshi and Panoramax extensions share.
//!
//! Each provider's photos are fetched by map tile for the view (and a margin
//! around it) once the map settles, a batch at a time, cached per tile, and
//! drawn as markers with the camera's heading. A click opens the photo in a
//! side-panel section: the picture (a double-click shows it full size), when
//! and by whom it was taken, its license, and a link to the provider's page.
//!
//! A provider says how to ask for and read its tiles and photo details by
//! implementing [`Source`]; [`Photos`] does the rest. Messages come from the
//! extension's own locale files, so each one carries the `photos.*` and
//! `photo.*` keys (see the Mapillary extension's `locales/en.toml`).

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::time::{Duration, Instant};

use osm_gpui_extension_api::{
    self as osm, Bounds, Element, HttpRequest, Image, LatLon, Marker, Panel,
};

pub mod mvt;

/// More tiles than this under the view means the map is zoomed out too far
/// to show photos.
pub const MAX_TILES_PER_VIEW: usize = 40;
/// Cached tiles before those out of view are dropped.
const MAX_CACHED_TILES: usize = 300;
/// How long a tile that failed to load is left alone.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// The side-panel section a photo opens in.
pub const PHOTO_PANEL: &str = "photo";
/// Storage key: the photos were shown when the app last ran.
const SHOWN_KEY: &str = "shown";
/// The menu command that shows and hides the photos, with a checkmark
/// (`menu = "overlays"`, `toggle = true` in the manifest).
pub const TOGGLE_COMMAND: &str = "photos";

/// One street-level photo.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Photo {
    /// The provider's own id.
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    /// Camera heading, degrees clockwise from north, when known.
    pub heading: Option<f32>,
    /// Capture date as `YYYY-MM-DD` when known.
    pub captured: Option<String>,
    pub creator: Option<String>,
    pub license: Option<String>,
    /// Small preview, and a larger rendition for the panel.
    pub thumb_url: Option<String>,
    pub large_url: Option<String>,
}

impl Photo {
    /// This photo (from a tile) with what `detail` knows: its values win,
    /// except that a value it lacks keeps this photo's.
    pub fn merged_with(&self, detail: Photo) -> Photo {
        Photo {
            id: self.id.clone(),
            lat: self.lat,
            lon: self.lon,
            heading: detail.heading.or(self.heading),
            captured: detail.captured.or_else(|| self.captured.clone()),
            creator: detail.creator.or_else(|| self.creator.clone()),
            license: detail.license.or_else(|| self.license.clone()),
            thumb_url: detail.thumb_url.or_else(|| self.thumb_url.clone()),
            large_url: detail.large_url.or_else(|| self.large_url.clone()),
        }
    }

    /// The picture to show: the larger rendition, else the preview.
    pub fn picture_url(&self) -> Option<&str> {
        self.large_url.as_deref().or(self.thumb_url.as_deref())
    }
}

// --- Map tiles ---

/// A Web Mercator map tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    pub z: u32,
    pub x: u32,
    pub y: u32,
}

/// Where tile-unit position `(x, y)` at zoom `z` is.
fn tile_position_to_lat_lon(z: u32, x: f64, y: f64) -> (f64, f64) {
    let n = 2.0_f64.powi(z as i32);
    let lat = (PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees();
    (lat, x / n * 360.0 - 180.0)
}

impl Tile {
    /// The tile's box.
    pub fn bounds(&self) -> Bounds {
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        let (max_lat, min_lon) = tile_position_to_lat_lon(self.z, x, y);
        let (min_lat, max_lon) = tile_position_to_lat_lon(self.z, x + 1.0, y + 1.0);
        Bounds {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        }
    }

    /// Where the point `(px, py)` of this tile, in a grid `extent` units wide,
    /// lies: `(lat, lon)`.
    pub fn point(&self, extent: u32, px: f32, py: f32) -> (f64, f64) {
        let e = f64::from(extent.max(1));
        tile_position_to_lat_lon(
            self.z,
            f64::from(self.x) + f64::from(px) / e,
            f64::from(self.y) + f64::from(py) / e,
        )
    }
}

fn lon_to_x(lon: f64, zoom: u32) -> f64 {
    (lon + 180.0) / 360.0 * 2.0_f64.powi(zoom as i32)
}

fn lat_to_y(lat: f64, zoom: u32) -> f64 {
    let lat = lat.clamp(-85.05112878, 85.05112878).to_radians();
    (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0 * 2.0_f64.powi(zoom as i32)
}

/// The tiles at `zoom` that cover `b`, row by row.
pub fn tiles_covering(b: &Bounds, zoom: u32) -> Vec<Tile> {
    let max_index = (1u32.checked_shl(zoom).unwrap_or(u32::MAX) - 1) as f64;
    let index = |v: f64| v.floor().clamp(0.0, max_index) as u32;
    let (x0, x1) = (
        index(lon_to_x(b.min_lon, zoom)),
        index(lon_to_x(b.max_lon, zoom)),
    );
    let (y0, y1) = (
        index(lat_to_y(b.max_lat, zoom)),
        index(lat_to_y(b.min_lat, zoom)),
    );
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(Tile { z: zoom, x, y });
        }
    }
    out
}

/// `b` grown by `factor` of its size on every side.
pub fn expand(b: &Bounds, factor: f64) -> Bounds {
    let (dw, dh) = (
        (b.max_lon - b.min_lon) * factor,
        (b.max_lat - b.min_lat) * factor,
    );
    Bounds {
        min_lat: (b.min_lat - dh).max(-85.0),
        max_lat: (b.max_lat + dh).min(85.0),
        min_lon: (b.min_lon - dw).max(-180.0),
        max_lon: (b.max_lon + dw).min(180.0),
    }
}

// --- Dates ---

/// `YYYY-MM-DD` (UTC) from Unix milliseconds, rounding toward the earlier day.
pub fn date_from_millis(ms: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = ms.div_euclid(1000).div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// --- Providers ---

/// One provider of street-level photos.
pub trait Source {
    /// The zoom of the tiles to ask for.
    fn zoom(&self) -> u32;
    /// The markers' colour, 0xRRGGBB.
    fn color(&self) -> u32;
    /// The request for tile `t`, or why there can be none (a missing
    /// credential), as a status-bar message. Such a failure stops fetching
    /// until [`Photos::unblock`].
    fn tile_request(&self, t: Tile) -> Result<HttpRequest, String>;
    /// The photos in tile `t`'s answer.
    fn decode_tile(&self, t: Tile, body: &[u8]) -> Result<Vec<Photo>, String>;
    /// The request for what a tile leaves out of `photo` (image URLs, the
    /// author), if the provider has one.
    fn detail_request(&self, _photo: &Photo) -> Option<HttpRequest> {
        None
    }
    /// The details in a [`Source::detail_request`] answer.
    fn decode_detail(&self, _body: &[u8]) -> Result<Option<Photo>, String> {
        Ok(None)
    }
    /// The photo's page on the provider's site.
    fn web_url(&self, id: &str) -> String;
    /// What to say when the provider refuses the credential.
    fn unauthorized_message(&self) -> String {
        osm::t("photos.unauthorized")
    }
    /// `message` with anything secret in it removed (an error can quote a
    /// request URL that carries a token).
    fn redact(&self, message: &str) -> String {
        message.to_string()
    }
}

/// A provider's photos on the map.
pub struct Photos<S: Source> {
    pub source: S,
    shown: bool,
    tiles: HashMap<Tile, Vec<Photo>>,
    /// Tiles that failed, and when.
    failed: HashMap<Tile, Instant>,
    /// A failure that would only repeat (no or refused credential, a spent
    /// budget) stops fetching until the user acts.
    blocked: bool,
    /// The view needs more tiles than are fetched; the status bar said so.
    zoomed_out: bool,
    /// The photo open in the panel.
    open: Option<Photo>,
}

impl<S: Source> Photos<S> {
    /// The photos are shown again if they were when the app last ran.
    pub fn new(source: S) -> Self {
        let shown = osm::storage_get(SHOWN_KEY).is_some();
        osm::set_command_checked(TOGGLE_COMMAND, shown);
        Photos {
            source,
            shown,
            tiles: HashMap::new(),
            failed: HashMap::new(),
            blocked: false,
            zoomed_out: false,
            open: None,
        }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Show Photos.
    pub fn show(&mut self) -> osm::Result<()> {
        self.shown = true;
        osm::set_command_checked(TOGGLE_COMMAND, true);
        osm::storage_set(SHOWN_KEY, Some("1"));
        self.blocked = false;
        self.zoomed_out = false;
        self.fetch()
    }

    /// Hide Photos: no markers, no photo panel, nothing cached.
    pub fn hide(&mut self) {
        self.shown = false;
        osm::set_command_checked(TOGGLE_COMMAND, false);
        osm::storage_set(SHOWN_KEY, None);
        self.tiles.clear();
        self.failed.clear();
        self.open = None;
        osm::set_markers(&[]);
        osm::show_panel(PHOTO_PANEL, None);
    }

    /// Forget what was loaded and look again.
    pub fn refresh(&mut self) -> osm::Result<()> {
        self.tiles.clear();
        self.failed.clear();
        self.blocked = false;
        self.zoomed_out = false;
        if self.shown {
            self.fetch()
        } else {
            Ok(())
        }
    }

    /// Fetching may start again (after the user changed the credential).
    pub fn unblock(&mut self) {
        self.blocked = false;
        self.failed.clear();
    }

    /// The map settled: fetch the tiles it moved onto.
    pub fn viewport_changed(&mut self) -> osm::Result<()> {
        if self.shown && !self.blocked {
            self.fetch()
        } else {
            Ok(())
        }
    }

    fn fetch(&mut self) -> osm::Result<()> {
        let needed = tiles_covering(&expand(&osm::viewport(), 0.15), self.source.zoom());
        if needed.len() > MAX_TILES_PER_VIEW {
            if !std::mem::replace(&mut self.zoomed_out, true) {
                osm::set_status(&osm::t("photos.zoom_in"));
            }
            return Ok(());
        }
        self.zoomed_out = false;
        if self.tiles.len() > MAX_CACHED_TILES {
            let keep: HashSet<Tile> = needed.iter().copied().collect();
            self.tiles.retain(|t, _| keep.contains(t));
        }
        let now = Instant::now();
        let missing: Vec<Tile> = needed
            .into_iter()
            .filter(|t| !self.tiles.contains_key(t))
            .filter(|t| {
                self.failed
                    .get(t)
                    .is_none_or(|at| now.duration_since(*at) >= RETRY_AFTER)
            })
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut requests = Vec::with_capacity(missing.len());
        for t in &missing {
            match self.source.tile_request(*t) {
                Ok(r) => requests.push(r),
                Err(message) => {
                    self.blocked = true;
                    osm::set_status(&message);
                    return Ok(());
                }
            }
        }
        let answers = osm::http_fetch_many(&requests);
        let mut problem: Option<String> = None;
        for (t, answer) in missing.into_iter().zip(answers) {
            match answer {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    match self.source.decode_tile(t, &resp.body) {
                        Ok(photos) => {
                            self.failed.remove(&t);
                            self.tiles.insert(t, photos);
                        }
                        Err(e) => {
                            self.failed.insert(t, now);
                            problem = Some(osm::t_args("photos.failed", &[("detail", &e)]));
                        }
                    }
                }
                // An empty tile.
                Ok(resp) if resp.status == 204 || resp.status == 404 => {
                    self.tiles.insert(t, Vec::new());
                }
                Ok(resp) if resp.status == 401 || resp.status == 403 => {
                    self.blocked = true;
                    problem = Some(self.source.unauthorized_message());
                    break;
                }
                Ok(resp) if resp.status == 429 => {
                    self.blocked = true;
                    problem = Some(osm::t("photos.rate_limited"));
                    break;
                }
                Ok(resp) => {
                    self.failed.insert(t, now);
                    let detail = format!("HTTP {}", resp.status);
                    problem = Some(osm::t_args("photos.failed", &[("detail", &detail)]));
                }
                Err(e) => {
                    self.failed.insert(t, now);
                    let detail = self.source.redact(&e);
                    problem = Some(osm::t_args("photos.failed", &[("detail", &detail)]));
                }
            }
        }
        self.draw();
        let message = problem.unwrap_or_else(|| {
            let count = self.photos().len();
            if count == 1 {
                osm::t("photos.loaded_one")
            } else {
                osm::t_args("photos.loaded", &[("count", &count.to_string())])
            }
        });
        osm::set_status(&message);
        Ok(())
    }

    /// Every loaded photo once (a photo on a tile edge can come from two
    /// tiles), in a stable order.
    fn photos(&self) -> Vec<&Photo> {
        let mut seen = HashSet::new();
        let mut out: Vec<&Photo> = self
            .tiles
            .values()
            .flatten()
            .filter(|p| seen.insert(p.id.as_str()))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn draw(&self) {
        let open = self.open.as_ref().map(|p| p.id.as_str());
        let color = self.source.color();
        let markers: Vec<Marker> = self
            .photos()
            .into_iter()
            .map(|p| Marker {
                id: p.id.clone(),
                at: LatLon {
                    lat: p.lat,
                    lon: p.lon,
                },
                color,
                selected: open == Some(p.id.as_str()),
                heading: p.heading,
            })
            .collect();
        osm::set_markers(&markers);
    }

    /// A marker was clicked: open its photo.
    pub fn marker_clicked(&mut self, id: &str) -> osm::Result<()> {
        let photo = self
            .photos()
            .into_iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("no photo {id}"))?;
        self.open(photo)
    }

    /// Open `photo` in the panel, first asking for its details when it came
    /// from a tile (which leaves out the image URLs and the author).
    pub fn open(&mut self, photo: Photo) -> osm::Result<()> {
        let mut photo = photo;
        if photo.picture_url().is_none() {
            if let Some(request) = self.source.detail_request(&photo) {
                match osm::http_fetch(&request) {
                    Ok(resp) if (200..300).contains(&resp.status) => {
                        match self.source.decode_detail(&resp.body) {
                            Ok(Some(detail)) => photo = photo.merged_with(detail),
                            Ok(None) => {}
                            Err(e) => {
                                osm::set_status(&osm::t_args("photos.failed", &[("detail", &e)]))
                            }
                        }
                    }
                    Ok(resp) if resp.status == 401 || resp.status == 403 => {
                        osm::set_status(&self.source.unauthorized_message())
                    }
                    Ok(resp) => osm::set_status(&osm::t_args(
                        "photos.failed",
                        &[("detail", &format!("HTTP {}", resp.status))],
                    )),
                    Err(e) => osm::set_status(&osm::t_args(
                        "photos.failed",
                        &[("detail", &self.source.redact(&e))],
                    )),
                }
                // Keep what was learned for the next time it is opened.
                for list in self.tiles.values_mut() {
                    for p in list.iter_mut().filter(|p| p.id == photo.id) {
                        *p = photo.clone();
                    }
                }
            }
        }
        osm::show_panel(PHOTO_PANEL, Some(&self.panel(&photo)));
        self.open = Some(photo);
        if self.shown {
            self.draw();
        }
        Ok(())
    }

    /// The user closed one of the extension's panels. Returns whether it
    /// was the photo panel.
    pub fn panel_closed(&mut self, panel: &str) -> bool {
        if panel != PHOTO_PANEL {
            return false;
        }
        self.open = None;
        if self.shown {
            self.draw();
        }
        true
    }

    fn panel(&self, photo: &Photo) -> Panel {
        let name = osm::t("name");
        let mut elements = Vec::new();
        match photo.picture_url() {
            Some(url) => elements.push(Element::Picture(Image {
                url: url.to_string(),
                alt: osm::t("photo.no_picture"),
            })),
            None => elements.push(Element::Muted(osm::t("photo.no_picture"))),
        }
        if let Some(date) = &photo.captured {
            elements.push(Element::Muted(osm::t_args(
                "photo.captured",
                &[("date", date)],
            )));
        }
        if let Some(creator) = &photo.creator {
            elements.push(Element::Muted(osm::t_args(
                "photo.by",
                &[("creator", creator)],
            )));
        }
        if let Some(license) = &photo.license {
            elements.push(Element::Muted(osm::t_args(
                "photo.license",
                &[("license", license)],
            )));
        }
        if let Some(h) = photo.heading {
            elements.push(Element::Muted(osm::t_args(
                "photo.heading",
                &[("degrees", &(h.round() as i32).to_string())],
            )));
        }
        elements.push(Element::Link((
            osm::t_args("photo.open_site", &[("provider", &name)]),
            self.source.web_url(&photo.id),
        )));
        Panel {
            title: osm::t("photo.title"),
            elements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_from_millis() {
        assert_eq!(date_from_millis(0), "1970-01-01");
        assert_eq!(date_from_millis(951_782_400_000), "2000-02-29");
        assert_eq!(date_from_millis(-1), "1969-12-31");
        assert_eq!(date_from_millis(1_700_000_000_000), "2023-11-14");
        // Sub-second parts floor toward the earlier day.
        assert_eq!(date_from_millis(86_399_999), "1970-01-01");
        assert_eq!(date_from_millis(86_400_000), "1970-01-02");
    }

    #[test]
    fn tile_corners_and_points() {
        let t = Tile { z: 1, x: 1, y: 0 };
        let b = t.bounds();
        assert!((b.min_lon - 0.0).abs() < 1e-9 && (b.max_lon - 180.0).abs() < 1e-9);
        assert!((b.max_lat - 85.0511287798).abs() < 1e-6 && b.min_lat.abs() < 1e-9);
        let (lat, lon) = t.point(4096, 4096.0, 4096.0);
        assert!((lon - 180.0).abs() < 1e-9 && lat.abs() < 1e-9);
    }

    #[test]
    fn tiles_covering_a_box_row_by_row() {
        // Around the London Eye at zoom 15: a 2x2 block.
        let b = Bounds {
            min_lat: 51.500,
            max_lat: 51.506,
            min_lon: -0.125,
            max_lon: -0.115,
        };
        let tiles = tiles_covering(&b, 15);
        assert!(!tiles.is_empty() && tiles.len() <= 6, "{tiles:?}");
        assert!(tiles
            .windows(2)
            .all(|w| (w[0].y, w[0].x) < (w[1].y, w[1].x)));
        for t in &tiles {
            let tb = t.bounds();
            assert!(tb.max_lat > b.min_lat && tb.min_lat < b.max_lat);
            assert!(tb.max_lon > b.min_lon && tb.min_lon < b.max_lon);
        }
        // A tile covers itself.
        let t = Tile {
            z: 14,
            x: 8000,
            y: 5000,
        };
        let inner = expand(&t.bounds(), -0.1);
        assert_eq!(tiles_covering(&inner, 14), vec![t]);
    }

    #[test]
    fn merging_keeps_what_the_detail_lacks() {
        let tile = Photo {
            id: "a".into(),
            lat: 1.0,
            lon: 2.0,
            heading: Some(10.0),
            captured: Some("2020-01-01".into()),
            ..Default::default()
        };
        let detail = Photo {
            id: "a".into(),
            creator: Some("me".into()),
            large_url: Some("https://x/l.jpg".into()),
            ..Default::default()
        };
        let m = tile.merged_with(detail);
        assert_eq!((m.lat, m.lon), (1.0, 2.0));
        assert_eq!(m.heading, Some(10.0));
        assert_eq!(m.creator.as_deref(), Some("me"));
        assert_eq!(m.picture_url(), Some("https://x/l.jpg"));
    }
}
