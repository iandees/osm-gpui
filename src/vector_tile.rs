//! Decoding a Mapbox vector tile into what the map draws: lines, triangulated
//! polygon fills and points, all in Web Mercator metres so a frame only has
//! to project them.
//!
//! The tile carries no style, so each layer gets a colour from its name (water
//! blue, buildings brown, roads orange, ...), falling back to a stable colour
//! picked from the name for names nothing is known about.

use std::sync::Arc;

use mvt_reader::feature::Value;
use mvt_reader::Reader;

use crate::tiles::TileCoord;

/// A point in Web Mercator metres.
pub type Merc = [f64; 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomKind {
    Point,
    Line,
    Polygon,
}

/// One feature of a tile: what it is, its attributes and its geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    /// The name of the source layer it came from.
    pub layer: Arc<str>,
    pub color: u32,
    pub kind: GeomKind,
    /// The feature id, when the tile has one.
    pub id: Option<u64>,
    /// Attributes, sorted by key.
    pub props: Vec<(String, String)>,
    /// `[min_x, min_y, max_x, max_y]` of the geometry.
    pub bbox: [f64; 4],
    /// Triangles of a filled area: three vertices each.
    pub tris: Vec<Merc>,
    /// Lines, and the outlines (rings) of areas.
    pub lines: Vec<Vec<Merc>>,
    pub dots: Vec<Merc>,
}

impl Feature {
    fn vertex_count(&self) -> usize {
        self.tris.len() + self.lines.iter().map(Vec::len).sum::<usize>() + self.dots.len()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodedTile {
    pub features: Vec<Feature>,
}

impl DecodedTile {
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Vertices held, as a rough size for the tile cache.
    pub fn vertex_count(&self) -> usize {
        self.features.iter().map(Feature::vertex_count).sum()
    }
}

/// The colour (`0xRRGGBBAA`, opaque) a layer called `name` is drawn in.
pub fn layer_color(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["water", "ocean", "sea", "lake", "river"]) {
        0x3b82f6ff
    } else if has(&["building"]) {
        0x92400eff
    } else if has(&[
        "park",
        "forest",
        "wood",
        "landcover",
        "green",
        "natural",
        "grass",
    ]) {
        0x16a34aff
    } else if has(&["landuse", "land_use"]) {
        0x84cc16ff
    } else if has(&["road", "street", "transport", "highway", "path", "rail"]) {
        0xea580cff
    } else if has(&["boundar", "admin", "border"]) {
        0x9333eaff
    } else if has(&["poi", "place", "address"]) {
        0xdb2777ff
    } else {
        // FNV-1a over the name, spread over hues the way a colour wheel would.
        let mut h: u32 = 0x811c9dc5;
        for b in n.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x01000193);
        }
        hsv_to_rgba(h % 360, 0.65, 0.8)
    }
}

fn hsv_to_rgba(h: u32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = h as f32 / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |f: f32| ((f + m) * 255.0).round() as u32;
    byte(r) << 24 | byte(g) << 16 | byte(b) << 8 | 0xff
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Int(i) | Value::SInt(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
    }
}

