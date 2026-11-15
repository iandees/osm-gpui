//! ArcGIS REST (Esri) imagery: reading a server's catalogue of folders and
//! services, a MapServer's or ImageServer's description, and turning a
//! chosen service (and, for a MapServer, a choice of its layers) into a tile
//! URL template the tile layer can fetch.
//!
//! A service with a tile cache on the standard Web Mercator grid is fetched
//! from its `tile/{z}/{y}/{x}` endpoint. Anything else (a dynamic service, a
//! cache on another grid, or a MapServer shown with a subset of its layers)
//! is drawn per tile by `export` / `exportImage` with the tile's box.

use std::collections::BTreeSet;

use crate::http::{HttpClient, HttpRequest, RetryPolicy};
use crate::tiles::BBOX_3857_PLACEHOLDER;

/// Zoom range given to a service drawn by `export`, which renders any scale.
pub const DYNAMIC_MIN_ZOOM: u32 = 0;
pub const DYNAMIC_MAX_ZOOM: u32 = 22;

/// The two kinds of ArcGIS service that draw images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    MapServer,
    ImageServer,
}

impl ServiceKind {
    fn from_segment(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("MapServer") {
            Some(Self::MapServer)
        } else if s.eq_ignore_ascii_case("ImageServer") {
            Some(Self::ImageServer)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MapServer => "MapServer",
            Self::ImageServer => "ImageServer",
        }
    }
}

/// One service: its base URL (ending in `/MapServer` or `/ImageServer`), its
/// path below the catalogue root (`Folder/Name`) and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRef {
    pub url: String,
    pub path: String,
    pub kind: ServiceKind,
}

impl ServiceRef {
    /// The service's own name: the last part of its path.
    pub fn short_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Where in a server a URL points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The catalogue root (`…/rest/services`) or a folder below it.
    Directory {
        root: String,
        folder: Option<String>,
    },
    /// A service, and the layer of it the URL named (`…/MapServer/3`).
    Service {
        root: Option<String>,
        service: ServiceRef,
        layer: Option<u32>,
    },
}

impl Location {
    /// The URL this location is read from (without `?f=json`).
    pub fn url(&self) -> String {
        match self {
            Location::Directory { root, folder: None } => root.clone(),
            Location::Directory {
                root,
                folder: Some(f),
            } => format!("{root}/{f}"),
            Location::Service { service, .. } => service.url.clone(),
        }
    }

    /// The directory one step up: a service's folder (or the root), a
    /// folder's root. `None` at the root, or for a service whose catalogue
    /// root the URL did not show.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Directory { folder: None, .. } => None,
            Location::Directory {
                root,
                folder: Some(f),
            } => Some(Location::Directory {
                root: root.clone(),
                folder: f.rsplit_once('/').map(|(head, _)| head.to_string()),
            }),
            Location::Service {
                root: Some(root),
                service,
                ..
            } => Some(Location::Directory {
                root: root.clone(),
                folder: service
                    .path
                    .rsplit_once('/')
                    .map(|(head, _)| head.to_string()),
            }),
            Location::Service { root: None, .. } => None,
        }
    }
}

/// A URL as typed, split into where it points and the `token` it carried (a
/// secured server's access token, sent again with every request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub location: Location,
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsriError {
    /// The text is not an http(s) URL of an ArcGIS REST catalogue, folder or
    /// MapServer/ImageServer.
    NotArcGisUrl,
    /// The request failed (network or HTTP status).
    Http(String),
    /// The reply was not the JSON expected.
    BadResponse(String),
    /// The server answered with an ArcGIS error object (e.g. "Token Required").
    Server { code: Option<i64>, message: String },
}

impl std::fmt::Display for EsriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EsriError::NotArcGisUrl => write!(f, "not an ArcGIS REST services URL"),
            EsriError::Http(e) => write!(f, "{e}"),
            EsriError::BadResponse(e) => write!(f, "unexpected response: {e}"),
            EsriError::Server {
                code: Some(c),
                message,
            } => write!(f, "{message} ({c})"),
            EsriError::Server {
                code: None,
                message,
            } => write!(f, "{message}"),
        }
    }
}

