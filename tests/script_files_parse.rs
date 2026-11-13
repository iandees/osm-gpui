//! Every checked-in `.osmscript` must parse. Scripts are only executed by the
//! `#[ignore]`d GUI harness, so without this a parser change can silently
//! break them.

use osm_gpui::script::parse;
use std::path::{Path, PathBuf};

const SCRIPT_DIRS: &[&str] = &[
    "docs/screenshots",
    "docs/live-test",
    "docs/perf",
    "tests/ui",
];

/// Scripts known not to parse, with the parser error, so a new break is not
/// hidden behind an existing one. Empty means every script parses.
const KNOWN_UNPARSEABLE: &[(&str, &str)] = &[];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "osmscript") {
            out.push(path);
        }
    }
}

#[test]
fn every_script_file_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in SCRIPT_DIRS {
        collect(&root.join(dir), &mut files);
    }
    files.sort();

    // A moved or renamed directory must not turn this into a vacuous pass.
    assert!(
        files.len() >= 50,
        "found only {} .osmscript files under {SCRIPT_DIRS:?}",
        files.len()
    );

    let mut failures = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(root).unwrap().display().to_string();
        let source = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let known = KNOWN_UNPARSEABLE.iter().any(|(name, _)| *name == rel);
        match (parse(&source), known) {
            (Err(e), false) => failures.push(format!("{rel}:{}: {}", e.line_no, e.message)),
            (Ok(_), true) => failures.push(format!(
                "{rel}: now parses; remove it from KNOWN_UNPARSEABLE"
            )),
            _ => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} script(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
