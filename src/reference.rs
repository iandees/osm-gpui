//! Read-only reference data: the features of a GPX or GeoJSON file, kept in a
//! form that is cheap to cull, draw, hit-test and snap to.
//!
//! Lines are stored in Web Mercator meters, cut into chunks of at most
//! [`CHUNK_VERTICES`] vertices that each carry a bounding box. A track of a
//! million points is therefore a few thousand chunks, and a frame touches only
//! the chunks that intersect the view. Markers (points, waypoints) are kept in
//! one flat list. The parsers (`gpx`, `geojson`) fill a [`Builder`]; the layer
//! (`layers::reference_layer`) draws the result.

use crate::coordinates::{lat_lon_to_mercator, mercator_to_lat_lon};
use crate::tr;
use crate::viewport::Viewport;

/// Longest run of vertices in one chunk of a line.
pub const CHUNK_VERTICES: usize = 256;

/// The file format a reference layer was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gpx,
    GeoJson,
}

impl Format {
    /// Short tag shown beside the layer in the Layers panel.
    pub fn tag(self) -> &'static str {
        match self {
            Format::Gpx => "GPX",
            Format::GeoJson => "GeoJSON",
        }
    }
}

/// What a feature is, for the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Track,
    Route,
    Waypoint,
    /// A GeoJSON feature or bare geometry.
    Geo,
}

/// Why a file could not be used as a reference layer.
#[derive(Debug, Clone, PartialEq)]
pub enum RefError {
    /// The file is not well-formed XML / JSON (or not the expected document).
    Malformed(String),
    /// A coordinate is outside the WGS84 range (a projected file, for example).
    NotGeographic { x: f64, y: f64 },
    /// Well-formed, but nothing to draw.
    NoGeometry,
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefError::Malformed(detail) => {
                write!(f, "{}", tr!("reference.error.malformed", detail = detail))
            }
            RefError::NotGeographic { x, y } => {
                write!(f, "{}", tr!("reference.error.not_geographic", x = x, y = y))
            }
            RefError::NoGeometry => write!(f, "{}", tr!("reference.error.no_geometry")),
        }
    }
}

impl std::error::Error for RefError {}

/// An axis-aligned box in Web Mercator meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MercBox {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl MercBox {
    pub const EMPTY: MercBox = MercBox {
        min_x: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        min_y: f64::INFINITY,
        max_y: f64::NEG_INFINITY,
    };

    pub fn extend(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_y = self.min_y.min(y);
        self.max_y = self.max_y.max(y);
    }

    pub fn is_empty(&self) -> bool {
        self.min_x > self.max_x
    }

    /// Whether the two boxes overlap (touching counts).
    pub fn intersects(&self, view: &MercBox) -> bool {
        self.max_x >= view.min_x
            && self.min_x <= view.max_x
            && self.max_y >= view.min_y
            && self.min_y <= view.max_y
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    pub fn padded(&self, by: f64) -> MercBox {
        MercBox {
            min_x: self.min_x - by,
            max_x: self.max_x + by,
            min_y: self.min_y - by,
            max_y: self.max_y + by,
        }
    }
}

/// One feature of a file: what is shown when it is clicked.
#[derive(Debug, Clone, PartialEq)]
pub struct RefFeature {
    pub kind: FeatureKind,
    /// The GeoJSON geometry type (`"LineString"`, ...); empty for GPX.
    pub geometry: &'static str,
    pub name: Option<String>,
    /// GeoJSON `properties`, or a GPX element's `desc`/`cmt`/`type`/`sym`.
    pub props: Vec<(String, String)>,
}

/// A run of at most [`CHUNK_VERTICES`] connected vertices.
#[derive(Debug, Clone)]
pub struct Part {
    pub feature: u32,
    pub bbox: MercBox,
    pub pts: Vec<[f64; 2]>,
}

/// A point feature.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    pub feature: u32,
    pub at: [f64; 2],
}

