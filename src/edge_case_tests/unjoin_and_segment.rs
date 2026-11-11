//! Further situations (batch L): "Unjoin Node from Way" (JOSM
//! `UnJoinNodeWayActionTest`, ticket 10396) and "Delete Way Segment" (JOSM
//! `DeleteCommandTest.testDeleteWaySegment*`).

use super::*;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

fn restriction(from: i64, via: i64, to: i64) -> Build {
    Build::new().relation(
        900,
        &[("type", "restriction"), ("restriction", "no_left_turn")],
        &[wm(from, "from"), nm(via, "via"), wm(to, "to")],
    )
}

// ---------------------------------------------------------------------------
// Unjoin Node from Way

#[gpui::test]
fn unjoin_gives_only_the_selected_way_a_bare_copy_and_the_original_keeps_everything(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 3, 4, 5])
        .node(2, &[("highway", "traffic_signals")])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 2, 5], &road())
        .relation(900, &[("type", "junction")], &[nm(2, "node")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        let steps = s.steps();
        s.select(&[(N, 2), (W, 10)]);
        assert!(s.act("unjoin"), "{}", s.status());
        assert_eq!(s.status(), "Unjoined a node from a way");
        let copy = s.way_nodes(10).unwrap()[1];
        assert!(copy < 0, "fresh node: {copy}");
        assert_eq!(s.way_nodes(11), Some(vec![4, 2, 5]));
        // Same position, untagged; the original keeps tags and membership.
        assert_eq!(s.snap().nodes[&copy].0, before.nodes[&2].0);
        assert_eq!(s.tags(N, copy), vec![]);
        assert_eq!(s.tags(N, 2), pairs(&[("highway", "traffic_signals")]));
        assert_eq!(s.members(900), mem(&[(2, "node")]));
        assert_eq!(s.steps(), steps + 1, "one undo step");
        // New node, modified way.
        assert_eq!(s.counts(), (1, 1, 0));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn which_way_is_unjoined_is_the_selected_one_even_if_it_is_the_older_way(cx: &mut TestAppContext) {
    // Disconnect would give the later way the copy; Unjoin gives it to the
    // way that was named.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 2, 5], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (W, 11)]);
        assert!(s.act("unjoin"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert!(s.way_nodes(11).unwrap()[1] < 0);
    });
}

#[gpui::test]
fn unjoin_without_a_way_selected_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 2, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("unjoin"));
        assert!(
            s.status().contains("Select a node and a way"),
            "{}",
            s.status()
        );
        s.select(&[(N, 2), (W, 10), (W, 11)]);
        assert!(!s.act("unjoin"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn unjoin_of_a_node_that_is_not_in_the_way_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 5, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 5), (W, 10)]);
        assert!(!s.act("unjoin"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn unjoin_of_an_end_node_is_refused_and_says_to_use_separate(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[1, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (W, 10)]);
        assert!(!s.act("unjoin"));
        assert!(s.status().contains("end of the way"), "{}", s.status());
        s.select(&[(N, 3), (W, 10)]);
        assert!(!s.act("unjoin"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        // Disconnect still handles the end node.
        s.select(&[(N, 1), (W, 10)]);
        assert!(s.act("separate"), "{}", s.status());
    });
}

#[gpui::test]
fn unjoin_of_a_node_nothing_else_uses_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        // Passes node 4 twice but is the only way there.
        .way(11, &[3, 4, 5, 4, 1], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 10)]);
        assert!(!s.act("unjoin"));
        assert!(s.status().contains("no other way"), "{}", s.status());
        s.select(&[(N, 4), (W, 11)]);
        assert!(!s.act("unjoin"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn unjoin_replaces_every_visit_of_the_way_with_one_new_node(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3, 4, 5, 3, 6], &road())
        .way(11, &[7, 3, 2], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("unjoin"), "{}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert!(nodes[2] < 0);
        assert_eq!(nodes[2], nodes[5], "both visits share the new node");
        assert_eq!(s.way_nodes(11), Some(vec![7, 3, 2]));
        assert_eq!(s.counts().0, 1);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn unjoin_on_a_ring_keeps_it_closed_whichever_node_it_is(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3, 4, 1], &[("building", "yes")])
        .way(11, &[5, 1, 6], &road())
        .way(12, &[7, 3, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // The closing node (first and last in the list).
        s.select(&[(N, 1), (W, 10)]);
        assert!(s.act("unjoin"), "{}", s.status());
        let ring = s.way_nodes(10).unwrap();
        assert_eq!(ring.first(), ring.last(), "ring stays closed: {ring:?}");
        assert!(ring[0] < 0);
        assert_eq!(&ring[1..4], &[2, 3, 4]);
        assert_eq!(s.way_nodes(11), Some(vec![5, 1, 6]));
        assert_eq!(s.counts(), (1, 1, 0), "one new node, not one per ring end");
        undo_redo(s, &before);
        s.undo();
        // An ordinary ring vertex.
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("unjoin"), "{}", s.status());
        let ring = s.way_nodes(10).unwrap();
        assert_eq!(ring.first(), ring.last());
        assert!(ring[2] < 0);
        assert_eq!(s.way_nodes(12), Some(vec![7, 3, 5]));
    });
}

