//! Multipolygon assembly, area state and hit resolution in the layer.

use super::super::{MpState, OsmLayer};
use crate::coordinates::LatLon;
use crate::layers::{EditError, EditableLayer, LayerId};
use crate::multipolygon::Problem;
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::selection::{resolve_hits, FeatureKind};
use crate::viewport::Viewport;
use gpui::{point, px, size, Pixels, Point};
use std::collections::HashMap;
use std::sync::Arc;

/// Node at grid column `c`, row `r`: 0.001 degrees apart from (40, -74).
fn nid(c: i64, r: i64) -> i64 {
    1 + r * 100 + c
}

fn lat_lon(c: i64, r: i64) -> (f64, f64) {
    (40.0 + r as f64 * 0.001, -74.0 + c as f64 * 0.001)
}

/// A closed square of grid nodes, as a way's node list.
fn square(c0: i64, r0: i64, c1: i64, r1: i64) -> Vec<i64> {
    vec![
        nid(c0, r0),
        nid(c1, r0),
        nid(c1, r1),
        nid(c0, r1),
        nid(c0, r0),
    ]
}

fn way(id: i64, nodes: Vec<i64>) -> OsmWay {
    OsmWay {
        id,
        nodes,
        version: 1,
        tags: HashMap::new(),
    }
}

fn member(way_id: i64, role: &str) -> OsmMember {
    OsmMember {
        member_type: MemberType::Way,
        reference: way_id,
        role: role.to_string(),
    }
}

fn relation(id: i64, tags: &[(&str, &str)], members: Vec<OsmMember>) -> OsmRelation {
    OsmRelation {
        id,
        version: 1,
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        members,
    }
}

const LAKE_TAGS: [(&str, &str); 2] = [("type", "multipolygon"), ("natural", "water")];

/// A lake (way 10, 10x10 cells) with an island (way 11) as relation 500,
/// plus a two-way outer (ways 20 and 21 making a square east of the
/// lake) as relation 501, and a never-closing one (way 30 + 31) as 502.
fn layer() -> OsmLayer {
    let mut nodes = HashMap::new();
    for r in 0..=12 {
        for c in 0..=40 {
            let (lat, lon) = lat_lon(c, r);
            nodes.insert(
                nid(c, r),
                OsmNode {
                    id: nid(c, r),
                    lat,
                    lon,
                    version: 1,
                    tags: HashMap::new(),
                },
            );
        }
    }
    let mut ways = HashMap::new();
    for w in [
        way(10, square(0, 0, 10, 10)),
        way(11, square(3, 3, 6, 6)),
        // 15..=25 square split into a west and an east half.
        way(20, vec![nid(20, 0), nid(15, 0), nid(15, 10), nid(20, 10)]),
        way(21, vec![nid(20, 0), nid(25, 0), nid(25, 10), nid(20, 10)]),
        // Two halves of a ring that stops short of closing.
        way(30, vec![nid(30, 0), nid(35, 0), nid(35, 10)]),
        way(31, vec![nid(35, 10), nid(30, 10)]),
    ] {
        ways.insert(w.id, w);
    }
    let data = OsmData {
        nodes,
        ways,
        relations: vec![
            relation(
                500,
                &LAKE_TAGS,
                vec![member(10, "outer"), member(11, "inner")],
            ),
            relation(
                501,
                &[("type", "multipolygon"), ("landuse", "forest")],
                vec![member(20, "outer"), member(21, "outer")],
            ),
            relation(
                502,
                &[("type", "multipolygon"), ("landuse", "forest")],
                vec![member(30, "outer"), member(31, "outer")],
            ),
        ],
        bounds: Vec::new(),
    };
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
}

fn viewport() -> Viewport {
    Viewport::new(
        LatLon::new(40.006, -73.985),
        14.0,
        size(px(800.0), px(600.0)),
    )
}

