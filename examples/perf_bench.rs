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
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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
            nodes.insert(
                id,
                OsmNode {
                    id,
                    lat,
                    lon,
                    tags: HashMap::new(),
                    version: 1,
                },
            );
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
        ways.insert(
            wid,
            OsmWay {
                id: wid,
                nodes: node_ids,
                tags: t,
                version: 1,
            },
        );
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
        nodes.insert(
            id,
            OsmNode {
                id,
                lat,
                lon,
                tags: t,
                version: 1,
            },
        );
    }
    Arc::new(OsmData {
        nodes,
        ways,
        relations: Vec::new(),
        bounds: Vec::new(),
    })
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
    render_ways_cpu_inner(
        ways_sorted,
        way_vertices,
        way_bboxes,
        viewport,
        stylesheet,
        cached_styles,
        min_seg_px,
        false,
    )
}

/// `render_ways_cpu`, optionally also emitting every segment a second time
/// into one shared, slightly wider casing path — as the real loop does at
/// zoom >= `CASING_MIN_ZOOM`.
#[allow(clippy::too_many_arguments)]
fn render_ways_cpu_inner(
    ways_sorted: &[&OsmWay],
    way_vertices: &[Vec<(i64, f64, f64)>],
    way_bboxes: &[Option<(f64, f64, f64, f64)>],
    viewport: &Viewport,
    stylesheet: &Stylesheet,
    cached_styles: Option<&[(u32, f32)]>,
    min_seg_px: f32,
    casing: bool,
) -> u64 {
    let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
    struct Group {
        path: Option<Path<Pixels>>,
    }
    let mut casing_path: Option<Path<Pixels>> = None;
    let mut groups: HashMap<(u32, u32), Group> = HashMap::new();
    let mut visible = 0u64;
    for (i, verts) in way_vertices.iter().enumerate() {
        if verts.len() < 2 {
            continue;
        }
        let Some((min_x, max_x, min_y, max_y)) = way_bboxes[i] else {
            continue;
        };
        if max_x < vmin_x || min_x > vmax_x || max_y < vmin_y || min_y > vmax_y {
            continue;
        }
        visible += 1;
        let (color, width) = match cached_styles {
            Some(cs) => cs[i],
            None => {
                let s = stylesheet.way_style(&ways_sorted[i].tags, ways_sorted[i].is_closed());
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
                if casing {
                    push_quad(&mut casing_path, p0, p, half_width + 1.0);
                }
                push_quad(&mut g.path, p0, p, half_width);
            }
            prev = Some(p);
        }
    }
    let mut verts_total = casing_path.map_or(0, |p| p.vertices.len() as u64);
    for (_, g) in groups {
        if let Some(p) = g.path {
            verts_total += p.vertices.len() as u64;
        }
    }
    visible.wrapping_add(verts_total)
}

