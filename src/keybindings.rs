//! Canonical table of user-customizable keyboard shortcuts, and helpers to
//! resolve a shortcut's effective (possibly user-overridden) key spec.
//!
//! The entries here split into two groups, both driven by the same
//! `AppSettings.keybindings` override map, but consumed differently:
//! - The non-`Modes` entries correspond to gpui `Action` structs
//!   defined in the binary crate (`src/main.rs`) and are turned into real
//!   `gpui::KeyBinding`s there (this module can't reference those `Action`
//!   types — they live in a different crate).
//! - The `Modes` and `Actions` entries are unmodified single letters, matched
//!   directly against `KeyDownEvent.keystroke.key` in the map canvas's
//!   `on_key_down` handler (`src/main.rs`), never registered as
//!   `KeyBinding`s (an unmodified-letter `KeyBinding` would fire even while
//!   a text input elsewhere in the app has focus).

use crate::settings_store::AppSettings;
use gpui::SharedString;

/// Grouping used to organize the Settings > Keyboard Shortcuts page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutCategory {
    General,
    File,
    Edit,
    View,
    Modes,
    /// One-shot operations on the selection (square, circularize, ...).
    /// Like `Modes`, these are bare unmodified keys.
    Actions,
}

impl ShortcutCategory {
    /// Whether shortcuts in this category are matched as bare, unmodified
    /// keys in the map canvas's key handler (and so can't take modifiers).
    pub fn is_bare_key_category(self) -> bool {
        matches!(self, ShortcutCategory::Modes | ShortcutCategory::Actions)
    }
}

/// One customizable shortcut: a stable id (used as the
/// `AppSettings.keybindings` key), its built-in default keystroke spec, and
/// which category it's grouped under in the Settings UI. Its display name is
/// looked up by id in the message catalog (`shortcut.<id>`, see [`label`]);
/// the id itself never changes with the language.
///
/// [`label`]: ShortcutDef::label
#[derive(Clone, Copy, Debug)]
pub struct ShortcutDef {
    pub id: &'static str,
    pub default_spec: &'static str,
    pub category: ShortcutCategory,
}

impl ShortcutDef {
    /// Localized display name, e.g. "Switch to Select mode".
    pub fn label(&self) -> SharedString {
        crate::i18n::tr_dynamic(&format!("shortcut.{}", self.id))
    }
}

impl ShortcutCategory {
    /// Localized heading of the Settings group for this category.
    pub fn title(self) -> SharedString {
        match self {
            Self::General => crate::tr!("shortcut_category.general"),
            Self::File => crate::tr!("shortcut_category.file"),
            Self::Edit => crate::tr!("shortcut_category.edit"),
            Self::View => crate::tr!("shortcut_category.view"),
            Self::Modes => crate::tr!("shortcut_category.modes"),
            Self::Actions => crate::tr!("shortcut_category.actions"),
        }
    }
}

/// The full set of customizable shortcuts, in the order they should be
/// displayed within their category.
pub const SHORTCUTS: &[ShortcutDef] = &[
    ShortcutDef {
        id: "open_settings",
        default_spec: "cmd-,",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "quit",
        default_spec: "cmd-q",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "open_osm_file",
        default_spec: "cmd-o",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "download_osm",
        default_spec: "cmd-shift-d",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "upload_osm",
        default_spec: "cmd-shift-u",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "export_osm_file",
        default_spec: "cmd-e",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "undo",
        default_spec: "cmd-z",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "redo",
        default_spec: "cmd-shift-z",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "select_all",
        default_spec: "cmd-a",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "zoom_in",
        default_spec: "cmd-=",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_out",
        default_spec: "cmd--",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_to_data",
        default_spec: "cmd-0",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_to_selection",
        default_spec: "cmd-j",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "mode_select",
        default_spec: "s",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_add",
        default_spec: "a",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_building",
        default_spec: "b",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_extrude",
        default_spec: "x",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "action_square",
        default_spec: "q",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_circularize",
        default_spec: "o",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_reverse",
        default_spec: "r",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_join",
        default_spec: "c",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_separate",
        default_spec: "g",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_extend",
        default_spec: "e",
        category: ShortcutCategory::Actions,
    },
];

/// Look up a shortcut's definition by id. Panics if `id` isn't in
/// `SHORTCUTS` — a programmer error, since callers only ever pass ids
/// sourced from `SHORTCUTS` itself.
pub fn def(id: &str) -> &'static ShortcutDef {
    SHORTCUTS
        .iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("unknown shortcut id: {id}"))
}

/// The effective keystroke spec for `id`: the user's override if present in
/// `settings.keybindings`, else the built-in default.
pub fn effective_spec(settings: &AppSettings, id: &str) -> String {
    settings
        .keybindings
        .get(id)
        .cloned()
        .unwrap_or_else(|| def(id).default_spec.to_string())
}

/// All shortcuts' effective `(id, spec)` pairs, in `SHORTCUTS` order.
pub fn effective_bindings(settings: &AppSettings) -> Vec<(&'static str, String)> {
    SHORTCUTS
        .iter()
        .map(|d| (d.id, effective_spec(settings, d.id)))
        .collect()
}

/// Keys that can never be assigned to a customizable shortcut, because
/// they're hardcoded interaction primitives elsewhere (cancel, confirm,
/// delete-selection) — see the design doc's "Out of scope" section.
const RESERVED_KEYS: &[&str] = &["escape", "enter", "return", "delete", "backspace"];

