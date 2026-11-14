use gpui::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::layers::area_labels;
use crate::layers::diff::{baseline_for_loaded_file, diff_osm_data, LayerDiff};
use crate::layers::labels;
use crate::layers::merge::{self, MergeStats};
use crate::layers::node_markers::{self, NodeForm};
use crate::layers::resolve;
use crate::layers::way_stroke::{self, Layer, PathSink};
use crate::layers::{EditError, EditableLayer, HighlightKind, LayerId, MapLayer};
use crate::osm::{FileEdits, OsmData, OsmNode, OsmWay};
use crate::osm_upload::UploadResult;
use crate::selection::{
    nearest_point_on_segment, point_to_segment_distance, DeletedFeatureSnapshot,
    DeletedRelationSnapshot, FeatureKind, FeatureRef, HitCandidate, PurgedFeatures,
};
use crate::style::mapcss::{NodeShape, NodeVisual};
use crate::style::{Direction, NodeStyle, Stylesheet, TextPosition, WayStyle};
use crate::viewport::Viewport;
use rstar::{
    primitives::{GeomWithData, Rectangle},
    RTree, AABB,
};

const SELECTION_ACCENT: u32 = 0xFF4081;
/// Highlight color of the members of a selected relation; apart from the
/// selection pink and from the map's own colors.
const RELATION_MEMBER_ACCENT: u32 = 0x7C4DFF;

/// Near-black used for the casing under ways, node marker outlines, and the
/// outer ring of the hover highlight — whatever needs to read against light
/// imagery.
const CASING_COLOR: u32 = 0x1C1C1E;

/// The zoom at which node styles are resolved into the per-node cache.
/// Node rules with zoom ranges are evaluated at this zoom only (way styles
/// are re-resolved per zoom bucket; nodes are not yet).
const NODE_STYLE_ZOOM: f64 = 18.0;

/// How far the casing extends beyond each side of a way's stroke, in pixels.
const CASING_WIDTH: f32 = 1.0;

/// Below this zoom the casing is skipped: ways are dense enough that it
/// only adds clutter, and it doubles the way pass's vertex count exactly
/// when the most ways are on screen (measured with `examples/perf_bench`:
/// 2.8 ms -> 5.2 ms of CPU per frame for 8k ways all visible at z14). It is
/// drawn from zoom 16, where editing starts (`COARSE_BELOW_ZOOM`): on a
/// 290k-node extract at z15.5 it was about 40 percent of the vertices.
const CASING_MIN_ZOOM: f64 = 16.0;

/// Below this zoom turn-restriction markers aren't drawn or clickable.
/// (Which node markers are shown is decided by `node_markers`.)
const VERTEX_MIN_ZOOM: f64 = 16.0;

/// Below this zoom direction arrows aren't drawn.
const ARROW_MIN_ZOOM: f64 = 16.0;

/// Path length between direction arrows along a way, in pixels.
const ARROW_SPACING_PX: f32 = 96.0;

/// A direction arrow is only placed on a segment at least this long, so it
/// never overhangs the segment's ends.
const ARROW_MIN_SEGMENT_PX: f32 = 12.0;

/// Half the length of a direction arrow along the way, in pixels.
const ARROW_HALF_LENGTH: f32 = 4.5;

/// How far a direction arrow's base extends beyond each side of the way's
/// stroke, in pixels.
const ARROW_OVERHANG: f32 = 3.0;

/// Minimum on-screen length (in pixels) a way segment must accumulate before
/// it's emitted as its own quad. Segments shorter than this while zoomed out
/// are skipped (their span just gets absorbed into the next emitted
/// segment), cutting emitted vertices with no visible geometry change. The
/// way's final vertex is always emitted exactly regardless of this
/// threshold, so endpoints/junctions and hit-testing (which reads the
/// undecimated cached geometry, not this render-only path) are unaffected.
const MIN_SEGMENT_PX: f32 = 1.0;

/// Below this zoom the way pass draws coarser (see `WayLod`): a longer
/// minimum segment, tiny ways collapsed to one stroke or left out, tiny area
/// fills left out. At and above it (zoom 16, where editing starts) only the
/// one-pixel segment merge applies.
const COARSE_BELOW_ZOOM: f64 = 16.0;

/// Below this zoom, ways drawn no wider than `MINOR_WAY_WIDTH` (footpaths,
/// buildings' and land-use outlines, barriers: everything the default
/// stylesheet draws at 1.5 px or less) get no outline. Areas keep their fill.
/// At this scale they are far too small to pick out, and they are most of the
/// vertices of a dense city.
const MINOR_WAYS_BELOW_ZOOM: f64 = 15.0;

/// Widest stroke, in pixels, that counts as a minor way.
const MINOR_WAY_WIDTH: f32 = 1.5;

/// Douglas-Peucker tolerance at and above `COARSE_BELOW_ZOOM`, in pixels: a
/// vertex that moves the line by under a third of a pixel is not drawn.
const TOLERANCE_PX: f32 = 0.35;

/// Douglas-Peucker tolerance below `COARSE_BELOW_ZOOM`, in pixels.
const COARSE_TOLERANCE_PX: f32 = 1.0;

/// Minimum merged segment length below `COARSE_BELOW_ZOOM`, in pixels.
const COARSE_MIN_SEGMENT_PX: f32 = 2.0;

/// Below `COARSE_BELOW_ZOOM`, a way whose bounding box is under this many
/// pixels across is drawn as a single stroke across the box.
const COLLAPSE_BELOW_PX: f32 = 3.0;

/// Below `COARSE_BELOW_ZOOM`, a way whose bounding box is under this many
/// pixels across is not drawn at all.
const SKIP_BELOW_PX: f32 = 0.75;