/// `(min_lat, max_lat, min_lon, max_lon)`.
pub type GeoExtent = (f64, f64, f64, f64);

/// The parsed content of one file.
#[derive(Debug, Clone)]
pub struct ReferenceData {
    pub format: Format,
    pub features: Vec<RefFeature>,
    pub parts: Vec<Part>,
    pub markers: Vec<Marker>,
    pub vertex_count: usize,
    extent: Option<GeoExtent>,
}

impl ReferenceData {
    /// The geographic extent of everything in the file.
    pub fn extent(&self) -> Option<GeoExtent> {
        self.extent
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty() && self.markers.is_empty()
    }

    /// Chunks of lines that may be visible in `view`.
    pub fn visible_parts<'a>(&'a self, view: &'a MercBox) -> impl Iterator<Item = &'a Part> + 'a {
        self.parts.iter().filter(move |p| p.bbox.intersects(view))
    }

    /// Markers inside `view`.
    pub fn visible_markers<'a>(
        &'a self,
        view: &'a MercBox,
    ) -> impl Iterator<Item = &'a Marker> + 'a {
        self.markers
            .iter()
            .filter(move |m| view.contains(m.at[0], m.at[1]))
    }

    /// The point of the data nearest to `screen_pt` (view-relative pixels)
    /// within `tolerance_px`. A vertex or marker within the tolerance wins
    /// over the nearest point on a segment.
    pub fn nearest(
        &self,
        viewport: &Viewport,
        screen_pt: gpui::Point<gpui::Pixels>,
        tolerance_px: f32,
    ) -> Option<Nearest> {
        let (cx, cy) = viewport.screen_to_mercator(screen_pt);
        let (ppm, _) = viewport.projector().pixels_per_meter();
        if !(ppm.is_finite() && ppm > 0.0) {
            return None;
        }
        self.nearest_mercator((cx, cy), tolerance_px as f64 / ppm)
    }

    /// `nearest` in Web Mercator meters.
    pub fn nearest_mercator(&self, at: (f64, f64), tol_m: f64) -> Option<Nearest> {
        let (cx, cy) = at;
        let area = MercBox {
            min_x: cx - tol_m,
            max_x: cx + tol_m,
            min_y: cy - tol_m,
            max_y: cy + tol_m,
        };
        let tol2 = tol_m * tol_m;
        let mut vertex: Option<(f64, Nearest)> = None;
        let mut segment: Option<(f64, Nearest)> = None;
        let mut offer_vertex = |d2: f64, n: Nearest| {
            if d2 <= tol2 && vertex.as_ref().is_none_or(|(best, _)| d2 < *best) {
                vertex = Some((d2, n));
            }
        };
        for m in self.visible_markers(&area) {
            let d2 = sq(m.at[0] - cx) + sq(m.at[1] - cy);
            offer_vertex(
                d2,
                Nearest {
                    feature: m.feature,
                    at: (m.at[0], m.at[1]),
                    is_vertex: true,
                },
            );
        }
        for part in self.visible_parts(&area) {
            for (i, p) in part.pts.iter().enumerate() {
                let d2 = sq(p[0] - cx) + sq(p[1] - cy);
                offer_vertex(
                    d2,
                    Nearest {
                        feature: part.feature,
                        at: (p[0], p[1]),
                        is_vertex: true,
                    },
                );
                if let Some(q) = part.pts.get(i + 1) {
                    let (sx, sy) = closest_on_segment((cx, cy), (p[0], p[1]), (q[0], q[1]));
                    let d2 = sq(sx - cx) + sq(sy - cy);
                    if d2 <= tol2 && segment.as_ref().is_none_or(|(best, _)| d2 < *best) {
                        segment = Some((
                            d2,
                            Nearest {
                                feature: part.feature,
                                at: (sx, sy),
                                is_vertex: false,
                            },
                        ));
                    }
                }
            }
        }
        vertex.or(segment).map(|(_, n)| n)
    }
}

