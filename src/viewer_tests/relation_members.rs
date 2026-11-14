//! Editing relation membership from the side panel and from selection actions.

use super::{
    pairs, pending, redo_step, relation_members, route_over, select, setup_network,
    setup_network_with, tags_of, undo_step,
};
use crate::{side_panel, MemberRef, TagEditContext};
use gpui::TestAppContext;
use osm_gpui::coordinates::LatLon;

#[gpui::test]
fn side_panel_lists_the_relations_of_the_one_selected_feature(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network_with(cx, |data| {
        data.relations[0].tags.insert("ref".into(), "7B".into());
    });
    window
        .update(cx, |view, _window, _cx| {
            // Way 102 and node 7 are in relation 900, which has no name,
            // so its ref stands in.
            select(view, Way, &[102]);
            let membership = |role: &str, index| side_panel::Membership {
                relation_id: 900,
                name: "7B".to_string(),
                role: role.to_string(),
                index,
            };
            assert_eq!(view.selected_memberships(), vec![membership("", 0)]);
            select(view, Node, &[7]);
            assert_eq!(view.selected_memberships(), vec![membership("stop", 1)]);
            // In no relation, several features, or nothing selected: no
            // section.
            select(view, Way, &[100]);
            assert!(view.selected_memberships().is_empty());
            select(view, Way, &[102, 100]);
            assert!(view.selected_memberships().is_empty());
            select(view, Way, &[]);
            assert!(view.selected_memberships().is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn a_relation_selected_from_a_members_row_edits_and_zooms(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Relation, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[102]);
            let membership = view.selected_memberships().remove(0);
            // What clicking the member's Relations row does.
            view.select_relation(layer_id, membership.relation_id);
            let only = view.selected.as_slice();
            assert_eq!(only.len(), 1);
            assert_eq!((only[0].kind, only[0].id), (Relation, 900));
            // A relation lists no memberships of its own.
            assert!(view.selected_memberships().is_empty());

            // Tags edit like a way's: one undo step each.
            let ctx = TagEditContext {
                features: view.selected.clone(),
                original_key: String::new(),
                original_value: String::new(),
                is_add: true,
            };
            view.apply_tag_edit(&ctx, "ref", "7B");
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(tags_of(view, Relation, 900), pairs(&[("ref", "7B")]));
            assert_eq!(pending(view), (0, 1, 0));
            undo_step(view);
            assert!(tags_of(view, Relation, 900).is_empty());
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(tags_of(view, Relation, 900), pairs(&[("ref", "7B")]));

            // Copy Tags works.
            view.run_selection_action("copy_tags", cx);
            assert!(view
                .status_message
                .as_ref()
                .is_some_and(|(m, _)| m.starts_with("Copied")));
            assert_eq!(relation_members(view, 900).len(), 2);
            // Nothing to drag.
            assert!(view.resolve_move_targets().is_empty());

            // Zoom to Selection fits the loaded members (way 102 and
            // node 7), not the whole layer.
            view.viewport.pan_to(LatLon::new(10.0, 10.0));
            assert!(view.fit_to_features(&view.selected.clone()));
            let LatLon { lat, lon } = view.viewport.center();
            assert!((lat - 40.002).abs() < 1e-3 && (lon + 73.994).abs() < 1e-3);
        })
        .unwrap();
}

#[gpui::test]
fn role_move_and_remove_edits_are_one_undo_step_each_and_redoable(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let at = |index| MemberRef {
                layer_id,
                relation_id: 900,
                index,
            };
            let original = relation_members(view, 900);
            assert_eq!(
                original,
                vec![(102, String::new()), (7, "stop".to_string())]
            );

            // Role: set, undo, redo; an empty role is fine; no change is
            // not an undo step.
            view.set_relation_member_role(at(0), "outer");
            assert_eq!(view.undo_stack.actions.len(), 1);
            view.set_relation_member_role(at(0), "outer");
            assert_eq!(view.undo_stack.actions.len(), 1);
            let renamed = relation_members(view, 900);
            assert_eq!(renamed[0], (102, "outer".to_string()));
            assert_eq!(pending(view), (0, 1, 0));
            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(relation_members(view, 900), renamed);
            view.set_relation_member_role(at(1), "");
            assert_eq!(relation_members(view, 900)[1], (7, String::new()));
            undo_step(view);
            undo_step(view);
            assert_eq!(relation_members(view, 900), original);

            // Reorder: move down, undo, redo; the ends don't move.
            assert_eq!(view.undo_stack.cursor, 0);
            view.move_relation_member(at(0), 1);
            assert_eq!(view.undo_stack.cursor, 1);
            let moved = relation_members(view, 900);
            assert_eq!(moved, vec![original[1].clone(), original[0].clone()]);
            view.move_relation_member(at(1), 1);
            view.move_relation_member(at(0), -1);
            assert_eq!(relation_members(view, 900), moved);
            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            redo_step(view);
            assert_eq!(relation_members(view, 900), moved);
            undo_step(view);

            // Remove: one step; the last member is refused with a reason.
            assert_eq!(view.undo_stack.cursor, 0);
            view.remove_relation_member(at(0));
            assert_eq!(view.undo_stack.cursor, 1);
            assert_eq!(relation_members(view, 900), vec![original[1].clone()]);
            assert_eq!(pending(view), (0, 1, 0));
            view.remove_relation_member(at(0));
            assert_eq!(view.undo_stack.cursor, 1);
            assert_eq!(relation_members(view, 900).len(), 1);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Can't remove the last member of relation 900")
            );
            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            redo_step(view);
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn removing_a_feature_from_a_relation_on_its_relations_row_is_undoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let original = relation_members(view, 900);
            // Node 7 is the relation's second member.
            select(view, Node, &[7]);
            let membership = view.selected_memberships().remove(0);
            view.remove_relation_member(crate::MemberRef {
                layer_id,
                relation_id: membership.relation_id,
                index: membership.index,
            });
            assert_eq!(relation_members(view, 900), vec![original[0].clone()]);
            assert!(view.selected_memberships().is_empty());
            assert_eq!(pending(view), (0, 1, 0));

            // The way is now the last member: its row refuses.
            select(view, Way, &[102]);
            let membership = view.selected_memberships().remove(0);
            view.remove_relation_member(crate::MemberRef {
                layer_id,
                relation_id: membership.relation_id,
                index: membership.index,
            });
            assert_eq!(relation_members(view, 900).len(), 1);
            assert!(view
                .status_message
                .as_ref()
                .is_some_and(|(m, _)| m.contains("last member")));

            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn add_to_relation_appends_the_new_members_as_one_undo_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let original = relation_members(view, 900);
            // Way 102 is already a member; 100, 103 and node 5 are not.
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[100, 102, 103]);
            view.selected.push(osm_gpui::selection::FeatureRef {
                layer_id,
                kind: Node,
                id: 5,
            });
            // The action asks for a relation first and changes nothing.
            view.run_selection_action("add_to_relation", cx);
            assert!(view.dialogs.pending_relation_picker);
            assert!(view.undo_stack.actions.is_empty());
            // (Opening the picker needs a window root, which the test
            // window lacks.)
            view.dialogs.pending_relation_picker = false;

            view.add_selection_to_relation(900);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let added = relation_members(view, 900);
            assert_eq!(
                added,
                vec![
                    original[0].clone(),
                    original[1].clone(),
                    (100, String::new()),
                    (103, String::new()),
                    (5, String::new()),
                ]
            );
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Added 3 features to relation 900")
            );
            assert_eq!(pending(view), (0, 1, 0));

            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(relation_members(view, 900), added);

            // Everything is a member now: nothing to do, no undo step.
            view.add_selection_to_relation(900);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("The selected features are already members of 900")
            );
        })
        .unwrap();
}

