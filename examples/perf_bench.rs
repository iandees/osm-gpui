//! CPU-side rendering-path benchmarks. Run with:
//!   cargo run --release --example perf_bench
//!
//! Uses a synthetic dataset shaped like a dense urban extract (default:
//! 60k nodes / 8k ways, ~12 nodes per way) and times the per-frame and
//! per-interaction code paths that don't require a live GPUI window:
//! style resolution, the render_canvas CPU loop (cull + project + quad
//! build), click hit-testing, box-select, and the commit/rebuild path.

use gpui::{point, px, size, Path, PathVertex, Pixels, Point};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::{EditableLayer, LayerId, MapLayer};
use osm_gpui::osm::{OsmData, OsmNode, OsmWay};
use osm_gpui::style::Stylesheet;
use osm_gpui::viewport::Viewport;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const CENTER_LAT: f64 = 44.9778;
const CENTER_LON: f64 = -93.2650;

/// Deterministic LCG so runs are comparable.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Ways are spatially compact chains (random anchor + ~10m steps), like real
/// street/building geometry, so bbox culling behaves realistically. Extra
/// standalone POI nodes are sprinkled on top.
fn build_dataset(n_ways: usize, nodes_per_way: usize, n_poi_nodes: usize) -> Arc<OsmData> {
    let mut rng = Lcg(42);
    let mut nodes = HashMap::new();
    let mut ways = HashMap::with_capacity(n_ways);
    let mut next_id: i64 = 0;
    // ~0.0001 deg is roughly 10m — a typical way-vertex spacing.
    const STEP: f64 = 0.0001;
    for wid in 0..n_ways as i64 {
        let mut lat = CENTER_LAT + (rng.f64() - 0.5) * 0.05;
        let mut lon = CENTER_LON + (rng.f64() - 0.5) * 0.05;
        let mut node_ids = Vec::with_capacity(nodes_per_way);
        for _ in 0..nodes_per_way {
            let id = next_id;
            next_id += 1;
            nodes.insert(id, OsmNode { id, lat, lon, tags: HashMap::new(), version: 1 });
            node_ids.push(id);
            lat += (rng.f64() - 0.5) * 2.0 * STEP;
            lon += (rng.f64() - 0.5) * 2.0 * STEP;
        }
        let t = match wid % 10 {
            0..=3 => tags(&[("highway", "residential"), ("name", "Street")]),
            4 => tags(&[("highway", "footway")]),
            5..=7 => tags(&[("building", "yes")]),
            8 => tags(&[("waterway", "stream")]),
            _ => HashMap::new(),
        };
        ways.insert(wid, OsmWay { id: wid, nodes: node_ids, tags: t, version: 1 });
    }
    for _ in 0..n_poi_nodes {
        let id = next_id;
        next_id += 1;
        let lat = CENTER_LAT + (rng.f64() - 0.5) * 0.05;
        let lon = CENTER_LON + (rng.f64() - 0.5) * 0.05;
        let t = match id % 3 {
            0 => tags(&[("amenity", "cafe"), ("name", "Cafe")]),
            1 => tags(&[("shop", "convenience")]),
            _ => HashMap::new(),
        };
        nodes.insert(id, OsmNode { id, lat, lon, tags: t, version: 1 });
    }
    Arc::new(OsmData { nodes, ways, relations: Vec::new(), bounds: None })
}

fn bench<F: FnMut() -> u64>(name: &str, iters: usize, mut f: F) {
    // Warmup.
    let mut sink = 0u64;
    for _ in 0..2 {
        sink = sink.wrapping_add(f());
    }
    let mut samples: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        sink = sink.wrapping_add(f());
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = samples[samples.len() / 2];
    let min = samples[0];
    let max = samples[samples.len() - 1];
    println!("{name:<55} med {med:>8.3} ms  (min {min:.3}, max {max:.3})  [sink {sink}]");
}

