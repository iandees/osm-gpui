//! Minimal HTTP client seam shared by every networking call site (osm_api, auth,
//! tile_cache, imagery) so request/retry logic can be tested against a scripted
//! `FakeClient` instead of hitting the real network.

use std::time::Duration;

/// HTTP method used by a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
}

/// Request body. `Form` is `application/x-www-form-urlencoded` (OAuth token
/// exchange/refresh); `Text` is a raw string sent as-is, whose `Content-Type`
/// the caller sets with a header (OSM changeset XML).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Form(Vec<(String, String)>),
    Text(String),
}

/// An outgoing HTTP request, independent of any particular HTTP client implementation.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
    pub timeout: Duration,
}

impl HttpRequest {
    /// A `GET` request with the default 30s timeout and no extra headers.
    pub fn get(url: impl Into<String>) -> Self {
        HttpRequest {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(30),
        }
    }

    /// A `POST` request with a form-encoded body.
    pub fn post_form(url: impl Into<String>, form: Vec<(String, String)>) -> Self {
        HttpRequest {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body: Some(Body::Form(form)),
            timeout: Duration::from_secs(30),
        }
    }

    /// A request with a raw text body (`Content-Type` is the caller's to set).
    pub fn with_text(method: Method, url: impl Into<String>, text: Option<String>) -> Self {
        HttpRequest {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: text.map(Body::Text),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// Add an `Authorization: Bearer <token>` header.
    pub fn bearer(self, token: &str) -> Self {
        self.header("Authorization", format!("Bearer {}", token))
    }
}

/// A successful (2xx) HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn into_string(self) -> Result<String, HttpError> {
        String::from_utf8(self.body).map_err(|e| HttpError::Transport(e.to_string()))
    }
}

/// Failure to complete an HTTP request: either the transport itself failed (DNS,
/// connect, TLS, timeout, response-body I/O) or the server responded with a non-2xx
/// status.
#[derive(Debug, Clone)]
pub enum HttpError {
    Transport(String),
    Status {
        status: u16,
        body: Vec<u8>,
    },
    /// The caller's progress callback asked to stop while the body was being
    /// read. Never retried.
    Cancelled,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Cancelled => write!(f, "cancelled"),
            HttpError::Transport(msg) => write!(f, "transport error: {}", msg),
            HttpError::Status { status, .. } => write!(f, "HTTP {}", status),
        }
    }
}

impl std::error::Error for HttpError {}

/// An [`HttpError`] with its body decoded, the one place the status/transport
/// split and the lossy body conversion live. Module error enums implement
/// `From<HttpError>` on top of this instead of matching `HttpError` themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpFailure {
    /// The server answered with a non-2xx status.
    Status { status: u16, body: String },
    /// The request never completed (DNS, connect, TLS, timeout, body I/O).
    Transport(String),
    /// The caller's progress callback abandoned the transfer.
    Cancelled,
}

impl From<HttpError> for HttpFailure {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Status { status, body } => HttpFailure::Status {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            },
            HttpError::Transport(msg) => HttpFailure::Transport(msg),
            HttpError::Cancelled => HttpFailure::Cancelled,
        }
    }
}

/// Seam over the concrete HTTP implementation. Production code uses `UreqClient`;
/// tests use `fake::FakeClient` to script responses without touching the network.
pub trait HttpClient: Send + Sync {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;

    /// Like `request`, but reports the response body's progress as it is
    /// read: `on_progress(bytes_so_far, content_length)` where the length is
    /// `None` when the server didn't send one. Returning `false` abandons the
    /// transfer with `HttpError::Cancelled`. The default reads everything
    /// first and reports once at the end.
    fn request_with_progress(
        &self,
        req: HttpRequest,
        on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<HttpResponse, HttpError> {
        let resp = self.request(req)?;
        let len = resp.body.len() as u64;
        if on_progress(len, Some(len)) {
            Ok(resp)
        } else {
            Err(HttpError::Cancelled)
        }
    }
}

/// What `default_client` hands out until the binary installs a real client:
/// every request fails, so nothing built without one (tests, tools) can reach
/// the network by accident.
struct OfflineClient;

impl HttpClient for OfflineClient {
    fn request(&self, _req: HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Transport("no HTTP client installed".to_string()))
    }
}