fn push_quad(
    path: &mut Option<Path<Pixels>>,
    p0: Point<Pixels>,
    p1: Point<Pixels>,
    half_width: f32,
) {
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
    // `cargo run --release --example perf_bench -- labels` runs only the
    // street-label section.
    if std::env::args().any(|a| a == "labels") {
        line_labels_section();
        return;
    }
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
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for nid in &w.nodes {
            if let Some(&(mx, my)) = merc.get(nid) {
                vs.push((*nid, mx, my));
                if mx < min_x {
                    min_x = mx;
                }
                if mx > max_x {
                    max_x = mx;
                }
                if my < min_y {
                    min_y = my;
                }
                if my > max_y {
                    max_y = my;
                }
            }
        }
        way_bboxes.push(if vs.is_empty() {
            None
        } else {
            Some((min_x, max_x, min_y, max_y))
        });
        way_vertices.push(vs);
    }
    let cached_way_styles: Vec<(u32, f32)> = ways_sorted
        .iter()
        .map(|w| {
            let s = stylesheet.way_style(&w.tags, w.is_closed());
            (s.color, s.width)
        })
        .collect();

    println!("\n-- per-frame render CPU (everything except GPU submit) --");
    bench("ways z14 all visible: current", 30, || {
        render_ways_cpu(
            &ways_sorted,
            &way_vertices,
            &way_bboxes,
            &viewport,
            &stylesheet,
            None,
            0.0,
        )
    });
    bench("ways z14: precomputed styles (proposed)", 30, || {
        render_ways_cpu(
            &ways_sorted,
            &way_vertices,
            &way_bboxes,
            &viewport,
            &stylesheet,
            Some(&cached_way_styles),
            0.0,
        )
    });
    bench(
        "ways z14: cached styles + 1px decimation (proposed)",
        30,
        || {
            render_ways_cpu(
                &ways_sorted,
                &way_vertices,
                &way_bboxes,
                &viewport,
                &stylesheet,
                Some(&cached_way_styles),
                1.0,
            )
        },
    );
    bench("ways z14: cached styles + decimation + casing", 30, || {
        render_ways_cpu_inner(
            &ways_sorted,
            &way_vertices,
            &way_bboxes,
            &viewport,
            &stylesheet,
            Some(&cached_way_styles),
            1.0,
            true,
        )
    });
    bench("ways z17 (culling active): current", 30, || {
        render_ways_cpu(
            &ways_sorted,
            &way_vertices,
            &way_bboxes,
            &viewport_z17,
            &stylesheet,
            None,
            0.0,
        )
    });
    bench("nodes: cull+hashmap+style (current)", 30, || {
        render_nodes_cpu(&data, &flat, &viewport, &stylesheet, false)
    });
    bench(
        "nodes: cull only, style skipped (proposed cache)",
        30,
        || render_nodes_cpu(&data, &flat, &viewport, &stylesheet, true),
    );

    println!("\n-- style resolution microbench --");
    let hw_tags = tags(&[
        ("highway", "residential"),
        ("name", "X"),
        ("surface", "asphalt"),
    ]);
    bench("way_style x 8k calls", 30, || {
        let mut acc = 0u64;
        for _ in 0..8_000 {
            acc = acc.wrapping_add(stylesheet.way_style(&hw_tags, false).color as u64);
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
    bench(
        "commit_node_moves: 1 node, Arc SHARED (make_mut clones once)",
        10,
        || {
            let mut l = OsmLayer::new_with_data(LayerId(1), "bench", data.clone());
            l.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
            l.is_modified() as u64
        },
    );
    // Production reality: nothing outside tests ever holds a second
    // Arc<OsmData> clone across an edit (see OsmLayer::get_osm_data's doc
    // comment), so the layer's Arc is uniquely held -> Arc::make_mut
    // mutates in place, no dataset clone at all. Give this bench its own
    // OsmData (a one-time deep clone, not timed) so it's fully decoupled
    // from the `data` Arc kept alive elsewhere in `main`.
    let unique_data: Arc<OsmData> = Arc::new((*data).clone());
    let mut l_unique = OsmLayer::new_with_data(LayerId(1), "bench-unique", unique_data);
    bench(
        "commit_node_moves: 1 node, Arc UNIQUE (make_mut, in place)",
        30,
        || {
            l_unique.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
            l_unique.is_modified() as u64
        },
    );
    bench("set_osm_data rebuild alone", 10, || {
        let mut l = OsmLayer::new(LayerId(1));
        l.set_osm_data(data.clone());
        l.has_data() as u64
    });

    multipolygon_section(&data);
    line_labels_section();
}

/// ~320 named streets of ~400 m at random angles with a little wobble.
fn build_street_dataset() -> Arc<OsmData> {
    let mut rng = Lcg(7);
    let mut nodes = HashMap::new();
    let mut ways = HashMap::new();
    let mut next_id = 1i64;
    let names = [
        "Main Street",
        "Washington Boulevard",
        "Oak Avenue",
        "Elm St",
        "Riverside Terrace",
        "Lincoln Road",
        "Maple Drive",
        "Broadway",
    ];
    for wid in 0..320i64 {
        let (cl, co) = (
            CENTER_LAT + (rng.f64() - 0.5) * 0.016,
            CENTER_LON + (rng.f64() - 0.5) * 0.022,
        );
        let ang = rng.f64() * std::f64::consts::PI;
        let mut ids = Vec::new();
        for i in 0..=16 {
            let t = (i as f64 / 16.0 - 0.5) * 0.0036; // ~400 m
            let wob = (rng.f64() - 0.5) * 0.00004;
            let id = next_id;
            next_id += 1;
            nodes.insert(
                id,
                OsmNode {
                    id,
                    lat: cl + t * ang.sin() + wob,
                    lon: co + t * ang.cos() / 0.7 + wob,
                    tags: HashMap::new(),
                    version: 1,
                },
            );
            ids.push(id);
        }
        ways.insert(
            wid,
            OsmWay {
                id: wid,
                nodes: ids,
                tags: tags(&[
                    ("highway", "residential"),
                    ("name", names[(wid % 8) as usize]),
                ]),
                version: 1,
            },
        );
    }
    Arc::new(OsmData {
        nodes,
        ways,
        relations: Vec::new(),
        bounds: Vec::new(),
    })
}

fn line_labels_section() {
    use osm_gpui::layers::line_labels::{Candidate, FrameOut, LabelStyle, LineLabelCache};
    println!("\n--- line labels (rotated glyph outlines) ---");
    let data = build_street_dataset();
    let layer = OsmLayer::new_with_data(LayerId(1), "streets", data);
    let bounds = gpui::Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(1600.0), px(1000.0)),
    };
    for z in [16.0, 17.0] {
        let vp = Viewport::new(CENTER_LAT, CENTER_LON, z, size(px(1600.0), px(1000.0)));
        let first = layer.line_label_frame(&vp, bounds);
        println!(
            "z{z}: {} labels placed, {} fill verts + {} halo verts ({:.0} + {:.0} per label), first frame built {} layouts",
            first.labels,
            first.fill.len(),
            first.halo.len(),
            first.fill.len() as f64 / first.labels.max(1) as f64,
            first.halo.len() as f64 / first.labels.max(1) as f64,
            first.layouts_built
        );
        bench(
            &format!("line_label_frame z{z} warm (cache hit, pan)"),
            50,
            || {
                let o = layer.line_label_frame(&vp, bounds);
                (o.fill.len() + o.halo.len()) as u64
            },
        );
        // Alternate between two zooms so every call is a cold layout.
        let vp2 = Viewport::new(
            CENTER_LAT,
            CENTER_LON,
            z + 0.01,
            size(px(1600.0), px(1000.0)),
        );
        let mut flip = false;
        bench(
            &format!("line_label_frame z{z} cold (zoom changed)"),
            20,
            || {
                flip = !flip;
                let o = layer.line_label_frame(if flip { &vp2 } else { &vp }, bounds);
                (o.fill.len() + o.halo.len()) as u64
            },
        );
    }

    // Vertex budget vs flatten tolerance, on a cache driven directly.
    let data = build_street_dataset();
    let vp = Viewport::new(CENTER_LAT, CENTER_LON, 16.0, size(px(1600.0), px(1000.0)));
    let proj = vp.projector();
    #[allow(clippy::type_complexity)]
    let mut verts: Vec<(i64, Vec<(i64, f64, f64)>, String)> = Vec::new();
    for w in data.ways.values() {
        let v: Vec<(i64, f64, f64)> = w
            .nodes
            .iter()
            .map(|id| {
                let n = &data.nodes[id];
                let (mx, my) = osm_gpui::coordinates::lat_lon_to_mercator(n.lat, n.lon);
                (*id, mx, my)
            })
            .collect();
        verts.push((w.id, v, w.tags["name"].clone()));
    }
    println!("tolerance sweep at z16 (fill_tol / halo_tol px):");
    for (ft, ht) in [(0.05, 0.1), (0.1, 0.2), (0.2, 0.4), (0.4, 0.8)] {
        let mut c = LineLabelCache::new(LabelStyle {
            fill_tol: ft,
            halo_tol: ht,
            ..LabelStyle::default()
        });
        let cands: Vec<Candidate> = verts
            .iter()
            .map(|(id, v, n)| Candidate {
                way_id: *id,
                text: n,
                verts: v,
            })
            .collect();
        let mut out = FrameOut::default();
        let t0 = Instant::now();
        c.frame(
            &cands,
            16.0,
            |x, y| proj.to_screen(x, y),
            (0.0, 0.0),
            (1600.0, 1000.0),
            &mut out,
        );
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let (mf, mh) = c.mesh_vertex_totals();
        println!(
            "  {ft:.2}/{ht:.2}: {} labels, {:.0} fill + {:.0} halo verts/label, {} cached glyph meshes ({mf} fill + {mh} halo verts total), cold {ms:.1} ms",
            out.labels,
            out.fill.len() as f64 / out.labels.max(1) as f64,
            out.halo.len() as f64 / out.labels.max(1) as f64,
            c.glyph_cache_len()
        );
    }
}