/// Below `COARSE_BELOW_ZOOM`, an area under this many pixels across gets no
/// fill (its outline covers it).
const TINY_FILL_BELOW_PX: f32 = 2.0;

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
    /// A box containing nothing, for `extend` to grow.
    fn empty() -> Self {
        Self {
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

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

fn way_rtree_entry(b: WayBbox, way_id: i64) -> GeomWithData<Rectangle<[f64; 2]>, i64> {
    GeomWithData::new(
        Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
        way_id,
    )
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

/// Areas smaller than this on screen (width, height in px) cannot hold any
/// label text and are not considered.
const MIN_AREA_PX: (f32, f32) = (16.0, 11.0);

/// A named area ready for placement: its item plus what painting needs.
struct AreaItem {
    item: labels::Item,
    label: Arc<area_labels::AreaLabel>,
    line: Arc<ShapedLine>,
    /// Where the text is centred, Mercator metres.
    mx: f64,
    my: f64,
}

/// How close, in pixels, the pointer must be to a way's line to hover it.
const WAY_HIT_TOL: f32 = 6.0;

/// Nodes shown in full whatever the zoom because they are selected; see
/// `OsmLayer::pins`.
#[derive(Default)]
struct Pins {
    nodes: HashSet<i64>,
    /// Indices into `way_vertices`.
    ways: HashSet<usize>,
    /// Index into `way_vertices` of the way whose vertices are previewed
    /// (the one under the pointer), if any.
    preview: Option<usize>,
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
    way_vertices: Vec<WayGeom>,
    /// Resolved per-way style, aligned with `osm_data.ways`/`way_vertices` by
    /// index. Resolved once (here) instead of every frame in the render
    /// loop.
    way_styles: Vec<WayStyle>,
    /// Cached earcut triangle indices per way (into `way_vertices[i]` minus
    /// the closing duplicate), `Some` only for closed ways whose resolved
    /// style has a fill. Aligned with `way_vertices`/`way_styles` by index.
    /// See `compute_fill_tris`.
    way_fill_tris: Vec<Option<Vec<u32>>>,
    /// Ids of the ways that are a member of any relation; filled only when
    /// the stylesheet has `way:in-relation` rules (see
    /// [`relation_member_ways`]) and kept current by relation edits.
    member_ways: HashSet<i64>,
    /// Assembled and triangulated multipolygon relations; see [`MpCache`].
    mp: MpCache,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
    /// Street-name labels along ways (prototype); see `line_labels`.
    line_labels: std::sync::Mutex<crate::layers::line_labels::LineLabelCache>,
    /// The street-name labels chosen at the last settle.
    line_placed: std::sync::Mutex<crate::layers::line_labels::LineState>,
    /// The downloaded rectangles (`OsmData::bounds`) projected to mercator,
    /// so a frame does no trig for them. Rebuilt wherever `osm_data`'s bounds
    /// can change: construction, `set_osm_data` (which `merge_download` ends
    /// in) and `clear_osm_data`.
    download_rects: Vec<crate::download_bounds::MercRect>,
    /// Precomputed mercator positions (+ resolved style) for every node.
    node_cache: NodeCache,
    /// Stylesheet used to pick per-feature colors/weights from OSM tags.
    stylesheet: Arc<Stylesheet>,
    /// Feature to highlight (set each frame by MapViewer).
    highlight: Vec<FeatureRef>,
    /// The way under the pointer in Select mode, whose vertices are shown
    /// (small and light) and can be hovered, clicked and dragged. Set by
    /// `update_vertex_preview`; costs nothing while `None`.
    preview_way: Option<i64>,
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
    node_to_ways: HashMap<i64, HashSet<usize>>,
    /// Transient screen-space offset applied to the given node ids while
    /// rendering, for live drag feedback. Never touches `osm_data`.
    drag_preview: Option<(HashSet<i64>, Point<Pixels>)>,
    /// Sizes of last frame's way-pass batches (see `VertexHints`).
    vertex_hints: VertexHints,
    /// Whether this layer has had a committed move since it was loaded.
    modified: bool,
    /// Bumped by every change to the layer's data or to its baseline (every
    /// edit, an undo or redo of one, a download merged in, an upload applied):
    /// the autosave compares it with the value it last saved to tell, without
    /// looking at the data, whether there is anything new to write.
    revision: u64,
    /// Next id for a new node: negative (not yet uploaded) and below every
    /// node id in the data, so a reloaded file's local ids cannot collide.
    next_new_id: i64,
    /// Next id for a new way, derived from the way ids the same way.
    next_placeholder_way_id: i64,
    /// Label candidates, shaped text and the last placement; see
    /// [`labels`]. A mutex because painting takes `&self` and layers are
    /// `Sync`; it is only ever locked on the UI thread.
    label_cache: std::sync::Mutex<labels::LabelCache>,
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

/// Starting point for `next_placeholder_way_id`, as `compute_next_new_id` is
/// for nodes.
fn compute_next_way_id(data: &OsmData) -> i64 {
    data.ways.keys().copied().min().unwrap_or(0).min(0) - 1
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
            styles.push(stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM));
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
fn project_way_vertices(way: &OsmWay, node_cache: &NodeCache) -> (WayGeom, Option<WayBbox>) {
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
    (WayGeom::new(verts), bbox)
}

/// A way's projected vertices (`(node id, x, y)`, Web Mercator meters) with
/// each vertex's *significance*: the Douglas-Peucker deviation, in meters, at
/// which the vertex first becomes necessary. Dropping every vertex whose
/// significance is under a tolerance gives exactly the Douglas-Peucker
/// simplification at that tolerance, so the renderer can simplify a way to
/// any zoom with one compare per vertex (and project only the survivors).
///
/// Built only by [`WayGeom::new`], which every path that (re)projects a way
/// goes through (`compute_way_tables`, `commit_node_moves`, `restore_way`,
/// add/replace way), so the significance is always that of the current
/// geometry: an edit replaces the whole `WayGeom` of each way it touches.
/// Derefs to the vertex slice for everything that does not care.
#[derive(Debug, Clone, PartialEq, Default)]
struct WayGeom {
    verts: Vec<(i64, f64, f64)>,
    sig: Vec<f32>,
}

impl std::ops::Deref for WayGeom {
    type Target = [(i64, f64, f64)];
    fn deref(&self) -> &Self::Target {
        &self.verts
    }
}

impl WayGeom {
    fn new(verts: Vec<(i64, f64, f64)>) -> Self {
        let sig = vertex_significance(&verts);
        Self { verts, sig }
    }
}

/// Douglas-Peucker significance of every vertex of a polyline or ring (see
/// [`WayGeom`]): the end points get infinity; each other vertex gets the
/// distance (meters) from the chord of the range that was split at it.
/// Iterative (no recursion) so a long coastline cannot overflow the stack.
fn vertex_significance(verts: &[(i64, f64, f64)]) -> Vec<f32> {
    let n = verts.len();
    let mut sig = vec![0.0f32; n];
    if n == 0 {
        return sig;
    }
    sig[0] = f32::INFINITY;
    sig[n - 1] = f32::INFINITY;
    let mut stack: Vec<(usize, usize)> = vec![(0, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (_, ax, ay) = verts[lo];
        let (_, bx, by) = verts[hi];
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let mut best = lo;
        let mut best_d2 = -1.0f64;
        for (k, &(_, px_, py_)) in verts.iter().enumerate().take(hi).skip(lo + 1) {
            // Distance to the chord line; to the end point for a closed ring
            // (zero-length chord).
            let d2 = if len2 > 0.0 {
                let cross = dx * (py_ - ay) - dy * (px_ - ax);
                cross * cross / len2
            } else {
                (px_ - ax) * (px_ - ax) + (py_ - ay) * (py_ - ay)
            };
            if d2 > best_d2 {
                best_d2 = d2;
                best = k;
            }
        }
        // Significance can only shrink along a branch of the split tree, so
        // clamp to the parent's (the property that makes thresholding equal
        // to running Douglas-Peucker at that tolerance).
        let parent = {
            let lo_s = sig[lo];
            let hi_s = sig[hi];
            lo_s.min(hi_s)
        };
        sig[best] = (best_d2.sqrt() as f32).min(parent);
        stack.push((lo, best));
        stack.push((best, hi));
    }
    sig
}

/// Per-way tables produced by [`compute_way_tables`]: parallel vectors of
/// way ids, bounding boxes, pre-projected `(node_id, x, y)` vertex lists,
/// resolved styles, and fill triangulations, all indexed identically.
type WayTables = (
    Vec<i64>,
    Vec<Option<WayBbox>>,
    Vec<WayGeom>,
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

/// The ids of the ways that are a member of any relation, for
/// `way:in-relation` rules. Empty (and not worth computing) for a stylesheet
/// with no such rule.
fn relation_member_ways(data: &OsmData, stylesheet: &Stylesheet) -> HashSet<i64> {
    if !stylesheet.uses_relation_membership() {
        return HashSet::new();
    }
    data.relations
        .iter()
        .flat_map(|r| &r.members)
        .filter(|m| m.member_type == crate::osm::MemberType::Way)
        .map(|m| m.reference)
        .collect()
}

/// Build per-way bboxes, pre-projected vertex lists, resolved styles, and
/// fill triangulations in a single pass so neither the bbox pass nor the
/// render path has to walk the node HashMap (or the stylesheet, or the
/// tessellator) per vertex/way.
fn compute_way_tables(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
    members: &HashSet<i64>,
) -> WayTables {
    let mut ids = Vec::with_capacity(data.ways.len());
    let mut bboxes = Vec::with_capacity(data.ways.len());
    let mut vertices = Vec::with_capacity(data.ways.len());
    let mut styles = Vec::with_capacity(data.ways.len());
    let mut fill_tris = Vec::with_capacity(data.ways.len());
    for way in sorted_ways(data) {
        let (verts, bbox) = project_way_vertices(way, node_cache);
        let style =
            stylesheet.way_style_cached(&way.tags, way.is_closed(), members.contains(&way.id));
        fill_tris.push(compute_fill_tris(&verts, &style));
        ids.push(way.id);
        bboxes.push(bbox);
        vertices.push(verts);
        styles.push(style);
    }
    (ids, bboxes, vertices, styles, fill_tris)
}

/// One assembled polygon of a multipolygon relation, projected and
/// triangulated once.
#[derive(Debug, Clone)]
struct MpPolygon {
    /// `(node id, x, y)` in Web Mercator meters for the outer ring followed
    /// by each hole, none repeating its first vertex. The node ids let the
    /// renderer apply a drag preview.
    verts: Vec<(i64, f64, f64)>,
    /// Start offset of each ring in `verts`, then `verts.len()`: ring `k` is
    /// `verts[ring_starts[k]..ring_starts[k + 1]]`; ring 0 is the outer one.
    ring_starts: Vec<usize>,
    /// Earcut triangles (indices into `verts`), outer minus holes. Empty when
    /// the relation's style has no fill.
    tris: Vec<u32>,
    bbox: WayBbox,
}

impl MpPolygon {
    fn ring(&self, k: usize) -> &[(i64, f64, f64)] {
        &self.verts[self.ring_starts[k]..self.ring_starts[k + 1]]
    }
    fn ring_count(&self) -> usize {
        self.ring_starts.len() - 1
    }
}

/// A multipolygon relation that assembled into valid rings.
#[derive(Debug, Clone)]
struct MpEntry {
    /// Resolved from the relation's tags as for a closed way.
    style: WayStyle,
    polygons: Vec<MpPolygon>,
    bbox: WayBbox,
    /// Area in mercator square meters, holes excluded.
    area: f64,
    /// Whether the relation carries tags besides `type`. An untagged
    /// multipolygon is the old style, whose tags live on the outer way: that
    /// way stays what a click selects.
    tagged: bool,
    /// The relation's tags, for its area label.
    tags: HashMap<String, String>,
}

/// What became of a relation in the cache (tests inspect this).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MpState {
    /// Not a multipolygon/boundary (or not in the data).
    NotArea,
    /// A multipolygon that couldn't be assembled; it is not drawn.
    Invalid(crate::multipolygon::Problem),
    /// Assembled into polygons with this many holes in all.
    Valid { polygons: usize, holes: usize },
}

/// Assembled multipolygon relations, kept in step with the data.
///
/// Built from scratch with the rest of the layer's caches, and then patched
/// per relation: every edit that can change a relation's rings (a member
/// node moved, a member way reshaped, split, joined, added or removed, the
/// member list or the relation's tags changed, and the undo/redo of each)
/// calls [`OsmLayer::mp_refresh_relations`] or [`OsmLayer::mp_refresh_ways`]
/// for just the relations involved. `version` counts those refreshes so a
/// test (or a future consumer) can tell a rebuild happened.
#[derive(Debug, Clone, Default)]
struct MpCache {
    entries: BTreeMap<i64, MpEntry>,
    invalid: BTreeMap<i64, crate::multipolygon::Problem>,
    /// Member way id -> area relations listing it (valid or not).
    way_to_rels: HashMap<i64, Vec<i64>>,
    /// Relation id -> its member way ids, to unmap them on refresh.
    rel_ways: HashMap<i64, Vec<i64>>,
    /// Route relations (`type=route`) by id.
    routes: BTreeMap<i64, crate::relation_overlay::RouteEntry>,
    /// Member way id -> the route relations listing it, ascending. The
    /// first one decides the way's overlay colour.
    route_ways: HashMap<i64, Vec<i64>>,
    /// Turn restrictions (`type=restriction`) with a drawable position.
    restrictions: BTreeMap<i64, crate::relation_overlay::RestrictionEntry>,
    /// Restriction member node id -> restriction relations, so moving,
    /// deleting or restoring that node refreshes them.
    node_to_rels: HashMap<i64, Vec<i64>>,
    /// Restriction id -> its member node ids, to unmap them on refresh.
    rel_nodes: HashMap<i64, Vec<i64>>,
    version: u64,
}

/// Assemble and triangulate one relation. `None` when it isn't an area
/// relation.
fn build_mp(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
    relation: &crate::osm::OsmRelation,
) -> Option<Result<MpEntry, crate::multipolygon::Problem>> {
    use crate::multipolygon::{assemble, triangulate, WayMember};
    if !crate::multipolygon::is_area_relation(&relation.tags) {
        return None;
    }
    let members: Vec<WayMember> = relation
        .members
        .iter()
        .filter(|m| m.member_type == crate::osm::MemberType::Way)
        .map(|m| WayMember {
            way_id: m.reference,
            role: &m.role,
            nodes: data.ways.get(&m.reference).map(|w| w.nodes.as_slice()),
        })
        .collect();
    let position = |id: i64| {
        node_cache
            .index_by_id
            .get(&id)
            .map(|&i| (node_cache.flat[i].1, node_cache.flat[i].2))
    };
    let assembled = match assemble(&members, &position) {
        Ok(a) => a,
        Err(problem) => return Some(Err(problem)),
    };
    let style = stylesheet.way_style_cached(&relation.tags, true, false);
    let mut polygons = Vec::with_capacity(assembled.polygons.len());
    let mut total = WayBbox::empty();
    for polygon in &assembled.polygons {
        let mut verts: Vec<(i64, f64, f64)> = Vec::new();
        let mut ring_starts = vec![0];
        let mut bbox = WayBbox::empty();
        for (k, ring) in std::iter::once(&polygon.outer)
            .chain(polygon.inners.iter())
            .enumerate()
        {
            for &id in &ring[..ring.len() - 1] {
                let Some((x, y)) = position(id) else {
                    return Some(Err(crate::multipolygon::Problem::MissingNode(id)));
                };
                verts.push((id, x, y));
                if k == 0 {
                    bbox.extend(x, y);
                }
            }
            ring_starts.push(verts.len());
        }
        total.extend(bbox.min_x, bbox.min_y);
        total.extend(bbox.max_x, bbox.max_y);
        let tris = if style.fill.is_some() {
            let rings: Vec<Vec<(f64, f64)>> = (0..ring_starts.len() - 1)
                .map(|k| {
                    verts[ring_starts[k]..ring_starts[k + 1]]
                        .iter()
                        .map(|&(_, x, y)| (x, y))
                        .collect()
                })
                .collect();
            let refs: Vec<&[(f64, f64)]> = rings.iter().map(|r| r.as_slice()).collect();
            triangulate(&refs).unwrap_or_default()
        } else {
            Vec::new()
        };
        polygons.push(MpPolygon {
            verts,
            ring_starts,
            tris,
            bbox,
        });
    }
    Some(Ok(MpEntry {
        style,
        polygons,
        bbox: total,
        area: assembled.area(),
        tagged: relation.tags.keys().any(|k| k != "type"),
        tags: relation.tags.clone(),
    }))
}

impl MpCache {
    fn rebuild_all(&mut self, data: &OsmData, node_cache: &NodeCache, stylesheet: &Stylesheet) {
        let version = self.version + 1;
        *self = MpCache::default();
        self.version = version;
        for relation in &data.relations {
            self.install(relation, build_mp(data, node_cache, stylesheet, relation));
            self.install_overlay(relation, data, node_cache);
        }
    }

    /// The overlay colour of a way that belongs to a route, if any.
    fn route_colour_of_way(&self, way_id: i64) -> Option<u32> {
        let rel = self.route_ways.get(&way_id)?.first()?;
        self.routes.get(rel).map(|r| r.colour)
    }

    fn remove_overlay(&mut self, id: i64) {
        if let Some(route) = self.routes.remove(&id) {
            for way in route.ways {
                if let Some(rels) = self.route_ways.get_mut(&way) {
                    rels.retain(|&r| r != id);
                    if rels.is_empty() {
                        self.route_ways.remove(&way);
                    }
                }
            }
        }
        self.restrictions.remove(&id);
        if let Some(nodes) = self.rel_nodes.remove(&id) {
            for node in nodes {
                if let Some(rels) = self.node_to_rels.get_mut(&node) {
                    rels.retain(|&r| r != id);
                    if rels.is_empty() {
                        self.node_to_rels.remove(&node);
                    }
                }
            }
        }
    }

    /// Cache the route / restriction drawing of `relation`, if it is one.
    /// A restriction is also mapped from its member ways (through
    /// `way_to_rels`/`rel_ways`, like area relations) and member nodes, so
    /// the edits that move them refresh it. A route only lists way ids, so it
    /// needs no geometry refresh.
    fn install_overlay(
        &mut self,
        relation: &crate::osm::OsmRelation,
        data: &OsmData,
        node_cache: &NodeCache,
    ) {
        if let Some(route) = crate::relation_overlay::build_route(relation) {
            for &w in &route.ways {
                let rels = self.route_ways.entry(w).or_default();
                if let Err(at) = rels.binary_search(&relation.id) {
                    rels.insert(at, relation.id);
                }
            }
            self.routes.insert(relation.id, route);
            return;
        }
        if !crate::relation_overlay::is_restriction(&relation.tags) {
            return;
        }
        // Map every member, drawable or not, so the edit that supplies a
        // missing via (an undo that restores it) refreshes the restriction.
        let mut ways: Vec<i64> = Vec::new();
        let mut nodes: Vec<i64> = Vec::new();
        for m in &relation.members {
            match m.member_type {
                crate::osm::MemberType::Way => ways.push(m.reference),
                crate::osm::MemberType::Node => nodes.push(m.reference),
                crate::osm::MemberType::Relation => {}
            }
        }
        ways.sort_unstable();
        ways.dedup();
        nodes.sort_unstable();
        nodes.dedup();
        for &w in &ways {
            self.way_to_rels.entry(w).or_default().push(relation.id);
        }
        for &n in &nodes {
            self.node_to_rels.entry(n).or_default().push(relation.id);
        }
        self.rel_ways.insert(relation.id, ways);
        self.rel_nodes.insert(relation.id, nodes);
        let position = |id: i64| {
            node_cache
                .index_by_id
                .get(&id)
                .map(|&i| (node_cache.flat[i].1, node_cache.flat[i].2))
        };
        if let Some(r) = crate::relation_overlay::build_restriction(data, relation, &position) {
            self.restrictions.insert(relation.id, r);
        }
    }

    /// Recompute just these relations (dropping any that are gone or no
    /// longer area relations).
    fn refresh(
        &mut self,
        ids: &[i64],
        data: &OsmData,
        node_cache: &NodeCache,
        stylesheet: &Stylesheet,
    ) {
        if ids.is_empty() {
            return;
        }
        self.version += 1;
        for &id in ids {
            self.entries.remove(&id);
            self.invalid.remove(&id);
            self.remove_overlay(id);
            if let Some(ways) = self.rel_ways.remove(&id) {
                for way in ways {
                    if let Some(rels) = self.way_to_rels.get_mut(&way) {
                        rels.retain(|&r| r != id);
                        if rels.is_empty() {
                            self.way_to_rels.remove(&way);
                        }
                    }
                }
            }
            if let Some(relation) = data.relations.iter().find(|r| r.id == id) {
                self.install(relation, build_mp(data, node_cache, stylesheet, relation));
                self.install_overlay(relation, data, node_cache);
            }
        }
    }

    fn install(
        &mut self,
        relation: &crate::osm::OsmRelation,
        built: Option<Result<MpEntry, crate::multipolygon::Problem>>,
    ) {
        let Some(built) = built else {
            return;
        };
        let mut ways: Vec<i64> = relation
            .members
            .iter()
            .filter(|m| m.member_type == crate::osm::MemberType::Way)
            .map(|m| m.reference)
            .collect();
        ways.sort_unstable();
        ways.dedup();
        for &w in &ways {
            self.way_to_rels.entry(w).or_default().push(relation.id);
        }
        self.rel_ways.insert(relation.id, ways);
        match built {
            Ok(entry) => {
                self.entries.insert(relation.id, entry);
            }
            Err(problem) => {
                self.invalid.insert(relation.id, problem);
            }
        }
    }
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
    way_vertices: &[WayGeom],
    way_styles: &mut [WayStyle],
    way_fill_tris: &mut [Option<Vec<u32>>],
    stylesheet: &Stylesheet,
    members: &HashSet<i64>,
) {
    match kind {
        FeatureKind::Node => {
            let Some(node) = data.nodes.get(&id) else {
                return;
            };
            if let Some(&idx) = node_cache.index_by_id.get(&id) {
                node_cache.styles[idx] = stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM);
            }
        }
        FeatureKind::Way => {
            let Some(way) = data.ways.get(&id) else {
                return;
            };
            if let Some(&idx) = way_id_to_index.get(&id) {
                let style =
                    stylesheet.way_style_cached(&way.tags, way.is_closed(), members.contains(&id));
                way_fill_tris[idx] = compute_fill_tris(&way_vertices[idx], &style);
                way_styles[idx] = style;
            }
        }
        // A relation has no drawn style of its own.
        FeatureKind::Relation => {}
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
fn build_node_to_ways(ways: &[&OsmWay]) -> HashMap<i64, HashSet<usize>> {
    let mut map: HashMap<i64, HashSet<usize>> = HashMap::new();
    for (idx, way) in ways.iter().enumerate() {
        for nid in &way.nodes {
            map.entry(*nid).or_default().insert(idx);
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
#[cfg(test)]
fn decimate_segments(pts: &[Point<Pixels>], min_px: f32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    decimate_into(pts, min_px, &mut out);
    out
}

/// `decimate_segments` into a reused buffer (cleared first), so the per-way
/// loop of a frame does not allocate.
fn decimate_into(pts: &[Point<Pixels>], min_px: f32, out: &mut Vec<(usize, usize)>) {
    out.clear();
    if pts.len() < 2 {
        return;
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

/// The triangle for a direction arrow centered on the segment `p0`-`p1`,
/// pointing from `p0` to `p1` (or the other way if `reverse`): tip first,
/// then the two base corners. `half_length` is measured along the segment
/// and `half_base` across it. `None` for a degenerate segment.
fn arrow_triangle(
    p0: Point<Pixels>,
    p1: Point<Pixels>,
    reverse: bool,
    half_length: f32,
    half_base: f32,
) -> Option<[Point<Pixels>; 3]> {
    let (x0, y0) = (f32::from(p0.x), f32::from(p0.y));
    let (x1, y1) = (f32::from(p1.x), f32::from(p1.y));
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < f32::EPSILON {
        return None;
    }
    let sign = if reverse { -1.0 } else { 1.0 };
    let (ux, uy) = (dx / len * sign, dy / len * sign);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (bx, by) = (mx - ux * half_length, my - uy * half_length);
    Some([
        point(px(mx + ux * half_length), px(my + uy * half_length)),
        point(px(bx - uy * half_base), px(by + ux * half_base)),
        point(px(bx + uy * half_base), px(by - ux * half_base)),
    ])
}

/// Which segments of the polyline `pts` get a direction arrow: the index
/// `i` of each chosen segment `pts[i]`-`pts[i + 1]`. One arrow per
/// `ARROW_SPACING_PX` of path length, starting half a spacing in so even a
/// short way gets one, and never on a segment too short to hold it.
fn arrow_segments(pts: &[Point<Pixels>]) -> Vec<usize> {
    let mut chosen = Vec::new();
    let mut since_arrow = ARROW_SPACING_PX / 2.0;
    for (i, pair) in pts.windows(2).enumerate() {
        let dx = f32::from(pair[1].x - pair[0].x);
        let dy = f32::from(pair[1].y - pair[0].y);
        let len = (dx * dx + dy * dy).sqrt();
        since_arrow += len;
        if since_arrow >= ARROW_SPACING_PX && len >= ARROW_MIN_SEGMENT_PX {
            since_arrow = 0.0;
            chosen.push(i);
        }
    }
    chosen
}

/// A circle of `diameter` centered on `center`, as quad bounds.
fn circle_bounds(center: Point<Pixels>, diameter: f32) -> Bounds<Pixels> {
    let half = px(diameter / 2.0);
    Bounds {
        origin: point(center.x - half, center.y - half),
        size: size(px(diameter), px(diameter)),
    }
}

/// Paint a single round node marker at `screen_point` (already including
/// the canvas origin offset) using `style`: a dot of the style's color and
/// size inside a 1px dark outline, so it reads against any imagery. Shared
/// by the per-frame node-marker pass in `render_canvas` and by
/// `render_highlight`'s `FeatureKind::Way` arm, which repaints a
/// highlighted way's vertex markers on top of the highlight stroke so they
/// aren't hidden underneath it.
fn paint_node_marker(window: &mut Window, screen_point: Point<Pixels>, style: NodeStyle) {
    let diameter = style.size + 2.0;
    window.paint_quad(quad(
        circle_bounds(screen_point, diameter),
        px(diameter / 2.0),
        rgb(style.color),
        px(1.0),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
}

/// A previewed vertex (see `NodeForm::Preview`): a small white dot in a dark
/// outline, deliberately lighter than the style-coloured marker of a selected
/// way's vertex so hover and selection read differently.
fn paint_preview_marker(window: &mut Window, screen_point: Point<Pixels>) {
    let diameter = node_markers::PREVIEW_DIAMETER;
    window.paint_quad(quad(
        circle_bounds(screen_point, diameter),
        px(diameter / 2.0),
        rgb(0xFFFFFF),
        px(1.5),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
}

/// The simple form of a tagged node at low zoom: a tiny empty square of outer
/// edge `edge` with a `SIMPLE_STROKE` outline, as four thin rectangles (eight
/// triangles) of the frame's shared `path`. The corner is snapped to half a
/// logical pixel, a whole device pixel on a 2x display, so the lines stay
/// crisp.
fn push_simple_node_marker(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    center: Point<Pixels>,
    edge: f32,
) {
    let snap = |v: f32| (v * 2.0).round() / 2.0;
    let x0 = snap(f32::from(center.x) - edge / 2.0);
    let y0 = snap(f32::from(center.y) - edge / 2.0);
    let (x1, y1) = (x0 + edge, y0 + edge);
    let s = node_markers::SIMPLE_STROKE;
    let mut rect = |l: f32, t: f32, r: f32, b: f32| {
        let (tl, tr) = (point(px(l), px(t)), point(px(r), px(t)));
        let (bl, br) = (point(px(l), px(b)), point(px(r), px(b)));
        push_triangle(path, bounds_min_max, tl, tr, bl);
        push_triangle(path, bounds_min_max, tr, br, bl);
    };
    rect(x0, y0, x1, y0 + s); // top
    rect(x0, y1 - s, x1, y1); // bottom
    rect(x0, y0 + s, x0 + s, y1 - s); // left
    rect(x1 - s, y0 + s, x1, y1 - s); // right
}

/// A stylesheet-drawn node marker (`symbol-shape`) of `visual.size` pixels:
/// a hollow square or a `+`, in `SIMPLE_STROKE` lines, added to the frame's
/// shared `path` for its colour.
fn push_styled_node_marker(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    center: Point<Pixels>,
    visual: NodeVisual,
) {
    match visual.shape {
        NodeShape::Cross => {
            let snap = |v: f32| (v * 2.0).round() / 2.0;
            let (cx, cy) = (snap(f32::from(center.x)), snap(f32::from(center.y)));
            let (h, s) = (visual.size / 2.0, node_markers::SIMPLE_STROKE);
            let mut rect = |l: f32, t: f32, r: f32, b: f32| {
                let (tl, tr) = (point(px(l), px(t)), point(px(r), px(t)));
                let (bl, br) = (point(px(l), px(b)), point(px(r), px(b)));
                push_triangle(path, bounds_min_max, tl, tr, bl);
                push_triangle(path, bounds_min_max, tr, br, bl);
            };
            rect(cx - h, cy - s / 2.0, cx + h, cy + s / 2.0);
            rect(cx - s / 2.0, cy - h, cx + s / 2.0, cy + h);
        }
        _ => push_simple_node_marker(path, bounds_min_max, center, visual.size),
    }
}

/// Extra half-width of a route overlay line beyond the way's casing, in
/// pixels, so the way stays readable on top of it.
const ROUTE_OVERLAY_EXTRA: f32 = 3.0;

/// Diameter of a turn-restriction marker, in pixels.
const RESTRICTION_MARKER_DIAMETER: f32 = 14.0;

/// Where a turn-restriction marker sits relative to its via position (right
/// and up), so it doesn't cover the via node and the node stays pickable.
const RESTRICTION_OFFSET_PX: (f32, f32) = (11.0, -11.0);

/// Paint a turn-restriction marker centered on `center`: a red ring for a
/// `no_*` restriction, a blue disc for an `only_*` one, on a white backing.
fn paint_restriction_marker(window: &mut Window, center: Point<Pixels>, only: bool) {
    use crate::relation_overlay::{RESTRICTION_NO_COLOUR, RESTRICTION_ONLY_COLOUR};
    let d = RESTRICTION_MARKER_DIAMETER;
    window.paint_quad(quad(
        circle_bounds(center, d),
        px(d / 2.0),
        rgb(0xFFFFFF),
        px(1.0),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
    if only {
        window.paint_quad(quad(
            circle_bounds(center, d - 4.0),
            px((d - 4.0) / 2.0),
            rgb(RESTRICTION_ONLY_COLOUR),
            px(0.0),
            gpui::transparent_black(),
            BorderStyle::Solid,
        ));
    } else {
        paint_ring(window, center, d - 2.0, 2.5, RESTRICTION_NO_COLOUR);
    }
}

/// Paint a `width`-px ring of `diameter` centered on `center`.
fn paint_ring(window: &mut Window, center: Point<Pixels>, diameter: f32, width: f32, color: u32) {
    window.paint_quad(quad(
        circle_bounds(center, diameter),
        px(diameter / 2.0),
        gpui::transparent_black(),
        px(width),
        rgb(color),
        BorderStyle::Solid,
    ));
}

/// The `(halo, core)` colors of a highlight: a wide outer stroke and a
/// narrower inner one. The pairs are chosen so one of the two always
/// contrasts with whatever is underneath.
fn highlight_colors(kind: HighlightKind) -> (u32, u32) {
    match kind {
        HighlightKind::Selected => (0xFFFFFF, SELECTION_ACCENT),
        HighlightKind::Hovered => (CASING_COLOR, 0xFFFFFF),
        HighlightKind::Member => (0xFFFFFF, RELATION_MEMBER_ACCENT),
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
            member_ways: HashSet::new(),
            mp: MpCache::default(),
            layer_bbox: None,
            line_labels: Default::default(),
            line_placed: Default::default(),
            download_rects: Vec::new(),
            node_cache: NodeCache {
                index_by_id: HashMap::new(),
                flat: Vec::new(),
                styles: Vec::new(),
            },
            stylesheet: crate::style::custom::active_stylesheet(),
            highlight: Vec::new(),
            preview_way: None,
            node_index: RTree::new(),
            way_index: RTree::new(),
            way_id_to_index: HashMap::new(),
            node_to_ways: HashMap::new(),
            drag_preview: None,
            vertex_hints: VertexHints::default(),
            modified: false,
            revision: 0,
            next_new_id: -1,
            next_placeholder_way_id: -1,
            label_cache: Default::default(),
        }
    }

    pub fn new_with_data<N: Into<String>>(id: LayerId, name: N, osm_data: Arc<OsmData>) -> Self {
        let fs_load = crate::frame_stats::start();
        let stylesheet = crate::style::custom::active_stylesheet();
        let fs_phase = crate::frame_stats::start();
        let node_cache = compute_node_cache(&osm_data, &stylesheet);
        crate::frame_stats::lap("load.node_cache", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let member_ways = relation_member_ways(&osm_data, &stylesheet);
        let (way_ids, way_bboxes, way_vertices, way_styles, way_fill_tris) =
            compute_way_tables(&osm_data, &node_cache, &stylesheet, &member_ways);
        crate::frame_stats::lap("load.way_tables_and_fill_tris", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let mut mp = MpCache::default();
        mp.rebuild_all(&osm_data, &node_cache, &stylesheet);
        crate::frame_stats::lap("cache.mp_rebuild_all", fs_phase);
        let layer_bbox = compute_layer_bbox(&node_cache);
        let node_index = build_node_index(&node_cache);
        let ways_sorted = sorted_ways(&osm_data);
        let way_index = build_way_index(&way_bboxes, &ways_sorted);
        let way_id_to_index = build_way_id_index(&ways_sorted);
        let node_to_ways = build_node_to_ways(&ways_sorted);
        let next_new_id = compute_next_new_id(&osm_data);
        let next_placeholder_way_id = compute_next_way_id(&osm_data);
        let osm_data_bounds = osm_data.bounds.clone();
        crate::frame_stats::lap("load.new_with_data_total", fs_load);
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
            member_ways,
            mp,
            layer_bbox,
            line_labels: Default::default(),
            line_placed: Default::default(),
            download_rects: crate::download_bounds::merc_rects(&osm_data_bounds),
            node_cache,
            stylesheet,
            highlight: Vec::new(),
            preview_way: None,
            node_index,
            way_index,
            way_id_to_index,
            node_to_ways,
            drag_preview: None,
            vertex_hints: VertexHints::default(),
            modified: false,
            revision: 0,
            next_new_id,
            next_placeholder_way_id,
            label_cache: Default::default(),
        }
    }

    /// Build a layer from a loaded file's data together with the pending
    /// local edits the file records (see `FileEdits`), so those edits are
    /// still pending — and still uploadable — after a save and reopen. The
    /// server baseline is reconstructed by `baseline_for_loaded_file`.
    pub fn new_with_edits<N: Into<String>>(
        id: LayerId,
        name: N,
        osm_data: Arc<OsmData>,
        edits: &FileEdits,
    ) -> Self {
        let baseline = baseline_for_loaded_file(&osm_data, edits);
        let mut layer = Self::new_with_data(id, name, osm_data.clone());
        layer.modified = !diff_osm_data(&baseline, &osm_data).is_empty();
        layer.revision += 1;
        layer.original_data = Some(Arc::new(baseline));
        layer
    }

    /// This layer as a JOSM-style `.osm` document, including its pending
    /// edits; see `osm_export`. `None` when the layer holds no data.
    pub fn export_xml(&self) -> Option<String> {
        let current = self.osm_data.as_deref()?;
        let original = self.original_data.as_deref().unwrap_or(current);
        Some(crate::osm_export::to_osm_xml(original, current))
    }

    /// Load `osm_data`, rebuilding every cache from scratch. It becomes the
    /// server baseline too (`original_data`), so `modified` is cleared.
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.rebuild_caches(&osm_data);
        self.osm_data = Some(osm_data.clone());
        self.original_data = Some(osm_data);
        self.modified = false;
        self.revision += 1;
    }

    /// Recompute every derived cache and index from `osm_data`.
    fn rebuild_caches(&mut self, osm_data: &Arc<OsmData>) {
        self.node_cache = compute_node_cache(osm_data, &self.stylesheet);
        self.member_ways = relation_member_ways(osm_data, &self.stylesheet);
        let (ids, bboxes, verts, styles, fill_tris) = compute_way_tables(
            osm_data,
            &self.node_cache,
            &self.stylesheet,
            &self.member_ways,
        );
        self.way_ids = ids;
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.way_styles = styles;
        self.way_fill_tris = fill_tris;
        self.mp
            .rebuild_all(osm_data, &self.node_cache, &self.stylesheet);
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        let ways_sorted = sorted_ways(osm_data);
        self.way_index = build_way_index(&self.way_bboxes, &ways_sorted);
        self.way_id_to_index = build_way_id_index(&ways_sorted);
        self.node_to_ways = build_node_to_ways(&ways_sorted);
        self.next_new_id = compute_next_new_id(osm_data);
        self.next_placeholder_way_id = compute_next_way_id(osm_data);
        self.download_rects = crate::download_bounds::merc_rects(&osm_data.bounds);
    }

    /// Record that the layer's data changed by a local edit (or the undo or
    /// redo of one).
    fn mark_modified(&mut self) {
        self.modified = true;
        self.revision += 1;
    }

    /// The rectangles (Web Mercator meters) this layer's data was downloaded
    /// for; empty when that is not known. See `render_all_download_bounds`.
    pub fn download_rects(&self) -> &[crate::download_bounds::MercRect] {
        &self.download_rects
    }

    /// Whether the layer has edits not yet written to a file or uploaded.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Re-derive `is_modified` from the layer's actual difference to its
    /// server baseline. Edits (and the undo of one) set the flag without
    /// looking at the diff, so after an undo back to the baseline this is
    /// what clears it.
    pub fn refresh_modified(&mut self) {
        if self.modified {
            self.modified = !self.diff_for_upload().is_empty();
        }
    }

    /// How many nodes and ways the layer holds; `None` before any data is
    /// loaded.
    pub fn feature_counts(&self) -> Option<(usize, usize)> {
        let data = self.osm_data.as_ref()?;
        Some((data.nodes.len(), data.ways.len()))
    }

    /// The layer's change counter; see the `revision` field.
    pub fn edit_revision(&self) -> u64 {
        self.revision
    }

    /// The layer's current data and its server baseline, for the autosave to
    /// serialize elsewhere. Cheap: two reference-count bumps, nothing is
    /// copied (an edit made while the other side still holds them pays one
    /// copy-on-write of the data instead). `None` for a layer with no data.
    pub fn autosave_snapshot(&self) -> Option<(Arc<OsmData>, Arc<OsmData>)> {
        let current = self.osm_data.clone()?;
        let original = self
            .original_data
            .clone()
            .unwrap_or_else(|| current.clone());
        Some((current, original))
    }

    /// Run `change` on copies of the baseline and current data and, if it
    /// reports a change, install them: caches rebuilt like `merge_download`
    /// (this is a once-per-conflict path), the baseline kept separate from
    /// the data, and `modified` recomputed from the diff since a resolution
    /// can leave nothing to upload.
    fn change_snapshots(
        &mut self,
        change: impl FnOnce(&mut OsmData, &mut OsmData) -> bool,
    ) -> bool {
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return false;
        };
        let (mut current, mut original) = ((*current).clone(), (*original).clone());
        if !change(&mut original, &mut current) {
            return false;
        }
        self.set_osm_data(Arc::new(current));
        self.original_data = Some(Arc::new(original));
        self.modified = !self.diff_for_upload().is_empty();
        self.revision += 1;
        true
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
                    bounds: Vec::new(),
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
        Self::remap_osm_data(&mut data, result);
        self.set_osm_data(Arc::new(data));
    }

    /// Reconcile this layer with ONE accepted chunk of an upload that is
    /// still going (or stopped part way), leaving the edits of the chunks not
    /// yet accepted pending: `result`'s ids and versions are applied to the
    /// data as in `apply_upload_result`, and the server baseline is advanced
    /// by exactly the objects in `chunk` (created ones added under their new
    /// ids, modified ones replaced, deleted ones removed), so the next
    /// `diff_for_upload` is what remains to be sent. `chunk` is the diff
    /// that was sent for `result` (placeholder ids as the layer still holds
    /// them for its own new objects).
    pub fn apply_chunk_result(&mut self, result: &UploadResult, chunk: &LayerDiff) {
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        Self::remap_osm_data(&mut data, result);

        let new_id = |remap: &HashMap<i64, (i64, i32)>, id: i64| remap.get(&id).map(|&(n, _)| n);
        for n in &chunk.created_nodes {
            if let Some(id) = new_id(&result.node_id_remap, n.id) {
                if let Some(node) = data.nodes.get(&id) {
                    base.nodes.insert(id, node.clone());
                }
            }
        }
        for n in &chunk.modified_nodes {
            if let Some(node) = data.nodes.get(&n.id) {
                base.nodes.insert(n.id, node.clone());
            }
        }
        for w in &chunk.created_ways {
            if let Some(id) = new_id(&result.way_id_remap, w.id) {
                if let Some(way) = data.ways.get(&id) {
                    base.ways.insert(id, way.clone());
                }
            }
        }
        for w in &chunk.modified_ways {
            if let Some(way) = data.ways.get(&w.id) {
                base.ways.insert(w.id, way.clone());
            }
        }
        for r in &chunk.created_relations {
            if let Some(id) = new_id(&result.relation_id_remap, r.id) {
                if let Some(rel) = data.relations.iter().find(|x| x.id == id) {
                    base.relations.retain(|x| x.id != id);
                    base.relations.push(rel.clone());
                }
            }
        }
        for r in &chunk.modified_relations {
            if let Some(rel) = data.relations.iter().find(|x| x.id == r.id) {
                match base.relations.iter_mut().find(|x| x.id == r.id) {
                    Some(slot) => *slot = rel.clone(),
                    None => base.relations.push(rel.clone()),
                }
            }
        }
        for &(id, _) in &chunk.deleted_node_ids {
            if !data.nodes.contains_key(&id) {
                base.nodes.remove(&id);
            }
        }
        for &(id, _) in &chunk.deleted_way_ids {
            if !data.ways.contains_key(&id) {
                base.ways.remove(&id);
            }
        }
        for &(id, _) in &chunk.deleted_relation_ids {
            if !data.relations.iter().any(|x| x.id == id) {
                base.relations.retain(|x| x.id != id);
            }
        }

        self.set_osm_data(Arc::new(data));
        // `set_osm_data` treats its argument as a fresh server baseline;
        // restore the real one so the unsent chunks stay pending.
        self.original_data = Some(Arc::new(base));
        self.modified = !self.diff_for_upload().is_empty();
    }

    fn remap_osm_data(data: &mut OsmData, result: &UploadResult) {
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
        let mut way_id_map: HashMap<i64, i64> = HashMap::new();
        for (old_id, new_id, new_version) in way_updates {
            if let Some(mut way) = data.ways.remove(&old_id) {
                way.id = new_id;
                way.version = new_version;
                data.ways.insert(new_id, way);
                way_id_map.insert(old_id, new_id);
            }
        }

        // Relations: member refs to created nodes/ways/relations switch from
        // their placeholder ids to the real ones, and a relation the server
        // accepted gets its new id (when created) and version.
        let relation_id_map: HashMap<i64, i64> = data
            .relations
            .iter()
            .filter_map(|r| {
                result
                    .relation_id_remap
                    .get(&r.id)
                    .map(|&(new_id, _)| (r.id, new_id))
            })
            .collect();
        for relation in &mut data.relations {
            for member in &mut relation.members {
                let map = match member.member_type {
                    crate::osm::MemberType::Node => &node_id_map,
                    crate::osm::MemberType::Way => &way_id_map,
                    crate::osm::MemberType::Relation => &relation_id_map,
                };
                if let Some(&new_id) = map.get(&member.reference) {
                    member.reference = new_id;
                }
            }
            if let Some(&(new_id, new_version)) = result.relation_id_remap.get(&relation.id) {
                relation.id = new_id;
                relation.version = new_version;
            }
        }
    }

    /// Test-only: replace `osm_data` directly, leaving `original_data` and
    /// the derived caches alone.
    #[cfg(test)]
    pub(crate) fn set_osm_data_for_test(&mut self, data: Arc<OsmData>) {
        self.osm_data = Some(data);
        self.mark_modified();
    }

    // -- Cache patching shared by the edit primitives. Each `index_*` method
    // brings the derived caches and indices up to date for one change to
    // `osm_data`; none touches `osm_data`, `modified` or `revision`. --

    fn grow_layer_bbox(&mut self, b: WayBbox) {
        match &mut self.layer_bbox {
            Some(lb) => {
                lb.extend(b.min_x, b.min_y);
                lb.extend(b.max_x, b.max_y);
            }
            None => self.layer_bbox = Some(b),
        }
    }

    /// Make the caches and indices show `node` at its current position and
    /// style, or drop it from them if its coordinates are invalid (as
    /// `compute_node_cache` never includes such nodes). `layer_bbox` only
    /// grows.
    fn index_node_set(&mut self, node: &OsmNode) {
        let id = node.id;
        let Some((lat, lon)) = validate_coords(node.lat, node.lon) else {
            self.index_node_removed(id);
            return;
        };
        let (mx, my) = lat_lon_to_mercator(lat, lon);
        let style = self.stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM);
        if let Some(&idx) = self.node_cache.index_by_id.get(&id) {
            let (_, old_mx, old_my) = self.node_cache.flat[idx];
            self.node_index
                .remove(&GeomWithData::new([old_mx, old_my], id));
            self.node_cache.flat[idx] = (id, mx, my);
            self.node_cache.styles[idx] = style;
        } else {
            let idx = self.node_cache.flat.len();
            self.node_cache.flat.push((id, mx, my));
            self.node_cache.styles.push(style);
            self.node_cache.index_by_id.insert(id, idx);
        }
        self.node_index.insert(GeomWithData::new([mx, my], id));
        self.grow_layer_bbox(WayBbox {
            min_x: mx,
            max_x: mx,
            min_y: my,
            max_y: my,
        });
    }

    /// Drop a node from the position caches and indices, including its
    /// `node_to_ways` entry.
    fn index_node_removed(&mut self, id: i64) {
        if let Some(idx) = self.node_cache.index_by_id.remove(&id) {
            let (_, mx, my) = self.node_cache.flat[idx];
            self.node_index.remove(&GeomWithData::new([mx, my], id));
            self.node_cache.flat.remove(idx);
            self.node_cache.styles.remove(idx);
            // Removing from the middle shifts every later index.
            for v in self.node_cache.index_by_id.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
        }
        self.node_to_ways.remove(&id);
    }

    fn way_style_of(&self, way: &OsmWay) -> WayStyle {
        self.stylesheet.way_style_cached(
            &way.tags,
            way.is_closed(),
            self.member_ways.contains(&way.id),
        )
    }

    /// Recompute the vertices, bbox, style and fill of the way at `way_idx`
    /// from `way` (a snapshot, so no borrow of `osm_data` is held) and the
    /// current node cache, replacing its R-tree entry. Callers refresh the
    /// multipolygons afterwards.
    fn reproject_way(&mut self, way_idx: usize, way: &OsmWay) {
        if let Some(old) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&way_rtree_entry(old, way.id));
        }
        let (verts, bbox) = project_way_vertices(way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(way_rtree_entry(b, way.id));
            self.grow_layer_bbox(b);
        }
        let style = self.way_style_of(way);
        self.way_fill_tris[way_idx] = compute_fill_tris(&verts, &style);
        self.way_vertices[way_idx] = verts;
        self.way_bboxes[way_idx] = bbox;
        self.way_styles[way_idx] = style;
    }

    /// Append a way to every index-aligned cache and the indices. Callers
    /// refresh the multipolygons afterwards.
    fn index_way_added(&mut self, way: &OsmWay) {
        let (verts, bbox) = project_way_vertices(way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(way_rtree_entry(b, way.id));
            self.grow_layer_bbox(b);
        }
        let way_idx = self.way_vertices.len();
        self.way_id_to_index.insert(way.id, way_idx);
        for nid in &way.nodes {
            self.node_to_ways.entry(*nid).or_default().insert(way_idx);
        }
        let style = self.way_style_of(way);
        self.way_ids.push(way.id);
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
    }

    /// Remove the way at `way_idx` from every index-aligned cache and the
    /// indices, shifting the indices of the ways after it.
    fn index_way_removed(&mut self, way_idx: usize, way_id: i64) {
        if let Some(old) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&way_rtree_entry(old, way_id));
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
            *ways = ways
                .iter()
                .filter(|&&w| w != way_idx)
                .map(|&w| if w > way_idx { w - 1 } else { w })
                .collect();
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());
    }

    /// Update `node_to_ways` for the way at `way_idx` after its node list
    /// went from `old` to `new`.
    fn index_way_nodes_changed(&mut self, way_idx: usize, old: &[i64], new: &[i64]) {
        let kept: HashSet<i64> = new.iter().copied().collect();
        for dropped in old.iter().filter(|n| !kept.contains(n)) {
            if let Some(ways) = self.node_to_ways.get_mut(dropped) {
                ways.remove(&way_idx);
                if ways.is_empty() {
                    self.node_to_ways.remove(dropped);
                }
            }
        }
        for &added in new {
            self.node_to_ways.entry(added).or_default().insert(way_idx);
        }
    }

    /// Apply `edit` to the node list of way `way_id`, then bring the way's
    /// caches and the multipolygons up to date and mark the layer modified.
    /// `edit` returns `Err`, having changed nothing, to refuse.
    fn edit_way_nodes<R>(
        &mut self,
        way_id: i64,
        edit: impl FnOnce(&mut Vec<i64>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        let &way_idx = self
            .way_id_to_index
            .get(&way_id)
            .ok_or(EditError::UnknownWay(way_id))?;
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        let way = data
            .ways
            .get_mut(&way_id)
            .ok_or(EditError::UnknownWay(way_id))?;
        let old = way.nodes.clone();
        let result = edit(&mut way.nodes)?;
        let snapshot = way.clone();
        self.index_way_nodes_changed(way_idx, &old, &snapshot.nodes);
        self.reproject_way(way_idx, &snapshot);
        self.mark_modified();
        self.mp_refresh_ways(&[way_id]);
        Ok(result)
    }

    /// Why `take_node` refused: the node is in a way, or does not exist.
    fn why_node_stays(&self, id: i64) -> EditError {
        if self.node_to_ways.get(&id).is_some_and(|w| !w.is_empty()) {
            EditError::NodeInWay(id)
        } else {
            EditError::UnknownNode(id)
        }
    }

    /// Apply `edit` to the tags of a node, way or relation, then refresh the
    /// cached style that follows from them.
    fn edit_tags(
        &mut self,
        kind: FeatureKind,
        id: i64,
        edit: impl FnOnce(&mut HashMap<String, String>),
    ) -> Result<(), EditError> {
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.get_mut(&id).map(|w| &mut w.tags),
            FeatureKind::Relation => data
                .relations
                .iter_mut()
                .find(|r| r.id == id)
                .map(|r| &mut r.tags),
        };
        let tags = tags.ok_or_else(|| EditError::unknown(kind, id))?;
        edit(tags);
        self.modified = true;
        self.revision += 1;
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
            &self.member_ways,
        );
        if kind == FeatureKind::Relation {
            // The relation's style (or whether it is a multipolygon at all)
            // follows its tags.
            self.mp_refresh_relations(&[id]);
        }
        Ok(())
    }

    /// Bring `member_ways` and the styles of the ways whose membership
    /// changed up to date after a relation was created, deleted or its
    /// members replaced. Does nothing for a stylesheet with no
    /// `way:in-relation` rules.
    fn refresh_member_ways(&mut self) {
        if !self.stylesheet.uses_relation_membership() {
            return;
        }
        let Some(data) = self.osm_data.clone() else {
            return;
        };
        let now = relation_member_ways(&data, &self.stylesheet);
        let changed: Vec<i64> = now
            .symmetric_difference(&self.member_ways)
            .copied()
            .collect();
        self.member_ways = now;
        for id in changed {
            let (Some(&idx), Some(way)) = (self.way_id_to_index.get(&id), data.ways.get(&id))
            else {
                continue;
            };
            let style = self.stylesheet.way_style_cached(
                &way.tags,
                way.is_closed(),
                self.member_ways.contains(&id),
            );
            self.way_fill_tris[idx] = compute_fill_tris(&self.way_vertices[idx], &style);
            self.way_styles[idx] = style;
        }
    }

    /// Reassemble and retriangulate these relations (and drop any that are
    /// gone or no longer multipolygons). Every edit that can change a
    /// relation's rings ends in one of the `mp_refresh_*` calls.
    fn mp_refresh_relations(&mut self, relation_ids: &[i64]) {
        let Some(data) = self.osm_data.clone() else {
            return;
        };
        let fs_phase = crate::frame_stats::start();
        self.mp
            .refresh(relation_ids, &data, &self.node_cache, &self.stylesheet);
        if !relation_ids.is_empty() {
            crate::frame_stats::lap("cache.mp_refresh", fs_phase);
        }
    }

    /// Refresh the multipolygon relations that list any of these ways. Cheap
    /// (a map lookup) when none does, which is the common case.
    fn mp_refresh_ways(&mut self, way_ids: &[i64]) {
        let mut relations: Vec<i64> = way_ids
            .iter()
            .filter_map(|w| self.mp.way_to_rels.get(w))
            .flatten()
            .copied()
            .collect();
        relations.sort_unstable();
        relations.dedup();
        self.mp_refresh_relations(&relations);
    }

    /// The route overlay colour of a way, as the renderer sees it.
    #[cfg(test)]
    pub(crate) fn route_overlay_colour(&self, way_id: i64) -> Option<u32> {
        self.mp.route_colour_of_way(way_id)
    }

    /// The drawn restriction markers: `(relation id, mercator x, y, only)`.
    #[cfg(test)]
    pub(crate) fn restriction_markers(&self) -> Vec<(i64, f64, f64, bool)> {
        self.mp
            .restrictions
            .iter()
            .map(|(&id, r)| (id, r.x, r.y, r.only))
            .collect()
    }

    /// Refresh the turn restrictions whose `via` node is one of these.
    fn mp_refresh_nodes(&mut self, node_ids: &[i64]) {
        if self.mp.node_to_rels.is_empty() {
            return;
        }
        let mut relations: Vec<i64> = node_ids
            .iter()
            .filter_map(|n| self.mp.node_to_rels.get(n))
            .flatten()
            .copied()
            .collect();
        relations.sort_unstable();
        relations.dedup();
        self.mp_refresh_relations(&relations);
    }

    /// How many times the multipolygon cache has been rebuilt or patched.
    #[cfg(test)]
    pub(crate) fn multipolygon_version(&self) -> u64 {
        self.mp.version
    }

    /// The cached mercator position of `node_id` in the relation's polygons,
    /// and the relation's total fill triangle count.
    #[cfg(test)]
    pub(crate) fn multipolygon_geometry(
        &self,
        relation_id: i64,
        node_id: i64,
    ) -> Option<((f64, f64), usize)> {
        let entry = self.mp.entries.get(&relation_id)?;
        let tris = entry.polygons.iter().map(|p| p.tris.len() / 3).sum();
        let pos = entry
            .polygons
            .iter()
            .flat_map(|p| p.verts.iter())
            .find(|v| v.0 == node_id)
            .map(|v| (v.1, v.2))?;
        Some((pos, tris))
    }

    /// What became of this relation in the multipolygon cache.
    #[cfg(test)]
    pub(crate) fn multipolygon_state(&self, relation_id: i64) -> MpState {
        if let Some(entry) = self.mp.entries.get(&relation_id) {
            MpState::Valid {
                polygons: entry.polygons.len(),
                holes: entry.polygons.iter().map(|p| p.ring_count() - 1).sum(),
            }
        } else if let Some(&problem) = self.mp.invalid.get(&relation_id) {
            MpState::Invalid(problem)
        } else {
            MpState::NotArea
        }
    }

    fn relations_containing(&self, member_type: crate::osm::MemberType, id: i64) -> Vec<i64> {
        self.osm_data
            .as_ref()
            .map(|d| {
                d.relations
                    .iter()
                    .filter(|r| {
                        r.members
                            .iter()
                            .any(|m| m.member_type == member_type && m.reference == id)
                    })
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Insert (or overwrite) a node, creating empty data if the layer has
    /// none, and patch the caches. Keeps `next_new_id` below `id` so a later
    /// auto-allocation never collides with an explicitly inserted id (a redo).
    fn insert_node(
        &mut self,
        id: i64,
        lat: f64,
        lon: f64,
        tags: HashMap<String, String>,
        version: i32,
    ) {
        let node = OsmNode {
            id,
            lat,
            lon,
            version,
            tags,
        };
        let arc = self
            .osm_data
            .get_or_insert_with(|| Arc::new(OsmData::default()));
        Arc::make_mut(arc).nodes.insert(id, node.clone());
        self.index_node_set(&node);
        if id <= self.next_new_id {
            self.next_new_id = id - 1;
        }
        self.mark_modified();
    }

    /// Remove a node from the data and caches. Refuses (`None`, nothing
    /// changed) if the node is unknown or still in a way: removing it would
    /// leave the way pointing at nothing, so the way goes first.
    fn take_node(&mut self, id: i64) -> Option<OsmNode> {
        if self
            .node_to_ways
            .get(&id)
            .is_some_and(|ways| !ways.is_empty())
        {
            return None;
        }
        let node = Arc::make_mut(self.osm_data.as_mut()?).nodes.remove(&id)?;
        self.index_node_removed(id);
        self.mark_modified();
        self.mp_refresh_nodes(&[id]);
        Some(node)
    }

    /// Remove a way from the data and caches; its nodes stay. `None`
    /// (nothing changed) if the way is unknown.
    fn take_way(&mut self, id: i64) -> Option<OsmWay> {
        let way_idx = *self.way_id_to_index.get(&id)?;
        let way = Arc::make_mut(self.osm_data.as_mut()?).ways.remove(&id)?;
        self.index_way_removed(way_idx, id);
        self.mark_modified();
        self.mp_refresh_ways(&[id]);
        Some(way)
    }

    /// Add `way` to the data and every cache, creating empty data if the
    /// layer has none. Member nodes that are not currently cached (deleted,
    /// not yet restored) contribute no vertex, as in `compute_way_tables`.
    fn insert_way(&mut self, way: OsmWay) {
        let id = way.id;
        self.index_way_added(&way);
        let arc = self
            .osm_data
            .get_or_insert_with(|| Arc::new(OsmData::default()));
        Arc::make_mut(arc).ways.insert(id, way);
        self.mark_modified();
        self.mp_refresh_ways(&[id]);
    }

    /// The data as a snapshot: edits go through `Arc::make_mut`, so a held
    /// snapshot never changes, and costs the next edit one copy of the
    /// dataset. Keep snapshots short-lived.
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
        self.mp = MpCache {
            version: self.mp.version + 1,
            ..MpCache::default()
        };
        self.layer_bbox = None;
        self.download_rects.clear();
        self.node_cache.index_by_id.clear();
        self.node_cache.flat.clear();
        self.node_cache.styles.clear();
        self.node_index = RTree::new();
        self.way_index = RTree::new();
        self.way_id_to_index.clear();
        self.node_to_ways.clear();
        self.drag_preview = None;
        self.modified = false;
        self.revision += 1;
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
        self.revision += 1;
        if let Some(data) = self.osm_data.clone() {
            self.node_cache = compute_node_cache(&data, &self.stylesheet);
            self.member_ways = relation_member_ways(&data, &self.stylesheet);
            let (ids, bboxes, verts, styles, fill_tris) =
                compute_way_tables(&data, &self.node_cache, &self.stylesheet, &self.member_ways);
            self.way_ids = ids;
            self.way_bboxes = bboxes;
            self.way_vertices = verts;
            self.way_styles = styles;
            self.way_fill_tris = fill_tris;
            self.mp
                .rebuild_all(&data, &self.node_cache, &self.stylesheet);
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

    /// Marker radius a tagged node is drawn with at `zoom` (0 when hidden).
    fn label_marker_radius(style: NodeStyle, zoom: f64) -> f32 {
        match node_markers::node_form(style.tagged, false, false, zoom) {
            NodeForm::Hidden => 0.0,
            NodeForm::Simple(edge) => edge / 2.0,
            _ => (style.size + 2.0) / 2.0,
        }
    }

    /// The (cached) label of node `id`, resolved from the stylesheet at
    /// `zoom` on first use.
    fn node_label_for(
        &self,
        cache: &mut labels::LabelCache,
        id: i64,
        zoom: f64,
    ) -> Option<Arc<labels::NodeLabel>> {
        let labels::LabelCache { nodes, icons, .. } = cache;
        nodes
            .entry(id)
            .or_insert_with(|| {
                let tags = &self.osm_data.as_ref()?.nodes.get(&id)?.tags;
                let st = self.stylesheet.node_text_style(tags, zoom);
                labels::resolve_node_label(&st, tags, icons, zoom).map(Arc::new)
            })
            .clone()
    }

    /// Identifies what label placement depends on besides the data: the
    /// stylesheet, and the benchmark knobs (so a script changing them
    /// forces a new placement).
    fn label_sheet_id(&self) -> usize {
        Arc::as_ptr(&self.stylesheet) as usize ^ crate::layers::line_labels::bench_stamp()
    }

    /// The (cached) label text and style of the area `key` (a closed way or
    /// a tagged multipolygon relation) at `zoom`.
    fn area_label_for(
        &self,
        cache: &mut labels::LabelCache,
        key: labels::Key,
        zoom: f64,
    ) -> Option<Arc<area_labels::AreaLabel>> {
        cache.areas.label(key, || {
            let data = self.osm_data.as_ref()?;
            if key.0 == labels::KIND_REL {
                let e = self.mp.entries.get(&key.1)?;
                if !e.tagged {
                    // Old-style: the outer way carries the tags and its own
                    // label.
                    return None;
                }
                let st = self.stylesheet.way_text_style(&e.tags, true, zoom);
                area_labels::resolve_area_label(&st, &e.tags, zoom)
            } else {
                let way = data.ways.get(&key.1)?;
                if !way.is_closed() {
                    return None;
                }
                let st = self.stylesheet.way_text_style(&way.tags, true, zoom);
                area_labels::resolve_area_label(&st, &way.tags, zoom)
            }
        })
    }

    /// The (cached) label geometry of the area `key`: its polygons in
    /// Mercator metres with their poles of inaccessibility. Keyed by a hash
    /// of the vertices, so it is rebuilt exactly when an edit changed them.
    fn area_geom_for(
        &self,
        cache: &mut labels::LabelCache,
        key: labels::Key,
    ) -> Option<Arc<area_labels::AreaGeom>> {
        use area_labels::{AreaGeom, Fingerprint};
        let xy = |v: &[(i64, f64, f64)]| -> Vec<(f64, f64)> {
            v.iter().map(|&(_, x, y)| (x, y)).collect()
        };
        if key.0 == labels::KIND_REL {
            let e = self.mp.entries.get(&key.1)?;
            let mut fp = Fingerprint::default();
            for p in &e.polygons {
                for k in 0..p.ring_count() {
                    fp.ring(p.ring(k).iter().map(|&(_, x, y)| (x, y)));
                }
            }
            cache.areas.geom(key, fp, || {
                AreaGeom::build(
                    e.polygons
                        .iter()
                        .map(|p| (0..p.ring_count()).map(|k| xy(p.ring(k))).collect())
                        .collect(),
                )
            })
        } else {
            let idx = *self.way_id_to_index.get(&key.1)?;
            let verts = &self.way_vertices[idx];
            if verts.len() < 4 || verts.first()?.0 != verts.last()?.0 {
                return None;
            }
            let mut fp = Fingerprint::default();
            fp.ring(verts.iter().map(|&(_, x, y)| (x, y)));
            cache
                .areas
                .geom(key, fp, || AreaGeom::build(vec![vec![xy(verts)]]))
        }
    }

    /// Locate and shape one area's label for the current view. `None` when
    /// it has no geometry, does not fit, or (unless `forced`) fits nowhere.
    #[allow(clippy::too_many_arguments)]
    fn locate_area_label(
        &self,
        cache: &mut labels::LabelCache,
        view: &area_labels::AreaView,
        key: labels::Key,
        label: &Arc<area_labels::AreaLabel>,
        forced: bool,
        budget: &mut usize,
        window: &mut Window,
    ) -> Option<(area_labels::Located, Arc<ShapedLine>)> {
        let geom = self.area_geom_for(cache, key)?;
        let line = cache.shapes.get(window, &label.text, label.font_px);
        let size = (f32::from(line.width), f32::from(line.ascent + line.descent));
        let scales = area_labels::font_scales(label.font_px);
        let at = area_labels::locate(&geom, view, size, &scales, forced, budget)?;
        let line = if at.scale == 1.0 {
            line
        } else {
            cache
                .shapes
                .get(window, &label.text, label.font_px * at.scale)
        };
        Some((at, line))
    }

    /// The named areas in view as placement items (`Anchor::Center`, no
    /// marker). Cheap for what is skipped: tiny areas are dropped on their
    /// cached bounding box before any lookup, labels and geometry are cached
    /// across placements, and only the top `MAX_AREA_CANDIDATES` by priority
    /// are located and shaped. An area whose text equals that of a node label
    /// inside it (a building and its own POI node) is dropped: the point
    /// label is the better one.
    #[allow(clippy::too_many_arguments)]
    fn area_items(
        &self,
        cache: &mut labels::LabelCache,
        viewport: &Viewport,
        cam: labels::CameraSig,
        forced: &HashSet<labels::Key>,
        node_texts: &HashMap<SharedString, Vec<(f64, f64)>>,
        window: &mut Window,
    ) -> Vec<AreaItem> {
        let zoom = cam.zoom;
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let view = area_labels::AreaView::new(vmin_x, vmax_x, vmax_y, cam.w, cam.h);
        let ppm = view.ppm;
        // (key, label, longer side in px)
        let mut found: Vec<(labels::Key, Arc<area_labels::AreaLabel>, f32)> = Vec::new();
        let tiny =
            |w: f64, h: f64| (w * ppm) < MIN_AREA_PX.0 as f64 || (h * ppm) < MIN_AREA_PX.1 as f64;
        let env = AABB::from_corners([vmin_x, vmin_y], [vmax_x, vmax_y]);
        for g in self.way_index.locate_in_envelope_intersecting(env) {
            let id = g.data;
            let key = (labels::KIND_WAY, id);
            let Some(&idx) = self.way_id_to_index.get(&id) else {
                continue;
            };
            let Some(b) = self.way_bboxes[idx] else {
                continue;
            };
            let (bw, bh) = (b.max_x - b.min_x, b.max_y - b.min_y);
            if tiny(bw, bh) && !forced.contains(&key) {
                continue;
            }
            if let Some(l) = self.area_label_for(cache, key, zoom) {
                found.push((key, l, ((bw.max(bh)) * ppm) as f32));
            }
        }
        for (&id, e) in &self.mp.entries {
            let key = (labels::KIND_REL, id);
            let b = &e.bbox;
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let (bw, bh) = (b.max_x - b.min_x, b.max_y - b.min_y);
            if tiny(bw, bh) && !forced.contains(&key) {
                continue;
            }
            if let Some(l) = self.area_label_for(cache, key, zoom) {
                found.push((key, l, ((bw.max(bh)) * ppm) as f32));
            }
        }
        // A member way repeating its relation's name is the same label.
        let rel_text: HashMap<i64, &SharedString> = found
            .iter()
            .filter(|f| f.0 .0 == labels::KIND_REL)
            .map(|f| (f.0 .1, &f.1.text))
            .collect();
        if !rel_text.is_empty() {
            let dup: HashSet<labels::Key> = found
                .iter()
                .filter(|f| f.0 .0 == labels::KIND_WAY)
                .filter(|f| {
                    self.mp.way_to_rels.get(&f.0 .1).is_some_and(|rels| {
                        rels.iter().any(|r| rel_text.get(r) == Some(&&f.1.text))
                    })
                })
                .map(|f| f.0)
                .collect();
            found.retain(|f| !dup.contains(&f.0));
        }
        let rank = |f: &(labels::Key, Arc<area_labels::AreaLabel>, f32)| {
            (
                forced.contains(&f.0),
                f.1.priority + area_labels::size_bonus(f.2),
            )
        };
        found.sort_by(|a, b| rank(b).cmp(&rank(a)).then(a.0.cmp(&b.0)));
        found.truncate(area_labels::MAX_AREA_CANDIDATES);

        let mut budget = area_labels::CLIP_BUDGET;
        let mut out = Vec::with_capacity(found.len());
        for (key, label, bpx) in found {
            let is_forced = forced.contains(&key);
            let Some((at, line)) =
                self.locate_area_label(cache, &view, key, &label, is_forced, &mut budget, window)
            else {
                continue;
            };
            if !is_forced && Self::names_a_node_inside(node_texts, &label.text, &at, &view) {
                continue;
            }
            let sp = viewport.mercator_to_screen(at.mx, at.my);
            let (w, h) = (f32::from(line.width), f32::from(line.ascent + line.descent));
            out.push(AreaItem {
                item: labels::Item {
                    key,
                    center: (f32::from(sp.x), f32::from(sp.y)),
                    marker_r: 0.0,
                    icon_r: None,
                    text: Some((w, h, labels::Anchor::Center)),
                    priority: label.priority + area_labels::size_bonus(bpx),
                    forced: is_forced,
                },
                label,
                line,
                mx: at.mx,
                my: at.my,
            });
        }
        out
    }

    /// Whether a node label with the same `text` sits near the located
    /// area label (within a text's width of it): then the area label would
    /// only repeat it.
    fn names_a_node_inside(
        node_texts: &HashMap<SharedString, Vec<(f64, f64)>>,
        text: &SharedString,
        at: &area_labels::Located,
        view: &area_labels::AreaView,
    ) -> bool {
        let Some(nodes) = node_texts.get(text) else {
            return false;
        };
        let reach = 60.0 / view.ppm;
        nodes
            .iter()
            .any(|&(x, y)| (x - at.mx).abs() < reach && (y - at.my).abs() < reach)
    }

    /// Place the labels and icons of the nodes in view (the expensive half;
    /// run when the camera settles, not every frame).
    fn place_labels(
        &self,
        cache: &mut labels::LabelCache,
        viewport: &Viewport,
        cam: labels::CameraSig,
        selected: &HashSet<i64>,
        forced_areas: &HashSet<labels::Key>,
        window: &mut Window,
    ) -> labels::Placement {
        let fs = crate::frame_stats::start();
        let zoom = cam.zoom;
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        // Nodes a little outside the view can still have a label that
        // reaches in.
        let margin = 150.0 / (cam.w as f64 / cam.span_x.max(1e-9));
        let mut cands: Vec<(usize, labels::Item, Arc<labels::NodeLabel>)> = Vec::new();
        let mut fixed: Vec<labels::Rect> = Vec::new();
        for (idx, &(id, mx, my)) in self.node_cache.flat.iter().enumerate() {
            if mx < vmin_x - margin
                || mx > vmax_x + margin
                || my < vmin_y - margin
                || my > vmax_y + margin
            {
                continue;
            }
            let style = self.node_cache.styles[idx];
            if !style.tagged {
                continue;
            }
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let c = (f32::from(sp.x), f32::from(sp.y));
            let r = Self::label_marker_radius(style, zoom);
            match self.node_label_for(cache, id, zoom) {
                Some(l) => {
                    let item = labels::Item {
                        key: (labels::KIND_NODE, id),
                        center: c,
                        marker_r: r,
                        icon_r: l.icon.as_ref().map(|i| i.backing() / 2.0),
                        text: None,
                        priority: l.priority,
                        forced: selected.contains(&id),
                    };
                    cands.push((idx, item, l));
                }
                None if r > 0.0 => fixed.push(labels::Rect::around(c.0, c.1, r, r)),
                None => {}
            }
        }
        // Keep the most important candidates; the rest are plain obstacles.
        if cands.len() > labels::MAX_CANDIDATES {
            cands.sort_by(|a, b| {
                b.1.forced
                    .cmp(&a.1.forced)
                    .then(b.1.priority.cmp(&a.1.priority))
                    .then(a.1.key.cmp(&b.1.key))
            });
            for (_, it, _) in cands.drain(labels::MAX_CANDIDATES..) {
                if it.marker_r > 0.0 {
                    fixed.push(labels::Rect::around(
                        it.center.0,
                        it.center.1,
                        it.marker_r,
                        it.marker_r,
                    ));
                }
            }
        }
        // Shape (cached) to learn each text's size.
        let mut lines: Vec<Option<Arc<ShapedLine>>> = Vec::with_capacity(cands.len());
        for (_, it, l) in cands.iter_mut() {
            match &l.text {
                Some(t) => {
                    let line = cache.shapes.get(window, t, l.font_px);
                    let w = f32::from(line.width);
                    let h = f32::from(line.ascent + line.descent);
                    it.text = Some((w, h, l.pref));
                    lines.push(Some(line));
                }
                None => lines.push(None),
            }
        }
        let mut items: Vec<labels::Item> = cands.iter().map(|c| c.1.clone()).collect();
        // Named areas (after the nodes, so their item indices follow).
        let n_nodes = items.len();
        let mut node_texts: HashMap<SharedString, Vec<(f64, f64)>> = HashMap::new();
        for (idx, _, l) in &cands {
            if let Some(t) = &l.text {
                let (_, mx, my) = self.node_cache.flat[*idx];
                node_texts.entry(t.clone()).or_default().push((mx, my));
            }
        }
        let areas = self.area_items(cache, viewport, cam, forced_areas, &node_texts, window);
        items.extend(areas.iter().map(|a| a.item.clone()));
        let prev = cache.prev_state();
        let out = labels::place(
            &items,
            &fixed,
            &prev,
            &labels::Config::for_view(cam.w, cam.h),
        );
        let mut placed = Vec::with_capacity(out.len());
        let mut by_id = HashMap::with_capacity(out.len());
        for p in out {
            if p.item >= n_nodes {
                let a = &areas[p.item - n_nodes];
                if let Some((_, r)) = p.text {
                    placed.push(labels::PlacedLabel::new_area(
                        a.item.key.0,
                        a.item.key.1,
                        a.mx,
                        a.my,
                        labels::PlacedText {
                            line: a.line.clone(),
                            dx: r.x0 - a.item.center.0,
                            dy: r.y0 - a.item.center.1,
                            color: a.label.color,
                            halo_color: a.label.halo_color,
                            halo_radius: a.label.halo_radius,
                            halo_alpha: a.label.halo_alpha,
                        },
                    ));
                }
                continue;
            }
            let (idx, item, l) = &cands[p.item];
            let (id, mx, my) = self.node_cache.flat[*idx];
            let icon = if p.icon {
                l.icon.clone().map(|choice| labels::PlacedIcon {
                    choice,
                    fill: self.node_cache.styles[*idx].color,
                })
            } else {
                None
            };
            let text = match (p.text, &lines[p.item]) {
                (Some((anchor, r)), Some(line)) => Some((
                    labels::PlacedText {
                        line: line.clone(),
                        dx: r.x0 - item.center.0,
                        dy: r.y0 - item.center.1,
                        color: l.color,
                        halo_color: l.halo_color,
                        halo_radius: l.halo_radius,
                        halo_alpha: l.halo_alpha,
                    },
                    anchor,
                )),
                _ => None,
            };
            by_id.insert(id, placed.len());
            placed.push(labels::PlacedLabel::new(id, mx, my, icon, text));
        }
        let n_text = placed.iter().filter(|l| l.text.is_some()).count();
        crate::frame_stats::lap("labels.place", fs);
        crate::frame_stats::count("labels.candidates", items.len());
        crate::frame_stats::count("labels.placed", placed.len());
        labels::Placement {
            cam,
            revision: self.revision,
            sheet: self.label_sheet_id(),
            labels: placed,
            by_id,
            fade_end: None,
            halo: labels::halo_for(n_text),
        }
    }

    /// Paint labels and POI icons above everything `render_canvas` and the
    /// highlights drew; `selected` and `hovered` keep their labels even when
    /// collision would drop them. Places when the camera has settled (or the
    /// data changed), otherwise just re-projects the last placement.
    #[allow(clippy::too_many_arguments)]
    pub fn render_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        selected: &[FeatureRef],
        hovered: Option<&FeatureRef>,
        window: &mut Window,
        cx: &App,
    ) {
        let zoom = viewport.zoom_level();
        if self.osm_data.is_none() || zoom < labels::LABEL_MIN_ZOOM {
            return;
        }
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        if let Some(lb) = &self.layer_bbox {
            if lb.max_x < vmin_x || lb.min_x > vmax_x || lb.max_y < vmin_y || lb.min_y > vmax_y {
                return;
            }
        }
        let fs = crate::frame_stats::start();
        let cam = labels::CameraSig {
            zoom,
            min_x: vmin_x,
            max_y: vmax_y,
            span_x: vmax_x - vmin_x,
            w: f32::from(viewport.transform.screen_size.width),
            h: f32::from(viewport.transform.screen_size.height),
        };
        let sheet = self.label_sheet_id();
        let mut cache = self.label_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.validate(self.revision, sheet, zoom.floor() as i32);
        let now = std::time::Instant::now();
        let step = cache.step(cam, self.revision, sheet, now);
        if step == labels::Step::Recompute {
            let ids: HashSet<i64> = selected
                .iter()
                .chain(hovered)
                .filter(|f| f.layer_id == self.id && f.kind == FeatureKind::Node)
                .map(|f| f.id)
                .collect();
            let forced_areas: HashSet<labels::Key> = selected
                .iter()
                .chain(hovered)
                .filter(|f| f.layer_id == self.id)
                .filter_map(|f| match f.kind {
                    FeatureKind::Way => Some((labels::KIND_WAY, f.id)),
                    FeatureKind::Relation => Some((labels::KIND_REL, f.id)),
                    FeatureKind::Node => None,
                })
                .collect();
            let p = self.place_labels(&mut cache, viewport, cam, &ids, &forced_areas, window);
            self.place_line_labels(viewport, cam, &p, now);
            cache.store(p, now);
        }
        // Frames are requested only while something is in flight: waiting to
        // settle, or a fade-in running.
        let line_fade_end = self
            .line_placed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fade_end;
        let fade_end = cache
            .placement
            .as_ref()
            .and_then(|p| p.fade_end)
            .max(line_fade_end);
        if labels::wants_frame(step, fade_end, now) {
            window.request_animation_frame();
        }
        if step == labels::Step::HideAndWait {
            return;
        }
        // Street names under the point labels and icons they yielded to.
        self.paint_line_labels(viewport, bounds, now, window);
        let ox = bounds.origin.x;
        let oy = bounds.origin.y;
        let (w, h) = (cam.w, cam.h);
        let on_screen = |x: f32, y: f32| x > -300.0 && x < w + 300.0 && y > -100.0 && y < h + 100.0;
        // Taken out of the cache so painting can also borrow it for the
        // forced labels below.
        let Some(placement) = cache.placement.take() else {
            return;
        };
        let halo = placement.halo;
        for l in &placement.labels {
            let drag = if l.kind == labels::KIND_NODE {
                self.drag_preview_offset(l.id)
            } else {
                Point::default()
            };
            let sp = viewport.mercator_to_screen(l.mx, l.my) + drag;
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let at = point(sp.x + ox, sp.y + oy);
            let alpha = labels::fade_alpha(l.born, now);
            if let Some(icon) = &l.icon {
                labels::paint_icon(window, cx, icon, at, alpha);
            }
            if let Some(t) = &l.text {
                labels::paint_text(
                    window,
                    t,
                    point(at.x + px(t.dx), at.y + px(t.dy)),
                    alpha,
                    halo,
                );
            }
        }
        // Selected and hovered nodes keep their label even where the
        // placement dropped it: drawn beside the marker, collision ignored.
        for f in selected.iter().chain(hovered) {
            if f.layer_id != self.id
                || f.kind != FeatureKind::Node
                || placement
                    .by_id
                    .get(&f.id)
                    .is_some_and(|&i| placement.labels[i].text.is_some())
            {
                continue;
            }
            let Some(&idx) = self.node_cache.index_by_id.get(&f.id) else {
                continue;
            };
            let Some(label) = self.node_label_for(&mut cache, f.id, zoom) else {
                continue;
            };
            let Some(text) = &label.text else { continue };
            let (_, mx, my) = self.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my) + self.drag_preview_offset(f.id);
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let line = cache.shapes.get(window, text, label.font_px);
            let reach = Self::label_marker_radius(self.node_cache.styles[idx], zoom)
                .max((self.node_cache.styles[idx].size + 2.0) / 2.0);
            let r = labels::label_rect(
                label.pref,
                (0.0, 0.0),
                reach,
                3.0,
                f32::from(line.width),
                f32::from(line.ascent + line.descent),
            );
            let t = labels::PlacedText {
                line,
                dx: r.x0,
                dy: r.y0,
                color: label.color,
                halo_color: label.halo_color,
                halo_radius: label.halo_radius,
                halo_alpha: label.halo_alpha,
            };
            let at = point(sp.x + ox, sp.y + oy);
            labels::paint_text(
                window,
                &t,
                point(at.x + px(t.dx), at.y + px(t.dy)),
                1.0,
                labels::HaloMode::Offsets,
            );
        }
        // Likewise a selected or hovered area whose label was not placed
        // (the selection changed since): shown at its label point, fitted or
        // not, collision ignored.
        let mut forced_area_budget = area_labels::CLIP_BUDGET;
        let view = area_labels::AreaView::new(vmin_x, vmax_x, vmax_y, w, h);
        for f in selected.iter().chain(hovered) {
            let kind = match f.kind {
                FeatureKind::Way => labels::KIND_WAY,
                FeatureKind::Relation => labels::KIND_REL,
                FeatureKind::Node => continue,
            };
            if f.layer_id != self.id
                || placement
                    .labels
                    .iter()
                    .any(|l| l.kind == kind && l.id == f.id && l.text.is_some())
            {
                continue;
            }
            let key = (kind, f.id);
            let Some(label) = self.area_label_for(&mut cache, key, zoom) else {
                continue;
            };
            let Some((at, line)) = self.locate_area_label(
                &mut cache,
                &view,
                key,
                &label,
                true,
                &mut forced_area_budget,
                window,
            ) else {
                continue;
            };
            let sp = viewport.mercator_to_screen(at.mx, at.my);
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let t = labels::PlacedText {
                dx: -f32::from(line.width) / 2.0,
                dy: -f32::from(line.ascent + line.descent) / 2.0,
                line,
                color: label.color,
                halo_color: label.halo_color,
                halo_radius: label.halo_radius,
                halo_alpha: label.halo_alpha,
            };
            let atp = point(sp.x + ox, sp.y + oy);
            labels::paint_text(
                window,
                &t,
                point(atp.x + px(t.dx), atp.y + px(t.dy)),
                1.0,
                labels::HaloMode::Offsets,
            );
        }
        cache.placement = Some(placement);
        crate::frame_stats::lap("labels.paint", fs);
    }

    /// The CPU half of the multipolygon fill pass of `render_canvas` (cull,
    /// project, emit triangles — everything but handing the path to the GPU),
    /// run through the same code, and the number of path vertices it
    /// produced. For `examples/perf_bench.rs`, which has no window.
    pub fn multipolygon_fill_cpu(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> usize {
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let screen_rect = (
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.origin.x + bounds.size.width),
            f32::from(bounds.origin.y + bounds.size.height),
        );
        let mut path: Option<Path<Pixels>> = None;
        let mut min_max = (0.0, 0.0, 0.0, 0.0);
        let mut pts = Vec::new();
        for entry in self.mp.entries.values() {
            let b = &entry.bbox;
            if entry.style.fill.is_none()
                || b.max_x < vmin_x
                || b.min_x > vmax_x
                || b.max_y < vmin_y
                || b.min_y > vmax_y
            {
                continue;
            }
            for polygon in &entry.polygons {
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            }
        }
        path.map_or(0, |p| p.vertices.len())
    }

    /// Paint a multipolygon's fill (holes left out) as a translucent wash in
    /// `color`, for the selection and hover highlights. Triangulates on the
    /// fly only when the relation's own style has no fill (the cached
    /// triangles exist only for filled styles); that's a per-selection cost,
    /// not a per-edit one.
    fn render_area_tint(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        entry: &MpEntry,
        color: u32,
        kind: HighlightKind,
    ) {
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let b = &entry.bbox;
        if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
            return;
        }
        let alpha = match kind {
            HighlightKind::Selected => 0.30,
            _ => 0.20,
        };
        let screen_rect = (
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.origin.x + bounds.size.width),
            f32::from(bounds.origin.y + bounds.size.height),
        );
        let mut path: Option<Path<Pixels>> = None;
        let mut min_max = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        let mut pts = Vec::new();
        for polygon in &entry.polygons {
            if polygon.tris.is_empty() {
                // Unfilled style: triangulate this polygon just for the wash.
                let rings: Vec<Vec<(f64, f64)>> = (0..polygon.ring_count())
                    .map(|k| polygon.ring(k).iter().map(|&(_, x, y)| (x, y)).collect())
                    .collect();
                let refs: Vec<&[(f64, f64)]> = rings.iter().map(|r| r.as_slice()).collect();
                let Some(tris) = crate::multipolygon::triangulate(&refs) else {
                    continue;
                };
                let filled = MpPolygon {
                    tris,
                    ..polygon.clone()
                };
                self.push_polygon_fill(
                    &filled,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            } else {
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            }
        }
        if let Some(mut path) = path {
            let (min_x, max_x, min_y, max_y) = min_max;
            path.bounds = Bounds {
                origin: point(px(min_x), px(min_y)),
                size: size(px(max_x - min_x), px(max_y - min_y)),
            };
            let a = (alpha * 255.0) as u32;
            window.paint_path(path, rgba((color << 8) | a));
        }
    }

    /// Project `polygon` to the screen and append its fill triangles to
    /// `path`. `screen_rect` is `(left, top, right, bottom)` of the canvas in
    /// window pixels; triangles wholly outside it are skipped, so a huge
    /// multipolygon zoomed in on costs only what is visible. A polygon with
    /// an unprojectable vertex is skipped whole (the triangle indices would
    /// no longer line up).
    #[allow(clippy::too_many_arguments)]
    fn push_polygon_fill(
        &self,
        polygon: &MpPolygon,
        viewport: &Viewport,
        origin: (Pixels, Pixels),
        screen_rect: (f32, f32, f32, f32),
        pts: &mut Vec<Point<Pixels>>,
        path: &mut Option<Path<Pixels>>,
        bounds_min_max: &mut (f32, f32, f32, f32),
    ) {
        pts.clear();
        for &(node_id, mx, my) in &polygon.verts {
            let mut sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                return;
            }
            sp += self.drag_preview_offset(node_id);
            pts.push(point(sp.x + origin.0, sp.y + origin.1));
        }
        let (left, top, right, bottom) = screen_rect;
        for t in polygon.tris.as_chunks::<3>().0 {
            let tri = [pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]];
            let xs = tri.map(|p| f32::from(p.x));
            let ys = tri.map(|p| f32::from(p.y));
            if xs.iter().all(|&x| x < left)
                || xs.iter().all(|&x| x > right)
                || ys.iter().all(|&y| y < top)
                || ys.iter().all(|&y| y > bottom)
            {
                continue;
            }
            push_triangle(path, bounds_min_max, tri[0], tri[1], tri[2]);
        }
    }

    /// The nodes shown in full whatever the zoom: selected nodes and the
    /// nodes of selected ways (as node ids and way indices).
    fn pins(&self) -> Pins {
        let mut pins = Pins::default();
        for f in &self.highlight {
            match f.kind {
                FeatureKind::Node => {
                    pins.nodes.insert(f.id);
                }
                FeatureKind::Way => {
                    if let Some(&i) = self.way_id_to_index.get(&f.id) {
                        pins.ways.insert(i);
                    }
                }
                FeatureKind::Relation => {}
            }
        }
        pins.preview = self
            .preview_way
            .and_then(|w| self.way_id_to_index.get(&w).copied());
        pins
    }

    /// How node `id` (at `idx` in the node cache) is shown at `zoom`: the
    /// one answer behind drawing, hover, click and box select.
    fn node_form_at(&self, pins: &Pins, idx: usize, id: i64, zoom: f64) -> NodeForm {
        let in_way = |pred: &dyn Fn(&usize) -> bool| {
            self.node_to_ways
                .get(&id)
                .is_some_and(|ws| ws.iter().any(pred))
        };
        let pinned = pins.nodes.contains(&id)
            || (!pins.ways.is_empty() && in_way(&|w| pins.ways.contains(w)));
        let previewed = pins.preview.is_some_and(|p| in_way(&|w| *w == p));
        let style = self.node_cache.styles[idx];
        let tagged = style.tagged;
        let form = node_markers::node_form_previewed(
            tagged,
            self.is_standalone(tagged, id, zoom),
            pinned,
            previewed,
            zoom,
        );
        match self.styled_node_form(&style, id, zoom) {
            Some(styled) if !pinned => styled,
            _ => form,
        }
    }

    /// The stylesheet's own marker for node `id`, if its rules ask for a
    /// hollow square or a cross for a node in as many ways as this one and
    /// the view is zoomed in far enough.
    fn styled_node_form(&self, style: &NodeStyle, id: i64, zoom: f64) -> Option<NodeForm> {
        if zoom < node_markers::STYLED_MIN_ZOOM || !style.has_custom_marker() {
            return None;
        }
        let ways = self.node_to_ways.get(&id).map_or(0, |w| w.len());
        let visual = style.visual(ways);
        visual.shape.is_custom().then_some(NodeForm::Styled(visual))
    }

    /// The way whose vertices are currently previewed, if any.
    pub fn vertex_preview_way(&self) -> Option<i64> {
        self.preview_way
    }

    /// Whether node `node_id` is a vertex of way `way_id`.
    fn node_in_way(&self, node_id: i64, way_id: i64) -> bool {
        self.way_id_to_index.get(&way_id).is_some_and(|idx| {
            self.node_to_ways
                .get(&node_id)
                .is_some_and(|ws| ws.contains(idx))
        })
    }

    /// Distance in pixels from `pt` to the polyline of way `way_id`.
    fn way_screen_distance(
        &self,
        viewport: &Viewport,
        pt: Point<Pixels>,
        way_id: i64,
    ) -> Option<f32> {
        let verts = &self.way_vertices[*self.way_id_to_index.get(&way_id)?];
        let mut best: Option<f32> = None;
        let mut prev: Option<Point<Pixels>> = None;
        for &(_, mx, my) in verts.iter() {
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            if let Some(p0) = prev {
                let d = point_to_segment_distance(pt, p0, sp);
                best = Some(best.map_or(d, |b| b.min(d)));
            }
            prev = Some(sp);
        }
        best
    }

    /// Whether node `id` is an untagged node that belongs to no way, so it
    /// is drawn like a tagged one. The way lookup only runs for untagged
    /// nodes at zooms where a standalone node would be drawn at all.
    fn is_standalone(&self, tagged: bool, id: i64, zoom: f64) -> bool {
        !tagged
            && zoom >= node_markers::TAGGED_MIN_ZOOM
            && self
                .node_to_ways
                .get(&id)
                .is_none_or(|ways| ways.is_empty())
    }

    /// The node nearest `screen_pt` within `tol_px`, whether or not its
    /// marker is drawn. Add mode snaps a new point onto it, so a vertex
    /// that is not drawn can still be connected to.
    pub fn snap_to_node(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<i64> {
        self.snap_to_node_where(viewport, screen_pt, tol_px, |_| true)
    }

    /// Like `snap_to_node`, considering only nodes `accept` allows (the
    /// dragged node itself, or a node of an incompatible way, are left out).
    pub fn snap_to_node_where(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        accept: impl Fn(i64) -> bool,
    ) -> Option<i64> {
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 2.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);
        let mut best: Option<(f32, i64)> = None;
        for item in self.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            if !accept(id) {
                continue;
            }
            let Some(&idx) = self.node_cache.index_by_id.get(&id) else {
                continue;
            };
            let (_, mx, my) = self.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= tol_px && best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, id));
            }
        }
        best.map(|(_, id)| id)
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
        self.hit_test_segment_preferring(viewport, screen_pt, tol_px, None)
    }

    /// Like `hit_test_segment`, but a segment of `prefer_way` within
    /// tolerance wins over a nearer segment of any other way (so overlapping
    /// ways resolve to the selected one).
    pub fn hit_test_segment_preferring(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        prefer_way: Option<i64>,
    ) -> Option<(i64, i64, i64, usize)> {
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        // (preferred, distance, way, a, b, index): preferred ways sort first.
        let mut best: Option<(bool, f32, i64, i64, i64, usize)> = None;
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
                continue;
            };
            let preferred = prefer_way == Some(way_id);
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
                if d <= tol_px
                    && best.as_ref().is_none_or(|&(bp, bd, ..)| {
                        (preferred && !bp) || (preferred == bp && d < bd)
                    })
                {
                    best = Some((preferred, d, way_id, id_a, id_b, i));
                }
            }
        }
        best.map(|(_, _, way_id, a, b, idx)| (way_id, a, b, idx))
    }

    /// Like `hit_test_segment`, but also returns the nearest point on the
    /// winning segment as `(lat, lon)` — the coordinates a snapped node
    /// should adopt. Shares the same rtree query and per-segment loop; see
    /// `hit_test_segment`'s doc comment for the shared parameters.
    pub fn snap_to_way(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<(i64, i64, i64, usize, f64, f64)> {
        self.snap_to_way_where(viewport, screen_pt, tol_px, |_| true)
    }

    /// Like `snap_to_way`, considering only ways `accept` allows (ways of an
    /// incompatible kind, or one that already holds the node, are left out).
    pub fn snap_to_way_where(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        accept: impl Fn(i64) -> bool,
    ) -> Option<(i64, i64, i64, usize, f64, f64)> {
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize, Point<Pixels>)> = None;
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            if !accept(way_id) {
                continue;
            }
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
                    let nearest = nearest_point_on_segment(screen_pt, sp_a, sp_b);
                    best = Some((d, way_id, id_a, id_b, i, nearest));
                }
            }
        }
        best.map(|(_, way_id, a, b, idx, nearest)| {
            let (lat, lon) = viewport.screen_to_geo(nearest);
            (way_id, a, b, idx, lat, lon)
        })
    }
}

