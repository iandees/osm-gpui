use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::{OsmData, OsmWay};
use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::selection::{FeatureKind, FeatureRef, HitCandidate, point_to_segment_distance};
use crate::style::{Stylesheet, NodeStyle, WayStyle};
use rstar::{RTree, AABB, primitives::{GeomWithData, Rectangle}};

const SELECTION_ACCENT: u32 = 0xFF4081;

/// Minimum on-screen length (in pixels) a way segment must accumulate before
/// it's emitted as its own quad. Segments shorter than this while zoomed out
/// are skipped (their span just gets absorbed into the next emitted
/// segment), cutting emitted vertices with no visible geometry change. The
/// way's final vertex is always emitted exactly regardless of this
/// threshold, so endpoints/junctions and hit-testing (which reads the
/// undecimated cached geometry, not this render-only path) are unaffected.
const MIN_SEGMENT_PX: f32 = 1.0;

/// Per-way axis-aligned bounding box in Web Mercator meters. Used to cull
/// offscreen ways with a cheap min/max compare against the viewport's
/// mercator-space view bounds — no trig per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WayBbox {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl WayBbox {
    /// Conservatively grow this bbox to include `(x, y)`. Never shrinks.
    fn extend(&mut self, x: f64, y: f64) {
        if x < self.min_x { self.min_x = x; }
        if x > self.max_x { self.max_x = x; }
        if y < self.min_y { self.min_y = y; }
        if y > self.max_y { self.max_y = y; }
    }
}

/// Pre-projected node coordinates (Web Mercator meters) aligned with the
/// iteration order used by the render loops. Computing this once at
/// `set_osm_data` time eliminates the per-frame `lat_lon_to_mercator` (tan+ln)
/// from every node and way vertex. Also caches each node's resolved style
/// (color/size), resolved once from the stylesheet + tags here instead of
/// every frame in the render loop.
#[derive(Debug, Clone)]
struct NodeCache {
    /// node id -> index into `flat`/`styles`. Used by the way-vertex build
    /// pass and by incremental cache updates (`commit_node_moves`).
    index_by_id: HashMap<i64, usize>,
    /// Flat list of all nodes as `(id, mercator_x, mercator_y)` for cache-
    /// friendly iteration in the node paint loop.
    flat: Vec<(i64, f64, f64)>,
    /// Resolved style per node, aligned with `flat` by index.
    styles: Vec<NodeStyle>,
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
    /// Resolved per-way style, aligned with `osm_data.ways`/`way_vertices` by
    /// index. Resolved once (here) instead of every frame in the render
    /// loop.
    way_styles: Vec<WayStyle>,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
    /// Precomputed mercator positions (+ resolved style) for every node.
    node_cache: NodeCache,
    /// Stylesheet used to pick per-feature colors/weights from OSM tags.
    stylesheet: Arc<Stylesheet>,
    /// Feature to highlight (set each frame by MapViewer).
    highlight: Vec<FeatureRef>,
    /// Spatial index of all nodes (mercator x/y -> node id), rebuilt whenever
    /// data changes. Used by box-select (`hit_test_rect`) and indexed
    /// point-click hit-testing (`hit_test`).
    node_index: RTree<GeomWithData<[f64; 2], i64>>,
    /// Spatial index of all way bounding boxes (mercator meters -> way id),
    /// rebuilt whenever data changes. `locate_in_envelope` on this index
    /// returns ways whose bbox is fully contained in the query rect, which is
    /// exactly the "fully enclosed" box-select rule for ways. Also used as a
    /// coarse candidate filter for indexed point-click hit-testing.
    way_index: RTree<GeomWithData<Rectangle<[f64; 2]>, i64>>,
    /// OSM way id -> index into `way_vertices`/`way_bboxes`/`way_styles`.
    /// Lets `hit_test` go from an R-tree candidate (way id) straight to its
    /// cached geometry without a linear scan of `osm_data.ways`.
    way_id_to_index: HashMap<i64, usize>,
    /// node id -> indices of ways (into `way_vertices`/`way_bboxes`) that
    /// reference it. Built once at load time; lets `commit_node_moves`
    /// recompute only the affected ways' caches instead of every way.
    node_to_ways: HashMap<i64, Vec<usize>>,
    /// Transient screen-space offset applied to the given node ids while
    /// rendering, for live drag feedback. Never touches `osm_data`.
    drag_preview: Option<(HashSet<i64>, Point<Pixels>)>,
    /// Whether this layer has had a committed move since it was loaded.
    modified: bool,
}

