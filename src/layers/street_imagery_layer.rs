//! The Street Imagery layer: photo locations from one provider, drawn as dots
//! with a small wedge showing which way the camera faced.
//!
//! Like the Notes layer this is not OSM data: not editable, never modified, no
//! upload diff, export or autosave snapshot. Its dots are found by
//! `MapLayer::hit_test_point` here and by nothing else.

use gpui::{point, px, rgba, AnyElement, Bounds, PathBuilder, Pixels, Point, Window};

use crate::layers::{LayerHit, LayerId, MapLayer};
use crate::street_imagery::{PhotoStore, Provider, StreetPhoto};
use crate::viewport::Viewport;

/// One colour per provider, so layers read apart on the map.
const MAPILLARY_RGBA: u32 = 0x05cb63ff;
const FOTOSHI_RGBA: u32 = 0x2563ebff;
const PANORAMAX_RGBA: u32 = 0x9333eaff;
const HALO_RGBA: u32 = 0xfffffff0;
const SELECTED_RGBA: u32 = 0xf59e0bff;

const DOT_RADIUS: f32 = 4.5;
const WEDGE_LENGTH: f32 = 15.0;
/// Half the wedge's opening angle, in degrees.
const WEDGE_HALF_ANGLE: f32 = 22.0;
const HIT_RADIUS: f32 = 9.0;
/// Past this many loaded photos the layer draws plain dots, no wedges.
const DENSE_PHOTOS: usize = 1500;
const DENSE_DOT_RADIUS: f32 = 3.0;

fn color_of(provider: Provider) -> u32 {
    match provider {
        Provider::Mapillary => MAPILLARY_RGBA,
        Provider::Fotoshi => FOTOSHI_RGBA,
        Provider::Panoramax => PANORAMAX_RGBA,
    }
}

pub struct StreetImageryLayer {
    id: LayerId,
    name: String,
    visible: bool,
    provider: Provider,
    store: PhotoStore,
}

impl StreetImageryLayer {
    pub fn new(id: LayerId, name: String, provider: Provider, store: PhotoStore) -> Self {
        Self {
            id,
            name,
            visible: true,
            provider,
            store,
        }
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }
}

fn circle_outline(x: f32, y: f32, r: f32) -> Vec<(f32, f32)> {
    (0..16)
        .map(|i| {
            let a = (i as f32) * std::f32::consts::TAU / 16.0;
            (x + r * a.cos(), y + r * a.sin())
        })
        .collect()
}

/// The wedge from the dot out towards `heading` (degrees clockwise from
/// north; screen y grows downwards).
fn wedge_outline(x: f32, y: f32, heading: f32, scale: f32) -> Vec<(f32, f32)> {
    let len = WEDGE_LENGTH * scale;
    let mut pts = vec![(x, y)];
    for step in -2..=2 {
        let deg = heading + (step as f32) * WEDGE_HALF_ANGLE / 2.0;
        let a = deg.to_radians();
        pts.push((x + len * a.sin(), y - len * a.cos()));
    }
    pts
}

fn paint_polygon(window: &mut Window, pts: &[(f32, f32)], color: u32, stroke: Option<f32>) {
    let mut builder = match stroke {
        Some(w) => PathBuilder::stroke(px(w)),
        None => PathBuilder::fill(),
    };
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 {
            builder.move_to(point(px(*x), px(*y)));
        } else {
            builder.line_to(point(px(*x), px(*y)));
        }
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, rgba(color));
    }
}

/// The photo whose dot is under `pos` (map-local pixels), nearest first, with
/// its distance from `pos` in pixels.
fn hit_test(
    store: &PhotoStore,
    viewport: &Viewport,
    pos: Point<Pixels>,
) -> Option<(f32, StreetPhoto)> {
    let (mx, my) = (pos.x.as_f32(), pos.y.as_f32());
    let mut best: Option<(f32, StreetPhoto)> = None;
    store.with(|s| {
        for p in &s.photos {
            let sp = viewport.geo_to_screen(p.lat, p.lon);
            let (dx, dy) = (mx - sp.x.as_f32(), my - sp.y.as_f32());
            let d = (dx * dx + dy * dy).sqrt();
            if d <= HIT_RADIUS && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, p.clone()));
            }
        }
    });
    best
}

