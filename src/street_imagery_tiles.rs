//! Street imagery by map tile.
//!
//! Mapillary and Panoramax publish their photo locations as vector tiles, so
//! the map asks for the tiles under the view (cached per tile) instead of
//! searching by bounding box. Fotoshi has no photo tiles, so its tiles are
//! virtual: the same grid, each one a STAC search of the tile's box.
//!
//! - Mapillary `https://tiles.mapillary.com/maps/vtp/mly1_public/2/{z}/{x}/{y}`
//!   (needs the access token): layer `image` at zoom 14, one point per photo.
//! - Panoramax `https://api.panoramax.xyz/api/map/2/{z}/{x}/{y}.mvt` (no key):
//!   layer `pictures` from zoom 15, one point per photo.
//!
//! A tile only says where a photo is and a little about it; the image URLs
//! and the author come from a detail request when the photo is opened (see
//! [`fetch_detail_with`]).

use std::collections::HashMap;

use mvt_reader::feature::Value as MvtValue;
use mvt_reader::Reader;

use crate::coordinates::GeoBounds;
use crate::http::{fetch_with_retries, HttpClient, HttpRequest, RetryPolicy};
use crate::street_imagery::{
    date_from_millis, fotoshi_url, parse_fotoshi, parse_mapillary_image, parse_panoramax,
    ImageryError, Provider, StreetPhoto, FOTOSHI_ANON_LIMIT, FOTOSHI_KEYED_LIMIT,
};

/// More tiles than this under the view means the map is zoomed out too far
/// to show photos.
pub const MAX_TILES_PER_VIEW: usize = 40;

/// A tile of the web-mercator grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// The zoom of the tiles each provider is asked for: where its photo points
/// are published (Mapillary 14, Panoramax 15), and for Fotoshi's virtual
/// tiles about 1.2 km at the equator.
pub fn tile_zoom(provider: Provider) -> u8 {
    match provider {
        Provider::Mapillary => 14,
        Provider::Fotoshi | Provider::Panoramax => 15,
    }
}

fn lon_to_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * f64::from(1u32 << z)
}

fn lat_to_y(lat: f64, z: u8) -> f64 {
    let lat = lat.clamp(-85.05112878, 85.05112878).to_radians();
    (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * f64::from(1u32 << z)
}

fn x_to_lon(x: f64, z: u8) -> f64 {
    x / f64::from(1u32 << z) * 360.0 - 180.0
}

fn y_to_lat(y: f64, z: u8) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * y / f64::from(1u32 << z));
    n.sinh().atan().to_degrees()
}

/// The tiles at zoom `z` that touch `b`, row by row.
pub fn tiles_covering(b: &GeoBounds, z: u8) -> Vec<TileId> {
    let max = (1u32 << z) - 1;
    let clamp = |v: f64| (v.floor().max(0.0) as u32).min(max);
    let (x0, x1) = (clamp(lon_to_x(b.min_lon, z)), clamp(lon_to_x(b.max_lon, z)));
    // Tile y grows southwards.
    let (y0, y1) = (clamp(lat_to_y(b.max_lat, z)), clamp(lat_to_y(b.min_lat, z)));
    let mut out = Vec::with_capacity(((x1 - x0 + 1) * (y1 - y0 + 1)) as usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(TileId { z, x, y });
        }
    }
    out
}

pub fn tile_bounds(t: TileId) -> GeoBounds {
    GeoBounds::new(
        y_to_lat(f64::from(t.y) + 1.0, t.z),
        y_to_lat(f64::from(t.y), t.z),
        x_to_lon(f64::from(t.x), t.z),
        x_to_lon(f64::from(t.x) + 1.0, t.z),
    )
}

/// Where the point `(px, py)` of a tile of `extent` units lies.
fn tile_point_to_geo(t: TileId, extent: u32, px: f32, py: f32) -> (f64, f64) {
    let e = f64::from(extent.max(1));
    (
        y_to_lat(f64::from(t.y) + f64::from(py) / e, t.z),
        x_to_lon(f64::from(t.x) + f64::from(px) / e, t.z),
    )
}