/// What the previous frame's way pass ended up with: the vertex count of each
/// batch, keyed by kind and colour/width. The next frame reserves that much
/// up front, so the (up to millions of vertices, tens of MB) paths are not
/// grown by doubling and copying while they are filled.
#[derive(Default)]
struct VertexHints(std::sync::Mutex<HashMap<u64, usize>>);

impl VertexHints {
    fn snapshot(&self) -> HashMap<u64, usize> {
        self.0.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn replace(&self, hints: HashMap<u64, usize>) {
        if let Ok(mut m) = self.0.lock() {
            *m = hints;
        }
    }
}

/// Which kind of batch a hint key belongs to.
#[derive(Clone, Copy)]
enum BatchKind {
    Fill = 1,
    Overlay = 2,
    Casing = 3,
    Stroke = 4,
}

/// A batch of raw triangles painted in one colour, with the bounds of its
/// vertices accumulated as it grows.
struct Batch {
    rgba: u32,
    /// Draw-order layer (`z-index`, see [`z_key`]); batches of one kind are
    /// painted in ascending `z`, then first-seen order.
    z: i32,
    /// Key of this batch's `VertexHints` entry.
    key: u64,
    path: Option<Path<Pixels>>,
    /// (min_x, max_x, min_y, max_y), accumulated by `PathSink` and
    /// `push_triangle`.
    bounds: (f32, f32, f32, f32),
}

impl Batch {
    fn new(kind: BatchKind, rgba: u32, z: i32, hints: &HashMap<u64, usize>) -> Self {
        let key = ((kind as u64) << 60) ^ ((rgba as u64) << 24) ^ (z as u32 as u64);
        // The previous frame's size plus a margin, capped so that a stale
        // hint (after zooming in from far out) cannot reserve gigabytes.
        let path = hints.get(&key).map(|&n| {
            let mut p = Path::new(point(px(0.0), px(0.0)));
            p.vertices.reserve((n + n / 8).min(MAX_HINT_VERTICES));
            p
        });
        Self {
            rgba,
            z,
            key,
            path,
            bounds: (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ),
        }
    }

