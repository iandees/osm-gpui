//! Further situations (batch B): merge of non-neighbouring nodes, disconnect
//! and the upload order, join with odd selections, and deleting a way that a
//! relation lists twice.

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
// Merge nodes in one way that are not neighbours

#[gpui::test]
fn merge_of_non_neighbours_in_one_way_makes_the_way_revisit_the_survivor(cx: &mut TestAppContext) {
    // Design: allowed. The way becomes [1,2,3,2]: a legal (self-touching)
    // way in OSM, with node 2 visited twice and 4 gone.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4)]);
        assert!(s.act("merge"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 2]));
        assert!(s.has_node(2) && !s.has_node(4));
        s.assert_no_dangling();
        // Node 4 is deleted, way 10 modified.
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merge_of_non_neighbours_with_the_other_node_selected_first_keeps_that_node(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        s.select(&[(N, 4), (N, 2)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 3, 4]));
        assert!(!s.has_node(2));
    });
}

// ---------------------------------------------------------------------------
// Disconnect and the upload order

#[gpui::test]
fn disconnect_creates_the_new_node_before_the_way_that_names_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 4, 5])
        .node(3, &[])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("separate"));
        let copy = s.way_nodes(11).unwrap()[0];
        assert!(copy < 0);
        assert_eq!(s.counts(), (1, 1, 0));
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let modify = at(&xml, "<modify>");
        assert!(create < modify, "creations come first: {xml}");
        let created = &xml[create..modify];
        assert!(created.contains(&format!("<node id=\"{copy}\"")), "{xml}");
        // The way that names the new node is only modified, after the
        // creation block, and it refers to the new node by its negative id.
        let modified = &xml[modify..];
        assert!(
            at(modified, "<way id=\"11\"") < at(modified, &format!("<nd ref=\"{copy}\"")),
            "{xml}"
        );
        assert!(!created.contains("<way "), "no way is created: {xml}");
        undo_redo(s, &before);
        // After the redo the node is created again, still before the way.
        let xml = s.xml();
        assert!(at(&xml, "<create>") < at(&xml, "<modify>"), "{xml}");
    });
}

#[gpui::test]
fn disconnect_of_a_ring_and_a_second_disconnect_create_nodes_before_both_ways(
    cx: &mut TestAppContext,
) {
    // Two disconnects in a row: two created nodes, both before the modified
    // ways, and three undos walk back one step at a time.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .way(12, &[2, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("separate"));
        let one = s.snap();
        s.select(&[(N, 4)]);
        assert!(s.act("separate"));
        assert_eq!(s.counts().0, 2);
        let xml = s.xml();
        let last_create_node = xml.rfind("<node ").unwrap();
        let first_modify = at(&xml, "<modify>");
        assert!(last_create_node < first_modify, "{xml}");
        let after = s.snap();
        s.undo();
        assert_eq!(s.snap(), one);
        s.undo();
        assert_eq!(s.snap(), before);
        s.redo();
        s.redo();
        assert_eq!(s.snap(), after);
    });
}

// ---------------------------------------------------------------------------
// Join with odd selections

#[gpui::test]
fn join_of_a_single_way_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(!s.act("join"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn join_of_a_closed_way_and_an_open_way_touching_it_is_refused(cx: &mut TestAppContext) {
    // Design: a ring has no free end, so it cannot be chained with a tail
    // (iD refuses the same). Nothing changes and no undo step is recorded.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 1], &road())
        .way(11, &[1, 4], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"));
        assert!(s.status().contains("end to end"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn join_of_two_closed_ways_sharing_a_node_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3, 1], &road())
        .way(11, &[1, 4, 5, 1], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn join_of_a_way_with_itself_changes_nothing(cx: &mut TestAppContext) {
    // The same way listed twice in the selection.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 10)]);
        assert!(!s.act("join"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

// ---------------------------------------------------------------------------
// Delete a way a relation lists twice

#[gpui::test]
fn deleting_a_way_listed_twice_then_undo_restores_both_slots_in_place(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .way(12, &[3, 4], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[
                wm(11, "first"),
                wm(10, "a"),
                wm(12, ""),
                wm(10, "b"),
                wm(11, "last"),
            ],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert_eq!(
            s.members(900),
            mem(&[(11, "first"), (12, ""), (11, "last")])
        );
        s.assert_no_dangling();
        let d = s.diff();
        assert_eq!(d.deleted_way_ids, vec![(10, 3)]);
        assert_eq!(d.modified_relations.len(), 1);
        let xml = s.xml();
        assert!(at(&xml, "<modify>") < at(&xml, "<delete>"));
        s.undo();
        assert_eq!(
            s.members(900),
            mem(&[(11, "first"), (10, "a"), (12, ""), (10, "b"), (11, "last")]),
            "both slots are back at their original positions with their roles"
        );
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
        s.redo();
        assert_eq!(
            s.members(900),
            mem(&[(11, "first"), (12, ""), (11, "last")])
        );
        // Undo a second time to be sure the redo kept enough to undo again.
        s.undo();
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn deleting_a_way_that_is_the_only_two_entries_of_a_relation_is_refused(cx: &mut TestAppContext) {
    // Design: like a single-member relation (see X3), a relation whose every
    // slot is the deleted way would become empty, so the delete is refused.
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, "a"), wm(10, "b")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(!s.act("delete"), "status: {}", s.status());
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}
