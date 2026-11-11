//! Create Multipolygon (JOSM `CreateMultipolygonAction`): loose ways in,
//! one `type=multipolygon` relation out, with roles from nesting depth.

use super::*;
use osm_gpui::osm::OsmNode;

/// Node id for grid point `(x, y)` (both 1..=98); placed about 100 m apart.
fn n(x: i64, y: i64) -> i64 {
    x * 100 + y
}

fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<i64> {
    vec![n(x0, y0), n(x1, y0), n(x1, y1), n(x0, y1), n(x0, y0)]
}

/// A way to build: id, node ids, tags.
type GridWay<'a> = (i64, Vec<i64>, Vec<(&'a str, &'a str)>);

/// Add every node the ways name, at the grid position its id encodes.
fn on_grid(mut b: Build, ways: &[GridWay]) -> Build {
    for (id, nodes, tags) in ways {
        for &node in nodes {
            b.d().nodes.entry(node).or_insert(OsmNode {
                id: node,
                lat: 40.0 + (node % 100) as f64 * 0.001,
                lon: -74.0 + (node / 100) as f64 * 0.001,
                version: 3,
                tags: HashMap::new(),
            });
        }
        b = b.way(*id, nodes, tags);
    }
    b
}

fn world(ways: &[GridWay]) -> OsmData {
    on_grid(Build::new(), ways).done()
}

fn pick(ids: &[i64]) -> Vec<(FeatureKind, i64)> {
    ids.iter().map(|&i| (W, i)).collect()
}

fn lake_and_island() -> OsmData {
    world(&[
        (
            10,
            square(1, 1, 9, 9),
            vec![("natural", "water"), ("source", "survey")],
        ),
        (11, square(3, 3, 5, 5), vec![]),
    ])
}