fn at(viewport: &Viewport, c: f64, r: f64) -> Point<Pixels> {
    let p = viewport.geo_to_screen(LatLon::new(40.0 + r * 0.001, -74.0 + c * 0.001));
    point(p.x, p.y)
}

/// What a double-click at grid position (c, r) would select.
fn double_click(layer: &OsmLayer, c: f64, r: f64) -> Option<(FeatureKind, i64)> {
    let v = viewport();
    let p = at(&v, c, r);
    if !layer.hit_test(&v, p).is_empty() {
        return resolve_hits(vec![layer.hit_test(&v, p)]).map(|h| (h.kind, h.id));
    }
    resolve_hits(vec![layer.hit_test_interior(&v, p)]).map(|h| (h.kind, h.id))
}

fn valid(polygons: usize, holes: usize) -> MpState {
    MpState::Valid { polygons, holes }
}

#[test]
fn relations_assemble_when_the_layer_loads() {
    let layer = layer();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    assert_eq!(layer.multipolygon_state(501), valid(1, 0));
    assert_eq!(
        layer.multipolygon_state(502),
        MpState::Invalid(Problem::Unclosed)
    );
    assert_eq!(layer.multipolygon_state(999), MpState::NotArea);
    // The lake style has a fill, so it is triangulated: 4 outer + 4 hole
    // vertices make 8 triangles.
    let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
    assert_eq!(tris, 8);
}

#[test]
fn double_click_in_the_water_selects_the_relation_but_on_the_island_the_island() {
    let layer = layer();
    assert_eq!(
        double_click(&layer, 1.5, 8.5),
        Some((FeatureKind::Relation, 500))
    );
    // The island is a hole of the relation: the island way wins.
    assert_eq!(double_click(&layer, 4.5, 4.5), Some((FeatureKind::Way, 11)));
    assert_eq!(double_click(&layer, 11.5, 11.5), None);
}

#[test]
fn a_two_way_outer_is_selectable_by_its_interior() {
    let layer = layer();
    assert_eq!(
        double_click(&layer, 17.5, 5.5),
        Some((FeatureKind::Relation, 501))
    );
    assert_eq!(
        double_click(&layer, 22.5, 5.5),
        Some((FeatureKind::Relation, 501))
    );
}

#[test]
fn a_broken_relation_is_not_selectable_by_its_interior() {
    let layer = layer();
    assert_eq!(double_click(&layer, 32.5, 5.5), None);
}

#[test]
fn an_untagged_old_style_relation_leaves_the_click_to_its_outer_way() {
    let mut layer = layer();
    layer
        .remove_tag(FeatureKind::Relation, 500, "natural")
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    assert_eq!(double_click(&layer, 1.5, 8.5), Some((FeatureKind::Way, 10)));
}

#[test]
fn moving_a_member_node_rebuilds_only_the_relations_it_belongs_to() {
    let mut layer = layer();
    let before = layer.multipolygon_version();
    let (old, _) = layer.multipolygon_geometry(500, nid(10, 10)).unwrap();

    // A node of another relation's ring: this one's cache is untouched
    // but the version still counts the patch.
    layer
        .commit_node_moves(&[(nid(30, 0), 40.0, -73.969)])
        .unwrap();
    assert!(layer.multipolygon_version() > before);

    // A node no relation uses: no patch at all.
    let before = layer.multipolygon_version();
    layer
        .commit_node_moves(&[(nid(12, 12), 40.0121, -73.9881)])
        .unwrap();
    assert_eq!(layer.multipolygon_version(), before);

    layer
        .commit_node_moves(&[(nid(10, 10), 40.0105, -73.9895)])
        .unwrap();
    assert!(layer.multipolygon_version() > before);
    let (new, tris) = layer.multipolygon_geometry(500, nid(10, 10)).unwrap();
    assert_ne!(new, old);
    assert_eq!(tris, 8);
}

