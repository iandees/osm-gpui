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
    /// Every category, in the order the Settings page lists them. A shortcut
    /// whose category is missing here would have no row on that page.
    pub const ALL: [ShortcutCategory; 6] = [
        Self::General,
        Self::File,
        Self::Edit,
        Self::View,
        Self::Modes,
        Self::Actions,
    ];

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
        default_spec: "secondary-,",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "quit",
        default_spec: "secondary-q",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "open_osm_file",
        default_spec: "secondary-o",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "download_osm",
        default_spec: "secondary-shift-d",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "upload_osm",
        default_spec: "secondary-shift-u",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "export_osm_file",
        default_spec: "secondary-e",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "undo",
        default_spec: "secondary-z",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "redo",
        default_spec: "secondary-shift-z",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "select_all",
        default_spec: "secondary-a",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "find",
        default_spec: "secondary-f",
        category: ShortcutCategory::Edit,
    },
    // Not secondary-h: that is Hide on macOS.
    ShortcutDef {
        id: "show_history",
        default_spec: "secondary-shift-h",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "copy_features",
        default_spec: "secondary-c",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "paste_features",
        default_spec: "secondary-v",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "duplicate_features",
        default_spec: "secondary-d",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "copy_tags",
        default_spec: "secondary-shift-c",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "paste_tags",
        default_spec: "secondary-shift-v",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "zoom_in",
        default_spec: "secondary-=",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_out",
        default_spec: "secondary--",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_to_data",
        default_spec: "secondary-0",
        category: ShortcutCategory::View,
    },
    ShortcutDef {
        id: "zoom_to_selection",
        default_spec: "secondary-j",
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
        id: "mode_arc",
        default_spec: "v",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_improve",
        default_spec: "i",
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
    ShortcutDef {
        id: "action_split",
        default_spec: "p",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_merge",
        default_spec: "m",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_straighten",
        default_spec: "t",
        category: ShortcutCategory::Actions,
    },
    ShortcutDef {
        id: "action_align",
        default_spec: "l",
        category: ShortcutCategory::Actions,
    },
    // Not r: that is Reverse Way.
    ShortcutDef {
        id: "action_rotate",
        default_spec: "w",
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
/// delete-selection).
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

/// Why a recorded spec cannot become `id`'s shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The key is a hardcoded interaction primitive (see [`is_reserved`]).
    Reserved,
    /// A bare-key category (`Modes`, `Actions`) only takes unmodified keys.
    NeedsBareKey,
    /// Another shortcut already uses the spec; carries that shortcut's label.
    InUse(SharedString),
}

/// Whether `spec` may be saved as `id`'s shortcut. The checks run in the
/// order reserved, bare-key category, conflict, so that is also which
/// rejection wins when several apply.
pub fn validate(settings: &AppSettings, id: &str, spec: &str) -> Result<(), Rejection> {
    if is_reserved(spec) {
        return Err(Rejection::Reserved);
    }
    if def(id).category.is_bare_key_category() && !is_bare_key(spec) {
        return Err(Rejection::NeedsBareKey);
    }
    match conflict(settings, id, spec) {
        Some(other) => Err(Rejection::InUse(other)),
        None => Ok(()),
    }
}

/// Whether `id`'s override can be dropped. Its default may since have been
/// taken by another shortcut's rebind, which would leave two on one spec.
pub fn validate_reset(settings: &AppSettings, id: &str) -> Result<(), Rejection> {
    match conflict(settings, id, def(id).default_spec) {
        Some(other) => Err(Rejection::InUse(other)),
        None => Ok(()),
    }
}

/// Drop every override, so each shortcut in [`SHORTCUTS`] is back on its
/// default. The defaults are distinct (see the `no_two_defaults_collide`
/// test), so no per-shortcut conflict check is needed.
pub fn reset_all(settings: &mut AppSettings) {
    settings.keybindings.clear();
}

/// What a key press means while a shortcut is being recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Only modifiers are held so far; keep waiting for a real key.
    ModifierOnly,
    /// Escape abandons the recording.
    Cancel,
    /// The finished spec, to be validated.
    Spec(String),
}

