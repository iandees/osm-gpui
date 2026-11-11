use gpui::*;
use std::collections::HashSet;

use crate::viewport::Viewport;

pub mod diff;
pub mod grid_layer;
pub mod merge;
pub mod node_markers;
pub mod osm_layer;
pub mod resolve;
pub mod tile_layer;
pub mod way_stroke;

/// Wash laid over the part of the map that was not downloaded (RGBA): a deep
/// slate at moderate opacity. Dimming alone barely shows on dark aerial
/// imagery, so the hatch below carries the "not downloaded" signal there.
const DOWNLOAD_WASH_RGBA: u32 = 0x0f172a70;
/// Light diagonal hatch drawn over the wash (RGBA), its line width and its
/// spacing along the x axis, in pixels. Visible on dark imagery, and on
/// light imagery because the wash underneath darkens it.
const DOWNLOAD_HATCH_RGBA: u32 = 0xffffff40;
const DOWNLOAD_HATCH_WIDTH: f32 = 1.0;
const DOWNLOAD_HATCH_SPACING: f32 = 9.0;
/// Colour of the line along the edge of the downloaded area (RGBA).
const DOWNLOAD_EDGE_RGBA: u32 = 0x1e3a8acc;
/// Width of that line, in pixels.
const DOWNLOAD_EDGE_WIDTH: f32 = 2.0;
/// Pale halo drawn under the edge line (RGBA) and its total width, so the
/// edge reads on dark aerial imagery as well as on light Carto.
const DOWNLOAD_HALO_RGBA: u32 = 0xf1f5f9f2;
const DOWNLOAD_HALO_WIDTH: f32 = 5.0;

/// Stable identity for a layer, independent of its (user-visible, possibly
/// renamed-for-uniqueness) display name. Allocated monotonically by
/// `LayerManager::alloc_id`; every layer stores the id it was allocated at
/// construction time and returns it from `MapLayer::id`.
///
/// The inner value is crate-visible (not private) so call sites across the
/// `osm_gpui` lib/bin split (e.g. `undo.rs`, tests) can construct one
/// directly when needed (tests, or reconstructing a value round-tripped
/// through undo state) without a real `LayerManager` — the only thing that
/// actually matters for correctness is that *production* ids always come
/// from `LayerManager::alloc_id`'s monotonic counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(pub u64);

/// Trait that all map layers must implement: the universal surface (name,
/// identity, visibility, rendering, stats, attribution). Vector-editing
/// operations (hit-testing, tags, node moves, create/delete...) live on
/// `EditableLayer` instead, reached via `as_editable`/`as_editable_mut` —
/// see that trait's doc comment.
pub trait MapLayer: Send + Sync {
    /// Get the name of this layer for debugging/UI purposes
    fn name(&self) -> &str;

    /// This layer's stable identity. See `LayerId`.
    fn id(&self) -> LayerId;

    /// Downcast support for callers that need `OsmLayer`-specific methods
    /// not otherwise part of this trait or `EditableLayer` (e.g. Extrude
    /// mode's segment hit-testing). For a simple "does this layer support
    /// editing at all" check, prefer `as_editable`/`as_editable_mut`
    /// instead — `is_some()` there already answers that without a downcast.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Check if this layer is currently visible
    fn is_visible(&self) -> bool;

    /// Set visibility of this layer
    fn set_visible(&mut self, visible: bool);

    /// Render this layer as GPUI elements (for raster/image content)
    fn render_elements(&self, viewport: &Viewport) -> Vec<AnyElement>;