#[gpui::test]
fn add_to_relation_needs_features_and_a_relation_to_add_them_to(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network_with(cx, |data| data.relations.clear());
    window
        .update(cx, |view, _window, cx| {
            select(view, Way, &[100]);
            view.run_selection_action("add_to_relation", cx);
            assert!(!view.dialogs.pending_relation_picker);
            assert!(view
                .status_message
                .as_ref()
                .is_some_and(|(m, _)| m.starts_with("Select nodes or ways")));
        })
        .unwrap();
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // A selected relation alone has nothing to add.
            view.select_relation(view.active_layer.unwrap(), 900);
            view.run_selection_action("add_to_relation", cx);
            assert!(!view.dialogs.pending_relation_picker);
        })
        .unwrap();
}

#[gpui::test]
fn removing_a_restrictions_from_via_or_to_member_is_refused(cx: &mut TestAppContext) {
    use osm_gpui::osm::{MemberType, OsmMember, OsmRelation};
    let window = setup_network_with(cx, |data| {
        let member = |member_type, reference, role: &str| OsmMember {
            member_type,
            reference,
            role: role.to_string(),
        };
        data.relations = vec![OsmRelation {
            id: 900,
            version: 1,
            tags: [("type".to_string(), "restriction".to_string())].into(),
            members: vec![
                member(MemberType::Way, 100, "from"),
                member(MemberType::Node, 3, "via"),
                member(MemberType::Way, 101, "to"),
                member(MemberType::Node, 7, "note"),
            ],
        }];
    });
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            for (index, role) in [(0, "from"), (1, "via"), (2, "to")] {
                view.remove_relation_member(MemberRef {
                    layer_id,
                    relation_id: 900,
                    index,
                });
                assert!(view.undo_stack.actions.is_empty());
                assert_eq!(relation_members(view, 900).len(), 4);
                let status = view.status_message.as_ref().unwrap().0.to_string();
                assert!(status.contains(&format!("'{role}'")), "{status}");
            }
            // The other member may go.
            view.remove_relation_member(MemberRef {
                layer_id,
                relation_id: 900,
                index: 3,
            });
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(relation_members(view, 900).len(), 3);
        })
        .unwrap();
}