#[gpui::test]
fn unjoin_is_refused_for_the_from_way_of_a_restriction_at_its_via_node(cx: &mut TestAppContext) {
    let data = restriction(10, 2, 11)
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 2, 5], &road())
        .way(12, &[6, 2, 7], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (W, 10)]);
        assert!(!s.act("unjoin"));
        assert!(s.status().contains("turn restriction"), "{}", s.status());
        s.select(&[(N, 2), (W, 11)]);
        assert!(!s.act("unjoin"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
        // A way that is not part of the restriction can be unjoined.
        s.select(&[(N, 2), (W, 12)]);
        assert!(s.act("unjoin"), "{}", s.status());
        assert_eq!(s.members(900), mem(&[(10, "from"), (2, "via"), (11, "to")]));
    });
}

#[gpui::test]
fn unjoin_keeps_the_way_node_count_so_a_full_way_stays_valid(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=2000).collect();
    let data = Build::new()
        .nodes(&ids)
        .nodes(&[3001, 3002])
        .way(10, &ids, &road())
        .way(11, &[3001, 1000, 3002], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1000), (W, 10)]);
        assert!(s.act("unjoin"), "{}", s.status());
        assert_eq!(s.way_nodes(10).unwrap().len(), 2000);
        let limits = osm_gpui::limits::Limits {
            way_nodes: 2000,
            relation_members: 32000,
            changeset_elements: 10000,
        };
        assert!(osm_gpui::limits::check_upload(&[&s.diff()], &limits).is_ok());
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn unjoin_uploads_the_new_node_before_the_way_that_uses_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[4, 2, 5], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (W, 10)]);
        assert!(s.act("unjoin"));
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let node = at(&xml, "<node id=\"-1\"");
        let modify = at(&xml, "<modify>");
        let way = at(&xml, "<way id=\"10\"");
        assert!(create < node && node < modify && modify < way, "{xml}");
        assert!(xml.contains("<nd ref=\"-1\""), "{xml}");
        assert!(
            !xml.contains("<way id=\"11\""),
            "the other way is untouched"
        );
    });
}

// ---------------------------------------------------------------------------
// Delete Way Segment

fn five() -> Build {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3, 4, 5], &road())
}

