//! Named secrets that are not OAuth logins: the street-imagery provider
//! credentials (Mapillary's access token, Fotoshi's API key).
//!
//! They used to sit in `settings.json` beside ordinary preferences. They are
//! stored like the OAuth tokens instead: in the platform keychain, or in an
//! owner-only file when the keychain refuses (`<config_dir>/osm-gpui/secrets.json`).
//! That file also holds a non-secret index of which names live in the keychain,
//! so a user who never set a secret never causes a keychain read, and one who did
//! is read only when the value is first needed.
//!
//! Reads are cached for the session and every `set`/`clear` updates the cache at
//! once, so the value in effect never lags what the user typed. Writes follow the
//! persistence policy (`crate::persist::Policy`): nothing is written under
//! `Disabled` (every test) or `ReadOnly` (scripted runs), where a value lives in
//! memory only.

use crate::auth::{SecretBackend, SecretReadError, SystemKeyring, TokenSecret};
use crate::persist::{self, WriteOpts, WriteOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub const MAPILLARY_TOKEN: &str = "mapillary_token";
pub const FOTOSHI_API_KEY: &str = "fotoshi_api_key";

/// The keychain account of a named secret, kept apart from the OAuth server
/// URLs that are the other accounts of the same service.
fn account(name: &str) -> String {
    format!("secret:{name}")
}

/// The on-disk form. `values` holds secrets only where the keychain could not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SecretsFile {
    #[serde(default)]
    in_keychain: BTreeSet<String>,
    #[serde(default)]
    values: BTreeMap<String, String>,
}

struct State {
    index: SecretsFile,
    /// Names already resolved this session; `None` is "no secret" (including a
    /// keychain read that failed, which is not retried until the next launch so a
    /// refused prompt does not reappear every frame).
    cache: HashMap<String, Option<String>>,
}

pub(crate) struct Vault {
    path: Option<PathBuf>,
    /// Short-lived: never held across keychain or disk I/O.
    state: Mutex<State>,
    /// Serialises reads from and writes to the keychain and the file, so the file
    /// on disk is always the last state applied. Lock order: `io`, then `state`.
    io: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Vault {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        let index = match &path {
            Some(p) => persist::load_json(p, "secrets"),
            None => SecretsFile::default(),
        };
        Vault {
            path,
            state: Mutex::new(State {
                index,
                cache: HashMap::new(),
            }),
            io: Mutex::new(()),
        }
    }

    /// Whether `name` has a secret, without reading the keychain.
    pub(crate) fn is_set(&self, name: &str) -> bool {
        let s = lock(&self.state);
        match s.cache.get(name) {
            Some(v) => v.is_some(),
            None => s.index.values.contains_key(name) || s.index.in_keychain.contains(name),
        }
    }

    /// The secret, reading the keychain at most once per session and only when
    /// the index says it is there.
    pub(crate) fn get(&self, backend: &dyn SecretBackend, name: &str) -> Option<String> {
        if let Some(v) = lock(&self.state).cache.get(name) {
            return v.clone();
        }
        let _io = lock(&self.io);
        let in_keychain = {
            let s = lock(&self.state);
            if let Some(v) = s.cache.get(name) {
                return v.clone();
            }
            if let Some(v) = s.index.values.get(name) {
                let v = Some(v.clone());
                drop(s);
                lock(&self.state).cache.insert(name.to_string(), v.clone());
                return v;
            }
            s.index.in_keychain.contains(name)
        };
        let value = if in_keychain {
            match backend.read(&account(name)) {
                Ok(secret) => Some(secret.into_value()),
                Err(SecretReadError::NoEntry) => None,
                Err(SecretReadError::Failed(detail)) => {
                    eprintln!("secrets: keychain read of {name} failed: {detail}");
                    None
                }
            }
        } else {
            None
        };
        lock(&self.state)
            .cache
            .insert(name.to_string(), value.clone());
        value
    }

