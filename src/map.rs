use crate::viewport::Viewport;
use gpui::*;

/// Map data structures
#[derive(Debug, Clone)]
pub struct MapFeature {
    pub id: String,
    pub geometry: FeatureGeometry,
    pub properties: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum FeatureGeometry {
    Point {
        lat: f64,
        lon: f64,
    },
    LineString {
        points: Vec<(f64, f64)>,
    },
    Polygon {
        exterior: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
}

/// Map layer for organizing different types of features
#[derive(Debug, Clone)]
pub struct MapLayer {
    pub name: String,
    pub features: Vec<MapFeature>,
    pub visible: bool,
    pub style: LayerStyle,
}

#[derive(Debug, Clone)]
pub struct LayerStyle {
    pub stroke_color: Hsla,
    pub stroke_width: f32,
    pub fill_color: Option<Hsla>,
    pub point_radius: f32,
}

impl Default for LayerStyle {
    fn default() -> Self {
        Self {
            stroke_color: rgb(0x3b82f6).into(),
            stroke_width: 1.0,
            fill_color: None,
            point_radius: 3.0,
        }
    }
}

/// Main map view component
pub struct MapView {
    viewport: Viewport,
    layers: Vec<MapLayer>,
    background_color: Hsla,
    grid_visible: bool,
    debug_info_visible: bool,
}

impl MapView {
    pub fn new(cx: &mut AppContext) -> Self {
        let screen_size = size(px(800.0), px(600.0));

        // Default to New York City coordinates
        let viewport = Viewport::new(40.7128, -74.0060, 10.0, screen_size);

        let mut map_view = Self {
            viewport,
            layers: Vec::new(),
            background_color: rgb(0x1a202c).into(),
            grid_visible: true,
            debug_info_visible: false,
        };

        // Add sample data
        map_view.add_sample_data();

        map_view
    }

    /// Add some sample map data for demonstration
    fn add_sample_data(&mut self) {
        // Create a sample layer with some features around NYC
        let mut sample_layer = MapLayer {
            name: "Sample Features".to_string(),
            features: Vec::new(),
            visible: true,
            style: LayerStyle {
                stroke_color: rgb(0x10b981).into(),
                stroke_width: 2.0,
                fill_color: Some(hsla(0.33, 0.6, 0.5, 0.3)),
                point_radius: 5.0,
            },
        };

        // Add some sample points of interest
        sample_layer.features.push(MapFeature {
            id: "central_park".to_string(),
            geometry: FeatureGeometry::Point {
                lat: 40.7829,
                lon: -73.9654,
            },
            properties: {
                let mut props = std::collections::HashMap::new();
                props.insert("name".to_string(), "Central Park".to_string());
                props.insert("type".to_string(), "park".to_string());
                props
            },
        });

        sample_layer.features.push(MapFeature {
            id: "brooklyn_bridge".to_string(),
            geometry: FeatureGeometry::Point {
                lat: 40.7061,
                lon: -73.9969,
            },
            properties: {
                let mut props = std::collections::HashMap::new();
                props.insert("name".to_string(), "Brooklyn Bridge".to_string());
                props.insert("type".to_string(), "landmark".to_string());
                props
            },
        });

        // Add a sample line (simplified subway line)
        sample_layer.features.push(MapFeature {
            id: "subway_line".to_string(),
            geometry: FeatureGeometry::LineString {
                points: vec![
                    (40.7831, -73.9712), // Times Square
                    (40.7614, -73.9776), // Herald Square
                    (40.7505, -73.9934), // Union Square
                    (40.7282, -74.0776), // Wall Street
                ],
            },
            properties: {
                let mut props = std::collections::HashMap::new();
                props.insert("name".to_string(), "Sample Subway Line".to_string());
                props.insert("type".to_string(), "transit".to_string());
                props
            },
        });

        self.layers.push(sample_layer);
    }

    /// Add a new layer to the map
    pub fn add_layer(&mut self, layer: MapLayer) {
        self.layers.push(layer);
    }

    /// Remove a layer by name
    pub fn remove_layer(&mut self, name: &str) {
        self.layers.retain(|layer| layer.name != name);
    }

    /// Toggle layer visibility
    pub fn toggle_layer(&mut self, name: &str) {
        if let Some(layer) = self.layers.iter_mut().find(|l| l.name == name) {
            layer.visible = !layer.visible;
        }
    }

    /// Set viewport to show all features
    pub fn fit_to_features(&mut self) {
        if self.layers.is_empty() {
            return;
        }

        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;

        for layer in &self.layers {
            if !layer.visible {
                continue;
            }

            for feature in &layer.features {
                match &feature.geometry {
                    FeatureGeometry::Point { lat, lon } => {
                        min_lat = min_lat.min(*lat);
                        max_lat = max_lat.max(*lat);
                        min_lon = min_lon.min(*lon);
                        max_lon = max_lon.max(*lon);
                    }
                    FeatureGeometry::LineString { points } => {
                        for (lat, lon) in points {
                            min_lat = min_lat.min(*lat);
                            max_lat = max_lat.max(*lat);
                            min_lon = min_lon.min(*lon);
                            max_lon = max_lon.max(*lon);
                        }
                    }
                    FeatureGeometry::Polygon { exterior, holes: _ } => {
                        for (lat, lon) in exterior {
                            min_lat = min_lat.min(*lat);
                            max_lat = max_lat.max(*lat);
                            min_lon = min_lon.min(*lon);
                            max_lon = max_lon.max(*lon);
                        }
                    }
                }
            }
        }

        if min_lat != f64::INFINITY {
            let center_lat = (min_lat + max_lat) / 2.0;
            let center_lon = (min_lon + max_lon) / 2.0;

            // Calculate appropriate zoom level
            let lat_span = max_lat - min_lat;
            let lon_span = max_lon - min_lon;
            let max_span = lat_span.max(lon_span);

            // Rough zoom calculation (could be more sophisticated)
            let zoom = (180.0 / (max_span * 1.5)).log2().floor().max(1.0).min(18.0);

            self.viewport.pan_to(center_lat, center_lon);
            self.viewport.set_zoom(zoom);
        }
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Window) {
        self.viewport.handle_mouse_down(event.position);
        cx.refresh();
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Window) {
        if self.viewport.handle_scroll(event.position, event.delta) {
            cx.refresh();
        }
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Window) {
        match event.keystroke.key.as_str() {
            "g" => {
                self.grid_visible = !self.grid_visible;
                cx.refresh();
            }
            "d" => {
                self.debug_info_visible = !self.debug_info_visible;
                cx.refresh();
            }
            "f" => {
                self.fit_to_features();
                cx.refresh();
            }
            "r" => {
                self.viewport.reset(40.7128, -74.0060, 10.0);
                cx.refresh();
            }
            _ => {}
        }
    }

    /// Render the geographic grid
    fn render_grid(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        if !self.grid_visible {
            return;
        }

        let grid_color = rgb(0x374151);

        // Determine appropriate grid spacing based on zoom level
        let grid_spacing = match self.viewport.zoom_level() {
            z if z <= 3.0 => 10.0,   // 10 degree grid
            z if z <= 6.0 => 5.0,    // 5 degree grid
            z if z <= 8.0 => 1.0,    // 1 degree grid
            z if z <= 10.0 => 0.5,   // 0.5 degree grid
            z if z <= 12.0 => 0.1,   // 0.1 degree grid
            z if z <= 14.0 => 0.05,  // 0.05 degree grid
            z if z <= 16.0 => 0.01,  // 0.01 degree grid
            z if z <= 18.0 => 0.005, // 0.005 degree grid
            _ => 0.001,              // 0.001 degree grid for very high zoom
        };

        // Calculate geographic bounds of the current view
        let top_left = self.viewport.screen_to_geo(point(px(0.0), px(0.0)));
        let bottom_right = self
            .viewport
            .screen_to_geo(point(bounds.size.width, bounds.size.height));

        // Draw longitude lines (vertical lines)
        let start_lon = (top_left.1 / grid_spacing).floor() * grid_spacing;
        let end_lon = (bottom_right.1 / grid_spacing).ceil() * grid_spacing;

        let mut lon = start_lon;
        while lon <= end_lon {
            let top_screen = self.viewport.geo_to_screen(top_left.0, lon);
            let bottom_screen = self.viewport.geo_to_screen(bottom_right.0, lon);

            // Only draw if the line is within screen bounds
            if top_screen.x >= px(-10.0) && top_screen.x <= bounds.size.width + px(10.0) {
                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(top_screen.x, px(0.0)));
                builder.line_to(point(bottom_screen.x, bounds.size.height));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, grid_color);
                }
            }
            lon += grid_spacing;
        }