#[inline]
fn sq(v: f64) -> f64 {
    v * v
}

/// The point of segment `a`-`b` closest to `p`.
pub fn closest_on_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= f64::EPSILON {
        return a;
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    (a.0 + t * dx, a.1 + t * dy)
}

/// What `ReferenceData::nearest` found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nearest {
    pub feature: u32,
    /// The point, in Web Mercator meters.
    pub at: (f64, f64),
    /// A vertex or marker, as opposed to a point inside a segment.
    pub is_vertex: bool,
}

impl Nearest {
    /// The point as `(lat, lon)`.
    pub fn lat_lon(&self) -> (f64, f64) {
        mercator_to_lat_lon(self.at.0, self.at.1)
    }
}

/// Collects features while a file is parsed, checking every coordinate.
pub struct Builder {
    format: Format,
    features: Vec<RefFeature>,
    parts: Vec<Part>,
    markers: Vec<Marker>,
    vertex_count: usize,
    extent: Option<GeoExtent>,
}

impl Builder {
    pub fn new(format: Format) -> Self {
        Builder {
            format,
            features: Vec::new(),
            parts: Vec::new(),
            markers: Vec::new(),
            vertex_count: 0,
            extent: None,
        }
    }

    /// Start a feature; geometry is added to it with `line` and `point`.
    pub fn feature(
        &mut self,
        kind: FeatureKind,
        geometry: &'static str,
        name: Option<String>,
        props: Vec<(String, String)>,
    ) -> u32 {
        self.features.push(RefFeature {
            kind,
            geometry,
            name: name.filter(|n| !n.trim().is_empty()),
            props,
        });
        (self.features.len() - 1) as u32
    }

    /// Check a `lon, lat` pair and widen the extent. Fails for anything that
    /// is not a WGS84 position.
    fn accept(&mut self, lon: f64, lat: f64) -> Result<(f64, f64), RefError> {
        if !(lon.is_finite()
            && lat.is_finite()
            && (-180.0..=180.0).contains(&lon)
            && (-90.0..=90.0).contains(&lat))
        {
            return Err(RefError::NotGeographic { x: lon, y: lat });
        }
        let e = self.extent.get_or_insert((lat, lat, lon, lon));
        e.0 = e.0.min(lat);
        e.1 = e.1.max(lat);
        e.2 = e.2.min(lon);
        e.3 = e.3.max(lon);
        self.vertex_count += 1;
        Ok(lat_lon_to_mercator(lat, lon))
    }

    /// Add a polyline of `(lon, lat)` positions. Fewer than two positions
    /// draw nothing (but are still checked).
    pub fn line(&mut self, feature: u32, lon_lat: &[(f64, f64)]) -> Result<(), RefError> {
        let mut merc: Vec<[f64; 2]> = Vec::with_capacity(lon_lat.len());
        for &(lon, lat) in lon_lat {
            let (x, y) = self.accept(lon, lat)?;
            merc.push([x, y]);
        }
        if merc.len() < 2 {
            return Ok(());
        }
        let mut start = 0;
        while start + 1 < merc.len() {
            let end = (start + CHUNK_VERTICES).min(merc.len());
            let pts = merc[start..end].to_vec();
            let mut bbox = MercBox::EMPTY;
            for p in &pts {
                bbox.extend(p[0], p[1]);
            }
            self.parts.push(Part { feature, bbox, pts });
            // The next chunk starts at this one's last vertex: no gap.
            start = end - 1;
        }
        Ok(())
    }

    pub fn point(&mut self, feature: u32, lon: f64, lat: f64) -> Result<(), RefError> {
        let (x, y) = self.accept(lon, lat)?;
        self.markers.push(Marker {
            feature,
            at: [x, y],
        });
        Ok(())
    }

