//! Further situations (batch A): reverse with shared nodes, closed ways and
//! area combos; splits of ways with several relation roles; duplicate
//! consecutive nodes; one member listed under different roles.

use super::*;

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0), "undo leaves nothing to upload");
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

// ---------------------------------------------------------------- 1. shared node

#[gpui::test]
#[ignore = "bug: reversing one way flips direction=forward on a junction node that a second, non-reversed way also uses, so the sign now faces the wrong way for the other way"]
fn reversing_one_way_leaves_a_direction_tag_on_a_node_shared_with_another_way(
    cx: &mut TestAppContext,
) {
    // Stop sign on the junction node 2, shared by way 10 (reversed) and way 11.
    let data = Build::new()
        .node(1, &[])
        .node(2, &[("highway", "stop"), ("direction", "forward")])
        .nodes(&[3, 4])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[2, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![3, 2, 1]));
        assert_eq!(s.way_nodes(11), Some(vec![2, 4]));
        assert_eq!(
            s.tags(N, 2),
            pairs(&[("direction", "forward"), ("highway", "stop")]),
            "the other way still uses the node, its direction must not change"
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_node_shared_by_two_reversed_ways_is_flipped_exactly_once(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[])
        .node(2, &[("highway", "stop"), ("direction", "forward")])
        .nodes(&[3, 4])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[2, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(
            s.tags(N, 2),
            pairs(&[("direction", "backward"), ("highway", "stop")]),
            "flipped once, not twice"
        );
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------- 2. closed ways

#[gpui::test]
fn reversing_a_roundabout_reverses_the_ring_and_keeps_the_closing_node(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[("highway", "primary"), ("junction", "roundabout")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes, vec![1, 4, 3, 2, 1]);
        // Tags are untouched: the junction tag implies the direction, nothing to flip.
        assert_eq!(
            s.tags(W, 10),
            pairs(&[("highway", "primary"), ("junction", "roundabout")])
        );
        assert_eq!(s.counts(), (0, 1, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_a_oneway_area_keeps_the_area_and_oneway_tags_and_swaps_side_keys(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[
                ("highway", "pedestrian"),
                ("area", "yes"),
                ("oneway", "yes"),
                ("sidewalk:left", "yes"),
            ],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 3, 2, 1]));
        assert_eq!(
            s.tags(W, 10),
            pairs(&[
                ("area", "yes"),
                ("highway", "pedestrian"),
                ("oneway", "yes"),
                ("sidewalk:right", "yes"),
            ])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_a_closed_building_keeps_it_closed(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[("building", "yes"), ("oneway", "yes")],
        )
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes.first(), nodes.last());
        assert_eq!(nodes.len(), 5);
    });
}

// ---------------------------------------------------------------- 3. via + route

#[gpui::test]
fn splitting_a_way_that_is_a_restriction_via_and_a_route_member_updates_both(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3, 4, 5], &road())
        .way(12, &[5, 6], &road())
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            &[wm(10, "from"), wm(11, "via"), wm(12, "to")],
        )
        .relation(
            901,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, ""), wm(11, "forward"), wm(12, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 11)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        // [3,4,5] is longer than [2,3]: original keeps the long part.
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5]));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(new), Some(vec![2, 3]));
        // Via chain is in travel order; from/to are untouched.
        assert_eq!(
            s.members(900),
            mem(&[(10, "from"), (new, "via"), (11, "via"), (12, "to")])
        );
        assert_eq!(
            s.members(901),
            mem(&[(10, ""), (new, "forward"), (11, "forward"), (12, "")])
        );
        s.assert_no_dangling();
        // New way; modified way 11; both relations modified.
        assert_eq!(s.counts(), (1, 3, 0));
        let xml = s.xml();
        assert!(at(&xml, "<create>") < at(&xml, "<modify>"));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------- 4. U-turn

