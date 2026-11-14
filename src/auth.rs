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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::http::{HttpClient, HttpError, HttpFailure, HttpRequest, HttpResponse, RetryPolicy};
use crate::settings_store::PRIMARY_API_URL;
use crate::tr;

/// Client ID used when no per-server override is configured in settings (see
/// `client_id_for`). Registered on the primary OSM instance; the dev instance has its
/// own separate app database and generally needs its own registered client_id.
const DEFAULT_CLIENT_ID: &str = "8cdZSV_ejt5jaqy4MYOMFrlOQgsR56PpIVI3RK0knf4";
// This is a PKCE loopback flow, i.e. a public client: the code_verifier already proves
// possession of the authorization code, so no client_secret is needed (and one embedded
// in a public repo would protect nothing anyway). If OSM's token endpoint ever rejects
// requests for lack of a client_secret, the fix is to re-register the OSM OAuth
// application as a public/PKCE client, not to bring the secret back.
//
// `write_api` is required to create/upload/close changesets (see `osm_upload.rs`).
// Existing logins made before this scope was added won't have write access; the user
// will need to log in again (re-authorizing grants the new scope) before uploading.
//
// `write_notes` is required to comment on, close, reopen and create notes (see
// `notes.rs`). A login made before it was added lacks it: the server answers 403 to a
// note write, the note panel says so, and signing in again grants it.
const SCOPES: &str = "read_prefs write_api write_notes";

/// The client_id to use for a given OAuth base URL: the user's configured override for
/// that server if one is set in settings, otherwise `DEFAULT_CLIENT_ID`. OSM's primary
/// and dev instances have separate app registrations, so a client_id valid on one is
/// unknown to the other ("invalid_client") and each server may need its own.
pub fn client_id_for(oauth_base_url: &str) -> String {
    client_id_from(
        &crate::settings_store::snapshot().client_ids,
        oauth_base_url,
    )
}

/// Client ID registered for the OSM development server (a public, non-confidential
/// PKCE client, not a secret). Used for the dev server when settings hold no override.
const DEV_CLIENT_ID: &str = "Wg9OmNYvMvftXbLvVnMX-9QJHiFzOXJbtNgVDSNRBRU";

/// `client_id_for` against an explicit override map: the override for the server wins,
/// then the built-in id for the dev server, then `DEFAULT_CLIENT_ID`.
fn client_id_from(overrides: &HashMap<String, String>, oauth_base_url: &str) -> String {
    if let Some(id) = overrides.get(oauth_base_url).filter(|s| !s.is_empty()) {
        return id.clone();
    }
    if oauth_base_url.trim_end_matches('/') == crate::settings_store::DEV_API_URL {
        return DEV_CLIENT_ID.to_string();
    }
    DEFAULT_CLIENT_ID.to_string()
}
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const CALLBACK_PATH: &str = "/callback";

/// Service name under which all osm-gpui tokens are stored in the platform keyring.
/// Accounts within that service are keyed by OAuth base URL (see module docs).
/// Only referenced from the `#[cfg(not(test))]` half of `keyring_entry`.
#[cfg_attr(test, allow(dead_code))]
const KEYRING_SERVICE: &str = "osm-gpui";

#[derive(Debug)]
pub enum AuthError {
    Network(String),
    Http {
        status: u16,
        body: String,
    },
    Parse(String),
    NoRedirect,
    StateMismatch,
    NoConfigDir,
    /// The user (or OSM) explicitly denied/rejected the authorization request, e.g. by
    /// clicking "Cancel" on the consent screen. Distinct from a timeout so the UI can
    /// show a clear, non-misleading message.
    Denied {
        reason: String,
    },
    /// `ensure_fresh_token` was called for a server with no stored login.
    NotLoggedIn,
    /// The stored token is expired and there's no refresh token to renew it with; the
    /// user needs to log in again.
    NoRefreshToken,
    /// The saved login exists but the system keychain would not give up its secret
    /// (access denied, prompt cancelled or never answered, platform failure). The stored
    /// login is kept untouched and the read can be retried; `detail` is the platform's
    /// reason.
    KeychainRead {
        detail: String,
    },
}

