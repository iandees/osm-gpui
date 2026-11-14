//! Creating, deleting and re-splitting relations, and keyboard navigation of member rows.

use super::{
    editable, pending, redo_step, relation_members, route_over, select, setup_network,
    setup_network_with, status_text, undo_step, CENTER_LAT, CENTER_LON,
};
use crate::undo::UndoableAction;
use crate::{selection_actions, EditMode, MapViewer};
use gpui::prelude::*;
use gpui::{point, px, size, TestAppContext};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::viewport::Viewport;
use std::sync::Arc;

#[gpui::test]
fn split_way_in_a_relation_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network_with(cx, route_over(&[(100, ""), (101, "forward"), (102, "")]));
    window
        .update(cx, |view, _window, cx| {
            // Node 4 on way 101 only.
            let layer_id = view.active_layer.unwrap();
            view.set_selection(vec![
                osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Node,
                    id: 4,
                },
                osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Way,
                    id: 101,
                },
            ]);
            view.run_selection_action("split", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let after = relation_members(view, 900);
            assert_eq!(after.len(), 4);
            assert_eq!(after[1], (101, "forward".to_string()));
            assert!(after[2].0 < 0 && after[2].1 == "forward");
            // The new way, way 101, and the relation.
            assert_eq!(pending(view), (1, 2, 0));

            undo_step(view);
            assert_eq!(
                relation_members(view, 900),
                vec![
                    (100, String::new()),
                    (101, "forward".to_string()),
                    (102, String::new())
                ]
            );
            assert_eq!(pending(view), (0, 0, 0));

            redo_step(view);
            assert_eq!(relation_members(view, 900), after);
            assert_eq!(pending(view), (1, 2, 0));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn set_relation_members_undoes_and_redoes_and_is_labelled(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let before = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_editable()
                .unwrap()
                .relation(900)
                .unwrap()
                .members;
            let mut after = before.clone();
            after.push(osm_gpui::osm::OsmMember {
                member_type: osm_gpui::osm::MemberType::Way,
                reference: 101,
                role: "outer".to_string(),
            });
            view.layer_manager
                .find_layer_mut(layer_id)
                .unwrap()
                .as_editable_mut()
                .unwrap()
                .set_relation_members(900, after.clone())
                .unwrap();
            let action = UndoableAction::set_relation_members(layer_id, 900, before, after);
            assert_eq!(action.description(), "Changed the members of a relation");
            view.undo_stack.push(action);
            assert_eq!(relation_members(view, 900).len(), 3);
            assert_eq!(pending(view), (0, 1, 0));

            undo_step(view);
            assert_eq!(relation_members(view, 900).len(), 2);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(
                relation_members(view, 900).last(),
                Some(&(101, "outer".to_string()))
            );
            assert_eq!(pending(view), (0, 1, 0));
        })
        .unwrap();
}

#[gpui::test]
fn create_and_delete_relation_undo_and_redo(cx: &mut TestAppContext) {
    use osm_gpui::osm::{MemberType, OsmMember};
    use osm_gpui::selection::DeletedRelationSnapshot;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let member = |member_type, reference| OsmMember {
                member_type,
                reference,
                role: "x".to_string(),
            };

            // Create a relation containing existing relation 900.
            let new_id = editable(view)
                .create_relation(
                    vec![("type".to_string(), "route".to_string())],
                    vec![
                        member(MemberType::Relation, 900),
                        member(MemberType::Way, 101),
                    ],
                )
                .unwrap();
            assert!(new_id < 0);
            let snapshot = DeletedRelationSnapshot {
                relation: editable(view).relation(new_id).unwrap(),
                index: editable(view).relations().len() - 1,
            };
            let create = UndoableAction::create_relation(layer_id, snapshot);
            assert_eq!(create.description(), "Created a relation");
            view.undo_stack.push(create);
            assert_eq!(pending(view), (1, 0, 0));
            assert_eq!(editable(view).relations_containing_relation(900), [new_id]);
            // 900 is a member now, so it can't be deleted.
            assert!(editable(view).delete_relation(900).is_err());

            undo_step(view);
            assert!(editable(view).relation(new_id).is_none());
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(editable(view).relation(new_id).unwrap().members.len(), 2);
            assert_eq!(pending(view), (1, 0, 0));
            // A later allocation never reuses the id.
            let other = editable(view).create_relation(vec![], vec![]).unwrap();
            assert!(other < new_id);
            editable(view).delete_relation(other).unwrap();

            // Delete both; the created one has to go first.
            let original_900 = editable(view).relation(900).unwrap();
            let created = editable(view).delete_relation(new_id).unwrap();
            let removed = editable(view).delete_relation(900).unwrap();
            assert_eq!(removed.relation, original_900);
            let delete = UndoableAction::Batch {
                label: "Deleted relations".to_string(),
                actions: vec![
                    UndoableAction::delete_relation(layer_id, created),
                    UndoableAction::delete_relation(layer_id, removed),
                ],
            };
            view.undo_stack.push(delete);
            assert_eq!(pending(view), (0, 0, 1));
            assert_eq!(
                UndoableAction::delete_relation(
                    layer_id,
                    DeletedRelationSnapshot {
                        relation: original_900.clone(),
                        index: 0
                    }
                )
                .description(),
                "Deleted a relation"
            );

            undo_step(view);
            assert_eq!(editable(view).relation(900).unwrap(), original_900);
            assert_eq!(editable(view).relation(new_id).unwrap().members.len(), 2);
            assert_eq!(pending(view), (1, 0, 0));
            redo_step(view);
            assert!(editable(view).relation(900).is_none());
            assert!(editable(view).relation(new_id).is_none());
            assert_eq!(pending(view), (0, 0, 1));
        })
        .unwrap();
}