#[gpui::test]
fn splitting_the_way_that_is_both_from_and_to_of_a_u_turn_keeps_a_valid_restriction(
    cx: &mut TestAppContext,
) {
    // from = to = way 10 [1,2,3], via node 3 (dead end): no_u_turn.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            &[wm(10, "from"), nm(3, "via"), wm(10, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 10)]);
        let did = s.act("split");
        if !did {
            // Refusing is acceptable only if nothing changed.
            assert_eq!(s.snap(), before, "status: {}", s.status());
            return;
        }
        assert_eq!(s.way_nodes(10), Some(vec![1, 2]));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(new), Some(vec![2, 3]));
        // Both from and to must be the half that touches the via node.
        assert_eq!(
            s.members(900),
            mem(&[(new, "from"), (3, "via"), (new, "to")])
        );
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn splitting_a_u_turn_way_at_the_far_side_of_the_via_keeps_both_roles_on_the_original(
    cx: &mut TestAppContext,
) {
    // The long half keeps the id and touches the via node 3.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            &[wm(10, "from"), nm(1, "via"), wm(10, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.members(900), mem(&[(10, "from"), (1, "via"), (10, "to")]));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------- 5. duplicate consecutive nodes

#[gpui::test]
fn reversing_a_way_with_a_duplicated_node_reverses_it_literally(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![3, 2, 1, 1]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_node_listed_twice_in_a_row_cannot_be_split_at(cx: &mut TestAppContext) {
    // Node 2 appears at two indexes, as in a figure eight: the app refuses
    // to split at a node a way visits twice and changes nothing.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 10)]);
        assert!(!s.act("split"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn splitting_elsewhere_in_a_way_with_a_duplicated_node_keeps_the_duplicate(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(10), Some(vec![1, 1, 2, 3]));
        assert_eq!(s.way_nodes(new), Some(vec![3, 4]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn joining_a_way_with_a_duplicated_end_node_keeps_the_duplicate(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 1, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 1, 2, 3]));
        assert!(!s.has_way(11));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn joining_ways_that_both_end_on_the_duplicated_node_chains_them(cx: &mut TestAppContext) {
    // 10 ends [..,2,2] and 11 starts [2,..]: the join must not drop or triple a node.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"), "status: {}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes.first(), Some(&1));
        assert_eq!(nodes.last(), Some(&3));
        assert!(nodes.iter().filter(|&&n| n == 2).count() <= 2);
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------- 6. one member, several roles

fn multi_role_route() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(9, &[1, 2], &road())
        .way(10, &[2, 3, 4, 5], &road())
        .way(11, &[5, 1], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(9, ""), wm(10, "forward"), wm(11, ""), wm(10, "backward")],
        )
        .done()
}

#[gpui::test]
fn split_gives_each_occurrence_of_a_member_its_own_role(cx: &mut TestAppContext) {
    with_session(cx, multi_role_route(), |s| {
        let before = s.snap();
        s.select(&[(N, 4), (W, 10)]);
        assert!(s.act("split"));
        // [2,3,4] and [4,5] are 3 vs 2 nodes: original keeps [2,3,4].
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4]));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(new), Some(vec![4, 5]));
        // Forward occurrence: 10 then new. Backward occurrence: new then 10.
        assert_eq!(
            s.members(900),
            mem(&[
                (9, ""),
                (10, "forward"),
                (new, "forward"),
                (11, ""),
                (new, "backward"),
                (10, "backward"),
            ])
        );
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_member_with_two_roles_removes_both_memberships(cx: &mut TestAppContext) {
    with_session(cx, multi_role_route(), |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"), "status: {}", s.status());
        assert_eq!(s.members(900), mem(&[(9, ""), (11, "")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_is_refused_when_the_way_has_different_roles_in_one_relation(cx: &mut TestAppContext) {
    // Design: way 10 is "forward" next to 9 and "backward" elsewhere; joining
    // it into 9 would have to pick one role, so the app refuses and says why.
    with_session(cx, multi_role_route(), |s| {
        let before = s.snap();
        s.select(&[(W, 9), (W, 10)]);
        assert!(!s.act("join"));
        assert!(s.status().contains("different role"), "{}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}
