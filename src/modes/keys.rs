//! Key handling for the map area: a pure classifier from a keystroke to what
//! it means, and the `MapViewer` methods that act on each meaning.

use gpui::{Context, Modifiers, Window};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::settings_store::AppSettings;

use crate::{selection_actions, MapViewer, SetMode};

/// What a key press on the map means, decided before anything is changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MapKey {
    /// Typing in the imagery offset panel's fields, or a modifier chord
    /// (those belong to the keymap, never to the bare-key handling).
    Ignored,
    /// Held Space turns a left-drag into a pan.
    Space,
    Escape,
    /// Enter while a Rotate is in progress: keep the rotation.
    FinishRotate,
    /// Enter in Add mode: finish the way being drawn.
    FinishDrawing,
    DeleteSelection,
    /// A bare key bound to an edit mode.
    Mode(EditMode),
    /// A bare key bound to a selection action (by action id).
    SelectionAction(&'static str),
    Other,
}

impl MapKey {
    /// `offset_field_focused`, `rotating` and `mode` are the only state the
    /// meaning of a key depends on.
    pub(crate) fn classify(
        key: &str,
        modifiers: Modifiers,
        offset_field_focused: bool,
        rotating: bool,
        mode: EditMode,
        settings: &AppSettings,
    ) -> MapKey {
        if offset_field_focused || modifiers.platform || modifiers.control || modifiers.alt {
            return MapKey::Ignored;
        }
        match key {
            "space" => MapKey::Space,
            "escape" => MapKey::Escape,
            "enter" if rotating => MapKey::FinishRotate,
            "enter" if mode == EditMode::Add => MapKey::FinishDrawing,
            "delete" | "backspace" => MapKey::DeleteSelection,
            _ => Self::bound_key(key, settings),
        }
    }

    /// Mode-switch and selection-action shortcuts are handled here rather
    /// than as global key bindings so they only fire while the map area has
    /// focus (see the comment by `cx.bind_keys` in `main()`).
    fn bound_key(key: &str, settings: &AppSettings) -> MapKey {
        let key_mode = EditMode::ALL
            .into_iter()
            .find(|m| key == osm_gpui::keybindings::effective_spec(settings, m.shortcut_id()));
        if let Some(mode) = key_mode {
            MapKey::Mode(mode)
        } else if let Some(action) = selection_actions::find_by_key(settings, key) {
            MapKey::SelectionAction(action.id)
        } else {
            MapKey::Other
        }
    }
}

impl MapViewer {
    pub(crate) fn on_map_key_down(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = ev.keystroke.key.as_str();
        let modifiers = ev.keystroke.modifiers;
        let meaning = MapKey::classify(
            key,
            modifiers,
            self.offset_field_focused(window, cx),
            self.pointer.rotate_session.is_some(),
            self.mode(),
            &osm_gpui::settings_store::snapshot(),
        );
        match meaning {
            MapKey::Ignored => {}
            MapKey::Space => {
                if !self.pointer.space_held {
                    self.pointer.space_held = true;
                    cx.notify();
                }
            }
            meaning => {
                // Adjusting imagery offset: arrows nudge the imagery, Escape
                // cancels, Enter keeps. Otherwise arrow keys pan, +/- zoom.
                if self.offset_adjust_key(key, modifiers.shift, window, cx)
                    || self.handle_navigation_key(key, cx)
                {
                    return;
                }
                self.run_map_key(meaning, window, cx);
            }
        }
    }

    fn run_map_key(&mut self, meaning: MapKey, window: &mut Window, cx: &mut Context<Self>) {
        match meaning {
            MapKey::Escape => self.escape(cx),
            MapKey::FinishRotate => {
                self.finish_rotate();
                cx.notify();
            }
            MapKey::FinishDrawing => {
                if self.take_add_progress().is_some() {
                    cx.notify();
                }
            }
            MapKey::DeleteSelection => self.delete_selected_features(cx),
            MapKey::Mode(mode) => self.on_set_mode(&SetMode { mode }, window, cx),
            MapKey::SelectionAction(id) => self.run_selection_action(id, cx),
            MapKey::Ignored | MapKey::Space | MapKey::Other => {}
        }
    }

