//! The Notes layer: OSM notes drawn as small pin markers above the data.
//!
//! Notes are not OSM data. This layer is not editable, never modified, has no
//! upload diff, no export and no autosave snapshot, so it cannot reach pending
//! edits, the upload, autosave, the validator or search (which all work from
//! `as_editable`/`is_modified`/`autosave_snapshot`); its pins are found by
//! `MapLayer::hit_test_point` here and by nothing else.

use gpui::{AnyElement, Bounds, Pixels, Point, Window};

use crate::layers::paint::{circle_outline, paint_polygon, PaintFrame};
use crate::layers::{LayerHit, LayerId, LayerKind, MapLayer};
use crate::notes::{Note, NoteStore};
use crate::viewport::Viewport;

/// Open notes: a strong orange-red that stands out on pale Carto tiles and on
/// dark aerial imagery.
const OPEN_RGBA: u32 = 0xd9480fff;
/// Closed notes: a muted slate, translucent, so resolved notes recede.
const CLOSED_RGBA: u32 = 0x7c8794c8;
/// Halo around every pin (white reads on dark imagery and on light tiles
/// alike, and separates the pin from the data under it).
const HALO_RGBA: u32 = 0xfffffff2;
/// Halo of the pin that is open in the side panel.
const SELECTED_HALO_RGBA: u32 = 0x1d4ed8ff;
const DOT_RGBA: u32 = 0xffffffff;

/// Distance from the pin's tip (the note's position) up to the head's centre,
/// and the head's radius, in pixels.
const HEAD_OFFSET: f32 = 16.0;
const HEAD_RADIUS: f32 = 8.0;
/// Extra pixels around the pin that still count as a click on it.
const HIT_SLOP: f32 = 3.0;

pub struct NotesLayer {
    id: LayerId,
    name: String,
    visible: bool,
    store: NoteStore,
}

impl NotesLayer {
    pub fn new(id: LayerId, name: String, store: NoteStore) -> Self {
        Self {
            id,
            name,
            visible: true,
            store,
        }
    }
}

/// The outline of a pin whose tip is at `(x, y)`: the tip, then the arc of the
/// head from one tangent point round the top to the other.
fn pin_outline(x: f32, y: f32, scale: f32) -> Vec<(f32, f32)> {
    let (cy, r) = (y - HEAD_OFFSET * scale, HEAD_RADIUS * scale);
    let mut pts = vec![(x, y)];
    // In screen coordinates 90 degrees is straight down, towards the tip; the
    // tangent points from the tip are 60 degrees either side of that.
    let mut deg = 150.0f32;
    while deg <= 390.0 + 0.01 {
        let a = deg.to_radians();
        pts.push((x + r * a.cos(), cy + r * a.sin()));
        deg += 15.0;
    }
    pts
}

/// The id of the note whose pin is under `pos` (map-local pixels), the
/// topmost one when pins overlap. Closed notes count: they are drawn too.
fn hit_test(store: &NoteStore, viewport: &Viewport, pos: Point<Pixels>) -> Option<i64> {
    let (mx, my) = (pos.x.as_f32(), pos.y.as_f32());
    store.with(|s| {
        // Drawn open-last, so test that way round.
        let in_draw_order: Vec<&Note> = draw_order(&s.notes);
        in_draw_order.into_iter().rev().find_map(|n| {
            let p = viewport.geo_to_screen(n.lat, n.lon);
            let (x, y) = (p.x.as_f32(), p.y.as_f32());
            let near_head = {
                let (dx, dy) = (mx - x, my - (y - HEAD_OFFSET));
                (dx * dx + dy * dy).sqrt() <= HEAD_RADIUS + HIT_SLOP
            };
            // The tail between head and tip.
            let on_tail = (mx - x).abs() <= 4.0 && my >= y - HEAD_OFFSET && my <= y + HIT_SLOP;
            (near_head || on_tail).then_some(n.id)
        })
    })
}

/// Closed notes first, so open ones are drawn (and hit) on top.
fn draw_order(notes: &[Note]) -> Vec<&Note> {
    let mut v: Vec<&Note> = notes.iter().collect();
    v.sort_by_key(|n| n.is_open());
    v
}

