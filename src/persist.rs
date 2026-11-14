//! Shared helpers for the app's on-disk JSON stores (settings, custom imagery,
//! OAuth token bookkeeping) and other atomically-written cache files (tile
//! cache, ELI imagery cache).
//!
//! The main pieces:
//!
//! - [`write_atomic`]: write bytes to a file such that a concurrent reader,
//!   or a crash mid-write, never observes a torn/truncated file.
//! - [`JsonStore`]: the `OnceLock<Arc<Mutex<T>>>` in-memory cache pattern
//!   used to share one JSON-backed value between the app and its settings
//!   windows.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// What the process may do with the user's real saved state (settings, custom
/// imagery, imagery offsets, OAuth logins, autosaves: everything under the
/// app's config and data directories).
///
/// The policy is process-wide and starts as [`Policy::Disabled`]. Only the real
/// binary's `main()` raises it (to `Normal`, or `ReadOnly` for a scripted run),
/// so every test binary, whether it is the library's or the binary crate's,
/// stays at the safe default without any `cfg(test)` in the library (which
/// would not be active when the binary's tests link the library).
///
/// Caches (tiles, imagery indexes) are regenerable and are not governed by the
/// policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The real saved state is neither read nor written.
    Disabled,
    /// The real saved state is read; writes to it are skipped (the in-memory
    /// copy still updates). [`allow_read_only_writes_to`] names exceptions.
    ReadOnly,
    /// The real saved state is read and written.
    Normal,
}

static POLICY: AtomicU8 = AtomicU8::new(0);
static READ_ONLY_EXCEPTIONS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static SKIPPED_WRITES: AtomicUsize = AtomicUsize::new(0);

/// Set the process-wide policy. Only `main()` should call this.
pub fn set_policy(policy: Policy) {
    let v = match policy {
        Policy::Disabled => 0,
        Policy::ReadOnly => 1,
        Policy::Normal => 2,
    };
    POLICY.store(v, Ordering::SeqCst);
}

pub fn policy() -> Policy {
    match POLICY.load(Ordering::SeqCst) {
        1 => Policy::ReadOnly,
        2 => Policy::Normal,
        _ => Policy::Disabled,
    }
}

/// Under `ReadOnly`, let writes to exactly `path` through (the one explicit
/// exception mechanism; has no effect under `Disabled`).
pub fn allow_read_only_writes_to(path: PathBuf) {
    READ_ONLY_EXCEPTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(path);
}

/// How many writes to real saved state the policy has skipped so far.
pub fn skipped_writes() -> usize {
    SKIPPED_WRITES.load(Ordering::SeqCst)
}

fn state_roots_from(config: Option<PathBuf>, data: Option<PathBuf>) -> Vec<PathBuf> {
    [config, data]
        .into_iter()
        .flatten()
        .map(|d| d.join("osm-gpui"))
        .collect()
}

fn state_roots() -> Vec<PathBuf> {
    state_roots_from(dirs::config_dir(), dirs::data_local_dir())
}

/// The path of the named file in the app's config directory, or `None` when the
/// policy forbids touching real state (or the platform has no config dir).
pub fn config_file(name: &str) -> Option<PathBuf> {
    if policy() == Policy::Disabled {
        return None;
    }
    Some(dirs::config_dir()?.join("osm-gpui").join(name))
}

/// The pure decision behind [`write_permitted`].
fn decide_write(policy: Policy, exceptions: &[PathBuf], roots: &[PathBuf], path: &Path) -> bool {
    if !roots.iter().any(|r| path.starts_with(r)) {
        // Not the user's saved state (a test's temp dir, `--autosave-dir`, a
        // cache, a file the user chose).
        return true;
    }
    match policy {
        Policy::Normal => true,
        Policy::ReadOnly => exceptions.iter().any(|e| e == path),
        Policy::Disabled => false,
    }
}

/// Whether the policy lets `path` be written. Paths outside the user's saved
/// state are always permitted.
pub fn write_permitted(path: &Path) -> bool {
    let exceptions = READ_ONLY_EXCEPTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    decide_write(policy(), &exceptions, &state_roots(), path)
}

