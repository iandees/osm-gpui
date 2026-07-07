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

use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse, RetryPolicy, UreqClient};
use crate::settings_store::PRIMARY_API_URL;

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
const SCOPES: &str = "read_prefs write_api";

/// The client_id to use for a given OAuth base URL: the user's configured override for
/// that server if one is set in settings, otherwise `DEFAULT_CLIENT_ID`. OSM's primary
/// and dev instances have separate app registrations, so a client_id valid on one is
/// unknown to the other ("invalid_client") and each server may need its own.
pub fn client_id_for(oauth_base_url: &str) -> String {
    crate::settings_store::snapshot()
        .client_ids
        .get(oauth_base_url)
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
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
                write!(
                    f,
                    "Login expired and can't be refreshed; please sign in again"
                )
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
pub fn login(api_base_url: &str) -> Result<LoginResult, AuthError> {
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
        &UreqClient::new(),
        &oauth_base,
        vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("client_id".to_string(), client_id.clone()),
            ("code".to_string(), code),
            ("redirect_uri".to_string(), redirect_uri),
            ("code_verifier".to_string(), code_verifier),
        ],
    )?;

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
    refresh_with(&UreqClient::new(), oauth_base_url)
}

/// Same as `refresh`, but against an injected `HttpClient` so it's testable without a
/// real network. Kept `pub(crate)` since tests are the only other caller.
pub(crate) fn refresh_with(
    client: &dyn HttpClient,
    oauth_base_url: &str,
) -> Result<StoredToken, AuthError> {
    let existing = current_token(oauth_base_url).ok_or(AuthError::NotLoggedIn)?;
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
/// (e.g. the OSM API download path in main.rs) should use this instead of
/// `current_token` directly, which never refreshes.
pub fn ensure_fresh_token(oauth_base_url: &str) -> Result<StoredToken, AuthError> {
    ensure_fresh_token_with(&UreqClient::new(), oauth_base_url)
}

/// Same as `ensure_fresh_token`, but against an injected `HttpClient` so it's testable
/// without a real network. Kept `pub(crate)` since tests are the only other caller.
pub(crate) fn ensure_fresh_token_with(
    client: &dyn HttpClient,
    oauth_base_url: &str,
) -> Result<StoredToken, AuthError> {
    let token = current_token(oauth_base_url).ok_or(AuthError::NotLoggedIn)?;
    if !token.is_expired() {
        return Ok(token);
    }
    refresh_with(client, oauth_base_url)
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
        Err(HttpError::Status { status, body }) => Err(AuthError::Http {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        }),
        Err(HttpError::Transport(msg)) => Err(AuthError::Network(msg)),
    }
}

/// `GET /api/0.6/user/details.json` with the given bearer token. Returns (display_name, id).
fn fetch_user_details(api_base_url: &str, access_token: &str) -> Result<(String, u64), AuthError> {
    fetch_user_details_with(&UreqClient::new(), api_base_url, access_token)
}

/// Same as `fetch_user_details`, but against an injected `HttpClient` so it's testable
/// without a real network.
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

    let body = match response {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| AuthError::Network(e.to_string()))?,
        Err(HttpError::Status { status, body }) => {
            return Err(AuthError::Http {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        Err(HttpError::Transport(msg)) => return Err(AuthError::Network(msg)),
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
    // Never touch the real platform keyring from unit tests: on macOS,
    // accessing it from a freshly-built (unsigned) test binary can trigger a
    // Keychain access prompt that blocks forever in a non-interactive test
    // run. Tests exercise the fallback-file path instead, which is what they
    // actually mean to cover.
    #[cfg(test)]
    {
        let _ = oauth_base_url;
        None
    }
    #[cfg(not(test))]
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
            eprintln!(
                "auth: keyring set_password failed ({}), falling back to file storage",
                e
            );
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
    /// OAuth base URLs whose `access_token`/`refresh_token` in `tokens` are
    /// placeholder-empty and still need a one-time platform-keyring read.
    /// Populated by `load()` for entries with no on-disk fallback secret;
    /// drained lazily by `current_token()` the first time each URL is
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
    }) else {
        return;
    };
    save(&snapshot);
}