static DEFAULT_CLIENT: std::sync::OnceLock<std::sync::Arc<dyn HttpClient>> =
    std::sync::OnceLock::new();

/// Install the process-wide client, once, at startup. Later calls are ignored.
/// It only seeds objects that cannot be handed a client (the `MapViewer`
/// constructor used by `cx.add_window`, the settings-change hook); code that
/// makes requests takes the client as a parameter.
pub fn install_default_client(client: std::sync::Arc<dyn HttpClient>) {
    let _ = DEFAULT_CLIENT.set(client);
}

/// The installed client, or one that fails every request when none is.
pub fn default_client() -> std::sync::Arc<dyn HttpClient> {
    DEFAULT_CLIENT
        .get()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(OfflineClient))
}

/// Cap on how much of an *error* response body is read into memory: error pages are
/// occasionally large HTML documents and the body is only ever used for diagnostics.
/// Successful responses (tile bytes, OSM XML, JSON) are read in full.
const MAX_ERROR_BODY_BYTES: u64 = 16 * 1024;

/// Production `HttpClient` backed by `ureq`. Sends the app-wide User-Agent
/// (`crate::USER_AGENT`) on every request so all outgoing HTTP identifies itself
/// consistently regardless of which module made the request.
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqClient;

impl UreqClient {
    pub fn new() -> Self {
        UreqClient
    }
}

impl HttpClient for UreqClient {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.request_with_progress(req, &mut |_, _| true)
    }

    fn request_with_progress(
        &self,
        req: HttpRequest,
        on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<HttpResponse, HttpError> {
        use crate::transfer::{is_read_cancelled, CountingReader};
        use std::io::Read;

        let mut builder = match req.method {
            Method::Get => ureq::get(&req.url),
            Method::Post => ureq::post(&req.url),
            Method::Put => ureq::put(&req.url),
        };
        builder = builder.set("User-Agent", crate::USER_AGENT);
        for (name, value) in &req.headers {
            builder = builder.set(name, value);
        }
        builder = builder.timeout(req.timeout);

        let result = match &req.body {
            Some(Body::Form(pairs)) => {
                let pairs: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                builder.send_form(&pairs)
            }
            Some(Body::Text(text)) => builder.send_string(text),
            None => builder.call(),
        };

        match result {
            Ok(resp) => {
                let status = resp.status();
                let total = resp
                    .header("Content-Length")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let mut body = Vec::new();
                CountingReader::new(resp.into_reader(), |n| on_progress(n, total))
                    .read_to_end(&mut body)
                    .map_err(|e| {
                        if is_read_cancelled(&e) {
                            HttpError::Cancelled
                        } else {
                            HttpError::Transport(e.to_string())
                        }
                    })?;
                Ok(HttpResponse { status, body })
            }
            Err(ureq::Error::Status(status, resp)) => {
                let mut body = Vec::new();
                let _ = resp
                    .into_reader()
                    .take(MAX_ERROR_BODY_BYTES)
                    .read_to_end(&mut body);
                Err(HttpError::Status { status, body })
            }
            Err(ureq::Error::Transport(t)) => Err(HttpError::Transport(t.to_string())),
        }
    }
}

/// Bounded retry schedule for a request: total attempts (including the first) and the
/// delay before each retry, indexed by (attempt number - 1).
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub delays: Vec<Duration>,
}

impl RetryPolicy {
    /// The retry policy shared by the OSM API and tile downloads: 3 attempts total,
    /// with 200ms/500ms delays between them.
    pub fn standard() -> Self {
        RetryPolicy {
            max_attempts: 3,
            delays: vec![Duration::from_millis(200), Duration::from_millis(500)],
        }
    }

    /// A single attempt, no retries.
    pub fn none() -> Self {
        RetryPolicy {
            max_attempts: 1,
            delays: Vec::new(),
        }
    }
}