const SERVICES_SEGMENT: &str = "/rest/services";

/// Read a pasted URL: a catalogue root (`https://host/arcgis/rest/services`),
/// a folder below it, a service (`…/Name/MapServer`) or one of its layers
/// (`…/MapServer/3`). Query strings and fragments are dropped, except for a
/// `token` parameter.
pub fn parse_url(input: &str) -> Result<ParsedUrl, EsriError> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(EsriError::NotArcGisUrl);
    }
    let without_fragment = input.split('#').next().unwrap_or(input);
    let (path_part, query) = match without_fragment.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (without_fragment, None),
    };
    let token = query.and_then(|q| {
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| k.eq_ignore_ascii_case("token"))
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
    });
    let path_part = path_part.trim_end_matches('/');

    let root_end = path_part
        .to_ascii_lowercase()
        .find(SERVICES_SEGMENT)
        .map(|i| i + SERVICES_SEGMENT.len())
        .filter(|&end| {
            // `/rest/services` must be a whole segment, not `/rest/servicesX`.
            path_part[end..].is_empty() || path_part[end..].starts_with('/')
        });

    let location = match root_end {
        Some(end) => {
            let root = path_part[..end].to_string();
            let segments: Vec<&str> = path_part[end..]
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            match segments
                .iter()
                .position(|s| ServiceKind::from_segment(s).is_some())
            {
                Some(i) if i > 0 => {
                    let kind = ServiceKind::from_segment(segments[i]).unwrap();
                    let path = segments[..i].join("/");
                    Location::Service {
                        service: ServiceRef {
                            url: format!("{root}/{path}/{}", kind.as_str()),
                            path,
                            kind,
                        },
                        layer: segments.get(i + 1).and_then(|s| s.parse().ok()),
                        root: Some(root),
                    }
                }
                Some(_) => return Err(EsriError::NotArcGisUrl),
                None => Location::Directory {
                    folder: (!segments.is_empty()).then(|| segments.join("/")),
                    root,
                },
            }
        }
        None => {
            // No catalogue root in sight: only a service URL can be read.
            let scheme_end = path_part.find("://").map(|i| i + 3).unwrap_or(0);
            let segments: Vec<&str> = path_part[scheme_end..].split('/').collect();
            let i = segments
                .iter()
                .position(|s| ServiceKind::from_segment(s).is_some())
                .filter(|&i| i >= 2)
                .ok_or(EsriError::NotArcGisUrl)?;
            let kind = ServiceKind::from_segment(segments[i]).unwrap();
            Location::Service {
                root: None,
                service: ServiceRef {
                    url: format!("{}{}", &path_part[..scheme_end], segments[..=i].join("/")),
                    path: segments[i - 1].to_string(),
                    kind,
                },
                layer: segments.get(i + 1).and_then(|s| s.parse().ok()),
            }
        }
    };
    Ok(ParsedUrl { location, token })
}

/// A catalogue directory: its subfolders and its image services (services of
/// other kinds are left out).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Directory {
    /// Full folder paths below the root.
    pub folders: Vec<String>,
    pub services: Vec<ServiceRef>,
}

/// One layer of a MapServer.
#[derive(Debug, Clone, PartialEq)]
pub struct MapLayer {
    pub id: u32,
    pub name: String,
    pub parent_id: Option<u32>,
    pub default_visibility: bool,
    /// Whether this is a group of other layers.
    pub is_group: bool,
}

/// A tile cache laid out on the standard Web Mercator grid, so its
/// `tile/{z}/{y}/{x}` endpoint is an ordinary XYZ tile source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebMercatorCache {
    pub min_zoom: u32,
    pub max_zoom: u32,
    /// Tile width in pixels: 256, or 512 for Esri's high-resolution scheme
    /// (each level's tiles cover the same cells as the 256 grid's, at twice
    /// the pixels).
    pub tile_size: u32,
}

/// Pixel size of the images drawn by `export` / `exportImage`: one
/// high-resolution image per grid cell, so a Hi-DPI display needs one
/// request per cell rather than four a zoom deeper.
pub const EXPORT_TILE_SIZE: u32 = 512;

