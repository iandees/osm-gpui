//! Fotoshi for osm-gpui: photo locations on the map and a photo's picture
//! and details in the side panel; with an API key, the photos Fotoshi has of
//! the selected OSM feature, when the user turns that lookup on.
//!
//! - Photos by area: `GET https://api.fotoshi.com/v1/stac/search?bbox=..`
//!   (STAC). Fotoshi has no photo tiles, so the map's tile grid is used as
//!   virtual tiles, each a search of its box. It works anonymously (20 photos
//!   a page, 200 requests a day); an API key raises the page to 100.
//! - A feature's photos: `GET https://api.fotoshi.com/v1/data/features
//!   ?osm_id=w123` (needs the key). This tells Fotoshi what is selected, so
//!   it is off until the user turns it on.

use osm_gpui_extension_api::{
    self as osm, button, ButtonStyle, Element, Extension, FeatureKind, HttpRequest, Inputs, Panel,
    TextInput,
};
use serde_json::Value;
use street_photos::{Photo, Photos, Source, Tile, TOGGLE_COMMAND};

const API: &str = "https://api.fotoshi.com/v1";
/// Photos per search: anonymous callers get at most 20 a page.
const ANON_LIMIT: usize = 20;
const KEYED_LIMIT: usize = 100;

const KEY: &str = "api-key";
/// Storage: look up the selected feature's photos.
const LOOKUP: &str = "lookup";
const SETTINGS_PANEL: &str = "settings";
const FEATURE_PANEL: &str = "feature";

pub struct Fotoshi;