#[gpui::test]
fn a_middle_segment_splits_the_way_and_the_original_keeps_the_longer_part(cx: &mut TestAppContext) {
    with_session(cx, five().done(), |s| {
        let before = s.snap();
        let steps = s.steps();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.status(), "Deleted a way segment");
        // [1 2] and [3 4 5]: the original keeps the longer one.
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 5]));
        let new = s.new_ways();
        assert_eq!(new.len(), 1, "no way for the segment itself");
        assert_eq!(s.way_nodes(new[0]), Some(vec![1, 2]));
        assert_eq!(s.tags(W, new[0]), pairs(&road()));
        // Every node stays: each is the end of a remaining part.
        for n in 1..=5 {
            assert!(s.has_node(n));
        }
        assert_eq!(s.steps(), steps + 1, "one undo step");
        assert_eq!(s.counts(), (1, 1, 0));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn the_left_part_stays_when_it_is_longer_or_ties(cx: &mut TestAppContext) {
    with_session(cx, five().done(), |s| {
        s.select(&[(N, 4), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.way_nodes(s.new_ways()[0]), Some(vec![4, 5]));
    });
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2]));
        assert_eq!(s.way_nodes(s.new_ways()[0]), Some(vec![3, 4]));
    });
}

#[gpui::test]
fn it_does_not_matter_which_node_was_selected_first_or_whether_the_way_is_too(
    cx: &mut TestAppContext,
) {
    with_session(cx, five().done(), |s| {
        s.select(&[(N, 3), (N, 2), (W, 10)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 5]));
    });
}

#[gpui::test]
fn a_segment_at_the_end_shortens_the_way_and_deletes_the_orphaned_end_node(
    cx: &mut TestAppContext,
) {
    with_session(cx, five().done(), |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 5]));
        assert!(s.new_ways().is_empty());
        assert!(!s.has_node(1), "untagged and unshared: deleted");
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
        s.undo();
        assert!(s.has_node(1));
        // The other end.
        s.select(&[(N, 5), (N, 4)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4]));
        assert!(!s.has_node(5));
    });
}

#[gpui::test]
fn tagged_shared_or_member_end_nodes_survive_the_segment(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[("barrier", "gate")])
        .nodes(&[2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .way(12, &[6, 7, 3], &road())
        .relation(900, &[("type", "junction")], &[nm(7, "")])
        .done();
    with_session(cx, data, |s| {
        // Tagged end node.
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert!(s.has_node(1));
        assert_eq!(s.way_nodes(10), Some(vec![2, 3]));
        // End node shared with another way.
        s.select(&[(N, 2), (N, 3)]);
        assert!(
            !s.act("delete_segment"),
            "a two-node way is the whole segment"
        );
        s.select(&[(N, 6), (N, 7), (W, 12)]);
        // End node 6 goes; 7 is in a relation and stays (it is not an end).
        assert!(s.act("delete_segment"), "{}", s.status());
        assert!(!s.has_node(6));
        assert!(s.has_node(7));
        assert_eq!(s.way_nodes(12), Some(vec![7, 3]));
    });
}

#[gpui::test]
fn deleting_the_only_segment_of_a_two_node_way_is_refused(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1, 2]).way(10, &[1, 2], &road()).done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2)]);
        assert!(!s.act("delete_segment"));
        assert!(
            s.status().contains("delete the way instead"),
            "{}",
            s.status()
        );
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn a_closed_way_is_opened_at_the_segment_without_creating_anything(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 1], &[("building", "yes")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        // Travels on from 3 round to 2.
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 1, 2]));
        assert!(s.new_ways().is_empty());
        for n in 1..=4 {
            assert!(s.has_node(n));
        }
        assert_eq!(s.counts(), (0, 1, 0));
        undo_redo(s, &before);
        s.undo();
        // The segment that closes the ring (4 - 1) works too.
        s.select(&[(N, 4), (N, 1)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4]));
    });
}

