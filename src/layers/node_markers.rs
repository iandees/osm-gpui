//! Which node markers are shown at which zoom.
//!
//! Drawing a marker costs about 1.3 us (one `paint_quad` each), so a dense
//! download viewed whole would spend most of a frame on dots too small and
//! too close together to read or click. This module decides, once per node
//! at load time, the zoom from which its marker is shown (`reveal`), so the
//! per-frame test is one float comparison and the same function answers
//! drawing, hover, click and box-select.
//!
//! The rule is a density rule. Nodes are placed in priority order (tagged
//! nodes, then way junctions and endpoints, then plain vertices; ties broken
//! by a hash of the node id, so the result does not depend on input order).
//! For each zoom level from `FIRST_LEVEL` up, the world is cut into square
//! cells that are `cell_px(level)` screen pixels wide at that level; a node
//! is revealed at the first level where no already placed node sits in its
//! cell, and then occupies its cell at every later level. So markers are
//! never closer than roughly `cell_px` pixels apart, isolated nodes show
//! early, dense clusters show a few nodes and gain more as you zoom in, and
//! the choice depends only on world coordinates: it is stable under panning
//! and a node never disappears when zooming in. From `FULL_DETAIL_ZOOM` the
//! cell size is zero and every node is shown, as before.

use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};

/// First zoom level for which cells are computed. Nodes whose cell is free
/// here are shown from this zoom; nothing is shown below it.
pub const FIRST_LEVEL: f64 = 10.0;
/// Spacing of the computed levels, in zoom.
pub const LEVEL_STEP: f64 = 0.5;
/// From this zoom every node is shown at full size.
pub const FULL_DETAIL_ZOOM: f64 = 18.5;
/// A marker grows from nothing to full size over this many zoom levels
/// before its reveal zoom, so nodes do not pop in.
pub const FADE_ZOOMS: f64 = 0.3;
/// Untagged nodes (way vertices) are never shown below this zoom, however
/// sparse the data.
pub const UNTAGGED_FLOOR: f32 = 15.0;

/// Minimum screen spacing, in pixels, between markers at `zoom`
/// (piecewise linear; zero from `FULL_DETAIL_ZOOM`).
pub fn cell_px(zoom: f64) -> f64 {
    const TABLE: [(f64, f64); 5] = [
        (10.0, 26.0),
        (16.0, 26.0),
        (17.0, 24.0),
        (18.0, 14.0),
        (18.5, 0.0),
    ];
    if zoom >= FULL_DETAIL_ZOOM {
        return 0.0;
    }
    for w in TABLE.windows(2) {
        let ((z0, c0), (z1, c1)) = (w[0], w[1]);
        if zoom <= z1 {
            let t = ((zoom - z0) / (z1 - z0)).clamp(0.0, 1.0);
            return c0 + (c1 - c0) * t;
        }
    }
    0.0
}

/// Mercator meters per logical pixel at `zoom`.
pub fn meters_per_px(zoom: f64) -> f64 {
    40075016.686 / (256.0 * 2f64.powf(zoom))
}

/// Placement priority of a node; lower places first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Tagged = 0,
    /// Shared by several ways, or the end of an open way.
    Junction = 1,
    Plain = 2,
}

/// Reveal zoom of a node that is not placed by the density pass (created or
/// retagged after load): no thinning, only the floor of its kind.
pub fn unthinned_reveal(tagged: bool) -> f32 {
    if tagged {
        FIRST_LEVEL as f32
    } else {
        UNTAGGED_FLOOR
    }
}

/// Size factor of a marker with the given reveal zoom: 0 hidden, 1 full,
/// in between while it grows in.
pub fn marker_scale(reveal: f32, zoom: f64) -> f32 {
    if zoom >= FULL_DETAIL_ZOOM {
        return 1.0;
    }
    let t = (zoom - (reveal as f64 - FADE_ZOOMS)) / FADE_ZOOMS;
    t.clamp(0.0, 1.0) as f32
}

/// Whether the marker is drawn at all.
pub fn marker_drawn(reveal: f32, zoom: f64) -> bool {
    marker_scale(reveal, zoom) > 0.0
}

/// Whether the marker is shown clearly enough to be a hover, click and
/// box-select target (at least half size). The single answer shared by
/// drawing, hit testing and box select.
pub fn marker_interactive(reveal: f32, zoom: f64) -> bool {
    marker_scale(reveal, zoom) >= 0.5
}

#[derive(Default)]
struct CellHasher(u64);

impl Hasher for CellHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    fn write_u64(&mut self, n: u64) {
        let mut x = n;
        x ^= x >> 32;
        x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 29;
        self.0 = x;
    }
}

