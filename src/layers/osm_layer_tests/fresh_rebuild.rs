//! Every edit primitive patches the layer's derived tables in place. After
//! each one the layer must hold exactly what a layer built from scratch on
//! the same data holds.

use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditError, EditableLayer, LayerId};
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::selection::FeatureKind;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Node, way and relation rules that make the style caches depend on other
/// objects: `:connection` on how many ways use a node, `:in-relation` on
/// relation membership, `[building]` on tags.
const CSS: &str = "way { width: 2; color: #888888; } \
    way[building] { fill-color: #aa5555; fill-opacity: 0.5; } \
    way:in-relation { width: 6; } \
    node { symbol-size: 3; } \
    node:connection { symbol-size: 6; } \
    node:tagged, node:unconnected { symbol-shape: cross; }";

fn tags(list: &[(&str, &str)]) -> HashMap<String, String> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node(id: i64, row: f64, col: f64, t: &[(&str, &str)]) -> OsmNode {
    OsmNode {
        id,
        lat: 40.0 + row * 0.001,
        lon: -74.0 + col * 0.001,
        version: 1,
        tags: tags(t),
    }
}

fn way(id: i64, nodes: &[i64], t: &[(&str, &str)]) -> OsmWay {
    OsmWay {
        id,
        nodes: nodes.to_vec(),
        version: 1,
        tags: tags(t),
    }
}

fn member(member_type: MemberType, reference: i64, role: &str) -> OsmMember {
    OsmMember {
        member_type,
        reference,
        role: role.to_string(),
    }
}

/// Road 100 `1-2-3-4` crossed at node 3 by road 101 `3-5-6` and path 106
/// `3-14` (a node in three ways), footway 105 `4-15` off its end node, a
/// building 102 whose closing node 6 is road 101's end, a water multipolygon
/// 200 (outer 103, inner 104), a bus route 201 over both roads and a turn
/// restriction 202 through node 3. Nodes 8 and 9 are in no way; node 7 is a
/// tagged point.
fn data() -> OsmData {
    let nodes = [
        node(1, 0.0, 0.0, &[]),
        node(2, 0.0, 1.0, &[]),
        node(3, 0.0, 2.0, &[]),
        node(4, 0.0, 3.0, &[]),
        node(5, 1.0, 2.0, &[]),
        node(6, 2.0, 2.0, &[]),
        node(7, 1.0, 0.0, &[("amenity", "bench")]),
        node(8, 1.0, 1.0, &[]),
        node(9, 1.0, 3.0, &[]),
        node(11, 2.0, 3.0, &[]),
        node(12, 3.0, 3.0, &[]),
        node(13, 3.0, 2.0, &[]),
        node(14, -1.0, 2.0, &[]),
        node(15, -1.0, 3.0, &[]),
        node(20, 10.0, 10.0, &[]),
        node(21, 10.0, 12.0, &[]),
        node(22, 12.0, 12.0, &[]),
        node(23, 12.0, 10.0, &[]),
        node(24, 10.5, 10.5, &[]),
        node(25, 10.5, 11.5, &[]),
        node(26, 11.5, 10.5, &[]),
    ];
    let ways = [
        way(100, &[1, 2, 3, 4], &[("highway", "residential")]),
        way(101, &[3, 5, 6], &[("highway", "service")]),
        way(102, &[6, 11, 12, 13, 6], &[("building", "yes")]),
        way(103, &[20, 21, 22, 23, 20], &[]),
        way(104, &[24, 25, 26, 24], &[]),
        way(105, &[4, 15], &[("highway", "footway")]),
        way(106, &[3, 14], &[("highway", "path")]),
    ];
    let relations = vec![
        OsmRelation {
            id: 200,
            version: 2,
            tags: tags(&[("type", "multipolygon"), ("natural", "water")]),
            members: vec![
                member(MemberType::Way, 103, "outer"),
                member(MemberType::Way, 104, "inner"),
            ],
        },
        OsmRelation {
            id: 201,
            version: 1,
            tags: tags(&[("type", "route"), ("route", "bus"), ("ref", "7")]),
            members: vec![
                member(MemberType::Way, 100, ""),
                member(MemberType::Way, 101, ""),
            ],
        },
        OsmRelation {
            id: 202,
            version: 1,
            tags: tags(&[("type", "restriction"), ("restriction", "no_left_turn")]),
            members: vec![
                member(MemberType::Way, 100, "from"),
                member(MemberType::Node, 3, "via"),
                member(MemberType::Way, 101, "to"),
            ],
        },
    ];
    OsmData {
        nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
        ways: ways.into_iter().map(|w| (w.id, w)).collect(),
        relations,
        bounds: Vec::new(),
    }
}

