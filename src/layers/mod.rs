use gpui::*;
use std::collections::HashSet;

use crate::ids::{NodeId, RelationId, WayId};
use crate::tags::Tags;
use crate::viewport::Viewport;

use osm_layer::OsmLayer;
use reference_layer::ReferenceLayer;

pub mod area_labels;
pub mod diff;
pub mod extension_layer;
pub mod image_overlay;
pub mod labels;
pub mod line_labels;
pub mod merge;
pub mod node_markers;
pub mod notes_layer;
pub mod osm_layer;
pub mod paint;
pub mod reference_layer;
pub mod resolve;
pub mod tile_layer;
pub mod vector_tile_layer;
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

/// Stable identity for a layer, independent of its display name (which may be
/// renamed for uniqueness). Allocated monotonically by
/// `LayerManager::alloc_id`.
///
/// The field is public so the binary crate (`undo.rs`) and tests can build one
/// without a `LayerManager`; production ids must always come from `alloc_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(pub u64);

/// Trait that all map layers must implement: the universal surface (name,
/// identity, visibility, rendering, stats). What only one kind of layer has
/// is reached through a typed accessor that is `None` everywhere else:
/// `as_osm` (the data layer), `as_imagery` (tile imagery), `as_reference`
/// (GPX/GeoJSON traces) and `as_editable` (the vector-editing surface; see
/// `EditableLayer`). A new layer type decides which of these it answers.
pub trait MapLayer: Send + Sync {
    fn name(&self) -> &str;

    /// This layer's stable identity. See `LayerId`.
    fn id(&self) -> LayerId;

    /// Which type of layer this is. Code that needs to know what a layer
    /// is asks this, not whether some accessor happens to answer.
    fn kind(&self) -> LayerKind;

    fn is_visible(&self) -> bool;

    fn set_visible(&mut self, visible: bool);

    /// What this layer has under `point` (view-relative pixels), within the
    /// reach the layer itself defines. The OSM data layer answers `None`: its
    /// features are hit-tested and selected through `EditableLayer`. A layer
    /// with something clickable answers with a `LayerHit` variant.
    fn hit_test_point(&self, _viewport: &Viewport, _point: Point<Pixels>) -> Option<LayerHit> {
        None
    }

    /// Render this layer as GPUI elements (for raster/image content).
    ///
    /// `window` and `cx` let raster layers read the display scale factor and
    /// probe their image assets while building elements.
    fn render_elements(
        &self,
        viewport: &Viewport,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement>;

    /// Render this layer using canvas drawing (for vector content)
    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window);

    /// Get layer statistics for display. Code that needs a number reads it
    /// from the typed accessor instead of parsing these strings.
    fn stats(&self) -> Vec<(String, String)> {
        vec![]
    }

    /// This layer as the OSM data layer, if it is one.
    fn as_osm(&self) -> Option<&OsmLayer> {
        None
    }

    /// Mutable counterpart to `as_osm`.
    fn as_osm_mut(&mut self) -> Option<&mut OsmLayer> {
        None
    }

    /// This layer as tile imagery, if it is.
    fn as_imagery(&self) -> Option<&dyn ImageryLayer> {
        None
    }

    /// Mutable counterpart to `as_imagery`.
    fn as_imagery_mut(&mut self) -> Option<&mut dyn ImageryLayer> {
        None
    }

    /// This layer as a photo / scanned-map overlay, if it is one.
    fn as_image_overlay(&self) -> Option<&image_overlay::ImageOverlayLayer> {
        None
    }

    /// Mutable counterpart to `as_image_overlay`.
    fn as_image_overlay_mut(&mut self) -> Option<&mut image_overlay::ImageOverlayLayer> {
        None
    }

    /// This layer as a reference (GPX/GeoJSON) layer, if it is one.
    fn as_reference(&self) -> Option<&ReferenceLayer> {
        None
    }

    /// Mutable counterpart to `as_reference`.
    fn as_reference_mut(&mut self) -> Option<&mut ReferenceLayer> {
        None
    }

    /// This layer's vector-editing surface, if it has one.
    fn as_editable(&self) -> Option<&dyn EditableLayer> {
        None
    }

    /// Mutable counterpart to `as_editable`.
    fn as_editable_mut(&mut self) -> Option<&mut dyn EditableLayer> {
        None
    }
}

/// The type of a layer: one variant per `MapLayer` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerKind {
    /// Tile imagery.
    Tile,
    /// OSM data.
    Osm,
    Notes,
    /// GPX, GeoJSON and downloaded GPS traces.
    Reference,
    /// Tiles of a vector-tile archive (PMTiles).
    VectorTile,
    /// An extension's markers.
    Extension,
    /// A photo or scanned map stretched over the map by the user.
    ImageOverlay,
}

impl LayerKind {
    /// Imagery: drawn as image elements (`MapLayer::render_elements`), not on
    /// a canvas, and placed at the bottom of the stack when added.
    pub fn is_imagery(self) -> bool {
        matches!(self, LayerKind::Tile | LayerKind::ImageOverlay)
    }

    /// Pins drawn over the map (notes, extension markers), kept above the
    /// data when a data layer is added.
    fn is_pin(self) -> bool {
        matches!(self, LayerKind::Notes | LayerKind::Extension)
    }
}

/// A run of consecutive non-imagery layers in the stack, painted on one
/// canvas, bottom first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanvasRun {
    /// Whether the downloaded-area wash is painted first, under the run's
    /// layers. Exactly one run of a plan has it: the first one above the
    /// topmost imagery layer drawn (or the bottom one, with no imagery).
    pub wash: bool,
    /// The layers, bottom first.
    pub layers: Vec<LayerId>,
}

/// One step of drawing the layer stack; see `LayerManager::draw_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawStep {
    /// An imagery layer, drawn as its `render_elements`.
    Elements(LayerId),
    /// Layers painted on a canvas with their `render_canvas`.
    Canvas(CanvasRun),
}

/// `DrawStep` with the imagery's elements built; see
/// `LayerManager::render_stack`.
pub enum StackPart {
    Elements(Vec<AnyElement>),
    Canvas(CanvasRun),
}

/// What a layer found under a click, in the layer's own terms.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerHit {
    /// A note's pin; the note's id.
    Note(i64),
    /// A GPX/GeoJSON/trace feature; its index in the layer's data.
    ReferenceFeature(usize),
    /// A feature of a vector-tile layer.
    VectorFeature(std::sync::Arc<crate::layers::vector_tile_layer::VectorFeatureInfo>),
    /// One of an extension's markers: the extension's id and the marker's.
    ExtensionMarker { extension: String, marker: String },
}