/// Replica of render_canvas's way loop: cull, resolve style, project, build
/// quads grouped by style. Everything except the final window.paint_path.
fn render_ways_cpu(
    ways_sorted: &[&OsmWay],
    way_vertices: &[Vec<(i64, f64, f64)>],
    way_bboxes: &[Option<(f64, f64, f64, f64)>],
    viewport: &Viewport,
    stylesheet: &Stylesheet,
    cached_styles: Option<&[(u32, f32)]>,
    // Skip emitting a segment until it has accumulated at least this many
    // screen pixels of length (0.0 = current behavior, no decimation).
    min_seg_px: f32,
) -> u64 {
    let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
    struct Group {
        path: Option<Path<Pixels>>,
    }
    let mut groups: HashMap<(u32, u32), Group> = HashMap::new();
    let mut visible = 0u64;
    for (i, verts) in way_vertices.iter().enumerate() {
        if verts.len() < 2 {
            continue;
        }
        let Some((min_x, max_x, min_y, max_y)) = way_bboxes[i] else { continue };
        if max_x < vmin_x || min_x > vmax_x || max_y < vmin_y || min_y > vmax_y {
            continue;
        }
        visible += 1;
        let (color, width) = match cached_styles {
            Some(cs) => cs[i],
            None => {
                let s = stylesheet.way_style(&ways_sorted[i].tags);
                (s.color, s.width)
            }
        };
        let half_width = width / 2.0;
        let g = groups
            .entry((color, width.to_bits()))
            .or_insert_with(|| Group { path: None });
        let mut prev: Option<Point<Pixels>> = None;
        let n = verts.len();
        for (vi, &(_id, mx, my)) in verts.iter().enumerate() {
            let sp = viewport.mercator_to_screen(mx, my);
            let p = point(sp.x, sp.y);
            if let Some(p0) = prev {
                if min_seg_px > 0.0 && vi != n - 1 {
                    let dx = f32::from(p.x) - f32::from(p0.x);
                    let dy = f32::from(p.y) - f32::from(p0.y);
                    // Accumulate: keep `prev` anchored until the pending
                    // segment reaches the pixel threshold (always emit to
                    // the final vertex so endpoints stay exact).
                    if dx * dx + dy * dy < min_seg_px * min_seg_px {
                        continue;
                    }
                }
                push_quad(&mut g.path, p0, p, half_width);
            }
            prev = Some(p);
        }
    }
    let mut verts_total = 0u64;
    for (_, g) in groups {
        if let Some(p) = g.path {
            verts_total += p.vertices.len() as u64;
        }
    }
    visible.wrapping_add(verts_total)
}

fn push_quad(path: &mut Option<Path<Pixels>>, p0: Point<Pixels>, p1: Point<Pixels>, half_width: f32) {
    let x0 = f32::from(p0.x);
    let y0 = f32::from(p0.y);
    let x1 = f32::from(p1.x);
    let y1 = f32::from(p1.y);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len < f32::EPSILON {
        return;
    }
    let nx = -dy / len * half_width;
    let ny = dx / len * half_width;
    let a = point(px(x0 + nx), px(y0 + ny));
    let b = point(px(x0 - nx), px(y0 - ny));
    let c = point(px(x1 - nx), px(y1 - ny));
    let d = point(px(x1 + nx), px(y1 + ny));
    let st = point(0., 1.);
    let p = path.get_or_insert_with(|| Path::new(a));
    for v in [a, b, c, a, c, d] {
        p.vertices.push(PathVertex {
            xy_position: v,
            st_position: st,
            content_mask: Default::default(),
        });
    }
}

/// Node loop replica: cull, per-node HashMap lookup + style resolution.
fn render_nodes_cpu(
    data: &OsmData,
    flat: &[(i64, f64, f64)],
    viewport: &Viewport,
    stylesheet: &Stylesheet,
    skip_style: bool,
) -> u64 {
    let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
    let mut acc = 0u64;
    for &(id, mx, my) in flat {
        if mx < vmin_x || mx > vmax_x || my < vmin_y || my > vmax_y {
            continue;
        }
        let sp = viewport.mercator_to_screen(mx, my);
        let sizef = if skip_style {
            5.0
        } else {
            match data.nodes.get(&id) {
                Some(n) => stylesheet.node_style(&n.tags).size,
                None => 5.0,
            }
        };
        acc = acc.wrapping_add((f32::from(sp.x) + sizef) as u64);
    }
    acc
}