    /// Render this layer using canvas drawing (for vector content)
    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window);

    /// The rectangles (Web Mercator meters) this layer's data was downloaded
    /// for; empty when that is not known. See `render_all_download_bounds`.
    fn download_rects(&self) -> &[crate::download_bounds::MercRect] {
        &[]
    }

    /// Update this layer (called on each frame)
    fn update(&mut self) {}

    /// Get layer statistics for debugging
    fn stats(&self) -> Vec<(String, String)> {
        vec![]
    }

    /// Whether this layer has uncommitted-to-disk edits (e.g. moved nodes).
    /// Default: `false`. Kept on `MapLayer` (rather than gated behind
    /// `as_editable`) since it's queried uniformly across every layer, both
    /// for the quit-confirmation check and the per-layer "modified" dot in
    /// the side panel — routing it through `as_editable` would just push an
    /// `unwrap_or(false)` onto every call site for no benefit.
    fn is_modified(&self) -> bool {
        false
    }

    /// Required source-credit (text and optional link) for this layer's
    /// content, if any (e.g. "© OpenStreetMap contributors"). Default:
    /// `None`. Layers that render tiles from a source requiring attribution
    /// should override this.
    fn attribution(&self) -> Option<&crate::imagery::AttributionInfo> {
        None
    }

    /// The imagery shift of a tile layer (`None` for every other kind of
    /// layer). Only where imagery pixels are painted moves; see
    /// `imagery_offset`.
    fn imagery_offset(&self) -> Option<crate::imagery_offset::ImageryOffset> {
        None
    }

    /// Set the imagery shift; ignored by layers that are not imagery.
    fn set_imagery_offset(&mut self, _offset: crate::imagery_offset::ImageryOffset) {}

    /// Stable identity of the imagery source (ELI id, `osm-carto`, or
    /// `custom:<url template>`), the key under which offsets are remembered.
    fn imagery_id(&self) -> Option<&str> {
        None
    }

    /// Compute the set of created/modified/deleted nodes/ways needed to
    /// bring the server up to date with this layer's current data, relative
    /// to whatever snapshot it was last loaded/synced from. Default: an
    /// empty diff (nothing to upload). Only `OsmLayer` overrides this.
    ///
    /// Kept on `MapLayer` (like `is_modified`) rather than gated behind
    /// `as_editable`: callers iterate every layer uniformly to build an
    /// upload summary without first checking which ones are editable.
    fn diff_for_upload(&self) -> crate::layers::diff::LayerDiff {
        crate::layers::diff::LayerDiff::default()
    }

    /// Apply a successful changeset upload's result (server-assigned ids/
    /// versions) back into this layer's data, then mark it clean. Default:
    /// no-op. Only `OsmLayer` overrides this.
    fn apply_upload_result(&mut self, _result: &crate::osm_upload::UploadResult) {}

    /// Apply one accepted chunk of a multi-changeset upload (see
    /// `OsmLayer::apply_chunk_result`): the layer keeps whatever has not been
    /// uploaded yet as pending. Default: no-op.
    fn apply_chunk_result(
        &mut self,
        _result: &crate::osm_upload::UploadResult,
        _chunk: &crate::layers::diff::LayerDiff,
    ) {
    }

    /// This layer's data as a JOSM-style `.osm` document, including pending
    /// local edits, or `None` if it has nothing to export. Default: `None`.
    /// Only `OsmLayer` overrides this.
    fn export_xml(&self) -> Option<String> {
        None
    }

    /// A counter that changes whenever the layer's data or baseline does
    /// (see `OsmLayer`); the autosave uses it to skip layers that have not
    /// changed since they were last written. Default: always 0.
    fn edit_revision(&self) -> u64 {
        0
    }

    /// The layer's current data and its server baseline, shared (not copied)
    /// so a background thread can serialize them; see
    /// `OsmLayer::autosave_snapshot`. Default: `None`.
    fn autosave_snapshot(
        &self,
    ) -> Option<(
        std::sync::Arc<crate::osm::OsmData>,
        std::sync::Arc<crate::osm::OsmData>,
    )> {
        None
    }

    /// This layer's vector-editing surface, if it has one. Default: `None`.
    /// `OsmLayer` is the only implementation today; `TileLayer`/`GridLayer`
    /// have no features to hit-test, tag, move, or delete.
    fn as_editable(&self) -> Option<&dyn EditableLayer> {
        None
    }

    /// Mutable counterpart to `as_editable`. Default: `None`.
    fn as_editable_mut(&mut self) -> Option<&mut dyn EditableLayer> {
        None
    }
}

/// Why a feature is being highlighted, which picks the overlay's colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// Part of the current selection.
    Selected,
    /// Under the cursor: what a click would select.
    Hovered,
    /// A member of the selected relation: drawn like a selection but in a
    /// different color, so it reads as "belongs to the selected thing".
    Member,
}