/// The `dpi` asked of a MapServer `export` at [`EXPORT_TILE_SIZE`]: twice
/// ArcGIS's 96, so symbols and labels come out at their usual size on a
/// 256-point grid cell and the map scale (which ArcGIS works out from the
/// image size and dpi) is that of a 256-pixel tile, keeping scale-dependent
/// layers switching at their usual zooms.
const EXPORT_DPI: u32 = 192;

/// What a MapServer or ImageServer says about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServiceInfo {
    /// A display name: the map name, unless it is ArcGIS's placeholder.
    pub title: Option<String>,
    pub description: Option<String>,
    pub copyright: Option<String>,
    pub layers: Vec<MapLayer>,
    pub cache: Option<WebMercatorCache>,
    /// Whether the service can draw arbitrary boxes (`export` /
    /// `exportImage`). Tile-only hosted services cannot.
    pub dynamic: bool,
}

impl ServiceInfo {
    /// The layers drawn when none are named: those visible by default.
    pub fn default_layers(&self) -> BTreeSet<u32> {
        self.layers
            .iter()
            .filter(|l| l.default_visibility)
            .map(|l| l.id)
            .collect()
    }

    /// How deep `layer` sits in the group tree (0 at the top).
    pub fn depth(&self, layer: &MapLayer) -> usize {
        let mut depth = 0;
        let mut parent = layer.parent_id;
        while let Some(p) = parent {
            depth += 1;
            if depth > 32 {
                break;
            }
            parent = self
                .layers
                .iter()
                .find(|l| l.id == p)
                .and_then(|l| l.parent_id);
        }
        depth
    }
}

fn json_get(
    client: &dyn HttpClient,
    url: &str,
    token: Option<&str>,
) -> Result<serde_json::Value, EsriError> {
    let mut full = format!("{url}?f=json");
    if let Some(t) = token {
        full.push_str("&token=");
        full.push_str(t);
    }
    let resp =
        crate::http::fetch_with_retries(client, &HttpRequest::get(full), &RetryPolicy::standard())
            .map_err(|e| EsriError::Http(e.to_string()))?;
    let body = resp
        .into_string()
        .map_err(|e| EsriError::BadResponse(e.to_string()))?;
    parse_json_reply(&body)
}

/// Parse a reply body, turning ArcGIS's in-band `{"error": …}` (sent with a
/// 200 status) into an error.
fn parse_json_reply(body: &str) -> Result<serde_json::Value, EsriError> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| EsriError::BadResponse(e.to_string()))?;
    if let Some(err) = v.get("error") {
        return Err(EsriError::Server {
            code: err.get("code").and_then(|c| c.as_i64()),
            message: err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("error")
                .to_string(),
        });
    }
    if !v.is_object() {
        return Err(EsriError::BadResponse("not a JSON object".into()));
    }
    Ok(v)
}

/// Fetch and read a catalogue directory.
pub fn fetch_directory(
    client: &dyn HttpClient,
    root: &str,
    folder: Option<&str>,
    token: Option<&str>,
) -> Result<Directory, EsriError> {
    let url = match folder {
        Some(f) => format!("{root}/{f}"),
        None => root.to_string(),
    };
    let v = json_get(client, &url, token)?;
    Ok(parse_directory(&v, root, folder))
}