/// Keys that are reserved only when unmodified: the map's hardcoded
/// navigation keys (hold Space to pan-drag, arrows pan, `+`/`=`/`-` zoom).
/// With a modifier they're fair game — `cmd-=` is Zoom In's default.
const RESERVED_BARE_KEYS: &[&str] = &["space", "left", "right", "up", "down", "=", "+", "-"];

/// True if `spec`'s key portion (ignoring any modifier prefix) is reserved
/// and can't be assigned to a customizable shortcut.
pub fn is_reserved(spec: &str) -> bool {
    let key = spec.rsplit('-').next().unwrap_or(spec);
    RESERVED_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key))
        || RESERVED_BARE_KEYS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(spec))
}

/// True if `spec` has no modifier prefix (e.g. `"a"`, not `"cmd-a"`).
/// Bare-key categories (`Modes`, `Actions`) must satisfy this, since they're
/// matched as bare unmodified keys (see the module doc comment).
pub fn is_bare_key(spec: &str) -> bool {
    !spec.contains('-')
}

/// If `spec` is already used by a *different* shortcut's effective
/// binding, return that shortcut's label. Used to block duplicate
/// assignments when recording a new shortcut.
pub fn conflict(settings: &AppSettings, id: &str, spec: &str) -> Option<SharedString> {
    effective_bindings(settings)
        .into_iter()
        .find(|(other_id, other_spec)| *other_id != id && other_spec == spec)
        .map(|(other_id, _)| def(other_id).label())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(overrides: &[(&str, &str)]) -> AppSettings {
        AppSettings {
            keybindings: overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn every_shortcut_and_category_has_a_display_name() {
        // A missing key renders as "<locale>.<key>".
        for d in SHORTCUTS {
            let label = d.label();
            assert!(!label.starts_with("en."), "{}: {label}", d.id);
        }
        for category in [
            ShortcutCategory::General,
            ShortcutCategory::File,
            ShortcutCategory::Edit,
            ShortcutCategory::View,
            ShortcutCategory::Modes,
            ShortcutCategory::Actions,
        ] {
            assert!(!category.title().starts_with("en."));
        }
        assert_eq!(def("undo").label(), "Undo");
        assert_eq!(ShortcutCategory::Modes.title(), "Modes");
    }

    #[test]
    fn effective_spec_falls_back_to_default() {
        let s = AppSettings::default();
        assert_eq!(effective_spec(&s, "undo"), "cmd-z");
    }

    #[test]
    fn effective_spec_uses_override() {
        let s = settings_with(&[("undo", "cmd-alt-z")]);
        assert_eq!(effective_spec(&s, "undo"), "cmd-alt-z");
    }

    #[test]
    fn effective_bindings_covers_all_ids_in_order() {
        let s = AppSettings::default();
        let ids: Vec<_> = effective_bindings(&s)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let expected: Vec<_> = SHORTCUTS.iter().map(|d| d.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn no_two_defaults_collide() {
        for (i, a) in SHORTCUTS.iter().enumerate() {
            for b in &SHORTCUTS[i + 1..] {
                assert_ne!(
                    a.default_spec, b.default_spec,
                    "{} and {} share a default spec",
                    a.id, b.id
                );
            }
        }
    }

    #[test]
    fn reserved_keys_are_rejected() {
        assert!(is_reserved("escape"));
        assert!(is_reserved("cmd-delete"));
        assert!(is_reserved("backspace"));
        assert!(!is_reserved("cmd-o"));
    }

    #[test]
    fn navigation_keys_are_reserved_only_when_bare() {
        assert!(is_reserved("space"));
        assert!(is_reserved("left"));
        assert!(is_reserved("="));
        assert!(is_reserved("-"));
        assert!(!is_reserved("cmd-="));
        assert!(!is_reserved("cmd--"));
        assert!(!is_reserved("cmd-left"));
    }

    #[test]
    fn no_default_is_reserved() {
        for d in SHORTCUTS {
            assert!(!is_reserved(d.default_spec), "{} is reserved", d.id);
        }
    }

    #[test]
    fn bare_key_check() {
        assert!(is_bare_key("a"));
        assert!(!is_bare_key("cmd-a"));
        assert!(!is_bare_key("shift-a"));
    }

    #[test]
    fn conflict_detects_duplicate_and_ignores_self() {
        let s = AppSettings::default();
        // "cmd-z" is undo's default spec; assigning it to redo conflicts.
        assert_eq!(conflict(&s, "redo", "cmd-z").as_deref(), Some("Undo"));
        // Assigning undo's own current spec to undo itself is not a conflict.
        assert_eq!(conflict(&s, "undo", "cmd-z"), None);
    }

    #[test]
    fn conflict_none_when_spec_unused() {
        let s = AppSettings::default();
        assert_eq!(conflict(&s, "undo", "cmd-alt-shift-z"), None);
    }

    /// Mirrors `settings_window::reset_shortcut`'s pre-removal guard: resetting
    /// `mode_add` back to its default ("a") must be flagged as a conflict once
    /// `mode_building` has been rebound onto "a", even though `mode_building`'s
    /// rebind was legal at the time it was made (mode_add had already vacated
    /// "a" by moving to "c").
    #[test]
    fn reset_would_collide_with_another_shortcuts_current_binding() {
        let s = settings_with(&[("mode_add", "c"), ("mode_building", "a")]);

        // What reset_shortcut would compute as the post-removal candidate spec.
        let candidate = def("mode_add").default_spec;
        assert_eq!(candidate, "a");

        // The guard must detect that "a" is already mode_building's spec.
        assert_eq!(
            conflict(&s, "mode_add", candidate).as_deref(),
            Some("Switch to Building mode")
        );
    }
}