fn layer() -> OsmLayer {
    let mut l = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data()));
    l.set_stylesheet(Arc::new(crate::style::Stylesheet::parse(CSS).unwrap()));
    l
}

/// Every derived table, normalised so that two layers holding the same data
/// compare equal whatever order their parallel arrays happen to be in.
fn tables(l: &OsmLayer) -> Vec<(&'static str, String)> {
    let data = l.store.osm_data.as_ref().expect("data");
    let n = l.derived.way_ids.len();
    assert_eq!(n, data.ways.len(), "way_ids against the data's ways");
    assert_eq!(l.derived.way_bboxes.len(), n, "way_bboxes length");
    assert_eq!(l.derived.way_vertices.len(), n, "way_vertices length");
    assert_eq!(l.render.way_styles.len(), n, "way_styles length");
    assert_eq!(l.render.way_fill_tris.len(), n, "way_fill_tris length");
    assert_eq!(l.derived.way_id_to_index.len(), n, "way_id_to_index length");
    for (id, &i) in &l.derived.way_id_to_index {
        assert_eq!(l.derived.way_ids[i], *id, "way_id_to_index of way {id}");
    }
    let ways: BTreeMap<i64, _> = l
        .derived
        .way_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            (
                *id,
                (
                    l.derived.way_bboxes[i],
                    &l.derived.way_vertices[i],
                    l.render.way_styles[i],
                    &l.render.way_fill_tris[i],
                ),
            )
        })
        .collect();
    assert_eq!(ways.len(), n, "a way id appears twice in way_ids");

    let cache = &l.derived.node_cache;
    assert_eq!(cache.flat.len(), cache.styles.len(), "node cache arrays");
    assert_eq!(
        cache.flat.len(),
        cache.index_by_id.len(),
        "node cache index"
    );
    for (id, &i) in &cache.index_by_id {
        assert_eq!(cache.flat[i].0, *id, "node cache index of node {id}");
    }
    let nodes: BTreeMap<i64, _> = cache
        .flat
        .iter()
        .zip(&cache.styles)
        .map(|(&(id, x, y), style)| (id, (x, y, *style)))
        .collect();

    let mut node_index: Vec<_> = l
        .derived
        .node_index
        .iter()
        .map(|g| (g.data, g.geom()[0], g.geom()[1]))
        .collect();
    node_index.sort_by_key(|e| e.0);
    let mut way_index: Vec<_> = l
        .derived
        .way_index
        .iter()
        .map(|g| (g.data, g.geom().lower(), g.geom().upper()))
        .collect();
    way_index.sort_by_key(|e| e.0);

    let node_to_ways: BTreeMap<i64, BTreeSet<i64>> = l
        .derived
        .node_to_ways
        .iter()
        .filter(|(_, idx)| !idx.is_empty())
        .map(|(node, idx)| (*node, idx.iter().map(|&i| l.derived.way_ids[i]).collect()))
        .collect();

    let member_ways: BTreeSet<i64> = l.render.member_ways.iter().copied().collect();

    let mp = &l.derived.mp;
    let entries: BTreeMap<i64, _> = mp
        .entries
        .iter()
        .map(|(id, e)| {
            let polygons: Vec<_> = e
                .polygons
                .iter()
                .map(|p| (&p.verts, &p.ring_starts, &p.tris, p.bbox))
                .collect();
            let t: BTreeMap<_, _> = e.tags.iter().collect();
            (*id, (e.style, polygons, e.bbox, e.area, e.tagged, t))
        })
        .collect();
    let sorted = |m: &HashMap<i64, Vec<i64>>| -> BTreeMap<i64, Vec<i64>> {
        m.iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| {
                let mut v = v.clone();
                v.sort_unstable();
                (*k, v)
            })
            .collect()
    };

    vec![
        ("ways", format!("{ways:?}")),
        ("node cache", format!("{nodes:?}")),
        ("node_index", format!("{node_index:?}")),
        ("way_index", format!("{way_index:?}")),
        ("node_to_ways", format!("{node_to_ways:?}")),
        ("member_ways", format!("{member_ways:?}")),
        ("download_rects", format!("{:?}", l.render.download_rects)),
        ("multipolygons", format!("{entries:?}")),
        ("invalid multipolygons", format!("{:?}", mp.invalid)),
        (
            "multipolygon way_to_rels",
            format!("{:?}", sorted(&mp.way_to_rels)),
        ),
        (
            "multipolygon rel_ways",
            format!("{:?}", sorted(&mp.rel_ways)),
        ),
        ("routes", format!("{:?}", mp.routes)),
        ("route_ways", format!("{:?}", sorted(&mp.route_ways))),
        ("restrictions", format!("{:?}", mp.restrictions)),
        (
            "restriction node_to_rels",
            format!("{:?}", sorted(&mp.node_to_rels)),
        ),
        (
            "restriction rel_nodes",
            format!("{:?}", sorted(&mp.rel_nodes)),
        ),
    ]
}

