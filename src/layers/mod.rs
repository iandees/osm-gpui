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