/// Decode the MVT `bytes` of tile `t`. An empty body is an empty tile.
pub fn decode(bytes: &[u8], t: TileCoord) -> Result<DecodedTile, String> {
    let mut out = DecodedTile::default();
    if bytes.is_empty() {
        return Ok(out);
    }
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("tile: {e}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("tile: {e}"))?;
    let bounds = t.mercator_bounds();
    let size = bounds.max_x - bounds.min_x;
    for layer in layers {
        let color = layer_color(&layer.name);
        let name: Arc<str> = Arc::from(layer.name.as_str());
        let extent = f64::from(layer.extent.max(1));
        let to_merc = |p: geo_types::Coord<f32>| -> Merc {
            [
                bounds.min_x + f64::from(p.x) / extent * size,
                bounds.max_y - f64::from(p.y) / extent * size,
            ]
        };
        let features = reader
            .get_features(layer.layer_index)
            .map_err(|e| format!("tile: {e}"))?;
        for f in features {
            let mut props: Vec<(String, String)> = f
                .properties
                .unwrap_or_default()
                .iter()
                .filter_map(|(k, v)| value_text(v).map(|v| (k.clone(), v)))
                .collect();
            props.sort();
            let mut feature = Feature {
                layer: name.clone(),
                color,
                kind: GeomKind::Point,
                id: f.id,
                props,
                bbox: [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
                tris: Vec::new(),
                lines: Vec::new(),
                dots: Vec::new(),
            };
            add_geometry(&mut feature, f.geometry, &to_merc);
            let [x0, y0, x1, y1] = feature.bbox;
            if x0 <= x1 && y0 <= y1 {
                out.features.push(feature);
            }
        }
    }
    Ok(out)
}

fn widen(f: &mut Feature, p: Merc) {
    f.bbox[0] = f.bbox[0].min(p[0]);
    f.bbox[1] = f.bbox[1].min(p[1]);
    f.bbox[2] = f.bbox[2].max(p[0]);
    f.bbox[3] = f.bbox[3].max(p[1]);
}

fn add_geometry(
    out: &mut Feature,
    g: geo_types::Geometry<f32>,
    to_merc: &impl Fn(geo_types::Coord<f32>) -> Merc,
) {
    use geo_types::Geometry as G;
    match g {
        G::Point(p) => add_dot(out, to_merc(p.0)),
        G::MultiPoint(mp) => {
            for p in mp.0 {
                add_dot(out, to_merc(p.0));
            }
        }
        G::LineString(l) => add_line(out, &l, GeomKind::Line, to_merc),
        G::MultiLineString(ml) => {
            for l in &ml.0 {
                add_line(out, l, GeomKind::Line, to_merc);
            }
        }
        G::Polygon(p) => add_polygon(out, &p, to_merc),
        G::MultiPolygon(mp) => {
            for p in &mp.0 {
                add_polygon(out, p, to_merc);
            }
        }
        G::GeometryCollection(gc) => {
            for g in gc.0 {
                add_geometry(out, g, to_merc);
            }
        }
        _ => {}
    }
}

fn add_dot(out: &mut Feature, at: Merc) {
    out.kind = GeomKind::Point;
    widen(out, at);
    out.dots.push(at);
}

fn add_line(
    out: &mut Feature,
    l: &geo_types::LineString<f32>,
    kind: GeomKind,
    to_merc: &impl Fn(geo_types::Coord<f32>) -> Merc,
) {
    if l.0.len() < 2 {
        return;
    }
    // A feature that has an area stays an area when its rings are added.
    if out.kind != GeomKind::Polygon {
        out.kind = kind;
    }
    let pts: Vec<Merc> = l.0.iter().map(|&c| to_merc(c)).collect();
    for &p in &pts {
        widen(out, p);
    }
    out.lines.push(pts);
}

fn add_polygon(
    out: &mut Feature,
    p: &geo_types::Polygon<f32>,
    to_merc: &impl Fn(geo_types::Coord<f32>) -> Merc,
) {
    // Rings as earcut wants them: one flat list, holes marked by where they start.
    let mut flat: Vec<f64> = Vec::new();
    let mut holes: Vec<usize> = Vec::new();
    let mut ring = |l: &geo_types::LineString<f32>, hole: bool| {
        let mut pts: Vec<Merc> = l.0.iter().map(|&c| to_merc(c)).collect();
        // A ring repeats its first point at the end; earcut does not want it.
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 3 {
            return false;
        }
        if hole {
            holes.push(flat.len() / 2);
        }
        flat.extend(pts.iter().flatten());
        true
    };
    if !ring(p.exterior(), false) {
        return;
    }
    for h in p.interiors() {
        ring(h, true);
    }
    out.kind = GeomKind::Polygon;
    if let Ok(idx) = earcutr::earcut(&flat, &holes, 2) {
        out.tris
            .extend(idx.iter().map(|&i| [flat[2 * i], flat[2 * i + 1]]));
    }
    // The outline of the area, including its holes.
    add_line(out, p.exterior(), GeomKind::Polygon, to_merc);
    for h in p.interiors() {
        add_line(out, h, GeomKind::Polygon, to_merc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn zigzag(v: i32) -> u64 {
        ((v << 1) ^ (v >> 31)) as u64
    }

    /// A tile of one layer holding a square polygon, a line and a point.
    fn tile() -> Vec<u8> {
        let feature = |geom_type: u64, geometry: &[u64]| {
            let mut f = Vec::new();
            field(3, 0, &mut f);
            varint(geom_type, &mut f);
            let mut packed = Vec::new();
            for g in geometry {
                varint(*g, &mut packed);
            }
            bytes_field(4, &packed, &mut f);
            f
        };
        let cmd = |id: u64, count: u64| (count << 3) | id;
        let point = feature(1, &[cmd(1, 1), zigzag(2048), zigzag(2048)]);
        let line = feature(
            2,
            &[
                cmd(1, 1),
                zigzag(0),
                zigzag(0),
                cmd(2, 1),
                zigzag(100),
                zigzag(100),
            ],
        );
        // Clockwise on screen (positive area in tile space), as an exterior ring.
        let square = feature(
            3,
            &[
                cmd(1, 1),
                zigzag(1000),
                zigzag(1000),
                cmd(2, 3),
                zigzag(500),
                zigzag(0),
                zigzag(0),
                zigzag(500),
                zigzag(-500),
                zigzag(0),
                cmd(7, 1),
            ],
        );
        let mut layer = Vec::new();
        field(15, 0, &mut layer);
        varint(2, &mut layer);
        bytes_field(1, b"water", &mut layer);
        for f in [point, line, square] {
            bytes_field(2, &f, &mut layer);
        }
        field(5, 0, &mut layer);
        varint(4096, &mut layer);
        let mut tile = Vec::new();
        bytes_field(3, &layer, &mut tile);
        tile
    }

    /// A tile of one point feature with an id and two attributes.
    fn tile_with_tags() -> Vec<u8> {
        let mut f = Vec::new();
        field(1, 0, &mut f);
        varint(7, &mut f); // id
        let mut tags = Vec::new();
        for v in [0, 0, 1, 1] {
            varint(v, &mut tags);
        }
        bytes_field(2, &tags, &mut f); // tags: key0=value0, key1=value1
        field(3, 0, &mut f);
        varint(1, &mut f);
        let mut g = Vec::new();
        for v in [(1 << 3) | 1, zigzag(10), zigzag(10)] {
            varint(v, &mut g);
        }
        bytes_field(4, &g, &mut f);
        let mut layer = Vec::new();
        field(15, 0, &mut layer);
        varint(2, &mut layer);
        bytes_field(1, b"poi", &mut layer);
        bytes_field(2, &f, &mut layer);
        bytes_field(3, b"height", &mut layer);
        bytes_field(3, b"name", &mut layer);
        let mut v0 = Vec::new();
        field(3, 1, &mut v0); // double_value
        v0.extend_from_slice(&12.5f64.to_le_bytes());
        bytes_field(4, &v0, &mut layer);
        let mut v1 = Vec::new();
        bytes_field(1, b"Lake", &mut v1);
        bytes_field(4, &v1, &mut layer);
        field(5, 0, &mut layer);
        varint(4096, &mut layer);
        let mut tile = Vec::new();
        bytes_field(3, &layer, &mut tile);
        tile
    }

    #[test]
    fn a_tile_decodes_to_features_in_mercator_metres() {
        let t = TileCoord::new(0, 0, 0);
        let d = decode(&tile(), t).unwrap();
        assert_eq!(d.features.len(), 3);
        let kinds: Vec<_> = d.features.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [GeomKind::Point, GeomKind::Line, GeomKind::Polygon]);
        // The point is at the middle of the world tile.
        let dot = &d.features[0];
        assert!(dot.dots[0][0].abs() < 1.0 && dot.dots[0][1].abs() < 1.0);
        assert_eq!(dot.bbox[0], dot.bbox[2]);
        // A square is two triangles and one outline ring.
        let square = &d.features[2];
        assert_eq!(square.tris.len(), 6);
        assert_eq!(square.lines.len(), 1);
        assert!(square.bbox[0] < square.bbox[2] && square.bbox[1] < square.bbox[3]);
        assert!(d.features.iter().all(|f| f.color == 0x3b82f6ff));
        assert!(d.features.iter().all(|f| &*f.layer == "water"));
    }

    #[test]
    fn attributes_and_ids_are_kept_sorted_as_text() {
        let t = TileCoord::new(0, 0, 0);
        let d = decode(&tile_with_tags(), t).unwrap();
        let f = &d.features[0];
        assert_eq!(f.id, Some(7));
        assert_eq!(
            f.props,
            [
                ("height".to_string(), "12.5".to_string()),
                ("name".to_string(), "Lake".to_string()),
            ]
        );
    }

    #[test]
    fn an_empty_body_is_an_empty_tile_and_junk_is_an_error() {
        let t = TileCoord::new(0, 0, 0);
        assert!(decode(&[], t).unwrap().is_empty());
        assert!(decode(&[0xff, 0xff, 0xff, 0xff, 0xff], t).is_err());
    }

    #[test]
    fn layers_are_coloured_by_what_their_name_suggests() {
        assert_eq!(layer_color("water"), layer_color("Water_polygons"));
        assert_ne!(layer_color("water"), layer_color("roads"));
        // An unknown name is stable, and opaque.
        assert_eq!(layer_color("zzz"), layer_color("zzz"));
        assert_eq!(layer_color("zzz") & 0xff, 0xff);
    }
}
