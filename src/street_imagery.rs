//! Street-level imagery: photo locations from Mapillary, Fotoshi and
//! Panoramax.
//!
//! The part of the feature that needs no window: the photo model, the
//! providers' JSON parsers, and the shared store the Street Imagery layer
//! draws from. Where the photos come from, by map tile, is in
//! `street_imagery_tiles`. Photos are not OSM data: nothing here touches a
//! data layer, the undo stack, the validator, autosave or search.
//!
//! - Mapillary and Panoramax publish vector tiles of their photo points.
//! - Fotoshi STAC: `GET https://api.fotoshi.com/v1/stac/search?bbox=..` works
//!   anonymously (20 photos a page, 200 requests a day, a bbox of at most 2
//!   degrees); an API key raises the page to 100.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::coordinates::GeoBounds;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpFailure, HttpRequest, RetryPolicy,
};
use crate::tiles::TileCoord;

/// Fotoshi photos per search: anonymous callers get at most 20 a page.
pub const FOTOSHI_ANON_LIMIT: usize = 20;
pub const FOTOSHI_KEYED_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Mapillary,
    Fotoshi,
    Panoramax,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Mapillary => "Mapillary",
            Provider::Fotoshi => "Fotoshi",
            Provider::Panoramax => "Panoramax",
        }
    }

    /// Position in [`Provider::ALL`].
    pub fn index(self) -> usize {
        match self {
            Provider::Mapillary => 0,
            Provider::Fotoshi => 1,
            Provider::Panoramax => 2,
        }
    }

    pub const ALL: [Provider; 3] = [Provider::Mapillary, Provider::Fotoshi, Provider::Panoramax];

    /// Whether the provider needs a token from the user's settings.
    pub fn needs_token(self) -> bool {
        matches!(self, Provider::Mapillary)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreetPhoto {
    pub provider: Provider,
    /// The provider's own id, as a string (Mapillary serves numeric ids too).
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
            Provider::Panoramax => {
                format!("https://api.panoramax.xyz/#focus=pic&pic={}", self.id)
            }
        }
    }
}