    /// The finished path with its bounds set, or `None` if nothing was
    /// pushed. Records the final size in `hints`.
    fn finish(self, hints: &mut HashMap<u64, usize>) -> Option<(u32, Path<Pixels>)> {
        let mut path = self.path?;
        if path.vertices.is_empty() {
            return None;
        }
        hints.insert(self.key, path.vertices.len());
        let (min_x, max_x, min_y, max_y) = self.bounds;
        path.bounds = Bounds {
            origin: point(px(min_x), px(min_y)),
            size: size(px(max_x - min_x), px(max_y - min_y)),
        };
        Some((self.rgba, path))
    }
}

/// Upper bound of a reserved batch, in vertices (32 bytes each).
const MAX_HINT_VERTICES: usize = 8 << 20;

/// The index of the batch for `rgba` in `batches`, creating it on first
/// use. First-encounter order, so paint order is deterministic across frames
/// (a `HashMap`'s iteration order is randomized per process, which caused
/// visible z-order flicker between overlapping fills of different colors).
fn batch_index(
    index: &mut HashMap<(i32, u32), usize>,
    batches: &mut Vec<Batch>,
    kind: BatchKind,
    z: i32,
    rgba: u32,
    hints: &HashMap<u64, usize>,
) -> usize {
    *index.entry((z, rgba)).or_insert_with(|| {
        batches.push(Batch::new(kind, rgba, z, hints));
        batches.len() - 1
    })
}

/// A `z-index` as a sortable layer number (eighths, so fractional indices
/// like 0.5 order correctly).
fn z_key(z_index: f32) -> i32 {
    (z_index.clamp(-1.0e6, 1.0e6) * 8.0).round() as i32
}

/// Finish `batches` into paths in draw order: ascending `z`, ties by
/// first-seen order (a stable sort), so the order is the same every frame.
fn finish_sorted(
    mut batches: Vec<Batch>,
    hints: &mut HashMap<u64, usize>,
) -> Vec<(u32, Path<Pixels>)> {
    batches.sort_by_key(|b| b.z);
    batches
        .into_iter()
        .filter_map(|b| b.finish(hints))
        .collect()
}

/// The way pass of one frame, recorded but not yet painted: area fills, route
/// overlays, the casing and the strokes, each batched into as few paths as
/// there are colours or widths. Building it needs no `Window`, so it can be
/// timed and tested on its own (`examples/way_bench.rs`).
pub struct WayScene {
    fills: Vec<(u32, Path<Pixels>)>,
    overlays: Vec<(u32, Path<Pixels>)>,
    casing: Option<(u32, Path<Pixels>)>,
    /// The casings from `casing-width` and the strokes, in one list in
    /// `z-index` order: at each layer the casings come first, so a higher
    /// layer's casing covers a lower layer's stroke.
    lines: Vec<(u32, Path<Pixels>)>,
}

impl WayScene {
    /// Total triangle-list vertices in the scene (6 per stroke segment).
    pub fn vertex_count(&self) -> usize {
        let sum =
            |v: &[(u32, Path<Pixels>)]| -> usize { v.iter().map(|(_, p)| p.vertices.len()).sum() };
        let casing = self.casing.as_ref().map_or(0, |(_, p)| p.vertices.len());
        sum(&self.fills) + sum(&self.overlays) + casing + sum(&self.lines)
    }