#[gpui::test]
fn new_relation_collects_the_selection_and_is_one_undo_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Relation, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[101, 100]);
            view.selected.push(osm_gpui::selection::FeatureRef {
                layer_id,
                kind: Node,
                id: 7,
            });
            view.run_selection_action("new_relation", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.undo_stack.actions[0].description(),
                "Created a relation"
            );
            assert_eq!(pending(view), (1, 0, 0));

            // The new relation is selected, with the members in
            // selection order and empty roles, and no tags.
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].kind, Relation);
            let new_id = view.selected[0].id;
            assert!(new_id < 0);
            let relation = editable(view).relation(new_id).unwrap();
            assert!(relation.tags.is_empty());
            let members: Vec<_> = relation
                .members
                .iter()
                .map(|m| (m.reference, m.role.as_str()))
                .collect();
            assert_eq!(members, [(101, ""), (100, ""), (7, "")]);
            assert_eq!(
                status_text(view),
                Some(
                    "Created a relation with 3 features. Add its tags and roles in the side panel"
                )
            );

            undo_step(view);
            assert!(editable(view).relation(new_id).is_none());
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(editable(view).relation(new_id).unwrap(), relation);
            assert_eq!(pending(view), (1, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn new_relation_needs_a_selection_and_accepts_relations(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            view.set_selection(Vec::new());
            view.run_selection_action("new_relation", cx);
            assert!(view.undo_stack.actions.is_empty());
            assert!(status_text(view).is_some_and(|m| m.starts_with("Select nodes, ways")));

            // A selected relation can be a member of the new one.
            view.select_relation(view.active_layer.unwrap(), 900);
            select(view, Way, &[100]);
            view.selected.push(osm_gpui::selection::FeatureRef {
                layer_id: view.active_layer.unwrap(),
                kind: osm_gpui::selection::FeatureKind::Relation,
                id: 900,
            });
            view.run_selection_action("new_relation", cx);
            let new_id = view.selected[0].id;
            let relation = editable(view).relation(new_id).unwrap();
            assert_eq!(relation.members.len(), 2);
            assert_eq!(relation.members[1].reference, 900);
            assert_eq!(editable(view).relations_containing_relation(900), [new_id]);
        })
        .unwrap();
}

#[gpui::test]
fn deleting_a_selected_relation_keeps_its_members_and_undoes(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            let original = editable(view).relation(900).unwrap();
            view.select_relation(layer_id, 900);
            view.delete_selected_features(cx);

            assert!(editable(view).relation(900).is_none());
            // Its member way and node survive.
            assert!(editable(view).way_node_ids(102).is_some());
            assert!(editable(view).node_lat_lon(7).is_some());
            assert!(view.selected.is_empty());
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.undo_stack.actions[0].description(),
                "Deleted a relation"
            );
            assert_eq!(status_text(view), Some("Deleted 1 relation"));
            assert_eq!(pending(view), (0, 0, 1));

            undo_step(view);
            assert_eq!(editable(view).relation(900).unwrap(), original);
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert!(editable(view).relation(900).is_none());
            assert_eq!(pending(view), (0, 0, 1));
        })
        .unwrap();
}

