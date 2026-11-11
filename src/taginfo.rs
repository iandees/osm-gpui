//! Tag suggestions from taginfo (https://taginfo.openstreetmap.org), the
//! supplement to the loaded data and the bundled schema in `crate::tag_suggest`.
//!
//! This module holds everything that can be tested without a window:
//! deciding whether a request may be made at all (`plan`), building the URL,
//! parsing the answer, and `TaginfoState`, the debounce / stale-response /
//! cache / back-off bookkeeping the UI drives.
//!
//! API (taginfo API v4, public, no key; confirmed against the API
//! documentation at https://taginfo.openstreetmap.org/taginfo/apidoc):
//!
//! * keys:   `GET /api/4/keys/all?query=PREFIX&sortname=count_all&sortorder=desc&rp=20&page=1`
//!   returning `data: [{"key": ..., "count_all": ...}]`
//! * values: `GET /api/4/key/values?key=KEY&query=PREFIX&sortname=count_all&sortorder=desc&rp=20&page=1`
//!   returning `data: [{"value": ..., "count": ...}]`
//!
//! `query` is a substring match on both.

use crate::http::{HttpClient, HttpError, HttpRequest};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const API_BASE: &str = "https://taginfo.openstreetmap.org/api/4";

/// How long typing must pause before a request goes out.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// Keys shorter than this (in characters) are never sent.
pub const MIN_KEY_CHARS: usize = 2;
/// Values shorter than this (in characters) are never sent.
pub const MIN_VALUE_CHARS: usize = 1;
/// Longest text ever sent.
const MAX_QUERY_CHARS: usize = 64;
const CACHE_CAPACITY: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// One suggestion returned by taginfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaginfoItem {
    pub text: String,
    pub count: u64,
}

/// Keys whose values are free text or personal, so what the user types for
/// them is never sent to taginfo (the value input only suggests from the
/// loaded data for these). Exact keys, then key prefixes (`name:` covers
/// every translation, and so on).
const FREE_TEXT_KEYS: &[&str] = &[
    "name",
    "alt_name",
    "old_name",
    "short_name",
    "official_name",
    "loc_name",
    "int_name",
    "nat_name",
    "reg_name",
    "description",
    "note",
    "fixme",
    "FIXME",
    "todo",
    "comment",
    "operator",
    "brand",
    "network",
    "ref",
    "phone",
    "fax",
    "email",
    "website",
    "url",
    "image",
    "wikidata",
    "wikipedia",
    "start_date",
    "end_date",
    "check_date",
    "opening_hours",
    "collection_times",
    "service_times",
    "inscription",
    "source",
    "created_by",
];
const FREE_TEXT_PREFIXES: &[&str] = &[
    "name:",
    "alt_name:",
    "old_name:",
    "addr:",
    "contact:",
    "description:",
    "note:",
    "fixme:",
    "operator:",
    "brand:",
    "ref:",
    "source:",
    "wikipedia:",
    "wikidata:",
    "website:",
    "phone:",
    "image:",
    "check_date:",
    "opening_hours:",
];

/// Whether `key` holds free text or personal data (see `FREE_TEXT_KEYS`).
pub fn is_free_text_key(key: &str) -> bool {
    FREE_TEXT_KEYS.contains(&key) || FREE_TEXT_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// A lookup to make.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaginfoQuery {
    Keys { prefix: String },
    Values { key: String, prefix: String },
}

impl TaginfoQuery {
    /// The request URL. Every part is percent-encoded (keys contain `:`,
    /// and text can be any language).
    pub fn url(&self) -> String {
        use urlencoding::encode;
        match self {
            TaginfoQuery::Keys { prefix } => format!(
                "{API_BASE}/keys/all?query={}&sortname=count_all&sortorder=desc&rp=20&page=1",
                encode(prefix)
            ),
            TaginfoQuery::Values { key, prefix } => format!(
                "{API_BASE}/key/values?key={}&query={}&sortname=count_all&sortorder=desc&rp=20&page=1",
                encode(key),
                encode(prefix)
            ),
        }
    }
}

