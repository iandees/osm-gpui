//! Node moves, tag edits and drag previews keep the layer's caches consistent.

use super::{data_with, empty_tags, viewport_centered_on};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::{EditError, EditableLayer};
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::selection::{FeatureKind, FeatureRef};
use gpui::{point, px, size, Bounds};
use std::sync::Arc;

fn bounds_data(rects: &[(f64, f64, f64, f64)]) -> OsmData {
    let mut d = (*data_with(vec![], vec![])).clone();
    d.bounds = rects
        .iter()
        .map(
            |&(min_lat, max_lat, min_lon, max_lon)| crate::osm::OsmBounds {
                min_lat,
                max_lat,
                min_lon,
                max_lon,
            },
        )
        .collect();
    d
}

#[test]
fn layer_without_bounds_has_no_download_rects() {
    let layer = OsmLayer::new_with_data(LayerId(1), "x", data_with(vec![], vec![]));
    assert!(layer.download_rects().is_empty());
}

#[test]
fn download_rects_follow_loading_and_merging() {
    let mut layer = OsmLayer::new_with_data(
        LayerId(1),
        "x",
        Arc::new(bounds_data(&[(40.0, 40.1, -74.1, -74.0)])),
    );
    assert_eq!(layer.download_rects().len(), 1);

    layer.merge_download(bounds_data(&[(40.1, 40.2, -74.1, -74.0)]));
    assert_eq!(layer.download_rects().len(), 2);
    let exported = layer.export_xml().unwrap();
    assert_eq!(exported.matches("<bounds ").count(), 2);

    // Merging data with no bounds keeps what is known.
    layer.merge_download(bounds_data(&[]));
    assert_eq!(layer.download_rects().len(), 2);
    // The merge isn't an edit.
    assert!(!layer.is_modified());

    layer.clear_osm_data();
    assert!(layer.download_rects().is_empty());
}

#[test]
fn way_node_ids_returns_members_in_order() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
    assert_eq!(layer.way_node_ids(999), None);
}

#[test]
fn node_lat_lon_reflects_current_data() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(layer.node_lat_lon(1), Some((40.0, -74.0)));
    assert_eq!(layer.node_lat_lon(999), None);
}

#[test]
fn commit_node_moves_updates_data_and_marks_modified() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 41.0,
        lon: -75.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert!(!layer.is_modified());
    layer.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();

    assert!(layer.is_modified());
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((40.5, -74.5))
    );
    // Untouched node is unaffected.
    assert_eq!(
        updated.nodes.get(&2).map(|n| (n.lat, n.lon)),
        Some((41.0, -75.0))
    );
}

#[test]
fn commit_node_moves_empty_is_noop() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer.commit_node_moves(&[]).unwrap();
    assert!(!layer.is_modified());
}

#[test]
fn set_tag_inserts_and_overwrites_on_node() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer
        .set_tag(FeatureKind::Node, 1, "highway", "residential")
        .unwrap();
    assert!(layer.is_modified());
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.nodes.get(&1).unwrap().tags.get("highway"),
        Some(&"residential".to_string())
    );

    layer
        .set_tag(FeatureKind::Node, 1, "highway", "trunk")
        .unwrap();
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.nodes.get(&1).unwrap().tags.get("highway"),
        Some(&"trunk".to_string())
    );
}

#[test]
fn set_tag_on_way() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer
        .set_tag(FeatureKind::Way, 10, "surface", "paved")
        .unwrap();
    assert!(layer.is_modified());
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.ways[&10].tags.get("surface"),
        Some(&"paved".to_string())
    );
}

#[test]
fn set_tag_missing_feature_id_is_noop() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(
        layer.set_tag(FeatureKind::Node, 999, "highway", "residential"),
        Err(EditError::UnknownNode(999))
    );
    assert!(!layer.is_modified());
}

#[test]
fn remove_tag_removes_existing_key() {
    let mut tags = empty_tags();
    tags.insert("highway".to_string(), "residential".to_string());
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer.remove_tag(FeatureKind::Node, 1, "highway").unwrap();
    assert!(layer.is_modified());
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(updated.nodes.get(&1).unwrap().tags.get("highway"), None);
}

#[test]
fn remove_tag_missing_feature_id_is_noop() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(
        layer.remove_tag(FeatureKind::Node, 999, "highway"),
        Err(EditError::UnknownNode(999))
    );
    assert!(!layer.is_modified());
}

#[test]
fn drag_preview_does_not_mutate_data() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let mut ids = std::collections::HashSet::new();
    ids.insert(1);
    layer.set_drag_preview(&ids, point(px(50.0), px(0.0)));

    assert!(!layer.is_modified());
    let unchanged = layer.get_osm_data().unwrap();
    assert_eq!(
        unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((40.0, -74.0))
    );

    layer.clear_drag_preview();
    let still_unchanged = layer.get_osm_data().unwrap();
    assert_eq!(
        still_unchanged.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((40.0, -74.0))
    );
}

