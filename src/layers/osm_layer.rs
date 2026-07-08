use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::layers::diff::{diff_osm_data, LayerDiff};
use crate::layers::{EditableLayer, LayerId, MapLayer};
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::osm_upload::UploadResult;
use crate::selection::{
    point_to_segment_distance, DeletedFeatureSnapshot, FeatureKind, FeatureRef, HitCandidate,
};
use crate::style::{NodeStyle, Stylesheet, WayStyle};
use crate::viewport::Viewport;
use rstar::{
    primitives::{GeomWithData, Rectangle},
    RTree, AABB,
};

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
        if x < self.min_x {
            self.min_x = x;
        }
        if x > self.max_x {
            self.max_x = x;
        }
        if y < self.min_y {
            self.min_y = y;
        }
        if y > self.max_y {
            self.max_y = y;
        }
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
    id: LayerId,
    name: String,
    visible: bool,
    osm_data: Option<Arc<OsmData>>,
    /// The data as it was when this layer was last loaded/downloaded (or
    /// last reconciled with a successful upload) — the stable "before"
    /// state `diff_for_upload` compares `osm_data` against. Set in
    /// `new_with_data` and refreshed in `set_osm_data`/
    /// `apply_upload_result`; left untouched by in-place edit methods
    /// (`commit_node_moves`/`set_tag`/`remove_tag`), which only mutate
    /// `osm_data`.
    original_data: Option<Arc<OsmData>>,
    /// OSM way id for each entry of `way_bboxes`/`way_vertices`/`way_styles`,
    /// aligned by index. `osm_data.ways` is a `HashMap` keyed by id (not
    /// positionally ordered), so this is what lets index-aligned code (e.g.
    /// `commit_node_moves`) recover a way's id from a position in those
    /// parallel arrays.
    way_ids: Vec<i64>,
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
    /// Cached earcut triangle indices per way (into `way_vertices[i]` minus
    /// the closing duplicate), `Some` only for closed ways whose resolved
    /// style has a fill. Aligned with `way_vertices`/`way_styles` by index.
    /// See `compute_fill_tris`.
    way_fill_tris: Vec<Option<Vec<u32>>>,
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
    /// Next id to hand out to an auto-allocated new node (`create_node`
    /// called with `id: None`), always negative per OSM's not-yet-uploaded
    /// convention. Initialized from `min(existing node ids) - 1` so a
    /// reloaded, previously-saved-locally file (which might already contain
    /// negative ids) can't collide; decremented on every allocation.
    next_new_id: i64,
    /// Next id to hand out for a locally-created way (JOSM-style negative
    /// placeholder ids; no upload/remap path exists yet). Ways have no
    /// pre-existing allocator (unlike nodes' `next_new_id`/`create_node`,
    /// which already predates this), so this is a fresh counter dedicated to
    /// `add_way`.
    next_placeholder_way_id: i64,
}

/// Starting point for `next_new_id`: one below the most negative existing
/// node id, or `-1` if every existing id is non-negative (the normal case
/// for freshly downloaded OSM data).
fn compute_next_new_id(data: &OsmData) -> i64 {
    let min_existing = data.nodes.keys().copied().min().unwrap_or(0);
    if min_existing < 0 {
        min_existing - 1
    } else {
        -1
    }
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
    NodeCache {
        index_by_id,
        flat,
        styles,
    }
}

fn compute_layer_bbox(node_cache: &NodeCache) -> Option<WayBbox> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(_id, mx, my) in &node_cache.flat {
        if mx < min_x {
            min_x = mx;
        }
        if mx > max_x {
            max_x = mx;
        }
        if my < min_y {
            min_y = my;
        }
        if my > max_y {
            max_y = my;
        }
    }
    if min_x.is_finite() {
        Some(WayBbox {
            min_x,
            max_x,
            min_y,
            max_y,
        })
    } else {
        None
    }
}

/// Project one way's member nodes into mercator-space vertices (paired with
/// their node ids, so callers can match against an active `drag_preview`
/// set) and compute its bbox, both from `node_cache` in a single pass. Node
/// ids not present in `node_cache` (invalid coords, or a currently-deleted
/// node) are skipped, same as `compute_way_tables` always did. Shared by
/// `compute_way_tables` (full rebuild), `commit_node_moves` (per-touched-way
/// patch), and `restore_way` (single restored way) so the projection math
/// lives in exactly one place.
fn project_way_vertices(
    way: &OsmWay,
    node_cache: &NodeCache,
) -> (Vec<(i64, f64, f64)>, Option<WayBbox>) {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut verts = Vec::with_capacity(way.nodes.len());
    for nid in &way.nodes {
        if let Some(&idx) = node_cache.index_by_id.get(nid) {
            let (_, mx, my) = node_cache.flat[idx];
            if mx < min_x {
                min_x = mx;
            }
            if mx > max_x {
                max_x = mx;
            }
            if my < min_y {
                min_y = my;
            }
            if my > max_y {
                max_y = my;
            }
            verts.push((*nid, mx, my));
        }
    }
    let bbox = if verts.is_empty() {
        None
    } else {
        Some(WayBbox {
            min_x,
            max_x,
            min_y,
            max_y,
        })
    };
    (verts, bbox)
}

/// Per-way tables produced by [`compute_way_tables`]: parallel vectors of
/// way ids, bounding boxes, pre-projected `(node_id, x, y)` vertex lists,
/// resolved styles, and fill triangulations, all indexed identically.
type WayTables = (
    Vec<i64>,
    Vec<Option<WayBbox>>,
    Vec<Vec<(i64, f64, f64)>>,
    Vec<WayStyle>,
    Vec<Option<Vec<u32>>>,
);

/// Earcut triangle indices for a closed, fill-styled way's projected ring,
/// or `None` when the way shouldn't be filled (open ring, no fill style,
/// degenerate geometry). Indices reference `verts[..verts.len()-1]` — the
/// duplicated closing vertex is excluded. Never panics: earcut failure or
/// an empty result just means no fill. The ring check runs on the
/// *projected* vertex list, not the raw node refs — if a member node failed
/// to resolve, `verts` may be shorter and no longer ring-shaped, and the
/// node-id equality check catches that safely.
fn compute_fill_tris(verts: &[(i64, f64, f64)], style: &WayStyle) -> Option<Vec<u32>> {
    style.fill?;
    if verts.len() < 4 || verts.first()?.0 != verts.last()?.0 {
        return None;
    }
    let ring = &verts[..verts.len() - 1];
    let mut flat = Vec::with_capacity(ring.len() * 2);
    for &(_, x, y) in ring {
        flat.push(x);
        flat.push(y);
    }
    let tris = earcutr::earcut(&flat, &[], 2).ok()?;
    if tris.is_empty() {
        return None;
    }
    Some(tris.into_iter().map(|i| i as u32).collect())
}

/// Build per-way bboxes, pre-projected vertex lists, resolved styles, and
/// fill triangulations in a single pass so neither the bbox pass nor the
/// render path has to walk the node HashMap (or the stylesheet, or the
/// tessellator) per vertex/way.
fn compute_way_tables(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
) -> WayTables {
    let mut ids = Vec::with_capacity(data.ways.len());
    let mut bboxes = Vec::with_capacity(data.ways.len());
    let mut vertices = Vec::with_capacity(data.ways.len());
    let mut styles = Vec::with_capacity(data.ways.len());
    let mut fill_tris = Vec::with_capacity(data.ways.len());
    for way in sorted_ways(data) {
        let (verts, bbox) = project_way_vertices(way, node_cache);
        let style = stylesheet.way_style(&way.tags, way.is_closed());
        fill_tris.push(compute_fill_tris(&verts, &style));
        ids.push(way.id);
        bboxes.push(bbox);
        vertices.push(verts);
        styles.push(style);
    }
    (ids, bboxes, vertices, styles, fill_tris)
}

/// Recompute and return the resolved style for a single already-mutated
/// feature's tags, for the caller to store into the appropriate cache slot.
/// Shared by `set_tag`/`remove_tag` (identical "look up which cache array
/// entry this feature's style lives at" dispatch, previously duplicated).
/// Free function (not a method) so it can be called while the caller still
/// holds a live `&mut OsmData` borrowed out of `self.osm_data` via
/// `Arc::make_mut` — a `&mut self` method call wouldn't type-check there.
/// That split-borrow role is also why it takes each cache slice as its own
/// parameter rather than a struct.
#[allow(clippy::too_many_arguments)]
fn apply_style_refresh(
    kind: FeatureKind,
    id: i64,
    data: &OsmData,
    node_cache: &mut NodeCache,
    way_id_to_index: &HashMap<i64, usize>,
    way_vertices: &[Vec<(i64, f64, f64)>],
    way_styles: &mut [WayStyle],
    way_fill_tris: &mut [Option<Vec<u32>>],
    stylesheet: &Stylesheet,
) {
    match kind {
        FeatureKind::Node => {
            let Some(node) = data.nodes.get(&id) else {
                return;
            };
            if let Some(&idx) = node_cache.index_by_id.get(&id) {
                node_cache.styles[idx] = stylesheet.node_style(&node.tags);
            }
        }
        FeatureKind::Way => {
            let Some(way) = data.ways.get(&id) else {
                return;
            };
            if let Some(&idx) = way_id_to_index.get(&id) {
                let style = stylesheet.way_style(&way.tags, way.is_closed());
                way_fill_tris[idx] = compute_fill_tris(&way_vertices[idx], &style);
                way_styles[idx] = style;
            }
        }
    }
}

/// Return every way in `data.ways` sorted by id. `OsmData.ways` is a
/// `HashMap` (for O(1) lookup by id), whose iteration order is unspecified;
/// every derived cache that must line up index-for-index with another
/// derived cache (`way_bboxes`/`way_vertices`/`way_styles`/`way_id_to_index`/
/// `node_to_ways`, and the `way_index` R-tree build) is built by walking this
/// same sorted-by-id order, so rendering/iteration stays deterministic across
/// runs and reloads of the same data.
fn sorted_ways(data: &OsmData) -> Vec<&OsmWay> {
    let mut ways: Vec<&OsmWay> = data.ways.values().collect();
    ways.sort_unstable_by_key(|w| w.id);
    ways
}

/// node id -> OSM way id, for every way that references it. Built once at
/// data-load time so `commit_node_moves` can find exactly which ways need
/// their cached vertex/bbox tables recomputed after a node move, instead of
/// rescanning every way.
fn build_node_to_ways(ways: &[&OsmWay]) -> HashMap<i64, Vec<usize>> {
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
fn build_way_id_index(ways: &[&OsmWay]) -> HashMap<i64, usize> {
    ways.iter()
        .enumerate()
        .map(|(idx, w)| (w.id, idx))
        .collect()
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
        if x < *min_x {
            *min_x = x;
        }
        if x > *max_x {
            *max_x = x;
        }
        if y < *min_y {
            *min_y = y;
        }
        if y > *max_y {
            *max_y = y;
        }
    }

    let a = point(px(ax), px(ay));
    let b = point(px(bx), px(by));
    let c = point(px(cx), px(cy));
    let d = point(px(dxp), px(dyp));
    let st = point(0., 1.);

    let p = path.get_or_insert_with(|| Path::new(a));
    p.vertices.push(PathVertex {
        xy_position: a,
        st_position: st,
        content_mask: Default::default(),
    });
    p.vertices.push(PathVertex {
        xy_position: b,
        st_position: st,
        content_mask: Default::default(),
    });
    p.vertices.push(PathVertex {
        xy_position: c,
        st_position: st,
        content_mask: Default::default(),
    });
    p.vertices.push(PathVertex {
        xy_position: a,
        st_position: st,
        content_mask: Default::default(),
    });
    p.vertices.push(PathVertex {
        xy_position: c,
        st_position: st,
        content_mask: Default::default(),
    });
    p.vertices.push(PathVertex {
        xy_position: d,
        st_position: st,
        content_mask: Default::default(),
    });
}

