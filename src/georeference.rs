//! The maths of placing a photo or scanned map on the map: four corner pins
//! in Web Mercator, the projective transform they define, and a CPU warp of
//! the picture into a north-up raster that gpui can draw as a plain image
//! (gpui paints images axis-aligned only).
//!
//! The corners are the picture's top-left, top-right, bottom-right and
//! bottom-left, in that order. Free corners give a full perspective fit;
//! `scale_rotate_about` keeps the picture's shape (what a flat scan needs).

use crate::coordinates::Mercator;
use crate::download_bounds::MercRect;
use image::RgbaImage;

/// A picture's corners: top-left, top-right, bottom-right, bottom-left.
pub type Corners = [Mercator; 4];

/// A quad whose area is below this fraction of its bounding box is treated
/// as collapsed.
const MIN_AREA_FRACTION: f64 = 1e-4;

/// A 3x3 projective transform, row-major.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography([f64; 9]);

impl Homography {
    /// The transform taking the unit square (u right, v down: top-left (0,0),
    /// top-right (1,0), bottom-right (1,1), bottom-left (0,1)) to `corners`.
    pub fn unit_square_to(corners: &Corners) -> Homography {
        let [p0, p1, p2, p3] = *corners;
        let (dx1, dx2, dx3) = (p1.x - p2.x, p3.x - p2.x, p0.x - p1.x + p2.x - p3.x);
        let (dy1, dy2, dy3) = (p1.y - p2.y, p3.y - p2.y, p0.y - p1.y + p2.y - p3.y);
        if dx3.abs() < 1e-12 && dy3.abs() < 1e-12 {
            // A parallelogram: affine.
            return Homography([
                p1.x - p0.x,
                p2.x - p1.x,
                p0.x,
                p1.y - p0.y,
                p2.y - p1.y,
                p0.y,
                0.0,
                0.0,
                1.0,
            ]);
        }
        let den = dx1 * dy2 - dx2 * dy1;
        let g = (dx3 * dy2 - dx2 * dy3) / den;
        let h = (dx1 * dy3 - dx3 * dy1) / den;
        Homography([
            p1.x - p0.x + g * p1.x,
            p3.x - p0.x + h * p3.x,
            p0.x,
            p1.y - p0.y + g * p1.y,
            p3.y - p0.y + h * p3.y,
            p0.y,
            g,
            h,
            1.0,
        ])
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        (
            (m[0] * x + m[1] * y + m[2]) / w,
            (m[3] * x + m[4] * y + m[5]) / w,
        )
    }

    /// The inverse transform, or `None` when singular.
    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.0;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if !det.is_finite() || det.abs() < 1e-30 {
            return None;
        }
        Some(Homography(c.map(|v| v / det)))
    }
}

/// The smallest rectangle holding `corners`.
pub fn bounds(corners: &Corners) -> MercRect {
    let xs = corners.map(|c| c.x);
    let ys = corners.map(|c| c.y);
    MercRect::new(
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ys.iter().cloned().fold(f64::INFINITY, f64::min),
        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    )
}

