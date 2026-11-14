use super::*;

/// Nodes shown in full whatever the zoom because they are selected; see
/// `OsmLayer::pins`.
#[derive(Default)]
pub(super) struct Pins {
    pub(super) nodes: HashSet<i64>,
    /// Indices into `way_vertices`.
    pub(super) ways: HashSet<usize>,
    /// Index into `way_vertices` of the way whose vertices are previewed
    /// (the one under the pointer), if any.
    pub(super) preview: Option<usize>,
}

/// A circle of `diameter` centered on `center`, as quad bounds.
pub(super) fn circle_bounds(center: Point<Pixels>, diameter: f32) -> Bounds<Pixels> {
    let half = px(diameter / 2.0);
    Bounds {
        origin: point(center.x - half, center.y - half),
        size: size(px(diameter), px(diameter)),
    }
}

/// Paint a single round node marker at `screen_point` (already including
/// the canvas origin offset) using `style`: a dot of the style's color and
/// size inside a 1px dark outline, so it reads against any imagery. Shared
/// by the per-frame node-marker pass in `render_canvas` and by
/// `render_highlight`'s `FeatureKind::Way` arm, which repaints a
/// highlighted way's vertex markers on top of the highlight stroke so they
/// aren't hidden underneath it.
pub(super) fn paint_node_marker(
    window: &mut Window,
    screen_point: Point<Pixels>,
    style: NodeStyle,
) {
    let diameter = style.size + 2.0;
    window.paint_quad(quad(
        circle_bounds(screen_point, diameter),
        px(diameter / 2.0),
        rgb(style.color),
        px(1.0),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
}

/// A previewed vertex (see `NodeForm::Preview`): a small white dot in a dark
/// outline, deliberately lighter than the style-coloured marker of a selected
/// way's vertex so hover and selection read differently.
pub(super) fn paint_preview_marker(window: &mut Window, screen_point: Point<Pixels>) {
    let diameter = node_markers::PREVIEW_DIAMETER;
    window.paint_quad(quad(
        circle_bounds(screen_point, diameter),
        px(diameter / 2.0),
        rgb(0xFFFFFF),
        px(1.5),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
}

/// The simple form of a tagged node at low zoom: a tiny empty square of outer
/// edge `edge` with a `SIMPLE_STROKE` outline, as four thin rectangles (eight
/// triangles) of the frame's shared `path`. The corner is snapped to half a
/// logical pixel, a whole device pixel on a 2x display, so the lines stay
/// crisp.
pub(super) fn push_simple_node_marker(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    center: Point<Pixels>,
    edge: f32,
) {
    let snap = |v: f32| (v * 2.0).round() / 2.0;
    let x0 = snap(f32::from(center.x) - edge / 2.0);
    let y0 = snap(f32::from(center.y) - edge / 2.0);
    let (x1, y1) = (x0 + edge, y0 + edge);
    let s = node_markers::SIMPLE_STROKE;
    let mut rect = |l: f32, t: f32, r: f32, b: f32| {
        let (tl, tr) = (point(px(l), px(t)), point(px(r), px(t)));
        let (bl, br) = (point(px(l), px(b)), point(px(r), px(b)));
        push_triangle(path, bounds_min_max, tl, tr, bl);
        push_triangle(path, bounds_min_max, tr, br, bl);
    };
    rect(x0, y0, x1, y0 + s); // top
    rect(x0, y1 - s, x1, y1); // bottom
    rect(x0, y0 + s, x0 + s, y1 - s); // left
    rect(x1 - s, y0 + s, x1, y1 - s); // right
}

/// A stylesheet-drawn node marker (`symbol-shape`) of `visual.size` pixels:
/// a hollow square or a `+`, in `SIMPLE_STROKE` lines, added to the frame's
/// shared `path` for its colour.
pub(super) fn push_styled_node_marker(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    center: Point<Pixels>,
    visual: NodeVisual,
) {
    match visual.shape {
        NodeShape::Cross => {
            let snap = |v: f32| (v * 2.0).round() / 2.0;
            let (cx, cy) = (snap(f32::from(center.x)), snap(f32::from(center.y)));
            let (h, s) = (visual.size / 2.0, node_markers::SIMPLE_STROKE);
            let mut rect = |l: f32, t: f32, r: f32, b: f32| {
                let (tl, tr) = (point(px(l), px(t)), point(px(r), px(t)));
                let (bl, br) = (point(px(l), px(b)), point(px(r), px(b)));
                push_triangle(path, bounds_min_max, tl, tr, bl);
                push_triangle(path, bounds_min_max, tr, br, bl);
            };
            rect(cx - h, cy - s / 2.0, cx + h, cy + s / 2.0);
            rect(cx - s / 2.0, cy - h, cx + s / 2.0, cy + h);
        }
        _ => push_simple_node_marker(path, bounds_min_max, center, visual.size),
    }
}

/// Extra half-width of a route overlay line beyond the way's casing, in
/// pixels, so the way stays readable on top of it.
pub(super) const ROUTE_OVERLAY_EXTRA: f32 = 3.0;

/// Diameter of a turn-restriction marker, in pixels.
pub(super) const RESTRICTION_MARKER_DIAMETER: f32 = 14.0;

/// Where a turn-restriction marker sits relative to its via position (right
/// and up), so it doesn't cover the via node and the node stays pickable.
pub(super) const RESTRICTION_OFFSET_PX: (f32, f32) = (11.0, -11.0);

/// Paint a turn-restriction marker centered on `center`: a red ring for a
/// `no_*` restriction, a blue disc for an `only_*` one, on a white backing.
pub(super) fn paint_restriction_marker(window: &mut Window, center: Point<Pixels>, only: bool) {
    use crate::relation_overlay::{RESTRICTION_NO_COLOUR, RESTRICTION_ONLY_COLOUR};
    let d = RESTRICTION_MARKER_DIAMETER;
    window.paint_quad(quad(
        circle_bounds(center, d),
        px(d / 2.0),
        rgb(0xFFFFFF),
        px(1.0),
        rgb(CASING_COLOR),
        BorderStyle::Solid,
    ));
    if only {
        window.paint_quad(quad(
            circle_bounds(center, d - 4.0),
            px((d - 4.0) / 2.0),
            rgb(RESTRICTION_ONLY_COLOUR),
            px(0.0),
            gpui::transparent_black(),
            BorderStyle::Solid,
        ));
    } else {
        paint_ring(window, center, d - 2.0, 2.5, RESTRICTION_NO_COLOUR);
    }
}

/// Paint a `width`-px ring of `diameter` centered on `center`.
pub(super) fn paint_ring(
    window: &mut Window,
    center: Point<Pixels>,
    diameter: f32,
    width: f32,
    color: u32,
) {
    window.paint_quad(quad(
        circle_bounds(center, diameter),
        px(diameter / 2.0),
        gpui::transparent_black(),
        px(width),
        rgb(color),
        BorderStyle::Solid,
    ));
}

/// The `(halo, core)` colors of a highlight: a wide outer stroke and a
/// narrower inner one. The pairs are chosen so one of the two always
/// contrasts with whatever is underneath.
pub(super) fn highlight_colors(kind: HighlightKind) -> (u32, u32) {
    match kind {
        HighlightKind::Selected => (0xFFFFFF, SELECTION_ACCENT),
        HighlightKind::Hovered => (CASING_COLOR, 0xFFFFFF),
        HighlightKind::Member => (0xFFFFFF, RELATION_MEMBER_ACCENT),
    }
}

impl OsmLayer {
    /// The nodes shown in full whatever the zoom: selected nodes and the
    /// nodes of selected ways (as node ids and way indices).
    pub(super) fn pins(&self) -> Pins {
        let mut pins = Pins::default();
        for f in &self.view.highlight {
            match f.kind {
                FeatureKind::Node => {
                    pins.nodes.insert(f.id);
                }
                FeatureKind::Way => {
                    if let Some(&i) = self.derived.way_id_to_index.get(&f.id) {
                        pins.ways.insert(i);
                    }
                }
                FeatureKind::Relation => {}
            }
        }
        pins.preview = self
            .view
            .preview_way
            .and_then(|w| self.derived.way_id_to_index.get(&w).copied());
        pins
    }

    /// How node `id` (at `idx` in the node cache) is shown at `zoom`: the
    /// one answer behind drawing, hover, click and box select.
    pub(super) fn node_form_at(&self, pins: &Pins, idx: usize, id: i64, zoom: f64) -> NodeForm {
        let in_way = |pred: &dyn Fn(&usize) -> bool| {
            self.derived
                .node_to_ways
                .get(&id)
                .is_some_and(|ws| ws.iter().any(pred))
        };
        let pinned = pins.nodes.contains(&id)
            || (!pins.ways.is_empty() && in_way(&|w| pins.ways.contains(w)));
        let previewed = pins.preview.is_some_and(|p| in_way(&|w| *w == p));
        let style = self.derived.node_cache.styles[idx];
        let tagged = style.tagged;
        let form = node_markers::node_form_previewed(
            tagged,
            self.is_standalone(tagged, id, zoom),
            pinned,
            previewed,
            zoom,
        );
        match self.styled_node_form(&style, id, zoom) {
            Some(styled) if !pinned => styled,
            _ => form,
        }
    }

    /// The stylesheet's own marker for node `id`, if its rules ask for a
    /// hollow square or a cross for a node in as many ways as this one and
    /// the view is zoomed in far enough.
    pub(super) fn styled_node_form(
        &self,
        style: &NodeStyle,
        id: i64,
        zoom: f64,
    ) -> Option<NodeForm> {
        if zoom < node_markers::STYLED_MIN_ZOOM || !style.has_custom_marker() {
            return None;
        }
        let ways = self.derived.node_to_ways.get(&id).map_or(0, |w| w.len());
        let visual = style.visual(ways);
        visual.shape.is_custom().then_some(NodeForm::Styled(visual))
    }
}