impl MapLayer for StreetImageryLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn hit_test_point(&self, viewport: &Viewport, point: Point<Pixels>) -> Option<LayerHit> {
        hit_test(&self.store, viewport, point)
            .map(|(distance_px, photo)| LayerHit::Photo { photo, distance_px })
    }

    fn render_elements(
        &self,
        _viewport: &Viewport,
        _window: &mut gpui::Window,
        _cx: &mut gpui::App,
    ) -> Vec<AnyElement> {
        vec![]
    }

    fn render_canvas(&self, viewport: &Viewport, bounds: Bounds<Pixels>, window: &mut Window) {
        let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        let (ox, oy) = (bounds.origin.x.as_f32(), bounds.origin.y.as_f32());
        let color = color_of(self.provider);
        self.store.with(|s| {
            let dense = s.photos.len() > DENSE_PHOTOS;
            for p in &s.photos {
                let sp = viewport.geo_to_screen(p.lat, p.lon);
                let (x, y) = (sp.x.as_f32(), sp.y.as_f32());
                if !(x.is_finite() && y.is_finite())
                    || x < -20.0
                    || y < -20.0
                    || x > w + 20.0
                    || y > h + 20.0
                {
                    continue;
                }
                let (x, y) = (x + ox, y + oy);
                let selected = s
                    .selected
                    .as_ref()
                    .is_some_and(|(pr, id)| *pr == p.provider && *id == p.id);
                let scale = if selected { 1.3 } else { 1.0 };
                let fill = if selected { SELECTED_RGBA } else { color };
                // With thousands of photos on the map, plain small dots keep
                // the frame fast; the photo that is open is always drawn in full.
                if dense && !selected {
                    paint_polygon(window, &circle_outline(x, y, DENSE_DOT_RADIUS), fill, None);
                    continue;
                }
                if let Some(heading) = p.heading {
                    let wedge = wedge_outline(x, y, heading, scale);
                    paint_polygon(window, &wedge, HALO_RGBA, Some(2.0));
                    paint_polygon(window, &wedge, fill, None);
                }
                let dot = circle_outline(x, y, DOT_RADIUS * scale);
                paint_polygon(window, &dot, HALO_RGBA, Some(2.0));
                paint_polygon(window, &dot, fill, None);
            }
        });
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![("photos".to_string(), self.store.len().to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn photo(id: &str, lat: f64, lon: f64) -> StreetPhoto {
        StreetPhoto {
            provider: Provider::Fotoshi,
            id: id.into(),
            lat,
            lon,
            heading: Some(90.0),
            captured: None,
            creator: None,
            license: None,
            thumb_url: None,
            large_url: None,
        }
    }

    fn viewport() -> Viewport {
        Viewport::new(10.0, 20.0, 15.0, size(px(800.0), px(600.0)))
    }

    #[test]
    fn the_layer_is_not_osm_data() {
        let layer = StreetImageryLayer::new(
            LayerId(3),
            "Fotoshi".into(),
            Provider::Fotoshi,
            PhotoStore::default(),
        );
        assert!(layer.as_editable().is_none());
        assert!(layer.as_osm().is_none());
        assert_eq!(layer.id(), LayerId(3));
    }

    #[test]
    fn hit_test_picks_the_nearest_dot_within_reach() {
        let vp = viewport();
        let store = PhotoStore::default();
        store.insert_tile(
            crate::tiles::TileCoord { z: 15, x: 0, y: 0 },
            vec![photo("a", 10.0, 20.0), photo("b", 10.0, 20.00005)],
        );
        let at = vp.geo_to_screen(10.0, 20.0);
        let layer = layer_over(store);
        let Some(LayerHit::Photo { photo, .. }) = layer.hit_test_point(&vp, at) else {
            panic!("on a dot");
        };
        assert_eq!(photo.id, "a");
        let far = point(at.x + px(200.0), at.y);
        assert!(layer.hit_test_point(&vp, far).is_none());
    }

    fn layer_over(store: PhotoStore) -> StreetImageryLayer {
        StreetImageryLayer::new(LayerId(3), "Fotoshi".into(), Provider::Fotoshi, store)
    }

    #[test]
    fn the_wedge_points_along_the_heading() {
        // Heading 90 is east: the tip's x is greater than the origin's.
        let east = wedge_outline(100.0, 100.0, 90.0, 1.0);
        assert!(east[3].0 > 110.0 && (east[3].1 - 100.0).abs() < 0.1);
        // Heading 0 is north: up the screen.
        let north = wedge_outline(100.0, 100.0, 0.0, 1.0);
        assert!(north[3].1 < 90.0 && (north[3].0 - 100.0).abs() < 0.1);
    }
}
