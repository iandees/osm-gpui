use gpui::*;
use std::collections::HashMap;
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::OsmData;
use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::selection::{FeatureKind, FeatureRef, HitCandidate, point_to_segment_distance};

const SELECTION_ACCENT: u32 = 0xFF4081;

/// Per-way axis-aligned bounding box in Web Mercator meters. Used to cull
/// offscreen ways with a cheap min/max compare against the viewport's
/// mercator-space view bounds — no trig per frame.
#[derive(Debug, Clone, Copy)]
struct WayBbox {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

/// Pre-projected node coordinates (Web Mercator meters) aligned with the
/// iteration order used by the render loops. Computing this once at
/// `set_osm_data` time eliminates the per-frame `lat_lon_to_mercator` (tan+ln)
/// from every node and way vertex.
#[derive(Debug, Clone)]
struct NodeCache {
    /// (mercator_x, mercator_y) keyed by node id. Used by the way-vertex
    /// build pass.
    by_id: HashMap<i64, (f64, f64)>,
    /// Flat list of all nodes as `(id, mercator_x, mercator_y)` for cache-
    /// friendly iteration in the node paint loop.
    flat: Vec<(i64, f64, f64)>,
}

/// Layer for rendering OSM vector data (nodes and ways)
pub struct OsmLayer {
    name: String,
    visible: bool,
    osm_data: Option<Arc<OsmData>>,
    /// Cached bboxes aligned with `osm_data.ways` by index. `None` means the
    /// way had no valid nodes and should be skipped.
    way_bboxes: Vec<Option<WayBbox>>,
    /// Pre-projected way vertex lists in Web Mercator meters, aligned with
    /// `osm_data.ways` by index. Lets the render loop walk a contiguous
    /// `&[(f64, f64)]` per way instead of doing a `HashMap::get` per node id.
    way_vertices: Vec<Vec<(f64, f64)>>,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
    /// Precomputed mercator positions for every node.
    node_cache: NodeCache,
    node_color: Rgba,
    way_color: Rgba,
    node_size: f32,
    way_width: f32,
    /// Feature to highlight (set each frame by MapViewer).
    highlight: Option<FeatureRef>,
}

fn compute_node_cache(data: &OsmData) -> NodeCache {
    let mut by_id = HashMap::with_capacity(data.nodes.len());
    let mut flat = Vec::with_capacity(data.nodes.len());
    for node in data.nodes.values() {
        if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
            let (mx, my) = lat_lon_to_mercator(lat, lon);
            by_id.insert(node.id, (mx, my));
            flat.push((node.id, mx, my));
        }
    }
    NodeCache { by_id, flat }
}

fn compute_layer_bbox(node_cache: &NodeCache) -> Option<WayBbox> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(_id, mx, my) in &node_cache.flat {
        if mx < min_x { min_x = mx; }
        if mx > max_x { max_x = mx; }
        if my < min_y { min_y = my; }
        if my > max_y { max_y = my; }
    }
    if min_x.is_finite() {
        Some(WayBbox { min_x, max_x, min_y, max_y })
    } else {
        None
    }
}

/// Build per-way bboxes and pre-projected vertex lists in a single pass so
/// neither the bbox pass nor the render path has to walk the node HashMap
/// per vertex.
fn compute_way_tables(
    data: &OsmData,
    node_cache: &NodeCache,
) -> (Vec<Option<WayBbox>>, Vec<Vec<(f64, f64)>>) {
    let mut bboxes = Vec::with_capacity(data.ways.len());
    let mut vertices = Vec::with_capacity(data.ways.len());
    for way in &data.ways {
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut verts = Vec::with_capacity(way.nodes.len());
        for nid in &way.nodes {
            if let Some(&(mx, my)) = node_cache.by_id.get(nid) {
                if mx < min_x { min_x = mx; }
                if mx > max_x { max_x = mx; }
                if my < min_y { min_y = my; }
                if my > max_y { max_y = my; }
                verts.push((mx, my));
            }
        }
        if verts.is_empty() {
            bboxes.push(None);
        } else {
            bboxes.push(Some(WayBbox { min_x, max_x, min_y, max_y }));
        }
        vertices.push(verts);
    }
    (bboxes, vertices)
}