/// The vector-editing surface a layer may support: hit-testing, selection
/// highlight, tag read/write, node moves, and create/delete. Reached from a
/// `&dyn MapLayer`/`&mut dyn MapLayer` via `as_editable`/`as_editable_mut`.
/// `OsmLayer` is the only implementation; layers with no editable feature
/// data (tiles, the coordinate grid) simply don't implement this trait
/// rather than carrying a pile of no-op overrides.
pub trait EditableLayer {
    /// Return hit candidates near a screen-space point. Implementations
    /// should only return candidates within their own tolerance.
    fn hit_test(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<crate::selection::HitCandidate>;

    /// Return every feature inside a screen-space rectangle. Nodes: point
    /// inside the rect. Ways: fully enclosed (all vertices inside).
    fn hit_test_rect(
        &self,
        viewport: &Viewport,
        rect: Bounds<Pixels>,
    ) -> Vec<crate::selection::FeatureRef>;

    /// Merge freshly downloaded server data into this layer, keeping local
    /// edits. See `merge::merge_download` for the rules.
    fn merge_download(&mut self, incoming: crate::osm::OsmData) -> merge::MergeStats;

    /// Whether this layer holds the node, way or relation `(kind, id)`
    /// (locally deleted objects are not held).
    fn has_feature(&self, kind: crate::selection::FeatureKind, id: i64) -> bool;

    /// Whether `(kind, id)` is an object of the server that the user has
    /// deleted here: known to the layer's baseline, absent from its data.
    fn is_locally_deleted(&self, kind: crate::selection::FeatureKind, id: i64) -> bool;

    /// Upload-conflict resolution "keep mine": make `version` the base
    /// version of `(kind, id)` (see `resolve::set_base_version`). Returns
    /// whether the layer knew the object.
    fn set_base_version(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
        version: i32,
    ) -> bool;

    /// Upload-conflict resolution "use theirs": replace `(kind, id)` — and,
    /// for a way, its nodes — with the server's copy, or remove it if the
    /// server deleted it (see `resolve::adopt_server_element`). Returns
    /// whether anything changed.
    fn adopt_server_element(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
        server: crate::osm_api::ServerElement,
    ) -> bool;

    /// Forget a local delete of `(kind, id)` the server has already made
    /// (see `resolve::forget_local_delete`). Returns whether it did.
    fn forget_local_delete(&mut self, kind: crate::selection::FeatureKind, id: i64) -> bool;

    /// Every node and way this layer owns, nodes first, each sorted by id.
    /// Backs Edit > Select All and View > Zoom to Data.
    fn all_features(&self) -> Vec<crate::selection::FeatureRef>;

    /// Run a parsed search over this layer's data (Edit > Find). `selected`
    /// answers the `selected` filter; with `only_selected` only selected
    /// objects are reported. Same order as `all_features`. Default: no data.
    fn search_features(
        &self,
        _query: &crate::search::Query,
        _selected: &std::collections::HashSet<(crate::selection::FeatureKind, i64)>,
        _only_selected: bool,
        _view: Option<crate::search::GeoBounds>,
    ) -> Vec<crate::selection::FeatureRef> {
        Vec::new()
    }

    /// Return closed-way ("area") candidates whose *interior* contains
    /// `screen_pt`, ignoring outline distance. Separate from `hit_test`
    /// (which only tests distance to a way's edges) so plain clicks keep
    /// requiring the outline; callers opt into this for double-click.
    /// Default: no interior hit-testing.
    fn hit_test_interior(
        &self,
        _viewport: &Viewport,
        _screen_pt: Point<Pixels>,
    ) -> Vec<crate::selection::HitCandidate> {
        Vec::new()
    }

    /// Tell the layer which features are currently selected.
    fn set_highlight(&mut self, features: &[crate::selection::FeatureRef]);

    /// Return key/value tags for the given feature if this layer owns it.
    fn feature_tags(&self, feature: &crate::selection::FeatureRef)
        -> Option<Vec<(String, String)>>;

    /// Classify the geometry (point/vertex/line/area) of a feature this
    /// layer owns, for preset matching. Returns `None` if the feature
    /// doesn't belong to this layer or isn't found in its data.
    fn feature_geometry(
        &self,
        feature: &crate::selection::FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Option<crate::presets::Geometry>;

    /// Geometries whose presets may be offered for a feature, best first:
    /// just its own geometry, except a closed way, which can be an area or a
    /// line. Empty if the feature isn't found.
    fn feature_geometry_candidates(
        &self,
        feature: &crate::selection::FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Vec<crate::presets::Geometry>;

    /// Whether `feature` is a closed way owned by this layer.
    fn feature_is_closed_way(&self, feature: &crate::selection::FeatureRef) -> bool;

    /// Draw a highlight overlay for `feature` if it belongs to this layer.
    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &crate::selection::FeatureRef,
        kind: HighlightKind,
    );

    /// Set a transient screen-space offset to apply when rendering the given
    /// node ids, for live drag feedback. Does not touch the underlying data.
    fn set_drag_preview(&mut self, node_ids: &HashSet<i64>, delta: Point<Pixels>);

    /// Clear any transient drag preview.
    fn clear_drag_preview(&mut self);

    /// Current (lat, lon) of a node this layer owns, if any.
    fn node_lat_lon(&self, node_id: i64) -> Option<(f64, f64)>;

    /// The member node ids of a way this layer owns, if any.
    fn way_node_ids(&self, way_id: i64) -> Option<Vec<i64>>;

    /// Commit a set of `(node_id, new_lat, new_lon)` moves into this layer's
    /// data, rebuilding derived caches once.
    fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]);

