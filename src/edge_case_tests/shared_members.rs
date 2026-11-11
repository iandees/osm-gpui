//! Nodes and ways that are members of several relations at once (turn
//! restriction via, route stop, multipolygon label, admin centre) through
//! Merge Nodes, Delete and Disconnect; copy / paste of members whose
//! relations or sibling members were not copied; and tag values and keys at
//! the server's 255-character limit through Reverse, Merge Nodes and Join.

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

fn restriction() -> [(&'static str, &'static str); 2] {
    [("type", "restriction"), ("restriction", "no_left_turn")]
}

fn route() -> [(&'static str, &'static str); 2] {
    [("type", "route"), ("route", "bus")]
}

fn long(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

fn typed(list: &[(MemberType, i64, &str)]) -> Vec<(MemberType, i64, String)> {
    list.iter()
        .map(|&(t, i, r)| (t, i, r.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Merge nodes that sit in several relations

/// Nodes 2 and 3 are the ends that get merged. Between them they are in a
/// via-restriction each, a bus route (different stop roles, far apart), a
/// route with the same role twice (far apart and adjacent), a role pair on
/// neighbours, a multipolygon label, and a relation naming node 3 under two
/// roles.
fn merge_network() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 11, 12, 13, 14])
        .node(2, &[("public_transport", "stop_position")])
        .node(3, &[("bus", "yes")])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .way(20, &[11, 12, 13, 14, 11], &[])
        .relation(
            900,
            &restriction(),
            &[wm(10, "from"), nm(2, "via"), wm(11, "to")],
        )
        .relation(
            901,
            &route(),
            &[
                nm(2, "stop_entry_only"),
                wm(10, ""),
                wm(11, ""),
                nm(3, "stop_exit_only"),
            ],
        )
        .relation(902, &route(), &[nm(2, "stop"), wm(10, ""), nm(3, "stop")])
        .relation(
            903,
            &route(),
            &[wm(10, ""), nm(2, "stop"), nm(3, "stop"), wm(11, "")],
        )
        .relation(904, &route(), &[nm(2, "stop"), nm(3, "platform")])
        .relation(
            905,
            &restriction(),
            &[wm(11, "from"), nm(3, "via"), wm(10, "to")],
        )
        .relation(
            906,
            &[("type", "multipolygon"), ("landuse", "forest")],
            &[wm(20, "outer"), nm(3, "label")],
        )
        .relation(907, &route(), &[nm(3, "a"), wm(10, ""), nm(3, "b")])
        .done()
}

fn check_merged_into(s: &mut Session, keep: i64) {
    let k = keep;
    assert_eq!(
        s.typed_members(900),
        typed(&[
            (MemberType::Way, 10, "from"),
            (MemberType::Node, k, "via"),
            (MemberType::Way, 11, "to")
        ])
    );
    assert_eq!(
        s.typed_members(905),
        typed(&[
            (MemberType::Way, 11, "from"),
            (MemberType::Node, k, "via"),
            (MemberType::Way, 10, "to")
        ])
    );
    // Different roles at different positions: both survive in place.
    assert_eq!(
        s.typed_members(901),
        typed(&[
            (MemberType::Node, k, "stop_entry_only"),
            (MemberType::Way, 10, ""),
            (MemberType::Way, 11, ""),
            (MemberType::Node, k, "stop_exit_only"),
        ])
    );
    // The same role twice but not next to each other: both kept.
    assert_eq!(
        s.typed_members(902),
        typed(&[
            (MemberType::Node, k, "stop"),
            (MemberType::Way, 10, ""),
            (MemberType::Node, k, "stop")
        ])
    );
    // The same role next to each other: the duplicate collapses.
    assert_eq!(
        s.typed_members(903),
        typed(&[
            (MemberType::Way, 10, ""),
            (MemberType::Node, k, "stop"),
            (MemberType::Way, 11, "")
        ])
    );
    // Different roles next to each other: nothing collapses.
    assert_eq!(
        s.typed_members(904),
        typed(&[
            (MemberType::Node, k, "stop"),
            (MemberType::Node, k, "platform")
        ])
    );
    assert_eq!(
        s.typed_members(906),
        typed(&[
            (MemberType::Way, 20, "outer"),
            (MemberType::Node, k, "label")
        ])
    );
    assert_eq!(
        s.typed_members(907),
        typed(&[
            (MemberType::Node, k, "a"),
            (MemberType::Way, 10, ""),
            (MemberType::Node, k, "b")
        ])
    );
}

#[gpui::test]
fn merging_nodes_in_many_relations_moves_every_membership_to_the_survivor(cx: &mut TestAppContext) {
    with_session(cx, merge_network(), |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"));
        assert!(s.has_node(2) && !s.has_node(3));
        check_merged_into(s, 2);
        // Tags of both nodes are on the survivor.
        assert_eq!(
            s.tags(N, 2),
            pairs(&[("bus", "yes"), ("public_transport", "stop_position")])
        );
        s.assert_no_dangling();
        assert_eq!(s.steps(), 1);
        let d = s.diff();
        // Restriction 900 already named node 2, so it is the one unchanged.
        assert_eq!(d.modified_relations.len(), 7);
        assert_eq!(d.deleted_node_ids, vec![(3, 3)]);
        // The relations stop naming node 3 before node 3 is deleted.
        let xml = s.xml();
        let del = at(&xml, "<delete>");
        assert!(at(&xml, "<modify>") < del);
        assert!(
            !xml[..del].contains("type=\"node\" ref=\"3\""),
            "a relation in <modify> still names the deleted node: {xml}"
        );
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merging_in_the_other_order_keeps_the_same_memberships_on_the_other_node(
    cx: &mut TestAppContext,
) {
    with_session(cx, merge_network(), |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 2)]);
        assert!(s.act("merge"));
        assert!(s.has_node(3) && !s.has_node(2));
        check_merged_into(s, 3);
        s.assert_no_dangling();
        assert_eq!(s.diff().deleted_node_ids, vec![(2, 3)]);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merging_into_a_survivor_that_is_in_no_relation_still_carries_the_other_nodes_memberships(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .relation(
            900,
            &restriction(),
            &[wm(10, "from"), nm(3, "via"), wm(11, "to")],
        )
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"));
        assert_eq!(
            s.typed_members(900),
            typed(&[
                (MemberType::Way, 10, "from"),
                (MemberType::Node, 2, "via"),
                (MemberType::Way, 11, "to")
            ])
        );
        assert_eq!(s.diff().modified_relations.len(), 1);
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 2. Delete a node that is a member of several relations

/// Ways 10 `[1,2,3]` and 11 `[3,4,5]` meet at node 3, which is also a bus
/// stop (route 901) and the admin centre of boundary 902.
fn stop_network(with_restriction: bool) -> OsmData {
    let mut b = Build::new()
        .nodes(&[1, 2, 3, 4, 5, 7, 8, 9, 10])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .way(30, &[7, 8, 9, 10, 7], &[])
        .relation(901, &route(), &[wm(10, ""), nm(3, "stop"), wm(11, "")])
        .relation(
            902,
            &[("type", "boundary"), ("boundary", "administrative")],
            &[wm(30, "outer"), nm(3, "admin_centre")],
        );
    if with_restriction {
        b = b.relation(
            900,
            &restriction(),
            &[wm(10, "from"), nm(3, "via"), wm(11, "to")],
        );
    }
    b.done()
}

#[gpui::test]
fn deleting_a_node_that_is_a_stop_and_an_admin_centre_cleans_up_both_relations(
    cx: &mut TestAppContext,
) {
    with_session(cx, stop_network(false), |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("delete"));
        assert!(!s.has_node(3));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2]));
        assert_eq!(s.way_nodes(11), Some(vec![4, 5]));
        assert_eq!(s.members(901), mem(&[(10, ""), (11, "")]));
        assert_eq!(s.members(902), mem(&[(30, "outer")]));
        s.assert_no_dangling();
        assert_eq!(s.steps(), 1);
        let d = s.diff();
        assert_eq!(d.modified_relations.len(), 2);
        assert_eq!(d.modified_ways.len(), 2);
        assert_eq!(d.deleted_node_ids, vec![(3, 3)]);
        let xml = s.xml();
        assert!(at(&xml, "<modify>") < at(&xml, "<delete>"));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn deleting_a_restriction_via_node_is_refused_and_leaves_the_other_relations_alone(
    cx: &mut TestAppContext,
) {
    with_session(cx, stop_network(true), |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(!s.act("delete"));
        assert_eq!(s.snap(), before, "nothing half-done");
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(s.steps(), 0);
        assert!(!s.status().is_empty());
    });
}