impl From<HttpError> for AuthError {
    fn from(e: HttpError) -> Self {
        match HttpFailure::from(e) {
            HttpFailure::Status { status, body } => AuthError::Http { status, body },
            HttpFailure::Transport(msg) => AuthError::Network(msg),
            HttpFailure::Cancelled => AuthError::Network(HttpError::Cancelled.to_string()),
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(msg) => write!(f, "{}", tr!("error.network", detail = msg)),
            AuthError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "{}",
                    tr!("error.auth.http", status = status, detail = first_line)
                )
            }
            AuthError::Parse(msg) => write!(f, "{}", tr!("error.api.parse", detail = msg)),
            AuthError::NoRedirect => write!(f, "{}", tr!("error.auth.no_redirect")),
            AuthError::StateMismatch => write!(f, "{}", tr!("error.auth.state_mismatch")),
            AuthError::NoConfigDir => write!(f, "{}", tr!("error.auth.no_config_dir")),
            AuthError::Denied { reason } => {
                write!(f, "{}", tr!("error.auth.denied", reason = reason))
            }
            AuthError::NotLoggedIn => write!(f, "{}", tr!("error.auth.not_logged_in")),
            AuthError::NoRefreshToken => write!(f, "{}", tr!("error.auth.expired")),
            AuthError::KeychainRead { detail } => {
                write!(f, "{}", tr!("error.auth.keychain_read", detail = detail))
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
            let decoded = urlencoding::decode(v)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| v.to_string());
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
pub fn login(client: &dyn HttpClient, api_base_url: &str) -> Result<LoginResult, AuthError> {
    let oauth_base = oauth_base_for(api_base_url);
    let client_id = client_id_for(&oauth_base);

    let server =
        tiny_http::Server::http("127.0.0.1:0").map_err(|e| AuthError::Network(e.to_string()))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let redirect_uri = format!("http://127.0.0.1:{}{}", port, CALLBACK_PATH);

    let code_verifier = generate_url_safe_token(32);
    let challenge = code_challenge(&code_verifier);
    let state = generate_url_safe_token(16);

    let authorize_url = format!(
        "{}/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        oauth_base,
        urlencoding::encode(&client_id),
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
    let response = tiny_http::Response::from_string(response_body).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..]).unwrap(),
    );
    let _ = request.respond(response);

    // OSM redirects with `error=access_denied` (and no `code`) when the user declines
    // authorization on the consent screen. Report that distinctly rather than letting it
    // fall through to a generic "no redirect" / timeout error.
    if let Some(error) = error {
        let reason = params.get("error_description").cloned().unwrap_or(error);
        return Err(AuthError::Denied { reason });
    }

    if got_state.as_deref() != Some(state.as_str()) {
        return Err(AuthError::StateMismatch);
    }
    let code = code.ok_or(AuthError::NoRedirect)?;

    let (access_token, refresh_token, expires_at) = token_request_with(
        client,
        &oauth_base,
        vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("client_id".to_string(), client_id.clone()),
            ("code".to_string(), code),
            ("redirect_uri".to_string(), redirect_uri),
            ("code_verifier".to_string(), code_verifier),
        ],
    )?;

    let (display_name, user_id) = fetch_user_details_with(client, api_base_url, &access_token)?;

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

/// The `refresh_token` grant for an already-resolved stored token, keeping the
/// previously-known display name/user id (the token endpoint doesn't return those).
/// Returns the new stored token and persists it.
fn refresh_from(
    client: &dyn HttpClient,
    oauth_base_url: &str,
    existing: StoredToken,
) -> Result<StoredToken, AuthError> {
    let refresh_token_value = existing
        .refresh_token
        .clone()
        .ok_or(AuthError::NoRefreshToken)?;
    let client_id = client_id_for(oauth_base_url);

    let (access_token, refresh_token, expires_at) = token_request_with(
        client,
        oauth_base_url,
        vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("client_id".to_string(), client_id),
            ("refresh_token".to_string(), refresh_token_value),
        ],
    )?;

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
/// (e.g. the upload path in main.rs) should use this instead of `try_current_token`
/// directly, which never refreshes. May block on the system keychain prompt the first
/// time (see `keyring_read_pending`).
pub fn ensure_fresh_token(
    client: &dyn HttpClient,
    oauth_base_url: &str,
) -> Result<StoredToken, AuthError> {
    ensure_fresh_token_notify(client, oauth_base_url, &mut || {})
}

/// `ensure_fresh_token`, calling `on_keychain_wait` just before a blocking system
/// keychain read starts (and only then), so the caller can tell the user why it waits.
pub fn ensure_fresh_token_notify(
    client: &dyn HttpClient,
    oauth_base_url: &str,
    on_keychain_wait: &mut dyn FnMut(),
) -> Result<StoredToken, AuthError> {
    ensure_fresh_token_using(client, &SystemKeyring, oauth_base_url, on_keychain_wait)
}

/// A fresh token for `oauth_base_url` only if its secret is already in memory; `None`
/// (so the caller goes anonymous) when there is no login, it is unusable, or its secret
/// has not yet been read from the keychain. Never performs a keychain read, so it can
/// never raise the OS prompt. Used by map downloads, which work without authentication.
pub fn fresh_token_if_loaded(client: &dyn HttpClient, oauth_base_url: &str) -> Option<StoredToken> {
    fresh_token_if_loaded_using(client, &SystemKeyring, oauth_base_url)
}

fn fresh_token_if_loaded_using(
    client: &dyn HttpClient,
    backend: &dyn SecretBackend,
    oauth_base_url: &str,
) -> Option<StoredToken> {
    if keyring_read_pending(oauth_base_url) {
        return None;
    }
    ensure_fresh_token_using(client, backend, oauth_base_url, &mut || {}).ok()
}

fn ensure_fresh_token_using(
    client: &dyn HttpClient,
    backend: &dyn SecretBackend,
    oauth_base_url: &str,
    on_keychain_wait: &mut dyn FnMut(),
) -> Result<StoredToken, AuthError> {
    let token = try_current_token_using(backend, oauth_base_url, on_keychain_wait)?
        .ok_or(AuthError::NotLoggedIn)?;
    if !token.is_expired() {
        return Ok(token);
    }
    refresh_from(client, oauth_base_url, token)
}

/// POST a token-endpoint request (authorization_code or refresh_token grant) and parse
/// the access/refresh/expiry triple out of the response, handling OSM's JSON body
/// shape. No retries: token requests aren't idempotent-safe to blindly retry (a
/// authorization `code` is single-use, and retrying a timed-out request risks the
/// server having already consumed it).
fn token_request_with(
    client: &dyn HttpClient,
    oauth_base: &str,
    form: Vec<(String, String)>,
) -> Result<(String, Option<String>, Option<i64>), AuthError> {
    let req = HttpRequest::post_form(format!("{}/oauth2/token", oauth_base), form);
    let response = crate::http::fetch_with_retries(client, &req, &RetryPolicy::none());
    parse_token_response(response)
}

/// Parse an access/refresh/expiry triple out of a token-endpoint response, handling the
/// HTTP result and OSM's JSON body shape. Shared by the initial code exchange and the
/// refresh-token grant.
fn parse_token_response(
    token_response: Result<HttpResponse, HttpError>,
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
        Err(e) => Err(e.into()),
    }
}

