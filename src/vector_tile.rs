//! Decoding a Mapbox vector tile into what the map draws: lines, triangulated
//! polygon fills and points, all in Web Mercator metres so a frame only has
//! to project them.
//!
//! The tile carries no style, so each layer gets a colour from its name (water
//! blue, buildings brown, roads orange, ...), falling back to a stable colour
//! picked from the name for names nothing is known about.

use mvt_reader::Reader;

use crate::tiles::TileCoord;

/// A point in Web Mercator metres.
pub type Merc = [f64; 2];

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub color: u32,
    pub pts: Vec<Merc>,
}

/// A filled area: `tris` holds three vertices per triangle.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub color: u32,
    pub tris: Vec<Merc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dot {
    pub color: u32,
    pub at: Merc,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodedTile {
    pub fills: Vec<Fill>,
    pub lines: Vec<Line>,
    pub dots: Vec<Dot>,
}

impl DecodedTile {
    pub fn is_empty(&self) -> bool {
        self.fills.is_empty() && self.lines.is_empty() && self.dots.is_empty()
    }

    /// Vertices held, as a rough size for the tile cache.
    pub fn vertex_count(&self) -> usize {
        self.fills.iter().map(|f| f.tris.len()).sum::<usize>()
            + self.lines.iter().map(|l| l.pts.len()).sum::<usize>()
            + self.dots.len()
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
            add_geometry(&mut out, color, f.geometry, &to_merc);
        }
    }
    Ok(out)
}

fn add_geometry(
    out: &mut DecodedTile,
    color: u32,
    g: geo_types::Geometry<f32>,
    to_merc: &impl Fn(geo_types::Coord<f32>) -> Merc,
) {
    use geo_types::Geometry as G;
    match g {
        G::Point(p) => out.dots.push(Dot {
            color,
            at: to_merc(p.0),
        }),
        G::MultiPoint(mp) => out.dots.extend(mp.0.into_iter().map(|p| Dot {
            color,
            at: to_merc(p.0),
        })),
        G::LineString(l) => add_line(out, color, &l, to_merc),
        G::MultiLineString(ml) => {
            for l in &ml.0 {
                add_line(out, color, l, to_merc);
            }
        }
        G::Polygon(p) => add_polygon(out, color, &p, to_merc),
        G::MultiPolygon(mp) => {
            for p in &mp.0 {
                add_polygon(out, color, p, to_merc);
            }
        }
        G::GeometryCollection(gc) => {
            for g in gc.0 {
                add_geometry(out, color, g, to_merc);
            }
        }
        _ => {}
    }
}

fn add_line(
    out: &mut DecodedTile,
    color: u32,
    l: &geo_types::LineString<f32>,
    to_merc: &impl Fn(geo_types::Coord<f32>) -> Merc,
) {
    if l.0.len() >= 2 {
        out.lines.push(Line {
            color,
            pts: l.0.iter().map(|&c| to_merc(c)).collect(),
        });
    }
}

fn add_polygon(
    out: &mut DecodedTile,
    color: u32,
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
    if let Ok(idx) = earcutr::earcut(&flat, &holes, 2) {
        let tris: Vec<Merc> = idx
            .iter()
            .map(|&i| [flat[2 * i], flat[2 * i + 1]])
            .collect();
        if !tris.is_empty() {
            out.fills.push(Fill { color, tris });
        }
    }
    // The outline of the area, including its holes.
    add_line(out, color, p.exterior(), to_merc);
    for h in p.interiors() {
        add_line(out, color, h, to_merc);
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

    #[test]
    fn a_tile_decodes_to_fills_lines_and_dots_in_mercator_metres() {
        let t = TileCoord::new(0, 0, 0);
        let d = decode(&tile(), t).unwrap();
        assert_eq!(d.dots.len(), 1);
        // The point is at the middle of the world tile.
        assert!(d.dots[0].at[0].abs() < 1.0 && d.dots[0].at[1].abs() < 1.0);
        // The line and the square's outline.
        assert_eq!(d.lines.len(), 2);
        // A square is two triangles.
        assert_eq!(d.fills.len(), 1);
        assert_eq!(d.fills[0].tris.len(), 6);
        assert!(d.fills.iter().all(|f| f.color == 0x3b82f6ff));
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
