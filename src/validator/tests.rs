use super::*;
use crate::osm::{OsmMember, OsmNode};

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node(id: i64, lat: f64, lon: f64, pairs: &[(&str, &str)]) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        version: 1,
        tags: tags(pairs),
    }
}

fn way(id: i64, nodes: &[i64], pairs: &[(&str, &str)]) -> OsmWay {
    OsmWay {
        id,
        nodes: nodes.to_vec(),
        version: 1,
        tags: tags(pairs),
    }
}

fn member(kind: MemberType, id: i64, role: &str) -> OsmMember {
    OsmMember {
        member_type: kind,
        reference: id,
        role: role.to_string(),
    }
}

fn relation(id: i64, members: Vec<OsmMember>, pairs: &[(&str, &str)]) -> OsmRelation {
    OsmRelation {
        id,
        members,
        version: 1,
        tags: tags(pairs),
    }
}

fn data(nodes: Vec<OsmNode>, ways: Vec<OsmWay>, relations: Vec<OsmRelation>) -> OsmData {
    OsmData {
        nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
        ways: ways.into_iter().map(|w| (w.id, w)).collect(),
        relations,
        bounds: Vec::new(),
    }
}

fn changed(nodes: &[i64], ways: &[i64], relations: &[i64]) -> Changes {
    Changes {
        nodes: nodes.iter().copied().collect(),
        ways: ways.iter().copied().collect(),
        relations: relations.iter().copied().collect(),
    }
}

fn run(d: &OsmData, c: &Changes) -> Vec<Finding> {
    validate(d, c)
}

fn of(findings: &[Finding], check: Check) -> Vec<&Finding> {
    findings.iter().filter(|f| f.check == check).collect()
}

fn has(findings: &[Finding], check: Check) -> bool {
    !of(findings, check).is_empty()
}

/// Four nodes 1..4 at the corners of a square near (40, -74), ids `base..`.
fn square(base: i64, lat: f64, lon: f64, size: f64) -> Vec<OsmNode> {
    vec![
        node(base, lat, lon, &[]),
        node(base + 1, lat, lon + size, &[]),
        node(base + 2, lat + size, lon + size, &[]),
        node(base + 3, lat + size, lon, &[]),
    ]
}

// ------------------------------------------------------------ untagged

#[test]
fn untagged_way_outside_relations_is_a_warning() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.001, &[])],
        vec![way(10, &[1, 2], &[])],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    let hits = of(&f, Check::UntaggedWay);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);
    assert_eq!(hits[0].features, vec![(FeatureKind::Way, 10)]);
}

#[test]
fn untagged_way_in_a_relation_or_tagged_is_fine() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.001, &[])],
        vec![
            way(10, &[1, 2], &[]),
            way(11, &[1, 2], &[("highway", "path")]),
        ],
        vec![relation(
            50,
            vec![member(MemberType::Way, 10, "outer")],
            &[("type", "multipolygon")],
        )],
    );
    let f = run(&d, &changed(&[], &[10, 11], &[]));
    assert!(!has(&f, Check::UntaggedWay));
}

#[test]
fn untagged_node_alone_is_a_warning() {
    let d = data(vec![node(1, 0.0, 0.0, &[])], vec![], vec![]);
    let f = run(&d, &changed(&[1], &[], &[]));
    let hits = of(&f, Check::UntaggedNode);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);
}

#[test]
fn untagged_node_in_a_way_or_relation_or_tagged_is_fine() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, 0.0, 0.002, &[]),
            node(4, 0.0, 0.003, &[("amenity", "bench")]),
        ],
        vec![way(10, &[1, 2], &[("highway", "path")])],
        vec![relation(
            50,
            vec![member(MemberType::Node, 3, "stop")],
            &[("type", "route")],
        )],
    );
    let f = run(&d, &changed(&[1, 2, 3, 4], &[], &[]));
    assert!(!has(&f, Check::UntaggedNode));
}

// --------------------------------------------------------------- shape

#[test]
fn way_with_fewer_than_two_nodes_is_an_error() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[])],
        vec![
            way(10, &[1], &[("highway", "path")]),
            way(11, &[], &[("highway", "path")]),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10, 11], &[]));
    let hits = of(&f, Check::WayTooShort);
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.severity == Severity::Error));
}

#[test]
fn way_with_two_nodes_is_long_enough() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.001, &[])],
        vec![way(10, &[1, 2], &[("highway", "path")])],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    assert!(!has(&f, Check::WayTooShort));
    assert!(!has(&f, Check::ClosedWayTooShort));
}

#[test]
fn closed_way_with_fewer_than_four_refs_is_an_error() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.001, &[])],
        vec![way(10, &[1, 2, 1], &[("landuse", "grass")])],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    let hits = of(&f, Check::ClosedWayTooShort);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Error);
    assert!(!has(&f, Check::WayTooShort));
}