pub fn classify_recorded(keystroke: &gpui::Keystroke) -> Recorded {
    if keystroke.key.is_empty() {
        Recorded::ModifierOnly
    } else if keystroke.key == "escape" {
        Recorded::Cancel
    } else {
        Recorded::Spec(keystroke.unparse())
    }
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
        for category in ShortcutCategory::ALL {
            assert!(!category.title().starts_with("en."));
        }
        assert_eq!(def("undo").label(), "Undo");
        assert_eq!(ShortcutCategory::Modes.title(), "Modes");
    }

    #[test]
    fn every_shortcut_belongs_to_a_listed_category() {
        for d in SHORTCUTS {
            assert!(ShortcutCategory::ALL.contains(&d.category), "{}", d.id);
        }
        for category in ShortcutCategory::ALL {
            assert!(
                SHORTCUTS.iter().any(|d| d.category == category),
                "{category:?} has no shortcuts"
            );
        }
    }

    #[test]
    fn default_modifier_shortcuts_use_the_platform_primary_key() {
        // `cmd` is the Windows/Super key off macOS, so a `cmd-` default would
        // be unusable there; `secondary` is Cmd on macOS and Ctrl elsewhere.
        for d in SHORTCUTS {
            assert!(!d.default_spec.contains("cmd"), "{}", d.id);
            if !is_bare_key(d.default_spec) {
                assert!(d.default_spec.starts_with("secondary-"), "{}", d.id);
            }
            assert!(gpui::Keystroke::parse(d.default_spec).is_ok(), "{}", d.id);
        }
        let stroke = gpui::Keystroke::parse("secondary-z").unwrap();
        assert_eq!(stroke.modifiers.platform, cfg!(target_os = "macos"));
        assert_eq!(stroke.modifiers.control, !cfg!(target_os = "macos"));
    }

    #[test]
    fn effective_spec_falls_back_to_default() {
        let s = AppSettings::default();
        assert_eq!(effective_spec(&s, "undo"), "secondary-z");
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
        // "secondary-z" is undo's default spec; assigning it to redo conflicts.
        assert_eq!(conflict(&s, "redo", "secondary-z").as_deref(), Some("Undo"));
        // Assigning undo's own current spec to undo itself is not a conflict.
        assert_eq!(conflict(&s, "undo", "secondary-z"), None);
    }

    #[test]
    fn conflict_none_when_spec_unused() {
        let s = AppSettings::default();
        assert_eq!(conflict(&s, "undo", "cmd-alt-shift-z"), None);
    }

    /// Resetting `mode_add` back to its default ("a") must be refused once
    /// `mode_building` has been rebound onto "a", even though that rebind was
    /// legal when it was made (mode_add had already vacated "a" by moving to
    /// "c").
    #[test]
    fn reset_would_collide_with_another_shortcuts_current_binding() {
        let s = settings_with(&[("mode_add", "c"), ("mode_building", "a")]);
        assert_eq!(def("mode_add").default_spec, "a");
        assert_eq!(
            validate_reset(&s, "mode_add"),
            Err(Rejection::InUse("Switch to Building mode".into()))
        );
    }

    #[test]
    fn reset_all_restores_every_default() {
        let mut s = settings_with(&[]);
        // Override every shortcut, including ones a count of "ten" would miss.
        for d in SHORTCUTS {
            s.keybindings
                .insert(d.id.to_string(), format!("override-{}", d.id));
        }
        for d in SHORTCUTS {
            assert_ne!(effective_spec(&s, d.id), d.default_spec, "{}", d.id);
        }
        reset_all(&mut s);
        assert!(s.keybindings.is_empty());
        for d in SHORTCUTS {
            assert_eq!(effective_spec(&s, d.id), d.default_spec, "{}", d.id);
        }
    }

    #[test]
    fn reset_is_allowed_when_the_default_is_free() {
        let s = settings_with(&[("mode_add", "c")]);
        assert_eq!(validate_reset(&s, "mode_add"), Ok(()));
        assert_eq!(validate_reset(&AppSettings::default(), "undo"), Ok(()));
    }

    #[test]
    fn validate_accepts_a_free_spec_for_each_kind_of_category() {
        let s = AppSettings::default();
        assert_eq!(validate(&s, "undo", "secondary-alt-shift-z"), Ok(()));
        assert_eq!(validate(&s, "mode_add", "k"), Ok(()));
        assert_eq!(validate(&s, "action_square", "k"), Ok(()));
        // Re-recording a shortcut's own current spec is not a conflict.
        assert_eq!(validate(&s, "undo", "secondary-z"), Ok(()));
    }

    #[test]
    fn validate_rejects_reserved_keys() {
        let s = AppSettings::default();
        assert_eq!(validate(&s, "undo", "escape"), Err(Rejection::Reserved));
        assert_eq!(
            validate(&s, "undo", "secondary-backspace"),
            Err(Rejection::Reserved)
        );
        assert_eq!(validate(&s, "mode_add", "space"), Err(Rejection::Reserved));
        assert_eq!(validate(&s, "zoom_in", "="), Err(Rejection::Reserved));
        // Reserved only when bare.
        assert_eq!(validate(&s, "zoom_in", "secondary-="), Ok(()));
    }

    #[test]
    fn bare_key_categories_reject_modifiers_but_others_accept_bare_keys() {
        let s = AppSettings::default();
        assert_eq!(
            validate(&s, "mode_add", "secondary-k"),
            Err(Rejection::NeedsBareKey)
        );
        assert_eq!(
            validate(&s, "action_square", "shift-k"),
            Err(Rejection::NeedsBareKey)
        );
        // Nothing requires a modifier on the other categories.
        assert_eq!(validate(&s, "undo", "k"), Ok(()));
    }

    #[test]
    fn validate_reports_the_conflicting_shortcut() {
        let s = AppSettings::default();
        assert_eq!(
            validate(&s, "redo", "secondary-z"),
            Err(Rejection::InUse("Undo".into()))
        );
        // A rebound shortcut conflicts on its new spec, not its old one.
        let s = settings_with(&[("undo", "secondary-alt-z")]);
        assert_eq!(validate(&s, "redo", "secondary-z"), Ok(()));
        assert_eq!(
            validate(&s, "redo", "secondary-alt-z"),
            Err(Rejection::InUse("Undo".into()))
        );
    }

    #[test]
    fn the_first_failing_check_wins() {
        let s = AppSettings::default();
        // "space" is reserved and bare-key-valid; reserved is reported first.
        assert_eq!(validate(&s, "undo", "space"), Err(Rejection::Reserved));
        // Not reserved, but modifier-bearing in a bare category and also
        // unused: the category rule is what rejects it.
        assert_eq!(
            validate(&s, "mode_add", "secondary-z"),
            Err(Rejection::NeedsBareKey)
        );
        // "s" is mode_select's: a bare spec passes the category rule, then conflicts.
        assert_eq!(
            validate(&s, "mode_add", "s"),
            Err(Rejection::InUse("Switch to Select mode".into()))
        );
    }

    fn keystroke(key: &str, modifiers: gpui::Modifiers) -> gpui::Keystroke {
        gpui::Keystroke {
            modifiers,
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn recording_waits_through_modifier_only_presses() {
        let held = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            classify_recorded(&keystroke("", held)),
            Recorded::ModifierOnly
        );
    }

    #[test]
    fn recording_escape_cancels_even_with_modifiers() {
        assert_eq!(
            classify_recorded(&keystroke("escape", Default::default())),
            Recorded::Cancel
        );
        let held = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            classify_recorded(&keystroke("escape", held)),
            Recorded::Cancel
        );
    }

    #[test]
    fn recording_a_key_yields_its_spec() {
        let held = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            classify_recorded(&keystroke("k", held)),
            Recorded::Spec("shift-k".to_string())
        );
        assert_eq!(
            classify_recorded(&keystroke("k", Default::default())),
            Recorded::Spec("k".to_string())
        );
    }
}
