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

    /// Tell the layer which feature (if any) is currently selected.
    /// Default: no-op. OsmLayer overrides this to drive `render_elements`.
    fn set_highlight(&mut self, _feature: Option<crate::selection::FeatureRef>) {}

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
}
