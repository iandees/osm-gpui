//! Split Way: relation membership, closed ways, end nodes, repeated nodes.

use super::*;

/// One undo restores the exact prior state, one redo reapplies the result.
fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0), "undo leaves nothing to upload");
    s.redo();
    assert_eq!(
        s.snap(),
        after,
        "redo reapplies the split with the same ids"
    );
}

/// One undo restores the prior state (leaving the split undone).
fn undo_only(s: &mut Session, before: &Snap) {
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
}

fn route(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("type", "route"), ("route", "bus")];
    for &(k, val) in extra {
        v.push((k, val));
    }
    v
}

#[gpui::test]
fn route_halves_stay_adjacent_in_travel_order(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2], &[("highway", "residential")])
        .way(
            11,
            &[2, 3, 4, 5, 6],
            &[("highway", "residential"), ("name", "Elm")],
        )
        .way(12, &[6, 7], &[("highway", "residential")])
        .relation(900, &route(&[]), &[wm(10, ""), wm(11, ""), wm(12, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 4), (W, 11)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        // Equal halves: the original keeps its id and the first half.
        assert_eq!(s.way_nodes(11), Some(vec![2, 3, 4]));
        let new = s.new_ways();
        assert_eq!(new.len(), 1);
        assert_eq!(s.way_nodes(new[0]), Some(vec![4, 5, 6]));
        assert_eq!(s.tags(W, new[0]), s.tags(W, 11));
        assert_eq!(
            s.members(900),
            mem(&[(10, ""), (11, ""), (new[0], ""), (12, "")])
        );
        s.assert_no_dangling();
        // New way, modified way, modified relation.
        assert_eq!(s.counts(), (1, 2, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn route_driven_against_the_way_direction_puts_the_new_half_first(cx: &mut TestAppContext) {
    // The way's nodes run 6..2 but the route travels 2..6.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2], &[])
        .way(11, &[6, 5, 4, 3, 2], &[("highway", "residential")])
        .way(12, &[6, 7], &[])
        .relation(
            900,
            &route(&[]),
            &[wm(10, ""), wm(11, "forward"), wm(12, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 4), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(11), Some(vec![6, 5, 4]));
        assert_eq!(s.way_nodes(new), Some(vec![4, 3, 2]));
        // The half touching way 10 comes first; the role is copied.
        assert_eq!(
            s.members(900),
            mem(&[(10, ""), (new, "forward"), (11, "forward"), (12, "")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn way_used_twice_in_a_route_gets_the_halves_in_the_right_order_each_time(cx: &mut TestAppContext) {
    // Out and back: the way is driven forward, then backward.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2], &[])
        .way(11, &[2, 3, 4, 5], &[("highway", "service")])
        .way(12, &[5, 6], &[])
        .relation(
            900,
            &route(&[]),
            &[wm(10, ""), wm(11, ""), wm(12, ""), wm(11, ""), wm(10, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 11)]);
        assert!(s.act("split"));
        // [2,3] is shorter than [3,4,5], so the original keeps the long part.
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5]));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(new), Some(vec![2, 3]));
        assert_eq!(
            s.members(900),
            mem(&[
                (10, ""),
                (new, ""),
                (11, ""),
                (12, ""),
                (11, ""),
                (new, ""),
                (10, "")
            ])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn restriction_from_way_follows_the_half_touching_the_via_node(cx: &mut TestAppContext) {
    // from = 11 [1,2,3,4], via node 4, to = 12 [4,5]. Split at 2: the
    // long half [2,3,4] keeps the id and touches the via; the short [1,2] does not.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(11, &[1, 2, 3, 4], &[("highway", "residential")])
        .way(12, &[4, 5], &[("highway", "residential")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            &[wm(11, "from"), nm(4, "via"), wm(12, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(11), Some(vec![2, 3, 4]));
        assert_eq!(s.members(900), mem(&[(11, "from"), (4, "via"), (12, "to")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn restriction_from_way_moves_to_the_new_half_when_that_touches_the_via(cx: &mut TestAppContext) {
    // The via is at the end of the way, which becomes the short new half.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(11, &[1, 2, 3, 4], &[("highway", "residential")])
        .way(12, &[4, 5], &[("highway", "residential")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_right_turn")],
            &[wm(11, "from"), nm(4, "via"), wm(12, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(11), Some(vec![1, 2, 3]));
        assert_eq!(s.way_nodes(new), Some(vec![3, 4]));
        assert_eq!(
            s.members(900),
            mem(&[(new, "from"), (4, "via"), (12, "to")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn splitting_a_via_way_keeps_both_halves_as_via(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2], &[("highway", "residential")])
        .way(11, &[2, 3, 4], &[("highway", "residential")])
        .way(12, &[4, 5], &[("highway", "residential")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            &[wm(10, "from"), wm(11, "via"), wm(12, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        let vias: Vec<_> = s
            .members(900)
            .into_iter()
            .filter(|m| m.1 == "via")
            .collect();
        assert_eq!(vias, mem(&[(11, "via"), (new, "via")]));
        // from and to are untouched.
        assert_eq!(s.members(900)[0], (10, "from".to_string()));
        assert_eq!(s.members(900).last().unwrap(), &(12, "to".to_string()));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn multipolygon_ring_split_at_two_nodes_keeps_the_role_on_both_halves(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8])
        .way(10, &[1, 2, 3, 4, 1], &[])
        .way(11, &[5, 6, 7, 8, 5], &[])
        .relation(
            900,
            &[("type", "multipolygon"), ("landuse", "forest")],
            &[wm(10, "outer"), wm(11, "inner")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 6), (N, 8), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(11), Some(vec![6, 7, 8]));
        assert_eq!(s.way_nodes(new), Some(vec![8, 5, 6]));
        assert_eq!(
            s.members(900),
            mem(&[(10, "outer"), (11, "inner"), (new, "inner")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn boundary_outer_ring_keeps_order_and_roles_of_the_other_members(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .node(9, &[("place", "town")])
        .way(10, &[1, 2, 3], &[("boundary", "administrative")])
        .way(11, &[3, 4, 5], &[("boundary", "administrative")])
        .way(12, &[5, 6, 1], &[("boundary", "administrative")])
        .relation(
            900,
            &[("type", "boundary"), ("boundary", "administrative")],
            &[
                nm(9, "admin_centre"),
                wm(10, "outer"),
                wm(11, "outer"),
                wm(12, "outer"),
            ],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 4), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(
            s.members(900),
            mem(&[
                (9, "admin_centre"),
                (10, "outer"),
                (11, "outer"),
                (new, "outer"),
                (12, "outer")
            ])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_relation_that_is_a_member_of_another_relation_is_not_disturbed(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(11, &[1, 2, 3], &[("highway", "residential")])
        .relation(900, &route(&[]), &[wm(11, "")])
        .relation(
            901,
            &[("type", "route_master"), ("route_master", "bus")],
            &[rm(900, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.members(900), mem(&[(11, ""), (new, "")]));
        assert_eq!(s.members(901), mem(&[(900, "")]));
        // Only the route changed; the master is not in the upload.
        let d = s.diff();
        assert_eq!(d.modified_relations.len(), 1);
        assert_eq!(d.modified_relations[0].id, 900);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn incomplete_route_keeps_unknown_members_in_place(cx: &mut TestAppContext) {
    // Ways 9001 / 9002 are members that were never downloaded.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(11, &[1, 2, 3], &[("highway", "residential")])
        .relation(
            900,
            &route(&[]),
            &[wm(9001, "a"), wm(11, "b"), wm(9002, "c")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        let m = s.members(900);
        // Unknown members keep their roles and relative order. Where the
        // new half goes cannot be known without the neighbours; the app
        // guesses "after" and does not warn (JOSM asks to download first).
        assert_eq!(m.first().unwrap(), &(9001, "a".to_string()));
        assert_eq!(m.last().unwrap(), &(9002, "c".to_string()));
        assert_eq!(m.len(), 4);
        assert!(m.contains(&(new, "b".to_string())));
        undo_redo(s, &before);
    });
}

/// The id of the new way whose nodes are exactly `nodes`.
fn new_way_with(s: &Session, nodes: &[i64]) -> i64 {
    let found: Vec<i64> = s
        .new_ways()
        .into_iter()
        .filter(|&w| s.way_nodes(w).as_deref() == Some(nodes))
        .collect();
    assert_eq!(found.len(), 1, "one new way holds {nodes:?}");
    found[0]
}

#[gpui::test]
fn closed_way_with_one_node_is_turned_to_start_and_end_there(cx: &mut TestAppContext) {
    // JOSM: the ring is opened at the node. In OSM terms the way stays a
    // closed node list (first == last); it is rewritten to start and end at
    // the node. Same id, version, tags and relations; no new way.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 1], &[("highway", "residential")])
        .relation(900, &route(&[]), &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 1, 2, 3]));
        assert!(s.new_ways().is_empty());
        assert_eq!(s.tags(W, 10), pairs(&[("highway", "residential")]));
        assert_eq!(s.members(900), mem(&[(10, "")]));
        // The same way, modified: no creation, relation untouched.
        let d = s.diff();
        assert_eq!(s.counts(), (0, 1, 0));
        assert_eq!(d.modified_ways[0].id, 10);
        assert_eq!(d.modified_ways[0].version, 3);
        // The selection is left as it was.
        assert_eq!(s.view.selected.len(), 2);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn closed_way_one_node_without_selecting_the_way_and_at_the_start_node(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 1], &[("highway", "residential")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // The ring's own start/end node: nothing would change.
        s.select(&[(N, 1), (W, 10)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        // The ring is the only way at the node, so the way needn't be selected.
        s.select(&[(N, 2)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 1, 2]));
        undo_only(s, &before);
    });
}

#[gpui::test]
fn closed_way_one_node_leaves_a_ring_alone_when_a_street_is_split_at_the_junction(
    cx: &mut TestAppContext,
) {
    // Node 3 joins a street and a building. Splitting "at the junction"
    // means the street; the building is not silently turned.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 1], &[("building", "yes")])
        .way(11, &[5, 3, 6], &[("highway", "residential")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4, 1]));
        assert_eq!(s.new_ways().len(), 1);
        undo_only(s, &before);
        // Naming the building does turn it.
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 1, 2, 3]));
        assert_eq!(s.way_nodes(11), Some(vec![5, 3, 6]));
    });
}

#[gpui::test]
fn closed_way_cut_at_three_nodes_gives_three_arcs(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5, 6, 1], &[("natural", "water")])
        .relation(900, &[("type", "multipolygon")], &[wm(10, "outer")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (N, 6)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        // Three arcs of equal length: the original keeps the first.
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4]));
        let a = new_way_with(s, &[4, 5, 6]);
        let b = new_way_with(s, &[6, 1, 2]);
        assert_eq!(s.tags(W, a), s.tags(W, 10));
        assert_eq!(s.tags(W, b), s.tags(W, 10));
        assert_eq!(
            s.members(900),
            mem(&[(10, "outer"), (a, "outer"), (b, "outer")])
        );
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn closed_area_split_gives_two_open_ways_that_both_keep_the_area_tags(cx: &mut TestAppContext) {
    // Design difference: iD turns a split area into a multipolygon; this
    // app (like JOSM) just copies the tags, leaving two open "buildings".
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[("building", "yes"), ("area", "yes")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 3)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.way_nodes(new), Some(vec![3, 4, 1]));
        assert_eq!(
            s.tags(W, new),
            pairs(&[("area", "yes"), ("building", "yes")])
        );
        assert_eq!(s.tags(W, 10), s.tags(W, new));
        // Selecting only the two nodes (no way) is enough.
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn closed_multipolygon_ring_member_is_split_with_both_halves_kept(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 1], &[("natural", "water")])
        .relation(900, &[("type", "multipolygon")], &[wm(10, "outer")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.members(900), mem(&[(10, "outer"), (new, "outer")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn roundabout_in_a_route_splits_and_both_halves_stay_members(cx: &mut TestAppContext) {
    // iD refuses to split a roundabout that is part of a route; JOSM and
    // this app allow it, and the relation must stay whole.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[("highway", "primary"), ("junction", "roundabout")],
        )
        .relation(900, &route(&[]), &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 3)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        assert_eq!(s.members(900), mem(&[(10, ""), (new, "")]));
        assert_eq!(s.tags(W, new), s.tags(W, 10));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn splitting_at_an_end_node_is_refused_and_changes_nothing(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        for end in [1, 3] {
            s.select(&[(N, end), (W, 10)]);
            assert!(!s.act("split"));
        }
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn splitting_an_open_way_at_two_nodes_at_once_gives_three_parts(cx: &mut TestAppContext) {
    // S16: JOSM and iD cut an open way into three at two nodes. The middle
    // part is the longest, so it keeps the id and version; the two ends are
    // new ways with a copy of the tags.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(
            10,
            &[1, 2, 3, 4, 5, 6],
            &[("highway", "path"), ("name", "P")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 5), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1, "one undo step for all the cuts");
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 5]));
        let first = new_way_with(s, &[1, 2]);
        let last = new_way_with(s, &[5, 6]);
        assert_eq!(s.new_ways().len(), 2);
        for w in [first, last] {
            assert_eq!(s.tags(W, w), s.tags(W, 10));
        }
        assert_eq!(s.tags(W, 10), pairs(&[("highway", "path"), ("name", "P")]));
        // 10 is modified (version kept), the two others are created.
        assert_eq!(s.counts(), (2, 1, 0));
        let d = s.diff();
        assert_eq!(d.modified_ways[0].id, 10);
        assert_eq!(d.modified_ways[0].version, 3);
        // The selection is not changed by the split.
        assert_eq!(s.view.selected.len(), 3);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn open_way_several_cuts_keep_the_longest_part_and_the_first_of_equals(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        .way(10, &[1, 2, 3, 4, 5, 6, 7], &[("highway", "path")])
        .way(11, &[8, 9], &[])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Parts [1,2,3], [3,4,5], [5,6,7]: all equal, the first keeps the id.
        s.select(&[(N, 3), (N, 5), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        new_way_with(s, &[3, 4, 5]);
        new_way_with(s, &[5, 6, 7]);
        undo_only(s, &before);
        // Parts [1,2], [2,3,4,5,6], [6,7]: the middle one is longest.
        s.select(&[(N, 2), (N, 6), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 5, 6]));
        // Parts [1,2], [2,3], [3..7]: the last is longest.
        s.undo();
        s.select(&[(N, 2), (N, 3), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 5, 6, 7]));
        new_way_with(s, &[1, 2]);
        new_way_with(s, &[2, 3]);
    });
}

#[gpui::test]
fn open_way_cut_ignores_a_selected_end_node_and_a_node_not_on_the_way(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4], &[("highway", "path")])
        .way(11, &[5, 6], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 3), (N, 6), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.new_ways().len(), 1);
        new_way_with(s, &[3, 4]);
        assert_eq!(s.way_nodes(11), Some(vec![5, 6]));
        undo_only(s, &before);
        // Only end nodes: still refused.
        s.select(&[(N, 1), (N, 4), (W, 10)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn two_selected_nodes_split_every_way_through_them(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3, 4], &[("highway", "residential")])
        .way(11, &[5, 2, 3, 6], &[("highway", "service")])
        .way(12, &[7, 3, 5], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        // 10 and 11 are cut twice (3 parts), 12 only at 3 (it has no 2).
        assert_eq!(s.new_ways().len(), 5);
        assert_eq!(s.way_nodes(12), Some(vec![7, 3]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn route_stays_in_travel_order_after_a_multi_node_split(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
        .way(10, &[1, 2], &[])
        .way(11, &[2, 3, 4, 5, 6, 7, 8, 9], &[("highway", "residential")])
        .way(12, &[9, 10], &[])
        .relation(900, &route(&[]), &[wm(10, ""), wm(11, "fwd"), wm(12, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 8), (W, 11)]);
        assert!(s.act("split"));
        // [2,3] | [3..8] (longest, keeps 11) | [8,9]
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5, 6, 7, 8]));
        let a = new_way_with(s, &[2, 3]);
        let b = new_way_with(s, &[8, 9]);
        assert_eq!(
            s.members(900),
            mem(&[(10, ""), (a, "fwd"), (11, "fwd"), (b, "fwd"), (12, "")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn route_driven_against_the_way_direction_stays_ordered_after_a_multi_node_split(
    cx: &mut TestAppContext,
) {
    // The way's nodes run 8..2 but the route travels 2..8.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        .way(10, &[1, 2], &[])
        .way(11, &[8, 7, 6, 5, 4, 3, 2], &[("highway", "residential")])
        .way(12, &[8, 9], &[])
        .relation(900, &route(&[]), &[wm(10, ""), wm(11, ""), wm(12, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 6), (N, 4), (W, 11)]);
        assert!(s.act("split"));
        // Parts along the way: [8,7,6] [6,5,4] [4,3,2], all of three nodes.
        assert_eq!(s.way_nodes(11), Some(vec![8, 7, 6]));
        let mid = new_way_with(s, &[6, 5, 4]);
        let end = new_way_with(s, &[4, 3, 2]);
        assert_eq!(
            s.members(900),
            mem(&[(10, ""), (end, ""), (mid, ""), (11, ""), (12, "")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn way_used_twice_in_a_route_is_split_into_every_part_each_time(cx: &mut TestAppContext) {
    // Out and back over the same way, split at two nodes.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2], &[])
        .way(11, &[2, 3, 4, 5, 6], &[("highway", "service")])
        .way(12, &[6, 7], &[])
        .relation(
            900,
            &route(&[]),
            &[wm(10, ""), wm(11, ""), wm(12, ""), wm(11, ""), wm(10, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 5), (W, 11)]);
        assert!(s.act("split"));
        // [2,3] | [3,4,5] (keeps 11) | [5,6]
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5]));
        let a = new_way_with(s, &[2, 3]);
        let b = new_way_with(s, &[5, 6]);
        assert_eq!(
            s.members(900),
            mem(&[
                (10, ""),
                (a, ""),
                (11, ""),
                (b, ""),
                (12, ""),
                (b, ""),
                (11, ""),
                (a, ""),
                (10, "")
            ])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn restriction_from_way_follows_the_part_touching_the_via_after_a_multi_node_split(
    cx: &mut TestAppContext,
) {
    // from = 11 [1..6], via node 6, to = 12 [6,7]. Cut at 2 and 4: the part
    // [4,5,6] touches the via and gets the "from" role.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(11, &[1, 2, 3, 4, 5, 6], &[("highway", "residential")])
        .way(12, &[6, 7], &[("highway", "residential")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            &[wm(11, "from"), nm(6, "via"), wm(12, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 11)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(11), Some(vec![2, 3, 4]));
        let from = new_way_with(s, &[4, 5, 6]);
        new_way_with(s, &[1, 2]);
        assert_eq!(
            s.members(900),
            mem(&[(from, "from"), (6, "via"), (12, "to")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn restriction_with_an_unknown_via_refuses_a_multi_node_split_whole(cx: &mut TestAppContext) {
    // No via member: the half cannot be told, so nothing is split.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(11, &[1, 2, 3, 4, 5, 6], &[("highway", "residential")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            &[wm(11, "from")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 11)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn multipolygon_ring_member_cut_at_several_nodes_keeps_the_role_on_every_part(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8])
        .way(10, &[1, 2, 3, 4, 1], &[])
        .way(11, &[5, 6, 7, 8, 5], &[])
        .relation(
            900,
            &[("type", "multipolygon"), ("landuse", "forest")],
            &[wm(10, "outer"), wm(11, "inner")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 5), (N, 6), (N, 7), (W, 11)]);
        assert!(s.act("split"));
        // Arcs [5,6] [6,7] [7,8,5]: the last is longest and keeps the id.
        assert_eq!(s.way_nodes(11), Some(vec![7, 8, 5]));
        let a = new_way_with(s, &[5, 6]);
        let b = new_way_with(s, &[6, 7]);
        // No neighbour says where the arcs go, so they follow the original.
        let m = s.members(900);
        assert_eq!(m.len(), 4);
        assert_eq!(m[..2], mem(&[(10, "outer"), (11, "inner")])[..]);
        for w in [a, b] {
            assert!(m.contains(&(w, "inner".to_string())));
        }
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn every_new_part_is_created_before_the_relation_that_names_it_in_the_upload(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(11, &[1, 2, 3, 4, 5, 6, 7], &[("highway", "residential")])
        .relation(900, &route(&[]), &[wm(11, "")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 3), (N, 5), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways();
        assert_eq!(new.len(), 2);
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let modify = at(&xml, "<modify>");
        assert!(create < modify);
        for w in new {
            assert!(xml[create..modify].contains(&format!("<way id=\"{w}\"")));
            assert!(xml[modify..].contains(&format!("ref=\"{w}\"")));
        }
        // The kept way is modified, not created.
        assert!(xml[modify..].contains("<way id=\"11\""));
    });
}

#[gpui::test]
fn split_parts_stay_within_the_way_node_limit_of_the_server(cx: &mut TestAppContext) {
    // A way longer than the server accepts (it was drawn offline) is fine
    // again once it is cut into parts.
    let ids: Vec<i64> = (1..=9).collect();
    let data = Build::new()
        .nodes(&ids)
        .way(10, &ids, &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let limits = osm_gpui::limits::Limits {
            way_nodes: 4,
            relation_members: 100,
            changeset_elements: 1000,
        };
        s.select(&[(N, 4), (N, 7), (W, 10)]);
        assert!(s.act("split"));
        let d = s.diff();
        // Parts [1..4] [4..7] [7..9].
        for w in d.created_ways.iter().chain(&d.modified_ways) {
            assert!(w.nodes.len() <= 4);
        }
        assert!(osm_gpui::limits::check_upload(&[&d], &limits).is_ok());
    });
}

#[gpui::test]
fn split_that_adds_parts_to_a_full_relation_is_refused_whole(cx: &mut TestAppContext) {
    let max =
        osm_gpui::capabilities::max_relation_members(&osm_gpui::settings_store::api_base_url());
    // One way plus filler members that were never downloaded.
    let mut members = vec![wm(11, "")];
    members.extend((0..max - 1).map(|i| wm(9001 + i as i64, "")));
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(11, &[1, 2, 3, 4, 5], &[("highway", "residential")])
        .relation(900, &route(&[]), &members)
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // One cut adds one member and exceeds the maximum already.
        s.select(&[(N, 2), (N, 4), (W, 11)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn a_node_the_way_passes_through_twice_is_split_at_every_interior_pass(cx: &mut TestAppContext) {
    // S17, figure eight through node 3. JOSM (ticket 11184) and iD cut at the
    // repeated node; this app cuts at every interior occurrence, so the loop
    // becomes a way of its own.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5, 3, 6], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        assert_eq!(s.steps(), 1);
        // [1,2,3] | [3,4,5,3] (longest) | [3,6]
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 5, 3]));
        new_way_with(s, &[1, 2, 3]);
        new_way_with(s, &[3, 6]);
        undo_only(s, &before);
        // The way need not be selected.
        s.select(&[(N, 3)]);
        assert!(s.act("split"));
        assert_eq!(s.new_ways().len(), 2);
    });
}

#[gpui::test]
fn a_p_shaped_way_is_split_at_the_interior_pass_only(cx: &mut TestAppContext) {
    // Ends on node 2, which it also passes on the way: only the interior
    // pass is a cut, so the loop [2,3,4,2] stays together.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(11, &[1, 2, 3, 4, 2], &[("highway", "path")])
        .way(12, &[2, 3, 4, 2, 5], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(11), Some(vec![2, 3, 4, 2]));
        new_way_with(s, &[1, 2]);
        undo_only(s, &before);
        // Starting on the repeated node.
        s.select(&[(N, 2), (W, 12)]);
        assert!(s.act("split"));
        assert_eq!(s.way_nodes(12), Some(vec![2, 3, 4, 2]));
        new_way_with(s, &[2, 5]);
    });
}

#[gpui::test]
fn figure_eight_in_a_route_gets_all_three_parts_in_order(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2], &[])
        .way(11, &[2, 3, 4, 5, 3, 6], &[("highway", "service")])
        .way(12, &[6, 7], &[])
        .relation(900, &route(&[]), &[wm(10, ""), wm(11, ""), wm(12, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 11)]);
        assert!(s.act("split"));
        // [2,3] | [3,4,5,3] (keeps 11) | [3,6]
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5, 3]));
        let a = new_way_with(s, &[2, 3]);
        let b = new_way_with(s, &[3, 6]);
        assert_eq!(
            s.members(900),
            mem(&[(10, ""), (a, ""), (11, ""), (b, ""), (12, "")])
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_ring_node_visited_twice_still_cannot_be_split(cx: &mut TestAppContext) {
    // A closed way that passes through a node twice has no single arc
    // between two cuts; refused, nothing changes.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 2, 4, 1], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(!s.act("split"));
        s.select(&[(N, 2), (W, 10)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn split_at_a_shared_node_splits_every_way_through_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3], &[("highway", "residential")])
        .way(11, &[4, 2, 5, 6], &[("highway", "service")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(s.act("split"));
        assert_eq!(s.new_ways().len(), 2);
        assert_eq!(s.steps(), 1);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn the_longer_part_keeps_the_original_id_and_version(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3, 4, 5], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (W, 10)]);
        assert!(s.act("split"));
        // 10 keeps [2..5] (the longer part): it is the way that is *modified*
        // on the server, and the short part is a creation.
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 5]));
        let d = s.diff();
        assert_eq!(d.created_ways.len(), 1);
        assert_eq!(d.created_ways[0].nodes, vec![1, 2]);
        assert_eq!(d.modified_ways.len(), 1);
        assert_eq!(d.modified_ways[0].id, 10);
        assert_eq!(d.modified_ways[0].version, 3);
    });
}

#[gpui::test]
fn split_new_way_is_created_before_the_relation_that_names_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(11, &[1, 2, 3], &[("highway", "residential")])
        .relation(900, &route(&[]), &[wm(11, "")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let modify = at(&xml, "<modify>");
        assert!(create < modify);
        // The relation (modify) names the placeholder id of a way that is
        // created earlier in the same document.
        assert!(xml[create..modify].contains(&format!("<way id=\"{new}\"")));
        assert!(xml[modify..].contains(&format!("ref=\"{new}\"")));
    });
}