/// The layer's derived tables equal those of a layer rebuilt from its data.
pub(super) fn assert_fresh(l: &OsmLayer, step: &str) {
    let data = l.store.osm_data.clone().expect("data");
    let mut fresh = OsmLayer::new_with_data(LayerId(99), "fresh", data);
    fresh.set_stylesheet(l.store.stylesheet.clone());
    let (got, want) = (tables(l), tables(&fresh));
    for ((name, got), (_, want)) in got.iter().zip(&want) {
        assert_eq!(got, want, "{step}: {name} differs from a fresh rebuild");
    }
    // The layer's bounding box is only ever extended, so after edits it may
    // be larger than a fresh one, never smaller.
    if let Some(f) = fresh.derived.layer_bbox {
        let b = l
            .derived
            .layer_bbox
            .unwrap_or_else(|| panic!("{step}: layer_bbox is missing"));
        assert!(
            b.min_x <= f.min_x && b.min_y <= f.min_y && b.max_x >= f.max_x && b.max_y >= f.max_y,
            "{step}: layer_bbox {b:?} does not cover the fresh {f:?}"
        );
    }
}

#[test]
#[should_panic(expected = "differs from a fresh rebuild")]
fn the_comparison_notices_a_stale_table() {
    let mut l = layer();
    l.derived.way_bboxes[0] = None;
    assert_fresh(&l, "a table patched wrongly");
}

fn start() -> OsmLayer {
    let l = layer();
    assert_fresh(&l, "the fixture");
    l
}

fn all_kinds_of_tag_edit_target() -> [(FeatureKind, i64); 8] {
    [
        (FeatureKind::Way, 100),
        (FeatureKind::Way, 102),
        (FeatureKind::Way, 103),
        (FeatureKind::Node, 7),
        (FeatureKind::Node, 8),
        (FeatureKind::Node, 3),
        (FeatureKind::Relation, 200),
        (FeatureKind::Relation, 201),
    ]
}