/// Run `req` against `client`, retrying transport errors and retryable HTTP statuses
/// (`crate::is_retryable_status`) up to `policy.max_attempts` times with the delays in
/// `policy.delays`. Other error statuses are returned immediately since retrying won't
/// help.
pub fn fetch_with_retries(
    client: &dyn HttpClient,
    req: &HttpRequest,
    policy: &RetryPolicy,
) -> Result<HttpResponse, HttpError> {
    fetch_with_retries_progress(client, req, policy, &mut |_, _| true)
}

/// `fetch_with_retries`, reporting body progress through
/// `HttpClient::request_with_progress`. The byte count restarts from zero on
/// each attempt. A `false` from `on_progress` ends the fetch with
/// `HttpError::Cancelled` and is not retried.
pub fn fetch_with_retries_progress(
    client: &dyn HttpClient,
    req: &HttpRequest,
    policy: &RetryPolicy,
    on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<HttpResponse, HttpError> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match client.request_with_progress(req.clone(), &mut *on_progress) {
            Ok(resp) => return Ok(resp),
            Err(HttpError::Cancelled) => return Err(HttpError::Cancelled),
            Err(HttpError::Status { status, body }) => {
                if attempt < policy.max_attempts && crate::is_retryable_status(status) {
                    std::thread::sleep(policy.delays[(attempt - 1) as usize]);
                    continue;
                }
                return Err(HttpError::Status { status, body });
            }
            Err(e @ HttpError::Transport(_)) => {
                if attempt < policy.max_attempts && !is_denied(&e) {
                    std::thread::sleep(policy.delays[(attempt - 1) as usize]);
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Start of the message of every request an offline run refuses. Such a
/// refusal is final, so `fetch_with_retries_progress` does not retry it.
pub const DENIED_PREFIX: &str = "offline run:";

fn denied(url: &str) -> HttpError {
    HttpError::Transport(format!(
        "{DENIED_PREFIX} request to {} denied ({url})",
        url_host(url).unwrap_or("?")
    ))
}

fn is_denied(e: &HttpError) -> bool {
    matches!(e, HttpError::Transport(m) if m.starts_with(DENIED_PREFIX))
}

/// The host of an `http(s)` URL, lowercased and without port or userinfo.
fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

/// The path of an `http(s)` URL, from its first `/` up to any query or
/// fragment (`/` when there is none).
fn url_path(url: &str) -> &str {
    let Some((_, rest)) = url.split_once("://") else {
        return "/";
    };
    let after_authority = &rest[rest.find(['/', '?', '#']).unwrap_or(rest.len())..];
    if !after_authority.starts_with('/') {
        return "/";
    }
    after_authority.split(['?', '#']).next().unwrap_or("/")
}

/// Which requests a [`Rule`] applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteMatch {
    /// The URL starts with this text.
    Prefix(String),
    /// The URL's host equals `host` (case-insensitive, any port), and its
    /// path starts with `path_prefix` when one is given.
    Host {
        host: String,
        path_prefix: Option<String>,
    },
}

impl RouteMatch {
    pub fn matches(&self, url: &str) -> bool {
        match self {
            RouteMatch::Prefix(p) => url.starts_with(p.as_str()),
            RouteMatch::Host { host, path_prefix } => {
                url_host(url).is_some_and(|h| h.eq_ignore_ascii_case(host))
                    && path_prefix
                        .as_deref()
                        .is_none_or(|p| url_path(url).starts_with(p))
            }
        }
    }
}

type CustomHandler = dyn Fn(&HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync;

/// How a [`Rule`] answers a request it matched.
#[derive(Clone)]
pub enum RouteHandler {
    /// Serve the file's bytes with status 200. Read on every request, so a
    /// missing file fails that request, not the whole run.
    File(std::path::PathBuf),
    /// A fixed answer: a 2xx status is a response, anything else an
    /// `HttpError::Status`.
    Fixed { status: u16, body: Vec<u8> },
    /// Refuse the request like an offline run does.
    Deny,
    /// Compute the answer from the request.
    Custom(std::sync::Arc<CustomHandler>),
}

impl std::fmt::Debug for RouteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteHandler::File(p) => write!(f, "File({})", p.display()),
            RouteHandler::Fixed { status, .. } => write!(f, "Fixed({status})"),
            RouteHandler::Deny => write!(f, "Deny"),
            RouteHandler::Custom(_) => write!(f, "Custom"),
        }
    }
}

impl RouteHandler {
    fn answer(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        match self {
            RouteHandler::File(path) => std::fs::read(path)
                .map(|body| HttpResponse { status: 200, body })
                .map_err(|e| {
                    HttpError::Transport(format!(
                        "fixture file {} for {} unreadable: {e}",
                        path.display(),
                        req.url
                    ))
                }),
            RouteHandler::Fixed { status, body } if (200..300).contains(status) => {
                Ok(HttpResponse {
                    status: *status,
                    body: body.clone(),
                })
            }
            RouteHandler::Fixed { status, body } => Err(HttpError::Status {
                status: *status,
                body: body.clone(),
            }),
            RouteHandler::Deny => Err(denied(&req.url)),
            RouteHandler::Custom(f) => f(req),
        }
    }
}

/// One routing rule: requests matching `matcher` are answered by `handler`.
#[derive(Debug, Clone)]
pub struct Rule {
    pub matcher: RouteMatch,
    pub handler: RouteHandler,
}

/// Refuses every request, naming the host and that the run is offline.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAllClient;

impl HttpClient for DenyAllClient {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(denied(&req.url))
    }
}

