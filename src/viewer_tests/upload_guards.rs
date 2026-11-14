//! What is refused while an upload runs, and the signed-out prompts.

use super::{editable, make_pending_change, pending, select, setup, status_text};
use crate::{DeleteSelection, MapViewer, MemberRef, Redo, TagEditContext, Undo, UploadToOsm};
use gpui::{point, px, Context, TestAppContext, Window};
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};
use osm_gpui::{auth, settings_store};

#[gpui::test]
fn edits_and_undo_are_refused_while_an_upload_runs(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            make_pending_change(view);
            // Not uploading: nothing is refused.
            assert!(!view.edits_blocked(cx));
            assert!(view.status_message.is_none());

            view.transfer = Some(TransferHandle::new(
                TransferKind::Upload,
                TransferPhase::OpeningChangeset,
            ));
            for handler in [
                |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                    v.on_undo(&Undo, w, cx)
                },
                |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                    v.on_redo(&Redo, w, cx)
                },
                |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                    v.on_delete_selection(&DeleteSelection, w, cx)
                },
            ] {
                view.status_message = None;
                handler(view, window, cx);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("An upload is in progress. Wait for it to finish before editing.")
                );
            }

            // A download does not freeze editing.
            view.transfer = Some(TransferHandle::new(
                TransferKind::Download,
                TransferPhase::Downloading,
            ));
            assert!(!view.edits_blocked(cx));

            // Quitting during an upload is refused with a message.
            view.status_message = None;
            view.refuse_quit_during_upload(cx);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("An upload is in progress. Wait for it to finish before quitting.")
            );
        })
        .unwrap();
}

/// Every way the side panel (and the map) changes the layers is refused
/// during an upload, with the standard message and nothing recorded.
#[gpui::test]
fn side_panel_edits_are_refused_while_an_upload_runs(cx: &mut TestAppContext) {
    use osm_gpui::osm::{MemberType, OsmMember};
    use osm_gpui::selection::{FeatureKind, FeatureRef};
    const REFUSAL: &str = "An upload is in progress. Wait for it to finish before editing.";
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            let members = vec![
                OsmMember {
                    member_type: MemberType::Node,
                    reference: 1,
                    role: "a".into(),
                },
                OsmMember {
                    member_type: MemberType::Node,
                    reference: 2,
                    role: "b".into(),
                },
            ];
            let relation_id = editable(view)
                .create_relation(Vec::new(), members.clone())
                .unwrap();
            let node1 = FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: 1,
            };
            let ctx = TagEditContext {
                features: vec![node1],
                original_key: String::new(),
                original_value: String::new(),
                is_add: true,
            };
            let member = MemberRef {
                layer_id,
                relation_id,
                index: 0,
            };
            let preset_tags =
                std::collections::HashMap::from([("amenity".to_string(), "cafe".to_string())]);
            select(view, FeatureKind::Node, &[1]);

            // Each edit, as the side panel makes it.
            type Edit = Box<dyn Fn(&mut MapViewer, &mut Context<MapViewer>)>;
            let edits: Vec<(&str, Edit)> = vec![
                ("tag edit", {
                    let ctx = TagEditContext {
                        features: ctx.features.clone(),
                        original_key: String::new(),
                        original_value: String::new(),
                        is_add: true,
                    };
                    Box::new(move |v, _| v.apply_tag_edit(&ctx, "amenity", "cafe"))
                }),
                ("tag delete", Box::new(|v, cx| v.delete_tag("amenity", cx))),
                (
                    "preset",
                    Box::new({
                        let tags = preset_tags.clone();
                        move |v, _| v.apply_preset(&node1, &tags, &[])
                    }),
                ),
                (
                    "field / NSI preset",
                    Box::new({
                        let tags = preset_tags.clone();
                        move |v, _| v.apply_nsi_preset(&node1, tags.clone())
                    }),
                ),
                (
                    "member role",
                    Box::new(move |v, _| v.set_relation_member_role(member, "z")),
                ),
                (
                    "member order",
                    Box::new(move |v, _| v.move_relation_member(member, 1)),
                ),
                (
                    "member remove",
                    Box::new(move |v, _| v.remove_relation_member(member)),
                ),
                (
                    "add to relation",
                    Box::new(move |v, _| v.add_selection_to_relation(relation_id)),
                ),
                (
                    "map add click",
                    Box::new(|v, _| v.handle_add_click(point(px(400.0), px(300.0)), false)),
                ),
            ];

            view.transfer = Some(TransferHandle::new(
                TransferKind::Upload,
                TransferPhase::UploadingChanges { count: 1 },
            ));
            let before = (
                pending(view),
                view.undo_stack.actions.len(),
                view.relation_of(layer_id, relation_id).unwrap().members,
            );
            for (name, edit) in &edits {
                view.status_message = None;
                edit(view, cx);
                assert_eq!(status_text(view), Some(REFUSAL), "{name} not refused");
                let after = (
                    pending(view),
                    view.undo_stack.actions.len(),
                    view.relation_of(layer_id, relation_id).unwrap().members,
                );
                assert_eq!(before, after, "{name} changed the data");
            }

            // A download does not block the same edits.
            view.transfer = Some(TransferHandle::new(
                TransferKind::Download,
                TransferPhase::Downloading,
            ));
            view.status_message = None;
            view.apply_tag_edit(&ctx, "amenity", "cafe");
            view.set_relation_member_role(member, "z");
            assert_ne!(status_text(view), Some(REFUSAL));
            assert_eq!(view.undo_stack.actions.len(), 2);
        })
        .unwrap();
}

