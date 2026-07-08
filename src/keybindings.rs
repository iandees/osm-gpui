//! Canonical table of user-customizable keyboard shortcuts, and helpers to
//! resolve a shortcut's effective (possibly user-overridden) key spec.
//!
//! The ten entries here split into two groups, both driven by the same
//! `AppSettings.keybindings` override map, but consumed differently:
//! - The seven non-`Modes` entries correspond to gpui `Action` structs
//!   defined in the binary crate (`src/main.rs`) and are turned into real
//!   `gpui::KeyBinding`s there (this module can't reference those `Action`
//!   types — they live in a different crate).
//! - The three `Modes` entries are unmodified single letters, matched
//!   directly against `KeyDownEvent.keystroke.key` in the map canvas's
//!   `on_key_down` handler (`src/main.rs`), never registered as
//!   `KeyBinding`s (an unmodified-letter `KeyBinding` would fire even while
//!   a text input elsewhere in the app has focus).

use crate::settings_store::AppSettings;

/// Grouping used to organize the Settings > Keyboard Shortcuts page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutCategory {
    General,
    File,
    Edit,
    Modes,
}

/// One customizable shortcut: a stable id (used as the
/// `AppSettings.keybindings` key), its built-in default keystroke spec, a
/// human-readable label, and which category it's grouped under in the
/// Settings UI.
#[derive(Clone, Copy, Debug)]
pub struct ShortcutDef {
    pub id: &'static str,
    pub default_spec: &'static str,
    pub label: &'static str,
    pub category: ShortcutCategory,
}

/// The full set of customizable shortcuts, in the order they should be
/// displayed within their category.
pub const SHORTCUTS: &[ShortcutDef] = &[
    ShortcutDef {
        id: "open_settings",
        default_spec: "cmd-,",
        label: "Settings…",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "quit",
        default_spec: "cmd-q",
        label: "Quit",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "open_osm_file",
        default_spec: "cmd-o",
        label: "Open…",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "download_osm",
        default_spec: "cmd-shift-d",
        label: "Download from OSM",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "upload_osm",
        default_spec: "cmd-shift-u",
        label: "Upload to OSM…",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "undo",
        default_spec: "cmd-z",
        label: "Undo",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "redo",
        default_spec: "cmd-shift-z",
        label: "Redo",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "mode_add",
        default_spec: "a",
        label: "Switch to Add mode",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_building",
        default_spec: "b",
        label: "Switch to Building mode",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_extrude",
        default_spec: "x",
        label: "Switch to Extrude mode",
        category: ShortcutCategory::Modes,
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

/// True if `spec`'s key portion (ignoring any modifier prefix) is reserved
/// and can't be assigned to a customizable shortcut.
pub fn is_reserved(spec: &str) -> bool {
    let key = spec.rsplit('-').next().unwrap_or(spec);
    RESERVED_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key))
}

/// True if `spec` has no modifier prefix (e.g. `"a"`, not `"cmd-a"`).
/// `Modes`-category shortcuts must satisfy this, since they're matched as
/// bare unmodified keys (see the module doc comment).
pub fn is_bare_key(spec: &str) -> bool {
    !spec.contains('-')
}

/// If `spec` is already used by a *different* shortcut's effective
/// binding, return that shortcut's label. Used to block duplicate
/// assignments when recording a new shortcut.
pub fn conflict(settings: &AppSettings, id: &str, spec: &str) -> Option<&'static str> {
    effective_bindings(settings)
        .into_iter()
        .find(|(other_id, other_spec)| *other_id != id && other_spec == spec)
        .map(|(other_id, _)| def(other_id).label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings_with(overrides: &[(&str, &str)]) -> AppSettings {
        let mut s = AppSettings::default();
        s.keybindings = overrides
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
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
    fn bare_key_check() {
        assert!(is_bare_key("a"));
        assert!(!is_bare_key("cmd-a"));
        assert!(!is_bare_key("shift-a"));
    }

    #[test]
    fn conflict_detects_duplicate_and_ignores_self() {
        let s = AppSettings::default();
        // "cmd-z" is undo's default spec; assigning it to redo conflicts.
        assert_eq!(conflict(&s, "redo", "cmd-z"), Some("Undo"));
        // Assigning undo's own current spec to undo itself is not a conflict.
        assert_eq!(conflict(&s, "undo", "cmd-z"), None);
    }

    #[test]
    fn conflict_none_when_spec_unused() {
        let s = AppSettings::default();
        assert_eq!(conflict(&s, "undo", "cmd-alt-shift-z"), None);
    }
}