#[gpui::test]
fn deleting_the_only_member_of_one_relation_is_refused_even_if_the_others_would_be_fine(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[3, 4, 5], &road())
        .relation(901, &route(), &[wm(10, ""), nm(3, "stop"), wm(11, "")])
        .relation(
            902,
            &[("type", "boundary"), ("boundary", "administrative")],
            &[nm(3, "admin_centre")],
        )
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(!s.act("delete"));
        assert_eq!(s.snap(), before, "route 901 must not lose its stop alone");
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn deleting_a_vertex_whose_short_way_is_a_restriction_member_changes_nothing(
    cx: &mut TestAppContext,
) {
    // Node 5 shortens way 10 (fine) and would delete the two-node way 11,
    // which is the `to` of a restriction. The refusal must come before way 10
    // is shortened.
    let data = Build::new()
        .nodes(&[4, 5, 6, 7, 8, 9])
        .way(10, &[4, 5, 6], &road())
        .way(11, &[5, 7], &road())
        .way(12, &[8, 9], &road())
        .relation(
            900,
            &restriction(),
            &[wm(12, "from"), nm(8, "via"), wm(11, "to")],
        )
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 5)]);
        assert!(!s.act("delete"));
        assert_eq!(s.snap(), before);
        assert_eq!(s.way_nodes(10), Some(vec![4, 5, 6]));
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn deleting_a_refused_and_an_allowed_node_together_deletes_only_the_allowed_one_in_one_step(
    cx: &mut TestAppContext,
) {
    let mut data = stop_network(true);
    // A loose stop that only route 903 knows.
    data.nodes.insert(
        20,
        OsmNode {
            id: 20,
            lat: 40.01,
            lon: -74.01,
            version: 3,
            tags: HashMap::new(),
        },
    );
    data.relations.push(OsmRelation {
        id: 903,
        version: 2,
        tags: [("type", "route"), ("route", "bus")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        members: vec![
            OsmMember {
                member_type: MemberType::Way,
                reference: 10,
                role: String::new(),
            },
            OsmMember {
                member_type: MemberType::Node,
                reference: 20,
                role: "stop".into(),
            },
        ],
    });
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 3), (N, 20)]);
        assert!(s.act("delete"));
        assert!(s.has_node(3), "the via node was refused");
        assert!(!s.has_node(20));
        assert_eq!(s.members(903), mem(&[(10, "")]));
        // The refused node's relations are untouched.
        assert_eq!(s.members(901), mem(&[(10, ""), (3, "stop"), (11, "")]));
        assert_eq!(s.members(902), mem(&[(30, "outer"), (3, "admin_centre")]));
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.steps(), 1);
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 3. Disconnect a node that is a relation member