/// Mutations now go through `Arc::make_mut` instead of always deep-
/// cloning `OsmData` — but that must not change observable data-sharing
/// semantics. A snapshot `Arc<OsmData>` taken *before* an edit (e.g. via
/// `get_osm_data`, exactly like an in-flight background save/export
/// would hold) still sees the pre-edit state afterward: `make_mut`
/// clones once, transparently, the moment it discovers a second live
/// `Arc` reference, so old snapshots are never mutated out from under
/// their holder. This is the same snapshot isolation the old
/// always-clone code gave by construction.
#[test]
fn snapshot_taken_before_edit_is_unaffected_by_make_mut() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    // Take a snapshot Arc *before* the edit — a second live reference to
    // the same underlying OsmData, exactly the case make_mut must clone
    // for rather than mutate in place.
    let snapshot = layer.get_osm_data().unwrap();
    assert_eq!(
        snapshot.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((40.0, -74.0))
    );

    layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();

    // The live layer sees the move...
    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((41.0, -75.0))
    );
    // ...but the old snapshot, held separately, still sees the original
    // position: it was never mutated in place.
    assert_eq!(
        snapshot.nodes.get(&1).map(|n| (n.lat, n.lon)),
        Some((40.0, -74.0))
    );

    // Also true for tag edits, not just node moves.
    let snapshot2 = layer.get_osm_data().unwrap();
    layer.set_tag(FeatureKind::Node, 1, "name", "Test").unwrap();
    assert_eq!(
        snapshot2.nodes.get(&1).and_then(|n| n.tags.get("name")),
        None
    );
    let updated2 = layer.get_osm_data().unwrap();
    assert_eq!(
        updated2
            .nodes
            .get(&1)
            .and_then(|n| n.tags.get("name"))
            .map(String::as_str),
        Some("Test")
    );
}

#[test]
fn commit_node_moves_updates_both_referencing_ways_and_indices() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let n3 = OsmNode {
        id: 3,
        lat: 40.002,
        lon: -74.002,
        tags: empty_tags(),
        version: 1,
    };
    // Both ways reference node 1.
    let way_a = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: empty_tags(),
        version: 1,
    };
    let way_b = OsmWay {
        id: 20,
        nodes: vec![1, 3],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2, n3], vec![way_a, way_b]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let old_bbox_a = layer.derived.way_bboxes[0];
    let old_bbox_b = layer.derived.way_bboxes[1];

    let new_lat = 41.0;
    let new_lon = -75.0;
    layer.commit_node_moves(&[(1, new_lat, new_lon)]).unwrap();

    assert!(layer.is_modified());

    // Both ways' cached bboxes must reflect the new position.
    assert_ne!(
        layer.derived.way_bboxes[0], old_bbox_a,
        "way A's bbox should have changed"
    );
    assert_ne!(
        layer.derived.way_bboxes[1], old_bbox_b,
        "way B's bbox should have changed"
    );

    // Both ways' cached vertex lists must carry node 1's new mercator
    // position.
    let (new_mx, new_my) = crate::coordinates::lat_lon_to_mercator(new_lat, new_lon);
    for verts in [
        &layer.derived.way_vertices[0],
        &layer.derived.way_vertices[1],
    ] {
        let &(_, mx, my) = verts.iter().find(|&&(id, _, _)| id == 1).unwrap();
        assert!((mx - new_mx).abs() < 1e-6);
        assert!((my - new_my).abs() < 1e-6);
    }

    // hit_test at the NEW location finds node 1 (and, via node priority,
    // stops there — but the way-vertex assertions above already prove
    // the way geometry moved too).
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 10,
    }]);
    let viewport_new = viewport_centered_on(new_lat, new_lon);
    let hits_new = layer.hit_test(&viewport_new, point(px(400.0), px(300.0)));
    assert!(
        hits_new
            .iter()
            .any(|h| h.kind == FeatureKind::Node && h.feature.id == 1),
        "expected node 1 at its new location: {:?}",
        hits_new
    );

    // hit_test at the OLD location must NOT find node 1 anymore.
    let viewport_old = viewport_centered_on(40.0, -74.0);
    let hits_old = layer.hit_test(&viewport_old, point(px(400.0), px(300.0)));
    assert!(
        !hits_old.iter().any(|h| h.feature.id == 1),
        "node 1 should no longer be hit-testable at its old location: {:?}",
        hits_old
    );
}

