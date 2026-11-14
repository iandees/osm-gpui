//! Geometry for showing where data has been downloaded.
//!
//! A layer knows the lat/lon rectangles its data was downloaded for. To make
//! "nothing here" distinguishable from "not downloaded here", the map dims
//! everything outside the union of those rectangles and outlines the edge of
//! the downloaded area.
//!
//! Lat/lon-aligned rectangles stay axis-aligned in Web Mercator, so all of
//! this works on axis-aligned rectangles in Mercator meters (y grows
//! northwards) and the caller only projects corners to the screen.
//!
//! The outside region is found by a coordinate-grid sweep: every rectangle
//! edge (clipped to the view) and the view edges cut the view into a small
//! grid of cells, each of which is either wholly inside or wholly outside the
//! union. The uncovered cells are merged back into as few non-overlapping
//! rectangles as is cheap, and the outline is the set of cell edges that
//! separate a covered cell from an uncovered one.

use crate::coordinates::{lat_lon_to_mercator, LatLon};
use crate::osm::OsmBounds;

/// An axis-aligned rectangle in Web Mercator meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MercRect {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl MercRect {
    pub fn new(min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> Self {
        Self {
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }

    /// The Mercator rectangle of a lat/lon one. `None` for a degenerate or
    /// non-finite rectangle (e.g. a `<bounds>` element missing attributes),
    /// which would otherwise dim the whole map.
    pub fn from_bounds(b: &OsmBounds) -> Option<Self> {
        if ![b.min_lat, b.max_lat, b.min_lon, b.max_lon]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let min = lat_lon_to_mercator(LatLon::new(b.min_lat, b.min_lon));
        let max = lat_lon_to_mercator(LatLon::new(b.max_lat, b.max_lon));
        (min.x < max.x && min.y < max.y).then_some(Self::new(min.x, max.x, min.y, max.y))
    }

    fn intersect(&self, other: &MercRect) -> Option<MercRect> {
        let r = MercRect::new(
            self.min_x.max(other.min_x),
            self.max_x.min(other.max_x),
            self.min_y.max(other.min_y),
            self.max_y.min(other.max_y),
        );
        (r.min_x < r.max_x && r.min_y < r.max_y).then_some(r)
    }
}

/// An axis-aligned line segment in Mercator meters: `(x0, y0)` to `(x1, y1)`,
/// with either `x0 == x1` or `y0 == y1`, and the start before the end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// What to draw for a set of downloaded rectangles within a view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutsideRegion {
    /// Non-overlapping rectangles that together cover the part of the view
    /// outside every downloaded rectangle.
    pub dim: Vec<MercRect>,
    /// The edges of the downloaded area that lie inside the view. Edges
    /// shared by two rectangles are not included, and neither is the part of
    /// the view border where the area merely runs off-screen.
    pub outline: Vec<Segment>,
}

/// Mercator rectangles for a layer's `<bounds>`, skipping unusable ones.
pub fn merc_rects(bounds: &[OsmBounds]) -> Vec<MercRect> {
    bounds.iter().filter_map(MercRect::from_bounds).collect()
}