/// Whether a write happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// The policy forbade it; nothing on disk changed.
    Skipped,
}

/// Options controlling how [`write_atomic`] writes a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOpts {
    /// If true, the file is chmod'd to owner-only read/write (0600 on unix,
    /// a no-op elsewhere) before it becomes visible under its final name.
    /// Used for files that may contain secrets (e.g. OAuth token fallback).
    pub restrict_permissions: bool,
}

impl WriteOpts {
    pub const fn new() -> Self {
        Self {
            restrict_permissions: false,
        }
    }

    pub const fn restrict_permissions(mut self) -> Self {
        self.restrict_permissions = true;
        self
    }
}

/// Atomically write `bytes` to `path`: write to a unique sibling temp file
/// first, then `rename` into place. `rename` is atomic on POSIX filesystems
/// (macOS/Linux), so concurrent writers/readers of the same path can never
/// observe a torn/truncated file, and two concurrent writers can't stomp on
/// each other's temp files (the temp name is uniquified by pid + thread id).
///
/// Creates `path`'s parent directory if it doesn't already exist.
///
/// Subject to the [`Policy`]: a write to the user's real saved state that the
/// policy forbids is skipped (and reported as `Ok`).
pub fn write_atomic(path: &Path, bytes: &[u8], opts: WriteOpts) -> std::io::Result<()> {
    try_write_atomic(path, bytes, opts).map(|_| ())
}

/// [`write_atomic`], reporting whether the policy skipped the write.
pub fn try_write_atomic(
    path: &Path,
    bytes: &[u8],
    opts: WriteOpts,
) -> std::io::Result<WriteOutcome> {
    write_if(write_permitted(path), path, bytes, opts)
}

/// The write itself, given the policy's verdict.
fn write_if(
    permitted: bool,
    path: &Path,
    bytes: &[u8],
    opts: WriteOpts,
) -> std::io::Result<WriteOutcome> {
    if !permitted {
        SKIPPED_WRITES.fetch_add(1, Ordering::SeqCst);
        return Ok(WriteOutcome::Skipped);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let unique = format!(
        "{}.tmp.{}.{:?}",
        path.display(),
        std::process::id(),
        std::thread::current().id()
    );
    let tmp_path = PathBuf::from(unique);
    std::fs::write(&tmp_path, bytes)?;
    if opts.restrict_permissions {
        restrict_permissions(&tmp_path)?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(WriteOutcome::Written)
}

/// Restrict a file's permissions to owner-only read/write. No-op on non-unix
/// platforms (Windows ACLs already default to the owning user).
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Deserialize `T` from the JSON file at `path`. Missing, unreadable, or
/// malformed files fall back to `T::default()` (logged to stderr, tagged
/// with `label` and `path`, for anything other than a plain missing file).
///
/// A file that exists but cannot be parsed is copied to a `.bak` sibling
/// first (when the [`Policy`] permits writing there), because the caller's
/// next save would otherwise replace the user's data with defaults.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path, label: &str) -> T {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            eprintln!("{label}: read {:?} failed: {}", path, e);
            return T::default();
        }
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{label}: parse {:?} failed: {}", path, e);
            let mut name = path.as_os_str().to_owned();
            name.push(".bak");
            let backup = PathBuf::from(name);
            if write_permitted(&backup) {
                match std::fs::copy(path, &backup) {
                    Ok(_) => eprintln!("{label}: kept the unreadable file as {:?}", backup),
                    Err(e) => eprintln!("{label}: backup to {:?} failed: {}", backup, e),
                }
            }
            T::default()
        }
    }
}

/// `deserialize_with` for an enum-valued field: a value this build does not
/// know (written by a newer version) becomes the field's default instead of
/// failing the whole file.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Serialize `value` as pretty JSON and atomically write it to `path` (see
/// [`write_atomic`]).
pub fn save_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    opts: WriteOpts,
) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &json, opts)
}