#[test]
fn proper_closed_way_and_open_three_node_way_are_fine() {
    let mut nodes = square(1, 0.0, 0.0, 0.001);
    nodes.push(node(9, 0.0, 0.002, &[]));
    let d = data(
        nodes,
        vec![
            way(10, &[1, 2, 3, 4, 1], &[("building", "yes")]),
            way(11, &[1, 2, 9], &[("highway", "path")]),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10, 11], &[]));
    assert!(!has(&f, Check::ClosedWayTooShort));
    assert!(!has(&f, Check::WayTooShort));
}

#[test]
fn consecutive_duplicate_node_is_an_error() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, 0.0, 0.002, &[]),
        ],
        vec![way(10, &[1, 2, 2, 3], &[("highway", "path")])],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    let hits = of(&f, Check::DuplicateWayNode);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Error);
}

#[test]
fn a_node_repeated_later_or_closing_a_ring_is_not_a_consecutive_duplicate() {
    let nodes = square(1, 0.0, 0.0, 0.001);
    let d = data(
        nodes,
        vec![
            way(10, &[1, 2, 3, 4, 1], &[("building", "yes")]),
            way(11, &[1, 2, 1, 3], &[("highway", "path")]),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10, 11], &[]));
    assert!(!has(&f, Check::DuplicateWayNode));
}

// ----------------------------------------------------- duplicate nodes

#[test]
fn new_node_on_top_of_an_old_one_is_a_warning() {
    let d = data(
        vec![
            node(1, 40.0, -74.0, &[("amenity", "cafe")]),
            node(-1, 40.0, -74.0, &[("shop", "bakery")]),
        ],
        vec![],
        vec![],
    );
    let f = run(&d, &changed(&[-1], &[], &[]));
    let hits = of(&f, Check::DuplicateNodes);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);
    assert_eq!(
        hits[0].features,
        vec![(FeatureKind::Node, -1), (FeatureKind::Node, 1)]
    );
}

#[test]
fn two_untouched_nodes_at_one_position_are_not_blamed() {
    let d = data(
        vec![
            node(1, 40.0, -74.0, &[("amenity", "cafe")]),
            node(2, 40.0, -74.0, &[("shop", "bakery")]),
            node(3, 40.5, -74.5, &[("shop", "bakery")]),
        ],
        vec![],
        vec![],
    );
    let f = run(&d, &changed(&[3], &[], &[]));
    assert!(!has(&f, Check::DuplicateNodes));
}

#[test]
fn nodes_a_hair_apart_are_not_duplicates() {
    let d = data(
        vec![
            node(1, 40.0, -74.0, &[("amenity", "cafe")]),
            node(2, 40.000_000_2, -74.0, &[("shop", "bakery")]),
        ],
        vec![],
        vec![],
    );
    let f = run(&d, &changed(&[2], &[], &[]));
    assert!(!has(&f, Check::DuplicateNodes));
}

#[test]
fn three_nodes_at_one_position_are_one_finding() {
    let d = data(
        vec![
            node(1, 40.0, -74.0, &[("a", "b")]),
            node(2, 40.0, -74.0, &[("a", "b")]),
            node(3, 40.0, -74.0, &[("a", "b")]),
        ],
        vec![],
        vec![],
    );
    let f = run(&d, &changed(&[1, 2], &[], &[]));
    let hits = of(&f, Check::DuplicateNodes);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].features.len(), 3);
}

// ------------------------------------------------------------- crossing

/// A horizontal way `id` from (lat, lon0) to (lat, lon1) through two own
/// nodes, and a vertical one from (lat0, lon) to (lat1, lon).
fn cross_data(a_tags: &[(&str, &str)], b_tags: &[(&str, &str)]) -> OsmData {
    data(
        vec![
            node(1, 0.0, -0.001, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, -0.001, 0.0, &[]),
            node(4, 0.001, 0.0, &[]),
        ],
        vec![way(10, &[1, 2], a_tags), way(11, &[3, 4], b_tags)],
        vec![],
    )
}

fn crossings(a: &[(&str, &str)], b: &[(&str, &str)]) -> usize {
    let f = run(&cross_data(a, b), &changed(&[], &[10], &[]));
    of(&f, Check::CrossingWays).len()
}

#[test]
fn crossing_pairs_that_matter_are_warned_about() {
    let road = [("highway", "residential")];
    let path = [("highway", "footway")];
    let rail = [("railway", "rail")];
    let river = [("waterway", "river")];
    let house = [("building", "yes")];
    assert_eq!(crossings(&road, &path), 1, "highway x highway");
    assert_eq!(crossings(&road, &rail), 1, "highway x railway");
    assert_eq!(crossings(&rail, &road), 1, "railway x highway");
    assert_eq!(crossings(&road, &river), 1, "highway x waterway");
    assert_eq!(crossings(&house, &road), 1, "building x highway");
    assert_eq!(crossings(&road, &house), 1, "highway x building");
    assert_eq!(crossings(&house, &house), 1, "building x building");
    let f = run(&cross_data(&road, &path), &changed(&[], &[10], &[]));
    let hit = &of(&f, Check::CrossingWays)[0];
    assert_eq!(hit.severity, Severity::Warning);
    assert_eq!(
        hit.features,
        vec![(FeatureKind::Way, 10), (FeatureKind::Way, 11)]
    );
}

