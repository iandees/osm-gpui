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

    /// Paint labels and icons: place when the camera has settled (or the
    /// data changed), otherwise just re-project the last placement.
    fn paint_labels(
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

    fn refresh_modified(&mut self) {
        if self.modified {
            self.modified = !OsmLayer::diff_for_upload(self).is_empty();
        }
    }

    fn as_editable(&self) -> Option<&dyn EditableLayer> {
        Some(self)
    }

    fn as_editable_mut(&mut self) -> Option<&mut dyn EditableLayer> {
        Some(self)
    }

    fn set_stylesheet(&mut self, stylesheet: Arc<Stylesheet>) {
        OsmLayer::set_stylesheet(self, stylesheet);
    }

    fn diff_for_upload(&self) -> LayerDiff {
        OsmLayer::diff_for_upload(self)
    }

    fn apply_upload_result(&mut self, result: &UploadResult) {
        OsmLayer::apply_upload_result(self, result);
    }

    fn apply_chunk_result(&mut self, result: &UploadResult, chunk: &LayerDiff) {
        OsmLayer::apply_chunk_result(self, result, chunk);
    }

    fn export_xml(&self) -> Option<String> {
        OsmLayer::export_xml(self)
    }

    fn edit_revision(&self) -> u64 {
        OsmLayer::edit_revision(self)
    }

    fn autosave_snapshot(&self) -> Option<(Arc<OsmData>, Arc<OsmData>)> {
        OsmLayer::autosave_snapshot(self)
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

    fn download_rects(&self) -> &[crate::download_bounds::MercRect] {
        &self.download_rects
    }

    fn render_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        selected: &[FeatureRef],
        hovered: Option<&FeatureRef>,
        window: &mut Window,
        cx: &App,
    ) {
        self.paint_labels(viewport, bounds, selected, hovered, window, cx);
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
        for &id in relations {
            let base_index = base.relations.iter().position(|r| r.id == id);
            if let Some(i) = base_index {
                base.relations.remove(i);
            }
            if let Some(i) = data.relations.iter().position(|r| r.id == id) {
                let r = data.relations.remove(i);
                purged.relations.push((r, i, base_index.unwrap_or(i)));
            }
        }
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
        self.revision += 1;
        resolve::set_base_version(
            Arc::make_mut(original),
            Arc::make_mut(current),
            kind,
            id,
            version,
        )
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
mod tests {
    use crate::layers::node_markers::{self, NodeForm};
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::{EditError, EditableLayer, LayerId, MapLayer};
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::{FeatureKind, FeatureRef};
    use crate::style::Stylesheet;
    use crate::viewport::Viewport;
    use gpui::{point, px, size, Bounds};
    use std::collections::{HashMap, HashSet};
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
            bounds: Vec::new(),
        })
    }

    fn bounds_data(rects: &[(f64, f64, f64, f64)]) -> OsmData {
        let mut d = (*data_with(vec![], vec![])).clone();
        d.bounds = rects
            .iter()
            .map(
                |&(min_lat, max_lat, min_lon, max_lon)| crate::osm::OsmBounds {
                    min_lat,
                    max_lat,
                    min_lon,
                    max_lon,
                },
            )
            .collect();
        d
    }

    #[test]
    fn layer_without_bounds_has_no_download_rects() {
        let layer = OsmLayer::new_with_data(LayerId(1), "x", data_with(vec![], vec![]));
        assert!(layer.download_rects().is_empty());
    }

    #[test]
    fn download_rects_follow_loading_and_merging() {
        let mut layer = OsmLayer::new_with_data(
            LayerId(1),
            "x",
            Arc::new(bounds_data(&[(40.0, 40.1, -74.1, -74.0)])),
        );
        assert_eq!(layer.download_rects().len(), 1);

        layer.merge_download(bounds_data(&[(40.1, 40.2, -74.1, -74.0)]));
        assert_eq!(layer.download_rects().len(), 2);
        let exported = layer.export_xml().unwrap();
        assert_eq!(exported.matches("<bounds ").count(), 2);

        // Merging data with no bounds keeps what is known.
        layer.merge_download(bounds_data(&[]));
        assert_eq!(layer.download_rects().len(), 2);
        // The merge isn't an edit.
        assert!(!layer.is_modified());

        layer.clear_osm_data();
        assert!(layer.download_rects().is_empty());
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
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        // Plain vertices are drawn (and hit) only on a selected way.
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 10,
        }]);

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
    fn hit_test_interior_hits_closed_way_center_but_plain_hit_test_misses() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let d = 0.01;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat - d,
            lon: center_lon - d,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat - d,
            lon: center_lon + d,
            version: 1,
            tags: empty_tags(),
        };
        let n3 = OsmNode {
            id: 3,
            lat: center_lat + d,
            lon: center_lon + d,
            version: 1,
            tags: empty_tags(),
        };
        let n4 = OsmNode {
            id: 4,
            lat: center_lat + d,
            lon: center_lon - d,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 4, 1],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2, n3, n4], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let center = point(px(400.0), px(300.0));

        // The center of the square is far from every edge — a plain
        // hit_test (edge/outline distance only) should not select it.
        assert!(layer.hit_test(&viewport, center).is_empty());

        // Interior hit-testing should find it.
        let hits = layer.hit_test_interior(&viewport, center);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, FeatureKind::Way);
        assert_eq!(hits[0].feature.id, 10);
    }

    #[test]
    fn resolve_hits_picks_smallest_of_nested_closed_ways() {
        let center_lat = 40.0;
        let center_lon = -74.0;

        // Outer, large square.
        let d_outer = 0.01;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat - d_outer,
            lon: center_lon - d_outer,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat - d_outer,
            lon: center_lon + d_outer,
            version: 1,
            tags: empty_tags(),
        };
        let n3 = OsmNode {
            id: 3,
            lat: center_lat + d_outer,
            lon: center_lon + d_outer,
            version: 1,
            tags: empty_tags(),
        };
        let n4 = OsmNode {
            id: 4,
            lat: center_lat + d_outer,
            lon: center_lon - d_outer,
            version: 1,
            tags: empty_tags(),
        };
        let outer_way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 4, 1],
            version: 1,
            tags: empty_tags(),
        };

        // Inner, small square nested entirely inside the outer one, drawn
        // first so a plain "last one wins" tie-break would pick the outer
        // way instead.
        let d_inner = 0.001;
        let n5 = OsmNode {
            id: 5,
            lat: center_lat - d_inner,
            lon: center_lon - d_inner,
            version: 1,
            tags: empty_tags(),
        };
        let n6 = OsmNode {
            id: 6,
            lat: center_lat - d_inner,
            lon: center_lon + d_inner,
            version: 1,
            tags: empty_tags(),
        };
        let n7 = OsmNode {
            id: 7,
            lat: center_lat + d_inner,
            lon: center_lon + d_inner,
            version: 1,
            tags: empty_tags(),
        };
        let n8 = OsmNode {
            id: 8,
            lat: center_lat + d_inner,
            lon: center_lon - d_inner,
            version: 1,
            tags: empty_tags(),
        };
        let inner_way = OsmWay {
            id: 20,
            nodes: vec![5, 6, 7, 8, 5],
            version: 1,
            tags: empty_tags(),
        };

        let data = data_with(
            vec![n1, n2, n3, n4, n5, n6, n7, n8],
            vec![inner_way, outer_way],
        );
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let center = point(px(400.0), px(300.0));

        let hits = layer.hit_test_interior(&viewport, center);
        assert_eq!(hits.len(), 2);

        let winner = crate::selection::resolve_hits(vec![hits]).unwrap();
        assert_eq!(winner.id, 20, "smallest enclosing way should win");
    }

    #[test]
    fn hit_test_interior_ignores_open_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let d = 0.01;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat - d,
            lon: center_lon - d,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat - d,
            lon: center_lon + d,
            version: 1,
            tags: empty_tags(),
        };
        let n3 = OsmNode {
            id: 3,
            lat: center_lat + d,
            lon: center_lon + d,
            version: 1,
            tags: empty_tags(),
        };
        let n4 = OsmNode {
            id: 4,
            lat: center_lat + d,
            lon: center_lon - d,
            version: 1,
            tags: empty_tags(),
        };
        // Not closed: first node ref (1) != last (4).
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 4],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2, n3, n4], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let center = point(px(400.0), px(300.0));

        assert!(layer.hit_test_interior(&viewport, center).is_empty());
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
    fn snap_to_way_on_line_returns_exact_click_point() {
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

        let (way_id, a, b, idx, lat, lon) = layer
            .snap_to_way(&viewport, point(px(400.0), px(300.0)), 6.0)
            .expect("expected a snap hit");
        assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
        assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
        assert!((lon - center_lon).abs() < 1e-6, "got lon {}", lon);
    }

    #[test]
    fn snap_to_way_off_line_projects_onto_segment() {
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

        // 3px above the (horizontal) line, within the 6px tolerance.
        let (way_id, a, b, idx, lat, lon) = layer
            .snap_to_way(&viewport, point(px(403.0), px(297.0)), 6.0)
            .expect("expected a snap hit");
        assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
        // The line is flat (both endpoints share center_lat), so the
        // projected point's lat matches the line exactly regardless of the
        // click's vertical offset.
        assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
        assert!(
            lon > center_lon - 0.001 && lon < center_lon + 0.001,
            "got lon {}",
            lon
        );
    }

    #[test]
    fn snap_to_way_none_when_out_of_tolerance() {
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

        let hit = layer.snap_to_way(&viewport, point(px(0.0), px(0.0)), 6.0);
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
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        // Plain vertices are boxed only when their way is selected.
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 10,
        }]);

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
        layer.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();

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

        layer.commit_node_moves(&[]).unwrap();
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

        layer
            .set_tag(FeatureKind::Node, 1, "highway", "residential")
            .unwrap();
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).unwrap().tags.get("highway"),
            Some(&"residential".to_string())
        );

        layer
            .set_tag(FeatureKind::Node, 1, "highway", "trunk")
            .unwrap();
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

        layer
            .set_tag(FeatureKind::Way, 10, "surface", "paved")
            .unwrap();
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

        assert_eq!(
            layer.set_tag(FeatureKind::Node, 999, "highway", "residential"),
            Err(EditError::UnknownNode(999))
        );
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

        layer.remove_tag(FeatureKind::Node, 1, "highway").unwrap();
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

        assert_eq!(
            layer.remove_tag(FeatureKind::Node, 999, "highway"),
            Err(EditError::UnknownNode(999))
        );
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

        layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();

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
        layer.set_tag(FeatureKind::Node, 1, "name", "Test").unwrap();
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

    #[test]
    fn arrow_segments_space_arrows_along_the_path_and_skip_short_segments() {
        let at = |x: f32| point(px(x), px(0.0));
        // A short way still gets one arrow, on its only segment.
        assert_eq!(super::arrow_segments(&[at(0.0), at(60.0)]), vec![0]);
        // Too short to hold an arrow at all.
        assert!(super::arrow_segments(&[at(0.0), at(5.0)]).is_empty());
        // 100px segments: an arrow on each (spacing is under 100px).
        assert_eq!(
            super::arrow_segments(&[at(0.0), at(100.0), at(200.0), at(300.0)]),
            vec![0, 1, 2]
        );
        // 30px segments: one arrow, then roughly every fourth segment.
        let dense: Vec<_> = (0..12).map(|i| at(i as f32 * 30.0)).collect();
        assert_eq!(super::arrow_segments(&dense), vec![1, 5, 9]);
        assert!(super::arrow_segments(&[]).is_empty());
    }

    #[test]
    fn arrow_triangle_points_along_the_segment_from_its_midpoint() {
        let p0 = point(px(0.0), px(0.0));
        let p1 = point(px(100.0), px(0.0));
        let xy = |p: gpui::Point<gpui::Pixels>| (f32::from(p.x), f32::from(p.y));

        let [tip, left, right] = super::arrow_triangle(p0, p1, false, 5.0, 3.0).unwrap();
        assert_eq!(xy(tip), (55.0, 0.0));
        assert_eq!(xy(left), (45.0, 3.0));
        assert_eq!(xy(right), (45.0, -3.0));

        // Reversed: same midpoint, tip toward p0.
        let [tip, ..] = super::arrow_triangle(p0, p1, true, 5.0, 3.0).unwrap();
        assert_eq!(xy(tip), (45.0, 0.0));

        assert!(super::arrow_triangle(p0, p0, false, 5.0, 3.0).is_none());
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
        layer.commit_node_moves(&[(1, new_lat, new_lon)]).unwrap();

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
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 10,
        }]);
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
    fn adjacent_closed_ways_sharing_two_nodes_keep_node_to_ways_consistent() {
        // Two closed ways (rings) sharing an edge (nodes 1 and 2), mimicking
        // two adjacent building/parcel polygons. Each way's node list closes
        // the ring by repeating its first node id as its last id, so node 1
        // appears twice in way_a's own node list and node 2 appears twice in
        // way_b's — this is exactly the shape that exposed a duplicate/
        // dropped `node_to_ways` entry bug when one of the two ways is
        // dragged and the move is committed.
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
            lon: -74.0,
            tags: empty_tags(),
            version: 1,
        };
        let n4 = OsmNode {
            id: 4,
            lat: 40.001,
            lon: -73.999,
            tags: empty_tags(),
            version: 1,
        };
        // way_a: ring 1 -> 2 -> 3 -> 1 (closed).
        let way_a = OsmWay {
            id: 10,
            nodes: vec![1, 2, 3, 1],
            tags: empty_tags(),
            version: 1,
        };
        // way_b: ring 1 -> 4 -> 2 -> 1 (closed), sharing the 1-2 edge with way_a.
        let way_b = OsmWay {
            id: 20,
            nodes: vec![1, 4, 2, 1],
            tags: empty_tags(),
            version: 1,
        };
        let data = data_with(vec![n1, n2, n3, n4], vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let old_bbox_a = layer.way_bboxes[0];
        let old_bbox_b = layer.way_bboxes[1];

        // Drag+commit both shared nodes (1 and 2), as a whole-way move would.
        layer
            .commit_node_moves(&[(1, 41.0, -75.0), (2, 41.001, -75.001)])
            .unwrap();

        assert!(layer.is_modified());
        assert_ne!(
            layer.way_bboxes[0], old_bbox_a,
            "way A's bbox should update"
        );
        assert_ne!(
            layer.way_bboxes[1], old_bbox_b,
            "way B's bbox should update"
        );

        // Every node id must map to exactly the way indices that actually
        // reference it, with no duplicates and none dropped, despite each
        // way listing its ring-closing node twice.
        assert_eq!(
            layer.node_to_ways.get(&1).cloned(),
            Some([0usize, 1usize].into_iter().collect())
        );
        assert_eq!(
            layer.node_to_ways.get(&2).cloned(),
            Some([0usize, 1usize].into_iter().collect())
        );
        assert_eq!(
            layer.node_to_ways.get(&3).cloned(),
            Some([0usize].into_iter().collect())
        );
        assert_eq!(
            layer.node_to_ways.get(&4).cloned(),
            Some([1usize].into_iter().collect())
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
        layer.commit_node_moves(&[(1, new_lat, new_lon)]).unwrap();

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
        layer.commit_node_moves(&[(3, new_lat, new_lon)]).unwrap();

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
        layer
            .set_tag(FeatureKind::Way, 10, "highway", "residential")
            .unwrap();
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
        layer
            .set_tag(FeatureKind::Node, 1, "amenity", "cafe")
            .unwrap();
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

    fn area_label_sheet() -> Arc<Stylesheet> {
        Arc::new(Stylesheet::parse("area|z17-[building], area[leisure] { text: name; }").unwrap())
    }

    #[test]
    fn area_label_geometry_is_cached_and_follows_vertex_edits_only() {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
        layer.set_stylesheet(area_label_sheet());
        let mut cache = crate::layers::labels::LabelCache::default();
        let key = (crate::layers::labels::KIND_WAY, 10);
        let validate = |layer: &OsmLayer, cache: &mut crate::layers::labels::LabelCache| {
            cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 18);
        };
        validate(&layer, &mut cache);
        assert!(
            layer.area_label_for(&mut cache, key, 18.0).is_none(),
            "no name, no label"
        );

        layer.set_tag(FeatureKind::Way, 10, "name", "Hall").unwrap();
        validate(&layer, &mut cache);
        let l = layer.area_label_for(&mut cache, key, 18.0).unwrap();
        assert_eq!(l.text.as_ref(), "Hall");
        cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 16);
        assert!(
            layer.area_label_for(&mut cache, key, 16.5).is_none(),
            "below the rule's zoom (a fresh bucket)"
        );
        validate(&layer, &mut cache);

        let g1 = layer.area_geom_for(&mut cache, key).unwrap();
        let g2 = layer.area_geom_for(&mut cache, key).unwrap();
        assert!(Arc::ptr_eq(&g1, &g2));
        assert_eq!(cache.areas.builds, 1, "computed once");

        // A tag edit leaves the geometry alone.
        layer
            .set_tag(FeatureKind::Way, 10, "name", "Hall 2")
            .unwrap();
        validate(&layer, &mut cache);
        assert_eq!(
            layer
                .area_label_for(&mut cache, key, 18.0)
                .unwrap()
                .text
                .as_ref(),
            "Hall 2"
        );
        let _ = layer.area_geom_for(&mut cache, key).unwrap();
        assert_eq!(cache.areas.builds, 1);

        // Moving a corner rebuilds it, and the pole moves.
        layer.commit_node_moves(&[(3, 40.004, -73.997)]).unwrap();
        validate(&layer, &mut cache);
        let g3 = layer.area_geom_for(&mut cache, key).unwrap();
        assert_eq!(cache.areas.builds, 2, "a vertex edit invalidates");
        assert!(g3.polys[0].bbox.2 > g1.polys[0].bbox.2);
        assert_ne!(
            (g3.polys[0].pole.x, g3.polys[0].pole.y),
            (g1.polys[0].pole.x, g1.polys[0].pole.y)
        );
    }

    #[test]
    fn multipolygon_area_label_uses_the_relation_tags_and_avoids_holes() {
        use crate::osm::{MemberType, OsmMember, OsmRelation};
        let mk = |id, dlat: f64, dlon: f64| OsmNode {
            id,
            lat: 40.0 + dlat,
            lon: -74.0 + dlon,
            version: 1,
            tags: empty_tags(),
        };
        let way = |id, nodes: Vec<i64>| OsmWay {
            id,
            nodes,
            version: 1,
            tags: empty_tags(),
        };
        let mut base = (*data_with(
            vec![
                mk(1, 0.0, 0.0),
                mk(2, 0.0, 0.01),
                mk(3, 0.01, 0.01),
                mk(4, 0.01, 0.0),
                mk(5, 0.002, 0.002),
                mk(6, 0.002, 0.008),
                mk(7, 0.008, 0.008),
                mk(8, 0.008, 0.002),
            ],
            vec![way(10, vec![1, 2, 3, 4, 1]), way(11, vec![5, 6, 7, 8, 5])],
        ))
        .clone();
        let member = |reference, role: &str| OsmMember {
            member_type: MemberType::Way,
            reference,
            role: role.to_string(),
        };
        let mut tags = HashMap::new();
        for (k, v) in [
            ("type", "multipolygon"),
            ("leisure", "park"),
            ("name", "Ring Park"),
        ] {
            tags.insert(k.to_string(), v.to_string());
        }
        base.relations.push(OsmRelation {
            id: 500,
            version: 1,
            tags,
            members: vec![member(10, "outer"), member(11, "inner")],
        });
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
        layer.set_stylesheet(area_label_sheet());
        let mut cache = crate::layers::labels::LabelCache::default();
        cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 17);
        let key = (crate::layers::labels::KIND_REL, 500);
        let l = layer.area_label_for(&mut cache, key, 17.0).unwrap();
        assert_eq!(l.text.as_ref(), "Ring Park");
        assert_eq!(l.priority, 300);
        let g = layer.area_geom_for(&mut cache, key).unwrap();
        assert_eq!(g.polys.len(), 1);
        assert_eq!(g.polys[0].rings.len(), 2, "outer ring plus the hole");
        let hole_edge = crate::coordinates::lat_lon_to_mercator(40.002, -74.002);
        let hole_far = crate::coordinates::lat_lon_to_mercator(40.008, -73.992);
        let p = g.polys[0].pole;
        let in_hole =
            p.x > hole_edge.0 && p.x < hole_far.0 && p.y > hole_edge.1 && p.y < hole_far.1;
        assert!(!in_hole, "pole {p:?} is in the hole");
        // The member ways carry no tags, so they get no label of their own.
        assert!(layer
            .area_label_for(&mut cache, (crate::layers::labels::KIND_WAY, 10), 17.0)
            .is_none());
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
        layer
            .set_tag(FeatureKind::Way, 10, "building", "yes")
            .unwrap();
        assert!(
            layer.way_fill_tris[0].is_some(),
            "tag edit must recompute fill"
        );
        layer.remove_tag(FeatureKind::Way, 10, "building").unwrap();
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
        layer.delete_feature(FeatureKind::Way, 10).unwrap();
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

        assert_eq!(
            layer.create_node(41.0, -75.0, Some(-1)),
            Err(EditError::IdInUse(FeatureKind::Node, -1))
        );
        assert_eq!(layer.create_node(41.0, -75.0, Some(-99)), Ok(-99));
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
            layer.delete_feature(FeatureKind::Node, 1).unwrap_err(),
            EditError::NodeInWay(1),
            "node still referenced by a way must be refused"
        );
        assert!(!layer.is_modified());

        assert!(layer.delete_feature(FeatureKind::Way, 10).is_ok());
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
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 20,
        }]);
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
        assert_eq!(
            layer.delete_feature(FeatureKind::Node, 999).unwrap_err(),
            EditError::UnknownNode(999)
        );
        assert_eq!(
            layer.delete_feature(FeatureKind::Way, 999).unwrap_err(),
            EditError::UnknownWay(999)
        );
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

        layer.restore_feature(snapshot).unwrap();
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

        layer.restore_feature(snapshot).unwrap();
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        let restored = layer.get_osm_data().unwrap();
        let way = restored.ways.get(&10).unwrap();
        assert_eq!(way.tags.get("highway"), Some(&"residential".to_string()));

        // node_to_ways was patched so the restored way is picked up by a
        // node-move on one of its members.
        layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();
        let (mx, my) = crate::coordinates::lat_lon_to_mercator(41.0, -75.0);
        let verts = layer.way_vertices[*layer.way_id_to_index.get(&10).unwrap()].clone();
        let &(_, vmx, vmy) = verts.iter().find(|&&(id, _, _)| id == 1).unwrap();
        assert!((vmx - mx).abs() < 1e-6);
        assert!((vmy - my).abs() < 1e-6);
    }

    #[test]
    fn undoing_a_delete_keeps_the_server_version_in_the_uploaded_modify() {
        let node = |id, version| OsmNode {
            id,
            lat: 40.0,
            lon: -74.0,
            tags: empty_tags(),
            version,
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            tags: empty_tags(),
            version: 7,
        };
        let data = data_with(vec![node(1, 2), node(2, 2), node(3, 4)], vec![way]);
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let way_snapshot = layer.delete_feature(FeatureKind::Way, 10).unwrap();
        let node_snapshot = layer.delete_feature(FeatureKind::Node, 3).unwrap();
        assert_eq!((way_snapshot.version, node_snapshot.version), (7, 4));
        // Redo (delete again after undo) reads the version from the layer.
        layer.restore_feature(node_snapshot.clone()).unwrap();
        layer.restore_feature(way_snapshot.clone()).unwrap();
        assert_eq!(layer.get_osm_data().unwrap().nodes[&3].version, 4);
        assert_eq!(layer.get_osm_data().unwrap().ways[&10].version, 7);
        // Undone: nothing to upload yet.
        assert!(layer.diff_for_upload().is_empty());

        layer
            .set_tag(FeatureKind::Way, 10, "highway", "path")
            .unwrap();
        layer
            .set_tag(FeatureKind::Node, 3, "amenity", "cafe")
            .unwrap();
        let xml = crate::osm_upload::build_osm_change_xml(1, &[("L", layer.diff_for_upload())]);
        assert!(xml.contains(r#"<way id="10" version="7""#), "{xml}");
        assert!(
            xml.contains(r#"<node id="3" lat="40" lon="-74" version="4""#),
            "{xml}"
        );

        // Deleting the restored objects again reports the original versions.
        layer.delete_feature(FeatureKind::Way, 10).unwrap();
        layer.delete_feature(FeatureKind::Node, 3).unwrap();
        let diff = layer.diff_for_upload();
        assert_eq!(diff.deleted_way_ids, vec![(10, 7)]);
        assert_eq!(diff.deleted_node_ids, vec![(3, 4)]);
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

        layer
            .set_tag(FeatureKind::Node, 1, "amenity", "cafe")
            .unwrap();
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

        layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();
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

        layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();
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
    fn set_relation_members_returns_the_previous_list_and_marks_the_layer_modified() {
        use crate::osm::{MemberType, OsmMember, OsmRelation};
        let member = |reference| OsmMember {
            member_type: MemberType::Way,
            reference,
            role: "r".to_string(),
        };
        let mut base = (*data_with(vec![], vec![])).clone();
        base.relations.push(OsmRelation {
            id: 500,
            version: 3,
            tags: empty_tags(),
            members: vec![member(10)],
        });
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
        assert!(!layer.is_modified());

        assert_eq!(
            layer.set_relation_members(404, vec![]),
            Err(EditError::UnknownRelation(404))
        );
        assert!(!layer.is_modified(), "unknown relation changes nothing");

        let previous = layer.set_relation_members(500, vec![member(10), member(11)]);
        assert_eq!(previous, Ok(vec![member(10)]));
        assert!(layer.is_modified());
        assert_eq!(layer.relation(500).unwrap().members.len(), 2);
        assert_eq!(layer.relations_containing_way(11), vec![500]);
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_relations.len(), 1);
        assert_eq!(diff.modified_relations[0].version, 3);

        // Putting the old list back leaves nothing to upload.
        layer.set_relation_members(500, vec![member(10)]).unwrap();
        assert!(layer.diff_for_upload().is_empty());
    }

    #[test]
    fn relation_member_highlight_differs_from_selection_and_hover() {
        use crate::layers::HighlightKind::{Hovered, Member, Selected};
        let colors = |kind| super::highlight_colors(kind);
        assert_ne!(colors(Member), colors(Selected));
        assert_ne!(colors(Member), colors(Hovered));
    }

    #[test]
    fn relation_tags_read_edit_and_remove_through_the_layer() {
        use crate::osm::OsmRelation;
        let mut base = (*data_with(vec![], vec![])).clone();
        base.relations.push(OsmRelation {
            id: 500,
            version: 3,
            tags: HashMap::from([("type".to_string(), "route".to_string())]),
            members: vec![],
        });
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
        let rel = FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Relation,
            id: 500,
        };
        let missing = FeatureRef { id: 404, ..rel };
        assert_eq!(
            EditableLayer::feature_tags(&layer, &rel),
            Some(vec![("type".to_string(), "route".to_string())])
        );
        assert_eq!(EditableLayer::feature_tags(&layer, &missing), None);
        assert_eq!(
            EditableLayer::feature_geometry(&layer, &rel, crate::presets::area_keys()),
            Some(crate::presets::Geometry::Relation)
        );
        assert!(!layer.is_modified());

        layer
            .set_tag(FeatureKind::Relation, 500, "name", "Blue Line")
            .unwrap();
        assert!(layer.is_modified());
        assert_eq!(EditableLayer::feature_tags(&layer, &rel).unwrap().len(), 2);
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_relations.len(), 1);
        assert_eq!(diff.modified_relations[0].tags["name"], "Blue Line");

        layer
            .remove_tag(FeatureKind::Relation, 500, "name")
            .unwrap();
        assert!(layer.diff_for_upload().is_empty(), "back to the original");

        // `delete_feature` is for nodes and ways; a missing relation is refused.
        assert!(layer.delete_feature(FeatureKind::Relation, 500).is_err());
        assert!(layer.relation(500).is_some());
        assert_eq!(
            layer.set_tag(FeatureKind::Relation, 404, "a", "b"),
            Err(EditError::UnknownRelation(404))
        );
    }

    #[test]
    fn modified_relation_uploads_with_placeholders_and_is_remapped_afterwards() {
        use crate::osm::{MemberType, OsmMember, OsmRelation};
        let member = |member_type, reference, role: &str| OsmMember {
            member_type,
            reference,
            role: role.to_string(),
        };
        let mut base = (*data_with(
            vec![OsmNode {
                id: 1,
                lat: 40.0,
                lon: -74.0,
                version: 1,
                tags: empty_tags(),
            }],
            vec![OsmWay {
                id: 10,
                nodes: vec![1],
                version: 2,
                tags: empty_tags(),
            }],
        ))
        .clone();
        base.relations.push(OsmRelation {
            id: 500,
            version: 7,
            tags: empty_tags(),
            members: vec![member(MemberType::Way, 10, "route")],
        });
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base.clone()));

        // A new way -2 (with a new node -1) joins relation 500 as a member.
        let mut edited = base.clone();
        edited.nodes.insert(
            -1,
            OsmNode {
                id: -1,
                lat: 40.001,
                lon: -74.001,
                version: 0,
                tags: empty_tags(),
            },
        );
        edited.ways.insert(
            -2,
            OsmWay {
                id: -2,
                nodes: vec![1, -1],
                version: 0,
                tags: empty_tags(),
            },
        );
        edited.relations[0]
            .members
            .push(member(MemberType::Way, -2, "route"));
        edited.relations[0]
            .members
            .push(member(MemberType::Node, -1, "stop"));
        layer.set_osm_data_for_test(Arc::new(edited));

        let diff = layer.diff_for_upload();
        assert_eq!(diff.counts(), (2, 1, 0));
        let xml = crate::osm_upload::build_osm_change_xml(3, &[("L", diff)]);
        let way_create = xml.find(r#"<way id="-2""#).unwrap();
        let relation_modify = xml.find(r#"<relation id="500" version="7""#).unwrap();
        assert!(way_create < relation_modify, "{xml}");
        assert!(
            xml.contains(r#"<member type="way" ref="-2" role="route"/>"#),
            "{xml}"
        );

        let mut result = crate::osm_upload::UploadResult::default();
        result.node_id_remap.insert(-1, (999, 1));
        result.way_id_remap.insert(-2, (888, 1));
        result.relation_id_remap.insert(500, (500, 8));
        layer.apply_upload_result(&result);

        let updated = layer.get_osm_data().unwrap();
        let relation = &updated.relations[0];
        assert_eq!(relation.version, 8);
        assert_eq!(
            relation
                .members
                .iter()
                .map(|m| (m.member_type, m.reference))
                .collect::<Vec<_>>(),
            vec![
                (MemberType::Way, 10),
                (MemberType::Way, 888),
                (MemberType::Node, 999)
            ]
        );
        assert!(!layer.is_modified());
        assert!(layer.diff_for_upload().is_empty());
    }

    fn relation_member(
        member_type: crate::osm::MemberType,
        reference: i64,
    ) -> crate::osm::OsmMember {
        crate::osm::OsmMember {
            member_type,
            reference,
            role: String::new(),
        }
    }

    fn layer_with_server_relation() -> OsmLayer {
        use crate::osm::{MemberType, OsmRelation};
        let mut base = (*data_with(vec![], vec![])).clone();
        base.relations.push(OsmRelation {
            id: 500,
            version: 7,
            tags: empty_tags(),
            members: vec![relation_member(MemberType::Way, 10)],
        });
        OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base))
    }

    #[test]
    fn create_relation_allocates_unique_negative_ids_shared_with_nodes_and_ways() {
        use crate::osm::MemberType::Way;
        let mut layer = layer_with_server_relation();
        let n = layer.add_node(40.0, -74.0);
        let w = layer.add_way(vec![n], vec![]);
        let r1 = layer.create_relation(vec![], vec![]).unwrap();
        let r2 = layer
            .create_relation(
                vec![("type".to_string(), "multipolygon".to_string())],
                vec![relation_member(Way, w)],
            )
            .unwrap();
        assert!(r1 < 0 && r2 < r1);
        assert!(r1 < n.min(w), "below every node and way id handed out");
        // Later nodes and ways stay clear of the relation ids.
        let n2 = layer.add_node(40.1, -74.1);
        assert!(n2 < r2);
        assert_eq!(layer.relation(r2).unwrap().version, 0);
        assert_eq!(layer.relations().len(), 3);
        assert!(layer.is_modified());
        let diff = layer.diff_for_upload();
        assert_eq!(diff.created_relations.len(), 2);
    }

    #[test]
    fn delete_relation_refuses_a_member_relation_and_restores_in_place() {
        use crate::osm::MemberType::Relation;
        let mut layer = layer_with_server_relation();
        let child = layer.create_relation(vec![], vec![]).unwrap();
        let parent = layer
            .create_relation(vec![], vec![relation_member(Relation, child)])
            .unwrap();
        assert_eq!(layer.relations_containing_relation(child), vec![parent]);
        assert!(layer.delete_relation(child).is_err());
        assert!(layer.relation(child).is_some());
        assert!(layer.delete_relation(404).is_err());

        // A server relation: delete, restore at the same position, version
        // intact, and nothing left to upload.
        let before = layer.relations();
        let snapshot = layer.delete_relation(500).unwrap();
        assert_eq!(snapshot.relation.version, 7);
        assert_eq!(snapshot.index, 0);
        assert_eq!(layer.diff_for_upload().deleted_relation_ids, vec![(500, 7)]);
        assert!(layer.restore_relation(snapshot.clone()).is_ok());
        assert!(
            layer.restore_relation(snapshot).is_err(),
            "id already present"
        );
        assert_eq!(layer.relations(), before);
        assert!(layer.diff_for_upload().deleted_relation_ids.is_empty());

        // Once the container is gone the member can go too, and redo of the
        // create (restore) brings back the same ids.
        let parent_snapshot = layer.delete_relation(parent).unwrap();
        let child_snapshot = layer.delete_relation(child).unwrap();
        assert!(layer.diff_for_upload().created_relations.is_empty());
        assert!(layer.restore_relation(child_snapshot).is_ok());
        assert!(layer.restore_relation(parent_snapshot).is_ok());
        assert_eq!(layer.relations(), before);
    }

    #[test]
    fn created_relations_are_remapped_inside_other_relations_after_upload() {
        use crate::osm::MemberType::{Node, Relation, Way};
        let mut layer = layer_with_server_relation();
        let n = layer.add_node(40.0, -74.0);
        let w = layer.add_way(vec![n], vec![]);
        let child = layer
            .create_relation(vec![], vec![relation_member(Node, n)])
            .unwrap();
        let parent = layer
            .create_relation(
                vec![("type".to_string(), "route".to_string())],
                vec![relation_member(Relation, child), relation_member(Way, w)],
            )
            .unwrap();
        // The server relation also gains the new parent as a member.
        let mut members = layer.relation(500).unwrap().members;
        members.push(relation_member(Relation, parent));
        layer.set_relation_members(500, members).unwrap();

        let diff = layer.diff_for_upload();
        assert_eq!(diff.counts(), (4, 1, 0));
        let xml = crate::osm_upload::build_osm_change_xml(5, &[("L", diff)]);
        assert!(xml.find("<relation id=\"-2\"") < xml.find("<relation id=\"-3\""));
        assert!(xml.find("<relation id=\"-3\"") < xml.find("<modify>"));

        let mut result = crate::osm_upload::UploadResult::default();
        result.node_id_remap.insert(n, (900, 1));
        result.way_id_remap.insert(w, (800, 1));
        result.relation_id_remap.insert(child, (701, 1));
        result.relation_id_remap.insert(parent, (702, 1));
        result.relation_id_remap.insert(500, (500, 8));
        layer.apply_upload_result(&result);

        let summary: Vec<_> = layer
            .relations()
            .iter()
            .map(|r| {
                (
                    r.id,
                    r.version,
                    r.members
                        .iter()
                        .map(|m| (m.member_type, m.reference))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (500, 8, vec![(Way, 10), (Relation, 702)]),
                (701, 1, vec![(Node, 900)]),
                (702, 1, vec![(Relation, 701), (Way, 800)]),
            ]
        );
        assert!(!layer.is_modified());
        assert!(layer.diff_for_upload().is_empty());
    }

    #[test]
    fn deleted_relation_drops_out_of_the_baseline_after_upload() {
        let mut layer = layer_with_server_relation();
        layer.delete_relation(500).unwrap();
        assert_eq!(layer.diff_for_upload().counts(), (0, 0, 1));
        // The server confirms a delete with no new_id, so no remap entry.
        layer.apply_upload_result(&crate::osm_upload::UploadResult::default());
        assert!(layer.diff_for_upload().is_empty());
        assert!(layer.relation(500).is_none());
    }

    #[test]
    fn merge_keeps_a_locally_created_relation_and_a_locally_deleted_one() {
        use crate::osm::{MemberType::Way, OsmData, OsmRelation};
        let mut layer = layer_with_server_relation();
        let created = layer
            .create_relation(vec![], vec![relation_member(Way, 10)])
            .unwrap();
        layer.delete_relation(500).unwrap();

        // The server still has 500 (at a newer version) plus an unrelated 501.
        let server = |id, version| OsmRelation {
            id,
            version,
            tags: empty_tags(),
            members: vec![relation_member(Way, 10)],
        };
        let incoming = OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: vec![server(500, 9), server(501, 1)],
            bounds: Vec::new(),
        };
        let stats = layer.merge_download(incoming);
        assert_eq!(
            stats.kept_local, 1,
            "the deleted relation 500 is kept deleted"
        );
        assert!(layer.relation(500).is_none());
        assert!(layer.relation(501).is_some());
        assert!(layer.relation(created).is_some());
        let diff = layer.diff_for_upload();
        assert_eq!(diff.created_relations.len(), 1);
        // Still deleted, against the version the layer last knew.
        assert_eq!(diff.deleted_relation_ids, vec![(500, 7)]);
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

        layer.remove_node(id).unwrap();

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

        layer.remove_way(way_id).unwrap();

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

        layer.extend_way(way_id, n3).unwrap();

        let data = layer.get_osm_data().unwrap();
        let way = data.ways.get(&way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, n2, n3]);
    }

    #[test]
    fn reverse_way_flips_node_order_and_is_its_own_inverse() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let n3 = layer.add_node(40.001, -74.0);
        let way_id = layer.add_way(vec![n1, n2, n3], vec![]);

        assert!(layer.reverse_way(way_id).is_ok());
        assert_eq!(layer.way_node_ids(way_id), Some(vec![n3, n2, n1]));
        assert!(layer.is_modified());

        assert!(layer.reverse_way(way_id).is_ok());
        assert_eq!(layer.way_node_ids(way_id), Some(vec![n1, n2, n3]));

        assert!(layer.reverse_way(999).is_err());
    }

    #[test]
    fn set_way_nodes_rewrites_the_list_and_tracks_node_usage() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let n3 = layer.add_node(40.001, -74.0);
        let w1 = layer.add_way(vec![n1, n2], vec![]);
        let w2 = layer.add_way(vec![n2, n3], vec![]);
        assert_eq!(layer.ways_using_node(n2), vec![w2, w1]);

        // Swap w2's use of n2 for a fresh node at the same spot.
        let copy = layer.add_node(40.0, -74.0);
        assert!(layer.set_way_nodes(w2, vec![copy, n3]).is_ok());
        assert_eq!(layer.way_node_ids(w2), Some(vec![copy, n3]));
        assert_eq!(layer.ways_using_node(n2), vec![w1]);
        assert_eq!(layer.ways_using_node(copy), vec![w2]);

        // A node no way uses any more can be deleted again.
        assert!(layer.set_way_nodes(w2, vec![n3, n1]).is_ok());
        assert!(layer.delete_feature(FeatureKind::Node, copy).is_ok());
        assert!(layer.set_way_nodes(999, vec![]).is_err());
    }

    #[test]
    fn relations_containing_way_reads_member_refs() {
        let node = |id| OsmNode {
            id,
            lat: 40.0,
            lon: -74.0 + id as f64 * 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = |id, nodes: Vec<i64>| OsmWay {
            id,
            nodes,
            version: 1,
            tags: empty_tags(),
        };
        let mut data = (*data_with(
            vec![node(1), node(2), node(3)],
            vec![way(10, vec![1, 2]), way(11, vec![2, 3])],
        ))
        .clone();
        data.relations.push(crate::osm::OsmRelation {
            id: 500,
            members: vec![
                crate::osm::OsmMember {
                    member_type: crate::osm::MemberType::Way,
                    reference: 11,
                    role: String::new(),
                },
                // A node with the same numeric id is not the way.
                crate::osm::OsmMember {
                    member_type: crate::osm::MemberType::Node,
                    reference: 10,
                    role: String::new(),
                },
            ],
            version: 1,
            tags: HashMap::new(),
        });
        let layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data));
        assert_eq!(layer.relations_containing_way(11), vec![500]);
        assert!(layer.relations_containing_way(10).is_empty());
        // Node membership is by node ref, not way ref.
        assert_eq!(layer.relations_containing_node(10), vec![500]);
        assert!(layer.relations_containing_node(11).is_empty());
    }

    #[test]
    fn insert_node_into_way_splices_at_index_and_remove_node_from_way_undoes_it() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);

        let mid = layer
            .insert_node_into_way(way_id, 1, 40.0, -74.0005)
            .unwrap();
        let data = layer.get_osm_data().unwrap();
        let way = data.ways.get(&way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, mid, n2]);

        layer.remove_node_from_way(way_id, 1).unwrap();
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
            nodes: vec![1, 2, 2, 1],
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

    mod batching {
        use super::super::{batch_index, finish_sorted, z_key, Batch, BatchKind};
        use gpui::{point, px};
        use std::collections::{HashMap, HashSet};

        const KINDS: [BatchKind; 4] = [
            BatchKind::Fill,
            BatchKind::Overlay,
            BatchKind::Casing,
            BatchKind::Stroke,
        ];

        fn with_triangle(mut b: Batch) -> Batch {
            super::super::push_triangle(
                &mut b.path,
                &mut b.bounds,
                point(px(0.0), px(0.0)),
                point(px(1.0), px(0.0)),
                point(px(0.0), px(1.0)),
            );
            b
        }

        #[test]
        fn z_key_orders_fractional_and_negative_indices() {
            let zs = [-2.0f32, -0.5, -0.125, 0.0, 0.125, 0.5, 1.0, 1.5, 10.0];
            let keys: Vec<i32> = zs.iter().map(|&z| z_key(z)).collect();
            assert!(keys.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
            assert_eq!(z_key(0.5), 4);
            assert_eq!(z_key(-0.5), -4);
            assert_eq!(z_key(0.0), 0);
        }

        #[test]
        fn z_key_clamps_extremes() {
            assert_eq!(z_key(f32::MAX), z_key(1.0e6));
            assert_eq!(z_key(f32::MIN), z_key(-1.0e6));
            assert_eq!(z_key(f32::INFINITY), z_key(1.0e6));
            assert_eq!(z_key(f32::NEG_INFINITY), z_key(-1.0e6));
        }

        #[test]
        fn batch_keys_do_not_collide() {
            let hints = HashMap::new();
            let colours = [
                0x0000_0000u32,
                0x0000_00ff,
                0x0000_ff00,
                0x00ff_0000,
                0xff00_0000,
                0xffff_ffff,
                0x1234_5678,
                0x1234_56ff,
                0x1234_5600,
            ];
            let zs = [-8_000_000, -1000, -8, -1, 0, 1, 8, 1000, 8_000_000];
            let mut seen = HashSet::new();
            let mut n = 0;
            for kind in KINDS {
                for &rgba in &colours {
                    for &z in &zs {
                        seen.insert(Batch::new(kind, rgba, z, &hints).key);
                        n += 1;
                    }
                }
            }
            assert_eq!(seen.len(), n);
        }

        #[test]
        fn batch_index_reuses_by_z_and_colour_only() {
            let hints = HashMap::new();
            let mut index = HashMap::new();
            let mut batches = Vec::new();
            let mut idx =
                |z, rgba| batch_index(&mut index, &mut batches, BatchKind::Stroke, z, rgba, &hints);
            let a = idx(0, 0xff);
            assert_eq!(idx(0, 0xff), a);
            assert_ne!(idx(1, 0xff), a);
            assert_ne!(idx(0, 0xfe), a);
            assert_eq!(batches.len(), 3);
        }

        #[test]
        fn finish_sorted_orders_by_z_then_first_seen() {
            let hints = HashMap::new();
            let mut index = HashMap::new();
            let mut batches = Vec::new();
            // First-seen order: (z 8, A), (z -4, B), (z 8, C), (z -4, D).
            for (z, rgba) in [(8, 0xA), (-4, 0xB), (8, 0xC), (-4, 0xD)] {
                batch_index(&mut index, &mut batches, BatchKind::Fill, z, rgba, &hints);
            }
            let batches: Vec<Batch> = batches.into_iter().map(with_triangle).collect();
            let mut next = HashMap::new();
            let out = finish_sorted(batches, &mut next);
            let order: Vec<u32> = out.iter().map(|(c, _)| *c).collect();
            assert_eq!(order, [0xB, 0xD, 0xA, 0xC]);
            assert_eq!(next.len(), 4);
        }

        #[test]
        fn finish_sorted_drops_empty_batches() {
            let hints = HashMap::new();
            let empty = Batch::new(BatchKind::Fill, 1, 0, &hints);
            let full = with_triangle(Batch::new(BatchKind::Fill, 2, 0, &hints));
            let mut next = HashMap::new();
            let out = finish_sorted(vec![empty, full], &mut next);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].0, 2);
        }
    }

    // --- Upload conflicts, end to end against the fake HTTP client --------

    mod conflicts {
        use super::*;
        use crate::http::fake::{ok, status_err, FakeClient};
        use crate::osm_api::{fetch_element_with, ServerElement};
        use crate::osm_upload::{run_upload, ConflictKind, UploadConflict, UploadError};

        const BASE: &str = "https://osm.test";
        const MISMATCH: &str = "Version mismatch: Provided 1, server had: 2 of Node 1";

        fn node(id: i64, lat: f64, version: i32) -> OsmNode {
            OsmNode {
                id,
                lat,
                lon: -74.0,
                version,
                tags: empty_tags(),
            }
        }

        /// A layer with node 1 (v1) and way 10 (v1) over nodes 1, 2; node 1 is
        /// edited locally (tagged).
        fn edited_layer() -> OsmLayer {
            let way = OsmWay {
                id: 10,
                nodes: vec![1, 2],
                version: 1,
                tags: empty_tags(),
            };
            let data = data_with(vec![node(1, 40.0, 1), node(2, 40.001, 1)], vec![way]);
            let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
            layer
                .set_tag(FeatureKind::Node, 1, "amenity", "cafe")
                .unwrap();
            layer
        }

        /// Upload `layer`'s diff against `client`; the conflict it is refused with.
        fn upload(layer: &OsmLayer, client: &FakeClient) -> Result<(), UploadConflict> {
            let diff = layer.diff_for_upload();
            match run_upload(client, BASE, "tok", "edit", &[("L", diff)], &mut |_| {}) {
                Ok(_) => Ok(()),
                Err(f) => match f.error {
                    UploadError::Conflict(c) => Err(c),
                    other => panic!("unexpected {other:?}"),
                },
            }
        }

        /// The osmChange body of the second request (the upload) `client` saw.
        fn sent_xml(client: &FakeClient, upload_index: usize) -> String {
            let requests = client.requests.lock().unwrap();
            let uploads: Vec<_> = requests
                .iter()
                .filter(|r| r.url.ends_with("/upload"))
                .collect();
            match &uploads[upload_index].body {
                Some(crate::http::Body::Text(xml)) => xml.clone(),
                other => panic!("no xml body: {other:?}"),
            }
        }

        fn refused_session() -> Vec<Result<crate::http::HttpResponse, crate::http::HttpError>> {
            vec![ok(200, "42"), status_err(409, MISMATCH), ok(200, "")]
        }

        fn server_node_xml(version: i32, lat: f64) -> String {
            format!(
                r#"<osm version="0.6"><node id="1" visible="true" version="{version}" lat="{lat}" lon="-74.0"><tag k="shop" v="bakery"/></node></osm>"#
            )
        }

        #[test]
        fn keep_mine_makes_the_retry_overwrite_the_servers_version() {
            let mut layer = edited_layer();
            let client = FakeClient::new(refused_session());
            let conflict = upload(&layer, &client).unwrap_err();
            assert_eq!(
                conflict.kind,
                ConflictKind::VersionMismatch {
                    provided: 1,
                    server: 2
                }
            );
            assert!(sent_xml(&client, 0).contains(r#"version="1" changeset="42""#));

            // Fetch the server's version number and adopt it as the base.
            let (kind, id) = conflict.object.unwrap();
            let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.0))]);
            let server = fetch_element_with(&fetch, BASE, None, kind, id, false).unwrap();
            let version = server.version(kind, id).unwrap();
            assert!(EditableLayer::set_base_version(
                &mut layer, kind, id, version
            ));

            // The edit is still there, now against version 2.
            assert_eq!(
                layer.feature_tags(&FeatureRef {
                    layer_id: LayerId(1),
                    kind,
                    id
                }),
                Some(vec![("amenity".to_string(), "cafe".to_string())])
            );
            let retry = FakeClient::new(vec![
                ok(200, "43"),
                ok(
                    200,
                    r#"<diffResult><node old_id="1" new_id="1" new_version="3"/></diffResult>"#,
                ),
                ok(200, ""),
            ]);
            upload(&layer, &retry).unwrap();
            let xml = sent_xml(&retry, 0);
            assert!(
                xml.contains(r#"<node id="1" lat="40" lon="-74" version="2" changeset="43">"#),
                "{xml}"
            );
            assert!(xml.contains(r#"<tag k="amenity" v="cafe"/>"#), "{xml}");
        }

        #[test]
        fn use_theirs_replaces_the_object_and_the_retry_leaves_it_out() {
            let mut layer = edited_layer();
            // A second, unrelated edit that must survive.
            layer
                .set_tag(FeatureKind::Way, 10, "highway", "path")
                .unwrap();
            let client = FakeClient::new(refused_session());
            let conflict = upload(&layer, &client).unwrap_err();
            let (kind, id) = conflict.object.unwrap();

            let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.5))]);
            let server = fetch_element_with(&fetch, BASE, None, kind, id, false).unwrap();
            assert!(EditableLayer::adopt_server_element(
                &mut layer, kind, id, server
            ));

            let tags = layer
                .feature_tags(&FeatureRef {
                    layer_id: LayerId(1),
                    kind,
                    id,
                })
                .unwrap();
            assert_eq!(tags, vec![("shop".to_string(), "bakery".to_string())]);
            assert_eq!(layer.node_lat_lon(1), Some((40.5, -74.0)));

            let retry = FakeClient::new(vec![
                ok(200, "43"),
                ok(
                    200,
                    r#"<diffResult><way old_id="10" new_id="10" new_version="2"/></diffResult>"#,
                ),
                ok(200, ""),
            ]);
            upload(&layer, &retry).unwrap();
            let xml = sent_xml(&retry, 0);
            assert!(!xml.contains(r#"<node id="1""#), "node 1 dropped: {xml}");
            assert!(xml.contains(r#"<way id="10" version="1""#), "{xml}");
            assert!(layer.is_modified());
        }

        #[test]
        fn use_theirs_that_leaves_nothing_to_upload_clears_the_modified_flag() {
            let mut layer = edited_layer();
            assert!(layer.is_modified());
            let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.0))]);
            let server =
                fetch_element_with(&fetch, BASE, None, FeatureKind::Node, 1, false).unwrap();
            assert!(EditableLayer::adopt_server_element(
                &mut layer,
                FeatureKind::Node,
                1,
                server
            ));
            assert!(layer.diff_for_upload().is_empty());
            assert!(!layer.is_modified());
        }

        #[test]
        fn use_theirs_on_an_object_deleted_on_the_server_removes_it_locally() {
            let mut layer = edited_layer();
            // The way uses node 1, so edit the way instead: delete-on-server
            // for way 10 after a local tag edit.
            layer
                .set_tag(FeatureKind::Way, 10, "highway", "path")
                .unwrap();
            let client = FakeClient::new(vec![
                ok(200, "42"),
                status_err(410, "The way with the id 10 has already been deleted"),
                ok(200, ""),
            ]);
            let conflict = upload(&layer, &client).unwrap_err();
            assert_eq!(conflict.kind, ConflictKind::AlreadyDeleted);
            let (kind, id) = conflict.object.unwrap();
            assert_eq!((kind, id), (FeatureKind::Way, 10));

            let fetch = FakeClient::new(vec![status_err(410, "")]);
            let server = fetch_element_with(&fetch, BASE, None, kind, id, true).unwrap();
            assert_eq!(server, ServerElement::Gone);
            assert!(EditableLayer::adopt_server_element(
                &mut layer, kind, id, server
            ));
            assert!(!EditableLayer::has_feature(&layer, kind, id));
            assert!(layer.way_node_ids(10).is_none());

            let diff = layer.diff_for_upload();
            assert!(diff.modified_ways.is_empty() && diff.deleted_way_ids.is_empty());
            assert_eq!(diff.modified_nodes.len(), 1, "the node edit stays");
        }

        #[test]
        fn drop_my_change_forgets_a_delete_the_server_already_did() {
            let mut layer = edited_layer();
            // Delete way 10 locally.
            layer.delete_feature(FeatureKind::Way, 10).unwrap();
            assert!(EditableLayer::is_locally_deleted(
                &layer,
                FeatureKind::Way,
                10
            ));
            assert_eq!(layer.diff_for_upload().deleted_way_ids, vec![(10, 1)]);

            assert!(EditableLayer::forget_local_delete(
                &mut layer,
                FeatureKind::Way,
                10
            ));
            assert!(layer.diff_for_upload().deleted_way_ids.is_empty());
            assert!(!EditableLayer::is_locally_deleted(
                &layer,
                FeatureKind::Way,
                10
            ));
            // Nothing to forget for an object that is simply there.
            assert!(!EditableLayer::forget_local_delete(
                &mut layer,
                FeatureKind::Node,
                1
            ));
        }

        #[test]
        fn keep_mine_on_a_locally_deleted_object_updates_the_delete_version() {
            let mut layer = edited_layer();
            layer.delete_feature(FeatureKind::Way, 10).unwrap();
            assert!(EditableLayer::set_base_version(
                &mut layer,
                FeatureKind::Way,
                10,
                5
            ));
            assert_eq!(layer.diff_for_upload().deleted_way_ids, vec![(10, 5)]);
        }
    }

    // --- Chunked upload, end to end against the fake HTTP client ----------

    mod chunked {
        use super::*;
        use crate::http::fake::{ok, status_err, FakeClient};
        use crate::upload_chunks::run_chunked_upload;

        const BASE: &str = "https://osm.test";

        fn new_node(id: i64) -> OsmNode {
            OsmNode {
                id,
                lat: 40.0,
                lon: -74.0,
                version: 0,
                tags: empty_tags(),
            }
        }

        fn new_way(id: i64, nodes: &[i64]) -> OsmWay {
            OsmWay {
                id,
                nodes: nodes.to_vec(),
                version: 0,
                tags: empty_tags(),
            }
        }

        /// A layer whose only changes are 6 new nodes (-1..-6) and 2 new
        /// ways: -1 over -1,-2,-3 and -2 over -4,-5,-6. With a limit of 3 the
        /// plan is [n-1 n-2 n-3] [w-1 n-4 n-5] [n-6 w-2].
        fn layer_with_new_data() -> OsmLayer {
            let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
            layer.set_osm_data_for_test(data_with(
                (1..=6).map(|i| new_node(-i)).collect(),
                vec![new_way(-1, &[-1, -2, -3]), new_way(-2, &[-4, -5, -6])],
            ));
            layer
        }

        fn nodes_result(pairs: &[(i64, i64)]) -> String {
            let mut s = String::from("<diffResult>");
            for (old, new) in pairs {
                s.push_str(&format!(
                    r#"<node old_id="{old}" new_id="{new}" new_version="1"/>"#
                ));
            }
            s.push_str("</diffResult>");
            s
        }

        fn bodies(client: &FakeClient) -> Vec<String> {
            client
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.url.ends_with("/upload"))
                .map(|r| match &r.body {
                    Some(crate::http::Body::Text(x)) => x.clone(),
                    other => panic!("{other:?}"),
                })
                .collect()
        }

        fn upload(
            layer: &mut OsmLayer,
            client: &FakeClient,
            max: usize,
        ) -> crate::upload_chunks::ChunkedUpload {
            let layers = [("L", layer.diff_for_upload())];
            // Each accepted chunk is applied as it lands, as the app does.
            run_chunked_upload(
                client,
                BASE,
                "tok",
                "edit",
                &layers,
                max,
                &mut |_, _| {},
                &mut |done| crate::upload_chunks::apply_to_layer(layer, done),
            )
            .unwrap()
        }

        #[test]
        fn a_failure_in_the_middle_leaves_exactly_the_unsent_changes_pending() {
            let mut layer = layer_with_new_data();
            assert_eq!(layer.diff_for_upload().counts(), (8, 0, 0));
            let client = FakeClient::new(vec![
                ok(200, "101"),
                ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
                ok(200, ""),
                ok(200, "102"),
                status_err(412, "Way -1 requires the nodes with id in (1), which either do not exist, or are not visible."),
                ok(200, ""),
            ]);
            let out = upload(&mut layer, &client, 3);
            assert_eq!(out.total, 3);
            assert_eq!(out.completed.len(), 1);
            assert!(out.failure.is_some());

            // Chunk 1 is on the server and reflected locally: real ids, v1.
            let data = layer.get_osm_data().unwrap();
            for id in [5001, 5002, 5003] {
                assert_eq!(data.nodes[&id].version, 1);
            }
            assert!(!data.nodes.contains_key(&-1));
            // Way -1 (in chunk 2) now names the real nodes.
            assert_eq!(data.ways[&-1].nodes, vec![5001, 5002, 5003]);

            // What is pending is exactly chunks 2 and 3.
            assert!(layer.is_modified());
            let rest = layer.diff_for_upload();
            let mut nodes: Vec<i64> = rest.created_nodes.iter().map(|n| n.id).collect();
            nodes.sort();
            assert_eq!(nodes, vec![-6, -5, -4]);
            let mut ways: Vec<i64> = rest.created_ways.iter().map(|w| w.id).collect();
            ways.sort();
            assert_eq!(ways, vec![-2, -1]);
            assert_eq!(rest.counts(), (5, 0, 0));
            assert!(rest.modified_nodes.is_empty() && rest.deleted_node_ids.is_empty());
        }

        /// Records the start of each changeset in `log`.
        struct Logging {
            inner: FakeClient,
            log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl crate::http::HttpClient for Logging {
            fn request(
                &self,
                req: crate::http::HttpRequest,
            ) -> Result<crate::http::HttpResponse, crate::http::HttpError> {
                if req.url.ends_with("/create") {
                    self.log.lock().unwrap().push("create".to_string());
                }
                self.inner.request(req)
            }
        }

        #[test]
        fn the_layer_has_server_ids_for_chunk_one_before_chunk_two_is_opened() {
            let mut layer = layer_with_new_data();
            let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let client = Logging {
                inner: FakeClient::new(vec![
                    ok(200, "101"),
                    ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
                    ok(200, ""),
                    // Chunk 2 never gets as far as an upload: the sequence ends.
                    crate::http::fake::transport_err("connection reset"),
                ]),
                log: log.clone(),
            };
            let layers = [("L", layer.diff_for_upload())];
            let seen = log.clone();
            let out = run_chunked_upload(
                &client,
                BASE,
                "tok",
                "edit",
                &layers,
                3,
                &mut |_, _| {},
                &mut |done| {
                    crate::upload_chunks::apply_to_layer(&mut layer, done);
                    let mut ids: Vec<i64> = layer
                        .get_osm_data()
                        .unwrap()
                        .nodes
                        .keys()
                        .copied()
                        .filter(|&id| id > 0)
                        .collect();
                    ids.sort();
                    seen.lock().unwrap().push(format!("layer has {ids:?}"));
                },
            )
            .unwrap();
            assert_eq!(
                *log.lock().unwrap(),
                vec!["create", "layer has [5001, 5002, 5003]", "create"]
            );

            // The sequence stopped right after chunk 1 (as if dropped): what
            // is pending is exactly chunks 2 and 3, so another upload would
            // not create chunk 1's objects again.
            assert_eq!(out.completed.len(), 1);
            let rest = layer.diff_for_upload();
            assert_eq!(rest.counts(), (5, 0, 0));
            assert!(rest.created_nodes.iter().all(|n| n.id < 0));
            assert!(layer.is_modified());
        }

        #[test]
        fn pressing_upload_again_sends_only_the_remainder_and_finishes_clean() {
            let mut layer = layer_with_new_data();
            let client = FakeClient::new(vec![
                ok(200, "101"),
                ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
                ok(200, ""),
                ok(200, "102"),
                status_err(409, "The changeset 102 was closed at now"),
                ok(200, ""),
            ]);
            upload(&mut layer, &client, 3);

            // Second attempt: 5 elements left -> [w-1 n-4 n-5] [n-6 w-2].
            let retry = FakeClient::new(vec![
                ok(200, "201"),
                ok(
                    200,
                    r#"<diffResult><node old_id="-4" new_id="5004" new_version="1"/><node old_id="-5" new_id="5005" new_version="1"/><way old_id="-1" new_id="6001" new_version="1"/></diffResult>"#,
                ),
                ok(200, ""),
                ok(200, "202"),
                ok(
                    200,
                    r#"<diffResult><node old_id="-6" new_id="5006" new_version="1"/><way old_id="-2" new_id="6002" new_version="1"/></diffResult>"#,
                ),
                ok(200, ""),
            ]);
            let out = upload(&mut layer, &retry, 3);
            assert!(out.failure.is_none());
            assert_eq!(out.completed.len(), 2);

            let sent = bodies(&retry);
            assert_eq!(sent.len(), 2);
            // Nothing from chunk 1 is sent again; way -1 uses its server ids.
            assert!(!sent.iter().any(|b| b.contains(r#"id="5001""#)));
            assert!(sent[0].contains(r#"<nd ref="5001"/><nd ref="5002"/><nd ref="5003"/>"#));
            // Way -2: nodes 5004/5005 from the earlier chunk, -6 its own.
            assert!(sent[1].contains(r#"<nd ref="5004"/><nd ref="5005"/><nd ref="-6"/>"#));

            // Everything is on the server and nothing is pending.
            assert!(!layer.is_modified());
            assert!(layer.diff_for_upload().is_empty());
            let data = layer.get_osm_data().unwrap();
            assert_eq!(data.nodes.len(), 6);
            assert_eq!(data.ways[&6002].nodes, vec![5004, 5005, 5006]);
            assert!(data.nodes.keys().all(|&id| id > 0));
        }

        #[test]
        fn modified_and_deleted_objects_are_applied_per_chunk_too() {
            // Existing nodes 1, 2 (edited), 3 (deleted); limit 2 => [m1 m2] [d3].
            let mut layer = OsmLayer::new_with_data(
                LayerId(1),
                "L",
                data_with(
                    vec![
                        OsmNode {
                            version: 1,
                            ..new_node(1)
                        },
                        OsmNode {
                            version: 1,
                            ..new_node(2)
                        },
                        OsmNode {
                            version: 1,
                            ..new_node(3)
                        },
                    ],
                    vec![],
                ),
            );
            layer.set_tag(FeatureKind::Node, 1, "a", "b").unwrap();
            layer.set_tag(FeatureKind::Node, 2, "a", "b").unwrap();
            layer.delete_feature(FeatureKind::Node, 3).unwrap();
            let client = FakeClient::new(vec![
                ok(200, "1"),
                ok(
                    200,
                    r#"<diffResult><node old_id="1" new_id="1" new_version="2"/><node old_id="2" new_id="2" new_version="2"/></diffResult>"#,
                ),
                ok(200, ""),
                ok(200, "2"),
                status_err(409, "Node 3 is still used by way 9"),
                ok(200, ""),
            ]);
            upload(&mut layer, &client, 2);
            let data = layer.get_osm_data().unwrap();
            assert_eq!(data.nodes[&1].version, 2);
            assert_eq!(data.nodes[&2].version, 2);
            let rest = layer.diff_for_upload();
            assert_eq!(rest.counts(), (0, 0, 1));
            assert_eq!(rest.deleted_node_ids, vec![(3, 1)]);
            assert!(layer.is_modified());
        }
    }

    // ---- node marker zoom rule ----

    fn tagged() -> HashMap<String, String> {
        let mut tags = HashMap::new();
        tags.insert("amenity".to_string(), "cafe".to_string());
        tags
    }

    /// One untagged way of `n` nodes 0.0001 degrees (about 8 m) apart along
    /// latitude 40, plus a tagged node (id 1000) beside the middle one and an
    /// untagged node on no way (id 2000) ten steps further along.
    fn way_with_poi_layer(n: i64) -> OsmLayer {
        let mut nodes: Vec<OsmNode> = (1..=n)
            .map(|i| OsmNode {
                id: i,
                lat: 40.0,
                lon: -74.0 + i as f64 * 0.0001,
                version: 1,
                tags: empty_tags(),
            })
            .collect();
        nodes.push(OsmNode {
            id: 1000,
            lat: 40.0003,
            lon: -74.0 + (n / 2) as f64 * 0.0001,
            version: 1,
            tags: tagged(),
        });
        nodes.push(OsmNode {
            id: 2000,
            lat: 40.0003,
            lon: -74.0 + (n / 2 + 10) as f64 * 0.0001,
            version: 1,
            tags: empty_tags(),
        });
        let way = OsmWay {
            id: 100,
            nodes: (1..=n).collect(),
            version: 1,
            tags: HashMap::new(),
        };
        OsmLayer::new_with_data(LayerId(1), "L", data_with(nodes, vec![way]))
    }

    fn view_at(zoom: f64) -> Viewport {
        Viewport::new(
            40.0,
            -74.0 + 30.0 * 0.0001,
            zoom,
            size(px(800.0), px(600.0)),
        )
    }

    fn node_point(layer: &OsmLayer, vp: &Viewport, id: i64) -> gpui::Point<gpui::Pixels> {
        let idx = layer.node_cache.index_by_id[&id];
        let (_, mx, my) = layer.node_cache.flat[idx];
        vp.mercator_to_screen(mx, my)
    }

    #[test]
    fn hidden_vertex_is_not_a_target_and_the_way_is_hit() {
        let layer = way_with_poi_layer(60);
        let vp = view_at(16.0);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "{hits:?}");
    }

    #[test]
    fn unselected_vertex_is_never_a_target_even_at_high_zoom() {
        let layer = way_with_poi_layer(60);
        for z in [17.0, node_markers::FULL_MIN_ZOOM, 20.0] {
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
            assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
        }
    }

    #[test]
    fn vertex_of_a_selected_way_is_clickable_at_high_zoom() {
        let mut layer = way_with_poi_layer(60);
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 100,
        }]);
        for z in [18.0, 20.0] {
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
            assert_eq!(hits[0].kind, FeatureKind::Node, "z{z}");
            assert_eq!(hits[0].feature.id, 30, "z{z}");
        }
    }

    #[test]
    fn standalone_untagged_node_is_a_target_like_a_tagged_one() {
        let layer = way_with_poi_layer(60);
        for z in [12.0, 16.0] {
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 2000));
            assert!(hits.iter().all(|h| h.feature.id != 2000), "z{z}");
        }
        for z in [17.0, 18.0, 20.0] {
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 2000));
            assert_eq!(hits[0].feature.id, 2000, "z{z}");
        }
    }

    #[test]
    fn add_mode_snaps_to_an_undrawn_vertex() {
        let layer = way_with_poi_layer(60);
        for z in [14.0, 17.0, 19.0] {
            let vp = view_at(z);
            let near = node_point(&layer, &vp, 30);
            // Not a hit target, but a snap target.
            let hits = layer.hit_test(&vp, near);
            assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
            assert_eq!(layer.snap_to_node(&vp, near, 8.0), Some(30), "z{z}");
        }
        // Far from any node: nothing to snap to.
        let vp = view_at(19.0);
        let mut far = node_point(&layer, &vp, 30);
        far.y += px(100.0);
        assert_eq!(layer.snap_to_node(&vp, far, 8.0), None);
    }

    #[test]
    fn simple_tagged_node_is_a_target_at_low_zoom() {
        let layer = way_with_poi_layer(60);
        for z in [12.0, 16.0] {
            // Not drawn, so not a target.
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 1000));
            assert!(hits.iter().all(|h| h.feature.id != 1000), "z{z}");
        }
        for z in [node_markers::TAGGED_MIN_ZOOM, 17.5] {
            let vp = view_at(z);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 1000));
            assert_eq!(hits[0].kind, FeatureKind::Node, "z{z}");
            assert_eq!(hits[0].feature.id, 1000);
            // Just outside the simple radius it is not hit.
            let mut away = node_point(&layer, &vp, 1000);
            away.x += px(node_markers::SIMPLE_HIT_RADIUS + 1.0);
            let hits = layer.hit_test(&vp, away);
            assert!(hits.iter().all(|h| h.feature.id != 1000), "z{z}");
        }
    }

    #[test]
    fn nodes_of_a_selected_way_and_selected_nodes_are_targets_at_low_zoom() {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(16.0);
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 100,
        }]);
        // Vertices are about a pixel apart here: 30 is among the node hits.
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Node));
        assert!(hits.iter().any(|h| h.feature.id == 30));

        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 20,
        }]);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 20));
        assert_eq!(hits[0].feature.id, 20);
        assert_eq!(hits[0].kind, FeatureKind::Node);
    }

    #[test]
    fn box_select_and_hit_test_agree_on_which_nodes_are_shown() {
        let layer = way_with_poi_layer(60);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        for z in [14.0, 16.0, 17.9, 18.0, 19.0] {
            let vp = view_at(z);
            let boxed: HashSet<i64> = layer
                .hit_test_rect(&vp, rect)
                .into_iter()
                .filter(|f| f.kind == FeatureKind::Node)
                .map(|f| f.id)
                .collect();
            for id in (1..=60).chain([1000, 2000]) {
                let sp = node_point(&layer, &vp, id);
                let in_view = sp.x >= px(0.0) && sp.x <= px(800.0) && sp.y >= px(0.0);
                let in_view = in_view && sp.y <= px(600.0);
                let clicked = layer
                    .hit_test(&vp, sp)
                    .iter()
                    .any(|h| h.kind == FeatureKind::Node && h.feature.id == id);
                if in_view && boxed.contains(&id) {
                    assert!(clicked, "z{z}: node {id} boxed but not clickable");
                }
                if id >= 1000 {
                    assert_eq!(
                        boxed.contains(&id),
                        z >= node_markers::TAGGED_MIN_ZOOM,
                        "z{z}: tagged/standalone node {id} boxed only from its minimum zoom"
                    );
                } else {
                    assert!(!boxed.contains(&id), "z{z}: undrawn vertex {id} boxed");
                }
            }
        }
    }

    #[test]
    fn marker_form_follows_the_view_in_the_layer() {
        let layer = way_with_poi_layer(5);
        let pins = layer.pins();
        let vertex = layer.node_cache.index_by_id[&3];
        let poi = layer.node_cache.index_by_id[&1000];
        assert_eq!(layer.node_form_at(&pins, vertex, 3, 17.0), NodeForm::Hidden);
        assert_eq!(layer.node_form_at(&pins, vertex, 3, 18.5), NodeForm::Hidden);
        assert_eq!(
            layer.node_form_at(&pins, poi, 1000, 17.0),
            NodeForm::Simple(node_markers::SIMPLE_SIZE)
        );
        let stray = layer.node_cache.index_by_id[&2000];
        assert_eq!(
            layer.node_form_at(&pins, stray, 2000, 17.0),
            NodeForm::Simple(node_markers::SIMPLE_SIZE)
        );
        assert_eq!(layer.node_form_at(&pins, stray, 2000, 18.5), NodeForm::Full);
    }

    #[test]
    fn a_stylesheet_with_square_and_cross_markers_shows_every_node_from_z16() {
        let mut layer = way_with_poi_layer(5);
        let wire = crate::style::custom::BUILT_IN_STYLES
            .iter()
            .find(|s| s.id == "wireframe")
            .unwrap();
        layer.set_stylesheet(Arc::new(Stylesheet::parse(wire.source).unwrap()));
        let pins = layer.pins();
        let vertex = layer.node_cache.index_by_id[&3];
        let poi = layer.node_cache.index_by_id[&1000];
        let stray = layer.node_cache.index_by_id[&2000];
        let form = |idx, id, z| layer.node_form_at(&pins, idx, id, z);
        // Vertices are hidden when zoomed out, then a small square.
        assert_eq!(form(vertex, 3, 15.0), NodeForm::Hidden);
        let NodeForm::Styled(v) = form(vertex, 3, 16.0) else {
            panic!("vertex should be styled");
        };
        assert_eq!(v.shape, crate::style::mapcss::NodeShape::Square);
        // Tagged and free-standing nodes are crosses.
        let NodeForm::Styled(p) = form(poi, 1000, 16.0) else {
            panic!("poi should be styled");
        };
        assert_eq!(p.shape, crate::style::mapcss::NodeShape::Cross);
        let NodeForm::Styled(f) = form(stray, 2000, 19.0) else {
            panic!("free node should be styled");
        };
        assert_eq!(f.shape, crate::style::mapcss::NodeShape::Cross);
        assert!(NodeForm::Styled(v).hit_radius() > 0.0);
    }

    #[test]
    fn relation_membership_restyles_ways_when_a_relation_changes() {
        let mut layer = way_with_poi_layer(5);
        let wire = crate::style::custom::BUILT_IN_STYLES
            .iter()
            .find(|s| s.id == "wireframe")
            .unwrap();
        layer.set_stylesheet(Arc::new(Stylesheet::parse(wire.source).unwrap()));
        let idx = layer.way_id_to_index[&100];
        assert_eq!(layer.way_styles[idx].casing_width, 0.0);
        let members = vec![crate::osm::OsmMember {
            member_type: crate::osm::MemberType::Way,
            reference: 100,
            role: String::new(),
        }];
        let rel = layer.create_relation(vec![], members.clone()).unwrap();
        assert!(layer.way_styles[idx].casing_width > 0.0);
        let snapshot = layer.delete_relation(rel).unwrap();
        assert_eq!(layer.way_styles[idx].casing_width, 0.0);
        assert!(layer.restore_relation(snapshot).is_ok());
        assert!(layer.way_styles[idx].casing_width > 0.0);
        let rel2 = layer.relations()[0].id;
        layer.set_relation_members(rel2, vec![]).unwrap();
        assert_eq!(layer.way_styles[idx].casing_width, 0.0);
    }

    #[test]
    fn box_select_includes_vertices_of_a_selected_way() {
        let mut layer = way_with_poi_layer(60);
        layer.set_highlight(&[FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id: 100,
        }]);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        let vp = view_at(19.0);
        let boxed = layer.hit_test_rect(&vp, rect);
        assert!(boxed
            .iter()
            .any(|f| f.kind == FeatureKind::Node && f.id == 30));
    }

    fn way_ref(id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id,
        }
    }

    fn node_ref(id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id,
        }
    }

    /// Preview way 100 as a hover on its line would.
    fn preview_100(layer: &mut OsmLayer, vp: &Viewport) -> bool {
        let on_line = node_point(layer, vp, 30);
        layer.update_vertex_preview(vp, on_line, Some(&way_ref(100)), true)
    }

    #[test]
    fn preview_vertices_are_drawn_and_targets_from_the_preview_zoom_only() {
        for z in [16.0, 17.0, 17.99] {
            let mut layer = way_with_poi_layer(60);
            let vp = view_at(z);
            preview_100(&mut layer, &vp);
            assert_eq!(layer.vertex_preview_way(), None, "z{z}: no preview");
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
            assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
        }
        for z in [18.0, 19.0, 21.0] {
            let mut layer = way_with_poi_layer(60);
            let vp = view_at(z);
            assert!(preview_100(&mut layer, &vp), "z{z}");
            assert_eq!(layer.vertex_preview_way(), Some(100));
            let pins = layer.pins();
            let idx = layer.node_cache.index_by_id[&30];
            assert_eq!(layer.node_form_at(&pins, idx, 30, z), NodeForm::Preview);
            let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
            assert_eq!(hits.len(), 1, "z{z}");
            assert_eq!(hits[0].feature, node_ref(30), "z{z}");
        }
        // A preview left over from a higher zoom does not show or target
        // anything once the view is below the threshold.
        let mut layer = way_with_poi_layer(60);
        preview_100(&mut layer, &view_at(19.0));
        let low = view_at(17.0);
        let hits = layer.hit_test(&low, node_point(&layer, &low, 30));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
    }

    #[test]
    fn previewed_vertex_wins_over_the_line_and_is_hit_slightly_off_it() {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(19.0);
        let at = node_point(&layer, &vp, 30);
        // Without a preview the line is hit.
        assert_eq!(layer.hit_test(&vp, at)[0].kind, FeatureKind::Way);
        preview_100(&mut layer, &vp);
        let hits = layer.hit_test(&vp, at);
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Node));
        // Off the vertex but within its radius (and within the line's own
        // tolerance): still the node, not the way.
        let near = point(at.x + px(5.0), at.y + px(3.0));
        let hits = layer.hit_test(&vp, near);
        assert_eq!(hits[0].feature, node_ref(30));
        // Beyond the vertex radius but on the line: the way.
        let on_line = point(at.x + px(20.0), at.y);
        assert_eq!(layer.hit_test(&vp, on_line)[0].kind, FeatureKind::Way);
    }

    #[test]
    fn only_the_preview_ways_vertices_are_previewed() {
        // Ways 100 (nodes 1-5) and 200 (nodes 5-9) share node 5; way 300 is
        // apart.
        let mut nodes = Vec::new();
        let mut add = |id: i64, lat: f64, lon: f64| {
            nodes.push(OsmNode {
                id,
                lat,
                lon,
                version: 1,
                tags: empty_tags(),
            });
        };
        for i in 1..=5 {
            add(i, 40.0, -74.0 + i as f64 * 0.0001);
        }
        for i in 6..=9 {
            add(i, 40.0 + (i - 5) as f64 * 0.0001, -74.0 + 5.0 * 0.0001);
        }
        for i in 11..=15 {
            add(i, 40.002, -74.0 + (i - 10) as f64 * 0.0001);
        }
        let way = |id: i64, nodes: Vec<i64>| OsmWay {
            id,
            nodes,
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(
            nodes,
            vec![
                way(100, vec![1, 2, 3, 4, 5]),
                way(200, vec![5, 6, 7, 8, 9]),
                way(300, vec![11, 12, 13, 14, 15]),
            ],
        );
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
        let vp = Viewport::new(40.0003, -74.0003, 19.0, size(px(800.0), px(600.0)));
        let forms = |layer: &OsmLayer| -> Vec<i64> {
            let pins = layer.pins();
            (1..=15)
                .filter(|id| {
                    layer.node_cache.index_by_id.get(id).is_some_and(|&idx| {
                        layer.node_form_at(&pins, idx, *id, 19.0) == NodeForm::Preview
                    })
                })
                .collect()
        };
        assert!(forms(&layer).is_empty());
        let mid = node_point(&layer, &vp, 3);
        layer.update_vertex_preview(&vp, mid, Some(&way_ref(100)), true);
        assert_eq!(forms(&layer), vec![1, 2, 3, 4, 5]);
        // The shared node appears when either way is hovered.
        let mid = node_point(&layer, &vp, 7);
        layer.update_vertex_preview(&vp, mid, Some(&way_ref(200)), true);
        assert_eq!(forms(&layer), vec![5, 6, 7, 8, 9]);
        layer.update_vertex_preview(&vp, mid, None, false);
        assert!(forms(&layer).is_empty());
    }

    #[test]
    fn preview_hysteresis_and_switching() {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(19.0);
        preview_100(&mut layer, &vp);
        let at = node_point(&layer, &vp, 30);
        let off = |dy: f32| point(at.x + px(24.0), at.y + px(dy));
        // Hovering nothing, 10px off the line (past its 6px tolerance but
        // inside tolerance + vertex radius): still previewed.
        assert!(!layer.update_vertex_preview(&vp, off(10.0), None, true));
        assert_eq!(layer.vertex_preview_way(), Some(100));
        // Well away: dropped.
        assert!(layer.update_vertex_preview(&vp, off(16.0), None, true));
        assert_eq!(layer.vertex_preview_way(), None);
        // Hovering one of its vertices keeps the preview; a node that is
        // not one of its vertices drops it; another way replaces it.
        preview_100(&mut layer, &vp);
        layer.update_vertex_preview(&vp, at, Some(&node_ref(30)), true);
        assert_eq!(layer.vertex_preview_way(), Some(100));
        layer.update_vertex_preview(&vp, at, Some(&node_ref(1000)), true);
        assert_eq!(layer.vertex_preview_way(), None);
        preview_100(&mut layer, &vp);
        layer.update_vertex_preview(&vp, at, Some(&way_ref(999)), true);
        assert_eq!(layer.vertex_preview_way(), None);
        // Hovering a feature of another layer drops it too.
        preview_100(&mut layer, &vp);
        let foreign = FeatureRef {
            layer_id: LayerId(2),
            kind: FeatureKind::Way,
            id: 100,
        };
        layer.update_vertex_preview(&vp, at, Some(&foreign), true);
        assert_eq!(layer.vertex_preview_way(), None);
        // Disabled (other mode, dialog, imagery offset) clears it.
        preview_100(&mut layer, &vp);
        layer.update_vertex_preview(&vp, at, Some(&way_ref(100)), false);
        assert_eq!(layer.vertex_preview_way(), None);
    }

    #[test]
    fn box_select_counts_previewed_vertices_as_drawn_only_while_previewed() {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(19.0);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        let has_vertex = |layer: &OsmLayer| {
            layer
                .hit_test_rect(&vp, rect)
                .iter()
                .any(|f| f.kind == FeatureKind::Node && f.id == 30)
        };
        assert!(!has_vertex(&layer));
        preview_100(&mut layer, &vp);
        assert!(has_vertex(&layer));
        // The viewer clears the preview as soon as a box-select drag moves.
        let at = node_point(&layer, &vp, 30);
        layer.update_vertex_preview(&vp, at, None, false);
        assert!(!has_vertex(&layer));
    }
}

