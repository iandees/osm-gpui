//! An extension's markers: the pins an extension set with `set-markers`,
//! drawn above the data. Like the Notes layer it holds no OSM data: it is
//! not editable, never modified, and is not saved; a click on a pin is a
//! [`LayerHit::ExtensionMarker`] that the window hands back to the
//! extension.

use std::sync::{Arc, Mutex};

use gpui::{AnyElement, Bounds, Pixels, Point, Window};

use crate::extensions::runtime::Marker;
use crate::layers::paint::{circle_outline, paint_polygon, PaintFrame};
use crate::layers::{LayerHit, LayerId, LayerKind, MapLayer};
use crate::viewport::Viewport;

const RADIUS: f32 = 7.0;
const SELECTED_RADIUS: f32 = 9.5;
const HIT_SLOP: f32 = 3.0;
const HALO_RGBA: u32 = 0xfffffff2;
const SELECTED_HALO_RGBA: u32 = 0x1d4ed8ff;

/// The markers, shared by the layer (which draws them) and the window (which
/// replaces them when the extension asks).
#[derive(Clone, Default)]
pub struct MarkerStore(Arc<Mutex<Vec<Marker>>>);

impl MarkerStore {
    pub fn replace(&self, markers: Vec<Marker>) {
        *self.0.lock().unwrap() = markers;
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn with<T>(&self, f: impl FnOnce(&[Marker]) -> T) -> T {
        f(&self.0.lock().unwrap())
    }
}

pub struct ExtensionLayer {
    id: LayerId,
    name: String,
    /// The extension the markers belong to.
    extension: String,
    visible: bool,
    store: MarkerStore,
}

impl ExtensionLayer {
    pub fn new(id: LayerId, name: String, extension: String, store: MarkerStore) -> Self {
        Self {
            id,
            name,
            extension,
            visible: true,
            store,
        }
    }

    pub fn extension(&self) -> &str {
        &self.extension
    }
}

/// Selected markers last, so they are drawn (and hit) on top.
fn draw_order(markers: &[Marker]) -> Vec<&Marker> {
    let mut v: Vec<&Marker> = markers.iter().collect();
    v.sort_by_key(|m| m.selected);
    v
}

fn radius(m: &Marker) -> f32 {
    if m.selected {
        SELECTED_RADIUS
    } else {
        RADIUS
    }
}

impl MapLayer for ExtensionLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn kind(&self) -> LayerKind {
        LayerKind::Extension
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn hit_test_point(&self, viewport: &Viewport, point: Point<Pixels>) -> Option<LayerHit> {
        let (mx, my) = (point.x.as_f32(), point.y.as_f32());
        self.store.with(|markers| {
            draw_order(markers).into_iter().rev().find_map(|m| {
                let p = viewport.geo_to_screen(crate::coordinates::LatLon::new(m.at.lat, m.at.lon));
                let (dx, dy) = (mx - p.x.as_f32(), my - p.y.as_f32());
                ((dx * dx + dy * dy).sqrt() <= radius(m) + HIT_SLOP).then(|| {
                    LayerHit::ExtensionMarker {
                        extension: self.extension.clone(),
                        marker: m.id.clone(),
                    }
                })
            })
        })
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
        let frame = PaintFrame::new(viewport, bounds);
        let (w, h) = frame.view_size();
        self.store.with(|markers| {
            for m in draw_order(markers) {
                let (x, y) = frame.local_geo(m.at.lat, m.at.lon);
                if !(x.is_finite() && y.is_finite())
                    || x < -20.0
                    || y < -20.0
                    || x > w + 20.0
                    || y > h + 20.0
                {
                    continue;
                }
                let (x, y) = frame.to_window((x, y));
                let r = radius(m);
                let (halo, halo_w) = if m.selected {
                    (SELECTED_HALO_RGBA, 4.0)
                } else {
                    (HALO_RGBA, 2.5)
                };
                let outline = circle_outline(x, y, r);
                paint_polygon(window, &outline, halo, Some(halo_w));
                paint_polygon(window, &outline, (m.color << 8) | 0xff, None);
            }
        });
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![("markers".to_string(), self.store.len().to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::LatLon as GeoLatLon;
    use crate::extensions::runtime::LatLon;
    use gpui::{px, size};

    fn marker(id: &str, lat: f64, lon: f64, selected: bool) -> Marker {
        Marker {
            id: id.into(),
            at: LatLon { lat, lon },
            color: 0x7c3aed,
            selected,
        }
    }

    #[test]
    fn a_click_on_a_marker_names_it_and_its_extension() {
        let store = MarkerStore::default();
        store.replace(vec![
            marker("a", 45.0, -93.0, false),
            marker("b", 45.0001, -93.0, true),
        ]);
        let layer = ExtensionLayer::new(LayerId(3), "MR".into(), "maproulette".into(), store);
        let vp = Viewport::new(GeoLatLon::new(45.0, -93.0), 18.0, size(px(800.), px(600.)));
        let at = vp.geo_to_screen(GeoLatLon::new(45.0, -93.0));
        assert_eq!(
            layer.hit_test_point(&vp, at),
            Some(LayerHit::ExtensionMarker {
                extension: "maproulette".into(),
                marker: "a".into()
            })
        );
        let far = gpui::point(at.x + px(100.), at.y);
        assert_eq!(layer.hit_test_point(&vp, far), None);
    }
}
