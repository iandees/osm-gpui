//! Purge: drop objects from the layer's
//! downloaded data without deleting them. They leave the data and the server
//! baseline together, so the upload diff never mentions them.

use super::*;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0), "restored as downloaded, unmodified");
    s.redo();
    assert_eq!(s.snap(), after, "redo purges the same objects again");
    assert_eq!(s.counts(), (0, 0, 0));
}

#[gpui::test]
fn an_unmodified_node_is_purged_not_deleted(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1], &road())
        .node(3, &[("amenity", "cafe")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_node(3));
        assert!(s.has_node(1) && s.has_node(2));
        assert_eq!(s.steps(), 1, "one undo step");
        assert_eq!(s.counts(), (0, 0, 0), "nothing to upload");
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_way_takes_its_unshared_nodes_with_it(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_way(10));
        assert!(!s.has_node(1) && !s.has_node(2) && !s.has_node(3));
        assert!(s.has_node(4), "an unrelated node stays");
        assert_eq!(s.counts(), (0, 0, 0));
        assert!(s.xml().contains("<osmChange"));
        assert!(!s.xml().contains("<delete"), "{}", s.xml());
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_node_shared_with_another_way_or_a_relation_stays(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4], &road())
        .relation(900, &[("type", "site")], &[nm(2, "stop")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_way(10));
        assert!(s.has_node(3), "used by way 11");
        assert!(s.has_node(2), "member of relation 900");
        assert!(!s.has_node(1), "nobody else needs it");
        assert!(
            s.status().contains('2'),
            "kept count mentioned: {}",
            s.status()
        );
        assert_eq!(s.way_nodes(11), Some(vec![3, 4]));
        assert_eq!(s.counts(), (0, 0, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_node_that_a_remaining_way_uses_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("purge"));
        assert!(s.status().contains("10"), "names the way: {}", s.status());
        assert_eq!(s.snap(), before, "nothing changed, not even node 3");
        assert_eq!(s.steps(), 0);
        // With the way in the selection it is fine.
        s.select(&[(N, 2), (W, 10)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_node(2) && !s.has_way(10));
    });
}

#[gpui::test]
fn objects_with_pending_edits_are_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .done();
    with_session(cx, data, |s| {
        s.set_tag(N, 1, "name", "edited");
        s.set_tag(W, 11, "name", "edited");
        let new = s.editable_mut().add_node(40.0, -74.0);
        let diff = s.counts();

        // A modified node (way 10 is not selected, so 1 is also in a way).
        s.select(&[(W, 10)]);
        assert!(
            s.act("purge"),
            "unmodified way, child 1 is edited and stays"
        );
        assert!(s.has_node(1), "the edited child stays");
        assert!(!s.has_node(2));
        s.undo();

        s.select(&[(N, 1), (W, 10)]);
        assert!(!s.act("purge"));
        assert!(s.status().contains("not uploaded"), "{}", s.status());

        s.select(&[(W, 11)]);
        assert!(!s.act("purge"), "modified way refused");
        s.select(&[(N, new)]);
        assert!(!s.act("purge"), "locally created node refused");
        assert_eq!(s.counts(), diff, "pending edits untouched");
        assert!(s.has_way(11) && s.has_node(new));
    });
}

#[gpui::test]
fn a_relation_with_an_edited_member_list_is_refused(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &road())
        .relation(900, &[("type", "route")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(R, 900)]);
        s.editable_mut()
            .set_relation_members(900, Vec::new())
            .unwrap();
        assert!(!s.act("purge"));
        assert!(s.status().contains("900"), "{}", s.status());
        assert!(s.editable().relation(900).is_some());
    });
}

#[gpui::test]
fn relation_members_that_are_purged_stay_as_members_not_downloaded(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(10, "forward"), wm(11, "forward")],
        )
        .relation(901, &[("type", "superroute")], &[rm(900, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (R, 900)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_way(10));
        assert!(s.editable().relation(900).is_none());
        // Way 11 still holds node 2; relation 901 still names 900.
        assert!(s.has_way(11) && s.has_node(2));
        assert_eq!(
            s.typed_members(901),
            vec![(MemberType::Relation, 900, String::new())]
        );
        assert_eq!(s.counts(), (0, 0, 0), "no deletes, no modifies");
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn purging_a_way_leaves_the_relation_naming_it_unchanged(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .relation(900, &[("type", "route")], &[wm(10, "forward"), wm(11, "")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_way(10));
        assert_eq!(s.members(900), mem(&[(10, "forward"), (11, "")]));
        assert_eq!(s.counts(), (0, 0, 0), "the relation is not modified");
        // Editing the relation afterwards keeps the missing member (X12).
        s.select(&[(W, 11)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(10, "forward")]));
        s.undo();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn upload_xml_is_empty_for_purged_objects(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .relation(900, &[("type", "route")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10), (R, 900)]);
        assert!(s.act("purge"), "{}", s.status());
        let diff = s.diff();
        assert!(diff.is_empty(), "{diff:?}");
        let xml = s.xml();
        assert!(
            !xml.contains("<delete") && !xml.contains("<modify"),
            "{xml}"
        );
        // The purge is not a "modification" the layer would ask to save.
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn the_selection_is_cleared_after_a_purge(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1, 2]).done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 2)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(s.view.selected.is_empty());
        assert!(!s.has_node(1) && !s.has_node(2));
        assert!(s.status().contains('2'), "{}", s.status());
    });
}

#[gpui::test]
fn purge_with_nothing_loaded_to_purge_is_a_no_op(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1]).done();
    with_session(cx, data, |s| {
        s.select(&[(N, 99)]);
        assert!(!s.act("purge"));
        assert!(s.has_node(1));
        s.select(&[]);
        assert!(!s.act("purge"));
    });
}

#[gpui::test]
fn node_and_relation_member_limits_do_not_apply(cx: &mut TestAppContext) {
    // A way over the 2,000-node limit and a relation over the member limit
    // can be purged: they are not edited, only forgotten.
    let ids: Vec<i64> = (1..=2500).collect();
    let members: Vec<(MemberType, i64, &str)> = (1..=400).map(|i| nm(i, "")).collect();
    let data = Build::new()
        .nodes(&ids)
        .way(10, &ids, &road())
        .relation(900, &[("type", "site")], &members)
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (R, 900)]);
        assert!(s.act("purge"), "{}", s.status());
        assert!(!s.has_way(10) && !s.has_node(1));
        assert_eq!(s.counts(), (0, 0, 0));
        s.undo();
        assert_eq!(s.snap(), before);
    });
}
