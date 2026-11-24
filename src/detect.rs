//! Finding the outline of the thing under a click in an aerial image: grow a
//! region of similar colour from the clicked pixel, trace its boundary, and
//! either fit the best rectangle (a building) or keep the simplified outline
//! (an area of trees). Pure pixel work on a small raster, so it is testable
//! on drawn pictures. It is a colour heuristic, not a trained model: shadows,
//! two-tone roofs and canopy edges will fool it.

/// A point in the raster, in pixels (x right, y down).
pub type Pt = (f64, f64);

/// A small picture. `None` is a pixel nothing is known about (the imagery was
/// not loaded there).
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Option<[u8; 3]>>,
}

impl Raster {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![None; width * height],
        }
    }

    pub fn at(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        self.pixels[y * self.width + x]
    }

    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        self.pixels[y * self.width + x] = Some(rgb);
    }
}

/// How different two colours look, roughly in 0..=255 (a weighted RGB
/// distance that is more sensitive to green and to red-blue balance, like the
/// eye).
fn colour_distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (dr, dg, db) = (
        a[0] as f64 - b[0] as f64,
        a[1] as f64 - b[1] as f64,
        a[2] as f64 - b[2] as f64,
    );
    let mean_r = (a[0] as f64 + b[0] as f64) / 2.0;
    let wr = 2.0 + mean_r / 256.0;
    let wb = 2.0 + (255.0 - mean_r) / 256.0;
    ((wr * dr * dr + 4.0 * dg * dg + wb * db * db) / 9.0).sqrt()
}

/// The typical colour around `seed`: the average of the known pixels within
/// two pixels.
fn seed_colour(r: &Raster, (sx, sy): (usize, usize)) -> Option<[u8; 3]> {
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    for y in sy.saturating_sub(2)..=(sy + 2).min(r.height - 1) {
        for x in sx.saturating_sub(2)..=(sx + 2).min(r.width - 1) {
            if let Some(p) = r.at(x, y) {
                for (s, c) in sum.iter_mut().zip(p) {
                    *s += c as u32;
                }
                n += 1;
            }
        }
    }
    (n > 0).then(|| sum.map(|s| (s / n) as u8))
}

type Mask = Vec<bool>;

/// The pixels connected to `seed` whose colour is within `tolerance` of the
/// seed's.
fn flood(r: &Raster, seed: (usize, usize), tolerance: f64) -> Option<Mask> {
    let target = seed_colour(r, seed)?;
    let mut mask = vec![false; r.width * r.height];
    let mut stack = vec![seed];
    mask[seed.1 * r.width + seed.0] = true;
    while let Some((x, y)) = stack.pop() {
        let around = [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ];
        for (nx, ny) in around {
            if nx >= r.width || ny >= r.height || mask[ny * r.width + nx] {
                continue;
            }
            if r.at(nx, ny)
                .is_some_and(|p| colour_distance(p, target) <= tolerance)
            {
                mask[ny * r.width + nx] = true;
                stack.push((nx, ny));
            }
        }
    }
    Some(mask)
}

fn erode(mask: &Mask, w: usize, h: usize) -> Mask {
    let mut out = vec![false; mask.len()];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            out[y * w + x] = (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| mask[yy * w + xx]));
        }
    }
    out
}

fn dilate(mask: &Mask, w: usize, h: usize) -> Mask {
    let mut out = vec![false; mask.len()];
    for y in 0..h {
        for x in 0..w {
            if mask[y * w + x] {
                for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                        out[yy * w + xx] = true;
                    }
                }
            }
        }
    }
    out
}

/// Remove specks and thin spurs: erode then dilate `radius` times.
fn open(mut mask: Mask, w: usize, h: usize, radius: usize) -> Mask {
    for _ in 0..radius {
        mask = erode(&mask, w, h);
    }
    for _ in 0..radius {
        mask = dilate(&mask, w, h);
    }
    mask
}

/// The connected part of `mask` that holds `seed`, or failing that the
/// biggest connected part.
fn component(mask: &Mask, w: usize, h: usize, seed: (usize, usize)) -> Mask {
    let mut label = vec![0usize; mask.len()];
    let mut sizes = vec![0usize];
    for start in 0..mask.len() {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len();
        sizes.push(0);
        let mut stack = vec![start];
        label[start] = id;
        while let Some(i) = stack.pop() {
            sizes[id] += 1;
            let (x, y) = (i % w, i / w);
            let around = [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ];
            for (nx, ny) in around {
                if nx < w && ny < h && mask[ny * w + nx] && label[ny * w + nx] == 0 {
                    label[ny * w + nx] = id;
                    stack.push(ny * w + nx);
                }
            }
        }
    }
    let at_seed = label[seed.1 * w + seed.0];
    let keep = if at_seed != 0 {
        at_seed
    } else {
        (1..sizes.len()).max_by_key(|&i| sizes[i]).unwrap_or(0)
    };
    label.iter().map(|&l| l != 0 && l == keep).collect()
}

/// Turn pixels not reachable from the border without crossing the region into
/// region pixels, so an outline has no holes.
fn fill_holes(mask: &Mask, w: usize, h: usize) -> Mask {
    let mut outside = vec![false; mask.len()];
    let mut stack: Vec<usize> = Vec::new();
    for x in 0..w {
        stack.push(x);
        stack.push((h - 1) * w + x);
    }
    for y in 0..h {
        stack.push(y * w);
        stack.push(y * w + w - 1);
    }
    while let Some(i) = stack.pop() {
        if mask[i] || outside[i] {
            continue;
        }
        outside[i] = true;
        let (x, y) = (i % w, i / w);
        if x > 0 {
            stack.push(i - 1);
        }
        if x + 1 < w {
            stack.push(i + 1);
        }
        if y > 0 {
            stack.push(i - w);
        }
        if y + 1 < h {
            stack.push(i + w);
        }
    }
    outside.iter().map(|&o| !o).collect()
}

/// The pixel-corner polygon around a hole-free, single-component mask,
/// counter-clockwise on screen: the region's edges chained one after another.
fn boundary(mask: &Mask, w: usize, h: usize) -> Vec<Pt> {
    let inside = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    // Directed edges with the region on their right (screen y down).
    let mut next: std::collections::HashMap<(i64, i64), Vec<(i64, i64)>> = Default::default();
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if !inside(x, y) {
                continue;
            }
            if !inside(x, y - 1) {
                next.entry((x, y)).or_default().push((x + 1, y));
            }
            if !inside(x + 1, y) {
                next.entry((x + 1, y)).or_default().push((x + 1, y + 1));
            }
            if !inside(x, y + 1) {
                next.entry((x + 1, y + 1)).or_default().push((x, y + 1));
            }
            if !inside(x - 1, y) {
                next.entry((x, y + 1)).or_default().push((x, y));
            }
        }
    }
    let Some(&start) = next.keys().min() else {
        return Vec::new();
    };
    let mut out = vec![(start.0 as f64, start.1 as f64)];
    let mut at = start;
    let mut heading = (0i64, 0i64);
    while let Some(options) = next.get_mut(&at) {
        // Where a corner touches diagonally, keep turning the same way round
        // so the trace stays on one outline.
        let pick = options
            .iter()
            .position(|&(nx, ny)| (nx - at.0, ny - at.1) != (-heading.0, -heading.1))
            .unwrap_or(0);
        if options.is_empty() {
            break;
        }
        let to = options.remove(pick);
        heading = (to.0 - at.0, to.1 - at.1);
        at = to;
        if at == start {
            break;
        }
        out.push((at.0 as f64, at.1 as f64));
    }
    out
}

