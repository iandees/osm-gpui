//! The selection-action table says how each action runs (`Run`), where the
//! menus list it and which one the context menu sets apart; these tests pin
//! that, and check the one upload guard in `run_selection_action`.

use crate::clipboard_actions::ClipboardOp;
use crate::selection_actions::{in_menu, Dialog, Interactive, MenuSlot, Run, ACTIONS};
use crate::test_support::{with_session, Build, W};
use gpui::TestAppContext;
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};

fn pinned(slot: MenuSlot) -> Vec<(String, &'static str)> {
    in_menu(slot).map(|a| (a.label_key(), a.id)).collect()
}

#[test]
fn every_action_has_the_run_kind_its_id_implies() {
    for action in ACTIONS {
        let expected = match action.id {
            "copy_features" => Some(Run::Clipboard(ClipboardOp::Copy)),
            "paste_features" => Some(Run::Clipboard(ClipboardOp::Paste)),
            "paste_features_source" => Some(Run::Clipboard(ClipboardOp::PasteAtSource)),
            "duplicate_features" => Some(Run::Clipboard(ClipboardOp::Duplicate)),
            "add_to_relation" => Some(Run::Dialog(Dialog::RelationPicker)),
            "parallel" => Some(Run::Dialog(Dialog::ParallelOffset)),
            "rotate" => Some(Run::Interactive(Interactive::Rotate)),
            _ => None,
        };
        match (action.run, expected) {
            (Run::Apply(_), None) => {}
            (Run::TagClipboard(_), None) => {
                assert!(
                    matches!(action.id, "copy_tags" | "paste_tags"),
                    "{}",
                    action.id
                )
            }
            (Run::Clipboard(a), Some(Run::Clipboard(b))) => assert_eq!(a, b, "{}", action.id),
            (Run::Dialog(a), Some(Run::Dialog(b))) => assert_eq!(a, b, "{}", action.id),
            (Run::Interactive(a), Some(Run::Interactive(b))) => assert_eq!(a, b, "{}", action.id),
            _ => panic!("{} has the wrong kind of Run", action.id),
        }
    }
}

#[test]
fn action_ids_are_unique() {
    for (i, a) in ACTIONS.iter().enumerate() {
        assert!(
            ACTIONS[i + 1..].iter().all(|b| b.id != a.id),
            "duplicate id {}",
            a.id
        );
    }
}

#[test]
fn only_copy_is_allowed_during_an_upload() {
    let readers: Vec<_> = ACTIONS
        .iter()
        .filter(|a| !a.run.edits())
        .map(|a| a.id)
        .collect();
    assert_eq!(readers, ["copy_features"]);
}

#[test]
fn only_delete_is_set_apart_in_the_context_menu() {
    let apart: Vec<_> = ACTIONS
        .iter()
        .filter(|a| a.destructive)
        .map(|a| a.id)
        .collect();
    assert_eq!(apart, ["delete"]);
}

#[test]
fn the_edit_menu_lists_the_clipboard_and_tag_actions() {
    let key = |s: &str| format!("action.{s}.label");
    assert_eq!(
        pinned(MenuSlot::EditClipboard),
        vec![
            (key("copy_features"), "copy_features"),
            (key("paste_features"), "paste_features"),
            (key("paste_features_source"), "paste_features_source"),
            (key("duplicate_features"), "duplicate_features"),
        ]
    );
    assert_eq!(
        pinned(MenuSlot::EditTags),
        vec![
            (key("copy_tags"), "copy_tags"),
            (key("paste_tags"), "paste_tags"),
        ]
    );
}

#[test]
fn the_tools_menu_lists_every_other_action_but_delete_and_the_edit_ones() {
    let tools: Vec<_> = in_menu(MenuSlot::Tools).map(|a| a.id).collect();
    for hidden in [
        "copy_features",
        "paste_features",
        "paste_features_source",
        "duplicate_features",
        "copy_tags",
        "paste_tags",
        "delete",
    ] {
        assert!(!tools.contains(&hidden), "{hidden} is not a Tools item");
    }
    assert_eq!(tools.len() + 7, ACTIONS.len());
}

