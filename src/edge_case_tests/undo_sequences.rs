//! Undo and redo across several operations in a row.

use super::*;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn route_data() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(9, &[1, 2], &road())
        .way(10, &[2, 3, 4, 5], &road())
        .way(11, &[5, 6], &road())
        .relation(
            900,
            &[("type", "route"), ("route", "bus")],
            &[wm(9, ""), wm(10, "forward"), wm(11, "")],
        )
        .done()
}

#[gpui::test]
fn split_then_join_again_leaves_nothing_to_upload_and_undoes_in_two_steps(cx: &mut TestAppContext) {
    with_session(cx, route_data(), |s| {
        let original = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        let after_split = s.snap();
        let new = s.new_ways()[0];
        // Join the halves back: the server way survives, the new one goes.
        s.select(&[(W, 10), (W, new)]);
        assert!(s.act("join"));
        assert_eq!(s.way_nodes(10), Some(vec![2, 3, 4, 5]));
        assert!(!s.has_way(new));
        assert_eq!(s.members(900), mem(&[(9, ""), (10, "forward"), (11, "")]));
        assert_eq!(s.counts(), (0, 0, 0), "net no change");
        let after_join = s.snap();
        assert_eq!(after_join, original);

        s.undo();
        assert_eq!(s.snap(), after_split);
        s.undo();
        assert_eq!(s.snap(), original);
        assert_eq!(s.counts(), (0, 0, 0));

        s.redo();
        assert_eq!(s.snap(), after_split);
        assert_eq!(s.new_ways(), vec![new], "redo recreates the same id");
        s.redo();
        assert_eq!(s.snap(), after_join);
    });
}

#[gpui::test]
fn split_separate_merge_join_then_undo_all_and_redo_all(cx: &mut TestAppContext) {
    with_session(cx, route_data(), |s| {
        let original = s.snap();
        let mut states = vec![original.clone()];

        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        states.push(s.snap());
        let new = s.new_ways()[0];

        // Disconnect the halves at the split node, then merge it back.
        s.select(&[(N, 3), (W, new)]);
        assert!(s.act("separate"));
        states.push(s.snap());
        let copy = s
            .way_nodes(new)
            .unwrap()
            .iter()
            .copied()
            .find(|&n| n < 0)
            .unwrap();
        s.select(&[(N, 3), (N, copy)]);
        assert!(s.act("merge"));
        states.push(s.snap());

        s.select(&[(W, 10), (W, new)]);
        assert!(s.act("join"));
        states.push(s.snap());
        assert_eq!(s.steps(), 4);

        for i in (0..4).rev() {
            s.undo();
            assert_eq!(s.snap(), states[i], "after undoing step {}", i + 1);
        }
        assert_eq!(s.counts(), (0, 0, 0));
        for state in states.iter().skip(1) {
            s.redo();
            assert_eq!(&s.snap(), state);
        }
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn delete_then_undo_restores_the_way_its_version_and_its_route_slot(cx: &mut TestAppContext) {
    with_session(cx, route_data(), |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert_eq!(s.members(900), mem(&[(9, ""), (11, "")]));
        s.undo();
        assert_eq!(s.snap(), before);
        assert_eq!(s.members(900), mem(&[(9, ""), (10, "forward"), (11, "")]));
        // Back at the baseline: not even the version moved.
        assert_eq!(s.counts(), (0, 0, 0));
        s.redo();
        assert!(!s.has_way(10));
        assert_eq!(s.counts(), (0, 1, 1));
    });
}

#[gpui::test]
fn a_new_edit_after_undo_discards_the_redo_branch(cx: &mut TestAppContext) {
    with_session(cx, route_data(), |s| {
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        s.undo();
        s.select(&[(W, 9)]);
        assert!(s.act("reverse"));
        assert!(s.view.undo_stack.redo().is_none());
        assert_eq!(s.way_nodes(9), Some(vec![2, 1]));
        // The next split gets the id the abandoned one had (nothing else
        // used it), and the route is consistent.
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("split"));
        s.assert_no_dangling();
        assert_eq!(s.members(900).len(), 4);
    });
}