    /// Number of paths the scene hands to the GPU. Each is rasterised and
    /// composited separately, over its own bounding box.
    pub fn path_count(&self) -> usize {
        self.fills.len() + self.overlays.len() + self.casing.is_some() as usize + self.lines.len()
    }

    /// Hand the paths to the window: fills first so outlines stay on top,
    /// then route overlays, the casing and the strokes.
    fn paint(self, window: &mut Window) {
        let fs_phase = crate::frame_stats::start();
        for (rgba, path) in self.fills {
            window.paint_path(path, gpui::rgba(rgba));
        }
        crate::frame_stats::lap("osm.fill_paint", fs_phase);
        let fs_phase = crate::frame_stats::start();
        for (rgba, path) in self.overlays {
            window.paint_path(path, gpui::rgba(rgba));
        }
        if let Some((rgba, path)) = self.casing {
            window.paint_path(path, gpui::rgba(rgba));
        }
        for (rgba, path) in self.lines {
            window.paint_path(path, gpui::rgba(rgba));
        }
        crate::frame_stats::lap("osm.ways_paint", fs_phase);
    }
}

/// How much of a way is drawn at the current zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WayDetail {
    /// Not drawn: under a pixel across.
    Skip,
    /// Drawn as one short stroke across its bounding box.
    Collapse,
    /// Every (merged) segment.
    Full,
}

/// The level-of-detail rules of the way pass at one zoom. Pure, so the
/// zoom rule is unit-tested. From `COARSE_BELOW_ZOOM` up (the editing
/// zooms) nothing is simplified beyond the one-pixel segment merge.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WayLod {
    coarse: bool,
    /// Whether minor ways (see `MINOR_WAYS_BELOW_ZOOM`) are left out.
    drops_minor: bool,
    /// Vertices that deviate less than this many pixels from the line
    /// through their neighbours are dropped (Douglas-Peucker tolerance).
    tolerance_px: f32,
    min_segment_px: f32,
}

impl WayLod {
    fn at(zoom: f64) -> Self {
        if zoom >= COARSE_BELOW_ZOOM {
            Self {
                coarse: false,
                drops_minor: false,
                tolerance_px: TOLERANCE_PX,
                min_segment_px: MIN_SEGMENT_PX,
            }
        } else {
            Self {
                coarse: true,
                drops_minor: zoom < MINOR_WAYS_BELOW_ZOOM,
                tolerance_px: COARSE_TOLERANCE_PX,
                min_segment_px: COARSE_MIN_SEGMENT_PX,
            }
        }
    }

    /// Whether a way stroked `width` pixels wide is outlined at all.
    fn strokes(&self, width: f32) -> bool {
        !(self.drops_minor && width <= MINOR_WAY_WIDTH)
    }

    /// What to draw of a way whose bounding box is `extent_px` across at its
    /// longest side.
    fn detail(&self, extent_px: f32) -> WayDetail {
        if !self.coarse || extent_px >= COLLAPSE_BELOW_PX {
            WayDetail::Full
        } else if extent_px < SKIP_BELOW_PX {
            WayDetail::Skip
        } else {
            WayDetail::Collapse
        }
    }

    /// Whether an area whose bounding box is `extent_px` across gets a fill:
    /// a fill under two pixels across is hidden by its own outline.
    fn fills(&self, extent_px: f32) -> bool {
        !self.coarse || extent_px >= TINY_FILL_BELOW_PX
    }
}

/// Longest side of a mercator bounding box in screen pixels.
#[inline]
fn bbox_extent_px(b: &WayBbox, ppm_x: f64, ppm_y: f64) -> f32 {
    ((b.max_x - b.min_x) * ppm_x).max((b.max_y - b.min_y) * ppm_y) as f32
}

