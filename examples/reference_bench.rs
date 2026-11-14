//! Load and draw cost of big GPX / GeoJSON files, without a window. Run with:
//!   cargo run --release --example reference_bench
//!
//! Generates a GPX of about 50 MB (a few long winding tracks plus waypoints)
//! and a GeoJSON of a few hundred thousand vertices in memory, times parsing,
//! then times `ReferenceLayer::build_scene` (cull, project, batch into paths)
//! at several zooms over the data. Painting and the GPU are not included.

use gpui::{point, px, size, Bounds};
use osm_gpui::coordinates::LatLon;
use osm_gpui::layers::reference_layer::ReferenceLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::viewport::Viewport;
use std::fmt::Write;
use std::time::Instant;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

fn wiggle(i: usize, k: f64) -> (f64, f64) {
    let t = i as f64 * 0.00002;
    (
        LAT + 0.05 * (t * 3.0 + k).sin() + 0.01 * k + 0.0003 * (t * 400.0).sin(),
        LON + t * 0.2 - 0.1 + 0.0003 * (t * 330.0).cos(),
    )
}

fn make_gpx(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 4096);
    s.push_str("<?xml version=\"1.0\"?>\n<gpx version=\"1.1\" creator=\"bench\">\n");
    for w in 0..200 {
        let (lat, lon) = wiggle(w * 700, 1.0);
        let _ = writeln!(
            s,
            "<wpt lat=\"{lat:.6}\" lon=\"{lon:.6}\"><name>wp {w}</name></wpt>"
        );
    }
    let mut k = 0;
    while s.len() < target_bytes {
        let _ = write!(s, "<trk><name>track {k}</name><trkseg>");
        for i in 0..100_000 {
            let (lat, lon) = wiggle(i, k as f64);
            let _ = writeln!(
                s,
                "<trkpt lat=\"{lat:.7}\" lon=\"{lon:.7}\"><ele>{:.1}</ele><time>2024-05-01T10:{:02}:{:02}Z</time></trkpt>",
                12.5 + (i % 50) as f64,
                (i / 60) % 60,
                i % 60
            );
        }
        s.push_str("</trkseg></trk>\n");
        k += 1;
    }
    s.push_str("</gpx>\n");
    s
}

fn make_geojson(vertices: usize) -> String {
    let mut s = String::from("{\"type\":\"FeatureCollection\",\"features\":[");
    let per = 500;
    let n = vertices / per;
    for f in 0..n {
        if f > 0 {
            s.push(',');
        }
        let _ = write!(
            s,
            "{{\"type\":\"Feature\",\"properties\":{{\"name\":\"f{f}\",\"id\":{f}}},\"geometry\":{{\"type\":\"LineString\",\"coordinates\":["
        );
        for i in 0..per {
            if i > 0 {
                s.push(',');
            }
            let (lat, lon) = wiggle(f * per + i, (f % 7) as f64);
            let _ = write!(s, "[{lon:.7},{lat:.7}]");
        }
        s.push_str("]}}");
    }
    s.push_str("]}");
    s
}

fn report(name: &str, layer: &ReferenceLayer) {
    let view = size(px(1064.0), px(876.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: view,
    };
    println!("{name}: zoom  median_ms  p95_ms  path_vertices  paths");
    for z in [8.0, 10.0, 12.0, 14.0, 16.0, 18.0] {
        let viewport = Viewport::new(LatLon::new(LAT + 0.01, LON), z, view);
        let first = layer.build_scene(&viewport, bounds);
        let (verts, paths) = (first.vertex_count(), first.path_count());
        let mut ms: Vec<f64> = (0..30)
            .map(|_| {
                let t = Instant::now();
                let scene = layer.build_scene(&viewport, bounds);
                let e = t.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(&scene);
                e
            })
            .collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "  {:<5} {:>9.3} {:>7.3} {:>13} {:>6}",
            z,
            ms[ms.len() / 2],
            ms[(ms.len() * 95 / 100).min(ms.len() - 1)],
            verts,
            paths
        );
    }
}

fn main() {
    let gpx = make_gpx(50 << 20);
    println!("GPX: {:.1} MB", gpx.len() as f64 / 1e6);
    let t = Instant::now();
    let data = osm_gpui::gpx::parse(gpx.as_bytes()).expect("gpx");
    println!(
        "GPX parse: {:.0} ms ({} vertices, {} parts, {} markers)",
        t.elapsed().as_secs_f64() * 1000.0,
        data.vertex_count,
        data.parts.len(),
        data.markers.len()
    );
    report("GPX", &ReferenceLayer::new(LayerId(1), "gpx", data));

    let gj = make_geojson(400_000);
    println!("GeoJSON: {:.1} MB", gj.len() as f64 / 1e6);
    let t = Instant::now();
    let data = osm_gpui::geojson::parse(gj.as_bytes()).expect("geojson");
    println!(
        "GeoJSON parse: {:.0} ms ({} vertices, {} parts)",
        t.elapsed().as_secs_f64() * 1000.0,
        data.vertex_count,
        data.parts.len()
    );
    report("GeoJSON", &ReferenceLayer::new(LayerId(2), "geojson", data));
}