#[test]
fn reshaping_splitting_and_removing_member_ways_updates_the_relation() {
    let mut layer = layer();
    // Cutting the ring open invalidates the lake...
    let ring = square(0, 0, 10, 10);
    assert!(layer.set_way_nodes(10, ring[..4].to_vec()).is_ok());
    assert_eq!(
        layer.multipolygon_state(500),
        MpState::Invalid(Problem::Unclosed)
    );
    // (An open way has no interior either, so nothing is selectable.)
    assert_eq!(double_click(&layer, 1.5, 8.5), None);
    // ...and putting it back (the undo) restores it.
    assert!(layer.set_way_nodes(10, ring).is_ok());
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));

    // A member way cannot be deleted while the relation lists it.
    assert_eq!(
        layer.delete_feature(FeatureKind::Way, 11).unwrap_err(),
        EditError::InRelation {
            kind: FeatureKind::Way,
            id: 11,
            relation: 500
        }
    );
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    // Purging the island way (a member) leaves a relation with a missing
    // member; restoring it brings the hole back.
    let purged = layer.purge_features(&[], &[11], &[]);
    assert_eq!(
        layer.multipolygon_state(500),
        MpState::Invalid(Problem::MissingWay(11))
    );
    layer.restore_purged(purged);
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));

    // Adding a vertex to the outer way keeps it valid and re-triangulates.
    layer.insert_node_into_way(10, 1, 39.9997, -73.995).unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
    assert_eq!(tris, 9);
    layer.remove_node_from_way(10, 1).unwrap();
    let (_, tris) = layer.multipolygon_geometry(500, nid(0, 0)).unwrap();
    assert_eq!(tris, 8);
}

#[test]
fn changing_members_roles_and_tags_updates_the_relation() {
    let mut layer = layer();
    // Dropping the inner leaves a plain outer.
    let previous = layer
        .set_relation_members(500, vec![member(10, "outer")])
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 0));
    // Undo: the old member list is back, with its hole.
    layer.set_relation_members(500, previous).unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));

    // The island as an outer too: two polygons, no hole.
    layer
        .set_relation_members(500, vec![member(10, "outer"), member(11, "outer")])
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(2, 0));
    layer
        .set_relation_members(500, vec![member(10, "outer"), member(11, "inner")])
        .unwrap();

    // Removing the type makes it a plain relation; putting it back
    // makes it a multipolygon again.
    layer
        .remove_tag(FeatureKind::Relation, 500, "type")
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
    assert_eq!(double_click(&layer, 1.5, 8.5), Some((FeatureKind::Way, 10)));
    layer
        .set_tag(FeatureKind::Relation, 500, "type", "multipolygon")
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));

    // Delete and restore the relation (undo of a delete).
    let snapshot = layer.delete_relation(500).unwrap();
    assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
    assert!(layer.restore_relation(snapshot).is_ok());
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));

    // A newly created multipolygon appears at once.
    let id = layer
        .create_relation(
            vec![
                ("type".to_string(), "multipolygon".to_string()),
                ("building".to_string(), "yes".to_string()),
            ],
            vec![member(11, "outer")],
        )
        .unwrap();
    assert_eq!(layer.multipolygon_state(id), valid(1, 0));
}

#[test]
fn a_style_change_re_resolves_the_fill() {
    let mut layer = layer();
    assert!(layer.multipolygon_geometry(500, nid(0, 0)).unwrap().1 > 0);
    // No fill style for this tag: assembled (selectable) but not
    // triangulated.
    layer
        .set_tag(FeatureKind::Relation, 500, "natural", "bare_rock")
        .unwrap();
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    assert_eq!(layer.multipolygon_geometry(500, nid(0, 0)).unwrap().1, 0);
}

#[test]
fn the_cache_is_rebuilt_from_scratch_on_reload_and_dropped_on_clear() {
    let mut layer = layer();
    let data = layer.get_osm_data().unwrap();
    layer.set_osm_data(data);
    assert_eq!(layer.multipolygon_state(500), valid(1, 1));
    layer.clear_osm_data();
    assert_eq!(layer.multipolygon_state(500), MpState::NotArea);
}
