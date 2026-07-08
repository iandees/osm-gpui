//! Runs the real `osm-gpui` binary against each `.osmscript` file under
//! `tests/ui/`, asserting exit status 0 — `assert_mode` failures, crashes,
//! and `wait_idle` timeouts all surface as non-zero exit.
//!
//! Ignored by default: launching the app touches the macOS Keychain and can
//! hang unattended (see docs/superpowers/specs/2026-07-08-ui-mode-tests-design.md).
//! Run explicitly: `cargo test --test ui_scripts -- --ignored`

use std::path::Path;
use std::process::Command;

fn run_script(name: &str) {
    let script_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/ui")
        .join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_osm-gpui"))
        .arg("--script")
        .arg(&script_path)
        .output()
        .unwrap_or_else(|e| panic!("failed to launch osm-gpui: {e}"));

    assert!(
        output.status.success(),
        "{name} failed (status {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[ignore]
fn mode_switching() {
    run_script("mode_switching.osmscript");
}

#[test]
#[ignore]
fn mode_requires_layer() {
    run_script("mode_requires_layer.osmscript");
}

#[test]
#[ignore]
fn select_area_double_click() {
    run_script("select_area_double_click.osmscript");
}

#[test]
#[ignore]
fn select_area_double_click_nested() {
    run_script("select_area_double_click_nested.osmscript");
}
