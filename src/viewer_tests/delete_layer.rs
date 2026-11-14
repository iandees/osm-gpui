//! Deleting a layer: the confirmation dialog, focus handling and the undo history it drops.

use super::{setup, way_fixture, CENTER_LAT, CENTER_LON};
use crate::undo::UndoableAction;
use crate::{DeleteLayer, MapViewer, Redo, Undo};
use gpui::prelude::*;
use gpui::{point, px, TestAppContext};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};
use std::sync::Arc;

/// Adds a second editable layer (a copy of the fixture) and returns its id.
fn add_second_layer(view: &mut MapViewer) -> LayerId {
    let id = view.layer_manager.alloc_id();
    let layer = OsmLayer::new_with_data(
        id,
        "L2".to_string(),
        Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
    );
    view.layer_manager.add_layer(Box::new(layer));
    id
}

/// Records a Create Node step by clicking at `pt` in Add mode on `layer`.
fn click_node_in(view: &mut MapViewer, layer: LayerId, pt: (f32, f32)) {
    view.active_layer = Some(layer);
    view.set_add_progress(None);
    view.handle_add_click(point(px(pt.0), px(pt.1)), false);
    view.set_add_progress(None);
}

/// History: [B create, A create, A extend, B create], A being the first
/// layer of `setup` and B a second one. Returns (A, B).
fn mixed_layer_history(view: &mut MapViewer) -> (LayerId, LayerId) {
    let a = view.active_layer.unwrap();
    let b = add_second_layer(view);
    click_node_in(view, b, (50.0, 50.0));
    click_node_in(view, a, (50.0, 100.0));
    view.active_layer = Some(a);
    view.handle_add_click(point(px(50.0), px(150.0)), false);
    view.handle_add_click(point(px(50.0), px(200.0)), false);
    view.set_add_progress(None);
    click_node_in(view, b, (50.0, 250.0));
    (a, b)
}

#[gpui::test]
fn deleting_a_layer_removes_its_undo_and_redo_steps(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let (a, b) = mixed_layer_history(view);
            let b_steps = view
                .undo_stack
                .actions
                .iter()
                .filter(|s| s.layers() == vec![b])
                .count();
            assert_eq!(b_steps, 2);
            assert!(view.undo_stack.actions.len() > b_steps);
            // Make B's last step redoable, and select/hover in A.
            view.on_undo(&Undo, window, cx);
            view.selected = vec![osm_gpui::selection::FeatureRef {
                layer_id: a,
                kind: osm_gpui::selection::FeatureKind::Node,
                id: 1,
            }];
            view.hovered = view.selected.first().copied();
            view.active_layer = Some(a);
            let revision = view.undo_stack.revision;

            // A has unsaved edits, so the action itself only asks; the
            // confirmed deletion is what is under test here.
            view.delete_layer_now(a, cx);

            assert!(view.layer_manager.find_layer(a).is_none());
            assert!(view.undo_stack.revision > revision);
            assert_eq!(view.undo_stack.actions.len(), b_steps);
            assert!(view
                .undo_stack
                .actions
                .iter()
                .all(|s| s.layers() == vec![b]));
            // B's first step is undoable, its undone last step redoable.
            assert_eq!(view.undo_stack.cursor, 1);
            assert!(view.selected.is_empty());
            assert!(view.hovered.is_none());
            assert_eq!(view.active_layer, None);

            let b_created = |view: &MapViewer| {
                view.layer_manager
                    .find_layer(b)
                    .unwrap()
                    .as_osm()
                    .unwrap()
                    .diff_for_upload()
                    .counts()
                    .0
            };
            assert_eq!(b_created(view), 1);
            view.on_redo(&Redo, window, cx);
            assert_eq!(b_created(view), 2);
            view.on_undo(&Undo, window, cx);
            view.on_undo(&Undo, window, cx);
            assert_eq!(b_created(view), 0);
            assert!(view.status_message.is_none());
            // Nothing left to undo: a further undo is a quiet no-op.
            view.on_undo(&Undo, window, cx);
            assert_eq!(view.undo_stack.cursor, 0);
        })
        .unwrap();
}

/// Press Cancel (`false`) or Delete Layer (`true`) in the open
/// delete-layer confirmation. The dialog's event is handled when this
/// update ends, so callers inspect the result in a later `update`.
fn press_delete_layer_dialog(
    window: gpui::WindowHandle<MapViewer>,
    cx: &mut TestAppContext,
    confirm: bool,
) {
    window
        .update(cx, |view, _window, cx| {
            let dialog = view.delete_layer_dialog.clone().expect("dialog is open");
            dialog.update(cx, |dialog, cx| {
                if confirm {
                    dialog.press_confirm(cx)
                } else {
                    dialog.press_cancel(cx)
                }
            });
        })
        .unwrap();
}