/// An ordered list of [`Rule`]s over a fallback client: the first rule that
/// matches answers, and a request no rule matches goes to the fallback.
pub struct RoutedClient {
    rules: Vec<Rule>,
    fallback: std::sync::Arc<dyn HttpClient>,
}

impl RoutedClient {
    pub fn new(rules: Vec<Rule>, fallback: std::sync::Arc<dyn HttpClient>) -> Self {
        RoutedClient { rules, fallback }
    }

    /// Rules over a fallback that denies everything they do not answer.
    pub fn offline(rules: Vec<Rule>) -> Self {
        Self::new(rules, std::sync::Arc::new(DenyAllClient))
    }
}

impl HttpClient for RoutedClient {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        match self.rules.iter().find(|r| r.matcher.matches(&req.url)) {
            Some(rule) => rule.handler.answer(&req),
            None => self.fallback.request(req),
        }
    }

    fn request_with_progress(
        &self,
        req: HttpRequest,
        on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<HttpResponse, HttpError> {
        match self.rules.iter().find(|r| r.matcher.matches(&req.url)) {
            Some(rule) => {
                let resp = rule.handler.answer(&req)?;
                let len = resp.body.len() as u64;
                if on_progress(len, Some(len)) {
                    Ok(resp)
                } else {
                    Err(HttpError::Cancelled)
                }
            }
            None => self.fallback.request_with_progress(req, on_progress),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    rules: Vec<ManifestRule>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestMatch {
    prefix: Option<String>,
    host: Option<String>,
    path_prefix: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    #[serde(rename = "match")]
    matcher: ManifestMatch,
    file: Option<String>,
    status: Option<u16>,
    body: Option<String>,
    deny: Option<bool>,
}

/// Parse a fixtures manifest:
///
/// `{"rules": [{"match": {"prefix": URL} | {"host": H, "path_prefix": P?},
///   "file": PATH | "status": N, "body": TEXT? | "deny": true}]}`
///
/// Files are resolved against `base_dir` (the manifest's directory). Rules
/// keep their order, which is their precedence.
pub fn parse_manifest(text: &str, base_dir: &std::path::Path) -> Result<Vec<Rule>, String> {
    let manifest: ManifestFile =
        serde_json::from_str(text).map_err(|e| format!("invalid fixtures manifest: {e}"))?;
    manifest
        .rules
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let at = |msg: &str| format!("fixtures manifest rule {}: {msg}", i + 1);
            let matcher = match (r.matcher.prefix, r.matcher.host, r.matcher.path_prefix) {
                (Some(p), None, None) => RouteMatch::Prefix(p),
                (None, Some(host), path_prefix) => RouteMatch::Host { host, path_prefix },
                (None, None, _) => return Err(at("\"match\" needs \"prefix\" or \"host\"")),
                (Some(_), _, _) => {
                    return Err(at(
                        "\"prefix\" cannot be combined with \"host\" or \"path_prefix\"",
                    ))
                }
            };
            let handler = match (r.file, r.status, r.body, r.deny) {
                (Some(file), None, None, None | Some(false)) => {
                    RouteHandler::File(base_dir.join(file))
                }
                (None, Some(status), body, None | Some(false)) => RouteHandler::Fixed {
                    status,
                    body: body.unwrap_or_default().into_bytes(),
                },
                (None, None, None, Some(true)) => RouteHandler::Deny,
                _ => {
                    return Err(at(
                        "needs exactly one of \"file\", \"status\" (with optional \"body\") or \"deny\": true",
                    ))
                }
            };
            Ok(Rule { matcher, handler })
        })
        .collect()
}

/// Read and parse the manifest at `path`, and check that every file it names
/// exists, so a typo fails at startup instead of as a missing tile later.
pub fn load_manifest(path: &std::path::Path) -> Result<Vec<Rule>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read fixtures manifest {}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(std::path::Path::new("."));
    let rules = parse_manifest(&text, base)?;
    for rule in &rules {
        if let RouteHandler::File(f) = &rule.handler {
            if !f.is_file() {
                return Err(format!(
                    "fixtures manifest {} names a missing file: {}",
                    path.display(),
                    f.display()
                ));
            }
        }
    }
    Ok(rules)
}

