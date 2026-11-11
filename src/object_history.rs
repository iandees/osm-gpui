//! The history of one OSM object (its "versions"), as the server's
//! `GET /api/0.6/[node|way|relation]/#id/history` reports it, and what changed
//! between versions.
//!
//! Not to be confused with the app's undo history in the side panel: this is
//! the object's life on the server. Nothing here touches the UI toolkit, so
//! all of it is unit-testable.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::http::{
    fetch_with_retries_progress, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};
use crate::osm::{MemberType, OsmMember};
use crate::selection::FeatureKind;
use crate::{tr, tr_count};

/// Versions shown before "show more".
pub const INITIAL_VERSIONS_SHOWN: usize = 25;
/// How many more "show more" reveals.
pub const SHOW_MORE_STEP: usize = 50;
/// Moves smaller than this many metres are coordinate noise, not a move.
const MOVE_THRESHOLD_M: f64 = 0.05;

/// What an object consists of at one moment: its tags and its geometry or
/// membership. The server's versions and the local, possibly edited, object
/// are both described this way so they can be diffed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectState {
    pub tags: BTreeMap<String, String>,
    /// `(lat, lon)` of a node.
    pub position: Option<(f64, f64)>,
    /// A way's node ids, in order.
    pub nodes: Vec<i64>,
    /// A relation's members, in order.
    pub members: Vec<OsmMember>,
}

/// One version of an object as the server recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryVersion {
    pub version: u32,
    /// Seconds since the Unix epoch.
    pub timestamp: Option<i64>,
    pub user: Option<String>,
    pub uid: Option<i64>,
    pub changeset: Option<i64>,
    /// False for a deletion; the state is then empty.
    pub visible: bool,
    pub state: ObjectState,
}

/// All the versions of one object the server would show, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectHistory {
    pub kind: FeatureKind,
    pub id: i64,
    pub versions: Vec<HistoryVersion>,
}

impl ObjectHistory {
    /// The newest version, deleted or not.
    pub fn latest(&self) -> Option<&HistoryVersion> {
        self.versions.last()
    }

    /// The newest version that is not a deletion.
    pub fn latest_visible(&self) -> Option<&HistoryVersion> {
        self.versions.iter().rev().find(|v| v.visible)
    }

    pub fn version(&self, number: u32) -> Option<&HistoryVersion> {
        self.versions.iter().find(|v| v.version == number)
    }

    /// Version numbers missing from the sequence (redacted versions are not
    /// served), as inclusive `(from, to)` runs, oldest first.
    pub fn missing_runs(&self) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut expected = 1u32;
        for v in &self.versions {
            if v.version > expected {
                runs.push((expected, v.version - 1));
            }
            expected = v.version.saturating_add(1);
        }
        runs
    }
}

/// What the Selection header shows about an object's last edit.
#[derive(Debug, Clone, PartialEq)]
pub struct LastEdit {
    pub version: u32,
    pub user: Option<String>,
    pub changeset: Option<i64>,
    pub timestamp: Option<i64>,
}

impl LastEdit {
    pub fn from_version(v: &HistoryVersion) -> Self {
        LastEdit {
            version: v.version,
            user: v.user.clone(),
            changeset: v.changeset,
            timestamp: v.timestamp,
        }
    }
}

// ---------------------------------------------------------------- parsing

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryParseError(pub String);

impl std::fmt::Display for HistoryParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn attr_map(e: &BytesStart) -> Result<Vec<(String, String)>, HistoryParseError> {
    let mut out = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| HistoryParseError(e.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .unescape_value()
            .map_err(|e| HistoryParseError(e.to_string()))?
            .into_owned();
        out.push((key, value));
    }
    Ok(out)
}

fn get<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn element_kind(name: &[u8]) -> Option<FeatureKind> {
    match name {
        b"node" => Some(FeatureKind::Node),
        b"way" => Some(FeatureKind::Way),
        b"relation" => Some(FeatureKind::Relation),
        _ => None,
    }
}