#[gpui::test]
fn disconnecting_a_stop_node_leaves_every_membership_on_the_original_node(cx: &mut TestAppContext) {
    let mut data = stop_network(false);
    data.relations.push(OsmRelation {
        id: 906,
        version: 2,
        tags: [("type", "multipolygon")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        members: vec![
            OsmMember {
                member_type: MemberType::Way,
                reference: 30,
                role: "outer".into(),
            },
            OsmMember {
                member_type: MemberType::Node,
                reference: 3,
                role: "label".into(),
            },
        ],
    });
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("separate"));
        let copy = s.way_nodes(11).unwrap()[0];
        assert!(copy < 0);
        // The original keeps its place in every relation; the copy is in none.
        assert_eq!(s.members(901), mem(&[(10, ""), (3, "stop"), (11, "")]));
        assert_eq!(s.members(902), mem(&[(30, "outer"), (3, "admin_centre")]));
        assert_eq!(s.members(906), mem(&[(30, "outer"), (3, "label")]));
        assert!(s.diff().modified_relations.is_empty());
        assert_eq!(s.counts(), (1, 1, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn the_membership_stays_with_the_original_node_even_when_its_own_way_is_the_one_detached(
    cx: &mut TestAppContext,
) {
    // Selecting way 10 with the node detaches way 10 onto the new node, so
    // the original node (and its stop role) now belongs to way 11 only.
    with_session(cx, stop_network(false), |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 10)]);
        assert!(s.act("separate"));
        let nodes = s.way_nodes(10).unwrap();
        assert!(*nodes.last().unwrap() < 0, "way 10 got the copy: {nodes:?}");
        assert_eq!(s.way_nodes(11), Some(vec![3, 4, 5]));
        assert_eq!(s.members(901), mem(&[(10, ""), (3, "stop"), (11, "")]));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn disconnecting_a_restriction_via_node_must_not_silently_break_the_restriction(
    cx: &mut TestAppContext,
) {
    with_session(cx, stop_network(true), |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        let done = s.act("separate");
        if !done {
            assert_eq!(s.snap(), before);
            return;
        }
        // If it was allowed, `from` and `to` must both still touch `via`.
        let via = s
            .typed_members(900)
            .into_iter()
            .find(|m| m.2 == "via")
            .unwrap()
            .1;
        for way in [10, 11] {
            assert!(
                s.way_nodes(way).unwrap().contains(&via),
                "way {way} no longer touches the via node {via}"
            );
        }
    });
}

#[gpui::test]
fn disconnecting_an_unrelated_way_from_a_restriction_via_node_is_allowed(cx: &mut TestAppContext) {
    let mut data = stop_network(true);
    data.nodes.insert(
        6,
        OsmNode {
            id: 6,
            lat: 40.02,
            lon: -74.02,
            version: 3,
            tags: HashMap::new(),
        },
    );
    data.ways.insert(
        12,
        OsmWay {
            id: 12,
            nodes: vec![3, 6],
            version: 3,
            tags: HashMap::new(),
        },
    );
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 3), (W, 12)]);
        assert!(s.act("separate"));
        assert!(s.way_nodes(12).unwrap()[0] < 0);
        // The restriction's ways still touch the via node.
        assert!(s.way_nodes(10).unwrap().contains(&3));
        assert!(s.way_nodes(11).unwrap().contains(&3));
        assert_eq!(s.members(900), mem(&[(10, "from"), (3, "via"), (11, "to")]));
        undo_redo(s, &before);
    });
}