/// The client a `--script` run uses. `real` answers whatever `rules` do not,
/// unless the run is `offline`, in which case everything else is denied.
pub fn script_client(
    offline: bool,
    rules: Vec<Rule>,
    real: std::sync::Arc<dyn HttpClient>,
) -> std::sync::Arc<dyn HttpClient> {
    if offline {
        std::sync::Arc::new(RoutedClient::offline(rules))
    } else if rules.is_empty() {
        real
    } else {
        std::sync::Arc::new(RoutedClient::new(rules, real))
    }
}

/// Test double for `HttpClient`: returns scripted responses in order, one per call to
/// `request`. Compiled in non-test builds too because the binary's tests link this
/// library built without `cfg(test)`.
#[doc(hidden)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeClient {
        responses: Mutex<Vec<Result<HttpResponse, HttpError>>>,
        pub requests: Mutex<Vec<HttpRequest>>,
    }

    impl FakeClient {
        /// `responses` are consumed in order, oldest first, one per `request` call.
        pub fn new(responses: Vec<Result<HttpResponse, HttpError>>) -> Self {
            FakeClient {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl HttpClient for FakeClient {
        fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
            self.requests.lock().unwrap().push(req);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                panic!("FakeClient: no more scripted responses");
            }
            responses.remove(0)
        }
    }

    pub fn ok(status: u16, body: impl Into<Vec<u8>>) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status,
            body: body.into(),
        })
    }

    pub fn status_err(status: u16, body: impl Into<Vec<u8>>) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Status {
            status,
            body: body.into(),
        })
    }

    pub fn transport_err(msg: &str) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Transport(msg.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    #[test]
    fn failures_split_into_status_transport_and_cancelled() {
        assert_eq!(
            HttpFailure::from(HttpError::Status {
                status: 418,
                body: b"teapot \xff".to_vec(),
            }),
            HttpFailure::Status {
                status: 418,
                body: "teapot \u{fffd}".to_string(),
            }
        );
        assert_eq!(
            HttpFailure::from(HttpError::Transport("reset".into())),
            HttpFailure::Transport("reset".into())
        );
        assert_eq!(
            HttpFailure::from(HttpError::Cancelled),
            HttpFailure::Cancelled
        );
    }

    #[test]
    fn the_default_client_fails_every_request_until_one_is_installed() {
        let err = default_client()
            .request(HttpRequest::get("https://example.invalid/"))
            .unwrap_err();
        assert!(matches!(err, HttpError::Transport(_)));
    }

    #[test]
    fn retries_then_succeeds_on_retryable_status() {
        let client = FakeClient::new(vec![status_err(503, "busy"), ok(200, "done")]);
        let req = HttpRequest::get("https://example.test/x");
        let resp = fetch_with_retries(&client, &req, &RetryPolicy::standard()).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"done");
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn does_not_retry_non_retryable_status() {
        let client = FakeClient::new(vec![status_err(400, "bad request")]);
        let req = HttpRequest::get("https://example.test/x");
        let err = fetch_with_retries(&client, &req, &RetryPolicy::standard()).unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 400, .. }));
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn exhausts_retries_and_returns_last_error() {
        let client = FakeClient::new(vec![
            status_err(503, "1"),
            status_err(503, "2"),
            status_err(503, "3"),
        ]);
        let req = HttpRequest::get("https://example.test/x");
        let err = fetch_with_retries(&client, &req, &RetryPolicy::standard()).unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 503, .. }));
        assert_eq!(client.request_count(), 3);
    }

    #[test]
    fn retries_transport_errors() {
        let client = FakeClient::new(vec![transport_err("dns"), ok(200, "done")]);
        let req = HttpRequest::get("https://example.test/x");
        let resp = fetch_with_retries(&client, &req, &RetryPolicy::standard()).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn progress_is_reported_and_cancel_is_not_retried() {
        let client = FakeClient::new(vec![ok(200, "twelve bytes")]);
        let req = HttpRequest::get("https://example.test/x");
        let mut seen = Vec::new();
        let resp =
            fetch_with_retries_progress(&client, &req, &RetryPolicy::standard(), &mut |n, t| {
                seen.push((n, t));
                true
            })
            .unwrap();
        assert_eq!(resp.body, b"twelve bytes");
        assert_eq!(seen, vec![(12, Some(12))]);

        let client = FakeClient::new(vec![ok(200, "x"), ok(200, "x")]);
        let err =
            fetch_with_retries_progress(&client, &req, &RetryPolicy::standard(), &mut |_, _| false)
                .unwrap_err();
        assert!(matches!(err, HttpError::Cancelled));
        assert_eq!(client.request_count(), 1);
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("osm-gpui-http-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn prefix(p: &str) -> RouteMatch {
        RouteMatch::Prefix(p.to_string())
    }

    fn fixed(status: u16, body: &str) -> RouteHandler {
        RouteHandler::Fixed {
            status,
            body: body.as_bytes().to_vec(),
        }
    }

    fn fallback() -> std::sync::Arc<FakeClient> {
        std::sync::Arc::new(FakeClient::new(vec![ok(200, "fallback")]))
    }

    #[test]
    fn url_parts_ignore_port_userinfo_query_and_case() {
        assert_eq!(
            url_host("https://User@Tile.Example:8080/a/b?q=1"),
            Some("Tile.Example")
        );
        assert_eq!(url_host("https://example.test"), Some("example.test"));
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_path("https://example.test/a/b?q=/x"), "/a/b");
        assert_eq!(url_path("https://example.test?q=/x"), "/");
        assert_eq!(url_path("https://example.test"), "/");
    }

    #[test]
    fn host_rules_match_any_path_unless_a_prefix_is_given() {
        let any = RouteMatch::Host {
            host: "tile.example.test".into(),
            path_prefix: None,
        };
        assert!(any.matches("https://TILE.example.test:443/1/2/3.png"));
        assert!(!any.matches("https://other.example.test/1/2/3.png"));
        assert!(!any.matches("https://evil.test/?h=tile.example.test"));
        let scoped = RouteMatch::Host {
            host: "tile.example.test".into(),
            path_prefix: Some("/api/".into()),
        };
        assert!(scoped.matches("https://tile.example.test/api/4/x"));
        assert!(!scoped.matches("https://tile.example.test/other"));
    }

    #[test]
    fn the_first_matching_rule_wins_and_unmatched_requests_fall_back() {
        let fb = fallback();
        let client = RoutedClient::new(
            vec![
                Rule {
                    matcher: prefix("https://a.test/special/"),
                    handler: fixed(200, "special"),
                },
                Rule {
                    matcher: prefix("https://a.test/"),
                    handler: fixed(404, "gone"),
                },
            ],
            fb.clone(),
        );
        let resp = client
            .request(HttpRequest::get("https://a.test/special/x"))
            .unwrap();
        assert_eq!(resp.body, b"special");
        let err = client
            .request(HttpRequest::get("https://a.test/y"))
            .unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 404, ref body } if body == b"gone"));
        assert_eq!(fb.request_count(), 0);
        let resp = client.request(HttpRequest::get("https://b.test/")).unwrap();
        assert_eq!(resp.body, b"fallback");
        assert_eq!(fb.request_count(), 1);
    }

    #[test]
    fn a_file_rule_serves_the_file_and_reports_a_missing_one() {
        let dir = temp_dir("file-rule");
        std::fs::write(dir.join("t.png"), b"bytes").unwrap();
        let client = RoutedClient::offline(vec![
            Rule {
                matcher: prefix("https://a.test/ok"),
                handler: RouteHandler::File(dir.join("t.png")),
            },
            Rule {
                matcher: prefix("https://a.test/missing"),
                handler: RouteHandler::File(dir.join("nope.png")),
            },
        ]);
        let resp = client
            .request(HttpRequest::get("https://a.test/ok"))
            .unwrap();
        assert_eq!((resp.status, resp.body), (200, b"bytes".to_vec()));
        let err = client
            .request(HttpRequest::get("https://a.test/missing"))
            .unwrap_err();
        assert!(err.to_string().contains("nope.png"), "{err}");
    }

    #[test]
    fn a_deny_rule_beats_a_later_serving_rule() {
        let client = RoutedClient::offline(vec![
            Rule {
                matcher: prefix("https://a.test/private"),
                handler: RouteHandler::Deny,
            },
            Rule {
                matcher: prefix("https://a.test/"),
                handler: fixed(200, "ok"),
            },
        ]);
        assert!(client
            .request(HttpRequest::get("https://a.test/private/1"))
            .is_err());
        assert!(client
            .request(HttpRequest::get("https://a.test/public"))
            .is_ok());
    }

    #[test]
    fn the_offline_fallback_names_the_host_and_is_not_retried() {
        let client = RoutedClient::offline(Vec::new());
        let err = client
            .request(HttpRequest::get("https://api.example.test/api/0.6/map"))
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("offline run"), "{text}");
        assert!(text.contains("api.example.test"), "{text}");
        assert!(matches!(err, HttpError::Transport(_)));

        struct Counting(std::sync::atomic::AtomicUsize);
        impl HttpClient for Counting {
            fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                DenyAllClient.request(req)
            }
        }
        let counting = Counting(Default::default());
        let started = std::time::Instant::now();
        let err = fetch_with_retries(
            &counting,
            &HttpRequest::get("https://x.test/"),
            &RetryPolicy::standard(),
        )
        .unwrap_err();
        assert!(is_denied(&err));
        assert_eq!(counting.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn a_rule_answer_reports_progress_and_honours_cancel() {
        let client = RoutedClient::offline(vec![Rule {
            matcher: prefix("https://a.test/"),
            handler: fixed(200, "abcd"),
        }]);
        let mut seen = Vec::new();
        client
            .request_with_progress(HttpRequest::get("https://a.test/"), &mut |n, t| {
                seen.push((n, t));
                true
            })
            .unwrap();
        assert_eq!(seen, vec![(4, Some(4))]);
        let err = client
            .request_with_progress(HttpRequest::get("https://a.test/"), &mut |_, _| false)
            .unwrap_err();
        assert!(matches!(err, HttpError::Cancelled));
    }

    #[test]
    fn manifests_resolve_files_against_their_directory() {
        let rules = parse_manifest(
            r#"{"rules": [
                {"match": {"host": "tile.example.test"}, "file": "blank.png"},
                {"match": {"prefix": "https://a.test/x"}, "status": 404, "body": "no"},
                {"match": {"host": "b.test", "path_prefix": "/p"}, "deny": true}
            ]}"#,
            std::path::Path::new("/fx/dir"),
        )
        .unwrap();
        assert_eq!(rules.len(), 3);
        assert!(
            matches!(&rules[0].handler, RouteHandler::File(p) if p == std::path::Path::new("/fx/dir/blank.png"))
        );
        assert!(matches!(
            &rules[1].handler,
            RouteHandler::Fixed { status: 404, .. }
        ));
        assert!(matches!(rules[2].handler, RouteHandler::Deny));
        assert_eq!(
            rules[2].matcher,
            RouteMatch::Host {
                host: "b.test".into(),
                path_prefix: Some("/p".into())
            }
        );
    }

    #[test]
    fn manifests_reject_unknown_keys_and_ambiguous_rules() {
        let dir = std::path::Path::new(".");
        let err = parse_manifest(r#"{"rules": [], "extra": 1}"#, dir).unwrap_err();
        assert!(err.contains("unknown field `extra`"), "{err}");
        let err = parse_manifest(
            r#"{"rules": [{"match": {"host": "a"}, "file": "f", "fiel": 1}]}"#,
            dir,
        )
        .unwrap_err();
        assert!(err.contains("unknown field `fiel`"), "{err}");
        let err = parse_manifest(r#"{"rules": [{"match": {}, "deny": true}]}"#, dir).unwrap_err();
        assert!(err.contains("rule 1") && err.contains("prefix"), "{err}");
        let err = parse_manifest(
            r#"{"rules": [{"match": {"host": "a"}, "file": "f", "deny": true}]}"#,
            dir,
        )
        .unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        let err = parse_manifest(
            r#"{"rules": [{"match": {"prefix": "https://a", "host": "a"}, "deny": true}]}"#,
            dir,
        )
        .unwrap_err();
        assert!(err.contains("cannot be combined"), "{err}");
    }

    #[test]
    fn load_manifest_checks_that_named_files_exist() {
        let dir = temp_dir("manifest");
        let manifest = dir.join("m.json");
        std::fs::write(
            &manifest,
            r#"{"rules": [{"match": {"host": "a.test"}, "file": "blank.png"}]}"#,
        )
        .unwrap();
        let err = load_manifest(&manifest).unwrap_err();
        assert!(
            err.contains("missing file") && err.contains("blank.png"),
            "{err}"
        );
        std::fs::write(dir.join("blank.png"), b"x").unwrap();
        assert_eq!(load_manifest(&manifest).unwrap().len(), 1);
        assert!(load_manifest(&dir.join("absent.json")).is_err());
    }

    #[test]
    fn the_checked_in_offline_manifest_loads_and_serves_a_tile() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/screenshots/fixtures/offline/http-fixtures.json");
        let client = RoutedClient::offline(load_manifest(&path).unwrap());
        let tile = client
            .request(HttpRequest::get("https://tile.openstreetmap.org/3/4/2.png"))
            .unwrap();
        assert!(tile.body.starts_with(b"\x89PNG"));
        assert!(client
            .request(HttpRequest::get("https://example.test/"))
            .is_err());
    }

    #[test]
    fn script_client_only_routes_when_something_asks_for_it() {
        let fb = fallback();
        let plain = script_client(false, Vec::new(), fb.clone());
        assert_eq!(
            plain
                .request(HttpRequest::get("https://a.test/"))
                .unwrap()
                .body,
            b"fallback"
        );
        let offline = script_client(true, Vec::new(), fb.clone());
        assert!(offline
            .request(HttpRequest::get("https://a.test/"))
            .is_err());
        assert_eq!(fb.request_count(), 1);
    }

    #[test]
    fn retry_policy_none_makes_a_single_attempt() {
        let client = FakeClient::new(vec![status_err(503, "busy")]);
        let req = HttpRequest::get("https://example.test/x");
        let err = fetch_with_retries(&client, &req, &RetryPolicy::none()).unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 503, .. }));
        assert_eq!(client.request_count(), 1);
    }
}
