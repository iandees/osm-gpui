//! GeoJSON reader (RFC 7946): `FeatureCollection`, `Feature` and bare
//! geometries of every type become reference features. Coordinates are WGS84
//! `[longitude, latitude]`; a third value (elevation) is ignored, and a file
//! whose numbers are not valid longitudes and latitudes is refused rather than
//! drawn in the wrong place.
//!
//! A feature's `properties` are kept as text key/value pairs. The value of
//! `name` (or, failing that, `title` or `label`) becomes the feature's name
//! and is not repeated in the list.

use serde_json::Value;

use crate::reference::{Builder, FeatureKind, Format, RefError, ReferenceData};

/// Property keys tried, in order, for a feature's name.
const NAME_KEYS: [&str; 5] = ["name", "Name", "NAME", "title", "label"];

fn malformed(detail: impl Into<String>) -> RefError {
    RefError::Malformed(detail.into())
}

/// Parse a GeoJSON document.
pub fn parse(bytes: &[u8]) -> Result<ReferenceData, RefError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root: Value = serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    let mut builder = Builder::new(Format::GeoJson);
    read_object(&mut builder, &root)?;
    builder.finish()
}

/// What a feature's properties yielded.
struct Props {
    name: Option<String>,
    list: Vec<(String, String)>,
}

fn read_properties(value: Option<&Value>) -> Props {
    let mut props = Props {
        name: None,
        list: Vec::new(),
    };
    let Some(Value::Object(map)) = value else {
        return props;
    };
    let name_key = NAME_KEYS
        .iter()
        .find(|k| matches!(map.get(**k), Some(Value::String(s)) if !s.trim().is_empty()))
        .copied();
    for (key, value) in map {
        let text = match value {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };
        if Some(key.as_str()) == name_key {
            props.name = Some(text);
        } else {
            props.list.push((key.clone(), text));
        }
    }
    props
}

/// A top-level object or a member of a collection: a `FeatureCollection`, a
/// `Feature` or a geometry.
fn read_object(builder: &mut Builder, value: &Value) -> Result<(), RefError> {
    let Value::Object(map) = value else {
        return Err(malformed("expected a GeoJSON object"));
    };
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("missing \"type\""))?;
    match kind {
        "FeatureCollection" => {
            let features = map
                .get("features")
                .and_then(Value::as_array)
                .ok_or_else(|| malformed("a FeatureCollection without \"features\""))?;
            for f in features {
                read_object(builder, f)?;
            }
            Ok(())
        }
        "Feature" => {
            let p = read_properties(map.get("properties"));
            let geometry = match map.get("geometry") {
                None | Some(Value::Null) => return Ok(()),
                Some(g) => g,
            };
            let mut feature = None;
            let ctx = FeatureCtx {
                name: p.name,
                props: p.list,
            };
            read_geometry(builder, geometry, &ctx, &mut feature)
        }
        _ => {
            // A bare geometry.
            let ctx = FeatureCtx {
                name: None,
                props: Vec::new(),
            };
            let mut feature = None;
            read_geometry(builder, value, &ctx, &mut feature)
        }
    }
}

/// The name and properties a geometry inherits from its `Feature`.
struct FeatureCtx {
    name: Option<String>,
    props: Vec<(String, String)>,
}