/// A circle of `n` nodes of radius `radius_deg` degrees around
/// `(lat, lon)`, wobbled a little so it isn't a perfect polygon. Node ids
/// start at `first_id`.
fn ring_nodes(
    rng: &mut Lcg,
    first_id: i64,
    n: usize,
    lat: f64,
    lon: f64,
    radius_deg: f64,
) -> Vec<OsmNode> {
    (0..n)
        .map(|i| {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            let r = radius_deg * (1.0 + (rng.f64() - 0.5) * 0.1);
            OsmNode {
                id: first_id + i as i64,
                lat: lat + r * a.sin(),
                lon: lon + r * a.cos() * 1.3,
                tags: HashMap::new(),
                version: 1,
            }
        })
        .collect()
}

/// `base` plus multipolygon relations: 200 lakes/forests (120-node outer ring
/// in 2 ways, one 40-node island) scattered over the same box, and one huge
/// lake (6000-node outer ring in 40 ways, 3000-node island).
fn with_multipolygons(base: &OsmData) -> OsmData {
    use osm_gpui::osm::{MemberType, OsmMember, OsmRelation};
    let mut rng = Lcg(7);
    let mut data = base.clone();
    let mut next_node: i64 = 10_000_000;
    let mut next_way: i64 = 1_000_000;
    let member = |way: i64, role: &str| OsmMember {
        member_type: MemberType::Way,
        reference: way,
        role: role.to_string(),
    };
    let add_ring = |data: &mut OsmData,
                    nodes: Vec<OsmNode>,
                    split: usize,
                    role: &str,
                    next_way: &mut i64|
     -> Vec<OsmMember> {
        let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
        for n in nodes {
            data.nodes.insert(n.id, n);
        }
        // `split` ways that chain around the ring, the last one closing it.
        let per = ids.len() / split;
        let mut members = Vec::new();
        for s in 0..split {
            let lo = s * per;
            let hi = if s + 1 == split {
                ids.len()
            } else {
                (s + 1) * per + 1
            };
            let mut way_nodes: Vec<i64> = ids[lo..hi].to_vec();
            if s + 1 == split {
                way_nodes.push(ids[0]);
            }
            let id = *next_way;
            *next_way += 1;
            data.ways.insert(
                id,
                OsmWay {
                    id,
                    nodes: way_nodes,
                    tags: HashMap::new(),
                    version: 1,
                },
            );
            members.push(member(id, role));
        }
        members
    };
    let mut relations = Vec::new();
    for r in 0..200i64 {
        let lat = CENTER_LAT + (rng.f64() - 0.5) * 0.045;
        let lon = CENTER_LON + (rng.f64() - 0.5) * 0.045;
        let outer = ring_nodes(&mut rng, next_node, 120, lat, lon, 0.0008);
        next_node += 120;
        let inner = ring_nodes(&mut rng, next_node, 40, lat, lon, 0.0003);
        next_node += 40;
        let mut members = add_ring(&mut data, outer, 2, "outer", &mut next_way);
        members.extend(add_ring(&mut data, inner, 1, "inner", &mut next_way));
        relations.push(OsmRelation {
            id: 1 + r,
            members,
            version: 1,
            tags: if r % 2 == 0 {
                tags(&[("type", "multipolygon"), ("natural", "water")])
            } else {
                tags(&[("type", "multipolygon"), ("landuse", "forest")])
            },
        });
    }
    let outer = ring_nodes(&mut rng, next_node, 6000, CENTER_LAT, CENTER_LON, 0.01);
    next_node += 6000;
    let inner = ring_nodes(&mut rng, next_node, 3000, CENTER_LAT, CENTER_LON, 0.004);
    let mut members = add_ring(&mut data, outer, 40, "outer", &mut next_way);
    members.extend(add_ring(&mut data, inner, 1, "inner", &mut next_way));
    relations.push(OsmRelation {
        id: HUGE_RELATION,
        members,
        version: 1,
        tags: tags(&[("type", "multipolygon"), ("natural", "water")]),
    });
    data.relations = relations;
    data
}