/// A process-wide, lazily-initialized, mutex-guarded in-memory cache of a
/// value of type `T` — the pattern shared by every JSON-backed store in this
/// app (settings, custom imagery, OAuth token bookkeeping). Callers own
/// persistence (deciding when/where to load and save); `JsonStore` only
/// manages the shared in-memory copy.
pub struct JsonStore<T> {
    cell: OnceLock<Arc<Mutex<T>>>,
}

impl<T> JsonStore<T> {
    pub const fn new() -> Self {
        Self {
            cell: OnceLock::new(),
        }
    }

    /// Initialize the store with `value`. Call this once at startup; later
    /// calls are no-ops (the first value set wins), matching `OnceLock`
    /// semantics.
    pub fn init(&self, value: T) {
        let _ = self.cell.set(Arc::new(Mutex::new(value)));
    }

    /// Mutate the in-memory value in place via `f`, returning a clone of the
    /// value after mutation. Returns `None` (and logs via `label`) if the
    /// store hasn't been initialized yet or its mutex is poisoned.
    pub fn update<F>(&self, label: &str, f: F) -> Option<T>
    where
        T: Clone,
        F: FnOnce(&mut T),
    {
        let store = self.cell.get()?;
        match store.lock() {
            Ok(mut g) => {
                f(&mut g);
                Some(g.clone())
            }
            Err(e) => {
                eprintln!("{label}: mutex poisoned, skipping update: {e}");
                None
            }
        }
    }

    /// A clone of the current in-memory value, or `T::default()` if the
    /// store hasn't been initialized or its mutex is poisoned (logged via
    /// `label` in the poisoned case).
    pub fn snapshot(&self, label: &str) -> T
    where
        T: Clone + Default,
    {
        let Some(store) = self.cell.get() else {
            return T::default();
        };
        match store.lock() {
            Ok(g) => g.clone(),
            Err(e) => {
                eprintln!("{label}: mutex poisoned, returning default: {e}");
                T::default()
            }
        }
    }

    /// Read a derived value out of the store without cloning the whole
    /// value (useful when `T` is large, e.g. a map, and the caller only
    /// needs one entry). Returns `None` if the store hasn't been
    /// initialized or its mutex is poisoned (logged via `label`).
    pub fn read<R>(&self, label: &str, f: impl FnOnce(&T) -> R) -> Option<R> {
        let store = self.cell.get()?;
        match store.lock() {
            Ok(g) => Some(f(&g)),
            Err(e) => {
                eprintln!("{label}: mutex poisoned, read failed: {e}");
                None
            }
        }
    }
}

