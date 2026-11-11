//! Join Ways (JOSM "Combine Way", iD "Join") and Merge Nodes.

use super::*;

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

// ---------------------------------------------------------------------------
// Join

#[gpui::test]
fn join_reverses_a_way_to_make_the_chain(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(
            11,
            &[5, 4, 3],
            &[("highway", "residential"), ("name", "Elm")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4, 5]));
        assert!(!s.has_way(11));
        // Tags of both survive on the one way.
        assert_eq!(
            s.tags(W, 10),
            pairs(&[("highway", "residential"), ("name", "Elm")])
        );
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_keeps_the_direction_of_a_way_that_cannot_be_reversed(cx: &mut TestAppContext) {
    // Way 11 has a direction-dependent tag. Reversing way 10 (plain) instead
    // of 11 keeps its meaning.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(
            11,
            &[5, 4, 3],
            &[("highway", "residential"), ("lanes:forward", "2")],
        )
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        assert_eq!(s.way_nodes(10), Some(vec![5, 4, 3, 2, 1]));
        assert!(s
            .tags(W, 10)
            .contains(&("lanes:forward".to_string(), "2".to_string())));
    });
}

#[gpui::test]
fn join_closes_two_arcs_into_a_ring(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[1, 4, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes.first(), nodes.last(), "the result is a closed way");
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_refuses_a_t_junction_and_three_ways_meeting_at_an_end(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[2, 4], &road())
        .way(12, &[2, 5], &road())
        .way(13, &[3, 1, 2], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // T: way 11 starts in the middle of way 10.
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"));
        // Two ways ending on the middle of a third.
        s.select(&[(W, 11), (W, 12), (W, 10)]);
        assert!(!s.act("join"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn join_with_conflicting_tags_names_the_key_and_changes_nothing(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(
            10,
            &[1, 2],
            &[("highway", "residential"), ("surface", "asphalt")],
        )
        .way(
            11,
            &[2, 3],
            &[("highway", "residential"), ("surface", "gravel")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"));
        assert!(s.status().contains("surface"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn join_of_two_server_ways_keeps_the_first_selected_id(cx: &mut TestAppContext) {
    // Design difference: iD and JOSM keep the oldest way; this app keeps the
    // only server way if there is exactly one, else the first selected.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 11), (W, 10)]);
        assert!(s.act("join"));
        assert!(s.has_way(11));
        assert!(!s.has_way(10));
        assert_eq!(s.counts(), (0, 1, 1));
    });
}

#[gpui::test]
fn join_into_a_member_of_a_route_keeps_the_route_in_order(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(9, &[1, 2], &road())
        .way(10, &[2, 3, 4], &road())
        .way(11, &[4, 5], &road())
        .way(12, &[5, 6], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(9, ""), wm(10, ""), wm(11, ""), wm(12, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        // Adjacent members with the same role collapse into the survivor.
        assert_eq!(s.members(900), mem(&[(9, ""), (10, ""), (12, "")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_where_only_the_removed_way_is_a_member_points_the_member_at_the_survivor(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(8, &[1, 2], &road())
        .way(10, &[2, 3], &road())
        .way(11, &[3, 4], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(8, ""), wm(11, "forward")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // 10 survives (first selected, both are server ways); only 11 is a member.
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        assert_eq!(s.members(900), mem(&[(8, ""), (10, "forward")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_of_ways_that_are_not_adjacent_in_a_route_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .way(12, &[3, 4, 5], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, ""), wm(12, ""), wm(11, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn join_refuses_a_turn_restriction_member_and_a_role_clash(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .way(12, &[3, 4], &road())
        .way(13, &[1, 5], &road())
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            &[wm(10, "from"), nm(1, "via"), wm(13, "to")],
        )
        .relation(
            901,
            &[("type", "multipolygon")],
            &[wm(11, "outer"), wm(12, "inner")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 11), (W, 10)]);
        assert!(!s.act("join"), "10 is a restriction's from member");
        s.select(&[(W, 11), (W, 12)]);
        assert!(!s.act("join"), "outer and inner would merge");
        assert_eq!(s.snap(), before);
    });
}

// ---------------------------------------------------------------------------
// Merge nodes

#[gpui::test]
fn merge_unions_tags_and_moves_ways_and_relation_roles_to_the_survivor(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[])
        .node(2, &[("highway", "bus_stop")])
        .node(3, &[("name", "Main St")])
        .node(4, &[])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[nm(2, "stop")],
        )
        .relation(
            901,
            &[
                ("type", "public_transport"),
                ("public_transport", "stop_area"),
            ],
            &[nm(3, "platform")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"));
        // Both server nodes: the first selected survives.
        assert_eq!(
            s.tags(N, 2),
            pairs(&[("highway", "bus_stop"), ("name", "Main St")])
        );
        assert!(!s.has_node(3));
        assert_eq!(s.way_nodes(11), Some(vec![2, 4]));
        assert_eq!(s.members(900), mem(&[(2, "stop")]));
        assert_eq!(s.members(901), mem(&[(2, "platform")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merge_keeps_both_memberships_of_one_relation_when_the_roles_differ(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .relation(
            900,
            &[
                ("type", "public_transport"),
                ("public_transport", "stop_area"),
            ],
            &[nm(1, "stop"), wm(10, ""), nm(3, "platform")],
        )
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 3)]);
        assert!(s.act("merge"));
        assert_eq!(
            s.members(900),
            mem(&[(1, "stop"), (10, ""), (1, "platform")])
        );
    });
}

#[gpui::test]
fn merge_of_neighbours_in_one_way_does_not_repeat_the_node(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 4]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merge_of_the_two_ends_of_an_open_way_closes_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 4)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 1]));
    });
}

#[gpui::test]
fn merge_refuses_to_collapse_a_two_node_way_to_one_node(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2)]);
        assert!(!s.act("merge"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn merge_must_not_leave_a_degenerate_closed_way(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3, 1], &[("building", "yes")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 2)]);
        let did = s.act("merge");
        let way = s.way_nodes(10);
        let distinct = way.as_ref().map(|w| {
            let mut d = w.clone();
            d.sort();
            d.dedup();
            d.len()
        });
        assert!(
            !did || way.is_none() || distinct.unwrap() >= 3,
            "left {way:?}"
        );
    });
}

#[gpui::test]
fn merge_prefers_the_server_node_over_a_node_made_in_this_session(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[])
        .node(2, &[])
        .node(3, &[])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Detach way 11 from node 2 (gives it a fresh negative node), then
        // merge the copy back, selecting the new one first.
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("separate"));
        let copy = s.way_nodes(11).unwrap()[0];
        assert!(copy < 0);
        s.select(&[(N, copy), (N, 2)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(11), Some(vec![2, 3]));
        assert!(!s.has_node(copy));
        // Net effect nothing: the temporary node never reaches the server.
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn merge_with_conflicting_tags_names_the_key_and_changes_nothing(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[("name", "A")])
        .node(2, &[("name", "B")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2)]);
        assert!(!s.act("merge"));
        assert!(s.status().contains("name"));
        assert_eq!(s.snap(), before);
    });
}
