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

/// Open `build` as the only dialog of a window, draw it, run `act`, and return
/// the names `name` gives to the events it emitted meanwhile. Dialogs are
/// built on their own here because the opener table above bypasses the
/// subscriptions the host sets up to close them.
fn dialog_events<D, E>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut Context<D>) -> D + 'static,
    name: fn(&E) -> String,
    act: impl FnOnce(&mut gpui::VisualTestContext, &gpui::Entity<D>),
) -> Vec<String>
where
    D: gpui::Render + gpui::EventEmitter<E> + 'static,
    E: 'static,
{
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(gpui_component::init);
    let window = cx.add_window(|window, cx| {
        let dialog = cx.new(|cx| build(window, cx));
        gpui_component::Root::new(dialog, window, cx)
    });
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let root = window.root(vcx).unwrap();
    let dialog: gpui::Entity<D> = root
        .read_with(vcx, |root, _| root.view().clone())
        .downcast()
        .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let _sub = vcx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, event: &E, _| {
            sink.borrow_mut().push(name(event))
        })
    });
    vcx.draw(
        gpui::point(gpui::px(0.0), gpui::px(0.0)),
        gpui::size(
            gpui::AvailableSpace::Definite(gpui::px(1200.0)),
            gpui::AvailableSpace::Definite(gpui::px(800.0)),
        ),
        |_, _| gpui::AnyView::from(root.clone()),
    );
    act(vcx, &dialog);
    let events = seen.borrow().clone();
    events
}

fn press<D: 'static>(
    keys: &'static [&'static str],
) -> impl FnOnce(&mut gpui::VisualTestContext, &gpui::Entity<D>) {
    move |vcx, _| {
        for key in keys {
            vcx.simulate_keystrokes(key);
        }
    }
}

fn click_outside(vcx: &mut gpui::VisualTestContext) {
    vcx.simulate_click(
        gpui::point(gpui::px(10.0), gpui::px(10.0)),
        gpui::Modifiers::none(),
    );
}

macro_rules! closes_on_escape {
    ($cx:expr, $build:expr, $event:ty, $closing:pat) => {{
        let events = dialog_events(
            $cx,
            $build,
            |e: &$event| {
                if matches!(e, $closing) {
                    "closed"
                } else {
                    "other"
                }
                .to_string()
            },
            press(&["escape"]),
        );
        assert_eq!(events, ["closed"]);
    }};
}

#[gpui::test]
fn escape_closes_every_dialog_in_its_initial_state(cx: &mut TestAppContext) {
    use ui::*;
    closes_on_escape!(
        cx,
        custom_imagery_dialog::CustomImageryDialog::new,
        custom_imagery_dialog::DialogEvent,
        custom_imagery_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        quit_confirm_dialog::QuitConfirmDialog::new,
        quit_confirm_dialog::DialogEvent,
        quit_confirm_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| delete_layer_dialog::DeleteLayerDialog::new(
            osm_gpui::layers::LayerId(1),
            "L",
            0,
            w,
            cx
        ),
        delete_layer_dialog::DialogEvent,
        delete_layer_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| restore_dialog::RestoreDialog::new(Vec::new(), w, cx),
        restore_dialog::DialogEvent,
        restore_dialog::DialogEvent::Later
    );
    closes_on_escape!(
        cx,
        |w, cx| find_dialog::FindDialog::new(Default::default(), Vec::new(), w, cx),
        find_dialog::DialogEvent,
        find_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        about_dialog::AboutDialog::new,
        about_dialog::DialogEvent,
        about_dialog::DialogEvent::Closed
    );
    closes_on_escape!(
        cx,
        nsi_dialog::NsiPresetDialog::new,
        nsi_dialog::DialogEvent,
        nsi_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| preset_picker_dialog::PresetPickerDialog::new(Vec::new(), w, cx),
        preset_picker_dialog::DialogEvent,
        preset_picker_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| relation_picker_dialog::RelationPickerDialog::new(Vec::new(), w, cx),
        relation_picker_dialog::DialogEvent,
        relation_picker_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| parallel_dialog::ParallelDialog::new(Default::default(), w, cx),
        parallel_dialog::DialogEvent,
        parallel_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| upload_dialog::UploadDialog::new(w, cx, Vec::new()),
        upload_dialog::DialogEvent,
        upload_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| sign_in_dialog::SignInDialog::new(sign_in_dialog::SignInPurpose::Upload, w, cx),
        sign_in_dialog::DialogEvent,
        sign_in_dialog::DialogEvent::Cancelled
    );
    closes_on_escape!(
        cx,
        |w, cx| notice_dialog::NoticeDialog::new("t", "m", w, cx),
        notice_dialog::DialogEvent,
        notice_dialog::DialogEvent::Closed
    );
    closes_on_escape!(
        cx,
        |w, cx| note_dialog::NewNoteDialog::new(1.0, 2.0, w, cx),
        note_dialog::DialogEvent,
        note_dialog::DialogEvent::Cancelled
    );
}