#[test]
fn adjacent_closed_ways_sharing_two_nodes_keep_node_to_ways_consistent() {
    // Two closed ways (rings) sharing an edge (nodes 1 and 2), mimicking
    // two adjacent building/parcel polygons. Each way's node list closes
    // the ring by repeating its first node id as its last id, so node 1
    // appears twice in way_a's own node list and node 2 appears twice in
    // way_b's — this is exactly the shape that exposed a duplicate/
    // dropped `node_to_ways` entry bug when one of the two ways is
    // dragged and the move is committed.
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let n3 = OsmNode {
        id: 3,
        lat: 40.002,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n4 = OsmNode {
        id: 4,
        lat: 40.001,
        lon: -73.999,
        tags: empty_tags(),
        version: 1,
    };
    // way_a: ring 1 -> 2 -> 3 -> 1 (closed).
    let way_a = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 1],
        tags: empty_tags(),
        version: 1,
    };
    // way_b: ring 1 -> 4 -> 2 -> 1 (closed), sharing the 1-2 edge with way_a.
    let way_b = OsmWay {
        id: 20,
        nodes: vec![1, 4, 2, 1],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2, n3, n4], vec![way_a, way_b]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let old_bbox_a = layer.derived.way_bboxes[0];
    let old_bbox_b = layer.derived.way_bboxes[1];

    // Drag+commit both shared nodes (1 and 2), as a whole-way move would.
    layer
        .commit_node_moves(&[(1, 41.0, -75.0), (2, 41.001, -75.001)])
        .unwrap();

    assert!(layer.is_modified());
    assert_ne!(
        layer.derived.way_bboxes[0], old_bbox_a,
        "way A's bbox should update"
    );
    assert_ne!(
        layer.derived.way_bboxes[1], old_bbox_b,
        "way B's bbox should update"
    );

    // Every node id must map to exactly the way indices that actually
    // reference it, with no duplicates and none dropped, despite each
    // way listing its ring-closing node twice.
    assert_eq!(
        layer.derived.node_to_ways.get(&1).cloned(),
        Some([0usize, 1usize].into_iter().collect())
    );
    assert_eq!(
        layer.derived.node_to_ways.get(&2).cloned(),
        Some([0usize, 1usize].into_iter().collect())
    );
    assert_eq!(
        layer.derived.node_to_ways.get(&3).cloned(),
        Some([0usize].into_iter().collect())
    );
    assert_eq!(
        layer.derived.node_to_ways.get(&4).cloned(),
        Some([1usize].into_iter().collect())
    );
}

#[test]
fn commit_node_moves_new_position_hit_testable_by_point_and_rect() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let new_lat = 50.0;
    let new_lon = -80.0;
    layer.commit_node_moves(&[(1, new_lat, new_lon)]).unwrap();

    let viewport = viewport_centered_on(new_lat, new_lon);

    let point_hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(
        point_hits.iter().any(|h| h.feature.id == 1),
        "got {:?}",
        point_hits
    );

    let rect = Bounds {
        origin: point(px(300.0), px(200.0)),
        size: size(px(200.0), px(200.0)),
    };
    let rect_hits = layer.hit_test_rect(&viewport, rect);
    assert!(
        rect_hits
            .iter()
            .any(|f| f.kind == FeatureKind::Node && f.id == 1),
        "got {:?}",
        rect_hits
    );
}

#[test]
fn commit_node_moves_node_not_in_any_way_only_touches_node_cache() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    // n3 is a standalone POI node, not referenced by any way.
    let n3 = OsmNode {
        id: 3,
        lat: 41.0,
        lon: -75.0,
        tags: empty_tags(),
        version: 1,
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2, n3], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let way_vertices_before = layer.derived.way_vertices.clone();
    let way_bboxes_before = layer.derived.way_bboxes.clone();

    let new_lat = 42.0;
    let new_lon = -76.0;
    layer.commit_node_moves(&[(3, new_lat, new_lon)]).unwrap();

    assert!(layer.is_modified());
    assert_eq!(
        layer.derived.way_vertices, way_vertices_before,
        "unrelated way vertices must be untouched"
    );
    assert_eq!(
        layer.derived.way_bboxes, way_bboxes_before,
        "unrelated way bboxes must be untouched"
    );

    let (mx, my) = crate::coordinates::lat_lon_to_mercator(new_lat, new_lon);
    let idx = *layer.derived.node_cache.index_by_id.get(&3).unwrap();
    let (_, cached_mx, cached_my) = layer.derived.node_cache.flat[idx];
    assert!((cached_mx - mx).abs() < 1e-6);
    assert!((cached_my - my).abs() < 1e-6);
}

#[test]
fn set_tag_refreshes_cached_way_style() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let default_style = layer.render.way_styles[0];
    layer
        .set_tag(FeatureKind::Way, 10, "highway", "residential")
        .unwrap();
    let updated_style = layer.render.way_styles[0];

    assert_ne!(
        default_style, updated_style,
        "cached way style must be re-resolved after a tag edit, not left stale"
    );
}

#[test]
fn set_tag_refreshes_cached_node_style() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let idx = *layer.derived.node_cache.index_by_id.get(&1).unwrap();
    let default_style = layer.derived.node_cache.styles[idx];
    layer
        .set_tag(FeatureKind::Node, 1, "amenity", "cafe")
        .unwrap();
    let updated_style = layer.derived.node_cache.styles[idx];

    assert_ne!(
        default_style, updated_style,
        "cached node style must be re-resolved after a tag edit, not left stale"
    );
}
