//! A vector-tile overlay (from a PMTiles archive): the tiles under the view,
//! drawn as filled areas, lines and dots in a colour per source layer.
//!
//! Like the Notes and Street Imagery layers this is not OSM data: not
//! editable, never modified, no upload diff, export or autosave snapshot. It is
//! drawn and nothing else (no hit-testing: a tile has no style or identity to
//! offer).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{AnyElement, Bounds, Path, Pixels, Point, Window};

use crate::layers::paint::{intersects_view, PaintFrame, PathBatch};
use crate::layers::way_stroke::{self, Polyline, Pt, Shape, TriSink};
use crate::layers::{LayerHit, LayerId, LayerKind, MapLayer};
use crate::tiles::{tiles_covering_geo, TileCoord};
use crate::vector_tile::{DecodedTile, Feature, GeomKind};
use crate::viewport::Viewport;

/// Half-width of a line in pixels, and the radius of a dot.
const LINE_HW: f32 = 0.9;
const DOT_RADIUS: f32 = 2.5;
/// Vertices closer than this to the last one kept are dropped.
const MIN_SEGMENT_PX: f32 = 1.0;
/// Fills are translucent so the imagery underneath still shows.
const FILL_ALPHA: u32 = 0x55;
/// How much of the data a frame draws. The coarse one is for frames while
/// the map is being dragged, which have to be quick; the full one is drawn
/// as soon as it stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detail {
    /// Anything under this many pixels across (a line or an area) is not drawn.
    min_feature_px: f64,
    /// An area under this many pixels across is drawn without its outline.
    outline_min_px: f64,
    /// A filled triangle covering fewer square pixels than this is skipped.
    min_triangle_px2: f32,
    /// Outline vertices of an area closer than this are merged (a line keeps
    /// `MIN_SEGMENT_PX`).
    min_outline_segment_px: f32,
}

impl Detail {
    pub const FULL: Detail = Detail {
        min_feature_px: 0.5,
        outline_min_px: 8.0,
        min_triangle_px2: 0.1,
        min_outline_segment_px: 2.0,
    };
    pub const COARSE: Detail = Detail {
        min_feature_px: 2.0,
        outline_min_px: 30.0,
        min_triangle_px2: 1.0,
        min_outline_segment_px: 4.0,
    };
}
/// How near a click has to be to a dot or line to count.
pub const HIT_TOLERANCE_PX: f32 = 6.0;
const SELECTED_FILL: u32 = 0xf59e0b66;
const SELECTED_LINE: u32 = 0xf59e0bff;
const SELECTED_LINE_HW: f32 = 2.2;
const SELECTED_DOT_RADIUS: f32 = 5.0;

/// What a click on a feature found: where it lives and its attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorFeatureInfo {
    /// The source layer's name.
    pub layer: Arc<str>,
    pub kind: GeomKind,
    pub id: Option<u64>,
    /// Attributes, sorted by key.
    pub props: Vec<(String, String)>,
    /// The tile it was found in and its place in that tile, which is how the
    /// layer finds it again to highlight it.
    pub tile: TileCoord,
    pub index: u32,
}

#[derive(Debug, Default)]
struct StoreInner {
    tiles: HashMap<TileCoord, Arc<DecodedTile>>,
    selected: Option<(TileCoord, u32)>,
}

/// The decoded tiles on the map, shared between the app (which fills it) and
/// the layer (which draws it).
#[derive(Debug, Clone, Default)]
pub struct VectorTileStore(Arc<Mutex<StoreInner>>);