/// Douglas-Peucker on a closed ring: fewer points that stay within `epsilon`
/// pixels of the original.
pub fn simplify_ring(ring: &[Pt], epsilon: f64) -> Vec<Pt> {
    if ring.len() < 4 {
        return ring.to_vec();
    }
    // Split at the point farthest from the first so both halves are open
    // chains.
    let far = (1..ring.len())
        .max_by(|&a, &b| dist(ring[0], ring[a]).total_cmp(&dist(ring[0], ring[b])))
        .unwrap_or(1);
    let mut first: Vec<Pt> = ring[..=far].to_vec();
    let mut second: Vec<Pt> = ring[far..].to_vec();
    second.push(ring[0]);
    let mut out = dp(&first, epsilon);
    out.pop();
    let mut tail = dp(&second, epsilon);
    tail.pop();
    out.extend(tail);
    first.clear();
    out
}

fn dist(a: Pt, b: Pt) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn point_line_distance(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len == 0.0 {
        return dist(p, a);
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len
}

fn dp(chain: &[Pt], epsilon: f64) -> Vec<Pt> {
    if chain.len() < 3 {
        return chain.to_vec();
    }
    let (a, b) = (chain[0], chain[chain.len() - 1]);
    let (idx, d) = (1..chain.len() - 1)
        .map(|i| (i, point_line_distance(chain[i], a, b)))
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap();
    if d <= epsilon {
        return vec![a, b];
    }
    let mut left = dp(&chain[..=idx], epsilon);
    left.pop();
    left.extend(dp(&chain[idx..], epsilon));
    left
}

fn convex_hull(points: &[Pt]) -> Vec<Pt> {
    let mut pts: Vec<Pt> = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Pt, a: Pt, b: Pt| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut hull: Vec<Pt> = Vec::new();
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &Pt>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The smallest rectangle (any rotation) that holds all `points`, as four
/// corners in order.
pub fn min_area_rect(points: &[Pt]) -> Option<[Pt; 4]> {
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f64, [Pt; 4])> = None;
    for i in 0..hull.len() {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let len = dist(a, b);
        if len == 0.0 {
            continue;
        }
        let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let (mut min_u, mut max_u, mut min_v, mut max_v) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &hull {
            let (u, v) = (
                (p.0 - a.0) * ux + (p.1 - a.1) * uy,
                -(p.0 - a.0) * uy + (p.1 - a.1) * ux,
            );
            min_u = min_u.min(u);
            max_u = max_u.max(u);
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        let area = (max_u - min_u) * (max_v - min_v);
        if best.as_ref().is_none_or(|(a0, _)| area < *a0) {
            let at = |u: f64, v: f64| (a.0 + u * ux - v * uy, a.1 + u * uy + v * ux);
            best = Some((
                area,
                [
                    at(min_u, min_v),
                    at(max_u, min_v),
                    at(max_u, max_v),
                    at(min_u, max_v),
                ],
            ));
        }
    }
    best.map(|(_, corners)| corners)
}

/// How much of the raster a region may cover before it is taken to have
/// leaked into the ground around the thing clicked.
const MAX_COVERAGE: f64 = 0.6;
/// Smallest region worth an outline, in pixels.
const MIN_PIXELS: usize = 40;

/// The region around `seed` as a hole-free mask, or `None` when there is
/// nothing sensible there (nothing known, too small, or it took over the
/// picture).
fn region(r: &Raster, seed: (usize, usize), tolerance: f64) -> Option<Mask> {
    if seed.0 >= r.width || seed.1 >= r.height {
        return None;
    }
    let grown = flood(r, seed, tolerance)?;
    let cleaned = open(grown, r.width, r.height, 2);
    let one = component(&cleaned, r.width, r.height, seed);
    let filled = fill_holes(&one, r.width, r.height);
    let size = filled.iter().filter(|&&p| p).count();
    if size < MIN_PIXELS || size as f64 > MAX_COVERAGE * (r.width * r.height) as f64 {
        return None;
    }
    // A region that runs off the picture is cut short, not an outline.
    let touches_edge = (0..r.width).any(|x| filled[x] || filled[(r.height - 1) * r.width + x])
        || (0..r.height).any(|y| filled[y * r.width] || filled[y * r.width + r.width - 1]);
    (!touches_edge).then_some(filled)
}

/// The part of a model's `mask` (`width * height` flags) that holds `seed`,
/// without specks or thin spurs: as it is, and with its holes filled in. `None`
/// if it is too small.
fn mask_region(
    mask: &[bool],
    width: usize,
    height: usize,
    seed: (usize, usize),
) -> Option<(Mask, Mask)> {
    if seed.0 >= width || seed.1 >= height || mask.len() != width * height {
        return None;
    }
    let cleaned = open(mask.to_vec(), width, height, 1);
    let solid = component(&cleaned, width, height, seed);
    let filled = fill_holes(&solid, width, height);
    (filled.iter().filter(|&&p| p).count() >= MIN_PIXELS).then_some((filled, solid))
}

/// A mask proposed by a segmentation model, with the model's confidence in it.
pub struct MaskCandidate<'a> {
    pub mask: &'a [bool],
    pub confidence: f32,
}

/// Candidates the model is not sure of are passed over...
const MIN_CONFIDENCE: f32 = 0.8;
/// ...and so are ones covering more than this share of the picture, which
/// have taken in the ground around the object.
const MAX_SHARE: f64 = 0.5;
/// How well a mask must match its best rectangle (intersection over union) to
/// be taken for a building as it is.
const MIN_RECTANGULARITY: f64 = 0.8;

/// A pixel this much darker than the lit part of an object is taken to be
/// shadow (of itself or of something nearby), not the object.
const SHADOW_RATIO: f64 = 0.35;

fn luminance([r, g, b]: [u8; 3]) -> f64 {
    0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)
}

/// `mask` without its shadowed pixels and its lawn: those darker than `SHADOW_RATIO` of the
/// brightness of the lit part of the masked area (its 75th percentile).
/// Aerial imagery is lit from one side, so a mask that has run onto the dark
/// strip beside a building is trimmed back to the building.
fn without_shadow_or_lawn(mask: &[bool], picture: &Raster) -> Vec<bool> {
    let mut histogram = [0usize; 256];
    for (flag, px) in mask.iter().zip(&picture.pixels) {
        if let (true, Some(rgb)) = (flag, px) {
            histogram[luminance(*rgb) as usize] += 1;
        }
    }
    let total: usize = histogram.iter().sum();
    let (mut seen, mut lit) = (0usize, 0usize);
    for (level, count) in histogram.iter().enumerate() {
        seen += count;
        if seen * 4 >= total * 3 {
            lit = level;
            break;
        }
    }
    let floor = SHADOW_RATIO * lit as f64;
    mask.iter()
        .zip(&picture.pixels)
        .map(|(&flag, px)| {
            flag && px.is_none_or(|rgb| luminance(rgb) >= floor && !looks_like_lawn(rgb))
        })
        .collect()
}

