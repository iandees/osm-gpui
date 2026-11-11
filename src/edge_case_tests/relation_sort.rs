//! Sort members of a relation: ways into connected order, roles and
//! other members untouched, one undo step, and only the relation in the
//! upload.

use super::*;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn route() -> [(&'static str, &'static str); 2] {
    [("type", "route"), ("route", "bus")]
}

/// Ways 1..=4 chain 1-2, 2-3, 3-4, 4-5; ways 5 and 6 chain 10-11-12 apart
/// from them; way 22 is stored backwards (3 to 2); way 30 is a roundabout
/// over 2, 6, 7; way 31 leaves it at 7; ways 40-42 are a closed loop.
fn network(members: &[(MemberType, i64, &str)]) -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 20])
        .way(1, &[1, 2], &road())
        .way(2, &[2, 3], &road())
        .way(3, &[3, 4], &road())
        .way(4, &[4, 5], &road())
        .way(5, &[10, 11], &road())
        .way(6, &[11, 12], &road())
        .way(22, &[3, 2], &road())
        .way(30, &[2, 6, 7, 2], &road())
        .way(31, &[7, 8], &road())
        .way(40, &[1, 2], &road())
        .way(41, &[2, 3], &road())
        .way(42, &[3, 1], &road())
        .way(43, &[1, 2], &road())
        .way(44, &[2, 3], &road())
        .way(45, &[3, 4], &road())
        .way(46, &[4, 1], &road())
        .relation(900, &route(), members)
        .done()
}

/// Sort relation 900 the way the Members panel button does; returns whether
/// an undo step was recorded.
fn sort(s: &mut Session) -> bool {
    let before = s.view.undo_stack.revision;
    let layer = s.layer;
    s.view.sort_relation_members(layer, 900);
    s.view.undo_stack.revision != before
}

/// Sort, expect the order `expected` (ids only), one undo step and only the
/// relation in the upload.
fn expect_sorted(cx: &mut TestAppContext, members: &[(MemberType, i64, &str)], expected: &[i64]) {
    let expected = expected.to_vec();
    with_session(cx, network(members), move |s| {
        let before = s.snap();
        let mut roles_before = s.members(900);
        assert!(sort(s), "an undo step is recorded");
        let got: Vec<i64> = s.members(900).iter().map(|m| m.0).collect();
        assert_eq!(got, expected);
        let mut roles_after = s.members(900);
        roles_before.sort();
        roles_after.sort();
        assert_eq!(roles_before, roles_after, "every member keeps its role");
        assert_eq!(s.steps(), 1);
        assert_eq!(s.counts(), (0, 1, 0), "only the relation is modified");
        assert!(!s.xml().contains("<way"), "no way in the upload");
        let after = s.snap();
        s.undo();
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
        s.redo();
        assert_eq!(s.snap(), after);
    });
}

fn expect_unchanged(cx: &mut TestAppContext, members: &[(MemberType, i64, &str)]) {
    with_session(cx, network(members), |s| {
        let before = s.snap();
        assert!(!sort(s), "no undo step");
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn scrambled_route_is_chained_and_other_members_stay(cx: &mut TestAppContext) {
    expect_sorted(
        cx,
        &[
            nm(20, "stop"),
            wm(3, "forward"),
            wm(1, ""),
            wm(4, "forward"),
            wm(2, ""),
            rm(9001, "sub"),
        ],
        &[20, 1, 2, 3, 4, 9001],
    );
}

#[gpui::test]
fn reversed_ways_are_ordered_not_reversed(cx: &mut TestAppContext) {
    // Way 22 is stored 3 to 2; the walk 1-2-3-4 uses it backwards.
    expect_sorted(cx, &[wm(22, ""), wm(3, ""), wm(1, "")], &[1, 22, 3]);
    with_session(cx, network(&[wm(22, ""), wm(3, ""), wm(1, "")]), |s| {
        sort(s);
        assert_eq!(s.way_nodes(22), Some(vec![3, 2]), "the way is not reversed");
    });
}

#[gpui::test]
fn already_sorted_is_a_no_op(cx: &mut TestAppContext) {
    expect_unchanged(cx, &[wm(1, ""), wm(2, ""), wm(3, ""), wm(4, "")]);
    expect_unchanged(cx, &[wm(1, "")]);
    expect_unchanged(cx, &[]);
}

#[gpui::test]
fn forward_and_backward_roles_constrain_the_walk(cx: &mut TestAppContext) {
    expect_sorted(cx, &[wm(2, "forward"), wm(1, "forward")], &[1, 2]);
    // Way 1 walked backwards ends at node 1, so nothing connects: unchanged.
    expect_unchanged(cx, &[wm(2, "forward"), wm(1, "backward")]);
}

#[gpui::test]
fn gaps_keep_chains_in_original_order(cx: &mut TestAppContext) {
    expect_sorted(
        cx,
        &[wm(2, ""), wm(6, ""), wm(3, ""), wm(5, ""), wm(1, "")],
        &[1, 2, 3, 6, 5],
    );
}

#[gpui::test]
fn roundabout_connects_on_any_node(cx: &mut TestAppContext) {
    expect_sorted(cx, &[wm(30, ""), wm(31, ""), wm(1, "")], &[1, 30, 31]);
}

#[gpui::test]
fn loop_starts_at_the_first_member(cx: &mut TestAppContext) {
    expect_sorted(
        cx,
        &[wm(44, ""), wm(46, ""), wm(43, ""), wm(45, "")],
        &[44, 45, 46, 43],
    );
    expect_unchanged(cx, &[wm(40, ""), wm(41, ""), wm(42, "")]);
}

#[gpui::test]
fn out_and_back_route_stays_a_valid_walk(cx: &mut TestAppContext) {
    expect_unchanged(cx, &[wm(1, ""), wm(2, ""), wm(2, ""), wm(1, "")]);
    let scrambled = [
        wm(1, ""),
        wm(3, ""),
        wm(2, ""),
        wm(3, ""),
        wm(1, ""),
        wm(2, ""),
    ];
    with_session(cx, network(&scrambled), |s| {
        assert!(sort(s));
        let got: Vec<i64> = s.members(900).iter().map(|m| m.0).collect();
        let mut counts = got.clone();
        counts.sort();
        assert_eq!(counts, vec![1, 1, 2, 2, 3, 3], "every entry is kept");
        for pair in got.windows(2) {
            let a = s.way_nodes(pair[0]).unwrap();
            let b = s.way_nodes(pair[1]).unwrap();
            assert!(a.iter().any(|n| b.contains(n)), "{pair:?} are connected");
        }
    });
}

#[gpui::test]
fn ways_that_are_not_downloaded_keep_their_slot(cx: &mut TestAppContext) {
    expect_sorted(
        cx,
        &[wm(3, ""), wm(9002, ""), wm(1, ""), wm(2, "")],
        &[1, 9002, 2, 3],
    );
}