impl VectorTileStore {
    fn with<R>(&self, f: impl FnOnce(&mut StoreInner) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn insert(&self, t: TileCoord, tile: DecodedTile) {
        self.with(|s| s.tiles.insert(t, Arc::new(tile)));
    }

    pub fn has(&self, t: TileCoord) -> bool {
        self.with(|s| s.tiles.contains_key(&t))
    }

    pub fn get(&self, t: TileCoord) -> Option<Arc<DecodedTile>> {
        self.with(|s| s.tiles.get(&t).cloned())
    }

    pub fn len(&self) -> usize {
        self.with(|s| s.tiles.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop the tiles for which `keep` is false (the selected feature's tile
    /// is kept, so its highlight does not vanish).
    pub fn retain(&self, keep: impl Fn(&TileCoord) -> bool) {
        self.with(|s| {
            let selected = s.selected.map(|(t, _)| t);
            s.tiles.retain(|t, _| keep(t) || Some(*t) == selected);
        });
    }

    pub fn clear(&self) {
        self.with(|s| {
            s.tiles.clear();
            s.selected = None;
        });
    }

    /// The feature drawn highlighted: its tile and place in it.
    pub fn set_selected(&self, sel: Option<(TileCoord, u32)>) {
        self.with(|s| s.selected = sel);
    }
}

/// The archive zoom whose tiles are shown at map zoom `zoom`. Vector tiles are
/// laid out for 512-pixel tiles, so they are one zoom lower than the 256-pixel
/// grid the map is measured in; beyond the archive's zooms the nearest one is
/// used (scaled up, or down).
pub fn tile_zoom(zoom: f64, min_zoom: u32, max_zoom: u32) -> u32 {
    let wanted = zoom.round() as i64 - 1;
    wanted.clamp(i64::from(min_zoom), i64::from(max_zoom.max(min_zoom))) as u32
}

/// The tiles at `z` a view needs, with a margin so panning shows data at once.
pub fn needed_tiles(viewport: &Viewport, z: u32) -> Vec<TileCoord> {
    let b = viewport.visible_bounds();
    let (dlat, dlon) = (
        (b.max_lat - b.min_lat) * 0.15,
        (b.max_lon - b.min_lon) * 0.15,
    );
    let padded = crate::coordinates::GeoBounds::new(
        (b.min_lat - dlat).max(-85.0),
        (b.max_lat + dlat).min(85.0),
        b.min_lon - dlon,
        b.max_lon + dlon,
    );
    tiles_covering_geo(&padded, z)
}

pub struct VectorTileLayer {
    id: LayerId,
    name: String,
    visible: bool,
    min_zoom: u32,
    max_zoom: u32,
    store: VectorTileStore,
}

impl VectorTileLayer {
    pub fn new(
        id: LayerId,
        name: String,
        min_zoom: u32,
        max_zoom: u32,
        store: VectorTileStore,
    ) -> Self {
        Self {
            id,
            name,
            visible: true,
            min_zoom,
            max_zoom,
            store,
        }
    }

    /// The tiles to draw this frame: the ones the view needs, each stood in
    /// for by its nearest loaded ancestor while it is still loading, so the
    /// map does not go blank when it is zoomed.
    fn tiles_to_draw(&self, viewport: &Viewport) -> Vec<(TileCoord, Arc<DecodedTile>)> {
        let z = tile_zoom(viewport.zoom_level(), self.min_zoom, self.max_zoom);
        let mut seen: HashSet<TileCoord> = HashSet::new();
        let mut out = Vec::new();
        for t in needed_tiles(viewport, z) {
            let mut candidate = Some(t);
            while let Some(c) = candidate {
                if let Some(tile) = self.store.get(c) {
                    if seen.insert(c) {
                        out.push((c, tile));
                    }
                    break;
                }
                candidate = c.parent();
            }
        }
        // Coarse tiles first, so detail is drawn over what stands in for it.
        out.sort_by_key(|(t, _)| t.z);
        out
    }

    /// Cull, project and batch this frame's geometry. Needs no `Window`.
    pub fn build_scene(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
    ) -> Vec<(u32, Path<Pixels>)> {
        self.build_scene_at(viewport, bounds, Detail::FULL)
    }

    /// `build_scene` at a given level of detail.
    pub fn build_scene_at(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        detail: Detail,
    ) -> Vec<(u32, Path<Pixels>)> {
        let frame = PaintFrame::new(viewport, bounds);
        let proj = viewport.projector();
        let (ppm, _) = proj.pixels_per_meter();
        let view = viewport.mercator_view_bounds();
        let mut scene = Scene::default();
        let project = |m: [f64; 2]| frame.to_window(proj.to_screen(m[0], m[1]));

        for (t, tile) in self.tiles_to_draw(viewport) {
            let tb = t.mercator_bounds();
            if !intersects_view((tb.min_x, tb.max_x, tb.min_y, tb.max_y), view) {
                continue;
            }
            for f in &tile.features {
                // Off the view, or too small to see.
                let [x0, y0, x1, y1] = f.bbox;
                if !intersects_view((x0, x1, y0, y1), view) {
                    continue;
                }
                let extent_px = (x1 - x0).max(y1 - y0) * ppm;
                if f.kind != GeomKind::Point && extent_px < detail.min_feature_px {
                    continue;
                }
                scene.feature(f, extent_px, &project, Look::Normal, detail);
            }
        }
        // The selected feature, over everything.
        if let Some((t, i)) = self.store.with(|s| s.selected) {
            if let Some(f) = self
                .store
                .get(t)
                .and_then(|tile| tile.features.get(i as usize).cloned())
            {
                let [x0, y0, x1, y1] = f.bbox;
                let extent_px = (x1 - x0).max(y1 - y0) * ppm;
                scene.feature(&f, extent_px, &project, Look::Selected, detail);
            }
        }
        scene.finish()
    }

    /// The feature under `point`: the nearest dot or line within reach, else
    /// the area that contains it; later (upper) features win ties.
    fn feature_at(&self, viewport: &Viewport, point: Point<Pixels>) -> Option<VectorFeatureInfo> {
        let m = viewport.screen_to_mercator(point);
        let (ppm, _) = viewport.projector().pixels_per_meter();
        if ppm <= 0.0 {
            return None;
        }
        let tol = HIT_TOLERANCE_PX as f64 / ppm;
        // (rank, distance): dots and lines before the areas under them.
        let mut best: Option<((u8, f64), VectorFeatureInfo)> = None;
        for (t, tile) in self.tiles_to_draw(viewport).into_iter().rev() {
            for (i, f) in tile.features.iter().enumerate().rev() {
                let [x0, y0, x1, y1] = f.bbox;
                if m.x < x0 - tol || m.x > x1 + tol || m.y < y0 - tol || m.y > y1 + tol {
                    continue;
                }
                let Some(score) = score_hit(f, m.x, m.y, tol) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(b, _)| score < *b) {
                    best = Some((
                        score,
                        VectorFeatureInfo {
                            layer: f.layer.clone(),
                            kind: f.kind,
                            id: f.id,
                            props: f.props.clone(),
                            tile: t,
                            index: i as u32,
                        },
                    ));
                }
            }
        }
        best.map(|(_, info)| info)
    }
}

/// How well `f` is hit at `(x, y)` (Mercator metres) with reach `tol`: the
/// rank (0 dot, 1 line, 2 area) and the distance, or `None` for a miss.
fn score_hit(f: &Feature, x: f64, y: f64, tol: f64) -> Option<(u8, f64)> {
    let mut best: Option<(u8, f64)> = None;
    fn note(best: &mut Option<(u8, f64)>, rank: u8, d: f64) {
        if best.is_none_or(|b| (rank, d) < b) {
            *best = Some((rank, d));
        }
    }
    for d in &f.dots {
        let dist = ((d[0] - x).powi(2) + (d[1] - y).powi(2)).sqrt();
        if dist <= tol {
            note(&mut best, 0, dist);
        }
    }
    for line in &f.lines {
        for seg in line.windows(2) {
            let dist = segment_distance(seg[0], seg[1], x, y);
            if dist <= tol {
                note(&mut best, 1, dist);
            }
        }
    }
    if f.kind == GeomKind::Polygon && best.is_none_or(|b| b.0 > 2) {
        let inside = f
            .tris
            .as_chunks::<3>()
            .0
            .iter()
            .any(|t| in_triangle(t, x, y));
        if inside {
            note(&mut best, 2, 0.0);
        }
    }
    best
}

fn segment_distance(a: [f64; 2], b: [f64; 2], x: f64, y: f64) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((x - a[0]) * dx + (y - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    ((a[0] + t * dx - x).powi(2) + (a[1] + t * dy - y).powi(2)).sqrt()
}

fn in_triangle(t: &[[f64; 2]; 3], x: f64, y: f64) -> bool {
    let side = |a: [f64; 2], b: [f64; 2]| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
    let (d1, d2, d3) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Normal,
    Selected,
}

/// One frame's triangles, batched by (stage, colour): fills under lines under
/// dots, the selected feature last.
#[derive(Default)]
struct Scene {
    batches: BTreeMap<(u8, u32), PathBatch>,
    polyline: Polyline,
    pts: Vec<Pt>,
}

impl Scene {
    fn feature(
        &mut self,
        f: &Feature,
        extent_px: f64,
        project: &impl Fn([f64; 2]) -> Pt,
        look: Look,
        detail: Detail,
    ) {
        let sel = look == Look::Selected;
        let (base, fill_color, line_hw, dot_r) = if sel {
            (3, SELECTED_FILL, SELECTED_LINE_HW, SELECTED_DOT_RADIUS)
        } else {
            (0, (f.color & 0xffff_ff00) | FILL_ALPHA, LINE_HW, DOT_RADIUS)
        };
        let line_color = if sel { SELECTED_LINE } else { f.color };
        if !f.tris.is_empty() {
            let mut sink = self.batches.entry((base, fill_color)).or_default().sink();
            for tri in f.tris.as_chunks::<3>().0 {
                let (a, b, c) = (project(tri[0]), project(tri[1]), project(tri[2]));
                let area2 = (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1);
                // (A NaN area fails this too.)
                if area2.abs() >= 2.0 * detail.min_triangle_px2 {
                    sink.tri(a, b, c);
                }
            }
        }
        // A small area is its fill alone; the outline would only blur it.
        let outlines = f.kind != GeomKind::Polygon || sel || extent_px >= detail.outline_min_px;
        if outlines {
            // A line this thin shows no gaps without joins and caps, which
            // are most of a stroke's triangles; the highlight is thicker.
            let shape = if sel && extent_px >= way_stroke::SHAPE_MIN_EXTENT_PX as f64 {
                Shape::FULL
            } else {
                Shape::PLAIN
            };
            let min_seg = if f.kind == GeomKind::Polygon && !sel {
                detail.min_outline_segment_px
            } else {
                MIN_SEGMENT_PX
            };
            let min_seg2 = min_seg * min_seg;
            for line in &f.lines {
                self.pts.clear();
                let last = line.len().saturating_sub(1);
                for (i, m) in line.iter().enumerate() {
                    let p = project(*m);
                    if !(p.0.is_finite() && p.1.is_finite()) {
                        continue;
                    }
                    if let Some(&(lx, ly)) = self.pts.last() {
                        if (p.0 - lx).powi(2) + (p.1 - ly).powi(2) < min_seg2 && i != last {
                            continue;
                        }
                    }
                    self.pts.push(p);
                }
                if self.pts.len() < 2 {
                    continue;
                }
                self.polyline.set(self.pts.iter().copied());
                let batch = self.batches.entry((base + 1, line_color)).or_default();
                self.polyline.emit(&mut batch.sink(), line_hw, shape);
            }
        }
        for &d in &f.dots {
            let c = project(d);
            if !(c.0.is_finite() && c.1.is_finite()) {
                continue;
            }
            let mut sink = self
                .batches
                .entry((base + 2, line_color))
                .or_default()
                .sink();
            const SIDES: usize = 8;
            let step = std::f32::consts::TAU / SIDES as f32;
            let at = |i: usize| {
                let a = step * i as f32;
                (c.0 + dot_r * a.cos(), c.1 + dot_r * a.sin())
            };
            for i in 0..SIDES {
                sink.tri(c, at(i), at(i + 1));
            }
        }
    }

    fn finish(self) -> Vec<(u32, Path<Pixels>)> {
        self.batches
            .into_iter()
            .filter_map(|((_, color), batch)| batch.finish().map(|p| (color, p)))
            .collect()
    }
}

impl MapLayer for VectorTileLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn kind(&self) -> LayerKind {
        LayerKind::VectorTile
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn hit_test_point(&self, viewport: &Viewport, point: Point<Pixels>) -> Option<LayerHit> {
        self.feature_at(viewport, point)
            .map(|info| LayerHit::VectorFeature(Arc::new(info)))
    }

    fn render_elements(
        &self,
        _viewport: &Viewport,
        _window: &mut Window,
        _cx: &mut gpui::App,
    ) -> Vec<AnyElement> {
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let detail = if viewport.is_dragging {
            Detail::COARSE
        } else {
            Detail::FULL
        };
        for (rgba, path) in self.build_scene_at(viewport, bounds, detail) {
            window.paint_path(path, gpui::rgba(rgba));
        }
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![("tiles".to_string(), self.store.len().to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::LatLon;
    use gpui::{point, px, size};

    fn view(lat: f64, lon: f64, zoom: f64) -> (Viewport, Bounds<Pixels>) {
        let sz = size(px(800.0), px(600.0));
        (
            Viewport::new(LatLon::new(lat, lon), zoom, sz),
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: sz,
            },
        )
    }

    /// A tile with a triangle, a line and a dot all around the tile's middle.
    fn sample_tile(t: TileCoord) -> DecodedTile {
        let b = t.mercator_bounds();
        let (cx, cy) = ((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0);
        let d = (b.max_x - b.min_x) / 20.0;
        let feature = |layer: &str, kind, color, bbox, f: &dyn Fn(&mut Feature)| {
            let mut x = Feature {
                layer: Arc::from(layer),
                color,
                kind,
                id: None,
                props: vec![("name".to_string(), layer.to_string())],
                bbox,
                tris: vec![],
                lines: vec![],
                dots: vec![],
            };
            f(&mut x);
            x
        };
        DecodedTile {
            features: vec![
                feature(
                    "water",
                    GeomKind::Polygon,
                    0x3b82f6ff,
                    [cx, cy, cx + d, cy + d],
                    &|x| x.tris = vec![[cx, cy], [cx + d, cy], [cx, cy + d]],
                ),
                feature(
                    "roads",
                    GeomKind::Line,
                    0xea580cff,
                    [cx - d, cy, cx + d, cy + d],
                    &|x| x.lines = vec![vec![[cx - d, cy], [cx + d, cy + d]]],
                ),
                feature("poi", GeomKind::Point, 0xdb2777ff, [cx, cy, cx, cy], &|x| {
                    x.dots = vec![[cx, cy]]
                }),
            ],
        }
    }

    fn layer(store: VectorTileStore) -> VectorTileLayer {
        VectorTileLayer::new(LayerId(5), "Tiles".into(), 0, 14, store)
    }

    #[test]
    fn the_layer_is_not_osm_data() {
        let l = layer(VectorTileStore::default());
        assert!(l.as_editable().is_none());
        assert!(l.as_osm().is_none());
        assert_eq!(l.kind(), LayerKind::VectorTile);
    }

    #[test]
    fn tiles_are_one_zoom_below_the_map_zoom_and_stay_inside_the_archive() {
        assert_eq!(tile_zoom(13.4, 0, 14), 12);
        assert_eq!(tile_zoom(20.0, 0, 14), 14);
        assert_eq!(tile_zoom(1.0, 3, 14), 3);
        // A degenerate archive range never panics.
        assert_eq!(tile_zoom(10.0, 5, 2), 5);
    }

    #[test]
    fn a_loaded_tile_draws_a_path_per_colour_and_stage() {
        let (vp, bounds) = view(40.0, -74.0, 14.0);
        let z = tile_zoom(14.0, 0, 14);
        let store = VectorTileStore::default();
        for t in needed_tiles(&vp, z) {
            store.insert(t, sample_tile(t));
        }
        let scene = layer(store).build_scene(&vp, bounds);
        // fill, line and dot colours
        assert_eq!(scene.len(), 3);
        assert!(scene.iter().all(|(_, p)| !p.vertices.is_empty()));
    }

    #[test]
    fn nothing_loaded_or_nothing_in_view_draws_nothing() {
        let (vp, bounds) = view(40.0, -74.0, 14.0);
        assert!(layer(VectorTileStore::default())
            .build_scene(&vp, bounds)
            .is_empty());
        // Tiles that are not under the view are skipped.
        let store = VectorTileStore::default();
        let far = TileCoord::new(0, 0, tile_zoom(14.0, 0, 14));
        store.insert(far, sample_tile(far));
        assert!(layer(store).build_scene(&vp, bounds).is_empty());
    }

    #[test]
    fn a_missing_tile_is_stood_in_for_by_a_loaded_ancestor() {
        let (vp, bounds) = view(40.0, -74.0, 14.0);
        let z = tile_zoom(14.0, 0, 14);
        let store = VectorTileStore::default();
        // Only a coarse tile around the view is loaded.
        let any = needed_tiles(&vp, z)[0];
        let ancestor = TileCoord::new(any.x / 4, any.y / 4, z - 2);
        store.insert(ancestor, sample_tile(ancestor));
        let l = layer(store);
        let drawn = l.tiles_to_draw(&vp);
        assert!(drawn.iter().all(|(t, _)| t.z == z - 2));
        assert_eq!(drawn.len(), 1, "an ancestor is drawn once, not per child");
        let _ = bounds;
    }

    /// A view centred on a tile's middle, where `sample_tile` has its features.
    fn centred_on(t: TileCoord, zoom: f64) -> (Viewport, Bounds<Pixels>) {
        let b = t.mercator_bounds();
        let at = crate::coordinates::mercator_to_lat_lon(crate::coordinates::Mercator::new(
            (b.min_x + b.max_x) / 2.0,
            (b.min_y + b.max_y) / 2.0,
        ));
        view(at.lat, at.lon, zoom)
    }

    fn loaded_at(zoom: f64) -> (VectorTileLayer, Viewport, Bounds<Pixels>, TileCoord) {
        let z = tile_zoom(zoom, 0, 14);
        let t = TileCoord::new(1 << (z - 1), 1 << (z - 1), z);
        let store = VectorTileStore::default();
        store.insert(t, sample_tile(t));
        let (vp, bounds) = centred_on(t, zoom);
        (layer(store), vp, bounds, t)
    }

    #[test]
    fn a_click_picks_a_dot_over_the_line_and_area_under_it() {
        let (l, vp, bounds, t) = loaded_at(14.0);
        let centre = point(bounds.size.width / 2.0, bounds.size.height / 2.0);
        let Some(LayerHit::VectorFeature(info)) = l.hit_test_point(&vp, centre) else {
            panic!("on the dot");
        };
        assert_eq!(&*info.layer, "poi");
        assert_eq!(info.kind, GeomKind::Point);
        assert_eq!((info.tile, info.index), (t, 2));
        assert_eq!(info.props, [("name".to_string(), "poi".to_string())]);
    }

    #[test]
    fn a_click_inside_an_area_away_from_its_edges_picks_the_area() {
        let (l, vp, bounds, _) = loaded_at(14.0);
        let (ppm, _) = vp.projector().pixels_per_meter();
        // Just inside the triangle's right angle corner, 12 px from the dot
        // and clear of the line.
        let at = point(
            bounds.size.width / 2.0 + px(12.0),
            bounds.size.height / 2.0 - px(3.0),
        );
        let _ = ppm;
        let hit = l.hit_test_point(&vp, at);
        let Some(LayerHit::VectorFeature(info)) = hit else {
            panic!("inside the area");
        };
        assert_eq!(&*info.layer, "water");
    }

    #[test]
    fn a_click_on_nothing_hits_nothing() {
        let (l, vp, bounds, _) = loaded_at(14.0);
        let far = point(px(20.0), bounds.size.height - px(20.0));
        assert!(l.hit_test_point(&vp, far).is_none());
        assert!(layer(VectorTileStore::default())
            .hit_test_point(&vp, point(px(400.0), px(300.0)))
            .is_none());
    }

    #[test]
    fn the_selected_feature_is_drawn_again_in_the_highlight() {
        let (l, vp, bounds, t) = loaded_at(14.0);
        let plain = l.build_scene(&vp, bounds);
        l.store.set_selected(Some((t, 1)));
        let with = l.build_scene(&vp, bounds);
        assert!(with.len() > plain.len());
        assert!(with.iter().any(|(c, _)| *c == SELECTED_LINE));
        l.store.set_selected(None);
        assert_eq!(l.build_scene(&vp, bounds).len(), plain.len());
    }

    #[test]
    fn features_too_small_to_see_are_skipped_but_dots_stay() {
        let (l, _, _, t) = loaded_at(14.0);
        // Zoomed far out, the line and area are under a pixel: only the dot.
        let (vp, bounds) = centred_on(t, 9.0);
        let z = tile_zoom(9.0, 0, 14);
        let t9 = TileCoord::new(1 << (z - 1), 1 << (z - 1), z);
        let store = VectorTileStore::default();
        store.insert(t9, sample_tile(t9));
        let scene = layer(store).build_scene(&vp, bounds);
        let colors: Vec<u32> = scene.iter().map(|(c, _)| *c).collect();
        assert!(colors.contains(&0xdb2777ff));
        let _ = l;
    }
}
