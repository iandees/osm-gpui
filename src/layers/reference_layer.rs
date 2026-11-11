//! A read-only layer drawing the lines and markers of a GPX or GeoJSON file.
//!
//! The layer has no editing surface (`as_editable` stays `None`) and is never
//! modified, so everything that is keyed on those (selection, search, the
//! validator, upload, autosave, the unsaved-edits prompts) passes it by.
//!
//! A frame culls the file's line chunks against the view, projects the
//! survivors (dropping vertices closer than a pixel to the last one kept) and
//! batches every stroke into a handful of paths with the same stroke builder
//! the way pass uses: one path for the casing, one for the line, and one each
//! for the marker halos and fills.

use gpui::*;

use crate::layers::way_stroke::{self, Layer, PathSink, Polyline};
use crate::layers::{LayerId, MapLayer};
use crate::reference::{Format, MercBox, ReferenceData};
use crate::viewport::Viewport;

/// Colours, as `0xRRGGBBAA`. A pale casing under a saturated core reads on the
/// pale Carto tiles and on dark aerial imagery alike.
const GPX_LINE: u32 = 0xe6007eff;
const GEOJSON_LINE: u32 = 0x7c3aedff;
const CASING: u32 = 0xffffffd9;
const MARKER_HALO: u32 = 0xffffffe6;

/// Half-widths of the stroke and its casing, in pixels.
const LINE_HW: f32 = 1.0;
const CASING_HW: f32 = 2.25;
/// Radius of a marker and of the halo around it, in pixels.
const MARKER_RADIUS: f32 = 3.5;
const HALO_RADIUS: f32 = 5.0;
/// Vertices closer than this to the previous one kept are dropped.
const MIN_SEGMENT_PX: f32 = 1.0;
/// At most this many markers are drawn in a frame; with more in view, every
/// n-th is.
const MAX_MARKERS: usize = 4000;
/// How far outside the view (pixels) things are still drawn, so strokes and
/// markers cut by the edge are complete.
const VIEW_MARGIN_PX: f64 = 12.0;
/// How near a click has to be to a reference line or marker to count.
pub const HIT_TOLERANCE_PX: f32 = 6.0;

/// Sides of the polygon a marker is drawn as.
const MARKER_SIDES: usize = 8;

pub struct ReferenceLayer {
    id: LayerId,
    name: String,
    visible: bool,
    data: ReferenceData,
}

impl ReferenceLayer {
    pub fn new(id: LayerId, name: impl Into<String>, data: ReferenceData) -> Self {
        Self {
            id,
            name: name.into(),
            visible: true,
            data,
        }
    }

    pub fn data(&self) -> &ReferenceData {
        &self.data
    }

    pub fn format(&self) -> Format {
        self.data.format
    }