#[gpui::test]
fn deleting_a_relation_member_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // Way 102 is a member of relation 900.
            let original = relation_members(view, 900);
            select(view, Way, &[102]);
            view.run_selection_action("delete", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Deleted 1 feature and removed it from 1 relation")
            );
            let remaining = relation_members(view, 900);
            assert_eq!(remaining, vec![(7, "stop".to_string())]);
            // The deleted way, its orphaned node, and the relation that
            // lost it.
            assert_eq!(pending(view), (0, 1, 2));

            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(relation_members(view, 900), remaining);
            assert_eq!(pending(view), (0, 1, 2));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn joining_relation_member_ways_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network_with(cx, route_over(&[(100, "forward"), (101, "forward")]));
    window
        .update(cx, |view, _window, cx| {
            let original = relation_members(view, 900);
            select(view, Way, &[100, 101]);
            view.run_selection_action("join", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let joined = relation_members(view, 900);
            assert_eq!(joined, vec![(100, "forward".to_string())]);
            // Joined way, relation, and the deleted way.
            assert_eq!(pending(view), (0, 2, 1));

            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(relation_members(view, 900), joined);
            assert_eq!(pending(view), (0, 2, 1));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn merging_relation_member_nodes_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // Relation 900 holds way 102 and node 7 ("stop").
            let original = relation_members(view, 900);
            select(view, Node, &[6, 7]);
            view.run_selection_action("merge", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let merged = relation_members(view, 900);
            assert_eq!(merged[1], (6, "stop".to_string()));
            assert_eq!(pending(view), (0, 1, 1));

            undo_step(view);
            assert_eq!(relation_members(view, 900), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(relation_members(view, 900), merged);
            assert_eq!(pending(view), (0, 1, 1));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}