#[test]
fn crossings_between_other_kinds_are_not_checked() {
    let road = [("highway", "residential")];
    let rail = [("railway", "rail")];
    let river = [("waterway", "river")];
    let house = [("building", "yes")];
    assert_eq!(crossings(&rail, &rail), 0);
    assert_eq!(crossings(&rail, &river), 0);
    assert_eq!(crossings(&river, &river), 0);
    assert_eq!(crossings(&house, &river), 0);
    assert_eq!(crossings(&house, &rail), 0);
    assert_eq!(crossings(&road, &[("landuse", "grass")]), 0);
    assert_eq!(crossings(&road, &[]), 0);
}

#[test]
fn bridges_tunnels_layers_and_fords_explain_a_crossing() {
    let road = [("highway", "residential")];
    let river = [("waterway", "river")];
    assert_eq!(
        crossings(&[("highway", "primary"), ("bridge", "yes")], &river),
        0
    );
    assert_eq!(
        crossings(&river, &[("highway", "primary"), ("tunnel", "culvert")]),
        0
    );
    assert_eq!(
        crossings(
            &[("highway", "service"), ("tunnel", "building_passage")],
            &[("building", "yes")]
        ),
        0
    );
    assert_eq!(
        crossings(&road, &[("highway", "path"), ("bridge", "viaduct")]),
        0
    );
    // A different layer is a deliberate crossing.
    assert_eq!(crossings(&road, &[("highway", "path"), ("layer", "1")]), 0);
    // The same layer written two ways still crosses.
    assert_eq!(crossings(&road, &[("highway", "path"), ("layer", "0")]), 1);
    assert_eq!(
        crossings(
            &[("highway", "path"), ("layer", "1")],
            &[("highway", "path"), ("layer", "1")]
        ),
        1
    );
    // bridge=no is not a bridge.
    assert_eq!(
        crossings(&road, &[("highway", "path"), ("bridge", "no")]),
        1
    );
    // A ford is where a road meets a stream.
    assert_eq!(
        crossings(&[("highway", "track"), ("ford", "yes")], &river),
        0
    );
    // Unbuilt roads and trams in the road are not worth a warning.
    assert_eq!(crossings(&[("highway", "construction")], &road), 0);
    assert_eq!(crossings(&[("railway", "tram")], &road), 0);
}

