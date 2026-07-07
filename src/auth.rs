//! OAuth2 (PKCE) login against an OpenStreetMap server.
//!
//! Access/refresh tokens are secrets and are stored in the platform credential store
//! (macOS Keychain / Secret Service / Windows Credential Manager) via the `keyring`
//! crate, keyed by the OAuth server's base URL so switching between the primary and dev
//! API servers keeps separate logins and switching servers can't collide tokens.
//! Non-secret bookkeeping (display name, user id, expiry) is cached as JSON in
//! `<config_dir>/osm-gpui/oauth.json`. If the platform keyring is unavailable, the
//! access/refresh tokens fall back to that same file, written with restrictive (0600)
//! permissions. See https://wiki.openstreetmap.org/wiki/OAuth for the flow this
//! implements.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::settings_store::PRIMARY_API_URL;

const CLIENT_ID: &str = "8cdZSV_ejt5jaqy4MYOMFrlOQgsR56PpIVI3RK0knf4";
// This is a PKCE loopback flow, i.e. a public client: the code_verifier already proves
// possession of the authorization code, so no client_secret is needed (and one embedded
// in a public repo would protect nothing anyway). If OSM's token endpoint ever rejects
// requests for lack of a client_secret, the fix is to re-register the OSM OAuth
// application as a public/PKCE client, not to bring the secret back.
//
// Only `read_prefs` is requested for now: the app has no upload/changeset code yet, so
// there's nothing that consumes write access. Add `write_api` back to SCOPES when
// upload functionality lands.
const SCOPES: &str = "read_prefs";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const CALLBACK_PATH: &str = "/callback";

const USER_AGENT: &str = concat!("osm-gpui/", env!("CARGO_PKG_VERSION"));

/// Service name under which all osm-gpui tokens are stored in the platform keyring.
/// Accounts within that service are keyed by OAuth base URL (see module docs).
const KEYRING_SERVICE: &str = "osm-gpui";

#[derive(Debug)]
pub enum AuthError {
    Network(String),
    Http { status: u16, body: String },
    Parse(String),
    NoRedirect,
    StateMismatch,
    NoConfigDir,
    /// The user (or OSM) explicitly denied/rejected the authorization request, e.g. by
    /// clicking "Cancel" on the consent screen. Distinct from a timeout so the UI can
    /// show a clear, non-misleading message.
    Denied { reason: String },
    /// `ensure_fresh_token` was called for a server with no stored login.
    NotLoggedIn,
    /// The stored token is expired and there's no refresh token to renew it with; the
    /// user needs to log in again.
    NoRefreshToken,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(msg) => write!(f, "Network error: {}", msg),
            AuthError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(f, "OSM OAuth error {}: {}", status, first_line)
            }
            AuthError::Parse(msg) => write!(f, "Failed to parse OSM response: {}", msg),
            AuthError::NoRedirect => write!(f, "Login timed out waiting for browser redirect"),
            AuthError::StateMismatch => write!(f, "Login failed (state mismatch)"),
            AuthError::NoConfigDir => write!(f, "No config directory available to store login"),
            AuthError::Denied { reason } => write!(f, "Sign in was not completed: {}", reason),
            AuthError::NotLoggedIn => write!(f, "Not logged in"),
            AuthError::NoRefreshToken => {
                write!(f, "Login expired and can't be refreshed; please sign in again")
            }
        }
    }
}

/// The website/OAuth host that corresponds to an API base URL. The production API
/// (`api.openstreetmap.org`) is served from a separate host (`www.openstreetmap.org`)
/// than its OAuth endpoints; the dev instance serves both API and OAuth from the same host.
pub fn oauth_base_for(api_base_url: &str) -> String {
    if api_base_url.trim_end_matches('/') == PRIMARY_API_URL {
        "https://www.openstreetmap.org".to_string()
    } else {
        api_base_url.trim_end_matches('/').to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredToken {
    pub access_token: String,
    pub display_name: String,
    pub user_id: u64,
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) at which `access_token` expires, if the server told us.
    pub expires_at: Option<i64>,
}

impl StoredToken {
    /// Whether `access_token` is past its known expiry. Tokens with no known expiry
    /// (`expires_at: None`) are treated as never expiring.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => now_unix() >= exp,
            None => false,
        }
    }
}

pub struct LoginResult {
    pub oauth_base_url: String,
    pub token: StoredToken,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn generate_url_safe_token(num_bytes: usize) -> String {
    use rand::RngCore;
    let mut bytes = vec![0u8; num_bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes)
}

fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest)
}

