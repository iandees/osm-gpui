//! Distributing extensions: the registry, the index the app reads, and the
//! packages it installs.
//!
//! The **registry** (`registry/extensions.toml`) lists the published
//! extensions, one table per id: where the source is (`repository` + `rev`,
//! or nothing for this repository) and `path` inside it, and the `version`
//! being published. Publishing or updating an extension is a pull request to
//! it, which is where it is reviewed.
//!
//! The `build-extension-index` tool (run by CI) builds each entry, packages
//! it ([`package`]: a `.tar.gz` of `extension.toml`, `extension.wasm` and
//! `locales/*.toml`) and writes the **index** ([`Index`], `index.json`): each
//! extension's id, version, names and descriptions in every language it
//! ships, the API version it was built against, what it may reach, and where
//! to download its package with the package's SHA-256. The index and the
//! packages are static files (a GitHub release); there is no server.
//!
//! The app fetches the index, offers what it can run, and installs a package
//! only when its checksum, id, version and API version all match
//! ([`install_package`]).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{Capabilities, Manifest};
use super::runtime::{api_version_of, API_VERSION};
use crate::http::{HttpClient, HttpRequest};

/// The index format this app reads.
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// Where the published index lives (a rolling GitHub release that
/// `.github/workflows/extensions.yml` updates).
pub const DEFAULT_INDEX_URL: &str =
    "https://github.com/iandees/osm-gpui/releases/download/extension-index/index.json";

/// Largest package or index the app downloads, and the most a package may
/// unpack to.
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// The index URL: `OSM_GPUI_EXTENSION_INDEX` when set (scripts and tests),
/// else [`DEFAULT_INDEX_URL`].
pub fn index_url() -> String {
    std::env::var("OSM_GPUI_EXTENSION_INDEX").unwrap_or_else(|_| DEFAULT_INDEX_URL.to_string())
}

// --- The registry file ---

/// One published extension in `registry/extensions.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    /// Git URL of the source; none for this repository.
    #[serde(default)]
    pub repository: Option<String>,
    /// The commit to build (required with `repository`).
    #[serde(default)]
    pub rev: Option<String>,
    /// The extension's folder inside the repository.
    #[serde(default)]
    pub path: Option<String>,
    /// The version being published; must equal its manifest's.
    pub version: String,
}

pub fn parse_registry(text: &str) -> Result<BTreeMap<String, RegistryEntry>, String> {
    let entries: BTreeMap<String, RegistryEntry> =
        toml::from_str(text).map_err(|e| e.to_string())?;
    for (id, e) in &entries {
        if e.repository.is_some() && e.rev.is_none() {
            return Err(format!("{id}: `repository` needs a `rev` to build"));
        }
        if let Some(path) = &e.path {
            if !is_plain_relative(Path::new(path)) {
                return Err(format!("{id}: `path` must be relative, without `..`"));
            }
        }
    }
    Ok(entries)
}

fn is_plain_relative(p: &Path) -> bool {
    p.components()
        .all(|c| matches!(c, PathComponent::Normal(_) | PathComponent::CurDir))
}

// --- The index ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Index {
    pub schema_version: u32,
    pub extensions: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    pub id: String,
    /// The manifest's name and description (English).
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: String,
    /// The extension API version it was built against.
    pub api_version: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Its `name` and `description` messages by language tag, from its
    /// locale files.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub descriptions: BTreeMap<String, String>,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
}

impl IndexEntry {
    pub fn name_in(&self, locale: &str) -> &str {
        localized(&self.names, locale).unwrap_or(&self.name)
    }

    pub fn description_in(&self, locale: &str) -> &str {
        localized(&self.descriptions, locale).unwrap_or(&self.description)
    }

    /// Whether this app can run it.
    pub fn is_compatible(&self) -> bool {
        api_compatible(&self.api_version)
    }
}

/// `messages[locale]`, trying `de-AT`, then `de`, then `en`.
fn localized<'a>(messages: &'a BTreeMap<String, String>, locale: &str) -> Option<&'a str> {
    super::locales::Locales::chain(locale)
        .iter()
        .find_map(|tag| messages.get(tag).map(String::as_str))
}

pub fn parse_index(text: &str) -> Result<Index, String> {
    let index: Index = serde_json::from_str(text).map_err(|e| format!("bad index: {e}"))?;
    if index.schema_version > INDEX_SCHEMA_VERSION {
        return Err(format!(
            "the index is schema {} (this app reads {INDEX_SCHEMA_VERSION}); update the app",
            index.schema_version
        ));
    }
    Ok(index)
}