impl<T> Default for JsonStore<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::sync::Arc as StdArc;
    use std::thread;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-persist-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[test]
    fn default_policy_is_disabled_and_hides_real_paths() {
        // No test ever raises the policy: only the binary's `main()` does.
        assert_eq!(policy(), Policy::Disabled);
        assert_eq!(config_file("settings.json"), None);
    }

    #[test]
    fn default_policy_skips_writes_to_the_real_state_directory() {
        // The precondition comes first, so a broken guard fails the test
        // before anything is written to the real directory.
        let Some(config) = dirs::config_dir() else {
            return;
        };
        let real = config.join("osm-gpui").join("persist-guard-probe.json");
        assert!(!write_permitted(&real), "guard must forbid real state");
        let before = skipped_writes();
        let outcome = try_write_atomic(&real, b"{}", WriteOpts::default()).unwrap();
        assert_eq!(outcome, WriteOutcome::Skipped);
        assert!(skipped_writes() > before);
        assert!(!real.exists());
    }

    #[test]
    fn decisions_per_policy() {
        let root = PathBuf::from("/home/u/.config/osm-gpui");
        let roots = [root.clone()];
        let settings = root.join("settings.json");
        let oauth = root.join("oauth.json");
        let outside = PathBuf::from("/tmp/elsewhere/settings.json");
        let exceptions = [oauth.clone()];

        for policy in [Policy::Disabled, Policy::ReadOnly, Policy::Normal] {
            assert!(decide_write(policy, &exceptions, &roots, &outside));
        }
        assert!(!decide_write(
            Policy::Disabled,
            &exceptions,
            &roots,
            &settings
        ));
        assert!(!decide_write(Policy::Disabled, &exceptions, &roots, &oauth));
        assert!(!decide_write(
            Policy::ReadOnly,
            &exceptions,
            &roots,
            &settings
        ));
        assert!(decide_write(Policy::ReadOnly, &exceptions, &roots, &oauth));
        assert!(decide_write(Policy::Normal, &exceptions, &roots, &settings));
    }

    #[test]
    fn state_roots_cover_config_and_data_directories() {
        let roots = state_roots_from(Some("/c".into()), Some("/d".into()));
        assert_eq!(
            roots,
            vec![PathBuf::from("/c/osm-gpui"), PathBuf::from("/d/osm-gpui")]
        );
        assert!(state_roots_from(None, None).is_empty());
    }

    #[test]
    fn read_only_reads_but_does_not_write_and_normal_writes() {
        let root = tmp_dir("policy-root");
        let roots = [root.clone()];
        let file = root.join("settings.json");
        fs::write(&file, b"original").unwrap();

        // ReadOnly: the existing file is readable, a write is skipped.
        let allowed = decide_write(Policy::ReadOnly, &[], &roots, &file);
        let outcome = write_if(allowed, &file, b"changed", WriteOpts::default()).unwrap();
        assert_eq!(outcome, WriteOutcome::Skipped);
        assert_eq!(fs::read(&file).unwrap(), b"original");

        // Disabled: also skipped.
        let allowed = decide_write(Policy::Disabled, &[], &roots, &file);
        let outcome = write_if(allowed, &file, b"changed", WriteOpts::default()).unwrap();
        assert_eq!(outcome, WriteOutcome::Skipped);
        assert_eq!(fs::read(&file).unwrap(), b"original");

        // Normal: written, even to a new file.
        let allowed = decide_write(Policy::Normal, &[], &roots, &file);
        let outcome = write_if(allowed, &file, b"changed", WriteOpts::default()).unwrap();
        assert_eq!(outcome, WriteOutcome::Written);
        assert_eq!(fs::read(&file).unwrap(), b"changed");
        let fresh = root.join("new.json");
        let allowed = decide_write(Policy::Normal, &[], &roots, &fresh);
        write_if(allowed, &fresh, b"x", WriteOpts::default()).unwrap();
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_atomic_creates_parent_dir_and_full_content() {
        let dir = tmp_dir("mkdir-parent").join("nested").join("dirs");
        let target = dir.join("file.bin");
        let payload = vec![0xABu8; 4096];
        write_atomic(&target, &payload, WriteOpts::default()).unwrap();
        assert_eq!(fs::read(&target).unwrap(), payload);
    }

    #[test]
    fn write_atomic_leaves_no_tmp_files() {
        let dir = tmp_dir("no-leftovers");
        let target = dir.join("file.bin");
        write_atomic(&target, b"hello", WriteOpts::default()).unwrap();
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);
    }

    #[test]
    fn write_atomic_overwrites_existing_file_fully() {
        let dir = tmp_dir("overwrite");
        let target = dir.join("file.bin");
        write_atomic(&target, &[0x11u8; 10], WriteOpts::default()).unwrap();
        write_atomic(&target, &[0x22u8; 20], WriteOpts::default()).unwrap();
        assert_eq!(fs::read(&target).unwrap(), vec![0x22u8; 20]);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_restrict_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perms");
        let target = dir.join("secret.json");
        write_atomic(&target, b"{}", WriteOpts::new().restrict_permissions()).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// Concurrent writers to the same target path must never produce a torn
    /// file and must never collide on each other's temp file names (the temp
    /// name is uniquified by pid + thread id).
    #[test]
    fn write_atomic_concurrent_writers_never_tear() {
        let dir = tmp_dir("collision");
        let target = StdArc::new(dir.join("shared.bin"));

        let handles: Vec<_> = (0u8..8)
            .map(|i| {
                let target = target.clone();
                thread::spawn(move || {
                    let payload = vec![i; 1024];
                    write_atomic(&target, &payload, WriteOpts::default()).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        // Whichever writer finished last, the file must be exactly one
        // writer's full, untorn payload (a uniform byte value throughout).
        let written = fs::read(&*target).unwrap();
        assert_eq!(written.len(), 1024);
        assert!(written.iter().all(|&b| b == written[0]));

        // No leftover temp files from any writer.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);
    }

    #[test]
    fn json_round_trip() {
        let dir = tmp_dir("json-round-trip");
        let path = dir.join("sample.json");
        let value = Sample {
            name: "alice".into(),
            count: 7,
        };
        save_json(&path, &value, WriteOpts::default()).unwrap();
        let loaded: Sample = load_json(&path, "test");
        assert_eq!(loaded, value);
    }

    #[test]
    fn load_json_missing_file_is_default() {
        let dir = tmp_dir("missing");
        let path = dir.join("sample.json");
        let loaded: Sample = load_json(&path, "test");
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn load_json_corrupt_file_falls_back_to_default() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("sample.json");
        fs::write(&path, b"not valid json {{").unwrap();
        let loaded: Sample = load_json(&path, "test");
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn load_json_corrupt_file_is_kept_as_bak() {
        let dir = tmp_dir("corrupt-bak");
        let path = dir.join("sample.json");
        fs::write(&path, b"not valid json {{").unwrap();
        let loaded: Sample = load_json(&path, "test");
        assert_eq!(loaded, Sample::default());
        assert_eq!(
            fs::read(dir.join("sample.json.bak")).unwrap(),
            b"not valid json {{"
        );
    }

    #[test]
    fn load_json_good_or_missing_file_makes_no_bak() {
        let dir = tmp_dir("no-bak");
        let path = dir.join("sample.json");
        let _: Sample = load_json(&path, "test");
        fs::write(&path, br#"{"name":"a","count":1}"#).unwrap();
        let _: Sample = load_json(&path, "test");
        assert!(!dir.join("sample.json.bak").exists());
    }

    #[test]
    fn load_json_does_not_back_up_into_protected_state() {
        let Some(config) = dirs::config_dir() else {
            return;
        };
        let mut name = config
            .join("osm-gpui")
            .join("persist-guard-probe.json")
            .into_os_string();
        name.push(".bak");
        assert!(!write_permitted(Path::new(&name)));
    }

    #[test]
    fn json_store_snapshot_before_init_is_default() {
        let store: JsonStore<Sample> = JsonStore::new();
        assert_eq!(store.snapshot("test"), Sample::default());
    }

    #[test]
    fn json_store_init_update_snapshot_round_trip() {
        let store: JsonStore<Sample> = JsonStore::new();
        store.init(Sample {
            name: "bob".into(),
            count: 1,
        });
        assert_eq!(
            store.snapshot("test"),
            Sample {
                name: "bob".into(),
                count: 1
            }
        );

        let updated = store.update("test", |v| v.count += 1);
        assert_eq!(
            updated,
            Some(Sample {
                name: "bob".into(),
                count: 2
            })
        );
        assert_eq!(
            store.snapshot("test"),
            Sample {
                name: "bob".into(),
                count: 2
            }
        );
    }

    #[test]
    fn json_store_read_derives_without_full_clone_semantics() {
        let store: JsonStore<Sample> = JsonStore::new();
        store.init(Sample {
            name: "carol".into(),
            count: 3,
        });
        let name_len = store.read("test", |v| v.name.len());
        assert_eq!(name_len, Some(5));
    }

    #[test]
    fn json_store_read_before_init_is_none() {
        let store: JsonStore<Sample> = JsonStore::new();
        assert_eq!(store.read("test", |v: &Sample| v.count), None);
    }

    #[test]
    fn json_store_update_before_init_is_none() {
        let store: JsonStore<Sample> = JsonStore::new();
        assert_eq!(store.update("test", |v: &mut Sample| v.count += 1), None);
    }

    #[test]
    fn json_store_second_init_is_ignored() {
        let store: JsonStore<Sample> = JsonStore::new();
        store.init(Sample {
            name: "first".into(),
            count: 1,
        });
        store.init(Sample {
            name: "second".into(),
            count: 2,
        });
        assert_eq!(
            store.snapshot("test"),
            Sample {
                name: "first".into(),
                count: 1
            }
        );
    }
}