impl OsmLayer {
    /// Ways in view whose MapCSS text style asks for a label along the line
    /// (`text: name; text-position: line`), with the style resolved at
    /// `zoom`. Run when placement is recomputed, not every frame.
    fn line_label_candidates(
        &self,
        viewport: &Viewport,
        zoom: f64,
    ) -> Vec<crate::layers::line_labels::Candidate<'_>> {
        use crate::layers::line_labels::{clear_offset, Candidate, LabelStyle};
        let mut cands = Vec::new();
        let Some(data) = self.osm_data.as_ref() else {
            return cands;
        };
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let defaults = LabelStyle::default();
        for (i, bbox) in self.way_bboxes.iter().enumerate() {
            let Some(b) = bbox else { continue };
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let way_id = self.way_ids[i];
            let Some(way) = data.ways.get(&way_id) else {
                continue;
            };
            let st = self
                .stylesheet
                .way_text_style(&way.tags, way.is_closed(), zoom);
            if st.position != Some(TextPosition::Line) {
                continue;
            }
            let Some(text) = st
                .text
                .as_deref()
                .and_then(|k| labels::label_text(k, &way.tags))
            else {
                continue;
            };
            let size_px = st.font_size.unwrap_or(defaults.size_px);
            // The road's half-width on screen (stroke plus casing), so the
            // text clears it even when the stylesheet's offset is small.
            let ws = &self.way_styles[i];
            let casing = if zoom >= CASING_MIN_ZOOM {
                ws.casing_width.max(CASING_WIDTH)
            } else {
                ws.casing_width
            };
            let half_width = ws.width / 2.0 + casing;
            let offset = clear_offset(
                st.offset.unwrap_or(0.35 * size_px + 2.0),
                size_px,
                half_width,
            );
            cands.push(Candidate {
                way_id,
                text,
                verts: &self.way_vertices[i],
                style: LabelStyle {
                    size_px,
                    halo_px: st.halo_radius.unwrap_or(defaults.halo_px),
                    offset_px: offset,
                    color: st.color.unwrap_or(defaults.color),
                    halo_color: st.halo_color.unwrap_or(defaults.halo_color),
                    halo_alpha: st.halo_opacity.unwrap_or(defaults.halo_alpha),
                },
                priority: highway_label_rank(&way.tags),
            });
        }
        cands
    }

    /// Choose the street-name labels for the current camera, yielding to
    /// the point labels and icons in `points`. Stores the result for
    /// `paint_line_labels`.
    fn place_line_labels(
        &self,
        viewport: &Viewport,
        cam: labels::CameraSig,
        points: &labels::Placement,
        now: std::time::Instant,
    ) {
        let fs = crate::frame_stats::start();
        let mut obstacles: Vec<labels::Rect> = Vec::new();
        for l in &points.labels {
            let sp = viewport.mercator_to_screen(l.mx, l.my);
            let (cx, cy) = (f32::from(sp.x), f32::from(sp.y));
            if let Some(icon) = &l.icon {
                let r = icon.choice.backing() / 2.0 + 1.0;
                obstacles.push(labels::Rect::around(cx, cy, r, r));
            }
            if let Some(t) = &l.text {
                let (w, h) = (
                    f32::from(t.line.width),
                    f32::from(t.line.ascent + t.line.descent),
                );
                obstacles.push(labels::Rect::new(
                    cx + t.dx,
                    cy + t.dy,
                    cx + t.dx + w,
                    cy + t.dy + h,
                ));
            }
        }
        let cands = self.line_label_candidates(viewport, cam.zoom);
        let mut placed_slot = self.line_placed.lock().unwrap_or_else(|e| e.into_inner());
        let prev: HashSet<(i64, usize)> = placed_slot
            .placed
            .iter()
            .map(|p| (p.way_id, p.slot))
            .collect();
        let to_screen = |mx: f64, my: f64| {
            let sp = viewport.mercator_to_screen(mx, my);
            (f32::from(sp.x), f32::from(sp.y))
        };
        let (placed, _built) = match self.line_labels.lock() {
            Ok(mut cache) => cache.place(
                &cands,
                cam.zoom,
                to_screen,
                (cam.w, cam.h),
                &obstacles,
                &prev,
            ),
            Err(_) => (Vec::new(), 0),
        };
        crate::frame_stats::count("linelabels.candidates", cands.len());
        crate::frame_stats::count("linelabels.placed", placed.len());
        placed_slot.store(placed, cam.zoom, now, labels::ZOOM_KEEP, labels::FADE);
        crate::frame_stats::lap("linelabels.place", fs);
    }

    /// Paint the placed street-name labels: halos first, then glyph fills.
    /// Per frame this only re-projects each label's reference point and
    /// transforms its cached triangles.
    fn paint_line_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        now: std::time::Instant,
        window: &mut Window,
    ) {
        use crate::layers::line_labels::{emit_frame, FrameOut};
        if self.drag_preview.is_some() {
            return;
        }
        let fs_phase = crate::frame_stats::start();
        let placed = self.line_placed.lock().unwrap_or_else(|e| e.into_inner());
        if placed.placed.is_empty() {
            return;
        }
        let mut out = FrameOut::default();
        let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        emit_frame(
            &placed.placed,
            |mx, my| {
                let sp = viewport.mercator_to_screen(mx, my);
                (f32::from(sp.x), f32::from(sp.y))
            },
            origin,
            view,
            now,
            &mut out,
        );
        drop(placed);
        crate::frame_stats::lap("linelabels.build", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let (nf, nh) = out.vertex_counts();
        crate::frame_stats::count("linelabels.drawn", out.labels);
        crate::frame_stats::count("linelabels.vertices", nf + nh);
        // `repeat` > 1 is a GPU-load multiplier for benchmarking.
        let repeat = crate::layers::line_labels::bench_knobs().repeat;
        for (verts, color) in out.halo.iter().chain(out.fill.iter()).map(|(c, v)| (v, *c)) {
            if verts.is_empty() {
                continue;
            }
            let st = point(0., 1.);
            let mut path = Path::new(point(px(verts[0].0), px(verts[0].1)));
            let (mut min_x, mut max_x, mut min_y, mut max_y) = (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            );
            path.vertices.reserve(verts.len());
            for &(x, y) in verts.iter() {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                path.vertices.push(PathVertex {
                    xy_position: point(px(x), px(y)),
                    st_position: st,
                    content_mask: Default::default(),
                });
            }
            path.bounds = Bounds {
                origin: point(px(min_x), px(min_y)),
                size: size(px(max_x - min_x), px(max_y - min_y)),
            };
            for _ in 1..repeat {
                window.paint_path(path.clone(), rgba(color));
            }
            window.paint_path(path, rgba(color));
        }
        crate::frame_stats::lap("linelabels.paint", fs_phase);
    }

    /// Street-name label triangles for the current view with no point labels
    /// in the way: candidates, placement and emission in one call. For
    /// `examples/perf_bench.rs`, which has no window.
    pub fn line_label_frame(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
    ) -> crate::layers::line_labels::FrameOut {
        use crate::layers::line_labels::FrameOut;
        let mut out = FrameOut::default();
        let zoom = viewport.zoom_level();
        let cands = self.line_label_candidates(viewport, zoom);
        let proj = viewport.projector();
        let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if let Ok(mut cache) = self.line_labels.lock() {
            cache.frame(
                &cands,
                zoom,
                |mx, my| proj.to_screen(mx, my),
                origin,
                view,
                &mut out,
            );
        }
        out
    }
}

/// Order among street names: bigger roads first.
fn highway_label_rank(tags: &HashMap<String, String>) -> i32 {
    match tags.get("highway").map(String::as_str) {
        Some("motorway" | "trunk") => 90,
        Some("primary") => 80,
        Some("secondary") => 70,
        Some("tertiary") => 60,
        Some("unclassified" | "residential") => 50,
        Some("service" | "living_street" | "pedestrian") => 40,
        Some(_) => 30,
        None => 20,
    }
}

impl OsmLayer {
    /// Record the way pass (fills, route overlays, casing, strokes) for this
    /// frame: cull in Mercator space, project, merge sub-pixel segments and
    /// batch everything into one path per colour/width. `bounds` is the
    /// canvas rectangle.
    ///
    /// Nothing is cached across frames except what the edit paths already
    /// maintain (`way_vertices`, `way_bboxes`, `way_styles`, `way_fill_tris`,
    /// the `MpCache`), so no edit can leave this stale.
    pub fn build_way_scene(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> WayScene {
        let origin_x = bounds.origin.x;
        let origin_y = bounds.origin.y;
        let (ox, oy) = (f32::from(origin_x), f32::from(origin_y));
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let proj = viewport.projector();
        let (ppm_x, ppm_y) = proj.pixels_per_meter();
        let zoom = viewport.zoom_level();
        let lod = WayLod::at(zoom);
        // Zoom-range rules: cached styles are zoom-agnostic and indexed by
        // signature into this per-zoom-bucket table (rebuilt only when the
        // bucket changes). `None` for a stylesheet with no zoom selectors.
        let zoom_table = self
            .stylesheet
            .is_zoom_dependent()
            .then(|| self.stylesheet.zoom_styles(zoom));
        let at_zoom = |s: WayStyle| -> WayStyle {
            match zoom_table.as_ref().and_then(|t| t.get(s.sig as usize)) {
                Some(z) => WayStyle {
                    direction: s.direction,
                    ..*z
                },
                None => s,
            }
        };
        // While nodes are being dragged the cached bounding boxes are stale,
        // so draw every way in full.
        let dragging = self.drag_preview.is_some();
        let hints = self.vertex_hints.snapshot();
        // The simplification tolerance in Web Mercator meters (nothing is
        // dropped while dragging, for the same reason).
        let tolerance_m = if dragging {
            0.0
        } else {
            (lod.tolerance_px as f64 / ppm_x) as f32
        };

        // Area fills: painted before strokes so outlines and nodes stay on
        // top. Triangulation is cached (`way_fill_tris`, computed at
        // load/edit time); per-frame work is projection + batched
        // raw-triangle emission, grouped by RGBA color.
        let mut fill_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut fills: Vec<Batch> = Vec::new();
        let mut fill_pts: Vec<Point<Pixels>> = Vec::new();

        // Multipolygon relations first, so a closed way's own fill (an
        // island, a building inside a forest) paints over them.
        let screen_rect = (
            ox,
            oy,
            f32::from(origin_x + bounds.size.width),
            f32::from(origin_y + bounds.size.height),
        );
        let fs_phase = crate::frame_stats::start();
        for entry in self.mp.entries.values() {
            let fill_style = at_zoom(entry.style);
            let Some(fill) = fill_style.fill else {
                continue;
            };
            let b = &entry.bbox;
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let alpha = (fill.opacity * 255.0).round() as u32;
            let group_idx = batch_index(
                &mut fill_index,
                &mut fills,
                BatchKind::Fill,
                z_key(fill_style.z_index),
                (fill.color << 8) | alpha,
                &hints,
            );
            let group = &mut fills[group_idx];
            for polygon in &entry.polygons {
                let pb = &polygon.bbox;
                if pb.max_x < vmin_x || pb.min_x > vmax_x || pb.max_y < vmin_y || pb.min_y > vmax_y
                {
                    continue;
                }
                if !dragging && !lod.fills(bbox_extent_px(pb, ppm_x, ppm_y)) {
                    continue;
                }
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (origin_x, origin_y),
                    screen_rect,
                    &mut fill_pts,
                    &mut group.path,
                    &mut group.bounds,
                );
            }
        }
        crate::frame_stats::lap("osm.mp_fill_build", fs_phase);

        let fs_phase = crate::frame_stats::start();
        for (i, tris) in self.way_fill_tris.iter().enumerate() {
            let Some(tris) = tris else { continue };
            let fill_style = at_zoom(self.way_styles[i]);
            let Some(fill) = fill_style.fill else {
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
            if !dragging && !lod.fills(bbox_extent_px(bbox, ppm_x, ppm_y)) {
                continue;
            }

            let verts = &self.way_vertices[i];
            let ring_len = verts.len() - 1; // closing duplicate excluded
            fill_pts.clear();
            for &(node_id, mx, my) in &verts[..ring_len] {
                let (x, y) = proj.to_screen(mx, my);
                if !(x.is_finite() && y.is_finite()) {
                    break;
                }
                let sp = point(px(x), px(y)) + self.drag_preview_offset(node_id);
                fill_pts.push(point(sp.x + origin_x, sp.y + origin_y));
            }
            // Any invalid projection breaks index alignment with the cached
            // triangle indices — skip the fill (the stroke still draws).
            if fill_pts.len() != ring_len {
                continue;
            }

            let alpha = (fill.opacity * 255.0).round() as u32;
            let group_idx = batch_index(
                &mut fill_index,
                &mut fills,
                BatchKind::Fill,
                z_key(fill_style.z_index),
                (fill.color << 8) | alpha,
                &hints,
            );
            let group = &mut fills[group_idx];
            for t in tris.as_chunks::<3>().0 {
                push_triangle(
                    &mut group.path,
                    &mut group.bounds,
                    fill_pts[t[0] as usize],
                    fill_pts[t[1] as usize],
                    fill_pts[t[2] as usize],
                );
            }
        }
        crate::frame_stats::lap("osm.way_fill_build", fs_phase);

        // Ways: bbox-cull in Mercator space, then group visible ways by
        // `(color, width)` style, one `paint_path` call per group. Vertex lookup is a single contiguous slice per way
        // (`way_vertices[i]`) — no HashMap indirection.
        //
        // Each segment is emitted as two raw triangles (an oriented quad)
        // pushed directly into the group's `Path`, bypassing
        // `PathBuilder::stroke`'s Lyon tessellation pass entirely — that
        // tessellator dominated frame time (~1.3-1.8ms for ~3800 visible
        // ways, measured under a synthetic 8k-node/4k-way dataset), since it
        // reruns from scratch every repaint (e.g. every mouse-move while
        // panning). Joins and end caps are built by hand in `way_stroke`
        // (mitered where possible, which costs no extra vertices) for
        // strokes wide enough to show them, at the editing zooms only.
        //
        // One path per stroke *colour*, whatever the width: every extra path
        // is rasterised and composited over its whole bounding box by the
        // GPU (a dozen near-screen-sized paths cost more than a million
        // vertices), and a width is only the half-width of each segment's
        // quad.
        let mut stroke_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut strokes: Vec<Batch> = Vec::new();
        // Consecutive ways mostly share a style, so remember the last lookup.
        let mut last_stroke: Option<((i32, u32), usize)> = None;

        // Route overlays: a translucent line under each route member way,
        // one path per colour. Looked up per visible way from the cache
        // (`MpCache::route_ways`), never by scanning relations.
        let has_routes = !self.mp.route_ways.is_empty();
        let mut overlay_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut overlays: Vec<Batch> = Vec::new();
        let overlay_alpha = (crate::relation_overlay::ROUTE_ALPHA * 255.0).round() as u32;

        // The casing: one dark path under every way's stroke (and direction
        // arrows), slightly wider than the stroke, so thin light lines stay
        // readable over light imagery. Built in the same pass as the strokes
        // from the same projected points, and painted before any of them.
        let draw_casing = zoom >= CASING_MIN_ZOOM;
        let draw_arrows = zoom >= ARROW_MIN_ZOOM;
        let mut casing = Batch::new(BatchKind::Casing, (CASING_COLOR << 8) | 0xFF, 0, &hints);
        // Casings asked for by the stylesheet (`casing-width`), one per colour.
        let mut styled_casing_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut styled_casings: Vec<Batch> = Vec::new();

        // Reused scratch buffers, so the (potentially tens of thousands of)
        // visible ways of a frame don't each allocate.
        let mut scratch_pts: Vec<Point<Pixels>> = Vec::new();
        let mut segs: Vec<(usize, usize)> = Vec::new();
        let mut polyline = way_stroke::Polyline::default();

        let mut ways_in_view = 0usize;
        let mut ways_skipped = 0usize;
        let mut ways_collapsed = 0usize;

        let fs_phase = crate::frame_stats::start();
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
            ways_in_view += 1;

            let detail = if dragging {
                WayDetail::Full
            } else {
                lod.detail(bbox_extent_px(bbox, ppm_x, ppm_y))
            };
            if detail == WayDetail::Skip {
                ways_skipped += 1;
                continue;
            }

            // Style is resolved once per way at data-load time
            // (`compute_way_tables`/`commit_node_moves`/`set_stylesheet`),
            // not here — no stylesheet lookup or tags HashMap access in this
            // per-frame loop.
            let style = at_zoom(self.way_styles[i]);
            if !dragging && !lod.strokes(style.width) {
                ways_skipped += 1;
                continue;
            }
            let half_width = style.width / 2.0;
            let z = z_key(style.z_index);
            let stroke_rgba = (style.color << 8) | (style.opacity * 255.0).round() as u32;
            let stroke_idx = match last_stroke {
                Some(((lz, c), idx)) if lz == z && c == stroke_rgba => idx,
                _ => {
                    let idx = batch_index(
                        &mut stroke_index,
                        &mut strokes,
                        BatchKind::Stroke,
                        z,
                        stroke_rgba,
                        &hints,
                    );
                    last_stroke = Some(((z, stroke_rgba), idx));
                    idx
                }
            };
            // Dashes only where a segment is long enough to show them; a
            // collapsed way is a single stroke across its bounding box.
            let dashes = style
                .dashes
                .filter(|_| detail == WayDetail::Full && !lod.coarse);
            let stroke = &mut strokes[stroke_idx];

            // An explicit casing replaces the default dark one.
            let styled_casing_idx = (style.casing_width > 0.0).then(|| {
                batch_index(
                    &mut styled_casing_index,
                    &mut styled_casings,
                    BatchKind::Casing,
                    z,
                    (style.casing_color << 8) | 0xFF,
                    &hints,
                )
            });
            let default_casing = draw_casing && styled_casing_idx.is_none();

            let overlay_idx = if has_routes {
                self.mp.route_colour_of_way(self.way_ids[i]).map(|colour| {
                    batch_index(
                        &mut overlay_index,
                        &mut overlays,
                        BatchKind::Overlay,
                        0,
                        (colour << 8) | overlay_alpha,
                        &hints,
                    )
                })
            } else {
                None
            };

            scratch_pts.clear();
            let min_segment_px = if detail == WayDetail::Collapse {
                // Two corners of the bounding box: one stroke, whatever its
                // length.
                ways_collapsed += 1;
                for (mx, my) in [(bbox.min_x, bbox.min_y), (bbox.max_x, bbox.max_y)] {
                    let (x, y) = proj.to_screen(mx, my);
                    scratch_pts.push(point(px(x + ox), px(y + oy)));
                }
                0.0
            } else {
                // Only the vertices significant at this zoom (Douglas-
                // Peucker, see `WayGeom`) are projected at all.
                for (&(node_id, mx, my), &sig) in verts.verts.iter().zip(&verts.sig) {
                    if sig < tolerance_m {
                        continue;
                    }
                    let (x, y) = proj.to_screen(mx, my);
                    if !(x.is_finite() && y.is_finite()) {
                        continue;
                    }
                    let sp = point(px(x), px(y)) + self.drag_preview_offset(node_id);
                    scratch_pts.push(point(sp.x + origin_x, sp.y + origin_y));
                }
                lod.min_segment_px
            };
            if scratch_pts.len() < 2 {
                continue;
            }

            // Decimate: skip segments under the minimum screen length while
            // zoomed out, but always emit a segment ending at the way's
            // final projected vertex so junctions/endpoints are never
            // rounded away. This is purely a rendering optimization —
            // `way_vertices` (read by hit-testing and selection highlight)
            // is untouched.
            decimate_into(&scratch_pts, min_segment_px, &mut segs);

            // The merged polyline (the anchors of `segs`, in order), stroked
            // as one: with joins at its bends and caps at its ends when the
            // stroke is wide enough to show them (see `way_stroke`), never
            // in the coarse zoom path or for a collapsed way.
            // Ways a few pixels across (most buildings at zoom 16) are too
            // small for a bend to show, and are the bulk of the cost.
            let shaped_lod = !lod.coarse
                && detail == WayDetail::Full
                && (dragging
                    || bbox_extent_px(bbox, ppm_x, ppm_y) >= way_stroke::SHAPE_MIN_EXTENT_PX);
            let casing_extent = if styled_casing_idx.is_some() {
                style.casing_width
            } else {
                CASING_WIDTH
            };
            let (casing_hw, overlay_hw) = (
                half_width + casing_extent,
                half_width + casing_extent + ROUTE_OVERLAY_EXTRA,
            );
            let widest_hw = if overlay_idx.is_some() {
                overlay_hw
            } else if default_casing || styled_casing_idx.is_some() {
                casing_hw
            } else {
                half_width
            };
            let with_joins = shaped_lod && widest_hw * 2.0 >= way_stroke::JOIN_MIN_WIDTH_PX;
            let caps = with_joins && style.width >= way_stroke::CAP_MIN_WIDTH_PX;
            let shape_of = |hw: f32| way_stroke::Shape {
                joins: with_joins && hw * 2.0 >= way_stroke::JOIN_MIN_WIDTH_PX,
                caps,
            };
            if with_joins {
                let anchors = std::iter::once(scratch_pts[segs[0].0])
                    .chain(segs.iter().map(|&(_, b)| scratch_pts[b]))
                    .map(|p| (f32::from(p.x), f32::from(p.y)));
                polyline.set(anchors);
                let overlay_layer = overlay_idx.map(|oi| {
                    let og = &mut overlays[oi];
                    Layer {
                        sink: PathSink {
                            path: &mut og.path,
                            bounds: &mut og.bounds,
                        },
                        hw: overlay_hw,
                        shape: shape_of(overlay_hw),
                    }
                });
                let casing_layer = default_casing.then(|| Layer {
                    sink: PathSink {
                        path: &mut casing.path,
                        bounds: &mut casing.bounds,
                    },
                    hw: casing_hw,
                    shape: shape_of(casing_hw),
                });
                let styled_layer = styled_casing_idx.map(|ci| {
                    let cb = &mut styled_casings[ci];
                    Layer {
                        sink: PathSink {
                            path: &mut cb.path,
                            bounds: &mut cb.bounds,
                        },
                        hw: casing_hw,
                        shape: shape_of(casing_hw),
                    }
                });
                let stroke_layer = Layer {
                    sink: PathSink {
                        path: &mut stroke.path,
                        bounds: &mut stroke.bounds,
                    },
                    hw: half_width,
                    shape: shape_of(half_width),
                };
                polyline.emit_layers(&mut [
                    overlay_layer,
                    casing_layer,
                    styled_layer,
                    dashes.is_none().then_some(stroke_layer),
                ]);
            } else {
                // Plain rectangles, as before joins existed.
                for &(a, b) in &segs {
                    let pa = (f32::from(scratch_pts[a].x), f32::from(scratch_pts[a].y));
                    let pb = (f32::from(scratch_pts[b].x), f32::from(scratch_pts[b].y));
                    if let Some(oi) = overlay_idx {
                        let og = &mut overlays[oi];
                        let mut sink = PathSink {
                            path: &mut og.path,
                            bounds: &mut og.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, overlay_hw);
                    }
                    if default_casing {
                        let mut sink = PathSink {
                            path: &mut casing.path,
                            bounds: &mut casing.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, casing_hw);
                    }
                    if let Some(ci) = styled_casing_idx {
                        let cb = &mut styled_casings[ci];
                        let mut sink = PathSink {
                            path: &mut cb.path,
                            bounds: &mut cb.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, casing_hw);
                    }
                    if dashes.is_none() {
                        let mut sink = PathSink {
                            path: &mut stroke.path,
                            bounds: &mut stroke.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, half_width);
                    }
                }
            }
            if let (Some((dash, gap)), Some(first)) = (dashes, segs.first()) {
                let anchors: Vec<(f32, f32)> = std::iter::once(scratch_pts[first.0])
                    .chain(segs.iter().map(|&(_, b)| scratch_pts[b]))
                    .map(|p| (f32::from(p.x), f32::from(p.y)))
                    .collect();
                let mut sink = PathSink {
                    path: &mut stroke.path,
                    bounds: &mut stroke.bounds,
                };
                way_stroke::emit_dashed(&mut sink, &anchors, dash, gap, half_width);
            }

            let arrows = draw_arrows && style.direction != Direction::None;
            let reverse = style.direction == Direction::Reverse;
            // Start half a spacing in, so even a short way gets an arrow.
            let mut since_arrow = ARROW_SPACING_PX / 2.0;
            for &(a, b) in &segs {
                let (p0, p1) = (scratch_pts[a], scratch_pts[b]);
                if !arrows {
                    continue;
                }
                let dx = f32::from(p1.x - p0.x);
                let dy = f32::from(p1.y - p0.y);
                let len = (dx * dx + dy * dy).sqrt();
                since_arrow += len;
                if since_arrow < ARROW_SPACING_PX || len < ARROW_MIN_SEGMENT_PX {
                    continue;
                }
                since_arrow = 0.0;
                let half_base = half_width + ARROW_OVERHANG;
                if let Some([tip, left, right]) =
                    arrow_triangle(p0, p1, reverse, ARROW_HALF_LENGTH, half_base)
                {
                    push_triangle(&mut stroke.path, &mut stroke.bounds, tip, left, right);
                }
                if default_casing {
                    if let Some([tip, left, right]) = arrow_triangle(
                        p0,
                        p1,
                        reverse,
                        ARROW_HALF_LENGTH + 2.0 * CASING_WIDTH,
                        half_base + 1.5 * CASING_WIDTH,
                    ) {
                        push_triangle(&mut casing.path, &mut casing.bounds, tip, left, right);
                    }
                }
            }
        }
        crate::frame_stats::count("count.ways_in_view", ways_in_view);
        crate::frame_stats::count("count.ways_skipped", ways_skipped);
        crate::frame_stats::count("count.ways_collapsed", ways_collapsed);
        crate::frame_stats::lap("osm.ways_build", fs_phase);

        let mut next_hints = HashMap::new();
        let scene = WayScene {
            fills: finish_sorted(fills, &mut next_hints),
            overlays: overlays
                .into_iter()
                .filter_map(|b| b.finish(&mut next_hints))
                .collect(),
            casing: casing.finish(&mut next_hints),
            lines: {
                // Per z-index layer: casings, then strokes. Stable, so equal
                // layers keep first-seen order.
                let mut all: Vec<(i32, u8, Batch)> = styled_casings
                    .into_iter()
                    .map(|b| (b.z, 0, b))
                    .chain(strokes.into_iter().map(|b| (b.z, 1, b)))
                    .collect();
                all.sort_by_key(|&(z, order, _)| (z, order));
                all.into_iter()
                    .filter_map(|(_, _, b)| b.finish(&mut next_hints))
                    .collect()
            },
        };
        self.vertex_hints.replace(next_hints);
        crate::frame_stats::count("count.way_path_vertices", scene.vertex_count());
        crate::frame_stats::count("count.way_paths", scene.path_count());
        scene
    }
}

