use gpui::*;
use std::sync::Arc;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::osm::OsmData;
use crate::coordinates::{is_point_valid, validate_coords};
use crate::selection::{FeatureKind, FeatureRef, HitCandidate, point_to_segment_distance};

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

    fn hit_test(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = 8.0;
        const WAY_TOL: f32 = 6.0;

        let Some(ref data) = self.osm_data else { return Vec::new(); };

        // Phase 1: nodes within NODE_TOL.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        for node in data.nodes.values() {
            if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
                if !viewport.is_visible(lat, lon) { continue; }
                let sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) { continue; }
                let dx = sp.x.0 - screen_pt.x.0;
                let dy = sp.y.0 - screen_pt.y.0;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= NODE_TOL {
                    node_hits.push(HitCandidate {
                        feature: FeatureRef {
                            layer_name: self.name.clone(),
                            kind: FeatureKind::Node,
                            id: node.id,
                        },
                        kind: FeatureKind::Node,
                        dist_px: dist,
                    });
                }
            }
        }
        if !node_hits.is_empty() {
            return node_hits;
        }

        // Phase 2: ways within WAY_TOL. Compute shortest segment distance per way.
        let mut way_hits: Vec<HitCandidate> = Vec::new();
        for way in data.ways.iter() {
            if way.nodes.len() < 2 { continue; }
            let mut projected: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
            for node_id in &way.nodes {
                if let Some(n) = data.nodes.get(node_id) {
                    if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                        let sp = viewport.geo_to_screen(lat, lon);
                        if is_point_valid(sp) {
                            projected.push(sp);
                        }
                    }
                }
            }
            if projected.len() < 2 { continue; }
            let mut best = f32::INFINITY;
            for w in projected.windows(2) {
                let d = point_to_segment_distance(screen_pt, w[0], w[1]);
                if d < best { best = d; }
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_name: self.name.clone(),
                        kind: FeatureKind::Way,
                        id: way.id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: best,
                });
            }
        }
        way_hits
    }
}

#[cfg(test)]
mod tests {
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::MapLayer;
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::FeatureKind;
    use crate::viewport::Viewport;
    use gpui::{point, px, size};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    /// Build a viewport whose center projects a chosen (lat, lon) to the middle
    /// of an 800x600 map area. Zoom 18 is high enough that a degree-scale offset
    /// in node positions translates to many pixels.
    fn viewport_centered_on(lat: f64, lon: f64) -> Viewport {
        Viewport::new(lat, lon, 18.0, size(px(800.0), px(600.0)))
    }

    fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> Arc<OsmData> {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id, n);
        }
        Arc::new(OsmData {
            nodes: map,
            ways,
            relations: Vec::new(),
            bounds: None,
        })
    }

    #[test]
    fn hit_test_node_wins_over_coincident_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // Node #1 projects to viewport center (400, 300). Click exactly on it.
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, FeatureKind::Node);
        assert_eq!(hits[0].feature.id, 1);
    }

    #[test]
    fn hit_test_falls_through_to_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon - 0.001, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // Click at viewport center (400,300), which is on the way between n1 and n2
        // but far from either endpoint.
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
        assert!(hits.iter().any(|h| h.feature.id == 10));
    }

    #[test]
    fn hit_test_no_match_returns_empty() {
        let n = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n], vec![]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data("L", data);

        // Node is at (400, 300); click far away.
        let hits = layer.hit_test(&viewport, point(px(50.0), px(50.0)));
        assert!(hits.is_empty(), "unexpected hits: {:?}", hits);
    }
}