pub fn fetch_index(http: &dyn HttpClient, url: &str) -> Result<Index, String> {
    let mut req = HttpRequest::get(url);
    req = req.header("Accept", "application/json");
    let resp = http
        .request_with_progress(req, &mut |read, _| read as usize <= MAX_INDEX_BYTES)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8(resp.body).map_err(|_| "the index is not UTF-8".to_string())?;
    parse_index(&text)
}

// --- Versions ---

/// Compare dotted versions numerically (`0.10.0` > `0.9.1`); a part that is
/// not a number compares as text.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts =
        |v: &str| -> Vec<String> { v.trim().split(['.', '-', '+']).map(String::from).collect() };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (
            pa.get(i).map(String::as_str).unwrap_or("0"),
            pb.get(i).map(String::as_str).unwrap_or("0"),
        );
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Whether an extension built against API `version` runs here: same major
/// (for 0.x, same minor), and not newer than this app's.
pub fn api_compatible(version: &str) -> bool {
    let nums = |v: &str| -> Option<(u64, u64, u64)> {
        let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
    };
    let (Some(ext), Some(host)) = (nums(version), nums(API_VERSION)) else {
        return false;
    };
    let same_line = if host.0 == 0 {
        ext.0 == 0 && ext.1 == host.1
    } else {
        ext.0 == host.0
    };
    same_line && ext <= host
}

