//! Build the extension index: every extension in the registry is built for
//! wasm32-wasip2, packaged, and listed in `index.json`.
//!
//! ```text
//! cargo run --bin build-extension-index -- \
//!     --registry registry/extensions.toml --out dist \
//!     --base-url https://github.com/iandees/osm-gpui/releases/download/extension-index/
//! ```
//!
//! `dist/` then holds `index.json` and one `<id>-<version>.tar.gz` per
//! extension, ready to upload to `--base-url`. Entries without `repository`
//! are built from this checkout; others are cloned at their `rev` into the
//! build folder (`--build-dir`, default `<out>/.build`).

use std::path::{Path, PathBuf};
use std::process::Command;

use osm_gpui::extensions::registry::{
    self, package, parse_registry, sha256_hex, Index, IndexEntry, RegistryEntry,
    INDEX_SCHEMA_VERSION,
};
use osm_gpui::extensions::runtime::api_version_of;
use osm_gpui::extensions::{build_dev_extension_in, read_manifest, Locales};

struct Args {
    registry: PathBuf,
    out: PathBuf,
    base_url: String,
    build_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut registry = None;
    let mut out = None;
    let mut base_url = None;
    let mut build_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--registry" => registry = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--base-url" => base_url = Some(value()?),
            "--build-dir" => build_dir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let mut base_url = base_url.ok_or("--base-url is required")?;
    if !base_url.ends_with('/') {
        base_url.push('/');
    }
    Ok(Args {
        registry: registry.ok_or("--registry is required")?,
        out: out.ok_or("--out is required")?,
        base_url,
        build_dir,
    })
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{cmd:?} failed ({status})"))
}

/// The source folder of `entry`: in this checkout, or cloned at its `rev`.
fn source_dir(
    id: &str,
    entry: &RegistryEntry,
    repo_root: &Path,
    build_dir: &Path,
) -> Result<PathBuf, String> {
    let root = match (&entry.repository, &entry.rev) {
        (None, _) => repo_root.to_path_buf(),
        (Some(url), Some(rev)) => {
            let dir = build_dir.join("src").join(id);
            let _ = std::fs::remove_dir_all(&dir);
            run(Command::new("git")
                .args(["clone", "--quiet", "--no-checkout", url])
                .arg(&dir))?;
            run(Command::new("git").arg("-C").arg(&dir).args([
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                rev,
            ]))?;
            dir
        }
        (Some(_), None) => return Err(format!("{id}: repository without rev")),
    };
    Ok(match &entry.path {
        Some(p) => root.join(p),
        None => root,
    })
}

fn build_entry(
    id: &str,
    entry: &RegistryEntry,
    args: &Args,
    repo_root: &Path,
    build_dir: &Path,
) -> Result<IndexEntry, String> {
    let src = source_dir(id, entry, repo_root, build_dir)?;
    let manifest = read_manifest(&src)?;
    if manifest.id != id {
        return Err(format!("{id}: its manifest says id {:?}", manifest.id));
    }
    if manifest.version != entry.version {
        return Err(format!(
            "{id}: the registry publishes {} but its manifest is {}",
            entry.version, manifest.version
        ));
    }
    eprintln!("building {id} {} from {}", manifest.version, src.display());
    let wasm = build_dev_extension_in(&src, Some(&build_dir.join("target")))?;
    let wasm_bytes = std::fs::read(&wasm).map_err(|e| e.to_string())?;
    let api_version = api_version_of(&wasm_bytes)?;
    let bytes = package(&src, &wasm)?;
    let file = format!("{id}-{}.tar.gz", manifest.version);
    std::fs::write(args.out.join(&file), &bytes).map_err(|e| e.to_string())?;
    let locales = Locales::load(&src);
    Ok(IndexEntry {
        id: id.to_string(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        authors: manifest.authors.clone(),
        repository: entry
            .repository
            .clone()
            .unwrap_or_else(|| manifest.repository.clone()),
        api_version,
        capabilities: manifest.capabilities.clone(),
        names: locales.all_of("name"),
        descriptions: locales.all_of("description"),
        download_url: format!("{}{file}", args.base_url),
        sha256: sha256_hex(&bytes),
        size: bytes.len() as u64,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let result = (|| -> Result<usize, String> {
        let text = std::fs::read_to_string(&args.registry)
            .map_err(|e| format!("{}: {e}", args.registry.display()))?;
        let entries = parse_registry(&text)?;
        // The registry sits in `<repo>/registry/`.
        let repo_root = args
            .registry
            .canonicalize()
            .map_err(|e| e.to_string())?
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or("cannot find the repository root")?;
        std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
        let build_dir = args
            .build_dir
            .clone()
            .unwrap_or_else(|| args.out.join(".build"));
        let mut index = Index {
            schema_version: INDEX_SCHEMA_VERSION,
            extensions: Vec::new(),
        };
        for (id, entry) in &entries {
            index
                .extensions
                .push(build_entry(id, entry, &args, &repo_root, &build_dir)?);
        }
        let json = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())?;
        std::fs::write(args.out.join("index.json"), json).map_err(|e| e.to_string())?;
        // Check what was written reads back.
        registry::parse_index(&std::fs::read_to_string(args.out.join("index.json")).unwrap())?;
        Ok(index.extensions.len())
    })();
    match result {
        Ok(n) => eprintln!(
            "wrote {} with {n} extension(s)",
            args.out.join("index.json").display()
        ),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