#[test]
fn commit_node_moves_matches_a_fresh_rebuild() {
    let mut l = start();
    let there = [
        (3, 40.0004, -73.9975),
        (22, 40.0125, -73.9875),
        (7, 40.0011, -74.0003),
        (8, 40.0013, -73.9990),
        (1, 39.9996, -74.0004),
    ];
    l.commit_node_moves(&there).unwrap();
    assert_fresh(&l, "after moving five nodes");
    let back = [
        (3, 40.0, -73.998),
        (22, 40.012, -73.988),
        (7, 40.001, -74.0),
        (8, 40.001, -73.999),
        (1, 40.0, -74.0),
    ];
    l.commit_node_moves(&back).unwrap();
    assert_fresh(&l, "after moving them back");
}

#[test]
fn set_tag_matches_a_fresh_rebuild() {
    let mut l = start();
    l.set_tag(FeatureKind::Way, 100, "highway", "primary")
        .unwrap();
    assert_fresh(&l, "retyping the road");
    l.set_tag(FeatureKind::Way, 102, "landuse", "forest")
        .unwrap();
    assert_fresh(&l, "tagging the building ring as a wood too");
    l.set_tag(FeatureKind::Way, 103, "building", "yes").unwrap();
    assert_fresh(&l, "tagging the multipolygon's outer way");
    l.set_tag(FeatureKind::Node, 7, "amenity", "cafe").unwrap();
    assert_fresh(&l, "retagging the point");
    l.set_tag(FeatureKind::Node, 8, "name", "x").unwrap();
    assert_fresh(&l, "tagging a loose node");
    l.set_tag(FeatureKind::Node, 3, "highway", "traffic_signals")
        .unwrap();
    assert_fresh(&l, "tagging the junction");
    l.set_tag(FeatureKind::Relation, 200, "natural", "wood")
        .unwrap();
    assert_fresh(&l, "retagging the multipolygon");
    l.set_tag(FeatureKind::Relation, 201, "route", "bicycle")
        .unwrap();
    assert_fresh(&l, "retagging the route");
    l.set_tag(
        FeatureKind::Relation,
        202,
        "restriction",
        "only_straight_on",
    )
    .unwrap();
    assert_fresh(&l, "retagging the restriction");
    assert_eq!(
        l.set_tag(FeatureKind::Way, 999, "a", "b"),
        Err(EditError::UnknownWay(999))
    );
    assert_fresh(&l, "tagging a way that is not there");
}

#[test]
fn remove_tag_matches_a_fresh_rebuild() {
    let mut l = start();
    for (kind, id, key) in [
        (FeatureKind::Way, 100, "highway"),
        (FeatureKind::Way, 102, "building"),
        (FeatureKind::Node, 7, "amenity"),
        (FeatureKind::Relation, 200, "natural"),
        (FeatureKind::Relation, 200, "type"),
        (FeatureKind::Relation, 201, "route"),
        (FeatureKind::Relation, 202, "restriction"),
        (FeatureKind::Node, 3, "no_such_key"),
    ] {
        l.remove_tag(kind, id, key).unwrap();
        assert_fresh(&l, &format!("removing {key} from {kind:?} {id}"));
    }
}

#[test]
fn tag_edits_match_a_fresh_rebuild_on_every_kind_of_target() {
    let mut l = start();
    for (kind, id) in all_kinds_of_tag_edit_target() {
        l.set_tag(kind, id, "note", "n").unwrap();
        assert_fresh(&l, &format!("noting {kind:?} {id}"));
        l.remove_tag(kind, id, "note").unwrap();
        assert_fresh(&l, &format!("un-noting {kind:?} {id}"));
    }
}

#[test]
fn create_node_matches_a_fresh_rebuild() {
    let mut l = start();
    let a = l.create_node(40.002, -73.999, None).unwrap();
    assert_fresh(&l, "after a node with a fresh id");
    let b = l.create_node(40.003, -73.997, Some(-500)).unwrap();
    assert_eq!(b, -500);
    assert_fresh(&l, "after a node with a forced id");
    assert_eq!(
        l.create_node(40.0, -74.0, Some(a)),
        Err(EditError::IdInUse(FeatureKind::Node, a))
    );
    assert_fresh(&l, "after refusing an id that is taken");
}

