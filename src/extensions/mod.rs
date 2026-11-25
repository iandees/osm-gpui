//! Extensions: WebAssembly components that add to the app, in the manner of
//! Zed's extensions.
//!
//! An extension is a folder with an `extension.toml` ([`manifest`]) and an
//! `extension.wasm` built for `wasm32-wasip2` against the WIT interface in
//! `crates/osm-gpui-extension-api/wit` (Rust extensions use that crate). It
//! runs sandboxed in wasmtime ([`runtime`]): no files, sockets or
//! environment; HTTPS only to the hosts its manifest lists; a fuel limit per
//! call. It cannot touch the app directly. It draws markers, shows
//! side-panel sections (as many as it likes, each with its own id) and moves
//! the map by asking the host, which applies those requests on the UI thread.
//! It keeps secrets in the system keychain through the host (namespaced to
//! its id), and carries its own translations in `locales/<tag>.toml`
//! ([`locales`]), which name its menu commands and answer its `translate`
//! calls in the app's language.
//!
//! Installed extensions live in `<config>/osm-gpui/extensions/installed/<id>/`
//! and are all loaded at startup. "Install Dev Extension…" builds a Rust
//! extension from its source folder with `cargo` and installs the result,
//! as Zed's command of the same name does.

pub mod locales;
pub mod manifest;
pub mod registry;
pub mod runtime;

use std::path::{Path, PathBuf};
use std::process::Command;

pub use locales::Locales;
pub use manifest::Manifest;
pub use runtime::{CallContext, Effect, LoadedExtension, Outcome, Services};

/// The Rust target extensions are built for.
pub const WASM_TARGET: &str = "wasm32-wasip2";

/// `<config>/osm-gpui/extensions`, or `OSM_GPUI_EXTENSIONS_DIR` when set
/// (scripts and tests point it at a scratch folder).
pub fn root_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("OSM_GPUI_EXTENSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(dirs::config_dir()?.join("osm-gpui").join("extensions"))
}

pub fn installed_dir(root: &Path) -> PathBuf {
    root.join("installed")
}

/// Where extension `id` keeps its storage.
pub fn storage_path(root: &Path, id: &str) -> PathBuf {
    root.join("data").join(format!("{id}.json"))
}

/// The folders of the installed extensions, sorted.
pub fn installed(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(installed_dir(root)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        // Staging folders (`.<id>.new`) are not installs.
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .filter(|p| p.join("extension.toml").is_file() && p.join("extension.wasm").is_file())
        .collect();
    dirs.sort();
    dirs
}

/// The manifest in `dir`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("extension.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Manifest::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The `cargo` and `rustc` to build with: rustup's (which has the wasm
/// target) when there is one, as a `PATH` cargo may be a distribution's
/// without it.
fn toolchain() -> (PathBuf, Option<PathBuf>) {
    let rustup = dirs::home_dir()
        .map(|h| h.join(".cargo").join("bin").join("rustup"))
        .filter(|p| p.is_file());
    if let Some(rustup) = rustup {
        let which = |tool: &str| {
            Command::new(&rustup)
                .args(["which", tool])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        };
        if let (Some(cargo), Some(rustc)) = (which("cargo"), which("rustc")) {
            return (cargo, Some(rustc));
        }
    }
    (PathBuf::from("cargo"), None)
}

/// Build the Rust extension in `src` (release, for [`WASM_TARGET`]) and
/// return the built wasm. Slow: call off the UI thread.
pub fn build_dev_extension(src: &Path) -> Result<PathBuf, String> {
    build_dev_extension_in(src, None)
}

/// [`build_dev_extension`] with cargo's build files in `target_dir` instead
/// of where the extension's own configuration puts them.
pub fn build_dev_extension_in(src: &Path, target_dir: Option<&Path>) -> Result<PathBuf, String> {
    let manifest_path = src.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Err(format!(
            "{} has no Cargo.toml (only Rust extensions can be built)",
            src.display()
        ));
    }
    let (cargo, rustc) = toolchain();
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(src).args([
        "build",
        "--release",
        "--target",
        WASM_TARGET,
        "--message-format",
        "json-render-diagnostics",
    ]);
    if let Some(rustc) = &rustc {
        cmd.env("RUSTC", rustc);
    }
    if let Some(dir) = target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {}: {e}", cargo.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        let hint = if stderr.contains("may not be installed") {
            format!("\nInstall the target with: rustup target add {WASM_TARGET}")
        } else {
            String::new()
        };
        return Err(format!("cargo build failed:\n{}{hint}", tail.join("\n")));
    }
    // The last cdylib artifact cargo reports is the extension.
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact")
        .filter_map(|v| {
            v["filenames"]
                .as_array()?
                .iter()
                .filter_map(|f| f.as_str())
                .find(|f| f.ends_with(".wasm"))
                .map(PathBuf::from)
        })
        .next()
        .ok_or_else(|| "cargo built no .wasm (is the crate a cdylib?)".to_string())
}

/// The file in a dev extension's installed folder naming its source folder.
const DEV_SOURCE: &str = "dev-source";

/// The source folder of the dev extension installed in `dir`; `None` for
/// one installed from the index.
pub fn dev_source(dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(DEV_SOURCE)).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

/// Install the dev extension in `src` (built as `wasm`), remembering where
/// its source is so it can be rebuilt.
pub fn install_dev(root: &Path, src: &Path, wasm: &Path) -> Result<PathBuf, String> {
    let dest = install(root, src, wasm)?;
    let src = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    std::fs::write(dest.join(DEV_SOURCE), src.display().to_string()).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Copy the extension in `src` (its manifest, and `wasm`) into the installed
/// extensions under `root`, replacing an earlier install of the same id.
/// Returns the installed folder.
pub fn install(root: &Path, src: &Path, wasm: &Path) -> Result<PathBuf, String> {
    let manifest = read_manifest(src)?;
    let dest = installed_dir(root).join(&manifest.id);
    let staging = installed_dir(root).join(format!(".{}.new", manifest.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    std::fs::copy(src.join("extension.toml"), staging.join("extension.toml"))
        .map_err(|e| e.to_string())?;
    std::fs::copy(wasm, staging.join("extension.wasm")).map_err(|e| e.to_string())?;
    // Its translations, if it has any.
    if let Ok(entries) = std::fs::read_dir(src.join("locales")) {
        let locales = staging.join("locales");
        std::fs::create_dir_all(&locales).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("toml") {
                std::fs::copy(&path, locales.join(entry.file_name())).map_err(|e| e.to_string())?;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Remove installed extension `id` (its storage is kept).
pub fn uninstall(root: &Path, id: &str) -> Result<(), String> {
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || id.is_empty()
    {
        return Err(format!("bad extension id {id:?}"));
    }
    std::fs::remove_dir_all(installed_dir(root).join(id)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
