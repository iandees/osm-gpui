//! A vector-tile overlay (from a PMTiles archive): the tiles under the view,
//! drawn as filled areas, lines and dots in a colour per source layer.
//!
//! Like the Notes and Street Imagery layers this is not OSM data: not
//! editable, never modified, no upload diff, export or autosave snapshot. It is
//! drawn and nothing else (no hit-testing: a tile has no style or identity to
//! offer).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{AnyElement, Bounds, Path, Pixels, Window};

use crate::layers::paint::{intersects_view, PaintFrame, PathBatch};
use crate::layers::way_stroke::{Polyline, Pt, Shape, TriSink};
use crate::layers::{LayerId, LayerKind, MapLayer};
use crate::tiles::{tiles_covering_geo, TileCoord};
use crate::vector_tile::DecodedTile;
use crate::viewport::Viewport;

/// Half-width of a line in pixels, and the radius of a dot.
const LINE_HW: f32 = 0.9;
const DOT_RADIUS: f32 = 2.5;
/// Vertices closer than this to the last one kept are dropped.
const MIN_SEGMENT_PX: f32 = 1.0;
/// Fills are translucent so the imagery underneath still shows.
const FILL_ALPHA: u32 = 0x55;

/// The decoded tiles on the map, shared between the app (which fills it) and
/// the layer (which draws it).
#[derive(Debug, Clone, Default)]
pub struct VectorTileStore(Arc<Mutex<HashMap<TileCoord, Arc<DecodedTile>>>>);

impl VectorTileStore {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<TileCoord, Arc<DecodedTile>>) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn insert(&self, t: TileCoord, tile: DecodedTile) {
        self.with(|m| m.insert(t, Arc::new(tile)));
    }

    pub fn has(&self, t: TileCoord) -> bool {
        self.with(|m| m.contains_key(&t))
    }

    pub fn get(&self, t: TileCoord) -> Option<Arc<DecodedTile>> {
        self.with(|m| m.get(&t).cloned())
    }

    pub fn len(&self) -> usize {
        self.with(|m| m.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop the tiles for which `keep` is false.
    pub fn retain(&self, keep: impl Fn(&TileCoord) -> bool) {
        self.with(|m| m.retain(|t, _| keep(t)));
    }

    pub fn clear(&self) {
        self.with(|m| m.clear());
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
        let frame = PaintFrame::new(viewport, bounds);
        let proj = viewport.projector();
        let view = viewport.mercator_view_bounds();
        // (stage, colour): fills under lines under dots.
        let mut batches: BTreeMap<(u8, u32), PathBatch> = BTreeMap::new();
        let mut polyline = Polyline::default();
        let mut pts: Vec<Pt> = Vec::new();
        let min_seg2 = MIN_SEGMENT_PX * MIN_SEGMENT_PX;

        for (t, tile) in self.tiles_to_draw(viewport) {
            let tb = t.mercator_bounds();
            if !intersects_view((tb.min_x, tb.max_x, tb.min_y, tb.max_y), view) {
                continue;
            }
            let project = |m: [f64; 2]| frame.to_window(proj.to_screen(m[0], m[1]));
            for fill in &tile.fills {
                let color = (fill.color & 0xffff_ff00) | FILL_ALPHA;
                let mut sink = batches.entry((0, color)).or_default().sink();
                for tri in fill.tris.as_chunks::<3>().0 {
                    let (a, b, c) = (project(tri[0]), project(tri[1]), project(tri[2]));
                    if [a, b, c].iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
                        sink.tri(a, b, c);
                    }
                }
            }
            for line in &tile.lines {
                pts.clear();
                let last = line.pts.len().saturating_sub(1);
                for (i, m) in line.pts.iter().enumerate() {
                    let p = project(*m);
                    if !(p.0.is_finite() && p.1.is_finite()) {
                        continue;
                    }
                    if let Some(&(lx, ly)) = pts.last() {
                        if (p.0 - lx).powi(2) + (p.1 - ly).powi(2) < min_seg2 && i != last {
                            continue;
                        }
                    }
                    pts.push(p);
                }
                if pts.len() < 2 {
                    continue;
                }
                polyline.set(pts.iter().copied());
                let batch = batches.entry((1, line.color)).or_default();
                polyline.emit(&mut batch.sink(), LINE_HW, Shape::FULL);
            }
            for dot in &tile.dots {
                let c = project(dot.at);
                if !(c.0.is_finite() && c.1.is_finite()) {
                    continue;
                }
                let mut sink = batches.entry((2, dot.color)).or_default().sink();
                const SIDES: usize = 8;
                let step = std::f32::consts::TAU / SIDES as f32;
                let at = |i: usize| {
                    let a = step * i as f32;
                    (c.0 + DOT_RADIUS * a.cos(), c.1 + DOT_RADIUS * a.sin())
                };
                for i in 0..SIDES {
                    sink.tri(c, at(i), at(i + 1));
                }
            }
        }
        batches
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

    fn render_elements(
        &self,
        _viewport: &Viewport,
        _window: &mut Window,
        _cx: &mut gpui::App,
    ) -> Vec<AnyElement> {
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        for (rgba, path) in self.build_scene(viewport, bounds) {
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
    use crate::vector_tile::{Dot, Fill, Line};
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
        DecodedTile {
            fills: vec![Fill {
                color: 0x3b82f6ff,
                tris: vec![[cx, cy], [cx + d, cy], [cx, cy + d]],
            }],
            lines: vec![Line {
                color: 0xea580cff,
                pts: vec![[cx - d, cy], [cx + d, cy + d]],
            }],
            dots: vec![Dot {
                color: 0xdb2777ff,
                at: [cx, cy],
            }],
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
}
