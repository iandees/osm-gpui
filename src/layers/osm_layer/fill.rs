use super::*;
use crate::coordinates::Mercator;

impl OsmLayer {
    /// The CPU half of the multipolygon fill pass of `render_canvas` (cull,
    /// project, emit triangles — everything but handing the path to the GPU),
    /// run through the same code, and the number of path vertices it
    /// produced. For `examples/perf_bench.rs`, which has no window.
    pub fn multipolygon_fill_cpu(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> usize {
        let view = viewport.mercator_view_bounds();
        let screen_rect = (
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.origin.x + bounds.size.width),
            f32::from(bounds.origin.y + bounds.size.height),
        );
        let mut path: Option<Path<Pixels>> = None;
        let mut min_max = (0.0, 0.0, 0.0, 0.0);
        let mut pts = Vec::new();
        for entry in self.derived.mp.entries.values() {
            if entry.style.fill.is_none() || !entry.bbox.intersects(view) {
                continue;
            }
            for polygon in &entry.polygons {
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            }
        }
        path.map_or(0, |p| p.vertices.len())
    }

    /// Paint a multipolygon's fill (holes left out) as a translucent wash in
    /// `color`, for the selection and hover highlights. Triangulates on the
    /// fly only when the relation's own style has no fill (the cached
    /// triangles exist only for filled styles); that's a per-selection cost,
    /// not a per-edit one.
    pub(super) fn render_area_tint(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        entry: &MpEntry,
        color: u32,
        kind: HighlightKind,
    ) {
        if !entry.bbox.intersects(viewport.mercator_view_bounds()) {
            return;
        }
        let alpha = match kind {
            HighlightKind::Selected => 0.30,
            _ => 0.20,
        };
        let screen_rect = (
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.origin.x + bounds.size.width),
            f32::from(bounds.origin.y + bounds.size.height),
        );
        let mut path: Option<Path<Pixels>> = None;
        let mut min_max = EMPTY_BOUNDS;
        let mut pts = Vec::new();
        for polygon in &entry.polygons {
            if polygon.tris.is_empty() {
                // Unfilled style: triangulate this polygon just for the wash.
                let rings: Vec<Vec<(f64, f64)>> = (0..polygon.ring_count())
                    .map(|k| polygon.ring(k).iter().map(|&(_, x, y)| (x, y)).collect())
                    .collect();
                let refs: Vec<&[(f64, f64)]> = rings.iter().map(|r| r.as_slice()).collect();
                let Some(tris) = crate::multipolygon::triangulate(&refs) else {
                    continue;
                };
                let filled = MpPolygon {
                    tris,
                    ..polygon.clone()
                };
                self.push_polygon_fill(
                    &filled,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            } else {
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (bounds.origin.x, bounds.origin.y),
                    screen_rect,
                    &mut pts,
                    &mut path,
                    &mut min_max,
                );
            }
        }
        if let Some(mut path) = path {
            set_path_bounds(&mut path, min_max);
            let a = (alpha * 255.0) as u32;
            window.paint_path(path, rgba((color << 8) | a));
        }
    }

    /// Project `polygon` to the screen and append its fill triangles to
    /// `path`. `screen_rect` is `(left, top, right, bottom)` of the canvas in
    /// window pixels; triangles wholly outside it are skipped, so a huge
    /// multipolygon zoomed in on costs only what is visible. A polygon with
    /// an unprojectable vertex is skipped whole (the triangle indices would
    /// no longer line up).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_polygon_fill(
        &self,
        polygon: &MpPolygon,
        viewport: &Viewport,
        origin: (Pixels, Pixels),
        screen_rect: (f32, f32, f32, f32),
        pts: &mut Vec<Point<Pixels>>,
        path: &mut Option<Path<Pixels>>,
        bounds_min_max: &mut (f32, f32, f32, f32),
    ) {
        pts.clear();
        for &(node_id, mx, my) in &polygon.verts {
            let mut sp = viewport.mercator_to_screen(Mercator::new(mx, my));
            if !is_point_valid(sp) {
                return;
            }
            sp += self.drag_preview_offset(node_id);
            pts.push(point(sp.x + origin.0, sp.y + origin.1));
        }
        let (left, top, right, bottom) = screen_rect;
        for t in polygon.tris.as_chunks::<3>().0 {
            let tri = [pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]];
            let xs = tri.map(|p| f32::from(p.x));
            let ys = tri.map(|p| f32::from(p.y));
            if xs.iter().all(|&x| x < left)
                || xs.iter().all(|&x| x > right)
                || ys.iter().all(|&y| y < top)
                || ys.iter().all(|&y| y > bottom)
            {
                continue;
            }
            push_triangle(path, bounds_min_max, tri[0], tri[1], tri[2]);
        }
    }
}