impl MapLayer for NotesLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn id(&self) -> LayerId {
        self.id
    }

    fn kind(&self) -> LayerKind {
        LayerKind::Notes
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn hit_test_point(&self, viewport: &Viewport, point: Point<Pixels>) -> Option<LayerHit> {
        hit_test(&self.store, viewport, point).map(LayerHit::Note)
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
        self.store.with(|s| {
            for n in draw_order(&s.notes) {
                let (x, y) = frame.local_geo(n.lat, n.lon);
                if !(x.is_finite() && y.is_finite())
                    || x < -20.0
                    || y < -4.0
                    || x > w + 20.0
                    || y > h + 40.0
                {
                    continue;
                }
                let (x, y) = frame.to_window((x, y));
                let selected = s.selected == Some(n.id);
                let scale = if selected { 1.2 } else { 1.0 };
                let outline = pin_outline(x, y, scale);
                let (halo, halo_w) = if selected {
                    (SELECTED_HALO_RGBA, 4.0)
                } else {
                    (HALO_RGBA, 3.0)
                };
                paint_polygon(window, &outline, halo, Some(halo_w));
                paint_polygon(
                    window,
                    &outline,
                    if n.is_open() { OPEN_RGBA } else { CLOSED_RGBA },
                    None,
                );
                paint_polygon(
                    window,
                    &circle_outline(x, y - HEAD_OFFSET * scale, 2.8 * scale),
                    DOT_RGBA,
                    None,
                );
            }
        });
    }

    fn stats(&self) -> Vec<(String, String)> {
        vec![("notes".to_string(), self.store.len().to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::{CommentAction, NoteComment, NoteStatus};
    use gpui::{point, px, size};

    fn note(id: i64, lat: f64, lon: f64, status: NoteStatus) -> Note {
        Note {
            id,
            lat,
            lon,
            status,
            created: None,
            closed: None,
            comments: vec![NoteComment {
                date: None,
                uid: None,
                user: None,
                action: CommentAction::Opened,
                text: "x".into(),
            }],
        }
    }

    fn viewport() -> Viewport {
        Viewport::new(10.0, 20.0, 15.0, size(px(800.0), px(600.0)))
    }

    fn layer_with(notes: Vec<Note>) -> (NotesLayer, NoteStore) {
        let store = NoteStore::default();
        store.replace(notes);
        (
            NotesLayer::new(LayerId(7), "Notes".into(), store.clone()),
            store,
        )
    }

    #[test]
    fn the_layer_is_not_osm_data() {
        // What pending edits, the upload, autosave, the validator and search
        // each look at: none of it exists for this layer.
        let (layer, _) = layer_with(vec![note(1, 10.0, 20.0, NoteStatus::Open)]);
        assert!(layer.as_editable().is_none(), "search/validator/selection");
        assert!(
            layer.as_osm().is_none(),
            "pending edits, quit prompt, upload, autosave, export, download bounds"
        );
        assert_eq!(layer.id(), LayerId(7));
    }

    #[test]
    fn pins_are_hit_on_head_and_tail_only() {
        let vp = viewport();
        let (_, store) = layer_with(vec![note(1, 10.0, 20.0, NoteStatus::Open)]);
        let tip = vp.geo_to_screen(10.0, 20.0);
        let at = |dx: f32, dy: f32| point(tip.x + px(dx), tip.y + px(dy));
        assert_eq!(hit_test(&store, &vp, at(0.0, -HEAD_OFFSET)), Some(1));
        assert_eq!(hit_test(&store, &vp, at(0.0, -3.0)), Some(1));
        assert_eq!(hit_test(&store, &vp, at(5.0, -HEAD_OFFSET - 4.0)), Some(1));
        // Beside the head, and below the tip: not a hit.
        assert_eq!(hit_test(&store, &vp, at(20.0, -HEAD_OFFSET)), None);
        assert_eq!(hit_test(&store, &vp, at(0.0, 15.0)), None);
        assert_eq!(hit_test(&store, &vp, at(0.0, -40.0)), None);
    }

    #[test]
    fn the_trait_method_reports_the_note_under_the_click() {
        let vp = viewport();
        let (layer, _) = layer_with(vec![note(4, 10.0, 20.0, NoteStatus::Open)]);
        let tip = vp.geo_to_screen(10.0, 20.0);
        let head = point(tip.x, tip.y - px(HEAD_OFFSET));
        assert_eq!(layer.hit_test_point(&vp, head), Some(LayerHit::Note(4)));
        let away = point(tip.x + px(60.0), tip.y);
        assert_eq!(layer.hit_test_point(&vp, away), None);
    }

    #[test]
    fn an_open_pin_is_on_top_of_a_closed_one() {
        let vp = viewport();
        let (_, store) = layer_with(vec![
            note(1, 10.0, 20.0, NoteStatus::Open),
            note(2, 10.0, 20.0, NoteStatus::Closed),
        ]);
        let tip = vp.geo_to_screen(10.0, 20.0);
        let pos = point(tip.x, tip.y - px(HEAD_OFFSET));
        assert_eq!(hit_test(&store, &vp, pos), Some(1));
        let order: Vec<i64> = store.with(|s| draw_order(&s.notes).iter().map(|n| n.id).collect());
        assert_eq!(order, vec![2, 1]);
    }

    #[test]
    fn the_pin_outline_starts_at_the_tip_and_stays_within_its_head() {
        let pts = pin_outline(100.0, 200.0, 1.0);
        assert_eq!(pts[0], (100.0, 200.0));
        for (x, y) in &pts[1..] {
            let d = ((x - 100.0).powi(2) + (y - (200.0 - HEAD_OFFSET)).powi(2)).sqrt();
            assert!((d - HEAD_RADIUS).abs() < 0.01);
            assert!(*y <= 200.0);
        }
        assert!(pts.len() > 10);
    }
}