    /// The finished data, or `NoGeometry` if nothing can be drawn.
    pub fn finish(self) -> Result<ReferenceData, RefError> {
        if self.parts.is_empty() && self.markers.is_empty() {
            return Err(RefError::NoGeometry);
        }
        Ok(ReferenceData {
            format: self.format,
            features: self.features,
            parts: self.parts,
            markers: self.markers,
            vertex_count: self.vertex_count,
            extent: self.extent,
        })
    }
}

/// The format of a file, by extension first and then by looking at its start.
/// `None` for an `.osm` file or anything unrecognised (the caller treats that
/// as OSM XML).
pub fn detect_format(path: &std::path::Path, head: &[u8]) -> Option<Format> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("gpx") => return Some(Format::Gpx),
        Some("geojson") => return Some(Format::GeoJson),
        Some("osm") => return None,
        _ => {}
    }
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(Format::GeoJson);
    }
    if trimmed.starts_with('<') {
        // The root element decides: <gpx ...> versus <osm ...>.
        let mut rest = trimmed;
        while let Some(stripped) = rest.strip_prefix("<?").or_else(|| rest.strip_prefix("<!")) {
            match stripped.find('>') {
                Some(end) => rest = stripped[end + 1..].trim_start(),
                None => break,
            }
        }
        if rest.starts_with("<gpx") {
            return Some(Format::Gpx);
        }
    }
    None
}

/// Read and parse a GPX or GeoJSON file. Blocking: call it off the UI thread.
pub fn load_file(path: &std::path::Path, format: Format) -> Result<ReferenceData, RefError> {
    let bytes = std::fs::read(path).map_err(|e| RefError::Malformed(e.to_string()))?;
    match format {
        Format::Gpx => crate::gpx::parse(&bytes),
        Format::GeoJson => crate::geojson::parse(&bytes),
    }
}

/// The format of the file at `path` if it is a reference file, reading its
/// first bytes when the extension does not say.
pub fn sniff_path(path: &std::path::Path) -> Option<Format> {
    use std::io::Read;
    let mut head = [0u8; 512];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    detect_format(path, &head[..n])
}

