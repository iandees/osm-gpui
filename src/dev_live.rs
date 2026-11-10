//! Safety layer for the dev-server live-test harness (`examples/dev_live.rs`).
//!
//! The harness runs the app's real upload code against the OpenStreetMap
//! development server and nothing else. This module is what makes "nothing
//! else" true in code rather than by convention:
//!
//! - [`API_BASE`] is a constant; no flag, environment variable or setting can
//!   change it.
//! - [`ensure_dev_url`] refuses any URL that is not on that exact host, and
//!   [`GuardedClient`] runs it on every request right before it is sent, so
//!   every call the upload code makes (create, upload, close, token exchange)
//!   passes through it.
//! - The login lives in a file of its own ([`token_path`]), never the platform
//!   keyring and never the app's `oauth.json`/`settings.json`.
//!
//! Secrets are never printed: [`DevToken`]'s `Debug` output redacts them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::auth::{self, AuthError, StoredToken};
use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use crate::settings_store::DEV_API_URL;

/// The only API server the harness talks to.
pub const API_BASE: &str = DEV_API_URL;

/// A request the guard refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRefused {
    pub url: String,
}

impl std::fmt::Display for HostRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing {:?}: the live-test harness may only talk to {}",
            self.url, API_BASE
        )
    }
}

impl std::error::Error for HostRefused {}

/// Allow `url` only if it is on the dev server: exactly `API_BASE`, or
/// `API_BASE` followed by `/`, `?` or `#`. A look-alike that merely starts with
/// the same characters (`...org.evil.example`, `...org@evil.example`) is
/// refused because the character after the host is not one of those.
pub fn ensure_dev_url(url: &str) -> Result<(), HostRefused> {
    let ok = match url.strip_prefix(API_BASE) {
        Some("") => true,
        Some(rest) => rest.starts_with(['/', '?', '#']),
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err(HostRefused {
            url: url.to_string(),
        })
    }
}

/// An `HttpClient` that checks every request's URL with [`ensure_dev_url`]
/// before handing it to `inner`; a refused request is never sent.
pub struct GuardedClient<C> {
    inner: C,
}

impl<C: HttpClient> GuardedClient<C> {
    pub fn new(inner: C) -> Self {
        GuardedClient { inner }
    }
}

impl<C: HttpClient> HttpClient for GuardedClient<C> {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        if let Err(refused) = ensure_dev_url(&req.url) {
            return Err(HttpError::Transport(refused.to_string()));
        }
        self.inner.request(req)
    }
    // `request_with_progress` keeps the trait's default, which goes through
    // `request` above.
}

/// The harness's own login: one file, one account, dev server only.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct DevToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds at which `access_token` expires, if the server said.
    pub expires_at: Option<i64>,
    pub display_name: String,
    pub user_id: u64,
}

impl std::fmt::Debug for DevToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevToken")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("display_name", &self.display_name)
            .field("user_id", &self.user_id)
            .finish()
    }
}

impl From<StoredToken> for DevToken {
    fn from(t: StoredToken) -> Self {
        DevToken {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: t.expires_at,
            display_name: t.display_name,
            user_id: t.user_id,
        }
    }
}

impl From<&DevToken> for StoredToken {
    fn from(t: &DevToken) -> Self {
        StoredToken {
            access_token: t.access_token.clone(),
            display_name: t.display_name.clone(),
            user_id: t.user_id,
            refresh_token: t.refresh_token.clone(),
            expires_at: t.expires_at,
        }
    }
}

/// `<config_dir>/osm-gpui/dev-live-token.json`.
pub fn token_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("osm-gpui")
            .join("dev-live-token.json"),
    )
}

/// Write `token` to `path`, creating the file with owner-only (0600)
/// permissions on Unix from the start (no moment when it is readable by
/// others) and replacing any earlier file atomically.
pub fn save_token_to(path: &Path, token: &DevToken) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(token)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(&json)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