#[test]
fn ways_sharing_a_node_at_the_crossing_do_not_cross() {
    let d = data(
        vec![
            node(1, 0.0, -0.001, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, -0.001, 0.0, &[]),
            node(4, 0.001, 0.0, &[]),
            node(5, 0.0, 0.0, &[]),
        ],
        vec![
            way(10, &[1, 5, 2], &[("highway", "residential")]),
            way(11, &[3, 5, 4], &[("highway", "residential")]),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    assert!(!has(&f, Check::CrossingWays));
}

#[test]
fn ways_that_do_not_meet_do_not_cross() {
    let d = data(
        vec![
            node(1, 0.0, -0.001, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, 0.002, 0.0, &[]),
            node(4, 0.004, 0.0, &[]),
        ],
        vec![
            way(10, &[1, 2], &[("highway", "residential")]),
            way(11, &[3, 4], &[("highway", "residential")]),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    assert!(!has(&f, Check::CrossingWays));
}

#[test]
fn an_old_crossing_nobody_touched_is_not_reported() {
    // The two crossing roads are untouched; only a distant node changed.
    let mut d = cross_data(&[("highway", "residential")], &[("highway", "path")]);
    d.nodes
        .insert(99, node(99, 5.0, 5.0, &[("amenity", "bench")]));
    let f = run(&d, &changed(&[99], &[], &[]));
    assert!(!has(&f, Check::CrossingWays));
}

#[test]
fn moving_a_node_of_a_way_checks_that_way_for_crossings() {
    // Only node 2 changed (it was dragged across the other road); the way
    // itself is not in the change set but is affected.
    let d = cross_data(&[("highway", "residential")], &[("highway", "path")]);
    let f = run(&d, &changed(&[2], &[], &[]));
    assert_eq!(of(&f, Check::CrossingWays).len(), 1);
}

#[test]
fn a_crossing_between_two_changed_ways_is_reported_once() {
    let d = cross_data(&[("highway", "residential")], &[("highway", "path")]);
    let f = run(&d, &changed(&[], &[10, 11], &[]));
    assert_eq!(of(&f, Check::CrossingWays).len(), 1);
}

#[test]
fn a_building_ring_crossing_a_road_is_found_on_any_segment() {
    let mut nodes = square(1, 0.0, 0.0, 0.002);
    nodes.push(node(20, 0.001, -0.001, &[]));
    nodes.push(node(21, 0.001, 0.001, &[]));
    let d = data(
        nodes,
        vec![
            way(10, &[1, 2, 3, 4, 1], &[("building", "yes")]),
            way(11, &[20, 21], &[("highway", "service")]),
        ],
        vec![],
    );
    // Road runs from outside the house to the middle of it: one crossing.
    let f = run(&d, &changed(&[], &[10], &[]));
    assert_eq!(of(&f, Check::CrossingWays).len(), 1);
}

// ----------------------------------------------------------- area tags

fn open_way_with(pairs: &[(&str, &str)]) -> Vec<Finding> {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, 0.001, 0.001, &[]),
        ],
        vec![way(10, &[1, 2, 3], pairs)],
        vec![],
    );
    run(&d, &changed(&[], &[10], &[]))
}

#[test]
fn area_tags_on_an_open_way_are_a_warning() {
    for pairs in [
        vec![("building", "yes")],
        vec![("landuse", "residential")],
        vec![("natural", "water")],
        vec![("area", "yes"), ("highway", "pedestrian")],
        vec![("leisure", "park")],
    ] {
        let f = open_way_with(&pairs);
        let hits = of(&f, Check::AreaTagsOnOpenWay);
        assert_eq!(hits.len(), 1, "{pairs:?}");
        assert_eq!(hits[0].severity, Severity::Warning);
    }
}

#[test]
fn line_tags_and_explicit_non_areas_on_an_open_way_are_fine() {
    for pairs in [
        vec![("highway", "residential")],
        vec![("natural", "coastline")],
        vec![("natural", "tree_row")],
        vec![("waterway", "river")],
        vec![("power", "line")],
        vec![("barrier", "fence")],
        vec![("building", "yes"), ("area", "no")],
        vec![("building", "no")],
        vec![("highway", "service"), ("amenity", "parking_aisle")],
        vec![],
    ] {
        let f = open_way_with(&pairs);
        assert!(!has(&f, Check::AreaTagsOnOpenWay), "{pairs:?}");
    }
}

#[test]
fn area_tags_on_a_closed_way_are_fine() {
    let d = data(
        square(1, 0.0, 0.0, 0.001),
        vec![way(10, &[1, 2, 3, 4, 1], &[("building", "yes")])],
        vec![],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    assert!(!has(&f, Check::AreaTagsOnOpenWay));
}

#[test]
fn an_open_tagged_way_that_is_part_of_a_multipolygon_is_fine() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.001, &[]),
            node(3, 0.001, 0.001, &[]),
        ],
        vec![way(10, &[1, 2, 3], &[("landuse", "forest")])],
        vec![relation(
            50,
            vec![member(MemberType::Way, 10, "outer")],
            &[("type", "multipolygon")],
        )],
    );
    let f = run(&d, &changed(&[], &[10], &[]));
    assert!(!has(&f, Check::AreaTagsOnOpenWay));
}

// ------------------------------------------------------- multipolygons

fn mp_data(members: Vec<OsmMember>, extra_ways: Vec<OsmWay>) -> OsmData {
    let mut nodes = square(1, 0.0, 0.0, 0.002);
    nodes.extend(square(11, 0.0005, 0.0005, 0.001));
    let mut ways = vec![
        way(100, &[1, 2, 3, 4, 1], &[]),
        way(101, &[11, 12, 13, 14, 11], &[]),
    ];
    ways.extend(extra_ways);
    data(
        nodes,
        ways,
        vec![relation(
            50,
            members,
            &[("type", "multipolygon"), ("natural", "water")],
        )],
    )
}

#[test]
fn a_good_multipolygon_has_no_findings() {
    let d = mp_data(
        vec![
            member(MemberType::Way, 100, "outer"),
            member(MemberType::Way, 101, "inner"),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn multipolygon_without_an_outer_member_is_a_warning() {
    let d = mp_data(vec![member(MemberType::Way, 101, "inner")], vec![]);
    let f = run(&d, &changed(&[], &[], &[50]));
    let hits = of(&f, Check::MultipolygonNoOuter);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);
    assert_eq!(hits[0].features, vec![(FeatureKind::Relation, 50)]);
}

#[test]
fn an_empty_role_counts_as_outer_but_is_still_a_role_problem() {
    let d = mp_data(vec![member(MemberType::Way, 100, "")], vec![]);
    let f = run(&d, &changed(&[], &[], &[50]));
    assert!(!has(&f, Check::MultipolygonNoOuter));
    assert_eq!(of(&f, Check::MultipolygonRole).len(), 1);
}

#[test]
fn unknown_roles_are_a_warning_and_good_roles_are_not() {
    let d = mp_data(
        vec![
            member(MemberType::Way, 100, "outer"),
            member(MemberType::Way, 101, "hole"),
        ],
        vec![],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    let hits = of(&f, Check::MultipolygonRole);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);

    let good = mp_data(
        vec![
            member(MemberType::Way, 100, "outer"),
            member(MemberType::Way, 101, "inner"),
        ],
        vec![],
    );
    assert!(!has(
        &run(&good, &changed(&[], &[], &[50])),
        Check::MultipolygonRole
    ));
}

#[test]
fn rings_that_do_not_close_are_a_warning() {
    // Two open halves that never meet again.
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.002, &[]),
            node(3, 0.002, 0.002, &[]),
        ],
        vec![way(100, &[1, 2, 3], &[])],
        vec![relation(
            50,
            vec![member(MemberType::Way, 100, "outer")],
            &[("type", "multipolygon")],
        )],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    let hits = of(&f, Check::MultipolygonOpenRing);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].severity, Severity::Warning);
}