#[gpui::test]
fn upload_without_any_layer_shows_a_status_message(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, window, cx| {
            view.open_upload_dialog_as(Some("me"), window, cx);
            assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
            assert!(view.status_message.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn upload_signed_out_shows_the_sign_in_dialog(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            make_pending_change(view);
            view.open_upload_dialog_as(None, window, cx);
            assert!(view.sign_in_dialog.is_some());
            assert!(view.upload_dialog.is_none());
            let dialog = view.sign_in_dialog.as_ref().unwrap().read(cx);
            assert_eq!(
                dialog.purpose(),
                osm_gpui::ui::sign_in_dialog::SignInPurpose::Upload
            );
        })
        .unwrap();
}

#[gpui::test]
fn new_note_signed_out_shows_the_notes_sign_in_wording(cx: &mut TestAppContext) {
    use osm_gpui::ui::sign_in_dialog::SignInPurpose;
    let stored = auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
    if !osm_gpui::ui::sign_in_dialog::needs_sign_in(stored.as_deref()) {
        return;
    }
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.on_new_note(&crate::notes_ui::NewNote, window, cx);
            let dialog = view.sign_in_dialog.as_ref().expect("sign-in prompt");
            assert_eq!(dialog.read(cx).purpose(), SignInPurpose::Notes);
            assert!(!view.notes_armed());
            assert!(view.upload_dialog.is_none());
        })
        .unwrap();
}

/// Signed out, Comment, Close and Reopen in the note panel open the notes
/// sign-in prompt, send nothing and keep the typed text and the panel.
#[gpui::test]
fn signed_out_note_panel_actions_open_the_sign_in_prompt(cx: &mut TestAppContext) {
    use osm_gpui::script::NoteOp;
    use osm_gpui::ui::sign_in_dialog::{DialogEvent, SignInPurpose};
    let stored = auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
    if !osm_gpui::ui::sign_in_dialog::needs_sign_in(stored.as_deref()) {
        return;
    }
    let window = setup(cx);
    let ops = [
        NoteOp::Comment("hello there".to_string()),
        NoteOp::Close(Some("hello there".to_string())),
        NoteOp::Reopen(Some("hello there".to_string())),
    ];
    for op in ops {
        window
            .update(cx, |view, window, cx| {
                let load = NoteOp::LoadFile("docs/live-test/notes-sample.xml".to_string());
                view.script_note_op(&load, window, cx).unwrap();
                view.script_note_op(&NoteOp::Open(1001), window, cx)
                    .unwrap();
                let result = view.script_note_op(&op, window, cx);
                assert!(result.unwrap_err().contains("not signed in"));
                // Nothing was started, the panel is still open and the
                // typed text is still in the box.
                let (text, has_error, writing) =
                    view.note_panel_state(cx).expect("panel stays open");
                assert_eq!(text, "hello there");
                assert!(!has_error && !writing);
                let dialog = view.sign_in_dialog.as_ref().expect("sign-in prompt");
                assert_eq!(dialog.read(cx).purpose(), SignInPurpose::Notes);
                assert!(view.upload_dialog.is_none());
                dialog.update(cx, |_, cx| cx.emit(DialogEvent::Cancelled));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(view.sign_in_dialog.is_none());
                assert_eq!(view.note_panel_state(cx).unwrap().0, "hello there");
            })
            .unwrap();
    }
}

/// A signed-out comment/close/reopen all go through `show_sign_in_dialog(Notes, ..)`; closing the prompt
/// (as after "Open Settings") leaves no upload dialog behind.
#[gpui::test]
fn note_panel_sign_in_uses_notes_wording_and_never_opens_upload(cx: &mut TestAppContext) {
    use osm_gpui::ui::sign_in_dialog::SignInPurpose;
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.show_sign_in_dialog(SignInPurpose::Notes, window, cx);
            let dialog = view.sign_in_dialog.as_ref().expect("sign-in prompt");
            assert_eq!(dialog.read(cx).purpose(), SignInPurpose::Notes);
            dialog.update(cx, |_, cx| {
                cx.emit(osm_gpui::ui::sign_in_dialog::DialogEvent::Cancelled)
            });
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, _, _| {
            assert!(view.sign_in_dialog.is_none());
            assert!(view.upload_dialog.is_none());
        })
        .unwrap();
}

/// Dispatching `UploadToOsm` through the window (as the native menu and the
/// shortcut do) must reach a window-scoped handler: a global one would
/// re-look-up the checked-out window and do nothing.
#[gpui::test]
fn upload_action_dispatched_through_the_window_opens_a_dialog(cx: &mut TestAppContext) {
    use gpui::Action as _;
    // Signed out -> the sign-in prompt. (A signed-in upload dialog needs
    // gpui-component's `Root`, which this test window lacks, so skip on a
    // machine with a stored login.)
    let stored = auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
    if !osm_gpui::ui::sign_in_dialog::needs_sign_in(stored.as_deref()) {
        return;
    }
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            make_pending_change(view);
            window.focus(&view.focus_handle, cx);
        })
        .unwrap();
    // Draw so the dispatch tree (with the root's action listeners) exists.
    cx.update(|cx| {
        window.update(cx, |_, window, _| window.refresh()).unwrap();
    });
    cx.run_until_parked();
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(UploadToOsm.boxed_clone(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, _, _| {
            assert!(view.sign_in_dialog.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn nothing_to_export_without_an_osm_layer(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.exportable_layer().is_none());
        })
        .unwrap();
}
