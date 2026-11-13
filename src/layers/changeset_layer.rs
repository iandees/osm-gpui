//! A read-only layer drawing one view of a changeset (what it changed, the
//! map before it, or the map after it); see `changeset_view`.
//!
//! Like `reference_layer` it has no editing surface, so selection, search,
//! validation, upload and autosave all pass it by.

use gpui::*;

use crate::changeset_view::{Shapes, Tint};
use crate::layers::way_stroke::{self, Layer, PathSink, Polyline, TriSink};
use crate::layers::{LayerId, MapLayer};
use crate::viewport::Viewport;

const ADDED: u32 = 0x16a34aff;
const REMOVED: u32 = 0xdc2626ff;
const CHANGED: u32 = 0xd97706ff;
const CASING: u32 = 0xffffffd9;
const LINE_HW: f32 = 1.5;
const CASING_HW: f32 = 3.0;
const MARKER_RADIUS: f32 = 4.5;
const HALO_RADIUS: f32 = 6.0;
const MARKER_SIDES: usize = 8;
/// How far outside the view (pixels) shapes are still drawn.
const VIEW_MARGIN_PX: f32 = 16.0;

/// Which part of the changeset a layer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Changes,
    Before,
    After,
}

impl Part {
    pub fn tag(self) -> &'static str {
        match self {
            Part::Changes => "changes",
            Part::Before => "before",
            Part::After => "after",
        }
    }
}

fn color(t: Tint) -> u32 {
    match t {
        Tint::Added => ADDED,
        Tint::Removed => REMOVED,
        Tint::Changed => CHANGED,
    }
}

pub struct ChangesetLayer {
    id: LayerId,
    name: String,
    visible: bool,
    changeset: i64,
    part: Part,
    shapes: Shapes,
}

impl ChangesetLayer {
    pub fn new(
        id: LayerId,
        name: impl Into<String>,
        changeset: i64,
        part: Part,
        shapes: Shapes,
        visible: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            visible,
            changeset,
            part,
            shapes,
        }
    }

    pub fn changeset(&self) -> i64 {
        self.changeset
    }

    pub fn part(&self) -> Part {
        self.part
    }

    pub fn shapes(&self) -> &Shapes {
        &self.shapes
    }

    fn build_scene(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> Vec<(u32, Path<Pixels>)> {
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let proj = viewport.projector();
        let project = |(lat, lon): (f64, f64)| {
            let (mx, my) = crate::coordinates::lat_lon_to_mercator(lat, lon);
            let (x, y) = proj.to_screen(mx, my);
            (x + ox, y + oy)
        };
        let on_screen = |p: (f32, f32)| {
            p.0 >= ox - VIEW_MARGIN_PX
                && p.0 <= ox + w + VIEW_MARGIN_PX
                && p.1 >= oy - VIEW_MARGIN_PX
                && p.1 <= oy + h + VIEW_MARGIN_PX
        };

        let mut casing = Batch::new();
        let mut lines: [Batch; 3] = [Batch::new(), Batch::new(), Batch::new()];
        let mut halos = Batch::new();
        let mut fills: [Batch; 3] = [Batch::new(), Batch::new(), Batch::new()];
        let slot = |t: Tint| match t {
            Tint::Added => 0,
            Tint::Removed => 1,
            Tint::Changed => 2,
        };

        let mut polyline = Polyline::default();
        for (tint, line) in &self.shapes.lines {
            let pts: Vec<_> = line.iter().map(|&p| project(p)).collect();
            if pts.len() < 2 || pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
                continue;
            }
            // Cull by the line's screen bounding box.
            let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for p in &pts {
                x0 = x0.min(p.0);
                x1 = x1.max(p.0);
                y0 = y0.min(p.1);
                y1 = y1.max(p.1);
            }
            if x1 < ox - VIEW_MARGIN_PX
                || x0 > ox + w + VIEW_MARGIN_PX
                || y1 < oy - VIEW_MARGIN_PX
                || y0 > oy + h + VIEW_MARGIN_PX
            {
                continue;
            }
            polyline.set(pts.iter().copied());
            let (c, l) = (&mut casing, &mut lines[slot(*tint)]);
            polyline.emit_layers(&mut [
                Some(Layer {
                    sink: PathSink {
                        path: &mut c.path,
                        bounds: &mut c.bounds,
                    },
                    hw: CASING_HW,
                    shape: way_stroke::Shape::FULL,
                }),
                Some(Layer {
                    sink: PathSink {
                        path: &mut l.path,
                        bounds: &mut l.bounds,
                    },
                    hw: LINE_HW,
                    shape: way_stroke::Shape::FULL,
                }),
            ]);
        }
        for (tint, p) in &self.shapes.points {
            let c = project(*p);
            if !on_screen(c) {
                continue;
            }
            polygon(
                &mut PathSink {
                    path: &mut halos.path,
                    bounds: &mut halos.bounds,
                },
                c,
                HALO_RADIUS,
            );
            let f = &mut fills[slot(*tint)];
            polygon(
                &mut PathSink {
                    path: &mut f.path,
                    bounds: &mut f.bounds,
                },
                c,
                MARKER_RADIUS,
            );
        }

        let tints = [Tint::Added, Tint::Removed, Tint::Changed];
        let mut paths = Vec::new();
        if let Some(p) = casing.finish() {
            paths.push((CASING, p));
        }
        for (t, b) in tints.iter().zip(lines) {
            if let Some(p) = b.finish() {
                paths.push((color(*t), p));
            }
        }
        if let Some(p) = halos.finish() {
            paths.push((CASING, p));
        }
        for (t, b) in tints.iter().zip(fills) {
            if let Some(p) = b.finish() {
                paths.push((color(*t), p));
            }
        }
        paths
    }
}

fn polygon(sink: &mut PathSink<'_>, c: way_stroke::Pt, radius: f32) {
    let step = std::f32::consts::TAU / MARKER_SIDES as f32;
    let at = |i: usize| {
        let a = step * i as f32 + step / 2.0;
        (c.0 + radius * a.cos(), c.1 + radius * a.sin())
    };
    for i in 0..MARKER_SIDES {
        sink.tri(c, at(i), at(i + 1));
    }
}

struct Batch {
    path: Option<Path<Pixels>>,
    bounds: (f32, f32, f32, f32),
}

impl Batch {
    fn new() -> Self {
        Batch {
            path: None,
            bounds: (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ),
        }
    }

    fn finish(self) -> Option<Path<Pixels>> {
        let mut path = self.path?;
        if path.vertices.is_empty() {
            return None;
        }
        let (min_x, max_x, min_y, max_y) = self.bounds;
        path.bounds = Bounds {
            origin: point(px(min_x), px(min_y)),
            size: size(px(max_x - min_x), px(max_y - min_y)),
        };
        Some(path)
    }
}

impl MapLayer for ChangesetLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn render_elements(
        &self,
        _viewport: &Viewport,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Vec<AnyElement> {
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        for (rgba, path) in self.build_scene(viewport, bounds) {
            window.paint_path(path, gpui::rgba(rgba));
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![
            ("Changeset".to_string(), self.changeset.to_string()),
            ("Showing".to_string(), self.part.tag().to_string()),
            ("Lines".to_string(), self.shapes.lines.len().to_string()),
            ("Points".to_string(), self.shapes.points.len().to_string()),
        ]
    }
}