fn sign_in_event(e: &ui::sign_in_dialog::DialogEvent) -> String {
    match e {
        ui::sign_in_dialog::DialogEvent::OpenSettings => "open-settings",
        ui::sign_in_dialog::DialogEvent::Cancelled => "cancelled",
    }
    .to_string()
}

#[gpui::test]
fn sign_in_enter_opens_settings_and_escape_cancels(cx: &mut TestAppContext) {
    use ui::sign_in_dialog::{SignInDialog, SignInPurpose};
    let build = |w: &mut Window, cx: &mut Context<SignInDialog>| {
        SignInDialog::new(SignInPurpose::Upload, w, cx)
    };
    assert_eq!(
        dialog_events(cx, build, sign_in_event, press(&["enter"])),
        ["open-settings"]
    );
    assert_eq!(
        dialog_events(cx, build, sign_in_event, press(&["a", "escape"])),
        ["cancelled"]
    );
}

fn relation_event(e: &ui::relation_picker_dialog::DialogEvent) -> String {
    match e {
        ui::relation_picker_dialog::DialogEvent::Submitted(id) => format!("submitted {id}"),
        ui::relation_picker_dialog::DialogEvent::Cancelled => "cancelled".to_string(),
    }
}

#[gpui::test]
fn relation_picker_arrows_move_the_selection_and_enter_submits_it(cx: &mut TestAppContext) {
    use osm_gpui::relations::RelationChoice;
    use ui::relation_picker_dialog::RelationPickerDialog;
    let build = |w: &mut Window, cx: &mut Context<RelationPickerDialog>| {
        let choices = [(10, "A"), (20, "B")]
            .map(|(id, name)| RelationChoice {
                id,
                name: name.to_string(),
                member_count: 1,
            })
            .to_vec();
        RelationPickerDialog::new(choices, w, cx)
    };
    for (keys, expected) in [
        (&["enter"][..], "submitted 10"),
        (&["down", "enter"][..], "submitted 20"),
        (&["down", "down", "enter"][..], "submitted 20"),
        (&["down", "up", "enter"][..], "submitted 10"),
        (&["up", "enter"][..], "submitted 10"),
    ] {
        let events = dialog_events(cx, build, relation_event, |vcx, _| {
            for key in keys {
                if *key == "enter" {
                    // `simulate_keystrokes` would type a newline into the
                    // search field, which gpui cannot shape.
                    vcx.simulate_event(gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            modifiers: gpui::Modifiers::none(),
                            key: "enter".into(),
                            key_char: Some(String::new()),
                        },
                        is_held: false,
                        prefer_character_input: false,
                    });
                } else {
                    vcx.simulate_keystrokes(key);
                }
            }
        });
        assert_eq!(events, [expected], "{keys:?}");
    }
}

#[gpui::test]
fn new_note_ignores_escape_while_sending_and_enter_never_submits(cx: &mut TestAppContext) {
    use ui::note_dialog::{DialogEvent, NewNoteDialog};
    let build =
        |w: &mut Window, cx: &mut Context<NewNoteDialog>| NewNoteDialog::new(1.0, 2.0, w, cx);
    let name = |e: &DialogEvent| match e {
        DialogEvent::Submit(_) => "submit".to_string(),
        DialogEvent::Cancelled => "cancelled".to_string(),
    };
    let busy = dialog_events(cx, build, name, |vcx, dialog| {
        dialog.update(vcx, |d, cx| d.begin(cx));
        vcx.simulate_keystrokes("escape");
    });
    assert!(busy.is_empty());
    let enter = dialog_events(cx, build, name, |vcx, dialog| {
        dialog.update_in(vcx, |d, w, cx| d.set_text("hello", w, cx));
        vcx.simulate_keystrokes("enter");
    });
    assert!(enter.is_empty());
}

#[gpui::test]
fn only_the_preset_picker_closes_when_the_backdrop_is_clicked(cx: &mut TestAppContext) {
    use ui::preset_picker_dialog::{DialogEvent, PresetPickerDialog};
    let events = dialog_events(
        cx,
        |w, cx| PresetPickerDialog::new(Vec::new(), w, cx),
        |e: &DialogEvent| match e {
            DialogEvent::Cancelled => "cancelled".to_string(),
            _ => "other".to_string(),
        },
        |vcx, _| click_outside(vcx),
    );
    assert_eq!(events, ["cancelled"]);

    use ui::sign_in_dialog::{SignInDialog, SignInPurpose};
    let events = dialog_events(
        cx,
        |w, cx| SignInDialog::new(SignInPurpose::Upload, w, cx),
        sign_in_event,
        |vcx, _| click_outside(vcx),
    );
    assert!(events.is_empty());
}