/// Tab walks the enabled up/down/remove buttons of the Members rows in
/// row order, and a row counts as focused (so its buttons are shown)
/// whenever focus is on any of its buttons.
#[gpui::test]
fn tab_reaches_member_buttons_in_row_order_and_reveals_the_row(cx: &mut TestAppContext) {
    let mut data = selection_actions::test_support::network_data();
    route_over(&[(100, ""), (101, ""), (102, "")])(&mut data);
    cx.update(gpui_component::init);
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| MapViewer::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let root = window.root(vcx).unwrap();
    let view: gpui::Entity<MapViewer> = root
        .read_with(vcx, |root, _| root.view().clone())
        .downcast::<MapViewer>()
        .unwrap();
    view.update(vcx, |view, _| {
        view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
        let layer_id = view.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(layer_id, "Net".to_string(), Arc::new(data));
        view.layer_manager.add_layer(Box::new(layer));
        view.active_layer = Some(layer_id);
        view.enter_mode(EditMode::Select);
        view.set_selection(vec![osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Relation,
            id: 900,
        }]);
    });
    let draw = |vcx: &mut gpui::VisualTestContext| {
        let root = root.clone();
        vcx.draw(
            point(px(0.0), px(0.0)),
            size(
                gpui::AvailableSpace::Definite(px(1200.0)),
                gpui::AvailableSpace::Definite(px(800.0)),
            ),
            |_, _| gpui::AnyView::from(root),
        );
    };
    draw(vcx);

    // Which row (if any) holds focus now.
    let focused_row = |vcx: &mut gpui::VisualTestContext| -> Option<usize> {
        view.update_in(vcx, |view, window, cx| {
            view.member_controls_focus
                .borrow()
                .iter()
                .position(|handle| handle.contains_focused(window, cx))
        })
    };
    // Real key presses: the window's `Root` turns Tab into `focus_next`.
    let tab = |vcx: &mut gpui::VisualTestContext, forward: bool| {
        vcx.simulate_keystrokes(if forward { "tab" } else { "shift-tab" });
        draw(vcx);
    };

    let mut stops: Vec<usize> = Vec::new();
    for _ in 0..80 {
        tab(vcx, true);
        if let Some(row) = focused_row(vcx) {
            stops.push(row);
        }
    }
    // Row 0 has down+remove, row 1 up+down+remove, row 2 up+remove, and
    // the cycle repeats once Tab wraps around.
    let one_pass = [0, 0, 1, 1, 1, 2, 2];
    assert!(
        stops.starts_with(&one_pass),
        "unexpected tab order {stops:?}"
    );
    assert!(stops.len() >= one_pass.len() * 2, "Tab never wrapped");

    // Shift-Tab walks back the other way: from row 1's first button
    // (reached by Tab until the row changes) it returns to row 0.
    while focused_row(vcx) != Some(0) {
        tab(vcx, true);
    }
    while focused_row(vcx) != Some(1) {
        tab(vcx, true);
    }
    tab(vcx, false);
    assert_eq!(focused_row(vcx), Some(0));
}

#[gpui::test]
fn deleting_a_relation_that_is_a_member_is_refused_until_its_parent_goes(cx: &mut TestAppContext) {
    use osm_gpui::osm::{MemberType, OsmMember};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            let parent = editable(view)
                .create_relation(
                    vec![],
                    vec![OsmMember {
                        member_type: MemberType::Relation,
                        reference: 900,
                        role: String::new(),
                    }],
                )
                .unwrap();

            // Alone, 900 can't go: the parent would be left dangling.
            view.select_relation(layer_id, 900);
            view.delete_selected_features(cx);
            assert!(editable(view).relation(900).is_some());
            assert!(view.undo_stack.actions.is_empty());
            assert!(status_text(view)
                .is_some_and(|m| m.starts_with("Can't delete relation 900")
                    && m.contains(&parent.to_string())));

            // Selected together with its parent, both go as one step.
            view.select_relation(layer_id, 900);
            view.selected.push(osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Relation,
                id: parent,
            });
            view.delete_selected_features(cx);
            assert!(editable(view).relation(900).is_none());
            assert!(editable(view).relation(parent).is_none());
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(status_text(view), Some("Deleted 2 relations"));

            undo_step(view);
            assert!(editable(view).relation(900).is_some());
            assert!(editable(view).relation(parent).is_some());
        })
        .unwrap();
}
