use gpui::*;
use std::collections::HashSet;

use crate::viewport::Viewport;

pub mod diff;
pub mod grid_layer;
pub mod osm_layer;
pub mod tile_layer;

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

    /// Draw a highlight overlay for `feature` if it belongs to this layer.
    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &crate::selection::FeatureRef,
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
        self.layers
            .iter()
            .filter(|layer| layer.is_visible())
            .map(|layer| {
                layer
                    .as_editable()
                    .map(|e| e.hit_test(viewport, screen_pt))
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
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        if let Some(layer) = self.find_layer(feature.layer_id) {
            if layer.is_visible() {
                if let Some(editable) = layer.as_editable() {
                    editable.render_highlight(viewport, bounds, window, feature);
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
                bounds: None,
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
            bounds: None,
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
            bounds: None,
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