/// Remove the stored token for the given OAuth base URL, if any.
pub fn logout(oauth_base_url: &str) {
    let Some(snapshot) = TOKEN_STORE.update("auth", |g| {
        g.tokens.remove(oauth_base_url);
    }) else {
        return;
    };
    keyring_delete_secret(oauth_base_url);
    save(&snapshot);
}

/// The stored token for the given OAuth base URL, if the user is logged in
/// there. The first call for a URL whose secret lives only in the platform
/// keyring (see `load()`) performs a one-time keyring read here, resolving
/// and caching it in `TOKEN_STORE`; every later call (for that URL) is a
/// plain in-memory lookup and never touches the keyring again this run.
pub fn current_token(oauth_base_url: &str) -> Option<StoredToken> {
    let needs_keyring = TOKEN_STORE
        .read("auth", |g| g.pending_keyring.contains(oauth_base_url))
        .unwrap_or(false);
    if needs_keyring {
        let secret = keyring_load_secret(oauth_base_url);
        TOKEN_STORE.update("auth", |g| {
            g.pending_keyring.remove(oauth_base_url);
            match secret {
                Some(secret) => {
                    if let Some(entry) = g.tokens.get_mut(oauth_base_url) {
                        entry.access_token = secret.access_token;
                        entry.refresh_token = secret.refresh_token;
                    }
                }
                // No secret available in the keyring after all; drop the stale
                // metadata rather than surface a token-less "logged in" state
                // (matches `load()`'s prior behavior for this case).
                None => {
                    g.tokens.remove(oauth_base_url);
                }
            }
        });
    }

    TOKEN_STORE
        .read("auth", |g| g.tokens.get(oauth_base_url).cloned())
        .flatten()
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

fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("osm-gpui").join("oauth.json"))
}

/// Load persisted OAuth bookkeeping into an in-memory `TokenStore`, without
/// touching the platform keyring. Entries whose secret lives only in the
/// keyring (i.e. no on-disk fallback) are populated with an empty
/// `access_token`/`refresh_token` and flagged in `pending_keyring`;
/// `current_token()` resolves them from the keyring on first actual use.
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
            // token is actually requested (see `current_token`).
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
    TokenStore { tokens, pending_keyring }
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
        let token = ensure_fresh_token_with(&client, oauth_base).unwrap();
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
        let token = ensure_fresh_token_with(&client, oauth_base).unwrap();
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
        let err = ensure_fresh_token_with(&client, oauth_base).unwrap_err();
        assert!(matches!(err, AuthError::Http { status: 400, .. }));
    }

    #[test]
    fn ensure_fresh_token_with_not_logged_in_is_an_error() {
        init_store(TokenStore::default());
        let client = crate::http::fake::FakeClient::new(vec![]);
        let err =
            ensure_fresh_token_with(&client, "https://auth-test-nologin.example").unwrap_err();
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
            !store.pending_keyring.contains("https://www.openstreetmap.org"),
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
            store.pending_keyring.contains("https://api06.dev.openstreetmap.org"),
            "an entry with no fallback secret must defer to the keyring, not read it here"
        );
        let token = store.tokens.get("https://api06.dev.openstreetmap.org").unwrap();
        assert_eq!(token.access_token, "", "placeholder until current_token() resolves it");
        assert_eq!(token.display_name, "bob", "non-secret metadata is available immediately");
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
        assert_eq!(store.pending_keyring.len(), 1, "only the keyring-only entry should be deferred");
        assert!(store.pending_keyring.contains("https://api06.dev.openstreetmap.org"));
        assert!(!store.pending_keyring.contains("https://www.openstreetmap.org"));
    }
}