    /// Store `value` (trimmed; empty clears). The in-memory value changes at
    /// once. Returns whether it was durably stored, so a caller moving a secret
    /// out of another file knows it may drop the old copy.
    pub(crate) fn set(&self, backend: &dyn SecretBackend, name: &str, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            self.clear(backend, name);
            return true;
        }
        let _io = lock(&self.io);
        lock(&self.state)
            .cache
            .insert(name.to_string(), Some(value.to_string()));
        let in_keychain = backend.write(&account(name), &TokenSecret::from_value(value));
        let index = {
            let mut s = lock(&self.state);
            if in_keychain {
                s.index.in_keychain.insert(name.to_string());
                s.index.values.remove(name);
            } else {
                s.index.values.insert(name.to_string(), value.to_string());
                s.index.in_keychain.remove(name);
            }
            s.index.clone()
        };
        self.save_index(&index)
    }

    pub(crate) fn clear(&self, backend: &dyn SecretBackend, name: &str) {
        let _io = lock(&self.io);
        let (was_in_keychain, index, changed) = {
            let mut s = lock(&self.state);
            s.cache.insert(name.to_string(), None);
            let was_in_keychain = s.index.in_keychain.remove(name);
            let had_value = s.index.values.remove(name).is_some();
            (
                was_in_keychain,
                s.index.clone(),
                was_in_keychain || had_value,
            )
        };
        if was_in_keychain {
            backend.delete(&account(name));
        }
        if changed {
            self.save_index(&index);
        }
    }

    /// Write the index file; whether it was written.
    fn save_index(&self, index: &SecretsFile) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        let result = serde_json::to_vec_pretty(index)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            .and_then(|json| {
                persist::try_write_atomic(path, &json, WriteOpts::new().restrict_permissions())
            });
        match result {
            Ok(WriteOutcome::Written) => {
                persist::report_save_ok("secrets");
                true
            }
            Ok(WriteOutcome::Skipped) => false,
            Err(e) => {
                persist::report_save_failure("secrets", &format!("{e} ({})", path.display()));
                false
            }
        }
    }

    /// Move legacy plaintext values (from `settings.json`) in, returning the names
    /// that are now durably stored and so can be blanked at the source. A name
    /// that already has a secret keeps it; the legacy value is just dropped.
    pub(crate) fn migrate(
        &self,
        backend: &dyn SecretBackend,
        legacy: &[(&'static str, &str)],
    ) -> Vec<&'static str> {
        let mut moved = Vec::new();
        for &(name, value) in legacy {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if self.is_set(name) || self.set(backend, name, value) {
                moved.push(name);
            } else {
                // Not durable (read-only run or a failed write): keep serving it
                // for this session and leave the legacy copy in place.
                lock(&self.state)
                    .cache
                    .entry(name.to_string())
                    .or_insert_with(|| Some(value.to_string()));
            }
        }
        moved
    }
}

fn vault() -> &'static Vault {
    static VAULT: OnceLock<Vault> = OnceLock::new();
    VAULT.get_or_init(|| Vault::new(persist::config_file("secrets.json")))
}

/// Whether `name` has a secret. Never reads the keychain: safe every frame.
pub fn is_set(name: &str) -> bool {
    vault().is_set(name)
}

/// The secret, if any. The first call for a secret that lives in the keychain
/// reads it (which may raise the system prompt); later calls are in memory.
pub fn get(name: &str) -> Option<String> {
    vault().get(&SystemKeyring, name)
}

/// Store `value`; empty clears. Takes effect in memory immediately.
pub fn set(name: &str, value: &str) {
    vault().set(&SystemKeyring, name, value);
}

pub fn clear(name: &str) {
    vault().clear(&SystemKeyring, name);
}

