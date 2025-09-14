use gpui::*;
use std::sync::Arc;

use crate::viewport::Viewport;

pub mod tile_layer;
pub mod osm_layer;
pub mod grid_layer;

/// Trait that all map layers must implement
pub trait MapLayer: Send + Sync {
    /// Get the name of this layer for debugging/UI purposes
    fn name(&self) -> &'static str;

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

    /// Allow downcasting to concrete types
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
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
}