#[test]
fn add_node_matches_a_fresh_rebuild() {
    let mut l = start();
    l.add_node(40.002, -73.999);
    assert_fresh(&l, "after one node");
    l.add_node(40.2, -73.5);
    assert_fresh(&l, "after a node far outside the data");
}

#[test]
fn add_way_matches_a_fresh_rebuild() {
    let mut l = start();
    let a = l.add_node(40.002, -73.999);
    let b = l.add_node(40.002, -73.996);
    let c = l.add_node(40.004, -73.996);
    l.add_way(vec![1, a], vec![]);
    assert_fresh(&l, "after an untagged open way off the road");
    let ring = l.add_way(
        vec![a, b, c, a],
        vec![("building".to_string(), "yes".to_string())],
    );
    assert_fresh(&l, "after a closed building");
    l.add_way(
        vec![8, 9],
        vec![("highway".to_string(), "path".to_string())],
    );
    assert_fresh(&l, "after a way over two loose nodes");
    l.add_way(vec![ring], vec![]);
    assert_fresh(&l, "after a way naming a way id as a node");
}

#[test]
fn extend_way_matches_a_fresh_rebuild() {
    let mut l = start();
    l.extend_way(106, 8).unwrap();
    assert_fresh(&l, "extending a path onto a loose node");
    l.extend_way(100, 9).unwrap();
    assert_fresh(&l, "extending a route member road");
    l.extend_way(105, 3).unwrap();
    assert_fresh(&l, "extending onto the three-way junction");
    assert_eq!(l.extend_way(999, 3), Err(EditError::UnknownWay(999)));
    assert_fresh(&l, "extending a way that is not there");
}

#[test]
fn insert_node_into_way_matches_a_fresh_rebuild() {
    let mut l = start();
    l.insert_node_into_way(100, 1, 40.0003, -73.9985).unwrap();
    assert_fresh(&l, "into the middle of a road");
    l.insert_node_into_way(102, 2, 40.0025, -73.9965).unwrap();
    assert_fresh(&l, "into a closed ring");
    l.insert_node_into_way(103, 1, 40.0101, -73.9990).unwrap();
    assert_fresh(&l, "into a multipolygon member");
    l.insert_node_into_way(101, 3, 40.003, -73.998).unwrap();
    assert_fresh(&l, "at the end of a road");
}

#[test]
fn remove_node_matches_a_fresh_rebuild() {
    let mut l = start();
    l.remove_node(8).unwrap();
    assert_fresh(&l, "removing a loose node");
    l.remove_node(9).unwrap();
    assert_fresh(&l, "removing another");
    l.remove_node(7).unwrap();
    assert_fresh(&l, "removing the tagged point");
}

#[test]
fn remove_way_matches_a_fresh_rebuild() {
    let mut l = start();
    l.remove_way(105).unwrap();
    assert_fresh(&l, "removing a leaf footway");
    l.remove_way(100).unwrap();
    assert_fresh(&l, "removing the route and restriction member road");
    l.remove_way(103).unwrap();
    assert_fresh(&l, "removing the multipolygon's outer way");
    l.remove_way(102).unwrap();
    assert_fresh(&l, "removing the building");
}

#[test]
fn remove_node_from_way_matches_a_fresh_rebuild() {
    let mut l = start();
    l.remove_node_from_way(100, 3).unwrap();
    assert_fresh(&l, "dropping a road's end node");
    l.remove_node_from_way(102, 1).unwrap();
    assert_fresh(&l, "dropping a ring vertex");
    l.remove_node_from_way(101, 0).unwrap();
    assert_fresh(&l, "dropping a road's start node, a junction");
    l.remove_node_from_way(103, 1).unwrap();
    assert_fresh(&l, "dropping a multipolygon outer vertex");
}

