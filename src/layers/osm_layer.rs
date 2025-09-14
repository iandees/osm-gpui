use gpui::*;
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::OsmData;
use crate::coordinates::{is_point_valid, validate_coords};

/// Layer for rendering OSM vector data (nodes and ways)
pub struct OsmLayer {
    name: String,
    visible: bool,
    osm_data: Option<Arc<OsmData>>,
    node_color: Rgba,
    way_color: Rgba,
    node_size: f32,
    way_width: f32,
}

impl OsmLayer {
    pub fn new() -> Self {
        Self {
            name: "OSM Data".to_string(),
            visible: true,
            osm_data: None,
            node_color: rgb(0xFFD700), // Yellow for nodes
            way_color: rgb(0x4169E1),  // Royal blue for ways
            node_size: 10.0,
            way_width: 4.0,
        }
    }

    pub fn new_with_data<N: Into<String>>(name: N, osm_data: Arc<OsmData>) -> Self {
        Self {
            name: name.into(),
            visible: true,
            osm_data: Some(osm_data),
            node_color: rgb(0xFFD700),
            way_color: rgb(0x4169E1),
            node_size: 10.0,
            way_width: 4.0,
        }
    }

    /// Set the OSM data for this layer
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.osm_data = Some(osm_data);
    }

    /// Get the OSM data from this layer
    pub fn get_osm_data(&self) -> Option<Arc<OsmData>> {
        self.osm_data.clone()
    }

    /// Clear the OSM data
    pub fn clear_osm_data(&mut self) {
        self.osm_data = None;
    }

    /// Check if this layer has data
    pub fn has_data(&self) -> bool {
        self.osm_data.is_some()
    }

    /// Set styling options
    pub fn set_style(&mut self, node_color: Rgba, way_color: Rgba, node_size: f32, way_width: f32) {
        self.node_color = node_color;
        self.way_color = way_color;
        self.node_size = node_size;
        self.way_width = way_width;
    }
}

impl MapLayer for OsmLayer {
    fn name(&self) -> &str { &self.name }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn render_elements(&self, viewport: &Viewport) -> Vec<AnyElement> {
        let mut elements = Vec::new();

        let Some(ref osm_data) = self.osm_data else {
            return elements;
        };

        // Render OSM nodes as positioned div elements (same coordinate space as tiles)
        for node in osm_data.nodes.values() {
            if let Some((valid_lat, valid_lon)) = validate_coords(node.lat, node.lon) {
                if viewport.is_visible(valid_lat, valid_lon) {
                    let screen_pos = viewport.geo_to_screen(valid_lat, valid_lon);
                    if is_point_valid(screen_pos) {
                        let node_element = div()
                            .absolute()
                            .left(px(screen_pos.x.0 - self.node_size / 2.0))
                            .top(px(screen_pos.y.0 - self.node_size / 2.0))
                            .w(px(self.node_size))
                            .h(px(self.node_size))
                            .bg(self.node_color)
                            .into_any_element();

                        elements.push(node_element);
                    }
                }
            }
        }

        // Remove the GPUI element way rendering to improve performance
        // Ways will be rendered in canvas for better performance

        elements
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(ref osm_data) = self.osm_data else {
            return;
        };

        let origin_x = bounds.origin.x;
        let origin_y = bounds.origin.y;

        // Render OSM ways as lines using direct viewport coordinates
        for way in osm_data.ways.iter() {
            if way.nodes.len() >= 2 {
                let mut valid_points = Vec::new();
                for node_id in &way.nodes {
                    if let Some(node) = osm_data.nodes.get(node_id) {
                        if let Some((valid_lat, valid_lon)) = validate_coords(node.lat, node.lon) {
                            let screen_pos = viewport.geo_to_screen(valid_lat, valid_lon);
                            if is_point_valid(screen_pos) {
                                // Apply canvas bounds origin offset so paths line up with absolutely positioned elements
                                valid_points.push(point(
                                    px(screen_pos.x.0 + origin_x.0),
                                    px(screen_pos.y.0 + origin_y.0),
                                ));
                            }
                        }
                    }
                }

                if valid_points.len() >= 2 {
                    let mut builder = PathBuilder::stroke(px(self.way_width));
                    for (i, screen_pos) in valid_points.iter().enumerate() {
                        if i == 0 {
                            builder.move_to(*screen_pos);
                        } else {
                            builder.line_to(*screen_pos);
                        }
                    }

                    if let Ok(path) = builder.build() {
                        window.paint_path(path, self.way_color);
                    }
                }
            }
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        if let Some(ref osm_data) = self.osm_data {
            vec![
                ("Nodes".to_string(), osm_data.nodes.len().to_string()),
                ("Ways".to_string(), osm_data.ways.len().to_string()),
                ("Node Size".to_string(), self.node_size.to_string()),
                ("Way Width".to_string(), self.way_width.to_string()),
            ]
        } else {
            vec![("Status".to_string(), "No data loaded".to_string())]
        }
    }
}