/// Read a directory reply. Folder names are full paths below the root in a
/// root listing, but some servers list a nested folder's subfolders by their
/// last name only: those are prefixed with `folder`.
pub fn parse_directory(v: &serde_json::Value, root: &str, folder: Option<&str>) -> Directory {
    let folders = v
        .get("folders")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str())
                .map(|name| match folder {
                    Some(parent) if !name.starts_with(&format!("{parent}/")) => {
                        format!("{parent}/{name}")
                    }
                    _ => name.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut services: Vec<ServiceRef> = v
        .get("services")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let path = s.get("name")?.as_str()?.trim_matches('/').to_string();
                    let kind = ServiceKind::from_segment(s.get("type")?.as_str()?)?;
                    Some(ServiceRef {
                        url: format!("{root}/{path}/{}", kind.as_str()),
                        path,
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    services.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
            .then((a.kind as u8).cmp(&(b.kind as u8)))
    });
    Directory { folders, services }
}

/// Fetch and read a service's description.
pub fn fetch_service(
    client: &dyn HttpClient,
    service: &ServiceRef,
    token: Option<&str>,
) -> Result<ServiceInfo, EsriError> {
    let v = json_get(client, &service.url, token)?;
    Ok(parse_service(&v, service.kind))
}

fn non_empty_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|s| s.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn parse_service(v: &serde_json::Value, kind: ServiceKind) -> ServiceInfo {
    let title = match kind {
        // ArcGIS names every map document's data frame "Layers" by default.
        ServiceKind::MapServer => non_empty_str(v, "mapName").filter(|n| n != "Layers"),
        ServiceKind::ImageServer => non_empty_str(v, "name"),
    };
    let description = non_empty_str(v, "serviceDescription")
        .or_else(|| non_empty_str(v, "description"))
        .map(|d| strip_html(&d))
        .filter(|d| !d.is_empty());
    let copyright = non_empty_str(v, "copyrightText");
    let layers = v
        .get("layers")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(parse_layer).collect())
        .unwrap_or_default();
    let cache = if v
        .get("singleFusedMapCache")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        v.get("tileInfo").and_then(web_mercator_cache)
    } else {
        None
    };
    let capabilities = v.get("capabilities").and_then(|c| c.as_str()).unwrap_or("");
    // A hosted tile service lists "TilesOnly"; everything else can export.
    let dynamic = !capabilities
        .split(',')
        .any(|c| c.trim().eq_ignore_ascii_case("TilesOnly"));
    ServiceInfo {
        title,
        description,
        copyright,
        layers,
        cache,
        dynamic,
    }
}

fn parse_layer(v: &serde_json::Value) -> Option<MapLayer> {
    let id = u32::try_from(v.get("id")?.as_i64()?).ok()?;
    let parent_id = v
        .get("parentLayerId")
        .and_then(|p| p.as_i64())
        .and_then(|p| u32::try_from(p).ok());
    let is_group = v
        .get("subLayerIds")
        .and_then(|s| s.as_array())
        .is_some_and(|a| !a.is_empty());
    Some(MapLayer {
        id,
        name: non_empty_str(v, "name").unwrap_or_else(|| format!("Layer {id}")),
        parent_id,
        default_visibility: v
            .get("defaultVisibility")
            .and_then(|b| b.as_bool())
            .unwrap_or(true),
        is_group,
    })
}

/// Resolution, in metres per pixel, of zoom 0 on the 256-pixel Web Mercator
/// grid.
const ZOOM0_RESOLUTION: f64 = 156_543.033_928_040_97;
const WEB_MERCATOR_WKIDS: [i64; 4] = [102100, 102113, 3857, 900913];

/// The zoom range of `tileInfo` when it is the standard grid: Web Mercator,
/// 256- or 512-pixel tiles, origin at the world's north-west corner, and
/// each level's tiles covering the cells of the standard zoom it is
/// numbered by.
fn web_mercator_cache(tile_info: &serde_json::Value) -> Option<WebMercatorCache> {
    let sr = tile_info.get("spatialReference")?;
    let wkid_ok = ["wkid", "latestWkid"]
        .iter()
        .filter_map(|k| sr.get(*k).and_then(|w| w.as_i64()))
        .any(|w| WEB_MERCATOR_WKIDS.contains(&w));
    if !wkid_ok {
        return None;
    }
    let rows = tile_info.get("rows")?.as_u64()?;
    let cols = tile_info.get("cols")?.as_u64()?;
    if rows != cols || !(rows == 256 || rows == 512) {
        return None;
    }
    let tile_size = rows as u32;
    let origin = tile_info.get("origin")?;
    let ox = origin.get("x")?.as_f64()?;
    let oy = origin.get("y")?.as_f64()?;
    let half = crate::coordinates::MERCATOR_HALF_WORLD_M;
    if (ox + half).abs() > 1.0 || (oy - half).abs() > 1.0 {
        return None;
    }
    let lods = tile_info.get("lods")?.as_array()?;
    let mut levels = Vec::with_capacity(lods.len());
    for lod in lods {
        let level = u32::try_from(lod.get("level")?.as_i64()?).ok()?;
        let resolution = lod.get("resolution")?.as_f64()?;
        // A tile of `tile_size` pixels spans the cell of a 256-pixel one.
        let expected = ZOOM0_RESOLUTION * 256.0 / f64::from(tile_size) / 2f64.powi(level as i32);
        if ((resolution - expected) / expected).abs() > 1e-3 {
            return None;
        }
        levels.push(level);
    }
    Some(WebMercatorCache {
        min_zoom: *levels.iter().min()?,
        max_zoom: *levels.iter().max()?,
        tile_size,
    })
}

