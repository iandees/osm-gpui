//! Light/dark appearance: which gpui-component theme is in effect and how it
//! follows the saved preference, the `--theme` flag and the OS appearance.

use crate::settings_store::{self, ThemePreference};
use gpui::{App, WindowAppearance};
use gpui_component::{Theme, ThemeMode};
use std::sync::Mutex;

/// Theme forced for this run only (`--theme`, or the script harness's
/// `SetThemeDark`/`SetThemeLight`). Kept out of the settings store so it can
/// never reach settings.json.
static RUN_OVERRIDE: Mutex<Option<ThemeMode>> = Mutex::new(None);

pub fn set_run_override(mode: Option<ThemeMode>) {
    *RUN_OVERRIDE.lock().unwrap() = mode;
}

pub fn run_override() -> Option<ThemeMode> {
    *RUN_OVERRIDE.lock().unwrap()
}

/// The effective light/dark mode: a per-run override wins, then an explicit
/// Light/Dark preference, and System follows `system`.
pub fn resolve(
    preference: ThemePreference,
    over: Option<ThemeMode>,
    system: ThemeMode,
) -> ThemeMode {
    if let Some(mode) = over {
        return mode;
    }
    match preference {
        ThemePreference::System => system,
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::Dark => ThemeMode::Dark,
    }
}

/// The OS appearance, preferably as seen by an open window (more reliable
/// than the app-level query on Linux).
fn system_mode(cx: &mut App) -> ThemeMode {
    let from_window = cx
        .windows()
        .first()
        .and_then(|w| w.update(cx, |_, window, _| window.appearance()).ok());
    let appearance: WindowAppearance = from_window.unwrap_or_else(|| cx.window_appearance());
    ThemeMode::from(appearance)
}

/// Apply the effective theme given the OS appearance `system`, and repaint
/// every window.
pub fn apply_with_system(system: ThemeMode, cx: &mut App) {
    let mode = resolve(settings_store::snapshot().theme, run_override(), system);
    Theme::change(mode, None, cx);
    Theme::sync_scrollbar_appearance(cx);
    cx.refresh_windows();
}

/// Apply the effective theme using the current OS appearance.
pub fn apply(cx: &mut App) {
    let system = system_mode(cx);
    apply_with_system(system, cx);
}

/// Force `mode` for this run only (nothing is saved) and repaint.
pub fn apply_run_override(mode: ThemeMode, cx: &mut App) {
    set_run_override(Some(mode));
    apply(cx);
}

/// The user's choice: store it (on disk too when `persist`), drop any
/// per-run override so the choice shows, and repaint every window.
pub fn set_preference(preference: ThemePreference, persist: bool, cx: &mut App) {
    set_run_override(None);
    let mut settings = settings_store::snapshot();
    settings.theme = preference;
    if persist {
        settings_store::update_store(settings);
    } else {
        settings_store::update_store_in_memory(settings);
    }
    apply(cx);
}

/// Follow OS appearance changes seen by `window` while the preference is
/// System and no per-run override is set.
pub fn observe_system_appearance(window: &mut gpui::Window) {
    window
        .observe_window_appearance(|window, cx| {
            apply_with_system(ThemeMode::from(window.appearance()), cx);
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ThemeMode::{Dark, Light};

    #[test]
    fn system_preference_follows_the_os() {
        assert_eq!(resolve(ThemePreference::System, None, Light), Light);
        assert_eq!(resolve(ThemePreference::System, None, Dark), Dark);
    }

    #[test]
    fn explicit_preference_ignores_the_os() {
        assert_eq!(resolve(ThemePreference::Light, None, Dark), Light);
        assert_eq!(resolve(ThemePreference::Dark, None, Light), Dark);
    }

    #[test]
    fn cli_override_beats_everything() {
        for pref in ThemePreference::ALL {
            for sys in [Light, Dark] {
                assert_eq!(resolve(pref, Some(Dark), sys), Dark);
                assert_eq!(resolve(pref, Some(Light), sys), Light);
            }
        }
    }
}
