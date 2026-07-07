use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::{OsmData, OsmWay};
use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::selection::{FeatureKind, FeatureRef, HitCandidate, point_to_segment_distance};
use crate::style::Stylesheet;
use rstar::{RTree, AABB, primitives::{GeomWithData, Rectangle}};

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
    /// Pre-projected way vertex lists (node id + Web Mercator meters),
    /// aligned with `osm_data.ways` by index. Lets the render loop walk a
    /// contiguous slice per way instead of doing a `HashMap::get` per node
    /// id. The node id is carried alongside the position so the render loop
    /// can match vertices against an active `drag_preview` set.
    way_vertices: Vec<Vec<(i64, f64, f64)>>,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
    /// Precomputed mercator positions for every node.
    node_cache: NodeCache,
    /// Stylesheet used to pick per-feature colors/weights from OSM tags.
    stylesheet: Arc<Stylesheet>,
    /// Feature to highlight (set each frame by MapViewer).
    highlight: Vec<FeatureRef>,
    /// Spatial index of all nodes (mercator x/y -> node id), rebuilt whenever
    /// data changes. Used by box-select (`hit_test_rect`).
    node_index: RTree<GeomWithData<[f64; 2], i64>>,
    /// Spatial index of all way bounding boxes (mercator meters -> way id),
    /// rebuilt whenever data changes. `locate_in_envelope` on this index
    /// returns ways whose bbox is fully contained in the query rect, which is
    /// exactly the "fully enclosed" box-select rule for ways.
    way_index: RTree<GeomWithData<Rectangle<[f64; 2]>, i64>>,
    /// Transient screen-space offset applied to the given node ids while
    /// rendering, for live drag feedback. Never touches `osm_data`.
    drag_preview: Option<(HashSet<i64>, Point<Pixels>)>,
    /// Whether this layer has had a committed move since it was loaded.
    modified: bool,
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
) -> (Vec<Option<WayBbox>>, Vec<Vec<(i64, f64, f64)>>) {
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
                verts.push((*nid, mx, my));
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

/// Push an oriented rectangle covering the segment `p0`-`p1` (as two raw
/// triangles) into `path`, creating it lazily on first use. Skips
/// degenerate (zero-length) segments.
///
/// Pushes vertices directly (`Path::vertices` is a public field) instead of
/// going through `Path::push_triangle`, which recomputes `path.bounds` via
/// three separate `Bounds::union` calls per triangle. `bounds_min_max`
/// accumulates the same information as cheap float compares instead; the
/// caller writes it into `path.bounds` once, after all segments in the
/// group are pushed.
fn push_segment_quad(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    p0: Point<Pixels>,
    p1: Point<Pixels>,
    half_width: f32,
) {
    let x0 = f32::from(p0.x);
    let y0 = f32::from(p0.y);
    let x1 = f32::from(p1.x);
    let y1 = f32::from(p1.y);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len < f32::EPSILON {
        return;
    }
    let nx = -dy / len * half_width;
    let ny = dx / len * half_width;

    let ax = x0 + nx;
    let ay = y0 + ny;
    let bx = x0 - nx;
    let by = y0 - ny;
    let cx = x1 - nx;
    let cy = y1 - ny;
    let dxp = x1 + nx;
    let dyp = y1 + ny;

    let (min_x, max_x, min_y, max_y) = bounds_min_max;
    for &(x, y) in &[(ax, ay), (bx, by), (cx, cy), (dxp, dyp)] {
        if x < *min_x { *min_x = x; }
        if x > *max_x { *max_x = x; }
        if y < *min_y { *min_y = y; }
        if y > *max_y { *max_y = y; }
    }

    let a = point(px(ax), px(ay));
    let b = point(px(bx), px(by));
    let c = point(px(cx), px(cy));
    let d = point(px(dxp), px(dyp));
    let st = point(0., 1.);

    let p = path.get_or_insert_with(|| Path::new(a));
    p.vertices.push(PathVertex { xy_position: a, st_position: st, content_mask: Default::default() });
    p.vertices.push(PathVertex { xy_position: b, st_position: st, content_mask: Default::default() });
    p.vertices.push(PathVertex { xy_position: c, st_position: st, content_mask: Default::default() });
    p.vertices.push(PathVertex { xy_position: a, st_position: st, content_mask: Default::default() });
    p.vertices.push(PathVertex { xy_position: c, st_position: st, content_mask: Default::default() });
    p.vertices.push(PathVertex { xy_position: d, st_position: st, content_mask: Default::default() });
}

/// Bulk-build a point index over every node's mercator position.
fn build_node_index(node_cache: &NodeCache) -> RTree<GeomWithData<[f64; 2], i64>> {
    let items: Vec<_> = node_cache
        .flat
        .iter()
        .map(|&(id, mx, my)| GeomWithData::new([mx, my], id))
        .collect();
    RTree::bulk_load(items)
}

/// Bulk-build a bounding-box index over every way's mercator bbox. Ways with
/// no valid bbox (no resolvable nodes) are skipped.
fn build_way_index(way_bboxes: &[Option<WayBbox>], ways: &[OsmWay]) -> RTree<GeomWithData<Rectangle<[f64; 2]>, i64>> {
    let items: Vec<_> = way_bboxes
        .iter()
        .zip(ways.iter())
        .filter_map(|(bbox, way)| {
            let b = bbox.as_ref()?;
            Some(GeomWithData::new(
                Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way.id,
            ))
        })
        .collect();
    RTree::bulk_load(items)
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
            stylesheet: Arc::new(Stylesheet::load_default()),
            highlight: Vec::new(),
            node_index: RTree::new(),
            way_index: RTree::new(),
            drag_preview: None,
            modified: false,
        }
    }

    pub fn new_with_data<N: Into<String>>(name: N, osm_data: Arc<OsmData>) -> Self {
        let node_cache = compute_node_cache(&osm_data);
        let (way_bboxes, way_vertices) = compute_way_tables(&osm_data, &node_cache);
        let layer_bbox = compute_layer_bbox(&node_cache);
        let node_index = build_node_index(&node_cache);
        let way_index = build_way_index(&way_bboxes, &osm_data.ways);
        Self {
            name: name.into(),
            visible: true,
            osm_data: Some(osm_data),
            way_bboxes,
            way_vertices,
            layer_bbox,
            node_cache,
            stylesheet: Arc::new(Stylesheet::load_default()),
            highlight: Vec::new(),
            node_index,
            way_index,
            drag_preview: None,
            modified: false,
        }
    }

    /// Set the OSM data for this layer
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.node_cache = compute_node_cache(&osm_data);
        let (bboxes, verts) = compute_way_tables(&osm_data, &self.node_cache);
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        self.way_index = build_way_index(&self.way_bboxes, &osm_data.ways);
        self.osm_data = Some(osm_data);
    }

    /// Commit a set of node moves: clones the current `OsmData`, applies the
    /// given `(node_id, new_lat, new_lon)` updates, marks the layer modified,
    /// and rebuilds every derived cache/index once via `set_osm_data`.
    /// No-op if this layer has no data or `moves` is empty.
    pub fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) {
        if moves.is_empty() {
            return;
        }
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        for &(id, lat, lon) in moves {
            if let Some(node) = data.nodes.get_mut(&id) {
                node.lat = lat;
                node.lon = lon;
            }
        }
        self.modified = true;
        self.set_osm_data(Arc::new(data));
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
        self.node_index = RTree::new();
        self.way_index = RTree::new();
        self.drag_preview = None;
        self.modified = false;
    }

    /// Check if this layer has data
    pub fn has_data(&self) -> bool {
        self.osm_data.is_some()
    }

    /// Replace the stylesheet used for per-feature styling.
    pub fn set_stylesheet(&mut self, stylesheet: Arc<Stylesheet>) {
        self.stylesheet = stylesheet;
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

    fn set_highlight(&mut self, features: &[FeatureRef]) {
        self.highlight = features.to_vec();
    }

    fn set_drag_preview(&mut self, node_ids: &HashSet<i64>, delta: Point<Pixels>) {
        self.drag_preview = Some((node_ids.clone(), delta));
    }

    fn clear_drag_preview(&mut self) {
        self.drag_preview = None;
    }

    fn is_modified(&self) -> bool {
        self.modified
    }

    fn node_lat_lon(&self, node_id: i64) -> Option<(f64, f64)> {
        let data = self.osm_data.as_ref()?;
        let n = data.nodes.get(&node_id)?;
        Some((n.lat, n.lon))
    }

    fn way_node_ids(&self, way_id: i64) -> Option<Vec<i64>> {
        let data = self.osm_data.as_ref()?;
        let way = data.ways.iter().find(|w| w.id == way_id)?;
        Some(way.nodes.clone())
    }

    fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) {
        OsmLayer::commit_node_moves(self, moves);
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // Node rendering moved to `render_canvas` (paint_quad) to avoid the
        // per-node GPUI element layout cost. The selection ring is drawn in
        // `render_highlight` as a canvas outline.
        Vec::new()
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(ref osm_data) = self.osm_data else { return; };

        let origin_x = bounds.origin.x;
        let origin_y = bounds.origin.y;
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

        // Ways: bbox-cull in Mercator space, then group visible ways by
        // `(color, width)` style. Each group becomes one `paint_path` call,
        // preserving the batching gains from PR #5 while honoring per-way
        // style. Vertex lookup is a single contiguous slice per way
        // (`way_vertices[i]`) — no HashMap indirection.
        //
        // Group key uses `f32::to_bits` for width so we don't need a float-
        // hashing crate.
        //
        // Each segment is emitted as two raw triangles (an oriented quad)
        // pushed directly into the group's `Path`, bypassing
        // `PathBuilder::stroke`'s Lyon tessellation pass entirely — that
        // tessellator dominated frame time (~1.3-1.8ms for ~3800 visible
        // ways, measured under a synthetic 8k-node/4k-way dataset), since it
        // reruns from scratch every repaint (e.g. every mouse-move while
        // panning). No mitered joins at segment corners; at typical way
        // widths (2-6px) this isn't visible, and it mirrors the same
        // tradeoff already made for node rendering below.
        struct WayGroup {
            color: u32,
            half_width: f32,
            path: Option<Path<Pixels>>,
            /// (min_x, max_x, min_y, max_y), accumulated by `push_segment_quad`.
            bounds_min_max: (f32, f32, f32, f32),
        }
        let mut way_groups: HashMap<(u32, u32), WayGroup> = HashMap::new();

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

            let way_tags = &osm_data.ways[i].tags;
            let style = self.stylesheet.way_style(way_tags);
            let key = (style.color, style.width.to_bits());
            let group = way_groups.entry(key).or_insert_with(|| WayGroup {
                color: style.color,
                half_width: style.width / 2.0,
                path: None,
                bounds_min_max: (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
            });

            let mut prev: Option<Point<Pixels>> = None;
            for &(node_id, mx, my) in verts {
                let mut sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) { continue; }
                if let Some((ref ids, delta)) = self.drag_preview {
                    if ids.contains(&node_id) {
                        sp += delta;
                    }
                }
                let p = point(sp.x + origin_x, sp.y + origin_y);
                if let Some(p0) = prev {
                    push_segment_quad(&mut group.path, &mut group.bounds_min_max, p0, p, group.half_width);
                }
                prev = Some(p);
            }
        }

        for (_, g) in way_groups {
            if let Some(mut path) = g.path {
                let (min_x, max_x, min_y, max_y) = g.bounds_min_max;
                path.bounds = Bounds {
                    origin: point(px(min_x), px(min_y)),
                    size: size(px(max_x - min_x), px(max_y - min_y)),
                };
                window.paint_path(path, rgb(g.color));
            }
        }

        // Nodes: iterate the flat cache (contiguous Vec) for better locality,
        // reject offscreen ones with a mercator-space AABB test, and paint
        // visible ones as filled quads on the canvas so we skip GPUI's
        // per-element layout pass. Batching nodes into a single PathBuilder
        // fill path was tried and turned out much slower — Lyon's fill
        // tessellator is not tuned for thousands of tiny rectangles.
        //
        // Per-node style comes from the stylesheet via the node's tags.
        for &(id, mx, my) in &self.node_cache.flat {
            if mx < vmin_x || mx > vmax_x || my < vmin_y || my > vmax_y {
                continue;
            }
            let mut sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) { continue; }
            if let Some((ref ids, delta)) = self.drag_preview {
                if ids.contains(&id) {
                    sp += delta;
                }
            }
            let style = match osm_data.nodes.get(&id) {
                Some(n) => self.stylesheet.node_style(&n.tags),
                None => crate::style::NodeStyle::default(),
            };
            let half = px(style.size / 2.0);
            let quad_bounds = Bounds {
                origin: point(
                    sp.x + origin_x - half,
                    sp.y + origin_y - half,
                ),
                size: size(px(style.size), px(style.size)),
            };
            window.paint_quad(fill(quad_bounds, rgb(style.color)));
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        if let Some(ref osm_data) = self.osm_data {
            vec![
                ("Nodes".to_string(), osm_data.nodes.len().to_string()),
                ("Ways".to_string(), osm_data.ways.len().to_string()),
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
                let dist = (sp - screen_pt).magnitude() as f32;
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

    fn hit_test_rect(&self, viewport: &Viewport, rect: Bounds<Pixels>) -> Vec<FeatureRef> {
        let (x1, y1) = viewport.screen_to_mercator(rect.origin);
        let (x2, y2) = viewport.screen_to_mercator(point(
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ));
        let min_x = x1.min(x2);
        let max_x = x1.max(x2);
        let min_y = y1.min(y2);
        let max_y = y1.max(y2);
        let envelope = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        let mut out = Vec::new();
        for item in self.node_index.locate_in_envelope(envelope) {
            out.push(FeatureRef {
                layer_name: self.name.clone(),
                kind: FeatureKind::Node,
                id: item.data,
            });
        }
        for item in self.way_index.locate_in_envelope(envelope) {
            out.push(FeatureRef {
                layer_name: self.name.clone(),
                kind: FeatureKind::Way,
                id: item.data,
            });
        }
        out
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
                let mut sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) { return; }
                if let Some((ref ids, delta)) = self.drag_preview {
                    if ids.contains(&feature.id) {
                        sp += delta;
                    }
                }
                let node_style = self.stylesheet.node_style(&n.tags);
                let ring_size = node_style.size * 2.0;
                let half = px(ring_size / 2.0);
                let ring_bounds = Bounds {
                    origin: point(
                        sp.x + bounds.origin.x - half,
                        sp.y + bounds.origin.y - half,
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
                            let mut sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                if let Some((ref ids, delta)) = self.drag_preview {
                                    if ids.contains(node_id) {
                                        sp += delta;
                                    }
                                }
                                pts.push(point(sp.x + origin_x, sp.y + origin_y));
                            }
                        }
                    }
                }
                if pts.len() < 2 { return; }

                let way_style = self.stylesheet.way_style(&way.tags);
                let mut builder = PathBuilder::stroke(px(way_style.width + 4.0));
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
    use gpui::{point, px, size, Bounds};
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

    #[test]
    fn hit_test_rect_selects_contained_nodes_and_fully_enclosed_ways() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        // n1 and n2 sit exactly at the viewport center (mercator-identical);
        // n3 is a full degree away, so its mercator position is far outside
        // any modest screen-space rect around the center.
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n3 = OsmNode { id: 3, lat: center_lat + 1.0, lon: center_lon + 1.0, tags: empty_tags() };
        // way_in's bbox is the (degenerate) point at the center: fully enclosed.
        let way_in = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        // way_partial's bbox spans from the center to the far node: NOT fully
        // enclosed by a modest rect around the center.
        let way_partial = OsmWay { id: 20, nodes: vec![1, 3], tags: empty_tags() };
        let data = data_with(vec![n1, n2, n3], vec![way_in, way_partial]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // A screen-space box symmetric around the viewport's center screen
        // point (400, 300) — always brackets the center's mercator position
        // regardless of zoom level.
        let rect = Bounds {
            origin: point(px(300.0), px(200.0)),
            size: size(px(200.0), px(200.0)),
        };

        let hits = layer.hit_test_rect(&viewport, rect);
        let node_ids: Vec<i64> = hits
            .iter()
            .filter(|f| f.kind == FeatureKind::Node)
            .map(|f| f.id)
            .collect();
        let way_ids: Vec<i64> = hits
            .iter()
            .filter(|f| f.kind == FeatureKind::Way)
            .map(|f| f.id)
            .collect();

        assert!(node_ids.contains(&1) && node_ids.contains(&2), "got {:?}", node_ids);
        assert!(!node_ids.contains(&3), "far node should not be selected: {:?}", node_ids);
        assert!(way_ids.contains(&10), "fully-enclosed way should be selected: {:?}", way_ids);
        assert!(!way_ids.contains(&20), "partially-overlapping way should not be selected: {:?}", way_ids);
    }

    #[test]
    fn hit_test_rect_empty_when_no_data() {
        let layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        assert!(layer.hit_test_rect(&viewport, rect).is_empty());
    }

    #[test]
    fn way_node_ids_returns_members_in_order() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let layer = OsmLayer::new_with_data("L", data);

        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        assert_eq!(layer.way_node_ids(999), None);
    }

    #[test]
    fn node_lat_lon_reflects_current_data() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data("L", data);

        assert_eq!(layer.node_lat_lon(1), Some((40.0, -74.0)));
        assert_eq!(layer.node_lat_lon(999), None);
    }

    #[test]
    fn commit_node_moves_updates_data_and_marks_modified() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(!layer.is_modified());
        layer.commit_node_moves(&[(1, 40.5, -74.5)]);

        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(updated.nodes.get(&1).map(|n| (n.lat, n.lon)), Some((40.5, -74.5)));
        // Untouched node is unaffected.
        assert_eq!(updated.nodes.get(&2).map(|n| (n.lat, n.lon)), Some((41.0, -75.0)));
    }

    #[test]
    fn commit_node_moves_empty_is_noop() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.commit_node_moves(&[]);
        assert!(!layer.is_modified());
    }

    #[test]
    fn drag_preview_does_not_mutate_data() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let mut ids = std::collections::HashSet::new();
        ids.insert(1);
        layer.set_drag_preview(&ids, point(px(50.0), px(0.0)));

        assert!(!layer.is_modified());
        let unchanged = layer.get_osm_data().unwrap();
        assert_eq!(unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)), Some((40.0, -74.0)));

        layer.clear_drag_preview();
        let still_unchanged = layer.get_osm_data().unwrap();
        assert_eq!(still_unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)), Some((40.0, -74.0)));
    }
}