// --- Packages ---

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Package the extension whose manifest (and `locales/`) are in `src`, with
/// its built `wasm`, as a `.tar.gz`.
pub fn package(src: &Path, wasm: &Path) -> Result<Vec<u8>, String> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    let add = |tar: &mut tar::Builder<_>, name: &str, path: &Path| -> Result<(), String> {
        let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        tar.append_file(name, &mut f)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    add(&mut tar, "extension.toml", &src.join("extension.toml"))?;
    add(&mut tar, "extension.wasm", wasm)?;
    let mut locales: Vec<PathBuf> = std::fs::read_dir(src.join("locales"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    locales.retain(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml"));
    locales.sort();
    for p in locales {
        let name = format!(
            "locales/{}",
            p.file_name().and_then(|n| n.to_str()).unwrap_or_default()
        );
        add(&mut tar, &name, &p)?;
    }
    let gz = tar.into_inner().map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
}

/// Whether `name` is a file a package may hold.
fn allowed_entry(name: &Path) -> bool {
    let parts: Vec<&str> = name
        .components()
        .filter_map(|c| match c {
            PathComponent::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if parts.len() != name.components().count() {
        return false;
    }
    match parts.as_slice() {
        ["extension.toml"] | ["extension.wasm"] => true,
        ["locales", file] => {
            file.ends_with(".toml")
                && file
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        }
        _ => false,
    }
}

/// Unpack package `bytes` into `dest` (which must not exist yet). Only
/// regular files at the expected paths are written; anything else in the
/// archive is refused.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut total = 0u64;
    for entry in archive.entries().map_err(|e| format!("bad package: {e}"))? {
        let mut entry = entry.map_err(|e| format!("bad package: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad package: {e}"))?
            .into_owned();
        if !entry.header().entry_type().is_file() || !allowed_entry(&path) {
            return Err(format!(
                "the package holds an unexpected {}",
                path.display()
            ));
        }
        total += entry.size();
        if total > MAX_PACKAGE_BYTES {
            return Err("the package is too large".into());
        }
        let out = dest.join(&path);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| format!("bad package: {e}"))?;
        std::fs::write(&out, data).map_err(|e| e.to_string())?;
    }
    for required in ["extension.toml", "extension.wasm"] {
        if !dest.join(required).is_file() {
            return Err(format!("the package has no {required}"));
        }
    }
    Ok(())
}

/// Install package `bytes` for index `entry` under `root`, replacing an
/// earlier install of it. Refused unless the checksum matches the index,
/// the manifest's id and version are the entry's, and its wasm was built
/// against an API version this app runs.
pub fn install_package(root: &Path, entry: &IndexEntry, bytes: &[u8]) -> Result<PathBuf, String> {
    if sha256_hex(bytes) != entry.sha256.to_ascii_lowercase() {
        return Err(format!(
            "the download of {} does not match the index's checksum",
            entry.id
        ));
    }
    let installed = super::installed_dir(root);
    std::fs::create_dir_all(&installed).map_err(|e| e.to_string())?;
    let staging = installed.join(format!(".{}.download", entry.id));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        unpack(bytes, &staging)?;
        let manifest = super::read_manifest(&staging)?;
        if manifest.id != entry.id || manifest.version != entry.version {
            return Err(format!(
                "the package is {} {}, not {} {}",
                manifest.id, manifest.version, entry.id, entry.version
            ));
        }
        let wasm = std::fs::read(staging.join("extension.wasm")).map_err(|e| e.to_string())?;
        let api = api_version_of(&wasm)?;
        if !api_compatible(&api) {
            return Err(format!(
                "{} needs extension API {api}; this app has {API_VERSION}",
                entry.id
            ));
        }
        Ok(manifest)
    })();
    let manifest: Manifest = match result {
        Ok(m) => m,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    let dest = installed.join(&manifest.id);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Download `entry`'s package and install it. Slow: call off the UI thread.
pub fn download_and_install(
    http: &dyn HttpClient,
    root: &Path,
    entry: &IndexEntry,
) -> Result<PathBuf, String> {
    if !entry.download_url.starts_with("https://") && !entry.download_url.starts_with("http://") {
        return Err(format!("{}: bad download URL", entry.id));
    }
    let limit = entry.size.min(MAX_PACKAGE_BYTES);
    let resp = http
        .request_with_progress(HttpRequest::get(&entry.download_url), &mut |read, _| {
            read <= limit
        })
        .map_err(|e| format!("downloading {} failed: {e}", entry.id))?;
    install_package(root, entry, &resp.body)
}

// --- The app's extension state: which installed extensions are off ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

fn state_path(root: &Path) -> PathBuf {
    root.join("state.json")
}

pub fn load_state(root: &Path) -> State {
    std::fs::read_to_string(state_path(root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_state(root: &Path, state: &State) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(state_path(root), text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("0.10.0", "0.9.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
    }

    #[test]
    fn api_compatibility_follows_semver_with_0x_minor_lines() {
        assert!(api_compatible(API_VERSION));
        assert!(!api_compatible("0.1.0"), "an older 0.x line");
        assert!(!api_compatible("0.3.0"));
        assert!(!api_compatible("1.0.0"));
        assert!(!api_compatible("0.2.99"), "newer than the app");
        assert!(!api_compatible("garbage"));
    }

    #[test]
    fn the_registry_parses_and_refuses_unbuildable_entries() {
        let r = parse_registry(
            "[maproulette]\npath = \"extensions/maproulette\"\nversion = \"0.1.0\"\n\n\
             [other]\nrepository = \"https://github.com/x/y\"\nrev = \"abc\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(
            r["maproulette"].path.as_deref(),
            Some("extensions/maproulette")
        );
        assert!(parse_registry("[x]\nrepository = \"https://a/b\"\nversion = \"1\"\n").is_err());
        assert!(parse_registry("[x]\npath = \"../etc\"\nversion = \"1\"\n").is_err());
    }

    #[test]
    fn the_repository_registry_parses() {
        let text = include_str!("../../registry/extensions.toml");
        let r = parse_registry(text).unwrap();
        let mr = &r["maproulette"];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(mr.path.as_deref().unwrap());
        let manifest = super::super::read_manifest(&dir).unwrap();
        assert_eq!(
            manifest.version, mr.version,
            "registry and manifest versions agree"
        );
    }

    #[test]
    fn package_entries_are_limited_to_the_extension_files() {
        assert!(allowed_entry(Path::new("extension.toml")));
        assert!(allowed_entry(Path::new("locales/de-CH.toml")));
        assert!(!allowed_entry(Path::new("../extension.toml")));
        assert!(!allowed_entry(Path::new("/etc/passwd")));
        assert!(!allowed_entry(Path::new("locales/../../x.toml")));
        assert!(!allowed_entry(Path::new("bin/run.sh")));
        assert!(!allowed_entry(Path::new("locales/x.sh")));
    }

    #[test]
    fn localized_names_fall_back_through_the_language_chain() {
        let entry = IndexEntry {
            id: "x".into(),
            name: "Name".into(),
            version: "1".into(),
            description: "Desc".into(),
            authors: vec![],
            repository: String::new(),
            api_version: API_VERSION.into(),
            capabilities: Default::default(),
            names: [("de".to_string(), "Name DE".to_string())].into(),
            descriptions: BTreeMap::new(),
            download_url: String::new(),
            sha256: String::new(),
            size: 0,
        };
        assert_eq!(entry.name_in("de-AT"), "Name DE");
        assert_eq!(entry.name_in("fr"), "Name");
        assert_eq!(entry.description_in("de"), "Desc");
        assert!(entry.is_compatible());
    }

    #[test]
    fn a_newer_index_schema_is_refused() {
        assert!(parse_index(r#"{"schema_version":2,"extensions":[]}"#).is_err());
        assert!(parse_index(r#"{"schema_version":1,"extensions":[]}"#).is_ok());
    }
}