#[test]
fn reverse_way_matches_a_fresh_rebuild() {
    let mut l = start();
    for id in [100, 102, 103, 104] {
        assert!(l.reverse_way(id).is_ok());
        assert_fresh(&l, &format!("reversing way {id}"));
    }
    assert_eq!(l.reverse_way(999), Err(EditError::UnknownWay(999)));
    assert_fresh(&l, "reversing a way that is not there");
}

#[test]
fn set_way_nodes_matches_a_fresh_rebuild() {
    let mut l = start();
    assert!(l.set_way_nodes(100, vec![1, 2, 3]).is_ok());
    assert_fresh(&l, "shortening a road");
    assert!(l.set_way_nodes(102, vec![6, 13, 12, 11, 6]).is_ok());
    assert_fresh(&l, "re-winding a ring");
    assert!(l.set_way_nodes(103, vec![21, 22, 23, 20, 21]).is_ok());
    assert_fresh(&l, "rotating a multipolygon's outer ring");
    assert!(l.set_way_nodes(101, vec![3, 5, 13, 6]).is_ok());
    assert_fresh(&l, "re-routing a road through a ring node");
    assert!(l.set_way_nodes(106, vec![8, 9]).is_ok());
    assert_fresh(&l, "moving a path onto loose nodes");
    assert_eq!(
        l.set_way_nodes(999, vec![1, 2]),
        Err(EditError::UnknownWay(999))
    );
    assert_fresh(&l, "setting the nodes of a way that is not there");
}

#[test]
fn delete_and_restore_feature_match_a_fresh_rebuild() {
    let mut l = start();
    let loose = l.delete_feature(FeatureKind::Node, 8).unwrap();
    assert_fresh(&l, "deleting a loose node");
    let footway = l.delete_feature(FeatureKind::Way, 105).unwrap();
    assert_fresh(&l, "deleting a footway");
    let path = l.delete_feature(FeatureKind::Way, 106).unwrap();
    assert_fresh(&l, "deleting a path");
    let end = l.delete_feature(FeatureKind::Node, 14).unwrap();
    assert_fresh(&l, "deleting the path's now unused end node");
    let outer = l.delete_feature(FeatureKind::Way, 103).unwrap();
    assert_fresh(&l, "deleting a multipolygon's outer way");
    let road = l.delete_feature(FeatureKind::Way, 100).unwrap();
    assert_fresh(&l, "deleting the route and restriction member road");

    assert_eq!(
        l.delete_feature(FeatureKind::Node, 3).unwrap_err(),
        EditError::NodeInWay(3)
    );
    assert_fresh(&l, "refusing a node that a way still uses");

    for (what, snapshot) in [
        ("the road", road),
        ("the outer way", outer),
        ("the path's end node", end),
        ("the path", path),
        ("the footway", footway),
        ("the loose node", loose),
    ] {
        l.restore_feature(snapshot).unwrap();
        assert_fresh(&l, &format!("restoring {what}"));
    }
}

#[test]
fn set_relation_members_matches_a_fresh_rebuild() {
    let mut l = start();
    l.set_relation_members(200, vec![member(MemberType::Way, 103, "outer")])
        .unwrap();
    assert_fresh(&l, "dropping the hole");
    l.set_relation_members(
        200,
        vec![
            member(MemberType::Way, 103, "outer"),
            member(MemberType::Way, 104, "inner"),
            member(MemberType::Way, 102, "inner"),
        ],
    )
    .unwrap();
    assert_fresh(&l, "adding a second hole outside the outer ring");
    l.set_relation_members(
        201,
        vec![
            member(MemberType::Way, 101, ""),
            member(MemberType::Way, 105, ""),
        ],
    )
    .unwrap();
    assert_fresh(&l, "re-routing the route");
    l.set_relation_members(
        202,
        vec![
            member(MemberType::Way, 100, "from"),
            member(MemberType::Node, 5, "via"),
            member(MemberType::Way, 101, "to"),
        ],
    )
    .unwrap();
    assert_fresh(&l, "moving the restriction's via node");
    assert_eq!(
        l.set_relation_members(999, vec![]),
        Err(EditError::UnknownRelation(999))
    );
    assert_fresh(&l, "setting the members of a relation that is not there");
}

