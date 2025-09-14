use gpui::*;

use crate::layers::MapLayer;
use crate::viewport::Viewport;
use crate::coordinates::{is_point_valid, validate_coords};

/// Layer for rendering coordinate grid lines
pub struct GridLayer {
    visible: bool,
    grid_color: Rgba,
}

impl GridLayer {
    pub fn new() -> Self {
        Self {
            visible: true,
            grid_color: rgb(0x374151),
        }
    }

    pub fn set_color(&mut self, color: Rgba) {
        self.grid_color = color;
    }

    /// Determine appropriate grid spacing based on zoom level
    fn calculate_grid_spacing(zoom_level: f64) -> f64 {
        match zoom_level {
            z if z <= 3.0 => 10.0,   // 10 degree grid
            z if z <= 6.0 => 5.0,    // 5 degree grid
            z if z <= 8.0 => 1.0,    // 1 degree grid
            z if z <= 10.0 => 0.5,   // 0.5 degree grid
            z if z <= 12.0 => 0.1,   // 0.1 degree grid
            z if z <= 14.0 => 0.05,  // 0.05 degree grid
            z if z <= 16.0 => 0.01,  // 0.01 degree grid
            z if z <= 18.0 => 0.005, // 0.005 degree grid
            _ => 0.001,              // 0.001 degree grid for very high zoom
        }
    }
}

impl MapLayer for GridLayer {
    fn name(&self) -> &'static str {
        "Coordinate Grid"
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        // Grid is rendered using canvas, not elements
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let zoom_level = viewport.zoom_level();
        let (center_lat, center_lon) = viewport.center();
        let bounds_geo = viewport.visible_bounds();

        let grid_spacing = Self::calculate_grid_spacing(zoom_level);

        // Draw longitude lines (vertical lines)
        let start_lon = (bounds_geo.min_lon / grid_spacing).floor() * grid_spacing;
        let end_lon = (bounds_geo.max_lon / grid_spacing).ceil() * grid_spacing;

        let mut lon = start_lon;
        while lon <= end_lon {
            if let Some((valid_lat, valid_lon)) = validate_coords(center_lat, lon) {
                let screen_point = viewport.geo_to_screen(valid_lat, valid_lon);

                if is_point_valid(screen_point) {
                    let x = screen_point.x.0;

                    // Only draw if the line is within screen bounds
                    if x >= -10.0 && x <= bounds.size.width.0 + 10.0 {
                        let mut builder = PathBuilder::stroke(px(1.0));
                        builder.move_to(point(px(x), px(0.0)));
                        builder.line_to(point(px(x), bounds.size.height));
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, self.grid_color);
                        }
                    }
                }
            }
            lon += grid_spacing;
        }

        // Draw latitude lines (horizontal lines)
        let start_lat = (bounds_geo.min_lat / grid_spacing).floor() * grid_spacing;
        let end_lat = (bounds_geo.max_lat / grid_spacing).ceil() * grid_spacing;

        let mut lat = start_lat;
        while lat <= end_lat {
            if let Some((valid_lat, valid_lon)) = validate_coords(lat, center_lon) {
                let screen_point = viewport.geo_to_screen(valid_lat, valid_lon);

                if is_point_valid(screen_point) {
                    let y = screen_point.y.0;

                    // Only draw if the line is within screen bounds
                    if y >= -10.0 && y <= bounds.size.height.0 + 10.0 {
                        let mut builder = PathBuilder::stroke(px(1.0));
                        builder.move_to(point(px(0.0), px(y)));
                        builder.line_to(point(bounds.size.width, px(y)));
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, self.grid_color);
                        }
                    }
                }
            }
            lat += grid_spacing;
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![
            ("Grid Spacing".to_string(), "Dynamic".to_string()),
            ("Color".to_string(), format!("#{:06x}",
                (self.grid_color.r * 255.0) as u32 * 65536 +
                (self.grid_color.g * 255.0) as u32 * 256 +
                (self.grid_color.b * 255.0) as u32)),
        ]
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
