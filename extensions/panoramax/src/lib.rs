//! Panoramax for osm-gpui: the federated catalogue's photo points on the map
//! (vector tiles, no key), and a photo's picture and details in the side
//! panel.
//!
//! - Tiles: `https://api.panoramax.xyz/api/map/2/{z}/{x}/{y}.mvt`, layer
//!   `pictures` from zoom 15, one point per photo (`id`, `heading`, `ts`).
//! - Details: `https://api.panoramax.xyz/api/search?ids=<id>` (STAC), with the
//!   image URLs on the instance that holds the photo.

use osm_gpui_extension_api::{self as osm, Extension, HttpRequest};
use serde_json::Value;
use street_photos::{mvt, Photo, Photos, Source, Tile};

const API: &str = "https://api.panoramax.xyz";

pub struct Panoramax;

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A tile's `pictures` layer.
pub fn decode_tile(t: Tile, bytes: &[u8]) -> Result<Vec<Photo>, String> {
    let mut out = Vec::new();
    for (lat, lon, props) in mvt::points(bytes, "pictures", t)? {
        let Some(id) = props.get("id").and_then(mvt::text) else {
            continue;
        };
        out.push(Photo {
            id,
            lat,
            lon,
            heading: props.get("heading").and_then(mvt::number).map(|h| h as f32),
            captured: props
                .get("ts")
                .and_then(mvt::text)
                .and_then(|d| d.get(..10).map(str::to_string)),
            ..Default::default()
        });
    }
    Ok(out)
}

/// A STAC search answer `{"features":[{id, geometry, properties, assets,
/// providers}]}`.
pub fn parse_search(body: &str) -> Result<Vec<Photo>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or("no \"features\" array")?;
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(id) = str_of(f.get("id")) else {
            continue;
        };
        let Some(c) = f
            .get("geometry")
            .and_then(|g| g.get("coordinates"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let (Some(lon), Some(lat)) = (
            c.first().and_then(Value::as_f64),
            c.get(1).and_then(Value::as_f64),
        ) else {
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
        out.push(Photo {
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

impl Source for Panoramax {
    fn zoom(&self) -> u32 {
        15
    }

    fn color(&self) -> u32 {
        0x9333ea
    }

    fn tile_request(&self, t: Tile) -> Result<HttpRequest, String> {
        Ok(osm::get(&format!(
            "{API}/api/map/2/{}/{}/{}.mvt",
            t.z, t.x, t.y
        )))
    }

    fn decode_tile(&self, t: Tile, body: &[u8]) -> Result<Vec<Photo>, String> {
        decode_tile(t, body)
    }

    fn detail_request(&self, photo: &Photo) -> Option<HttpRequest> {
        Some(osm::get(&format!(
            "{API}/api/search?ids={}",
            urlencoding::encode(&photo.id)
        )))
    }

    fn decode_detail(&self, body: &[u8]) -> Result<Option<Photo>, String> {
        Ok(parse_search(&String::from_utf8_lossy(body))?
            .into_iter()
            .next())
    }

    fn web_url(&self, id: &str) -> String {
        format!("{API}/#focus=pic&pic={id}")
    }
}

struct PanoramaxExtension {
    photos: Photos<Panoramax>,
}

impl Extension for PanoramaxExtension {
    fn new() -> Self {
        PanoramaxExtension {
            photos: Photos::new(Panoramax),
        }
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        match id {
            "show-photos" => self.photos.show(),
            "hide-photos" => {
                self.photos.hide();
                Ok(())
            }
            "refresh-photos" => self.photos.refresh(),
            other => Err(format!("unknown command {other}")),
        }
    }

    fn viewport_changed(&mut self) -> osm::Result<()> {
        self.photos.viewport_changed()
    }

    fn marker_clicked(&mut self, id: &str) -> osm::Result<()> {
        self.photos.marker_clicked(id)
    }

    fn panel_closed(&mut self, panel: &str) {
        self.photos.panel_closed(panel);
    }
}

osm::register_extension!(PanoramaxExtension);

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = r#"{"type":"FeatureCollection","features":[
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
    fn search_answers_read_stac_features() {
        let photos = parse_search(SEARCH).unwrap();
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
        assert!(parse_search("{}").is_err());
    }

    #[test]
    fn a_real_tile_decodes_to_photos() {
        let bytes = include_bytes!("../../street-photos/fixtures/panoramax_15_7898_11796.mvt");
        let t = Tile {
            z: 15,
            x: 7898,
            y: 11796,
        };
        let photos = decode_tile(t, bytes).unwrap();
        assert!(!photos.is_empty());
        assert!(photos.iter().all(|p| p.id.len() == 36), "uuids");
        assert!(photos.iter().any(|p| p.heading.is_some()));
        assert!(photos.iter().any(|p| p
            .captured
            .as_deref()
            .is_some_and(|d| d.len() == 10 && d.starts_with("20"))));
        assert!(Panoramax.web_url("aaa").ends_with("pic=aaa"));
    }
}
