//! Further situations (batch E): editing the rings of multipolygons. Merge
//! between rings, Reverse, rings that shrink below three distinct points, and
//! Split / Join of a ring that two multipolygons share.

use super::*;

fn undo_redo(s: &mut Session, before: &Snap) {
    let after = s.snap();
    s.undo();
    assert_eq!(&s.snap(), before, "one undo restores the prior state");
    assert_eq!(s.counts(), (0, 0, 0));
    s.redo();
    assert_eq!(s.snap(), after, "redo reapplies the same result");
}

const MP: [(&str, &str); 2] = [("type", "multipolygon"), ("natural", "water")];
const MP_PLAIN: [(&str, &str); 1] = [("type", "multipolygon")];

/// Outer square 10 = [1,2,3,4,1] and inner triangle 11 = [5,6,7,5] of
/// multipolygon 900.
fn lake() -> Build {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7])
        .way(10, &[1, 2, 3, 4, 1], &[])
        .way(11, &[5, 6, 7, 5], &[])
        .relation(900, &MP, &[wm(10, "outer"), wm(11, "inner")])
}

// ---------------------------------------------------------------------------
// 1. Merge nodes between rings

#[gpui::test]
fn merging_a_node_of_the_outer_ring_with_one_of_the_inner_ring_touches_the_rings(
    cx: &mut TestAppContext,
) {
    // An inner ring touching the outer ring at one point is legal in OSM.
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 5)]);
        assert!(s.act("merge"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4, 1]));
        assert_eq!(s.way_nodes(11), Some(vec![3, 6, 7, 3]));
        assert!(!s.has_node(5));
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        s.assert_no_dangling();
        // Node 5 deleted, way 11 modified, way 10 and the relation untouched.
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merging_the_inner_node_first_keeps_the_inner_ring_node(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        s.select(&[(N, 5), (N, 3)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 5, 4, 1]));
        assert_eq!(s.way_nodes(11), Some(vec![5, 6, 7, 5]));
        assert!(!s.has_node(3));
        s.assert_no_dangling();
    });
}

#[gpui::test]
fn merging_nodes_of_two_different_multipolygons_joins_their_rings(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8])
        .way(10, &[1, 2, 3, 4, 1], &[])
        .way(20, &[5, 6, 7, 8, 5], &[])
        .relation(900, &MP, &[wm(10, "outer")])
        .relation(
            901,
            &[("type", "multipolygon"), ("landuse", "forest")],
            &[wm(20, "outer")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 6)]);
        assert!(s.act("merge"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3, 4, 1]));
        assert_eq!(s.way_nodes(20), Some(vec![5, 2, 7, 8, 5]));
        // Neither relation changed.
        assert_eq!(s.members(900), mem(&[(10, "outer")]));
        assert_eq!(s.members(901), mem(&[(20, "outer")]));
        assert_eq!(
            s.tags(R, 900),
            pairs(&[("natural", "water"), ("type", "multipolygon")])
        );
        s.assert_no_dangling();
        assert_eq!(s.counts(), (0, 1, 1));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 2. Reverse a ring