#[gpui::test]
fn nodes_that_are_not_next_to_each_other_or_not_two_are_refused(cx: &mut TestAppContext) {
    with_session(cx, five().done(), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4)]);
        assert!(!s.act("delete_segment"));
        assert!(s.status().contains("next to each other"), "{}", s.status());
        s.select(&[(N, 2)]);
        assert!(!s.act("delete_segment"));
        s.select(&[(N, 2), (N, 3), (N, 4)]);
        assert!(!s.act("delete_segment"));
        s.select(&[(W, 10)]);
        assert!(!s.act("delete_segment"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn a_segment_shared_by_two_ways_needs_the_way_to_be_selected(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8])
        .way(10, &[1, 2, 3, 4], &road())
        .way(11, &[5, 2, 3, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("delete_segment"));
        assert!(s.status().contains("select the way too"), "{}", s.status());
        assert_eq!(s.snap(), before);
        s.select(&[(N, 2), (N, 3), (W, 11)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(
            s.way_nodes(10),
            Some(vec![1, 2, 3, 4]),
            "the other way is untouched"
        );
        assert_eq!(s.way_nodes(11), Some(vec![5, 2]));
        let new = s.new_ways();
        assert_eq!(s.way_nodes(new[0]), Some(vec![3, 6]));
    });
}

#[gpui::test]
fn a_way_that_runs_along_the_segment_twice_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4, 3, 2, 1], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("delete_segment"));
        assert!(s.status().contains("more than once"), "{}", s.status());
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn a_way_that_passes_the_node_twice_elsewhere_keeps_it_in_both_parts(cx: &mut TestAppContext) {
    // [1 2 3 4 2 5]: the segment 3 - 4 is only there once, node 2 is visited
    // twice and stays in both parts.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3, 4, 2, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 4)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.way_nodes(s.new_ways()[0]), Some(vec![4, 2, 5]));
        assert!(s.has_node(2));
        undo_redo(s, &before);
        s.undo();
        // A segment at an end whose node is visited again does not lose it.
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 2, 5]));
    });
}

#[gpui::test]
fn a_route_gets_both_remaining_parts_in_order_and_not_the_segment(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(9, &[6, 1], &road())
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[5, 7], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(9, "a"), wm(10, "b"), wm(11, "c")],
        )
        .relation(901, &[("type", "route"), ("route", "bus")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Left part [1 2 3] stays as way 10, right part [4 5] is new.
        s.select(&[(N, 3), (N, 4)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        let new = s.new_ways()[0];
        assert_eq!(
            s.members(900),
            mem(&[(9, "a"), (10, "b"), (new, "b"), (11, "c")])
        );
        assert_eq!(s.members(901), mem(&[(10, ""), (new, "")]));
        assert_eq!(s.snap().relations.len(), 2);
        assert_eq!(s.counts(), (1, 3, 0));
        undo_redo(s, &before);
        s.undo();
        // Right part [3 4 5] stays, left part [1 2] is new and goes first.
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(10), Some(vec![3, 4, 5]));
        assert_eq!(
            s.members(900),
            mem(&[(9, "a"), (new, "b"), (10, "b"), (11, "c")])
        );
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn a_shortened_way_stays_in_its_relations_unchanged(cx: &mut TestAppContext) {
    let data = five()
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(10, "a")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.members(900), mem(&[(10, "a")]));
        // Only the way and the node changed, not the relation.
        assert_eq!(s.counts(), (0, 1, 1));
    });
}

#[gpui::test]
fn a_restriction_whose_via_node_stays_with_one_part_follows_that_part(cx: &mut TestAppContext) {
    let data = restriction(10, 1, 11)
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[1, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Via 1 stays with the left part [1 2]; the right [3 4 5] is longer
        // and the original, so `from` is re-pointed to the new way.
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(new), Some(vec![1, 2]));
        assert_eq!(
            s.members(900),
            mem(&[(new, "from"), (1, "via"), (11, "to")])
        );
        undo_redo(s, &before);
    });
    // Via 5 stays with the original: nothing in the relation changes.
    let data = restriction(10, 5, 11)
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[5, 6], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.members(900), mem(&[(10, "from"), (5, "via"), (11, "to")]));
    });
}