/// Move the provider secrets still in `settings.json` into secret storage and
/// blank them there. Call once at startup, after the settings store is
/// initialised. Writes only; it never reads the keychain.
pub fn migrate_from_settings() {
    let settings = crate::settings_store::snapshot();
    let moved = vault().migrate(
        &SystemKeyring,
        &[
            (MAPILLARY_TOKEN, &settings.mapillary_token),
            (FOTOSHI_API_KEY, &settings.fotoshi_api_key),
        ],
    );
    if moved.is_empty() {
        return;
    }
    crate::settings_store::update(true, |s| {
        if moved.contains(&MAPILLARY_TOKEN) {
            s.mapillary_token.clear();
        }
        if moved.contains(&FOTOSHI_API_KEY) {
            s.fotoshi_api_key.clear();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct MemoryBackend {
        items: Mutex<HashMap<String, String>>,
        reads: AtomicUsize,
        refuse_writes: bool,
    }

    impl SecretBackend for MemoryBackend {
        fn read(&self, account: &str) -> Result<TokenSecret, SecretReadError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            match self.items.lock().unwrap().get(account) {
                Some(v) => Ok(TokenSecret::from_value(v)),
                None => Err(SecretReadError::NoEntry),
            }
        }
        fn write(&self, account: &str, secret: &TokenSecret) -> bool {
            if self.refuse_writes {
                return false;
            }
            self.items
                .lock()
                .unwrap()
                .insert(account.to_string(), secret.clone().into_value());
            true
        }
        fn delete(&self, account: &str) {
            self.items.lock().unwrap().remove(account);
        }
    }

    fn tmp_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-secrets-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("secrets.json")
    }

    #[test]
    fn set_get_and_clear() {
        let backend = MemoryBackend::default();
        let vault = Vault::new(Some(tmp_file("set-get-clear")));
        assert!(!vault.is_set(MAPILLARY_TOKEN));
        assert_eq!(vault.get(&backend, MAPILLARY_TOKEN), None);

        assert!(vault.set(&backend, MAPILLARY_TOKEN, "  MLY|abc  "));
        assert!(vault.is_set(MAPILLARY_TOKEN));
        assert_eq!(
            vault.get(&backend, MAPILLARY_TOKEN).as_deref(),
            Some("MLY|abc")
        );
        assert_eq!(backend.items.lock().unwrap().len(), 1);

        vault.clear(&backend, MAPILLARY_TOKEN);
        assert!(!vault.is_set(MAPILLARY_TOKEN));
        assert_eq!(vault.get(&backend, MAPILLARY_TOKEN), None);
        assert!(backend.items.lock().unwrap().is_empty());

        // Setting an empty value is a clear.
        vault.set(&backend, FOTOSHI_API_KEY, "k");
        vault.set(&backend, FOTOSHI_API_KEY, "   ");
        assert!(!vault.is_set(FOTOSHI_API_KEY));
    }

    #[test]
    fn a_later_launch_finds_the_secret_and_reads_the_keychain_once() {
        let path = tmp_file("relaunch");
        let backend = MemoryBackend::default();
        Vault::new(Some(path.clone())).set(&backend, MAPILLARY_TOKEN, "tok");
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(
            !file.contains("tok\""),
            "the secret is not in the index: {file}"
        );

        let relaunched = Vault::new(Some(path));
        assert!(relaunched.is_set(MAPILLARY_TOKEN));
        assert_eq!(
            backend.reads.load(Ordering::SeqCst),
            0,
            "is_set never reads"
        );
        for _ in 0..3 {
            assert_eq!(
                relaunched.get(&backend, MAPILLARY_TOKEN).as_deref(),
                Some("tok")
            );
        }
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_secret_never_set_never_touches_the_keychain() {
        let backend = MemoryBackend::default();
        let vault = Vault::new(Some(tmp_file("never-set")));
        assert_eq!(vault.get(&backend, MAPILLARY_TOKEN), None);
        assert_eq!(vault.get(&backend, FOTOSHI_API_KEY), None);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_refusing_keychain_falls_back_to_the_owner_only_file() {
        let path = tmp_file("fallback");
        let backend = MemoryBackend {
            refuse_writes: true,
            ..Default::default()
        };
        assert!(Vault::new(Some(path.clone())).set(&backend, FOTOSHI_API_KEY, "key"));
        let relaunched = Vault::new(Some(path.clone()));
        assert_eq!(
            relaunched.get(&backend, FOTOSHI_API_KEY).as_deref(),
            Some("key")
        );
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn migration_moves_values_and_is_idempotent() {
        let backend = MemoryBackend::default();
        let vault = Vault::new(Some(tmp_file("migrate")));
        let legacy = [(MAPILLARY_TOKEN, "old-token"), (FOTOSHI_API_KEY, "")];
        assert_eq!(vault.migrate(&backend, &legacy), vec![MAPILLARY_TOKEN]);
        assert_eq!(
            vault.get(&backend, MAPILLARY_TOKEN).as_deref(),
            Some("old-token")
        );

        // The user changes it; a second run with the stale legacy value (the
        // blanking never happened) must not revert it.
        vault.set(&backend, MAPILLARY_TOKEN, "new-token");
        assert_eq!(vault.migrate(&backend, &legacy), vec![MAPILLARY_TOKEN]);
        assert_eq!(
            vault.get(&backend, MAPILLARY_TOKEN).as_deref(),
            Some("new-token")
        );
        // Blank legacy values are not migrated at all.
        assert!(vault
            .migrate(&backend, &[(MAPILLARY_TOKEN, ""), (FOTOSHI_API_KEY, " ")])
            .is_empty());
    }

    #[test]
    fn without_a_writable_policy_nothing_is_written_and_nothing_is_blanked() {
        // The test default: no path (`Disabled`), so the vault cannot persist.
        assert_eq!(persist::policy(), persist::Policy::Disabled);
        assert_eq!(persist::config_file("secrets.json"), None);
        let backend = MemoryBackend::default();
        let vault = Vault::new(None);
        assert!(!vault.set(&backend, MAPILLARY_TOKEN, "tok"));
        assert_eq!(vault.get(&backend, MAPILLARY_TOKEN).as_deref(), Some("tok"));

        let fresh = Vault::new(None);
        let moved = fresh.migrate(&backend, &[(MAPILLARY_TOKEN, "legacy")]);
        assert!(moved.is_empty(), "the legacy copy must stay");
        assert_eq!(
            fresh.get(&backend, MAPILLARY_TOKEN).as_deref(),
            Some("legacy"),
            "but it still works for the session"
        );
    }

    #[test]
    fn a_read_only_policy_skips_the_file_write() {
        // `try_write_atomic` consults the policy for paths under the real config
        // directories; this stands in for it with the real path under `Disabled`.
        let real = dirs::config_dir().map(|d| d.join("osm-gpui").join("secrets.json"));
        let Some(real) = real else { return };
        assert_eq!(persist::policy(), persist::Policy::Disabled);
        let backend = MemoryBackend {
            refuse_writes: true,
            ..Default::default()
        };
        let before = std::fs::metadata(&real)
            .ok()
            .and_then(|m| m.modified().ok());
        let blocked = Vault {
            path: Some(real.clone()),
            state: Mutex::new(State {
                index: SecretsFile::default(),
                cache: HashMap::new(),
            }),
            io: Mutex::new(()),
        };
        assert!(!blocked.set(&backend, MAPILLARY_TOKEN, "tok"));
        let after = std::fs::metadata(&real)
            .ok()
            .and_then(|m| m.modified().ok());
        assert_eq!(before, after);
    }
}