fn compute_node_cache(data: &OsmData, stylesheet: &Stylesheet) -> NodeCache {
    let mut index_by_id = HashMap::with_capacity(data.nodes.len());
    let mut flat = Vec::with_capacity(data.nodes.len());
    let mut styles = Vec::with_capacity(data.nodes.len());
    for node in data.nodes.values() {
        if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
            let (mx, my) = lat_lon_to_mercator(lat, lon);
            index_by_id.insert(node.id, flat.len());
            flat.push((node.id, mx, my));
            styles.push(stylesheet.node_style(&node.tags));
        }
    }
    NodeCache { index_by_id, flat, styles }
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

/// Build per-way bboxes, pre-projected vertex lists, and resolved styles in
/// a single pass so neither the bbox pass nor the render path has to walk
/// the node HashMap (or the stylesheet) per vertex/way.
fn compute_way_tables(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
) -> (Vec<Option<WayBbox>>, Vec<Vec<(i64, f64, f64)>>, Vec<WayStyle>) {
    let mut bboxes = Vec::with_capacity(data.ways.len());
    let mut vertices = Vec::with_capacity(data.ways.len());
    let mut styles = Vec::with_capacity(data.ways.len());
    for way in &data.ways {
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut verts = Vec::with_capacity(way.nodes.len());
        for nid in &way.nodes {
            if let Some(&idx) = node_cache.index_by_id.get(nid) {
                let (_, mx, my) = node_cache.flat[idx];
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
        styles.push(stylesheet.way_style(&way.tags));
    }
    (bboxes, vertices, styles)
}

/// node id -> OSM way id, for every way that references it. Built once at
/// data-load time so `commit_node_moves` can find exactly which ways need
/// their cached vertex/bbox tables recomputed after a node move, instead of
/// rescanning every way.
fn build_node_to_ways(ways: &[OsmWay]) -> HashMap<i64, Vec<usize>> {
    let mut map: HashMap<i64, Vec<usize>> = HashMap::new();
    for (idx, way) in ways.iter().enumerate() {
        for nid in &way.nodes {
            map.entry(*nid).or_default().push(idx);
        }
    }
    map
}

/// OSM way id -> index into `way_vertices`/`way_bboxes`/`way_styles`. Lets
/// hot paths (indexed hit-testing, incremental move updates) go straight to
/// the cached arrays instead of a linear `Vec::iter().find()` scan.
fn build_way_id_index(ways: &[OsmWay]) -> HashMap<i64, usize> {
    ways.iter().enumerate().map(|(idx, w)| (w.id, idx)).collect()
}

/// Decide which segments of an already-projected screen-space polyline to
/// actually emit: skip interior vertices until the pending segment (measured
/// from the last emitted "anchor" point) has accumulated at least `min_px`
/// screen pixels, but always emit a final segment ending at the last point
/// so way endpoints are never rounded away. Returns `(anchor_idx, end_idx)`
/// pairs indexing into `pts`.
///
/// Pure/allocation-light and independent of `Window`/`Path` so it can be
/// unit-tested directly.
fn decimate_segments(pts: &[Point<Pixels>], min_px: f32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if pts.len() < 2 {
        return out;
    }
    let min_px2 = min_px * min_px;
    let mut anchor = 0usize;
    let last = pts.len() - 1;
    for i in 1..pts.len() {
        let dx = f32::from(pts[i].x - pts[anchor].x);
        let dy = f32::from(pts[i].y - pts[anchor].y);
        let dist2 = dx * dx + dy * dy;
        if i == last || dist2 >= min_px2 {
            out.push((anchor, i));
            anchor = i;
        }
    }
    out
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
            way_styles: Vec::new(),
            layer_bbox: None,
            node_cache: NodeCache { index_by_id: HashMap::new(), flat: Vec::new(), styles: Vec::new() },
            stylesheet: Arc::new(Stylesheet::load_default()),
            highlight: Vec::new(),
            node_index: RTree::new(),
            way_index: RTree::new(),
            way_id_to_index: HashMap::new(),
            node_to_ways: HashMap::new(),
            drag_preview: None,
            modified: false,
        }
    }

    pub fn new_with_data<N: Into<String>>(name: N, osm_data: Arc<OsmData>) -> Self {
        let stylesheet = Arc::new(Stylesheet::load_default());
        let node_cache = compute_node_cache(&osm_data, &stylesheet);
        let (way_bboxes, way_vertices, way_styles) = compute_way_tables(&osm_data, &node_cache, &stylesheet);
        let layer_bbox = compute_layer_bbox(&node_cache);
        let node_index = build_node_index(&node_cache);
        let way_index = build_way_index(&way_bboxes, &osm_data.ways);
        let way_id_to_index = build_way_id_index(&osm_data.ways);
        let node_to_ways = build_node_to_ways(&osm_data.ways);
        Self {
            name: name.into(),
            visible: true,
            osm_data: Some(osm_data),
            way_bboxes,
            way_vertices,
            way_styles,
            layer_bbox,
            node_cache,
            stylesheet,
            highlight: Vec::new(),
            node_index,
            way_index,
            way_id_to_index,
            node_to_ways,
            drag_preview: None,
            modified: false,
        }
    }

    /// Set the OSM data for this layer. Rebuilds every derived cache/index
    /// from scratch — this is the path used for bulk data loads (e.g. an
    /// initial download or a full reload). `commit_node_moves` uses a
    /// cheaper incremental path instead since it only ever touches a
    /// handful of nodes/ways.
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.node_cache = compute_node_cache(&osm_data, &self.stylesheet);
        let (bboxes, verts, styles) = compute_way_tables(&osm_data, &self.node_cache, &self.stylesheet);
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.way_styles = styles;
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        self.way_index = build_way_index(&self.way_bboxes, &osm_data.ways);
        self.way_id_to_index = build_way_id_index(&osm_data.ways);
        self.node_to_ways = build_node_to_ways(&osm_data.ways);
        self.osm_data = Some(osm_data);
    }

    /// Commit a set of node moves: clones the current `OsmData`, applies the
    /// given `(node_id, new_lat, new_lon)` updates, marks the layer modified,
    /// and incrementally patches the derived caches/indices in place:
    /// - `node_cache` entries for the moved node ids only.
    /// - `way_vertices`/`way_bboxes`/`way_styles` for exactly the ways
    ///   referencing a moved node (via `node_to_ways`), not every way.
    /// - `node_index`/`way_index` via targeted remove+insert, not a full
    ///   `bulk_load`.
    /// - `layer_bbox`, extended conservatively (never shrinks).
    ///
    /// A full `OsmData` clone is still unavoidable here without deeper
    /// restructuring, but the expensive part this replaces is the
    /// *derived-cache rebuild* (rebuilding every way's vertex/bbox table and
    /// bulk-loading both R-trees from scratch), not the clone itself.
    ///
    /// No-op if this layer has no data or `moves` is empty.
    pub fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) {
        if moves.is_empty() {
            return;
        }
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();

        let mut moved_ids: Vec<i64> = Vec::with_capacity(moves.len());
        for &(id, lat, lon) in moves {
            if let Some(node) = data.nodes.get_mut(&id) {
                node.lat = lat;
                node.lon = lon;
                moved_ids.push(id);
            }
        }
        self.modified = true;
        if moved_ids.is_empty() {
            // None of the moved ids were actually present; nothing to patch.
            self.osm_data = Some(Arc::new(data));
            return;
        }

        // Ways referencing any moved node — these are the only ones whose
        // cached vertex list/bbox can possibly have changed.
        let mut touched_ways: HashSet<usize> = HashSet::new();
        for &id in &moved_ids {
            if let Some(way_idxs) = self.node_to_ways.get(&id) {
                touched_ways.extend(way_idxs.iter().copied());
            }
        }

        // -- Patch node_cache + node_index for each moved node. --
        for &id in &moved_ids {
            // Remove the stale R-tree entry (old mercator position) first —
            // we need the OLD position, which is still in node_cache here.
            if let Some(&old_idx) = self.node_cache.index_by_id.get(&id) {
                let (_, old_mx, old_my) = self.node_cache.flat[old_idx];
                self.node_index.remove(&GeomWithData::new([old_mx, old_my], id));
            }

            let Some(node) = data.nodes.get(&id) else { continue };
            match validate_coords(node.lat, node.lon) {
                Some((lat, lon)) => {
                    let (mx, my) = lat_lon_to_mercator(lat, lon);
                    let style = self.stylesheet.node_style(&node.tags);
                    if let Some(&idx) = self.node_cache.index_by_id.get(&id) {
                        self.node_cache.flat[idx] = (id, mx, my);
                        self.node_cache.styles[idx] = style;
                    } else {
                        let idx = self.node_cache.flat.len();
                        self.node_cache.flat.push((id, mx, my));
                        self.node_cache.styles.push(style);
                        self.node_cache.index_by_id.insert(id, idx);
                    }
                    self.node_index.insert(GeomWithData::new([mx, my], id));

                    // Conservatively extend layer_bbox — never shrinks, so a
                    // move away from a bbox extreme won't shrink it back
                    // (harmless: it just means slightly less aggressive
                    // off-screen culling until the next full reload).
                    match &mut self.layer_bbox {
                        Some(lb) => lb.extend(mx, my),
                        None => self.layer_bbox = Some(WayBbox { min_x: mx, max_x: mx, min_y: my, max_y: my }),
                    }
                }
                None => {
                    // New coords are invalid: drop this node from the
                    // position caches/index (mirrors compute_node_cache,
                    // which never included invalid-coordinate nodes).
                    if let Some(idx) = self.node_cache.index_by_id.remove(&id) {
                        self.node_cache.flat.remove(idx);
                        self.node_cache.styles.remove(idx);
                        // Removing from the middle of `flat` shifts every
                        // later index by one; fix up index_by_id.
                        for v in self.node_cache.index_by_id.values_mut() {
                            if *v > idx { *v -= 1; }
                        }
                    }
                }
            }
        }

        // -- Patch way_vertices/way_bboxes/way_styles + way_index for the
        // touched ways only. --
        for &way_idx in &touched_ways {
            let way = &data.ways[way_idx];

            // Remove the way's stale R-tree entry (old bbox), if any.
            if let Some(old_bbox) = self.way_bboxes[way_idx] {
                self.way_index.remove(&GeomWithData::new(
                    Rectangle::from_corners([old_bbox.min_x, old_bbox.min_y], [old_bbox.max_x, old_bbox.max_y]),
                    way.id,
                ));
            }

            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            let mut verts = Vec::with_capacity(way.nodes.len());
            for nid in &way.nodes {
                if let Some(&idx) = self.node_cache.index_by_id.get(nid) {
                    let (_, mx, my) = self.node_cache.flat[idx];
                    if mx < min_x { min_x = mx; }
                    if mx > max_x { max_x = mx; }
                    if my < min_y { min_y = my; }
                    if my > max_y { max_y = my; }
                    verts.push((*nid, mx, my));
                }
            }
            let new_bbox = if verts.is_empty() {
                None
            } else {
                Some(WayBbox { min_x, max_x, min_y, max_y })
            };
            if let Some(b) = new_bbox {
                self.way_index.insert(GeomWithData::new(
                    Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                    way.id,
                ));
                match &mut self.layer_bbox {
                    Some(lb) => { lb.extend(b.min_x, b.min_y); lb.extend(b.max_x, b.max_y); }
                    None => self.layer_bbox = Some(b),
                }
            }
            self.way_vertices[way_idx] = verts;
            self.way_bboxes[way_idx] = new_bbox;
            self.way_styles[way_idx] = self.stylesheet.way_style(&way.tags);
        }

        self.osm_data = Some(Arc::new(data));
    }

    /// Set (insert or overwrite) a single tag on one node or way this layer
    /// owns. Marks the layer modified whenever the feature is found (same
    /// precedent as `commit_node_moves`: called at all implies modified,
    /// no finer no-op distinction). Doesn't rebuild geometry caches since
    /// tags don't affect vertex positions, but DOES refresh the cached
    /// resolved style for the affected feature, since that's tag-derived.
    /// No-op if the feature isn't found.
    pub fn set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.iter_mut().find(|w| w.id == id).map(|w| &mut w.tags),
        };
        let Some(tags) = tags else { return; };
        tags.insert(key.to_string(), value.to_string());
        self.modified = true;
        self.refresh_cached_style(kind, id, &data);
        self.osm_data = Some(Arc::new(data));
    }

    /// Remove a single tag key from one node or way this layer owns. Marks
    /// the layer modified whenever the feature is found, same precedent as
    /// `set_tag`. Also refreshes the cached resolved style, same as
    /// `set_tag`. No-op if the feature isn't found.
    pub fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.iter_mut().find(|w| w.id == id).map(|w| &mut w.tags),
        };
        let Some(tags) = tags else { return; };
        tags.remove(key);
        self.modified = true;
        self.refresh_cached_style(kind, id, &data);
        self.osm_data = Some(Arc::new(data));
    }

    /// Recompute and store the cached resolved style for a single feature
    /// after its tags changed. `data` must already reflect the new tags.
    fn refresh_cached_style(&mut self, kind: FeatureKind, id: i64, data: &OsmData) {
        match kind {
            FeatureKind::Node => {
                let Some(node) = data.nodes.get(&id) else { return; };
                if let Some(&idx) = self.node_cache.index_by_id.get(&id) {
                    self.node_cache.styles[idx] = self.stylesheet.node_style(&node.tags);
                }
            }
            FeatureKind::Way => {
                let Some(way) = data.ways.iter().find(|w| w.id == id) else { return; };
                if let Some(&idx) = self.way_id_to_index.get(&id) {
                    self.way_styles[idx] = self.stylesheet.way_style(&way.tags);
                }
            }
        }
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
        self.way_styles.clear();
        self.layer_bbox = None;
        self.node_cache.index_by_id.clear();
        self.node_cache.flat.clear();
        self.node_cache.styles.clear();
        self.node_index = RTree::new();
        self.way_index = RTree::new();
        self.way_id_to_index.clear();
        self.node_to_ways.clear();
        self.drag_preview = None;
        self.modified = false;
    }

    /// Check if this layer has data
    pub fn has_data(&self) -> bool {
        self.osm_data.is_some()
    }

    /// Replace the stylesheet used for per-feature styling. Re-resolves
    /// every cached per-feature style against the new stylesheet (a full
    /// pass over the data, but this is a rare/explicit action, not a
    /// per-frame or per-drag hot path).
    pub fn set_stylesheet(&mut self, stylesheet: Arc<Stylesheet>) {
        self.stylesheet = stylesheet;
        if let Some(data) = self.osm_data.clone() {
            self.node_cache = compute_node_cache(&data, &self.stylesheet);
            let (bboxes, verts, styles) = compute_way_tables(&data, &self.node_cache, &self.stylesheet);
            self.way_bboxes = bboxes;
            self.way_vertices = verts;
            self.way_styles = styles;
            self.layer_bbox = compute_layer_bbox(&self.node_cache);
            self.node_index = build_node_index(&self.node_cache);
            self.way_index = build_way_index(&self.way_bboxes, &data.ways);
        }
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

    fn set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
        OsmLayer::set_tag(self, kind, id, key, value);
    }

    fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) {
        OsmLayer::remove_tag(self, kind, id, key);
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // Node rendering moved to `render_canvas` (paint_quad) to avoid the
        // per-node GPUI element layout cost. The selection ring is drawn in
        // `render_highlight` as a canvas outline.
        Vec::new()
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.osm_data.is_none() { return; }

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

        // Reused scratch buffer for each way's projected screen points, so
        // decimation (below) can look ahead/behind without per-way
        // allocation churn across the (potentially thousands of) visible
        // ways in a frame.
        let mut scratch_pts: Vec<Point<Pixels>> = Vec::new();

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

            // Style is resolved once per way at data-load time
            // (`compute_way_tables`/`commit_node_moves`/`set_stylesheet`),
            // not here — no stylesheet lookup or tags HashMap access in this
            // per-frame loop.
            let style = self.way_styles[i];
            let key = (style.color, style.width.to_bits());
            let group = way_groups.entry(key).or_insert_with(|| WayGroup {
                color: style.color,
                half_width: style.width / 2.0,
                path: None,
                bounds_min_max: (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
            });

            scratch_pts.clear();
            for &(node_id, mx, my) in verts {
                let mut sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) { continue; }
                if let Some((ref ids, delta)) = self.drag_preview {
                    if ids.contains(&node_id) {
                        sp += delta;
                    }
                }
                scratch_pts.push(point(sp.x + origin_x, sp.y + origin_y));
            }
            if scratch_pts.len() < 2 {
                continue;
            }

            // Decimate: skip segments under ~1 screen pixel while zoomed
            // out (no visible geometry change), but always emit a segment
            // ending at the way's final projected vertex so junctions/
            // endpoints are never rounded away. This is purely a rendering
            // optimization — `way_vertices` (read by hit-testing and
            // selection highlight) is untouched.
            for (a, b) in decimate_segments(&scratch_pts, MIN_SEGMENT_PX) {
                push_segment_quad(&mut group.path, &mut group.bounds_min_max, scratch_pts[a], scratch_pts[b], group.half_width);
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
        // Per-node style is resolved once at data-load time (see
        // `compute_node_cache`/`commit_node_moves`/`set_stylesheet`) and
        // just read here — no stylesheet call or tags HashMap lookup per
        // node per frame.
        for (idx, &(id, mx, my)) in self.node_cache.flat.iter().enumerate() {
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
            let style = self.node_cache.styles[idx];
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
        // Generous multiplier on the query envelope vs. the exact pixel
        // tolerance above: the R-tree query is only a coarse candidate
        // filter, the refinement loops below do the exact distance check,
        // so over-including candidates costs a little extra refinement work
        // but never causes a missed hit. Mirrors the box-select envelope
        // approach in `hit_test_rect`.
        const ENVELOPE_PAD_FACTOR: f32 = 4.0;

        if self.osm_data.is_none() { return Vec::new(); }

        let max_tol = NODE_TOL.max(WAY_TOL);
        let pad = px(max_tol * ENVELOPE_PAD_FACTOR);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope = AABB::from_corners(
            [ex1.min(ex2), ey1.min(ey2)],
            [ex1.max(ex2), ey1.max(ey2)],
        );

        // Phase 1: nodes within NODE_TOL. Candidates come from the point
        // R-tree (already built for box-select); refinement reads the
        // cached mercator position and projects with `mercator_to_screen`
        // (no trig) instead of `geo_to_screen`.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        for item in self.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let Some(&idx) = self.node_cache.index_by_id.get(&id) else { continue };
            let (_, mx, my) = self.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) { continue; }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= NODE_TOL {
                node_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_name: self.name.clone(),
                        kind: FeatureKind::Node,
                        id,
                    },
                    kind: FeatureKind::Node,
                    dist_px: dist,
                });
            }
        }
        if !node_hits.is_empty() {
            return node_hits;
        }

        // Phase 2: ways within WAY_TOL. Candidates come from the way-bbox
        // R-tree. Note this uses `locate_in_envelope_intersecting` (not
        // `locate_in_envelope`, which requires the candidate's bbox to be
        // fully CONTAINED in the query envelope — the right semantics for
        // box-select in `hit_test_rect`, but wrong here: a long way's bbox
        // can be much bigger than our small click-envelope even when the
        // click lands right on the way). An intersection is guaranteed
        // whenever the closest point on the way is within `pad` of the
        // click, since pad >= WAY_TOL. Refinement walks the cached
        // (already-projected-to-mercator) vertex list for exactly that way.
        let mut way_hits: Vec<HitCandidate> = Vec::new();
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else { continue };
            let verts = &self.way_vertices[way_idx];
            if verts.len() < 2 { continue; }
            let mut best = f32::INFINITY;
            let mut prev: Option<Point<Pixels>> = None;
            for &(_, mx, my) in verts {
                let sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) { continue; }
                if let Some(p0) = prev {
                    let d = point_to_segment_distance(screen_pt, p0, sp);
                    if d < best { best = d; }
                }
                prev = Some(sp);
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_name: self.name.clone(),
                        kind: FeatureKind::Way,
                        id: way_id,
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
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
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
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon - 0.001, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
        assert!(hits.iter().any(|h| h.feature.id == 10));
    }

    #[test]
    fn hit_test_no_match_returns_empty() {
        let n = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
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
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon, version: 1, tags: empty_tags() };
        let n3 = OsmNode { id: 3, lat: center_lat + 1.0, lon: center_lon + 1.0, version: 1, tags: empty_tags() };
        // way_in's bbox is the (degenerate) point at the center: fully enclosed.
        let way_in = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        // way_partial's bbox spans from the center to the far node: NOT fully
        // enclosed by a modest rect around the center.
        let way_partial = OsmWay { id: 20, nodes: vec![1, 3], version: 1, tags: empty_tags() };
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
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let layer = OsmLayer::new_with_data("L", data);

        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        assert_eq!(layer.way_node_ids(999), None);
    }

    #[test]
    fn node_lat_lon_reflects_current_data() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data("L", data);

        assert_eq!(layer.node_lat_lon(1), Some((40.0, -74.0)));
        assert_eq!(layer.node_lat_lon(999), None);
    }

    #[test]
    fn commit_node_moves_updates_data_and_marks_modified() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, version: 1, tags: empty_tags() };
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
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.commit_node_moves(&[]);
        assert!(!layer.is_modified());
    }

    #[test]
    fn set_tag_inserts_and_overwrites_on_node() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Node, 1, "highway", "residential");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).unwrap().tags.get("highway"),
            Some(&"residential".to_string())
        );

        layer.set_tag(FeatureKind::Node, 1, "highway", "trunk");
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).unwrap().tags.get("highway"),
            Some(&"trunk".to_string())
        );
    }

    #[test]
    fn set_tag_on_way() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Way, 10, "surface", "paved");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.ways[0].tags.get("surface"),
            Some(&"paved".to_string())
        );
    }

    #[test]
    fn set_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Node, 999, "highway", "residential");
        assert!(!layer.is_modified());
    }

    #[test]
    fn remove_tag_removes_existing_key() {
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.remove_tag(FeatureKind::Node, 1, "highway");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(updated.nodes.get(&1).unwrap().tags.get("highway"), None);
    }

    #[test]
    fn remove_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.remove_tag(FeatureKind::Node, 999, "highway");
        assert!(!layer.is_modified());
    }

    #[test]
    fn drag_preview_does_not_mutate_data() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
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

    // -- Segment decimation (`decimate_segments`) --

    #[test]
    fn decimate_segments_skips_short_interior_but_keeps_endpoints() {
        // Interior points are within 1px of each other (well under the 1.0
        // threshold); the last point is far away. Decimation should collapse
        // the tightly-clustered interior points but the emitted segments
        // must still start at the true first point and end at the true
        // last point.
        let pts = vec![
            point(px(0.0), px(0.0)),
            point(px(0.2), px(0.0)),
            point(px(0.4), px(0.0)),
            point(px(0.6), px(0.0)),
            point(px(100.0), px(0.0)),
        ];
        let segments = super::decimate_segments(&pts, 1.0);
        assert_eq!(segments.first().unwrap().0, 0, "first segment must anchor at the true first vertex");
        assert_eq!(segments.last().unwrap().1, pts.len() - 1, "last segment must end at the true last vertex");
        assert!(
            segments.len() < pts.len() - 1,
            "expected fewer emitted segments than raw vertex count: {:?}",
            segments
        );

        // The endpoint screen positions among the emitted segments must
        // exactly match the undecimated (raw) projected positions — no
        // rounding/averaging, decimation only skips emitting some segments.
        let (first_anchor, _) = segments[0];
        assert_eq!(pts[first_anchor], pts[0]);
        let (_, last_end) = *segments.last().unwrap();
        assert_eq!(pts[last_end], pts[pts.len() - 1]);
    }

    #[test]
    fn decimate_segments_no_decimation_when_all_far_apart() {
        let pts = vec![
            point(px(0.0), px(0.0)),
            point(px(10.0), px(0.0)),
            point(px(20.0), px(0.0)),
        ];
        let segments = super::decimate_segments(&pts, 1.0);
        assert_eq!(segments, vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn decimate_segments_two_points_always_emits_even_if_close() {
        let pts = vec![point(px(0.0), px(0.0)), point(px(0.1), px(0.0))];
        let segments = super::decimate_segments(&pts, 1.0);
        assert_eq!(segments, vec![(0, 1)]);
    }

    #[test]
    fn decimate_segments_empty_or_single_point_emits_nothing() {
        assert!(super::decimate_segments(&[], 1.0).is_empty());
        assert!(super::decimate_segments(&[point(px(0.0), px(0.0))], 1.0).is_empty());
    }

    // -- Incremental `commit_node_moves` cache/index correctness --

    #[test]
    fn commit_node_moves_updates_both_referencing_ways_and_indices() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, tags: empty_tags() };
        let n3 = OsmNode { id: 3, lat: 40.002, lon: -74.002, tags: empty_tags() };
        // Both ways reference node 1.
        let way_a = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let way_b = OsmWay { id: 20, nodes: vec![1, 3], tags: empty_tags() };
        let data = data_with(vec![n1, n2, n3], vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let old_bbox_a = layer.way_bboxes[0];
        let old_bbox_b = layer.way_bboxes[1];

        let new_lat = 41.0;
        let new_lon = -75.0;
        layer.commit_node_moves(&[(1, new_lat, new_lon)]);

        assert!(layer.is_modified());

        // Both ways' cached bboxes must reflect the new position.
        assert_ne!(layer.way_bboxes[0], old_bbox_a, "way A's bbox should have changed");
        assert_ne!(layer.way_bboxes[1], old_bbox_b, "way B's bbox should have changed");

        // Both ways' cached vertex lists must carry node 1's new mercator
        // position.
        let (new_mx, new_my) = crate::coordinates::lat_lon_to_mercator(new_lat, new_lon);
        for verts in [&layer.way_vertices[0], &layer.way_vertices[1]] {
            let &(_, mx, my) = verts.iter().find(|&&(id, _, _)| id == 1).unwrap();
            assert!((mx - new_mx).abs() < 1e-6);
            assert!((my - new_my).abs() < 1e-6);
        }

        // hit_test at the NEW location finds node 1 (and, via node priority,
        // stops there — but the way-vertex assertions above already prove
        // the way geometry moved too).
        let viewport_new = viewport_centered_on(new_lat, new_lon);
        let hits_new = layer.hit_test(&viewport_new, point(px(400.0), px(300.0)));
        assert!(
            hits_new.iter().any(|h| h.kind == FeatureKind::Node && h.feature.id == 1),
            "expected node 1 at its new location: {:?}", hits_new
        );

        // hit_test at the OLD location must NOT find node 1 anymore.
        let viewport_old = viewport_centered_on(40.0, -74.0);
        let hits_old = layer.hit_test(&viewport_old, point(px(400.0), px(300.0)));
        assert!(
            !hits_old.iter().any(|h| h.feature.id == 1),
            "node 1 should no longer be hit-testable at its old location: {:?}", hits_old
        );
    }

    #[test]
    fn commit_node_moves_new_position_hit_testable_by_point_and_rect() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let new_lat = 50.0;
        let new_lon = -80.0;
        layer.commit_node_moves(&[(1, new_lat, new_lon)]);

        let viewport = viewport_centered_on(new_lat, new_lon);

        let point_hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(point_hits.iter().any(|h| h.feature.id == 1), "got {:?}", point_hits);

        let rect = Bounds {
            origin: point(px(300.0), px(200.0)),
            size: size(px(200.0), px(200.0)),
        };
        let rect_hits = layer.hit_test_rect(&viewport, rect);
        assert!(
            rect_hits.iter().any(|f| f.kind == FeatureKind::Node && f.id == 1),
            "got {:?}", rect_hits
        );
    }

    #[test]
    fn commit_node_moves_node_not_in_any_way_only_touches_node_cache() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, tags: empty_tags() };
        // n3 is a standalone POI node, not referenced by any way.
        let n3 = OsmNode { id: 3, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2, n3], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let way_vertices_before = layer.way_vertices.clone();
        let way_bboxes_before = layer.way_bboxes.clone();

        let new_lat = 42.0;
        let new_lon = -76.0;
        layer.commit_node_moves(&[(3, new_lat, new_lon)]);

        assert!(layer.is_modified());
        assert_eq!(layer.way_vertices, way_vertices_before, "unrelated way vertices must be untouched");
        assert_eq!(layer.way_bboxes, way_bboxes_before, "unrelated way bboxes must be untouched");

        let (mx, my) = crate::coordinates::lat_lon_to_mercator(new_lat, new_lon);
        let idx = *layer.node_cache.index_by_id.get(&3).unwrap();
        let (_, cached_mx, cached_my) = layer.node_cache.flat[idx];
        assert!((cached_mx - mx).abs() < 1e-6);
        assert!((cached_my - my).abs() < 1e-6);
    }

    // -- Cached style refresh on tag edit --

    #[test]
    fn set_tag_refreshes_cached_way_style() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let default_style = layer.way_styles[0];
        layer.set_tag(FeatureKind::Way, 10, "highway", "residential");
        let updated_style = layer.way_styles[0];

        assert_ne!(
            default_style, updated_style,
            "cached way style must be re-resolved after a tag edit, not left stale"
        );
    }

    #[test]
    fn set_tag_refreshes_cached_node_style() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let idx = *layer.node_cache.index_by_id.get(&1).unwrap();
        let default_style = layer.node_cache.styles[idx];
        layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
        let updated_style = layer.node_cache.styles[idx];

        assert_ne!(
            default_style, updated_style,
            "cached node style must be re-resolved after a tag edit, not left stale"
        );
    }
}