/// `GET /api/0.6/user/details.json` with the given bearer token. Returns (display_name, id).
fn fetch_user_details_with(
    client: &dyn HttpClient,
    api_base_url: &str,
    access_token: &str,
) -> Result<(String, u64), AuthError> {
    let url = format!(
        "{}/api/0.6/user/details.json",
        api_base_url.trim_end_matches('/')
    );
    let req = HttpRequest::get(url).bearer(access_token);
    let response = crate::http::fetch_with_retries(client, &req, &RetryPolicy::none());

    let body = response?
        .into_string()
        .map_err(|e| AuthError::Network(e.to_string()))?;

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
pub(crate) struct TokenSecret {
    access_token: String,
    refresh_token: Option<String>,
}

fn keyring_entry(oauth_base_url: &str) -> Option<keyring::Entry> {
    // Never touch the real platform keyring from unit tests: on macOS,
    // accessing it from a freshly-built (unsigned) test binary can trigger a
    // Keychain access prompt that blocks forever in a non-interactive test
    // run. Tests exercise the fallback-file path instead, which is what they
    // actually mean to cover.
    //
    // `cfg(test)` only covers the library's own tests; the binary's tests link
    // the library without it, so the persistence policy (which only the real
    // `main()` raises above `Disabled`) is what protects those.
    #[cfg(test)]
    {
        let _ = oauth_base_url;
        None
    }
    #[cfg(not(test))]
    if crate::persist::policy() == crate::persist::Policy::Disabled {
        None
    } else {
        match keyring::Entry::new(KEYRING_SERVICE, oauth_base_url) {
            Ok(entry) => Some(entry),
            Err(e) => {
                eprintln!(
                    "auth: keyring unavailable ({}), falling back to file storage",
                    e
                );
                None
            }
        }
    }
}

/// Whether a secret for `oauth_base_url` may be written to or deleted from the
/// platform keyring under the persistence policy. A read-only (scripted) run may
/// only touch the development server's login: that is how the live-test
/// sign-in persists, and it can never reach a production login.
fn secret_writes_allowed(oauth_base_url: &str) -> bool {
    use crate::persist::Policy;
    match crate::persist::policy() {
        Policy::Normal => true,
        Policy::ReadOnly => crate::settings_store::is_dev_server(oauth_base_url),
        Policy::Disabled => false,
    }
}

/// Why a keychain read produced no secret. Only `NoEntry` means the keychain
/// definitively holds nothing for this server; everything else (access denied, prompt
/// cancelled, interaction not allowed, platform failure, unparsable payload) is
/// ambiguous and must never be treated as "logged out".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SecretReadError {
    NoEntry,
    Failed(String),
}

/// Where secrets live. The real implementation is the platform keyring; tests inject a
/// fake so they never touch it.
pub(crate) trait SecretBackend {
    fn read(&self, oauth_base_url: &str) -> Result<TokenSecret, SecretReadError>;
    /// Whether the secret was stored; `false` means the caller falls back to the file.
    fn write(&self, oauth_base_url: &str, secret: &TokenSecret) -> bool;
    fn delete(&self, oauth_base_url: &str);
}

/// The platform keyring (inert under `cfg(test)`: see `keyring_entry`).
struct SystemKeyring;