impl StreetPhoto {
    /// This photo (from a tile) with what `detail` knows: its values win,
    /// except that a value it lacks keeps this photo's.
    pub fn merged_with(&self, detail: StreetPhoto) -> StreetPhoto {
        StreetPhoto {
            provider: self.provider,
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageryError {
    /// Mapillary needs an access token and none is set.
    NoToken,
    /// The map is zoomed out too far to show photos.
    TooLarge,
    /// 401/403: the token was refused.
    Unauthorized,
    /// 429: rate limit or daily budget used up.
    RateLimited,
    Network(String),
    Parse(String),
}

impl From<HttpError> for ImageryError {
    fn from(e: HttpError) -> Self {
        let text = e.to_string();
        match HttpFailure::from(e) {
            HttpFailure::Status {
                status: 401 | 403, ..
            } => ImageryError::Unauthorized,
            HttpFailure::Status { status: 429, .. } => ImageryError::RateLimited,
            _ => ImageryError::Network(text),
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

/// A Fotoshi STAC search of `b`, `limit` photos a page.
pub fn fotoshi_url(b: &GeoBounds, limit: usize) -> String {
    format!(
        "https://api.fotoshi.com/v1/stac/search?bbox={:.6},{:.6},{:.6},{:.6}&limit={}",
        b.min_lon, b.min_lat, b.max_lon, b.max_lat, limit
    )
}

/// Download an image's bytes (for the viewer).
pub fn fetch_image_with(client: &dyn HttpClient, url: &str) -> Result<Vec<u8>, ImageryError> {
    let resp = fetch_with_retries(client, &HttpRequest::get(url), &RetryPolicy::standard())?;
    Ok(resp.body)
}

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
pub(crate) fn date_from_millis(ms: i64) -> String {
    crate::civil_date::date_from_unix_millis(ms)
}

/// One Mapillary image object (`id`, `geometry`, ...). The computed
/// (corrected) position and heading win over the raw ones.
pub fn parse_mapillary_image(item: &Value) -> Option<StreetPhoto> {
    let id = item.get("id").and_then(|i| {
        i.as_str()
            .map(str::to_string)
            .or_else(|| i.as_i64().map(|n| n.to_string()))
    })?;
    let (lat, lon) = item
        .get("computed_geometry")
        .and_then(point_of)
        .or_else(|| item.get("geometry").and_then(point_of))?;
    Some(StreetPhoto {
        provider: Provider::Mapillary,
        id,
        lat,
        lon,
        heading: item
            .get("computed_compass_angle")
            .or_else(|| item.get("compass_angle"))
            .and_then(Value::as_f64)
            .map(|h| h as f32),
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
    })
}

/// Mapillary `{"data":[{id, geometry, ...}]}` (the images endpoint).
pub fn parse_mapillary(body: &str) -> Result<Vec<StreetPhoto>, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"data\" array".into()))?;
    Ok(data.iter().filter_map(parse_mapillary_image).collect())
}

/// The first `[lon, lat]` position in a GeoJSON geometry, whatever its type.
fn first_position(geometry: &Value) -> Option<(f64, f64)> {
    fn dig(v: &Value) -> Option<(f64, f64)> {
        let arr = v.as_array()?;
        if let (Some(lon), Some(lat)) = (arr.first()?.as_f64(), arr.get(1).and_then(Value::as_f64))
        {
            return Some((lat, lon));
        }
        arr.iter().find_map(dig)
    }
    dig(geometry.get("coordinates")?)
}

/// One Fotoshi photo. Two shapes carry it: a STAC item (`id`, `geometry`,
/// `properties`, `assets`) and a photo embedded in a feature of the data API
/// (`photo_id`, `captured_at`, `username`, `assets`, with the position, if
/// any, on the photo or else on the feature: `fallback`).
fn fotoshi_photo(item: &Value, fallback: Option<(f64, f64)>) -> Option<StreetPhoto> {
    let id = str_of(item.get("photo_id")).or_else(|| {
        item.get("id").and_then(|i| {
            i.as_str()
                .map(str::to_string)
                .or_else(|| i.as_i64().map(|n| n.to_string()))
        })
    })?;
    let (lat, lon) = item.get("geometry").and_then(point_of).or(fallback)?;
    // STAC items nest these in `properties`; embedded photos do not.
    let props = item.get("properties").unwrap_or(item);
    let prop = |key: &str| props.get(key).or_else(|| item.get(key));
    let asset = |key: &str| {
        str_of(
            item.get("assets")
                .and_then(|a| a.get(key))
                .and_then(|a| a.get("href")),
        )
    };
    let any_asset_license = || {
        item.get("assets")
            .and_then(Value::as_object)
            .and_then(|m| m.values().find_map(|a| str_of(a.get("license"))))
    };
    Some(StreetPhoto {
        provider: Provider::Fotoshi,
        id,
        lat,
        lon,
        heading: prop("view:azimuth")
            .or_else(|| prop("heading"))
            .and_then(Value::as_f64)
            .map(|h| h as f32),
        captured: str_of(prop("capture_date")).or_else(|| str_of(prop("captured_at"))),
        creator: str_of(prop("creator_name")).or_else(|| str_of(prop("username"))),
        license: str_of(prop("image_license")).or_else(any_asset_license),
        thumb_url: asset("thumbnail").or_else(|| asset("image-640")),
        large_url: asset("image-1280")
            .or_else(|| asset("image-640"))
            .or_else(|| asset("thumbnail")),
    })
}

/// Fotoshi STAC `{"features":[{id, geometry, properties, assets}]}`.
pub fn parse_fotoshi(body: &str) -> Result<Vec<StreetPhoto>, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"features\" array".into()))?;
    Ok(features
        .iter()
        .filter_map(|f| fotoshi_photo(f, None))
        .collect())
}

/// What Fotoshi has of one OSM feature.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectPhotos {
    /// All the public photos of the feature (more than `photos` when the
    /// request's limit cut the list).
    pub count: usize,
    pub photos: Vec<StreetPhoto>,
}

