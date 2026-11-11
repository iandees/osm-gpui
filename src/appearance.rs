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

/// The appearance to force on the native app (title bars, panels, menus):
/// `Some` when the theme is pinned by an override or an explicit preference,
/// `None` when it should follow the OS (System).
pub fn native_override(preference: ThemePreference, over: Option<ThemeMode>) -> Option<ThemeMode> {
    over.or(match preference {
        ThemePreference::System => None,
        ThemePreference::Light => Some(ThemeMode::Light),
        ThemePreference::Dark => Some(ThemeMode::Dark),
    })
}

/// What the native app appearance was last read back as after `apply` set it
/// (`None` = following the system). Written on the main thread, read by the
/// script harness's `assert_native_appearance`.
static NATIVE_READBACK: Mutex<Option<ThemeMode>> = Mutex::new(None);

pub fn native_readback() -> Option<ThemeMode> {
    *NATIVE_READBACK.lock().unwrap()
}

// objc 0.2's macros expand to a `cfg(feature = "cargo-clippy")` check.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod native {
    use gpui_component::ThemeMode;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    const DARK: &str = "NSAppearanceNameDarkAqua";
    const LIGHT: &str = "NSAppearanceNameAqua";

    /// Set `NSApp.appearance` (nil for `None`) and read it back. Main thread only.
    pub fn set(mode: Option<ThemeMode>) -> Option<ThemeMode> {
        // SAFETY: plain AppKit messages on the main thread; every pointer is
        // checked for nil before it is messaged.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return None;
            }
            let appearance: *mut Object = match mode {
                None => std::ptr::null_mut(),
                Some(mode) => {
                    let name =
                        CString::new(if mode == ThemeMode::Dark { DARK } else { LIGHT }).unwrap();
                    let ns: *mut Object =
                        msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
                    msg_send![class!(NSAppearance), appearanceNamed: ns]
                }
            };
            let _: () = msg_send![app, setAppearance: appearance];
            let current: *mut Object = msg_send![app, appearance];
            if current.is_null() {
                return None;
            }
            let name: *mut Object = msg_send![current, name];
            if name.is_null() {
                return None;
            }
            let utf8: *const c_char = msg_send![name, UTF8String];
            if utf8.is_null() {
                return None;
            }
            match CStr::from_ptr(utf8).to_str() {
                Ok(DARK) => Some(ThemeMode::Dark),
                Ok(LIGHT) => Some(ThemeMode::Light),
                _ => None,
            }
        }
    }
}

/// Force (or, with `None`, stop forcing) the native app appearance so the
/// window title bar and other system chrome match the theme. A no-op on
/// Windows and Linux.
fn set_native_appearance(mode: Option<ThemeMode>) {
    #[cfg(target_os = "macos")]
    let readback = native::set(mode);
    #[cfg(not(target_os = "macos"))]
    let readback = mode;
    *NATIVE_READBACK.lock().unwrap() = readback;
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

/// Apply the effective theme and the matching native appearance.
///
/// The native appearance is set first: forcing it to nil in System mode makes
/// the OS appearance read below the real one, rather than whatever we forced
/// earlier. When a mode is forced the OS value is not needed at all.
pub fn apply(cx: &mut App) {
    let forced = native_override(settings_store::snapshot().theme, run_override());
    set_native_appearance(forced);
    let system = match forced {
        Some(mode) => mode,
        None => system_mode(cx),
    };
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
/// System and no per-run override is set. While a mode is forced, the window
/// reports the forced appearance, so the notification is ignored then.
pub fn observe_system_appearance(window: &mut gpui::Window) {
    window
        .observe_window_appearance(|window, cx| {
            if native_override(settings_store::snapshot().theme, run_override()).is_none() {
                apply_with_system(ThemeMode::from(window.appearance()), cx);
            }
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
    fn native_appearance_is_forced_only_when_the_theme_is_pinned() {
        assert_eq!(native_override(ThemePreference::System, None), None);
        assert_eq!(native_override(ThemePreference::Light, None), Some(Light));
        assert_eq!(native_override(ThemePreference::Dark, None), Some(Dark));
        // A per-run override pins it whatever the preference says.
        for pref in ThemePreference::ALL {
            assert_eq!(native_override(pref, Some(Dark)), Some(Dark));
            assert_eq!(native_override(pref, Some(Light)), Some(Light));
        }
    }

    #[test]
    fn native_override_agrees_with_resolve() {
        for pref in ThemePreference::ALL {
            for over in [None, Some(Light), Some(Dark)] {
                for sys in [Light, Dark] {
                    assert_eq!(
                        native_override(pref, over).unwrap_or(sys),
                        resolve(pref, over, sys)
                    );
                }
            }
        }
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