impl OsmLayer {
    pub fn new() -> Self {
        Self {
            name: "OSM Data".to_string(),
            visible: true,
            osm_data: None,
            way_bboxes: Vec::new(),
            way_vertices: Vec::new(),
            layer_bbox: None,
            node_cache: NodeCache { by_id: HashMap::new(), flat: Vec::new() },
            node_color: rgb(0xFFD700), // Yellow for nodes
            way_color: rgb(0x4169E1),  // Royal blue for ways
            node_size: 10.0,
            way_width: 4.0,
            highlight: None,
        }
    }

    pub fn new_with_data<N: Into<String>>(name: N, osm_data: Arc<OsmData>) -> Self {
        let node_cache = compute_node_cache(&osm_data);
        let (way_bboxes, way_vertices) = compute_way_tables(&osm_data, &node_cache);
        let layer_bbox = compute_layer_bbox(&node_cache);
        Self {
            name: name.into(),
            visible: true,
            osm_data: Some(osm_data),
            way_bboxes,
            way_vertices,
            layer_bbox,
            node_cache,
            node_color: rgb(0xFFD700),
            way_color: rgb(0x4169E1),
            node_size: 10.0,
            way_width: 4.0,
            highlight: None,
        }
    }

    /// Set the OSM data for this layer
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.node_cache = compute_node_cache(&osm_data);
        let (bboxes, verts) = compute_way_tables(&osm_data, &self.node_cache);
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.osm_data = Some(osm_data);
    }

    /// Get the OSM data from this layer
    pub fn get_osm_data(&self) -> Option<Arc<OsmData>> {
        self.osm_data.clone()
    }

    /// Clear the OSM data
    pub fn clear_osm_data(&mut self) {
        self.osm_data = None;
        self.way_bboxes.clear();
        self.way_vertices.clear();
        self.layer_bbox = None;
        self.node_cache.by_id.clear();
        self.node_cache.flat.clear();
    }

    /// Check if this layer has data
    pub fn has_data(&self) -> bool {
        self.osm_data.is_some()
    }

    /// Set styling options
    pub fn set_style(&mut self, node_color: Rgba, way_color: Rgba, node_size: f32, way_width: f32) {
        self.node_color = node_color;
        self.way_color = way_color;
        self.node_size = node_size;
        self.way_width = way_width;
    }
}

impl MapLayer for OsmLayer {
    fn name(&self) -> &str { &self.name }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn set_highlight(&mut self, feature: Option<FeatureRef>) {
        self.highlight = feature;
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // Node rendering moved to `render_canvas` (paint_quad) to avoid the
        // per-node GPUI element layout cost. The selection ring is drawn in
        // `render_highlight` as a canvas outline.
        Vec::new()
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.osm_data.is_none() {
            return;
        }

        let origin_x = bounds.origin.x.0;
        let origin_y = bounds.origin.y.0;
        // Mercator-space view AABB. Culling and projection both happen in
        // this space so nothing in the hot loop touches trig.
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();

        // Layer-level early-out: if this layer's entire footprint is
        // off-screen, skip all per-vertex work.
        if let Some(lb) = &self.layer_bbox {
            if lb.max_x < vmin_x
                || lb.min_x > vmax_x
                || lb.max_y < vmin_y
                || lb.min_y > vmax_y
            {
                return;
            }
        }

        // Ways: bbox-cull in Mercator space, then batch all visible ways into
        // a single stroked path. Vertex lookup is a single contiguous slice
        // per way (`way_vertices[i]`) — no HashMap indirection. One
        // PathBuilder with many subpaths = one paint_path call per frame.
        // When per-rule styling arrives, group ways by
        // `(stroke_width, color)` and emit one path per group.
        let mut way_builder = PathBuilder::stroke(px(self.way_width));
        let mut way_points_pushed = false;

        for (i, verts) in self.way_vertices.iter().enumerate() {
            if verts.len() < 2 {
                continue;
            }
            let bbox = match self.way_bboxes.get(i).and_then(|b| b.as_ref()) {
                Some(b) => b,
                None => continue,
            };
            if bbox.max_x < vmin_x
                || bbox.min_x > vmax_x
                || bbox.max_y < vmin_y
                || bbox.min_y > vmax_y
            {
                continue;
            }

            let mut first = true;
            let mut emitted = 0;
            for &(mx, my) in verts {
                let sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) { continue; }
                let p = point(px(sp.x.0 + origin_x), px(sp.y.0 + origin_y));
                if first {
                    way_builder.move_to(p);
                    first = false;
                } else {
                    way_builder.line_to(p);
                }
                emitted += 1;
            }
            if emitted >= 2 {
                way_points_pushed = true;
            }
        }