/// Push one solid triangle into `path`, creating the path on first use and
/// widening `bounds_min_max`. Same raw-vertex batching approach as
/// `push_segment_quad` — no Lyon tessellation.
fn push_triangle(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    a: Point<Pixels>,
    b: Point<Pixels>,
    c: Point<Pixels>,
) {
    let (min_x, max_x, min_y, max_y) = bounds_min_max;
    for p in [a, b, c] {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        if x < *min_x {
            *min_x = x;
        }
        if x > *max_x {
            *max_x = x;
        }
        if y < *min_y {
            *min_y = y;
        }
        if y > *max_y {
            *max_y = y;
        }
    }
    let st = point(0., 1.);
    let p = path.get_or_insert_with(|| Path::new(a));
    for xy in [a, b, c] {
        p.vertices.push(PathVertex {
            xy_position: xy,
            st_position: st,
            content_mask: Default::default(),
        });
    }
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
fn build_way_index(
    way_bboxes: &[Option<WayBbox>],
    ways: &[&OsmWay],
) -> RTree<GeomWithData<Rectangle<[f64; 2]>, i64>> {
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
    pub fn new(id: LayerId) -> Self {
        Self {
            id,
            name: "OSM Data".to_string(),
            visible: true,
            osm_data: None,
            original_data: None,
            way_ids: Vec::new(),
            way_bboxes: Vec::new(),
            way_vertices: Vec::new(),
            way_styles: Vec::new(),
            way_fill_tris: Vec::new(),
            layer_bbox: None,
            node_cache: NodeCache {
                index_by_id: HashMap::new(),
                flat: Vec::new(),
                styles: Vec::new(),
            },
            stylesheet: Arc::new(Stylesheet::load_default()),
            highlight: Vec::new(),
            node_index: RTree::new(),
            way_index: RTree::new(),
            way_id_to_index: HashMap::new(),
            node_to_ways: HashMap::new(),
            drag_preview: None,
            modified: false,
            next_new_id: -1,
            next_placeholder_way_id: -1,
        }
    }

    pub fn new_with_data<N: Into<String>>(id: LayerId, name: N, osm_data: Arc<OsmData>) -> Self {
        let stylesheet = Arc::new(Stylesheet::load_default());
        let node_cache = compute_node_cache(&osm_data, &stylesheet);
        let (way_ids, way_bboxes, way_vertices, way_styles, way_fill_tris) =
            compute_way_tables(&osm_data, &node_cache, &stylesheet);
        let layer_bbox = compute_layer_bbox(&node_cache);
        let node_index = build_node_index(&node_cache);
        let ways_sorted = sorted_ways(&osm_data);
        let way_index = build_way_index(&way_bboxes, &ways_sorted);
        let way_id_to_index = build_way_id_index(&ways_sorted);
        let node_to_ways = build_node_to_ways(&ways_sorted);
        let next_new_id = compute_next_new_id(&osm_data);
        Self {
            id,
            name: name.into(),
            visible: true,
            osm_data: Some(osm_data.clone()),
            original_data: Some(osm_data),
            way_ids,
            way_bboxes,
            way_vertices,
            way_styles,
            way_fill_tris,
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
            next_new_id,
            next_placeholder_way_id: -1,
        }
    }

    /// Set the OSM data for this layer. Rebuilds every derived cache/index
    /// from scratch — this is the path used for bulk data loads (e.g. an
    /// initial download or a full reload). `commit_node_moves` uses a
    /// cheaper incremental path instead since it only ever touches a
    /// handful of nodes/ways.
    ///
    /// Also resets `original_data` to this new snapshot and clears
    /// `modified`: this data is by definition the new "known-good, matches
    /// the server" baseline, whether it arrived via a fresh download/reload
    /// or via `apply_upload_result` reconciling a successful upload.
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.node_cache = compute_node_cache(&osm_data, &self.stylesheet);
        let (ids, bboxes, verts, styles, fill_tris) =
            compute_way_tables(&osm_data, &self.node_cache, &self.stylesheet);
        self.way_ids = ids;
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.way_styles = styles;
        self.way_fill_tris = fill_tris;
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        let ways_sorted = sorted_ways(&osm_data);
        self.way_index = build_way_index(&self.way_bboxes, &ways_sorted);
        self.way_id_to_index = build_way_id_index(&ways_sorted);
        self.node_to_ways = build_node_to_ways(&ways_sorted);
        self.next_new_id = compute_next_new_id(&osm_data);
        self.osm_data = Some(osm_data.clone());
        self.original_data = Some(osm_data);
        self.modified = false;
    }

    /// Compute the diff between this layer's original (last-synced)
    /// snapshot and its current data — see `diff::diff_osm_data`. Returns
    /// an all-empty `LayerDiff` if there's no data or no original snapshot
    /// captured (the latter shouldn't happen in practice, since both
    /// `new_with_data` and `set_osm_data` always set one, but is handled
    /// defensively by treating everything current as newly created).
    pub fn diff_for_upload(&self) -> LayerDiff {
        let Some(current) = &self.osm_data else {
            return LayerDiff::default();
        };
        match &self.original_data {
            Some(original) => diff_osm_data(original, current),
            None => {
                let empty = OsmData {
                    nodes: HashMap::new(),
                    ways: HashMap::new(),
                    relations: Vec::new(),
                    bounds: None,
                };
                diff_osm_data(&empty, current)
            }
        }
    }

    /// Reconcile this layer's data with a successful changeset upload:
    /// remap any locally-created node/way ids (including in a way's `nodes`
    /// list, so a way that referenced a newly-created node's placeholder id
    /// doesn't end up pointing at a now-meaningless id) to their real
    /// server-assigned ids, and update every affected node/way's `version`.
    /// Then rebuilds every derived cache from scratch via `set_osm_data`
    /// (which also resets `original_data` to this reconciled state and
    /// clears `modified`).
    ///
    /// A full rebuild — rather than incrementally patching each cache like
    /// `commit_node_moves` does — is used here deliberately: this runs once
    /// per upload (not a per-frame/per-drag hot path), and a bulk id remap
    /// can touch an unbounded number of nodes/ways/index entries, so the
    /// incremental-patch discipline used elsewhere isn't worth the added
    /// complexity/risk for a one-time reconciliation pass.
    pub fn apply_upload_result(&mut self, result: &UploadResult) {
        let Some(current) = self.osm_data.clone() else {
            return;
        };
        let mut data = (*current).clone();

        // Remap node ids (covers true creates, where old id != new id, and
        // version-only updates for modified nodes, where old id == new id).
        let mut node_id_map: HashMap<i64, i64> = HashMap::new();
        let mut updates: Vec<(i64, i64, i32)> = Vec::new();
        for (&old_id, &(new_id, new_version)) in &result.node_id_remap {
            if data.nodes.contains_key(&old_id) {
                node_id_map.insert(old_id, new_id);
                updates.push((old_id, new_id, new_version));
            }
        }
        for (old_id, new_id, new_version) in updates {
            if let Some(mut node) = data.nodes.remove(&old_id) {
                node.id = new_id;
                node.version = new_version;
                data.nodes.insert(new_id, node);
            }
        }
        if !node_id_map.is_empty() {
            for way in data.ways.values_mut() {
                for nid in &mut way.nodes {
                    if let Some(&new_id) = node_id_map.get(nid) {
                        *nid = new_id;
                    }
                }
            }
        }

        // Remap way ids/versions (same create-vs-modify unification as
        // nodes). Since a way's id is also its `data.ways` key, this must
        // remove-then-reinsert under the new id rather than mutating
        // `way.id` in place through `values_mut()`, which would leave the
        // entry keyed under the stale id.
        let way_updates: Vec<(i64, i64, i32)> = data
            .ways
            .keys()
            .filter_map(|&old_id| {
                result
                    .way_id_remap
                    .get(&old_id)
                    .map(|&(new_id, new_version)| (old_id, new_id, new_version))
            })
            .collect();
        for (old_id, new_id, new_version) in way_updates {
            if let Some(mut way) = data.ways.remove(&old_id) {
                way.id = new_id;
                way.version = new_version;
                data.ways.insert(new_id, way);
            }
        }

        self.set_osm_data(Arc::new(data));
    }

    /// Test-only helper: replace `osm_data` directly, WITHOUT touching
    /// `original_data` or rebuilding derived caches. Used to simulate
    /// node/way creation and deletion in `diff_for_upload`/
    /// `apply_upload_result` tests before `create_node`/`delete_feature`
    /// exist on `OsmLayer` — those methods will presumably do the same
    /// "mutate `osm_data`, leave `original_data` alone" thing themselves.
    #[cfg(test)]
    pub(crate) fn set_osm_data_for_test(&mut self, data: Arc<OsmData>) {
        self.osm_data = Some(data);
        self.modified = true;
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
    /// Mutates `osm_data` via `Arc::make_mut` — in place, with no dataset
    /// clone at all, as long as this is the only live `Arc<OsmData>`
    /// reference (true for every production call path: nothing outside
    /// tests holds a second clone of the layer's `Arc<OsmData>` across an
    /// edit; see `get_osm_data`'s doc comment). If some other clone *is*
    /// alive (e.g. a test snapshot taken before this call), `make_mut`
    /// transparently falls back to cloning once, preserving the old
    /// snapshot-isolation semantics that the previous always-clone code
    /// gave by construction.
    ///
    /// The expensive part this whole function replaces was always the
    /// *derived-cache rebuild* (rebuilding every way's vertex/bbox table and
    /// bulk-loading both R-trees from scratch), not the `OsmData` clone —
    /// that rebuild is still avoided here by patching only the touched
    /// nodes/ways.
    ///
    /// No-op if this layer has no data or `moves` is empty.
    pub fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) {
        if moves.is_empty() {
            return;
        }
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);

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
                self.node_index
                    .remove(&GeomWithData::new([old_mx, old_my], id));
            }

            let Some(node) = data.nodes.get(&id) else {
                continue;
            };
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
                        None => {
                            self.layer_bbox = Some(WayBbox {
                                min_x: mx,
                                max_x: mx,
                                min_y: my,
                                max_y: my,
                            })
                        }
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
                            if *v > idx {
                                *v -= 1;
                            }
                        }
                    }
                }
            }
        }

        // -- Patch way_vertices/way_bboxes/way_styles + way_index for the
        // touched ways only. --
        for &way_idx in &touched_ways {
            let way_id = self.way_ids[way_idx];
            let Some(way) = data.ways.get(&way_id) else {
                continue;
            };

            // Remove the way's stale R-tree entry (old bbox), if any.
            if let Some(old_bbox) = self.way_bboxes[way_idx] {
                self.way_index.remove(&GeomWithData::new(
                    Rectangle::from_corners(
                        [old_bbox.min_x, old_bbox.min_y],
                        [old_bbox.max_x, old_bbox.max_y],
                    ),
                    way.id,
                ));
            }

            let (verts, new_bbox) = project_way_vertices(way, &self.node_cache);
            if let Some(b) = new_bbox {
                self.way_index.insert(GeomWithData::new(
                    Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                    way.id,
                ));
                match &mut self.layer_bbox {
                    Some(lb) => {
                        lb.extend(b.min_x, b.min_y);
                        lb.extend(b.max_x, b.max_y);
                    }
                    None => self.layer_bbox = Some(b),
                }
            }
            self.way_vertices[way_idx] = verts;
            self.way_bboxes[way_idx] = new_bbox;
            let style = self.stylesheet.way_style(&way.tags, way.is_closed());
            self.way_fill_tris[way_idx] = compute_fill_tris(&self.way_vertices[way_idx], &style);
            self.way_styles[way_idx] = style;
        }
    }

    /// Set (insert or overwrite) a single tag on one node or way this layer
    /// owns. Marks the layer modified whenever the feature is found (same
    /// precedent as `commit_node_moves`: called at all implies modified,
    /// no finer no-op distinction). Doesn't rebuild geometry caches since
    /// tags don't affect vertex positions, but DOES refresh the cached
    /// resolved style for the affected feature, since that's tag-derived.
    /// No-op if the feature isn't found.
    pub fn set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.get_mut(&id).map(|w| &mut w.tags),
        };
        let Some(tags) = tags else {
            return;
        };
        tags.insert(key.to_string(), value.to_string());
        self.modified = true;
        apply_style_refresh(
            kind,
            id,
            data,
            &mut self.node_cache,
            &self.way_id_to_index,
            &self.way_vertices,
            &mut self.way_styles,
            &mut self.way_fill_tris,
            &self.stylesheet,
        );
    }

    /// Remove a single tag key from one node or way this layer owns. Marks
    /// the layer modified whenever the feature is found, same precedent as
    /// `set_tag`. Also refreshes the cached resolved style, same as
    /// `set_tag`. No-op if the feature isn't found.
    pub fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) {
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.get_mut(&id).map(|w| &mut w.tags),
        };
        let Some(tags) = tags else {
            return;
        };
        tags.remove(key);
        self.modified = true;
        apply_style_refresh(
            kind,
            id,
            data,
            &mut self.node_cache,
            &self.way_id_to_index,
            &self.way_vertices,
            &mut self.way_styles,
            &mut self.way_fill_tris,
            &self.stylesheet,
        );
    }

    /// Insert a brand-new node at `(lat, lon)`, assigning the next
    /// placeholder id. Reuses `create_node`'s existing `next_new_id`
    /// allocator/patch path (rather than a separate counter) so ids handed
    /// out through this new editing-mode entry point can never collide with
    /// ids handed out through the pre-existing `create_node`/delete-undo
    /// path. Lazily initializes empty `OsmData` if this layer had none yet
    /// (`create_node` alone refuses to allocate against a `None` data set).
    /// Marks the layer modified. Returns the new node's id.
    pub fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        if self.osm_data.is_none() {
            self.osm_data = Some(Arc::new(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: None,
            }));
        }
        self.create_node(lat, lon, None)
            .expect("osm_data was just ensured to be Some")
    }

    /// Remove a node this layer owns from `OsmData` and every derived
    /// cache/index. Unlike `delete_node` (the existing delete-with-undo
    /// path), this does NOT refuse a node still referenced by a way — it's
    /// meant to be composed with `remove_node_from_way` by callers (e.g.
    /// undo of `insert_node_into_way`) that already know what they're doing.
    /// No-op if the node isn't present.
    pub fn remove_node(&mut self, node_id: i64) {
        let Some(current) = self.osm_data.clone() else {
            return;
        };
        let mut data = (*current).clone();
        if data.nodes.remove(&node_id).is_none() {
            return;
        }
        if let Some(idx) = self.node_cache.index_by_id.remove(&node_id) {
            let (_, mx, my) = self.node_cache.flat[idx];
            self.node_index
                .remove(&GeomWithData::new([mx, my], node_id));
            self.node_cache.flat.remove(idx);
            self.node_cache.styles.remove(idx);
            for v in self.node_cache.index_by_id.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
        }
        self.node_to_ways.remove(&node_id);
        self.modified = true;
        self.osm_data = Some(Arc::new(data));
    }

    /// Insert a brand-new way referencing existing node ids (must already
    /// exist in this layer — callers create nodes with `add_node` first),
    /// assigning the next placeholder way id. Appended at the end of the
    /// index-aligned caches, mirroring `restore_way`. Marks the layer
    /// modified. Returns the new way's id.
    pub fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        let id = self.next_placeholder_way_id;
        self.next_placeholder_way_id -= 1;

        if self.osm_data.is_none() {
            self.osm_data = Some(Arc::new(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: None,
            }));
        }
        let arc = self
            .osm_data
            .as_mut()
            .expect("osm_data was just ensured to be Some");
        let data = Arc::make_mut(arc);
        let way = OsmWay {
            id,
            nodes: node_ids,
            version: 0,
            tags: tags.into_iter().collect(),
        };

        let (verts, bbox) = project_way_vertices(&way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(GeomWithData::new(
                Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way.id,
            ));
            match &mut self.layer_bbox {
                Some(lb) => {
                    lb.extend(b.min_x, b.min_y);
                    lb.extend(b.max_x, b.max_y);
                }
                None => self.layer_bbox = Some(b),
            }
        }

        let way_idx = self.way_vertices.len();
        self.way_id_to_index.insert(way.id, way_idx);
        for nid in &way.nodes {
            self.node_to_ways.entry(*nid).or_default().push(way_idx);
        }
        self.way_ids.push(way.id);
        let style = self.stylesheet.way_style(&way.tags, way.is_closed());
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
        data.ways.insert(way.id, way);

        self.modified = true;
        id
    }

    /// Remove a way this layer owns from `OsmData` and every derived cache/
    /// index. Its member nodes are untouched. No-op if the way isn't
    /// present. Mirrors `delete_way`'s incremental index-shift patching
    /// (`data.ways` is a `HashMap` and needs no shifting; the index-aligned
    /// caches do).
    pub fn remove_way(&mut self, way_id: i64) {
        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        if data.ways.remove(&way_id).is_none() {
            return;
        }

        if let Some(old_bbox) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&GeomWithData::new(
                Rectangle::from_corners(
                    [old_bbox.min_x, old_bbox.min_y],
                    [old_bbox.max_x, old_bbox.max_y],
                ),
                way_id,
            ));
        }
        self.way_ids.remove(way_idx);
        self.way_vertices.remove(way_idx);
        self.way_bboxes.remove(way_idx);
        self.way_styles.remove(way_idx);
        self.way_fill_tris.remove(way_idx);
        self.way_id_to_index.remove(&way_id);
        for v in self.way_id_to_index.values_mut() {
            if *v > way_idx {
                *v -= 1;
            }
        }
        for ways in self.node_to_ways.values_mut() {
            ways.retain(|&w| w != way_idx);
            for w in ways.iter_mut() {
                if *w > way_idx {
                    *w -= 1;
                }
            }
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());

        self.modified = true;
    }

    /// Recompute and store the vertex/bbox/style/fill-tri cache entries for
    /// the way at `way_idx`, given its current (already-mutated) node list —
    /// shared tail of `extend_way`/`insert_node_into_way`/
    /// `remove_node_from_way`, all of which mutate a way's node list then
    /// need the same re-projection. `way` is a snapshot (not borrowed from
    /// `self.osm_data`) so this can freely borrow `self.node_cache`/
    /// `self.stylesheet`/etc. after the caller's mutable borrow of
    /// `self.osm_data` has ended.
    fn refresh_way_geometry_cache(&mut self, way_idx: usize, way_id: i64, way: &OsmWay) {
        if let Some(old_bbox) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&GeomWithData::new(
                Rectangle::from_corners(
                    [old_bbox.min_x, old_bbox.min_y],
                    [old_bbox.max_x, old_bbox.max_y],
                ),
                way_id,
            ));
        }
        let (verts, bbox) = project_way_vertices(way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(GeomWithData::new(
                Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way_id,
            ));
            match &mut self.layer_bbox {
                Some(lb) => {
                    lb.extend(b.min_x, b.min_y);
                    lb.extend(b.max_x, b.max_y);
                }
                None => self.layer_bbox = Some(b),
            }
        }
        let style = self.stylesheet.way_style(&way.tags, way.is_closed());
        self.way_fill_tris[way_idx] = compute_fill_tris(&verts, &style);
        self.way_vertices[way_idx] = verts;
        self.way_bboxes[way_idx] = bbox;
        self.way_styles[way_idx] = style;
    }

    /// Append `node_id` (must already exist in this layer) to an existing
    /// way's node list, and refresh that one way's derived caches. No-op if
    /// the way isn't found.
    pub fn extend_way(&mut self, way_id: i64, node_id: i64) {
        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let Some(way) = data.ways.get_mut(&way_id) else {
            return;
        };
        way.nodes.push(node_id);
        let way_snapshot = way.clone();

        self.node_to_ways.entry(node_id).or_default().push(way_idx);
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);

        self.modified = true;
    }

    /// Create a new node at `(lat, lon)` (via `add_node`'s same allocator/
    /// patch path) and splice it into an existing way's node list at `index`
    /// (0-based, into the node list — e.g. `index = 1` inserts between the
    /// way's 1st and 2nd nodes). Returns the new node's id. No-op on the way
    /// side (the node is still created, but never spliced into any way) if
    /// the way isn't found — callers only invoke this against a way just
    /// found via hit-testing, so this should not happen in practice.
    pub fn insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64 {
        let new_id = self.add_node(lat, lon);

        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return new_id;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return new_id;
        };
        let data = Arc::make_mut(arc);
        let Some(way) = data.ways.get_mut(&way_id) else {
            return new_id;
        };
        if index > way.nodes.len() {
            return new_id;
        }
        way.nodes.insert(index, new_id);
        let way_snapshot = way.clone();

        self.node_to_ways.entry(new_id).or_default().push(way_idx);
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);

        self.modified = true;
        new_id
    }

    /// Inverse of `insert_node_into_way`: splice the node out of `way_id`'s
    /// node list at `index`, and refresh that way's derived caches. Does
    /// NOT delete the node itself — callers combine this with `remove_node`
    /// when fully undoing an insert. No-op if the way isn't found or
    /// `index` is out of bounds.
    pub fn remove_node_from_way(&mut self, way_id: i64, index: usize) {
        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let Some(way) = data.ways.get_mut(&way_id) else {
            return;
        };
        if index >= way.nodes.len() {
            return;
        }
        let removed_node_id = way.nodes.remove(index);
        let way_snapshot = way.clone();

        if let Some(way_idxs) = self.node_to_ways.get_mut(&removed_node_id) {
            way_idxs.retain(|&i| i != way_idx);
        }
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);

        self.modified = true;
    }

    /// Create a new, tag-less node at `(lat, lon)`. If `id` is `None`, a
    /// fresh negative (not-yet-uploaded) id is allocated via `next_new_id`;
    /// if `Some(id)` is given (the redo path, so a recreated node reuses its
    /// original id), the call is refused (`None`) if a node with that id
    /// already exists. Incrementally patches `node_cache`/`node_index`
    /// exactly the way `commit_node_moves` does when it discovers a node
    /// needs a fresh cache entry — no full rebuild. No-op (`None`) if this
    /// layer has no data loaded at all.
    pub fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Option<i64> {
        self.osm_data.as_ref()?;
        let new_id = match id {
            Some(id) => {
                if self
                    .osm_data
                    .as_ref()
                    .is_some_and(|d| d.nodes.contains_key(&id))
                {
                    return None;
                }
                id
            }
            None => {
                let id = self.next_new_id;
                self.next_new_id -= 1;
                id
            }
        };
        self.insert_node(new_id, lat, lon, HashMap::new());
        Some(new_id)
    }

    /// Insert (or overwrite) a node with `id`/`lat`/`lon`/`tags` into
    /// `osm_data.nodes` and incrementally patch `node_cache`/`node_index`/
    /// `layer_bbox`, mirroring the "new cache entry" branch of
    /// `commit_node_moves`. Keeps `next_new_id` below `id` so a later
    /// auto-allocation never collides with an explicitly-inserted id (e.g.
    /// from a redo). Marks the layer modified. No-op if this layer has no
    /// data loaded.
    fn insert_node(&mut self, id: i64, lat: f64, lon: f64, tags: HashMap<String, String>) {
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let node = OsmNode {
            id,
            lat,
            lon,
            version: 0,
            tags,
        };
        data.nodes.insert(id, node.clone());

        if let Some((vlat, vlon)) = validate_coords(node.lat, node.lon) {
            let (mx, my) = lat_lon_to_mercator(vlat, vlon);
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
            match &mut self.layer_bbox {
                Some(lb) => lb.extend(mx, my),
                None => {
                    self.layer_bbox = Some(WayBbox {
                        min_x: mx,
                        max_x: mx,
                        min_y: my,
                        max_y: my,
                    })
                }
            }
        }

        if id <= self.next_new_id {
            self.next_new_id = id - 1;
        }
        self.modified = true;
    }

    /// Delete a node or way this layer owns. Dispatches to `delete_node`/
    /// `delete_way` — see each for its specific rules. Returns a snapshot
    /// sufficient to restore the feature via `restore_feature`, or `None` if
    /// nothing was deleted.
    pub fn delete_feature(&mut self, kind: FeatureKind, id: i64) -> Option<DeletedFeatureSnapshot> {
        match kind {
            FeatureKind::Node => self.delete_node(id),
            FeatureKind::Way => self.delete_way(id),
        }
    }

    /// Delete a standalone node.
    ///
    /// **v1 limitation:** refuses (`None`) to delete a node that's still
    /// referenced by any way (checked via `node_to_ways`), rather than also
    /// editing the way or breaking its geometry. Real editors (JOSM/iD)
    /// offer to delete the way too, or warn specifically about this; that's
    /// future work here. Callers wanting to delete such a node must delete
    /// the referencing way(s) first.
    ///
    /// On success, incrementally removes the node from `node_cache`/
    /// `node_index` (fixing up shifted indices exactly like
    /// `commit_node_moves`'s invalid-coordinate branch) and marks the layer
    /// modified.
    fn delete_node(&mut self, id: i64) -> Option<DeletedFeatureSnapshot> {
        if self
            .node_to_ways
            .get(&id)
            .is_some_and(|ways| !ways.is_empty())
        {
            return None;
        }
        let arc = self.osm_data.as_mut()?;
        let data = Arc::make_mut(arc);
        let node = data.nodes.remove(&id)?;

        if let Some(idx) = self.node_cache.index_by_id.remove(&id) {
            let (_, mx, my) = self.node_cache.flat[idx];
            self.node_index.remove(&GeomWithData::new([mx, my], id));
            self.node_cache.flat.remove(idx);
            self.node_cache.styles.remove(idx);
            for v in self.node_cache.index_by_id.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
        }
        self.node_to_ways.remove(&id);
        self.modified = true;

        let snapshot = DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id,
            tags: node.tags.into_iter().collect(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((node.lat, node.lon)),
        };
        Some(snapshot)
    }

    /// Delete a way's own record (tags + ordered node-id list) but leave its
    /// member nodes in place as ordinary standalone nodes — deleting a way
    /// never cascades to delete shared nodes, matching common editor
    /// behavior. Incrementally removes the way's entries from
    /// `way_vertices`/`way_bboxes`/`way_styles`/`way_index`/`way_id_to_index`
    /// and drops this way's index from every node's `node_to_ways` entry
    /// (shifting every index greater than the removed way's, since removing
    /// from the middle of the index-aligned `way_ids`/`way_vertices`/
    /// `way_bboxes`/`way_styles` arrays shifts everything after it; `data.ways`
    /// itself is a `HashMap` and needs no shifting). Marks the layer modified.
    /// `None` if the way isn't found.
    fn delete_way(&mut self, id: i64) -> Option<DeletedFeatureSnapshot> {
        let way_idx = *self.way_id_to_index.get(&id)?;
        let arc = self.osm_data.as_mut()?;
        let data = Arc::make_mut(arc);
        let way = data.ways.remove(&id)?;

        if let Some(old_bbox) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&GeomWithData::new(
                Rectangle::from_corners(
                    [old_bbox.min_x, old_bbox.min_y],
                    [old_bbox.max_x, old_bbox.max_y],
                ),
                id,
            ));
        }
        self.way_ids.remove(way_idx);
        self.way_vertices.remove(way_idx);
        self.way_bboxes.remove(way_idx);
        self.way_styles.remove(way_idx);
        self.way_fill_tris.remove(way_idx);
        self.way_id_to_index.remove(&id);
        for v in self.way_id_to_index.values_mut() {
            if *v > way_idx {
                *v -= 1;
            }
        }
        for ways in self.node_to_ways.values_mut() {
            ways.retain(|&w| w != way_idx);
            for w in ways.iter_mut() {
                if *w > way_idx {
                    *w -= 1;
                }
            }
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());
        self.modified = true;

        let snapshot = DeletedFeatureSnapshot {
            kind: FeatureKind::Way,
            id,
            tags: way.tags.into_iter().collect(),
            way_nodes: way.nodes,
            node_lat_lon: None,
        };
        Some(snapshot)
    }

    /// Re-insert a feature previously removed by `delete_feature`, using
    /// exactly the id/tags/geometry captured in `snapshot`. A restored way
    /// is appended at the end of `osm_data.ways` (not necessarily its
    /// original position) — draw/iteration order isn't semantically
    /// meaningful here, only correctness of the restored data and caches.
    /// No-op if a feature with that id already exists (defensive; shouldn't
    /// happen in the normal undo/redo flow).
    pub fn restore_feature(&mut self, snapshot: DeletedFeatureSnapshot) {
        match snapshot.kind {
            FeatureKind::Node => {
                let Some((lat, lon)) = snapshot.node_lat_lon else {
                    return;
                };
                if self
                    .osm_data
                    .as_ref()
                    .is_some_and(|d| d.nodes.contains_key(&snapshot.id))
                {
                    return;
                }
                self.insert_node(snapshot.id, lat, lon, snapshot.tags.into_iter().collect());
            }
            FeatureKind::Way => self.restore_way(snapshot),
        }
    }

    /// `restore_feature`'s way case: rebuilds the way's vertex list/bbox from
    /// whatever member nodes are currently cached (nodes that were also
    /// deleted and not yet restored simply won't contribute a vertex, same
    /// as `compute_way_tables` skipping unresolvable node ids), and adds this
    /// way's fresh index to each member node's `node_to_ways` entry.
    fn restore_way(&mut self, snapshot: DeletedFeatureSnapshot) {
        if self.way_id_to_index.contains_key(&snapshot.id) {
            return;
        }
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let way = OsmWay {
            id: snapshot.id,
            nodes: snapshot.way_nodes,
            version: 0,
            tags: snapshot.tags.into_iter().collect(),
        };

        let (verts, bbox) = project_way_vertices(&way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(GeomWithData::new(
                Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way.id,
            ));
            match &mut self.layer_bbox {
                Some(lb) => {
                    lb.extend(b.min_x, b.min_y);
                    lb.extend(b.max_x, b.max_y);
                }
                None => self.layer_bbox = Some(b),
            }
        }

        let way_idx = self.way_vertices.len();
        self.way_id_to_index.insert(way.id, way_idx);
        for nid in &way.nodes {
            self.node_to_ways.entry(*nid).or_default().push(way_idx);
        }
        self.way_ids.push(way.id);
        let style = self.stylesheet.way_style(&way.tags, way.is_closed());
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
        data.ways.insert(way.id, way);
        self.modified = true;
    }

    /// Get the OSM data from this layer, as a snapshot `Arc`.
    ///
    /// The returned `Arc<OsmData>` is a stable snapshot: layer edits
    /// (`commit_node_moves`, `set_tag`, `delete_feature`, ...) go through
    /// `Arc::make_mut`, which clones the dataset once if — and only if — a
    /// snapshot like this one is still alive, so holders always keep seeing
    /// the pre-edit state, never a mutation under their feet. Conversely,
    /// while nobody holds a snapshot (the normal in-app case: no production
    /// code path currently retains one across an edit — only tests do),
    /// edits mutate in place with no dataset clone at all. Keeping
    /// snapshots short-lived is therefore also what keeps edits cheap.
    pub fn get_osm_data(&self) -> Option<Arc<OsmData>> {
        self.osm_data.clone()
    }

    /// Clear the OSM data
    pub fn clear_osm_data(&mut self) {
        self.osm_data = None;
        self.original_data = None;
        self.way_ids.clear();
        self.way_bboxes.clear();
        self.way_vertices.clear();
        self.way_styles.clear();
        self.way_fill_tris.clear();
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
        self.next_new_id = -1;
        self.next_placeholder_way_id = -1;
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
            let (ids, bboxes, verts, styles, fill_tris) =
                compute_way_tables(&data, &self.node_cache, &self.stylesheet);
            self.way_ids = ids;
            self.way_bboxes = bboxes;
            self.way_vertices = verts;
            self.way_styles = styles;
            self.way_fill_tris = fill_tris;
            self.layer_bbox = compute_layer_bbox(&self.node_cache);
            self.node_index = build_node_index(&self.node_cache);
            self.way_index = build_way_index(&self.way_bboxes, &sorted_ways(&data));
        }
    }

    /// The screen-space offset to apply to `node_id`'s projected position
    /// this frame, if it's part of an active `drag_preview` — zero
    /// otherwise. Shared by every render/highlight path that projects node
    /// positions (`render_canvas`'s way and node passes, `render_highlight`'s
    /// node and way cases), which previously each repeated the same
    /// "`drag_preview` set-membership + offset" check inline.
    fn drag_preview_offset(&self, node_id: i64) -> Point<Pixels> {
        match &self.drag_preview {
            Some((ids, delta)) if ids.contains(&node_id) => *delta,
            _ => Point::default(),
        }
    }

    /// Find the way segment nearest `screen_pt`, within `tol_px`, returning
    /// `(way_id, node_id_a, node_id_b, segment_index)` for its two endpoints
    /// (in the way's node-list order) if within tolerance. Used by Extrude
    /// mode, which needs the segment's endpoints rather than just a
    /// `FeatureRef` to the whole way (unlike `hit_test`).
    pub fn hit_test_segment(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<(i64, i64, i64, usize)> {
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize)> = None;
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.way_vertices[way_idx];
            for i in 0..verts.len().saturating_sub(1) {
                let (id_a, ax, ay) = verts[i];
                let (id_b, bx, by) = verts[i + 1];
                let sp_a = viewport.mercator_to_screen(ax, ay);
                let sp_b = viewport.mercator_to_screen(bx, by);
                if !is_point_valid(sp_a) || !is_point_valid(sp_b) {
                    continue;
                }
                let d = point_to_segment_distance(screen_pt, sp_a, sp_b);
                if d <= tol_px && best.as_ref().is_none_or(|&(bd, ..)| d < bd) {
                    best = Some((d, way_id, id_a, id_b, i));
                }
            }
        }
        best.map(|(_, way_id, a, b, idx)| (way_id, a, b, idx))
    }
}

