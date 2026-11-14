//! Path decimation, arrow placement, fill triangulation and area labels.

use super::{data_with, empty_tags};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditableLayer, LayerId};
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::selection::FeatureKind;
use crate::style::Stylesheet;
use gpui::{point, px};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn decimate_segments_skips_short_interior_but_keeps_endpoints() {
    // Interior points are within 1px of each other (well under the 1.0
    // threshold); the last point is far away. Decimation should collapse
    // the tightly-clustered interior points but the emitted segments
    // must still start at the true first point and end at the true
    // last point.
    let pts = vec![
        point(px(0.0), px(0.0)),
        point(px(0.2), px(0.0)),
        point(px(0.4), px(0.0)),
        point(px(0.6), px(0.0)),
        point(px(100.0), px(0.0)),
    ];
    let segments = super::super::decimate_segments(&pts, 1.0);
    assert_eq!(
        segments.first().unwrap().0,
        0,
        "first segment must anchor at the true first vertex"
    );
    assert_eq!(
        segments.last().unwrap().1,
        pts.len() - 1,
        "last segment must end at the true last vertex"
    );
    assert!(
        segments.len() < pts.len() - 1,
        "expected fewer emitted segments than raw vertex count: {:?}",
        segments
    );

    // The endpoint screen positions among the emitted segments must
    // exactly match the undecimated (raw) projected positions — no
    // rounding/averaging, decimation only skips emitting some segments.
    let (first_anchor, _) = segments[0];
    assert_eq!(pts[first_anchor], pts[0]);
    let (_, last_end) = *segments.last().unwrap();
    assert_eq!(pts[last_end], pts[pts.len() - 1]);
}

#[test]
fn decimate_segments_no_decimation_when_all_far_apart() {
    let pts = vec![
        point(px(0.0), px(0.0)),
        point(px(10.0), px(0.0)),
        point(px(20.0), px(0.0)),
    ];
    let segments = super::super::decimate_segments(&pts, 1.0);
    assert_eq!(segments, vec![(0, 1), (1, 2)]);
}

#[test]
fn decimate_segments_two_points_always_emits_even_if_close() {
    let pts = vec![point(px(0.0), px(0.0)), point(px(0.1), px(0.0))];
    let segments = super::super::decimate_segments(&pts, 1.0);
    assert_eq!(segments, vec![(0, 1)]);
}

#[test]
fn decimate_segments_empty_or_single_point_emits_nothing() {
    assert!(super::super::decimate_segments(&[], 1.0).is_empty());
    assert!(super::super::decimate_segments(&[point(px(0.0), px(0.0))], 1.0).is_empty());
}

#[test]
fn arrow_segments_space_arrows_along_the_path_and_skip_short_segments() {
    let at = |x: f32| point(px(x), px(0.0));
    // A short way still gets one arrow, on its only segment.
    assert_eq!(super::super::arrow_segments(&[at(0.0), at(60.0)]), vec![0]);
    // Too short to hold an arrow at all.
    assert!(super::super::arrow_segments(&[at(0.0), at(5.0)]).is_empty());
    // 100px segments: an arrow on each (spacing is under 100px).
    assert_eq!(
        super::super::arrow_segments(&[at(0.0), at(100.0), at(200.0), at(300.0)]),
        vec![0, 1, 2]
    );
    // 30px segments: one arrow, then roughly every fourth segment.
    let dense: Vec<_> = (0..12).map(|i| at(i as f32 * 30.0)).collect();
    assert_eq!(super::super::arrow_segments(&dense), vec![1, 5, 9]);
    assert!(super::super::arrow_segments(&[]).is_empty());
}

#[test]
fn arrow_triangle_points_along_the_segment_from_its_midpoint() {
    let p0 = point(px(0.0), px(0.0));
    let p1 = point(px(100.0), px(0.0));
    let xy = |p: gpui::Point<gpui::Pixels>| (f32::from(p.x), f32::from(p.y));

    let [tip, left, right] = super::super::arrow_triangle(p0, p1, false, 5.0, 3.0).unwrap();
    assert_eq!(xy(tip), (55.0, 0.0));
    assert_eq!(xy(left), (45.0, 3.0));
    assert_eq!(xy(right), (45.0, -3.0));

    // Reversed: same midpoint, tip toward p0.
    let [tip, ..] = super::super::arrow_triangle(p0, p1, true, 5.0, 3.0).unwrap();
    assert_eq!(xy(tip), (45.0, 0.0));

    assert!(super::super::arrow_triangle(p0, p0, false, 5.0, 3.0).is_none());
}