#[cfg(test)]
mod multipolygon_tests {
    use super::{MpState, OsmLayer};
    use crate::layers::{EditableLayer, LayerId};
    use crate::multipolygon::Problem;
    use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
    use crate::selection::{resolve_hits, FeatureKind};
    use crate::viewport::Viewport;
    use gpui::{point, px, size, Pixels, Point};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Node at grid column `c`, row `r`: 0.001 degrees apart from (40, -74).
    fn nid(c: i64, r: i64) -> i64 {
        1 + r * 100 + c
    }

    fn lat_lon(c: i64, r: i64) -> (f64, f64) {
        (40.0 + r as f64 * 0.001, -74.0 + c as f64 * 0.001)
    }

    /// A closed square of grid nodes, as a way's node list.
    fn square(c0: i64, r0: i64, c1: i64, r1: i64) -> Vec<i64> {
        vec![
            nid(c0, r0),
            nid(c1, r0),
            nid(c1, r1),
            nid(c0, r1),
            nid(c0, r0),
        ]
    }

    fn way(id: i64, nodes: Vec<i64>) -> OsmWay {
        OsmWay {
            id,
            nodes,
            version: 1,
            tags: HashMap::new(),
        }
    }

    fn member(way_id: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type: MemberType::Way,
            reference: way_id,
            role: role.to_string(),
        }
    }

    fn relation(id: i64, tags: &[(&str, &str)], members: Vec<OsmMember>) -> OsmRelation {
        OsmRelation {
            id,
            version: 1,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            members,
        }
    }

    const LAKE_TAGS: [(&str, &str); 2] = [("type", "multipolygon"), ("natural", "water")];

    /// A lake (way 10, 10x10 cells) with an island (way 11) as relation 500,
    /// plus a two-way outer (ways 20 and 21 making a square east of the
    /// lake) as relation 501, and a never-closing one (way 30 + 31) as 502.
    fn layer() -> OsmLayer {
        let mut nodes = HashMap::new();
        for r in 0..=12 {
            for c in 0..=40 {
                let (lat, lon) = lat_lon(c, r);
                nodes.insert(
                    nid(c, r),
                    OsmNode {
                        id: nid(c, r),
                        lat,
                        lon,
                        version: 1,
                        tags: HashMap::new(),
                    },
                );
            }
        }
        let mut ways = HashMap::new();
        for w in [
            way(10, square(0, 0, 10, 10)),
            way(11, square(3, 3, 6, 6)),
            // 15..=25 square split into a west and an east half.
            way(20, vec![nid(20, 0), nid(15, 0), nid(15, 10), nid(20, 10)]),
            way(21, vec![nid(20, 0), nid(25, 0), nid(25, 10), nid(20, 10)]),
            // Two halves of a ring that stops short of closing.
            way(30, vec![nid(30, 0), nid(35, 0), nid(35, 10)]),
            way(31, vec![nid(35, 10), nid(30, 10)]),
        ] {
            ways.insert(w.id, w);
        }
        let data = OsmData {
            nodes,
            ways,
            relations: vec![
                relation(
                    500,
                    &LAKE_TAGS,
                    vec![member(10, "outer"), member(11, "inner")],
                ),
                relation(
                    501,
                    &[("type", "multipolygon"), ("landuse", "forest")],
                    vec![member(20, "outer"), member(21, "outer")],
                ),
                relation(
                    502,
                    &[("type", "multipolygon"), ("landuse", "forest")],
                    vec![member(30, "outer"), member(31, "outer")],
                ),
            ],
            bounds: Vec::new(),
        };
        OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
    }

    fn viewport() -> Viewport {
        Viewport::new(40.006, -73.985, 14.0, size(px(800.0), px(600.0)))
    }

    fn at(viewport: &Viewport, c: f64, r: f64) -> Point<Pixels> {
        let p = viewport.geo_to_screen(40.0 + r * 0.001, -74.0 + c * 0.001);
        point(p.x, p.y)
    }

    /// What a double-click at grid position (c, r) would select.
    fn double_click(layer: &OsmLayer, c: f64, r: f64) -> Option<(FeatureKind, i64)> {
        let v = viewport();
        let p = at(&v, c, r);
        if !layer.hit_test(&v, p).is_empty() {
            return resolve_hits(vec![layer.hit_test(&v, p)]).map(|h| (h.kind, h.id));
        }
        resolve_hits(vec![layer.hit_test_interior(&v, p)]).map(|h| (h.kind, h.id))
    }

    fn valid(polygons: usize, holes: usize) -> MpState {
        MpState::Valid { polygons, holes }
    }

    #[test]
    fn relations_assemble_when_the_layer_loads() {
        let layer = layer();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        assert_eq!(layer.multipolygon_state(501), valid(1, 0));
        assert_eq!(
            layer.multipolygon_state(502),
            MpState::Invalid(Problem::Unclosed)
        );
        assert_eq!(layer.multipolygon_state(999), MpState::NotArea);
        // The lake style has a fill, so it is triangulated: 4 outer + 4 hole
        // vertices make 8 triangles.
        let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
        assert_eq!(tris, 8);
    }

    #[test]
    fn double_click_in_the_water_selects_the_relation_but_on_the_island_the_island() {
        let layer = layer();
        assert_eq!(
            double_click(&layer, 1.5, 8.5),
            Some((FeatureKind::Relation, 500))
        );
        // The island is a hole of the relation: the island way wins.
        assert_eq!(double_click(&layer, 4.5, 4.5), Some((FeatureKind::Way, 11)));
        assert_eq!(double_click(&layer, 11.5, 11.5), None);
    }

    #[test]
    fn a_two_way_outer_is_selectable_by_its_interior() {
        let layer = layer();
        assert_eq!(
            double_click(&layer, 17.5, 5.5),
            Some((FeatureKind::Relation, 501))
        );
        assert_eq!(
            double_click(&layer, 22.5, 5.5),
            Some((FeatureKind::Relation, 501))
        );
    }

    #[test]
    fn a_broken_relation_is_not_selectable_by_its_interior() {
        let layer = layer();
        assert_eq!(double_click(&layer, 32.5, 5.5), None);
    }

    #[test]
    fn an_untagged_old_style_relation_leaves_the_click_to_its_outer_way() {
        let mut layer = layer();
        layer
            .remove_tag(FeatureKind::Relation, 500, "natural")
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        assert_eq!(double_click(&layer, 1.5, 8.5), Some((FeatureKind::Way, 10)));
    }

    #[test]
    fn moving_a_member_node_rebuilds_only_the_relations_it_belongs_to() {
        let mut layer = layer();
        let before = layer.multipolygon_version();
        let (old, _) = layer.multipolygon_geometry(500, nid(10, 10)).unwrap();

        // A node of another relation's ring: this one's cache is untouched
        // but the version still counts the patch.
        layer
            .commit_node_moves(&[(nid(30, 0), 40.0, -73.969)])
            .unwrap();
        assert!(layer.multipolygon_version() > before);

        // A node no relation uses: no patch at all.
        let before = layer.multipolygon_version();
        layer
            .commit_node_moves(&[(nid(12, 12), 40.0121, -73.9881)])
            .unwrap();
        assert_eq!(layer.multipolygon_version(), before);

        layer
            .commit_node_moves(&[(nid(10, 10), 40.0105, -73.9895)])
            .unwrap();
        assert!(layer.multipolygon_version() > before);
        let (new, tris) = layer.multipolygon_geometry(500, nid(10, 10)).unwrap();
        assert_ne!(new, old);
        assert_eq!(tris, 8);
    }

    #[test]
    fn reshaping_splitting_and_removing_member_ways_updates_the_relation() {
        let mut layer = layer();
        // Cutting the ring open invalidates the lake...
        let ring = square(0, 0, 10, 10);
        assert!(layer.set_way_nodes(10, ring[..4].to_vec()).is_ok());
        assert_eq!(
            layer.multipolygon_state(500),
            MpState::Invalid(Problem::Unclosed)
        );
        // (An open way has no interior either, so nothing is selectable.)
        assert_eq!(double_click(&layer, 1.5, 8.5), None);
        // ...and putting it back (the undo) restores it.
        assert!(layer.set_way_nodes(10, ring).is_ok());
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Deleting the island way (a member) leaves a relation with a missing
        // member; restoring it brings the hole back.
        let snapshot = layer.delete_feature(FeatureKind::Way, 11).unwrap();
        assert_eq!(
            layer.multipolygon_state(500),
            MpState::Invalid(Problem::MissingWay(11))
        );
        layer.restore_feature(snapshot).unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Adding a vertex to the outer way keeps it valid and re-triangulates.
        layer.insert_node_into_way(10, 1, 39.9997, -73.995).unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
        assert_eq!(tris, 9);
        layer.remove_node_from_way(10, 1).unwrap();
        let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
        assert_eq!(tris, 8);
    }

    #[test]
    fn changing_members_roles_and_tags_updates_the_relation() {
        let mut layer = layer();
        // Dropping the inner leaves a plain outer.
        let previous = layer
            .set_relation_members(500, vec![member(10, "outer")])
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 0));
        // Undo: the old member list is back, with its hole.
        layer.set_relation_members(500, previous).unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // The island as an outer too: two polygons, no hole.
        layer
            .set_relation_members(500, vec![member(10, "outer"), member(11, "outer")])
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(2, 0));
        layer
            .set_relation_members(500, vec![member(10, "outer"), member(11, "inner")])
            .unwrap();

        // Removing the type makes it a plain relation; putting it back
        // makes it a multipolygon again.
        layer
            .remove_tag(FeatureKind::Relation, 500, "type")
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
        assert_eq!(double_click(&layer, 1.5, 8.5), Some((FeatureKind::Way, 10)));
        layer
            .set_tag(FeatureKind::Relation, 500, "type", "multipolygon")
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Delete and restore the relation (undo of a delete).
        let snapshot = layer.delete_relation(500).unwrap();
        assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
        assert!(layer.restore_relation(snapshot).is_ok());
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // A newly created multipolygon appears at once.
        let id = layer
            .create_relation(
                vec![
                    ("type".to_string(), "multipolygon".to_string()),
                    ("building".to_string(), "yes".to_string()),
                ],
                vec![member(11, "outer")],
            )
            .unwrap();
        assert_eq!(layer.multipolygon_state(id), valid(1, 0));
    }

    #[test]
    fn a_style_change_re_resolves_the_fill() {
        let mut layer = layer();
        assert!(layer.multipolygon_geometry(500, nid(0, 0)).unwrap().1 > 0);
        // No fill style for this tag: assembled (selectable) but not
        // triangulated.
        layer
            .set_tag(FeatureKind::Relation, 500, "natural", "bare_rock")
            .unwrap();
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        assert_eq!(layer.multipolygon_geometry(500, nid(0, 0)).unwrap().1, 0);
    }

    #[test]
    fn the_cache_is_rebuilt_from_scratch_on_reload_and_dropped_on_clear() {
        let mut layer = layer();
        let data = layer.get_osm_data().unwrap();
        layer.set_osm_data(data);
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        layer.clear_osm_data();
        assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
    }
}