impl MapLayer for OsmLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn is_modified(&self) -> bool {
        self.modified
    }

    fn as_editable(&self) -> Option<&dyn EditableLayer> {
        Some(self)
    }

    fn as_editable_mut(&mut self) -> Option<&mut dyn EditableLayer> {
        Some(self)
    }

    fn diff_for_upload(&self) -> LayerDiff {
        OsmLayer::diff_for_upload(self)
    }

    fn apply_upload_result(&mut self, result: &UploadResult) {
        OsmLayer::apply_upload_result(self, result);
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

        let origin_x = bounds.origin.x;
        let origin_y = bounds.origin.y;
        // Mercator-space view AABB. Culling and projection both happen in
        // this space so nothing in the hot loop touches trig.
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();

        // Layer-level early-out: if this layer's entire footprint is
        // off-screen, skip all per-vertex work.
        if let Some(lb) = &self.layer_bbox {
            if lb.max_x < vmin_x || lb.min_x > vmax_x || lb.max_y < vmin_y || lb.min_y > vmax_y {
                return;
            }
        }

        // Area fills: paint before strokes so outlines and nodes stay on
        // top. Triangulation is cached (`way_fill_tris`, computed at
        // load/edit time); per-frame work is projection + batched
        // raw-triangle emission, grouped by RGBA color — mirroring the
        // stroke batching below.
        struct FillGroup {
            rgba: u32,
            path: Option<Path<Pixels>>,
            /// (min_x, max_x, min_y, max_y), accumulated by `push_triangle`.
            bounds_min_max: (f32, f32, f32, f32),
        }
        // Insertion-ordered (first-encounter order) instead of a HashMap so
        // flush order is deterministic across frames — HashMap iteration
        // order is randomized per-process, which caused visible z-order
        // flicker between overlapping fills of different colors.
        let mut fill_group_index: HashMap<u32, usize> = HashMap::new();
        let mut fill_groups: Vec<FillGroup> = Vec::new();
        let mut fill_pts: Vec<Point<Pixels>> = Vec::new();

        for (i, tris) in self.way_fill_tris.iter().enumerate() {
            let Some(tris) = tris else { continue };
            let Some(fill) = self.way_styles[i].fill else {
                continue;
            };
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

            let verts = &self.way_vertices[i];
            let ring_len = verts.len() - 1; // closing duplicate excluded
            fill_pts.clear();
            for &(node_id, mx, my) in &verts[..ring_len] {
                let mut sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) {
                    break;
                }
                sp += self.drag_preview_offset(node_id);
                fill_pts.push(point(sp.x + origin_x, sp.y + origin_y));
            }
            // Any invalid projection breaks index alignment with the cached
            // triangle indices — skip the fill (the stroke still draws).
            if fill_pts.len() != ring_len {
                continue;
            }

            let alpha = (fill.opacity * 255.0).round() as u32;
            let rgba_key = (fill.color << 8) | alpha;
            let group_idx = *fill_group_index.entry(rgba_key).or_insert_with(|| {
                fill_groups.push(FillGroup {
                    rgba: rgba_key,
                    path: None,
                    bounds_min_max: (
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                    ),
                });
                fill_groups.len() - 1
            });
            let group = &mut fill_groups[group_idx];
            for t in tris.chunks_exact(3) {
                push_triangle(
                    &mut group.path,
                    &mut group.bounds_min_max,
                    fill_pts[t[0] as usize],
                    fill_pts[t[1] as usize],
                    fill_pts[t[2] as usize],
                );
            }
        }

        for g in fill_groups {
            if let Some(mut path) = g.path {
                let (min_x, max_x, min_y, max_y) = g.bounds_min_max;
                path.bounds = Bounds {
                    origin: point(px(min_x), px(min_y)),
                    size: size(px(max_x - min_x), px(max_y - min_y)),
                };
                window.paint_path(path, rgba(g.rgba));
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
        // Same insertion-ordered fix as `fill_groups` above, for the same
        // reason: deterministic first-encounter flush order instead of
        // HashMap's randomized iteration order.
        let mut way_group_index: HashMap<(u32, u32), usize> = HashMap::new();
        let mut way_groups: Vec<WayGroup> = Vec::new();

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
            let group_idx = *way_group_index.entry(key).or_insert_with(|| {
                way_groups.push(WayGroup {
                    color: style.color,
                    half_width: style.width / 2.0,
                    path: None,
                    bounds_min_max: (
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                    ),
                });
                way_groups.len() - 1
            });
            let group = &mut way_groups[group_idx];

            scratch_pts.clear();
            for &(node_id, mx, my) in verts {
                let mut sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) {
                    continue;
                }
                sp += self.drag_preview_offset(node_id);
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
                push_segment_quad(
                    &mut group.path,
                    &mut group.bounds_min_max,
                    scratch_pts[a],
                    scratch_pts[b],
                    group.half_width,
                );
            }
        }

        for g in way_groups {
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
            if !is_point_valid(sp) {
                continue;
            }
            sp += self.drag_preview_offset(id);
            let style = self.node_cache.styles[idx];
            let half = px(style.size / 2.0);
            let quad_bounds = Bounds {
                origin: point(sp.x + origin_x - half, sp.y + origin_y - half),
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
}

impl EditableLayer for OsmLayer {
    fn set_highlight(&mut self, features: &[FeatureRef]) {
        self.highlight = features.to_vec();
    }

    fn set_drag_preview(&mut self, node_ids: &HashSet<i64>, delta: Point<Pixels>) {
        self.drag_preview = Some((node_ids.clone(), delta));
    }

    fn clear_drag_preview(&mut self) {
        self.drag_preview = None;
    }

    fn node_lat_lon(&self, node_id: i64) -> Option<(f64, f64)> {
        let data = self.osm_data.as_ref()?;
        let n = data.nodes.get(&node_id)?;
        Some((n.lat, n.lon))
    }

    fn way_node_ids(&self, way_id: i64) -> Option<Vec<i64>> {
        let data = self.osm_data.as_ref()?;
        let way = data.ways.get(&way_id)?;
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

    fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Option<i64> {
        OsmLayer::create_node(self, lat, lon, id)
    }

    fn delete_feature(&mut self, kind: FeatureKind, id: i64) -> Option<DeletedFeatureSnapshot> {
        OsmLayer::delete_feature(self, kind, id)
    }

    fn restore_feature(&mut self, snapshot: DeletedFeatureSnapshot) {
        OsmLayer::restore_feature(self, snapshot);
    }

    fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        OsmLayer::add_node(self, lat, lon)
    }

    fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        OsmLayer::add_way(self, node_ids, tags)
    }

    fn extend_way(&mut self, way_id: i64, node_id: i64) {
        OsmLayer::extend_way(self, way_id, node_id);
    }

    fn insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64 {
        OsmLayer::insert_node_into_way(self, way_id, index, lat, lon)
    }

    fn remove_node(&mut self, node_id: i64) {
        OsmLayer::remove_node(self, node_id);
    }

    fn remove_way(&mut self, way_id: i64) {
        OsmLayer::remove_way(self, way_id);
    }

    fn remove_node_from_way(&mut self, way_id: i64, index: usize) {
        OsmLayer::remove_node_from_way(self, way_id, index);
    }

    fn hit_test(&self, viewport: &Viewport, screen_pt: Point<Pixels>) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = 8.0;
        const WAY_TOL: f32 = 6.0;
        // Generous multiplier on the query envelope vs. the exact pixel
        // tolerance above: the R-tree query is only a coarse candidate
        // filter, the refinement loops below do the exact distance check,
        // so over-including candidates costs a little extra refinement work
        // but never causes a missed hit. Mirrors the box-select envelope
        // approach in `hit_test_rect`.
        const ENVELOPE_PAD_FACTOR: f32 = 4.0;

        if self.osm_data.is_none() {
            return Vec::new();
        }

        let max_tol = NODE_TOL.max(WAY_TOL);
        let pad = px(max_tol * ENVELOPE_PAD_FACTOR);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        // Phase 1: nodes within NODE_TOL. Candidates come from the point
        // R-tree (already built for box-select); refinement reads the
        // cached mercator position and projects with `mercator_to_screen`
        // (no trig) instead of `geo_to_screen`.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        for item in self.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let Some(&idx) = self.node_cache.index_by_id.get(&id) else {
                continue;
            };
            let (_, mx, my) = self.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= NODE_TOL {
                node_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
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
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.way_vertices[way_idx];
            if verts.len() < 2 {
                continue;
            }
            let mut best = f32::INFINITY;
            let mut prev: Option<Point<Pixels>> = None;
            for &(_, mx, my) in verts {
                let sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) {
                    continue;
                }
                if let Some(p0) = prev {
                    let d = point_to_segment_distance(screen_pt, p0, sp);
                    if d < best {
                        best = d;
                    }
                }
                prev = Some(sp);
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
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
                layer_id: self.id,
                kind: FeatureKind::Node,
                id: item.data,
            });
        }
        for item in self.way_index.locate_in_envelope(envelope) {
            out.push(FeatureRef {
                layer_id: self.id,
                kind: FeatureKind::Way,
                id: item.data,
            });
        }
        out
    }

    fn feature_tags(&self, feature: &FeatureRef) -> Option<Vec<(String, String)>> {
        if feature.layer_id != self.id {
            return None;
        }
        let data = self.osm_data.as_ref()?;
        let tags = match feature.kind {
            FeatureKind::Node => {
                let n = data.nodes.get(&feature.id)?;
                n.tags.clone()
            }
            FeatureKind::Way => {
                let w = data.ways.get(&feature.id)?;
                w.tags.clone()
            }
        };
        let mut kv: Vec<(String, String)> = tags.into_iter().collect();
        kv.sort_by(|a, b| a.0.cmp(&b.0));
        Some(kv)
    }

    fn feature_geometry(
        &self,
        feature: &FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Option<crate::presets::Geometry> {
        if feature.layer_id != self.id {
            return None;
        }
        let data = self.osm_data.as_ref()?;
        crate::presets::classify_geometry(data, feature.kind, feature.id, area_keys)
    }

    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
    ) {
        if feature.layer_id != self.id {
            return;
        }
        let Some(ref osm_data) = self.osm_data else {
            return;
        };

        match feature.kind {
            FeatureKind::Node => {
                let Some(n) = osm_data.nodes.get(&feature.id) else {
                    return;
                };
                let Some((lat, lon)) = validate_coords(n.lat, n.lon) else {
                    return;
                };
                let mut sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) {
                    return;
                }
                sp += self.drag_preview_offset(feature.id);
                let node_style = self.stylesheet.node_style(&n.tags);
                let ring_size = node_style.size * 2.0;
                let half = px(ring_size / 2.0);
                let ring_bounds = Bounds {
                    origin: point(sp.x + bounds.origin.x - half, sp.y + bounds.origin.y - half),
                    size: size(px(ring_size), px(ring_size)),
                };
                window.paint_quad(outline(
                    ring_bounds,
                    rgb(SELECTION_ACCENT),
                    BorderStyle::Solid,
                ));
            }
            FeatureKind::Way => {
                let Some(way) = osm_data.ways.get(&feature.id) else {
                    return;
                };
                if way.nodes.len() < 2 {
                    return;
                }

                let origin_x = bounds.origin.x;
                let origin_y = bounds.origin.y;

                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
                for node_id in &way.nodes {
                    if let Some(n) = osm_data.nodes.get(node_id) {
                        if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                            let mut sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                sp += self.drag_preview_offset(*node_id);
                                pts.push(point(sp.x + origin_x, sp.y + origin_y));
                            }
                        }
                    }
                }
                if pts.len() < 2 {
                    return;
                }

                let way_style = self.stylesheet.way_style(&way.tags, way.is_closed());
                let mut builder = PathBuilder::stroke(px(way_style.width + 4.0));
                for (i, p) in pts.iter().enumerate() {
                    if i == 0 {
                        builder.move_to(*p);
                    } else {
                        builder.line_to(*p);
                    }
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
    use crate::layers::{EditableLayer, LayerId, MapLayer};
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::FeatureKind;
    use crate::style::Stylesheet;
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
        let mut way_map = HashMap::new();
        for w in ways {
            way_map.insert(w.id, w);
        }
        Arc::new(OsmData {
            nodes: map,
            ways: way_map,
            relations: Vec::new(),
            bounds: None,
        })
    }

    #[test]
    fn hit_test_node_wins_over_coincident_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, FeatureKind::Node);
        assert_eq!(hits[0].feature.id, 1);
    }

    #[test]
    fn hit_test_falls_through_to_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
        assert!(hits.iter().any(|h| h.feature.id == 10));
    }

    #[test]
    fn hit_test_no_match_returns_empty() {
        let n = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n], vec![]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hits = layer.hit_test(&viewport, point(px(50.0), px(50.0)));
        assert!(hits.is_empty(), "unexpected hits: {:?}", hits);
    }

    #[test]
    fn hit_test_segment_finds_nearest_segment_and_endpoint_indices() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hit = layer.hit_test_segment(&viewport, point(px(400.0), px(300.0)), 6.0);
        assert_eq!(hit, Some((10, 1, 2, 0)));
    }

    #[test]
    fn hit_test_segment_none_when_out_of_tolerance() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hit = layer.hit_test_segment(&viewport, point(px(0.0), px(0.0)), 6.0);
        assert!(hit.is_none());
    }

    #[test]
    fn hit_test_rect_selects_contained_nodes_and_fully_enclosed_ways() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        // n1 and n2 sit exactly at the viewport center (mercator-identical);
        // n3 is a full degree away, so its mercator position is far outside
        // any modest screen-space rect around the center.
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: empty_tags(),
        };
        let n3 = OsmNode {
            id: 3,
            lat: center_lat + 1.0,
            lon: center_lon + 1.0,
            version: 1,
            tags: empty_tags(),
        };
        // way_in's bbox is the (degenerate) point at the center: fully enclosed.
        let way_in = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        // way_partial's bbox spans from the center to the far node: NOT fully
        // enclosed by a modest rect around the center.
        let way_partial = OsmWay {
            id: 20,
            nodes: vec![1, 3],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2, n3], vec![way_in, way_partial]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

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

        assert!(
            node_ids.contains(&1) && node_ids.contains(&2),
            "got {:?}",
            node_ids
        );
        assert!(
            !node_ids.contains(&3),
            "far node should not be selected: {:?}",
            node_ids
        );
        assert!(
            way_ids.contains(&10),
            "fully-enclosed way should be selected: {:?}",
            way_ids
        );
        assert!(
            !way_ids.contains(&20),
            "partially-overlapping way should not be selected: {:?}",
            way_ids
        );
    }

    #[test]
    fn hit_test_rect_empty_when_no_data() {
        let layer = OsmLayer::new(LayerId(1));
        let viewport = viewport_centered_on(40.0, -74.0);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        assert!(layer.hit_test_rect(&viewport, rect).is_empty());
    }

    #[test]
    fn way_node_ids_returns_members_in_order() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        assert_eq!(layer.way_node_ids(999), None);
    }

    #[test]
    fn node_lat_lon_reflects_current_data() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert_eq!(layer.node_lat_lon(1), Some((40.0, -74.0)));
        assert_eq!(layer.node_lat_lon(999), None);
    }

    #[test]
    fn commit_node_moves_updates_data_and_marks_modified() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 41.0,
            lon: -75.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert!(!layer.is_modified());
        layer.commit_node_moves(&[(1, 40.5, -74.5)]);

        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((40.5, -74.5))
        );
        // Untouched node is unaffected.
        assert_eq!(
            updated.nodes.get(&2).map(|n| (n.lat, n.lon)),
            Some((41.0, -75.0))
        );
    }

    #[test]
    fn commit_node_moves_empty_is_noop() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.commit_node_moves(&[]);
        assert!(!layer.is_modified());
    }

    #[test]
    fn set_tag_inserts_and_overwrites_on_node() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

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
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.set_tag(FeatureKind::Way, 10, "surface", "paved");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.ways[&10].tags.get("surface"),
            Some(&"paved".to_string())
        );
    }

    #[test]
    fn set_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.set_tag(FeatureKind::Node, 999, "highway", "residential");
        assert!(!layer.is_modified());
    }

    #[test]
    fn remove_tag_removes_existing_key() {
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.remove_tag(FeatureKind::Node, 1, "highway");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(updated.nodes.get(&1).unwrap().tags.get("highway"), None);
    }

    #[test]
    fn remove_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.remove_tag(FeatureKind::Node, 999, "highway");
        assert!(!layer.is_modified());
    }

    #[test]
    fn drag_preview_does_not_mutate_data() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let mut ids = std::collections::HashSet::new();
        ids.insert(1);
        layer.set_drag_preview(&ids, point(px(50.0), px(0.0)));

        assert!(!layer.is_modified());
        let unchanged = layer.get_osm_data().unwrap();
        assert_eq!(
            unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((40.0, -74.0))
        );

        layer.clear_drag_preview();
        let still_unchanged = layer.get_osm_data().unwrap();
        assert_eq!(
            still_unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((40.0, -74.0))
        );
    }

    /// Mutations now go through `Arc::make_mut` instead of always deep-
    /// cloning `OsmData` — but that must not change observable data-sharing
    /// semantics. A snapshot `Arc<OsmData>` taken *before* an edit (e.g. via
    /// `get_osm_data`, exactly like an in-flight background save/export
    /// would hold) still sees the pre-edit state afterward: `make_mut`
    /// clones once, transparently, the moment it discovers a second live
    /// `Arc` reference, so old snapshots are never mutated out from under
    /// their holder. This is the same snapshot isolation the old
    /// always-clone code gave by construction.
    #[test]
    fn snapshot_taken_before_edit_is_unaffected_by_make_mut() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        // Take a snapshot Arc *before* the edit — a second live reference to
        // the same underlying OsmData, exactly the case make_mut must clone
        // for rather than mutate in place.
        let snapshot = layer.get_osm_data().unwrap();
        assert_eq!(
            snapshot.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((40.0, -74.0))
        );

        layer.commit_node_moves(&[(1, 41.0, -75.0)]);

        // The live layer sees the move...
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((41.0, -75.0))
        );
        // ...but the old snapshot, held separately, still sees the original
        // position: it was never mutated in place.
        assert_eq!(
            snapshot.nodes.get(&1).map(|n| (n.lat, n.lon)),
            Some((40.0, -74.0))
        );

        // Also true for tag edits, not just node moves.
        let snapshot2 = layer.get_osm_data().unwrap();
        layer.set_tag(FeatureKind::Node, 1, "name", "Test");
        assert_eq!(
            snapshot2.nodes.get(&1).and_then(|n| n.tags.get("name")),
            None
        );
        let updated2 = layer.get_osm_data().unwrap();
        assert_eq!(
            updated2
                .nodes
                .get(&1)
                .and_then(|n| n.tags.get("name"))
                .map(String::as_str),
            Some("Test")
        );
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
        assert_eq!(
            segments.first().unwrap().0,
            0,
            "first segment must anchor at the true first vertex"
        );
        assert_eq!(
            segments.last().unwrap().1,
            pts.len() - 1,
            "last segment must end at the true last vertex"
        );
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
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        let n3 = OsmNode {
            id: 3,
            lat: 40.002,
            lon: -74.002,
            tags: empty_tags(),
            version: 1,
        };
        // Both ways reference node 1.
        let way_a = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: empty_tags(),
            version: 1,
        };
        let way_b = OsmWay {
            id: 20,
            nodes: vec![1, 3],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2, n3], vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let old_bbox_a = layer.way_bboxes[0];
        let old_bbox_b = layer.way_bboxes[1];

        let new_lat = 41.0;
        let new_lon = -75.0;
        layer.commit_node_moves(&[(1, new_lat, new_lon)]);

        assert!(layer.is_modified());

        // Both ways' cached bboxes must reflect the new position.
        assert_ne!(
            layer.way_bboxes[0], old_bbox_a,
            "way A's bbox should have changed"
        );
        assert_ne!(
            layer.way_bboxes[1], old_bbox_b,
            "way B's bbox should have changed"
        );

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
            hits_new
                .iter()
                .any(|h| h.kind == FeatureKind::Node && h.feature.id == 1),
            "expected node 1 at its new location: {:?}",
            hits_new
        );

        // hit_test at the OLD location must NOT find node 1 anymore.
        let viewport_old = viewport_centered_on(40.0, -74.0);
        let hits_old = layer.hit_test(&viewport_old, point(px(400.0), px(300.0)));
        assert!(
            !hits_old.iter().any(|h| h.feature.id == 1),
            "node 1 should no longer be hit-testable at its old location: {:?}",
            hits_old
        );
    }

    #[test]
    fn commit_node_moves_new_position_hit_testable_by_point_and_rect() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let new_lat = 50.0;
        let new_lon = -80.0;
        layer.commit_node_moves(&[(1, new_lat, new_lon)]);

        let viewport = viewport_centered_on(new_lat, new_lon);

        let point_hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(
            point_hits.iter().any(|h| h.feature.id == 1),
            "got {:?}",
            point_hits
        );

        let rect = Bounds {
            origin: point(px(300.0), px(200.0)),
            size: size(px(200.0), px(200.0)),
        };
        let rect_hits = layer.hit_test_rect(&viewport, rect);
        assert!(
            rect_hits
                .iter()
                .any(|f| f.kind == FeatureKind::Node && f.id == 1),
            "got {:?}",
            rect_hits
        );
    }

    #[test]
    fn commit_node_moves_node_not_in_any_way_only_touches_node_cache() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        // n3 is a standalone POI node, not referenced by any way.
        let n3 = OsmNode {
            id: 3,
            lat: 41.0,
            lon: -75.0,
            tags: empty_tags(),
            version: 1,
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2, n3], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let way_vertices_before = layer.way_vertices.clone();
        let way_bboxes_before = layer.way_bboxes.clone();

        let new_lat = 42.0;
        let new_lon = -76.0;
        layer.commit_node_moves(&[(3, new_lat, new_lon)]);

        assert!(layer.is_modified());
        assert_eq!(
            layer.way_vertices, way_vertices_before,
            "unrelated way vertices must be untouched"
        );
        assert_eq!(
            layer.way_bboxes, way_bboxes_before,
            "unrelated way bboxes must be untouched"
        );

        let (mx, my) = crate::coordinates::lat_lon_to_mercator(new_lat, new_lon);
        let idx = *layer.node_cache.index_by_id.get(&3).unwrap();
        let (_, cached_mx, cached_my) = layer.node_cache.flat[idx];
        assert!((cached_mx - mx).abs() < 1e-6);
        assert!((cached_my - my).abs() < 1e-6);
    }

    // -- Cached style refresh on tag edit --

    #[test]
    fn set_tag_refreshes_cached_way_style() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

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
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let idx = *layer.node_cache.index_by_id.get(&1).unwrap();
        let default_style = layer.node_cache.styles[idx];
        layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
        let updated_style = layer.node_cache.styles[idx];

        assert_ne!(
            default_style, updated_style,
            "cached node style must be re-resolved after a tag edit, not left stale"
        );
    }

    // -- Fill triangulation cache --

    fn square_ring_data() -> Arc<OsmData> {
        // A small square around (40, -74): nodes 1-4, way 10 closed via ref 1.
        let mk = |id, dlat: f64, dlon: f64| OsmNode {
            id,
            lat: 40.0 + dlat,
            lon: -74.0 + dlon,
            version: 1,
            tags: empty_tags(),
        };
        let mut way_tags = HashMap::new();
        way_tags.insert("building".to_string(), "yes".to_string());
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 4, 1],
            version: 1,
            tags: way_tags,
        };
        data_with(
            vec![
                mk(1, 0.0, 0.0),
                mk(2, 0.0, 0.001),
                mk(3, 0.001, 0.001),
                mk(4, 0.001, 0.0),
            ],
            vec![way],
        )
    }

    fn fill_stylesheet() -> Arc<Stylesheet> {
        Arc::new(
            Stylesheet::parse("area[building] { fill-color: #808080; fill-opacity: 0.4; }")
                .unwrap(),
        )
    }

    #[test]
    fn closed_way_with_fill_style_gets_triangulated() {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
        layer.set_stylesheet(fill_stylesheet());
        let tris = layer.way_fill_tris[0]
            .as_ref()
            .expect("closed building way must have fill triangles");
        // A quad triangulates into 2 triangles = 6 indices, all within the
        // 4-vertex deduped ring.
        assert_eq!(tris.len(), 6);
        assert!(tris.iter().all(|&i| i < 4));
    }

    #[test]
    fn open_way_gets_no_fill_triangles() {
        let mut data = square_ring_data();
        Arc::make_mut(&mut data).ways.get_mut(&10).unwrap().nodes = vec![1, 2, 3, 4];
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        layer.set_stylesheet(fill_stylesheet());
        assert_eq!(layer.way_fill_tris[0], None);
    }

    #[test]
    fn closed_way_without_fill_style_gets_no_triangles() {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
        layer.set_stylesheet(Arc::new(
            Stylesheet::parse("way[building] { color: #808080; }").unwrap(),
        ));
        assert_eq!(layer.way_fill_tris[0], None);
    }

    #[test]
    fn set_tag_refreshes_fill_triangles() {
        let mut data = square_ring_data();
        Arc::make_mut(&mut data)
            .ways
            .get_mut(&10)
            .unwrap()
            .tags
            .clear();
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        layer.set_stylesheet(fill_stylesheet());
        assert_eq!(layer.way_fill_tris[0], None, "no building tag yet");
        layer.set_tag(FeatureKind::Way, 10, "building", "yes");
        assert!(
            layer.way_fill_tris[0].is_some(),
            "tag edit must recompute fill"
        );
        layer.remove_tag(FeatureKind::Way, 10, "building");
        assert_eq!(layer.way_fill_tris[0], None, "tag removal must clear fill");
    }

    #[test]
    fn degenerate_ring_yields_no_fill_without_panic() {
        // All four ring nodes collinear.
        let mk = |id, dlon: f64| OsmNode {
            id,
            lat: 40.0,
            lon: -74.0 + dlon,
            version: 1,
            tags: empty_tags(),
        };
        let mut way_tags = HashMap::new();
        way_tags.insert("building".to_string(), "yes".to_string());
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 4, 1],
            version: 1,
            tags: way_tags,
        };
        let data = data_with(
            vec![mk(1, 0.0), mk(2, 0.001), mk(3, 0.002), mk(4, 0.003)],
            vec![way],
        );
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        layer.set_stylesheet(fill_stylesheet());
        assert_eq!(layer.way_fill_tris[0], None);
    }

    #[test]
    fn delete_way_shifts_fill_cache() {
        // Two ways; deleting the first must shift the second's fill entry down.
        let mut data = square_ring_data();
        {
            let d = Arc::make_mut(&mut data);
            let mut tags = HashMap::new();
            tags.insert("building".to_string(), "yes".to_string());
            d.ways.insert(
                20,
                OsmWay {
                    id: 20,
                    nodes: vec![1, 2, 3, 1],
                    version: 1,
                    tags,
                },
            );
        }
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        layer.set_stylesheet(fill_stylesheet());
        assert_eq!(layer.way_fill_tris.len(), 2);
        layer.delete_feature(FeatureKind::Way, 10);
        assert_eq!(layer.way_fill_tris.len(), 1);
        assert!(
            layer.way_fill_tris[0].is_some(),
            "surviving way 20 (a closed triangle) keeps its fill entry"
        );
    }

    // -- `create_node` / `delete_feature` / `restore_feature` --

    #[test]
    fn create_node_allocates_noncolliding_negative_id_and_is_hit_testable() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let new_lat = 40.5;
        let new_lon = -74.5;
        let id = layer
            .create_node(new_lat, new_lon, None)
            .expect("should allocate an id");
        assert!(id < 0, "new node id must be negative, got {}", id);
        assert!(layer.is_modified());

        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&id).map(|n| (n.lat, n.lon)),
            Some((new_lat, new_lon))
        );
        assert!(updated.nodes.get(&id).unwrap().tags.is_empty());

        let viewport = viewport_centered_on(new_lat, new_lon);
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(
            hits.iter()
                .any(|h| h.kind == FeatureKind::Node && h.feature.id == id),
            "got {:?}",
            hits
        );

        // A second auto-allocation must not collide with the first.
        let id2 = layer.create_node(new_lat, new_lon, None).unwrap();
        assert_ne!(id, id2);
    }

    #[test]
    fn create_node_starts_below_existing_negative_ids() {
        // Simulates a reloaded, previously-saved-locally file that already
        // contains negative (not-yet-uploaded) ids.
        let n1 = OsmNode {
            id: -5,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 0,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let id = layer.create_node(41.0, -75.0, None).unwrap();
        assert!(
            id < -5,
            "new id ({}) must not collide with existing negative id -5",
            id
        );
    }

    #[test]
    fn create_node_with_explicit_id_fails_on_collision() {
        let n1 = OsmNode {
            id: -1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 0,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert_eq!(layer.create_node(41.0, -75.0, Some(-1)), None);
        assert_eq!(layer.create_node(41.0, -75.0, Some(-99)), Some(-99));
    }

    #[test]
    fn delete_feature_refuses_node_referenced_by_way_but_succeeds_once_way_deleted() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert_eq!(
            layer.delete_feature(FeatureKind::Node, 1),
            None,
            "node still referenced by a way must be refused"
        );
        assert!(!layer.is_modified());

        assert!(layer.delete_feature(FeatureKind::Way, 10).is_some());
        // Now node 1 is no longer referenced by any way.
        let snapshot = layer
            .delete_feature(FeatureKind::Node, 1)
            .expect("standalone node should delete");
        assert_eq!(snapshot.kind, FeatureKind::Node);
        assert_eq!(snapshot.id, 1);
        assert_eq!(snapshot.node_lat_lon, Some((40.0, -74.0)));

        let updated = layer.get_osm_data().unwrap();
        assert!(!updated.nodes.contains_key(&1));
    }

    #[test]
    fn delete_feature_standalone_node_succeeds_directly() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let snapshot = layer
            .delete_feature(FeatureKind::Node, 1)
            .expect("standalone node should delete");
        assert!(layer.is_modified());
        assert_eq!(snapshot.node_lat_lon, Some((40.0, -74.0)));
        assert!(!layer.get_osm_data().unwrap().nodes.contains_key(&1));
        assert!(!layer.node_cache.index_by_id.contains_key(&1));

        let viewport = viewport_centered_on(40.0, -74.0);
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(
            !hits.iter().any(|h| h.feature.id == 1),
            "deleted node must not be hit-testable: {:?}",
            hits
        );
    }

    #[test]
    fn delete_feature_way_removes_way_but_keeps_nodes_and_fixes_other_ways() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        let n3 = OsmNode {
            id: 3,
            lat: 40.002,
            lon: -74.002,
            tags: empty_tags(),
            version: 1,
        };
        let mut tags10 = empty_tags();
        tags10.insert("highway".to_string(), "residential".to_string());
        let way_a = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: tags10,
            version: 1,
        };
        // way_b also references node 1, to verify node_to_ways stays correct
        // for the surviving way after way_a is removed.
        let way_b = OsmWay {
            id: 20,
            nodes: vec![1, 3],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2, n3], vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let snapshot = layer
            .delete_feature(FeatureKind::Way, 10)
            .expect("way should delete");
        assert!(layer.is_modified());
        assert_eq!(snapshot.kind, FeatureKind::Way);
        assert_eq!(snapshot.way_nodes, vec![1, 2]);
        assert_eq!(
            snapshot.tags,
            vec![("highway".to_string(), "residential".to_string())]
        );

        let updated = layer.get_osm_data().unwrap();
        assert!(!updated.ways.contains_key(&10), "way 10 must be gone");
        assert!(updated.ways.contains_key(&20), "way 20 must remain");
        // Member nodes of the deleted way are left in place.
        assert!(updated.nodes.contains_key(&1));
        assert!(updated.nodes.contains_key(&2));

        // way_b (still referencing node 1) must still be findable/correct.
        assert_eq!(layer.way_node_ids(20), Some(vec![1, 3]));
        let viewport = viewport_centered_on(40.0, -74.0);
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits
            .iter()
            .any(|h| h.feature.id == 1 && h.kind == FeatureKind::Node));
    }

    #[test]
    fn delete_feature_missing_id_returns_none() {
        let data = data_with(vec![], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        assert_eq!(layer.delete_feature(FeatureKind::Node, 999), None);
        assert_eq!(layer.delete_feature(FeatureKind::Way, 999), None);
        assert!(!layer.is_modified());
    }

    #[test]
    fn restore_feature_round_trips_deleted_node() {
        let mut tags = empty_tags();
        tags.insert("amenity".to_string(), "cafe".to_string());
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags,
            version: 1,
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let snapshot = layer.delete_feature(FeatureKind::Node, 1).unwrap();
        assert!(!layer.get_osm_data().unwrap().nodes.contains_key(&1));

        layer.restore_feature(snapshot);
        let restored = layer.get_osm_data().unwrap();
        let node = restored.nodes.get(&1).expect("node should be restored");
        assert_eq!((node.lat, node.lon), (40.0, -74.0));
        assert_eq!(node.tags.get("amenity"), Some(&"cafe".to_string()));

        // Hit-testable again after restore.
        let viewport = viewport_centered_on(40.0, -74.0);
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits
            .iter()
            .any(|h| h.feature.id == 1 && h.kind == FeatureKind::Node));
    }

    #[test]
    fn restore_feature_round_trips_deleted_way() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            tags: empty_tags(),
            version: 1,
        };
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags,
            version: 1,
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let snapshot = layer.delete_feature(FeatureKind::Way, 10).unwrap();
        assert!(layer.way_node_ids(10).is_none());

        layer.restore_feature(snapshot);
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        let restored = layer.get_osm_data().unwrap();
        let way = restored.ways.get(&10).unwrap();
        assert_eq!(way.tags.get("highway"), Some(&"residential".to_string()));

        // node_to_ways was patched so the restored way is picked up by a
        // node-move on one of its members.
        layer.commit_node_moves(&[(1, 41.0, -75.0)]);
        let (mx, my) = crate::coordinates::lat_lon_to_mercator(41.0, -75.0);
        let verts = layer.way_vertices[*layer.way_id_to_index.get(&10).unwrap()].clone();
        let &(_, vmx, vmy) = verts.iter().find(|&&(id, _, _)| id == 1).unwrap();
        assert!((vmx - mx).abs() < 1e-6);
        assert!((vmy - my).abs() < 1e-6);
    }

    // -- Upload diff/apply-result model --

    #[test]
    fn diff_for_upload_no_changes_is_empty() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        assert!(layer.diff_for_upload().is_empty());
    }

    #[test]
    fn diff_for_upload_detects_tag_edit_as_modified() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_nodes.len(), 1);
        assert_eq!(diff.modified_nodes[0].id, 1);
        assert!(diff.created_nodes.is_empty());
        assert!(diff.deleted_node_ids.is_empty());
    }

    #[test]
    fn diff_for_upload_detects_move_as_modified() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.commit_node_moves(&[(1, 41.0, -75.0)]);
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_nodes.len(), 1);
        assert_eq!(diff.modified_nodes[0].lat, 41.0);
    }

    #[test]
    fn apply_upload_result_updates_version_for_modified_node() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        layer.commit_node_moves(&[(1, 41.0, -75.0)]);
        assert!(layer.is_modified());

        let mut result = crate::osm_upload::UploadResult::default();
        result.node_id_remap.insert(1, (1, 2));
        layer.apply_upload_result(&result);

        let updated = layer.get_osm_data().unwrap();
        assert_eq!(updated.nodes.get(&1).unwrap().version, 2);
        assert!(
            !layer.is_modified(),
            "layer should be clean after a reconciled upload"
        );
        assert!(
            layer.diff_for_upload().is_empty(),
            "new baseline should match current data"
        );
    }

    #[test]
    fn apply_upload_result_remaps_created_node_id_in_referencing_way() {
        // A way that references a newly-created (negative local id) node.
        // After upload, that node gets a real server id — the way's `nodes`
        // list must be updated to point at the new id, or its geometry
        // would silently corrupt from then on.
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        // Simulate a locally-created node (-1) and a way referencing both
        // the pre-existing node 1 and the new node -1, by mutating the
        // layer's data directly (create_node/append_way don't exist yet).
        let current = layer.get_osm_data().unwrap();
        let mut new_data = (*current).clone();
        new_data.nodes.insert(
            -1,
            OsmNode {
                id: -1,
                lat: 40.001,
                lon: -74.001,
                version: 0,
                tags: empty_tags(),
            },
        );
        new_data.ways.insert(
            -2,
            OsmWay {
                id: -2,
                nodes: vec![1, -1],
                version: 0,
                tags: empty_tags(),
            },
        );
        layer.set_osm_data_for_test(Arc::new(new_data));

        let diff = layer.diff_for_upload();
        assert_eq!(diff.created_nodes.len(), 1);
        assert_eq!(diff.created_ways.len(), 1);

        let mut result = crate::osm_upload::UploadResult::default();
        result.node_id_remap.insert(-1, (999, 1));
        result.way_id_remap.insert(-2, (888, 1));
        layer.apply_upload_result(&result);

        let updated = layer.get_osm_data().unwrap();
        assert!(
            updated.nodes.contains_key(&999),
            "new node id should be present"
        );
        assert!(
            !updated.nodes.contains_key(&-1),
            "old local id should be gone"
        );
        let way = updated.ways.get(&888).expect("way should have its new id");
        assert_eq!(
            way.nodes,
            vec![1, 999],
            "way must reference the node's NEW id, not the stale local one"
        );
        assert!(!layer.is_modified());
        assert!(layer.diff_for_upload().is_empty());
    }

    #[test]
    fn apply_upload_result_confirms_delete_with_no_remap_entry() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.001,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        // Simulate a delete by directly removing node 2 from osm_data (the
        // real delete_feature API doesn't exist yet).
        let current = layer.get_osm_data().unwrap();
        let mut new_data = (*current).clone();
        new_data.nodes.remove(&2);
        layer.set_osm_data_for_test(Arc::new(new_data));

        let diff = layer.diff_for_upload();
        assert_eq!(diff.deleted_node_ids, vec![(2, 1)]);

        // Deletes get no remap entry in the diffResult; applying an empty
        // result should still clear `modified` and reset the baseline so
        // the deletion isn't reported again.
        let result = crate::osm_upload::UploadResult::default();
        layer.apply_upload_result(&result);
        assert!(!layer.is_modified());
        assert!(layer.diff_for_upload().is_empty());
    }

    // -- `add_node` / `remove_node` --

    #[test]
    fn add_node_assigns_decrementing_placeholder_ids_and_is_hit_testable() {
        let mut layer = OsmLayer::new(LayerId(1));
        let viewport = viewport_centered_on(40.0, -74.0);

        let id1 = layer.add_node(40.0, -74.0);
        let id2 = layer.add_node(40.0, -74.0);
        assert_eq!(id1, -1, "first placeholder id should be -1");
        assert_eq!(id2, -2, "ids should decrement");
        assert!(layer.is_modified());

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits
            .iter()
            .any(|h| h.feature.id == id1 && h.kind == FeatureKind::Node));
    }

    #[test]
    fn remove_node_drops_it_from_data_and_index() {
        let mut layer = OsmLayer::new(LayerId(1));
        let viewport = viewport_centered_on(40.0, -74.0);
        let id = layer.add_node(40.0, -74.0);

        layer.remove_node(id);

        let data = layer.get_osm_data().expect("data should still exist");
        assert!(!data.nodes.contains_key(&id));
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(
            hits.is_empty(),
            "removed node should not be hit-testable: {:?}",
            hits
        );
    }

    // -- `add_way` / `remove_way` --

    #[test]
    fn add_way_creates_closed_building_way_and_is_hit_testable() {
        let mut layer = OsmLayer::new(LayerId(1));
        let viewport = viewport_centered_on(40.0, -74.0);
        // Straddle the viewport center rather than placing a node exactly on
        // it (as `hit_test_falls_through_to_way` also does): `hit_test`
        // returns node hits before even considering ways, so a node sitting
        // exactly at the click point would shadow the way we're testing.
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -73.999);
        let way_id = layer.add_way(
            vec![n1, n2, n1],
            vec![("building".to_string(), "yes".to_string())],
        );
        assert_eq!(way_id, -1, "first placeholder way id should be -1");

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits
            .iter()
            .any(|h| h.feature.id == way_id && h.kind == FeatureKind::Way));
        let tags = layer
            .feature_tags(&crate::selection::FeatureRef {
                layer_id: layer.id(),
                kind: FeatureKind::Way,
                id: way_id,
            })
            .expect("way should have tags");
        assert!(tags.contains(&("building".to_string(), "yes".to_string())));
    }

    #[test]
    fn remove_way_drops_it_from_data_and_index_but_keeps_its_nodes() {
        let mut layer = OsmLayer::new(LayerId(1));
        let viewport = viewport_centered_on(40.0, -74.0);
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);

        layer.remove_way(way_id);

        let data = layer.get_osm_data().unwrap();
        assert!(!data.ways.contains_key(&way_id));
        assert!(
            data.nodes.contains_key(&n1),
            "removing the way must not remove its nodes"
        );
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(
            !hits.iter().any(|h| h.kind == FeatureKind::Way),
            "way should no longer hit-test: {:?}",
            hits
        );
    }

    // -- `extend_way` / `insert_node_into_way` / `remove_node_from_way` --

    #[test]
    fn extend_way_appends_node_and_updates_bbox() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);
        let n3 = layer.add_node(40.001, -74.0);

        layer.extend_way(way_id, n3);

        let data = layer.get_osm_data().unwrap();
        let way = data.ways.get(&way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, n2, n3]);
    }

    #[test]
    fn insert_node_into_way_splices_at_index_and_remove_node_from_way_undoes_it() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);

        let mid = layer.insert_node_into_way(way_id, 1, 40.0, -74.0005);
        let data = layer.get_osm_data().unwrap();
        let way = data.ways.get(&way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, mid, n2]);

        layer.remove_node_from_way(way_id, 1);
        let data = layer.get_osm_data().unwrap();
        let way = data.ways.get(&way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, n2]);
        assert!(
            data.nodes.contains_key(&mid),
            "remove_node_from_way must not delete the node itself"
        );
    }

    #[test]
    fn feature_geometry_classifies_unreferenced_node_as_point() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 1,
        };
        assert_eq!(
            layer.feature_geometry(&feature, &area_keys),
            Some(crate::presets::Geometry::Point)
        );
    }

    #[test]
    fn feature_geometry_returns_none_for_wrong_layer() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(2),
            kind: FeatureKind::Node,
            id: 1,
        };
        assert_eq!(layer.feature_geometry(&feature, &area_keys), None);
    }

    #[test]
    fn feature_geometry_classifies_closed_area_way() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.001,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let mut tags = HashMap::new();
        tags.insert("building".to_string(), "yes".to_string());
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 1],
            version: 1,
            tags,
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let area_keys = crate::presets::AreaKeys::from_json(r#"{"building": {}}"#).unwrap();
        let feature = crate::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 10,
        };
        assert_eq!(
            layer.feature_geometry(&feature, &area_keys),
            Some(crate::presets::Geometry::Area)
        );
    }
}