fn square_ring_data() -> Arc<OsmData> {
    // A small square around (40, -74): nodes 1-4, way 10 closed via ref 1.
    let mk = |id, dlat: f64, dlon: f64| OsmNode {
        id,
        lat: 40.0 + dlat,
        lon: -74.0 + dlon,
        version: 1,
        tags: empty_tags(),
    };
    let mut way_tags = HashMap::new();
    way_tags.insert("building".to_string(), "yes".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: way_tags,
    };
    data_with(
        vec![
            mk(1, 0.0, 0.0),
            mk(2, 0.0, 0.001),
            mk(3, 0.001, 0.001),
            mk(4, 0.001, 0.0),
        ],
        vec![way],
    )
}

fn fill_stylesheet() -> Arc<Stylesheet> {
    Arc::new(
        Stylesheet::parse("area[building] { fill-color: #808080; fill-opacity: 0.4; }").unwrap(),
    )
}

fn area_label_sheet() -> Arc<Stylesheet> {
    Arc::new(Stylesheet::parse("area|z17-[building], area[leisure] { text: name; }").unwrap())
}

#[test]
fn area_label_geometry_is_cached_and_follows_vertex_edits_only() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
    layer.set_stylesheet(area_label_sheet());
    let mut cache = crate::layers::labels::LabelCache::default();
    let key = (crate::layers::labels::KIND_WAY, 10);
    let validate = |layer: &OsmLayer, cache: &mut crate::layers::labels::LabelCache| {
        cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 18);
    };
    validate(&layer, &mut cache);
    assert!(
        layer.area_label_for(&mut cache, key, 18.0).is_none(),
        "no name, no label"
    );

    layer.set_tag(FeatureKind::Way, 10, "name", "Hall").unwrap();
    validate(&layer, &mut cache);
    let l = layer.area_label_for(&mut cache, key, 18.0).unwrap();
    assert_eq!(l.text.as_ref(), "Hall");
    cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 16);
    assert!(
        layer.area_label_for(&mut cache, key, 16.5).is_none(),
        "below the rule's zoom (a fresh bucket)"
    );
    validate(&layer, &mut cache);

    let g1 = layer.area_geom_for(&mut cache, key).unwrap();
    let g2 = layer.area_geom_for(&mut cache, key).unwrap();
    assert!(Arc::ptr_eq(&g1, &g2));
    assert_eq!(cache.areas.builds, 1, "computed once");

    // A tag edit leaves the geometry alone.
    layer
        .set_tag(FeatureKind::Way, 10, "name", "Hall 2")
        .unwrap();
    validate(&layer, &mut cache);
    assert_eq!(
        layer
            .area_label_for(&mut cache, key, 18.0)
            .unwrap()
            .text
            .as_ref(),
        "Hall 2"
    );
    let _ = layer.area_geom_for(&mut cache, key).unwrap();
    assert_eq!(cache.areas.builds, 1);

    // Moving a corner rebuilds it, and the pole moves.
    layer.commit_node_moves(&[(3, 40.004, -73.997)]).unwrap();
    validate(&layer, &mut cache);
    let g3 = layer.area_geom_for(&mut cache, key).unwrap();
    assert_eq!(cache.areas.builds, 2, "a vertex edit invalidates");
    assert!(g3.polys[0].bbox.2 > g1.polys[0].bbox.2);
    assert_ne!(
        (g3.polys[0].pole.x, g3.polys[0].pole.y),
        (g1.polys[0].pole.x, g1.polys[0].pole.y)
    );
}

