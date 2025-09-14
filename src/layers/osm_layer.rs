use gpui::*;
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::OsmData;
use crate::coordinates::{is_point_valid, validate_coords};

/// Layer for rendering OSM vector data (nodes and ways)
pub struct OsmLayer {
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
            visible: true,
            osm_data: None,
            node_color: rgb(0xFFD700), // Yellow for nodes
            way_color: rgb(0x4169E1),  // Royal blue for ways
            node_size: 10.0,
            way_width: 4.0,
        }
    }

    /// Set the OSM data for this layer
    pub fn set_osm_data(&mut self, osm_data: Arc<OsmData>) {
        self.osm_data = Some(osm_data);
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
    fn name(&self) -> &'static str {
        "OSM Data"
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // OSM data is rendered using canvas, not elements
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, _bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(ref osm_data) = self.osm_data else {
            return;
        };

        eprintln!("Rendering OSM data: {} nodes, {} ways", osm_data.nodes.len(), osm_data.ways.len());

        // Render OSM ways as lines
        for (way_idx, way) in osm_data.ways.iter().enumerate() {
            if way.nodes.len() >= 2 {
                let mut valid_points = Vec::new();
                for node_id in &way.nodes {
                    if let Some(node) = osm_data.nodes.get(node_id) {
                        if let Some((valid_lat, valid_lon)) = validate_coords(node.lat, node.lon) {
                            let screen_pos = viewport.geo_to_screen(valid_lat, valid_lon);
                            if is_point_valid(screen_pos) {
                                valid_points.push(screen_pos);
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

                    // Debug: print first few way screen positions
                    if way_idx < 3 {
                        eprintln!("Way {}: {:?}", way_idx, valid_points);
                    }
                }
            }
        }

        // Render OSM nodes as rectangles
        let mut node_count = 0;
        for node in osm_data.nodes.values() {
            if let Some((valid_lat, valid_lon)) = validate_coords(node.lat, node.lon) {
                if viewport.is_visible(valid_lat, valid_lon) {
                    let screen_pos = viewport.geo_to_screen(valid_lat, valid_lon);
                    if is_point_valid(screen_pos) {
                        let rect_size = px(self.node_size);
                        let mut builder = PathBuilder::fill();
                        builder.move_to(point(
                            screen_pos.x - rect_size / 2.0,
                            screen_pos.y - rect_size / 2.0,
                        ));
                        builder.line_to(point(
                            screen_pos.x + rect_size / 2.0,
                            screen_pos.y - rect_size / 2.0,
                        ));
                        builder.line_to(point(
                            screen_pos.x + rect_size / 2.0,
                            screen_pos.y + rect_size / 2.0,
                        ));
                        builder.line_to(point(
                            screen_pos.x - rect_size / 2.0,
                            screen_pos.y + rect_size / 2.0,
                        ));
                        builder.close();

                        if let Ok(path) = builder.build() {
                            window.paint_path(path, self.node_color);
                        }

                        // Debug: print first few node screen positions
                        if node_count < 5 {
                            eprintln!("Node {}: screen_pos={:?} lat/lon=({}, {})",
                                node_count, screen_pos, valid_lat, valid_lon);
                        }
                        node_count += 1;
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

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