/// Whether a pixel is grass: green well above blue, and at least as green as
/// it is red. Roofs are grey, tan or red-brown (more red than green) or blue-
/// grey (more blue than green), so a courtyard's lawn is told apart from the
/// roofs around it, while a gravel path, being roof-coloured, is not.
fn looks_like_lawn([r, g, b]: [u8; 3]) -> bool {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    g - b > 22 && g >= r - 2
}

/// A candidate that is usable: its region with holes filled, the region as it
/// is (holes, lawn and shadow left out) and the filled region's size.
struct Usable {
    filled: Mask,
    solid: Mask,
    size: usize,
    confidence: f32,
}

/// The usable candidates, biggest first; shadow and lawn are trimmed from each
/// first if `trim_shadow`.
fn usable(
    candidates: &[MaskCandidate<'_>],
    picture: &Raster,
    trim_shadow: bool,
    seed: (usize, usize),
) -> Vec<Usable> {
    let (width, height) = (picture.width, picture.height);
    let mut out: Vec<Usable> = candidates
        .iter()
        .filter(|c| c.confidence >= MIN_CONFIDENCE)
        .filter_map(|c| {
            let confidence = c.confidence;
            let trimmed;
            let mask = if trim_shadow {
                trimmed = without_shadow_or_lawn(c.mask, picture);
                &trimmed[..]
            } else {
                c.mask
            };
            mask_region(mask, width, height, seed).map(|(f, s)| (f, s, confidence))
        })
        .map(|(filled, solid, confidence)| {
            let size = filled.iter().filter(|&&p| p).count();
            Usable {
                filled,
                solid,
                size,
                confidence,
            }
        })
        .filter(|u| (u.size as f64) <= MAX_SHARE * (width * height) as f64)
        .collect();
    // The biggest candidates are tried first, but among those nearly as big
    // as the biggest the one the model is surest of comes first: a bigger,
    // shakier mask is usually the same building with some of the lot on it.
    let biggest = out.iter().map(|u| u.size).max().unwrap_or(0);
    let near = |u: &Usable| u.size as f64 >= NEAR_BIGGEST * biggest as f64;
    out.sort_by(|a, b| {
        near(b)
            .cmp(&near(a))
            .then_with(|| {
                if near(a) && near(b) {
                    b.confidence.total_cmp(&a.confidence)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then(b.size.cmp(&a.size))
    });
    out
}

/// A candidate this big, relative to the biggest, is compared with it on the
/// model's confidence rather than on size alone.
const NEAR_BIGGEST: f64 = 0.7;

/// Edges shorter than this, in pixels, are small jogs that are straightened
/// out of a building outline; the limit grows with the building, as a share
/// of its linear size, and is never below `MIN_EDGE_FLOOR`.
const MIN_EDGE_SHARE: f64 = 0.07;
const MIN_EDGE_FLOOR: f64 = 8.0;

/// How closely the plain rectangle must match for the outline to stay a
/// rectangle; below it a squared-off polygon is tried.
const RECTANGLE_GOOD_ENOUGH: f64 = 0.88;

/// A building outline with more corners than this is a trace of detail, not a
/// plan; the outline is simplified further, or left a rectangle.
const MAX_CORNERS: usize = 12;

/// A polygon has to match this much better than the rectangle to be used: a
/// small notch or a porch is not worth an outline of its own.
const POLYGON_GAIN: f64 = 0.06;

fn rotate(p: Pt, (sin, cos): (f64, f64)) -> Pt {
    (p.0 * cos - p.1 * sin, p.0 * sin + p.1 * cos)
}

/// Whether `p` is inside `ring` (even-odd).
fn contains(ring: &[Pt], p: Pt) -> bool {
    let n = ring.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

fn polygon_area(ring: &[Pt]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1)
        .sum::<f64>()
        .abs()
        / 2.0
}

/// The outline of `region` with every corner a right angle, turned to line up
/// with `angle` (radians): a U- or L-shaped building comes out as such. The
/// pixel boundary is simplified, each edge is snapped to one of the two
/// axes, runs of edges on the same axis are merged, small jogs are dropped,
/// and the corners are where consecutive edges meet.
fn squared_outline(
    region: &Mask,
    width: usize,
    height: usize,
    angle: f64,
    coarseness: f64,
) -> Option<Vec<Pt>> {
    let min_edge = (MIN_EDGE_SHARE * (region.iter().filter(|&&p| p).count() as f64).sqrt())
        .max(MIN_EDGE_FLOOR)
        * coarseness;
    let (sin, cos) = angle.sin_cos();
    let forward = (-sin, cos);
    let back = (sin, cos);
    let ring: Vec<Pt> = boundary(region, width, height)
        .into_iter()
        .map(|p| rotate(p, forward))
        .collect();
    let ring = simplify_ring(&ring, 2.5);
    if ring.len() < 4 {
        return None;
    }
    // (is_horizontal, the fixed coordinate, length)
    let mut edges: Vec<(bool, f64, f64)> = Vec::new();
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
        let horizontal = dx >= dy;
        let coord = if horizontal {
            (a.1 + b.1) / 2.0
        } else {
            (a.0 + b.0) / 2.0
        };
        edges.push((horizontal, coord, dx.max(dy).max(1e-9)));
    }
    // Merge neighbours on the same axis (also across the ring's seam) and
    // drop short jogs, until nothing changes.
    let merge = |edges: &mut Vec<(bool, f64, f64)>| {
        let mut i = 0;
        while edges.len() > 1 && i < edges.len() {
            let j = (i + 1) % edges.len();
            if i != j && edges[i].0 == edges[j].0 {
                let (h, ci, li) = edges[i];
                let (_, cj, lj) = edges[j];
                edges[i] = (h, (ci * li + cj * lj) / (li + lj), li + lj);
                edges.remove(j);
                if j < i {
                    i -= 1;
                }
            } else {
                i += 1;
            }
        }
    };
    merge(&mut edges);
    // A jog is an edge whose real length, the gap between its two neighbours'
    // positions, is small. Dropping it joins those neighbours, which lie on
    // the same axis, into one edge.
    loop {
        let n = edges.len();
        if n <= 4 {
            break;
        }
        let real = |k: usize| (edges[(k + 1) % n].1 - edges[(k + n - 1) % n].1).abs();
        let Some(short) = (0..n)
            .filter(|&k| real(k) < min_edge)
            .min_by(|&a, &b| real(a).total_cmp(&real(b)))
        else {
            break;
        };
        edges.remove(short);
        merge(&mut edges);
    }
    if edges.len() < 4 || !edges.len().is_multiple_of(2) {
        return None;
    }
    let n = edges.len();
    let corners: Vec<Pt> = (0..n)
        .map(|i| {
            let (h1, c1, _) = edges[i];
            let (_, c2, _) = edges[(i + 1) % n];
            let p = if h1 { (c2, c1) } else { (c1, c2) };
            rotate(p, back)
        })
        .collect();
    Some(corners)
}

/// How well `outline` matches `region` (intersection over union).
fn outline_fit(region: &Mask, width: usize, outline: &[Pt]) -> f64 {
    let inside = region
        .iter()
        .enumerate()
        .filter(|(i, &on)| {
            on && contains(
                outline,
                ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5),
            )
        })
        .count() as f64;
    let total = region.iter().filter(|&&p| p).count() as f64;
    inside / (polygon_area(outline) + total - inside).max(1.0)
}

/// Cut-off parts smaller than this share of a building are never wings.
const MIN_WING_SHARE: f64 = 0.08;

/// The `share` quantile (0 to 1) of the luminance of the known pixels of
/// `picture` under `mask`.
fn luminance_quantile(mask: &Mask, picture: &Raster, share: f64) -> f64 {
    let mut levels: Vec<f64> = mask
        .iter()
        .zip(&picture.pixels)
        .filter_map(|(&on, px)| px.filter(|_| on).map(luminance))
        .collect();
    levels.sort_by(f64::total_cmp);
    levels
        .get(((levels.len().max(1) - 1) as f64 * share) as usize)
        .copied()
        .unwrap_or(0.0)
}

/// Whether the part `appendage` that sticks out of `body` is more of the
/// building (the wing of an L) rather than a driveway or patio. Concrete is
/// brighter than a roof; a wing is as bright as the body's own lit slopes,
/// so a part whose typical pixel is brighter than nearly all of the body is
/// left out.
fn roof_like(appendage: &Mask, body: &Mask, picture: &Raster) -> bool {
    luminance_quantile(appendage, picture, 0.5) <= luminance_quantile(body, picture, 0.95)
}

/// See `trim_rectangle`.
const EMPTY_PENALTY: f64 = 1.5;

/// Pull in the sides of `rect` (a rectangle around all of `region`) while that
/// makes it match the region better, so a driveway or a porch sticking out of
/// a house is left outside instead of stretching the rectangle over it.
/// Returns the rectangle and how well it matches (intersection over union).
fn trim_rectangle(region: &Mask, width: usize, rect: [Pt; 4]) -> ([Pt; 4], f64) {
    let origin = rect[0];
    let along = |a: Pt, b: Pt| {
        let len = dist(a, b).max(1e-9);
        ((b.0 - a.0) / len, (b.1 - a.1) / len)
    };
    let (ux, uy) = along(rect[0], rect[1]);
    let (vx, vy) = along(rect[0], rect[3]);
    let points: Vec<(f64, f64)> = region
        .iter()
        .enumerate()
        .filter(|(_, &on)| on)
        .map(|(i, _)| {
            let (x, y) = (
                (i % width) as f64 + 0.5 - origin.0,
                (i / width) as f64 + 0.5 - origin.1,
            );
            (x * ux + y * uy, x * vx + y * vy)
        })
        .collect();
    let mut bounds = [0.0, dist(rect[0], rect[1]), 0.0, dist(rect[0], rect[3])];
    // How many region pixels a box holds, and how big it is.
    let measure = |b: &[f64; 4]| {
        let inside = points
            .iter()
            .filter(|p| p.0 >= b[0] && p.0 <= b[1] && p.1 >= b[2] && p.1 <= b[3])
            .count() as f64;
        (inside, (b[1] - b[0]).max(0.0) * (b[3] - b[2]).max(0.0))
    };
    // What the trimming maximises: pixels held, less a penalty for each
    // empty pixel of the box. An outer row or column therefore stays only
    // if it is more than 60% region, which is what cuts off a driveway (a
    // plain overlap ratio is too tolerant of a thin strip to bother).
    let score = |b: &[f64; 4]| {
        let (inside, area) = measure(b);
        inside - EMPTY_PENALTY * (area - inside)
    };
    let mut best = score(&bounds);
    // Steepest ascent: each round takes the one-pixel step, over the four
    // sides, that gains most, so a driveway's rows go before the columns of
    // the house beside it are even considered.
    for _ in 0..800 {
        let mut step: Option<(f64, [f64; 4])> = None;
        for side in 0..4 {
            let mut trial = bounds;
            trial[side] += if side % 2 == 0 { 1.0 } else { -1.0 };
            let s = score(&trial);
            if s > best + 1e-9 && step.as_ref().is_none_or(|(top, _)| s > *top) {
                step = Some((s, trial));
            }
        }
        let Some((s, trial)) = step else { break };
        bounds = trial;
        best = s;
    }
    let at = |u: f64, v: f64| (origin.0 + u * ux + v * vx, origin.1 + u * uy + v * vy);
    let (inside, area) = measure(&bounds);
    let fit = inside / (area + points.len() as f64 - inside).max(1.0);
    (
        [
            at(bounds[0], bounds[2]),
            at(bounds[1], bounds[2]),
            at(bounds[1], bounds[3]),
            at(bounds[0], bounds[3]),
        ],
        fit,
    )
}

/// Whether the part `mask` that sticks out of a building, with `at` in it, is
/// ground (a parking lot, a yard) rather than more building: asked about `at`,
/// the model answers with a mask that takes in most of the part and runs far
/// outside the building and is many times its size, as a lot does and a wing
/// of the roof does not.
fn is_ground(
    mask: &Mask,
    at: Pt,
    region: &Mask,
    probe: &mut dyn FnMut(Pt) -> Vec<OwnedCandidate>,
) -> bool {
    let size = mask.iter().filter(|&&p| p).count();
    // What the model is surest the thing at `at` is. A lot gives a mask of
    // the whole lot; a roof, even one the lot runs up to, gives a mask of the
    // roof.
    let answers = probe(at);
    let Some(best) = answers
        .iter()
        .filter(|c| c.mask.len() == mask.len())
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
    else {
        return false;
    };
    if best.confidence < MIN_CONFIDENCE {
        return false;
    }
    let covered = best.mask.iter().zip(mask).filter(|(&m, &p)| m && p).count();
    let beyond = best
        .mask
        .iter()
        .zip(region)
        .filter(|(&m, &r)| m && !r)
        .count();
    let whole = best.mask.iter().filter(|&&m| m).count();
    covered as f64 >= 0.7 * size as f64 && whole >= 6 * size && beyond >= 4 * size
}

/// A candidate that owns its mask, as a probe of the model returns it.
pub struct OwnedCandidate {
    pub mask: Vec<bool>,
    pub confidence: f32,
}

/// How much the brightness varies, over a 5 x 5 window, around each pixel of
/// `picture` (standard deviation; unknown pixels count as black). A roof is
/// smooth; a courtyard of lawn, gravel and shrubs is not.
fn local_contrast(picture: &Raster) -> Vec<f64> {
    let (w, h) = (picture.width, picture.height);
    // Integral images of brightness and its square, one row and column
    // bigger than the picture.
    let stride = w + 1;
    let mut sum = vec![0.0f64; stride * (h + 1)];
    let mut square = vec![0.0f64; stride * (h + 1)];
    for y in 0..h {
        let (mut row, mut row_sq) = (0.0, 0.0);
        for x in 0..w {
            let l = picture.at(x, y).map_or(0.0, luminance);
            row += l;
            row_sq += l * l;
            sum[(y + 1) * stride + x + 1] = sum[y * stride + x + 1] + row;
            square[(y + 1) * stride + x + 1] = square[y * stride + x + 1] + row_sq;
        }
    }
    let window = |table: &[f64], x0: usize, y0: usize, x1: usize, y1: usize| {
        table[y1 * stride + x1] - table[y0 * stride + x1] - table[y1 * stride + x0]
            + table[y0 * stride + x0]
    };
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, y0) = (x.saturating_sub(2), y.saturating_sub(2));
            let (x1, y1) = ((x + 3).min(w), (y + 3).min(h));
            let n = ((x1 - x0) * (y1 - y0)) as f64;
            let mean = window(&sum, x0, y0, x1, y1) / n;
            let var = window(&square, x0, y0, x1, y1) / n - mean * mean;
            out[y * w + x] = var.max(0.0).sqrt();
        }
    }
    out
}

