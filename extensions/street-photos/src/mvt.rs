//! Point features from a Mapbox vector tile (Mapillary's and Panoramax's
//! photo tiles).

use std::collections::HashMap;

use mvt_reader::Reader;

pub use mvt_reader::feature::Value;

use crate::Tile;

/// A point of a tile: latitude, longitude and its properties.
pub type Point = (f64, f64, HashMap<String, Value>);

/// The point features of `layer` in tile `t`'s `bytes`.
pub fn points(bytes: &[u8], layer: &str, t: Tile) -> Result<Vec<Point>, String> {
    // An empty body is a tile with nothing in it.
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("tile: {e}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("tile: {e}"))?;
    let Some(meta) = layers.iter().find(|l| l.name == layer) else {
        return Ok(Vec::new());
    };
    let features = reader
        .get_features(meta.layer_index)
        .map_err(|e| format!("tile: {e}"))?;
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        // The reader returns even a single point as a MultiPoint.
        let pts: Vec<geo_types::Point<f32>> = match f.geometry {
            geo_types::Geometry::Point(p) => vec![p],
            geo_types::Geometry::MultiPoint(mp) => mp.0,
            _ => continue,
        };
        let props = f.properties.unwrap_or_default();
        for p in pts {
            let (lat, lon) = t.point(meta.extent, p.x(), p.y());
            out.push((lat, lon, props.clone()));
        }
    }
    Ok(out)
}

/// A property as a number.
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(f64::from(*f)),
        Value::Double(d) => Some(*d),
        Value::Int(i) | Value::SInt(i) => Some(*i as f64),
        Value::UInt(u) => Some(*u as f64),
        _ => None,
    }
}

/// A property as text (numbers are written out).
pub fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Int(i) | Value::SInt(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Panoramax tile (zoom 15) over south Minneapolis.
    const PANORAMAX_TILE: &[u8] = include_bytes!("../fixtures/panoramax_15_7898_11796.mvt");

    #[test]
    fn a_real_tile_has_its_points_inside_it() {
        let t = Tile {
            z: 15,
            x: 7898,
            y: 11796,
        };
        let pts = points(PANORAMAX_TILE, "pictures", t).unwrap();
        assert!(!pts.is_empty(), "the fixture has pictures");
        let b = t.bounds();
        for (lat, lon, props) in &pts {
            assert!(*lat >= b.min_lat - 1e-4 && *lat <= b.max_lat + 1e-4);
            assert!(*lon >= b.min_lon - 1e-4 && *lon <= b.max_lon + 1e-4);
            assert!(props.get("id").and_then(text).is_some());
        }
        assert!(points(PANORAMAX_TILE, "nope", t).unwrap().is_empty());
        assert!(points(b"", "pictures", t).unwrap().is_empty());
    }
}