fn start_version(attrs: &[(String, String)], kind: FeatureKind) -> HistoryVersion {
    let visible = get(attrs, "visible") != Some("false");
    let position = if kind == FeatureKind::Node && visible {
        match (
            get(attrs, "lat").and_then(|v| v.parse::<f64>().ok()),
            get(attrs, "lon").and_then(|v| v.parse::<f64>().ok()),
        ) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            _ => None,
        }
    } else {
        None
    };
    HistoryVersion {
        version: get(attrs, "version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        timestamp: get(attrs, "timestamp").and_then(parse_timestamp),
        user: get(attrs, "user")
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        uid: get(attrs, "uid").and_then(|v| v.parse().ok()),
        changeset: get(attrs, "changeset").and_then(|v| v.parse().ok()),
        visible,
        state: ObjectState {
            position,
            ..ObjectState::default()
        },
    }
}

fn add_child(version: &mut HistoryVersion, name: &[u8], attrs: &[(String, String)]) {
    match name {
        b"tag" => {
            if let Some(k) = get(attrs, "k") {
                version
                    .state
                    .tags
                    .insert(k.to_string(), get(attrs, "v").unwrap_or("").to_string());
            }
        }
        b"nd" => {
            if let Some(r) = get(attrs, "ref").and_then(|v| v.parse().ok()) {
                version.state.nodes.push(r);
            }
        }
        b"member" => {
            let member_type = get(attrs, "type").and_then(|t| t.parse::<MemberType>().ok());
            let reference = get(attrs, "ref").and_then(|v| v.parse().ok());
            if let (Some(member_type), Some(reference)) = (member_type, reference) {
                version.state.members.push(OsmMember {
                    member_type,
                    reference,
                    role: get(attrs, "role").unwrap_or("").to_string(),
                });
            }
        }
        _ => {}
    }
}

/// Parse a `/history` response. Versions come back sorted oldest first and
/// those without a version number are dropped. A deleted version
/// (`visible="false"`) carries no state. Redacted versions simply do not
/// appear (see [`ObjectHistory::missing_runs`]).
pub fn parse_history(
    xml: &str,
    kind: FeatureKind,
    id: i64,
) -> Result<ObjectHistory, HistoryParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut versions: Vec<HistoryVersion> = Vec::new();
    let mut current: Option<HistoryVersion> = None;
    let mut saw_osm = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                if name.as_ref() == b"osm" {
                    saw_osm = true;
                } else if element_kind(name.as_ref()) == Some(kind) {
                    current = Some(start_version(&attr_map(&e)?, kind));
                } else if let Some(v) = current.as_mut() {
                    add_child(v, name.as_ref(), &attr_map(&e)?);
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref() == b"osm" {
                    saw_osm = true;
                } else if element_kind(name.as_ref()) == Some(kind) {
                    versions.push(start_version(&attr_map(&e)?, kind));
                } else if let Some(v) = current.as_mut() {
                    add_child(v, name.as_ref(), &attr_map(&e)?);
                }
            }
            Ok(Event::End(e)) => {
                if element_kind(e.name().as_ref()) == Some(kind) {
                    if let Some(v) = current.take() {
                        versions.push(v);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(HistoryParseError(e.to_string())),
            _ => {}
        }
    }
    if !saw_osm {
        return Err(HistoryParseError("not an OSM document".to_string()));
    }
    versions.retain(|v| v.version > 0);
    versions.sort_by_key(|v| v.version);
    versions.dedup_by_key(|v| v.version);
    Ok(ObjectHistory { kind, id, versions })
}