/// Parse the `code` and `state` query params out of a callback path like
/// `/callback?code=...&state=...`. Only handles the params we need.
fn parse_callback_query(url: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let Some((_, query)) = url.split_once('?') else {
        return params;
    };
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let decoded = urlencoding::decode(v).map(|s| s.into_owned()).unwrap_or_else(|_| v.to_string());
            params.insert(k.to_string(), decoded);
        }
    }
    params
}

/// The path portion of a request URL (i.e. everything before `?`).
fn url_path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Block on the loopback server until a request whose path is our OAuth callback path
/// arrives, or the overall deadline passes. Any other request (favicon probes, browser
/// preconnects, etc.) is answered with a bare 404 and discarded rather than being
/// mistaken for the OAuth response.
fn wait_for_callback(
    server: &tiny_http::Server,
    deadline: Instant,
) -> Result<tiny_http::Request, AuthError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AuthError::NoRedirect);
        }
        let request = server
            .recv_timeout(remaining)
            .map_err(|e| AuthError::Network(e.to_string()))?
            .ok_or(AuthError::NoRedirect)?;

        if url_path(request.url()).starts_with(CALLBACK_PATH) {
            return Ok(request);
        }

        let response = tiny_http::Response::empty(404);
        let _ = request.respond(response);
    }
}

/// Run the full OAuth2 PKCE login flow, blocking until it completes or times out.
/// Opens the user's browser and runs a local HTTP server on 127.0.0.1 to catch the
/// redirect. Call this from a background thread, not the UI thread.
pub fn login(api_base_url: &str) -> Result<LoginResult, AuthError> {
    let oauth_base = oauth_base_for(api_base_url);

    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| AuthError::Network(e.to_string()))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let redirect_uri = format!("http://127.0.0.1:{}{}", port, CALLBACK_PATH);

    let code_verifier = generate_url_safe_token(32);
    let challenge = code_challenge(&code_verifier);
    let state = generate_url_safe_token(16);

    let authorize_url = format!(
        "{}/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        oauth_base,
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(SCOPES),
        urlencoding::encode(&state),
        urlencoding::encode(&challenge),
    );

    if let Err(e) = open::that(&authorize_url) {
        eprintln!("auth: failed to open browser automatically: {}", e);
        eprintln!("auth: open this URL to sign in: {}", authorize_url);
    }

    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    let request = wait_for_callback(&server, deadline)?;

    let params = parse_callback_query(request.url());
    let code = params.get("code").cloned();
    let got_state = params.get("state").cloned();
    let error = params.get("error").cloned();

    let response_body = if code.is_some() {
        "<html><body><h3>Signed in to OpenStreetMap.</h3>You can close this tab and return to osm-gpui.</body></html>"
    } else {
        "<html><body><h3>Sign in failed.</h3>You can close this tab and return to osm-gpui.</body></html>"
    };
    let response = tiny_http::Response::from_string(response_body)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..]).unwrap());
    let _ = request.respond(response);

    // OSM redirects with `error=access_denied` (and no `code`) when the user declines
    // authorization on the consent screen. Report that distinctly rather than letting it
    // fall through to a generic "no redirect" / timeout error.
    if let Some(error) = error {
        let reason = params
            .get("error_description")
            .cloned()
            .unwrap_or(error);
        return Err(AuthError::Denied { reason });
    }

    if got_state.as_deref() != Some(state.as_str()) {
        return Err(AuthError::StateMismatch);
    }
    let code = code.ok_or(AuthError::NoRedirect)?;

    let token_response = ureq::post(&format!("{}/oauth2/token", oauth_base))
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(30))
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &code_verifier),
        ]);

    let (access_token, refresh_token, expires_at) = parse_token_response(token_response)?;

    let (display_name, user_id) = fetch_user_details(api_base_url, &access_token)?;

    let token = StoredToken {
        access_token,
        display_name,
        user_id,
        refresh_token,
        expires_at,
    };
    set_token(&oauth_base, token.clone());

    Ok(LoginResult {
        oauth_base_url: oauth_base,
        token,
    })
}