pub fn mapillary_tile_url(t: TileId, token: &str) -> String {
    format!(
        "https://tiles.mapillary.com/maps/vtp/mly1_public/2/{}/{}/{}?access_token={}",
        t.z,
        t.x,
        t.y,
        urlencoding::encode(token)
    )
}

pub fn panoramax_tile_url(t: TileId) -> String {
    format!(
        "https://api.panoramax.xyz/api/map/2/{}/{}/{}.mvt",
        t.z, t.x, t.y
    )
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn number(v: &MvtValue) -> Option<f64> {
    match v {
        MvtValue::Float(f) => Some(f64::from(*f)),
        MvtValue::Double(d) => Some(*d),
        MvtValue::Int(i) | MvtValue::SInt(i) => Some(*i as f64),
        MvtValue::UInt(u) => Some(*u as f64),
        _ => None,
    }
}

fn text(v: &MvtValue) -> Option<String> {
    match v {
        MvtValue::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        MvtValue::Int(i) | MvtValue::SInt(i) => Some(i.to_string()),
        MvtValue::UInt(u) => Some(u.to_string()),
        _ => None,
    }
}

/// A point feature of a tile: latitude, longitude and properties.
type TilePoint = (f64, f64, HashMap<String, MvtValue>);

/// The point features of `layer` with their position and properties.
fn points_of(bytes: &[u8], layer: &str, t: TileId) -> Result<Vec<TilePoint>, ImageryError> {
    // An empty body is a tile with nothing in it.
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let reader =
        Reader::new(bytes.to_vec()).map_err(|e| ImageryError::Parse(format!("tile: {e}")))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| ImageryError::Parse(format!("tile: {e}")))?;
    let Some(meta) = layers.iter().find(|l| l.name == layer) else {
        return Ok(Vec::new());
    };
    let features = reader
        .get_features(meta.layer_index)
        .map_err(|e| ImageryError::Parse(format!("tile: {e}")))?;
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        // The reader returns even a single point as a MultiPoint.
        let points: Vec<geo_types::Point<f32>> = match f.geometry {
            geo_types::Geometry::Point(p) => vec![p],
            geo_types::Geometry::MultiPoint(mp) => mp.0,
            _ => continue,
        };
        let props = f.properties.unwrap_or_default();
        for p in points {
            let (lat, lon) = tile_point_to_geo(t, meta.extent, p.x(), p.y());
            out.push((lat, lon, props.clone()));
        }
    }
    Ok(out)
}

/// Mapillary's `image` layer: `id`, `captured_at` (ms), `compass_angle`, ...
pub fn decode_mapillary_tile(bytes: &[u8], t: TileId) -> Result<Vec<StreetPhoto>, ImageryError> {
    let mut out = Vec::new();
    for (lat, lon, props) in points_of(bytes, "image", t)? {
        let Some(id) = props.get("id").and_then(text) else {
            continue;
        };
        out.push(StreetPhoto {
            provider: Provider::Mapillary,
            id,
            lat,
            lon,
            heading: props
                .get("compass_angle")
                .and_then(number)
                .map(|h| h as f32),
            captured: props
                .get("captured_at")
                .and_then(number)
                .map(|ms| date_from_millis(ms as i64)),
            creator: None,
            license: Some("CC BY-SA 4.0".to_string()),
            thumb_url: None,
            large_url: None,
        });
    }
    Ok(out)
}

