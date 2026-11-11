//! CPU time of the way pass on a real extract, without a window. Run with:
//!   cargo run --release --example way_bench -- path/to/extract.osm LAT LON
//!
//! Loads the file, builds the layer, then for a set of zooms builds the way
//! scene (`OsmLayer::build_way_scene`: cull, project, merge, batch into
//! paths) and prints the median and p95 of 30 builds in milliseconds and
//! the number of path vertices the scene hands to the GPU. The view is the
//! 1064 x 876 point map area of a 1400 x 900 window. This covers the CPU
//! half only; painting the paths and the GPU work are not included (use the
//! `frame_stats` script op in the real app for those).

use gpui::{point, px, size, Bounds};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::osm::OsmParser;
use osm_gpui::viewport::Viewport;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: way_bench FILE.osm LAT LON [ZOOM...]");
        std::process::exit(2);
    }
    let lat: f64 = args[2].parse().expect("LAT");
    let lon: f64 = args[3].parse().expect("LON");
    let zooms: Vec<f64> = if args.len() > 4 {
        args[4..].iter().map(|z| z.parse().expect("ZOOM")).collect()
    } else {
        vec![14.0, 15.0, 15.5, 16.0, 16.5, 17.0, 18.0, 18.5, 19.5]
    };

    let data = OsmParser::new().parse_file(&args[1]).expect("parse");
    println!("nodes {} ways {}", data.nodes.len(), data.ways.len());
    let layer = OsmLayer::new_with_data(LayerId(1), "bench", Arc::new(data));

    let view = size(px(1064.0), px(876.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: view,
    };
    println!("zoom    median_ms   p95_ms   path_vertices");
    for z in zooms {
        let viewport = Viewport::new(lat, lon, z, view);
        // Warm-up.
        let verts = layer.build_way_scene(&viewport, bounds).vertex_count();
        let mut ms: Vec<f64> = (0..30)
            .map(|_| {
                let t = Instant::now();
                let scene = layer.build_way_scene(&viewport, bounds);
                let e = t.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(&scene);
                e
            })
            .collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // With OSM_GPUI_FRAME_STATS=1 this also prints the per-phase table
        // (and the way counts) for the zoom just measured.
        osm_gpui::frame_stats::report(&format!("z{z}"));
        println!(
            "{:<6}  {:>9.2}  {:>7.2}  {:>13}",
            z,
            ms[ms.len() / 2],
            ms[(ms.len() * 95 / 100).min(ms.len() - 1)],
            verts
        );
    }
}
