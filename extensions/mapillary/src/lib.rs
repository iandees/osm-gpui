//! Mapillary for osm-gpui: photo points on the map (vector tiles) and a
//! photo's picture and details in the side panel. Mapillary needs an access
//! token (a client token from https://www.mapillary.com/dashboard/developers),
//! kept in the system keychain.
//!
//! - Tiles: `https://tiles.mapillary.com/maps/vtp/mly1_public/2/{z}/{x}/{y}`,
//!   layer `image` at zoom 14, one point per photo (`id`, `compass_angle`,
//!   `captured_at` in ms).
//! - Details: `https://graph.mapillary.com/<id>?fields=...`.

use osm_gpui_extension_api::{
    self as osm, button, ButtonStyle, Element, Extension, HttpRequest, Inputs, Panel, TextInput,
};
use serde_json::Value;
use street_photos::{date_from_millis, mvt, Photo, Photos, Source, Tile};

const TOKEN: &str = "access-token";
const TOKEN_PANEL: &str = "token";

pub struct Mapillary;

/// The saved access token.
fn token() -> Option<String> {
    osm::secret_get(TOKEN)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// `message` with the value of every `access_token=` query parameter
/// replaced, so an error that quotes a request URL never shows the token.
pub fn redact_access_tokens(message: &str) -> String {
    const KEY: &str = "access_token=";
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find(KEY) {
        let value_start = at + KEY.len();
        out.push_str(&rest[..value_start]);
        out.push_str("[redacted]");
        let value = &rest[value_start..];
        let end = value
            .find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')' | '>' | '#'))
            .unwrap_or(value.len());
        rest = &value[end..];
    }
    out.push_str(rest);
    out
}