type CellSet = HashSet<u64, BuildHasherDefault<CellHasher>>;

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Reveal zoom for every node. `nodes` holds `(id, mercator x, mercator y,
/// tier)`; the result is aligned with it. Independent of input order.
pub fn assign_reveal_zooms(nodes: &[(i64, f64, f64, Tier)]) -> Vec<f32> {
    let level_count = ((FULL_DETAIL_ZOOM - FIRST_LEVEL) / LEVEL_STEP).round() as usize + 1;
    let last = level_count - 1;
    let cell_m: Vec<f64> = (0..level_count)
        .map(|k| {
            let z = FIRST_LEVEL + k as f64 * LEVEL_STEP;
            cell_px(z) * meters_per_px(z)
        })
        .collect();
    let mut occupied: Vec<CellSet> = (0..level_count).map(|_| CellSet::default()).collect();

    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_unstable_by_key(|&i| (nodes[i].3, splitmix(nodes[i].0 as u64), nodes[i].0));

    let key = |x: f64, y: f64, size: f64| -> u64 {
        let cx = (x / size).floor() as i32;
        let cy = (y / size).floor() as i32;
        ((cx as u32 as u64) << 32) | cy as u32 as u64
    };

    let mut reveal = vec![FULL_DETAIL_ZOOM as f32; nodes.len()];
    for i in order {
        let (_, x, y, tier) = nodes[i];
        let first = if tier == Tier::Tagged {
            0
        } else {
            ((UNTAGGED_FLOOR as f64 - FIRST_LEVEL) / LEVEL_STEP).ceil() as usize
        };
        for k in first..last {
            if !occupied[k].contains(&key(x, y, cell_m[k])) {
                reveal[i] = (FIRST_LEVEL + k as f64 * LEVEL_STEP) as f32;
                for kk in k..last {
                    occupied[kk].insert(key(x, y, cell_m[kk]));
                }
                break;
            }
        }
    }
    reveal
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dense grid of plain nodes `step` meters apart plus one far away.
    fn grid(n: i64, step: f64) -> Vec<(i64, f64, f64, Tier)> {
        let mut v = Vec::new();
        for i in 0..n {
            for j in 0..n {
                v.push((i * n + j + 1, i as f64 * step, j as f64 * step, Tier::Plain));
            }
        }
        v
    }

    fn shown(nodes: &[(i64, f64, f64, Tier)], reveal: &[f32], zoom: f64) -> usize {
        (0..nodes.len())
            .filter(|&i| marker_interactive(reveal[i], zoom))
            .count()
    }

    #[test]
    fn scale_ramps_and_full_detail_shows_everything() {
        assert_eq!(marker_scale(17.0, 16.0), 0.0);
        assert_eq!(marker_scale(17.0, 17.0), 1.0);
        let mid = marker_scale(17.0, 17.0 - FADE_ZOOMS / 2.0);
        assert!((mid - 0.5).abs() < 1e-4);
        assert!(marker_drawn(17.0, 16.9) && !marker_interactive(17.0, 16.75));
        assert_eq!(marker_scale(18.0, FULL_DETAIL_ZOOM), 1.0);
        assert_eq!(marker_scale(99.0, FULL_DETAIL_ZOOM + 1.0), 1.0);
    }

    #[test]
    fn dense_cluster_is_thinned_and_everything_shows_at_full_detail() {
        // 40x40 nodes 2 m apart.
        let nodes = grid(40, 2.0);
        let reveal = assign_reveal_zooms(&nodes);
        let at16 = shown(&nodes, &reveal, 16.0);
        let at17 = shown(&nodes, &reveal, 17.0);
        assert!(at16 < nodes.len() / 20, "z16 shows {at16}");
        assert!(at17 < nodes.len() / 2, "z17 shows {at17}");
        assert!(at16 <= at17);
        assert_eq!(shown(&nodes, &reveal, FULL_DETAIL_ZOOM), nodes.len());
    }

    #[test]
    fn marker_count_is_bounded_by_the_cell_count() {
        // 60x60 nodes 3 m apart cover 177 m square.
        let nodes = grid(60, 3.0);
        let reveal = assign_reveal_zooms(&nodes);
        for z in [14.0, 16.0, 16.5, 17.0, 17.5, 18.0] {
            let size = cell_px(z) * meters_per_px(z);
            let cells = (177.0 / size).ceil() + 1.0;
            let n = nodes
                .iter()
                .enumerate()
                .filter(|&(i, _)| reveal[i] as f64 <= z)
                .count() as f64;
            assert!(
                n <= 2.0 * cells * cells,
                "{n} markers, {cells}^2 cells at z{z}"
            );
        }
    }

    #[test]
    fn sparse_node_shows_at_its_floor_where_dense_ones_are_hidden() {
        let mut nodes = grid(30, 2.0);
        nodes.push((10_000, 5_000.0, 5_000.0, Tier::Plain));
        let reveal = assign_reveal_zooms(&nodes);
        assert_eq!(reveal[nodes.len() - 1], UNTAGGED_FLOOR);
        assert!(shown(&nodes[..nodes.len() - 1], &reveal, UNTAGGED_FLOOR as f64) < 5);
    }

    #[test]
    fn tagged_nodes_win_their_cell_over_vertices() {
        let nodes = vec![
            (1, 0.0, 0.0, Tier::Plain),
            (2, 0.5, 0.5, Tier::Tagged),
            (3, 0.7, 0.2, Tier::Junction),
        ];
        let reveal = assign_reveal_zooms(&nodes);
        assert_eq!(reveal[1], FIRST_LEVEL as f32);
        assert!(reveal[0] >= reveal[2]);
        assert!(reveal[2] > reveal[1]);
    }

    #[test]
    fn result_does_not_depend_on_input_order_or_view() {
        let nodes = grid(25, 4.0);
        let a = assign_reveal_zooms(&nodes);
        let mut rev: Vec<_> = nodes.clone();
        rev.reverse();
        let b = assign_reveal_zooms(&rev);
        for (i, n) in nodes.iter().enumerate() {
            let j = rev.iter().position(|m| m.0 == n.0).unwrap();
            assert_eq!(a[i], b[j]);
        }
    }

    #[test]
    fn nodes_never_disappear_when_zooming_in() {
        let nodes = grid(30, 3.0);
        let reveal = assign_reveal_zooms(&nodes);
        let mut prev = 0;
        let mut z = 10.0;
        while z <= 19.0 {
            let n = shown(&nodes, &reveal, z);
            assert!(n >= prev);
            prev = n;
            z += 0.1;
        }
    }
}