    /// Escape backs out one thing at a time: a full-screen photo, a Rotate, a
    /// New Note waiting for its click, then the current mode's gesture and
    /// finally the mode itself.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) {
        if self.close_photo_fullscreen() || self.cancel_rotate() || self.cancel_armed_note() {
            cx.notify();
            return;
        }
        let cancelled_drag = self.cancel_move_drag(cx);
        self.mode_escape(cancelled_drag);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(
        key: &str,
        modifiers: Modifiers,
        focused: bool,
        rotating: bool,
        mode: EditMode,
    ) -> MapKey {
        MapKey::classify(
            key,
            modifiers,
            focused,
            rotating,
            mode,
            &AppSettings::default(),
        )
    }

    fn bare(key: &str) -> MapKey {
        classify(key, Modifiers::default(), false, false, EditMode::Select)
    }

    #[test]
    fn a_focused_offset_field_swallows_every_key() {
        for key in ["escape", "enter", "space", "delete", "a"] {
            let got = classify(key, Modifiers::default(), true, true, EditMode::Add);
            assert_eq!(got, MapKey::Ignored, "{key}");
        }
    }

    #[test]
    fn modifier_chords_belong_to_the_keymap() {
        let chords = [
            Modifiers::command(),
            Modifiers::control(),
            Modifiers::secondary_key(),
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ];
        for m in chords {
            for key in ["escape", "space", "delete", "enter"] {
                assert_eq!(
                    classify(key, m, false, true, EditMode::Add),
                    MapKey::Ignored
                );
            }
        }
    }

    #[test]
    fn shift_alone_does_not_make_a_chord() {
        assert_eq!(
            classify("escape", Modifiers::shift(), false, false, EditMode::Select),
            MapKey::Escape
        );
    }

    #[test]
    fn space_and_escape_are_recognised() {
        assert_eq!(bare("space"), MapKey::Space);
        assert_eq!(bare("escape"), MapKey::Escape);
    }

    #[test]
    fn enter_finishes_a_rotate_before_a_drawing() {
        let got = classify("enter", Modifiers::default(), false, true, EditMode::Add);
        assert_eq!(got, MapKey::FinishRotate);
    }

    #[test]
    fn enter_finishes_a_drawing_only_in_add_mode() {
        let add = classify("enter", Modifiers::default(), false, false, EditMode::Add);
        assert_eq!(add, MapKey::FinishDrawing);
        for mode in EditMode::ALL.into_iter().filter(|m| *m != EditMode::Add) {
            let got = classify("enter", Modifiers::default(), false, false, mode);
            assert_eq!(got, MapKey::Other, "{mode:?}");
        }
    }

    #[test]
    fn delete_and_backspace_delete_the_selection() {
        assert_eq!(bare("delete"), MapKey::DeleteSelection);
        assert_eq!(bare("backspace"), MapKey::DeleteSelection);
    }

    #[test]
    fn every_modes_bound_key_selects_that_mode() {
        let settings = AppSettings::default();
        for mode in EditMode::ALL {
            let spec = osm_gpui::keybindings::effective_spec(&settings, mode.shortcut_id());
            assert_eq!(bare(&spec), MapKey::Mode(mode), "{mode:?}");
        }
    }

    #[test]
    fn a_rebound_mode_key_follows_the_settings() {
        let mut settings = AppSettings::default();
        settings
            .keybindings
            .insert(EditMode::Arc.shortcut_id().to_string(), "j".to_string());
        let got = MapKey::classify(
            "j",
            Modifiers::default(),
            false,
            false,
            EditMode::Select,
            &settings,
        );
        assert_eq!(got, MapKey::Mode(EditMode::Arc));
    }

    #[test]
    fn a_selection_action_key_names_its_action() {
        let settings = AppSettings::default();
        let action = selection_actions::ACTIONS
            .iter()
            .find(|a| {
                a.shortcut_id.is_some_and(|sid| {
                    osm_gpui::keybindings::def(sid)
                        .category
                        .is_bare_key_category()
                })
            })
            .expect("a bare-key action");
        let spec = osm_gpui::keybindings::effective_spec(&settings, action.shortcut_id.unwrap());
        let got = bare(&spec);
        assert!(
            matches!(got, MapKey::SelectionAction(_) | MapKey::Mode(_)),
            "{spec}: {got:?}"
        );
    }

    #[test]
    fn an_unbound_key_is_other() {
        assert_eq!(bare("f13"), MapKey::Other);
    }
}