#[cfg(test)]
mod relation_overlay_tests {
    use super::{lat_lon_to_mercator, OsmLayer, RESTRICTION_OFFSET_PX};
    use crate::layers::{EditableLayer, LayerId};
    use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
    use crate::selection::FeatureKind;
    use crate::viewport::Viewport;
    use gpui::{point, px, size};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version: 1,
            tags: HashMap::new(),
        }
    }

    fn way(id: i64, nodes: &[i64]) -> OsmWay {
        OsmWay {
            id,
            nodes: nodes.to_vec(),
            version: 1,
            tags: HashMap::from([("highway".to_string(), "residential".to_string())]),
        }
    }

    fn rel(id: i64, tags: &[(&str, &str)], members: &[(MemberType, i64, &str)]) -> OsmRelation {
        OsmRelation {
            id,
            version: 1,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            members: members
                .iter()
                .map(|&(member_type, reference, role)| OsmMember {
                    member_type,
                    reference,
                    role: role.to_string(),
                })
                .collect(),
        }
    }

    /// Ways 1 (nodes 1-2) and 2 (nodes 2-3) meet at node 2. Route 10 (bus)
    /// has both, route 11 (bicycle, colour tag) has way 2 and a way that
    /// isn't loaded. Restriction 20 is a `no_left_turn` via node 2, 21 an
    /// `only_straight_on` via a node that isn't loaded.
    fn layer() -> OsmLayer {
        use MemberType::*;
        let mut nodes = HashMap::new();
        for n in [
            node(4, 40.002, -73.99),
            node(1, 40.0, -74.0),
            node(2, 40.0, -73.999),
            node(3, 40.001, -73.999),
        ] {
            nodes.insert(n.id, n);
        }
        let mut ways = HashMap::new();
        for w in [way(1, &[1, 2]), way(2, &[2, 3])] {
            ways.insert(w.id, w);
        }
        let data = OsmData {
            nodes,
            ways,
            relations: vec![
                rel(
                    11,
                    &[
                        ("type", "route"),
                        ("route", "bicycle"),
                        ("colour", "#ff0000"),
                    ],
                    &[(Way, 2, ""), (Way, 99, "")],
                ),
                rel(
                    10,
                    &[("type", "route"), ("route", "bus")],
                    &[(Way, 1, ""), (Way, 2, "")],
                ),
                rel(
                    12,
                    &[("type", "route_master"), ("route", "bus")],
                    &[(Way, 1, "")],
                ),
                rel(
                    20,
                    &[("type", "restriction"), ("restriction", "no_left_turn")],
                    &[(Way, 1, "from"), (Node, 2, "via"), (Way, 2, "to")],
                ),
                rel(
                    21,
                    &[("type", "restriction"), ("restriction", "only_straight_on")],
                    &[(Way, 1, "from"), (Node, 777, "via"), (Way, 2, "to")],
                ),
                rel(
                    22,
                    &[("type", "restriction"), ("restriction", "no_u_turn")],
                    &[(Way, 1, "from"), (Node, 4, "via"), (Way, 2, "to")],
                ),
            ],
            bounds: Vec::new(),
        };
        OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
    }

    #[test]
    fn a_way_in_several_routes_gets_the_lowest_relation_id() {
        let layer = layer();
        assert_eq!(layer.route_overlay_colour(1), Some(0x9B30D9));
        // Routes 10 (bus) and 11 (red): 10 wins.
        assert_eq!(layer.route_overlay_colour(2), Some(0x9B30D9));
        // Not a member of anything, or not loaded.
        assert_eq!(layer.route_overlay_colour(98), None);
    }

    #[test]
    fn route_edits_follow_the_relation() {
        let mut layer = layer();
        layer
            .set_tag(FeatureKind::Relation, 10, "type", "route_master")
            .unwrap();
        assert_eq!(layer.route_overlay_colour(1), None);
        assert_eq!(layer.route_overlay_colour(2), Some(0xFF0000));
        layer
            .set_tag(FeatureKind::Relation, 10, "type", "route")
            .unwrap();
        assert_eq!(layer.route_overlay_colour(1), Some(0x9B30D9));
    }

    #[test]
    fn restriction_markers_sit_at_the_via_and_skip_missing_vias() {
        let layer = layer();
        let markers = layer.restriction_markers();
        // 21's via node isn't loaded: no marker.
        assert_eq!(markers.iter().map(|m| m.0).collect::<Vec<_>>(), [20, 22]);
        let (_, x, y, only) = markers[0];
        assert!(!only);
        let (ex, ey) = lat_lon_to_mercator(40.0, -73.999);
        assert!((x - ex).abs() < 1e-6 && (y - ey).abs() < 1e-6);
    }

    #[test]
    fn moving_or_deleting_the_via_node_updates_the_marker() {
        let mut layer = layer();
        layer.commit_node_moves(&[(2, 40.0005, -73.998)]).unwrap();
        let (_, x, y, _) = layer.restriction_markers()[0];
        let (ex, ey) = lat_lon_to_mercator(40.0005, -73.998);
        assert!((x - ex).abs() < 1e-6 && (y - ey).abs() < 1e-6);

        // Node 4 is a standalone via, so it can be deleted.
        let snapshot = layer.delete_feature(FeatureKind::Node, 4).unwrap();
        assert_eq!(layer.restriction_markers().len(), 1);
        layer.restore_feature(snapshot).unwrap();
        assert_eq!(layer.restriction_markers().len(), 2);
    }

    #[test]
    fn clicking_the_marker_selects_the_restriction_only_at_vertex_zoom() {
        let mut layer = layer();
        let v = Viewport::new(40.0005, -73.9985, 18.0, size(px(800.0), px(600.0)));
        let via = v.geo_to_screen(40.0, -73.999);
        let on_marker = point(
            via.x + px(RESTRICTION_OFFSET_PX.0),
            via.y + px(RESTRICTION_OFFSET_PX.1),
        );
        let hit = layer.hit_test(&v, on_marker);
        assert_eq!(hit.len(), 1);
        assert_eq!(
            (hit[0].kind, hit[0].feature.id),
            (FeatureKind::Relation, 20)
        );
        // The via node itself is still the node (once it is selected: a
        // plain vertex is not otherwise a target).
        layer.set_highlight(&[crate::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 2,
        }]);
        let hit = layer.hit_test(&v, point(via.x, via.y));
        assert_eq!(hit[0].kind, FeatureKind::Node);
        // Zoomed out, no marker to click.
        let far = Viewport::new(40.0005, -73.9985, 12.0, size(px(800.0), px(600.0)));
        let via = far.geo_to_screen(40.0, -73.999);
        let hit = layer.hit_test(
            &far,
            point(
                via.x + px(RESTRICTION_OFFSET_PX.0),
                via.y + px(RESTRICTION_OFFSET_PX.1),
            ),
        );
        assert!(hit.iter().all(|h| h.kind != FeatureKind::Relation));
    }
}