    /// The feature under `screen_pt` (view-relative pixels), if any.
    pub fn hit(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Option<&crate::reference::RefFeature> {
        let n = self.data.nearest(viewport, screen_pt, HIT_TOLERANCE_PX)?;
        self.data.features.get(n.feature as usize)
    }

    /// The nearest vertex, marker or point on a segment within
    /// `tolerance_px` of `screen_pt`, as `(lat, lon)`.
    pub fn snap(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tolerance_px: f32,
    ) -> Option<(f64, f64)> {
        self.data
            .nearest(viewport, screen_pt, tolerance_px)
            .map(|n| n.lat_lon())
    }

    fn line_color(&self) -> u32 {
        match self.data.format {
            Format::Gpx => GPX_LINE,
            Format::GeoJson => GEOJSON_LINE,
        }
    }

    /// Cull, project and batch this frame's strokes and markers. Needs no
    /// `Window`, so it can be timed on its own (`examples/reference_bench.rs`).
    pub fn build_scene(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> RefScene {
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let proj = viewport.projector();
        let (ppm, _) = proj.pixels_per_meter();
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let margin = if ppm > 0.0 { VIEW_MARGIN_PX / ppm } else { 0.0 };
        let view = MercBox {
            min_x: vmin_x,
            max_x: vmax_x,
            min_y: vmin_y,
            max_y: vmax_y,
        }
        .padded(margin);

        let mut casing = Batch::new();
        let mut line = Batch::new();
        let mut halo = Batch::new();
        let mut fill = Batch::new();

        let mut polyline = Polyline::default();
        let mut pts: Vec<way_stroke::Pt> = Vec::new();
        let min_seg2 = MIN_SEGMENT_PX * MIN_SEGMENT_PX;
        for part in self.data.visible_parts(&view) {
            let extent_m =
                (part.bbox.max_x - part.bbox.min_x).max(part.bbox.max_y - part.bbox.min_y);
            // A chunk under a pixel across still draws as one short segment
            // (first to last vertex), so a zoomed-out track stays unbroken.
            pts.clear();
            let last = part.pts.len() - 1;
            for (i, p) in part.pts.iter().enumerate() {
                let (x, y) = proj.to_screen(p[0], p[1]);
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let (x, y) = (x + ox, y + oy);
                if let Some(&(lx, ly)) = pts.last() {
                    let d2 = (x - lx) * (x - lx) + (y - ly) * (y - ly);
                    if d2 < min_seg2 && i != last {
                        continue;
                    }
                }
                pts.push((x, y));
            }
            if pts.len() < 2 {
                continue;
            }
            let shape = if extent_m * ppm >= way_stroke::SHAPE_MIN_EXTENT_PX as f64 {
                way_stroke::Shape::FULL
            } else {
                way_stroke::Shape::PLAIN
            };
            polyline.set(pts.iter().copied());
            polyline.emit_layers(&mut [
                Some(Layer {
                    sink: PathSink {
                        path: &mut casing.path,
                        bounds: &mut casing.bounds,
                    },
                    hw: CASING_HW,
                    shape,
                }),
                Some(Layer {
                    sink: PathSink {
                        path: &mut line.path,
                        bounds: &mut line.bounds,
                    },
                    hw: LINE_HW,
                    shape,
                }),
            ]);
        }

        // Markers: thin out when there are very many in view.
        let in_view = self.data.visible_markers(&view).count();
        let stride = in_view.div_ceil(MAX_MARKERS).max(1);
        for m in self.data.visible_markers(&view).step_by(stride) {
            let (x, y) = proj.to_screen(m.at[0], m.at[1]);
            if !(x.is_finite() && y.is_finite()) {
                continue;
            }
            let c = (x + ox, y + oy);
            polygon(
                &mut PathSink {
                    path: &mut halo.path,
                    bounds: &mut halo.bounds,
                },
                c,
                HALO_RADIUS,
            );
            polygon(
                &mut PathSink {
                    path: &mut fill.path,
                    bounds: &mut fill.bounds,
                },
                c,
                MARKER_RADIUS,
            );
        }

        let color = self.line_color();
        let mut paths = Vec::new();
        for (rgba, batch) in [
            (CASING, casing),
            (color, line),
            (MARKER_HALO, halo),
            (color, fill),
        ] {
            if let Some(path) = batch.finish() {
                paths.push((rgba, path));
            }
        }
        RefScene { paths }
    }
}

/// A filled regular polygon around `c`.
fn polygon(sink: &mut PathSink<'_>, c: way_stroke::Pt, radius: f32) {
    use way_stroke::TriSink;
    let step = std::f32::consts::TAU / MARKER_SIDES as f32;
    let at = |i: usize| {
        let a = step * i as f32 + step / 2.0;
        (c.0 + radius * a.cos(), c.1 + radius * a.sin())
    };
    for i in 0..MARKER_SIDES {
        sink.tri(c, at(i), at(i + 1));
    }
}

/// Raw triangles painted in one colour, with the bounds accumulated as they
/// are pushed.
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

/// One frame of a reference layer: recorded, not yet painted.
pub struct RefScene {
    paths: Vec<(u32, Path<Pixels>)>,
}

impl RefScene {
    /// Total triangle-list vertices in the scene.
    pub fn vertex_count(&self) -> usize {
        self.paths.iter().map(|(_, p)| p.vertices.len()).sum()
    }

    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    fn paint(self, window: &mut Window) {
        for (rgba, path) in self.paths {
            window.paint_path(path, gpui::rgba(rgba));
        }
    }
}

impl MapLayer for ReferenceLayer {
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

    fn render_elements(&self, _viewport: &Viewport) -> Vec<AnyElement> {
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let fs_phase = crate::frame_stats::start();
        let scene = self.build_scene(viewport, bounds);
        crate::frame_stats::lap("ref.build", fs_phase);
        let fs_phase = crate::frame_stats::start();
        scene.paint(window);
        crate::frame_stats::lap("ref.paint", fs_phase);
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![
            ("Format".to_string(), self.data.format.tag().to_string()),
            ("Features".to_string(), self.data.features.len().to_string()),
            ("Vertices".to_string(), self.data.vertex_count.to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use super::*` pulls in gpui's own `test` attribute macro, which would
    // shadow the real one; see the note in `layers/mod.rs`'s tests.
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::LayerManager;
    use crate::reference::{Builder, FeatureKind};
    use std::collections::HashMap;
    use std::prelude::v1::test;
    use std::sync::Arc;

    fn line_layer(id: u64, format: Format) -> ReferenceLayer {
        let mut b = Builder::new(format);
        let f = b.feature(FeatureKind::Track, "", Some("t".into()), vec![]);
        b.line(f, &[(-74.001, 40.0), (-74.0, 40.0), (-73.999, 40.001)])
            .unwrap();
        b.point(f, -74.0, 40.0005).unwrap();
        ReferenceLayer::new(LayerId(id), "ref", b.finish().unwrap())
    }

    fn view(lat: f64, lon: f64, zoom: f64) -> (Viewport, Bounds<Pixels>) {
        let sz = size(px(800.0), px(600.0));
        (
            Viewport::new(lat, lon, zoom, sz),
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: sz,
            },
        )
    }

    #[test]
    fn a_reference_layer_has_no_editing_surface() {
        let layer = line_layer(1, Format::Gpx);
        assert!(layer.as_editable().is_none());
        assert!(!layer.is_modified());
        assert_eq!(layer.edit_revision(), 0);
        assert!(layer.autosave_snapshot().is_none());
        assert!(layer.export_xml().is_none());
        assert!(layer.diff_for_upload().counts() == (0, 0, 0));
        assert!(layer.download_rects().is_empty());
        assert!(layer.imagery_offset().is_none());
    }

    #[test]
    fn it_is_skipped_by_hit_testing_and_box_selection() {
        let mut m = LayerManager::new();
        m.add_layer(Box::new(line_layer(1, Format::Gpx)));
        let (vp, _) = view(40.0, -74.0, 18.0);
        let hits = m.hit_test_all(&vp, point(px(400.0), px(300.0)));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].is_empty());
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        assert!(m.hit_test_rect_all(&vp, rect).is_empty());
        assert!(m.hit_test_interior_all(&vp, point(px(400.0), px(300.0)))[0].is_empty());
    }

    #[test]
    fn scene_draws_visible_lines_and_markers_and_culls_the_rest() {
        let layer = line_layer(1, Format::GeoJson);
        let (vp, bounds) = view(40.0, -74.0, 17.0);
        let scene = layer.build_scene(&vp, bounds);
        // casing, line, halo, marker fill
        assert_eq!(scene.path_count(), 4);
        assert!(scene.vertex_count() > 0);
        // Far away: nothing.
        let (vp, bounds) = view(-30.0, 100.0, 17.0);
        assert_eq!(layer.build_scene(&vp, bounds).path_count(), 0);
    }

    #[test]
    fn a_long_track_stays_unbroken_zoomed_far_out() {
        // 3000 vertices over ~6 km: every chunk is a few pixels at zoom 12
        // and under one at zoom 8, yet the line still draws.
        let mut b = Builder::new(Format::Gpx);
        let f = b.feature(FeatureKind::Track, "", None, vec![]);
        let pts: Vec<(f64, f64)> = (0..3000).map(|i| (-74.0 + i as f64 * 2e-5, 40.0)).collect();
        b.line(f, &pts).unwrap();
        let layer = ReferenceLayer::new(LayerId(1), "t", b.finish().unwrap());
        for z in [8.0, 12.0] {
            let (vp, bounds) = view(40.0, -73.97, z);
            let scene = layer.build_scene(&vp, bounds);
            assert_eq!(scene.path_count(), 2, "zoom {z}");
        }
    }

    #[test]
    fn hit_and_snap_find_the_line_near_the_cursor() {
        let layer = line_layer(1, Format::Gpx);
        let (vp, _) = view(40.0, -74.0, 18.0);
        // The vertex at (40.0, -74.0) is the view centre.
        let centre = point(px(400.0), px(300.0));
        let f = layer.hit(&vp, centre).unwrap();
        assert_eq!(f.name.as_deref(), Some("t"));
        assert!(layer.hit(&vp, point(px(400.0), px(100.0))).is_none());
        let (lat, lon) = layer.snap(&vp, point(px(402.0), px(303.0)), 6.0).unwrap();
        assert!((lat - 40.0).abs() < 1e-6 && (lon + 74.0).abs() < 1e-6);
        // Between vertices: lands on the segment, on the line y=40.0.
        let (lat, _) = layer.snap(&vp, point(px(350.0), px(303.0)), 6.0).unwrap();
        assert!((lat - 40.0).abs() < 1e-6);
        assert!(layer.snap(&vp, point(px(350.0), px(380.0)), 6.0).is_none());
    }

    fn osm_layer(id: u64) -> OsmLayer {
        let data = Arc::new(crate::osm::OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        });
        OsmLayer::new_with_data(LayerId(id), format!("osm{id}"), data)
    }

    #[test]
    fn reference_layers_go_below_the_data_layers() {
        let mut m = LayerManager::new();
        m.add_layer(Box::new(osm_layer(1)));
        m.add_reference_layer(Box::new(line_layer(2, Format::Gpx)));
        m.add_reference_layer(Box::new(line_layer(3, Format::GeoJson)));
        let ids: Vec<u64> = m.layers().iter().map(|l| l.id().0).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn reference_layers_go_on_top_when_there_is_no_data_layer() {
        let mut m = LayerManager::new();
        m.add_layer(Box::new(crate::layers::grid_layer::GridLayer::new(
            LayerId(1),
        )));
        m.add_reference_layer(Box::new(line_layer(2, Format::Gpx)));
        let ids: Vec<u64> = m.layers().iter().map(|l| l.id().0).collect();
        assert_eq!(ids, vec![1, 2]);
    }
}