    /// Set (insert or overwrite) a single tag on a feature this layer owns.
    fn set_tag(&mut self, kind: crate::selection::FeatureKind, id: i64, key: &str, value: &str);

    /// Remove a single tag key from a feature this layer owns.
    fn remove_tag(&mut self, kind: crate::selection::FeatureKind, id: i64, key: &str);

    /// Create a new, tag-less node at `(lat, lon)` and return its id.
    /// `id`, when given, forces the new node to use exactly that id
    /// (fails/returns `None` if a feature with that id already exists) —
    /// used by redo, so a recreated node reuses its original id rather than
    /// allocating a fresh one. When `id` is `None`, the layer allocates a
    /// fresh negative (not-yet-uploaded) id.
    fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Option<i64>;

    /// Delete a node or way this layer owns, returning a snapshot with
    /// enough information to restore it later (see
    /// `crate::selection::DeletedFeatureSnapshot`). `None` if nothing was
    /// deleted (feature not found, or — for a node — refused because it's
    /// still referenced by a way; see `OsmLayer::delete_feature`'s doc
    /// comment for that limitation).
    fn delete_feature(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
    ) -> Option<crate::selection::DeletedFeatureSnapshot>;

    /// Re-insert a feature previously removed by `delete_feature`, using
    /// exactly the id/tags/geometry captured in `snapshot`.
    fn restore_feature(&mut self, snapshot: crate::selection::DeletedFeatureSnapshot);

    /// Insert a brand-new node at `(lat, lon)` and return its id.
    fn add_node(&mut self, lat: f64, lon: f64) -> i64;

    /// Insert a brand-new way referencing existing node ids, with the given
    /// tags, and return its id.
    fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64;

    /// Append a node id to an existing way.
    fn extend_way(&mut self, way_id: i64, node_id: i64);

    /// Create a new node and splice it into an existing way at `index`,
    /// returning the new node's id.
    fn insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64;

    /// Remove a node this layer owns (must not still be referenced by any
    /// way).
    fn remove_node(&mut self, node_id: i64);

    /// Remove a way this layer owns (its member nodes are untouched).
    fn remove_way(&mut self, way_id: i64);

    /// Inverse of `insert_node_into_way`: remove the node at `index` from a
    /// way's node list without deleting the node.
    fn remove_node_from_way(&mut self, way_id: i64, index: usize);

    /// Reverse a way's node order. Its own inverse (applying it twice
    /// restores the original). Returns whether the way was found.
    fn reverse_way(&mut self, way_id: i64) -> bool;

    /// Replace a way's whole node list (the nodes must already exist).
    /// Returns whether the way was found.
    fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> bool;

    /// The ids of the ways whose node list includes `node_id`.
    fn ways_using_node(&self, node_id: i64) -> Vec<i64>;

    /// The ids of the relations that have `way_id` as a member.
    fn relations_containing_way(&self, way_id: i64) -> Vec<i64>;

    /// The ids of the relations that have `node_id` as a member.
    fn relations_containing_node(&self, node_id: i64) -> Vec<i64>;

    /// A copy of the relation with this id (tags and members), if the layer
    /// has it.
    fn relation(&self, relation_id: i64) -> Option<crate::osm::OsmRelation>;

    /// The ids of every relation the layer has, in the order it stores them.
    fn relation_ids(&self) -> Vec<i64>;

    /// Replace a relation's whole member list and return the previous one
    /// (`None`, changing nothing, if the layer has no such relation).
    fn set_relation_members(
        &mut self,
        relation_id: i64,
        members: Vec<crate::osm::OsmMember>,
    ) -> Option<Vec<crate::osm::OsmMember>>;

    /// Copies of every relation in the layer.
    fn relations(&self) -> Vec<crate::osm::OsmRelation>;

    /// The ids of the relations that have `relation_id` as a member.
    fn relations_containing_relation(&self, relation_id: i64) -> Vec<i64>;