/// The level-of-detail rules and the data they depend on (`WayGeom`,
/// the way scene), including that no edit leaves them stale.
#[cfg(test)]
mod way_lod_tests {
    // Explicit imports: a glob of `super::*` brings gpui's `test` macro in
    // and shadows `#[test]`.
    use super::{
        vertex_significance, WayDetail, WayLod, CASING_MIN_ZOOM, COARSE_BELOW_ZOOM,
        COLLAPSE_BELOW_PX,
    };
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::{EditableLayer, LayerId};
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::viewport::Viewport;
    use gpui::{point, px, size, Bounds, Pixels};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn tags() -> HashMap<String, String> {
        HashMap::new()
    }

    fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            tags: tags(),
            version: 1,
        }
    }

    fn way(id: i64, nodes: Vec<i64>) -> OsmWay {
        OsmWay {
            id,
            nodes,
            tags: tags(),
            version: 1,
        }
    }

    fn vert(x: f64, y: f64) -> (i64, f64, f64) {
        (0, x, y)
    }

    #[test]
    fn significance_of_a_straight_line_is_zero_and_ends_are_infinite() {
        let sig = vertex_significance(&[vert(0.0, 0.0), vert(1.0, 0.0), vert(2.0, 0.0)]);
        assert_eq!(sig[0], f32::INFINITY);
        assert_eq!(sig[2], f32::INFINITY);
        assert_eq!(sig[1], 0.0);
    }

    #[test]
    fn significance_is_the_distance_from_the_chord() {
        let sig = vertex_significance(&[vert(0.0, 0.0), vert(5.0, 3.0), vert(10.0, 0.0)]);
        assert!((sig[1] - 3.0).abs() < 1e-6, "{sig:?}");
    }

    #[test]
    fn thresholding_the_significance_is_douglas_peucker() {
        // A spike (10) followed by a bump (0.5) next to it, then a flat run.
        let pts = [
            vert(0.0, 0.0),
            vert(2.0, 0.5),
            vert(4.0, 10.0),
            vert(6.0, 0.5),
            vert(8.0, 0.0),
            vert(10.0, 0.0),
        ];
        let sig = vertex_significance(&pts);
        // Never larger than the split vertex's ancestors, which is what
        // makes a threshold equal to running Douglas-Peucker.
        assert!(sig[2] >= sig[1] && sig[2] >= sig[3], "{sig:?}");
        let kept = |tol: f32| -> Vec<usize> { (0..pts.len()).filter(|&i| sig[i] >= tol).collect() };
        assert_eq!(kept(0.0).len(), pts.len());
        assert_eq!(kept(5.0), vec![0, 2, 5], "only the spike survives");
        assert_eq!(kept(100.0), vec![0, 5], "ends are always kept");
    }

    #[test]
    fn significance_of_a_closed_ring_keeps_its_far_corner() {
        let ring = [
            vert(0.0, 0.0),
            vert(10.0, 0.0),
            vert(10.0, 10.0),
            vert(0.0, 10.0),
            vert(0.0, 0.0),
        ];
        let sig = vertex_significance(&ring);
        assert_eq!(sig[0], f32::INFINITY);
        assert_eq!(sig[4], f32::INFINITY);
        assert!(sig[1..4].iter().all(|&s| s > 5.0), "{sig:?}");
    }

    #[test]
    fn significance_of_degenerate_input_does_not_panic() {
        assert!(vertex_significance(&[]).is_empty());
        assert_eq!(vertex_significance(&[vert(1.0, 1.0)]), vec![f32::INFINITY]);
        let same = vertex_significance(&[vert(1.0, 1.0), vert(1.0, 1.0), vert(1.0, 1.0)]);
        assert_eq!(same[1], 0.0);
    }

    #[test]
    fn the_coarse_rules_start_below_zoom_16() {
        assert!(WayLod::at(15.99).coarse);
        assert!(!WayLod::at(16.0).coarse);
        assert!(!WayLod::at(19.5).coarse);
        assert!(WayLod::at(14.0).tolerance_px > WayLod::at(16.0).tolerance_px);
        assert!(WayLod::at(14.0).min_segment_px > WayLod::at(16.0).min_segment_px);
    }

    #[test]
    fn tiny_ways_are_skipped_or_collapsed_only_when_coarse() {
        let coarse = WayLod::at(15.0);
        assert_eq!(coarse.detail(0.1), WayDetail::Skip);
        assert_eq!(coarse.detail(1.5), WayDetail::Collapse);
        assert_eq!(coarse.detail(COLLAPSE_BELOW_PX), WayDetail::Full);
        assert_eq!(coarse.detail(400.0), WayDetail::Full);
        let editing = WayLod::at(16.0);
        assert_eq!(editing.detail(0.1), WayDetail::Full);
        assert_eq!(editing.detail(1.5), WayDetail::Full);
    }

    #[test]
    fn tiny_fills_are_dropped_only_when_coarse() {
        assert!(!WayLod::at(15.0).fills(1.0));
        assert!(WayLod::at(15.0).fills(10.0));
        assert!(WayLod::at(17.0).fills(0.1));
    }

    #[test]
    fn minor_ways_are_dropped_only_below_zoom_15() {
        assert!(!WayLod::at(14.0).strokes(1.5));
        assert!(!WayLod::at(14.9).strokes(1.0));
        assert!(WayLod::at(14.0).strokes(2.0));
        assert!(WayLod::at(15.0).strokes(1.0));
        assert!(WayLod::at(16.0).strokes(1.0));
        assert!(WayLod::at(19.0).strokes(1.0));
    }

    #[test]
    fn casing_starts_at_the_editing_zoom() {
        assert_eq!(CASING_MIN_ZOOM, COARSE_BELOW_ZOOM);
    }

    // ---- the scene follows edits ---------------------------------------

    const LAT: f64 = 40.0;
    const LON: f64 = -74.0;

    /// A straight 170 m way east, with a kink 0.2 m off the line (node 2) and
    /// two vertices on it (nodes 3 and 4). At zoom 16 it simplifies to a
    /// single segment; at zoom 19 the kink is a pixel and is kept.
    fn layer() -> OsmLayer {
        let nodes = vec![
            node(1, LAT, LON),
            node(2, LAT + 0.000002, LON + 0.0005),
            node(3, LAT, LON + 0.001),
            node(4, LAT, LON + 0.0015),
            node(5, LAT, LON + 0.002),
        ];
        let data = OsmData {
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            ways: HashMap::from([(10, way(10, vec![1, 2, 3, 4, 5]))]),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        OsmLayer::new_with_data(LayerId(1), "t", Arc::new(data))
    }

    fn view(zoom: f64) -> (Viewport, Bounds<Pixels>) {
        let v = Viewport::new(LAT, LON + 0.001, zoom, size(px(800.0), px(600.0)));
        let b = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        (v, b)
    }

    fn scene_vertices(layer: &OsmLayer, zoom: f64) -> usize {
        let (v, b) = view(zoom);
        layer.build_way_scene(&v, b).vertex_count()
    }

    fn scene_paths(layer: &OsmLayer, zoom: f64) -> usize {
        let (v, b) = view(zoom);
        layer.build_way_scene(&v, b).path_count()
    }

    fn layer_styled(css: &str) -> OsmLayer {
        let mut l = layer();
        l.set_stylesheet(Arc::new(crate::style::Stylesheet::parse(css).unwrap()));
        l
    }

    #[test]
    fn a_styled_casing_adds_a_path_below_the_default_casing_zoom() {
        // Zoom 15: no default casing, so one stroke path.
        assert_eq!(scene_paths(&layer_styled("way { width: 3; }"), 15.0), 1);
        let cased = layer_styled("way { width: 3; casing-width: 2; casing-color: red; }");
        assert_eq!(scene_paths(&cased, 15.0), 2);
        // At zoom 17 the styled casing replaces the default one.
        assert_eq!(scene_paths(&cased, 17.0), 2);
    }

    /// Two overlapping ways over the same nodes, tagged `highway=lo` (way
    /// 10) and `highway=hi` (way 11).
    fn two_way_layer(css: &str) -> OsmLayer {
        let l = layer();
        let mut data = (**l.osm_data.as_ref().unwrap()).clone();
        let mut lo = way(10, vec![1, 2, 3, 4, 5]);
        lo.tags.insert("highway".into(), "lo".into());
        let mut hi = way(11, vec![1, 2, 3, 4, 5]);
        hi.tags.insert("highway".into(), "hi".into());
        data.ways = HashMap::from([(10, lo), (11, hi)]);
        let mut l = OsmLayer::new_with_data(LayerId(1), "t", Arc::new(data));
        l.set_stylesheet(Arc::new(crate::style::Stylesheet::parse(css).unwrap()));
        l
    }

    fn line_colors(l: &OsmLayer, zoom: f64) -> Vec<u32> {
        let (v, b) = view(zoom);
        l.build_way_scene(&v, b)
            .lines
            .iter()
            .map(|(rgba, _)| rgba >> 8)
            .collect()
    }

    #[test]
    fn z_index_orders_strokes_whatever_the_way_order() {
        // `lo` is way 10 and would draw first anyway; swap which one is on top.
        let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 1; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; }";
        let l = two_way_layer(css);
        assert_eq!(line_colors(&l, 17.0), vec![0x0000ff, 0xff0000]);
        let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 9; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; }";
        let l = two_way_layer(css);
        assert_eq!(line_colors(&l, 17.0), vec![0xff0000, 0x0000ff]);
    }

    #[test]
    fn a_higher_layers_casing_covers_a_lower_layers_stroke() {
        let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 1; casing-width: 1; casing-color: #111111; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; casing-width: 1; casing-color: #222222; }";
        let l = two_way_layer(css);
        assert_eq!(
            line_colors(&l, 17.0),
            vec![0x111111, 0x0000ff, 0x222222, 0xff0000]
        );
    }

    #[test]
    fn stroke_opacity_becomes_the_batch_alpha() {
        let l = layer_styled("way { color: #123456; width: 3; opacity: 0.5; }");
        let (v, b) = view(17.0);
        let scene = l.build_way_scene(&v, b);
        assert_eq!(scene.lines.len(), 1);
        assert_eq!(scene.lines[0].0, 0x1234_5600 | 128);
    }

    #[test]
    fn a_dashed_stroke_draws_fewer_pixels_than_a_solid_one() {
        let area = |l: &OsmLayer| {
            let (v, b) = view(18.0);
            l.build_way_scene(&v, b)
                .lines
                .iter()
                .map(|(_, p)| p.vertices.len())
                .sum::<usize>()
        };
        let solid = layer_styled("way { width: 3; }");
        let dashed = layer_styled("way { width: 3; dashes: 8,8; }");
        // Many short rectangles instead of one long one.
        assert!(area(&dashed) > area(&solid));
        // And never below the editing zoom, where the stroke is simplified.
        assert_eq!(scene_vertices(&dashed, 15.0), scene_vertices(&solid, 15.0));
    }

    #[test]
    fn equal_z_index_keeps_a_stable_order_across_frames() {
        let css = "way[highway] { width: 3; } way[highway=lo] { color: #0000ff; } way[highway=hi] { color: #ff0000; }";
        let l = two_way_layer(css);
        let first = line_colors(&l, 17.0);
        assert_eq!(first.len(), 2);
        for _ in 0..50 {
            assert_eq!(line_colors(&l, 17.0), first);
        }
    }

    #[test]
    fn zoom_gated_casing_follows_the_zoom_bucket() {
        let l = layer_styled("way { width: 3; } way|z15- { casing-width: 2; }");
        assert_eq!(scene_paths(&l, 14.5), 1);
        assert_eq!(scene_paths(&l, 15.2), 2);
        // And back: the per-bucket table is not stale.
        assert_eq!(scene_paths(&l, 14.5), 1);
    }

    /// The layer's cached geometry and its scene are what a layer freshly
    /// built from the same data would have, at every zoom tried.
    fn assert_same_as_fresh(layer: &OsmLayer) {
        let data = layer.osm_data.clone().expect("data");
        let fresh = OsmLayer::new_with_data(LayerId(9), "fresh", data);
        for (i, id) in layer.way_ids.iter().enumerate() {
            let j = fresh.way_ids.iter().position(|x| x == id).expect("way");
            assert_eq!(layer.way_vertices[i], fresh.way_vertices[j], "way {id}");
        }
        for zoom in [12.0, 15.5, 16.0, 17.0, 19.0] {
            assert_eq!(
                scene_vertices(layer, zoom),
                scene_vertices(&fresh, zoom),
                "zoom {zoom}"
            );
        }
    }

    #[test]
    fn detail_grows_with_zoom() {
        let l = layer();
        // z16 drops the kink 0.2 m off the line, z19 keeps it.
        assert!(scene_vertices(&l, 19.0) > scene_vertices(&l, 16.0));
        assert!(scene_vertices(&l, 16.0) > 0);
    }

    #[test]
    fn a_minor_way_has_no_outline_at_zoom_14_but_does_at_15() {
        // The test way is untagged: the default 1.5 px stroke.
        let l = layer();
        assert_eq!(scene_vertices(&l, 14.0), 0);
        assert!(scene_vertices(&l, 15.0) > 0);
    }

    #[test]
    fn moving_a_node_updates_the_simplification() {
        let mut l = layer();
        assert_same_as_fresh(&l);
        let before = scene_vertices(&l, 17.0);
        // Node 3 starts on the line and is simplified away. Pull it 55 m off
        // the line: it becomes significant, so the way draws more segments.
        l.commit_node_moves(&[(3, LAT + 0.0005, LON + 0.001)])
            .unwrap();
        assert_same_as_fresh(&l);
        let bent = scene_vertices(&l, 17.0);
        assert!(bent > before, "{bent} > {before}");
        // And back (what undo does): it is simplified away again.
        l.commit_node_moves(&[(3, LAT, LON + 0.001)]).unwrap();
        assert_same_as_fresh(&l);
        assert_eq!(scene_vertices(&l, 17.0), before);
    }

    #[test]
    fn adding_and_removing_ways_updates_the_scene() {
        let mut l = layer();
        let before = scene_vertices(&l, 17.0);
        let a = l.add_node(LAT + 0.0002, LON);
        let b = l.add_node(LAT + 0.0002, LON + 0.001);
        let c = l.add_node(LAT + 0.0004, LON + 0.002);
        let id = l.add_way(vec![a, b, c], Vec::new());
        assert_same_as_fresh(&l);
        assert!(scene_vertices(&l, 17.0) > before);
        l.remove_way(id).unwrap();
        assert_same_as_fresh(&l);
        assert_eq!(scene_vertices(&l, 17.0), before);
    }

    #[test]
    fn changing_a_ways_nodes_updates_the_scene() {
        let mut l = layer();
        // Zoom 19 keeps the 0.2 m kink (node 2), so dropping it shows.
        let before = scene_vertices(&l, 19.0);
        assert!(l.set_way_nodes(10, vec![1, 5]).is_ok());
        assert_same_as_fresh(&l);
        assert!(scene_vertices(&l, 19.0) < before);
        l.remove_node_from_way(10, 0).unwrap();
        assert_same_as_fresh(&l);
    }

    #[test]
    fn a_drag_in_progress_draws_every_vertex() {
        let mut l = layer();
        // z16 simplifies the 0.2 m kink away ...
        let simplified = scene_vertices(&l, 16.0);
        // ... but while nodes are dragged nothing is dropped, because the
        // cached significance is of the geometry before the drag.
        l.set_drag_preview(&HashSet::from([4]), point(px(3.0), px(0.0)));
        assert!(scene_vertices(&l, 16.0) > simplified);
        l.clear_drag_preview();
        assert_eq!(scene_vertices(&l, 16.0), simplified);
    }

    #[test]
    fn repeated_frames_build_identical_scenes() {
        // The vertex-capacity hints carry over from frame to frame and must
        // not change what is drawn.
        let l = layer();
        let first = scene_vertices(&l, 17.0);
        assert_eq!(scene_vertices(&l, 17.0), first);
        assert_eq!(scene_vertices(&l, 17.0), first);
    }
}