/// Drop HTML tags and collapse whitespace: service descriptions are often
/// HTML fragments.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A service chosen for adding, as a tile source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSource {
    pub url_template: String,
    pub min_zoom: u32,
    pub max_zoom: u32,
    /// Width of each tile image in pixels.
    pub tile_size: u32,
}

/// The tile source for `service`, showing the MapServer `layers` given (or
/// its default layers when `layers` is `None` or equal to the default set).
/// The tile cache is used whenever it can show what was asked for.
pub fn tile_source(
    service: &ServiceRef,
    info: &ServiceInfo,
    layers: Option<&BTreeSet<u32>>,
    token: Option<&str>,
) -> TileSource {
    let custom_layers = match (service.kind, layers) {
        (ServiceKind::MapServer, Some(l)) if *l != info.default_layers() => Some(l),
        _ => None,
    };
    let token_param = token.map(|t| format!("token={t}"));
    if let Some(cache) = info
        .cache
        .filter(|_| custom_layers.is_none() || !info.dynamic)
    {
        let mut url_template = format!("{}/tile/{{z}}/{{y}}/{{x}}", service.url);
        if let Some(t) = &token_param {
            url_template.push('?');
            url_template.push_str(t);
        }
        return TileSource {
            url_template,
            min_zoom: cache.min_zoom,
            max_zoom: cache.max_zoom,
            tile_size: cache.tile_size,
        };
    }
    let size = EXPORT_TILE_SIZE;
    let mut url_template = match service.kind {
        ServiceKind::MapServer => format!(
            "{}/export?bbox={BBOX_3857_PLACEHOLDER}&bboxSR=3857&imageSR=3857&size={size},{size}&dpi={EXPORT_DPI}&format=png32&transparent=true",
            service.url
        ),
        ServiceKind::ImageServer => format!(
            "{}/exportImage?bbox={BBOX_3857_PLACEHOLDER}&bboxSR=3857&imageSR=3857&size={size},{size}&format=jpgpng",
            service.url
        ),
    };
    if let Some(layers) = custom_layers {
        let ids: Vec<String> = layers.iter().map(u32::to_string).collect();
        url_template.push_str("&layers=show:");
        url_template.push_str(&ids.join(","));
    }
    url_template.push_str("&f=image");
    if let Some(t) = &token_param {
        url_template.push('&');
        url_template.push_str(t);
    }
    TileSource {
        url_template,
        min_zoom: DYNAMIC_MIN_ZOOM,
        max_zoom: DYNAMIC_MAX_ZOOM,
        tile_size: size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "https://gis.example.com/arcgis/rest/services";

    #[test]
    fn parses_a_catalogue_root() {
        let p = parse_url("https://gis.example.com/arcgis/rest/services/?f=pjson").unwrap();
        assert_eq!(
            p.location,
            Location::Directory {
                root: ROOT.into(),
                folder: None
            }
        );
        assert_eq!(p.token, None);
        assert_eq!(p.location.parent(), None);
    }

    #[test]
    fn parses_a_folder_and_its_parent() {
        let p = parse_url(&format!("{ROOT}/Imagery/Historic")).unwrap();
        assert_eq!(p.location.url(), format!("{ROOT}/Imagery/Historic"));
        assert_eq!(
            p.location.parent(),
            Some(Location::Directory {
                root: ROOT.into(),
                folder: Some("Imagery".into())
            })
        );
    }

    #[test]
    fn parses_a_service_layer_url_with_a_token() {
        let p = parse_url(&format!(
            "{ROOT}/Imagery/Ortho2020/mapserver/3?f=json&token=abc"
        ))
        .unwrap();
        let Location::Service { service, layer, .. } = &p.location else {
            panic!("expected a service: {:?}", p.location);
        };
        assert_eq!(service.url, format!("{ROOT}/Imagery/Ortho2020/MapServer"));
        assert_eq!(service.path, "Imagery/Ortho2020");
        assert_eq!(service.short_name(), "Ortho2020");
        assert_eq!(service.kind, ServiceKind::MapServer);
        assert_eq!(*layer, Some(3));
        assert_eq!(p.token.as_deref(), Some("abc"));
        assert_eq!(
            p.location.parent(),
            Some(Location::Directory {
                root: ROOT.into(),
                folder: Some("Imagery".into())
            })
        );
    }

    #[test]
    fn parses_a_service_url_without_a_catalogue_root() {
        let p = parse_url("https://tiles.example.com/proxy/Ortho/ImageServer").unwrap();
        let Location::Service { service, root, .. } = &p.location else {
            panic!();
        };
        assert_eq!(
            service.url,
            "https://tiles.example.com/proxy/Ortho/ImageServer"
        );
        assert_eq!(service.kind, ServiceKind::ImageServer);
        assert_eq!(*root, None);
        assert_eq!(p.location.parent(), None);
    }

    #[test]
    fn rejects_other_urls() {
        assert_eq!(
            parse_url("gis.example.com/rest/services"),
            Err(EsriError::NotArcGisUrl)
        );
        assert_eq!(
            parse_url("https://example.com/tiles/{z}/{x}/{y}.png"),
            Err(EsriError::NotArcGisUrl)
        );
        assert_eq!(
            parse_url(&format!("{ROOT}/MapServer")),
            Err(EsriError::NotArcGisUrl)
        );
    }

    #[test]
    fn directory_keeps_image_services_and_full_folder_paths() {
        let v = serde_json::json!({
            "currentVersion": 10.91,
            "folders": ["Sub"],
            "services": [
                {"name": "Imagery/Ortho", "type": "MapServer"},
                {"name": "Imagery/Parcels", "type": "FeatureServer"},
                {"name": "Imagery/Elevation", "type": "ImageServer"}
            ]
        });
        let d = parse_directory(&v, ROOT, Some("Imagery"));
        assert_eq!(d.folders, vec!["Imagery/Sub".to_string()]);
        let names: Vec<&str> = d.services.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(names, vec!["Imagery/Elevation", "Imagery/Ortho"]);
        assert_eq!(d.services[1].url, format!("{ROOT}/Imagery/Ortho/MapServer"));
    }

    #[test]
    fn an_in_band_error_is_an_error() {
        let e =
            parse_json_reply(r#"{"error":{"code":499,"message":"Token Required","details":[]}}"#);
        assert_eq!(
            e,
            Err(EsriError::Server {
                code: Some(499),
                message: "Token Required".into()
            })
        );
        assert!(matches!(
            parse_json_reply("<html>"),
            Err(EsriError::BadResponse(_))
        ));
    }

    fn web_mercator_tile_info(levels: std::ops::RangeInclusive<u32>) -> serde_json::Value {
        tile_info_of_size(levels, 256)
    }

    /// Esri's scheme for `size`-pixel tiles: level L's tiles cover the cells
    /// of standard zoom L, so 512-pixel levels have half the resolution
    /// number (level 0 at 78271.517 m/px).
    fn tile_info_of_size(levels: std::ops::RangeInclusive<u32>, size: u32) -> serde_json::Value {
        let factor = 256.0 / f64::from(size);
        let lods: Vec<serde_json::Value> = levels
            .map(|l| {
                serde_json::json!({
                    "level": l,
                    "resolution": ZOOM0_RESOLUTION * factor / 2f64.powi(l as i32),
                    "scale": 591657527.591555 * factor / 2f64.powi(l as i32)
                })
            })
            .collect();
        serde_json::json!({
            "rows": size, "cols": size, "dpi": 96,
            "origin": {"x": -20037508.342787, "y": 20037508.342787},
            "spatialReference": {"wkid": 102100, "latestWkid": 3857},
            "lods": lods
        })
    }

    fn sample_map_service(cached: bool) -> serde_json::Value {
        serde_json::json!({
            "mapName": "Layers",
            "serviceDescription": "<p>Leaf-off <b>2020</b> orthophotos&nbsp;</p>",
            "copyrightText": "Example County GIS",
            "capabilities": "Map,Query,Data",
            "singleFusedMapCache": cached,
            "layers": [
                {"id": 0, "name": "Roads", "parentLayerId": -1, "defaultVisibility": true, "subLayerIds": null},
                {"id": 1, "name": "Boundaries", "parentLayerId": -1, "defaultVisibility": true, "subLayerIds": [2, 3]},
                {"id": 2, "name": "City", "parentLayerId": 1, "defaultVisibility": true, "subLayerIds": null},
                {"id": 3, "name": "County", "parentLayerId": 1, "defaultVisibility": false, "subLayerIds": null}
            ],
            "tileInfo": web_mercator_tile_info(0..=19)
        })
    }

    #[test]
    fn reads_a_map_service() {
        let info = parse_service(&sample_map_service(true), ServiceKind::MapServer);
        assert_eq!(
            info.title, None,
            "the default 'Layers' map name is no title"
        );
        assert_eq!(
            info.description.as_deref(),
            Some("Leaf-off 2020 orthophotos")
        );
        assert_eq!(info.copyright.as_deref(), Some("Example County GIS"));
        assert_eq!(info.layers.len(), 4);
        assert!(info.layers[1].is_group);
        assert_eq!(info.depth(&info.layers[2]), 1);
        assert_eq!(info.default_layers(), BTreeSet::from([0, 1, 2]));
        assert_eq!(
            info.cache,
            Some(WebMercatorCache {
                min_zoom: 0,
                max_zoom: 19,
                tile_size: 256
            })
        );
        assert!(info.dynamic);
    }

    #[test]
    fn a_512_pixel_cache_on_the_standard_grid_is_used_as_high_resolution_tiles() {
        let mut v = sample_map_service(true);
        v["tileInfo"] = tile_info_of_size(0..=18, 512);
        let info = parse_service(&v, ServiceKind::MapServer);
        assert_eq!(
            info.cache,
            Some(WebMercatorCache {
                min_zoom: 0,
                max_zoom: 18,
                tile_size: 512
            })
        );
        let src = tile_source(&service(ServiceKind::MapServer), &info, None, None);
        assert!(src.url_template.ends_with("/tile/{z}/{y}/{x}"));
        assert_eq!(src.tile_size, 512);
    }

    #[test]
    fn a_cache_off_the_web_mercator_grid_is_not_used() {
        let mut v = sample_map_service(true);
        v["tileInfo"]["spatialReference"] = serde_json::json!({"wkid": 2263});
        assert_eq!(parse_service(&v, ServiceKind::MapServer).cache, None);

        // 512-pixel tiles at the 256 grid's resolutions: each tile would
        // cover four cells.
        let mut v = sample_map_service(true);
        v["tileInfo"]["rows"] = serde_json::json!(512);
        v["tileInfo"]["cols"] = serde_json::json!(512);
        assert_eq!(parse_service(&v, ServiceKind::MapServer).cache, None);

        let mut v = sample_map_service(true);
        v["tileInfo"] = tile_info_of_size(0..=10, 1024);
        assert_eq!(parse_service(&v, ServiceKind::MapServer).cache, None);

        // Levels renumbered from 0 at a non-standard resolution.
        let mut v = sample_map_service(true);
        v["tileInfo"]["lods"][0]["resolution"] = serde_json::json!(1000.0);
        assert_eq!(parse_service(&v, ServiceKind::MapServer).cache, None);

        // Not cached at all.
        assert_eq!(
            parse_service(&sample_map_service(false), ServiceKind::MapServer).cache,
            None
        );
    }

    fn service(kind: ServiceKind) -> ServiceRef {
        ServiceRef {
            url: format!("{ROOT}/Ortho/{}", kind.as_str()),
            path: "Ortho".into(),
            kind,
        }
    }

    #[test]
    fn a_cached_service_with_default_layers_uses_its_tiles() {
        let info = parse_service(&sample_map_service(true), ServiceKind::MapServer);
        let src = tile_source(&service(ServiceKind::MapServer), &info, None, None);
        assert_eq!(
            src.url_template,
            format!("{ROOT}/Ortho/MapServer/tile/{{z}}/{{y}}/{{x}}")
        );
        assert_eq!((src.min_zoom, src.max_zoom), (0, 19));
        assert_eq!(src.tile_size, 256);
        // Naming exactly the default layers is the same thing.
        let src2 = tile_source(
            &service(ServiceKind::MapServer),
            &info,
            Some(&info.default_layers()),
            Some("tok"),
        );
        assert_eq!(
            src2.url_template,
            format!("{ROOT}/Ortho/MapServer/tile/{{z}}/{{y}}/{{x}}?token=tok")
        );
    }

    #[test]
    fn a_layer_subset_is_exported() {
        let info = parse_service(&sample_map_service(true), ServiceKind::MapServer);
        let src = tile_source(
            &service(ServiceKind::MapServer),
            &info,
            Some(&BTreeSet::from([0, 3])),
            Some("tok"),
        );
        assert_eq!(
            src.url_template,
            format!(
                "{ROOT}/Ortho/MapServer/export?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=512,512&dpi=192&format=png32&transparent=true&layers=show:0,3&f=image&token=tok"
            )
        );
        assert_eq!(
            (src.min_zoom, src.max_zoom),
            (DYNAMIC_MIN_ZOOM, DYNAMIC_MAX_ZOOM)
        );
        assert_eq!(src.tile_size, EXPORT_TILE_SIZE);
    }

    #[test]
    fn a_tiles_only_service_keeps_its_tiles_whatever_layers_are_asked_for() {
        let mut v = sample_map_service(true);
        v["capabilities"] = serde_json::json!("Map,TilesOnly");
        let info = parse_service(&v, ServiceKind::MapServer);
        assert!(!info.dynamic);
        let src = tile_source(
            &service(ServiceKind::MapServer),
            &info,
            Some(&BTreeSet::from([0])),
            None,
        );
        assert!(src.url_template.ends_with("/tile/{z}/{y}/{x}"));
    }

    #[test]
    fn an_uncached_image_service_uses_export_image() {
        let v = serde_json::json!({"name": "Ortho", "capabilities": "Image,Metadata"});
        let info = parse_service(&v, ServiceKind::ImageServer);
        assert_eq!(info.title.as_deref(), Some("Ortho"));
        let src = tile_source(&service(ServiceKind::ImageServer), &info, None, None);
        assert_eq!(
            src.url_template,
            format!(
                "{ROOT}/Ortho/ImageServer/exportImage?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=512,512&format=jpgpng&f=image"
            )
        );
        let url = crate::tiles::url_from_template(
            &src.url_template,
            &crate::tiles::TileCoord::new(0, 0, 0),
        );
        assert!(
            url.contains("bbox=-20037508.340,-20037508.340,20037508.340,20037508.340&"),
            "{url}"
        );
    }

    #[test]
    fn fetches_a_directory_and_a_service_through_the_client() {
        use crate::http::fake::{ok, FakeClient};
        let client = FakeClient::new(vec![
            ok(
                200,
                r#"{"folders":["A"],"services":[{"name":"S","type":"ImageServer"}]}"#,
            ),
            ok(200, r#"{"error":{"code":403,"message":"Forbidden"}}"#),
        ]);
        let d = fetch_directory(&client, ROOT, None, None).unwrap();
        assert_eq!(d.folders, vec!["A".to_string()]);
        assert_eq!(d.services[0].url, format!("{ROOT}/S/ImageServer"));
        let e = fetch_service(&client, &d.services[0], None).unwrap_err();
        assert_eq!(e.to_string(), "Forbidden (403)");
    }
}