/// Exchange a refresh token for a new access token via the `refresh_token` grant,
/// keeping the previously-known display name/user id (the token endpoint doesn't return
/// those). Returns the new stored token and persists it.
pub fn refresh(oauth_base_url: &str) -> Result<StoredToken, AuthError> {
    let existing = current_token(oauth_base_url).ok_or(AuthError::NotLoggedIn)?;
    let refresh_token_value = existing.refresh_token.clone().ok_or(AuthError::NoRefreshToken)?;

    let token_response = ureq::post(&format!("{}/oauth2/token", oauth_base_url))
        .set("User-Agent", USER_AGENT)
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", &refresh_token_value),
        ]);

    let (access_token, refresh_token, expires_at) = parse_token_response(token_response)?;

    let token = StoredToken {
        access_token,
        display_name: existing.display_name,
        user_id: existing.user_id,
        // Some servers rotate the refresh token on use and some don't return a new one
        // at all; keep the old one if the response didn't include a replacement.
        refresh_token: refresh_token.or(existing.refresh_token),
        expires_at,
    };
    set_token(oauth_base_url, token.clone());
    Ok(token)
}

/// Return the current token for `oauth_base_url`, transparently refreshing it first if
/// it's expired and a refresh token is available. Callers that need a valid bearer token
/// (e.g. osm_api.rs call sites, once wired up) should use this instead of
/// `current_token` directly.
pub fn ensure_fresh_token(oauth_base_url: &str) -> Result<StoredToken, AuthError> {
    let token = current_token(oauth_base_url).ok_or(AuthError::NotLoggedIn)?;
    if !token.is_expired() {
        return Ok(token);
    }
    refresh(oauth_base_url)
}

/// Parse an access/refresh/expiry triple out of a token-endpoint response, handling the
/// ureq result and OSM's JSON body shape. Shared by the initial code exchange and the
/// refresh-token grant.
fn parse_token_response(
    token_response: Result<ureq::Response, ureq::Error>,
) -> Result<(String, Option<String>, Option<i64>), AuthError> {
    match token_response {
        Ok(resp) => {
            let body = resp
                .into_string()
                .map_err(|e| AuthError::Network(e.to_string()))?;
            let json: serde_json::Value =
                serde_json::from_str(&body).map_err(|e| AuthError::Parse(e.to_string()))?;
            let access_token = json
                .get("access_token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AuthError::Parse("missing access_token".to_string()))?
                .to_string();
            let refresh_token = json
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let expires_at = json
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .map(|secs| now_unix() + secs);
            Ok((access_token, refresh_token, expires_at))
        }
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(AuthError::Http { status, body })
        }
        Err(e) => Err(AuthError::Network(e.to_string())),
    }
}

/// `GET /api/0.6/user/details.json` with the given bearer token. Returns (display_name, id).
fn fetch_user_details(api_base_url: &str, access_token: &str) -> Result<(String, u64), AuthError> {
    let url = format!("{}/api/0.6/user/details.json", api_base_url.trim_end_matches('/'));
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Authorization", &format!("Bearer {}", access_token))
        .timeout(Duration::from_secs(30))
        .call();

    let body = match response {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| AuthError::Network(e.to_string()))?,
        Err(ureq::Error::Status(status, resp)) => {
            let mut buf = String::new();
            let _ = resp.into_reader().take(4096).read_to_string(&mut buf);
            return Err(AuthError::Http { status, body: buf });
        }
        Err(e) => return Err(AuthError::Network(e.to_string())),
    };

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| AuthError::Parse(e.to_string()))?;
    let user = json
        .get("user")
        .ok_or_else(|| AuthError::Parse("missing user".to_string()))?;
    let display_name = user
        .get("display_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AuthError::Parse("missing display_name".to_string()))?
        .to_string();
    let id = user
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| AuthError::Parse("missing id".to_string()))?;

    Ok((display_name, id))
}

// --- Token persistence. ---
//
// Secrets (access_token, refresh_token) live in the platform keyring, one entry per
// OAuth base URL under the `osm-gpui` service. Non-secret bookkeeping (display name,
// user id, expiry) is cached in a small JSON file so we know which servers have a login
// without querying the keyring for all of them. If the keyring can't be used (e.g. no
// platform secret store available), the secrets fall back into that same file, which is
// then chmod'd 0600.

/// A secret payload for one OAuth base URL, as stored in the keyring.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenSecret {
    access_token: String,
    refresh_token: Option<String>,
}

fn keyring_entry(oauth_base_url: &str) -> Option<keyring::Entry> {
    match keyring::Entry::new(KEYRING_SERVICE, oauth_base_url) {
        Ok(entry) => Some(entry),
        Err(e) => {
            eprintln!("auth: keyring unavailable ({}), falling back to file storage", e);
            None
        }
    }
}

