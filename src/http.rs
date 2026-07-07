//! Minimal HTTP client seam shared by every networking call site (osm_api, auth,
//! tile_cache, imagery) so request/retry logic can be tested against a scripted
//! `FakeClient` instead of hitting the real network. Modeled after the `AppHandle`
//! trait in `script::runner`: a trait for production code plus a fake for tests.

use std::time::Duration;

/// HTTP method used by a request. Only the methods the app actually needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Request body. `Form` is `application/x-www-form-urlencoded`, the only body shape
/// the app sends today (OAuth token exchange/refresh).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Form(Vec<(String, String)>),
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
    Status { status: u16, body: Vec<u8> },
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Transport(msg) => write!(f, "transport error: {}", msg),
            HttpError::Status { status, .. } => write!(f, "HTTP {}", status),
        }
    }
}

impl std::error::Error for HttpError {}

/// Seam over the concrete HTTP implementation. Production code uses `UreqClient`;
/// tests use `fake::FakeClient` to script responses without touching the network.
pub trait HttpClient: Send + Sync {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
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
        use std::io::Read;

        let mut builder = match req.method {
            Method::Get => ureq::get(&req.url),
            Method::Post => ureq::post(&req.url),
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
            None => builder.call(),
        };

        match result {
            Ok(resp) => {
                let status = resp.status();
                let mut body = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body)
                    .map_err(|e| HttpError::Transport(e.to_string()))?;
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
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match client.request(req.clone()) {
            Ok(resp) => return Ok(resp),
            Err(HttpError::Status { status, body }) => {
                if attempt < policy.max_attempts && crate::is_retryable_status(status) {
                    std::thread::sleep(policy.delays[(attempt - 1) as usize]);
                    continue;
                }
                return Err(HttpError::Status { status, body });
            }
            Err(e @ HttpError::Transport(_)) => {
                if attempt < policy.max_attempts {
                    std::thread::sleep(policy.delays[(attempt - 1) as usize]);
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Test double for `HttpClient`: returns scripted responses in order, one per call to
/// `request`. Public (not `#[cfg(test)]`-gated at the item level, only the module is)
/// so it can be used from the `#[cfg(test)]` modules of other files in this crate.
#[cfg(test)]
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
    fn retry_policy_none_makes_a_single_attempt() {
        let client = FakeClient::new(vec![status_err(503, "busy")]);
        let req = HttpRequest::get("https://example.test/x");
        let err = fetch_with_retries(&client, &req, &RetryPolicy::none()).unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 503, .. }));
        assert_eq!(client.request_count(), 1);
    }
}