    /// Create a relation with a fresh negative id (from the same pool new
    /// nodes and ways draw from) and return it; `None` if the layer has no
    /// data.
    fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<crate::osm::OsmMember>,
    ) -> Option<i64>;

    /// Delete a relation, returning a snapshot for `restore_relation`.
    /// `None`, changing nothing, if there is no such relation or it is a
    /// member of another relation (remove that membership first).
    fn delete_relation(
        &mut self,
        relation_id: i64,
    ) -> Option<crate::selection::DeletedRelationSnapshot>;

    /// Put a deleted (or, for redo, created) relation back with its original
    /// id, version, content and position. `false` if the id is taken.
    fn restore_relation(&mut self, snapshot: crate::selection::DeletedRelationSnapshot) -> bool;
}

/// Manager for all map layers
pub struct LayerManager {
    layers: Vec<Box<dyn MapLayer>>,
    /// Monotonic counter backing `alloc_id`. Starts at 1 (0 is never handed
    /// out, so it's safe to use as a sentinel if ever needed).
    next_id: u64,
}

impl Default for LayerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerManager {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            next_id: 1,
        }
    }

    /// Allocate a fresh, never-before-used `LayerId`. Callers construct
    /// their layer with this id (layer constructors take a `LayerId`) before
    /// handing it to `add_layer`.
    pub fn alloc_id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Add a new layer to the manager
    pub fn add_layer(&mut self, layer: Box<dyn MapLayer>) {
        self.layers.push(layer);
    }

    /// Remove the layer at `index`. Returns the removed layer, or `None` if
    /// `index` is out of bounds.
    pub fn remove_at(&mut self, index: usize) -> Option<Box<dyn MapLayer>> {
        if index >= self.layers.len() {
            return None;
        }
        Some(self.layers.remove(index))
    }

    /// Move the layer at `from` to position `to`. No-op if either index is
    /// out of bounds or if `from == to`.
    pub fn move_layer(&mut self, from: usize, to: usize) {
        let len = self.layers.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
    }

    /// Get all layers
    pub fn layers(&self) -> &[Box<dyn MapLayer>] {
        &self.layers
    }

    /// Get all mutable layers
    pub fn layers_mut(&mut self) -> &mut [Box<dyn MapLayer>] {
        &mut self.layers
    }

    /// Find a layer by id (immutable)
    pub fn find_layer(&self, id: LayerId) -> Option<&dyn MapLayer> {
        self.layers
            .iter()
            .find(|layer| layer.id() == id)
            .map(|b| b.as_ref())
    }

    /// Find a layer by id (mutable)
    pub fn find_layer_mut(&mut self, id: LayerId) -> Option<&mut (dyn MapLayer + '_)> {
        match self.layers.iter_mut().find(|layer| layer.id() == id) {
            Some(b) => Some(b.as_mut()),
            None => None,
        }
    }

    /// Find a layer by its exact display name. Used only for the "does a
    /// singleton layer like the built-in OSM Carto/Coordinate Grid already
    /// exist" checks at creation time, where there's no id yet to look up by
    /// — everywhere else, identity flows through `LayerId`.
    pub fn layer_named(&self, name: &str) -> Option<&dyn MapLayer> {
        self.layers
            .iter()
            .find(|layer| layer.name() == name)
            .map(|b| b.as_ref())
    }

    /// Return a display name based on `base` that's unique among current
    /// layers: `base` itself if unused, otherwise `"{base} (2)"`, `"{base}
    /// (3)"`, etc. — the first untaken candidate.
    pub fn unique_name(&self, base: &str) -> String {
        if self.layers.iter().all(|l| l.name() != base) {
            return base.to_string();
        }
        let mut i = 2;
        loop {
            let candidate = format!("{base} ({i})");
            if self.layers.iter().all(|l| l.name() != candidate) {
                return candidate;
            }
            i += 1;
        }
    }

    /// Render all visible layers as GPUI elements
    pub fn render_all_elements(&self, viewport: &Viewport) -> Vec<AnyElement> {
        let mut elements = Vec::new();

        for layer in &self.layers {
            if layer.is_visible() {
                elements.extend(layer.render_elements(viewport));
            }
        }

        elements
    }

    /// Dim the part of the view outside every downloaded rectangle of every
    /// visible layer, and outline the edge of the downloaded area. One
    /// region for all layers: area downloaded into any visible layer counts
    /// as known, so a second layer never dims the first one's data. Nothing
    /// is drawn while no visible layer knows its downloaded area. Call before
    /// `render_all_canvas` so features paint on top of the wash.
    pub fn render_all_download_bounds(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        let fs_wash = crate::frame_stats::start();
        let rects: Vec<crate::download_bounds::MercRect> = self
            .layers
            .iter()
            .filter(|layer| layer.is_visible())
            .flat_map(|layer| layer.download_rects().iter().copied())
            .collect();
        if rects.is_empty() {
            return;
        }
        let (min_x, max_x, min_y, max_y) = viewport.mercator_view_bounds();
        let region = crate::download_bounds::outside_region(
            crate::download_bounds::MercRect::new(min_x, max_x, min_y, max_y),
            &rects,
        );
        crate::frame_stats::lap("wash.region", fs_wash);

        // Mercator y grows northwards, screen y southwards.
        let to_screen = |mx: f64, my: f64| {
            let p = viewport.mercator_to_screen(mx, my);
            (
                p.x.as_f32() + bounds.origin.x.as_f32(),
                p.y.as_f32() + bounds.origin.y.as_f32(),
            )
        };
        let wash = rgba(DOWNLOAD_WASH_RGBA);
        let mut hatch = PathBuilder::stroke(px(DOWNLOAD_HATCH_WIDTH));
        let mut any_hatch = false;
        for r in &region.dim {
            let (x0, y0) = to_screen(r.min_x, r.max_y);
            let (x1, y1) = to_screen(r.max_x, r.min_y);
            if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
                continue;
            }
            window.paint_quad(fill(
                Bounds {
                    origin: point(px(x0), px(y0)),
                    size: size(px(x1 - x0), px(y1 - y0)),
                },
                wash,
            ));
            // Hatch: 45-degree lines x + y = c, clipped to the rectangle,
            // all in one path so it is a single draw call.
            let mut c = ((x0 + y0) / DOWNLOAD_HATCH_SPACING).floor() * DOWNLOAD_HATCH_SPACING;
            let c_end = x1 + y1;
            // Bound the line count so a huge rectangle at an extreme zoom
            // cannot stall painting.
            if (c_end - c) / DOWNLOAD_HATCH_SPACING > 4000.0 {
                continue;
            }
            while c <= c_end {
                let xa = x0.max(c - y1);
                let xb = x1.min(c - y0);
                if xa < xb {
                    hatch.move_to(point(px(xa), px(c - xa)));
                    hatch.line_to(point(px(xb), px(c - xb)));
                    any_hatch = true;
                }
                c += DOWNLOAD_HATCH_SPACING;
            }
        }
        if any_hatch {
            if let Ok(path) = hatch.build() {
                window.paint_path(path, rgba(DOWNLOAD_HATCH_RGBA));
            }
        }
        // A near-white halo under the dark blue line (a two-tone edge) keeps the edge visible over dark
        // imagery, where blue alone vanishes.
        for (width, color) in [
            (DOWNLOAD_HALO_WIDTH, DOWNLOAD_HALO_RGBA),
            (DOWNLOAD_EDGE_WIDTH, DOWNLOAD_EDGE_RGBA),
        ] {
            let half = width / 2.0;
            let color = rgba(color);
            for s in &region.outline {
                let (x0, y0) = to_screen(s.x0, s.y1);
                let (x1, y1) = to_screen(s.x1, s.y0);
                if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
                    continue;
                }
                // Axis-aligned, so a thin quad centred on the segment.
                window.paint_quad(fill(
                    Bounds {
                        origin: point(px(x0 - half), px(y0 - half)),
                        size: size(px(x1 - x0 + width), px(y1 - y0 + width)),
                    },
                    color,
                ));
            }
        }
        crate::frame_stats::lap("wash.total", fs_wash);
    }

    /// Render all visible layers using canvas drawing
    pub fn render_all_canvas(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        for layer in &self.layers {
            if layer.is_visible() {
                layer.render_canvas(viewport, bounds, window);
            }
        }
    }

    /// Update all layers
    pub fn update_all(&mut self) {
        for layer in &mut self.layers {
            layer.update();
        }
    }

    /// Get statistics from all layers
    pub fn get_all_stats(&self) -> Vec<(String, Vec<(String, String)>)> {
        self.layers
            .iter()
            .map(|layer| (layer.name().to_string(), layer.stats()))
            .collect()
    }

    /// Run hit_test against every visible layer, returning results in draw order.
    pub fn hit_test_all(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<Vec<crate::selection::HitCandidate>> {
        let fs_phase = crate::frame_stats::start();
        let hits = self
            .layers
            .iter()
            .filter(|layer| layer.is_visible())
            .map(|layer| {
                layer
                    .as_editable()
                    .map(|e| e.hit_test(viewport, screen_pt))
                    .unwrap_or_default()
            })
            .collect();
        crate::frame_stats::lap("hit_test", fs_phase);
        hits
    }

    /// Run hit_test_interior against every visible layer, returning results in draw order.
    pub fn hit_test_interior_all(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<Vec<crate::selection::HitCandidate>> {
        self.layers
            .iter()
            .filter(|layer| layer.is_visible())
            .map(|layer| {
                layer
                    .as_editable()
                    .map(|e| e.hit_test_interior(viewport, screen_pt))
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Run hit_test_rect against every visible layer, concatenated in draw order.
    pub fn hit_test_rect_all(
        &self,
        viewport: &Viewport,
        rect: Bounds<Pixels>,
    ) -> Vec<crate::selection::FeatureRef> {
        self.layers
            .iter()
            .filter(|layer| layer.is_visible())
            .flat_map(|layer| {
                layer
                    .as_editable()
                    .map(|e| e.hit_test_rect(viewport, rect))
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Hit-test only against the given selection: for each layer, run its
    /// normal `hit_test`, keep only candidates already present in
    /// `selected`, and resolve the nearest across layers. Used to detect
    /// whether a mouse-down landed on a currently-selected feature (to start
    /// a move-drag) as opposed to empty space (box-select).
    pub fn hit_test_selection(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        selected: &[crate::selection::FeatureRef],
    ) -> Option<crate::selection::FeatureRef> {
        let per_layer: Vec<Vec<crate::selection::HitCandidate>> = self
            .layers
            .iter()
            .filter(|layer| layer.is_visible())
            .map(|layer| {
                layer
                    .as_editable()
                    .map(|e| {
                        e.hit_test(viewport, screen_pt)
                            .into_iter()
                            .filter(|c| selected.contains(&c.feature))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        crate::selection::resolve_hits(per_layer)
    }

    /// Render `feature`'s highlight by asking the owning layer (matched by id).
    /// No-op if no layer with that id exists, it's not visible, or it's not
    /// editable.
    pub fn render_highlight(
        &self,
        feature: &crate::selection::FeatureRef,
        kind: HighlightKind,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        let fs_phase = crate::frame_stats::start();
        if let Some(layer) = self.find_layer(feature.layer_id) {
            if layer.is_visible() {
                if let Some(editable) = layer.as_editable() {
                    editable.render_highlight(viewport, bounds, window, feature, kind);
                    crate::frame_stats::lap(
                        match kind {
                            HighlightKind::Hovered => "highlight.hovered",
                            _ => "highlight.selected",
                        },
                        fs_phase,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use super::*` above also re-imports `gpui::test` (this file's
    // top-level `use gpui::*` brings it in, and gpui re-exports
    // `gpui_macros::test` — its own `#[gpui::test]` async-test attribute —
    // under the plain name `test`). Left alone, every `#[test]` below
    // silently resolves to *that* proc macro instead of the real one,
    // which doesn't understand a plain sync fn and blows the expansion
    // recursion limit once enough tests accumulate in this file. Shadow it
    // back to the real one explicitly.
    use std::prelude::v1::test;

    /// Build a `LayerManager` populated with one empty-data `OsmLayer` per
    /// name in `names`, in order — a lightweight stand-in "dummy" layer that
    /// reuses `OsmLayer` (already a real, well-exercised `MapLayer` impl)
    /// rather than adding a new from-scratch one purely for these tests: an
    /// extra `impl MapLayer for ...` living alongside this `mod tests`'s
    /// `use super::*` was found, while writing this, to trip a rustc
    /// recursion-limit bug during `#[test]` expansion. Reusing `OsmLayer`
    /// sidesteps that entirely.
    fn manager_with(names: &[&str]) -> LayerManager {
        use crate::osm::OsmData;
        use std::collections::HashMap;
        use std::sync::Arc;

        let mut manager = LayerManager::new();
        for name in names {
            let id = manager.alloc_id();
            let data = Arc::new(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: Vec::new(),
            });
            let layer = crate::layers::osm_layer::OsmLayer::new_with_data(id, *name, data);
            manager.add_layer(Box::new(layer));
        }
        manager
    }

    #[test]
    fn move_layer_down() {
        let mut m = manager_with(&["a", "b", "c"]);
        m.move_layer(0, 2);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }

    #[test]
    fn move_layer_up() {
        let mut m = manager_with(&["a", "b", "c"]);
        m.move_layer(2, 0);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn move_layer_same_index_is_noop() {
        let mut m = manager_with(&["a", "b"]);
        m.move_layer(1, 1);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn move_layer_out_of_bounds_is_noop() {
        let mut m = manager_with(&["a", "b"]);
        m.move_layer(0, 99);
        m.move_layer(99, 0);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn remove_at_removes_item() {
        let mut m = manager_with(&["a", "b", "c"]);
        let removed = m.remove_at(1);
        assert_eq!(removed.map(|l| l.name().to_string()), Some("b".to_string()));
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn remove_at_out_of_bounds_is_none() {
        let mut m = manager_with(&["a", "b"]);
        assert!(m.remove_at(5).is_none());
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn find_layer_by_id_finds_the_right_layer() {
        let mut manager = manager_with(&["a", "b"]);
        let id_a = manager.layers()[0].id();
        let id_b = manager.layers()[1].id();

        assert_eq!(manager.find_layer(id_b).map(|l| l.name()), Some("b"));
        assert_eq!(
            manager.find_layer_mut(id_a).map(|l| l.name().to_string()),
            Some("a".to_string())
        );
    }

    #[test]
    fn find_layer_by_id_unknown_id_is_none() {
        let mut manager = manager_with(&["a"]);
        let bogus = manager.alloc_id();
        assert!(manager.find_layer(bogus).is_none());
    }

    #[test]
    fn alloc_id_never_reuses_the_id_of_a_removed_layer() {
        let mut manager = manager_with(&["a", "b"]);
        let removed = manager.layers()[1].id();
        manager.remove_at(1);
        manager.remove_at(0);
        let next = manager.alloc_id();
        assert_ne!(next, removed);
        assert!(next.0 > removed.0);
    }

    #[test]
    fn unique_name_returns_base_when_untaken() {
        let m = manager_with(&["a", "b"]);
        assert_eq!(m.unique_name("c"), "c");
    }

    #[test]
    fn unique_name_appends_counter_on_collision() {
        let m = manager_with(&["a"]);
        assert_eq!(m.unique_name("a"), "a (2)");
    }

    #[test]
    fn unique_name_skips_past_multiple_collisions() {
        let m = manager_with(&["a", "a (2)", "a (3)"]);
        assert_eq!(m.unique_name("a"), "a (4)");
    }

    #[test]
    fn hit_test_selection_finds_selected_node_at_click_point() {
        use crate::layers::osm_layer::OsmLayer;
        use crate::layers::LayerManager;
        use crate::osm::{OsmData, OsmNode};
        use crate::selection::{FeatureKind, FeatureRef};
        use crate::viewport::Viewport;
        use gpui::{point, px, size};
        use std::collections::HashMap;
        use std::sync::Arc;

        let center_lat = 40.0;
        let center_lon = -74.0;
        let node = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: HashMap::new(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(1, node);
        let data = Arc::new(OsmData {
            nodes,
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        });

        let mut manager = LayerManager::new();
        let layer_id = manager.alloc_id();
        let layer = OsmLayer::new_with_data(layer_id, "L", data);
        manager.add_layer(Box::new(layer));

        let viewport = Viewport::new(center_lat, center_lon, 18.0, size(px(800.0), px(600.0)));
        let selected = vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Node,
            id: 1,
        }];

        let hit = manager.hit_test_selection(&viewport, point(px(400.0), px(300.0)), &selected);
        assert_eq!(hit, Some(selected[0]));
    }

    #[test]
    fn hit_test_selection_ignores_unselected_features() {
        use crate::layers::osm_layer::OsmLayer;
        use crate::layers::LayerManager;
        use crate::osm::{OsmData, OsmNode};
        use crate::selection::{FeatureKind, FeatureRef};
        use crate::viewport::Viewport;
        use gpui::{point, px, size};
        use std::collections::HashMap;
        use std::sync::Arc;

        let center_lat = 40.0;
        let center_lon = -74.0;
        let node = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: HashMap::new(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(1, node);
        let data = Arc::new(OsmData {
            nodes,
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        });

        let mut manager = LayerManager::new();
        let layer_id = manager.alloc_id();
        let layer = OsmLayer::new_with_data(layer_id, "L", data);
        manager.add_layer(Box::new(layer));

        let viewport = Viewport::new(center_lat, center_lon, 18.0, size(px(800.0), px(600.0)));
        // Selection references a *different* node id than the one under the cursor.
        let selected = vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Node,
            id: 999,
        }];

        let hit = manager.hit_test_selection(&viewport, point(px(400.0), px(300.0)), &selected);
        assert_eq!(hit, None);
    }
}