/// Panoramax's `pictures` layer: `id`, `ts` (RFC 3339), `heading`, ...
pub fn decode_panoramax_tile(bytes: &[u8], t: TileId) -> Result<Vec<StreetPhoto>, ImageryError> {
    let mut out = Vec::new();
    for (lat, lon, props) in points_of(bytes, "pictures", t)? {
        let Some(id) = props.get("id").and_then(text) else {
            continue;
        };
        out.push(StreetPhoto {
            provider: Provider::Panoramax,
            id,
            lat,
            lon,
            heading: props.get("heading").and_then(number).map(|h| h as f32),
            captured: props
                .get("ts")
                .and_then(text)
                .and_then(|d| d.get(..10).map(str::to_string)),
            creator: None,
            license: None,
            thumb_url: None,
            large_url: None,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// The photos of tile `t` from `provider`, and whether the tile may hold more
/// than were returned (only Fotoshi's searches are paged). `credential` is
/// Mapillary's access token, or the optional Fotoshi API key (which raises
/// its page size from 20 to 100).
pub fn fetch_tile_with(
    client: &dyn HttpClient,
    provider: Provider,
    t: TileId,
    credential: Option<&str>,
) -> Result<(Vec<StreetPhoto>, bool), ImageryError> {
    let credential = credential.map(str::trim).filter(|c| !c.is_empty());
    match provider {
        Provider::Mapillary => {
            let token = credential.ok_or(ImageryError::NoToken)?;
            let bytes = get_bytes(client, mapillary_tile_url(t, token), true)?;
            Ok((decode_mapillary_tile(&bytes, t)?, false))
        }
        Provider::Panoramax => {
            let bytes = get_bytes(client, panoramax_tile_url(t), true)?;
            Ok((decode_panoramax_tile(&bytes, t)?, false))
        }
        Provider::Fotoshi => {
            let limit = if credential.is_some() {
                FOTOSHI_KEYED_LIMIT
            } else {
                FOTOSHI_ANON_LIMIT
            };
            let url = fotoshi_url(&tile_bounds(t), limit);
            // Fotoshi meters requests: one attempt, never retried.
            let mut req = HttpRequest::get(url);
            if let Some(key) = credential {
                req = req.bearer(key);
            }
            let resp = fetch_with_retries(client, &req, &RetryPolicy::none())
                .map_err(ImageryError::from_http)?;
            let body = resp
                .into_string()
                .map_err(|e| ImageryError::Network(e.to_string()))?;
            let photos = parse_fotoshi(&body)?;
            let full = photos.len() >= limit;
            Ok((photos, full))
        }
    }
}

fn get_bytes(client: &dyn HttpClient, url: String, retry: bool) -> Result<Vec<u8>, ImageryError> {
    let policy = if retry {
        RetryPolicy::standard()
    } else {
        RetryPolicy::none()
    };
    fetch_with_retries(client, &HttpRequest::get(url), &policy)
        .map(|r| r.body)
        .map_err(ImageryError::from_http)
}

/// What a tile leaves out, filled in when a photo is opened: image URLs, the
/// author and the license. Fotoshi's search already has all of it.
pub fn fetch_detail_with(
    client: &dyn HttpClient,
    photo: &StreetPhoto,
    credential: Option<&str>,
) -> Result<StreetPhoto, ImageryError> {
    let detail = match photo.provider {
        Provider::Fotoshi => return Ok(photo.clone()),
        Provider::Mapillary => {
            let token = credential
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or(ImageryError::NoToken)?;
            let url = format!(
                "https://graph.mapillary.com/{}?fields=id,geometry,computed_geometry,\
                 compass_angle,computed_compass_angle,captured_at,creator,\
                 thumb_256_url,thumb_1024_url&access_token={}",
                urlencoding::encode(&photo.id),
                urlencoding::encode(token)
            );
            let body = String::from_utf8_lossy(&get_bytes(client, url, true)?).into_owned();
            let v: serde_json::Value =
                serde_json::from_str(&body).map_err(|e| ImageryError::Parse(e.to_string()))?;
            parse_mapillary_image(&v)
        }
        Provider::Panoramax => {
            let url = format!(
                "https://api.panoramax.xyz/api/search?ids={}",
                urlencoding::encode(&photo.id)
            );
            let body = String::from_utf8_lossy(&get_bytes(client, url, true)?).into_owned();
            parse_panoramax(&body)?.into_iter().next()
        }
    };
    detail
        .map(|d| photo.merged_with(d))
        .ok_or_else(|| ImageryError::Parse("photo not found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    /// A real Panoramax tile (zoom 15) over south Minneapolis.
    const PANORAMAX_TILE: &[u8] = include_bytes!("../tests/fixtures/panoramax_15_7898_11796.mvt");
    const PANORAMAX_TILE_ID: TileId = TileId {
        z: 15,
        x: 7898,
        y: 11796,
    };

    // --- a minimal MVT writer, for tiles we cannot download -----------------

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn field(num: u32, wire: u8, out: &mut Vec<u8>) {
        varint(u64::from(num << 3 | u32::from(wire)), out);
    }

    fn bytes_field(num: u32, data: &[u8], out: &mut Vec<u8>) {
        field(num, 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    enum Prop<'a> {
        Uint(u64),
        Double(f64),
        Str(&'a str),
    }

    /// A test point: `(x, y, [(key, value)])`.
    type TestPoint<'a> = (i32, i32, Vec<(&'a str, Prop<'a>)>);

    /// One layer of point features: `(x, y, [(key, value)])`.
    fn encode_tile(layer: &str, extent: u32, points: &[TestPoint]) -> Vec<u8> {
        let zigzag = |v: i32| ((v << 1) ^ (v >> 31)) as u64;
        let mut keys: Vec<String> = Vec::new();
        let mut values: Vec<Vec<u8>> = Vec::new();
        let mut features = Vec::new();
        for (x, y, props) in points {
            let mut f = Vec::new();
            let mut tags = Vec::new();
            for (k, v) in props {
                let ki = keys.iter().position(|e| e == k).unwrap_or_else(|| {
                    keys.push(k.to_string());
                    keys.len() - 1
                });
                let mut val = Vec::new();
                match v {
                    Prop::Uint(u) => {
                        field(5, 0, &mut val);
                        varint(*u, &mut val);
                    }
                    Prop::Double(d) => {
                        field(3, 1, &mut val);
                        val.extend_from_slice(&d.to_le_bytes());
                    }
                    Prop::Str(s) => bytes_field(1, s.as_bytes(), &mut val),
                }
                values.push(val);
                varint(ki as u64, &mut tags);
                varint((values.len() - 1) as u64, &mut tags);
            }
            bytes_field(2, &tags, &mut f);
            field(3, 0, &mut f);
            varint(1, &mut f); // POINT
            let mut geom = Vec::new();
            varint(1 | (1 << 3), &mut geom); // MoveTo, count 1
            varint(zigzag(*x), &mut geom);
            varint(zigzag(*y), &mut geom);
            bytes_field(4, &geom, &mut f);
            features.push(f);
        }
        let mut l = Vec::new();
        field(15, 0, &mut l);
        varint(2, &mut l);
        bytes_field(1, layer.as_bytes(), &mut l);
        for f in &features {
            bytes_field(2, f, &mut l);
        }
        for k in &keys {
            bytes_field(3, k.as_bytes(), &mut l);
        }
        for v in &values {
            bytes_field(4, v, &mut l);
        }
        field(5, 0, &mut l);
        varint(u64::from(extent), &mut l);
        let mut tile = Vec::new();
        bytes_field(3, &l, &mut tile);
        tile
    }

    // --- tests --------------------------------------------------------------

    #[test]
    fn tile_math_round_trips() {
        let t = TileId {
            z: 15,
            x: 7898,
            y: 11796,
        };
        let b = tile_bounds(t);
        assert!(b.min_lat < b.max_lat && b.min_lon < b.max_lon);
        // The tile contains the point it was computed from, and only itself
        // (plus neighbours when a box straddles an edge).
        let (lat, lon) = (44.926, -93.2234);
        assert!(b.min_lat <= lat && lat <= b.max_lat && b.min_lon <= lon && lon <= b.max_lon);
        let point = GeoBounds::new(lat, lat, lon, lon);
        assert_eq!(tiles_covering(&point, 15), vec![t]);
        // A box over a tile's own bounds, nudged inwards, is that tile.
        let inner = GeoBounds::new(
            b.min_lat + 1e-6,
            b.max_lat - 1e-6,
            b.min_lon + 1e-6,
            b.max_lon - 1e-6,
        );
        assert_eq!(tiles_covering(&inner, 15), vec![t]);
        // Twice as wide: two tiles side by side.
        let wide = GeoBounds::new(
            b.min_lat + 1e-6,
            b.max_lat - 1e-6,
            b.min_lon + 1e-6,
            b.max_lon + (b.max_lon - b.min_lon) - 1e-6,
        );
        assert_eq!(tiles_covering(&wide, 15).len(), 2);
    }

    #[test]
    fn tile_point_positions() {
        let t = TileId { z: 1, x: 1, y: 0 };
        // The corner of the tile is the NW corner of the NE quadrant.
        let (lat, lon) = tile_point_to_geo(t, 4096, 0.0, 0.0);
        assert!((lon - 0.0).abs() < 1e-9 && (lat - 85.0511287798).abs() < 1e-6);
        let (lat, lon) = tile_point_to_geo(t, 4096, 4096.0, 4096.0);
        assert!((lon - 180.0).abs() < 1e-9 && lat.abs() < 1e-9);
    }

    #[test]
    fn a_real_panoramax_tile_decodes() {
        let photos = decode_panoramax_tile(PANORAMAX_TILE, PANORAMAX_TILE_ID).unwrap();
        assert!(!photos.is_empty(), "the fixture has pictures");
        let b = tile_bounds(PANORAMAX_TILE_ID);
        for p in &photos {
            assert_eq!(p.provider, Provider::Panoramax);
            assert_eq!(p.id.len(), 36, "a uuid: {}", p.id);
            assert!(
                p.lat >= b.min_lat - 1e-4
                    && p.lat <= b.max_lat + 1e-4
                    && p.lon >= b.min_lon - 1e-4
                    && p.lon <= b.max_lon + 1e-4,
                "inside the tile (the buffer is a few percent)"
            );
        }
        assert!(photos.iter().any(|p| p.heading.is_some()));
        assert!(photos.iter().any(|p| p
            .captured
            .as_deref()
            .is_some_and(|d| d.len() == 10 && d.starts_with("20"))));
    }

    #[test]
    fn mapillary_tiles_decode() {
        let t = TileId { z: 14, x: 3, y: 5 };
        let tile = encode_tile(
            "image",
            4096,
            &[
                (
                    1024,
                    2048,
                    vec![
                        ("id", Prop::Uint(123_456_789_012)),
                        ("captured_at", Prop::Double(1_700_000_000_000.0)),
                        ("compass_angle", Prop::Double(271.5)),
                    ],
                ),
                (0, 0, vec![("sequence_id", Prop::Str("no id"))]),
            ],
        );
        let photos = decode_mapillary_tile(&tile, t).unwrap();
        assert_eq!(photos.len(), 1, "a feature without an id is skipped");
        let p = &photos[0];
        assert_eq!(p.id, "123456789012");
        assert_eq!(p.heading, Some(271.5));
        assert_eq!(p.captured.as_deref(), Some("2023-11-14"));
        let (lat, lon) = tile_point_to_geo(t, 4096, 1024.0, 2048.0);
        assert_eq!((p.lat, p.lon), (lat, lon));
        // Another layer's points are not photos.
        let other = encode_tile("sequence", 4096, &[(1, 1, vec![("id", Prop::Uint(1))])]);
        assert!(decode_mapillary_tile(&other, t).unwrap().is_empty());
    }

    #[test]
    fn empty_and_broken_tiles() {
        let t = TileId { z: 14, x: 0, y: 0 };
        assert!(decode_mapillary_tile(&[], t).unwrap().is_empty());
        assert!(matches!(
            decode_mapillary_tile(b"\xff\xff\xff not a tile", t),
            Err(ImageryError::Parse(_))
        ));
    }

    #[test]
    fn urls() {
        let t = TileId { z: 14, x: 3, y: 5 };
        assert_eq!(
            mapillary_tile_url(t, "MLY|a b"),
            "https://tiles.mapillary.com/maps/vtp/mly1_public/2/14/3/5?access_token=MLY%7Ca%20b"
        );
        assert_eq!(
            panoramax_tile_url(t),
            "https://api.panoramax.xyz/api/map/2/14/3/5.mvt"
        );
        assert_eq!(tile_zoom(Provider::Mapillary), 14);
        assert_eq!(tile_zoom(Provider::Panoramax), 15);
    }

    #[test]
    fn mapillary_needs_a_token_and_never_calls_without_one() {
        let client = FakeClient::new(vec![]);
        let t = TileId { z: 14, x: 3, y: 5 };
        for cred in [None, Some("  ")] {
            assert_eq!(
                fetch_tile_with(&client, Provider::Mapillary, t, cred).unwrap_err(),
                ImageryError::NoToken
            );
        }
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn statuses_map_to_errors() {
        let t = TileId { z: 14, x: 3, y: 5 };
        let c = FakeClient::new(vec![status_err(401, "")]);
        assert_eq!(
            fetch_tile_with(&c, Provider::Mapillary, t, Some("t")).unwrap_err(),
            ImageryError::Unauthorized
        );
        let c = FakeClient::new(vec![status_err(429, "")]);
        assert_eq!(
            fetch_tile_with(&c, Provider::Fotoshi, t, None).unwrap_err(),
            ImageryError::RateLimited
        );
    }

    #[test]
    fn fotoshi_tiles_search_the_tile_box_and_flag_a_full_page() {
        let t = TileId {
            z: 15,
            x: 7898,
            y: 11796,
        };
        let one = r#"{"features":[{"id":"p1","geometry":{"type":"Point","coordinates":[-93.2,44.9]},
            "properties":{},"assets":{}}]}"#;
        let c = FakeClient::new(vec![ok(200, one)]);
        let (photos, full) = fetch_tile_with(&c, Provider::Fotoshi, t, None).unwrap();
        assert_eq!(photos.len(), 1);
        assert!(!full);
        // A page of exactly the limit may be cut short.
        let feature = r#"{"id":"p","geometry":{"type":"Point","coordinates":[-93.2,44.9]}}"#;
        let page = format!(
            r#"{{"features":[{}]}}"#,
            vec![feature; FOTOSHI_ANON_LIMIT].join(",")
        );
        let c = FakeClient::new(vec![ok(200, page)]);
        assert!(fetch_tile_with(&c, Provider::Fotoshi, t, None).unwrap().1);
    }

    #[test]
    fn details_fill_in_what_the_tile_left_out() {
        let tile_photo = StreetPhoto {
            provider: Provider::Mapillary,
            id: "111".into(),
            lat: 45.0,
            lon: -111.0,
            heading: Some(90.0),
            captured: None,
            creator: None,
            license: Some("CC BY-SA 4.0".into()),
            thumb_url: None,
            large_url: None,
        };
        let body = r#"{"id":"111","geometry":{"type":"Point","coordinates":[-111.0,45.0]},
            "captured_at":1700000000000,"creator":{"username":"alice","id":"1"},
            "thumb_256_url":"https://x/256.jpg","thumb_1024_url":"https://x/1024.jpg"}"#;
        let c = FakeClient::new(vec![ok(200, body)]);
        let p = fetch_detail_with(&c, &tile_photo, Some("tok")).unwrap();
        assert_eq!(p.creator.as_deref(), Some("alice"));
        assert_eq!(p.large_url.as_deref(), Some("https://x/1024.jpg"));
        assert_eq!(p.captured.as_deref(), Some("2023-11-14"));
        // What the tile had and the detail lacks is kept.
        assert_eq!(p.heading, Some(90.0));

        let pmx = StreetPhoto {
            provider: Provider::Panoramax,
            id: "aaa".into(),
            ..tile_photo.clone()
        };
        let body = r#"{"features":[{"id":"aaa","geometry":{"type":"Point","coordinates":[-93.2,44.9]},
            "properties":{"license":"CC-BY-SA-4.0"},"providers":[{"name":"bob","roles":["producer"]}],
            "assets":{"sd":{"href":"https://p/sd.jpg"},"thumb":{"href":"https://p/t.jpg"}}}]}"#;
        let c = FakeClient::new(vec![ok(200, body)]);
        let p = fetch_detail_with(&c, &pmx, None).unwrap();
        assert_eq!(p.creator.as_deref(), Some("bob"));
        assert_eq!(p.large_url.as_deref(), Some("https://p/sd.jpg"));
        // Mapillary's detail needs the token.
        let c = FakeClient::new(vec![]);
        assert_eq!(
            fetch_detail_with(&c, &tile_photo, None).unwrap_err(),
            ImageryError::NoToken
        );
        // An empty answer is an error, not a blank photo.
        let c = FakeClient::new(vec![ok(200, r#"{"features":[]}"#)]);
        assert!(matches!(
            fetch_detail_with(&c, &pmx, None),
            Err(ImageryError::Parse(_))
        ));
    }
}
