//! Painting roads in the side-panel diagrams (the Junction and Lanes
//! sections): asphalt, lane markings and `turn:lanes` arrow glyphs, in
//! canvas pixels relative to an origin.

use gpui::{point, px, rgb, PathBuilder, Pixels, Point, Window};
use osm_gpui::lanes::Arrow;

pub(crate) const ASPHALT: u32 = 0x5B6168;
/// A road whose lane count isn't known.
pub(crate) const ASPHALT_UNKNOWN: u32 = 0x8A9097;
pub(crate) const LANE_LINE: u32 = 0xF4F4F4;
pub(crate) const CENTRE_LINE: u32 = 0xF2C200;
pub(crate) const ARROW: u32 = 0xFFFFFF;

/// 2-D vector helpers in canvas pixels (y down).
#[derive(Clone, Copy, Debug)]
pub(crate) struct V(pub(crate) f32, pub(crate) f32);

impl V {
    pub(crate) fn add(self, o: V) -> V {
        V(self.0 + o.0, self.1 + o.1)
    }
    pub(crate) fn sub(self, o: V) -> V {
        V(self.0 - o.0, self.1 - o.1)
    }
    pub(crate) fn mul(self, k: f32) -> V {
        V(self.0 * k, self.1 * k)
    }
    /// Rotate clockwise on screen by `deg`.
    pub(crate) fn rot(self, deg: f32) -> V {
        let (s, c) = deg.to_radians().sin_cos();
        V(self.0 * c - self.1 * s, self.0 * s + self.1 * c)
    }
    pub(crate) fn pt(self, origin: Point<Pixels>) -> Point<Pixels> {
        point(origin.x + px(self.0), origin.y + px(self.1))
    }
}

pub(crate) fn fill(window: &mut Window, o: Point<Pixels>, pts: &[V], colour: u32) {
    let mut b = PathBuilder::fill();
    for (i, p) in pts.iter().enumerate() {
        if i == 0 {
            b.move_to(p.pt(o));
        } else {
            b.line_to(p.pt(o));
        }
    }
    b.close();
    if let Ok(path) = b.build() {
        window.paint_path(path, rgb(colour));
    }
}

pub(crate) fn stroke(window: &mut Window, o: Point<Pixels>, pts: &[V], width: f32, colour: u32) {
    if pts.len() < 2 {
        return;
    }
    let mut b = PathBuilder::stroke(px(width));
    b.move_to(pts[0].pt(o));
    for p in &pts[1..] {
        b.line_to(p.pt(o));
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, rgb(colour));
    }
}

/// A dashed lane divider from `a` to `b`.
pub(crate) fn lane_divider(window: &mut Window, o: Point<Pixels>, a: V, b: V) {
    let (dash, gap) = (6.0, 5.0);
    let d = b.sub(a);
    let total = d.0.hypot(d.1);
    if total <= 0.0 {
        return;
    }
    let dir = d.mul(1.0 / total);
    let mut t = 0.0;
    let mut builder = PathBuilder::stroke(px(1.2));
    while t < total {
        let e = (t + dash).min(total);
        builder.move_to(a.add(dir.mul(t)).pt(o));
        builder.line_to(a.add(dir.mul(e)).pt(o));
        t = e + gap;
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, rgb(LANE_LINE));
    }
}

pub(crate) fn arrow_head(
    window: &mut Window,
    o: Point<Pixels>,
    tip: V,
    dir: V,
    size: f32,
    colour: u32,
) {
    let back = tip.sub(dir.mul(size));
    let side = V(-dir.1, dir.0).mul(size * 0.6);
    fill(window, o, &[tip, back.add(side), back.sub(side)], colour);
}

/// A `turn:lanes` glyph: a shaft along `h` from `base`, branching per arrow.
pub(crate) fn lane_glyph(
    window: &mut Window,
    o: Point<Pixels>,
    base: V,
    h: V,
    len: f32,
    arrows: &[Arrow],
) {
    let w = (len / 9.0).max(1.4);
    let head = len * 0.3;
    if arrows.is_empty() {
        // `none`: a short bar.
        stroke(window, o, &[base, base.add(h.mul(len * 0.5))], w, ARROW);
        return;
    }
    let fork = base.add(h.mul(len * 0.55));
    stroke(window, o, &[base, fork], w, ARROW);
    for &a in arrows {
        match a {
            Arrow::Through => {
                let tip = base.add(h.mul(len));
                stroke(window, o, &[fork, tip.sub(h.mul(head * 0.6))], w, ARROW);
                arrow_head(window, o, tip, h, head, ARROW);
            }
            Arrow::Reverse => {
                let side = h.rot(-90.0);
                let top = fork.add(h.mul(len * 0.2));
                let over = top.add(side.mul(len * 0.35));
                let tip = over.sub(h.mul(len * 0.35));
                stroke(
                    window,
                    o,
                    &[fork, top, over, tip.add(h.mul(head * 0.6))],
                    w,
                    ARROW,
                );
                arrow_head(window, o, tip, h.mul(-1.0), head, ARROW);
            }
            other => {
                let d = h.rot(other.angle());
                let tip = fork.add(d.mul(len * 0.5));
                stroke(window, o, &[fork, tip.sub(d.mul(head * 0.6))], w, ARROW);
                arrow_head(window, o, tip, d, head, ARROW);
            }
        }
    }
}