fn layer_count(view: &MapViewer) -> usize {
    view.layer_manager.layers().len()
}

/// Layer A (index 0) has unsaved edits after `mixed_layer_history`.
fn open_delete_dialog_for_edited_layer(
    window: gpui::WindowHandle<MapViewer>,
    cx: &mut TestAppContext,
) -> (LayerId, LayerId) {
    window
        .update(cx, |view, window, cx| {
            let (a, b) = mixed_layer_history(view);
            view.on_delete_layer(&DeleteLayer { index: 0 }, window, cx);
            (a, b)
        })
        .unwrap()
}

#[gpui::test]
fn deleting_a_layer_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
    window
        .update(cx, |view, _window, cx| {
            assert_eq!(layer_count(view), 2);
            assert!(view.layer_manager.find_layer(a).is_some());
            let dialog = view.delete_layer_dialog.clone().expect("dialog opens");
            let dialog = dialog.read(cx);
            assert_eq!(dialog.layer(), a);
            assert!(dialog.pending() > 0);
            let message = dialog.message();
            assert!(message.contains("unsaved edit"), "{message}");
            assert!(message.contains("cannot be undone"), "{message}");
            assert!(
                message.contains(&format!("{} unsaved edit", dialog.pending())),
                "{message}"
            );
        })
        .unwrap();
}

#[gpui::test]
fn cancelling_the_delete_keeps_the_layer_its_edits_and_history(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
    let before = window
        .update(cx, |view, _w, _cx| {
            (
                view.undo_stack.actions.len(),
                view.undo_stack.cursor,
                view.undo_stack.revision,
                MapViewer::unsaved_edit_count(view.layer_manager.find_layer(a).unwrap()),
            )
        })
        .unwrap();
    press_delete_layer_dialog(window, cx, false);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_none());
            assert_eq!(layer_count(view), 2);
            let after = (
                view.undo_stack.actions.len(),
                view.undo_stack.cursor,
                view.undo_stack.revision,
                MapViewer::unsaved_edit_count(view.layer_manager.find_layer(a).unwrap()),
            );
            assert_eq!(before, after);
            assert!(after.3 > 0);
        })
        .unwrap();
}

#[gpui::test]
fn enter_and_escape_cancel_the_delete(cx: &mut TestAppContext) {
    for key in ["enter", "escape"] {
        let window = setup(cx);
        let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
        cx.simulate_keystrokes(window.into(), key);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_none(), "{key} closes it");
                assert!(view.layer_manager.find_layer(a).is_some(), "{key} keeps it");
                assert_eq!(layer_count(view), 2);
            })
            .unwrap();
    }
}

/// Focus goes back to the map after the dialog took it, as the in-window
/// menu bar does when an item has been chosen.
fn focus_the_map_like_the_menu_bar_does(
    window: gpui::WindowHandle<MapViewer>,
    cx: &mut TestAppContext,
) {
    window
        .update(cx, |view, window, cx| {
            let map = view.focus_handle.clone();
            window.focus(&map, cx);
        })
        .unwrap();
}

#[gpui::test]
fn a_confirmation_opened_from_the_menu_is_focused_and_escape_closes_it(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
    focus_the_map_like_the_menu_bar_does(window, cx);
    // The host puts focus back in the dialog when the frame is drawn.
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear();
    })
    .unwrap();
    window
        .update(cx, |view, window, cx| {
            let dialog = view.delete_layer_dialog.clone().unwrap();
            let handle = gpui::Focusable::focus_handle(dialog.read(cx), cx);
            assert!(handle.contains_focused(window, cx));
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "escape");
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_none(), "escape closes it");
            assert!(view.layer_manager.find_layer(a).is_some());
        })
        .unwrap();
}

#[gpui::test]
fn the_dialog_host_leaves_focus_alone_without_a_dialog(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let map = view.focus_handle.clone();
            window.focus(&map, cx);
            assert!(!view.keep_dialog_focus(window, cx));
            assert!(map.is_focused(window));
        })
        .unwrap();
}

#[gpui::test]
fn confirming_the_delete_removes_the_layer_and_its_history(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
    press_delete_layer_dialog(window, cx, true);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_none());
            assert!(view.layer_manager.find_layer(a).is_none());
            assert!(view.layer_manager.find_layer(b).is_some());
            assert!(!view.undo_stack.actions.is_empty());
            assert!(view
                .undo_stack
                .actions
                .iter()
                .all(|s| s.layers() == vec![b]));
        })
        .unwrap();
}