impl MapLayer for OsmLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn as_osm(&self) -> Option<&OsmLayer> {
        Some(self)
    }

    fn as_osm_mut(&mut self) -> Option<&mut OsmLayer> {
        Some(self)
    }

    fn as_editable(&self) -> Option<&dyn EditableLayer> {
        Some(self)
    }

    fn as_editable_mut(&mut self) -> Option<&mut dyn EditableLayer> {
        Some(self)
    }

    fn render_elements(
        &self,
        _viewport: &Viewport,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Vec<AnyElement> {
        // Node rendering moved to `render_canvas` (paint_quad) to avoid the
        // per-node GPUI element layout cost. The selection ring is drawn in
        // `render_highlight` as a canvas outline.
        Vec::new()
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.osm_data.is_none() {
            return;
        }

        let fs_total = crate::frame_stats::start();
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

        // Fills, route overlays, casing and strokes: recorded, then painted.
        self.build_way_scene(viewport, bounds).paint(window);
        let zoom = viewport.zoom_level();

        // Nodes: iterate the flat cache (contiguous Vec) for better locality,
        // reject offscreen ones with a mercator-space AABB test, and paint
        // visible ones as filled quads on the canvas so we skip GPUI's
        // per-element layout pass. Batching nodes into a single PathBuilder
        // fill path was tried and turned out much slower — Lyon's fill
        // tessellator is not tuned for thousands of tiny rectangles. (The
        // simple markers of tagged nodes below are batched as raw triangles,
        // with no tessellation, and are several times cheaper than quads.)
        //
        // Per-node style is resolved once at data-load time (see
        // `compute_node_cache`/`commit_node_moves`/`set_stylesheet`) and
        // just read here — no stylesheet call or tags HashMap lookup per
        // node per frame.
        let fs_phase = crate::frame_stats::start();
        let show_vertices = zoom >= VERTEX_MIN_ZOOM;
        let mut nodes_in_view = 0usize;
        let mut markers_drawn = 0usize;
        // The stylesheet's own markers, one path per colour.
        type MarkerBatch = (Option<Path<Pixels>>, (f32, f32, f32, f32));
        let mut styled: HashMap<u32, MarkerBatch> = HashMap::new();
        let mut simple: Option<Path<Pixels>> = None;
        let mut simple_bounds = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for (idx, &(id, mx, my)) in self.node_cache.flat.iter().enumerate() {
            if mx < vmin_x || mx > vmax_x || my < vmin_y || my > vmax_y {
                continue;
            }
            nodes_in_view += 1;
            // Selected and hovered nodes and selected ways' nodes are painted
            // in full by `render_highlight`, so only the zoom rule applies.
            let style = self.node_cache.styles[idx];
            // An untagged vertex of a way is never drawn here (only the
            // highlight pass draws it, when selected): the way lookup runs
            // only for untagged nodes at zooms where a standalone one shows.
            let form = self.styled_node_form(&style, id, zoom).unwrap_or_else(|| {
                let standalone = self.is_standalone(style.tagged, id, zoom);
                node_markers::node_form(style.tagged, standalone, false, zoom)
            });
            if form == NodeForm::Hidden {
                continue;
            }
            let mut sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            sp += self.drag_preview_offset(id);
            markers_drawn += 1;
            let at = point(sp.x + origin_x, sp.y + origin_y);
            match form {
                NodeForm::Simple(edge) => {
                    push_simple_node_marker(&mut simple, &mut simple_bounds, at, edge)
                }
                NodeForm::Styled(visual) => {
                    let (path, bounds) = styled.entry(visual.color).or_insert((
                        None,
                        (
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                        ),
                    ));
                    push_styled_node_marker(path, bounds, at, visual);
                }
                _ => paint_node_marker(window, at, style),
            }
        }
        // All simple markers go out as one raw-triangle path (like the
        // direction arrows): much cheaper than a quad each.
        if let Some(mut path) = simple {
            let (min_x, max_x, min_y, max_y) = simple_bounds;
            path.bounds = Bounds {
                origin: point(px(min_x), px(min_y)),
                size: size(px(max_x - min_x), px(max_y - min_y)),
            };
            window.paint_path(path, rgba(node_markers::SIMPLE_RGBA));
        }
        for (color, (path, (min_x, max_x, min_y, max_y))) in styled {
            if let Some(mut path) = path {
                path.bounds = Bounds {
                    origin: point(px(min_x), px(min_y)),
                    size: size(px(max_x - min_x), px(max_y - min_y)),
                };
                window.paint_path(path, rgb(color));
            }
        }
        // The preview way's vertices (only this one way's, so hovering costs
        // a handful of quads): the ones no other rule shows.
        if let Some(wi) = self
            .preview_way
            .filter(|_| zoom >= node_markers::PREVIEW_MIN_ZOOM)
            .and_then(|w| self.way_id_to_index.get(&w).copied())
        {
            let pins = self.pins();
            for &(id, mx, my) in self.way_vertices[wi].iter() {
                let Some(&idx) = self.node_cache.index_by_id.get(&id) else {
                    continue;
                };
                if self.node_form_at(&pins, idx, id, zoom) != NodeForm::Preview {
                    continue;
                }
                let mut sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) {
                    continue;
                }
                sp += self.drag_preview_offset(id);
                paint_preview_marker(window, point(sp.x + origin_x, sp.y + origin_y));
            }
        }
        crate::frame_stats::count("count.nodes_in_view", nodes_in_view);
        crate::frame_stats::count("count.markers_drawn", markers_drawn);

        crate::frame_stats::lap("osm.nodes", fs_phase);
        let fs_phase = crate::frame_stats::start();
        // Turn restrictions: a sign-like marker beside the via position.
        if show_vertices {
            for r in self.mp.restrictions.values() {
                if r.x < vmin_x || r.x > vmax_x || r.y < vmin_y || r.y > vmax_y {
                    continue;
                }
                let sp = viewport.mercator_to_screen(r.x, r.y);
                if !is_point_valid(sp) {
                    continue;
                }
                paint_restriction_marker(
                    window,
                    point(
                        sp.x + origin_x + px(RESTRICTION_OFFSET_PX.0),
                        sp.y + origin_y + px(RESTRICTION_OFFSET_PX.1),
                    ),
                    r.only,
                );
            }
        }
        crate::frame_stats::lap("osm.restrictions", fs_phase);
        crate::frame_stats::lap("osm.total", fs_total);
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

    fn update_vertex_preview(
        &mut self,
        viewport: &Viewport,
        pt: Point<Pixels>,
        hovered: Option<&FeatureRef>,
        enabled: bool,
    ) -> bool {
        let next = if !enabled
            || self.osm_data.is_none()
            || viewport.zoom_level() < node_markers::PREVIEW_MIN_ZOOM
        {
            None
        } else {
            match hovered {
                Some(h) if h.layer_id == self.id => match h.kind {
                    FeatureKind::Way => self.way_id_to_index.contains_key(&h.id).then_some(h.id),
                    FeatureKind::Node => self.preview_way.filter(|&w| self.node_in_way(h.id, w)),
                    FeatureKind::Relation => None,
                },
                Some(_) => None,
                None => self.preview_way.filter(|&w| {
                    self.way_screen_distance(viewport, pt, w)
                        .is_some_and(|d| d <= WAY_HIT_TOL + node_markers::PREVIEW_HIT_RADIUS)
                }),
            }
        };
        let changed = next != self.preview_way;
        self.preview_way = next;
        changed
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

    fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) -> Result<(), EditError> {
        if moves.is_empty() {
            return Ok(());
        }
        let fs_phase = crate::frame_stats::start();
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        if let Some(&(id, _, _)) = moves.iter().find(|m| !data.nodes.contains_key(&m.0)) {
            return Err(EditError::UnknownNode(id));
        }
        let mut moved_ids: Vec<i64> = Vec::with_capacity(moves.len());
        for &(id, lat, lon) in moves {
            if let Some(node) = data.nodes.get_mut(&id) {
                node.lat = lat;
                node.lon = lon;
                moved_ids.push(id);
            }
        }

        // Only ways through a moved node can have a stale vertex list or bbox.
        let mut touched_ways: HashSet<usize> = HashSet::new();
        for &id in &moved_ids {
            if let Some(way_idxs) = self.node_to_ways.get(&id) {
                touched_ways.extend(way_idxs.iter().copied());
            }
        }

        // Snapshots, so the data borrow ends before the caches are patched.
        let moved_nodes: Vec<OsmNode> = moved_ids
            .iter()
            .filter_map(|id| data.nodes.get(id).cloned())
            .collect();
        let touched: Vec<(usize, OsmWay)> = touched_ways
            .iter()
            .filter_map(|&i| Some((i, data.ways.get(&self.way_ids[i])?.clone())))
            .collect();
        self.mark_modified();
        for node in &moved_nodes {
            self.index_node_set(node);
        }
        for (way_idx, way) in &touched {
            self.reproject_way(*way_idx, way);
        }

        let touched_way_ids: Vec<i64> = touched_ways.iter().map(|&i| self.way_ids[i]).collect();
        self.mp_refresh_ways(&touched_way_ids);
        self.mp_refresh_nodes(&moved_ids);
        crate::frame_stats::lap("edit.commit_node_moves_total", fs_phase);
        Ok(())
    }

    fn set_tag(
        &mut self,
        kind: FeatureKind,
        id: i64,
        key: &str,
        value: &str,
    ) -> Result<(), EditError> {
        self.edit_tags(kind, id, |tags| {
            tags.insert(key.to_string(), value.to_string());
        })
    }

    fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) -> Result<(), EditError> {
        self.edit_tags(kind, id, |tags| {
            tags.remove(key);
        })
    }

    fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Result<i64, EditError> {
        let Some(id) = id else {
            return Ok(self.add_node(lat, lon));
        };
        if self
            .osm_data
            .as_ref()
            .is_some_and(|d| d.nodes.contains_key(&id))
        {
            return Err(EditError::IdInUse(FeatureKind::Node, id));
        }
        self.insert_node(id, lat, lon, HashMap::new(), 0);
        Ok(id)
    }

    fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        let id = self.next_new_id;
        self.next_new_id -= 1;
        self.insert_node(id, lat, lon, HashMap::new(), 0);
        id
    }

    fn delete_feature(
        &mut self,
        kind: FeatureKind,
        id: i64,
    ) -> Result<DeletedFeatureSnapshot, EditError> {
        match kind {
            FeatureKind::Node => {
                let node = self.take_node(id).ok_or_else(|| self.why_node_stays(id))?;
                Ok(DeletedFeatureSnapshot {
                    kind,
                    id,
                    tags: node.tags.into_iter().collect(),
                    way_nodes: Vec::new(),
                    node_lat_lon: Some((node.lat, node.lon)),
                    version: node.version,
                })
            }
            FeatureKind::Way => {
                let way = self.take_way(id).ok_or(EditError::UnknownWay(id))?;
                Ok(DeletedFeatureSnapshot {
                    kind,
                    id,
                    tags: way.tags.into_iter().collect(),
                    way_nodes: way.nodes,
                    node_lat_lon: None,
                    version: way.version,
                })
            }
            // A relation's snapshot is a `DeletedRelationSnapshot`.
            FeatureKind::Relation => Err(EditError::NotNodeOrWay(kind)),
        }
    }

    fn restore_feature(&mut self, snapshot: DeletedFeatureSnapshot) -> Result<(), EditError> {
        let id = snapshot.id;
        match snapshot.kind {
            FeatureKind::Node => {
                let (lat, lon) = snapshot
                    .node_lat_lon
                    .ok_or(EditError::MissingCoordinates(id))?;
                if self
                    .osm_data
                    .as_ref()
                    .is_some_and(|d| d.nodes.contains_key(&id))
                {
                    return Err(EditError::IdInUse(FeatureKind::Node, id));
                }
                self.insert_node(
                    id,
                    lat,
                    lon,
                    snapshot.tags.into_iter().collect(),
                    snapshot.version,
                );
                // A restriction whose via this was draws again.
                self.mp_refresh_nodes(&[id]);
                Ok(())
            }
            FeatureKind::Way => {
                if self.way_id_to_index.contains_key(&id) {
                    return Err(EditError::IdInUse(FeatureKind::Way, id));
                }
                self.insert_way(OsmWay {
                    id,
                    nodes: snapshot.way_nodes,
                    version: snapshot.version,
                    tags: snapshot.tags.into_iter().collect(),
                });
                Ok(())
            }
            kind @ FeatureKind::Relation => Err(EditError::NotNodeOrWay(kind)),
        }
    }

    fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        let id = self.next_placeholder_way_id;
        self.next_placeholder_way_id -= 1;
        self.insert_way(OsmWay {
            id,
            nodes: node_ids,
            version: 0,
            tags: tags.into_iter().collect(),
        });
        id
    }

    fn extend_way(&mut self, way_id: i64, node_id: i64) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            nodes.push(node_id);
            Ok(())
        })
    }

    fn insert_node_into_way(
        &mut self,
        way_id: i64,
        index: usize,
        lat: f64,
        lon: f64,
    ) -> Result<i64, EditError> {
        // Check before creating the node, so a refusal leaves no orphan.
        let len = self
            .way_node_ids(way_id)
            .ok_or(EditError::UnknownWay(way_id))?
            .len();
        if index > len {
            return Err(EditError::IndexOutOfRange { way_id, index, len });
        }
        let new_id = self.add_node(lat, lon);
        self.edit_way_nodes(way_id, |nodes| {
            nodes.insert(index, new_id);
            Ok(new_id)
        })
    }

    fn remove_node(&mut self, node_id: i64) -> Result<(), EditError> {
        self.take_node(node_id)
            .map(drop)
            .ok_or_else(|| self.why_node_stays(node_id))
    }

    fn remove_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.take_way(way_id)
            .map(drop)
            .ok_or(EditError::UnknownWay(way_id))
    }

    fn remove_node_from_way(&mut self, way_id: i64, index: usize) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            if index >= nodes.len() {
                return Err(EditError::IndexOutOfRange {
                    way_id,
                    index,
                    len: nodes.len(),
                });
            }
            nodes.remove(index);
            Ok(())
        })
    }

    fn reverse_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            nodes.reverse();
            Ok(())
        })
    }

    fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            *nodes = node_ids;
            Ok(())
        })
    }

    fn ways_using_node(&self, node_id: i64) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .node_to_ways
            .get(&node_id)
            .into_iter()
            .flatten()
            .filter_map(|&idx| self.way_ids.get(idx).copied())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn relations_containing_way(&self, way_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Way, way_id)
    }
    fn relations_containing_node(&self, node_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Node, node_id)
    }

    fn relation(&self, relation_id: i64) -> Option<crate::osm::OsmRelation> {
        self.osm_data
            .as_ref()?
            .relations
            .iter()
            .find(|r| r.id == relation_id)
            .cloned()
    }

    fn relation_ids(&self) -> Vec<i64> {
        self.osm_data
            .as_ref()
            .map(|d| d.relations.iter().map(|r| r.id).collect())
            .unwrap_or_default()
    }

    fn set_relation_members(
        &mut self,
        relation_id: i64,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<Vec<crate::osm::OsmMember>, EditError> {
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        let relation = data
            .relations
            .iter_mut()
            .find(|r| r.id == relation_id)
            .ok_or(EditError::UnknownRelation(relation_id))?;
        let previous = std::mem::replace(&mut relation.members, members);
        self.mark_modified();
        self.mp_refresh_relations(&[relation_id]);
        self.refresh_member_ways();
        Ok(previous)
    }

    fn relations(&self) -> Vec<crate::osm::OsmRelation> {
        self.osm_data
            .as_ref()
            .map(|d| d.relations.clone())
            .unwrap_or_default()
    }

    fn relations_containing_relation(&self, relation_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Relation, relation_id)
    }

    fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<i64, EditError> {
        let lowest_relation = self
            .osm_data
            .as_ref()
            .ok_or(EditError::NoData)?
            .relations
            .iter()
            .map(|r| r.id)
            .min()
            .unwrap_or(0);
        let id = self
            .next_new_id
            .min(self.next_placeholder_way_id)
            .min(lowest_relation.min(0) - 1);
        self.next_new_id = id - 1;
        self.next_placeholder_way_id = id - 1;
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        data.relations.push(crate::osm::OsmRelation {
            id,
            members,
            version: 0,
            tags: tags.into_iter().collect(),
        });
        self.mark_modified();
        self.mp_refresh_relations(&[id]);
        self.refresh_member_ways();
        Ok(id)
    }

    fn delete_relation(&mut self, relation_id: i64) -> Result<DeletedRelationSnapshot, EditError> {
        let data = self.osm_data.as_ref().ok_or(EditError::NoData)?;
        if data.relations.iter().any(|r| {
            r.id != relation_id
                && r.members.iter().any(|m| {
                    m.member_type == crate::osm::MemberType::Relation && m.reference == relation_id
                })
        }) {
            return Err(EditError::RelationInUse(relation_id));
        }
        let index = data
            .relations
            .iter()
            .position(|r| r.id == relation_id)
            .ok_or(EditError::UnknownRelation(relation_id))?;
        let data = Arc::make_mut(self.osm_data.as_mut().ok_or(EditError::NoData)?);
        let relation = data.relations.remove(index);
        self.mark_modified();
        self.mp_refresh_relations(&[relation_id]);
        self.refresh_member_ways();
        Ok(DeletedRelationSnapshot { relation, index })
    }

    fn restore_relation(&mut self, snapshot: DeletedRelationSnapshot) -> Result<(), EditError> {
        let arc = self.osm_data.as_mut().ok_or(EditError::NoData)?;
        if arc.relations.iter().any(|r| r.id == snapshot.relation.id) {
            return Err(EditError::IdInUse(
                FeatureKind::Relation,
                snapshot.relation.id,
            ));
        }
        let data = Arc::make_mut(arc);
        let id = snapshot.relation.id;
        let at = snapshot.index.min(data.relations.len());
        data.relations.insert(at, snapshot.relation);
        if id <= self.next_new_id {
            self.next_new_id = id - 1;
        }
        if id <= self.next_placeholder_way_id {
            self.next_placeholder_way_id = id - 1;
        }
        self.mark_modified();
        self.mp_refresh_relations(&[id]);
        self.refresh_member_ways();
        Ok(())
    }

    fn has_pending_edit(&self, kind: FeatureKind, id: i64) -> bool {
        let (Some(current), Some(original)) = (&self.osm_data, &self.original_data) else {
            return self.osm_data.is_some();
        };
        match kind {
            FeatureKind::Node => match (current.nodes.get(&id), original.nodes.get(&id)) {
                (Some(a), Some(b)) => a != b,
                (Some(_), None) => true,
                _ => false,
            },
            FeatureKind::Way => match (current.ways.get(&id), original.ways.get(&id)) {
                (Some(a), Some(b)) => a != b,
                (Some(_), None) => true,
                _ => false,
            },
            FeatureKind::Relation => {
                let find = |d: &OsmData| d.relations.iter().find(|r| r.id == id).cloned();
                match (find(current), find(original)) {
                    (Some(a), Some(b)) => a != b,
                    (Some(_), None) => true,
                    _ => false,
                }
            }
        }
    }

    fn purge_features(&mut self, nodes: &[i64], ways: &[i64], relations: &[i64]) -> PurgedFeatures {
        let mut purged = PurgedFeatures::default();
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return purged;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        for &id in nodes {
            base.nodes.remove(&id);
            if let Some(n) = data.nodes.remove(&id) {
                purged.nodes.push(n);
            }
        }
        for &id in ways {
            base.ways.remove(&id);
            if let Some(w) = data.ways.remove(&id) {
                purged.ways.push(w);
            }
        }
        // Indices are read before anything is removed, so they are positions in
        // the untouched lists and `restore_purged` can re-insert in ascending
        // order.
        let doomed: HashSet<i64> = relations.iter().copied().collect();
        for (i, r) in data.relations.iter().enumerate() {
            if doomed.contains(&r.id) {
                let base_index = base
                    .relations
                    .iter()
                    .position(|b| b.id == r.id)
                    .unwrap_or(i);
                purged.relations.push((r.clone(), i, base_index));
            }
        }
        data.relations.retain(|r| !doomed.contains(&r.id));
        base.relations.retain(|r| !doomed.contains(&r.id));
        if purged.is_empty() {
            return purged;
        }
        let data = Arc::new(data);
        self.rebuild_caches(&data);
        self.osm_data = Some(data);
        self.original_data = Some(Arc::new(base));
        self.revision += 1;
        purged
    }

    fn restore_purged(&mut self, purged: PurgedFeatures) {
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        for n in purged.nodes {
            base.nodes.insert(n.id, n.clone());
            data.nodes.insert(n.id, n);
        }
        for w in purged.ways {
            base.ways.insert(w.id, w.clone());
            data.ways.insert(w.id, w);
        }
        // Lowest index first, so each insert lands where it used to be.
        let mut relations = purged.relations;
        relations.sort_by_key(|(_, i, _)| *i);
        for (r, i, _) in &relations {
            let at = (*i).min(data.relations.len());
            data.relations.insert(at, r.clone());
        }
        relations.sort_by_key(|(_, _, j)| *j);
        for (r, _, j) in relations {
            let at = j.min(base.relations.len());
            base.relations.insert(at, r);
        }
        let data = Arc::new(data);
        self.rebuild_caches(&data);
        self.osm_data = Some(data);
        self.original_data = Some(Arc::new(base));
        self.revision += 1;
    }

    fn hit_test(&self, viewport: &Viewport, screen_pt: Point<Pixels>) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = node_markers::FULL_HIT_RADIUS;
        const WAY_TOL: f32 = WAY_HIT_TOL;
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

        // Phase 0: a turn-restriction marker (drawn only where vertices are).
        // Checked first because the marker sits beside its via node, never
        // on it, so it can't steal a click aimed at the node.
        if viewport.zoom_level() >= VERTEX_MIN_ZOOM {
            let mut marker_hits: Vec<HitCandidate> = Vec::new();
            for (&relation_id, r) in &self.mp.restrictions {
                let sp = viewport.mercator_to_screen(r.x, r.y);
                if !is_point_valid(sp) {
                    continue;
                }
                let c = point(
                    sp.x + px(RESTRICTION_OFFSET_PX.0),
                    sp.y + px(RESTRICTION_OFFSET_PX.1),
                );
                let dist = (c - screen_pt).magnitude() as f32;
                if dist <= RESTRICTION_MARKER_DIAMETER / 2.0 + 1.0 {
                    marker_hits.push(HitCandidate {
                        feature: FeatureRef {
                            layer_id: self.id,
                            kind: FeatureKind::Relation,
                            id: relation_id,
                        },
                        kind: FeatureKind::Relation,
                        dist_px: dist,
                    });
                }
            }
            if !marker_hits.is_empty() {
                return marker_hits;
            }
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
        let zoom = viewport.zoom_level();
        let pins = self.pins();
        for item in self.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let Some(&idx) = self.node_cache.index_by_id.get(&id) else {
                continue;
            };
            // A node whose marker is not shown is not a target: the click
            // goes to the way underneath. A simple marker has a smaller
            // radius than a full one.
            let form = self.node_form_at(&pins, idx, id, zoom);
            if !form.is_shown() {
                continue;
            }
            let (_, mx, my) = self.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= form.hit_radius() {
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
            for &(_, mx, my) in verts.iter() {
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

    fn hit_test_interior(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        if self.osm_data.is_none() {
            return Vec::new();
        }

        let (mx, my) = viewport.screen_to_mercator(screen_pt);
        let envelope = AABB::from_corners([mx, my], [mx, my]);

        let mut hits: Vec<HitCandidate> = Vec::new();
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.way_vertices[way_idx];
            // Closed-ring convention: first and last node ref are the same
            // (mirrors `OsmWay::is_closed`).
            if verts.len() < 4 || verts.first().map(|v| v.0) != verts.last().map(|v| v.0) {
                continue;
            }
            let screen_verts: Vec<Point<Pixels>> = verts
                .iter()
                .map(|&(_, vx, vy)| viewport.mercator_to_screen(vx, vy))
                .collect();
            if !screen_verts.iter().all(|p| is_point_valid(*p)) {
                continue;
            }
            if crate::selection::point_in_polygon(screen_pt, &screen_verts) {
                hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Way,
                        id: way_id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: crate::selection::polygon_area(&screen_verts),
                });
            }
        }

        // Multipolygon relations compete with the closed ways on area, holes
        // excluded: the point must be inside an outer ring and in none of its
        // holes. Only tagged relations take part (an untagged, old-style
        // multipolygon leaves the click to its tagged outer way), and the
        // area is scaled by a hair so a relation beats a closed way of
        // exactly the same area, e.g. its own single outer way.
        let (x0, _) = viewport.screen_to_mercator(point(px(0.0), px(0.0)));
        let (x1, _) = viewport.screen_to_mercator(point(px(1.0), px(0.0)));
        let m_per_px = (x1 - x0).abs();
        for (&relation_id, entry) in &self.mp.entries {
            let b = &entry.bbox;
            if !entry.tagged || mx < b.min_x || mx > b.max_x || my < b.min_y || my > b.max_y {
                continue;
            }
            let xy = |v: &(i64, f64, f64)| (v.1, v.2);
            let inside = entry.polygons.iter().any(|p| {
                let pb = &p.bbox;
                mx >= pb.min_x
                    && mx <= pb.max_x
                    && my >= pb.min_y
                    && my <= pb.max_y
                    && crate::multipolygon::point_in_ring(mx, my, p.ring(0), xy)
                    && !(1..p.ring_count())
                        .any(|k| crate::multipolygon::point_in_ring(mx, my, p.ring(k), xy))
            });
            if inside && m_per_px > 0.0 {
                hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Relation,
                        id: relation_id,
                    },
                    kind: FeatureKind::Relation,
                    dist_px: (entry.area / (m_per_px * m_per_px)) as f32 * 0.999,
                });
            }
        }
        hits
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
        // Only nodes whose marker is shown: a box at low zoom selects the
        // ways it encloses and the tagged nodes, not the hidden vertices.
        let zoom = viewport.zoom_level();
        let pins = self.pins();
        for item in self.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let shown = self
                .node_cache
                .index_by_id
                .get(&id)
                .is_some_and(|&idx| self.node_form_at(&pins, idx, id, zoom).is_shown());
            if shown {
                out.push(FeatureRef {
                    layer_id: self.id,
                    kind: FeatureKind::Node,
                    id,
                });
            }
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

    fn merge_download(&mut self, incoming: OsmData) -> MergeStats {
        let empty = || OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        let mut current = self.osm_data.as_deref().cloned().unwrap_or_else(empty);
        let mut original = self.original_data.as_deref().cloned().unwrap_or_else(empty);

        let stats = merge::merge_download(&mut original, &mut current, incoming);

        let modified = self.modified;
        self.set_osm_data(Arc::new(current));
        // `set_osm_data` treats its argument as a fresh server baseline;
        // restore the real one so pending edits stay pending.
        self.original_data = Some(Arc::new(original));
        self.modified = modified;
        self.revision += 1;
        stats
    }

    fn has_feature(&self, kind: FeatureKind, id: i64) -> bool {
        let Some(data) = self.osm_data.as_ref() else {
            return false;
        };
        match kind {
            FeatureKind::Node => data.nodes.contains_key(&id),
            FeatureKind::Way => data.ways.contains_key(&id),
            FeatureKind::Relation => data.relations.iter().any(|r| r.id == id),
        }
    }

    fn is_locally_deleted(&self, kind: FeatureKind, id: i64) -> bool {
        let Some(original) = self.original_data.as_ref() else {
            return false;
        };
        let known = match kind {
            FeatureKind::Node => original.nodes.contains_key(&id),
            FeatureKind::Way => original.ways.contains_key(&id),
            FeatureKind::Relation => original.relations.iter().any(|r| r.id == id),
        };
        known && !EditableLayer::has_feature(self, kind, id)
    }

    fn feature_version(&self, kind: FeatureKind, id: i64) -> Option<i32> {
        let data = self.osm_data.as_ref()?;
        match kind {
            FeatureKind::Node => data.nodes.get(&id).map(|n| n.version),
            FeatureKind::Way => data.ways.get(&id).map(|w| w.version),
            FeatureKind::Relation => data
                .relations
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.version),
        }
    }

    fn set_base_version(&mut self, kind: FeatureKind, id: i64, version: i32) -> bool {
        let (Some(current), Some(original)) = (self.osm_data.as_mut(), self.original_data.as_mut())
        else {
            return false;
        };
        let changed = resolve::set_base_version(
            Arc::make_mut(original),
            Arc::make_mut(current),
            kind,
            id,
            version,
        );
        if changed {
            self.revision += 1;
        }
        changed
    }

    fn adopt_server_element(
        &mut self,
        kind: FeatureKind,
        id: i64,
        server: crate::osm_api::ServerElement,
    ) -> bool {
        self.change_snapshots(|original, current| {
            resolve::adopt_server_element(original, current, kind, id, server)
        })
    }

    fn forget_local_delete(&mut self, kind: FeatureKind, id: i64) -> bool {
        self.change_snapshots(|original, current| {
            resolve::forget_local_delete(original, current, kind, id)
        })
    }

    fn all_features(&self) -> Vec<FeatureRef> {
        let Some(data) = self.osm_data.as_ref() else {
            return Vec::new();
        };
        let mut node_ids: Vec<i64> = data.nodes.keys().copied().collect();
        node_ids.sort_unstable();
        let mut way_ids: Vec<i64> = data.ways.keys().copied().collect();
        way_ids.sort_unstable();
        let refs = |kind: FeatureKind, ids: Vec<i64>| {
            let layer_id = self.id;
            ids.into_iter()
                .map(move |id| FeatureRef { layer_id, kind, id })
        };
        let mut relation_ids: Vec<i64> = data.relations.iter().map(|r| r.id).collect();
        relation_ids.sort_unstable();
        refs(FeatureKind::Node, node_ids)
            .chain(refs(FeatureKind::Way, way_ids))
            .chain(refs(FeatureKind::Relation, relation_ids))
            .collect()
    }

    fn search_features(
        &self,
        query: &crate::search::Query,
        selected: &std::collections::HashSet<(FeatureKind, i64)>,
        only_selected: bool,
        view: Option<crate::search::GeoBounds>,
    ) -> Vec<FeatureRef> {
        let Some(data) = self.osm_data.as_deref() else {
            return Vec::new();
        };
        let ctx = crate::search::SearchContext {
            original: self.original_data.as_deref(),
            selected: Some(selected),
            view,
            only_selected,
        };
        crate::search::search(query, data, &ctx)
            .into_iter()
            .map(|(kind, id)| FeatureRef {
                layer_id: self.id,
                kind,
                id,
            })
            .collect()
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
            FeatureKind::Relation => {
                let r = data.relations.iter().find(|r| r.id == feature.id)?;
                r.tags.clone()
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
        if feature.kind == FeatureKind::Node {
            // Same answer as `classify_geometry`, from the node-to-ways
            // index instead of a scan of every way: a relation lists many
            // nodes and this runs for each of them every frame.
            data.nodes.get(&feature.id)?;
            let used = self
                .node_to_ways
                .get(&feature.id)
                .is_some_and(|ways| !ways.is_empty());
            return Some(if used {
                crate::presets::Geometry::Vertex
            } else {
                crate::presets::Geometry::Point
            });
        }
        crate::presets::classify_geometry(data, feature.kind, feature.id, area_keys)
    }

    fn feature_geometry_candidates(
        &self,
        feature: &FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Vec<crate::presets::Geometry> {
        if feature.layer_id == self.id && feature.kind == FeatureKind::Way {
            if let Some(way) = self.osm_data.as_ref().and_then(|d| d.ways.get(&feature.id)) {
                return crate::presets::way_candidate_geometries(
                    &way.tags,
                    way.is_closed(),
                    area_keys,
                );
            }
        }
        self.feature_geometry(feature, area_keys)
            .into_iter()
            .collect()
    }

    fn feature_is_closed_way(&self, feature: &FeatureRef) -> bool {
        feature.layer_id == self.id
            && feature.kind == FeatureKind::Way
            && self
                .osm_data
                .as_ref()
                .and_then(|d| d.ways.get(&feature.id))
                .is_some_and(|w| w.is_closed())
    }

    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
        kind: HighlightKind,
    ) {
        if feature.layer_id != self.id {
            return;
        }
        let Some(ref osm_data) = self.osm_data else {
            return;
        };
        let (halo, core) = highlight_colors(kind);

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
                // Off screen (a big selection mostly is): nothing to draw.
                const MARGIN: f32 = 30.0;
                if kind == HighlightKind::Selected
                    && (sp.x.as_f32() < -MARGIN
                        || sp.y.as_f32() < -MARGIN
                        || sp.x.as_f32() > bounds.size.width.as_f32() + MARGIN
                        || sp.y.as_f32() > bounds.size.height.as_f32() + MARGIN)
                {
                    return;
                }
                let node_style = self.stylesheet.node_style(&n.tags, NODE_STYLE_ZOOM);
                let center = point(sp.x + bounds.origin.x, sp.y + bounds.origin.y);
                // The marker itself, in full: the zoom rule may have hidden
                // it or drawn it small.
                paint_node_marker(window, center, node_style);
                // Two concentric rings around the marker: the core color
                // inside, the halo color outside.
                let core_diameter = node_style.size + 10.0;
                paint_ring(window, center, core_diameter + 4.0, 2.0, halo);
                paint_ring(window, center, core_diameter, 2.0, core);
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
                // Parallel to `pts`: each node's marker style, so the
                // vertex-marker repaint below can reuse the same projected
                // screen point without redoing coordinate validation/lookup.
                let mut node_styles: Vec<NodeStyle> = Vec::with_capacity(way.nodes.len());
                for node_id in &way.nodes {
                    if let Some(n) = osm_data.nodes.get(node_id) {
                        if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                            let mut sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                sp += self.drag_preview_offset(*node_id);
                                pts.push(point(sp.x + origin_x, sp.y + origin_y));
                                node_styles
                                    .push(self.stylesheet.node_style(&n.tags, NODE_STYLE_ZOOM));
                            }
                        }
                    }
                }
                if pts.len() < 2 {
                    return;
                }
                // A relation can have hundreds of member ways, most of them
                // off screen at any zoom that shows a few: skip those.
                // A selection of thousands (Edit > Find) does the same.
                if matches!(kind, HighlightKind::Member | HighlightKind::Selected) {
                    let xs = pts.iter().map(|p| p.x.as_f32());
                    let ys = pts.iter().map(|p| p.y.as_f32());
                    let (min_x, max_x) =
                        xs.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                    let (min_y, max_y) =
                        ys.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                    let (left, top) = (origin_x.as_f32(), origin_y.as_f32());
                    let right = left + bounds.size.width.as_f32();
                    let bottom = top + bounds.size.height.as_f32();
                    if max_x < left || min_x > right || max_y < top || min_y > bottom {
                        return;
                    }
                }

                // A wide halo stroke under a narrower core stroke.
                let way_style =
                    self.stylesheet
                        .way_style(&way.tags, way.is_closed(), viewport.zoom_level());
                // Drawn with the way pass's own stroke geometry (mitered
                // bends, round ends, a joined closing corner) rather than a
                // tessellated stroke with square ends.
                let mut polyline = way_stroke::Polyline::default();
                polyline.set(pts.iter().map(|p| (p.x.as_f32(), p.y.as_f32())));
                for (extra_width, color) in [(7.0, halo), (3.0, core)] {
                    let mut path: Option<Path<Pixels>> = None;
                    let mut bounds_min_max = (
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                    );
                    polyline.emit(
                        &mut PathSink {
                            path: &mut path,
                            bounds: &mut bounds_min_max,
                        },
                        (way_style.width + extra_width) / 2.0,
                        way_stroke::Shape::FULL,
                    );
                    if let Some(mut path) = path {
                        let (min_x, max_x, min_y, max_y) = bounds_min_max;
                        path.bounds = Bounds {
                            origin: point(px(min_x), px(min_y)),
                            size: size(px(max_x - min_x), px(max_y - min_y)),
                        };
                        window.paint_path(path, rgb(color));
                    }
                }

                // The strokes above cover the way's own direction arrows, so
                // draw them again on top: core-colored, each outlined in the
                // halo color.
                if way_style.direction != Direction::None && viewport.zoom_level() >= ARROW_MIN_ZOOM
                {
                    let reverse = way_style.direction == Direction::Reverse;
                    let half_base = (way_style.width + 3.0) / 2.0 + ARROW_OVERHANG;
                    for (grow, color) in [(1.5, halo), (0.0, core)] {
                        let mut path: Option<Path<Pixels>> = None;
                        let mut bounds_min_max = (
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                        );
                        for i in arrow_segments(&pts) {
                            if let Some([tip, left, right]) = arrow_triangle(
                                pts[i],
                                pts[i + 1],
                                reverse,
                                ARROW_HALF_LENGTH + 1.0 + 1.5 * grow,
                                half_base + grow,
                            ) {
                                push_triangle(&mut path, &mut bounds_min_max, tip, left, right);
                            }
                        }
                        if let Some(mut path) = path {
                            let (min_x, max_x, min_y, max_y) = bounds_min_max;
                            path.bounds = Bounds {
                                origin: point(px(min_x), px(min_y)),
                                size: size(px(max_x - min_x), px(max_y - min_y)),
                            };
                            window.paint_path(path, rgb(color));
                        }
                    }
                }

                // Re-paint the way's vertex markers on top of the highlight
                // strokes above — otherwise they fully cover the node-marker
                // quads painted earlier in `render_canvas`, hiding the way's
                // vertices. This also shows a highlighted way's vertices at
                // zooms where plain vertices are otherwise hidden.
                // Hover and relation members repaint only the markers the
                // zoom rule shows (the ones that can be targeted); a
                // selected way shows all of its vertices.
                let zoom = viewport.zoom_level();
                // The hovered way is the preview way: its vertices that are
                // not otherwise shown get the small preview dot (the
                // stroke above covered the ones painted by `render_canvas`).
                let preview = kind == HighlightKind::Hovered
                    && self.preview_way == Some(feature.id)
                    && zoom >= node_markers::PREVIEW_MIN_ZOOM;
                for (p, style) in pts.iter().zip(node_styles.iter()) {
                    if kind == HighlightKind::Selected
                        || node_markers::node_form(style.tagged, false, false, zoom).is_shown()
                    {
                        paint_node_marker(window, *p, *style);
                    } else if preview {
                        paint_preview_marker(window, *p);
                    }
                }
            }
            FeatureKind::Relation => {
                // A relation has no shape of its own: mark each member that
                // is loaded (nested relations' members too, one level).
                let Some(relation) = osm_data.relations.iter().find(|r| r.id == feature.id) else {
                    return;
                };
                // A multipolygon is an area: tint it in the highlight's color
                // so the whole shape reads as selected, under the members.
                if let Some(entry) = self.mp.entries.get(&feature.id) {
                    self.render_area_tint(viewport, bounds, window, entry, core, kind);
                }
                let find = |id: i64| osm_data.relations.iter().find(|r| r.id == id).cloned();
                for (member_type, id) in crate::relations::flatten_members(relation, &find) {
                    let kind = match member_type {
                        crate::osm::MemberType::Node => FeatureKind::Node,
                        crate::osm::MemberType::Way => FeatureKind::Way,
                        crate::osm::MemberType::Relation => continue,
                    };
                    let member = FeatureRef {
                        layer_id: self.id,
                        kind,
                        id,
                    };
                    self.render_highlight(viewport, bounds, window, &member, HighlightKind::Member);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "osm_layer_tests/mod.rs"]
mod tests;