/// The median local contrast over the pixels of `mask`, ignoring its rim.
fn median_contrast(mask: &[bool], contrast: &[f64]) -> f64 {
    let mut values: Vec<f64> = mask
        .iter()
        .zip(contrast)
        .filter(|(&on, _)| on)
        .map(|(_, &c)| c)
        .collect();
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// The share of the pixels under `mask` that look like grass.
fn lawn_share(mask: &[bool], picture: &Raster) -> f64 {
    let (mut lawn, mut all) = (0usize, 0usize);
    for (&on, px) in mask.iter().zip(&picture.pixels) {
        if let (true, Some(rgb)) = (on, px) {
            all += 1;
            lawn += usize::from(looks_like_lawn(*rgb));
        }
    }
    lawn as f64 / all.max(1) as f64
}

/// How far, in colour, a courtyard's typical pixel must be from the rest of
/// the building's (0 to 441, the distance between two RGB colours).
const COURTYARD_COLOUR_GAP: f64 = 28.0;

/// The distance between the median colour under `part` and the median colour
/// of the rest of `region`.
fn colour_gap(part: &[bool], region: &Mask, picture: &Raster) -> f64 {
    let median = |select: &dyn Fn(usize) -> bool| -> Option<[f64; 3]> {
        let mut channels: [Vec<u8>; 3] = Default::default();
        for (i, px) in picture.pixels.iter().enumerate() {
            if let (true, Some(rgb)) = (select(i), px) {
                for (c, v) in channels.iter_mut().zip(rgb) {
                    c.push(*v);
                }
            }
        }
        if channels[0].is_empty() {
            return None;
        }
        Some(channels.map(|mut c| {
            c.sort_unstable();
            f64::from(c[c.len() / 2])
        }))
    };
    let (Some(a), Some(b)) = (
        median(&|i| part[i] && region[i]),
        median(&|i| region[i] && !part[i]),
    ) else {
        return 0.0;
    };
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Local contrast above which a patch is taken to be ground rather than roof,
/// and the typical contrast a courtyard's mask must show to be cut out.
const BUSY_CONTRAST: f64 = 8.0;

/// The connected pieces of `mask`, each as the indices of its pixels.
fn components_of(mask: &Mask, width: usize, height: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut members = Vec::new();
        while let Some(i) = stack.pop() {
            members.push(i);
            let (x, y) = (i % width, i / width);
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx < width && ny < height && mask[ny * width + nx] && !seen[ny * width + nx] {
                    seen[ny * width + nx] = true;
                    stack.push(ny * width + nx);
                }
            }
        }
        out.push(members);
    }
    out
}