/// Read the token file; `Ok(None)` if it does not exist.
pub fn load_token_from(path: &Path) -> std::io::Result<Option<DevToken>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Delete the token file. Returns whether there was one.
pub fn delete_token_at(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Run the app's PKCE browser login against the dev server with the built-in
/// dev client id and save the result to `path`. Returns the saved token.
pub fn login(client: &dyn HttpClient, path: &Path) -> Result<DevToken, String> {
    let token: DevToken = auth::obtain_token(client, API_BASE, auth::DEV_CLIENT_ID)
        .map_err(|e| e.to_string())?
        .into();
    save_token_to(path, &token).map_err(|e| format!("could not save the token: {e}"))?;
    Ok(token)
}

/// The saved token, refreshed (and re-saved) first if it has expired.
pub fn fresh_token(client: &dyn HttpClient, path: &Path) -> Result<DevToken, String> {
    let token = load_token_from(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?
        .ok_or_else(|| {
            format!(
                "not logged in to the dev server: run `cargo run --example dev_live -- login` first ({} does not exist)",
                path.display()
            )
        })?;
    let stored = StoredToken::from(&token);
    if !stored.is_expired() {
        return Ok(token);
    }
    let renewed = auth::refresh_token_with(
        client,
        &auth::oauth_base_for(API_BASE),
        auth::DEV_CLIENT_ID,
        &stored,
    )
    .map_err(|e| match e {
        AuthError::NoRefreshToken => {
            "the saved token has expired and cannot be refreshed: run `login` again".to_string()
        }
        other => format!("refreshing the token failed: {other}"),
    })?;
    let renewed: DevToken = renewed.into();
    save_token_to(path, &renewed).map_err(|e| format!("could not save the token: {e}"))?;
    Ok(renewed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, FakeClient};
    use crate::http::Method;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-dev-live-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn token() -> DevToken {
        DevToken {
            access_token: "secret-access".into(),
            refresh_token: Some("secret-refresh".into()),
            expires_at: Some(1_900_000_000),
            display_name: "tester".into(),
            user_id: 7,
        }
    }

    #[test]
    fn api_base_is_the_dev_server() {
        assert_eq!(API_BASE, "https://master.apis.dev.openstreetmap.org");
    }

    #[test]
    fn guard_allows_the_dev_server() {
        for url in [
            "https://master.apis.dev.openstreetmap.org",
            "https://master.apis.dev.openstreetmap.org/",
            "https://master.apis.dev.openstreetmap.org/api/0.6/changeset/create",
            "https://master.apis.dev.openstreetmap.org/oauth2/token",
            "https://master.apis.dev.openstreetmap.org?x=1",
        ] {
            assert!(ensure_dev_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn guard_refuses_production() {
        for url in [
            "https://api.openstreetmap.org",
            "https://api.openstreetmap.org/api/0.6/changeset/create",
            "https://www.openstreetmap.org/oauth2/token",
        ] {
            assert!(ensure_dev_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn guard_refuses_any_other_host() {
        for url in [
            "",
            "https://example.com/api/0.6/changeset/create",
            "http://master.apis.dev.openstreetmap.org/api/0.6/changeset/create",
            "https://api06.dev.openstreetmap.org/api/0.6/changeset/create",
            "http://127.0.0.1:8080/api/0.6/changeset/create",
            "master.apis.dev.openstreetmap.org/api",
            " https://master.apis.dev.openstreetmap.org/api",
        ] {
            assert!(ensure_dev_url(url).is_err(), "{url:?}");
        }
    }

    #[test]
    fn guard_refuses_look_alike_hosts() {
        for url in [
            "https://master.apis.dev.openstreetmap.org.evil.example",
            "https://master.apis.dev.openstreetmap.org.evil.example/api/0.6/changeset/create",
            "https://master.apis.dev.openstreetmap.org@evil.example/api",
            "https://master.apis.dev.openstreetmap.org:444/api",
            "https://master.apis.dev.openstreetmap.orgx/api",
            "https://master.apis.dev.openstreetmap.org\\@evil.example/",
            "https://evil.example/https://master.apis.dev.openstreetmap.org/api",
        ] {
            assert!(ensure_dev_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn guarded_client_never_sends_a_refused_request() {
        let inner = FakeClient::new(vec![ok(200, "should not be used")]);
        let client = GuardedClient::new(inner);

        for url in [
            "https://api.openstreetmap.org/api/0.6/changeset/create",
            "https://master.apis.dev.openstreetmap.org.evil.example/api/0.6/changeset/create",
        ] {
            for method in [Method::Put, Method::Post, Method::Get] {
                let req = HttpRequest::with_text(method, url, Some("<osm/>".into()));
                let err = client.request(req).unwrap_err();
                assert!(
                    matches!(err, HttpError::Transport(_)),
                    "{method:?} {url}: {err}"
                );
            }
        }
        assert_eq!(client.inner.request_count(), 0);
    }

    #[test]
    fn guarded_client_passes_dev_requests_through() {
        let client = GuardedClient::new(FakeClient::new(vec![ok(200, "42")]));
        let req = HttpRequest::with_text(
            Method::Put,
            "https://master.apis.dev.openstreetmap.org/api/0.6/changeset/create",
            Some("<osm/>".into()),
        );
        let resp = client.request(req).unwrap();
        assert_eq!(resp.body, b"42");
        assert_eq!(client.inner.request_count(), 1);
    }

    #[test]
    fn token_file_round_trips() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("nested").join("dev-live-token.json");
        assert_eq!(load_token_from(&path).unwrap(), None);

        save_token_to(&path, &token()).unwrap();
        assert_eq!(load_token_from(&path).unwrap(), Some(token()));

        // Saving again replaces the file.
        let mut newer = token();
        newer.access_token = "newer".into();
        save_token_to(&path, &newer).unwrap();
        assert_eq!(load_token_from(&path).unwrap(), Some(newer));

        assert!(delete_token_at(&path).unwrap());
        assert!(!delete_token_at(&path).unwrap());
        assert_eq!(load_token_from(&path).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perms");
        let path = dir.join("dev-live-token.json");
        save_token_to(&path, &token()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Still 0600 after being replaced.
        save_token_to(&path, &token()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // No temp file left behind.
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn token_path_is_the_dedicated_file() {
        if let Some(path) = token_path() {
            assert!(path.ends_with("osm-gpui/dev-live-token.json"));
        }
    }

    #[test]
    fn debug_output_hides_the_secrets() {
        let shown = format!("{:?}", token());
        assert!(!shown.contains("secret-access"));
        assert!(!shown.contains("secret-refresh"));
        assert!(shown.contains("tester"));
    }

    #[test]
    fn fresh_token_without_a_file_says_to_log_in() {
        let dir = tmp_dir("no-file");
        let client = FakeClient::new(vec![]);
        let err = fresh_token(&client, &dir.join("dev-live-token.json")).unwrap_err();
        assert!(err.contains("login"), "{err}");
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn fresh_token_returns_an_unexpired_token_without_network() {
        let dir = tmp_dir("unexpired");
        let path = dir.join("dev-live-token.json");
        let mut t = token();
        t.expires_at = Some(i64::MAX / 2);
        save_token_to(&path, &t).unwrap();
        let client = FakeClient::new(vec![]);
        assert_eq!(fresh_token(&client, &path).unwrap(), t);
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn fresh_token_refreshes_and_saves_an_expired_token() {
        let dir = tmp_dir("expired");
        let path = dir.join("dev-live-token.json");
        let mut t = token();
        t.expires_at = Some(1);
        save_token_to(&path, &t).unwrap();
        let body = r#"{"access_token":"renewed","expires_in":3600}"#;
        let client = GuardedClient::new(FakeClient::new(vec![ok(200, body)]));
        let renewed = fresh_token(&client, &path).unwrap();
        assert_eq!(renewed.access_token, "renewed");
        assert_eq!(renewed.refresh_token.as_deref(), Some("secret-refresh"));
        assert_eq!(renewed.display_name, "tester");
        assert_eq!(load_token_from(&path).unwrap(), Some(renewed));
    }
}