/// Which input a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Key,
    Value,
}

/// The request to make for the text `typed` in a key or value input, or
/// `None` when nothing may be sent: the setting is off, the text is too
/// short or odd, or (for a value) the key holds free text.
///
/// `key` is the row's key (only used for values). This is the single gate
/// every request goes through.
pub fn plan(enabled: bool, target: Target, key: &str, typed: &str) -> Option<TaginfoQuery> {
    if !enabled {
        return None;
    }
    let typed = typed.trim();
    let chars = typed.chars().count();
    if chars > MAX_QUERY_CHARS || typed.chars().any(char::is_control) {
        return None;
    }
    match target {
        Target::Key => (chars >= MIN_KEY_CHARS).then(|| TaginfoQuery::Keys {
            prefix: typed.to_string(),
        }),
        Target::Value => {
            let key = key.trim();
            if key.is_empty()
                || key.chars().count() > MAX_QUERY_CHARS
                || key.chars().any(char::is_control)
                || is_free_text_key(key)
                || chars < MIN_VALUE_CHARS
            {
                return None;
            }
            Some(TaginfoQuery::Values {
                key: key.to_string(),
                prefix: typed.to_string(),
            })
        }
    }
}

/// Why a lookup produced nothing.
#[derive(Debug)]
pub enum FetchError {
    Http(HttpError),
    BadResponse,
}

/// Run `query` against `client` (one attempt; no retries: taginfo is a
/// courtesy, not a dependency).
pub fn fetch(
    client: &dyn HttpClient,
    query: &TaginfoQuery,
) -> Result<Vec<TaginfoItem>, FetchError> {
    let mut req = HttpRequest::get(query.url()).header("Accept", "application/json");
    req.timeout = REQUEST_TIMEOUT;
    let body = client
        .request(req)
        .map_err(FetchError::Http)?
        .into_string()
        .map_err(FetchError::Http)?;
    parse_response(query, &body).ok_or(FetchError::BadResponse)
}

/// Parse a taginfo answer. `None` if it is not the expected JSON; entries
/// that are empty or unreasonably long are skipped.
pub fn parse_response(query: &TaginfoQuery, body: &str) -> Option<Vec<TaginfoItem>> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = json.get("data")?.as_array()?;
    let (text_field, count_field) = match query {
        TaginfoQuery::Keys { .. } => ("key", "count_all"),
        TaginfoQuery::Values { .. } => ("value", "count"),
    };
    let items = data
        .iter()
        .filter_map(|entry| {
            let text = entry.get(text_field)?.as_str()?;
            if text.is_empty() || text.chars().count() > 255 || text.chars().any(char::is_control) {
                return None;
            }
            Some(TaginfoItem {
                text: text.to_string(),
                count: entry.get(count_field).and_then(|c| c.as_u64()).unwrap_or(0),
            })
        })
        .collect();
    Some(items)
}

/// What `TaginfoState::request` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Nothing to ask (or nothing allowed right now).
    Idle,
    /// Answered from the session cache.
    Cached(Vec<TaginfoItem>),
    /// A request will be due after the debounce; call `poll` then.
    Pending,
}

/// A request that `poll` handed out. Pass it back to `finish` with the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub generation: u64,
    pub query: TaginfoQuery,
}

struct Pending {
    query: TaginfoQuery,
    due: Instant,
}

/// Debounce, cancellation, cache and back-off for taginfo lookups. Time is
/// passed in, so all of it is testable. At most one request is in flight.
pub struct TaginfoState {
    cache: VecDeque<(TaginfoQuery, Vec<TaginfoItem>)>,
    pending: Option<Pending>,
    in_flight: bool,
    /// Bumped by every `request`; a `finish` for an older generation is stale.
    generation: u64,
    failures: u32,
    backoff_until: Option<Instant>,
}

impl Default for TaginfoState {
    fn default() -> Self {
        Self::new()
    }
}