#[gpui::test]
fn the_upload_guard_refuses_every_editing_action_in_one_place(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        s.view.transfer = Some(TransferHandle::new(
            TransferKind::Upload,
            TransferPhase::OpeningChangeset,
        ));
        let blocked = osm_gpui::tr!("status.upload_blocks_edits").to_string();
        for action in ACTIONS.iter().filter(|a| a.run.edits()) {
            s.view.status_message = None;
            let steps = s.steps();
            s.view.run_selection_action(action.id, s.cx);
            assert_eq!(s.status(), blocked, "{} was not refused", action.id);
            assert_eq!(s.steps(), steps, "{}", action.id);
            assert!(!s.view.dialogs.pending_relation_picker, "{}", action.id);
            assert!(!s.view.dialogs.pending_parallel_dialog, "{}", action.id);
            assert!(s.view.pointer.rotate_session.is_none(), "{}", action.id);
        }
        // Copy only reads, so the guard lets it through.
        s.view.status_message = None;
        s.view.run_selection_action("copy_features", s.cx);
        assert_ne!(s.status(), blocked);
    });
}

mod paste_applicability {
    use crate::clipboard_actions::Clipboards;
    use crate::selection_actions::{find, ActionEnv};
    use crate::test_support::{Build, N, W};
    use osm_gpui::feature_clipboard::FeatureClipboard;
    use osm_gpui::layers::osm_layer::OsmLayer;
    use osm_gpui::layers::{LayerId, LayerManager};
    use osm_gpui::selection::FeatureRef;
    use std::sync::Arc;

    fn applicable(id: &str, env: &ActionEnv) -> bool {
        (find(id).unwrap().is_applicable)(env)
    }

    /// Paste and Paste Tags are decided by the `ActionEnv` alone: the same
    /// selection and layers give a different answer only when the clipboards
    /// in the env differ, and the answer repeats.
    #[test]
    fn paste_applicability_is_a_pure_function_of_the_env() {
        let layer_id = LayerId(1);
        let data = Build::new()
            .nodes(&[1, 2])
            .way(10, &[1, 2], &[("highway", "path")])
            .done();
        let mut layers = LayerManager::new();
        layers.add_layer(Box::new(OsmLayer::new_with_data(
            layer_id,
            "L".to_string(),
            Arc::new(data),
        )));
        let way = FeatureRef {
            layer_id,
            kind: W,
            id: 10,
        };
        let node = FeatureRef {
            layer_id,
            kind: N,
            id: 1,
        };
        let selection = [way];
        let empty = Clipboards::default();
        let any_paste = |clipboards: &Clipboards| -> (bool, bool) {
            let env = ActionEnv {
                selection: &selection,
                active_layer: Some(layer_id),
                layers: &layers,
                clipboards,
            };
            let tags = applicable("paste_tags", &env);
            let features = applicable("paste_features", &env);
            assert_eq!(features, applicable("paste_features_source", &env));
            assert_eq!(tags, applicable("paste_tags", &env), "repeatable");
            (tags, features)
        };
        assert_eq!(any_paste(&empty), (false, false));

        let mut tags_only = Clipboards::default();
        tags_only.set_tags(vec![("a".into(), "b".into())]);
        let editable = layers.find_layer(layer_id).unwrap().as_editable().unwrap();
        let features_only = Clipboards {
            features: FeatureClipboard::copy(editable, layer_id, &[node]),
            ..Default::default()
        };
        assert!(features_only.has_features());

        assert_eq!(any_paste(&tags_only), (true, false));
        assert_eq!(any_paste(&features_only), (false, true));
        // Back to the empty env: nothing carried over between calls.
        assert_eq!(any_paste(&empty), (false, false));
    }
}