const HUGE_RELATION: i64 = 9_999;
/// A node of the huge lake's outer ring (the first node `with_multipolygons`
/// allocates after the 200 small relations' 200 x 160 nodes).
const HUGE_RING_NODE: i64 = 10_000_000 + 200 * 160 + 17;

/// The "before" and "after" for multipolygon rendering. `main` before this
/// change parsed relations and drew none of them, so a layer built from data
/// with its relations stripped does exactly the work `main` did; the layer
/// built from the full data adds assembly, triangulation and the fill pass.
fn multipolygon_section(base: &Arc<OsmData>) {
    let full = Arc::new(with_multipolygons(base));
    let stripped = {
        let mut d = (*full).clone();
        d.relations.clear();
        Arc::new(d)
    };
    println!(
        "\n-- multipolygons: {} relations (200 x ~160 nodes + one 9000-node lake in 41 ways) --",
        full.relations.len()
    );
    println!("   ('before' = relations stripped: the work main did; 'after' = relations drawn)");

    bench("load (new_with_data) BEFORE", 5, || {
        OsmLayer::new_with_data(LayerId(1), "b", stripped.clone()).has_data() as u64
    });
    bench("load (new_with_data) AFTER", 5, || {
        OsmLayer::new_with_data(LayerId(1), "a", full.clone()).has_data() as u64
    });

    let mut before = OsmLayer::new_with_data(LayerId(1), "before", stripped.clone());
    let mut after = OsmLayer::new_with_data(LayerId(1), "after", full.clone());

    let bounds = gpui::Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(1600.0), px(1000.0)),
    };
    let z14 = Viewport::new(CENTER_LAT, CENTER_LON, 14.0, size(px(1600.0), px(1000.0)));
    let z17 = Viewport::new(CENTER_LAT, CENTER_LON, 17.0, size(px(1600.0), px(1000.0)));
    let z17_edge = Viewport::new(
        CENTER_LAT,
        CENTER_LON + 0.0095,
        17.0,
        size(px(1600.0), px(1000.0)),
    );

    println!("\n   per frame, CPU side of the fill pass (cull + project + triangles):");
    bench("fill pass z14, everything visible: BEFORE (none)", 20, || 0);
    bench("fill pass z14, everything visible: AFTER", 20, || {
        after.multipolygon_fill_cpu(&z14, bounds) as u64
    });
    bench(
        "fill pass z17, center (huge lake hole): BEFORE (none)",
        20,
        || 0,
    );
    bench("fill pass z17, center (huge lake hole): AFTER", 20, || {
        after.multipolygon_fill_cpu(&z17, bounds) as u64
    });
    bench("fill pass z17, on the huge lake's rim: AFTER", 20, || {
        after.multipolygon_fill_cpu(&z17_edge, bounds) as u64
    });

    println!("\n   interaction:");
    let center = point(px(800.0), px(500.0));
    bench("hit_test_interior (double-click): BEFORE", 20, || {
        before.hit_test_interior(&z14, center).len() as u64
    });
    bench("hit_test_interior (double-click): AFTER", 20, || {
        after.hit_test_interior(&z14, center).len() as u64
    });

    println!("\n   edit (patch only the relations whose ways were touched):");
    bench(
        "commit_node_moves, node of a small lake: BEFORE",
        20,
        || {
            before.commit_node_moves(&[(10_000_000 + 5, CENTER_LAT + 0.01, CENTER_LON)]);
            before.is_modified() as u64
        },
    );
    bench("commit_node_moves, node of a small lake: AFTER", 20, || {
        after.commit_node_moves(&[(10_000_000 + 5, CENTER_LAT + 0.01, CENTER_LON)]);
        after.is_modified() as u64
    });
    bench(
        "commit_node_moves, node of the 9000-node lake: BEFORE",
        10,
        || {
            before.commit_node_moves(&[(HUGE_RING_NODE, CENTER_LAT + 0.0101, CENTER_LON)]);
            before.is_modified() as u64
        },
    );
    bench(
        "commit_node_moves, node of the 9000-node lake: AFTER",
        10,
        || {
            after.commit_node_moves(&[(HUGE_RING_NODE, CENTER_LAT + 0.0101, CENTER_LON)]);
            after.is_modified() as u64
        },
    );
    bench("commit_node_moves, node in no relation: BEFORE", 20, || {
        before.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
        before.is_modified() as u64
    });
    bench("commit_node_moves, node in no relation: AFTER", 20, || {
        after.commit_node_moves(&[(0, CENTER_LAT, CENTER_LON)]);
        after.is_modified() as u64
    });
}