fn keyring_store_secret(oauth_base_url: &str, secret: &TokenSecret) -> bool {
    let Some(entry) = keyring_entry(oauth_base_url) else {
        return false;
    };
    let Ok(json) = serde_json::to_string(secret) else {
        return false;
    };
    match entry.set_password(&json) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("auth: keyring set_password failed ({}), falling back to file storage", e);
            false
        }
    }
}

fn keyring_load_secret(oauth_base_url: &str) -> Option<TokenSecret> {
    let entry = keyring_entry(oauth_base_url)?;
    let json = entry.get_password().ok()?;
    serde_json::from_str(&json).ok()
}

fn keyring_delete_secret(oauth_base_url: &str) {
    if let Some(entry) = keyring_entry(oauth_base_url) {
        // NoEntry (nothing to delete) is fine; anything else is just logged.
        if let Err(e) = entry.delete_credential() {
            eprintln!("auth: keyring delete failed: {}", e);
        }
    }
}

/// Non-secret, on-disk bookkeeping for one OAuth base URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedToken {
    display_name: String,
    user_id: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    expires_at: Option<i64>,
    /// Only populated when the platform keyring couldn't be used; the file this lives
    /// in is chmod'd 0600 before any secret is written to it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    access_token_fallback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    refresh_token_fallback: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedStore {
    /// Keyed by OAuth base URL (e.g. "https://www.openstreetmap.org").
    tokens: HashMap<String, PersistedToken>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenStore {
    tokens: HashMap<String, StoredToken>,
}

static TOKEN_STORE: OnceLock<Arc<Mutex<TokenStore>>> = OnceLock::new();

pub fn init_store(store: TokenStore) {
    let _ = TOKEN_STORE.set(Arc::new(Mutex::new(store)));
}

fn set_token(oauth_base_url: &str, token: StoredToken) {
    let Some(store) = TOKEN_STORE.get() else { return };
    let snapshot = {
        let Ok(mut g) = store.lock() else { return };
        g.tokens.insert(oauth_base_url.to_string(), token);
        g.clone()
    };
    save(&snapshot);
}

/// Remove the stored token for the given OAuth base URL, if any.
pub fn logout(oauth_base_url: &str) {
    let Some(store) = TOKEN_STORE.get() else { return };
    let snapshot = {
        let Ok(mut g) = store.lock() else { return };
        g.tokens.remove(oauth_base_url);
        g.clone()
    };
    keyring_delete_secret(oauth_base_url);
    save(&snapshot);
}

/// The stored token for the given OAuth base URL, if the user is logged in there.
pub fn current_token(oauth_base_url: &str) -> Option<StoredToken> {
    TOKEN_STORE
        .get()
        .and_then(|s| s.lock().ok())
        .and_then(|g| g.tokens.get(oauth_base_url).cloned())
}

fn load_from(path: &Path) -> PersistedStore {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PersistedStore::default(),
        Err(e) => {
            eprintln!("auth: read {:?} failed: {}", path, e);
            return PersistedStore::default();
        }
    };
    match serde_json::from_slice::<PersistedStore>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("auth: parse {:?} failed: {}", path, e);
            PersistedStore::default()
        }
    }
}

/// Best-effort restriction of a file's permissions to owner-only read/write. No-op on
/// non-unix platforms (Windows ACLs already default to the owning user).
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn save_to(path: &Path, store: &PersistedStore) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    // May contain fallback secrets (see PersistedToken); lock it down before it's
    // visible under its final name.
    restrict_permissions(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("osm-gpui").join("oauth.json"))
}

pub fn load() -> TokenStore {
    let persisted = match default_path() {
        Some(p) => load_from(&p),
        None => PersistedStore::default(),
    };

    let mut tokens = HashMap::new();
    for (oauth_base_url, meta) in persisted.tokens {
        let (access_token, refresh_token) = match keyring_load_secret(&oauth_base_url) {
            Some(secret) => (secret.access_token, secret.refresh_token),
            None => match meta.access_token_fallback.clone() {
                Some(access_token) => (access_token, meta.refresh_token_fallback.clone()),
                // No secret available anywhere for this entry; drop the stale
                // metadata rather than surface a token-less "logged in" state.
                None => continue,
            },
        };
        tokens.insert(
            oauth_base_url,
            StoredToken {
                access_token,
                display_name: meta.display_name,
                user_id: meta.user_id,
                refresh_token,
                expires_at: meta.expires_at,
            },
        );
    }
    TokenStore { tokens }
}