impl LayerHit {
    /// Which hit wins when several layers have one under the same click:
    /// lower first. Notes and extension markers sit above reference
    /// features, whatever order the layers are stacked in.
    fn rank(&self) -> u8 {
        match self {
            LayerHit::Note(_) | LayerHit::ExtensionMarker { .. } => 0,
            LayerHit::ReferenceFeature(_) => 1,
            LayerHit::VectorFeature(_) => 2,
        }
    }
}

/// What tile imagery has that other layers do not.
pub trait ImageryLayer {
    /// Required source-credit (text and optional link) for this layer's
    /// content, if any (e.g. "© OpenStreetMap contributors").
    fn attribution(&self) -> Option<&crate::imagery::AttributionInfo>;

    /// The imagery shift. Only where imagery pixels are painted moves; see
    /// `imagery_offset`.
    fn imagery_offset(&self) -> crate::imagery_offset::ImageryOffset;

    /// Set the imagery shift.
    fn set_imagery_offset(&mut self, offset: crate::imagery_offset::ImageryOffset);

    /// Stable identity of the imagery source (ELI id, `osm-carto`, or
    /// `custom:<url template>`), the key under which offsets are remembered.
    fn imagery_id(&self) -> &str;

    /// How many tile files the layer's cache holds (0 while the cache is busy).
    fn cached_file_count(&self) -> usize;
}

/// The numbers the status line shows, read from the layers' typed accessors.
/// With several layers of a kind the last one wins.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LayerStats {
    pub cached_files: usize,
    pub osm_nodes: usize,
    pub osm_ways: usize,
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

/// Why an edit primitive of an `EditableLayer` refused; the layer is left
/// unchanged. Callers turn it into their own user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The layer holds no data.
    NoData,
    UnknownNode(NodeId),
    UnknownWay(WayId),
    UnknownRelation(RelationId),
    /// A node cannot be removed while a way still lists it.
    NodeInWay(NodeId),
    /// A relation cannot be deleted while another has it as a member.
    RelationInUse(RelationId),
    /// A node or way cannot be deleted while a relation still lists it.
    InRelation {
        kind: crate::selection::FeatureKind,
        id: i64,
        relation: RelationId,
    },
    /// A way cannot lose nodes down to fewer than two.
    WayTooShort {
        way_id: WayId,
        len: usize,
    },
    /// An object with this id exists already.
    IdInUse(crate::selection::FeatureKind, i64),
    /// A position past the end of a way's node list.
    IndexOutOfRange {
        way_id: WayId,
        index: usize,
        len: usize,
    },
    /// The operation covers nodes and ways only.
    NotNodeOrWay(crate::selection::FeatureKind),
    /// A node snapshot without coordinates.
    MissingCoordinates(NodeId),
}

impl EditError {
    /// The unknown-object error for `kind`.
    pub fn unknown(kind: crate::selection::FeatureKind, id: i64) -> Self {
        use crate::selection::FeatureKind;
        match kind {
            FeatureKind::Node => EditError::UnknownNode(NodeId(id)),
            FeatureKind::Way => EditError::UnknownWay(WayId(id)),
            FeatureKind::Relation => EditError::UnknownRelation(RelationId(id)),
        }
    }
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NoData => write!(f, "the layer has no data"),
            EditError::UnknownNode(id) => write!(f, "no node {id}"),
            EditError::UnknownWay(id) => write!(f, "no way {id}"),
            EditError::UnknownRelation(id) => write!(f, "no relation {id}"),
            EditError::NodeInWay(id) => write!(f, "node {id} is still part of a way"),
            EditError::RelationInUse(id) => {
                write!(f, "relation {id} is still a member of another relation")
            }
            EditError::InRelation { kind, id, relation } => {
                write!(f, "{kind:?} {id} is still a member of relation {relation}")
            }
            EditError::WayTooShort { way_id, len } => {
                write!(
                    f,
                    "way {way_id} would be left with {len} nodes; it needs at least 2"
                )
            }
            EditError::IdInUse(kind, id) => write!(f, "{kind:?} {id} already exists"),
            EditError::IndexOutOfRange { way_id, index, len } => {
                write!(
                    f,
                    "index {index} is past the end of way {way_id} ({len} nodes)"
                )
            }
            EditError::NotNodeOrWay(kind) => write!(f, "{kind:?} is not a node or way"),
            EditError::MissingCoordinates(id) => write!(f, "node {id} has no coordinates"),
        }
    }
}