/// The member of `members` nearest their centre, in picture pixels.
fn middle_of(members: &[usize], width: usize) -> Pt {
    let n = members.len().max(1) as f64;
    let cx = members.iter().map(|i| (i % width) as f64).sum::<f64>() / n;
    let cy = members.iter().map(|i| (i / width) as f64).sum::<f64>() / n;
    let at = members
        .iter()
        .min_by(|&&a, &&b| {
            let d = |i: usize| ((i % width) as f64 - cx).hypot((i / width) as f64 - cy);
            d(a).total_cmp(&d(b))
        })
        .copied()
        .unwrap_or(0);
    ((at % width) as f64 + 0.5, (at / width) as f64 + 0.5)
}

/// The connected pieces of `mask` with at least `min` pixels, each with its
/// size and the member nearest its centre, in picture pixels.
fn pieces(mask: &Mask, width: usize, height: usize, min: usize) -> Vec<(usize, Pt)> {
    components_of(mask, width, height)
        .into_iter()
        .filter(|m| m.len() >= min)
        .map(|m| (m.len(), middle_of(&m, width)))
        .collect()
}

/// Courtyards: parts of a region that are not roof. A courtyard is suspected
/// where the region holds a patch the trimming took out (lawn, shadow) or a
/// patch that is busy rather than smooth (grass, gravel, shrubs). Each suspect
/// is put to the model: clicking in it returns a mask of just the courtyard,
/// which is cut out of `region` when it is as busy as ground and not smooth
/// like a roof. A building around a courtyard then comes out as a U or a ring
/// of wings rather than a solid block. Only a few suspects are probed, biggest
/// first.
fn cut_out_courtyards(
    filled: Mask,
    solid: &Mask,
    picture: &Raster,
    seed: (usize, usize),
    keep: &[Pt],
    probe: &mut dyn FnMut(Pt) -> Vec<OwnedCandidate>,
) -> Mask {
    const MAX_PROBES: usize = 3;
    // A courtyard is a real part of a building's plan; a small light patch
    // on a roof (a skylight, a patio) is not.
    const MIN_CUTOUT_SHARE: f64 = 0.06;
    let (width, height) = (picture.width, picture.height);
    let total = filled.iter().filter(|&&p| p).count();
    let min_piece = ((MIN_CUTOUT_SHARE * total as f64) as usize).max(100);
    let contrast = local_contrast(picture);
    // Pixels inside the region that are not part of it as it stands.
    let gaps: Mask = filled.iter().zip(solid).map(|(&f, &s)| f && !s).collect();
    // Busy pixels inside the region, joined up and cleared of the thin lines
    // that roof edges and ridges make.
    let busy: Mask = filled
        .iter()
        .zip(&contrast)
        .map(|(&f, &c)| f && c >= BUSY_CONTRAST)
        .collect();
    let closed = erode(&dilate(&busy, width, height), width, height);
    let busy = open(closed, width, height, 3);
    let mut suspects = pieces(&gaps, width, height, min_piece);
    suspects.extend(pieces(&busy, width, height, min_piece));
    suspects.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    let mut region = filled;
    let mut cut = false;
    for (_, point) in suspects.into_iter().take(MAX_PROBES) {
        // The biggest confident mask of the patch that is mostly within the
        // region and not most of it.
        let cutout = probe(point)
            .into_iter()
            .filter(|c| c.confidence >= MIN_CONFIDENCE && c.mask.len() == region.len())
            .filter(|c| {
                let size = c.mask.iter().filter(|&&p| p).count();
                let inside = c.mask.iter().zip(&region).filter(|(&m, &r)| m && r).count();
                size >= min_piece
                    && (size as f64) <= 0.4 * total as f64
                    && inside as f64 >= 0.7 * size as f64
                    && (median_contrast(&c.mask, &contrast) >= BUSY_CONTRAST
                        || lawn_share(&c.mask, picture) >= 0.4)
                    // A courtyard does not look like the roof around it; the
                    // other half of a roof that is rough all over does.
                    && colour_gap(&c.mask, &region, picture) >= COURTYARD_COLOUR_GAP
                    // Nothing the user marked as part of the building, or
                    // clicked on, is a courtyard.
                    && !keep.iter().chain(std::iter::once(&(seed.0 as f64, seed.1 as f64))).any(|k| {
                        let (x, y) = (k.0 as usize, k.1 as usize);
                        x < width && y < height && c.mask[y * width + x]
                    })
            })
            .max_by_key(|c| c.mask.iter().filter(|&&p| p).count());
        if let Some(cutout) = cutout {
            cut = true;
            for (r, &m) in region.iter_mut().zip(&cutout.mask) {
                if m {
                    *r = false;
                }
            }
        }
    }
    // Once a courtyard is out, the wall or strip of lawn that closed it off
    // from the outside is a thin bridge between the wings: opening by more
    // than the usual specks takes it out and leaves the U open.
    let radius = if cut {
        ((0.03 * (total as f64).sqrt()) as usize).clamp(3, COURTYARD_OPEN)
    } else {
        1
    };
    let opened = open(region, width, height, radius);
    component(&opened, width, height, seed)
}