// ---------------------------------------------------------------------------
// 4. Copy / paste of members whose relations or siblings were not copied

fn relation_ids(s: &Session) -> Vec<i64> {
    let mut ids: Vec<i64> = s.snap().relations.keys().copied().collect();
    ids.sort();
    ids
}

#[gpui::test]
fn a_pasted_way_is_not_a_member_of_the_relations_the_original_was_in(cx: &mut TestAppContext) {
    with_session(cx, stop_network(true), |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        s.act("copy_features");
        assert!(s.act("paste_features_source"));
        let ways = s.new_ways();
        assert_eq!(ways.len(), 1);
        let pasted = ways[0];
        // No relation names the copy or any of its new nodes, and no
        // existing relation changed.
        let after = s.snap();
        assert_eq!(after.relations, before.relations);
        for (_, members) in after.relations.values() {
            assert!(members.iter().all(|(_, id, _)| *id >= 0 || *id > 9000));
        }
        assert_eq!(s.way_nodes(10), Some(vec![1, 2, 3]));
        assert_eq!(s.tags(W, pasted), pairs(&[("highway", "residential")]));
        // Three new nodes and the way; no relation is created or modified.
        assert_eq!(s.counts(), (4, 0, 0));
        s.assert_no_dangling();
        assert_eq!(s.steps(), 1);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn duplicating_a_stop_node_does_not_copy_its_memberships(cx: &mut TestAppContext) {
    with_session(cx, stop_network(true), |s| {
        let before = s.snap();
        s.select(&[(N, 3)]);
        assert!(s.act("duplicate_features"));
        assert_eq!(s.snap().relations, before.relations);
        assert_eq!(s.counts(), (1, 0, 0));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn a_pasted_relation_keeps_pointing_at_originals_that_were_not_copied(cx: &mut TestAppContext) {
    with_session(cx, stop_network(true), |s| {
        let before = s.snap();
        // Route 901 together with way 10 only: way 11 stays behind.
        s.select(&[(W, 10), (R, 901)]);
        s.act("copy_features");
        assert!(s.act("paste_features_source"));
        let new_way = s.new_ways()[0];
        // Way 10 brings its node 3 along, so the stop is copied too.
        let copied_stop = s.way_nodes(new_way).unwrap()[2];
        assert!(copied_stop < 0);
        let created: Vec<i64> = relation_ids(s).into_iter().filter(|&r| r < 0).collect();
        assert_eq!(created.len(), 1);
        // The copy of the route names the copies of way 10 and its node 3 and
        // the original of the way that was not copied: valid references.
        assert_eq!(
            s.members(created[0]),
            mem(&[(new_way, ""), (copied_stop, "stop"), (11, "")])
        );
        // The original route is unchanged.
        assert_eq!(s.members(901), mem(&[(10, ""), (3, "stop"), (11, "")]));
        s.assert_no_dangling();
        assert_eq!(s.steps(), 1);
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn pasting_a_relation_into_another_layer_drops_unsaved_members_that_layer_cannot_have(
    cx: &mut TestAppContext,
) {
    // Way -5 is only in this layer; way 11 is a server object.
    let data = Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .way(-5, &[3, 4], &road())
        .relation(
            901,
            &route(),
            &[wm(10, "a"), wm(11, "b"), wm(-5, "c"), nm(5, "stop")],
        )
        .done();
    with_session(cx, data, move |s| {
        let first = s.layer;
        s.select(&[(W, 10), (R, 901)]);
        s.act("copy_features");
        let second = s.view.layer_manager.alloc_id();
        s.view
            .layer_manager
            .add_layer(Box::new(OsmLayer::new_with_data(
                second,
                "Second".to_string(),
                Arc::new(OsmData {
                    nodes: HashMap::new(),
                    ways: HashMap::new(),
                    relations: Vec::new(),
                    bounds: Vec::new(),
                }),
            )));
        s.view.active_layer = Some(second);
        s.view.run_selection_action("paste_features_source", s.cx);
        let editable = s
            .view
            .layer_manager
            .find_layer(second)
            .unwrap()
            .as_editable()
            .unwrap();
        let relations = editable.relations();
        assert_eq!(relations.len(), 1);
        let members: Vec<(i64, String)> = relations[0]
            .members
            .iter()
            .map(|m| (m.reference, m.role.clone()))
            .collect();
        // Way 10 is the copy; positive ids stay (server objects, possibly
        // not loaded); the unsaved way -5 of the other layer is dropped.
        assert_eq!(members.len(), 3, "{members:?}");
        assert!(members[0].0 < 0 && members[0].1 == "a");
        assert_eq!(members[1], (11, "b".to_string()));
        assert_eq!(members[2], (5, "stop".to_string()));
        assert!(members.iter().all(|(id, _)| *id != -5));
        // The source layer is untouched.
        s.view.active_layer = Some(first);
        assert_eq!(s.members(901).len(), 4);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

// ---------------------------------------------------------------------------
// 5. Tag length limits (255 characters per key and per value)

fn tag_len_ok(tags: &[(String, String)]) -> bool {
    tags.iter()
        .all(|(k, v)| k.chars().count() <= 255 && v.chars().count() <= 255)
}

#[gpui::test]
fn reverse_swaps_a_pair_of_opposite_keys_without_losing_either_or_truncating(
    cx: &mut TestAppContext,
) {
    let a = long('a', 255);
    let b = long('b', 255);
    let data = Build::new()
        .nodes(&[1, 2])
        .way(
            10,
            &[1, 2],
            &[
                ("highway", "residential"),
                ("lanes:forward", &a),
                ("lanes:backward", &b),
                ("cycleway:left", "lane"),
                ("cycleway:right", "track"),
            ],
        )
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        let tags = s.tags(W, 10);
        assert_eq!(tags.len(), 5, "no key lost to a collision: {tags:?}");
        let get = |k: &str| {
            tags.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("lanes:forward"), Some(b));
        assert_eq!(get("lanes:backward"), Some(a));
        assert_eq!(get("cycleway:left").as_deref(), Some("track"));
        assert_eq!(get("cycleway:right").as_deref(), Some("lane"));
        assert!(tag_len_ok(&tags));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn reverse_must_not_grow_a_key_past_255_characters(cx: &mut TestAppContext) {
    let key = format!("{}:forward", long('k', 247));
    assert_eq!(key.chars().count(), 255);
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &[("highway", "residential"), (&key, "yes")])
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        if s.act("reverse") {
            assert!(
                tag_len_ok(&s.tags(W, 10)),
                "reverse produced an over-long key"
            );
        } else {
            assert_eq!(s.snap(), before);
        }
    });
}

#[gpui::test]
fn reverse_must_not_grow_a_value_past_255_characters(cx: &mut TestAppContext) {
    let value = format!("{}%", long('9', 254));
    assert_eq!(value.chars().count(), 255);
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &[("highway", "path"), ("incline", &value)])
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        if s.act("reverse") {
            assert!(
                tag_len_ok(&s.tags(W, 10)),
                "reverse produced an over-long value"
            );
        } else {
            assert_eq!(s.snap(), before);
        }
    });
}

#[gpui::test]
fn merge_nodes_unions_tags_with_255_character_keys_and_values_untouched(cx: &mut TestAppContext) {
    let key = long('k', 255);
    let v1 = long('x', 255);
    let v2 = long('y', 255);
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .node(2, &[("note", &v1), (&key, "yes")])
        .node(3, &[("note", &v1), ("description", &v2)])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(s.act("merge"));
        let tags = s.tags(N, 2);
        // The equal long value appears once, not concatenated or doubled.
        assert_eq!(tags, {
            let mut t = vec![
                ("description".to_string(), v2.clone()),
                (key.clone(), "yes".to_string()),
                ("note".to_string(), v1.clone()),
            ];
            t.sort();
            t
        });
        assert!(tag_len_ok(&tags));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn merge_nodes_with_different_255_character_values_for_one_key_is_refused(cx: &mut TestAppContext) {
    let v1 = long('x', 255);
    let v2 = long('y', 255);
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .node(2, &[("note", &v1)])
        .node(3, &[("note", &v2)])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(N, 2), (N, 3)]);
        assert!(!s.act("merge"));
        assert_eq!(s.snap(), before, "no value is concatenated or truncated");
        assert!(s.status().contains("note"), "{}", s.status());
        assert_eq!(s.steps(), 0);
    });
}

#[gpui::test]
fn join_unions_tags_with_255_character_values_untouched(cx: &mut TestAppContext) {
    let v1 = long('x', 255);
    let v2 = long('y', 255);
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &[("highway", "residential"), ("note", &v1)])
        .way(
            11,
            &[2, 3],
            &[("highway", "residential"), ("note", &v1), ("fixme", &v2)],
        )
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("join"));
        let tags = s.tags(W, 10);
        assert_eq!(tags.len(), 3, "{tags:?}");
        assert!(tags.contains(&("note".to_string(), v1.clone())));
        assert!(tags.contains(&("fixme".to_string(), v2.clone())));
        assert!(tag_len_ok(&tags));
        undo_redo(s, &before);
    });
}

#[gpui::test]
fn join_with_different_255_character_values_for_one_key_is_refused(cx: &mut TestAppContext) {
    let v1 = long('x', 255);
    let v2 = long('y', 255);
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &[("highway", "residential"), ("note", &v1)])
        .way(11, &[2, 3], &[("highway", "residential"), ("note", &v2)])
        .done();
    with_session(cx, data, move |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("join"));
        assert_eq!(s.snap(), before);
        assert!(s.status().contains("note"), "{}", s.status());
        assert_eq!(s.steps(), 0);
    });
}