#[test]
fn multipolygon_area_label_uses_the_relation_tags_and_avoids_holes() {
    use crate::osm::{MemberType, OsmMember, OsmRelation};
    let mk = |id, dlat: f64, dlon: f64| OsmNode {
        id,
        lat: 40.0 + dlat,
        lon: -74.0 + dlon,
        version: 1,
        tags: empty_tags(),
    };
    let way = |id, nodes: Vec<i64>| OsmWay {
        id,
        nodes,
        version: 1,
        tags: empty_tags(),
    };
    let mut base = (*data_with(
        vec![
            mk(1, 0.0, 0.0),
            mk(2, 0.0, 0.01),
            mk(3, 0.01, 0.01),
            mk(4, 0.01, 0.0),
            mk(5, 0.002, 0.002),
            mk(6, 0.002, 0.008),
            mk(7, 0.008, 0.008),
            mk(8, 0.008, 0.002),
        ],
        vec![way(10, vec![1, 2, 3, 4, 1]), way(11, vec![5, 6, 7, 8, 5])],
    ))
    .clone();
    let member = |reference, role: &str| OsmMember {
        member_type: MemberType::Way,
        reference,
        role: role.to_string(),
    };
    let mut tags = HashMap::new();
    for (k, v) in [
        ("type", "multipolygon"),
        ("leisure", "park"),
        ("name", "Ring Park"),
    ] {
        tags.insert(k.to_string(), v.to_string());
    }
    base.relations.push(OsmRelation {
        id: 500,
        version: 1,
        tags,
        members: vec![member(10, "outer"), member(11, "inner")],
    });
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
    layer.set_stylesheet(area_label_sheet());
    let mut cache = crate::layers::labels::LabelCache::default();
    cache.validate(layer.revision, Arc::as_ptr(&layer.stylesheet) as usize, 17);
    let key = (crate::layers::labels::KIND_REL, 500);
    let l = layer.area_label_for(&mut cache, key, 17.0).unwrap();
    assert_eq!(l.text.as_ref(), "Ring Park");
    assert_eq!(l.priority, 300);
    let g = layer.area_geom_for(&mut cache, key).unwrap();
    assert_eq!(g.polys.len(), 1);
    assert_eq!(g.polys[0].rings.len(), 2, "outer ring plus the hole");
    let hole_edge = crate::coordinates::lat_lon_to_mercator(40.002, -74.002);
    let hole_far = crate::coordinates::lat_lon_to_mercator(40.008, -73.992);
    let p = g.polys[0].pole;
    let in_hole = p.x > hole_edge.0 && p.x < hole_far.0 && p.y > hole_edge.1 && p.y < hole_far.1;
    assert!(!in_hole, "pole {p:?} is in the hole");
    // The member ways carry no tags, so they get no label of their own.
    assert!(layer
        .area_label_for(&mut cache, (crate::layers::labels::KIND_WAY, 10), 17.0)
        .is_none());
}

#[test]
fn closed_way_with_fill_style_gets_triangulated() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
    layer.set_stylesheet(fill_stylesheet());
    let tris = layer.way_fill_tris[0]
        .as_ref()
        .expect("closed building way must have fill triangles");
    // A quad triangulates into 2 triangles = 6 indices, all within the
    // 4-vertex deduped ring.
    assert_eq!(tris.len(), 6);
    assert!(tris.iter().all(|&i| i < 4));
}

#[test]
fn open_way_gets_no_fill_triangles() {
    let mut data = square_ring_data();
    Arc::make_mut(&mut data).ways.get_mut(&10).unwrap().nodes = vec![1, 2, 3, 4];
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn closed_way_without_fill_style_gets_no_triangles() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
    layer.set_stylesheet(Arc::new(
        Stylesheet::parse("way[building] { color: #808080; }").unwrap(),
    ));
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn set_tag_refreshes_fill_triangles() {
    let mut data = square_ring_data();
    Arc::make_mut(&mut data)
        .ways
        .get_mut(&10)
        .unwrap()
        .tags
        .clear();
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None, "no building tag yet");
    layer
        .set_tag(FeatureKind::Way, 10, "building", "yes")
        .unwrap();
    assert!(
        layer.way_fill_tris[0].is_some(),
        "tag edit must recompute fill"
    );
    layer.remove_tag(FeatureKind::Way, 10, "building").unwrap();
    assert_eq!(layer.way_fill_tris[0], None, "tag removal must clear fill");
}

#[test]
fn degenerate_ring_yields_no_fill_without_panic() {
    // All four ring nodes collinear.
    let mk = |id, dlon: f64| OsmNode {
        id,
        lat: 40.0,
        lon: -74.0 + dlon,
        version: 1,
        tags: empty_tags(),
    };
    let mut way_tags = HashMap::new();
    way_tags.insert("building".to_string(), "yes".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: way_tags,
    };
    let data = data_with(
        vec![mk(1, 0.0), mk(2, 0.001), mk(3, 0.002), mk(4, 0.003)],
        vec![way],
    );
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn delete_way_shifts_fill_cache() {
    // Two ways; deleting the first must shift the second's fill entry down.
    let mut data = square_ring_data();
    {
        let d = Arc::make_mut(&mut data);
        let mut tags = HashMap::new();
        tags.insert("building".to_string(), "yes".to_string());
        d.ways.insert(
            20,
            OsmWay {
                id: 20,
                nodes: vec![1, 2, 3, 1],
                version: 1,
                tags,
            },
        );
    }
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris.len(), 2);
    layer.delete_feature(FeatureKind::Way, 10).unwrap();
    assert_eq!(layer.way_fill_tris.len(), 1);
    assert!(
        layer.way_fill_tris[0].is_some(),
        "surviving way 20 (a closed triangle) keeps its fill entry"
    );
}
