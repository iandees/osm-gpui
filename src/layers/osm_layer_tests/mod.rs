//! Tests for `OsmLayer`, one file per area; the fixtures several areas share live here.

mod batching;
mod chunked;
mod conflicts;
mod create_and_delete;
mod hit_testing;
mod marker_visibility;
mod moves_and_tags;
mod multipolygon;
mod node_way_primitives;
mod relation_overlay;
mod relations;
mod render_geometry;
mod upload_diff;
mod vertex_preview;
mod way_lod;

use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::viewport::Viewport;
use gpui::{px, size};
use std::collections::HashMap;
use std::sync::Arc;

fn empty_tags() -> HashMap<String, String> {
    HashMap::new()
}

/// Build a viewport whose center projects a chosen (lat, lon) to the middle
/// of an 800x600 map area. Zoom 18 is high enough that a degree-scale offset
/// in node positions translates to many pixels.
fn viewport_centered_on(lat: f64, lon: f64) -> Viewport {
    Viewport::new(lat, lon, 18.0, size(px(800.0), px(600.0)))
}

fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> Arc<OsmData> {
    let mut map = HashMap::new();
    for n in nodes {
        map.insert(n.id, n);
    }
    let mut way_map = HashMap::new();
    for w in ways {
        way_map.insert(w.id, w);
    }
    Arc::new(OsmData {
        nodes: map,
        ways: way_map,
        relations: Vec::new(),
        bounds: Vec::new(),
    })
}

fn tagged() -> HashMap<String, String> {
    let mut tags = HashMap::new();
    tags.insert("amenity".to_string(), "cafe".to_string());
    tags
}

/// One untagged way of `n` nodes 0.0001 degrees (about 8 m) apart along
/// latitude 40, plus a tagged node (id 1000) beside the middle one and an
/// untagged node on no way (id 2000) ten steps further along.
fn way_with_poi_layer(n: i64) -> OsmLayer {
    let mut nodes: Vec<OsmNode> = (1..=n)
        .map(|i| OsmNode {
            id: i,
            lat: 40.0,
            lon: -74.0 + i as f64 * 0.0001,
            version: 1,
            tags: empty_tags(),
        })
        .collect();
    nodes.push(OsmNode {
        id: 1000,
        lat: 40.0003,
        lon: -74.0 + (n / 2) as f64 * 0.0001,
        version: 1,
        tags: tagged(),
    });
    nodes.push(OsmNode {
        id: 2000,
        lat: 40.0003,
        lon: -74.0 + (n / 2 + 10) as f64 * 0.0001,
        version: 1,
        tags: empty_tags(),
    });
    let way = OsmWay {
        id: 100,
        nodes: (1..=n).collect(),
        version: 1,
        tags: HashMap::new(),
    };
    OsmLayer::new_with_data(LayerId(1), "L", data_with(nodes, vec![way]))
}

fn view_at(zoom: f64) -> Viewport {
    Viewport::new(
        40.0,
        -74.0 + 30.0 * 0.0001,
        zoom,
        size(px(800.0), px(600.0)),
    )
}

fn node_point(layer: &OsmLayer, vp: &Viewport, id: i64) -> gpui::Point<gpui::Pixels> {
    let idx = layer.node_cache.index_by_id[&id];
    let (_, mx, my) = layer.node_cache.flat[idx];
    vp.mercator_to_screen(mx, my)
}
