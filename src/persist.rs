//! Shared helpers for the app's on-disk JSON stores (settings, custom imagery,
//! OAuth token bookkeeping) and other atomically-written cache files (tile
//! cache, ELI imagery cache).
//!
//! Two pieces are factored out here because they were previously
//! copy-pasted, slightly differently, in each call site:
//!
//! - [`write_atomic`]: write bytes to a file such that a concurrent reader,
//!   or a crash mid-write, never observes a torn/truncated file.
//! - [`JsonStore`]: the `OnceLock<Arc<Mutex<T>>>` in-memory cache pattern
//!   used to share one JSON-backed value between the app and its settings
//!   windows.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

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
pub fn write_atomic(path: &Path, bytes: &[u8], opts: WriteOpts) -> std::io::Result<()> {
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
    Ok(())
}

/// Best-effort restriction of a file's permissions to owner-only read/write.
/// No-op on non-unix platforms (Windows ACLs already default to the owning
/// user).
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
            T::default()
        }
    }
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
