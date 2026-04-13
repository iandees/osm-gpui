//! Editor Layer Index (ELI) support.
//!
//! Downloads, caches, and parses <https://osmlab.github.io/editor-layer-index/imagery.geojson>
//! into a list of `ImageryEntry` values that can be filtered by viewport
//! location for populating the Imagery menu. Only `tms` type entries are
//! supported — WMS, Bing, and other types are filtered out.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const ELI_URL: &str = "https://osmlab.github.io/editor-layer-index/imagery.geojson";
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days
const MAX_MENU_ENTRIES: usize = 30;

/// Axis-aligned latitude/longitude bounding box (inclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lat >= self.min_lat
            && lat <= self.max_lat
            && lon >= self.min_lon
            && lon <= self.max_lon
    }
}

/// A single imagery source from the Editor Layer Index.
#[derive(Debug, Clone)]
pub struct ImageryEntry {
    pub id: String,
    pub name: String,
    pub url_template: String,
    pub min_zoom: Option<u32>,
    pub max_zoom: Option<u32>,
    /// Global bounding box (axis-aligned) if the entry has a geometry.
    pub bbox: Option<GeoBounds>,
    /// Polygon rings (lon, lat) for precise containment tests. Only the outer
    /// ring of each polygon is retained (holes are not honored).
    pub polygon: Option<Vec<Vec<(f64, f64)>>>,
    pub best: bool,
    pub country_code: Option<String>,
    pub icon_url: Option<String>,
}

impl ImageryEntry {
    /// Does this entry's geometry cover the given point? An entry with no
    /// geometry is considered global and matches all points.
    pub fn covers(&self, lat: f64, lon: f64) -> bool {
        let Some(bbox) = self.bbox else {
            return true; // no geometry == global
        };
        if !bbox.contains(lat, lon) {
            return false;
        }
        // If we have polygon rings, do a more precise test. If any ring
        // contains the point, it's a match.
        if let Some(polygons) = &self.polygon {
            return polygons.iter().any(|ring| point_in_ring(lon, lat, ring));
        }
        true
    }
}

/// Fetch the ELI GeoJSON (using an on-disk cache with a 7-day TTL).
/// On network failure, falls back to any existing cache file.
pub fn fetch_and_cache() -> anyhow::Result<String> {
    let cache_path = cache_file_path();
    if let Some(body) = read_fresh_cache(&cache_path) {
        return Ok(body);
    }

    match download() {
        Ok(body) => {
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cache_path, &body);
            Ok(body)
        }
        Err(e) => {
            // Fallback: any existing cache, regardless of age.
            if let Ok(body) = fs::read_to_string(&cache_path) {
                eprintln!("imagery: ELI fetch failed ({}), using stale cache", e);
                Ok(body)
            } else {
                Err(e)
            }
        }
    }
}

fn cache_file_path() -> PathBuf {
    std::env::temp_dir()
        .join("osm-gpui-imagery-index")
        .join("imagery.geojson")
}

fn read_fresh_cache(path: &PathBuf) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age > CACHE_TTL {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn download() -> anyhow::Result<String> {
    let response = ureq::get(ELI_URL)
        .set(
            "User-Agent",
            "osm-gpui/0.1.0 (https://github.com/iandees/osm-gpui)",
        )
        .timeout(Duration::from_secs(30))
        .call()?;

    let mut body = String::new();
    response.into_reader().read_to_string(&mut body)?;
    Ok(body)
}

/// Parse the ELI GeoJSON body into a list of `tms`-type imagery entries.
/// Entries with `overlay == true` are filtered out.
pub fn parse(geojson_body: &str) -> Vec<ImageryEntry> {
    let root: serde_json::Value = match serde_json::from_str(geojson_body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("imagery: failed to parse ELI GeoJSON: {}", e);
            return Vec::new();
        }
    };

    let Some(features) = root.get("features").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(features.len() / 2);
    for feature in features {
        if let Some(entry) = parse_feature(feature) {
            out.push(entry);
        }
    }
    out
}

fn parse_feature(feature: &serde_json::Value) -> Option<ImageryEntry> {
    let props = feature.get("properties")?;
    let typ = props.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if typ != "tms" {
        return None;
    }
    if props.get("overlay").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let url_template = props.get("url").and_then(|v| v.as_str())?.to_string();
    let name = props
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed imagery")
        .to_string();
    let id = props
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.clone());
    let min_zoom = props
        .get("min_zoom")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let max_zoom = props
        .get("max_zoom")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let best = props.get("best").and_then(|v| v.as_bool()).unwrap_or(false);
    let country_code = props
        .get("country_code")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let icon_url = props
        .get("icon")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let (bbox, polygon) = match feature.get("geometry") {
        Some(g) if !g.is_null() => parse_geometry(g),
        _ => (None, None),
    };

    Some(ImageryEntry {
        id,
        name,
        url_template,
        min_zoom,
        max_zoom,
        bbox,
        polygon,
        best,
        country_code,
        icon_url,
    })
}