impl TaginfoState {
    pub fn new() -> Self {
        TaginfoState {
            cache: VecDeque::new(),
            pending: None,
            in_flight: false,
            generation: 0,
            failures: 0,
            backoff_until: None,
        }
    }

    /// Note that the input now wants `query` (`None`: it wants nothing, e.g.
    /// the input closed or the text is too short). Supersedes any earlier
    /// wish: a request already in flight will have its answer cached but not
    /// shown.
    pub fn request(&mut self, now: Instant, query: Option<TaginfoQuery>) -> Lookup {
        self.generation += 1;
        self.pending = None;
        let Some(query) = query else {
            return Lookup::Idle;
        };
        if let Some(pos) = self.cache.iter().position(|(q, _)| *q == query) {
            let entry = self.cache.remove(pos).expect("position is valid");
            let items = entry.1.clone();
            self.cache.push_back(entry);
            return Lookup::Cached(items);
        }
        if self.backing_off(now) {
            return Lookup::Idle;
        }
        self.pending = Some(Pending {
            query,
            due: now + DEBOUNCE,
        });
        Lookup::Pending
    }

    /// How long until `poll` could hand out a job, if one is waiting and
    /// nothing is in flight.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        if self.in_flight {
            return None;
        }
        let due = self.pending.as_ref()?.due;
        let due = self.backoff_until.map_or(due, |b| b.max(due));
        Some(due.saturating_duration_since(now))
    }

    /// The request to make now, if the debounce has elapsed and nothing else
    /// is in flight.
    pub fn poll(&mut self, now: Instant) -> Option<Job> {
        if self.in_flight || self.backing_off(now) {
            return None;
        }
        if self.pending.as_ref()?.due > now {
            return None;
        }
        let Pending { query, .. } = self.pending.take()?;
        self.in_flight = true;
        Some(Job {
            generation: self.generation,
            query,
        })
    }

    /// Record the outcome of `job`. Returns the items when they are still
    /// wanted (the input has not asked for something else since); a stale
    /// answer is cached and dropped. Errors start (or lengthen) a back-off.
    pub fn finish(
        &mut self,
        now: Instant,
        job: Job,
        result: Result<Vec<TaginfoItem>, FetchError>,
    ) -> Option<Vec<TaginfoItem>> {
        self.in_flight = false;
        let current = job.generation == self.generation;
        match result {
            Ok(items) => {
                self.failures = 0;
                self.backoff_until = None;
                self.cache.retain(|(q, _)| *q != job.query);
                self.cache.push_back((job.query, items.clone()));
                while self.cache.len() > CACHE_CAPACITY {
                    self.cache.pop_front();
                }
                current.then_some(items)
            }
            Err(_) => {
                self.failures += 1;
                let secs = (10u64 << (self.failures - 1).min(5)).min(300);
                self.backoff_until = Some(now + Duration::from_secs(secs));
                // What was wanted meanwhile is dropped rather than retried
                // into the back-off.
                self.pending = None;
                None
            }
        }
    }

    fn backing_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|b| now < b)
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    fn keys(p: &str) -> TaginfoQuery {
        TaginfoQuery::Keys { prefix: p.into() }
    }

    fn item(t: &str, c: u64) -> TaginfoItem {
        TaginfoItem {
            text: t.into(),
            count: c,
        }
    }

    #[test]
    fn key_url() {
        assert_eq!(
            keys("sur").url(),
            "https://taginfo.openstreetmap.org/api/4/keys/all?query=sur&sortname=count_all&sortorder=desc&rp=20&page=1"
        );
    }

    #[test]
    fn urls_are_percent_encoded() {
        let q = TaginfoQuery::Values {
            key: "addr:street".into(),
            prefix: "Straße & Co".into(),
        };
        assert_eq!(
            q.url(),
            "https://taginfo.openstreetmap.org/api/4/key/values?key=addr%3Astreet&query=Stra%C3%9Fe%20%26%20Co&sortname=count_all&sortorder=desc&rp=20&page=1"
        );
        assert!(keys("名前:ja")
            .url()
            .contains("query=%E5%90%8D%E5%89%8D%3Aja"));
    }

    #[test]
    fn parses_key_and_value_answers() {
        let body = r#"{"total":2,"data":[{"key":"surface","count_all":123,"in_wiki":true},{"key":"","count_all":1},{"count_all":5}]}"#;
        assert_eq!(
            parse_response(&keys("s"), body),
            Some(vec![item("surface", 123)])
        );
        let q = TaginfoQuery::Values {
            key: "surface".into(),
            prefix: "a".into(),
        };
        let body = r#"{"data":[{"value":"asphalt","count":99,"fraction":0.5},{"value":"x"}]}"#;
        assert_eq!(
            parse_response(&q, body),
            Some(vec![item("asphalt", 99), item("x", 0)])
        );
        assert_eq!(parse_response(&q, "not json"), None);
        assert_eq!(parse_response(&q, "{}"), None);
    }

    #[test]
    fn fetch_requests_the_url_and_parses() {
        let client = FakeClient::new(vec![ok(
            200,
            r#"{"data":[{"key":"surface","count_all":7}]}"#,
        )]);
        let got = fetch(&client, &keys("sur")).unwrap();
        assert_eq!(got, vec![item("surface", 7)]);
        let reqs = client.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url, keys("sur").url());
        assert!(reqs[0].timeout <= Duration::from_secs(10));
    }

    #[test]
    fn fetch_reports_failures() {
        let client = FakeClient::new(vec![status_err(503, ""), ok(200, "<html>")]);
        assert!(matches!(
            fetch(&client, &keys("su")),
            Err(FetchError::Http(_))
        ));
        assert!(matches!(
            fetch(&client, &keys("su")),
            Err(FetchError::BadResponse)
        ));
    }

    #[test]
    fn plan_requires_the_setting_and_enough_text() {
        assert_eq!(plan(false, Target::Key, "", "surface"), None);
        assert_eq!(plan(false, Target::Value, "surface", "asp"), None);
        assert_eq!(plan(true, Target::Key, "", "s"), None);
        assert_eq!(plan(true, Target::Key, "", " s "), None);
        assert_eq!(plan(true, Target::Key, "", "su"), Some(keys("su")));
        assert_eq!(plan(true, Target::Value, "", "asp"), None);
        assert_eq!(plan(true, Target::Value, "surface", ""), None);
        assert_eq!(
            plan(true, Target::Value, "surface", "asp"),
            Some(TaginfoQuery::Values {
                key: "surface".into(),
                prefix: "asp".into()
            })
        );
    }

    #[test]
    fn free_text_keys_never_send_values() {
        for key in [
            "name",
            "name:de",
            "addr:street",
            "phone",
            "contact:email",
            "email",
            "website",
            "url",
            "description",
            "note",
            "fixme",
            "operator",
            "brand",
            "ref",
            "wikidata",
            "wikipedia",
            "start_date",
            "opening_hours",
            "source",
            "source:geometry",
        ] {
            assert!(is_free_text_key(key), "{key}");
            assert_eq!(plan(true, Target::Value, key, "abc"), None, "{key}");
        }
        assert!(!is_free_text_key("surface"));
        assert!(!is_free_text_key("highway"));
        // ...but typing such a key's name is a key lookup like any other.
        assert!(plan(true, Target::Key, "", "name").is_some());
    }

    #[test]
    fn debounces_and_only_the_last_query_is_sent() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        assert_eq!(s.request(t0, Some(keys("su"))), Lookup::Pending);
        assert_eq!(s.poll(t0), None);
        let t1 = t0 + Duration::from_millis(200);
        assert_eq!(s.request(t1, Some(keys("sur"))), Lookup::Pending);
        // 300 ms after the first keystroke is not 300 ms after the last.
        assert_eq!(s.poll(t0 + DEBOUNCE), None);
        assert_eq!(s.wake_in(t0 + DEBOUNCE), Some(Duration::from_millis(200)));
        let job = s.poll(t1 + DEBOUNCE).expect("due");
        assert_eq!(job.query, keys("sur"));
        assert_eq!(s.poll(t1 + DEBOUNCE), None);
    }

    #[test]
    fn one_request_in_flight_and_stale_answers_are_dropped() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        s.request(t0, Some(keys("su")));
        let first = s.poll(t0 + DEBOUNCE).unwrap();
        // The user keeps typing while it is in flight.
        let t1 = t0 + DEBOUNCE + Duration::from_millis(10);
        s.request(t1, Some(keys("sur")));
        assert_eq!(s.poll(t1 + DEBOUNCE), None, "second must wait");
        assert_eq!(s.wake_in(t1), None);
        // The first answer arrives: stale, so not shown...
        assert_eq!(s.finish(t1, first, Ok(vec![item("surface", 1)])), None);
        // ...but the second may now go out.
        let second = s.poll(t1 + DEBOUNCE).unwrap();
        assert_eq!(second.query, keys("sur"));
        assert_eq!(
            s.finish(t1 + DEBOUNCE, second, Ok(vec![item("surface", 2)])),
            Some(vec![item("surface", 2)])
        );
    }

    #[test]
    fn answers_are_cached_for_the_session() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        s.request(t0, Some(keys("su")));
        let job = s.poll(t0 + DEBOUNCE).unwrap();
        s.finish(t0 + DEBOUNCE, job, Ok(vec![item("surface", 1)]));
        // Even the stale one above was cached; asking again needs no request.
        assert_eq!(
            s.request(t0 + DEBOUNCE, Some(keys("su"))),
            Lookup::Cached(vec![item("surface", 1)])
        );
        assert_eq!(s.poll(t0 + DEBOUNCE * 2), None);
    }

    #[test]
    fn cache_is_bounded() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        for i in 0..(CACHE_CAPACITY + 5) {
            s.request(t0, Some(keys(&format!("k{i}"))));
            let job = s.poll(t0 + DEBOUNCE).unwrap();
            s.finish(t0, job, Ok(vec![]));
        }
        assert_eq!(s.cache.len(), CACHE_CAPACITY);
        assert_eq!(s.request(t0, Some(keys("k0"))), Lookup::Pending);
    }

    #[test]
    fn backs_off_after_an_error() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        s.request(t0, Some(keys("su")));
        let job = s.poll(t0 + DEBOUNCE).unwrap();
        let t1 = t0 + DEBOUNCE;
        assert_eq!(s.finish(t1, job, Err(FetchError::BadResponse)), None);
        // No request is queued during the back-off...
        assert_eq!(
            s.request(t1 + Duration::from_secs(1), Some(keys("sur"))),
            Lookup::Idle
        );
        // ...but afterwards things work again, and the back-off grows.
        let t2 = t1 + Duration::from_secs(11);
        assert_eq!(s.request(t2, Some(keys("surf"))), Lookup::Pending);
        let job = s.poll(t2 + DEBOUNCE).unwrap();
        s.finish(t2, job, Err(FetchError::BadResponse));
        assert_eq!(
            s.request(t2 + Duration::from_secs(15), Some(keys("surfa"))),
            Lookup::Idle
        );
        assert_eq!(
            s.request(t2 + Duration::from_secs(21), Some(keys("surfac"))),
            Lookup::Pending
        );
    }

    #[test]
    fn asking_for_nothing_cancels() {
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        s.request(t0, Some(keys("su")));
        assert_eq!(s.request(t0, None), Lookup::Idle);
        assert_eq!(s.poll(t0 + DEBOUNCE * 2), None);
    }

    #[test]
    fn disabled_setting_makes_no_request_end_to_end() {
        // The UI only ever sends what `plan` returns.
        let client = FakeClient::new(vec![]);
        let t0 = Instant::now();
        let mut s = TaginfoState::new();
        let q = plan(false, Target::Key, "", "surface");
        assert_eq!(s.request(t0, q), Lookup::Idle);
        assert!(s.poll(t0 + DEBOUNCE * 5).is_none());
        assert_eq!(client.request_count(), 0);
    }
}