#[test]
fn a_ring_made_of_two_ways_closes() {
    let d = data(
        square(1, 0.0, 0.0, 0.002),
        vec![way(100, &[1, 2, 3], &[]), way(101, &[3, 4, 1], &[])],
        vec![relation(
            50,
            vec![
                member(MemberType::Way, 100, "outer"),
                member(MemberType::Way, 101, "outer"),
            ],
            &[("type", "multipolygon")],
        )],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    assert!(!has(&f, Check::MultipolygonOpenRing));
}

#[test]
fn a_multipolygon_whose_member_way_changed_is_checked_but_an_untouched_one_is_not() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.002, &[]),
            node(3, 0.002, 0.002, &[]),
            node(9, 3.0, 3.0, &[("amenity", "bench")]),
        ],
        vec![way(100, &[1, 2, 3], &[])],
        vec![relation(
            50,
            vec![member(MemberType::Way, 100, "outer")],
            &[("type", "multipolygon")],
        )],
    );
    assert!(has(
        &run(&d, &changed(&[], &[100], &[])),
        Check::MultipolygonOpenRing
    ));
    assert!(has(
        &run(&d, &changed(&[2], &[], &[])),
        Check::MultipolygonOpenRing
    ));
    assert!(!has(
        &run(&d, &changed(&[9], &[], &[])),
        Check::MultipolygonOpenRing
    ));
}

#[test]
fn only_multipolygons_get_the_ring_checks() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.002, &[])],
        vec![way(100, &[1, 2], &[("highway", "path")])],
        vec![relation(
            50,
            vec![member(MemberType::Way, 100, "")],
            &[("type", "route")],
        )],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    assert!(!has(&f, Check::MultipolygonRole));
    assert!(!has(&f, Check::MultipolygonNoOuter));
    assert!(!has(&f, Check::MultipolygonOpenRing));
}

// ----------------------------------------------------------- relations

#[test]
fn relation_without_members_or_type_is_a_warning() {
    let d = data(vec![], vec![], vec![relation(50, vec![], &[])]);
    let f = run(&d, &changed(&[], &[], &[50]));
    assert_eq!(of(&f, Check::RelationEmpty).len(), 1);
    assert_eq!(of(&f, Check::RelationNoType).len(), 1);
    assert!(f.iter().all(|x| x.severity == Severity::Warning));
}

#[test]
fn relation_with_members_and_type_is_fine() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[])],
        vec![],
        vec![relation(
            50,
            vec![member(MemberType::Node, 1, "stop")],
            &[("type", "route")],
        )],
    );
    let f = run(&d, &changed(&[], &[], &[50]));
    assert!(!has(&f, Check::RelationEmpty));
    assert!(!has(&f, Check::RelationNoType));
}

#[test]
fn an_untouched_empty_relation_is_not_blamed() {
    let d = data(vec![], vec![], vec![relation(50, vec![], &[])]);
    let f = run(&d, &changed(&[1], &[], &[]));
    assert!(f.is_empty());
}

// ---------------------------------------------------------------- tags

fn tag_findings(pairs: &[(&str, &str)]) -> Vec<Finding> {
    let d = data(vec![node(1, 0.0, 0.0, pairs)], vec![], vec![]);
    run(&d, &changed(&[1], &[], &[]))
}

#[test]
fn empty_key_and_empty_value_are_errors() {
    let f = tag_findings(&[("", "x")]);
    assert_eq!(of(&f, Check::TagEmptyKey).len(), 1);
    assert_eq!(of(&f, Check::TagEmptyKey)[0].severity, Severity::Error);
    let f = tag_findings(&[("name", "")]);
    assert_eq!(of(&f, Check::TagEmptyValue).len(), 1);
    assert_eq!(of(&f, Check::TagEmptyValue)[0].severity, Severity::Error);
    assert!(!has(&tag_findings(&[("name", "x")]), Check::TagEmptyValue));
    assert!(!has(&tag_findings(&[("name", "x")]), Check::TagEmptyKey));
}