fn parse_geometry(geom: &serde_json::Value) -> (Option<GeoBounds>, Option<Vec<Vec<(f64, f64)>>>) {
    let typ = geom.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let coords = match geom.get("coordinates") {
        Some(c) => c,
        None => return (None, None),
    };

    let rings: Vec<Vec<(f64, f64)>> = match typ {
        "Polygon" => {
            let Some(arr) = coords.as_array() else {
                return (None, None);
            };
            arr.iter().take(1).filter_map(parse_ring).collect()
        }
        "MultiPolygon" => {
            let Some(arr) = coords.as_array() else {
                return (None, None);
            };
            arr.iter()
                .filter_map(|poly| poly.as_array().and_then(|rings| rings.first()))
                .filter_map(parse_ring)
                .collect()
        }
        _ => return (None, None),
    };

    if rings.is_empty() {
        return (None, None);
    }

    // Compute axis-aligned bbox across all rings.
    let mut min_lat = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    for ring in &rings {
        for &(lon, lat) in ring {
            if lat < min_lat {
                min_lat = lat;
            }
            if lat > max_lat {
                max_lat = lat;
            }
            if lon < min_lon {
                min_lon = lon;
            }
            if lon > max_lon {
                max_lon = lon;
            }
        }
    }
    if !min_lat.is_finite() {
        return (None, None);
    }

    (
        Some(GeoBounds {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        }),
        Some(rings),
    )
}

fn parse_ring(v: &serde_json::Value) -> Option<Vec<(f64, f64)>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for pair in arr {
        let p = pair.as_array()?;
        let lon = p.first()?.as_f64()?;
        let lat = p.get(1)?.as_f64()?;
        out.push((lon, lat));
    }
    if out.len() < 3 {
        return None;
    }
    Some(out)
}

/// Ray-casting point-in-polygon test. Ring points are (lon, lat).
fn point_in_ring(x: f64, y: f64, ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        let intersects = ((yi > y) != (yj > y))
            && (x < (xj - xi) * (y - yi) / (yj - yi + f64::EPSILON) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Return the subset of entries visible for the given viewport center, sorted
/// `best`-first and then alphabetically by name. Caller should treat the
/// list's order as display order. The list is capped to avoid an unusable menu.
pub fn entries_for_viewport(
    all: &[ImageryEntry],
    center_lat: f64,
    center_lon: f64,
) -> Vec<ImageryEntry> {
    let mut matched: Vec<ImageryEntry> = all
        .iter()
        .filter(|e| e.covers(center_lat, center_lon))
        .cloned()
        .collect();

    matched.sort_by(|a, b| {
        b.best
            .cmp(&a.best)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    matched.truncate(MAX_MENU_ENTRIES);
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_GEOJSON: &str = r#"{
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "properties": {
                    "id": "global_tms",
                    "name": "Global TMS",
                    "type": "tms",
                    "url": "https://example.com/{z}/{x}/{y}.png",
                    "best": true
                },
                "geometry": null
            },
            {
                "type": "Feature",
                "properties": {
                    "id": "wms_filtered",
                    "name": "WMS Filtered",
                    "type": "wms",
                    "url": "https://example.com/wms"
                },
                "geometry": null
            },
            {
                "type": "Feature",
                "properties": {
                    "id": "overlay_filtered",
                    "name": "Overlay Filtered",
                    "type": "tms",
                    "url": "https://example.com/overlay/{z}/{x}/{y}.png",
                    "overlay": true
                },
                "geometry": null
            },
            {
                "type": "Feature",
                "properties": {
                    "id": "poly_entry",
                    "name": "Polygon Entry",
                    "type": "tms",
                    "url": "https://example.com/poly/{z}/{x}/{y}.png"
                },
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[
                        [-10.0, 40.0],
                        [10.0, 40.0],
                        [10.0, 50.0],
                        [-10.0, 50.0],
                        [-10.0, 40.0]
                    ]]
                }
            },
            {
                "type": "Feature",
                "properties": {
                    "id": "multi_entry",
                    "name": "Multipolygon Entry",
                    "type": "tms",
                    "url": "https://example.com/multi/{z}/{x}/{y}.png"
                },
                "geometry": {
                    "type": "MultiPolygon",
                    "coordinates": [
                        [[
                            [100.0, 0.0],
                            [101.0, 0.0],
                            [101.0, 1.0],
                            [100.0, 1.0],
                            [100.0, 0.0]
                        ]]
                    ]
                }
            }
        ]
    }"#;

    #[test]
    fn parses_sample_geojson() {
        let entries = parse(SAMPLE_GEOJSON);
        let ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"global_tms"));
        assert!(ids.contains(&"poly_entry"));
        assert!(ids.contains(&"multi_entry"));
        assert!(!ids.contains(&"wms_filtered"), "wms should be filtered out");
        assert!(
            !ids.contains(&"overlay_filtered"),
            "overlay should be filtered out"
        );
        let global = entries.iter().find(|e| e.id == "global_tms").unwrap();
        assert!(global.bbox.is_none());
        assert!(global.best);
        let poly = entries.iter().find(|e| e.id == "poly_entry").unwrap();
        let bbox = poly.bbox.expect("polygon should have bbox");
        assert!((bbox.min_lat - 40.0).abs() < 1e-9);
        assert!((bbox.max_lat - 50.0).abs() < 1e-9);
    }

    #[test]
    fn bbox_contains_point() {
        let entries = parse(SAMPLE_GEOJSON);
        let poly = entries.iter().find(|e| e.id == "poly_entry").unwrap();
        assert!(poly.covers(45.0, 0.0));
        assert!(!poly.covers(45.0, 50.0));
        assert!(!poly.covers(0.0, 0.0));

        let global = entries.iter().find(|e| e.id == "global_tms").unwrap();
        assert!(global.covers(0.0, 0.0));
        assert!(global.covers(-80.0, 179.0));
    }

    #[test]
    fn entries_for_viewport_sorts_best_first() {
        let entries = parse(SAMPLE_GEOJSON);
        // At (45, 0) both the global and the polygon entry should match.
        let shown = entries_for_viewport(&entries, 45.0, 0.0);
        assert!(shown.len() >= 2);
        assert!(shown[0].best, "best entries come first");
    }
}