/// A tile's `image` layer.
pub fn decode_tile(t: Tile, bytes: &[u8]) -> Result<Vec<Photo>, String> {
    let mut out = Vec::new();
    for (lat, lon, props) in mvt::points(bytes, "image", t)? {
        let Some(id) = props.get("id").and_then(mvt::text) else {
            continue;
        };
        out.push(Photo {
            id,
            lat,
            lon,
            heading: props
                .get("compass_angle")
                .and_then(mvt::number)
                .map(|h| h as f32),
            captured: props
                .get("captured_at")
                .and_then(mvt::number)
                .map(|ms| date_from_millis(ms as i64)),
            license: Some("CC BY-SA 4.0".to_string()),
            ..Default::default()
        });
    }
    Ok(out)
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

/// One Mapillary image object (the graph API). The computed (corrected)
/// position and heading win over the raw ones.
pub fn parse_image(item: &Value) -> Option<Photo> {
    let id = item.get("id").and_then(|i| {
        i.as_str()
            .map(str::to_string)
            .or_else(|| i.as_i64().map(|n| n.to_string()))
    })?;
    let (lat, lon) = item
        .get("computed_geometry")
        .and_then(point_of)
        .or_else(|| item.get("geometry").and_then(point_of))?;
    Some(Photo {
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

impl Source for Mapillary {
    fn zoom(&self) -> u32 {
        14
    }

    fn color(&self) -> u32 {
        0x05cb63
    }

    fn tile_request(&self, t: Tile) -> Result<HttpRequest, String> {
        let token = token().ok_or_else(|| osm::t("photos.no_token"))?;
        Ok(osm::get(&format!(
            "https://tiles.mapillary.com/maps/vtp/mly1_public/2/{}/{}/{}?access_token={}",
            t.z,
            t.x,
            t.y,
            urlencoding::encode(&token)
        )))
    }

    fn decode_tile(&self, t: Tile, body: &[u8]) -> Result<Vec<Photo>, String> {
        decode_tile(t, body)
    }

    fn detail_request(&self, photo: &Photo) -> Option<HttpRequest> {
        let token = token()?;
        Some(osm::get(&format!(
            "https://graph.mapillary.com/{}?fields=id,geometry,computed_geometry,\
             compass_angle,computed_compass_angle,captured_at,creator,\
             thumb_256_url,thumb_1024_url&access_token={}",
            urlencoding::encode(&photo.id),
            urlencoding::encode(&token)
        )))
    }

    fn decode_detail(&self, body: &[u8]) -> Result<Option<Photo>, String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        Ok(parse_image(&v))
    }

    fn web_url(&self, id: &str) -> String {
        format!("https://www.mapillary.com/app/?pKey={id}&focus=photo")
    }

    fn unauthorized_message(&self) -> String {
        osm::t("photos.unauthorized")
    }

    fn redact(&self, message: &str) -> String {
        redact_access_tokens(message)
    }
}

fn show_token_panel() {
    let has_token = token().is_some();
    let mut buttons = vec![
        button("save", &osm::t("token.save"), ButtonStyle::Primary),
        button("cancel", &osm::t("token.cancel"), ButtonStyle::Ghost),
    ];
    if has_token {
        buttons.insert(
            1,
            button("forget", &osm::t("token.forget"), ButtonStyle::Danger),
        );
    }
    osm::show_panel(
        TOKEN_PANEL,
        Some(&Panel {
            title: osm::t("token.title"),
            elements: vec![
                Element::Text(osm::t("token.explain")),
                Element::Link((
                    osm::t("token.find"),
                    "https://www.mapillary.com/dashboard/developers".into(),
                )),
                Element::Input(TextInput {
                    id: "token".into(),
                    label: osm::t("token.label"),
                    placeholder: osm::t(if has_token {
                        "token.placeholder_replace"
                    } else {
                        "token.placeholder"
                    }),
                    value: String::new(),
                    secret: true,
                }),
                Element::Buttons(buttons),
            ],
        }),
    );
}

struct MapillaryExtension {
    photos: Photos<Mapillary>,
}

impl Extension for MapillaryExtension {
    fn new() -> Self {
        MapillaryExtension {
            photos: Photos::new(Mapillary),
        }
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        match id {
            "show-photos" => {
                if token().is_none() {
                    // Without a token there would be nothing to show: ask.
                    osm::set_status(&osm::t("photos.no_token"));
                    show_token_panel();
                    return Ok(());
                }
                self.photos.show()
            }
            "hide-photos" => {
                self.photos.hide();
                Ok(())
            }
            "refresh-photos" => self.photos.refresh(),
            "settings" => {
                show_token_panel();
                Ok(())
            }
            other => Err(format!("unknown command {other}")),
        }
    }

    fn viewport_changed(&mut self) -> osm::Result<()> {
        self.photos.viewport_changed()
    }

    fn marker_clicked(&mut self, id: &str) -> osm::Result<()> {
        self.photos.marker_clicked(id)
    }

    fn panel_button(&mut self, panel: &str, button: &str, inputs: &Inputs) -> osm::Result<()> {
        if panel != TOKEN_PANEL {
            return Ok(());
        }
        match button {
            "save" => {
                let value = inputs.get("token").unwrap_or("").trim();
                if !value.is_empty() {
                    osm::secret_set(TOKEN, Some(value));
                    osm::set_status(&osm::t("token.saved"));
                    osm::show_panel(TOKEN_PANEL, None);
                    self.photos.unblock();
                    // The user wants to see photos: show them now.
                    return self.photos.show();
                }
                osm::show_panel(TOKEN_PANEL, None);
            }
            "forget" => {
                osm::secret_set(TOKEN, None);
                osm::set_status(&osm::t("token.forgotten"));
                osm::show_panel(TOKEN_PANEL, None);
                self.photos.hide();
            }
            _ => osm::show_panel(TOKEN_PANEL, None),
        }
        Ok(())
    }

    fn panel_closed(&mut self, panel: &str) {
        self.photos.panel_closed(panel);
    }
}

osm::register_extension!(MapillaryExtension);

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: &str = r#"{"id":"111","geometry":{"type":"Point","coordinates":[-111.0,45.0]},
         "computed_geometry":{"type":"Point","coordinates":[-111.0001,45.0001]},
         "compass_angle":10.0,"computed_compass_angle":12.5,
         "captured_at":1700000000000,"creator":{"username":"alice","id":"1"},
         "thumb_256_url":"https://x/256.jpg","thumb_1024_url":"https://x/1024.jpg"}"#;

    #[test]
    fn details_prefer_the_computed_position_and_heading() {
        let v: Value = serde_json::from_str(IMAGE).unwrap();
        let p = parse_image(&v).unwrap();
        assert_eq!((p.lat, p.lon), (45.0001, -111.0001));
        assert_eq!(p.heading, Some(12.5));
        assert_eq!(p.captured.as_deref(), Some("2023-11-14"));
        assert_eq!(p.creator.as_deref(), Some("alice"));
        assert_eq!(p.picture_url(), Some("https://x/1024.jpg"));
        // No position: not a photo. A numeric id is written out.
        assert!(parse_image(&serde_json::json!({"id":"333"})).is_none());
        let n = serde_json::json!({"id":222,"geometry":{"type":"Point","coordinates":[1.0,2.0]}});
        assert_eq!(parse_image(&n).unwrap().id, "222");
    }

    #[test]
    fn tokens_never_reach_a_message() {
        assert_eq!(
            redact_access_tokens(
                "error sending https://graph.mapillary.com/1?fields=id&access_token=MLY|123|abc (timed out)"
            ),
            "error sending https://graph.mapillary.com/1?fields=id&access_token=[redacted] (timed out)"
        );
        assert_eq!(
            redact_access_tokens("a?access_token=x&b=1 and c?access_token=y"),
            "a?access_token=[redacted]&b=1 and c?access_token=[redacted]"
        );
        assert_eq!(redact_access_tokens("no token here"), "no token here");
    }

    #[test]
    fn photo_pages_and_empty_tiles() {
        assert_eq!(
            Mapillary.web_url("42"),
            "https://www.mapillary.com/app/?pKey=42&focus=photo"
        );
        let t = Tile { z: 14, x: 1, y: 1 };
        assert!(decode_tile(t, b"").unwrap().is_empty());
    }
}
