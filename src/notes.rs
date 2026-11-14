//! OSM notes: user-reported map problems pinned to a location.
//!
//! This module is the part of the feature that needs no window: the note
//! model, the OSM API 0.6 calls (against an injected [`HttpClient`], so all of
//! it is tested with the `FakeClient`), the XML parser, how a download area is
//! cut into requests the server accepts, and the shared store the Notes layer
//! draws from. Notes are not OSM data: nothing here touches a data layer, the
//! undo stack, the validator, autosave or search.
//!
//! API (relative to the configured API base):
//! - `GET  /api/0.6/notes?bbox=l,b,r,t&limit=N&closed=D` (anonymous works)
//! - `POST /api/0.6/notes?lat=..&lon=..&text=..` create
//! - `POST /api/0.6/notes/#id/comment?text=..`, `.../close[?text=..]`,
//!   `.../reopen[?text=..]` (need the `write_notes` OAuth scope)
//!
//! The XML form is used throughout: every one of these answers with
//! `<osm><note ...>...</note></osm>`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::coordinates::GeoBounds;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpFailure, HttpRequest, Method, RetryPolicy,
};
use crate::tr;

/// Most notes asked of the server per request (the server allows up to 10000).
pub const NOTE_LIMIT: u32 = 1000;
/// Notes closed within this many days are shown with the open ones.
pub const CLOSED_DAYS: i32 = 7;
/// Most requests one refresh may make. Beyond this the downloaded area is too
/// big to cover and the visible map is used instead.
pub const MAX_FETCH_REQUESTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteStatus {
    Open,
    Closed,
    /// Hidden by a moderator; only moderators ever receive these.
    Hidden,
}