/// The data API's answer to an `osm_id` lookup: a FeatureCollection whose
/// features carry `coverage.photo_count` and a `photos` array. An empty
/// collection means no public coverage.
pub fn parse_fotoshi_object(body: &str) -> Result<ObjectPhotos, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"features\" array".into()))?;
    let mut out = ObjectPhotos::default();
    for f in features {
        let fallback = f.get("geometry").and_then(first_position);
        let before = out.photos.len();
        if let Some(photos) = f.get("photos").and_then(Value::as_array) {
            out.photos
                .extend(photos.iter().filter_map(|p| fotoshi_photo(p, fallback)));
        }
        let listed = out.photos.len() - before;
        out.count += f
            .get("coverage")
            .and_then(|c| c.get("photo_count"))
            .and_then(Value::as_u64)
            .map_or(listed, |n| n as usize);
    }
    Ok(out)
}

/// `osm_id` for an OSM element: `n123`, `w456` or `r789`.
pub fn fotoshi_osm_id(kind: char, id: i64) -> String {
    format!("{kind}{id}")
}

pub fn fotoshi_object_url(osm_id: &str) -> String {
    format!(
        "https://api.fotoshi.com/v1/data/features?osm_id={}&photo_limit=20&photo_size=thumbnail,1280",
        urlencoding::encode(osm_id)
    )
}

/// Ask Fotoshi (with `key`, which this lookup requires) for the photos of an
/// OSM feature. This tells Fotoshi what the user is looking at; the caller
/// only does it when the user has turned that on.
pub fn fetch_object_photos_with(
    client: &dyn HttpClient,
    osm_id: &str,
    key: &str,
) -> Result<ObjectPhotos, ImageryError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(ImageryError::Unauthorized);
    }
    let req = HttpRequest::get(fotoshi_object_url(osm_id)).bearer(key);
    // One attempt: Fotoshi meters requests.
    let resp = fetch_with_retries(client, &req, &RetryPolicy::none())?;
    let body = resp
        .into_string()
        .map_err(|e| ImageryError::Network(e.to_string()))?;
    parse_fotoshi_object(&body)
}

/// Panoramax STAC `{"features":[{id, geometry, properties, assets, providers}]}`
/// (the federated `api.panoramax.xyz/api/search`).
pub fn parse_panoramax(body: &str) -> Result<Vec<StreetPhoto>, ImageryError> {
    let v: Value = serde_json::from_str(body).map_err(|e| ImageryError::Parse(e.to_string()))?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| ImageryError::Parse("no \"features\" array".into()))?;
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(id) = str_of(f.get("id")) else {
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
        // The producer is named both in `providers` and in a property.
        let producer = f
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|ps| {
                ps.iter().find(|p| {
                    p.get("roles")
                        .and_then(Value::as_array)
                        .is_some_and(|r| r.iter().any(|r| r.as_str() == Some("producer")))
                })
            })
            .and_then(|p| str_of(p.get("name")))
            .or_else(|| str_of(props.get("geovisio:producer")));
        out.push(StreetPhoto {
            provider: Provider::Panoramax,
            id,
            lat,
            lon,
            heading: props
                .get("view:azimuth")
                .and_then(Value::as_f64)
                .map(|h| h as f32),
            // An RFC 3339 time; the date is its first ten characters.
            captured: str_of(props.get("datetime")).and_then(|d| d.get(..10).map(str::to_string)),
            creator: producer,
            license: str_of(props.get("license")),
            thumb_url: asset("thumb").or_else(|| str_of(props.get("geovisio:thumbnail"))),
            large_url: asset("sd")
                .or_else(|| asset("hd"))
                .or_else(|| asset("thumb")),
        });
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct PhotoStoreInner {
    /// Every photo of every loaded tile, for drawing and hit-testing.
    pub photos: Vec<StreetPhoto>,
    /// The photos of each loaded tile.
    pub tiles: HashMap<TileCoord, Vec<StreetPhoto>>,
    /// The photo open in the viewer, drawn highlighted: (provider, id).
    pub selected: Option<(Provider, String)>,
}