/// The region of `view` outside the union of `rects`, plus the outline of
/// that union. With no usable rectangles nothing is known about what was
/// downloaded, so nothing is drawn (an empty result, not "dim everything").
pub fn outside_region(view: MercRect, rects: &[MercRect]) -> OutsideRegion {
    let clipped: Vec<MercRect> = rects.iter().filter_map(|r| r.intersect(&view)).collect();
    if view.min_x >= view.max_x || view.min_y >= view.max_y {
        return OutsideRegion::default();
    }
    // Known area exists but none of it is in view: everything in view is outside.
    if clipped.is_empty() {
        return if rects.iter().any(|r| r.min_x < r.max_x && r.min_y < r.max_y) {
            OutsideRegion {
                dim: vec![view],
                outline: Vec::new(),
            }
        } else {
            OutsideRegion::default()
        };
    }

    let mut xs = vec![view.min_x, view.max_x];
    let mut ys = vec![view.min_y, view.max_y];
    for r in &clipped {
        xs.extend([r.min_x, r.max_x]);
        ys.extend([r.min_y, r.max_y]);
    }
    for v in [&mut xs, &mut ys] {
        v.sort_by(|a, b| a.total_cmp(b));
        v.dedup();
    }
    let (nx, ny) = (xs.len() - 1, ys.len() - 1);
    let index_of = |v: &[f64], x: f64| v.binary_search_by(|p| p.total_cmp(&x)).unwrap_or(0);

    // covered[j * nx + i]: is cell (column i, row j) inside some rectangle?
    let mut covered = vec![false; nx * ny];
    for r in &clipped {
        let (i0, i1) = (index_of(&xs, r.min_x), index_of(&xs, r.max_x));
        let (j0, j1) = (index_of(&ys, r.min_y), index_of(&ys, r.max_y));
        for j in j0..j1 {
            for i in i0..i1 {
                covered[j * nx + i] = true;
            }
        }
    }

    // Uncovered cells -> horizontal runs per row -> merged with an identical
    // run in the row below (fewer quads, no seams between them).
    let mut dim: Vec<MercRect> = Vec::new();
    for j in 0..ny {
        let mut i = 0;
        while i < nx {
            if covered[j * nx + i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < nx && !covered[j * nx + i] {
                i += 1;
            }
            let (x0, x1) = (xs[start], xs[i]);
            let (y0, y1) = (ys[j], ys[j + 1]);
            match dim
                .iter_mut()
                .rev()
                .find(|r| r.min_x == x0 && r.max_x == x1 && r.max_y == y0)
            {
                Some(r) => r.max_y = y1,
                None => dim.push(MercRect::new(x0, x1, y0, y1)),
            }
        }
    }

    // Outline: interior grid lines where coverage changes across the line.
    let mut outline: Vec<Segment> = Vec::new();
    for j in 1..ny {
        let mut i = 0;
        while i < nx {
            if covered[(j - 1) * nx + i] == covered[j * nx + i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < nx && covered[(j - 1) * nx + i] != covered[j * nx + i] {
                i += 1;
            }
            outline.push(Segment {
                x0: xs[start],
                y0: ys[j],
                x1: xs[i],
                y1: ys[j],
            });
        }
    }
    for i in 1..nx {
        let mut j = 0;
        while j < ny {
            if covered[j * nx + i - 1] == covered[j * nx + i] {
                j += 1;
                continue;
            }
            let start = j;
            while j < ny && covered[j * nx + i - 1] != covered[j * nx + i] {
                j += 1;
            }
            outline.push(Segment {
                x0: xs[i],
                y0: ys[start],
                x1: xs[i],
                y1: ys[j],
            });
        }
    }

    OutsideRegion { dim, outline }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::Mercator;
    use std::prelude::v1::test;

    fn r(min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> MercRect {
        MercRect::new(min_x, max_x, min_y, max_y)
    }

    fn area(rects: &[MercRect]) -> f64 {
        rects
            .iter()
            .map(|r| (r.max_x - r.min_x) * (r.max_y - r.min_y))
            .sum()
    }

    fn length(segs: &[Segment]) -> f64 {
        segs.iter()
            .map(|s| (s.x1 - s.x0).abs() + (s.y1 - s.y0).abs())
            .sum()
    }

    fn overlaps(a: &MercRect, b: &MercRect) -> bool {
        a.intersect(b).is_some()
    }

    fn assert_disjoint(rects: &[MercRect]) {
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!overlaps(a, b), "{a:?} overlaps {b:?}");
            }
        }
    }

    const VIEW: MercRect = MercRect {
        min_x: 0.0,
        max_x: 10.0,
        min_y: 0.0,
        max_y: 10.0,
    };

    #[test]
    fn no_rects_draws_nothing() {
        assert_eq!(outside_region(VIEW, &[]), OutsideRegion::default());
    }

    #[test]
    fn unusable_bounds_are_ignored() {
        let zero = OsmBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: 0.0,
            max_lon: 0.0,
        };
        assert!(merc_rects(&[zero]).is_empty());
        let nan = OsmBounds {
            min_lat: f64::NAN,
            max_lat: 1.0,
            min_lon: 0.0,
            max_lon: 1.0,
        };
        assert!(merc_rects(&[nan]).is_empty());
    }

    #[test]
    fn from_bounds_projects_corners() {
        let b = OsmBounds {
            min_lat: 40.0,
            max_lat: 41.0,
            min_lon: -75.0,
            max_lon: -74.0,
        };
        let m = MercRect::from_bounds(&b).unwrap();
        let Mercator { x, y } = lat_lon_to_mercator(LatLon::new(40.0, -75.0));
        assert_eq!((m.min_x, m.min_y), (x, y));
        assert!(m.max_x > m.min_x && m.max_y > m.min_y);
    }

    #[test]
    fn single_rect_inside_view_leaves_a_frame() {
        let rect = r(2.0, 6.0, 3.0, 8.0);
        let out = outside_region(VIEW, &[rect]);
        assert_disjoint(&out.dim);
        assert_eq!(area(&out.dim), 100.0 - 20.0);
        assert!(out.dim.iter().all(|d| !overlaps(d, &rect)));
        // Full perimeter of the rectangle.
        assert_eq!(length(&out.outline), 2.0 * (4.0 + 5.0));
        assert_eq!(out.outline.len(), 4);
    }

    #[test]
    fn two_disjoint_rects() {
        let a = r(1.0, 3.0, 1.0, 3.0);
        let b = r(5.0, 8.0, 5.0, 9.0);
        let out = outside_region(VIEW, &[a, b]);
        assert_disjoint(&out.dim);
        assert_eq!(area(&out.dim), 100.0 - 4.0 - 12.0);
        assert!(out.dim.iter().all(|d| !overlaps(d, &a) && !overlaps(d, &b)));
        assert_eq!(length(&out.outline), 8.0 + 14.0);
    }

    #[test]
    fn two_overlapping_rects_outline_is_the_union_boundary() {
        // Two 4x4 squares overlapping in a 2x2 corner: union area 28,
        // boundary of the union is a staircase of perimeter 16 + 16 - 8.
        let a = r(2.0, 6.0, 2.0, 6.0);
        let b = r(4.0, 8.0, 4.0, 8.0);
        let out = outside_region(VIEW, &[a, b]);
        assert_disjoint(&out.dim);
        assert_eq!(area(&out.dim), 100.0 - 28.0);
        assert_eq!(length(&out.outline), 24.0);
        // No outline edge strictly inside the union.
        for s in &out.outline {
            let mid = ((s.x0 + s.x1) / 2.0, (s.y0 + s.y1) / 2.0);
            let strictly_inside = |q: &MercRect| {
                mid.0 > q.min_x && mid.0 < q.max_x && mid.1 > q.min_y && mid.1 < q.max_y
            };
            assert!(!strictly_inside(&a) && !strictly_inside(&b), "{s:?}");
        }
    }

    #[test]
    fn adjacent_rects_share_no_outline_edge() {
        let a = r(2.0, 5.0, 2.0, 6.0);
        let b = r(5.0, 8.0, 2.0, 6.0);
        let out = outside_region(VIEW, &[a, b]);
        assert_eq!(area(&out.dim), 100.0 - 24.0);
        // Perimeter of the merged 6x4 rectangle only.
        assert_eq!(length(&out.outline), 20.0);
    }

    #[test]
    fn rect_partly_off_view_has_no_outline_on_the_view_border() {
        // Sticks out of the left side of the view.
        let rect = r(-5.0, 4.0, 2.0, 6.0);
        let out = outside_region(VIEW, &[rect]);
        assert_disjoint(&out.dim);
        assert_eq!(area(&out.dim), 100.0 - 4.0 * 4.0);
        // Right edge (4) plus top and bottom edges (4 each, clipped at x=0).
        assert_eq!(length(&out.outline), 4.0 + 4.0 + 4.0);
        assert!(out.outline.iter().all(|s| s.x0 >= 0.0 && s.x1 <= 10.0));
    }

    #[test]
    fn rect_covering_a_corner_of_the_view() {
        let rect = r(-5.0, 4.0, -5.0, 4.0);
        let out = outside_region(VIEW, &[rect]);
        assert_eq!(area(&out.dim), 100.0 - 16.0);
        assert_eq!(length(&out.outline), 8.0);
    }

    #[test]
    fn view_fully_inside_a_rect_draws_nothing() {
        let out = outside_region(VIEW, &[r(-5.0, 20.0, -5.0, 20.0)]);
        assert!(out.dim.is_empty());
        assert!(out.outline.is_empty());
    }

    #[test]
    fn view_fully_outside_every_rect_is_all_dimmed() {
        let out = outside_region(VIEW, &[r(20.0, 30.0, 20.0, 30.0)]);
        assert_eq!(out.dim, vec![VIEW]);
        assert!(out.outline.is_empty());
    }

    #[test]
    fn rect_touching_the_view_from_outside_dims_everything() {
        let out = outside_region(VIEW, &[r(10.0, 20.0, 0.0, 10.0)]);
        assert_eq!(out.dim, vec![VIEW]);
        assert!(out.outline.is_empty());
    }

    #[test]
    fn rect_matching_the_view_draws_nothing() {
        let out = outside_region(VIEW, &[VIEW]);
        assert!(out.dim.is_empty() && out.outline.is_empty());
    }

    #[test]
    fn dim_is_merged_into_few_rectangles() {
        let out = outside_region(VIEW, &[r(2.0, 6.0, 3.0, 8.0)]);
        // A frame around one rectangle: top, bottom, left, right bands.
        assert_eq!(out.dim.len(), 4);
    }

    #[test]
    fn identical_rects_do_not_double_count() {
        let rect = r(2.0, 6.0, 3.0, 8.0);
        let out = outside_region(VIEW, &[rect, rect]);
        assert_eq!(area(&out.dim), 80.0);
        assert_eq!(length(&out.outline), 18.0);
    }
}