/// Add `geometry`. All parts of one `Feature` (a `Multi*` or a collection)
/// share one reference feature, created when the first part is added.
fn read_geometry(
    builder: &mut Builder,
    geometry: &Value,
    ctx: &FeatureCtx,
    feature: &mut Option<u32>,
) -> Result<(), RefError> {
    let Value::Object(map) = geometry else {
        return Err(malformed("a geometry must be an object"));
    };
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("a geometry without \"type\""))?;
    let label: &'static str = match kind {
        "Point" => "Point",
        "MultiPoint" => "MultiPoint",
        "LineString" => "LineString",
        "MultiLineString" => "MultiLineString",
        "Polygon" => "Polygon",
        "MultiPolygon" => "MultiPolygon",
        "GeometryCollection" => "GeometryCollection",
        other => return Err(malformed(format!("unsupported geometry type \"{other}\""))),
    };
    if kind == "GeometryCollection" {
        let parts = map
            .get("geometries")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("a GeometryCollection without \"geometries\""))?;
        feature.get_or_insert_with(|| {
            builder.feature(FeatureKind::Geo, label, ctx.name.clone(), ctx.props.clone())
        });
        for part in parts {
            read_geometry(builder, part, ctx, feature)?;
        }
        return Ok(());
    }
    let mut id = |builder: &mut Builder| -> u32 {
        *feature.get_or_insert_with(|| {
            builder.feature(FeatureKind::Geo, label, ctx.name.clone(), ctx.props.clone())
        })
    };
    let coords = map
        .get("coordinates")
        .ok_or_else(|| malformed(format!("a {kind} without \"coordinates\"")))?;
    match kind {
        "Point" => {
            let (lon, lat) = position(coords)?;
            let f = id(builder);
            builder.point(f, lon, lat)
        }
        "MultiPoint" => {
            for p in array(coords)? {
                let (lon, lat) = position(p)?;
                let f = id(builder);
                builder.point(f, lon, lat)?;
            }
            Ok(())
        }
        "LineString" => {
            let line = positions(coords)?;
            let f = id(builder);
            builder.line(f, &line)
        }
        "MultiLineString" | "Polygon" => {
            for part in array(coords)? {
                let line = positions(part)?;
                let f = id(builder);
                builder.line(f, &line)?;
            }
            Ok(())
        }
        "MultiPolygon" => {
            for polygon in array(coords)? {
                for ring in array(polygon)? {
                    let line = positions(ring)?;
                    let f = id(builder);
                    builder.line(f, &line)?;
                }
            }
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn array(value: &Value) -> Result<&Vec<Value>, RefError> {
    value
        .as_array()
        .ok_or_else(|| malformed("coordinates must be arrays"))
}

/// One `[lon, lat]` or `[lon, lat, elevation]` position.
fn position(value: &Value) -> Result<(f64, f64), RefError> {
    let a = array(value)?;
    match (
        a.first().and_then(Value::as_f64),
        a.get(1).and_then(Value::as_f64),
    ) {
        (Some(lon), Some(lat)) => Ok((lon, lat)),
        _ => Err(malformed("a position needs two numbers")),
    }
}

fn positions(value: &Value) -> Result<Vec<(f64, f64)>, RefError> {
    array(value)?.iter().map(position).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(doc: &str) -> ReferenceData {
        parse(doc.as_bytes()).unwrap()
    }

    #[test]
    fn point() {
        let d = ok(r#"{"type":"Point","coordinates":[10.5,50.25]}"#);
        assert_eq!(d.markers.len(), 1);
        assert_eq!(d.features[0].geometry, "Point");
        assert_eq!(d.extent(), Some((50.25, 50.25, 10.5, 10.5)));
    }

    #[test]
    fn multipoint() {
        let d = ok(r#"{"type":"MultiPoint","coordinates":[[1,2],[3,4],[5,6,100]]}"#);
        assert_eq!(d.markers.len(), 3);
        assert_eq!(d.features.len(), 1);
    }

    #[test]
    fn linestring_with_elevation() {
        let d = ok(r#"{"type":"LineString","coordinates":[[1,2,10],[3,4,20]]}"#);
        assert_eq!((d.parts.len(), d.vertex_count), (1, 2));
    }

    #[test]
    fn multilinestring() {
        let d = ok(r#"{"type":"MultiLineString","coordinates":[[[1,2],[3,4]],[[5,6],[7,8]]]}"#);
        assert_eq!((d.parts.len(), d.features.len()), (2, 1));
    }

    #[test]
    fn polygon_rings_are_outlines() {
        let d = ok(r#"{"type":"Polygon","coordinates":[
            [[0,0],[4,0],[4,4],[0,4],[0,0]],
            [[1,1],[2,1],[2,2],[1,1]]]}"#);
        assert_eq!(d.parts.len(), 2);
        assert_eq!(d.parts[0].pts.first(), d.parts[0].pts.last());
    }

    #[test]
    fn multipolygon() {
        let d = ok(r#"{"type":"MultiPolygon","coordinates":[
            [[[0,0],[1,0],[1,1],[0,0]]],
            [[[5,5],[6,5],[6,6],[5,5]], [[5.2,5.1],[5.5,5.1],[5.5,5.4],[5.2,5.1]]]]}"#);
        assert_eq!((d.parts.len(), d.features.len()), (3, 1));
    }

    #[test]
    fn geometry_collection() {
        let d = ok(r#"{"type":"GeometryCollection","geometries":[
            {"type":"Point","coordinates":[1,1]},
            {"type":"LineString","coordinates":[[2,2],[3,3]]},
            {"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[4,4]}]}]}"#);
        assert_eq!(
            (d.markers.len(), d.parts.len(), d.features.len()),
            (2, 1, 1)
        );
        assert_eq!(d.features[0].geometry, "GeometryCollection");
    }

    #[test]
    fn feature_collection_keeps_properties_and_names() {
        let d = ok(r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"Cafe","amenity":"cafe","seats":12,"open":true,"x":null,"tags":{"a":1}},
             "geometry":{"type":"Point","coordinates":[1,2]}},
            {"type":"Feature","properties":null,"geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]}},
            {"type":"Feature","properties":{"title":"T"},"geometry":null}]}"#);
        assert_eq!(d.features.len(), 2);
        let cafe = &d.features[0];
        assert_eq!(cafe.name.as_deref(), Some("Cafe"));
        assert_eq!(
            cafe.props,
            vec![
                ("amenity".to_string(), "cafe".to_string()),
                ("seats".to_string(), "12".to_string()),
                ("open".to_string(), "true".to_string()),
                ("tags".to_string(), "{\"a\":1}".to_string()),
            ]
        );
        assert_eq!(d.features[1].name, None);
    }

    #[test]
    fn bare_feature() {
        let d = ok(
            r#"{"type":"Feature","properties":{"label":"L"},"geometry":{"type":"Point","coordinates":[1,2]}}"#,
        );
        assert_eq!(d.features[0].name.as_deref(), Some("L"));
    }

    #[test]
    fn malformed_json_is_an_error() {
        for doc in ["{", "", "[1,2", "not json"] {
            assert!(
                matches!(parse(doc.as_bytes()), Err(RefError::Malformed(_))),
                "{doc:?}"
            );
        }
    }

    #[test]
    fn well_formed_but_wrong_documents_are_errors() {
        for doc in [
            "[]",
            "42",
            r#"{}"#,
            r#"{"type":"Banana"}"#,
            r#"{"type":"Point"}"#,
            r#"{"type":"Point","coordinates":[1]}"#,
            r#"{"type":"Point","coordinates":["a","b"]}"#,
            r#"{"type":"LineString","coordinates":[[1,2],"x"]}"#,
            r#"{"type":"FeatureCollection"}"#,
        ] {
            assert!(
                matches!(parse(doc.as_bytes()), Err(RefError::Malformed(_))),
                "{doc:?}"
            );
        }
    }

    #[test]
    fn empty_collections_have_no_geometry() {
        for doc in [
            r#"{"type":"FeatureCollection","features":[]}"#,
            r#"{"type":"Feature","geometry":null,"properties":{}}"#,
            r#"{"type":"LineString","coordinates":[]}"#,
            r#"{"type":"GeometryCollection","geometries":[]}"#,
        ] {
            assert_eq!(
                parse(doc.as_bytes()).unwrap_err(),
                RefError::NoGeometry,
                "{doc}"
            );
        }
    }

    #[test]
    fn projected_coordinates_are_refused() {
        let doc = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},
            "geometry":{"type":"LineString","coordinates":[[500000.0,4649776.2],[500100.0,4649900.0]]}}]}"#;
        match parse(doc.as_bytes()).unwrap_err() {
            RefError::NotGeographic { x, y } => {
                assert_eq!((x, y), (500000.0, 4649776.2));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn latitude_out_of_range_is_refused() {
        // Swapped lat/lon puts "latitude" beyond 90.
        let err = parse(br#"{"type":"Point","coordinates":[40.0,-100.0]}"#).unwrap_err();
        assert!(matches!(err, RefError::NotGeographic { .. }), "{err:?}");
    }
}