/// The saved API key.
fn api_key() -> Option<String> {
    osm::secret_get(KEY)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn lookup_on() -> bool {
    osm::storage_get(LOOKUP).is_some()
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn point_of(v: &Value) -> Option<(f64, f64)> {
    let c = v.get("coordinates")?.as_array()?;
    Some((c.get(1)?.as_f64()?, c.first()?.as_f64()?))
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
fn photo_of(item: &Value, fallback: Option<(f64, f64)>) -> Option<Photo> {
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
    Some(Photo {
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

/// A STAC search answer `{"features":[{id, geometry, properties, assets}]}`.
pub fn parse_search(body: &str) -> Result<Vec<Photo>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or("no \"features\" array")?;
    Ok(features.iter().filter_map(|f| photo_of(f, None)).collect())
}

/// What Fotoshi has of one OSM feature.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FeaturePhotos {
    /// All the public photos of the feature (more than `photos` when the
    /// request's limit cut the list).
    pub count: usize,
    pub photos: Vec<Photo>,
}

/// The data API's answer to an `osm_id` lookup: features carrying
/// `coverage.photo_count` and a `photos` array. An empty collection means no
/// public coverage.
pub fn parse_feature(body: &str) -> Result<FeaturePhotos, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or("no \"features\" array")?;
    let mut out = FeaturePhotos::default();
    for f in features {
        let fallback = f.get("geometry").and_then(first_position);
        let before = out.photos.len();
        if let Some(photos) = f.get("photos").and_then(Value::as_array) {
            out.photos
                .extend(photos.iter().filter_map(|p| photo_of(p, fallback)));
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

/// The search of tile `t`'s box, `limit` photos a page.
pub fn search_url(t: Tile, limit: usize) -> String {
    let b = t.bounds();
    format!(
        "{API}/stac/search?bbox={:.6},{:.6},{:.6},{:.6}&limit={}",
        b.min_lon, b.min_lat, b.max_lon, b.max_lat, limit
    )
}

/// The lookup of OSM element `osm_id` (`n123`, `w456`, `r789`).
pub fn feature_url(osm_id: &str) -> String {
    format!(
        "{API}/data/features?osm_id={}&photo_limit=20&photo_size=thumbnail,1280",
        urlencoding::encode(osm_id)
    )
}

impl Source for Fotoshi {
    fn zoom(&self) -> u32 {
        // Virtual tiles about 1.2 km across at the equator.
        15
    }

    fn color(&self) -> u32 {
        0x2563eb
    }

    fn tile_request(&self, t: Tile) -> Result<HttpRequest, String> {
        let key = api_key();
        let limit = if key.is_some() {
            KEYED_LIMIT
        } else {
            ANON_LIMIT
        };
        let mut req = osm::get(&search_url(t, limit));
        if let Some(key) = key {
            req.headers
                .push(("Authorization".into(), format!("Bearer {key}")));
        }
        Ok(req)
    }

    fn decode_tile(&self, _t: Tile, body: &[u8]) -> Result<Vec<Photo>, String> {
        parse_search(&String::from_utf8_lossy(body))
    }

    fn web_url(&self, id: &str) -> String {
        format!("https://fotoshi.com/photos/{id}")
    }
}

/// The settings panel: the optional API key, and the feature lookup.
/// Its section of Settings > Extensions.
fn settings_panel() -> Panel {
    let has_key = api_key().is_some();
    let mut key_buttons = vec![button(
        "save",
        &osm::t("settings.save"),
        ButtonStyle::Primary,
    )];
    if has_key {
        key_buttons.push(button(
            "forget",
            &osm::t("settings.forget"),
            ButtonStyle::Danger,
        ));
    }
    let mut lookup = button(
        "lookup",
        &osm::t(if lookup_on() {
            "settings.lookup_off"
        } else {
            "settings.lookup_on"
        }),
        ButtonStyle::Secondary,
    );
    lookup.enabled = has_key;
    Panel {
        title: osm::t("settings.title"),
        elements: vec![
            Element::Text(osm::t("settings.explain")),
            Element::Link((osm::t("settings.find"), "https://fotoshi.com".into())),
            Element::Input(TextInput {
                id: "key".into(),
                label: osm::t("settings.label"),
                placeholder: osm::t(if has_key {
                    "settings.placeholder_replace"
                } else {
                    "settings.placeholder"
                }),
                value: String::new(),
                secret: true,
            }),
            Element::Buttons(key_buttons),
            Element::Divider,
            Element::Muted(osm::t(if lookup_on() {
                "settings.lookup_is_on"
            } else {
                "settings.lookup_is_off"
            })),
            Element::Buttons(vec![lookup]),
        ],
    }
}

struct FotoshiExtension {
    photos: Photos<Fotoshi>,
    /// The feature whose photos the feature panel lists (`w123`), and them.
    feature: Option<(String, FeaturePhotos)>,
}

impl FotoshiExtension {
    /// The one selected OSM feature as an `osm_id`, when it is on the server
    /// (new features have negative ids).
    fn selected_osm_id() -> Option<String> {
        let selection = osm::selection();
        let [f] = selection.as_slice() else {
            return None;
        };
        let kind = match f.kind {
            FeatureKind::Node => 'n',
            FeatureKind::Way => 'w',
            FeatureKind::Relation => 'r',
        };
        (f.id > 0).then(|| format!("{kind}{}", f.id))
    }

    fn close_feature_panel(&mut self) {
        if self.feature.take().is_some() {
            osm::show_panel(FEATURE_PANEL, None);
        }
    }

    /// Look up the selected feature's photos and list them, or close the
    /// list when there is nothing to look up or nothing found.
    fn update_feature(&mut self) -> osm::Result<()> {
        let key = api_key();
        let Some(osm_id) = Self::selected_osm_id().filter(|_| lookup_on() && key.is_some()) else {
            self.close_feature_panel();
            return Ok(());
        };
        if self.feature.as_ref().is_some_and(|(id, _)| *id == osm_id) {
            return Ok(());
        }
        let mut req = osm::get(&feature_url(&osm_id));
        if let Some(key) = key {
            req.headers
                .push(("Authorization".into(), format!("Bearer {key}")));
        }
        // One attempt: Fotoshi meters requests.
        let found = match osm::http_fetch(&req) {
            Ok(resp) if (200..300).contains(&resp.status) => {
                parse_feature(&String::from_utf8_lossy(&resp.body))?
            }
            Ok(resp) if resp.status == 401 || resp.status == 403 => {
                self.close_feature_panel();
                return Err(osm::t("photos.unauthorized"));
            }
            Ok(resp) if resp.status == 429 => {
                self.close_feature_panel();
                return Err(osm::t("photos.rate_limited"));
            }
            Ok(resp) => return Err(format!("Fotoshi: HTTP {}", resp.status)),
            Err(e) => return Err(osm::t_args("photos.failed", &[("detail", &e)])),
        };
        if found.photos.is_empty() {
            self.close_feature_panel();
            return Ok(());
        }
        let rows = found
            .photos
            .iter()
            .enumerate()
            .map(|(n, p)| {
                let label = match (&p.captured, &p.creator) {
                    (Some(d), Some(c)) => format!("{d} · {c}"),
                    (Some(d), None) => d.clone(),
                    (None, Some(c)) => c.clone(),
                    (None, None) => {
                        osm::t_args("feature.photo", &[("number", &(n + 1).to_string())])
                    }
                };
                Element::Buttons(vec![button(
                    &format!("photo-{n}"),
                    &label,
                    ButtonStyle::Ghost,
                )])
            })
            .collect::<Vec<_>>();
        let mut elements = vec![Element::Heading(osm::t_args(
            "feature.count",
            &[("count", &found.count.to_string())],
        ))];
        elements.extend(rows);
        osm::show_panel(
            FEATURE_PANEL,
            Some(&Panel {
                title: osm::t("feature.title"),
                elements,
            }),
        );
        self.feature = Some((osm_id, found));
        Ok(())
    }
}

impl Extension for FotoshiExtension {
    fn new() -> Self {
        FotoshiExtension {
            photos: Photos::new(Fotoshi),
            feature: None,
        }
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        match id {
            TOGGLE_COMMAND if self.photos.shown() => {
                self.photos.hide();
                Ok(())
            }
            TOGGLE_COMMAND => self.photos.show(),
            other => Err(format!("unknown command {other}")),
        }
    }

    fn viewport_changed(&mut self) -> osm::Result<()> {
        self.photos.viewport_changed()
    }

    fn selection_changed(&mut self) -> osm::Result<()> {
        self.update_feature()
    }

    fn marker_clicked(&mut self, id: &str) -> osm::Result<()> {
        self.photos.marker_clicked(id)
    }

    fn panel_button(&mut self, panel: &str, button: &str, inputs: &Inputs) -> osm::Result<()> {
        match (panel, button) {
            (SETTINGS_PANEL, "save") => {
                let value = inputs.get("key").unwrap_or("").trim();
                if !value.is_empty() {
                    osm::secret_set(KEY, Some(value));
                    osm::set_status(&osm::t("settings.saved"));
                    self.photos.unblock();
                    self.photos.refresh()?;
                }
            }
            (SETTINGS_PANEL, "forget") => {
                osm::secret_set(KEY, None);
                osm::storage_set(LOOKUP, None);
                osm::set_status(&osm::t("settings.forgotten"));
                self.close_feature_panel();
                self.photos.refresh()?;
            }
            (SETTINGS_PANEL, "lookup") => {
                if lookup_on() {
                    osm::storage_set(LOOKUP, None);
                    self.close_feature_panel();
                } else {
                    osm::storage_set(LOOKUP, Some("1"));
                    self.update_feature()?;
                }
            }
            (FEATURE_PANEL, b) => {
                let photo = b
                    .strip_prefix("photo-")
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| self.feature.as_ref()?.1.photos.get(n).cloned());
                if let Some(photo) = photo {
                    self.photos.open(photo)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn settings(&mut self) -> Option<Panel> {
        Some(settings_panel())
    }

    fn panel_closed(&mut self, panel: &str) {
        if panel == FEATURE_PANEL {
            self.feature = None;
        }
        self.photos.panel_closed(panel);
    }
}

osm::register_extension!(FotoshiExtension);

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","id":"p1","geometry":{"type":"Point","coordinates":[-111.02,45.67]},
         "properties":{"capture_date":"2024-05-06","view:azimuth":270.0,
           "creator_name":"Bob","username":"bobby","image_license":"CC-BY-4.0"},
         "assets":{"thumbnail":{"href":"https://f/t.jpg"},"image-1280":{"href":"https://f/1280.jpg"}}},
        {"type":"Feature","id":"p2","geometry":{"type":"Point","coordinates":[-111.03,45.68]},
         "properties":{"username":"carol"},"assets":{}}
    ]}"#;

    const FEATURE: &str = r#"{"type":"FeatureCollection","numberReturned":1,"features":[
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
    fn searches_read_stac_features() {
        let photos = parse_search(SEARCH).unwrap();
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
        assert!(parse_search("nope").is_err());
    }

    #[test]
    fn a_feature_lookup_reads_embedded_photos() {
        let o = parse_feature(FEATURE).unwrap();
        assert_eq!(o.count, 37, "the total, not the page");
        assert_eq!(o.photos.len(), 2);
        let p = &o.photos[0];
        assert_eq!(p.id, "ph1");
        // No position of its own: the feature's first vertex.
        assert_eq!((p.lat, p.lon), (44.9, -93.2));
        assert_eq!(p.creator.as_deref(), Some("dana"));
        assert_eq!(p.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(p.large_url.as_deref(), Some("https://f/1280-1.jpg"));
        let q = &o.photos[1];
        assert_eq!((q.lat, q.lon), (44.905, -93.205));
        assert_eq!(q.creator.as_deref(), Some("Eli"));
        assert_eq!(q.large_url.as_deref(), Some("https://f/t2.jpg"));
        let none = parse_feature(r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        assert_eq!(none, FeaturePhotos::default());
    }

    #[test]
    fn urls_carry_the_box_limit_and_osm_id() {
        let t = Tile {
            z: 15,
            x: 5000,
            y: 12000,
        };
        let u = search_url(t, 20);
        assert!(u.starts_with("https://api.fotoshi.com/v1/stac/search?bbox="));
        assert!(u.ends_with("&limit=20"));
        assert!(feature_url("w456").contains("osm_id=w456&photo_limit=20"));
        assert_eq!(Fotoshi.web_url("p1"), "https://fotoshi.com/photos/p1");
    }
}