#[gpui::test]
fn reversing_the_outer_ring_keeps_its_role_and_leaves_the_relation_alone(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 3, 2, 1]));
        assert_eq!(s.way_nodes(11), Some(vec![5, 6, 7, 5]));
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        // Only the way is modified; the relation is not uploaded.
        assert_eq!(s.counts(), (0, 1, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_the_inner_ring_keeps_its_role(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(11), Some(vec![5, 7, 6, 5]));
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        assert_eq!(s.counts(), (0, 1, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_both_rings_at_once_is_one_undo_step(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 3, 2, 1]));
        assert_eq!(s.way_nodes(11), Some(vec![5, 7, 6, 5]));
        assert_eq!(s.steps(), 1);
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_a_ring_twice_returns_to_the_original_and_uploads_nothing(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert!(s.act("reverse"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn reversing_a_ring_flips_direction_tags_but_not_the_relation(cx: &mut TestAppContext) {
    // A fence ring around a lake: the way's `fence_type:left` is swapped, the
    // node's `direction=forward` flips; multipolygon tags and roles stay.
    let data = Build::new()
        .nodes(&[1, 3, 4])
        .node(2, &[("direction", "forward")])
        .way(
            10,
            &[1, 2, 3, 4, 1],
            &[("barrier", "fence"), ("fence_type:left", "wood")],
        )
        .relation(900, &MP, &[wm(10, "outer")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![1, 4, 3, 2, 1]));
        assert_eq!(
            s.tags(W, 10),
            pairs(&[("barrier", "fence"), ("fence_type:right", "wood")])
        );
        assert_eq!(s.tags(N, 2), pairs(&[("direction", "backward")]));
        assert_eq!(
            s.tags(R, 900),
            pairs(&[("natural", "water"), ("type", "multipolygon")])
        );
        assert_eq!(s.members(900), mem(&[(10, "outer")]));
        // Way and node modified, relation not.
        assert_eq!(s.counts(), (0, 2, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reversing_a_ring_shared_by_two_multipolygons_leaves_both_alone(cx: &mut TestAppContext) {
    let data = shared_ring();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.members(900), mem(&[(10, "outer")]));
        assert_eq!(s.members(901), mem(&[(12, "outer"), (10, "inner")]));
        assert_eq!(s.counts(), (0, 1, 0));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 3. Rings that shrink below three distinct points

#[gpui::test]
fn merging_two_nodes_of_a_triangle_ring_is_refused(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 5), (N, 6)]);
        assert!(!s.act("merge"));
        assert!(!s.status().is_empty());
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn merging_the_closing_node_with_its_neighbour_in_a_triangle_is_refused(cx: &mut TestAppContext) {
    // 5 is both first and last entry of [5,6,7,5]; merging with 7 would leave
    // [7,6,7].
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 5), (N, 7)]);
        assert!(!s.act("merge"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn merging_two_nodes_of_a_square_ring_leaves_a_valid_triangle(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 4, 1]));
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merging_across_the_rings_cannot_shrink_either_ring(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        s.select(&[(N, 1), (N, 6)]);
        assert!(s.act("merge"));
        assert_eq!(s.way_nodes(10).unwrap().len(), 5);
        assert_eq!(s.way_nodes(11).unwrap().len(), 4);
    });
}

#[gpui::test]
fn deleting_a_node_of_a_triangle_ring_deletes_the_ring_and_drops_it_from_the_relation(
    cx: &mut TestAppContext,
) {
    // Triangle -> [5,7,5] has two distinct points, so the way goes, and the
    // relation keeps its other member.
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 6)]);
        assert!(s.act("delete"), "status: {}", s.status());
        assert!(!s.has_way(11), "degenerate ring must not survive");
        assert!(s.has_way(10));
        assert_eq!(s.members(900), mem(&[(10, "outer")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_node_that_would_remove_the_only_ring_of_a_multipolygon_is_refused(
    cx: &mut TestAppContext,
) {
    // Design: the triangle would become degenerate, so its way would go, and
    // that is the relation's only member (X3): refused, nothing changes.
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3, 1], &[])
        .relation(900, &MP, &[wm(10, "outer")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 2)]);
        assert!(!s.act("delete"));
        assert!(
            s.status().contains("only member of relation 900"),
            "{}",
            s.status()
        );
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn deleting_a_node_of_a_square_ring_keeps_it_a_member_with_its_role(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("delete"), "status: {}", s.status());
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 4, 1]));
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_the_closing_node_of_a_ring_keeps_it_closed(cx: &mut TestAppContext) {
    let data = lake().done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(N, 1)]);
        assert!(s.act("delete"), "status: {}", s.status());
        let nodes = s.way_nodes(10).unwrap();
        assert_eq!(nodes.first(), nodes.last(), "ring stays closed: {nodes:?}");
        assert_eq!(nodes.len(), 4);
        assert_eq!(s.members(900), mem(&[(10, "outer"), (11, "inner")]));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 4. Split and join of a ring shared by two multipolygons

/// Ring 10 = [1,2,3,4,1] is the outer of lake 900 and the inner of island
/// 901 (whose outer is 12).
fn shared_ring() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8])
        .way(10, &[1, 2, 3, 4, 1], &[])
        .way(12, &[5, 6, 7, 8, 5], &[])
        .relation(900, &MP, &[wm(10, "outer")])
        .relation(901, &MP_PLAIN, &[wm(12, "outer"), wm(10, "inner")])
        .done()
}