#[gpui::test]
fn a_lake_with_an_island_becomes_one_relation_with_the_area_tags_moved(cx: &mut TestAppContext) {
    with_session(cx, lake_and_island(), |s| {
        s.select(&pick(&[11, 10]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(s.steps(), 1);
        assert_eq!(
            s.tags(R, -1),
            pairs(&[("natural", "water"), ("type", "multipolygon")])
        );
        // Outer first, then inner, whatever the selection order was.
        assert_eq!(s.members(-1), mem(&[(10, "outer"), (11, "inner")]));
        assert_eq!(s.tags(W, 10), pairs(&[("source", "survey")]));
        assert_eq!(s.tags(W, 11), pairs(&[]));
        assert_eq!(
            s.view
                .selected
                .iter()
                .map(|f| (f.kind, f.id))
                .collect::<Vec<_>>(),
            vec![(R, -1)]
        );
        assert!(s.status().contains("1 outer, 1 inner"), "{}", s.status());
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn undo_removes_the_relation_and_puts_the_tags_back_and_redo_repeats_it(cx: &mut TestAppContext) {
    with_session(cx, lake_and_island(), |s| {
        let before = s.snap();
        s.select(&pick(&[10, 11]));
        assert!(s.act("create_multipolygon"));
        let after = s.snap();
        assert_ne!(before, after);
        s.undo();
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
        s.redo();
        assert_eq!(s.snap(), after);
        s.undo();
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn two_outers_with_an_inner_each_share_one_relation_and_keep_their_tags(cx: &mut TestAppContext) {
    let data = world(&[
        (20, square(1, 1, 20, 20), vec![("landuse", "forest")]),
        (21, square(3, 3, 6, 6), vec![]),
        (22, square(40, 1, 60, 20), vec![("landuse", "forest")]),
        (23, square(43, 3, 46, 6), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[23, 22, 21, 20]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(
            s.members(-1),
            mem(&[(22, "outer"), (20, "outer"), (23, "inner"), (21, "inner")])
        );
        // Several outers: no single owner for the tags, so nothing moves.
        assert_eq!(s.tags(R, -1), pairs(&[("type", "multipolygon")]));
        assert_eq!(s.tags(W, 20), pairs(&[("landuse", "forest")]));
        assert_eq!(s.tags(W, 22), pairs(&[("landuse", "forest")]));
        assert_eq!(s.steps(), 1);
        assert!(s.status().contains("2 outer, 2 inner"));
    });
}

#[gpui::test]
fn a_ring_made_of_two_open_ways_gets_both_as_outer_and_keeps_their_tags(cx: &mut TestAppContext) {
    let a = vec![n(1, 1), n(9, 1), n(9, 9)];
    let b = vec![n(1, 1), n(1, 9), n(9, 9)];
    let data = world(&[
        (30, a, vec![("natural", "wood")]),
        (31, b, vec![("natural", "wood")]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[30, 31]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(s.members(-1), mem(&[(30, "outer"), (31, "outer")]));
        assert_eq!(s.tags(R, -1), pairs(&[("type", "multipolygon")]));
        assert_eq!(s.tags(W, 30), pairs(&[("natural", "wood")]));
    });
}

#[gpui::test]
fn an_inner_ring_may_be_built_from_open_ways_too(cx: &mut TestAppContext) {
    let data = world(&[
        (40, square(1, 1, 20, 20), vec![]),
        (41, vec![n(5, 5), n(8, 5), n(8, 8)], vec![]),
        (42, vec![n(5, 5), n(5, 8), n(8, 8)], vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[42, 40, 41]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(
            s.members(-1),
            mem(&[(40, "outer"), (42, "inner"), (41, "inner")])
        );
    });
}

#[gpui::test]
fn rings_nested_three_deep_alternate_outer_inner_outer(cx: &mut TestAppContext) {
    let data = world(&[
        (50, square(1, 1, 30, 30), vec![("landuse", "meadow")]),
        (51, square(5, 5, 25, 25), vec![]),
        (52, square(10, 10, 20, 20), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[52, 51, 50]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(
            s.members(-1),
            mem(&[(52, "outer"), (50, "outer"), (51, "inner")])
        );
        // Two outers, so the meadow tag stays where it is.
        assert_eq!(s.tags(W, 50), pairs(&[("landuse", "meadow")]));
    });
}

#[gpui::test]
fn an_inner_touching_the_outer_at_a_shared_node_is_accepted(cx: &mut TestAppContext) {
    // The island's corner is the lake shore's corner.
    let island = vec![n(1, 1), n(4, 3), n(3, 4), n(1, 1)];
    let data = world(&[(60, square(1, 1, 9, 9), vec![]), (61, island, vec![])]);
    with_session(cx, data, |s| {
        s.select(&pick(&[60, 61]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(s.members(-1), mem(&[(60, "outer"), (61, "inner")]));
    });
}

#[gpui::test]
fn an_inner_running_along_the_outer_from_a_shared_node_is_refused(cx: &mut TestAppContext) {
    let data = world(&[
        (60, square(1, 1, 9, 9), vec![]),
        (61, square(1, 1, 4, 4), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[60, 61]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("overlap"), "{}", s.status());
    });
}

#[gpui::test]
fn two_outers_touching_at_one_corner_are_both_outer(cx: &mut TestAppContext) {
    let data = world(&[
        (62, square(1, 1, 4, 4), vec![]),
        (63, square(4, 4, 8, 8), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[62, 63]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(s.members(-1), mem(&[(62, "outer"), (63, "outer")]));
    });
}

#[gpui::test]
fn crossing_rings_are_refused_and_nothing_changes(cx: &mut TestAppContext) {
    let data = world(&[
        (70, square(1, 1, 6, 6), vec![]),
        (71, square(4, 4, 9, 9), vec![]),
    ]);
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&pick(&[70, 71]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("cross or overlap"), "{}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn rings_sharing_an_edge_are_refused(cx: &mut TestAppContext) {
    let data = world(&[
        (72, square(1, 1, 4, 4), vec![]),
        (73, square(4, 1, 8, 4), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[72, 73]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("overlap"), "{}", s.status());
    });
}

#[gpui::test]
fn a_chain_that_does_not_close_is_refused(cx: &mut TestAppContext) {
    let data = world(&[
        (80, vec![n(1, 1), n(9, 1), n(9, 9)], vec![]),
        (81, vec![n(9, 9), n(1, 9)], vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[80, 81]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("do not close"), "{}", s.status());
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn three_open_ways_ending_at_one_node_are_refused_as_ambiguous(cx: &mut TestAppContext) {
    let data = world(&[
        (82, vec![n(1, 1), n(9, 1)], vec![]),
        (83, vec![n(1, 1), n(1, 9)], vec![]),
        (84, vec![n(1, 1), n(5, 5)], vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[82, 83, 84]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("end at node"), "{}", s.status());
    });
}

#[gpui::test]
fn a_ring_that_touches_itself_is_refused(cx: &mut TestAppContext) {
    let figure_eight = vec![
        n(1, 1),
        n(5, 5),
        n(9, 1),
        n(9, 9),
        n(5, 5),
        n(1, 9),
        n(1, 1),
    ];
    with_session(cx, world(&[(85, figure_eight, vec![])]), |s| {
        s.select(&pick(&[85]));
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("twice"), "{}", s.status());
    });
}

#[gpui::test]
fn nothing_selected_or_only_nodes_is_refused_with_the_hint(cx: &mut TestAppContext) {
    with_session(cx, lake_and_island(), |s| {
        s.select(&[]);
        assert!(!s.act("create_multipolygon"));
        assert!(s.status().contains("Select closed ways"), "{}", s.status());
        s.select(&[(N, n(1, 1))]);
        assert!(!s.act("create_multipolygon"));
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn only_area_tags_and_the_name_move_everything_else_stays_on_the_way(cx: &mut TestAppContext) {
    let data = world(&[
        (
            90,
            square(1, 1, 9, 9),
            vec![
                ("landuse", "farmland"),
                ("name", "Big Field"),
                ("source", "bing"),
                ("boundary", "protected_area"),
                ("fixme", "check"),
            ],
        ),
        (91, square(3, 3, 5, 5), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[90, 91]));
        assert!(s.act("create_multipolygon"));
        assert_eq!(
            s.tags(R, -1),
            pairs(&[
                ("landuse", "farmland"),
                ("name", "Big Field"),
                ("type", "multipolygon")
            ])
        );
        assert_eq!(
            s.tags(W, 90),
            pairs(&[
                ("boundary", "protected_area"),
                ("fixme", "check"),
                ("source", "bing")
            ])
        );
    });
}

#[gpui::test]
fn a_name_alone_does_not_move_and_untagged_ways_stay_untagged(cx: &mut TestAppContext) {
    let data = world(&[
        (
            92,
            square(1, 1, 9, 9),
            vec![("name", "Pond"), ("source", "x")],
        ),
        (93, square(3, 3, 5, 5), vec![]),
    ]);
    with_session(cx, data, |s| {
        s.select(&pick(&[92, 93]));
        assert!(s.act("create_multipolygon"));
        assert_eq!(s.tags(R, -1), pairs(&[("type", "multipolygon")]));
        assert_eq!(s.tags(W, 92), pairs(&[("name", "Pond"), ("source", "x")]));
        assert_eq!(s.tags(W, 93), pairs(&[]));
        // No tag changed, so the way is not part of the upload.
        assert_eq!(s.counts(), (1, 0, 0));
    });
}

#[gpui::test]
fn a_ring_with_thousands_of_nodes_is_fine_the_node_limit_belongs_to_editing_ways(
    cx: &mut TestAppContext,
) {
    // 3,000 nodes around a big octagon-ish loop: more than a way may have
    // when created by editing, but the ways are not changed here.
    let mut ring: Vec<i64> = Vec::new();
    let mut b = Build::new();
    let count = 3000;
    for i in 0..count {
        let a = std::f64::consts::TAU * i as f64 / count as f64;
        let id = 100_000 + i as i64;
        b.d().nodes.insert(
            id,
            OsmNode {
                id,
                lat: 40.0 + 0.05 * a.sin(),
                lon: -74.0 + 0.05 * a.cos(),
                version: 3,
                tags: HashMap::new(),
            },
        );
        ring.push(id);
    }
    ring.push(ring[0]);
    let data = b.way(200, &ring, &[("natural", "water")]).done();
    with_session(cx, data, |s| {
        s.select(&pick(&[200]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert_eq!(s.members(-1), mem(&[(200, "outer")]));
        assert_eq!(s.way_nodes(200).unwrap().len(), 3001);
    });
}

#[gpui::test]
fn a_single_closed_way_makes_a_one_member_multipolygon(cx: &mut TestAppContext) {
    let data = world(&[(95, square(1, 1, 9, 9), vec![("building", "yes")])]);
    with_session(cx, data, |s| {
        s.select(&pick(&[95]));
        assert!(s.act("create_multipolygon"));
        assert_eq!(s.members(-1), mem(&[(95, "outer")]));
        assert_eq!(
            s.tags(R, -1),
            pairs(&[("building", "yes"), ("type", "multipolygon")])
        );
        assert_eq!(s.tags(W, 95), pairs(&[]));
    });
}

#[gpui::test]
fn relations_the_ways_are_already_in_are_untouched_and_a_shared_multipolygon_is_noted(
    cx: &mut TestAppContext,
) {
    let data = on_grid(
        Build::new(),
        &[
            (10, square(1, 1, 9, 9), vec![("natural", "water")]),
            (11, square(3, 3, 5, 5), vec![]),
        ],
    )
    .relation(
        500,
        &[("type", "multipolygon")],
        &[wm(10, "outer"), wm(11, "inner")],
    )
    .relation(501, &[("type", "route")], &[wm(10, "")])
    .done();
    with_session(cx, data, |s| {
        let relation_500 = s.members(500);
        let relation_501 = s.members(501);
        s.select(&pick(&[10, 11]));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        assert!(
            s.status().contains("2 of the ways already belong"),
            "{}",
            s.status()
        );
        assert_eq!(s.members(500), relation_500);
        assert_eq!(s.members(501), relation_501);
        assert_eq!(s.tags(R, 500), pairs(&[("type", "multipolygon")]));
        assert_eq!(s.members(-1), mem(&[(10, "outer"), (11, "inner")]));
        s.undo();
        assert_eq!(s.members(500), relation_500);
        assert_eq!(s.members(501), relation_501);
    });
}

#[gpui::test]
fn new_ways_go_up_before_the_relation_that_names_them(cx: &mut TestAppContext) {
    // Ways drawn this session have negative ids; the relation gets an id too,
    // and must come after them in the upload.
    let data = Build::new().done();
    with_session(cx, data, |s| {
        let mut ids = Vec::new();
        for (way_id, (x0, x1)) in [(-901, (1, 9)), (-902, (3, 5))] {
            let corners = [(x0, x0), (x1, x0), (x1, x1), (x0, x1)];
            let mut nodes: Vec<i64> = corners
                .iter()
                .map(|&(x, y)| {
                    s.editable_mut()
                        .add_node(40.0 + y as f64 * 0.001, -74.0 + x as f64 * 0.001)
                })
                .collect();
            nodes.push(nodes[0]);
            s.editable_mut()
                .restore_feature(osm_gpui::selection::DeletedFeatureSnapshot {
                    kind: W,
                    id: way_id,
                    tags: Vec::new(),
                    way_nodes: nodes,
                    node_lat_lon: None,
                    version: 0,
                });
            ids.push(way_id);
        }
        s.select(&pick(&ids));
        assert!(s.act("create_multipolygon"), "{}", s.status());
        let xml = s.xml();
        let relation = at(&xml, "<relation id=\"-");
        for id in &ids {
            assert!(at(&xml, &format!("<way id=\"{id}\"")) < relation, "{xml}");
        }
        assert!(xml.contains("k=\"type\" v=\"multipolygon\""), "{xml}");
    });
}