        if way_points_pushed {
            if let Ok(path) = way_builder.build() {
                window.paint_path(path, self.way_color);
            }
        }

        // Nodes: iterate the flat cache (contiguous Vec) for better locality,
        // reject offscreen ones with a mercator-space AABB test, and paint
        // visible ones as filled quads on the canvas so we skip GPUI's
        // per-element layout pass. Batching nodes into a single PathBuilder
        // fill path was tried and turned out much slower — Lyon's fill
        // tessellator is not tuned for thousands of tiny rectangles.
        let half = self.node_size / 2.0;
        for &(_id, mx, my) in &self.node_cache.flat {
            if mx < vmin_x || mx > vmax_x || my < vmin_y || my > vmax_y {
                continue;
            }
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) { continue; }
            let quad_bounds = Bounds {
                origin: point(
                    px(sp.x.0 + origin_x - half),
                    px(sp.y.0 + origin_y - half),
                ),
                size: size(px(self.node_size), px(self.node_size)),
            };
            window.paint_quad(fill(quad_bounds, self.node_color));
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        if let Some(ref osm_data) = self.osm_data {
            vec![
                ("Nodes".to_string(), osm_data.nodes.len().to_string()),
                ("Ways".to_string(), osm_data.ways.len().to_string()),
                ("Node Size".to_string(), self.node_size.to_string()),
                ("Way Width".to_string(), self.way_width.to_string()),
            ]
        } else {
            vec![("Status".to_string(), "No data loaded".to_string())]
        }
    }

    fn hit_test(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = 8.0;
        const WAY_TOL: f32 = 6.0;

        let Some(ref data) = self.osm_data else { return Vec::new(); };

        // Phase 1: nodes within NODE_TOL.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        for node in data.nodes.values() {
            if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
                if !viewport.is_visible(lat, lon) { continue; }
                let sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) { continue; }
                let dx = sp.x.0 - screen_pt.x.0;
                let dy = sp.y.0 - screen_pt.y.0;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= NODE_TOL {
                    node_hits.push(HitCandidate {
                        feature: FeatureRef {
                            layer_name: self.name.clone(),
                            kind: FeatureKind::Node,
                            id: node.id,
                        },
                        kind: FeatureKind::Node,
                        dist_px: dist,
                    });
                }
            }
        }
        if !node_hits.is_empty() {
            return node_hits;
        }

        // Phase 2: ways within WAY_TOL. Compute shortest segment distance per way.
        let mut way_hits: Vec<HitCandidate> = Vec::new();
        for way in data.ways.iter() {
            if way.nodes.len() < 2 { continue; }
            let mut projected: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
            for node_id in &way.nodes {
                if let Some(n) = data.nodes.get(node_id) {
                    if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                        let sp = viewport.geo_to_screen(lat, lon);
                        if is_point_valid(sp) {
                            projected.push(sp);
                        }
                    }
                }
            }
            if projected.len() < 2 { continue; }
            let mut best = f32::INFINITY;
            for w in projected.windows(2) {
                let d = point_to_segment_distance(screen_pt, w[0], w[1]);
                if d < best { best = d; }
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_name: self.name.clone(),
                        kind: FeatureKind::Way,
                        id: way.id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: best,
                });
            }
        }
        way_hits
    }

    fn feature_tags(&self, feature: &FeatureRef) -> Option<Vec<(String, String)>> {
        if feature.layer_name != self.name { return None; }
        let data = self.osm_data.as_ref()?;
        let tags = match feature.kind {
            FeatureKind::Node => {
                let n = data.nodes.get(&feature.id)?;
                n.tags.clone()
            }
            FeatureKind::Way => {
                let w = data.ways.iter().find(|w| w.id == feature.id)?;
                w.tags.clone()
            }
        };
        let mut kv: Vec<(String, String)> = tags.into_iter().collect();
        kv.sort_by(|a, b| a.0.cmp(&b.0));
        Some(kv)
    }

    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
    ) {
        if feature.layer_name != self.name { return; }
        let Some(ref osm_data) = self.osm_data else { return; };

        match feature.kind {
            FeatureKind::Node => {
                let Some(n) = osm_data.nodes.get(&feature.id) else { return; };
                let Some((lat, lon)) = validate_coords(n.lat, n.lon) else { return; };
                let sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) { return; }
                let ring_size = self.node_size * 2.0;
                let half = ring_size / 2.0;
                let ring_bounds = Bounds {
                    origin: point(
                        px(sp.x.0 + bounds.origin.x.0 - half),
                        px(sp.y.0 + bounds.origin.y.0 - half),
                    ),
                    size: size(px(ring_size), px(ring_size)),
                };
                window.paint_quad(outline(
                    ring_bounds,
                    rgb(SELECTION_ACCENT),
                    BorderStyle::Solid,
                ));
            }
            FeatureKind::Way => {
                let Some(way) = osm_data.ways.iter().find(|w| w.id == feature.id) else { return; };
                if way.nodes.len() < 2 { return; }

                let origin_x = bounds.origin.x;
                let origin_y = bounds.origin.y;

                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
                for node_id in &way.nodes {
                    if let Some(n) = osm_data.nodes.get(node_id) {
                        if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                            let sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                pts.push(point(
                                    px(sp.x.0 + origin_x.0),
                                    px(sp.y.0 + origin_y.0),
                                ));
                            }
                        }
                    }
                }
                if pts.len() < 2 { return; }

                let mut builder = PathBuilder::stroke(px(self.way_width + 4.0));
                for (i, p) in pts.iter().enumerate() {
                    if i == 0 { builder.move_to(*p); } else { builder.line_to(*p); }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, rgb(SELECTION_ACCENT));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::MapLayer;
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::FeatureKind;
    use crate::viewport::Viewport;
    use gpui::{point, px, size};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    /// Build a viewport whose center projects a chosen (lat, lon) to the middle
    /// of an 800x600 map area. Zoom 18 is high enough that a degree-scale offset
    /// in node positions translates to many pixels.
    fn viewport_centered_on(lat: f64, lon: f64) -> Viewport {
        Viewport::new(lat, lon, 18.0, size(px(800.0), px(600.0)))
    }

    fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> Arc<OsmData> {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id, n);
        }
        Arc::new(OsmData {
            nodes: map,
            ways,
            relations: Vec::new(),
            bounds: None,
        })
    }

    #[test]
    fn hit_test_node_wins_over_coincident_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, FeatureKind::Node);
        assert_eq!(hits[0].feature.id, 1);
    }

    #[test]
    fn hit_test_falls_through_to_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon - 0.001, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
        assert!(hits.iter().any(|h| h.feature.id == 10));
    }

    #[test]
    fn hit_test_no_match_returns_empty() {
        let n = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n], vec![]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data("L", data);

        let hits = layer.hit_test(&viewport, point(px(50.0), px(50.0)));
        assert!(hits.is_empty(), "unexpected hits: {:?}", hits);
    }
}