fn main() {
    let n_ways = 8_000;
    let nodes_per_way = 12;
    let n_poi = 10_000;
    let data = build_dataset(n_ways, nodes_per_way, n_poi);
    println!(
        "dataset: {} nodes ({} ways x {} + {} POIs); viewport 1600x1000",
        data.nodes.len(),
        n_ways,
        nodes_per_way,
        n_poi
    );
    let layer = OsmLayer::new_with_data(LayerId(1), "bench", data.clone());
    let stylesheet = Stylesheet::load_default();

    // Zoom 14 puts the whole 0.05-degree box on screen -> ~everything visible.
    let viewport = Viewport::new(CENTER_LAT, CENTER_LON, 14.0, size(px(1600.0), px(1000.0)));
    // Zoom 17: only a fraction of the box visible -> culling does its job.
    let viewport_z17 = Viewport::new(CENTER_LAT, CENTER_LON, 17.0, size(px(1600.0), px(1000.0)));

    // Rebuild the same derived tables the layer holds, so the render replica
    // matches the real loop's data layout.
    use osm_gpui::coordinates::lat_lon_to_mercator;
    let mut merc: HashMap<i64, (f64, f64)> = HashMap::with_capacity(data.nodes.len());
    let mut flat: Vec<(i64, f64, f64)> = Vec::with_capacity(data.nodes.len());
    for n in data.nodes.values() {
        let (mx, my) = lat_lon_to_mercator(n.lat, n.lon);
        merc.insert(n.id, (mx, my));
        flat.push((n.id, mx, my));
    }
    // Iterate ways sorted by id, matching OsmLayer's deterministic table order.
    let mut ways_sorted: Vec<&OsmWay> = data.ways.values().collect();
    ways_sorted.sort_by_key(|w| w.id);
    let mut way_vertices: Vec<Vec<(i64, f64, f64)>> = Vec::with_capacity(data.ways.len());
    let mut way_bboxes: Vec<Option<(f64, f64, f64, f64)>> = Vec::with_capacity(data.ways.len());
    for w in &ways_sorted {
        let mut vs = Vec::with_capacity(w.nodes.len());
        let (mut min_x, mut max_x, mut min_y, mut max_y) =
            (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for nid in &w.nodes {
            if let Some(&(mx, my)) = merc.get(nid) {
                vs.push((*nid, mx, my));
                if mx < min_x { min_x = mx; }
                if mx > max_x { max_x = mx; }
                if my < min_y { min_y = my; }
                if my > max_y { max_y = my; }
            }
        }
        way_bboxes.push(if vs.is_empty() { None } else { Some((min_x, max_x, min_y, max_y)) });
        way_vertices.push(vs);
    }
    let cached_way_styles: Vec<(u32, f32)> = ways_sorted
        .iter()
        .map(|w| {
            let s = stylesheet.way_style(&w.tags);
            (s.color, s.width)
        })
        .collect();

    println!("\n-- per-frame render CPU (everything except GPU submit) --");
    bench("ways z14 all visible: current", 30, || {
        render_ways_cpu(&ways_sorted, &way_vertices, &way_bboxes, &viewport, &stylesheet, None, 0.0)
    });
    bench("ways z14: precomputed styles (proposed)", 30, || {
        render_ways_cpu(&ways_sorted, &way_vertices, &way_bboxes, &viewport, &stylesheet, Some(&cached_way_styles), 0.0)
    });
    bench("ways z14: cached styles + 1px decimation (proposed)", 30, || {
        render_ways_cpu(&ways_sorted, &way_vertices, &way_bboxes, &viewport, &stylesheet, Some(&cached_way_styles), 1.0)
    });
    bench("ways z17 (culling active): current", 30, || {
        render_ways_cpu(&ways_sorted, &way_vertices, &way_bboxes, &viewport_z17, &stylesheet, None, 0.0)
    });
    bench("nodes: cull+hashmap+style (current)", 30, || {
        render_nodes_cpu(&data, &flat, &viewport, &stylesheet, false)
    });
    bench("nodes: cull only, style skipped (proposed cache)", 30, || {
        render_nodes_cpu(&data, &flat, &viewport, &stylesheet, true)
    });

    println!("\n-- style resolution microbench --");
    let hw_tags = tags(&[("highway", "residential"), ("name", "X"), ("surface", "asphalt")]);
    bench("way_style x 8k calls", 30, || {
        let mut acc = 0u64;
        for _ in 0..8_000 {
            acc = acc.wrapping_add(stylesheet.way_style(&hw_tags).color as u64);
        }
        acc
    });

    println!("\n-- interaction latency --");
    let click = point(px(800.0), px(500.0));
    bench("hit_test single click (current linear scan)", 20, || {
        layer.hit_test(&viewport, click).len() as u64
    });
    let rect = gpui::Bounds {
        origin: point(px(780.0), px(480.0)),
        size: size(px(40.0), px(40.0)),
    };
    bench("hit_test_rect same area (R-tree path)", 20, || {
        layer.hit_test_rect(&viewport, rect).len() as u64
    });

    println!("\n-- edit commit path --");
    // `data` (this closure's captured outer Arc) stays alive for the rest of
    // `main`, so every `data.clone()` here makes the fresh layer's Arc the
    // *second* live reference to the same OsmData -> Arc::make_mut can't
    // mutate in place and clones once per call, same cost profile as the
    // old always-full-clone code. This is the pessimistic case: some other
    // long-lived Arc (a snapshot, an in-flight export) is alive across the
    // edit.
    bench("commit_node_moves: 1 node, Arc SHARED (make_mut clones once)", 10, || {
        let mut l = OsmLayer::new_with_data(LayerId(1), "bench", data.clone());
        l.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
        l.is_modified() as u64
    });
    // Production reality: nothing outside tests ever holds a second
    // Arc<OsmData> clone across an edit (see OsmLayer::get_osm_data's doc
    // comment), so the layer's Arc is uniquely held -> Arc::make_mut
    // mutates in place, no dataset clone at all. Give this bench its own
    // OsmData (a one-time deep clone, not timed) so it's fully decoupled
    // from the `data` Arc kept alive elsewhere in `main`.
    let unique_data: Arc<OsmData> = Arc::new((*data).clone());
    let mut l_unique = OsmLayer::new_with_data(LayerId(1), "bench-unique", unique_data);
    bench("commit_node_moves: 1 node, Arc UNIQUE (make_mut, in place)", 30, || {
        l_unique.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
        l_unique.is_modified() as u64
    });
    bench("set_osm_data rebuild alone", 10, || {
        let mut l = OsmLayer::new(LayerId(1));
        l.set_osm_data(data.clone());
        l.has_data() as u64
    });
}