#[test]
fn create_relation_matches_a_fresh_rebuild() {
    let mut l = start();
    let wood = l
        .create_relation(
            vec![
                ("type".to_string(), "multipolygon".to_string()),
                ("natural".to_string(), "wood".to_string()),
            ],
            vec![member(MemberType::Way, 102, "outer")],
        )
        .unwrap();
    assert_fresh(&l, "after a multipolygon over the building ring");
    let route = l
        .create_relation(
            vec![
                ("type".to_string(), "route".to_string()),
                ("route".to_string(), "foot".to_string()),
            ],
            vec![
                member(MemberType::Way, 105, ""),
                member(MemberType::Way, 106, ""),
            ],
        )
        .unwrap();
    assert_fresh(&l, "after a route over two leaf ways");
    l.create_relation(
        vec![("type".to_string(), "site".to_string())],
        vec![
            member(MemberType::Relation, wood, ""),
            member(MemberType::Relation, route, ""),
        ],
    )
    .unwrap();
    assert_fresh(&l, "after a relation of relations");
}

#[test]
fn delete_and_restore_relation_match_a_fresh_rebuild() {
    let mut l = start();
    let mut snapshots = Vec::new();
    for id in [202, 201, 200] {
        snapshots.push(l.delete_relation(id).unwrap());
        assert_fresh(&l, &format!("deleting relation {id}"));
    }
    for snapshot in snapshots.into_iter().rev() {
        let id = snapshot.relation.id;
        assert!(l.restore_relation(snapshot).is_ok());
        assert_fresh(&l, &format!("restoring relation {id}"));
    }
}

#[test]
fn purge_and_restore_purged_match_a_fresh_rebuild() {
    let mut l = start();
    let purged = l.purge_features(&[14, 8], &[106], &[201]);
    assert!(!purged.is_empty());
    assert_fresh(&l, "after purging a path, its end node and the route");
    let multipolygon = l.purge_features(&[24, 25, 26], &[104], &[200]);
    assert_fresh(&l, "after purging the hole and the multipolygon");
    l.restore_purged(multipolygon);
    assert_fresh(&l, "after restoring the multipolygon");
    l.restore_purged(purged);
    assert_fresh(&l, "after restoring the path and the route");
}

#[test]
fn merge_download_matches_a_fresh_rebuild() {
    let mut l = start();
    l.commit_node_moves(&[(3, 40.0004, -73.9975)]).unwrap();
    l.set_tag(FeatureKind::Way, 102, "name", "mine").unwrap();

    let mut incoming = data();
    incoming.nodes.get_mut(&7).unwrap().version = 2;
    incoming
        .nodes
        .insert(40, node(40, 20.0, 20.0, &[("shop", "bakery")]));
    incoming.nodes.insert(41, node(41, 20.0, 21.0, &[]));
    incoming.nodes.insert(42, node(42, 21.0, 21.0, &[]));
    incoming.nodes.insert(43, node(43, 21.0, 20.0, &[]));
    incoming
        .ways
        .insert(300, way(300, &[41, 42, 43, 41], &[("building", "yes")]));
    incoming.ways.get_mut(&101).unwrap().version = 2;
    incoming
        .ways
        .get_mut(&101)
        .unwrap()
        .tags
        .insert("highway".to_string(), "tertiary".to_string());
    incoming.relations.push(OsmRelation {
        id: 301,
        version: 1,
        tags: tags(&[("type", "multipolygon"), ("landuse", "grass")]),
        members: vec![member(MemberType::Way, 300, "outer")],
    });
    l.merge_download(incoming);
    assert_fresh(&l, "after merging a download over local edits");
}