impl PhotoStoreInner {
    fn rebuild(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.photos = self
            .tiles
            .values()
            .flatten()
            .filter(|p| seen.insert(p.id.clone()))
            .cloned()
            .collect();
        // A stable order, so drawing does not flicker between rebuilds.
        self.photos.sort_by(|a, b| a.id.cmp(&b.id));
    }
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

    /// Set the photos of tile `t`. A photo on a tile's edge can come from two
    /// tiles; it is kept once.
    pub fn insert_tile(&self, t: TileCoord, photos: Vec<StreetPhoto>) {
        self.with(|s| {
            s.tiles.insert(t, photos);
            s.rebuild();
        });
    }

    pub fn has_tile(&self, t: TileCoord) -> bool {
        self.with(|s| s.tiles.contains_key(&t))
    }

    pub fn tile_count(&self) -> usize {
        self.with(|s| s.tiles.len())
    }

    /// Drop the tiles for which `keep` is false.
    pub fn retain_tiles(&self, keep: impl Fn(&TileCoord) -> bool) {
        self.with(|s| {
            s.tiles.retain(|t, _| keep(t));
            s.rebuild();
        });
    }

    /// Replace a photo (after its details arrived) wherever it is stored.
    pub fn update_photo(&self, photo: &StreetPhoto) {
        self.with(|s| {
            for list in s.tiles.values_mut() {
                for p in list.iter_mut().filter(|p| p.id == photo.id) {
                    *p = photo.clone();
                }
            }
            s.rebuild();
        });
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
            s.tiles.clear();
            s.selected = None;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const PANORAMAX: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","id":"aaa","geometry":{"type":"Point","coordinates":[-93.2234,44.926]},
         "properties":{"datetime":"2025-09-16T18:02:01.262000+00:00","view:azimuth":334,
           "license":"CC-BY-SA-4.0","geovisio:producer":"fallback",
           "exif":{"big":"ignored"}},
         "providers":[{"name":"someone","roles":["producer"]}],
         "assets":{"hd":{"href":"https://p/hd.jpg"},"sd":{"href":"https://p/sd.jpg"},
                   "thumb":{"href":"https://p/thumb.jpg"}}},
        {"type":"Feature","id":"bbb","geometry":{"type":"Point","coordinates":[-93.2,44.9]},
         "properties":{"geovisio:producer":"fallback","geovisio:thumbnail":"https://p/t2.jpg"},
         "assets":{}}
    ]}"#;

    #[test]
    fn panoramax_reads_stac_features() {
        let photos = parse_panoramax(PANORAMAX).unwrap();
        assert_eq!(photos.len(), 2);
        let p = &photos[0];
        assert_eq!((p.lat, p.lon), (44.926, -93.2234));
        assert_eq!(p.heading, Some(334.0));
        assert_eq!(p.captured.as_deref(), Some("2025-09-16"));
        assert_eq!(p.creator.as_deref(), Some("someone"));
        assert_eq!(p.license.as_deref(), Some("CC-BY-SA-4.0"));
        assert_eq!(p.thumb_url.as_deref(), Some("https://p/thumb.jpg"));
        assert_eq!(p.large_url.as_deref(), Some("https://p/sd.jpg"));
        // Without `providers` or assets, the properties fill in.
        assert_eq!(photos[1].creator.as_deref(), Some("fallback"));
        assert_eq!(photos[1].thumb_url.as_deref(), Some("https://p/t2.jpg"));
        assert_eq!(photos[1].large_url, None);
        assert_eq!(photos[1].heading, None);
        assert!(p.web_url().ends_with("pic=aaa"));
    }

    const OBJECT: &str = r#"{"type":"FeatureCollection","numberReturned":1,"features":[
        {"type":"Feature","osm_id":"w456","fid":"f1",
         "geometry":{"type":"LineString","coordinates":[[-93.2,44.9],[-93.21,44.91]]},
         "coverage":{"photo_count":37},
         "photos":[
           {"photo_id":"ph1","username":"dana","captured_at":"2026-05-27",
            "assets":{"thumbnail":{"href":"https://f/t1.jpg","license":"CC-BY-4.0"},
                      "image-1280":{"href":"https://f/1280-1.jpg"}}},
           {"photo_id":"ph2","creator_name":"Eli","username":"eli",
            "geometry":{"type":"Point","coordinates":[-93.205,44.905]},
            "assets":{"thumbnail":{"href":"https://f/t2.jpg"}}}
         ]}]}"#;

    #[test]
    fn a_fotoshi_object_lookup_reads_embedded_photos() {
        let o = parse_fotoshi_object(OBJECT).unwrap();
        assert_eq!(o.count, 37, "the total, not the page");
        assert_eq!(o.photos.len(), 2);
        let p = &o.photos[0];
        assert_eq!(p.id, "ph1");
        // No position of its own: the feature's first vertex.
        assert_eq!((p.lat, p.lon), (44.9, -93.2));
        assert_eq!(p.creator.as_deref(), Some("dana"));
        assert_eq!(p.captured.as_deref(), Some("2026-05-27"));
        assert_eq!(p.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(p.large_url.as_deref(), Some("https://f/1280-1.jpg"));
        let q = &o.photos[1];
        assert_eq!((q.lat, q.lon), (44.905, -93.205));
        assert_eq!(q.creator.as_deref(), Some("Eli"));
        assert_eq!(q.large_url.as_deref(), Some("https://f/t2.jpg"));
        // No coverage: an empty collection.
        let none = parse_fotoshi_object(r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        assert_eq!(none, ObjectPhotos::default());
    }

    #[test]
    fn the_object_lookup_needs_a_key_and_sends_the_osm_id() {
        use crate::http::fake::{ok, FakeClient};
        assert_eq!(fotoshi_osm_id('w', 456), "w456");
        assert!(fotoshi_object_url("w456").contains("osm_id=w456&photo_limit=20"));
        let c = FakeClient::new(vec![]);
        assert_eq!(
            fetch_object_photos_with(&c, "w456", " ").unwrap_err(),
            ImageryError::Unauthorized
        );
        assert_eq!(c.request_count(), 0, "no key, no request");
        let c = FakeClient::new(vec![ok(200, OBJECT)]);
        assert_eq!(fetch_object_photos_with(&c, "w456", "k").unwrap().count, 37);
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
        assert_eq!(date_from_millis(-1), "1969-12-31");
        assert_eq!(date_from_millis(-2_203_891_201_000), "1900-02-28");
        assert_eq!(date_from_millis(-2_203_891_200_000), "1900-03-01");
        // Sub-second parts floor toward the earlier day.
        assert_eq!(date_from_millis(86_399_999), "1970-01-01");
        assert_eq!(date_from_millis(86_400_000), "1970-01-02");
    }

    #[test]
    fn the_fotoshi_search_url_carries_the_box_and_limit() {
        let u = fotoshi_url(&small(), 20);
        assert!(u.starts_with("https://api.fotoshi.com/v1/stac/search?bbox=-111.050000,45.000000,"));
        assert!(u.ends_with("&limit=20"));
    }

    #[test]
    fn store_keeps_tiles_and_dedupes_edge_photos() {
        let store = PhotoStore::default();
        let all = parse_fotoshi(FOTOSHI).unwrap();
        let (a, b) = (
            TileCoord { z: 15, x: 1, y: 1 },
            TileCoord { z: 15, x: 2, y: 1 },
        );
        store.insert_tile(a, all.clone());
        // The second tile repeats p1, which sits on the edge.
        store.insert_tile(b, vec![all[0].clone()]);
        assert_eq!(store.len(), 2);
        assert!(store.has_tile(a) && store.has_tile(b));
        assert!(store.get(Provider::Fotoshi, "p1").is_some());
        assert!(store.get(Provider::Mapillary, "p1").is_none());
        store.retain_tiles(|t| *t == b);
        assert_eq!(store.len(), 1);
        assert_eq!(store.tile_count(), 1);
        let mut p = all[0].clone();
        p.creator = Some("renamed".into());
        store.update_photo(&p);
        assert_eq!(
            store
                .get(Provider::Fotoshi, "p1")
                .unwrap()
                .creator
                .as_deref(),
            Some("renamed")
        );
        store.clear();
        assert!(store.is_empty() && store.tile_count() == 0);
    }
}