#[test]
fn keys_and_values_over_255_characters_are_errors() {
    let long = "a".repeat(256);
    let max = "a".repeat(255);
    let f = tag_findings(&[("name", &long)]);
    assert_eq!(of(&f, Check::TagTooLong).len(), 1);
    assert_eq!(of(&f, Check::TagTooLong)[0].severity, Severity::Error);
    assert!(has(&tag_findings(&[(&long, "x")]), Check::TagTooLong));
    assert!(!has(&tag_findings(&[("name", &max)]), Check::TagTooLong));
    assert!(!has(&tag_findings(&[(&max, "x")]), Check::TagTooLong));
    // Characters, not bytes: 255 two-byte characters fit.
    let wide = "é".repeat(255);
    assert!(!has(&tag_findings(&[("name", &wide)]), Check::TagTooLong));
}

#[test]
fn leading_and_trailing_whitespace_is_a_warning() {
    for pairs in [
        [("name", " Cafe")],
        [("name", "Cafe ")],
        [(" name", "Cafe")],
        [("name\t", "Cafe")],
    ] {
        let f = tag_findings(&pairs);
        let hits = of(&f, Check::TagWhitespace);
        assert_eq!(hits.len(), 1, "{pairs:?}");
        assert_eq!(hits[0].severity, Severity::Warning);
    }
    // Inside is fine.
    assert!(!has(
        &tag_findings(&[("name", "Cafe Roma")]),
        Check::TagWhitespace
    ));
}

#[test]
fn fixme_on_a_changed_object_is_a_warning() {
    for key in ["fixme", "FIXME"] {
        let f = tag_findings(&[(key, "check the name")]);
        let hits = of(&f, Check::Fixme);
        assert_eq!(hits.len(), 1, "{key}");
        assert_eq!(hits[0].severity, Severity::Warning);
    }
    assert!(!has(&tag_findings(&[("note", "fixme")]), Check::Fixme));
}

#[test]
fn fixme_on_an_unchanged_object_is_not_reported() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[("fixme", "x")]), node(2, 1.0, 1.0, &[])],
        vec![],
        vec![],
    );
    let f = run(&d, &changed(&[2], &[], &[]));
    assert!(!has(&f, Check::Fixme));
}

#[test]
fn tag_checks_cover_ways_and_relations_too() {
    let d = data(
        vec![node(1, 0.0, 0.0, &[]), node(2, 0.0, 0.001, &[])],
        vec![way(10, &[1, 2], &[("highway", "path"), ("name", "")])],
        vec![relation(
            50,
            vec![member(MemberType::Way, 10, "")],
            &[("type", "route"), ("ref", " 5")],
        )],
    );
    let f = run(&d, &changed(&[], &[10], &[50]));
    assert!(of(&f, Check::TagEmptyValue)
        .iter()
        .any(|x| x.features == vec![(FeatureKind::Way, 10)]));
    assert!(of(&f, Check::TagWhitespace)
        .iter()
        .any(|x| x.features == vec![(FeatureKind::Relation, 50)]));
}

#[test]
fn deprecated_tags_are_a_warning_with_the_replacement() {
    for (k, v) in [
        ("highway", "ford"),
        ("landuse", "farm"),
        ("amenity", "public_building"),
        ("building", "entrance"),
        ("power", "sub_station"),
    ] {
        let f = tag_findings(&[(k, v)]);
        let hits = of(&f, Check::DeprecatedTag);
        assert_eq!(hits.len(), 1, "{k}={v}");
        assert_eq!(hits[0].severity, Severity::Warning);
        assert!(
            hits[0].message.contains(&format!("{k}={v}")),
            "{}",
            hits[0].message
        );
    }
    let f = tag_findings(&[("highway", "ford")]);
    assert!(of(&f, Check::DeprecatedTag)[0].message.contains("ford=yes"));
}

#[test]
fn current_tags_are_not_deprecated() {
    for (k, v) in [
        ("highway", "residential"),
        ("landuse", "farmland"),
        ("power", "substation"),
        ("ford", "yes"),
        ("entrance", "main"),
        ("building", "yes"),
    ] {
        assert!(
            !has(&tag_findings(&[(k, v)]), Check::DeprecatedTag),
            "{k}={v}"
        );
    }
}

// ------------------------------------------------------ whole validator

#[test]
fn nothing_changed_means_nothing_to_report() {
    // Full of old problems, none of them touched.
    let d = data(
        vec![node(1, 0.0, 0.0, &[("name", "")]), node(2, 0.0, 0.0, &[])],
        vec![way(10, &[1], &[])],
        vec![relation(50, vec![], &[])],
    );
    assert!(run(&d, &Changes::default()).is_empty());
}

