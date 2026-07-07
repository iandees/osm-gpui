use gpui::*;

use crate::viewport::Viewport;

pub mod tile_layer;
pub mod osm_layer;
pub mod grid_layer;

/// Trait that all map layers must implement
pub trait MapLayer: Send + Sync {
    /// Get the name of this layer for debugging/UI purposes
    fn name(&self) -> &str;

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

    /// Return hit candidates near a screen-space point. Default: none.
    /// Implementations should only return candidates within their own tolerance.
    fn hit_test(
        &self,
        _viewport: &Viewport,
        _screen_pt: Point<Pixels>,
    ) -> Vec<crate::selection::HitCandidate> {
        Vec::new()
    }

    /// Return every feature inside a screen-space rectangle. Default: none.
    /// Nodes: point inside the rect. Ways: fully enclosed (all vertices
    /// inside). Implementations only return their own features.
    fn hit_test_rect(
        &self,
        _viewport: &Viewport,
        _rect: Bounds<Pixels>,
    ) -> Vec<crate::selection::FeatureRef> {
        Vec::new()
    }

    /// Tell the layer which features are currently selected.
    /// Default: no-op. OsmLayer overrides this to store the set.
    fn set_highlight(&mut self, _features: &[crate::selection::FeatureRef]) {}

    /// Return key/value tags for the given feature if this layer owns it.
    /// Default: `None`.
    fn feature_tags(
        &self,
        _feature: &crate::selection::FeatureRef,
    ) -> Option<Vec<(String, String)>> {
        None
    }

    /// Draw a highlight overlay for `feature` if it belongs to this layer.
    /// Default: no-op.
    fn render_highlight(
        &self,
        _viewport: &Viewport,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _feature: &crate::selection::FeatureRef,
    ) {}

    /// Set a transient screen-space offset to apply when rendering the given
    /// node ids, for live drag feedback. Does not touch the underlying data.
    /// Default: no-op.
    fn set_drag_preview(&mut self, _node_ids: &std::collections::HashSet<i64>, _delta: Point<Pixels>) {}

    /// Clear any transient drag preview. Default: no-op.
    fn clear_drag_preview(&mut self) {}

    /// Whether this layer has uncommitted-to-disk edits (e.g. moved nodes).
    /// Default: `false`.
    fn is_modified(&self) -> bool {
        false
    }

    /// Current (lat, lon) of a node this layer owns, if any. Default: `None`.
    fn node_lat_lon(&self, _node_id: i64) -> Option<(f64, f64)> {
        None
    }

    /// The member node ids of a way this layer owns, if any. Default: `None`.
    fn way_node_ids(&self, _way_id: i64) -> Option<Vec<i64>> {
        None
    }

    /// Commit a set of `(node_id, new_lat, new_lon)` moves into this layer's
    /// data, rebuilding derived caches once. Default: no-op.
    fn commit_node_moves(&mut self, _moves: &[(i64, f64, f64)]) {}
}

/// Manager for all map layers
pub struct LayerManager {
    layers: Vec<Box<dyn MapLayer>>,
}

impl LayerManager {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
        }
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

    /// Find a layer by name (immutable)
    pub fn find_layer(&self, name: &str) -> Option<&Box<dyn MapLayer>> {
        self.layers.iter().find(|layer| layer.name() == name)
    }

    /// Find a layer by name
    pub fn find_layer_mut(&mut self, name: &str) -> Option<&mut Box<dyn MapLayer>> {
        self.layers.iter_mut().find(|layer| layer.name() == name)
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
    pub fn render_all_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
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
            .map(|layer| layer.hit_test(viewport, screen_pt))
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
            .flat_map(|layer| layer.hit_test_rect(viewport, rect))
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
                    .hit_test(viewport, screen_pt)
                    .into_iter()
                    .filter(|c| selected.contains(&c.feature))
                    .collect()
            })
            .collect();
        crate::selection::resolve_hits(per_layer)
    }

    /// Render `feature`'s highlight by asking the owning layer (matched by name).
    /// No-op if no layer with that name exists.
    pub fn render_highlight(
        &self,
        feature: &crate::selection::FeatureRef,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        if let Some(layer) = self.find_layer(&feature.layer_name) {
            if layer.is_visible() {
                layer.render_highlight(viewport, bounds, window, feature);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn apply_move(items: &mut Vec<&'static str>, from: usize, to: usize) {
        let len = items.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let item = items.remove(from);
        items.insert(to, item);
    }

    #[test]
    fn move_layer_down() {
        let mut v = vec!["a", "b", "c"];
        apply_move(&mut v, 0, 2);
        assert_eq!(v, vec!["b", "c", "a"]);
    }

    #[test]
    fn move_layer_up() {
        let mut v = vec!["a", "b", "c"];
        apply_move(&mut v, 2, 0);
        assert_eq!(v, vec!["c", "a", "b"]);
    }

    #[test]
    fn move_layer_same_index_is_noop() {
        let mut v = vec!["a", "b"];
        apply_move(&mut v, 1, 1);
        assert_eq!(v, vec!["a", "b"]);
    }

    fn apply_remove_at(items: &mut Vec<&'static str>, index: usize) -> Option<&'static str> {
        if index >= items.len() {
            return None;
        }
        Some(items.remove(index))
    }

    #[test]
    fn remove_at_removes_item() {
        let mut v = vec!["a", "b", "c"];
        let removed = apply_remove_at(&mut v, 1);
        assert_eq!(removed, Some("b"));
        assert_eq!(v, vec!["a", "c"]);
    }

    #[test]
    fn remove_at_out_of_bounds_is_none() {
        let mut v = vec!["a", "b"];
        assert_eq!(apply_remove_at(&mut v, 5), None);
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn move_layer_out_of_bounds_is_noop() {
        let mut v = vec!["a", "b"];
        apply_move(&mut v, 0, 99);
        apply_move(&mut v, 99, 0);
        assert_eq!(v, vec!["a", "b"]);
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
        let node = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: HashMap::new() };
        let mut nodes = HashMap::new();
        nodes.insert(1, node);
        let data = Arc::new(OsmData { nodes, ways: Vec::new(), relations: Vec::new(), bounds: None });
        let layer = OsmLayer::new_with_data("L", data);

        let mut manager = LayerManager::new();
        manager.add_layer(Box::new(layer));

        let viewport = Viewport::new(center_lat, center_lon, 18.0, size(px(800.0), px(600.0)));
        let selected = vec![FeatureRef { layer_name: "L".to_string(), kind: FeatureKind::Node, id: 1 }];

        let hit = manager.hit_test_selection(&viewport, point(px(400.0), px(300.0)), &selected);
        assert_eq!(hit, Some(selected[0].clone()));
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
        let node = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: HashMap::new() };
        let mut nodes = HashMap::new();
        nodes.insert(1, node);
        let data = Arc::new(OsmData { nodes, ways: Vec::new(), relations: Vec::new(), bounds: None });
        let layer = OsmLayer::new_with_data("L", data);

        let mut manager = LayerManager::new();
        manager.add_layer(Box::new(layer));

        let viewport = Viewport::new(center_lat, center_lon, 18.0, size(px(800.0), px(600.0)));
        // Selection references a *different* node id than the one under the cursor.
        let selected = vec![FeatureRef { layer_name: "L".to_string(), kind: FeatureKind::Node, id: 999 }];

        let hit = manager.hit_test_selection(&viewport, point(px(400.0), px(300.0)), &selected);
        assert_eq!(hit, None);
    }
}