#[gpui::test]
fn a_restriction_is_refused_when_the_segment_touches_the_via_node(cx: &mut TestAppContext) {
    let data = restriction(10, 3, 11)
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5], &road())
        .way(11, &[3, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Node 3 would belong to both parts: the half to follow is unclear.
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("delete_segment"));
        assert!(!s.status().is_empty());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_restriction_that_would_lose_its_via_node_at_a_shortened_end_is_refused(
    cx: &mut TestAppContext,
) {
    let data = restriction(10, 1, 11)
        .nodes(&[1, 2, 3, 6])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[1, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2)]);
        assert!(!s.act("delete_segment"));
        assert!(s.status().contains("turn restriction"), "{}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

fn route_of(total: usize) -> OsmData {
    let mut members: Vec<(MemberType, i64, &str)> = vec![wm(10, "")];
    while members.len() < total {
        members.push(nm(10_001 + members.len() as i64, "stop"));
    }
    five()
        .relation(900, &[("type", "route"), ("route", "bus")], &members)
        .done()
}

#[gpui::test]
fn a_relation_may_grow_to_exactly_32000_members_but_not_past_it(cx: &mut TestAppContext) {
    with_session(cx, route_of(31_999), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        assert_eq!(s.members(900).len(), 32_000);
        undo_redo(s, &before);
    });
    with_session(cx, route_of(32_000), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("delete_segment"));
        assert!(s.status().contains("32,001"), "{}", s.status());
        assert_eq!(s.snap(), before);
        // A segment at the end does not add a member, so it still works.
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
    });
}

#[gpui::test]
fn both_parts_fit_the_way_node_limit_that_the_whole_way_exceeded(cx: &mut TestAppContext) {
    let ids: Vec<i64> = (1..=9).collect();
    let data = Build::new()
        .nodes(&ids)
        .way(10, &ids, &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let limits = osm_gpui::limits::Limits {
            way_nodes: 5,
            relation_members: 100,
            changeset_elements: 1000,
        };
        assert!(osm_gpui::limits::check_upload(&[&s.diff()], &limits).is_ok());
        s.select(&[(N, 4), (N, 5)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        // Parts [1..4] and [5..9].
        let d = s.diff();
        for w in d.created_ways.iter().chain(&d.modified_ways) {
            assert!(w.nodes.len() <= 5);
        }
        assert!(osm_gpui::limits::check_upload(&[&d], &limits).is_ok());
    });
}

#[gpui::test]
fn upload_creates_the_new_way_then_modifies_the_old_one_then_deletes_nodes(
    cx: &mut TestAppContext,
) {
    with_session(cx, five().done(), |s| {
        // First a middle segment: one new way, one modified way.
        s.select(&[(N, 3), (N, 4)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let way = at(&xml, "<way id=\"-1\"");
        let modify = at(&xml, "<modify>");
        let old = at(&xml, "<way id=\"10\"");
        assert!(create < way && way < modify && modify < old, "{xml}");
        assert!(!xml.contains("<delete>"));
        // Then an end segment of the original: its end node is deleted.
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("delete_segment"), "{}", s.status());
        let xml = s.xml();
        let modify = at(&xml, "<modify>");
        let delete = at(&xml, "<delete>");
        assert!(modify < delete, "{xml}");
        assert!(xml[delete..].contains("<node id=\"1\""), "{xml}");
    });
}

#[gpui::test]
fn undoing_a_segment_deletion_after_other_edits_in_between_restores_each_step(
    cx: &mut TestAppContext,
) {
    with_session(cx, five().done(), |s| {
        let start = s.snap();
        s.set_tag(W, 10, "name", "Main");
        s.select(&[(N, 3), (N, 4)]);
        let with_name = s.snap();
        assert!(s.act("delete_segment"), "{}", s.status());
        // The new way carries the tags the original has at that moment.
        let new = s.new_ways()[0];
        assert_eq!(
            s.tags(W, new),
            pairs(&[("highway", "residential"), ("name", "Main")])
        );
        s.undo();
        assert_eq!(s.snap(), with_name);
        s.redo();
        assert_eq!(s.snap().ways.len(), 2);
        s.undo();
        assert_ne!(s.snap(), start);
    });
}