impl NoteStatus {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "closed" => Self::Closed,
            "hidden" => Self::Hidden,
            _ => Self::Open,
        }
    }

    pub fn label(self) -> gpui::SharedString {
        match self {
            Self::Open => tr!("notes.status.open"),
            Self::Closed => tr!("notes.status.closed"),
            Self::Hidden => tr!("notes.status.hidden"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentAction {
    Opened,
    Commented,
    Closed,
    Reopened,
    Hidden,
}

impl CommentAction {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "opened" => Self::Opened,
            "closed" => Self::Closed,
            "reopened" => Self::Reopened,
            "hidden" => Self::Hidden,
            _ => Self::Commented,
        }
    }

    pub fn label(self) -> gpui::SharedString {
        match self {
            Self::Opened => tr!("notes.action.opened"),
            Self::Commented => tr!("notes.action.commented"),
            Self::Closed => tr!("notes.action.closed"),
            Self::Reopened => tr!("notes.action.reopened"),
            Self::Hidden => tr!("notes.action.hidden"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteComment {
    /// Seconds since the Unix epoch.
    pub date: Option<i64>,
    pub uid: Option<u64>,
    /// `None` for an anonymous comment.
    pub user: Option<String>,
    pub action: CommentAction,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub status: NoteStatus,
    pub created: Option<i64>,
    pub closed: Option<i64>,
    pub comments: Vec<NoteComment>,
}

impl Note {
    pub fn is_open(&self) -> bool {
        self.status == NoteStatus::Open
    }
}

#[derive(Debug)]
pub enum NotesError {
    Http {
        status: u16,
        body: String,
    },
    Network(String),
    Parse(String),
    /// A local response file (script mode) could not be read.
    ReadFile {
        path: String,
        detail: String,
    },
}

impl From<HttpError> for NotesError {
    fn from(e: HttpError) -> Self {
        match HttpFailure::from(e) {
            HttpFailure::Status { status, body } => NotesError::Http { status, body },
            HttpFailure::Transport(msg) => NotesError::Network(msg),
            HttpFailure::Cancelled => NotesError::Network("cancelled".into()),
        }
    }
}

impl NotesError {
    /// The HTTP status, if the server answered with one.
    pub fn status(&self) -> Option<u16> {
        match self {
            NotesError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// A sentence for the user. Missing scope (403) and a rejected login (401)
    /// ask the user to sign in again; nothing here retries by itself.
    pub fn user_message(&self) -> String {
        match self {
            NotesError::Http { status: 401, .. } => tr!("notes.error.unauthorized").to_string(),
            NotesError::Http { status: 403, .. } => tr!("notes.error.scope").to_string(),
            NotesError::Http { status: 404, .. } | NotesError::Http { status: 410, .. } => {
                tr!("notes.error.gone").to_string()
            }
            NotesError::Http {
                status: 409, body, ..
            } => {
                let detail = body.lines().next().unwrap_or("").trim().to_string();
                tr!("notes.error.conflict", detail = detail).to_string()
            }
            NotesError::Http { status: 400, .. } => tr!("notes.error.rejected").to_string(),
            NotesError::Http { status: 429, .. } | NotesError::Http { status: 509, .. } => {
                tr!("notes.error.rate_limited").to_string()
            }
            NotesError::Http { status, body } => {
                let detail = body.lines().next().unwrap_or("").trim().to_string();
                tr!("notes.error.http", status = status, detail = detail).to_string()
            }
            NotesError::Network(msg) => tr!("error.network", detail = msg).to_string(),
            NotesError::Parse(msg) => tr!("notes.error.parse", detail = msg).to_string(),
            NotesError::ReadFile { path, detail } => format!("{path}: {detail}"),
        }
    }
}

impl std::fmt::Display for NotesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for NotesError {}

/// Parse the API's `2019-06-15 08:26:04 UTC` into Unix seconds.
pub fn parse_osm_time(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches("UTC").trim();
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(crate::civil_date::unix_seconds(y, m, day, h, mi, sec))
}

/// `2019-06-15` for Unix seconds.
pub fn format_date(ts: i64) -> String {
    crate::civil_date::date_from_unix_seconds(ts)
}

/// "3 days ago" for anything under a month old, the short date beyond that.
pub fn when_phrase(now: i64, ts: i64) -> String {
    let age = now.saturating_sub(ts);
    if age < 30 * 86_400 {
        crate::ui::restore_dialog::age_phrase(now.max(0) as u64, ts.max(0) as u64)
    } else {
        format_date(ts)
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse an API 0.6 notes document (`<osm><note>...</note>...</osm>`).
pub fn parse_notes(xml: &str) -> Result<Vec<Note>, NotesError> {
    let mut reader = Reader::from_str(xml);
    let mut notes = Vec::new();
    let mut note: Option<Note> = None;
    let mut comment: Option<NoteComment> = None;
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "note" {
                    let attr = |key: &str| -> Option<f64> {
                        e.attributes()
                            .flatten()
                            .find(|a| a.key.as_ref() == key.as_bytes())
                            .and_then(|a| a.unescape_value().ok()?.trim().parse::<f64>().ok())
                    };
                    let (Some(lat), Some(lon)) = (attr("lat"), attr("lon")) else {
                        return Err(NotesError::Parse("a note has no position".into()));
                    };
                    note = Some(Note {
                        id: 0,
                        lat,
                        lon,
                        status: NoteStatus::Open,
                        created: None,
                        closed: None,
                        comments: Vec::new(),
                    });
                } else if name == "comment" && note.is_some() {
                    comment = Some(NoteComment {
                        date: None,
                        uid: None,
                        user: None,
                        action: CommentAction::Commented,
                        text: String::new(),
                    });
                }
                text.clear();
                path.push(name);
            }
            Ok(Event::Text(t)) => {
                text.push_str(&t.unescape().map_err(|e| NotesError::Parse(e.to_string()))?)
            }
            Ok(Event::CData(c)) => text.push_str(&String::from_utf8_lossy(&c)),
            Ok(Event::End(_)) => {
                let Some(name) = path.pop() else { continue };
                match (name.as_str(), comment.as_mut(), note.as_mut()) {
                    ("comment", _, Some(n)) => {
                        if let Some(c) = comment.take() {
                            n.comments.push(c);
                        }
                    }
                    ("note", _, _) => {
                        if let Some(n) = note.take() {
                            notes.push(n);
                        }
                    }
                    ("date", Some(c), _) => c.date = parse_osm_time(&text),
                    ("uid", Some(c), _) => c.uid = text.trim().parse().ok(),
                    ("user", Some(c), _) => {
                        let u = text.trim();
                        c.user = (!u.is_empty()).then(|| u.to_string());
                    }
                    ("action", Some(c), _) => c.action = CommentAction::parse(&text),
                    ("text", Some(c), _) => c.text = text.clone(),
                    ("id", None, Some(n)) => n.id = text.trim().parse().unwrap_or(0),
                    ("status", None, Some(n)) => n.status = NoteStatus::parse(&text),
                    ("date_created", None, Some(n)) => n.created = parse_osm_time(&text),
                    ("date_closed", None, Some(n)) => n.closed = parse_osm_time(&text),
                    _ => {}
                }
                text.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(NotesError::Parse(e.to_string())),
        }
    }
    if notes.iter().any(|n| n.id <= 0) {
        return Err(NotesError::Parse("a note has no id".into()));
    }
    Ok(notes)
}

fn base(base_url: &str) -> &str {
    base_url.trim_end_matches('/')
}

pub fn notes_url(base_url: &str, bounds: &GeoBounds) -> String {
    format!(
        "{}/api/0.6/notes?bbox={:.7},{:.7},{:.7},{:.7}&limit={}&closed={}",
        base(base_url),
        bounds.min_lon,
        bounds.min_lat,
        bounds.max_lon,
        bounds.max_lat,
        NOTE_LIMIT,
        CLOSED_DAYS
    )
}

/// What one refresh found.
#[derive(Debug, Default, PartialEq)]
pub struct Fetched {
    pub notes: Vec<Note>,
    /// Some request returned as many notes as it was allowed to, so there may
    /// be more.
    pub truncated: bool,
}

/// `GET` the notes in each of `bboxes` and merge them by id. Anonymous unless
/// `token` is given. Reads retry like the map download does.
pub fn fetch_notes_with(
    client: &dyn HttpClient,
    base_url: &str,
    bboxes: &[GeoBounds],
    token: Option<&str>,
) -> Result<Fetched, NotesError> {
    let mut out = Fetched::default();
    for bounds in bboxes {
        let mut req = HttpRequest::get(notes_url(base_url, bounds));
        if let Some(token) = token {
            req = req.bearer(token);
        }
        let resp = fetch_with_retries(client, &req, &RetryPolicy::standard())?;
        let body = resp
            .into_string()
            .map_err(|e| NotesError::Network(e.to_string()))?;
        let notes = parse_notes(&body)?;
        if notes.len() >= NOTE_LIMIT as usize {
            out.truncated = true;
        }
        for note in notes {
            if !out.notes.iter().any(|n| n.id == note.id) {
                out.notes.push(note);
            }
        }
    }
    out.notes.sort_by_key(|n| n.id);
    Ok(out)
}

/// A write to the notes API.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteWrite {
    Create { lat: f64, lon: f64 },
    Comment(i64),
    Close(i64),
    Reopen(i64),
}

impl NoteWrite {
    /// Whether the server wants a `text` (a comment and a new note do; close
    /// and reopen take one only if given).
    pub fn needs_text(self) -> bool {
        matches!(self, Self::Create { .. } | Self::Comment(_))
    }
}

/// Percent-encode a query value (UTF-8 bytes; spaces, `&`, newlines, `+`
/// and everything non-ASCII included).
fn q(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

pub fn write_url(base_url: &str, write: NoteWrite, text: &str) -> String {
    let b = base(base_url);
    let with_text = |url: String| {
        if text.is_empty() {
            url
        } else {
            format!("{url}?text={}", q(text))
        }
    };
    match write {
        NoteWrite::Create { lat, lon } => format!(
            "{b}/api/0.6/notes?lat={lat:.7}&lon={lon:.7}&text={}",
            q(text)
        ),
        NoteWrite::Comment(id) => with_text(format!("{b}/api/0.6/notes/{id}/comment")),
        NoteWrite::Close(id) => with_text(format!("{b}/api/0.6/notes/{id}/close")),
        NoteWrite::Reopen(id) => with_text(format!("{b}/api/0.6/notes/{id}/reopen")),
    }
}

/// Perform `write` and return the note as the server now has it. Needs a
/// bearer token. Never retried: a repeated comment would be posted twice.
pub fn write_note_with(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    write: NoteWrite,
    text: &str,
) -> Result<Note, NotesError> {
    let text = text.trim();
    if write.needs_text() && text.is_empty() {
        return Err(NotesError::Http {
            status: 400,
            body: "empty text".into(),
        });
    }
    let mut req =
        HttpRequest::with_text(Method::Post, write_url(base_url, write, text), None).bearer(token);
    req.timeout = Duration::from_secs(30);
    let resp = fetch_with_retries(client, &req, &RetryPolicy::none())?;
    let body = resp
        .into_string()
        .map_err(|e| NotesError::Network(e.to_string()))?;
    parse_notes(&body)?
        .into_iter()
        .next()
        .ok_or_else(|| NotesError::Parse("the answer holds no note".into()))
}

/// Cut `b` into a grid of cells each at most `max_area` square degrees.
pub fn split_for_area(b: &GeoBounds, max_area: f64) -> Vec<GeoBounds> {
    let (w, h) = (b.width(), b.height());
    let area = w * h;
    if !(area.is_finite() && max_area > 0.0) || area <= max_area || w <= 0.0 || h <= 0.0 {
        return vec![*b];
    }
    // Aim a little under the limit so rounding never lands a cell above it.
    let target = max_area * 0.98;
    let n = (area / target).ceil().max(1.0);
    let mut cols = (n * w / h).sqrt().ceil().max(1.0) as usize;
    let mut rows = ((n / cols as f64).ceil().max(1.0)) as usize;
    while (w / cols as f64) * (h / rows as f64) > target {
        if w / cols as f64 > h / rows as f64 {
            cols += 1;
        } else {
            rows += 1;
        }
    }
    let mut cells = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            cells.push(GeoBounds::new(
                b.min_lat + h * r as f64 / rows as f64,
                b.min_lat + h * (r + 1) as f64 / rows as f64,
                b.min_lon + w * c as f64 / cols as f64,
                b.min_lon + w * (c + 1) as f64 / cols as f64,
            ));
        }
    }
    cells
}

/// What a refresh will request.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchPlan {
    /// The downloaded area, cut to the server's limit.
    Downloaded(Vec<GeoBounds>),
    /// The visible map only (no downloaded area, or it was too large to
    /// cover).
    Viewport(Vec<GeoBounds>),
    /// Even the visible map is too large: zoom in.
    TooLarge,
}

pub fn plan_fetch(
    downloaded: &[GeoBounds],
    viewport: &GeoBounds,
    max_area: f64,
    max_requests: usize,
) -> FetchPlan {
    let mut unique: Vec<GeoBounds> = Vec::new();
    for d in downloaded {
        if d.width() > 0.0 && d.height() > 0.0 && !unique.contains(d) {
            unique.push(*d);
        }
    }
    if !unique.is_empty() {
        let cells: Vec<GeoBounds> = unique
            .iter()
            .flat_map(|d| split_for_area(d, max_area))
            .collect();
        if cells.len() <= max_requests {
            return FetchPlan::Downloaded(cells);
        }
    }
    let cells = split_for_area(viewport, max_area);
    if cells.len() <= max_requests {
        FetchPlan::Viewport(cells)
    } else {
        FetchPlan::TooLarge
    }
}

/// A Web Mercator rectangle (metres) as latitude/longitude bounds.
pub fn merc_to_geo(r: &crate::download_bounds::MercRect) -> GeoBounds {
    const R: f64 = 6_378_137.0;
    let lon = |x: f64| (x / R).to_degrees();
    let lat = |y: f64| (y / R).sinh().atan().to_degrees();
    GeoBounds::new(lat(r.min_y), lat(r.max_y), lon(r.min_x), lon(r.max_x))
}

#[derive(Debug, Default)]
pub struct NoteStoreInner {
    pub notes: Vec<Note>,
    /// The note open in the side panel, drawn with a ring.
    pub selected: Option<i64>,
}

/// The notes on the map, shared between the app (which fills it) and the
/// Notes layer (which draws it).
#[derive(Debug, Clone, Default)]
pub struct NoteStore(Arc<Mutex<NoteStoreInner>>);

impl NoteStore {
    pub fn with<R>(&self, f: impl FnOnce(&mut NoteStoreInner) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn replace(&self, mut notes: Vec<Note>) {
        notes.sort_by_key(|n| n.id);
        self.with(|s| s.notes = notes);
    }

    /// Insert or update one note (after a write).
    pub fn upsert(&self, note: Note) {
        self.with(|s| match s.notes.iter_mut().find(|n| n.id == note.id) {
            Some(slot) => *slot = note,
            None => {
                s.notes.push(note);
                s.notes.sort_by_key(|n| n.id);
            }
        });
    }

    pub fn get(&self, id: i64) -> Option<Note> {
        self.with(|s| s.notes.iter().find(|n| n.id == id).cloned())
    }

    pub fn len(&self) -> usize {
        self.with(|s| s.notes.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_selected(&self, id: Option<i64>) {
        self.with(|s| s.selected = id);
    }

    pub fn clear(&self) {
        self.with(|s| {
            s.notes.clear();
            s.selected = None;
        });
    }
}

/// Load notes from a local file holding an API response (script mode only; the
/// `load_notes_file` op).
pub fn load_notes_file(path: &std::path::Path) -> Result<Vec<Note>, NotesError> {
    let xml = std::fs::read_to_string(path).map_err(|e| NotesError::ReadFile {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    parse_notes(&xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::{LatLon, Mercator};
    use crate::http::fake::{ok, status_err, transport_err, FakeClient};

    const BASE: &str = "https://api.openstreetmap.org";

    #[test]
    fn a_notes_file_that_cannot_be_read_or_parsed_has_its_own_errors() {
        let dir = std::env::temp_dir().join(format!("osm-gpui-notes-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("missing.xml");
        let err = load_notes_file(&missing).unwrap_err();
        assert!(matches!(&err, NotesError::ReadFile { path, .. } if path.ends_with("missing.xml")));
        assert!(err.to_string().contains("missing.xml"));

        let bad = dir.join("bad.xml");
        std::fs::write(&bad, "<osm><note").unwrap();
        assert!(matches!(load_notes_file(&bad), Err(NotesError::Parse(_))));

        let good = dir.join("good.xml");
        std::fs::write(&good, SAMPLE).unwrap();
        assert!(!load_notes_file(&good).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn http_failures_map_to_notes_errors() {
        assert!(matches!(
            NotesError::from(HttpError::Status {
                status: 403,
                body: b"scope".to_vec()
            }),
            NotesError::Http { status: 403, body } if body == "scope"
        ));
        assert!(matches!(
            NotesError::from(HttpError::Transport("x".into())),
            NotesError::Network(m) if m == "x"
        ));
        assert!(matches!(
            NotesError::from(HttpError::Cancelled),
            NotesError::Network(m) if m == "cancelled"
        ));
    }

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6" generator="OpenStreetMap server">
  <note lon="-0.1234567" lat="51.5000000">
    <id>101</id>
    <url>https://api.openstreetmap.org/api/0.6/notes/101</url>
    <comment_url>https://api.openstreetmap.org/api/0.6/notes/101/comment</comment_url>
    <close_url>https://api.openstreetmap.org/api/0.6/notes/101/close</close_url>
    <date_created>2024-03-01 10:00:00 UTC</date_created>
    <status>open</status>
    <comments>
      <comment>
        <date>2024-03-01 10:00:00 UTC</date>
        <uid>42</uid>
        <user>mapper</user>
        <user_url>https://www.openstreetmap.org/user/mapper</user_url>
        <action>opened</action>
        <text>The bridge &amp; the path are missing</text>
        <html>&lt;p&gt;The bridge &amp;amp; the path are missing&lt;/p&gt;</html>
      </comment>
      <comment>
        <date>2024-03-02 11:30:15 UTC</date>
        <action>commented</action>
        <text>Seen it too
second line</text>
        <html>&lt;p&gt;Seen it too&lt;/p&gt;</html>
      </comment>
      <comment>
        <date>2024-03-03 08:00:00 UTC</date>
        <uid>7</uid>
        <user>Zoë</user>
        <action>commented</action>
        <text>Café</text>
      </comment>
    </comments>
  </note>
  <note lon="2.0" lat="48.0">
    <id>102</id>
    <date_created>2024-01-01 00:00:00 UTC</date_created>
    <status>closed</status>
    <date_closed>2024-01-05 12:00:00 UTC</date_closed>
    <comments>
      <comment>
        <date>2024-01-01 00:00:00 UTC</date>
        <action>opened</action>
        <text>Old</text>
      </comment>
      <comment>
        <date>2024-01-05 12:00:00 UTC</date>
        <uid>7</uid>
        <user>fixer</user>
        <action>closed</action>
        <text></text>
      </comment>
    </comments>
  </note>
</osm>"#;

    fn single(id: i64, status: &str) -> String {
        format!(
            r#"<osm version="0.6"><note lon="1.0" lat="2.0"><id>{id}</id><date_created>2024-03-01 10:00:00 UTC</date_created><status>{status}</status><comments><comment><date>2024-03-01 10:00:00 UTC</date><uid>1</uid><user>u</user><action>opened</action><text>hi</text></comment></comments></note></osm>"#
        )
    }

    fn token_header(req: &HttpRequest) -> Option<String> {
        req.headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn parses_comments_anonymous_and_closed_notes() {
        let notes = parse_notes(SAMPLE).unwrap();
        assert_eq!(notes.len(), 2);
        let a = &notes[0];
        assert_eq!(a.id, 101);
        assert_eq!((a.lat, a.lon), (51.5, -0.1234567));
        assert_eq!(a.status, NoteStatus::Open);
        assert!(a.is_open());
        assert_eq!(a.comments.len(), 3);
        assert_eq!(a.comments[0].action, CommentAction::Opened);
        assert_eq!(a.comments[0].user.as_deref(), Some("mapper"));
        assert_eq!(a.comments[0].uid, Some(42));
        assert_eq!(a.comments[0].text, "The bridge & the path are missing");
        // Anonymous: no user, no uid.
        assert_eq!(a.comments[1].user, None);
        assert_eq!(a.comments[1].uid, None);
        assert_eq!(a.comments[1].text, "Seen it too\nsecond line");
        assert_eq!(a.comments[2].user.as_deref(), Some("Zoë"));
        assert_eq!(a.comments[2].text, "Café");
        assert_eq!(a.created, parse_osm_time("2024-03-01 10:00:00 UTC"));

        let b = &notes[1];
        assert_eq!(b.status, NoteStatus::Closed);
        assert!(!b.is_open());
        assert_eq!(b.closed, parse_osm_time("2024-01-05 12:00:00 UTC"));
        assert_eq!(b.comments[1].action, CommentAction::Closed);
        assert_eq!(b.comments[1].text, "");
        assert_eq!(b.comments[0].user, None);
    }

    #[test]
    fn empty_and_broken_documents() {
        assert!(parse_notes(r#"<osm version="0.6"/>"#).unwrap().is_empty());
        assert!(matches!(
            parse_notes("<osm><note lat='1'><id>1</id></note></osm>"),
            Err(NotesError::Parse(_))
        ));
        assert!(matches!(
            parse_notes("<osm><note lat='1' lon='2'><id>x</id></note></osm>"),
            Err(NotesError::Parse(_))
        ));
        assert!(matches!(
            parse_notes("<osm><note></osm>"),
            Err(NotesError::Parse(_))
        ));
    }

    #[test]
    fn times_parse_and_format() {
        assert_eq!(parse_osm_time("1970-01-01 00:00:00 UTC"), Some(0));
        assert_eq!(
            parse_osm_time("2024-03-01 10:00:00 UTC"),
            Some(1_709_287_200)
        );
        assert_eq!(parse_osm_time("nonsense"), None);
        assert_eq!(parse_osm_time("2024-13-01 10:00:00 UTC"), None);
        assert_eq!(format_date(1_709_287_200), "2024-03-01");
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn dates_before_1970_and_around_century_boundaries() {
        assert_eq!(format_date(-1), "1969-12-31");
        assert_eq!(format_date(-2_203_891_201), "1900-02-28");
        assert_eq!(format_date(-2_203_891_200), "1900-03-01");
        assert_eq!(format_date(946_684_799), "1999-12-31");
        assert_eq!(format_date(946_684_800), "2000-01-01");
        assert_eq!(
            parse_osm_time("1900-03-01 00:00:00 UTC"),
            Some(-2_203_891_200)
        );
        // Lenient edges: a missing seconds field and a leap second pass.
        assert_eq!(parse_osm_time("1970-01-01 00:01"), Some(60));
        assert_eq!(parse_osm_time("1970-01-01 00:00:60 UTC"), Some(60));
        assert_eq!(parse_osm_time("1970-01-01 24:00:00 UTC"), None);
    }

    #[test]
    fn when_phrase_is_relative_then_a_date() {
        let t = 1_709_287_200;
        assert_eq!(when_phrase(t + 3 * 86_400, t), "3 days ago");
        assert_eq!(when_phrase(t + 100 * 86_400, t), "2024-03-01");
    }

    #[test]
    fn fetch_url_orders_bbox_as_left_bottom_right_top() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        assert_eq!(
            notes_url("https://x.test/", &b),
            "https://x.test/api/0.6/notes?bbox=-74.0200000,40.7000000,-73.9800000,40.7500000&limit=1000&closed=7"
        );
    }

    #[test]
    fn fetch_is_anonymous_without_a_token_and_bearer_with_one() {
        let b = GeoBounds::new(0.0, 0.1, 0.0, 0.1);
        let client = FakeClient::new(vec![ok(200, SAMPLE)]);
        let got = fetch_notes_with(&client, BASE, &[b], None).unwrap();
        assert_eq!(got.notes.len(), 2);
        assert!(!got.truncated);
        {
            let reqs = client.requests.lock().unwrap();
            assert_eq!(reqs[0].method, Method::Get);
            assert!(token_header(&reqs[0]).is_none());
        }
        let client = FakeClient::new(vec![ok(200, SAMPLE)]);
        fetch_notes_with(&client, BASE, &[b], Some("tok")).unwrap();
        assert_eq!(
            token_header(&client.requests.lock().unwrap()[0]).as_deref(),
            Some("Bearer tok")
        );
    }

    #[test]
    fn fetch_merges_requests_by_id_and_reports_errors() {
        let b = GeoBounds::new(0.0, 0.1, 0.0, 0.1);
        let client = FakeClient::new(vec![ok(200, SAMPLE), ok(200, SAMPLE)]);
        let got = fetch_notes_with(&client, BASE, &[b, b], None).unwrap();
        assert_eq!(got.notes.len(), 2);
        assert_eq!(client.request_count(), 2);

        let client = FakeClient::new(vec![status_err(400, "too big")]);
        let err = fetch_notes_with(&client, BASE, &[b], None).unwrap_err();
        assert_eq!(err.status(), Some(400));

        let client = FakeClient::new(vec![
            transport_err("dns"),
            transport_err("dns"),
            transport_err("dns"),
        ]);
        assert!(matches!(
            fetch_notes_with(&client, BASE, &[b], None),
            Err(NotesError::Network(_))
        ));
    }

    #[test]
    fn write_urls_encode_text() {
        let nasty = "a b&c=d\nline2 é +%";
        let enc = "a%20b%26c%3Dd%0Aline2%20%C3%A9%20%2B%25";
        assert_eq!(
            write_url(BASE, NoteWrite::Comment(5), nasty),
            format!("{BASE}/api/0.6/notes/5/comment?text={enc}")
        );
        assert_eq!(
            write_url(BASE, NoteWrite::Close(5), nasty),
            format!("{BASE}/api/0.6/notes/5/close?text={enc}")
        );
        assert_eq!(
            write_url(BASE, NoteWrite::Reopen(5), nasty),
            format!("{BASE}/api/0.6/notes/5/reopen?text={enc}")
        );
        assert_eq!(
            write_url(
                BASE,
                NoteWrite::Create {
                    lat: -48.0,
                    lon: -123.0
                },
                nasty
            ),
            format!("{BASE}/api/0.6/notes?lat=-48.0000000&lon=-123.0000000&text={enc}")
        );
        // Close and reopen take no text at all when there is none.
        assert_eq!(
            write_url(BASE, NoteWrite::Close(5), ""),
            format!("{BASE}/api/0.6/notes/5/close")
        );
        assert_eq!(
            write_url(BASE, NoteWrite::Reopen(5), ""),
            format!("{BASE}/api/0.6/notes/5/reopen")
        );
    }

    #[test]
    fn each_write_posts_with_the_bearer_token_and_returns_the_note() {
        for (write, text, status, path) in [
            (
                NoteWrite::Create { lat: 1.0, lon: 2.0 },
                "new",
                "open",
                "notes?lat=1.0000000&lon=2.0000000&text=new",
            ),
            (NoteWrite::Comment(9), "c", "open", "notes/9/comment?text=c"),
            (
                NoteWrite::Close(9),
                "done",
                "closed",
                "notes/9/close?text=done",
            ),
            (NoteWrite::Reopen(9), "", "open", "notes/9/reopen"),
        ] {
            let client = FakeClient::new(vec![ok(200, single(9, status))]);
            let note = write_note_with(&client, BASE, "tok", write, text).unwrap();
            assert_eq!(note.id, 9);
            assert_eq!(note.is_open(), status == "open");
            let reqs = client.requests.lock().unwrap();
            assert_eq!(reqs.len(), 1);
            assert_eq!(reqs[0].method, Method::Post);
            assert_eq!(reqs[0].url, format!("{BASE}/api/0.6/{path}"));
            assert_eq!(token_header(&reqs[0]).as_deref(), Some("Bearer tok"));
            assert!(reqs[0].body.is_none());
        }
    }

    #[test]
    fn a_comment_or_new_note_needs_text_and_makes_no_request_without() {
        let client = FakeClient::new(vec![]);
        for write in [
            NoteWrite::Comment(1),
            NoteWrite::Create { lat: 0.0, lon: 0.0 },
        ] {
            let err = write_note_with(&client, BASE, "t", write, "  \n ").unwrap_err();
            assert_eq!(err.status(), Some(400));
        }
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn write_errors_map_to_clear_messages_and_are_never_retried() {
        for (status, needle) in [
            (409, "changed on the server"),
            (403, "sign in again"),
            (401, "sign in again"),
            (404, "no longer"),
            (410, "no longer"),
            (400, "rejected"),
            (429, "too many"),
            (503, "503"),
        ] {
            let client = FakeClient::new(vec![
                status_err(status, "The note 9 was closed at 2024-01-01"),
                ok(200, single(9, "open")),
            ]);
            let err = write_note_with(&client, BASE, "t", NoteWrite::Comment(9), "x").unwrap_err();
            assert_eq!(err.status(), Some(status));
            assert!(
                err.user_message().to_lowercase().contains(needle),
                "{status}: {}",
                err.user_message()
            );
            assert_eq!(client.request_count(), 1, "{status} must not be retried");
        }
        let client = FakeClient::new(vec![transport_err("offline")]);
        let err = write_note_with(&client, BASE, "t", NoteWrite::Close(9), "").unwrap_err();
        assert!(matches!(err, NotesError::Network(_)));
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn conflict_message_carries_the_servers_reason() {
        let e = NotesError::Http {
            status: 409,
            body: "The note 9 was closed at 2024-01-01\nmore".into(),
        };
        assert!(e
            .user_message()
            .contains("The note 9 was closed at 2024-01-01"));
    }

    #[test]
    fn small_areas_are_not_split() {
        let b = GeoBounds::new(0.0, 0.4, 0.0, 0.4);
        assert_eq!(split_for_area(&b, 25.0), vec![b]);
    }

    #[test]
    fn large_areas_split_into_cells_within_the_limit_that_cover_it() {
        let b = GeoBounds::new(10.0, 17.0, 20.0, 29.0);
        let cells = split_for_area(&b, 25.0);
        assert!(cells.len() > 1);
        for c in &cells {
            assert!(c.width() * c.height() <= 25.0, "{c:?}");
        }
        let total: f64 = cells.iter().map(|c| c.width() * c.height()).sum();
        assert!((total - 63.0).abs() < 1e-6);
        assert!(cells.iter().all(|c| c.min_lat >= 10.0 - 1e-9
            && c.max_lat <= 17.0 + 1e-9
            && c.min_lon >= 20.0 - 1e-9
            && c.max_lon <= 29.0 + 1e-9));
    }

    #[test]
    fn plan_uses_downloaded_areas_split_to_the_limit() {
        let dl = [GeoBounds::new(0.0, 6.0, 0.0, 6.0)];
        let view = GeoBounds::new(1.0, 2.0, 1.0, 2.0);
        match plan_fetch(&dl, &view, 25.0, 6) {
            FetchPlan::Downloaded(cells) => {
                assert_eq!(cells.len(), 2);
                assert!(cells.iter().all(|c| c.width() * c.height() <= 25.0));
            }
            other => panic!("{other:?}"),
        }
        // Duplicate rectangles are asked for once.
        let dl = [
            GeoBounds::new(0.0, 1.0, 0.0, 1.0),
            GeoBounds::new(0.0, 1.0, 0.0, 1.0),
        ];
        assert_eq!(
            plan_fetch(&dl, &view, 25.0, 6),
            FetchPlan::Downloaded(vec![dl[0]])
        );
    }

    #[test]
    fn plan_falls_back_to_the_viewport_when_the_download_is_too_large_or_missing() {
        let view = GeoBounds::new(1.0, 2.0, 1.0, 2.0);
        let huge = [GeoBounds::new(0.0, 50.0, 0.0, 50.0)];
        assert_eq!(
            plan_fetch(&huge, &view, 25.0, 6),
            FetchPlan::Viewport(vec![view])
        );
        assert_eq!(
            plan_fetch(&[], &view, 25.0, 6),
            FetchPlan::Viewport(vec![view])
        );
        let world = GeoBounds::new(-80.0, 80.0, -170.0, 170.0);
        assert_eq!(plan_fetch(&huge, &world, 25.0, 6), FetchPlan::TooLarge);
    }

    #[test]
    fn mercator_rectangles_convert_back_to_degrees() {
        let Mercator { x: x0, y: y0 } =
            crate::coordinates::lat_lon_to_mercator(LatLon::new(40.0, -74.0));
        let Mercator { x: x1, y: y1 } =
            crate::coordinates::lat_lon_to_mercator(LatLon::new(41.0, -73.0));
        let g = merc_to_geo(&crate::download_bounds::MercRect::new(x0, x1, y0, y1));
        assert!((g.min_lat - 40.0).abs() < 1e-6 && (g.max_lat - 41.0).abs() < 1e-6);
        assert!((g.min_lon + 74.0).abs() < 1e-6 && (g.max_lon + 73.0).abs() < 1e-6);
    }

    #[test]
    fn store_upserts_in_place() {
        let store = NoteStore::default();
        let mut notes = parse_notes(SAMPLE).unwrap();
        store.replace(notes.clone());
        assert_eq!(store.len(), 2);
        notes[0].status = NoteStatus::Closed;
        store.upsert(notes[0].clone());
        assert_eq!(store.len(), 2);
        assert_eq!(store.get(101).unwrap().status, NoteStatus::Closed);
        store.upsert(Note {
            id: 5,
            ..notes[0].clone()
        });
        assert_eq!(store.len(), 3);
        store.clear();
        assert!(store.is_empty());
    }
}
