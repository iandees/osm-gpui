//! Delete, Disconnect (Separate) and relation membership upkeep.

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
// Disconnect

#[gpui::test]
fn disconnect_gives_the_later_way_a_copy_and_the_original_keeps_tags_and_memberships(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 4, 5])
        .node(3, &[("highway", "traffic_signals")])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .relation(900, &[("type", "junction")], &[nm(3, "node")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("separate"));
        let copy = s.way_nodes(11).unwrap()[0];
        assert!(copy < 0);
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        // Design difference: iD copies the tags to the new node; here the
        // original keeps tags and relation membership, the copy is bare.
        assert_eq!(s.tags(N, copy), vec![]);
        assert_eq!(s.tags(N, 3), pairs(&[("highway", "traffic_signals")]));
        assert_eq!(s.members(900), mem(&[(3, "node")]));
        // New node, modified way.
        assert_eq!(s.counts(), (1, 1, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn disconnect_replaces_a_later_visit_of_the_same_way(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 5, 3, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("separate"));
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes[2], 3);
        assert!(nodes[5] < 0, "the second visit got its own node: {nodes:?}");
        assert_eq!(nodes.len(), 7);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn disconnecting_a_ring_from_another_way_keeps_the_ring_closed(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3, 4, 1], &[("building", "yes")])
        .way(11, &[5, 1, 6], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        // Detach only the ring from the shared closing node.
        s.select(&[(N, 1), (W, 10)]);
        assert!(s.act("separate"));
        let ring = s.way_nodes(10).unwrap();
        assert_eq!(ring.first(), ring.last(), "ring stays closed: {ring:?}");
        assert!(ring[0] < 0);
        assert_eq!(s.way_nodes(11), Some(vec![5, 1, 6]));
        // One new node, not one per end of the ring.
        assert_eq!(s.counts().0, 1);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn disconnect_of_a_node_used_once_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("separate"));
        assert_eq!(s.snap(), before);
    });
}

// ---------------------------------------------------------------------------
// Delete

#[gpui::test]
fn deleting_a_way_removes_it_from_the_route_and_keeps_the_rest_in_order(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .node(2, &[("name", "kept")])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .way(12, &[3, 4], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, "a"), wm(11, "b"), wm(12, "c")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 11)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(10, "a"), (12, "c")]));
        // Way deleted, relation modified; the upload deletes nothing else.
        let d = s.diff();
        assert_eq!(d.deleted_way_ids, vec![(11, 3)]);
        assert_eq!(d.modified_relations.len(), 1);
        // The relation is edited before the way goes (modify before delete).
        let xml = s.xml();
        assert!(at(&xml, "<modify>") < at(&xml, "<delete>"));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_way_listed_twice_in_a_route_loses_both_memberships(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, ""), wm(11, ""), wm(10, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(11, "")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_the_last_member_of_a_relation_is_refused(cx: &mut TestAppContext) {
    // Design difference: iD and JOSM delete the relation that becomes
    // empty; this app refuses so the user decides.
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &road())
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(!s.act("delete"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn deleting_a_way_keeps_nodes_that_are_tagged_or_shared(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 3, 4, 5])
        .node(2, &[("highway", "crossing")])
        .way(10, &[1, 2, 3, 4], &road())
        .way(11, &[4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert!(s.has_node(2), "tagged node stays");
        assert!(s.has_node(4), "node shared with way 11 stays");
        assert_eq!(s.way_nodes(11), Some(vec![4, 5]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
#[ignore = "bug: deleting a way leaves its untagged, unshared nodes behind as orphans (JOSM and iD delete them); see docs/testing/edge-cases-from-josm-and-id.md"]
fn deleting_a_way_deletes_its_untagged_unshared_nodes(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 3, 4, 5])
        .node(2, &[("highway", "crossing")])
        .way(10, &[1, 2, 3, 4], &road())
        .way(11, &[4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert!(!s.has_node(1), "orphan node 1 should go with its way");
        assert!(!s.has_node(3), "orphan node 3 should go with its way");
        assert!(s.has_node(2) && s.has_node(4));
        let d = s.diff();
        assert_eq!(d.deleted_node_ids.len(), 2);
    });
}

#[gpui::test]
#[ignore = "bug: a vertex of a way cannot be deleted (delete refuses nodes used by a way); JOSM and iD remove it from the way; see docs/testing/edge-cases-from-josm-and-id.md"]
fn deleting_a_vertex_removes_it_from_its_way(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(s.act("delete"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 3]));
        assert!(!s.has_node(2));
        undo_redo(s, &before);
    });
}

#[gpui::test]
#[ignore = "bug: deleting an end vertex of a two-node way is refused instead of deleting the way that would be too short; see docs/testing/edge-cases-from-josm-and-id.md"]
fn deleting_a_vertex_that_leaves_a_way_too_short_deletes_that_way(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1)]);
        assert!(s.act("delete"));
        assert!(!s.has_way(10));
        assert!(s.has_way(11));
    });
}

#[gpui::test]
fn deleting_a_node_used_by_a_way_changes_nothing_today(cx: &mut TestAppContext) {
    // What the app does now (see the two ignored tests above for what it
    // should do): the delete is refused and nothing is recorded.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("delete"));
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn deleting_a_way_with_its_nodes_in_one_selection_is_one_step(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1), (N, 2), (N, 3), (W, 10)]);
        assert!(s.act("delete"));
        assert_eq!(s.steps(), 1);
        assert_eq!(s.counts(), (0, 0, 4));
        // Upload deletes the way first, then its nodes.
        let xml = s.xml();
        let del = at(&xml, "<delete>");
        assert!(at(&xml[del..], "<way ") < at(&xml[del..], "<node "));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_relation_keeps_its_members(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &road())
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(R, 900)]);
        assert!(s.act("delete"));
        assert!(s.has_way(10) && s.has_node(1) && s.has_node(2));
        assert_eq!(s.counts(), (0, 0, 1));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_an_inner_ring_leaves_the_outer_member(cx: &mut TestAppContext) {
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
        s.select(&[(W, 11)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(10, "outer")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_node_member_that_is_not_on_a_way_edits_the_relation(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2])
        .node(7, &[("public_transport", "platform")])
        .way(10, &[1, 2], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[nm(7, "stop"), wm(10, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 7)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(10, "")]));
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn unknown_members_survive_edits_of_their_relation(cx: &mut TestAppContext) {
    // Ids above 9000 are members that were never downloaded.
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 1], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(9001, "x"), wm(10, "y"), nm(9002, "z"), wm(11, "w")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert_eq!(
            s.typed_members(900),
            vec![
                (MemberType::Way, 9001, "x".to_string()),
                (MemberType::Node, 9002, "z".to_string()),
                (MemberType::Way, 11, "w".to_string()),
            ]
        );
        undo_redo(s, &before);
    });
}
