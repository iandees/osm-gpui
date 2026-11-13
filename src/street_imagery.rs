//! Street-level imagery: photo locations from Mapillary and Fotoshi.
//!
//! The part of the feature that needs no window: the photo model, the two
//! providers' search calls (against an injected [`HttpClient`], so all of it is
//! tested with the `FakeClient`), their JSON parsers, and the shared store the
//! Street Imagery layer draws from. Photos are not OSM data: nothing here
//! touches a data layer, the undo stack, the validator, autosave or search.
//!
//! - Mapillary Graph API v4: `GET https://graph.mapillary.com/images?bbox=..
//!   &fields=..&limit=..` with an access token. A bbox must cover less than
//!   0.01 square degrees.
//! - Fotoshi STAC: `GET https://api.fotoshi.com/v1/stac/search?bbox=..` works
//!   anonymously (20 photos a page, 200 requests a day); a bbox spans at most
//!   2 degrees on each axis.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::coordinates::GeoBounds;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};

/// Mapillary refuses bboxes of 0.01 square degrees or more.
pub const MAPILLARY_MAX_AREA_DEG2: f64 = 0.01;
/// Fotoshi refuses bboxes wider or taller than this many degrees.
pub const FOTOSHI_MAX_SPAN_DEG: f64 = 2.0;
/// Photos asked for per request. Fotoshi serves anonymous callers at most 20.
pub const MAPILLARY_LIMIT: usize = 2000;
pub const FOTOSHI_LIMIT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Mapillary,
    Fotoshi,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Mapillary => "Mapillary",
            Provider::Fotoshi => "Fotoshi",
        }
    }

    /// Whether the provider needs a token from the user's settings.
    pub fn needs_token(self) -> bool {
        matches!(self, Provider::Mapillary)
    }

    /// The largest area one request may cover is limited, so the map must be
    /// zoomed in at least this far: the widest view, in degrees of longitude.
    pub fn max_view_width_deg(self) -> f64 {
        match self {
            // A 0.1 x 0.1 degree box is 0.01 square degrees; stay under it.
            Provider::Mapillary => 0.09,
            Provider::Fotoshi => 1.9,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreetPhoto {
    pub provider: Provider,
    /// The provider's own id (a string for both).
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    /// Camera heading, degrees clockwise from north, when known.
    pub heading: Option<f32>,
    /// Capture date as `YYYY-MM-DD` when known.
    pub captured: Option<String>,
    pub creator: Option<String>,
    pub license: Option<String>,
    /// Small preview, and a larger rendition for the viewer.
    pub thumb_url: Option<String>,
    pub large_url: Option<String>,
}

impl StreetPhoto {
    /// The photo's page on the provider's site.
    pub fn web_url(&self) -> String {
        match self.provider {
            Provider::Mapillary => format!(
                "https://www.mapillary.com/app/?pKey={}&focus=photo",
                self.id
            ),
            Provider::Fotoshi => format!("https://fotoshi.com/photos/{}", self.id),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageryError {
    /// Mapillary needs an access token and none is set.
    NoToken,
    /// The visible area is larger than the provider accepts.
    TooLarge,
    /// 401/403: the token was refused.
    Unauthorized,
    /// 429: rate limit or daily budget used up.
    RateLimited,
    Network(String),
    Parse(String),
}

impl ImageryError {
    fn from_http(e: HttpError) -> Self {
        match e {
            HttpError::Status {
                status: 401 | 403, ..
            } => ImageryError::Unauthorized,
            HttpError::Status { status: 429, .. } => ImageryError::RateLimited,
            other => ImageryError::Network(other.to_string()),
        }
    }
}

impl std::fmt::Display for ImageryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageryError::NoToken => write!(f, "no access token set"),
            ImageryError::TooLarge => write!(f, "area too large"),
            ImageryError::Unauthorized => write!(f, "access token refused"),
            ImageryError::RateLimited => write!(f, "rate limited"),
            ImageryError::Network(m) => write!(f, "network error: {m}"),
            ImageryError::Parse(m) => write!(f, "unreadable response: {m}"),
        }
    }
}

impl std::error::Error for ImageryError {}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Whether `bounds` is small enough for one request to `provider`.
pub fn area_allowed(provider: Provider, b: &GeoBounds) -> bool {
    let (w, h) = (b.width(), b.height());
    match provider {
        Provider::Mapillary => w * h < MAPILLARY_MAX_AREA_DEG2,
        Provider::Fotoshi => w <= FOTOSHI_MAX_SPAN_DEG && h <= FOTOSHI_MAX_SPAN_DEG,
    }
}

pub fn mapillary_url(b: &GeoBounds, token: &str) -> String {
    format!(
        "https://graph.mapillary.com/images?bbox={:.6},{:.6},{:.6},{:.6}&limit={}\
         &fields=id,geometry,computed_geometry,compass_angle,computed_compass_angle,\
         captured_at,creator,thumb_256_url,thumb_1024_url&access_token={}",
        b.min_lon,
        b.min_lat,
        b.max_lon,
        b.max_lat,
        MAPILLARY_LIMIT,
        urlencoding::encode(token)
    )
}

pub fn fotoshi_url(b: &GeoBounds) -> String {
    format!(
        "https://api.fotoshi.com/v1/stac/search?bbox={:.6},{:.6},{:.6},{:.6}&limit={}",
        b.min_lon, b.min_lat, b.max_lon, b.max_lat, FOTOSHI_LIMIT
    )
}

/// The photos in `bounds` from `provider`. `token` is Mapillary's access
/// token. Returns the photos and whether the provider's page limit was hit
/// (so there may be more).
pub fn fetch_with(
    client: &dyn HttpClient,
    provider: Provider,
    bounds: &GeoBounds,
    token: Option<&str>,
) -> Result<(Vec<StreetPhoto>, bool), ImageryError> {
    if !area_allowed(provider, bounds) {
        return Err(ImageryError::TooLarge);
    }
    let (url, limit) = match provider {
        Provider::Mapillary => {
            let token = token
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or(ImageryError::NoToken)?;
            (mapillary_url(bounds, token), MAPILLARY_LIMIT)
        }
        Provider::Fotoshi => (fotoshi_url(bounds), FOTOSHI_LIMIT),
    };
    // One attempt: both providers meter requests, and a pan refetches anyway.
    let resp = fetch_with_retries(client, &HttpRequest::get(url), &RetryPolicy::none())
        .map_err(ImageryError::from_http)?;
    let body = resp
        .into_string()
        .map_err(|e| ImageryError::Network(e.to_string()))?;
    let photos = match provider {
        Provider::Mapillary => parse_mapillary(&body)?,
        Provider::Fotoshi => parse_fotoshi(&body)?,
    };
    let truncated = photos.len() >= limit;
    Ok((photos, truncated))
}

pub fn fetch(
    provider: Provider,
    bounds: &GeoBounds,
    token: Option<&str>,
) -> Result<(Vec<StreetPhoto>, bool), ImageryError> {
    fetch_with(&UreqClient::new(), provider, bounds, token)
}

/// Download an image's bytes (for the viewer).
pub fn fetch_image_with(client: &dyn HttpClient, url: &str) -> Result<Vec<u8>, ImageryError> {
    let resp = fetch_with_retries(client, &HttpRequest::get(url), &RetryPolicy::standard())
        .map_err(ImageryError::from_http)?;
    Ok(resp.body)
}

pub fn fetch_image(url: &str) -> Result<Vec<u8>, ImageryError> {
    fetch_image_with(&UreqClient::new(), url)
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

fn point_of(v: &Value) -> Option<(f64, f64)> {
    let c = v.get("coordinates")?.as_array()?;
    Some((c.get(1)?.as_f64()?, c.first()?.as_f64()?))
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `YYYY-MM-DD` (UTC) from a Unix time in milliseconds.
fn date_from_millis(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Mapillary `{"data":[{id, geometry, ...}]}`. The computed (corrected)
/// position and heading win over the raw ones.
pub fn parse_mapillary(body: &str) -> Result<Vec<StreetPhoto>, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"data\" array".into()))?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let Some(id) = item.get("id").and_then(|i| {
            i.as_str()
                .map(str::to_string)
                .or_else(|| i.as_i64().map(|n| n.to_string()))
        }) else {
            continue;
        };
        let Some((lat, lon)) = item
            .get("computed_geometry")
            .and_then(point_of)
            .or_else(|| item.get("geometry").and_then(point_of))
        else {
            continue;
        };
        let heading = item
            .get("computed_compass_angle")
            .or_else(|| item.get("compass_angle"))
            .and_then(Value::as_f64)
            .map(|h| h as f32);
        out.push(StreetPhoto {
            provider: Provider::Mapillary,
            id,
            lat,
            lon,
            heading,
            captured: item
                .get("captured_at")
                .and_then(Value::as_i64)
                .map(date_from_millis),
            creator: item
                .get("creator")
                .and_then(|c| c.get("username"))
                .and_then(|u| u.as_str().map(str::to_string)),
            license: Some("CC BY-SA 4.0".to_string()),
            thumb_url: str_of(item.get("thumb_256_url")),
            large_url: str_of(item.get("thumb_1024_url")),
        });
    }
    Ok(out)
}

/// Fotoshi STAC `{"features":[{id, geometry, properties, assets}]}`.
pub fn parse_fotoshi(body: &str) -> Result<Vec<StreetPhoto>, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"features\" array".into()))?;
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(id) = f.get("id").and_then(|i| {
            i.as_str()
                .map(str::to_string)
                .or_else(|| i.as_i64().map(|n| n.to_string()))
        }) else {
            continue;
        };
        let Some((lat, lon)) = f.get("geometry").and_then(point_of) else {
            continue;
        };
        let props = f.get("properties").unwrap_or(&Value::Null);
        let asset = |key: &str| {
            str_of(
                f.get("assets")
                    .and_then(|a| a.get(key))
                    .and_then(|a| a.get("href")),
            )
        };
        out.push(StreetPhoto {
            provider: Provider::Fotoshi,
            id,
            lat,
            lon,
            heading: props
                .get("view:azimuth")
                .and_then(Value::as_f64)
                .map(|h| h as f32),
            captured: str_of(props.get("capture_date")),
            creator: str_of(props.get("creator_name")).or_else(|| str_of(props.get("username"))),
            license: str_of(props.get("image_license")),
            thumb_url: asset("thumbnail").or_else(|| asset("image-640")),
            large_url: asset("image-1280")
                .or_else(|| asset("image-640"))
                .or_else(|| asset("thumbnail")),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared store
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct PhotoStoreInner {
    pub photos: Vec<StreetPhoto>,
    /// The photo open in the viewer, drawn highlighted: (provider, id).
    pub selected: Option<(Provider, String)>,
}

/// The photos on the map, shared between the app (which fills it) and the
/// Street Imagery layer (which draws it).
#[derive(Debug, Clone, Default)]
pub struct PhotoStore(Arc<Mutex<PhotoStoreInner>>);

impl PhotoStore {
    pub fn with<R>(&self, f: impl FnOnce(&mut PhotoStoreInner) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn replace(&self, photos: Vec<StreetPhoto>) {
        self.with(|s| s.photos = photos);
    }

    pub fn get(&self, provider: Provider, id: &str) -> Option<StreetPhoto> {
        self.with(|s| {
            s.photos
                .iter()
                .find(|p| p.provider == provider && p.id == id)
                .cloned()
        })
    }

    pub fn len(&self) -> usize {
        self.with(|s| s.photos.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_selected(&self, sel: Option<(Provider, String)>) {
        self.with(|s| s.selected = sel);
    }

    pub fn clear(&self) {
        self.with(|s| {
            s.photos.clear();
            s.selected = None;
        });
    }
}

/// Which providers are on, for the View menu's checkmarks (the menus are built
/// outside the window, which owns the real state).
static MENU_MAPILLARY: AtomicBool = AtomicBool::new(false);
static MENU_FOTOSHI: AtomicBool = AtomicBool::new(false);

pub fn set_menu_shows(mapillary: bool, fotoshi: bool) {
    MENU_MAPILLARY.store(mapillary, Ordering::Relaxed);
    MENU_FOTOSHI.store(fotoshi, Ordering::Relaxed);
}

pub fn menu_shows(provider: Provider) -> bool {
    match provider {
        Provider::Mapillary => MENU_MAPILLARY.load(Ordering::Relaxed),
        Provider::Fotoshi => MENU_FOTOSHI.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    const MAPILLARY: &str = r#"{"data":[
        {"id":"111","geometry":{"type":"Point","coordinates":[-111.0,45.0]},
         "computed_geometry":{"type":"Point","coordinates":[-111.0001,45.0001]},
         "compass_angle":10.0,"computed_compass_angle":12.5,
         "captured_at":1700000000000,"creator":{"username":"alice","id":"1"},
         "thumb_256_url":"https://x/256.jpg","thumb_1024_url":"https://x/1024.jpg"},
        {"id":222,"geometry":{"type":"Point","coordinates":[-111.5,45.5]}},
        {"id":"333"}
    ]}"#;

    const FOTOSHI: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","id":"p1","geometry":{"type":"Point","coordinates":[-111.02,45.67]},
         "properties":{"capture_date":"2024-05-06","view:azimuth":270.0,
           "creator_name":"Bob","username":"bobby","image_license":"CC-BY-4.0"},
         "assets":{"thumbnail":{"href":"https://f/t.jpg"},"image-1280":{"href":"https://f/1280.jpg"}}},
        {"type":"Feature","id":"p2","geometry":{"type":"Point","coordinates":[-111.03,45.68]},
         "properties":{"username":"carol"},"assets":{}}
    ]}"#;

    fn small() -> GeoBounds {
        GeoBounds::new(45.0, 45.05, -111.05, -111.0)
    }

    #[test]
    fn mapillary_prefers_computed_position_and_heading() {
        let photos = parse_mapillary(MAPILLARY).unwrap();
        assert_eq!(photos.len(), 2, "the one without a position is skipped");
        let p = &photos[0];
        assert_eq!((p.lat, p.lon), (45.0001, -111.0001));
        assert_eq!(p.heading, Some(12.5));
        assert_eq!(p.captured.as_deref(), Some("2023-11-14"));
        assert_eq!(p.creator.as_deref(), Some("alice"));
        assert_eq!(p.thumb_url.as_deref(), Some("https://x/256.jpg"));
        assert_eq!(photos[1].id, "222");
        assert_eq!(photos[1].heading, None);
        assert_eq!(photos[1].thumb_url, None);
    }

    #[test]
    fn fotoshi_reads_stac_features() {
        let photos = parse_fotoshi(FOTOSHI).unwrap();
        assert_eq!(photos.len(), 2);
        let p = &photos[0];
        assert_eq!((p.lat, p.lon), (45.67, -111.02));
        assert_eq!(p.heading, Some(270.0));
        assert_eq!(p.captured.as_deref(), Some("2024-05-06"));
        assert_eq!(p.creator.as_deref(), Some("Bob"));
        assert_eq!(p.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(p.large_url.as_deref(), Some("https://f/1280.jpg"));
        assert_eq!(photos[1].creator.as_deref(), Some("carol"));
        assert_eq!(photos[1].thumb_url, None);
    }

    #[test]
    fn bad_json_is_a_parse_error() {
        assert!(matches!(parse_mapillary("{}"), Err(ImageryError::Parse(_))));
        assert!(matches!(parse_fotoshi("nope"), Err(ImageryError::Parse(_))));
    }

    #[test]
    fn dates_from_millis() {
        assert_eq!(date_from_millis(0), "1970-01-01");
        assert_eq!(date_from_millis(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn area_limits() {
        // 0.05 x 0.05 = 0.0025 deg2: fine for Mapillary.
        assert!(area_allowed(Provider::Mapillary, &small()));
        let big = GeoBounds::new(45.0, 45.2, -111.2, -111.0);
        assert!(!area_allowed(Provider::Mapillary, &big));
        assert!(area_allowed(Provider::Fotoshi, &big));
        let huge = GeoBounds::new(40.0, 43.0, -111.2, -111.0);
        assert!(!area_allowed(Provider::Fotoshi, &huge));
    }

    #[test]
    fn mapillary_needs_a_token_and_never_calls_without_one() {
        let client = FakeClient::new(vec![]);
        assert_eq!(
            fetch_with(&client, Provider::Mapillary, &small(), None).unwrap_err(),
            ImageryError::NoToken
        );
        assert_eq!(
            fetch_with(&client, Provider::Mapillary, &small(), Some("  ")).unwrap_err(),
            ImageryError::NoToken
        );
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn fetch_maps_statuses() {
        let c = FakeClient::new(vec![status_err(401, "")]);
        assert_eq!(
            fetch_with(&c, Provider::Mapillary, &small(), Some("t")).unwrap_err(),
            ImageryError::Unauthorized
        );
        let c = FakeClient::new(vec![status_err(429, "")]);
        assert_eq!(
            fetch_with(&c, Provider::Fotoshi, &small(), None).unwrap_err(),
            ImageryError::RateLimited
        );
    }

    #[test]
    fn fetch_returns_photos_and_flags_a_full_page() {
        let c = FakeClient::new(vec![ok(200, FOTOSHI)]);
        let (photos, truncated) = fetch_with(&c, Provider::Fotoshi, &small(), None).unwrap();
        assert_eq!(photos.len(), 2);
        assert!(!truncated);
    }

    #[test]
    fn urls_carry_bbox_and_token() {
        let u = mapillary_url(&small(), "MLY|a b");
        assert!(u.contains("bbox=-111.050000,45.000000,-111.000000,45.050000"));
        assert!(u.contains("access_token=MLY%7Ca%20b"));
        assert!(fotoshi_url(&small()).starts_with("https://api.fotoshi.com/v1/stac/search?bbox="));
    }

    #[test]
    fn store_roundtrip() {
        let store = PhotoStore::default();
        store.replace(parse_fotoshi(FOTOSHI).unwrap());
        assert_eq!(store.len(), 2);
        assert!(store.get(Provider::Fotoshi, "p1").is_some());
        assert!(store.get(Provider::Mapillary, "p1").is_none());
        store.clear();
        assert!(store.is_empty());
    }
}
