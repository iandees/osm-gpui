//! Preset field edits and reversing a way from the panel.

use super::{pairs, redo_step, select, setup, setup_network, tags_of, undo_step, way_nodes};
use crate::MapViewer;
use gpui::TestAppContext;

#[gpui::test]
fn reverse_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            view.set_selection(vec![osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Way,
                id: 10,
            }]);
            view.run_selection_action("reverse", cx);
            assert_eq!(way_nodes(view, layer_id, 10), vec![2, 1]);
            assert_eq!(view.undo_stack.actions.len(), 1);

            let action = view.undo_stack.undo().unwrap();
            view.apply_undo_action(&action, false);
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
            let action = view.undo_stack.redo().unwrap();
            view.apply_undo_action(&action, true);
            assert_eq!(way_nodes(view, layer_id, 10), vec![2, 1]);
        })
        .unwrap();
}

fn field_edit(view: &mut MapViewer, id: i64, key: &str, value: &str) {
    let layer_id = view.active_layer.unwrap();
    let way = osm_gpui::selection::FeatureRef {
        layer_id,
        kind: osm_gpui::selection::FeatureKind::Way,
        id,
    };
    view.apply_field_values(
        &way,
        std::collections::HashMap::from([(key.to_string(), value.to_string())]),
    );
}

#[gpui::test]
fn name_field_on_a_road_sets_changes_and_clears_the_tag_one_undo_step_each(
    cx: &mut TestAppContext,
) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            field_edit(view, 10, "highway", "tertiary");
            let base = view.undo_stack.actions.len();

            field_edit(view, 10, "name", "Fixture Street");
            assert_eq!(view.undo_stack.actions.len(), base + 1);
            assert!(tags_of(view, Way, 10).contains(&("name".into(), "Fixture Street".into())));

            field_edit(view, 10, "name", "Other Street");
            assert_eq!(view.undo_stack.actions.len(), base + 2);
            assert!(tags_of(view, Way, 10).contains(&("name".into(), "Other Street".into())));

            // Clearing removes the tag; it is not left as `name=`.
            field_edit(view, 10, "name", "");
            assert_eq!(view.undo_stack.actions.len(), base + 3);
            assert!(tags_of(view, Way, 10).iter().all(|(k, _)| k != "name"));

            // Clearing what is already empty changes nothing.
            field_edit(view, 10, "name", "  ");
            assert_eq!(view.undo_stack.actions.len(), base + 3);

            // Each edit undoes on its own.
            undo_step(view);
            assert!(tags_of(view, Way, 10).contains(&("name".into(), "Other Street".into())));
            undo_step(view);
            assert!(tags_of(view, Way, 10).contains(&("name".into(), "Fixture Street".into())));
            undo_step(view);
            assert!(tags_of(view, Way, 10).iter().all(|(k, _)| k != "name"));
            assert!(tags_of(view, Way, 10).contains(&("highway".into(), "tertiary".into())));
        })
        .unwrap();
}

#[gpui::test]
fn combo_field_edit_sets_and_clears_its_key(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let surface = osm_gpui::fields::field_index().get("surface").unwrap();
            assert_eq!(surface.widget(), osm_gpui::fields::Widget::Combo);
            let option = surface.options[0].value.clone();
            field_edit(view, 10, &surface.key, &option);
            assert!(tags_of(view, Way, 10).contains(&("surface".into(), option)));
            assert_eq!(view.undo_stack.actions.len(), 1);
            field_edit(view, 10, &surface.key, "");
            assert!(tags_of(view, Way, 10).iter().all(|(k, _)| k != "surface"));
            undo_step(view);
            assert!(tags_of(view, Way, 10).iter().any(|(k, _)| k == "surface"));
        })
        .unwrap();
}

#[gpui::test]
fn reverse_flips_direction_tags_as_one_undoable_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            let original = {
                let layer = view.layer_manager.find_layer_mut(layer_id).unwrap();
                let editable = layer.as_editable_mut().unwrap();
                editable.set_tag(Way, 100, "cycleway:left", "lane").unwrap();
                editable.set_tag(Way, 100, "incline", "5%").unwrap();
                tags_of(view, Way, 100)
            };
            select(view, Way, &[100]);
            view.run_selection_action("reverse", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(way_nodes(view, layer_id, 100), vec![3, 2, 1]);
            let flipped = pairs(&[
                ("cycleway:right", "lane"),
                ("highway", "residential"),
                ("incline", "-5%"),
            ]);
            assert_eq!(tags_of(view, Way, 100), flipped);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
            assert_eq!(tags_of(view, Way, 100), original);

            redo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![3, 2, 1]);
            assert_eq!(tags_of(view, Way, 100), flipped);
        })
        .unwrap();
}