// ------------------------------------------------------------------ dates

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2021-03-14T09:26:53Z` as Unix seconds. Offsets other than `Z` and
/// fractional seconds are ignored (the OSM API always writes UTC).
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let mut t = time.get(..8)?.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let se: i64 = t.next()?.parse().ok()?;
    Some(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + se)
}

/// `2021-03-14`: a short absolute date that reads the same in every language.
pub fn format_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

// ------------------------------------------------------------------- diff

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagChange {
    Added {
        key: String,
        value: String,
    },
    Removed {
        key: String,
        value: String,
    },
    Changed {
        key: String,
        old: String,
        new: String,
    },
}

/// What changed from one state to the next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StateChange {
    pub tags: Vec<TagChange>,
    /// Tags present and equal in both.
    pub unchanged_tags: usize,
    /// A node's move, in metres.
    pub moved_m: Option<f64>,
    pub nodes_added: usize,
    pub nodes_removed: usize,
    /// Same nodes, different order.
    pub nodes_reordered: bool,
    /// Member counts `(before, after)` when they differ.
    pub member_counts: Option<(usize, usize)>,
    /// Same number of members but different members, roles or order.
    pub members_changed: bool,
}

impl StateChange {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.moved_m.is_none()
            && self.nodes_added == 0
            && self.nodes_removed == 0
            && !self.nodes_reordered
            && self.member_counts.is_none()
            && !self.members_changed
    }
}

/// The change from `prev` (`None`: the object did not exist) to `cur`.
pub fn diff_states(prev: Option<&ObjectState>, cur: &ObjectState) -> StateChange {
    let empty = ObjectState::default();
    let before = prev.unwrap_or(&empty);
    let mut change = StateChange::default();

    for (k, v) in &cur.tags {
        match before.tags.get(k) {
            None => change.tags.push(TagChange::Added {
                key: k.clone(),
                value: v.clone(),
            }),
            Some(old) if old != v => change.tags.push(TagChange::Changed {
                key: k.clone(),
                old: old.clone(),
                new: v.clone(),
            }),
            Some(_) => change.unchanged_tags += 1,
        }
    }
    for (k, v) in &before.tags {
        if !cur.tags.contains_key(k) {
            change.tags.push(TagChange::Removed {
                key: k.clone(),
                value: v.clone(),
            });
        }
    }

    if let (Some((alat, alon)), Some((blat, blon))) = (before.position, cur.position) {
        let d = crate::imagery_offset::distance_m(alat, alon, blat, blon);
        if d > MOVE_THRESHOLD_M {
            change.moved_m = Some(d);
        }
    }

    if prev.is_some() || !cur.nodes.is_empty() {
        let was: HashSet<i64> = before.nodes.iter().copied().collect();
        let now: HashSet<i64> = cur.nodes.iter().copied().collect();
        change.nodes_added = now.difference(&was).count();
        change.nodes_removed = was.difference(&now).count();
        change.nodes_reordered =
            change.nodes_added == 0 && change.nodes_removed == 0 && before.nodes != cur.nodes;
    }

    if before.members.len() != cur.members.len() {
        change.member_counts = Some((before.members.len(), cur.members.len()));
    } else if before.members != cur.members {
        change.members_changed = true;
    }
    change
}

/// One row of the history view.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryEntry {
    /// The local object differs from the newest server version.
    Local { change: StateChange },
    /// A version the server served, with what it changed.
    Version { index: usize, change: StateChange },
    /// Versions the server will not serve (redacted).
    Redacted { from: u32, to: u32 },
}

/// The rows for `history`, newest first. `local` is the object as it is in
/// the editor; when it differs from the newest (not deleted) server version
/// it comes first as "your unsaved changes".
pub fn build_entries(history: &ObjectHistory, local: Option<&ObjectState>) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();

    if let (Some(local), Some(latest)) = (local, history.latest()) {
        if latest.visible {
            let change = diff_states(Some(&latest.state), local);
            if !change.is_empty() {
                entries.push(HistoryEntry::Local { change });
            }
        }
    }

    let mut prev: Option<&HistoryVersion> = None;
    let mut expected = 1u32;
    let mut chronological: Vec<HistoryEntry> = Vec::new();
    for (index, v) in history.versions.iter().enumerate() {
        if v.version > expected {
            chronological.push(HistoryEntry::Redacted {
                from: expected,
                to: v.version - 1,
            });
        }
        expected = v.version.saturating_add(1);
        let before = prev.filter(|p| p.visible).map(|p| &p.state);
        let change = diff_states(before, &v.state);
        chronological.push(HistoryEntry::Version { index, change });
        prev = Some(v);
    }
    entries.extend(chronological.into_iter().rev());
    entries
}

/// The tag edits that turn `current` into `target`: `(key, before, after)`
/// for each key that differs, in key order.
pub fn tag_revert_edits(
    current: &BTreeMap<String, String>,
    target: &BTreeMap<String, String>,
) -> Vec<(String, Option<String>, Option<String>)> {
    let mut edits = Vec::new();
    for (k, v) in target {
        if current.get(k) != Some(v) {
            edits.push((k.clone(), current.get(k).cloned(), Some(v.clone())));
        }
    }
    for (k, v) in current {
        if !target.contains_key(k) {
            edits.push((k.clone(), Some(v.clone()), None));
        }
    }
    edits.sort_by(|a, b| a.0.cmp(&b.0));
    edits
}

/// "12.3 m" or "1.4 km".
pub fn format_distance(metres: f64) -> String {
    if metres < 1000.0 {
        format!("{metres:.1} m")
    } else {
        format!("{:.2} km", metres / 1000.0)
    }
}

// ------------------------------------------------------------- the fetch

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    /// 404: the server has never heard of this object.
    NotFound,
    /// 410: permanently removed.
    Gone,
    Network(String),
    Http {
        status: u16,
    },
    Parse(String),
    /// Every version is hidden.
    Empty,
    Cancelled,
}

impl HistoryError {
    /// Worth offering a Retry button.
    pub fn is_retryable(&self) -> bool {
        matches!(self, HistoryError::Network(_))
            || matches!(self, HistoryError::Http { status } if crate::is_retryable_status(*status))
    }

    /// A readable message; `server` names the API server asked.
    pub fn message(&self, server: &str) -> String {
        match self {
            HistoryError::NotFound => {
                tr!("object_history.error.not_found", server = server).to_string()
            }
            HistoryError::Gone => tr!("object_history.error.gone").to_string(),
            HistoryError::Network(detail) => {
                tr!("object_history.error.network", detail = detail).to_string()
            }
            HistoryError::Http { status } => {
                tr!("object_history.error.http", status = status).to_string()
            }
            HistoryError::Parse(detail) => {
                tr!("object_history.error.parse", detail = detail).to_string()
            }
            HistoryError::Empty => tr!("object_history.error.empty").to_string(),
            HistoryError::Cancelled => tr!("object_history.error.cancelled").to_string(),
        }
    }
}

fn kind_noun(kind: FeatureKind) -> &'static str {
    match kind {
        FeatureKind::Node => "node",
        FeatureKind::Way => "way",
        FeatureKind::Relation => "relation",
    }
}

/// `{api}/api/0.6/way/123/history`
pub fn history_url(api_base: &str, kind: FeatureKind, id: i64) -> String {
    format!(
        "{}/api/0.6/{}/{}/history",
        api_base.trim_end_matches('/'),
        kind_noun(kind),
        id
    )
}

/// The object's page on the website: `{web}/way/123`.
pub fn web_object_url(web_base: &str, kind: FeatureKind, id: i64) -> String {
    format!(
        "{}/{}/{}",
        web_base.trim_end_matches('/'),
        kind_noun(kind),
        id
    )
}

/// The object's history page on the website: `{web}/way/123/history`.
pub fn web_history_url(web_base: &str, kind: FeatureKind, id: i64) -> String {
    format!("{}/history", web_object_url(web_base, kind, id))
}

pub fn web_changeset_url(web_base: &str, changeset: i64) -> String {
    format!("{}/changeset/{}", web_base.trim_end_matches('/'), changeset)
}

pub fn web_user_url(web_base: &str, user: &str) -> String {
    format!(
        "{}/user/{}",
        web_base.trim_end_matches('/'),
        urlencoding::encode(user)
    )
}

/// Fetch and parse an object's history. Anonymous: no credentials are ever
/// sent. Synchronous, so call it from a worker thread; setting `cancel`
/// abandons the transfer between chunks.
pub fn fetch_history_with(
    client: &dyn HttpClient,
    api_base: &str,
    kind: FeatureKind,
    id: i64,
    cancel: &AtomicBool,
) -> Result<ObjectHistory, HistoryError> {
    let req = HttpRequest::get(history_url(api_base, kind, id));
    let resp = fetch_with_retries_progress(client, &req, &RetryPolicy::standard(), &mut |_, _| {
        !cancel.load(Ordering::Relaxed)
    })
    .map_err(|e| match e {
        HttpError::Status { status: 404, .. } => HistoryError::NotFound,
        HttpError::Status { status: 410, .. } => HistoryError::Gone,
        HttpError::Status { status, .. } => HistoryError::Http { status },
        HttpError::Transport(msg) => HistoryError::Network(msg),
        HttpError::Cancelled => HistoryError::Cancelled,
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(HistoryError::Cancelled);
    }
    let body = resp
        .into_string()
        .map_err(|e| HistoryError::Network(e.to_string()))?;
    parse_response(&body, kind, id)
}

/// `fetch_history_with` against the real network.
pub fn fetch_history(
    api_base: &str,
    kind: FeatureKind,
    id: i64,
    cancel: &AtomicBool,
) -> Result<ObjectHistory, HistoryError> {
    fetch_history_with(&UreqClient::new(), api_base, kind, id, cancel)
}

/// Parse a history document, mapping its failures to [`HistoryError`].
pub fn parse_response(
    body: &str,
    kind: FeatureKind,
    id: i64,
) -> Result<ObjectHistory, HistoryError> {
    let history = parse_history(body, kind, id).map_err(|e| HistoryError::Parse(e.to_string()))?;
    if history.versions.is_empty() {
        return Err(HistoryError::Empty);
    }
    Ok(history)
}

/// The label of a gap row: `v3` or `v3-v5`, with a plural-aware sentence.
pub fn redacted_text(from: u32, to: u32) -> String {
    let range = if from == to {
        from.to_string()
    } else {
        format!("{from}\u{2013}{to}")
    };
    tr_count!("object_history.redacted", to - from + 1, range = range).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, transport_err, FakeClient};

    const NODE_HISTORY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6" generator="CGImap">
 <node id="42" visible="true" version="1" changeset="100" timestamp="2020-01-02T03:04:05Z" user="alice" uid="7" lat="40.0000000" lon="-74.0000000">
  <tag k="amenity" v="cafe"/>
  <tag k="name" v="Joe &amp; Co"/>
 </node>
 <node id="42" visible="true" version="2" changeset="101" timestamp="2020-06-01T00:00:00Z" user="bob" uid="8" lat="40.0001000" lon="-74.0000000">
  <tag k="amenity" v="cafe"/>
  <tag k="name" v="Joe's"/>
  <tag k="cuisine" v="coffee"/>
 </node>
 <node id="42" visible="false" version="3" changeset="102" timestamp="2021-01-01T00:00:00Z" user="carol" uid="9"/>
 <node id="42" visible="true" version="6" changeset="105" timestamp="2022-01-01T12:00:00Z" user="dave" uid="10" lat="40.0001000" lon="-74.0000000">
  <tag k="amenity" v="restaurant"/>
 </node>
</osm>"#;

    const WAY_HISTORY: &str = r#"<osm version="0.6">
 <way id="9" visible="true" version="1" changeset="1" timestamp="2019-05-05T10:00:00Z" user="a" uid="1">
  <nd ref="1"/><nd ref="2"/><nd ref="3"/>
  <tag k="highway" v="residential"/>
 </way>
 <way id="9" visible="true" version="2" changeset="2" timestamp="2019-06-05T10:00:00Z" user="b" uid="2">
  <nd ref="1"/><nd ref="2"/><nd ref="4"/><nd ref="5"/>
  <tag k="highway" v="tertiary"/>
  <tag k="name" v="Main"/>
 </way>
</osm>"#;

    const RELATION_HISTORY: &str = r#"<osm version="0.6">
 <relation id="5" visible="true" version="1" changeset="1" timestamp="2018-01-01T00:00:00Z" user="a" uid="1">
  <member type="way" ref="10" role="outer"/>
  <tag k="type" v="multipolygon"/>
 </relation>
 <relation id="5" visible="true" version="2" changeset="2" timestamp="2018-02-01T00:00:00Z" user="a" uid="1">
  <member type="way" ref="10" role="outer"/>
  <member type="way" ref="11" role="inner"/>
  <tag k="type" v="multipolygon"/>
 </relation>
</osm>"#;

    #[test]
    fn parses_a_node_history_with_a_deleted_version_and_a_redacted_gap() {
        let h = parse_history(NODE_HISTORY, FeatureKind::Node, 42).unwrap();
        let numbers: Vec<u32> = h.versions.iter().map(|v| v.version).collect();
        assert_eq!(numbers, vec![1, 2, 3, 6]);
        let v1 = &h.versions[0];
        assert_eq!(v1.user.as_deref(), Some("alice"));
        assert_eq!(v1.uid, Some(7));
        assert_eq!(v1.changeset, Some(100));
        assert_eq!(v1.timestamp, Some(1_577_934_245));
        assert_eq!(v1.state.position, Some((40.0, -74.0)));
        assert_eq!(v1.state.tags["name"], "Joe & Co");
        let v3 = &h.versions[2];
        assert!(!v3.visible);
        assert!(v3.state.tags.is_empty());
        assert_eq!(v3.state.position, None);
        // Versions 4 and 5 were redacted.
        assert_eq!(h.missing_runs(), vec![(4, 5)]);
        assert_eq!(h.latest().unwrap().version, 6);
        assert_eq!(h.latest_visible().unwrap().version, 6);
    }

    #[test]
    fn parses_ways_and_relations() {
        let w = parse_history(WAY_HISTORY, FeatureKind::Way, 9).unwrap();
        assert_eq!(w.versions[1].state.nodes, vec![1, 2, 4, 5]);
        assert_eq!(w.versions[1].state.tags["name"], "Main");
        let r = parse_history(RELATION_HISTORY, FeatureKind::Relation, 5).unwrap();
        assert_eq!(r.versions[1].state.members.len(), 2);
        assert_eq!(r.versions[1].state.members[1].role, "inner");
        assert_eq!(r.versions[1].state.members[1].member_type, MemberType::Way);
        assert!(r.missing_runs().is_empty());
    }

    #[test]
    fn a_history_missing_its_first_versions_has_a_leading_gap() {
        let xml = r#"<osm><node id="1" version="3" visible="true" lat="1" lon="2"/></osm>"#;
        let h = parse_history(xml, FeatureKind::Node, 1).unwrap();
        assert_eq!(h.missing_runs(), vec![(1, 2)]);
    }

    #[test]
    fn rubbish_is_a_parse_error_and_nothing_visible_is_empty() {
        assert!(parse_history("hello", FeatureKind::Node, 1).is_err());
        assert_eq!(
            parse_response("<osm/>", FeatureKind::Node, 1),
            Err(HistoryError::Empty)
        );
        assert!(matches!(
            parse_response("<osm><node", FeatureKind::Node, 1),
            Err(HistoryError::Parse(_))
        ));
    }

    #[test]
    fn timestamps_round_trip_through_dates() {
        let ts = parse_timestamp("2021-03-14T09:26:53Z").unwrap();
        assert_eq!(ts, 1_615_714_013);
        assert_eq!(format_date(ts), "2021-03-14");
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(
            format_date(parse_timestamp("2024-02-29T23:59:59Z").unwrap()),
            "2024-02-29"
        );
        assert_eq!(parse_timestamp("yesterday"), None);
    }

    fn state(tags: &[(&str, &str)]) -> ObjectState {
        ObjectState {
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..ObjectState::default()
        }
    }

    #[test]
    fn tag_changes_are_added_removed_changed_and_unchanged_counted() {
        let a = state(&[("a", "1"), ("b", "2"), ("c", "3"), ("keep", "x")]);
        let b = state(&[("a", "1"), ("b", "20"), ("d", "4"), ("keep", "x")]);
        let c = diff_states(Some(&a), &b);
        assert_eq!(
            c.tags,
            vec![
                TagChange::Changed {
                    key: "b".into(),
                    old: "2".into(),
                    new: "20".into()
                },
                TagChange::Added {
                    key: "d".into(),
                    value: "4".into()
                },
                TagChange::Removed {
                    key: "c".into(),
                    value: "3".into()
                },
            ]
        );
        assert_eq!(c.unchanged_tags, 2);
        assert!(!c.is_empty());
        assert!(diff_states(Some(&a), &a).is_empty());
    }

    #[test]
    fn creation_adds_everything() {
        let c = diff_states(None, &state(&[("a", "1")]));
        assert_eq!(c.tags.len(), 1);
        assert!(matches!(c.tags[0], TagChange::Added { .. }));
    }

    #[test]
    fn a_moved_node_reports_metres_and_noise_is_ignored() {
        let at = |lat: f64, lon: f64| ObjectState {
            position: Some((lat, lon)),
            ..ObjectState::default()
        };
        // 0.0001 degrees of latitude is about 11.1 m.
        let c = diff_states(Some(&at(40.0, -74.0)), &at(40.0001, -74.0));
        let d = c.moved_m.unwrap();
        assert!((d - 11.1).abs() < 0.3, "{d}");
        assert!(diff_states(Some(&at(40.0, -74.0)), &at(40.0, -74.0)).is_empty());
        assert!(diff_states(Some(&at(40.0, -74.0)), &at(40.000_000_1, -74.0)).is_empty());
    }

    #[test]
    fn way_node_changes_count_added_removed_and_reordering() {
        let ways = |nodes: &[i64]| ObjectState {
            nodes: nodes.to_vec(),
            ..ObjectState::default()
        };
        let c = diff_states(Some(&ways(&[1, 2, 3])), &ways(&[1, 2, 4, 5]));
        assert_eq!((c.nodes_added, c.nodes_removed), (2, 1));
        assert!(!c.nodes_reordered);
        let c = diff_states(Some(&ways(&[1, 2, 3])), &ways(&[3, 2, 1]));
        assert!(c.nodes_reordered);
        assert_eq!((c.nodes_added, c.nodes_removed), (0, 0));
    }

    #[test]
    fn relation_member_changes() {
        let h = parse_history(RELATION_HISTORY, FeatureKind::Relation, 5).unwrap();
        let c = diff_states(Some(&h.versions[0].state), &h.versions[1].state);
        assert_eq!(c.member_counts, Some((1, 2)));
        let mut swapped = h.versions[1].state.clone();
        swapped.members[0].role = "inner".into();
        let c = diff_states(Some(&h.versions[1].state), &swapped);
        assert!(c.members_changed);
        assert_eq!(c.member_counts, None);
    }

    #[test]
    fn entries_run_newest_first_with_deleted_and_redacted_rows() {
        let h = parse_history(NODE_HISTORY, FeatureKind::Node, 42).unwrap();
        let entries = build_entries(&h, None);
        // v6, the hidden 4-5, v3 (deleted), v2, v1.
        assert_eq!(entries.len(), 5);
        assert!(matches!(entries[0], HistoryEntry::Version { index: 3, .. }));
        assert_eq!(entries[1], HistoryEntry::Redacted { from: 4, to: 5 });
        assert!(matches!(entries[2], HistoryEntry::Version { index: 2, .. }));
        assert!(matches!(entries[4], HistoryEntry::Version { index: 0, .. }));
        // v3 removed both tags and the position...
        let HistoryEntry::Version { change, .. } = &entries[2] else {
            panic!()
        };
        assert_eq!(change.tags.len(), 3);
        // ...and v6, after the gap, is diffed against the deleted v3, so it
        // re-creates the object.
        let HistoryEntry::Version { change, .. } = &entries[0] else {
            panic!()
        };
        assert_eq!(change.tags.len(), 1);
        assert!(matches!(change.tags[0], TagChange::Added { .. }));
    }

    #[test]
    fn unsaved_local_changes_come_first_and_only_when_different() {
        let h = parse_history(WAY_HISTORY, FeatureKind::Way, 9).unwrap();
        let latest = h.latest().unwrap().state.clone();
        assert_eq!(build_entries(&h, Some(&latest)).len(), 2);

        let mut edited = latest.clone();
        edited.tags.insert("surface".into(), "asphalt".into());
        edited.tags.insert("highway".into(), "primary".into());
        edited.nodes.push(9);
        let entries = build_entries(&h, Some(&edited));
        assert_eq!(entries.len(), 3);
        let HistoryEntry::Local { change } = &entries[0] else {
            panic!("local first")
        };
        assert_eq!(change.tags.len(), 2);
        assert_eq!(change.nodes_added, 1);
    }

    #[test]
    fn revert_edits_turn_current_tags_into_the_target() {
        let cur = state(&[("a", "1"), ("b", "2"), ("c", "3")]).tags;
        let target = state(&[("a", "1"), ("b", "9"), ("d", "4")]).tags;
        let edits = tag_revert_edits(&cur, &target);
        assert_eq!(
            edits,
            vec![
                (
                    "b".to_string(),
                    Some("2".to_string()),
                    Some("9".to_string())
                ),
                ("c".to_string(), Some("3".to_string()), None),
                ("d".to_string(), None, Some("4".to_string())),
            ]
        );
        assert!(tag_revert_edits(&cur, &cur).is_empty());
    }

    #[test]
    fn request_urls_follow_the_api_base() {
        assert_eq!(
            history_url("https://api.openstreetmap.org", FeatureKind::Way, 123),
            "https://api.openstreetmap.org/api/0.6/way/123/history"
        );
        assert_eq!(
            history_url("https://example.test/", FeatureKind::Relation, 5),
            "https://example.test/api/0.6/relation/5/history"
        );
        assert_eq!(
            web_history_url("https://www.openstreetmap.org/", FeatureKind::Node, 1),
            "https://www.openstreetmap.org/node/1/history"
        );
        assert_eq!(
            web_changeset_url("https://www.openstreetmap.org", 77),
            "https://www.openstreetmap.org/changeset/77"
        );
        assert_eq!(
            web_user_url("https://www.openstreetmap.org", "Joe Q"),
            "https://www.openstreetmap.org/user/Joe%20Q"
        );
    }

    #[test]
    fn the_fetch_asks_for_the_history_url_anonymously() {
        let client = FakeClient::new(vec![ok(200, WAY_HISTORY)]);
        let cancel = AtomicBool::new(false);
        let h = fetch_history_with(
            &client,
            "https://api.example.test",
            FeatureKind::Way,
            9,
            &cancel,
        )
        .unwrap();
        assert_eq!(h.versions.len(), 2);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://api.example.test/api/0.6/way/9/history"
        );
        assert!(requests[0].headers.is_empty(), "no Authorization header");
    }

    #[test]
    fn errors_are_mapped_and_only_network_trouble_is_retryable() {
        let fetch = |responses| {
            let client = FakeClient::new(responses);
            fetch_history_with(
                &client,
                "https://api.example.test",
                FeatureKind::Node,
                1,
                &AtomicBool::new(false),
            )
        };
        assert_eq!(
            fetch(vec![status_err(404, "")]),
            Err(HistoryError::NotFound)
        );
        assert_eq!(fetch(vec![status_err(410, "")]), Err(HistoryError::Gone));
        assert_eq!(
            fetch(vec![status_err(403, "")]),
            Err(HistoryError::Http { status: 403 })
        );
        // A 503 is retried, then reported.
        assert_eq!(
            fetch(vec![
                status_err(503, ""),
                status_err(503, ""),
                status_err(503, "")
            ]),
            Err(HistoryError::Http { status: 503 })
        );
        let net = fetch(vec![
            transport_err("dns"),
            transport_err("dns"),
            transport_err("dns"),
        ]);
        assert_eq!(net, Err(HistoryError::Network("dns".into())));
        assert!(net.unwrap_err().is_retryable());
        assert!(!HistoryError::NotFound.is_retryable());
        assert!(!HistoryError::Gone.is_retryable());
        assert!(matches!(
            fetch(vec![ok(200, "<html>")]),
            Err(HistoryError::Parse(_))
        ));
    }

    #[test]
    fn a_cancelled_fetch_is_reported_as_cancelled() {
        let client = FakeClient::new(vec![ok(200, WAY_HISTORY)]);
        let cancel = AtomicBool::new(true);
        let r = fetch_history_with(
            &client,
            "https://api.example.test",
            FeatureKind::Way,
            9,
            &cancel,
        );
        assert_eq!(r, Err(HistoryError::Cancelled));
    }

    #[test]
    fn error_messages_are_readable() {
        let m = HistoryError::NotFound.message("https://api.example.test");
        assert!(m.contains("api.example.test"), "{m}");
        assert!(HistoryError::Gone.message("").contains("removed"));
        assert!(HistoryError::Network("timeout".into())
            .message("")
            .contains("timeout"));
        assert!(HistoryError::Http { status: 500 }
            .message("")
            .contains("500"));
        assert_eq!(redacted_text(4, 4), "Version 4 is hidden (redacted)");
        assert_eq!(
            redacted_text(4, 5),
            "Versions 4\u{2013}5 are hidden (redacted)"
        );
    }

    #[test]
    fn distances_read_in_metres_then_kilometres() {
        assert_eq!(format_distance(12.34), "12.3 m");
        assert_eq!(format_distance(1500.0), "1.50 km");
    }

    #[test]
    fn a_long_history_builds_quickly() {
        let mut xml = String::from("<osm>");
        for v in 1..=600 {
            xml.push_str(&format!(
                "<way id=\"1\" version=\"{v}\" visible=\"true\" changeset=\"{v}\" timestamp=\"2020-01-01T00:00:00Z\" user=\"u\" uid=\"1\">"
            ));
            for n in 0..200 {
                xml.push_str(&format!("<nd ref=\"{}\"/>", n + v));
            }
            xml.push_str(&format!("<tag k=\"v\" v=\"{v}\"/></way>"));
        }
        xml.push_str("</osm>");
        let h = parse_response(&xml, FeatureKind::Way, 1).unwrap();
        assert_eq!(h.versions.len(), 600);
        assert_eq!(build_entries(&h, None).len(), 600);
    }
}
