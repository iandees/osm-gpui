//! The map-interaction modes and the static facts about each one.
//!
//! `Select` is the ordinary click/drag/box-select behaviour; the others place
//! new geometry. Everything that is a fixed property of a mode (its script
//! keyword, shortcut id, locale keys, icon, whether it needs a layer) lives
//! here so a new mode is one variant plus one arm per method.

use gpui::SharedString;
use gpui_component::IconName;
use schemars::JsonSchema;
use serde::Deserialize;

/// The variant names are the serialized form used by `SetMode` in keymaps and
/// scripts (`{"mode":"Add"}`), so renaming one breaks existing files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
    Arc,
    Improve,
}

impl EditMode {
    /// Every mode in toolbar and menu order.
    pub const ALL: [EditMode; 6] = [
        EditMode::Select,
        EditMode::Add,
        EditMode::Building,
        EditMode::Extrude,
        EditMode::Arc,
        EditMode::Improve,
    ];

    /// Stable lowercase name: the script keyword, and the infix of the
    /// `mode.<key>.*` locale keys. The shortcut id is separate because it is
    /// registered in `keybindings::SHORTCUTS`.
    pub fn key(self) -> &'static str {
        match self {
            EditMode::Select => "select",
            EditMode::Add => "add",
            EditMode::Building => "building",
            EditMode::Extrude => "extrude",
            EditMode::Arc => "arc",
            EditMode::Improve => "improve",
        }
    }

    pub fn from_key(key: &str) -> Option<EditMode> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }

    /// The `keybindings::SHORTCUTS` id of the bare key that switches here.
    pub fn shortcut_id(self) -> &'static str {
        match self {
            EditMode::Select => "mode_select",
            EditMode::Add => "mode_add",
            EditMode::Building => "mode_building",
            EditMode::Extrude => "mode_extrude",
            EditMode::Arc => "mode_arc",
            EditMode::Improve => "mode_improve",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            EditMode::Select => IconName::Frame,
            EditMode::Add => IconName::Plus,
            EditMode::Building => IconName::Building2,
            EditMode::Extrude => IconName::LayoutDashboard,
            EditMode::Arc => IconName::Redo2,
            EditMode::Improve => IconName::Replace,
        }
    }

    /// Whether the mode writes new geometry and so needs an active editable
    /// layer. Select only inspects and moves existing features.
    pub fn needs_active_layer(self) -> bool {
        match self {
            EditMode::Select => false,
            EditMode::Add
            | EditMode::Building
            | EditMode::Extrude
            | EditMode::Arc
            | EditMode::Improve => true,
        }
    }

    /// Short toolbar label.
    pub fn label(self) -> SharedString {
        self.tr_part("label")
    }

    /// Full name, as shown in the Mode menu.
    pub fn name(self) -> SharedString {
        self.tr_part("name")
    }

    /// Unabbreviated hover text for the toolbar button.
    pub fn tooltip(self) -> SharedString {
        self.tr_part("tooltip")
    }

    /// Locale keys of every message this mode uses.
    pub fn locale_keys(self) -> [String; 3] {
        ["label", "name", "tooltip"].map(|part| format!("mode.{}.{part}", self.key()))
    }

    fn tr_part(self, part: &str) -> SharedString {
        crate::i18n::tr_dynamic(&format!("mode.{}.{part}", self.key()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trips() {
        for mode in EditMode::ALL {
            assert_eq!(EditMode::from_key(mode.key()), Some(mode));
        }
        assert_eq!(EditMode::from_key("nope"), None);
    }

    #[test]
    fn keys_and_shortcut_ids_are_unique() {
        for (i, a) in EditMode::ALL.iter().enumerate() {
            for b in &EditMode::ALL[i + 1..] {
                assert_ne!(a.key(), b.key());
                assert_ne!(a.shortcut_id(), b.shortcut_id());
            }
        }
    }

    #[test]
    fn every_modes_shortcut_belongs_to_a_mode() {
        let ids: Vec<&str> = EditMode::ALL.iter().map(|m| m.shortcut_id()).collect();
        for def in crate::keybindings::SHORTCUTS {
            let is_mode_shortcut = def.category == crate::keybindings::ShortcutCategory::Modes;
            assert_eq!(ids.contains(&def.id), is_mode_shortcut, "{}", def.id);
        }
    }

    #[test]
    fn every_mode_has_a_registered_modes_shortcut() {
        for mode in EditMode::ALL {
            // `def` panics on an unknown id.
            let def = crate::keybindings::def(mode.shortcut_id());
            assert_eq!(def.category, crate::keybindings::ShortcutCategory::Modes);
        }
    }

    #[test]
    fn every_mode_has_locale_messages_in_every_language() {
        for locale in crate::i18n::available_locales() {
            for mode in EditMode::ALL {
                for key in mode.locale_keys() {
                    assert!(
                        crate::i18n::lookup_in(&locale, &key).is_some(),
                        "{locale}: missing locale key {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn select_is_the_only_mode_that_works_without_a_layer() {
        for mode in EditMode::ALL {
            assert_eq!(mode.needs_active_layer(), mode != EditMode::Select);
        }
    }

    #[test]
    fn action_shape_is_the_variant_name() {
        let mode: EditMode = serde_json::from_str("\"Improve\"").unwrap();
        assert_eq!(mode, EditMode::Improve);
    }
}
