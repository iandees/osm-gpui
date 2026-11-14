use crate::coordinates::{is_point_valid, lat_lon_to_mercator, validate_coords};
use crate::layers::area_labels;
use crate::layers::diff::{baseline_for_loaded_file, diff_osm_data, LayerDiff};
use crate::layers::labels;
use crate::layers::merge::{self, MergeStats};
use crate::layers::node_markers::{self, NodeForm};
use crate::layers::resolve;
use crate::layers::way_stroke::{self, Layer, PathSink};
use crate::layers::{EditError, EditableLayer, HighlightKind, LayerId, LayerKind, MapLayer};
use crate::osm::{FileEdits, OsmData, OsmNode, OsmWay};
use crate::osm_upload::UploadResult;
use crate::selection::{
    nearest_point_on_segment, point_to_segment_distance, DeletedFeatureSnapshot,
    DeletedRelationSnapshot, FeatureKind, FeatureRef, HitCandidate, PurgedFeatures,
};
use crate::style::mapcss::{NodeShape, NodeVisual};
use crate::style::{Direction, NodeStyle, Stylesheet, TextPosition, WayStyle};
use crate::viewport::Viewport;
use gpui::*;
use rstar::{
    primitives::{GeomWithData, Rectangle},
    RTree, AABB,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

mod change;
mod derived;
mod edit;
mod fill;
mod highlight;
mod hit_test;
#[cfg(test)]
mod label_order_tests;
mod label_pass;
mod markers;
mod multipolygon;
mod scene;
mod upload;

use self::change::*;
use self::derived::*;
use self::hit_test::*;
use self::markers::*;
use self::multipolygon::*;
use self::scene::*;

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
/// are re-resolved per zoom bucket; nodes are not).
const NODE_STYLE_ZOOM: f64 = 18.0;

/// Below this zoom turn-restriction markers aren't drawn or clickable.
/// (Which node markers are shown is decided by `node_markers`.)
const VERTEX_MIN_ZOOM: f64 = 16.0;

/// Layer for rendering OSM vector data (nodes and ways)
pub struct OsmLayer {
    id: LayerId,
    name: String,
    visible: bool,
    store: Store,
    derived: Derived,
    render: RenderCaches,
    view: ViewState,
}

/// Authoritative state: the data, its baseline and the counters edits allocate from.
struct Store {
    osm_data: Option<Arc<OsmData>>,
    /// The data as it was when this layer was last loaded/downloaded (or
    /// last reconciled with a successful upload) — the stable "before"
    /// state `diff_for_upload` compares `osm_data` against. In-place edits
    /// only mutate `osm_data` and leave this alone.
    original_data: Option<Arc<OsmData>>,
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
    /// Stylesheet used to pick per-feature colors/weights from OSM tags.
    stylesheet: Arc<Stylesheet>,
}

/// Tables derived from `Store`, read by both rendering and hit testing; the edit primitives patch them in place.
struct Derived {
    /// OSM way id for each entry of `way_bboxes`/`way_vertices`/`way_styles`,
    /// aligned by index. `osm_data.ways` is a `HashMap` keyed by id (not
    /// positionally ordered), so this is what lets index-aligned code
    /// recover a way's id from a position in those parallel arrays.
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
    /// Precomputed mercator positions (+ resolved style) for every node.
    node_cache: NodeCache,
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
    /// reference it; lets an edit recompute only the affected ways' caches
    /// instead of every way.
    node_to_ways: HashMap<i64, HashSet<usize>>,
    /// Union AABB (mercator) of every node in this layer. Used as a cheap
    /// early-out in `render_canvas` so off-screen datasets do zero
    /// per-vertex work. `None` when there's no data.
    layer_bbox: Option<WayBbox>,
    /// Assembled and triangulated multipolygon relations; see [`MpCache`].
    mp: MpCache,
}

/// Caches only the render path reads.
struct RenderCaches {
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
    /// The downloaded rectangles (`OsmData::bounds`) projected to mercator,
    /// so a frame does no trig for them. Rebuilt wherever `osm_data`'s bounds
    /// can change: construction, `set_osm_data` (which `merge_download` ends
    /// in) and `clear_osm_data`.
    download_rects: Vec<crate::download_bounds::MercRect>,
    /// Sizes of last frame's way-pass batches (see `VertexHints`).
    vertex_hints: VertexHints,
    /// Label candidates, shaped text and the last placement; see
    /// [`labels`]. A mutex because painting takes `&self` and layers are
    /// `Sync`; it is only ever locked on the UI thread.
    label_cache: std::sync::Mutex<labels::LabelCache>,
    /// Street-name labels along ways; see `line_labels`.
    line_labels: std::sync::Mutex<crate::layers::line_labels::LineLabelCache>,
    /// The street-name labels chosen at the last settle.
    line_placed: std::sync::Mutex<crate::layers::line_labels::LineState>,
}

/// Transient view state; never touches `Store`.
struct ViewState {
    /// Feature to highlight (set each frame by MapViewer).
    highlight: Vec<FeatureRef>,
    /// The way under the pointer in Select mode, whose vertices are shown
    /// (small and light) and can be hovered, clicked and dragged. Set by
    /// `update_vertex_preview`; costs nothing while `None`.
    preview_way: Option<i64>,
    /// Transient screen-space offset applied to the given node ids while
    /// rendering, for live drag feedback. Never touches `osm_data`.
    drag_preview: Option<(HashSet<i64>, Point<Pixels>)>,
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

impl OsmLayer {
    /// An empty layer using the built-in default stylesheet. For tests,
    /// examples and benches; the app builds layers with `new_styled` and the
    /// currently selected style.
    pub fn new(id: LayerId) -> Self {
        Self::new_styled(id, crate::style::custom::built_in())
    }

    /// An empty layer that resolves its styles from `stylesheet`.
    pub fn new_styled(id: LayerId, stylesheet: Arc<Stylesheet>) -> Self {
        Self {
            id,
            name: "OSM Data".to_string(),
            visible: true,
            store: Store {
                osm_data: None,
                original_data: None,
                modified: false,
                revision: 0,
                next_new_id: -1,
                next_placeholder_way_id: -1,
                stylesheet,
            },
            derived: Derived {
                way_ids: Vec::new(),
                way_bboxes: Vec::new(),
                way_vertices: Vec::new(),
                node_cache: NodeCache {
                    index_by_id: HashMap::new(),
                    flat: Vec::new(),
                    styles: Vec::new(),
                },
                node_index: RTree::new(),
                way_index: RTree::new(),
                way_id_to_index: HashMap::new(),
                node_to_ways: HashMap::new(),
                layer_bbox: None,
                mp: MpCache::default(),
            },
            render: RenderCaches {
                way_styles: Vec::new(),
                way_fill_tris: Vec::new(),
                member_ways: HashSet::new(),
                download_rects: Vec::new(),
                vertex_hints: VertexHints::default(),
                label_cache: Default::default(),
                line_labels: Default::default(),
                line_placed: Default::default(),
            },
            view: ViewState {
                highlight: Vec::new(),
                preview_way: None,
                drag_preview: None,
            },
        }
    }

    /// A layer over `osm_data` using the built-in default stylesheet. For
    /// tests, examples and benches; the app uses `new_with_data_styled`.
    pub fn new_with_data<N: Into<String>>(id: LayerId, name: N, osm_data: Arc<OsmData>) -> Self {
        Self::new_with_data_styled(id, name, osm_data, crate::style::custom::built_in())
    }

    /// A layer over `osm_data` that resolves its styles from `stylesheet`.
    pub fn new_with_data_styled<N: Into<String>>(
        id: LayerId,
        name: N,
        osm_data: Arc<OsmData>,
        stylesheet: Arc<Stylesheet>,
    ) -> Self {
        let fs_load = crate::frame_stats::start();
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
            store: Store {
                osm_data: Some(osm_data.clone()),
                original_data: Some(osm_data),
                modified: false,
                revision: 0,
                next_new_id,
                next_placeholder_way_id,
                stylesheet,
            },
            derived: Derived {
                way_ids,
                way_bboxes,
                way_vertices,
                node_cache,
                node_index,
                way_index,
                way_id_to_index,
                node_to_ways,
                layer_bbox,
                mp,
            },
            render: RenderCaches {
                way_styles,
                way_fill_tris,
                member_ways,
                download_rects: crate::download_bounds::merc_rects(&osm_data_bounds),
                vertex_hints: VertexHints::default(),
                label_cache: Default::default(),
                line_labels: Default::default(),
                line_placed: Default::default(),
            },
            view: ViewState {
                highlight: Vec::new(),
                preview_way: None,
                drag_preview: None,
            },
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
        Self::new_with_edits_styled(id, name, osm_data, edits, crate::style::custom::built_in())
    }

    /// `new_with_edits` that resolves its styles from `stylesheet`.
    pub fn new_with_edits_styled<N: Into<String>>(
        id: LayerId,
        name: N,
        osm_data: Arc<OsmData>,
        edits: &FileEdits,
        stylesheet: Arc<Stylesheet>,
    ) -> Self {
        let baseline = baseline_for_loaded_file(&osm_data, edits);
        let mut layer = Self::new_with_data_styled(id, name, osm_data.clone(), stylesheet);
        layer.store.modified = !diff_osm_data(&baseline, &osm_data).is_empty();
        layer.store.revision += 1;
        layer.store.original_data = Some(Arc::new(baseline));
        layer
    }

    /// This layer as a JOSM-style `.osm` document, including its pending
    /// edits; see `osm_export`. `None` when the layer holds no data.
    pub fn export_xml(&self) -> Option<String> {
        let current = self.store.osm_data.as_deref()?;
        let original = self.store.original_data.as_deref().unwrap_or(current);
        Some(crate::osm_export::to_osm_xml(original, current))
    }

    /// Load `osm_data`, rebuilding every cache from scratch. It becomes the
    /// server baseline too (`original_data`), so `modified` is cleared.
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.rebuild_caches(&osm_data);
        self.store.osm_data = Some(osm_data.clone());
        self.store.original_data = Some(osm_data);
        self.store.modified = false;
        self.store.revision += 1;
    }

    /// The rectangles (Web Mercator meters) this layer's data was downloaded
    /// for; empty when that is not known. See `render_all_download_bounds`.
    pub fn download_rects(&self) -> &[crate::download_bounds::MercRect] {
        &self.render.download_rects
    }

    /// Whether the layer has edits not yet written to a file or uploaded.
    pub fn is_modified(&self) -> bool {
        self.store.modified
    }

    /// Re-derive `is_modified` from the layer's actual difference to its
    /// server baseline. Edits (and the undo of one) set the flag without
    /// looking at the diff, so after an undo back to the baseline this is
    /// what clears it.
    pub fn refresh_modified(&mut self) {
        if self.store.modified {
            self.store.modified = !self.diff_for_upload().is_empty();
        }
    }

    /// How many nodes and ways the layer holds; `None` before any data is
    /// loaded.
    pub fn feature_counts(&self) -> Option<(usize, usize)> {
        let data = self.store.osm_data.as_ref()?;
        Some((data.nodes.len(), data.ways.len()))
    }

    /// The layer's change counter; see the `revision` field.
    pub fn edit_revision(&self) -> u64 {
        self.store.revision
    }

    /// Test-only: replace `osm_data` directly, leaving `original_data` and
    /// the derived caches alone.
    #[cfg(test)]
    pub(crate) fn set_osm_data_for_test(&mut self, data: Arc<OsmData>) {
        self.store.osm_data = Some(data);
        self.store.modified = true;
        self.store.revision += 1;
    }

    /// The data as a snapshot: edits go through `Arc::make_mut`, so a held
    /// snapshot never changes, and costs the next edit one copy of the
    /// dataset. Keep snapshots short-lived.
    pub fn get_osm_data(&self) -> Option<Arc<OsmData>> {
        self.store.osm_data.clone()
    }

    /// Clear the OSM data
    pub fn clear_osm_data(&mut self) {
        self.store.osm_data = None;
        self.store.original_data = None;
        self.derived.way_ids.clear();
        self.derived.way_bboxes.clear();
        self.derived.way_vertices.clear();
        self.render.way_styles.clear();
        self.render.way_fill_tris.clear();
        self.derived.mp = MpCache {
            version: self.derived.mp.version + 1,
            ..MpCache::default()
        };
        self.derived.layer_bbox = None;
        self.render.download_rects.clear();
        self.derived.node_cache.index_by_id.clear();
        self.derived.node_cache.flat.clear();
        self.derived.node_cache.styles.clear();
        self.derived.node_index = RTree::new();
        self.derived.way_index = RTree::new();
        self.derived.way_id_to_index.clear();
        self.derived.node_to_ways.clear();
        self.view.drag_preview = None;
        self.store.modified = false;
        self.store.revision += 1;
        self.store.next_new_id = -1;
        self.store.next_placeholder_way_id = -1;
    }

    /// Check if this layer has data
    pub fn has_data(&self) -> bool {
        self.store.osm_data.is_some()
    }
}

impl MapLayer for OsmLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn kind(&self) -> LayerKind {
        LayerKind::Osm
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
        // Everything is painted in `render_canvas`, which avoids per-node
        // element layout.
        Vec::new()
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.store.osm_data.is_none() {
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
        if let Some(lb) = &self.derived.layer_bbox {
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
        // fill path is much slower — Lyon's fill
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
        for (idx, &(id, mx, my)) in self.derived.node_cache.flat.iter().enumerate() {
            if mx < vmin_x || mx > vmax_x || my < vmin_y || my > vmax_y {
                continue;
            }
            nodes_in_view += 1;
            // Selected and hovered nodes and selected ways' nodes are painted
            // in full by `render_highlight`, so only the zoom rule applies.
            let style = self.derived.node_cache.styles[idx];
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
            .view
            .preview_way
            .filter(|_| zoom >= node_markers::PREVIEW_MIN_ZOOM)
            .and_then(|w| self.derived.way_id_to_index.get(&w).copied())
        {
            let pins = self.pins();
            for &(id, mx, my) in self.derived.way_vertices[wi].iter() {
                let Some(&idx) = self.derived.node_cache.index_by_id.get(&id) else {
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
            for r in self.derived.mp.restrictions.values() {
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
        if let Some(ref osm_data) = self.store.osm_data {
            vec![
                ("Nodes".to_string(), osm_data.nodes.len().to_string()),
                ("Ways".to_string(), osm_data.ways.len().to_string()),
            ]
        } else {
            vec![("Status".to_string(), "No data loaded".to_string())]
        }
    }
}

#[cfg(test)]
#[path = "../osm_layer_tests/mod.rs"]
mod tests;