/// One status-bar line for a clicked feature: its name (or kind), then up to
/// a few `key=value` properties.
pub fn describe(feature: &RefFeature) -> String {
    const MAX_PROPS: usize = 6;
    const MAX_VALUE_CHARS: usize = 60;
    let kind = match feature.kind {
        FeatureKind::Track => tr!("reference.kind.track").to_string(),
        FeatureKind::Route => tr!("reference.kind.route").to_string(),
        FeatureKind::Waypoint => tr!("reference.kind.waypoint").to_string(),
        FeatureKind::Geo => feature.geometry.to_string(),
    };
    let title = match &feature.name {
        Some(name) => format!("{kind}: {name}"),
        None => kind,
    };
    if feature.props.is_empty() {
        return title;
    }
    let shown: Vec<String> = feature
        .props
        .iter()
        .take(MAX_PROPS)
        .map(|(k, v)| {
            let v: String = if v.chars().count() > MAX_VALUE_CHARS {
                let cut: String = v.chars().take(MAX_VALUE_CHARS).collect();
                format!("{cut}\u{2026}")
            } else {
                v.clone()
            };
            format!("{k}={v}")
        })
        .collect();
    let more = feature.props.len().saturating_sub(MAX_PROPS);
    let mut line = format!("{title} \u{2014} {}", shown.join(", "));
    if more > 0 {
        line.push_str(&format!(", +{more}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_feature(b: &mut Builder) -> u32 {
        b.feature(FeatureKind::Geo, "LineString", None, vec![])
    }

    #[test]
    fn long_lines_are_chunked_with_shared_end_vertices() {
        let mut b = Builder::new(Format::GeoJson);
        let f = line_feature(&mut b);
        let pts: Vec<(f64, f64)> = (0..1000).map(|i| (i as f64 * 1e-4, 10.0)).collect();
        b.line(f, &pts).unwrap();
        let data = b.finish().unwrap();
        assert!(data.parts.len() > 3);
        for pair in data.parts.windows(2) {
            assert_eq!(pair[0].pts.last(), pair[1].pts.first());
        }
        assert!(data.parts.iter().all(|p| p.pts.len() <= CHUNK_VERTICES));
        let segs: usize = data.parts.iter().map(|p| p.pts.len() - 1).sum();
        assert_eq!(segs, 999);
    }

    #[test]
    fn extent_covers_all_geometry() {
        let mut b = Builder::new(Format::GeoJson);
        let f = line_feature(&mut b);
        b.line(f, &[(-1.0, 50.0), (2.0, 51.0)]).unwrap();
        b.point(f, 3.0, 49.0).unwrap();
        let data = b.finish().unwrap();
        assert_eq!(data.extent(), Some((49.0, 51.0, -1.0, 3.0)));
        assert_eq!(data.vertex_count, 3);
    }

    #[test]
    fn projected_coordinates_are_rejected() {
        let mut b = Builder::new(Format::GeoJson);
        let f = line_feature(&mut b);
        let err = b
            .line(f, &[(500_000.0, 4_649_776.0), (500_100.0, 4_649_800.0)])
            .unwrap_err();
        assert!(matches!(err, RefError::NotGeographic { .. }));
        assert!(!err.to_string().is_empty());
        let mut b = Builder::new(Format::Gpx);
        let f = line_feature(&mut b);
        assert!(b.point(f, 10.0, 95.0).is_err());
        assert!(b.point(f, f64::NAN, 1.0).is_err());
    }

    #[test]
    fn nothing_to_draw_is_an_error() {
        let mut b = Builder::new(Format::Gpx);
        let f = line_feature(&mut b);
        // A one-position line draws nothing.
        b.line(f, &[(1.0, 1.0)]).unwrap();
        assert_eq!(b.finish().unwrap_err(), RefError::NoGeometry);
    }

    #[test]
    fn culling_keeps_only_chunks_in_view() {
        let mut b = Builder::new(Format::GeoJson);
        let f = line_feature(&mut b);
        // Three well separated short lines.
        b.line(f, &[(0.0, 0.0), (0.001, 0.0)]).unwrap();
        b.line(f, &[(10.0, 10.0), (10.001, 10.0)]).unwrap();
        b.line(f, &[(20.0, 20.0), (20.001, 20.0)]).unwrap();
        b.point(f, 10.0005, 10.0).unwrap();
        b.point(f, 30.0, 30.0).unwrap();
        let data = b.finish().unwrap();
        let (x0, y0) = lat_lon_to_mercator(9.0, 9.0);
        let (x1, y1) = lat_lon_to_mercator(11.0, 11.0);
        let view = MercBox {
            min_x: x0,
            max_x: x1,
            min_y: y0,
            max_y: y1,
        };
        assert_eq!(data.visible_parts(&view).count(), 1);
        assert_eq!(data.visible_markers(&view).count(), 1);
        let none = MercBox {
            min_x: -1.0,
            max_x: -0.5,
            min_y: 1e7,
            max_y: 1.1e7,
        };
        assert_eq!(data.visible_parts(&none).count(), 0);
    }

    #[test]
    fn nearest_prefers_vertices_then_segments() {
        let mut b = Builder::new(Format::GeoJson);
        let f = line_feature(&mut b);
        b.line(f, &[(0.0, 0.0), (0.01, 0.0)]).unwrap();
        let data = b.finish().unwrap();
        let (ax, ay) = lat_lon_to_mercator(0.0, 0.0);
        let (bx, _) = lat_lon_to_mercator(0.0, 0.01);
        // 3 m from the start vertex, tolerance 5 m: the vertex.
        let n = data.nearest_mercator((ax + 3.0, ay + 1.0), 5.0).unwrap();
        assert!(n.is_vertex);
        assert_eq!(n.at, (ax, ay));
        // Midway, 2 m off the line: a point inside the segment.
        let mid = (ax + bx) / 2.0;
        let n = data.nearest_mercator((mid, ay + 2.0), 5.0).unwrap();
        assert!(!n.is_vertex);
        assert!((n.at.0 - mid).abs() < 1e-9 && (n.at.1 - ay).abs() < 1e-9);
        let (lat, lon) = n.lat_lon();
        assert!(lat.abs() < 1e-6 && (lon - 0.005).abs() < 1e-6);
        // Out of tolerance.
        assert!(data.nearest_mercator((mid, ay + 50.0), 5.0).is_none());
    }

    #[test]
    fn nearest_finds_markers() {
        let mut b = Builder::new(Format::Gpx);
        let f = b.feature(FeatureKind::Waypoint, "", Some("w".into()), vec![]);
        b.point(f, 1.0, 1.0).unwrap();
        let data = b.finish().unwrap();
        let (x, y) = lat_lon_to_mercator(1.0, 1.0);
        let n = data.nearest_mercator((x + 2.0, y), 5.0).unwrap();
        assert_eq!((n.feature, n.is_vertex), (0, true));
    }

    #[test]
    fn closest_point_clamps_to_the_segment() {
        assert_eq!(
            closest_on_segment((5.0, 3.0), (0.0, 0.0), (10.0, 0.0)),
            (5.0, 0.0)
        );
        assert_eq!(
            closest_on_segment((-5.0, 3.0), (0.0, 0.0), (10.0, 0.0)),
            (0.0, 0.0)
        );
        assert_eq!(
            closest_on_segment((15.0, 3.0), (0.0, 0.0), (10.0, 0.0)),
            (10.0, 0.0)
        );
        // A zero-length segment.
        assert_eq!(
            closest_on_segment((1.0, 1.0), (2.0, 2.0), (2.0, 2.0)),
            (2.0, 2.0)
        );
    }

    #[test]
    fn format_is_routed_by_extension_then_content() {
        use std::path::Path;
        assert_eq!(detect_format(Path::new("a.gpx"), b""), Some(Format::Gpx));
        assert_eq!(
            detect_format(Path::new("a.GeoJSON"), b""),
            Some(Format::GeoJson)
        );
        assert_eq!(detect_format(Path::new("a.osm"), b"{"), None);
        assert_eq!(
            detect_format(Path::new("a.json"), b"  {\"type\":\"Point\"}"),
            Some(Format::GeoJson)
        );
        assert_eq!(
            detect_format(
                Path::new("a.xml"),
                b"<?xml version=\"1.0\"?>\n<!-- c -->\n<gpx version=\"1.1\">"
            ),
            Some(Format::Gpx)
        );
        assert_eq!(
            detect_format(
                Path::new("a.xml"),
                b"<?xml version=\"1.0\"?><osm version=\"0.6\">"
            ),
            None
        );
        assert_eq!(detect_format(Path::new("noext"), b"hello"), None);
    }

    #[test]
    fn describe_names_the_feature_and_lists_properties() {
        let f = RefFeature {
            kind: FeatureKind::Geo,
            geometry: "Point",
            name: Some("Cafe".into()),
            props: vec![("amenity".into(), "cafe".into())],
        };
        assert_eq!(describe(&f), "Point: Cafe \u{2014} amenity=cafe");
        let bare = RefFeature {
            kind: FeatureKind::Geo,
            geometry: "LineString",
            name: None,
            props: vec![],
        };
        assert_eq!(describe(&bare), "LineString");
        let many = RefFeature {
            kind: FeatureKind::Geo,
            geometry: "Point",
            name: None,
            props: (0..9).map(|i| (format!("k{i}"), "v".to_string())).collect(),
        };
        assert!(describe(&many).ends_with(", +3"));
    }
}
