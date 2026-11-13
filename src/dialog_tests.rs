//! The modal dialogs are enumerated once, by `MapViewer::open_modals`; these
//! tests check that every kind of dialog shows up in it.

use super::*;
use gpui::TestAppContext;
use osm_gpui::ui;

type Opener = fn(&mut MapViewer, &mut Window, &mut Context<MapViewer>);

fn view_window(cx: &mut TestAppContext) -> gpui::WindowHandle<gpui_component::Root> {
    cx.update(gpui_component::init);
    cx.add_window(|window, cx| {
        let view = cx.new(|cx| MapViewer::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    })
}

fn view_of(
    window: gpui::WindowHandle<gpui_component::Root>,
    vcx: &mut gpui::VisualTestContext,
) -> gpui::Entity<MapViewer> {
    let root = window.root(vcx).unwrap();
    root.read_with(vcx, |root, _| root.view().clone())
        .downcast::<MapViewer>()
        .unwrap()
}

/// One opener per dialog of `MapViewer::open_modals`.
fn openers() -> Vec<(&'static str, Opener)> {
    vec![
        ("custom imagery", |v, w, cx| {
            v.custom_imagery_dialog =
                Some(cx.new(|cx| ui::custom_imagery_dialog::CustomImageryDialog::new(w, cx)));
        }),
        ("quit confirm", |v, w, cx| {
            v.quit_confirm_dialog =
                Some(cx.new(|cx| ui::quit_confirm_dialog::QuitConfirmDialog::new(w, cx)));
        }),
        ("delete layer", |v, w, cx| {
            let id = v.layer_manager.alloc_id();
            v.delete_layer_dialog = Some(
                cx.new(|cx| ui::delete_layer_dialog::DeleteLayerDialog::new(id, "L", 0, w, cx)),
            );
        }),
        ("restore", |v, w, cx| {
            v.restore_dialog =
                Some(cx.new(|cx| ui::restore_dialog::RestoreDialog::new(Vec::new(), w, cx)));
        }),
        ("find", |v, w, cx| {
            v.find_dialog =
                Some(cx.new(|cx| {
                    ui::find_dialog::FindDialog::new(Default::default(), Vec::new(), w, cx)
                }));
        }),
        ("about", |v, w, cx| {
            v.about_dialog = Some(cx.new(|cx| ui::about_dialog::AboutDialog::new(w, cx)));
        }),
        ("nsi", |v, w, cx| {
            v.nsi_dialog = Some(cx.new(|cx| ui::nsi_dialog::NsiPresetDialog::new(w, cx)));
        }),
        ("preset picker", |v, w, cx| {
            v.preset_picker_dialog =
                Some(cx.new(|cx| {
                    ui::preset_picker_dialog::PresetPickerDialog::new(Vec::new(), w, cx)
                }));
        }),
        ("relation picker", |v, w, cx| {
            v.relation_picker_dialog = Some(cx.new(|cx| {
                ui::relation_picker_dialog::RelationPickerDialog::new(Vec::new(), w, cx)
            }));
        }),
        ("parallel way", |v, w, cx| {
            v.parallel_dialog = Some(
                cx.new(|cx| ui::parallel_dialog::ParallelDialog::new(Default::default(), w, cx)),
            );
        }),
        ("upload", |v, w, cx| {
            v.upload_dialog =
                Some(cx.new(|cx| ui::upload_dialog::UploadDialog::new(w, cx, Vec::new())));
        }),
        ("sign in", |v, w, cx| {
            v.sign_in_dialog = Some(cx.new(|cx| {
                ui::sign_in_dialog::SignInDialog::new(
                    ui::sign_in_dialog::SignInPurpose::Upload,
                    w,
                    cx,
                )
            }));
        }),
        ("notice", |v, w, cx| {
            v.notice_dialog =
                Some(cx.new(|cx| ui::notice_dialog::NoticeDialog::new("t", "m", w, cx)));
        }),
        ("new note", |v, w, cx| {
            v.open_new_note_dialog(1.0, 2.0, w, cx)
        }),
    ]
}

fn assert_each_opens(names: &[&str], cx: &mut TestAppContext) {
    for (name, open) in openers().into_iter().filter(|(n, _)| names.contains(n)) {
        let window = view_window(cx);
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let view = view_of(window, vcx);
        view.update_in(vcx, |view, window, cx| {
            assert!(!view.any_dialog_open(cx), "{name}: nothing open at start");
            open(view, window, cx);
            assert!(view.any_dialog_open(cx), "{name}: dialog counts as open");
            assert_eq!(view.open_modals(cx).len(), 1, "{name}");
        });
    }
}

#[gpui::test]
fn parallel_restore_and_new_note_dialogs_count_as_open(cx: &mut TestAppContext) {
    assert_each_opens(&["parallel way", "restore", "new note"], cx);
}

#[gpui::test]
fn every_dialog_in_the_modal_list_counts_as_open(cx: &mut TestAppContext) {
    let all: Vec<&str> = openers().into_iter().map(|(n, _)| n).collect();
    assert_each_opens(&all, cx);
}

#[gpui::test]
fn all_dialogs_open_together_are_all_listed(cx: &mut TestAppContext) {
    let window = view_window(cx);
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let view = view_of(window, vcx);
    view.update_in(vcx, |view, window, cx| {
        let openers = openers();
        for (_, open) in &openers {
            open(view, window, cx);
        }
        assert_eq!(view.open_modals(cx).len(), openers.len());
    });
}