        // Draw latitude lines (horizontal lines)
        let start_lat = (bottom_right.0 / grid_spacing).floor() * grid_spacing;
        let end_lat = (top_left.0 / grid_spacing).ceil() * grid_spacing;

        let mut lat = start_lat;
        while lat <= end_lat {
            let left_screen = self.viewport.geo_to_screen(lat, top_left.1);
            let right_screen = self.viewport.geo_to_screen(lat, bottom_right.1);

            // Only draw if the line is within screen bounds
            if left_screen.y >= px(-10.0) && left_screen.y <= bounds.size.height + px(10.0) {
                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(px(0.0), left_screen.y));
                builder.line_to(point(bounds.size.width, right_screen.y));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, grid_color);
                }
            }
            lat += grid_spacing;
        }
    }

    /// Render map features
    fn render_features(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        for layer in &self.layers {
            if !layer.visible {
                continue;
            }

            for feature in &layer.features {
                match &feature.geometry {
                    FeatureGeometry::Point { lat, lon } => {
                        let screen_pos = self.viewport.geo_to_screen(*lat, *lon);

                        // Only render if visible
                        if screen_pos.x >= px(-20.0)
                            && screen_pos.x <= bounds.size.width + px(20.0)
                            && screen_pos.y >= px(-20.0)
                            && screen_pos.y <= bounds.size.height + px(20.0)
                        {
                            // Draw feature as a circle
                            let radius = px(layer.style.point_radius);
                            let mut builder = PathBuilder::fill();
                            builder.circle(screen_pos, radius);
                            if let Ok(path) = builder.build() {
                                window.paint_path(path, layer.style.stroke_color);
                            }
                        }
                    }
                    FeatureGeometry::LineString { points } => {
                        if points.len() < 2 {
                            continue;
                        }

                        let mut builder = PathBuilder::stroke(px(layer.style.stroke_width));
                        let first_point = self.viewport.geo_to_screen(points[0].0, points[0].1);
                        builder.move_to(first_point);

                        for point in points.iter().skip(1) {
                            let screen_point = self.viewport.geo_to_screen(point.0, point.1);
                            builder.line_to(screen_point);
                        }

                        if let Ok(path) = builder.build() {
                            window.paint_path(path, layer.style.stroke_color);
                        }
                    }
                    FeatureGeometry::Polygon { exterior, holes: _ } => {
                        if exterior.len() < 3 {
                            continue;
                        }

                        // Fill the polygon if fill color is specified
                        if let Some(fill_color) = layer.style.fill_color {
                            let mut builder = PathBuilder::fill();
                            let first_point =
                                self.viewport.geo_to_screen(exterior[0].0, exterior[0].1);
                            builder.move_to(first_point);

                            for point in exterior.iter().skip(1) {
                                let screen_point = self.viewport.geo_to_screen(point.0, point.1);
                                builder.line_to(screen_point);
                            }
                            builder.close();

                            if let Ok(path) = builder.build() {
                                window.paint_path(path, fill_color);
                            }
                        }

                        // Draw the polygon outline
                        let mut builder = PathBuilder::stroke(px(layer.style.stroke_width));
                        let first_point = self.viewport.geo_to_screen(exterior[0].0, exterior[0].1);
                        builder.move_to(first_point);

                        for point in exterior.iter().skip(1) {
                            let screen_point = self.viewport.geo_to_screen(point.0, point.1);
                            builder.line_to(screen_point);
                        }
                        builder.close();

                        if let Ok(path) = builder.build() {
                            window.paint_path(path, layer.style.stroke_color);
                        }
                    }
                }
            }
        }
    }
}