impl SecretBackend for SystemKeyring {
    fn read(&self, oauth_base_url: &str) -> Result<TokenSecret, SecretReadError> {
        let Some(entry) = keyring_entry(oauth_base_url) else {
            return Err(SecretReadError::Failed("keyring unavailable".to_string()));
        };
        match entry.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map_err(|e| SecretReadError::Failed(format!("unreadable keychain entry: {e}"))),
            Err(keyring::Error::NoEntry) => Err(SecretReadError::NoEntry),
            Err(e) => Err(SecretReadError::Failed(e.to_string())),
        }
    }

    fn write(&self, oauth_base_url: &str, secret: &TokenSecret) -> bool {
        if !secret_writes_allowed(oauth_base_url) {
            return false;
        }
        let Some(entry) = keyring_entry(oauth_base_url) else {
            return false;
        };
        let Ok(json) = serde_json::to_string(secret) else {
            return false;
        };
        match entry.set_password(&json) {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "auth: keyring set_password failed ({}), falling back to file storage",
                    e
                );
                false
            }
        }
    }

    fn delete(&self, oauth_base_url: &str) {
        if !secret_writes_allowed(oauth_base_url) {
            return;
        }
        if let Some(entry) = keyring_entry(oauth_base_url) {
            // NoEntry (nothing to delete) is fine; anything else is just logged.
            if let Err(e) = entry.delete_credential() {
                eprintln!("auth: keyring delete failed: {}", e);
            }
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
    /// OAuth base URLs whose `access_token`/`refresh_token` in `tokens` are
    /// placeholder-empty and still need a one-time platform-keyring read.
    /// Populated by `load()` for entries with no on-disk fallback secret;
    /// drained lazily by `try_current_token` the first time each URL is
    /// actually looked up, so a plain app launch that never needs auth
    /// (e.g. just viewing tiles) never triggers a keychain prompt.
    pending_keyring: std::collections::HashSet<String>,
}

static TOKEN_STORE: crate::persist::JsonStore<TokenStore> = crate::persist::JsonStore::new();

pub fn init_store(store: TokenStore) {
    TOKEN_STORE.init(store);
}

fn set_token(oauth_base_url: &str, token: StoredToken) {
    let Some(snapshot) = TOKEN_STORE.update("auth", |g| {
        g.tokens.insert(oauth_base_url.to_string(), token);
        // A token we were just handed supersedes whatever the keychain holds; there is
        // nothing left to read lazily for this server.
        g.pending_keyring.remove(oauth_base_url);
    }) else {
        return;
    };
    save(&snapshot);
}

/// Remove the stored token for the given OAuth base URL, if any.
pub fn logout(oauth_base_url: &str) {
    let Some(snapshot) = TOKEN_STORE.update("auth", |g| {
        g.tokens.remove(oauth_base_url);
        g.pending_keyring.remove(oauth_base_url);
    }) else {
        return;
    };
    SystemKeyring.delete(oauth_base_url);
    save(&snapshot);
}

/// The display name of the account logged in at the given OAuth base URL, if
/// any. Unlike `try_current_token`, this never touches the platform keyring (the
/// display name is part of the on-disk metadata), so it's safe to call every
/// frame — e.g. from the status bar.
pub fn stored_display_name(oauth_base_url: &str) -> Option<String> {
    TOKEN_STORE
        .read("auth", |g| {
            g.tokens.get(oauth_base_url).map(|t| t.display_name.clone())
        })
        .flatten()
}

/// The non-secret login details (display name, user id, expiry) for the given OAuth
/// base URL, if the user is logged in there. The token strings may be empty
/// placeholders: this never touches the keyring, so it is safe on the UI thread.
pub fn stored_login(oauth_base_url: &str) -> Option<StoredToken> {
    TOKEN_STORE
        .read("auth", |g| g.tokens.get(oauth_base_url).cloned())
        .flatten()
}

/// Whether the next `try_current_token`/`ensure_fresh_token` for this URL will have to
/// read the secret from the platform keychain (which may raise an OS prompt and block).
pub fn keyring_read_pending(oauth_base_url: &str) -> bool {
    TOKEN_STORE
        .read("auth", |g| g.pending_keyring.contains(oauth_base_url))
        .unwrap_or(false)
}

/// Serializes keychain reads so two threads needing the same pending secret raise one
/// prompt, not two.
static KEYRING_READ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl TokenStore {
    /// Fold the outcome of a keychain read into the store. Only `NoEntry` (the
    /// keychain definitely holds nothing) drops the stored login; any other failure
    /// leaves the entry and its pending flag alone so a later attempt can retry.
    fn apply_read(
        &mut self,
        oauth_base_url: &str,
        result: Result<TokenSecret, SecretReadError>,
    ) -> Result<(), AuthError> {
        match result {
            Ok(secret) => {
                self.pending_keyring.remove(oauth_base_url);
                if let Some(entry) = self.tokens.get_mut(oauth_base_url) {
                    entry.access_token = secret.access_token;
                    entry.refresh_token = secret.refresh_token;
                }
                Ok(())
            }
            Err(SecretReadError::NoEntry) => {
                self.pending_keyring.remove(oauth_base_url);
                self.tokens.remove(oauth_base_url);
                Ok(())
            }
            Err(SecretReadError::Failed(detail)) => {
                eprintln!("auth: keychain read failed, keeping saved login: {detail}");
                Err(AuthError::KeychainRead { detail })
            }
        }
    }
}

/// Resolve a pending keychain-only secret, if any. `on_keychain_wait` runs just before
/// the (possibly prompt-blocked) read, and only when a read is actually going to happen.
/// Nothing is written to disk or the keychain here, so a process killed mid-read loses
/// nothing.
fn resolve_pending(
    backend: &dyn SecretBackend,
    oauth_base_url: &str,
    on_keychain_wait: &mut dyn FnMut(),
) -> Result<(), AuthError> {
    if !keyring_read_pending(oauth_base_url) {
        return Ok(());
    }
    on_keychain_wait();
    let _guard = KEYRING_READ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Another thread may have resolved it while we waited for the lock.
    if !keyring_read_pending(oauth_base_url) {
        return Ok(());
    }
    let result = backend.read(oauth_base_url);
    let mut outcome = Ok(());
    TOKEN_STORE.update("auth", |g| outcome = g.apply_read(oauth_base_url, result));
    outcome
}

/// The stored token for the given OAuth base URL: `Ok(None)` if the user is not logged
/// in there. The first call for a URL whose secret lives only in the platform keyring
/// (see `load()`) performs a one-time keyring read, caching it in `TOKEN_STORE`. If the
/// keychain refuses (denied, cancelled, ...) this returns `AuthError::KeychainRead` and
/// the saved login stays in place, still pending, for a later retry.
pub fn try_current_token(oauth_base_url: &str) -> Result<Option<StoredToken>, AuthError> {
    try_current_token_using(&SystemKeyring, oauth_base_url, &mut || {})
}

fn try_current_token_using(
    backend: &dyn SecretBackend,
    oauth_base_url: &str,
    on_keychain_wait: &mut dyn FnMut(),
) -> Result<Option<StoredToken>, AuthError> {
    resolve_pending(backend, oauth_base_url, on_keychain_wait)?;
    Ok(stored_login(oauth_base_url))
}

fn load_from(path: &Path) -> PersistedStore {
    crate::persist::load_json(path, "auth")
}

fn save_to(path: &Path, store: &PersistedStore) -> std::io::Result<()> {
    // May contain fallback secrets (see PersistedToken); lock the file down
    // before it's visible under its final name.
    crate::persist::save_json(
        path,
        store,
        crate::persist::WriteOpts::new().restrict_permissions(),
    )
}

/// The real `oauth.json`, or `None` where the persistence policy forbids
/// touching real state (the default: tests never read or rewrite it).
pub fn default_path() -> Option<PathBuf> {
    crate::persist::config_file("oauth.json")
}

/// Load persisted OAuth bookkeeping into an in-memory `TokenStore`, without
/// touching the platform keyring. Entries whose secret lives only in the
/// keyring (i.e. no on-disk fallback) are populated with an empty
/// `access_token`/`refresh_token` and flagged in `pending_keyring`;
/// `try_current_token` resolves them from the keyring on first actual use.
/// This keeps a plain app launch from prompting for the keychain password
/// once per logged-in server before the user has done anything that needs
/// auth.
pub fn load() -> TokenStore {
    let persisted = match default_path() {
        Some(p) => load_from(&p),
        None => PersistedStore::default(),
    };
    build_token_store(persisted)
}

/// Pure conversion from on-disk bookkeeping to an in-memory `TokenStore`,
/// with no filesystem or keyring access — the actual "defer to keyring"
/// decision `load()` documents, factored out so it's unit-testable without
/// touching the platform keyring.
fn build_token_store(persisted: PersistedStore) -> TokenStore {
    let mut tokens = HashMap::new();
    let mut pending_keyring = std::collections::HashSet::new();
    for (oauth_base_url, meta) in persisted.tokens {
        let (access_token, refresh_token) = match meta.access_token_fallback.clone() {
            Some(access_token) => (access_token, meta.refresh_token_fallback.clone()),
            // Secret lives in the keyring; defer the read until this URL's
            // token is actually requested (see `try_current_token`).
            None => {
                pending_keyring.insert(oauth_base_url.clone());
                (String::new(), None)
            }
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
    TokenStore {
        tokens,
        pending_keyring,
    }
}

fn save(store: &TokenStore) {
    use crate::persist::Policy;
    let policy = crate::persist::policy();
    if policy == Policy::Disabled {
        return;
    }
    let Some(p) = default_path() else {
        eprintln!("auth: no config dir, skipping save");
        return;
    };
    let persisted = if policy == Policy::ReadOnly {
        // A scripted run may only record the development server's login (the
        // live-test sign-in). Everything else on disk, and every other
        // server's keychain item, is left exactly as it is.
        if !crate::persist::write_permitted(&p) {
            return;
        }
        let dev_only = TokenStore {
            tokens: store
                .tokens
                .iter()
                .filter(|(url, _)| crate::settings_store::is_dev_server(url))
                .map(|(url, t)| (url.clone(), t.clone()))
                .collect(),
            pending_keyring: store
                .pending_keyring
                .iter()
                .filter(|url| crate::settings_store::is_dev_server(url))
                .cloned()
                .collect(),
        };
        merge_dev_logins(load_from(&p), build_persisted(&dev_only, &SystemKeyring))
    } else {
        build_persisted(store, &SystemKeyring)
    };
    if let Err(e) = save_to(&p, &persisted) {
        eprintln!("auth: save {:?} failed: {}", p, e);
    }
}

/// `on_disk` with its development-server entries replaced by `dev` (so a
/// development logout removes its entry); every other entry is kept as is.
fn merge_dev_logins(mut on_disk: PersistedStore, dev: PersistedStore) -> PersistedStore {
    on_disk
        .tokens
        .retain(|url, _| !crate::settings_store::is_dev_server(url));
    on_disk.tokens.extend(
        dev.tokens
            .into_iter()
            .filter(|(url, _)| crate::settings_store::is_dev_server(url)),
    );
    on_disk
}

/// The on-disk form of `store`, storing each resolved secret via `backend` (falling back
/// to the file when it refuses). Entries still pending a keychain read hold only empty
/// placeholders: they are written as metadata alone and their keychain item is left
/// untouched, so saving one server's token can neither blank another server's real
/// secret nor turn it into a fallback-less file entry that loses it.
fn build_persisted(store: &TokenStore, backend: &dyn SecretBackend) -> PersistedStore {
    let mut persisted = PersistedStore::default();
    for (oauth_base_url, token) in &store.tokens {
        if store.pending_keyring.contains(oauth_base_url) {
            persisted.tokens.insert(
                oauth_base_url.clone(),
                PersistedToken {
                    display_name: token.display_name.clone(),
                    user_id: token.user_id,
                    expires_at: token.expires_at,
                    access_token_fallback: None,
                    refresh_token_fallback: None,
                },
            );
            continue;
        }
        let secret = TokenSecret {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
        };
        let stored_in_keyring = backend.write(oauth_base_url, &secret);
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
    persisted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn http_failures_map_to_auth_errors() {
        assert!(matches!(
            AuthError::from(HttpError::Status {
                status: 401,
                body: b"nope".to_vec()
            }),
            AuthError::Http { status: 401, body } if body == "nope"
        ));
        assert!(matches!(
            AuthError::from(HttpError::Transport("dns".into())),
            AuthError::Network(m) if m == "dns"
        ));
        assert!(matches!(
            AuthError::from(HttpError::Cancelled),
            AuthError::Network(m) if m == "cancelled"
        ));
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-auth-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn client_id_dev_server_has_a_built_in_id() {
        let none = HashMap::new();
        assert_eq!(
            client_id_from(&none, "https://master.apis.dev.openstreetmap.org"),
            "Wg9OmNYvMvftXbLvVnMX-9QJHiFzOXJbtNgVDSNRBRU"
        );
    }

    #[test]
    fn client_id_override_beats_built_in_and_empty_override_is_ignored() {
        let dev = "https://master.apis.dev.openstreetmap.org".to_string();
        let mut m = HashMap::new();
        m.insert(dev.clone(), "mine".to_string());
        assert_eq!(client_id_from(&m, &dev), "mine");
        m.insert(dev.clone(), String::new());
        assert_eq!(client_id_from(&m, &dev), DEV_CLIENT_ID);
    }

    #[test]
    fn client_id_other_servers_use_the_default() {
        let none = HashMap::new();
        assert_eq!(
            client_id_from(&none, "https://www.openstreetmap.org"),
            DEFAULT_CLIENT_ID
        );
        assert_eq!(
            client_id_from(&none, "https://example.com"),
            DEFAULT_CLIENT_ID
        );
        assert_eq!(
            client_id_from(&none, "https://master.apis.dev.openstreetmap.org.evil.com"),
            DEFAULT_CLIENT_ID
        );
    }

    #[test]
    fn oauth_base_for_primary_api_is_website() {
        assert_eq!(
            oauth_base_for("https://api.openstreetmap.org"),
            "https://www.openstreetmap.org"
        );
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
        assert!(tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
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
    fn default_policy_hides_the_real_login_file_and_keyring() {
        assert_eq!(default_path(), None);
        assert!(!secret_writes_allowed("https://www.openstreetmap.org"));
        assert!(!secret_writes_allowed(crate::settings_store::DEV_API_URL));
        // Saving with no policy raised is a no-op (no file, no keychain).
        save(&TokenStore::default());
    }

    #[test]
    fn read_only_merge_replaces_only_development_logins() {
        let prod = "https://www.openstreetmap.org".to_string();
        let dev = crate::settings_store::DEV_API_URL.to_string();
        let token = |name: &str| PersistedToken {
            display_name: name.to_string(),
            ..PersistedToken::default()
        };
        let mut on_disk = PersistedStore::default();
        on_disk.tokens.insert(prod.clone(), token("prod-user"));
        on_disk.tokens.insert(dev.clone(), token("old-dev"));

        let mut new_dev = PersistedStore::default();
        new_dev.tokens.insert(dev.clone(), token("new-dev"));
        // Even if a production entry sneaks into the dev-only set it is ignored.
        new_dev.tokens.insert(prod.clone(), token("intruder"));
        let merged = merge_dev_logins(on_disk.clone(), new_dev);
        assert_eq!(merged.tokens[&prod].display_name, "prod-user");
        assert_eq!(merged.tokens[&dev].display_name, "new-dev");

        // A development logout removes only the development entry.
        let merged = merge_dev_logins(on_disk, PersistedStore::default());
        assert_eq!(merged.tokens[&prod].display_name, "prod-user");
        assert!(!merged.tokens.contains_key(&dev));
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
        assert_eq!(
            loaded.tokens["https://www.openstreetmap.org"].display_name,
            "alice"
        );
        assert_eq!(
            loaded.tokens["https://www.openstreetmap.org"].expires_at,
            Some(1_700_000_000)
        );
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
    fn ensure_fresh_token_with_returns_unexpired_token_without_network() {
        init_store(TokenStore::default());
        let oauth_base = "https://auth-test-valid.example";
        set_token(
            oauth_base,
            StoredToken {
                access_token: "valid-token".into(),
                display_name: "alice".into(),
                user_id: 1,
                refresh_token: Some("refresh-tok".into()),
                expires_at: Some(now_unix() + 3600),
            },
        );
        let client = crate::http::fake::FakeClient::new(vec![]);
        let token = ensure_fresh_token(&client, oauth_base).unwrap();
        assert_eq!(token.access_token, "valid-token");
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn ensure_fresh_token_with_refreshes_expired_token() {
        init_store(TokenStore::default());
        let oauth_base = "https://auth-test-expired-ok.example";
        set_token(
            oauth_base,
            StoredToken {
                access_token: "old-token".into(),
                display_name: "alice".into(),
                user_id: 1,
                refresh_token: Some("refresh-tok".into()),
                expires_at: Some(now_unix() - 10),
            },
        );
        let body = r#"{"access_token":"new-token","expires_in":3600}"#;
        let client = crate::http::fake::FakeClient::new(vec![crate::http::fake::ok(200, body)]);
        let token = ensure_fresh_token(&client, oauth_base).unwrap();
        assert_eq!(token.access_token, "new-token");
        assert_eq!(token.display_name, "alice");
        // Response omitted a new refresh_token, so the old one is kept.
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-tok"));
        assert!(!token.is_expired());
    }

    #[test]
    fn ensure_fresh_token_with_propagates_refresh_failure() {
        init_store(TokenStore::default());
        let oauth_base = "https://auth-test-expired-fail.example";
        set_token(
            oauth_base,
            StoredToken {
                access_token: "old-token".into(),
                display_name: "alice".into(),
                user_id: 1,
                refresh_token: Some("refresh-tok".into()),
                expires_at: Some(now_unix() - 10),
            },
        );
        let client = crate::http::fake::FakeClient::new(vec![crate::http::fake::status_err(
            400,
            "invalid_grant",
        )]);
        let err = ensure_fresh_token(&client, oauth_base).unwrap_err();
        assert!(matches!(err, AuthError::Http { status: 400, .. }));
    }

    #[test]
    fn ensure_fresh_token_with_not_logged_in_is_an_error() {
        init_store(TokenStore::default());
        let client = crate::http::fake::FakeClient::new(vec![]);
        let err = ensure_fresh_token(&client, "https://auth-test-nologin.example").unwrap_err();
        assert!(matches!(err, AuthError::NotLoggedIn));
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

    #[test]
    fn build_token_store_uses_fallback_secret_without_marking_pending() {
        let mut persisted = PersistedStore::default();
        persisted.tokens.insert(
            "https://www.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "alice".into(),
                user_id: 42,
                expires_at: None,
                access_token_fallback: Some("fallback-access".into()),
                refresh_token_fallback: Some("fallback-refresh".into()),
            },
        );

        let store = build_token_store(persisted);

        assert!(
            !store
                .pending_keyring
                .contains("https://www.openstreetmap.org"),
            "an entry with a fallback secret shouldn't need a keyring read"
        );
        let token = store.tokens.get("https://www.openstreetmap.org").unwrap();
        assert_eq!(token.access_token, "fallback-access");
        assert_eq!(token.refresh_token.as_deref(), Some("fallback-refresh"));
    }

    #[test]
    fn build_token_store_defers_keyring_only_entries() {
        let mut persisted = PersistedStore::default();
        persisted.tokens.insert(
            "https://api06.dev.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "bob".into(),
                user_id: 7,
                expires_at: None,
                access_token_fallback: None,
                refresh_token_fallback: None,
            },
        );

        let store = build_token_store(persisted);

        assert!(
            store
                .pending_keyring
                .contains("https://api06.dev.openstreetmap.org"),
            "an entry with no fallback secret must defer to the keyring, not read it here"
        );
        let token = store
            .tokens
            .get("https://api06.dev.openstreetmap.org")
            .unwrap();
        assert_eq!(
            token.access_token, "",
            "placeholder until try_current_token resolves it"
        );
        assert_eq!(
            token.display_name, "bob",
            "non-secret metadata is available immediately"
        );
    }

    #[test]
    fn build_token_store_handles_mixed_fallback_and_pending_entries() {
        let mut persisted = PersistedStore::default();
        persisted.tokens.insert(
            "https://www.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "alice".into(),
                user_id: 42,
                expires_at: None,
                access_token_fallback: Some("fallback-access".into()),
                refresh_token_fallback: None,
            },
        );
        persisted.tokens.insert(
            "https://api06.dev.openstreetmap.org".to_string(),
            PersistedToken {
                display_name: "bob".into(),
                user_id: 7,
                expires_at: None,
                access_token_fallback: None,
                refresh_token_fallback: None,
            },
        );

        let store = build_token_store(persisted);

        assert_eq!(store.tokens.len(), 2);
        assert_eq!(
            store.pending_keyring.len(),
            1,
            "only the keyring-only entry should be deferred"
        );
        assert!(store
            .pending_keyring
            .contains("https://api06.dev.openstreetmap.org"));
        assert!(!store
            .pending_keyring
            .contains("https://www.openstreetmap.org"));
    }

    // --- Keychain seam tests: a fake backend stands in for the platform keyring. ---

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct FakeBackend {
        read_result: Mutex<Result<TokenSecret, SecretReadError>>,
        reads: AtomicUsize,
        writes: Mutex<Vec<String>>,
        write_ok: bool,
    }

    impl FakeBackend {
        fn new(read_result: Result<TokenSecret, SecretReadError>) -> Self {
            FakeBackend {
                read_result: Mutex::new(read_result),
                reads: AtomicUsize::new(0),
                writes: Mutex::new(Vec::new()),
                write_ok: true,
            }
        }
        fn with_secret() -> Self {
            Self::new(Ok(secret("kc-access", Some("kc-refresh"))))
        }
        fn set_result(&self, r: Result<TokenSecret, SecretReadError>) {
            *self.read_result.lock().unwrap() = r;
        }
        fn read_count(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl SecretBackend for FakeBackend {
        fn read(&self, _url: &str) -> Result<TokenSecret, SecretReadError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.read_result.lock().unwrap().clone()
        }
        fn write(&self, url: &str, _secret: &TokenSecret) -> bool {
            self.writes.lock().unwrap().push(url.to_string());
            self.write_ok
        }
        fn delete(&self, _url: &str) {}
    }

    fn secret(access: &str, refresh: Option<&str>) -> TokenSecret {
        TokenSecret {
            access_token: access.to_string(),
            refresh_token: refresh.map(|s| s.to_string()),
        }
    }

    /// Put a keychain-only (pending) login for `url` into the global store.
    fn seed_pending(url: &str, name: &str) {
        init_store(TokenStore::default());
        TOKEN_STORE.update("auth", |g| {
            g.tokens.insert(
                url.to_string(),
                StoredToken {
                    access_token: String::new(),
                    display_name: name.to_string(),
                    user_id: 9,
                    refresh_token: None,
                    expires_at: None,
                },
            );
            g.pending_keyring.insert(url.to_string());
        });
    }

    fn denied() -> Result<TokenSecret, SecretReadError> {
        Err(SecretReadError::Failed("access denied".to_string()))
    }

    #[test]
    fn keychain_read_success_resolves_the_secret_once() {
        let url = "https://kc-ok.example";
        seed_pending(url, "alice");
        let backend = FakeBackend::with_secret();
        let t = try_current_token_using(&backend, url, &mut || {})
            .unwrap()
            .unwrap();
        assert_eq!(t.access_token, "kc-access");
        assert_eq!(t.refresh_token.as_deref(), Some("kc-refresh"));
        assert!(!keyring_read_pending(url));
        try_current_token_using(&backend, url, &mut || {}).unwrap();
        assert_eq!(backend.read_count(), 1, "later lookups are in-memory");
    }

    #[test]
    fn keychain_no_entry_drops_the_login() {
        let url = "https://kc-noentry.example";
        seed_pending(url, "alice");
        let backend = FakeBackend::new(Err(SecretReadError::NoEntry));
        let t = try_current_token_using(&backend, url, &mut || {}).unwrap();
        assert!(t.is_none());
        assert!(stored_display_name(url).is_none());
        assert!(!keyring_read_pending(url));
    }

    #[test]
    fn keychain_failure_keeps_the_login_and_a_retry_works() {
        let url = "https://kc-denied.example";
        seed_pending(url, "alice");
        let backend = FakeBackend::new(denied());

        let err = try_current_token_using(&backend, url, &mut || {}).unwrap_err();
        assert!(matches!(err, AuthError::KeychainRead { .. }));
        assert_eq!(
            stored_display_name(url).as_deref(),
            Some("alice"),
            "still signed in after a failed read"
        );
        assert!(keyring_read_pending(url), "still pending so it can retry");

        // Same again: still an error, still intact.
        assert!(try_current_token_using(&backend, url, &mut || {}).is_err());
        assert_eq!(backend.read_count(), 2);

        // The user allows access this time.
        backend.set_result(Ok(secret("kc-access", None)));
        let t = try_current_token_using(&backend, url, &mut || {})
            .unwrap()
            .unwrap();
        assert_eq!(t.access_token, "kc-access");
        assert!(!keyring_read_pending(url));
    }

    #[test]
    fn ensure_fresh_token_stops_with_keychain_error_and_keeps_login() {
        let url = "https://kc-upload-denied.example";
        seed_pending(url, "alice");
        let backend = FakeBackend::new(denied());
        let client = crate::http::fake::FakeClient::new(vec![]);
        let err = ensure_fresh_token_using(&client, &backend, url, &mut || {}).unwrap_err();
        assert!(matches!(err, AuthError::KeychainRead { .. }));
        assert_eq!(client.request_count(), 0);
        assert!(stored_display_name(url).is_some());
    }

    #[test]
    fn keychain_wait_callback_only_fires_when_a_read_happens() {
        let pending = "https://kc-wait-pending.example";
        seed_pending(pending, "alice");
        let backend = FakeBackend::with_secret();
        let mut waits = 0;
        ensure_fresh_token_using(
            &crate::http::fake::FakeClient::new(vec![]),
            &backend,
            pending,
            &mut || waits += 1,
        )
        .unwrap();
        assert_eq!(waits, 1);

        // Already in memory: no read, no wait notification.
        let loaded = "https://kc-wait-loaded.example";
        set_token(
            loaded,
            StoredToken {
                access_token: "t".into(),
                display_name: "bob".into(),
                user_id: 1,
                refresh_token: None,
                expires_at: None,
            },
        );
        let mut waits = 0;
        ensure_fresh_token_using(
            &crate::http::fake::FakeClient::new(vec![]),
            &backend,
            loaded,
            &mut || waits += 1,
        )
        .unwrap();
        assert_eq!(waits, 0);
        assert_eq!(backend.read_count(), 1);
    }

    #[test]
    fn download_token_never_reads_the_keychain() {
        let url = "https://kc-download.example";
        seed_pending(url, "alice");
        let backend = FakeBackend::with_secret();
        let client = crate::http::fake::FakeClient::new(vec![]);
        assert!(fresh_token_if_loaded_using(&client, &backend, url).is_none());
        assert_eq!(
            backend.read_count(),
            0,
            "download must not raise the prompt"
        );
        assert!(keyring_read_pending(url));

        // Once the secret has been resolved (e.g. by an upload) downloads use it.
        try_current_token_using(&backend, url, &mut || {}).unwrap();
        let t = fresh_token_if_loaded_using(&client, &backend, url).unwrap();
        assert_eq!(t.access_token, "kc-access");
    }

    #[test]
    fn saving_keeps_pending_entries_and_does_not_write_their_keychain_item() {
        let mut store = TokenStore::default();
        let tok = |name: &str, access: &str| StoredToken {
            access_token: access.to_string(),
            display_name: name.to_string(),
            user_id: 1,
            refresh_token: Some("r".into()),
            expires_at: Some(5),
        };
        store
            .tokens
            .insert("https://a.example".into(), tok("alice", "A"));
        store
            .tokens
            .insert("https://b.example".into(), tok("bob", ""));
        store.pending_keyring.insert("https://b.example".into());

        let backend = FakeBackend::with_secret();
        let persisted = build_persisted(&store, &backend);

        assert_eq!(
            *backend.writes.lock().unwrap(),
            vec!["https://a.example".to_string()],
            "the pending server's keychain item must not be overwritten with placeholders"
        );
        let b = &persisted.tokens["https://b.example"];
        assert_eq!(b.display_name, "bob");
        assert_eq!(b.expires_at, Some(5));
        assert!(b.access_token_fallback.is_none() && b.refresh_token_fallback.is_none());
        let a = &persisted.tokens["https://a.example"];
        assert!(a.access_token_fallback.is_none(), "stored in keychain");

        // Reloading the saved form still defers B to the keychain.
        let reloaded = build_token_store(persisted);
        assert!(reloaded.pending_keyring.contains("https://b.example"));
        assert!(
            reloaded.pending_keyring.contains("https://a.example"),
            "keychain-stored secrets are read lazily after a reload"
        );
    }

    #[test]
    fn failed_keychain_write_falls_back_to_the_file_instead_of_losing_the_token() {
        let mut store = TokenStore::default();
        store.tokens.insert(
            "https://a.example".into(),
            StoredToken {
                access_token: "A".into(),
                display_name: "alice".into(),
                user_id: 1,
                refresh_token: Some("R".into()),
                expires_at: None,
            },
        );
        let mut backend = FakeBackend::with_secret();
        backend.write_ok = false;
        let persisted = build_persisted(&store, &backend);
        let a = &persisted.tokens["https://a.example"];
        assert_eq!(a.access_token_fallback.as_deref(), Some("A"));
        assert_eq!(a.refresh_token_fallback.as_deref(), Some("R"));
    }

    #[test]
    fn set_token_supersedes_a_pending_read() {
        let url = "https://kc-relogin.example";
        seed_pending(url, "alice");
        set_token(
            url,
            StoredToken {
                access_token: "fresh".into(),
                display_name: "alice".into(),
                user_id: 9,
                refresh_token: None,
                expires_at: None,
            },
        );
        assert!(!keyring_read_pending(url));
    }

    #[test]
    fn keychain_error_message_is_user_facing() {
        let msg = AuthError::KeychainRead {
            detail: "denied".into(),
        }
        .to_string();
        assert!(msg.contains("keychain"), "{msg}");
    }
}
