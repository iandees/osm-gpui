use gpui::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::layers::diff::{baseline_for_loaded_file, diff_osm_data, LayerDiff};
use crate::layers::merge::{self, MergeStats};
use crate::layers::node_markers::{self, NodeForm};
use crate::layers::resolve;
use crate::layers::{EditableLayer, HighlightKind, LayerId, MapLayer};
use crate::osm::{FileEdits, OsmData, OsmNode, OsmWay};
use crate::osm_upload::UploadResult;
use crate::selection::{
    nearest_point_on_segment, point_to_segment_distance, DeletedFeatureSnapshot,
    DeletedRelationSnapshot, FeatureKind, FeatureRef, HitCandidate,
};
use crate::style::{Direction, NodeStyle, Stylesheet, WayStyle};
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

/// How far the casing extends beyond each side of a way's stroke, in pixels.
const CASING_WIDTH: f32 = 1.0;

/// Below this zoom the casing is skipped: ways are dense enough that it
/// only adds clutter, and it doubles the way pass's vertex count exactly
/// when the most ways are on screen (measured with `examples/perf_bench`:
/// 2.8 ms -> 5.2 ms of CPU per frame for 8k ways all visible at z14).
const CASING_MIN_ZOOM: f64 = 15.0;

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

/// Nodes shown in full whatever the zoom because they are selected; see
/// `OsmLayer::pins`.
#[derive(Default)]
struct Pins {
    nodes: HashSet<i64>,
    /// Indices into `way_vertices`.
    ways: HashSet<usize>,
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
    /// Assembled and triangulated multipolygon relations; see [`MpCache`].
    mp: MpCache,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
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
    let style = stylesheet.way_style(&relation.tags, true);
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

/// The simple form of a tagged node at low zoom: a small filled square, no
/// casing, as two triangles of the frame's shared `path`.
fn push_simple_node_marker(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    center: Point<Pixels>,
    edge: f32,
) {
    let h = px(edge / 2.0);
    let tl = point(center.x - h, center.y - h);
    let tr = point(center.x + h, center.y - h);
    let bl = point(center.x - h, center.y + h);
    let br = point(center.x + h, center.y + h);
    push_triangle(path, bounds_min_max, tl, tr, bl);
    push_triangle(path, bounds_min_max, tr, br, bl);
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
            mp: MpCache::default(),
            layer_bbox: None,
            download_rects: Vec::new(),
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
        let fs_load = crate::frame_stats::start();
        let stylesheet = Arc::new(Stylesheet::load_default());
        let fs_phase = crate::frame_stats::start();
        let node_cache = compute_node_cache(&osm_data, &stylesheet);
        crate::frame_stats::lap("load.node_cache", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let (way_ids, way_bboxes, way_vertices, way_styles, way_fill_tris) =
            compute_way_tables(&osm_data, &node_cache, &stylesheet);
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
            mp,
            layer_bbox,
            download_rects: crate::download_bounds::merc_rects(&osm_data_bounds),
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
        layer.original_data = Some(Arc::new(baseline));
        // Locally created ways in the file already use negative ids; new
        // ones must not collide with them.
        let lowest_way_id = osm_data.ways.keys().copied().min().unwrap_or(0);
        layer.next_placeholder_way_id = lowest_way_id.min(0) - 1;
        layer
    }

    /// This layer as a JOSM-style `.osm` document, including its pending
    /// edits; see `osm_export`. `None` when the layer holds no data.
    pub fn export_xml(&self) -> Option<String> {
        let current = self.osm_data.as_deref()?;
        let original = self.original_data.as_deref().unwrap_or(current);
        Some(crate::osm_export::to_osm_xml(original, current))
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
        self.mp
            .rebuild_all(&osm_data, &self.node_cache, &self.stylesheet);
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        let ways_sorted = sorted_ways(&osm_data);
        self.way_index = build_way_index(&self.way_bboxes, &ways_sorted);
        self.way_id_to_index = build_way_id_index(&ways_sorted);
        self.node_to_ways = build_node_to_ways(&ways_sorted);
        self.next_new_id = compute_next_new_id(&osm_data);
        self.download_rects = crate::download_bounds::merc_rects(&osm_data.bounds);
        self.osm_data = Some(osm_data.clone());
        self.original_data = Some(osm_data);
        self.modified = false;
    }

    /// Merge freshly downloaded server data into this layer by object id,
    /// keeping local edits — see `merge::merge_download` for the rules.
    /// Rebuilds every derived cache from scratch (like `set_osm_data`; a
    /// download is not a per-frame path), but preserves the split between
    /// the server baseline and local edits, and the `modified` flag.
    pub fn merge_download(&mut self, incoming: OsmData) -> MergeStats {
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
        stats
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
        true
    }

    /// See `resolve::set_base_version`. Only versions change, so nothing
    /// derived needs rebuilding.
    pub fn set_base_version(&mut self, kind: FeatureKind, id: i64, version: i32) -> bool {
        let (Some(current), Some(original)) = (self.osm_data.as_mut(), self.original_data.as_mut())
        else {
            return false;
        };
        resolve::set_base_version(
            Arc::make_mut(original),
            Arc::make_mut(current),
            kind,
            id,
            version,
        )
    }

    /// See `resolve::adopt_server_element`.
    pub fn adopt_server_element(
        &mut self,
        kind: FeatureKind,
        id: i64,
        server: crate::osm_api::ServerElement,
    ) -> bool {
        self.change_snapshots(|original, current| {
            resolve::adopt_server_element(original, current, kind, id, server)
        })
    }

    /// See `resolve::forget_local_delete`.
    pub fn forget_local_delete(&mut self, kind: FeatureKind, id: i64) -> bool {
        self.change_snapshots(|original, current| {
            resolve::forget_local_delete(original, current, kind, id)
        })
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

        // Multipolygons made of those ways follow the move.
        let touched_way_ids: Vec<i64> = touched_ways.iter().map(|&i| self.way_ids[i]).collect();
        self.mp_refresh_ways(&touched_way_ids);
        self.mp_refresh_nodes(&moved_ids);
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
            FeatureKind::Relation => data
                .relations
                .iter_mut()
                .find(|r| r.id == id)
                .map(|r| &mut r.tags),
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
        if kind == FeatureKind::Relation {
            // The relation's style (or whether it is a multipolygon at all)
            // follows its tags.
            self.mp_refresh_relations(&[id]);
        }
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
            FeatureKind::Relation => data
                .relations
                .iter_mut()
                .find(|r| r.id == id)
                .map(|r| &mut r.tags),
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
        if kind == FeatureKind::Relation {
            self.mp_refresh_relations(&[id]);
        }
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
                bounds: Vec::new(),
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
        let using_ways = self.ways_using_node(node_id);
        self.node_to_ways.remove(&node_id);
        self.modified = true;
        self.osm_data = Some(Arc::new(data));
        self.mp_refresh_ways(&using_ways);
        self.mp_refresh_nodes(&[node_id]);
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
                bounds: Vec::new(),
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
            self.node_to_ways.entry(*nid).or_default().insert(way_idx);
        }
        self.way_ids.push(way.id);
        let style = self.stylesheet.way_style(&way.tags, way.is_closed());
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
        data.ways.insert(way.id, way);

        self.modified = true;
        self.mp_refresh_ways(&[id]);
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
            *ways = ways
                .iter()
                .filter(|&&w| w != way_idx)
                .map(|&w| if w > way_idx { w - 1 } else { w })
                .collect();
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());