#[gpui::test]
fn splitting_a_ring_shared_by_two_multipolygons_gives_each_its_own_role_on_both_halves(
    cx: &mut TestAppContext,
) {
    with_session(cx, shared_ring(), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(s.act("split"), "status: {}", s.status());
        let new = s.new_ways()[0];
        let mut halves = vec![s.way_nodes(10).unwrap(), s.way_nodes(new).unwrap()];
        halves.sort();
        assert_eq!(halves, vec![vec![2, 3, 4], vec![4, 1, 2]]);
        let in_900 = s.members(900);
        assert_eq!(in_900.len(), 2);
        assert!(in_900.iter().all(|m| m.1 == "outer"), "{in_900:?}");
        assert!(in_900.iter().any(|m| m.0 == new) && in_900.iter().any(|m| m.0 == 10));
        let in_901 = s.members(901);
        assert_eq!(in_901.len(), 3);
        assert_eq!(in_901[0], (12, "outer".to_string()));
        assert!(in_901[1..].iter().all(|m| m.1 == "inner"), "{in_901:?}");
        assert!(in_901.iter().any(|m| m.0 == new) && in_901.iter().any(|m| m.0 == 10));
        s.assert_no_dangling();
        // New way created; way 10 and both relations modified.
        assert_eq!(s.counts(), (1, 3, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn split_ring_upload_creates_the_new_way_before_both_relations_that_name_it(
    cx: &mut TestAppContext,
) {
    with_session(cx, shared_ring(), |s| {
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let modify = at(&xml, "<modify>");
        assert!(create < modify, "{xml}");
        assert!(
            xml[create..modify].contains(&format!("<way id=\"{new}\"")),
            "{xml}"
        );
        let modified = &xml[modify..];
        for role in ["outer", "inner"] {
            let member = format!("<member type=\"way\" ref=\"{new}\" role=\"{role}\"");
            assert!(modified.contains(&member), "{member} missing: {xml}");
        }
    });
}

#[gpui::test]
fn joining_the_halves_of_a_shared_ring_restores_both_multipolygons(cx: &mut TestAppContext) {
    with_session(cx, shared_ring(), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 4), (W, 10)]);
        assert!(s.act("split"));
        let new = s.new_ways()[0];
        s.select(&[(W, 10), (W, new)]);
        assert!(s.act("join"), "status: {}", s.status());
        assert_eq!(s.steps(), 2);
        let m900 = s.members(900);
        assert_eq!(m900.len(), 1, "{m900:?}");
        assert_eq!(m900[0].1, "outer");
        let m901 = s.members(901);
        assert_eq!(m901.len(), 2, "{m901:?}");
        assert_eq!(m901[0], (12, "outer".to_string()));
        assert_eq!(m901[1].1, "inner");
        s.assert_no_dangling();
        // Two undos walk back to the start.
        s.undo();
        s.undo();
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

/// Two lakes side by side share the open way 11 as their common shore:
/// lake 900 = 10 + 11, lake 901 = 11 + 12.
fn shared_shore() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2, 3], &[])
        .way(11, &[3, 4, 1], &[])
        .way(12, &[1, 5, 6, 3], &[])
        .relation(900, &MP, &[wm(10, "outer"), wm(11, "outer")])
        .relation(901, &MP_PLAIN, &[wm(11, "outer"), wm(12, "outer")])
        .done()
}

#[gpui::test]
fn splitting_a_shared_shore_way_updates_both_multipolygons(cx: &mut TestAppContext) {
    with_session(cx, shared_shore(), |s| {
        let before = s.snap();
        s.select(&[(N, 4), (W, 11)]);
        assert!(s.act("split"), "status: {}", s.status());
        let new = s.new_ways()[0];
        assert_eq!(s.way_nodes(11), Some(vec![3, 4]));
        assert_eq!(s.way_nodes(new), Some(vec![4, 1]));
        assert_eq!(
            s.members(900),
            mem(&[(10, "outer"), (11, "outer"), (new, "outer")])
        );
        assert_eq!(
            s.members(901),
            mem(&[(11, "outer"), (new, "outer"), (12, "outer")])
        );
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn joining_a_shore_with_a_way_only_one_multipolygon_has_is_refused(cx: &mut TestAppContext) {
    // 10 is only in lake 900, 11 in both: joining would take 11 out of lake
    // 901 silently, so it must be refused.
    with_session(cx, shared_shore(), |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(
            !s.act("join"),
            "lake 901 now lists the whole ring {:?} in place of its shore: {:?}",
            s.way_nodes(10),
            s.members(901)
        );
        assert_eq!(s.snap(), before);
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn joining_two_shores_that_both_multipolygons_list_merges_each_relation(cx: &mut TestAppContext) {
    // Both ways are in both relations: the join closes a ring and each
    // relation ends with one member.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3], &[])
        .way(11, &[3, 4, 1], &[])
        .relation(900, &MP, &[wm(10, "outer"), wm(11, "outer")])
        .relation(901, &MP_PLAIN, &[wm(10, "outer"), wm(11, "outer")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"), "status: {}", s.status());
        assert_eq!(s.members(900).len(), 1);
        assert_eq!(s.members(901).len(), 1);
        s.assert_no_dangling();
        undo_redo(s, &before);
    });
}