/// How far a region is opened after a courtyard is cut out of it, in pixels.
const COURTYARD_OPEN: usize = 10;

/// The outline of a building among a model's candidate masks over `picture`:
/// the biggest candidate that an outline matches well, so a whole roof is
/// preferred to one of its slopes but not to a mask that has bled into the
/// yard. Each candidate has its shadow and lawn trimmed off first. The outline
/// is a rectangle (four points) where that fits, or else a polygon with right
/// angles, such as a U for a building around a courtyard.
pub fn building_from_candidates(
    candidates: &[MaskCandidate<'_>],
    picture: &Raster,
    seed: (usize, usize),
    keep: &[Pt],
    probe: &mut dyn FnMut(Pt) -> Vec<OwnedCandidate>,
) -> Option<Vec<Pt>> {
    let (width, height) = (picture.width, picture.height);
    let mut best: Option<(f64, Vec<Pt>)> = None;
    for Usable { filled, solid, .. } in usable(candidates, picture, true, seed) {
        let region = cut_out_courtyards(filled, &solid, picture, seed, keep, probe);
        let Some(base) = min_area_rect(&boundary(&region, width, height)) else {
            continue;
        };
        let (rect, _) = trim_rectangle(&region, width, base);
        // What is left of the region inside the trimmed rectangle: a driveway
        // or porch sticking out is not part of the building, but a courtyard
        // cut into it is.
        let clipped: Mask = region
            .iter()
            .enumerate()
            .map(|(i, &on)| {
                on && contains(&rect, ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5))
            })
            .collect();
        // Something left outside the rectangle that stands up from the ground
        // is a wing of the building (an L), not a driveway: keep it.
        let outside: Mask = region
            .iter()
            .zip(&clipped)
            .map(|(&r, &c)| r && !c)
            .collect();
        let region_size = region.iter().filter(|&&p| p).count();
        let mut clipped = clipped;
        for part in components_of(&outside, width, height) {
            if (part.len() as f64) < MIN_WING_SHARE * region_size as f64 {
                continue;
            }
            let mut mask: Mask = vec![false; region.len()];
            for &i in &part {
                mask[i] = true;
            }
            if roof_like(&mask, &clipped, picture)
                && !is_ground(&mask, middle_of(&part, width), &region, probe)
            {
                for &i in &part {
                    clipped[i] = true;
                }
            }
        }
        let kept = clipped.iter().filter(|&&p| p).count();
        let coverage = kept as f64 / region.iter().filter(|&&p| p).count().max(1) as f64;
        let rect_fit = outline_fit(&clipped, width, &rect);
        let mut outline = (rect_fit, rect.to_vec());
        if rect_fit < RECTANGLE_GOOD_ENOUGH {
            let angle = (base[1].1 - base[0].1).atan2(base[1].0 - base[0].0);
            // Straighten out ever more detail until the outline is simple
            // enough to be a building's: a handful of corners, not a
            // trace of every step and porch.
            for coarseness in [1.0, 1.7, 2.8, 4.5] {
                let Some(poly) = squared_outline(&clipped, width, height, angle, coarseness) else {
                    continue;
                };
                if poly.len() > MAX_CORNERS {
                    continue;
                }
                let fit = outline_fit(&clipped, width, &poly);
                if fit > rect_fit + POLYGON_GAIN {
                    outline = (fit, poly);
                }
                break;
            }
        }
        // A region that was mostly cut away is not what the outline shows.
        let score = outline.0 * coverage;
        if score >= MIN_RECTANGULARITY {
            return Some(outline.1);
        }
        if best.as_ref().is_none_or(|(f, _)| score > *f) {
            best = Some((score, outline.1));
        }
    }
    best.map(|(_, o)| o)
}