/// Whether `corners` make a proper convex quad (no fold, no collapse), the
/// only shapes a picture can be stretched to without turning inside out.
pub fn is_valid(corners: &Corners) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let (a, b, c) = (corners[i], corners[(i + 1) % 4], corners[(i + 2) % 4]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if !cross.is_finite() || cross == 0.0 {
            return false;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    let r = bounds(corners);
    let box_area = (r.max_x - r.min_x) * (r.max_y - r.min_y);
    quad_area(corners).abs() > box_area * MIN_AREA_FRACTION
}

fn quad_area(c: &Corners) -> f64 {
    let mut s = 0.0;
    for i in 0..4 {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        s += a.x * b.y - b.x * a.y;
    }
    s / 2.0
}

/// Whether `p` is inside the (convex) quad.
pub fn contains(corners: &Corners, p: Mercator) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross == 0.0 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// `corners` moved by `(dx, dy)` metres.
pub fn translated(corners: &Corners, dx: f64, dy: f64) -> Corners {
    corners.map(|c| Mercator::new(c.x + dx, c.y + dy))
}

/// All of `corners` scaled and rotated about `pivot` so that `from` lands on
/// `to`: the picture keeps its shape.
pub fn scale_rotate_about(
    corners: &Corners,
    pivot: Mercator,
    from: Mercator,
    to: Mercator,
) -> Corners {
    let (vx, vy) = (from.x - pivot.x, from.y - pivot.y);
    let (wx, wy) = (to.x - pivot.x, to.y - pivot.y);
    let len2 = vx * vx + vy * vy;
    if len2 == 0.0 {
        return *corners;
    }
    // The complex ratio w / v is the scale and rotation in one.
    let (re, im) = ((wx * vx + wy * vy) / len2, (wy * vx - wx * vy) / len2);
    corners.map(|c| {
        let (px, py) = (c.x - pivot.x, c.y - pivot.y);
        Mercator::new(pivot.x + px * re - py * im, pivot.y + px * im + py * re)
    })
}

/// `corners` with corner `index` pulled to `to`. With `keep_shape` the whole
/// picture is scaled and rotated about the opposite corner instead, so it is
/// not distorted.
pub fn drag_corner(corners: &Corners, index: usize, to: Mercator, keep_shape: bool) -> Corners {
    if keep_shape {
        scale_rotate_about(corners, corners[(index + 2) % 4], corners[index], to)
    } else {
        let mut out = *corners;
        out[index] = to;
        out
    }
}

/// Where a picture of `size` (width, height) goes by default: centred on
/// `centre`, `width` metres across, its shape kept.
pub fn initial_corners(size: (u32, u32), centre: Mercator, width: f64) -> Corners {
    let aspect = size.1.max(1) as f64 / size.0.max(1) as f64;
    let (hw, hh) = (width / 2.0, width * aspect / 2.0);
    [
        Mercator::new(centre.x - hw, centre.y + hh),
        Mercator::new(centre.x + hw, centre.y + hh),
        Mercator::new(centre.x + hw, centre.y - hh),
        Mercator::new(centre.x - hw, centre.y - hh),
    ]
}

/// A picture warped onto the north-up grid: premultiplied BGRA (what gpui
/// draws) covering `bounds`, transparent outside the picture.
pub struct Warped {
    pub bgra: RgbaImage,
    pub bounds: MercRect,
}

/// Resample `src` into the north-up raster of `corners`' bounding box, its
/// longest side `max_side` pixels (less for a small picture, which is not
/// blown up past twice its size). `None` for corners that are not a proper
/// quad.
pub fn warp(src: &RgbaImage, corners: &Corners, max_side: u32) -> Option<Warped> {
    if !is_valid(corners) || src.width() == 0 || src.height() == 0 {
        return None;
    }
    let to_unit = Homography::unit_square_to(corners).inverse()?;
    let b = bounds(corners);
    let (bw, bh) = (b.max_x - b.min_x, b.max_y - b.min_y);
    let longest = max_side
        .min(src.width().max(src.height()).saturating_mul(2))
        .max(16) as f64;
    let (ow, oh) = if bw >= bh {
        (longest as u32, ((longest * bh / bw).round() as u32).max(1))
    } else {
        (((longest * bw / bh).round() as u32).max(1), longest as u32)
    };
    let (sw, sh) = (src.width() as f64, src.height() as f64);
    let mut out = RgbaImage::new(ow, oh);
    for j in 0..oh {
        let y = b.max_y - (j as f64 + 0.5) / oh as f64 * bh;
        for i in 0..ow {
            let x = b.min_x + (i as f64 + 0.5) / ow as f64 * bw;
            let (u, v) = to_unit.apply(x, y);
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let px = sample(src, u * sw - 0.5, v * sh - 0.5);
            let a = px[3] as u32;
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            out.put_pixel(i, j, image::Rgba([pm(px[2]), pm(px[1]), pm(px[0]), px[3]]));
        }
    }
    Some(Warped {
        bgra: out,
        bounds: b,
    })
}

/// Bilinear sample at fractional pixel `(x, y)`, edges clamped.
fn sample(img: &RgbaImage, x: f64, y: f64) -> [u8; 4] {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |xi: i64, yi: i64| {
        img.get_pixel(xi.clamp(0, w - 1) as u32, yi.clamp(0, h - 1) as u32)
            .0
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (p00, p10, p01, p11) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );
    let mut out = [0u8; 4];
    for k in 0..4 {
        let top = p00[k] as f64 * (1.0 - fx) + p10[k] as f64 * fx;
        let bottom = p01[k] as f64 * (1.0 - fx) + p11[k] as f64 * fx;
        out[k] = (top * (1.0 - fy) + bottom * fy).round() as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(x: f64, y: f64) -> Mercator {
        Mercator::new(x, y)
    }

    fn square() -> Corners {
        [m(0.0, 10.0), m(10.0, 10.0), m(10.0, 0.0), m(0.0, 0.0)]
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn the_unit_square_corners_land_on_the_pins() {
        let skew = [m(1.0, 12.0), m(14.0, 9.0), m(11.0, -2.0), m(-3.0, 1.0)];
        let h = Homography::unit_square_to(&skew);
        for (uv, c) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
            .into_iter()
            .zip(skew)
        {
            assert!(close(h.apply(uv.0, uv.1), (c.x, c.y)), "{uv:?} -> {c:?}");
        }
    }

    #[test]
    fn the_inverse_undoes_the_transform() {
        let skew = [m(1.0, 12.0), m(14.0, 9.0), m(11.0, -2.0), m(-3.0, 1.0)];
        let h = Homography::unit_square_to(&skew);
        let inv = h.inverse().unwrap();
        let (x, y) = h.apply(0.3, 0.7);
        assert!(close(inv.apply(x, y), (0.3, 0.7)));
    }

    #[test]
    fn a_parallelogram_is_affine() {
        let h = Homography::unit_square_to(&[m(0.0, 0.0), m(4.0, 1.0), m(5.0, 4.0), m(1.0, 3.0)]);
        assert!(close(h.apply(0.5, 0.5), (2.5, 2.0)));
    }

    #[test]
    fn folded_or_collapsed_quads_are_invalid() {
        assert!(is_valid(&square()));
        // Bow-tie: two corners swapped.
        assert!(!is_valid(&[
            m(0.0, 10.0),
            m(10.0, 0.0),
            m(10.0, 10.0),
            m(0.0, 0.0)
        ]));
        // A concave dent.
        assert!(!is_valid(&[
            m(0.0, 10.0),
            m(10.0, 10.0),
            m(1.0, 9.0),
            m(0.0, 0.0)
        ]));
        // All on one line.
        assert!(!is_valid(&[
            m(0.0, 0.0),
            m(1.0, 1.0),
            m(2.0, 2.0),
            m(3.0, 3.0)
        ]));
    }

    #[test]
    fn contains_tells_inside_from_outside() {
        assert!(contains(&square(), m(5.0, 5.0)));
        assert!(!contains(&square(), m(11.0, 5.0)));
        assert!(!contains(&square(), m(5.0, -0.1)));
    }

    #[test]
    fn scale_rotate_keeps_the_shape() {
        // Pull the top-right pin from (10,10) to (0,20) about the bottom-left
        // pin: a 45 degree turn and a scale of sqrt(2).
        let out = scale_rotate_about(&square(), m(0.0, 0.0), m(10.0, 10.0), m(0.0, 20.0));
        assert!(close((out[1].x, out[1].y), (0.0, 20.0)));
        assert!(close((out[0].x, out[0].y), (-10.0, 10.0)));
        assert!(close((out[2].x, out[2].y), (10.0, 10.0)));
        assert!(is_valid(&out));
    }

    #[test]
    fn a_dragged_corner_moves_alone_unless_the_shape_is_kept() {
        let free = drag_corner(&square(), 1, m(12.0, 8.0), false);
        assert_eq!((free[1].x, free[1].y), (12.0, 8.0));
        assert_eq!(free[0], square()[0]);
        let kept = drag_corner(&square(), 1, m(0.0, 20.0), true);
        // Pivot is the opposite (bottom-left) corner, which stays put.
        assert!(close((kept[3].x, kept[3].y), (0.0, 0.0)));
        assert!(close((kept[1].x, kept[1].y), (0.0, 20.0)));
    }

    #[test]
    fn translating_moves_every_pin() {
        let out = translated(&square(), 3.0, -2.0);
        assert!(close((out[2].x, out[2].y), (13.0, -2.0)));
    }

    #[test]
    fn the_default_placement_keeps_the_aspect() {
        let c = initial_corners((400, 200), m(100.0, 50.0), 20.0);
        assert!(close((c[0].x, c[0].y), (90.0, 55.0)));
        assert!(close((c[2].x, c[2].y), (110.0, 45.0)));
    }

    /// 8x8: red top-left, green top-right, blue bottom-left, white
    /// bottom-right, in 4x4 blocks.
    fn quadrants() -> RgbaImage {
        RgbaImage::from_fn(8, 8, |x, y| match (x < 4, y < 4) {
            (true, true) => image::Rgba([255, 0, 0, 255]),
            (false, true) => image::Rgba([0, 255, 0, 255]),
            (true, false) => image::Rgba([0, 0, 255, 255]),
            (false, false) => image::Rgba([255, 255, 255, 255]),
        })
    }

    /// The BGRA output pixel read back as RGB.
    fn rgb(w: &Warped, x: u32, y: u32) -> [u8; 4] {
        let p = w.bgra.get_pixel(x, y).0;
        [p[2], p[1], p[0], p[3]]
    }

    #[test]
    fn an_upright_placement_reproduces_the_picture() {
        let w = warp(&quadrants(), &square(), 64).unwrap();
        assert_eq!((w.bgra.width(), w.bgra.height()), (16, 16));
        assert_eq!(rgb(&w, 1, 1), [255, 0, 0, 255]);
        assert_eq!(rgb(&w, 14, 1), [0, 255, 0, 255]);
        assert_eq!(rgb(&w, 1, 14), [0, 0, 255, 255]);
        assert_eq!(rgb(&w, 14, 14), [255, 255, 255, 255]);
    }

    #[test]
    fn a_turned_placement_leaves_the_corners_of_the_raster_empty() {
        // The square turned 45 degrees: a diamond in its bounding box.
        let diamond = [m(5.0, 10.0), m(10.0, 5.0), m(5.0, 0.0), m(0.0, 5.0)];
        let w = warp(&quadrants(), &diamond, 64).unwrap();
        let (n, e) = (w.bgra.width() - 1, w.bgra.height() - 1);
        for (x, y) in [(0, 0), (n, 0), (0, e), (n, e)] {
            assert_eq!(rgb(&w, x, y)[3], 0, "corner {x},{y} should be clear");
        }
        // The middle is picture.
        assert_eq!(rgb(&w, n / 2, e / 2)[3], 255);
        // Red (the picture's top-left) is now the top quarter of the diamond.
        let px = rgb(&w, n / 2, e / 4);
        assert!(px[0] > px[1] && px[0] > px[2], "{px:?}");
    }

    #[test]
    fn an_invalid_quad_does_not_warp() {
        let bow = [m(0.0, 10.0), m(10.0, 0.0), m(10.0, 10.0), m(0.0, 0.0)];
        assert!(warp(&quadrants(), &bow, 64).is_none());
    }

    #[test]
    fn a_small_picture_is_not_blown_up() {
        let w = warp(&quadrants(), &square(), 4096).unwrap();
        assert_eq!(w.bgra.width(), 16);
    }

    #[test]
    fn translucent_pixels_are_premultiplied() {
        let mut img = RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([200, 100, 0, 128]));
        let w = warp(&img, &square(), 16).unwrap();
        let p = w.bgra.get_pixel(1, 1).0;
        assert_eq!(p[3], 128);
        assert_eq!(p[2], 100); // red 200 * 128/255
        assert_eq!(p[1], 50);
    }
}