impl Render for MapView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_size = cx.viewport_size();
        self.viewport.update_size(viewport_size);

        let grid_visible = self.grid_visible;
        let debug_info_visible = self.debug_info_visible;
        let zoom_level = self.viewport.zoom_level();
        let center = self.viewport.center();
        let feature_count: usize = self.layers.iter().map(|l| l.features.len()).sum();

        div()
            .w_full()
            .h_full()
            .bg(self.background_color)
            .relative()
            .on_mouse_down(MouseButton::Left, |view, event, cx| {
                view.handle_mouse_down(event, cx);
            })
            .on_mouse_move(|view, event, cx| {
                if view.viewport.handle_mouse_move(event.position) {
                    cx.refresh();
                }
            })
            .on_mouse_up(MouseButton::Left, |view, _event, cx| {
                view.viewport.handle_mouse_up();
                cx.refresh();
            })
            .on_scroll_wheel(|view, event, cx| {
                view.handle_scroll(event, cx);
            })
            .on_key_down(|view, event, cx| {
                view.handle_key_down(event, cx);
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        // Render grid
                        if grid_visible {
                            let grid_color = rgb(0x374151);

                            // Determine appropriate grid spacing based on zoom level
                            let grid_spacing = match zoom_level {
                                z if z <= 3.0 => 10.0,   // 10 degree grid
                                z if z <= 6.0 => 5.0,    // 5 degree grid
                                z if z <= 8.0 => 1.0,    // 1 degree grid
                                z if z <= 10.0 => 0.5,   // 0.5 degree grid
                                z if z <= 12.0 => 0.1,   // 0.1 degree grid
                                z if z <= 14.0 => 0.05,  // 0.05 degree grid
                                z if z <= 16.0 => 0.01,  // 0.01 degree grid
                                z if z <= 18.0 => 0.005, // 0.005 degree grid
                                _ => 0.001,              // 0.001 degree grid for very high zoom
                            };

                            // Calculate viewport bounds for coordinate conversion
                            let lat_span = 180.0 / (2.0_f64.powf(zoom_level));
                            let lon_span = 360.0 / (2.0_f64.powf(zoom_level));
                            let pixels_per_degree_lat = bounds.size.height.to_f64() / lat_span;
                            let pixels_per_degree_lon = bounds.size.width.to_f64() / lon_span;

                            // Calculate geographic bounds of the current view
                            let top_left_lon = center.1 - lon_span / 2.0;
                            let top_left_lat = center.0 + lat_span / 2.0;
                            let bottom_right_lon = center.1 + lon_span / 2.0;
                            let bottom_right_lat = center.0 - lat_span / 2.0;

                            // Draw longitude lines (vertical lines)
                            let start_lon = (top_left_lon / grid_spacing).floor() * grid_spacing;
                            let end_lon = (bottom_right_lon / grid_spacing).ceil() * grid_spacing;

                            let mut lon = start_lon;
                            while lon <= end_lon {
                                let x = bounds.size.width * 0.5
                                    + px(((lon - center.1) * pixels_per_degree_lon) as f32);

                                // Only draw if the line is within screen bounds
                                if x >= px(-10.0) && x <= bounds.size.width + px(10.0) {
                                    let mut builder = PathBuilder::stroke(px(1.0));
                                    builder.move_to(point(x, px(0.0)));
                                    builder.line_to(point(x, bounds.size.height));
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, grid_color);
                                    }
                                }
                                lon += grid_spacing;
                            }

                            // Draw latitude lines (horizontal lines)
                            let start_lat =
                                (bottom_right_lat / grid_spacing).floor() * grid_spacing;
                            let end_lat = (top_left_lat / grid_spacing).ceil() * grid_spacing;

                            let mut lat = start_lat;
                            while lat <= end_lat {
                                let y = bounds.size.height * 0.5
                                    - px(((lat - center.0) * pixels_per_degree_lat) as f32);

                                // Only draw if the line is within screen bounds
                                if y >= px(-10.0) && y <= bounds.size.height + px(10.0) {
                                    let mut builder = PathBuilder::stroke(px(1.0));
                                    builder.move_to(point(px(0.0), y));
                                    builder.line_to(point(bounds.size.width, y));
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, grid_color);
                                    }
                                }
                                lat += grid_spacing;
                            }
                        }
                    },
                )
                .w_full()
                .h_full(),
            )
            .when(debug_info_visible, |div| {
                div.child(
                    div()
                        .absolute()
                        .top(px(10.0))
                        .left(px(10.0))
                        .p_4()
                        .bg(hsla(0.0, 0.0, 0.0, 0.8))
                        .rounded_lg()
                        .text_color(rgb(0xffffff))
                        .text_sm()
                        .child("Map Debug Info")
                        .child(div().child(format!("Zoom: {:.2}", zoom_level)))
                        .child(div().child(format!("Center: {:.6}, {:.6}", center.0, center.1)))
                        .child(div().child(format!("Features: {}", feature_count)))
                        .child(
                            div().child(format!(
                                "Grid: {}",
                                if grid_visible { "On" } else { "Off" }
                            )),
                        ),
                )
            })
            .child(
                div()
                    .absolute()
                    .bottom(px(10.0))
                    .left(px(10.0))
                    .p_2()
                    .bg(hsla(0.0, 0.0, 0.0, 0.6))
                    .text_color(rgb(0xffffff))
                    .text_xs()
                    .child("Controls: Drag to pan, scroll to zoom")
                    .child(
                        div().child("G: Toggle grid, D: Debug info, F: Fit to features, R: Reset"),
                    ),
            )
    }
}