        self.modified = true;
        self.mp_refresh_ways(&[way_id]);
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
        // A reshaped way reshapes the multipolygons it belongs to.
        self.mp_refresh_ways(&[way_id]);
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

        self.node_to_ways
            .entry(node_id)
            .or_default()
            .insert(way_idx);
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

        self.node_to_ways.entry(new_id).or_default().insert(way_idx);
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

        // Only drop this way's `node_to_ways` entry for `removed_node_id` if
        // the way no longer references that node at all — a closed way can
        // reference the same node twice (ring closure), so removing one
        // occurrence must not erase the way's remaining reference.
        if !way_snapshot.nodes.contains(&removed_node_id) {
            if let Some(way_idxs) = self.node_to_ways.get_mut(&removed_node_id) {
                way_idxs.remove(&way_idx);
            }
        }
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);

        self.modified = true;
    }

    /// Reverse the node order of a way and refresh that way's derived caches.
    /// Applying it twice restores the original order, so it is its own
    /// inverse for undo/redo. Returns whether the way was found.
    pub fn reverse_way(&mut self, way_id: i64) -> bool {
        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return false;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return false;
        };
        let data = Arc::make_mut(arc);
        let Some(way) = data.ways.get_mut(&way_id) else {
            return false;
        };
        way.nodes = crate::geometry::reversed(&way.nodes);
        let way_snapshot = way.clone();
        // The set of nodes (and so `node_to_ways`) is unchanged.
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);
        self.modified = true;
        true
    }

    /// Replace a way's node list and refresh its derived caches and the
    /// node-to-ways index. The nodes must already exist. Marks the layer
    /// modified. Returns whether the way was found.
    pub fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> bool {
        let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
            return false;
        };
        let Some(arc) = self.osm_data.as_mut() else {
            return false;
        };
        let data = Arc::make_mut(arc);
        let Some(way) = data.ways.get_mut(&way_id) else {
            return false;
        };
        let old = std::mem::replace(&mut way.nodes, node_ids);
        let way_snapshot = way.clone();

        for dropped in old.iter().filter(|n| !way_snapshot.nodes.contains(n)) {
            if let Some(way_idxs) = self.node_to_ways.get_mut(dropped) {
                way_idxs.remove(&way_idx);
                if way_idxs.is_empty() {
                    self.node_to_ways.remove(dropped);
                }
            }
        }
        for &added in &way_snapshot.nodes {
            self.node_to_ways.entry(added).or_default().insert(way_idx);
        }
        self.refresh_way_geometry_cache(way_idx, way_id, &way_snapshot);
        self.modified = true;
        true
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

    /// The ids of the ways whose node list includes `node_id`, ascending.
    pub fn ways_using_node(&self, node_id: i64) -> Vec<i64> {
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

    /// The ids of the relations that have `way_id` as a member.
    pub fn relations_containing_way(&self, way_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Way, way_id)
    }

    /// The ids of the relations that have `node_id` as a member.
    pub fn relations_containing_node(&self, node_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Node, node_id)
    }

    /// A copy of the relation with this id, if the layer has it.
    pub fn relation(&self, relation_id: i64) -> Option<crate::osm::OsmRelation> {
        self.osm_data
            .as_ref()?
            .relations
            .iter()
            .find(|r| r.id == relation_id)
            .cloned()
    }

    /// Replace a relation's whole member list and return the previous one,
    /// or `None` (changing nothing) if the layer has no such relation. Marks
    /// the layer modified. Members don't feed any render cache, so nothing
    /// else needs refreshing.
    pub fn set_relation_members(
        &mut self,
        relation_id: i64,
        members: Vec<crate::osm::OsmMember>,
    ) -> Option<Vec<crate::osm::OsmMember>> {
        let data = Arc::make_mut(self.osm_data.as_mut()?);
        let relation = data.relations.iter_mut().find(|r| r.id == relation_id)?;
        let previous = std::mem::replace(&mut relation.members, members);
        self.modified = true;
        self.mp_refresh_relations(&[relation_id]);
        Some(previous)
    }

    /// Copies of every relation in the layer, in storage order.
    pub fn relations(&self) -> Vec<crate::osm::OsmRelation> {
        self.osm_data
            .as_ref()
            .map(|d| d.relations.clone())
            .unwrap_or_default()
    }

    /// The ids of the relations that have `relation_id` as a member.
    pub fn relations_containing_relation(&self, relation_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Relation, relation_id)
    }

    /// Create a relation with these tags and members, assigning a fresh
    /// negative id, and return it. `None` (changing nothing) if the layer has
    /// no data loaded. The id is below every id the node, way and relation
    /// allocators have handed out or hold, so ids never collide across
    /// kinds. Member refs aren't resolved, so a member may be a feature that
    /// only exists while a neighbouring undo step is applied. Marks the layer
    /// modified.
    pub fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<crate::osm::OsmMember>,
    ) -> Option<i64> {
        let lowest_relation = self
            .osm_data
            .as_ref()?
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
        let data = Arc::make_mut(self.osm_data.as_mut()?);
        data.relations.push(crate::osm::OsmRelation {
            id,
            members,
            version: 0,
            tags: tags.into_iter().collect(),
        });
        self.modified = true;
        self.mp_refresh_relations(&[id]);
        Some(id)
    }

    /// Remove a relation, returning a snapshot for `restore_relation`.
    /// Refuses (`None`, changing nothing) if the relation doesn't exist or is
    /// itself a member of another relation; the caller removes that
    /// membership first. Its own members are untouched. Marks the layer
    /// modified.
    pub fn delete_relation(&mut self, relation_id: i64) -> Option<DeletedRelationSnapshot> {
        let data = self.osm_data.as_ref()?;
        if data.relations.iter().any(|r| {
            r.id != relation_id
                && r.members.iter().any(|m| {
                    m.member_type == crate::osm::MemberType::Relation && m.reference == relation_id
                })
        }) {
            return None;
        }
        let index = data.relations.iter().position(|r| r.id == relation_id)?;
        let data = Arc::make_mut(self.osm_data.as_mut()?);
        let relation = data.relations.remove(index);
        self.modified = true;
        self.mp_refresh_relations(&[relation_id]);
        Some(DeletedRelationSnapshot { relation, index })
    }

    /// Put a relation back exactly as the snapshot recorded it: same id,
    /// version, tags and members, at its original position. `false`
    /// (changing nothing) if a relation with that id exists or the layer has
    /// no data. Also the redo of a create.
    pub fn restore_relation(&mut self, snapshot: DeletedRelationSnapshot) -> bool {
        let Some(arc) = self.osm_data.as_mut() else {
            return false;
        };
        if arc.relations.iter().any(|r| r.id == snapshot.relation.id) {
            return false;
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
        self.modified = true;
        self.mp_refresh_relations(&[id]);
        true
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
        self.insert_node(new_id, lat, lon, HashMap::new(), 0);
        Some(new_id)
    }

    /// Insert (or overwrite) a node with `id`/`lat`/`lon`/`tags` into
    /// `osm_data.nodes` and incrementally patch `node_cache`/`node_index`/
    /// `layer_bbox`, mirroring the "new cache entry" branch of
    /// `commit_node_moves`. Keeps `next_new_id` below `id` so a later
    /// auto-allocation never collides with an explicitly-inserted id (e.g.
    /// from a redo). Marks the layer modified. No-op if this layer has no
    /// data loaded.
    fn insert_node(
        &mut self,
        id: i64,
        lat: f64,
        lon: f64,
        tags: HashMap<String, String>,
        version: i32,
    ) {
        let Some(arc) = self.osm_data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(arc);
        let node = OsmNode {
            id,
            lat,
            lon,
            version,
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
            // Relations can't be deleted yet.
            FeatureKind::Relation => None,
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
        self.mp_refresh_nodes(&[id]);

        let snapshot = DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id,
            tags: node.tags.into_iter().collect(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((node.lat, node.lon)),
            version: node.version,
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
            *ways = ways
                .iter()
                .filter(|&&w| w != way_idx)
                .map(|&w| if w > way_idx { w - 1 } else { w })
                .collect();
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());
        self.modified = true;
        self.mp_refresh_ways(&[id]);

        let snapshot = DeletedFeatureSnapshot {
            kind: FeatureKind::Way,
            id,
            tags: way.tags.into_iter().collect(),
            way_nodes: way.nodes,
            node_lat_lon: None,
            version: way.version,
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
                let id = snapshot.id;
                self.insert_node(
                    id,
                    lat,
                    lon,
                    snapshot.tags.into_iter().collect(),
                    snapshot.version,
                );
                // A restriction whose via this was draws again.
                self.mp_refresh_nodes(&[id]);
            }
            FeatureKind::Way => self.restore_way(snapshot),
            // Relations are never deleted yet, so there is nothing to restore.
            FeatureKind::Relation => {}
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
            version: snapshot.version,
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
            self.node_to_ways.entry(*nid).or_default().insert(way_idx);
        }
        self.way_ids.push(way.id);
        let style = self.stylesheet.way_style(&way.tags, way.is_closed());
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
        let restored_id = way.id;
        data.ways.insert(way.id, way);
        self.modified = true;
        self.mp_refresh_ways(&[restored_id]);
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
        pins
    }

    /// How node `id` (at `idx` in the node cache) is shown at `zoom`: the
    /// one answer behind drawing, hover, click and box select.
    fn node_form_at(&self, pins: &Pins, idx: usize, id: i64, zoom: f64) -> NodeForm {
        let pinned = pins.nodes.contains(&id)
            || (!pins.ways.is_empty()
                && self
                    .node_to_ways
                    .get(&id)
                    .is_some_and(|ws| ws.iter().any(|w| pins.ways.contains(w))));
        node_markers::node_form(self.node_cache.styles[idx].tagged, pinned, zoom)
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
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize, Point<Pixels>)> = None;
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

    fn apply_chunk_result(&mut self, result: &UploadResult, chunk: &LayerDiff) {
        OsmLayer::apply_chunk_result(self, result, chunk);
    }

    fn export_xml(&self) -> Option<String> {
        OsmLayer::export_xml(self)
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // Node rendering moved to `render_canvas` (paint_quad) to avoid the
        // per-node GPUI element layout cost. The selection ring is drawn in
        // `render_highlight` as a canvas outline.
        Vec::new()
    }

    fn download_rects(&self) -> &[crate::download_bounds::MercRect] {
        &self.download_rects
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

        // Multipolygon relations first, so a closed way's own fill (an
        // island, a building inside a forest) paints over them.
        let screen_rect = (
            f32::from(origin_x),
            f32::from(origin_y),
            f32::from(origin_x + bounds.size.width),
            f32::from(origin_y + bounds.size.height),
        );
        let fs_phase = crate::frame_stats::start();
        for entry in self.mp.entries.values() {
            let Some(fill) = entry.style.fill else {
                continue;
            };
            let b = &entry.bbox;
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
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
            for polygon in &entry.polygons {
                let pb = &polygon.bbox;
                if pb.max_x < vmin_x || pb.min_x > vmax_x || pb.max_y < vmin_y || pb.min_y > vmax_y
                {
                    continue;
                }
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (origin_x, origin_y),
                    screen_rect,
                    &mut fill_pts,
                    &mut group.path,
                    &mut group.bounds_min_max,
                );
            }
        }

        crate::frame_stats::lap("osm.mp_fill_build", fs_phase);

        let fs_phase = crate::frame_stats::start();
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
            for t in tris.as_chunks::<3>().0 {
                push_triangle(
                    &mut group.path,
                    &mut group.bounds_min_max,
                    fill_pts[t[0] as usize],
                    fill_pts[t[1] as usize],
                    fill_pts[t[2] as usize],
                );
            }
        }

        crate::frame_stats::lap("osm.way_fill_build", fs_phase);

        let fs_phase = crate::frame_stats::start();
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

        // Route overlays: a translucent line under each route member way,
        // one path per colour. Looked up per visible way from the cache
        // (`MpCache::route_ways`), never by scanning relations.
        struct OverlayGroup {
            rgba: u32,
            path: Option<Path<Pixels>>,
            bounds_min_max: (f32, f32, f32, f32),
        }
        let has_routes = !self.mp.route_ways.is_empty();
        let mut overlay_index: HashMap<u32, usize> = HashMap::new();
        let mut overlay_groups: Vec<OverlayGroup> = Vec::new();
        let overlay_alpha = (crate::relation_overlay::ROUTE_ALPHA * 255.0).round() as u32;

        // The casing: one dark path under every way's stroke (and direction
        // arrows), slightly wider than the stroke, so thin light lines stay
        // readable over light imagery. Built in the same pass as the strokes
        // from the same projected points, and painted before any of them.
        let zoom = viewport.zoom_level();
        let draw_casing = zoom >= CASING_MIN_ZOOM;
        let draw_arrows = zoom >= ARROW_MIN_ZOOM;
        let mut casing_path: Option<Path<Pixels>> = None;
        let mut casing_bounds = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );

        // Reused scratch buffer for each way's projected screen points, so
        // decimation (below) can look ahead/behind without per-way
        // allocation churn across the (potentially thousands of) visible
        // ways in a frame.
        let mut scratch_pts: Vec<Point<Pixels>> = Vec::new();

        crate::frame_stats::lap("osm.fill_paint", fs_phase);
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

            let overlay_idx = if has_routes {
                self.mp.route_colour_of_way(self.way_ids[i]).map(|colour| {
                    *overlay_index.entry(colour).or_insert_with(|| {
                        overlay_groups.push(OverlayGroup {
                            rgba: (colour << 8) | overlay_alpha,
                            path: None,
                            bounds_min_max: (
                                f32::INFINITY,
                                f32::NEG_INFINITY,
                                f32::INFINITY,
                                f32::NEG_INFINITY,
                            ),
                        });
                        overlay_groups.len() - 1
                    })
                })
            } else {
                None
            };

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
            let arrows = draw_arrows && style.direction != Direction::None;
            let reverse = style.direction == Direction::Reverse;
            // Start half a spacing in, so even a short way gets an arrow.
            let mut since_arrow = ARROW_SPACING_PX / 2.0;
            for (a, b) in decimate_segments(&scratch_pts, MIN_SEGMENT_PX) {
                let (p0, p1) = (scratch_pts[a], scratch_pts[b]);
                if let Some(oi) = overlay_idx {
                    let og = &mut overlay_groups[oi];
                    push_segment_quad(
                        &mut og.path,
                        &mut og.bounds_min_max,
                        p0,
                        p1,
                        group.half_width + CASING_WIDTH + ROUTE_OVERLAY_EXTRA,
                    );
                }
                if draw_casing {
                    push_segment_quad(
                        &mut casing_path,
                        &mut casing_bounds,
                        p0,
                        p1,
                        group.half_width + CASING_WIDTH,
                    );
                }
                push_segment_quad(
                    &mut group.path,
                    &mut group.bounds_min_max,
                    p0,
                    p1,
                    group.half_width,
                );

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
                let half_base = group.half_width + ARROW_OVERHANG;
                if let Some([tip, left, right]) =
                    arrow_triangle(p0, p1, reverse, ARROW_HALF_LENGTH, half_base)
                {
                    push_triangle(&mut group.path, &mut group.bounds_min_max, tip, left, right);
                }
                if draw_casing {
                    if let Some([tip, left, right]) = arrow_triangle(
                        p0,
                        p1,
                        reverse,
                        ARROW_HALF_LENGTH + 2.0 * CASING_WIDTH,
                        half_base + 1.5 * CASING_WIDTH,
                    ) {
                        push_triangle(&mut casing_path, &mut casing_bounds, tip, left, right);
                    }
                }
            }
        }

        crate::frame_stats::lap("osm.ways_build", fs_phase);
        let fs_phase = crate::frame_stats::start();
        for g in overlay_groups {
            if let Some(mut path) = g.path {
                let (min_x, max_x, min_y, max_y) = g.bounds_min_max;
                path.bounds = Bounds {
                    origin: point(px(min_x), px(min_y)),
                    size: size(px(max_x - min_x), px(max_y - min_y)),
                };
                window.paint_path(path, rgba(g.rgba));
            }
        }

        if let Some(mut path) = casing_path {
            let (min_x, max_x, min_y, max_y) = casing_bounds;
            path.bounds = Bounds {
                origin: point(px(min_x), px(min_y)),
                size: size(px(max_x - min_x), px(max_y - min_y)),
            };
            window.paint_path(path, rgb(CASING_COLOR));
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
        // tessellator is not tuned for thousands of tiny rectangles. (The
        // simple markers of tagged nodes below are batched as raw triangles,
        // with no tessellation, and are several times cheaper than quads.)
        //
        // Per-node style is resolved once at data-load time (see
        // `compute_node_cache`/`commit_node_moves`/`set_stylesheet`) and
        // just read here — no stylesheet call or tags HashMap lookup per
        // node per frame.
        crate::frame_stats::lap("osm.ways_paint", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let show_vertices = zoom >= VERTEX_MIN_ZOOM;
        let mut nodes_in_view = 0usize;
        let mut markers_drawn = 0usize;
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
            let form = node_markers::node_form(style.tagged, false, zoom);
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
            window.paint_path(path, rgb(node_markers::SIMPLE_COLOR));
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
        let fs_phase = crate::frame_stats::start();
        OsmLayer::commit_node_moves(self, moves);
        crate::frame_stats::lap("edit.commit_node_moves_total", fs_phase);
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

    fn reverse_way(&mut self, way_id: i64) -> bool {
        OsmLayer::reverse_way(self, way_id)
    }

    fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> bool {
        OsmLayer::set_way_nodes(self, way_id, node_ids)
    }

    fn ways_using_node(&self, node_id: i64) -> Vec<i64> {
        OsmLayer::ways_using_node(self, node_id)
    }

    fn relations_containing_way(&self, way_id: i64) -> Vec<i64> {
        OsmLayer::relations_containing_way(self, way_id)
    }
    fn relations_containing_node(&self, node_id: i64) -> Vec<i64> {
        OsmLayer::relations_containing_node(self, node_id)
    }

    fn relation(&self, relation_id: i64) -> Option<crate::osm::OsmRelation> {
        OsmLayer::relation(self, relation_id)
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
    ) -> Option<Vec<crate::osm::OsmMember>> {
        OsmLayer::set_relation_members(self, relation_id, members)
    }

    fn relations(&self) -> Vec<crate::osm::OsmRelation> {
        OsmLayer::relations(self)
    }

    fn relations_containing_relation(&self, relation_id: i64) -> Vec<i64> {
        OsmLayer::relations_containing_relation(self, relation_id)
    }

    fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<crate::osm::OsmMember>,
    ) -> Option<i64> {
        OsmLayer::create_relation(self, tags, members)
    }

    fn delete_relation(
        &mut self,
        relation_id: i64,
    ) -> Option<crate::selection::DeletedRelationSnapshot> {
        OsmLayer::delete_relation(self, relation_id)
    }

    fn restore_relation(&mut self, snapshot: crate::selection::DeletedRelationSnapshot) -> bool {
        OsmLayer::restore_relation(self, snapshot)
    }

    fn hit_test(&self, viewport: &Viewport, screen_pt: Point<Pixels>) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = node_markers::FULL_HIT_RADIUS;
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
        OsmLayer::merge_download(self, incoming)
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

    fn set_base_version(&mut self, kind: FeatureKind, id: i64, version: i32) -> bool {
        OsmLayer::set_base_version(self, kind, id, version)
    }

    fn adopt_server_element(
        &mut self,
        kind: FeatureKind,
        id: i64,
        server: crate::osm_api::ServerElement,
    ) -> bool {
        OsmLayer::adopt_server_element(self, kind, id, server)
    }

    fn forget_local_delete(&mut self, kind: FeatureKind, id: i64) -> bool {
        OsmLayer::forget_local_delete(self, kind, id)
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
                let node_style = self.stylesheet.node_style(&n.tags);
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
                                node_styles.push(self.stylesheet.node_style(&n.tags));
                            }
                        }
                    }
                }
                if pts.len() < 2 {
                    return;
                }
                // A relation can have hundreds of member ways, most of them
                // off screen at any zoom that shows a few: skip those.
                if kind == HighlightKind::Member {
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
                let way_style = self.stylesheet.way_style(&way.tags, way.is_closed());
                for (extra_width, color) in [(7.0, halo), (3.0, core)] {
                    let mut builder = PathBuilder::stroke(px(way_style.width + extra_width));
                    for (i, p) in pts.iter().enumerate() {
                        if i == 0 {
                            builder.move_to(*p);
                        } else {
                            builder.line_to(*p);
                        }
                    }
                    if let Ok(path) = builder.build() {
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
                for (p, style) in pts.iter().zip(node_styles.iter()) {
                    if kind == HighlightKind::Selected
                        || node_markers::node_form(style.tagged, false, zoom).is_shown()
                    {
                        paint_node_marker(window, *p, *style);
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
    use crate::layers::{EditableLayer, LayerId, MapLayer};
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
        layer.commit_node_moves(&[(1, 41.0, -75.0), (2, 41.001, -75.001)]);

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
        layer.restore_feature(node_snapshot.clone());
        layer.restore_feature(way_snapshot.clone());
        assert_eq!(layer.get_osm_data().unwrap().nodes[&3].version, 4);
        assert_eq!(layer.get_osm_data().unwrap().ways[&10].version, 7);
        // Undone: nothing to upload yet.
        assert!(layer.diff_for_upload().is_empty());

        layer.set_tag(FeatureKind::Way, 10, "highway", "path");
        layer.set_tag(FeatureKind::Node, 3, "amenity", "cafe");
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

        assert_eq!(layer.set_relation_members(404, vec![]), None);
        assert!(!layer.is_modified(), "unknown relation changes nothing");

        let previous = layer.set_relation_members(500, vec![member(10), member(11)]);
        assert_eq!(previous, Some(vec![member(10)]));
        assert!(layer.is_modified());
        assert_eq!(layer.relation(500).unwrap().members.len(), 2);
        assert_eq!(layer.relations_containing_way(11), vec![500]);
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_relations.len(), 1);
        assert_eq!(diff.modified_relations[0].version, 3);

        // Putting the old list back leaves nothing to upload.
        layer.set_relation_members(500, vec![member(10)]);
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

        layer.set_tag(FeatureKind::Relation, 500, "name", "Blue Line");
        assert!(layer.is_modified());
        assert_eq!(EditableLayer::feature_tags(&layer, &rel).unwrap().len(), 2);
        let diff = layer.diff_for_upload();
        assert_eq!(diff.modified_relations.len(), 1);
        assert_eq!(diff.modified_relations[0].tags["name"], "Blue Line");

        layer.remove_tag(FeatureKind::Relation, 500, "name");
        assert!(layer.diff_for_upload().is_empty(), "back to the original");

        // A relation can't be deleted yet, and a missing one is ignored.
        assert!(layer.delete_feature(FeatureKind::Relation, 500).is_none());
        assert!(layer.relation(500).is_some());
        layer.set_tag(FeatureKind::Relation, 404, "a", "b");
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
        assert!(layer.delete_relation(child).is_none());
        assert!(layer.relation(child).is_some());
        assert!(layer.delete_relation(404).is_none());

        // A server relation: delete, restore at the same position, version
        // intact, and nothing left to upload.
        let before = layer.relations();
        let snapshot = layer.delete_relation(500).unwrap();
        assert_eq!(snapshot.relation.version, 7);
        assert_eq!(snapshot.index, 0);
        assert_eq!(layer.diff_for_upload().deleted_relation_ids, vec![(500, 7)]);
        assert!(layer.restore_relation(snapshot.clone()));
        assert!(!layer.restore_relation(snapshot), "id already present");
        assert_eq!(layer.relations(), before);
        assert!(layer.diff_for_upload().deleted_relation_ids.is_empty());

        // Once the container is gone the member can go too, and redo of the
        // create (restore) brings back the same ids.
        let parent_snapshot = layer.delete_relation(parent).unwrap();
        let child_snapshot = layer.delete_relation(child).unwrap();
        assert!(layer.diff_for_upload().created_relations.is_empty());
        assert!(layer.restore_relation(child_snapshot));
        assert!(layer.restore_relation(parent_snapshot));
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
        layer.set_relation_members(500, members);

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
    fn reverse_way_flips_node_order_and_is_its_own_inverse() {
        let mut layer = OsmLayer::new(LayerId(1));
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let n3 = layer.add_node(40.001, -74.0);
        let way_id = layer.add_way(vec![n1, n2, n3], vec![]);

        assert!(layer.reverse_way(way_id));
        assert_eq!(layer.way_node_ids(way_id), Some(vec![n3, n2, n1]));
        assert!(layer.is_modified());

        assert!(layer.reverse_way(way_id));
        assert_eq!(layer.way_node_ids(way_id), Some(vec![n1, n2, n3]));

        assert!(!layer.reverse_way(999));
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
        assert!(layer.set_way_nodes(w2, vec![copy, n3]));
        assert_eq!(layer.way_node_ids(w2), Some(vec![copy, n3]));
        assert_eq!(layer.ways_using_node(n2), vec![w1]);
        assert_eq!(layer.ways_using_node(copy), vec![w2]);

        // A node no way uses any more can be deleted again.
        assert!(layer.set_way_nodes(w2, vec![n3, n1]));
        assert!(layer.delete_feature(FeatureKind::Node, copy).is_some());
        assert!(!layer.set_way_nodes(999, vec![]));
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
            layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
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
            layer.set_tag(FeatureKind::Way, 10, "highway", "path");
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
            layer.set_tag(FeatureKind::Way, 10, "highway", "path");
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
            layer.set_tag(FeatureKind::Node, 1, "a", "b");
            layer.set_tag(FeatureKind::Node, 2, "a", "b");
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
    /// latitude 40, plus a tagged node (id 1000) beside the middle one.
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
    fn vertex_is_a_target_from_the_threshold() {
        let layer = way_with_poi_layer(60);
        let vp = view_at(node_markers::UNTAGGED_MIN_ZOOM);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert_eq!(hits[0].kind, FeatureKind::Node);
        assert_eq!(hits[0].feature.id, 30);
    }

    #[test]
    fn simple_tagged_node_is_a_target_at_low_zoom() {
        let layer = way_with_poi_layer(60);
        for z in [12.0, 16.0] {
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
            for id in (1..=60).chain([1000]) {
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
                if id == 1000 {
                    assert!(boxed.contains(&id), "z{z}: tagged node not boxed");
                } else if z < node_markers::UNTAGGED_MIN_ZOOM {
                    assert!(!boxed.contains(&id), "z{z}: hidden vertex {id} boxed");
                } else if in_view {
                    assert!(boxed.contains(&id), "z{z}: vertex {id} not boxed");
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
        assert_eq!(layer.node_form_at(&pins, vertex, 3, 18.5), NodeForm::Full);
        assert_eq!(
            layer.node_form_at(&pins, poi, 1000, 17.0),
            NodeForm::Simple(node_markers::SIMPLE_SIZE)
        );
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
        layer.remove_tag(FeatureKind::Relation, 500, "natural");
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
        layer.commit_node_moves(&[(nid(30, 0), 40.0, -73.969)]);
        assert!(layer.multipolygon_version() > before);

        // A node no relation uses: no patch at all.
        let before = layer.multipolygon_version();
        layer.commit_node_moves(&[(nid(12, 12), 40.0121, -73.9881)]);
        assert_eq!(layer.multipolygon_version(), before);

        layer.commit_node_moves(&[(nid(10, 10), 40.0105, -73.9895)]);
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
        assert!(layer.set_way_nodes(10, ring[..4].to_vec()));
        assert_eq!(
            layer.multipolygon_state(500),
            MpState::Invalid(Problem::Unclosed)
        );
        // (An open way has no interior either, so nothing is selectable.)
        assert_eq!(double_click(&layer, 1.5, 8.5), None);
        // ...and putting it back (the undo) restores it.
        assert!(layer.set_way_nodes(10, ring));
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Deleting the island way (a member) leaves a relation with a missing
        // member; restoring it brings the hole back.
        let snapshot = layer.delete_feature(FeatureKind::Way, 11).unwrap();
        assert_eq!(
            layer.multipolygon_state(500),
            MpState::Invalid(Problem::MissingWay(11))
        );
        layer.restore_feature(snapshot);
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Adding a vertex to the outer way keeps it valid and re-triangulates.
        layer.insert_node_into_way(10, 1, 39.9997, -73.995);
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));
        let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
        assert_eq!(tris, 9);
        layer.remove_node_from_way(10, 1);
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
        layer.set_relation_members(500, previous);
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // The island as an outer too: two polygons, no hole.
        layer.set_relation_members(500, vec![member(10, "outer"), member(11, "outer")]);
        assert_eq!(layer.multipolygon_state(500), valid(2, 0));
        layer.set_relation_members(500, vec![member(10, "outer"), member(11, "inner")]);

        // Removing the type makes it a plain relation; putting it back
        // makes it a multipolygon again.
        layer.remove_tag(FeatureKind::Relation, 500, "type");
        assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
        assert_eq!(double_click(&layer, 1.5, 8.5), Some((FeatureKind::Way, 10)));
        layer.set_tag(FeatureKind::Relation, 500, "type", "multipolygon");
        assert_eq!(layer.multipolygon_state(500), valid(1, 1));

        // Delete and restore the relation (undo of a delete).
        let snapshot = layer.delete_relation(500).unwrap();
        assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
        assert!(layer.restore_relation(snapshot));
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
        layer.set_tag(FeatureKind::Relation, 500, "natural", "bare_rock");
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
        layer.set_tag(FeatureKind::Relation, 10, "type", "route_master");
        assert_eq!(layer.route_overlay_colour(1), None);
        assert_eq!(layer.route_overlay_colour(2), Some(0xFF0000));
        layer.set_tag(FeatureKind::Relation, 10, "type", "route");
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
        layer.commit_node_moves(&[(2, 40.0005, -73.998)]);
        let (_, x, y, _) = layer.restriction_markers()[0];
        let (ex, ey) = lat_lon_to_mercator(40.0005, -73.998);
        assert!((x - ex).abs() < 1e-6 && (y - ey).abs() < 1e-6);

        // Node 4 is a standalone via, so it can be deleted.
        let snapshot = layer.delete_feature(FeatureKind::Node, 4).unwrap();
        assert_eq!(layer.restriction_markers().len(), 1);
        layer.restore_feature(snapshot);
        assert_eq!(layer.restriction_markers().len(), 2);
    }

    #[test]
    fn clicking_the_marker_selects_the_restriction_only_at_vertex_zoom() {
        let layer = layer();
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
        // The via node itself is still the node.
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