/// The simplified outline of the biggest usable candidate mask: the whole
/// stand of trees rather than one crown.
pub fn area_from_candidates(
    candidates: &[MaskCandidate<'_>],
    picture: &Raster,
    seed: (usize, usize),
) -> Option<Vec<Pt>> {
    let region = usable(candidates, picture, false, seed)
        .into_iter()
        .next()?
        .filled;
    let ring = simplify_ring(&boundary(&region, picture.width, picture.height), 2.0);
    (ring.len() >= 3).then_some(ring)
}

/// The best-fitting rectangle around the region of similar colour at `seed`,
/// for a building seen from above.
pub fn detect_rectangle(r: &Raster, seed: (usize, usize), tolerance: f64) -> Option<[Pt; 4]> {
    let mask = region(r, seed, tolerance)?;
    min_area_rect(&boundary(&mask, r.width, r.height))
}

/// The simplified outline of the region of similar colour at `seed`, for an
/// area such as trees.
pub fn detect_area(r: &Raster, seed: (usize, usize), tolerance: f64) -> Option<Vec<Pt>> {
    let mask = region(r, seed, tolerance)?;
    let ring = simplify_ring(&boundary(&mask, r.width, r.height), 2.0);
    (ring.len() >= 3).then_some(ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUND: [u8; 3] = [120, 110, 90];
    const ROOF: [u8; 3] = [190, 60, 50];

    fn picture(w: usize, h: usize, paint: impl Fn(f64, f64) -> Option<[u8; 3]>) -> Raster {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                r.set(
                    x,
                    y,
                    paint(x as f64 + 0.5, y as f64 + 0.5).unwrap_or(GROUND),
                );
            }
        }
        r
    }

    fn area(poly: &[Pt]) -> f64 {
        let n = poly.len();
        (0..n)
            .map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1)
            .sum::<f64>()
            .abs()
            / 2.0
    }

    #[test]
    fn a_tilted_roof_is_fitted_with_a_matching_rectangle() {
        // A 60x30 roof turned 25 degrees about (100, 100).
        let (sin, cos) = 25f64.to_radians().sin_cos();
        let r = picture(200, 200, |x, y| {
            let (dx, dy) = (x - 100.0, y - 100.0);
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            (u.abs() <= 30.0 && v.abs() <= 15.0).then_some(ROOF)
        });
        let rect = detect_rectangle(&r, (100, 100), 30.0).expect("a rectangle");
        assert!((area(&rect) - 1800.0).abs() < 180.0, "area {}", area(&rect));
        let side = |a: Pt, b: Pt| dist(a, b);
        let (s1, s2) = (side(rect[0], rect[1]), side(rect[1], rect[2]));
        let (long, short) = (s1.max(s2), s1.min(s2));
        assert!((long - 60.0).abs() < 3.0, "long side {long}");
        assert!((short - 30.0).abs() < 3.0, "short side {short}");
    }

    #[test]
    fn a_blobby_area_keeps_its_shape_with_few_points() {
        let r = picture(200, 200, |x, y| {
            ((x - 100.0).powi(2) + (y - 100.0).powi(2) < 45.0 * 45.0).then_some([40, 110, 40])
        });
        let ring = detect_area(&r, (100, 100), 30.0).expect("an outline");
        let circle = std::f64::consts::PI * 45.0 * 45.0;
        assert!(
            (area(&ring) - circle).abs() / circle < 0.05,
            "area {}",
            area(&ring)
        );
        assert!(ring.len() < 60, "{} points", ring.len());
    }

    #[test]
    fn a_thin_road_leaking_from_the_roof_is_cut_off() {
        let r = picture(200, 200, |x, y| {
            let roof = (70.0..130.0).contains(&x) && (70.0..130.0).contains(&y);
            let path = (129.0..199.0).contains(&x) && (99.0..101.0).contains(&y);
            (roof || path).then_some(ROOF)
        });
        let rect = detect_rectangle(&r, (100, 100), 30.0).expect("a rectangle");
        let (xs, ys): (Vec<f64>, Vec<f64>) = rect.iter().cloned().unzip();
        let span = |v: &[f64]| {
            v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min)
        };
        assert!(span(&xs) < 70.0 && span(&ys) < 70.0, "{rect:?}");
    }

    /// A `w` x `w` picture of one grey level.
    fn flat(w: usize, level: u8) -> Raster {
        let mut r = Raster::new(w, w);
        for y in 0..w {
            for x in 0..w {
                r.set(x, y, [level; 3]);
            }
        }
        r
    }

    #[test]
    fn a_mask_that_took_in_the_shadow_is_trimmed_back_to_the_roof() {
        let w = 200;
        let mut picture = flat(w, 120);
        // A lit roof at x 60..140, y 60..110, and a dark strip of shadow
        // along its left side that the mask has swallowed.
        for y in 60..110 {
            for x in 60..140 {
                picture.set(x, y, [200, 200, 200]);
            }
            for x in 40..60 {
                picture.set(x, y, [30, 30, 30]);
            }
        }
        let with_shadow = rect_mask(w, 40..140, 60..110);
        let candidates = [MaskCandidate {
            mask: &with_shadow,
            confidence: 0.9,
        }];
        let rect =
            building_from_candidates(&candidates, &picture, (100, 80), &[], &mut |_| Vec::new())
                .unwrap();
        assert!((area(&rect) - 80.0 * 50.0).abs() < 100.0, "{}", area(&rect));
    }

    fn rect_mask(w: usize, x: std::ops::Range<usize>, y: std::ops::Range<usize>) -> Vec<bool> {
        (0..w * w)
            .map(|i| x.contains(&(i % w)) && y.contains(&(i / w)))
            .collect()
    }

    #[test]
    fn the_whole_roof_is_preferred_to_a_slope_of_it() {
        let w = 200;
        let slope = rect_mask(w, 60..100, 60..110);
        let roof = rect_mask(w, 60..140, 60..110);
        let candidates = [
            MaskCandidate {
                mask: &slope,
                confidence: 0.95,
            },
            MaskCandidate {
                mask: &roof,
                confidence: 0.9,
            },
        ];
        let mut picture = flat(w, 120);
        for y in 80..90 {
            for x in 0..60 {
                picture.set(x, y, [245; 3]);
            }
        }
        let rect =
            building_from_candidates(&candidates, &picture, (80, 80), &[], &mut |_| Vec::new())
                .unwrap();
        assert!((area(&rect) - 80.0 * 50.0).abs() < 100.0, "{}", area(&rect));
    }

    #[test]
    fn a_parking_lot_that_the_mask_ran_onto_is_left_out_even_when_it_looks_like_roof() {
        let w = 260;
        // A 100 x 60 block with a 50 x 30 piece of lot beside it, the same grey.
        let mut mask = rect_mask(w, 60..160, 60..120);
        for (i, flag) in mask.iter_mut().enumerate() {
            if (160..210).contains(&(i % w)) && (75..105).contains(&(i / w)) {
                *flag = true;
            }
        }
        let candidates = [MaskCandidate {
            mask: &mask,
            confidence: 0.95,
        }];
        // Asked about the lot, the model answers with the whole lot, which
        // runs far beyond the building.
        let lot = rect_mask(w, 160..260, 0..260);
        let mut probe = |_: Pt| {
            vec![OwnedCandidate {
                mask: lot.clone(),
                confidence: 0.9,
            }]
        };
        let outline =
            building_from_candidates(&candidates, &flat(w, 120), (100, 90), &[], &mut probe)
                .unwrap();
        assert_eq!(outline.len(), 4, "{outline:?}");
        assert!(
            (area(&outline) - 100.0 * 60.0).abs() < 300.0,
            "{}",
            area(&outline)
        );
    }

    #[test]
    fn a_wing_as_bright_as_the_roof_is_kept_as_an_l() {
        let w = 240;
        // An L: a 100 x 60 block with a 50 x 60 wing below its left end, all
        // the same roof.
        let mut mask = rect_mask(w, 60..160, 60..120);
        for (i, flag) in mask.iter_mut().enumerate() {
            if (60..110).contains(&(i % w)) && (120..180).contains(&(i / w)) {
                *flag = true;
            }
        }
        let candidates = [MaskCandidate {
            mask: &mask,
            confidence: 0.95,
        }];
        let outline =
            building_from_candidates(&candidates, &flat(w, 120), (100, 90), &[], &mut |_| {
                Vec::new()
            })
            .unwrap();
        assert_eq!(outline.len(), 6, "{outline:?}");
        assert!(
            (area(&outline) - (100.0 * 60.0 + 50.0 * 60.0)).abs() < 300.0,
            "{}",
            area(&outline)
        );
    }

    #[test]
    fn a_driveway_stuck_to_the_house_is_left_out_of_the_rectangle() {
        let w = 200;
        // A 100 x 60 house with a 30-wide driveway running 50 pixels off it.
        let mut mask = rect_mask(w, 50..150, 40..100);
        for (i, flag) in mask.iter_mut().enumerate() {
            if (70..100).contains(&(i % w)) && (100..150).contains(&(i / w)) {
                *flag = true;
            }
        }
        let candidates = [MaskCandidate {
            mask: &mask,
            confidence: 0.95,
        }];
        // The house is mid-grey, the concrete drive much brighter.
        let mut picture = flat(w, 120);
        for y in 100..150 {
            for x in 70..100 {
                picture.set(x, y, [245; 3]);
            }
        }
        let rect =
            building_from_candidates(&candidates, &picture, (100, 70), &[], &mut |_| Vec::new())
                .unwrap();
        assert!(
            (area(&rect) - 100.0 * 60.0).abs() < 300.0,
            "{}",
            area(&rect)
        );
    }

    #[test]
    fn a_building_around_a_courtyard_comes_out_u_shaped() {
        let w = 240;
        let mut picture = flat(w, 150);
        // A U: two wings and a base, with a lawn-coloured courtyard between
        // the wings that the mask (like a model's) has filled in.
        let roof = [150u8, 150, 140];
        let lawn = [150u8, 175, 120];
        for y in 40..200 {
            for x in 40..200 {
                let court = (100..140).contains(&x) && (40..140).contains(&y);
                picture.set(x, y, if court { lawn } else { roof });
            }
        }
        let filled = rect_mask(w, 40..200, 40..200);
        let candidates = [MaskCandidate {
            mask: &filled,
            confidence: 0.9,
        }];
        let outline =
            building_from_candidates(&candidates, &picture, (60, 100), &[], &mut |_| Vec::new())
                .unwrap();
        assert_eq!(outline.len(), 8, "{outline:?}");
        let expected = 160.0 * 160.0 - 40.0 * 100.0;
        assert!(
            (area(&outline) - expected).abs() < 600.0,
            "{}",
            area(&outline)
        );
    }

    #[test]
    fn a_courtyard_closed_off_by_a_thin_wall_is_cut_out_and_left_open() {
        let w = 260;
        let mut picture = flat(w, 150);
        let lawn = [150u8, 175, 120];
        // A roof 160 square; a lawn courtyard in its middle that reaches
        // to within 6 pixels of the top edge, so a thin strip of "building"
        // closes it off.
        for y in 40..200 {
            for x in 40..200 {
                let court = (100..140).contains(&x) && (46..140).contains(&y);
                picture.set(x, y, if court { lawn } else { [150, 150, 140] });
            }
        }
        let whole = rect_mask(w, 40..200, 40..200);
        let candidates = [MaskCandidate {
            mask: &whole,
            confidence: 0.9,
        }];
        // Asked about a point in the courtyard, the model answers with just it.
        let courtyard = rect_mask(w, 100..140, 46..140);
        let mut probe = |_: Pt| {
            vec![OwnedCandidate {
                mask: courtyard.clone(),
                confidence: 0.9,
            }]
        };
        let outline =
            building_from_candidates(&candidates, &picture, (60, 100), &[], &mut probe).unwrap();
        assert_eq!(outline.len(), 8, "{outline:?}");
        // The cut-out reaches the top edge: the building is the square less
        // the courtyard and the strip that closed it.
        let expected = 160.0 * 160.0 - 40.0 * 94.0 - 40.0 * 6.0;
        assert!(
            (area(&outline) - expected).abs() < 900.0,
            "{}",
            area(&outline)
        );
    }

    #[test]
    fn a_mask_that_bled_into_the_yard_is_passed_over() {
        let w = 200;
        let roof = rect_mask(w, 60..140, 60..110);
        // The roof plus a long strip of lawn: not rectangular, and unsure.
        let mut bled = roof.clone();
        for (i, flag) in bled.iter_mut().enumerate() {
            if (i % w) < 60 && (80..90).contains(&(i / w)) {
                *flag = true;
            }
        }
        let mut picture = flat(w, 120);
        for y in 80..90 {
            for x in 0..60 {
                picture.set(x, y, [245; 3]);
            }
        }
        let candidates = [
            MaskCandidate {
                mask: &bled,
                confidence: 0.99,
            },
            MaskCandidate {
                mask: &roof,
                confidence: 0.9,
            },
        ];
        let rect =
            building_from_candidates(&candidates, &picture, (80, 80), &[], &mut |_| Vec::new())
                .unwrap();
        assert!((area(&rect) - 80.0 * 50.0).abs() < 100.0, "{}", area(&rect));
    }

    #[test]
    fn a_mask_filling_most_of_the_picture_is_no_area() {
        let w = 100;
        let everything = vec![true; w * w];
        let candidates = [MaskCandidate {
            mask: &everything,
            confidence: 0.99,
        }];
        assert!(area_from_candidates(&candidates, &flat(w, 200), (50, 50)).is_none());
    }

    #[test]
    fn the_biggest_confident_candidate_gives_the_area() {
        let w = 200;
        let crown = rect_mask(w, 90..110, 90..110);
        let stand = rect_mask(w, 50..150, 60..140);
        let candidates = [
            MaskCandidate {
                mask: &crown,
                confidence: 0.97,
            },
            MaskCandidate {
                mask: &stand,
                confidence: 0.85,
            },
        ];
        let ring = area_from_candidates(&candidates, &flat(w, 200), (100, 100)).unwrap();
        assert!(
            (area(&ring) - 100.0 * 80.0).abs() < 100.0,
            "{}",
            area(&ring)
        );
    }

    #[test]
    fn a_picture_of_one_colour_has_no_outline() {
        let r = picture(100, 100, |_, _| None);
        assert!(detect_rectangle(&r, (50, 50), 30.0).is_none());
        assert!(detect_area(&r, (50, 50), 30.0).is_none());
    }

    #[test]
    fn a_region_that_runs_into_unknown_imagery_is_no_outline() {
        let mut r = picture(100, 100, |x, y| {
            ((30.0..70.0).contains(&x) && (30.0..70.0).contains(&y)).then_some(ROOF)
        });
        for y in 0..100 {
            for x in 0..100 {
                if x >= 50 {
                    r.pixels[y * 100 + x] = None;
                }
            }
        }
        // The roof runs into unknown imagery: not a whole building.
        assert!(detect_rectangle(&r, (80, 50), 30.0).is_none());
    }

    #[test]
    fn the_smallest_rectangle_of_a_diamond_is_the_diamond_turned_square() {
        let rect = min_area_rect(&[(0.0, 10.0), (10.0, 0.0), (20.0, 10.0), (10.0, 20.0)]).unwrap();
        assert!((area(&rect) - 200.0).abs() < 1e-6);
    }
}