#[test]
fn changes_naming_objects_that_are_gone_are_skipped() {
    // A deleted object is not in the data.
    let d = data(vec![], vec![], vec![]);
    assert!(run(&d, &changed(&[1], &[2], &[3])).is_empty());
}

#[test]
fn findings_come_errors_first_then_in_a_stable_order() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[]),
            node(2, 0.0, 0.001, &[("name", "")]),
            node(3, 0.0, 0.002, &[("fixme", "x")]),
        ],
        vec![way(10, &[1], &[("highway", "path")])],
        vec![],
    );
    let c = changed(&[1, 2, 3], &[10], &[]);
    let f = run(&d, &c);
    assert_eq!(f, run(&d, &c));
    let first_warning = f
        .iter()
        .position(|x| x.severity == Severity::Warning)
        .unwrap();
    assert!(f[..first_warning]
        .iter()
        .all(|x| x.severity == Severity::Error));
    assert!(f[first_warning..]
        .iter()
        .all(|x| x.severity == Severity::Warning));
    assert!(first_warning >= 2, "{f:?}");
}

#[test]
fn every_message_is_translated() {
    let d = data(
        vec![
            node(
                1,
                0.0,
                0.0,
                &[
                    ("fixme", "x"),
                    ("highway", "ford"),
                    ("", "v"),
                    ("k", ""),
                    (" s", "v"),
                ],
            ),
            node(2, 0.0, 0.0, &[("a", "b")]),
            node(3, 0.0, 0.002, &[]),
            node(4, 0.002, 0.002, &[]),
            node(5, -0.001, 0.001, &[]),
            node(6, 0.001, 0.001, &[]),
            node(7, 0.001, -0.001, &[]),
            node(8, 0.0, -0.001, &[]),
            node(9, 0.0, 0.003, &[]),
        ],
        vec![
            way(10, &[1], &[("building", "yes"), ("note", &"x".repeat(300))]),
            way(11, &[3, 3, 4], &[("highway", "path")]),
            way(12, &[3, 4, 3], &[("landuse", "grass")]),
            way(13, &[5, 6], &[("highway", "path")]),
            way(14, &[8, 9], &[("highway", "path")]),
            way(15, &[3, 4], &[("area", "yes")]),
            way(16, &[7, 6], &[]),
            way(17, &[3, 9], &[]),
        ],
        vec![
            relation(50, vec![], &[]),
            relation(
                51,
                vec![member(MemberType::Way, 16, "")],
                &[("type", "multipolygon")],
            ),
            relation(
                52,
                vec![member(MemberType::Way, 16, "inner")],
                &[("type", "multipolygon")],
            ),
        ],
    );
    let all_nodes: Vec<i64> = (1..=9).collect();
    let f = run(
        &d,
        &changed(&all_nodes, &[10, 11, 12, 13, 14, 15, 16, 17], &[50, 51, 52]),
    );
    let seen: HashSet<Check> = f.iter().map(|x| x.check).collect();
    for check in Check::ALL {
        // UntaggedNode is covered by `untagged_node_alone_is_a_warning`.
        if matches!(check, Check::UntaggedNode) {
            continue;
        }
        assert!(seen.contains(&check), "no finding for {}", check.id());
    }
    for x in &f {
        assert!(!x.message.is_empty());
        assert!(!x.message.contains("validator."), "{}", x.message);
        assert!(!x.message.contains("%{"), "{}", x.message);
    }
}

#[test]
fn check_ids_are_unique_and_round_trip() {
    let mut ids = HashSet::new();
    for check in Check::ALL {
        assert!(ids.insert(check.id()), "{}", check.id());
        assert_eq!(Check::from_id(check.id()), Some(check));
    }
    assert_eq!(Check::from_id("nope"), None);
}

#[test]
fn severities_split_errors_from_warnings() {
    let errors: Vec<&str> = Check::ALL
        .iter()
        .filter(|c| c.severity() == Severity::Error)
        .map(|c| c.id())
        .collect();
    assert_eq!(
        errors,
        vec![
            "way_too_short",
            "closed_way_too_short",
            "duplicate_way_node",
            "tag_empty_key",
            "tag_empty_value",
            "tag_too_long",
        ]
    );
}

#[test]
fn changes_from_a_diff_include_created_and_modified_but_not_deleted() {
    let diff = LayerDiff {
        created_nodes: vec![node(-1, 0.0, 0.0, &[])],
        modified_nodes: vec![node(2, 0.0, 0.0, &[])],
        deleted_node_ids: vec![(3, 1)],
        created_ways: vec![way(-10, &[], &[])],
        modified_ways: vec![way(11, &[], &[])],
        deleted_way_ids: vec![(12, 1)],
        created_relations: vec![relation(-20, vec![], &[])],
        modified_relations: vec![relation(21, vec![], &[])],
        deleted_relation_ids: vec![(22, 1)],
    };
    let c = Changes::from_diff(&diff);
    assert_eq!(c, changed(&[-1, 2], &[-10, 11], &[-20, 21]));
}

