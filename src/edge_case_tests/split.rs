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

#[gpui::test]
fn closed_way_needs_two_nodes_and_one_node_changes_nothing(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 1], &[("highway", "residential")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 10)]);
        assert!(!s.act("split"), "one node cannot split a ring");
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        // The ring's own closing node is no different.
        s.select(&[(N, 1), (W, 10)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
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
fn splitting_an_open_way_at_two_nodes_at_once_is_not_supported(cx: &mut TestAppContext) {
    // JOSM and iD cut an open way into three at two nodes. Feature absent
    // here: the action refuses and leaves the data alone.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3, 4, 5], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(!s.act("split"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn a_node_the_way_passes_through_twice_cannot_be_split_here(cx: &mut TestAppContext) {
    // Figure eight through node 3 and a P shape ending on node 2. JOSM and
    // iD split these at the repeated node; this app refuses (safe, but a
    // gap), changing nothing.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3, 4, 5, 3, 6], &[("highway", "path")])
        .way(11, &[1, 2, 3, 4, 2], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(!s.act("split"));
        s.select(&[(N, 2), (W, 11)]);
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