impl std::error::Error for EditError {}
/// The vector-editing surface a layer may support: hit-testing, selection
/// highlight, tag read/write, node moves, and create/delete. Reached from a
/// `&dyn MapLayer`/`&mut dyn MapLayer` via `as_editable`/`as_editable_mut`.
/// `OsmLayer` is the only implementation; layers with no editable feature
/// data (tiles) simply don't implement this trait
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

    /// The server version `(kind, id)` was downloaded at (the base version an
    /// upload would be checked against), if the layer holds the object.
    fn feature_version(&self, _kind: crate::selection::FeatureKind, _id: i64) -> Option<i32> {
        None
    }

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

    /// After a pointer move to `pt`: choose the way whose vertices to preview
    /// (shown small, hoverable, clickable and draggable), given what the
    /// hover hit test picked. `enabled` is false when previews are
    /// suppressed. Returns whether the preview changed. Default: none.
    fn update_vertex_preview(
        &mut self,
        _viewport: &Viewport,
        _pt: Point<Pixels>,
        _hovered: Option<&crate::selection::FeatureRef>,
        _enabled: bool,
    ) -> bool {
        false
    }

    /// The way whose vertices are currently previewed (see
    /// `update_vertex_preview`), if any. Default: none.
    fn vertex_preview_way_id(&self) -> Option<crate::ids::WayId> {
        None
    }

    /// Names of the named roads within `radius_m` meters of `feature`,
    /// nearest first (for `addr:street` suggestions). Default: none.
    fn nearby_road_names(
        &self,
        _feature: &crate::selection::FeatureRef,
        _radius_m: f64,
    ) -> Vec<String> {
        Vec::new()
    }

    /// Return key/value tags for the given feature if this layer owns it.
    fn feature_tags(&self, feature: &crate::selection::FeatureRef) -> Option<Tags>;

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
    /// `cx` supplies the theme, whose selection color a selected feature's
    /// highlight uses.
    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &crate::selection::FeatureRef,
        kind: HighlightKind,
        cx: &App,
    );

    /// Set a transient screen-space offset to apply when rendering the given
    /// node ids, for live drag feedback. Does not touch the underlying data.
    fn set_drag_preview(&mut self, node_ids: &HashSet<NodeId>, delta: Point<Pixels>);

    /// Clear any transient drag preview.
    fn clear_drag_preview(&mut self);

    /// The screen-space offset a live drag preview currently applies to a
    /// node (zero when it isn't being dragged).
    fn node_drag_offset(&self, node_id: NodeId) -> Point<Pixels>;

    /// Current position of a node this layer owns, if any.
    fn node_lat_lon(&self, node_id: NodeId) -> Option<crate::coordinates::LatLon>;

    /// The member node ids of a way this layer owns, if any.
    fn way_node_ids(&self, way_id: WayId) -> Option<Vec<NodeId>>;

    // -- Edit primitives. Each changes the data and every derived cache
    // together and marks the layer modified. A refusal returns `Err` having
    // changed nothing. --

    /// Move nodes: `(node_id, new_position)`. Moves none if any id is
    /// unknown. Caches are patched for the moved nodes and their ways only.
    fn commit_node_moves(
        &mut self,
        moves: &[(NodeId, crate::coordinates::LatLon)],
    ) -> Result<(), EditError>;

    /// Set (insert or overwrite) a tag on a node, way or relation.
    fn set_tag(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
        key: &str,
        value: &str,
    ) -> Result<(), EditError>;

    /// Remove a tag key from a node, way or relation.
    fn remove_tag(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
        key: &str,
    ) -> Result<(), EditError>;

    /// Create a tag-less node at `at` and return its id: a fresh
    /// negative one when `id` is `None`, else exactly `id` (redo), which must
    /// be unused.
    fn create_node(
        &mut self,
        at: crate::coordinates::LatLon,
        id: Option<NodeId>,
    ) -> Result<NodeId, EditError>;

    /// `create_node` with a fresh id, which cannot fail.
    fn add_node(&mut self, at: crate::coordinates::LatLon) -> NodeId;

    /// Delete a node or way, returning what `restore_feature` needs. A node
    /// still in a way, and a node or way still listed by a relation, is
    /// refused (edit the way or relation first); a way's nodes stay.
    /// Relations have their own `delete_relation`.
    fn delete_feature(
        &mut self,
        kind: crate::selection::FeatureKind,
        id: i64,
    ) -> Result<crate::selection::DeletedFeatureSnapshot, EditError>;

    /// Put a deleted node or way back with the snapshot's id, tags and
    /// geometry; its id must be unused.
    fn restore_feature(
        &mut self,
        snapshot: crate::selection::DeletedFeatureSnapshot,
    ) -> Result<(), EditError>;

    /// Add a way over existing nodes with a fresh negative id and return it.
    fn add_way(&mut self, node_ids: Vec<NodeId>, tags: Tags) -> WayId;

    /// Append a node id to a way.
    fn extend_way(&mut self, way_id: WayId, node_id: NodeId) -> Result<(), EditError>;

    /// Create a node and splice it into a way at `index` (`0..=len`), returning
    /// its id. Creates nothing if the way or index is bad.
    fn insert_node_into_way(
        &mut self,
        way_id: WayId,
        index: usize,
        at: crate::coordinates::LatLon,
    ) -> Result<NodeId, EditError>;

    /// Remove a node that no way or relation uses (undo of a create).
    fn remove_node(&mut self, node_id: NodeId) -> Result<(), EditError>;

    /// Remove a way no relation lists; its nodes stay.
    fn remove_way(&mut self, way_id: WayId) -> Result<(), EditError>;

    /// Take the node at `index` out of a way's node list; the node stays.
    /// Refused if the way would be left with fewer than two nodes.
    fn remove_node_from_way(&mut self, way_id: WayId, index: usize) -> Result<(), EditError>;

    /// Reverse a way's node order (its own inverse).
    fn reverse_way(&mut self, way_id: WayId) -> Result<(), EditError>;

    /// Replace a way's node list. The nodes should already exist. A list
    /// shorter than two nodes is refused when it shortens the way.
    fn set_way_nodes(&mut self, way_id: WayId, node_ids: Vec<NodeId>) -> Result<(), EditError>;
    /// The ids of the ways whose node list includes `node_id`.
    fn ways_using_node(&self, node_id: NodeId) -> Vec<WayId>;

    /// The ids of the relations that have `way_id` as a member.
    fn relations_containing_way(&self, way_id: WayId) -> Vec<RelationId>;

    /// The ids of the relations that have `node_id` as a member.
    fn relations_containing_node(&self, node_id: NodeId) -> Vec<RelationId>;

    /// A copy of the relation with this id (tags and members), if the layer
    /// has it.
    fn relation(&self, relation_id: RelationId) -> Option<crate::osm::OsmRelation>;

    /// The ids of every relation the layer has, in the order it stores them.
    fn relation_ids(&self) -> Vec<RelationId>;

    /// Replace a relation's whole member list and return the previous one.
    fn set_relation_members(
        &mut self,
        relation_id: RelationId,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<Vec<crate::osm::OsmMember>, EditError>;

    /// Copies of every relation in the layer.
    fn relations(&self) -> Vec<crate::osm::OsmRelation>;

    /// The ids of the relations that have `relation_id` as a member.
    fn relations_containing_relation(&self, relation_id: RelationId) -> Vec<RelationId>;

    /// Create a relation with a fresh negative id (from the same pool new
    /// nodes and ways draw from) and return it. Member refs are not checked.
    fn create_relation(
        &mut self,
        tags: Tags,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<RelationId, EditError>;

    /// Delete a relation, returning a snapshot for `restore_relation`.
    /// Refused while another relation has it as a member.
    fn delete_relation(
        &mut self,
        relation_id: RelationId,
    ) -> Result<crate::selection::DeletedRelationSnapshot, EditError>;

    /// Put a deleted (or, for redo, created) relation back with its original
    /// id, version, content and position; the id must be unused.
    fn restore_relation(
        &mut self,
        snapshot: crate::selection::DeletedRelationSnapshot,
    ) -> Result<(), EditError>;

    /// Whether the object has a pending local edit: it was created here, or
    /// differs from the server baseline. `false` for an unknown object.
    fn has_pending_edit(&self, kind: crate::selection::FeatureKind, id: i64) -> bool;

    /// Purge: drop these objects from the layer's data AND its server
    /// baseline, as if they had never been downloaded. Nothing is marked
    /// deleted and nothing is uploaded for them. The caller has checked that
    /// they are unmodified and that no remaining way uses a purged node.
    /// Relations that name them keep the members, now not downloaded.
    fn purge_features(
        &mut self,
        nodes: &[NodeId],
        ways: &[WayId],
        relations: &[RelationId],
    ) -> crate::selection::PurgedFeatures;

    /// Undo of `purge_features`: put the objects back in data and baseline.
    fn restore_purged(&mut self, purged: crate::selection::PurgedFeatures);

    /// Bring back objects the server has deleted, as edits of the existing
    /// objects: they join the data as given (ids and the deleted versions
    /// kept) and the baseline as a marked stand-in, so the diff reports each
    /// as modified rather than created. Undone by `purge_features`.
    fn undelete_objects(&mut self, objects: crate::selection::PurgedFeatures);
}

/// Read the status-line numbers off `layers` through their typed accessors.
pub fn compute_layer_stats(layers: &[Box<dyn MapLayer>]) -> LayerStats {
    let mut stats = LayerStats::default();
    for layer in layers {
        if let Some(imagery) = layer.as_imagery() {
            stats.cached_files = imagery.cached_file_count();
        }
        if let Some((nodes, ways)) = layer.as_osm().and_then(|osm| osm.feature_counts()) {
            stats.osm_nodes = nodes;
            stats.osm_ways = ways;
        }
    }
    stats
}

/// Manager for all map layers: the layer stack.
///
/// `layers()` is in drawing order: index 0 is drawn first, at the bottom of
/// the map, and the last layer on top. The Layers list shows the stack the
/// other way up (see `top_first`), so its first row is the layer drawn on
/// top. Index-based actions (`MoveLayer`, `DeleteLayer`, ...) take an index
/// into `layers()`.
pub struct LayerManager {
    layers: Vec<Box<dyn MapLayer>>,
    /// Monotonic counter backing `alloc_id`. Starts at 1 (0 is never handed
    /// out, so it's safe to use as a sentinel if ever needed).
    next_id: u64,
    /// Screen space the layers' labels share, refilled in layer order by
    /// every `render_all_labels` so labels of different layers never overlap.
    label_space: std::sync::Mutex<labels::LabelSpace>,
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
            label_space: std::sync::Mutex::new(labels::LabelSpace::new(0.0, 0.0)),
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

    /// Add a new layer to the stack where its kind belongs (see
    /// `placement`). Layers already in the stack keep their order, however
    /// the user arranged them.
    pub fn add_layer(&mut self, layer: Box<dyn MapLayer>) {
        let at = self.placement(layer.kind());
        self.layers.insert(at, layer);
    }

    /// Where `add_layer` puts a new layer of `kind`, as an index into
    /// `layers()` (bottom first):
    ///
    /// - Tile imagery: just above the topmost tile layer, or at the bottom
    ///   when there is none, so it goes under everything that is not imagery.
    /// - Photo overlays: just above the topmost imagery layer of any kind.
    /// - Reference layers (GPX, GeoJSON, GPS traces) and vector-tile
    ///   overlays (PMTiles): above the topmost imagery and below the first
    ///   OSM data layer above it, so the data being edited stays on top of
    ///   what it is traced from; on top when there is no such data layer.
    /// - OSM data: on top, but under the notes and extension-marker layers
    ///   at the top of the stack, so pins stay visible over the data.
    /// - Notes and extension markers: on top.
    pub fn placement(&self, kind: LayerKind) -> usize {
        let len = self.layers.len();
        let above_imagery = self
            .layers
            .iter()
            .rposition(|l| l.kind().is_imagery())
            .map_or(0, |i| i + 1);
        match kind {
            // New basemaps go above the other basemaps but under any photo
            // overlay, which is traced against the map below it.
            LayerKind::Tile => self
                .layers
                .iter()
                .rposition(|l| l.kind() == LayerKind::Tile)
                .map_or(0, |i| i + 1),
            LayerKind::ImageOverlay => above_imagery,
            LayerKind::Reference | LayerKind::VectorTile => self.layers[above_imagery..]
                .iter()
                .position(|l| l.kind() == LayerKind::Osm)
                .map_or(len, |i| i + above_imagery),
            LayerKind::Osm => {
                let pins_on_top = self
                    .layers
                    .iter()
                    .rev()
                    .take_while(|l| l.kind().is_pin())
                    .count();
                len - pins_on_top
            }
            LayerKind::Notes | LayerKind::Extension => len,
        }
    }

    /// The stack as the Layers list shows it: the layer drawn on top first,
    /// each with its index in `layers()` (what index-based actions take).
    pub fn top_first(&self) -> impl Iterator<Item = (usize, &dyn MapLayer)> {
        self.layers
            .iter()
            .enumerate()
            .rev()
            .map(|(i, l)| (i, l.as_ref()))
    }

    /// Remove the layer at `index`. Returns the removed layer, or `None` if
    /// `index` is out of bounds.
    pub fn remove_at(&mut self, index: usize) -> Option<Box<dyn MapLayer>> {
        if index >= self.layers.len() {
            return None;
        }
        Some(self.layers.remove(index))
    }

    /// Put `layer` where the layer `id` is, keeping its place in the list.
    /// Returns the old layer, or `None` (and drops `layer`) if `id` is gone.
    pub fn replace(&mut self, id: LayerId, layer: Box<dyn MapLayer>) -> Option<Box<dyn MapLayer>> {
        let slot = self.layers.iter_mut().find(|l| l.id() == id)?;
        Some(std::mem::replace(slot, layer))
    }

    /// Move the layer at `from` to position `to` (indices into `layers()`, so
    /// a larger `to` draws it higher up). No-op if either index is out of
    /// bounds or if `from == to`.
    pub fn move_layer(&mut self, from: usize, to: usize) {
        let len = self.layers.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
    }

    pub fn layers(&self) -> &[Box<dyn MapLayer>] {
        &self.layers
    }

    pub fn layers_mut(&mut self) -> &mut [Box<dyn MapLayer>] {
        &mut self.layers
    }

    pub fn find_layer(&self, id: LayerId) -> Option<&dyn MapLayer> {
        self.layers
            .iter()
            .find(|layer| layer.id() == id)
            .map(|b| b.as_ref())
    }

    pub fn find_layer_mut(&mut self, id: LayerId) -> Option<&mut (dyn MapLayer + '_)> {
        match self.layers.iter_mut().find(|layer| layer.id() == id) {
            Some(b) => Some(b.as_mut()),
            None => None,
        }
    }

    /// Find a layer by its exact display name. Only for the "does a singleton
    /// layer like OSM Carto exists" checks at creation
    /// time, when there is no id yet; everything else goes through `LayerId`.
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

    /// How to draw the visible layers, bottom first, so the stack's order is
    /// the drawing order whatever the kinds: each imagery layer is a step of
    /// its own (image elements) and each run of consecutive other layers is
    /// one canvas. With `show_imagery` false the imagery layers are left out,
    /// as a stylesheet's `canvas { show-imagery: no; }` asks.
    ///
    /// The downloaded-area wash goes right above the topmost imagery layer
    /// drawn (at the bottom without imagery): over imagery and under the
    /// data in the usual arrangement, and still visible when imagery has
    /// been moved above data. The plan always ends with a canvas step, on
    /// which the overlays (highlights, labels, previews) are painted too.
    pub fn draw_plan(&self, show_imagery: bool) -> Vec<DrawStep> {
        let drawn: Vec<&dyn MapLayer> = self
            .layers
            .iter()
            .filter(|l| l.is_visible() && (show_imagery || !l.kind().is_imagery()))
            .map(|l| l.as_ref())
            .collect();
        let topmost_imagery = drawn.iter().rposition(|l| l.kind().is_imagery());
        let mut steps = Vec::new();
        let mut run = topmost_imagery.is_none().then_some(CanvasRun {
            wash: true,
            layers: Vec::new(),
        });
        for (i, layer) in drawn.iter().enumerate() {
            if layer.kind().is_imagery() {
                if let Some(run) = run.take() {
                    steps.push(DrawStep::Canvas(run));
                }
                steps.push(DrawStep::Elements(layer.id()));
                if Some(i) == topmost_imagery {
                    run = Some(CanvasRun {
                        wash: true,
                        layers: Vec::new(),
                    });
                }
            } else {
                run.get_or_insert_with(CanvasRun::default)
                    .layers
                    .push(layer.id());
            }
        }
        steps.push(DrawStep::Canvas(run.unwrap_or_default()));
        steps
    }

    /// `draw_plan` with each imagery layer's elements built, ready to be laid
    /// out in order as absolutely positioned children of the map.
    pub fn render_stack(
        &self,
        viewport: &Viewport,
        show_imagery: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<StackPart> {
        self.draw_plan(show_imagery)
            .into_iter()
            .map(|step| match step {
                DrawStep::Elements(id) => StackPart::Elements(
                    self.find_layer(id)
                        .map(|l| l.render_elements(viewport, window, cx))
                        .unwrap_or_default(),
                ),
                DrawStep::Canvas(run) => StackPart::Canvas(run),
            })
            .collect()
    }

    /// Dim the part of the view outside every downloaded rectangle of every
    /// visible layer, and outline the edge of the downloaded area. One
    /// region for all layers: area downloaded into any visible layer counts
    /// as known, so a second layer never dims the first one's data. Nothing
    /// is drawn while no visible layer knows its downloaded area. Painted
    /// first on the canvas run `draw_plan` gives the wash, so that run's
    /// features paint on top of it.
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
            .filter_map(|layer| layer.as_osm())
            .flat_map(|osm| osm.download_rects().iter().copied())
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
        let frame = paint::PaintFrame::new(viewport, bounds);
        let to_screen = |mx: f64, my: f64| frame.window_mercator(mx, my);
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
        // A near-white halo under the dark blue line keeps the edge visible
        // over dark imagery, where blue alone vanishes.
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

    /// Paint the layers `ids` (bottom first, as a `CanvasRun` lists them)
    /// with their `render_canvas`. Layers gone or hidden since are skipped.
    pub fn render_canvas_run(
        &self,
        ids: &[LayerId],
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        for id in ids {
            if let Some(layer) = self.find_layer(*id).filter(|l| l.is_visible()) {
                layer.render_canvas(viewport, bounds, window);
            }
        }
    }

    /// Paint every visible layer's labels and icons.
    #[allow(clippy::too_many_arguments)]
    pub fn render_all_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        selected: &[crate::selection::FeatureRef],
        hovered: Option<&crate::selection::FeatureRef>,
        vis: labels::LabelVisibility,
        window: &mut Window,
        cx: &App,
    ) {
        let size = viewport.transform.screen_size;
        let mut space = self.label_space.lock().unwrap_or_else(|e| e.into_inner());
        *space = labels::LabelSpace::new(f32::from(size.width), f32::from(size.height));
        for layer in &self.layers {
            if layer.is_visible() {
                if let Some(osm) = layer.as_osm() {
                    osm.render_labels(
                        viewport, bounds, selected, hovered, vis, &mut space, window, cx,
                    );
                }
            }
        }
    }

    /// The status-line numbers; see `compute_layer_stats`.
    pub fn layer_stats(&self) -> LayerStats {
        compute_layer_stats(&self.layers)
    }

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

    /// The winning `MapLayer::hit_test_point` over the visible layers, with
    /// the layer that answered. Notes and extension markers beat reference
    /// features, which beat vector-tile features; a tie goes to the topmost
    /// layer.
    pub fn hit_test_point(
        &self,
        viewport: &Viewport,
        point: Point<Pixels>,
    ) -> Option<(LayerId, LayerHit)> {
        let mut best: Option<(LayerId, LayerHit)> = None;
        for layer in self.layers.iter().rev().filter(|l| l.is_visible()) {
            let Some(hit) = layer.hit_test_point(viewport, point) else {
                continue;
            };
            let better = best.as_ref().is_none_or(|(_, b)| hit.rank() < b.rank());
            if better {
                best = Some((layer.id(), hit));
            }
        }
        best
    }

    /// Tell every editable layer where the pointer is and what it hovers, so
    /// each can update which way's vertices it previews. Returns whether any
    /// layer's preview changed.
    pub fn update_vertex_preview(
        &mut self,
        viewport: &Viewport,
        pt: Point<Pixels>,
        hovered: Option<&crate::selection::FeatureRef>,
        enabled: bool,
    ) -> bool {
        let mut changed = false;
        for layer in &mut self.layers {
            if let Some(editable) = layer.as_editable_mut() {
                changed |= editable.update_vertex_preview(viewport, pt, hovered, enabled);
            }
        }
        changed
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
        cx: &App,
    ) {
        let fs_phase = crate::frame_stats::start();
        if let Some(layer) = self.find_layer(feature.layer_id) {
            if layer.is_visible() {
                if let Some(editable) = layer.as_editable() {
                    editable.render_highlight(viewport, bounds, window, feature, kind, cx);
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
    use crate::coordinates::LatLon;
    // `use super::*` above also re-imports `gpui::test` (this file's
    // top-level `use gpui::*` brings it in, and gpui re-exports
    // `gpui_macros::test` — its own `#[gpui::test]` async-test attribute —
    // under the plain name `test`). Left alone, every `#[test]` below
    // silently resolves to *that* proc macro instead of the real one,
    // which doesn't understand a plain sync fn and blows the expansion
    // recursion limit once enough tests accumulate in this file. Shadow it
    // back to the real one explicitly.
    use std::prelude::v1::test;

    /// A `LayerManager` with one empty-data `OsmLayer` per name, in order.
    /// It reuses `OsmLayer` instead of a test-only `impl MapLayer`: an extra
    /// impl next to this module's `use super::*` trips a rustc
    /// recursion-limit bug during `#[test]` expansion.
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

    /// The Layers list's order: top of the stack first.
    fn list_names(m: &LayerManager) -> Vec<&str> {
        m.top_first().map(|(_, l)| l.name()).collect()
    }

    #[test]
    fn move_layer_to_a_higher_index_raises_it_to_the_top_of_the_list() {
        let mut m = manager_with(&["a", "b", "c"]);
        assert_eq!(list_names(&m), vec!["c", "b", "a"]);
        m.move_layer(0, 2);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["b", "c", "a"]);
        assert_eq!(list_names(&m), vec!["a", "c", "b"]);
    }

    #[test]
    fn move_layer_to_a_lower_index_lowers_it_to_the_bottom_of_the_list() {
        let mut m = manager_with(&["a", "b", "c"]);
        m.move_layer(2, 0);
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
        assert_eq!(list_names(&m), vec!["b", "a", "c"]);
    }

    #[test]
    fn top_first_pairs_each_row_with_its_index() {
        let m = manager_with(&["a", "b", "c"]);
        let rows: Vec<(usize, &str)> = m.top_first().map(|(i, l)| (i, l.name())).collect();
        assert_eq!(rows, vec![(2, "c"), (1, "b"), (0, "a")]);
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
            id: NodeId(1),
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: Tags::new(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(NodeId(1), node);
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

        let viewport = Viewport::new(
            LatLon::new(center_lat, center_lon),
            18.0,
            size(px(800.0), px(600.0)),
        );
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
            id: NodeId(1),
            lat: center_lat,
            lon: center_lon,
            version: 1,
            tags: Tags::new(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(NodeId(1), node);
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

        let viewport = Viewport::new(
            LatLon::new(center_lat, center_lon),
            18.0,
            size(px(800.0), px(600.0)),
        );
        // Selection references a *different* node id than the one under the cursor.
        let selected = vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Node,
            id: 999,
        }];

        let hit = manager.hit_test_selection(&viewport, point(px(400.0), px(300.0)), &selected);
        assert_eq!(hit, None);
    }

    fn gpx_layer(id: LayerId) -> crate::layers::reference_layer::ReferenceLayer {
        use crate::reference::{Builder, FeatureKind, Format};
        let mut b = Builder::new(Format::Gpx);
        let f = b.feature(FeatureKind::Track, "", None, vec![]);
        b.line(f, &[LatLon::new(40.0, -74.0), LatLon::new(40.001, -73.999)])
            .unwrap();
        crate::layers::reference_layer::ReferenceLayer::new(id, "ref", b.finish().unwrap())
    }

    /// One layer of each type, in a fixed order, for tests that pin down what
    /// each type answers. The tile layer needs an executor, hence the context.
    fn one_of_each(cx: &mut gpui::TestAppContext) -> Vec<(&'static str, Box<dyn MapLayer>)> {
        use crate::layers::extension_layer::{ExtensionLayer, MarkerStore};
        use crate::layers::notes_layer::NotesLayer;
        use crate::layers::osm_layer::OsmLayer;
        use crate::layers::tile_layer::TileLayer;
        use std::sync::{Arc, Mutex};

        let cache = Arc::new(Mutex::new(crate::tile_cache::TileCache::new(
            cx.executor(),
            crate::idle_tracker::IdleTracker::new(),
        )));
        vec![
            ("tile", Box::new(TileLayer::new(LayerId(1), cache))),
            ("osm", Box::new(OsmLayer::new(LayerId(2)))),
            (
                "notes",
                Box::new(NotesLayer::new(
                    LayerId(3),
                    "notes".into(),
                    crate::notes::NoteStore::default(),
                )),
            ),
            ("reference", Box::new(gpx_layer(LayerId(4)))),
            (
                "extension",
                Box::new(ExtensionLayer::new(
                    LayerId(5),
                    "ext".into(),
                    "ext".into(),
                    MarkerStore::default(),
                )),
            ),
        ]
    }

    #[gpui::test]
    fn each_layer_type_answers_exactly_its_own_accessors(cx: &mut gpui::TestAppContext) {
        // (osm, imagery, reference, editable) per type. A new layer type has
        // to be added to `one_of_each` and to this table.
        let expected = [
            ("tile", (false, true, false, false)),
            ("osm", (true, false, false, true)),
            ("notes", (false, false, false, false)),
            ("reference", (false, false, true, false)),
            ("extension", (false, false, false, false)),
        ];
        let mut layers = one_of_each(cx);
        assert_eq!(layers.len(), expected.len());
        for ((kind, layer), (want_kind, want)) in layers.iter_mut().zip(expected) {
            assert_eq!(*kind, want_kind);
            let want_layer_kind = match want_kind {
                "tile" => LayerKind::Tile,
                "osm" => LayerKind::Osm,
                "notes" => LayerKind::Notes,
                "reference" => LayerKind::Reference,
                "extension" => LayerKind::Extension,
                other => panic!("no kind for {other}"),
            };
            assert_eq!(layer.kind(), want_layer_kind, "{kind}");
            let got = (
                layer.as_osm().is_some(),
                layer.as_imagery().is_some(),
                layer.as_reference().is_some(),
                layer.as_editable().is_some(),
            );
            assert_eq!(got, want, "{kind}");
            let got_mut = (
                layer.as_osm_mut().is_some(),
                layer.as_imagery_mut().is_some(),
                layer.as_reference_mut().is_some(),
                layer.as_editable_mut().is_some(),
            );
            assert_eq!(got_mut, want, "{kind} (mut)");
        }
    }

    #[test]
    fn a_note_beats_a_reference_feature_whatever_the_stacking() {
        use crate::layers::notes_layer::NotesLayer;
        use crate::notes::{CommentAction, Note, NoteComment, NoteStatus, NoteStore};

        let (lat, lon) = (40.0, -74.0);
        let viewport = Viewport::new(LatLon::new(lat, lon), 18.0, size(px(800.0), px(600.0)));
        // The pin's head is above its tip, so click the head; the track
        // vertex is put under that same pixel.
        let tip = viewport.geo_to_screen(LatLon::new(lat, lon));
        let click = point(tip.x, tip.y - px(16.0));
        let LatLon {
            lat: hit_lat,
            lon: hit_lon,
        } = viewport.screen_to_geo(click);

        let notes = NoteStore::default();
        notes.replace(vec![Note {
            id: 9,
            at: LatLon::new(lat, lon),
            status: NoteStatus::Open,
            created: None,
            closed: None,
            comments: vec![NoteComment {
                date: None,
                uid: None,
                user: None,
                action: CommentAction::Opened,
                text: "x".into(),
            }],
        }]);
        let reference = {
            use crate::reference::{Builder, FeatureKind, Format};
            let mut b = Builder::new(Format::Gpx);
            let f = b.feature(FeatureKind::Track, "", None, vec![]);
            b.line(
                f,
                &[
                    LatLon::new(hit_lat, hit_lon),
                    LatLon::new(hit_lat, hit_lon + 0.0001),
                ],
            )
            .unwrap();
            crate::layers::reference_layer::ReferenceLayer::new(
                LayerId(3),
                "ref",
                b.finish().unwrap(),
            )
        };

        // The notes layer is stacked at the bottom, the reference layer on top.
        let mut m = LayerManager::new();
        m.add_layer(Box::new(NotesLayer::new(
            LayerId(1),
            "n".into(),
            notes.clone(),
        )));
        m.add_layer(Box::new(reference));

        let at = |m: &LayerManager| m.hit_test_point(&viewport, click);
        assert_eq!(at(&m), Some((LayerId(1), LayerHit::Note(9))));

        m.find_layer_mut(LayerId(1)).unwrap().set_visible(false);
        assert!(matches!(
            at(&m),
            Some((LayerId(3), LayerHit::ReferenceFeature(0)))
        ));

        m.find_layer_mut(LayerId(3)).unwrap().set_visible(false);
        assert_eq!(at(&m), None);
    }

    #[gpui::test]
    fn a_reference_layer_goes_above_imagery_and_below_the_first_data_layer(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut by_kind: std::collections::HashMap<&str, Box<dyn MapLayer>> =
            one_of_each(cx).into_iter().collect();
        let mut m = LayerManager::new();
        m.add_layer(by_kind.remove("tile").unwrap());
        m.add_layer(by_kind.remove("osm").unwrap());
        m.add_layer(by_kind.remove("reference").unwrap());
        let ids: Vec<u64> = m.layers().iter().map(|l| l.id().0).collect();
        assert_eq!(ids, [1, 4, 2]);
    }

    #[test]
    fn a_reference_layer_with_no_imagery_goes_below_the_first_data_layer() {
        let mut m = manager_with(&["a", "b"]);
        let id = m.alloc_id();
        m.add_layer(Box::new(gpx_layer(id)));
        let names: Vec<&str> = m.layers().iter().map(|l| l.name()).collect();
        assert_eq!(names, ["ref", "a", "b"]);
    }

    /// A layer of `kind` with id `id`, for the placement and draw-plan tests.
    /// Imagery needs a tile cache, which needs an executor.
    fn layer_of(cx: &mut gpui::TestAppContext, kind: LayerKind, id: u64) -> Box<dyn MapLayer> {
        use crate::layers::extension_layer::{ExtensionLayer, MarkerStore};
        use crate::layers::notes_layer::NotesLayer;
        use crate::layers::osm_layer::OsmLayer;
        use crate::layers::tile_layer::TileLayer;
        use crate::layers::vector_tile_layer::{VectorTileLayer, VectorTileStore};
        use std::sync::{Arc, Mutex};

        let id = LayerId(id);
        match kind {
            LayerKind::Tile => {
                let cache = Arc::new(Mutex::new(crate::tile_cache::TileCache::new(
                    cx.executor(),
                    crate::idle_tracker::IdleTracker::new(),
                )));
                Box::new(TileLayer::new(id, cache))
            }
            LayerKind::Osm => Box::new(OsmLayer::new(id)),
            LayerKind::Notes => Box::new(NotesLayer::new(
                id,
                "notes".into(),
                crate::notes::NoteStore::default(),
            )),
            LayerKind::Reference => Box::new(gpx_layer(id)),
            LayerKind::VectorTile => Box::new(VectorTileLayer::new(
                id,
                "pmtiles".into(),
                0,
                14,
                VectorTileStore::default(),
            )),
            LayerKind::Extension => Box::new(ExtensionLayer::new(
                id,
                "ext".into(),
                "ext".into(),
                MarkerStore::default(),
            )),
            LayerKind::ImageOverlay => Box::new(image_overlay::ImageOverlayLayer::new(
                id,
                "photo".into(),
                image::RgbaImage::new(2, 2),
                crate::georeference::initial_corners((2, 2), Default::default(), 10.0),
            )),
        }
    }

    #[gpui::test]
    fn photo_overlays_sit_above_all_imagery_and_below_the_data(cx: &mut gpui::TestAppContext) {
        let mut m = LayerManager::new();
        m.add_layer(layer_of(cx, LayerKind::Tile, 1));
        m.add_layer(layer_of(cx, LayerKind::Osm, 2));
        m.add_layer(layer_of(cx, LayerKind::ImageOverlay, 3));
        assert_eq!(stack(&m), [1, 3, 2]);
        // A basemap added later still goes under the photo.
        m.add_layer(layer_of(cx, LayerKind::Tile, 4));
        assert_eq!(stack(&m), [1, 4, 3, 2]);
        // The photo is drawn as an element step, like imagery.
        assert_eq!(
            m.draw_plan(true),
            [
                DrawStep::Elements(LayerId(1)),
                DrawStep::Elements(LayerId(4)),
                DrawStep::Elements(LayerId(3)),
                run(true, &[2]),
            ]
        );
    }

    /// The ids of `m`'s layers, bottom first.
    fn stack(m: &LayerManager) -> Vec<u64> {
        m.layers().iter().map(|l| l.id().0).collect()
    }

    #[gpui::test]
    fn new_layers_are_placed_by_kind(cx: &mut gpui::TestAppContext) {
        let mut m = LayerManager::new();
        // Data first, then imagery: the imagery still goes to the bottom.
        m.add_layer(layer_of(cx, LayerKind::Osm, 1));
        m.add_layer(layer_of(cx, LayerKind::Notes, 2));
        m.add_layer(layer_of(cx, LayerKind::Tile, 3));
        assert_eq!(stack(&m), [3, 1, 2]);
        // A second imagery layer goes above the first, under the data.
        m.add_layer(layer_of(cx, LayerKind::Tile, 4));
        assert_eq!(stack(&m), [3, 4, 1, 2]);
        // Reference and vector-tile overlays: above imagery, under the data.
        m.add_layer(layer_of(cx, LayerKind::Reference, 5));
        m.add_layer(layer_of(cx, LayerKind::VectorTile, 6));
        assert_eq!(stack(&m), [3, 4, 5, 6, 1, 2]);
        // New OSM data goes on top of the data, but under the pins.
        m.add_layer(layer_of(cx, LayerKind::Osm, 7));
        assert_eq!(stack(&m), [3, 4, 5, 6, 1, 7, 2]);
        // Pins go on top.
        m.add_layer(layer_of(cx, LayerKind::Extension, 8));
        assert_eq!(stack(&m), [3, 4, 5, 6, 1, 7, 2, 8]);
        // The Layers list shows it the other way up.
        let list: Vec<u64> = m.top_first().map(|(_, l)| l.id().0).collect();
        assert_eq!(list, [8, 2, 7, 1, 6, 5, 4, 3]);
    }

    #[gpui::test]
    fn adding_a_layer_keeps_where_the_user_moved_the_others(cx: &mut gpui::TestAppContext) {
        let mut m = LayerManager::new();
        m.add_layer(layer_of(cx, LayerKind::Tile, 1));
        m.add_layer(layer_of(cx, LayerKind::Osm, 2));
        assert_eq!(stack(&m), [1, 2]);
        // The user raises the imagery above the data.
        m.move_layer(0, 1);
        assert_eq!(stack(&m), [2, 1]);
        // New data goes on top; the imagery stays where it was put.
        m.add_layer(layer_of(cx, LayerKind::Osm, 3));
        assert_eq!(stack(&m), [2, 1, 3]);
        // New imagery goes just above the topmost imagery.
        m.add_layer(layer_of(cx, LayerKind::Tile, 4));
        assert_eq!(stack(&m), [2, 1, 4, 3]);
    }

    fn run(wash: bool, layers: &[u64]) -> DrawStep {
        DrawStep::Canvas(CanvasRun {
            wash,
            layers: layers.iter().map(|&i| LayerId(i)).collect(),
        })
    }

    #[gpui::test]
    fn the_draw_plan_draws_imagery_under_data_with_the_wash_between(cx: &mut gpui::TestAppContext) {
        let mut m = LayerManager::new();
        m.add_layer(layer_of(cx, LayerKind::Tile, 1));
        m.add_layer(layer_of(cx, LayerKind::Osm, 2));
        m.add_layer(layer_of(cx, LayerKind::Notes, 3));
        // One canvas for every non-imagery layer: no more than before.
        assert_eq!(
            m.draw_plan(true),
            [DrawStep::Elements(LayerId(1)), run(true, &[2, 3])]
        );
        // Imagery hidden by the stylesheet: the wash goes to the bottom.
        assert_eq!(m.draw_plan(false), [run(true, &[2, 3])]);
    }

    #[gpui::test]
    fn the_draw_plan_follows_the_stack_when_imagery_is_moved_above_data(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut m = LayerManager::new();
        m.add_layer(layer_of(cx, LayerKind::Tile, 1));
        m.add_layer(layer_of(cx, LayerKind::Osm, 2));
        m.move_layer(0, 1);
        // The data, then the imagery over it, then the wash over that (on
        // an otherwise empty last canvas, which also carries the overlays).
        assert_eq!(
            m.draw_plan(true),
            [
                run(false, &[2]),
                DrawStep::Elements(LayerId(1)),
                run(true, &[])
            ]
        );

        // Imagery between two runs of data: each run is its own canvas.
        m.add_layer(layer_of(cx, LayerKind::Osm, 3));
        m.add_layer(layer_of(cx, LayerKind::Reference, 4));
        assert_eq!(stack(&m), [2, 1, 4, 3]);
        assert_eq!(
            m.draw_plan(true),
            [
                run(false, &[2]),
                DrawStep::Elements(LayerId(1)),
                run(true, &[4, 3])
            ]
        );
    }

    #[gpui::test]
    fn the_draw_plan_leaves_out_hidden_layers(cx: &mut gpui::TestAppContext) {
        let mut m = LayerManager::new();
        assert_eq!(m.draw_plan(true), [run(true, &[])]);
        m.add_layer(layer_of(cx, LayerKind::Tile, 1));
        m.add_layer(layer_of(cx, LayerKind::Tile, 2));
        m.add_layer(layer_of(cx, LayerKind::Osm, 3));
        m.find_layer_mut(LayerId(2)).unwrap().set_visible(false);
        assert_eq!(
            m.draw_plan(true),
            [DrawStep::Elements(LayerId(1)), run(true, &[3])]
        );
        m.find_layer_mut(LayerId(3)).unwrap().set_visible(false);
        assert_eq!(
            m.draw_plan(true),
            [DrawStep::Elements(LayerId(1)), run(true, &[])]
        );
    }

    #[test]
    fn layer_stats_take_the_last_layer_of_each_kind() {
        let mut m = manager_with(&["a", "b"]);
        assert_eq!(m.layer_stats(), LayerStats::default());

        let id = m.alloc_id();
        let mut data = crate::osm::OsmData {
            nodes: std::collections::HashMap::new(),
            ways: std::collections::HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        for n in 1..=3 {
            data.nodes.insert(
                NodeId(n),
                crate::osm::OsmNode {
                    id: NodeId(n),
                    lat: 0.0,
                    lon: 0.0,
                    version: 1,
                    tags: Default::default(),
                },
            );
        }
        let layer =
            crate::layers::osm_layer::OsmLayer::new_with_data(id, "c", std::sync::Arc::new(data));
        m.add_layer(Box::new(layer));
        assert_eq!(
            m.layer_stats(),
            LayerStats {
                cached_files: 0,
                osm_nodes: 3,
                osm_ways: 0
            }
        );
    }

    #[test]
    fn an_osm_layer_without_data_has_no_feature_counts() {
        let layer = crate::layers::osm_layer::OsmLayer::new(LayerId(1));
        assert_eq!(layer.feature_counts(), None);
        let mut m = LayerManager::new();
        m.add_layer(Box::new(layer));
        assert_eq!(m.layer_stats(), LayerStats::default());
    }
}