#[test]
fn report_counts_and_labels() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[("name", "")]),
            node(2, 0.0, 0.0, &[("fixme", "x")]),
        ],
        vec![],
        vec![],
    );
    let diff = LayerDiff {
        modified_nodes: vec![d.nodes[&1].clone(), d.nodes[&2].clone()],
        ..Default::default()
    };
    let mut report = Report::default();
    report.add_layer(LayerId(1), &d, &diff);
    assert!(report.checked_changes);
    assert_eq!(report.errors(), 1);
    assert_eq!(report.warnings(), 2); // the fixme, and the two nodes' shared position
    assert_eq!(report.count_of(Check::DuplicateNodes), 1);
    assert!(report.has_errors());
    assert_eq!(report.entries[0].finding.severity, Severity::Error);
    assert_eq!(report.entries[0].label, "Node 1");

    let empty = Report::default();
    assert!(empty.is_empty() && !empty.checked_changes && !empty.has_errors());
}

#[test]
fn feature_labels_use_the_name_when_there_is_one() {
    let d = data(
        vec![
            node(1, 0.0, 0.0, &[("name", "Fountain")]),
            node(2, 0.0, 0.0, &[("name", " ")]),
        ],
        vec![way(10, &[1, 2], &[("name", "Main Street")])],
        vec![relation(50, vec![], &[("name", "Route 5")])],
    );
    assert_eq!(feature_label(&d, FeatureKind::Node, 1), "Node 1 (Fountain)");
    assert_eq!(feature_label(&d, FeatureKind::Node, 2), "Node 2");
    assert_eq!(
        feature_label(&d, FeatureKind::Way, 10),
        "Way 10 (Main Street)"
    );
    assert_eq!(
        feature_label(&d, FeatureKind::Relation, 50),
        "Relation 50 (Route 5)"
    );
    assert_eq!(feature_label(&d, FeatureKind::Way, 99), "Way 99");
}

// ------------------------------------------------------------- scaling

/// A grid of `side` x `side` nodes joined into short streets, as a stand-in
/// for a big download. Returns the data and the ids of `modified` ways.
fn big_grid(side: i64, modified: usize) -> (OsmData, Changes) {
    let mut nodes = Vec::new();
    let mut ways = Vec::new();
    for y in 0..side {
        for x in 0..side {
            nodes.push(node(
                y * side + x + 1,
                40.0 + y as f64 * 0.0003,
                -74.0 + x as f64 * 0.0003,
                &[],
            ));
        }
    }
    let mut way_id = 1;
    // Rows of 10 nodes, and the same for columns, as roads; every tenth row a
    // building-ish ring is skipped to keep the data realistic enough.
    for y in 0..side {
        for chunk in (0..side).step_by(9) {
            let ids: Vec<i64> = (chunk..(chunk + 10).min(side))
                .map(|x| y * side + x + 1)
                .collect();
            if ids.len() >= 2 {
                ways.push(way(way_id, &ids, &[("highway", "residential")]));
                way_id += 1;
            }
        }
    }
    for x in 0..side {
        for chunk in (0..side).step_by(9) {
            let ids: Vec<i64> = (chunk..(chunk + 10).min(side))
                .map(|y| y * side + x + 1)
                .collect();
            if ids.len() >= 2 {
                ways.push(way(way_id, &ids, &[("highway", "service")]));
                way_id += 1;
            }
        }
    }
    let modified_ids: Vec<i64> = ways.iter().take(modified).map(|w| w.id).collect();
    let d = data(nodes, ways, vec![]);
    let c = changed(&[], &modified_ids, &[]);
    (d, c)
}

#[test]
fn a_large_dataset_with_many_changes_validates() {
    // 100,000 nodes would be slow in a debug build; the release timing test
    // below does that size. This keeps the shape covered on every test run.
    let (d, c) = big_grid(60, 300);
    let f = run(&d, &c);
    // A regular grid shares nodes at every junction: no crossings.
    assert!(!has(&f, Check::CrossingWays));
}

/// `cargo test --release validator::tests::timing -- --ignored --nocapture`
#[test]
#[ignore = "timing, run in release"]
fn timing_on_a_100k_node_download() {
    let (d, mut c) = big_grid(317, 3000); // ~100k nodes
                                          // Also move a thousand nodes and tag a thousand new ones.
    c.nodes.extend(d.nodes.keys().copied().take(1000));
    let started = std::time::Instant::now();
    let f = run(&d, &c);
    let took = started.elapsed();
    eprintln!(
        "validated {} nodes, {} ways with {} changed ways and {} changed nodes in {took:?}: {} findings",
        d.nodes.len(),
        d.ways.len(),
        c.ways.len(),
        c.nodes.len(),
        f.len()
    );
    assert!(took.as_millis() < 100, "{took:?}");
}