fn save(store: &TokenStore) {
    let mut persisted = PersistedStore::default();
    for (oauth_base_url, token) in &store.tokens {
        let secret = TokenSecret {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
        };
        let stored_in_keyring = keyring_store_secret(oauth_base_url, &secret);
        persisted.tokens.insert(
            oauth_base_url.clone(),
            PersistedToken {
                display_name: token.display_name.clone(),
                user_id: token.user_id,
                expires_at: token.expires_at,
                access_token_fallback: if stored_in_keyring {
                    None
                } else {
                    Some(token.access_token.clone())
                },
                refresh_token_fallback: if stored_in_keyring {
                    None
                } else {
                    token.refresh_token.clone()
                },
            },
        );
    }

    let Some(p) = default_path() else {
        eprintln!("auth: no config dir, skipping save");
        return;
    };
    if let Err(e) = save_to(&p, &persisted) {
        eprintln!("auth: save {:?} failed: {}", p, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-auth-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn oauth_base_for_primary_api_is_website() {
        assert_eq!(oauth_base_for("https://api.openstreetmap.org"), "https://www.openstreetmap.org");
    }

    #[test]
    fn oauth_base_for_dev_api_is_itself() {
        assert_eq!(
            oauth_base_for("https://master.apis.dev.openstreetmap.org"),
            "https://master.apis.dev.openstreetmap.org"
        );
    }

    #[test]
    fn code_challenge_is_deterministic_and_base64url() {
        let verifier = "abc123";
        let c1 = code_challenge(verifier);
        let c2 = code_challenge(verifier);
        assert_eq!(c1, c2);
        assert!(!c1.contains('+'));
        assert!(!c1.contains('/'));
        assert!(!c1.contains('='));
    }

    #[test]
    fn generate_url_safe_token_has_expected_length_and_charset() {
        let tok = generate_url_safe_token(32);
        assert!(tok.len() >= 40);
        assert!(tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn parse_callback_query_extracts_code_and_state() {
        let params = parse_callback_query("/callback?code=abc&state=xyz");
        assert_eq!(params.get("code"), Some(&"abc".to_string()));
        assert_eq!(params.get("state"), Some(&"xyz".to_string()));
    }

    #[test]
    fn parse_callback_query_extracts_error() {
        let params = parse_callback_query(
            "/callback?error=access_denied&error_description=User%20denied%20access&state=xyz",
        );
        assert_eq!(params.get("error"), Some(&"access_denied".to_string()));
        assert_eq!(
            params.get("error_description"),
            Some(&"User denied access".to_string())
        );
    }

    #[test]
    fn url_path_strips_query_string() {
        assert_eq!(url_path("/callback?code=abc"), "/callback");
        assert_eq!(url_path("/callback"), "/callback");
        assert_eq!(url_path("/favicon.ico"), "/favicon.ico");
    }

    #[test]
    fn persisted_store_round_trip() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("oauth.json");
        let mut store = PersistedStore::default();
        store.tokens.insert(
            "https://www.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "alice".into(),
                user_id: 42,
                expires_at: Some(1_700_000_000),
                access_token_fallback: None,
                refresh_token_fallback: None,
            },
        );
        save_to(&path, &store).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.tokens.len(), 1);
        assert_eq!(loaded.tokens["https://www.openstreetmap.org"].display_name, "alice");
        assert_eq!(loaded.tokens["https://www.openstreetmap.org"].expires_at, Some(1_700_000_000));
    }

    #[test]
    fn missing_token_file_is_empty() {
        let dir = tmp_dir("missing");
        let path = dir.join("oauth.json");
        let loaded = load_from(&path);
        assert!(loaded.tokens.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perms");
        let path = dir.join("oauth.json");
        let mut store = PersistedStore::default();
        store.tokens.insert(
            "https://www.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "alice".into(),
                user_id: 42,
                expires_at: None,
                access_token_fallback: Some("fallback-secret".into()),
                refresh_token_fallback: None,
            },
        );
        save_to(&path, &store).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn stored_token_expiry() {
        let mut token = StoredToken {
            access_token: "tok".into(),
            display_name: "alice".into(),
            user_id: 1,
            refresh_token: None,
            expires_at: None,
        };
        assert!(!token.is_expired(), "no expiry means never expired");

        token.expires_at = Some(now_unix() - 10);
        assert!(token.is_expired());

        token.expires_at = Some(now_unix() + 3600);
        assert!(!token.is_expired());
    }
}