#[gpui::test]
fn a_layer_without_unsaved_edits_is_deleted_without_asking(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let b = add_second_layer(view);
            assert_eq!(layer_count(view), 2);
            view.on_delete_layer(&DeleteLayer { index: 1 }, window, cx);
            assert!(view.delete_layer_dialog.is_none());
            assert!(view.layer_manager.find_layer(b).is_none());
            assert_eq!(layer_count(view), 1);
            // An index past the end is ignored.
            view.on_delete_layer(&DeleteLayer { index: 7 }, window, cx);
            assert!(view.delete_layer_dialog.is_none());
            assert_eq!(layer_count(view), 1);
        })
        .unwrap();
}

#[gpui::test]
fn deleting_is_refused_during_an_upload_before_the_dialog_and_at_confirm(cx: &mut TestAppContext) {
    let upload = || TransferHandle::new(TransferKind::Upload, TransferPhase::OpeningChangeset);

    // Before: no dialog opens, nothing is deleted.
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let (_a, _b) = mixed_layer_history(view);
            view.transfer = Some(upload());
            view.on_delete_layer(&DeleteLayer { index: 0 }, window, cx);
            assert!(view.delete_layer_dialog.is_none());
            assert_eq!(layer_count(view), 2);
            assert!(view.status_message.is_some());
        })
        .unwrap();

    // At confirm: the dialog was open, then an upload started.
    let window = setup(cx);
    let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_some());
            view.transfer = Some(upload());
            view.status_message = None;
        })
        .unwrap();
    press_delete_layer_dialog(window, cx, true);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_none());
            assert!(view.layer_manager.find_layer(a).is_some());
            assert_eq!(layer_count(view), 2);
            assert!(view.status_message.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn confirming_after_the_layer_disappeared_does_not_panic(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
    window
        .update(cx, |view, _w, cx| {
            // The layer goes away some other way while the dialog is up.
            view.delete_layer_now(a, cx);
            assert!(view.delete_layer_dialog.is_some());
        })
        .unwrap();
    press_delete_layer_dialog(window, cx, true);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.delete_layer_dialog.is_none());
            // The other layer is untouched.
            assert!(view.layer_manager.find_layer(b).is_some());
            assert_eq!(layer_count(view), 1);
        })
        .unwrap();
}

#[gpui::test]
fn the_delete_goes_by_layer_not_by_index_when_the_list_shifts(cx: &mut TestAppContext) {
    let window = setup(cx);
    let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
    window
        .update(cx, |view, _w, cx| {
            // Reordering moves A to index 1 while the dialog is open.
            view.reorder_layer(0, 1);
            cx.notify();
        })
        .unwrap();
    press_delete_layer_dialog(window, cx, true);
    window
        .update(cx, |view, _w, _cx| {
            assert!(view.layer_manager.find_layer(a).is_none());
            assert!(view.layer_manager.find_layer(b).is_some());
        })
        .unwrap();
}

#[gpui::test]
fn a_step_whose_layer_is_gone_is_skipped_with_a_message(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let a = view.active_layer.unwrap();
            let b = add_second_layer(view);
            // A batch with a part in each layer, then drop B behind the
            // history's back (as no current path does).
            let create = |layer, id| UndoableAction::create_node(layer, id, CENTER_LAT, CENTER_LON);
            view.undo_stack.push(UndoableAction::Batch {
                label: "stale".into(),
                actions: vec![create(a, -100), create(b, -101)],
            });
            let index = view
                .layer_manager
                .layers()
                .iter()
                .position(|l| l.id() == b)
                .unwrap();
            view.layer_manager.remove_at(index);

            view.on_undo(&Undo, window, cx);
            let (message, _) = view.status_message.clone().expect("a status message");
            assert_eq!(
                message,
                "That step belongs to a layer that no longer exists, so it was skipped."
            );
            // Nothing was half-applied: layer A never had node -100.
            let layer = view.layer_manager.find_layer(a).unwrap();
            assert!(layer.as_editable().unwrap().node_lat_lon(-100).is_none());
            view.status_message = None;
            view.on_redo(&Redo, window, cx);
            assert!(view.status_message.is_some());
            assert!(layer_has_no_node(view, a, -100));
        })
        .unwrap();
}

fn layer_has_no_node(view: &MapViewer, layer: LayerId, id: i64) -> bool {
    view.layer_manager
        .find_layer(layer)
        .and_then(|l| l.as_editable())
        .is_some_and(|e| e.node_lat_lon(id).is_none())
}
