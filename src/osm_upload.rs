//! OSM API 0.6 changeset lifecycle (create/upload/close) and osmChange XML
//! (de)serialization. See https://wiki.openstreetmap.org/wiki/API_v0.6#Changesets.
//!
//! Every network call here is synchronous and must be run from a background
//! executor, never the UI thread.

use std::collections::HashMap;

use crate::changeset_tags::ChangesetTags;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpFailure, HttpRequest, Method, RetryPolicy,
};
use crate::layers::diff::LayerDiff;
use crate::osm::{OsmNode, OsmRelation, OsmWay};
use crate::selection::FeatureKind;
use crate::tr;
use crate::transfer::TransferPhase;

/// Why the server refused a diff upload, in terms the editor can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    /// Someone else saved the object after we downloaded it: we sent
    /// `provided` as its base version, the server is at `server`.
    VersionMismatch { provided: i32, server: i32 },
    /// The object was already deleted on the server.
    AlreadyDeleted,
    /// We tried to delete an object that other objects still use.
    StillInUse { by: Vec<(FeatureKind, i64)> },
    /// A way or relation refers to objects that don't exist or were deleted.
    MissingDependency,
    /// Anything else the server refused with 409/410/412 (a closed changeset,
    /// an unrecognised message).
    Other,
}

/// A refused diff upload (HTTP 409, 410 or 412). The API stops at the first
/// problem, so this names one object at most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadConflict {
    pub kind: ConflictKind,
    /// The object the message is about, when it names one.
    pub object: Option<(FeatureKind, i64)>,
    /// HTTP status of the refusal.
    pub status: u16,
    /// The server's message, verbatim.
    pub message: String,
}

/// The words of `text` lowercased, split on anything but letters, digits and
/// `-` (ids can be negative in odd cases).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn element_word(word: &str) -> Option<FeatureKind> {
    match word.trim_end_matches('s') {
        "node" => Some(FeatureKind::Node),
        "way" => Some(FeatureKind::Way),
        "relation" => Some(FeatureKind::Relation),
        _ => None,
    }
}

/// Every `(kind, id)` named in `words`, in order: an element word, optional
/// filler ("with", "the", "id"), then a number. "The node with the id 7" and
/// "Way 5" both give one reference; "ways 1,2" gives two (a list shares the
/// element word).
fn element_refs(words: &[String]) -> Vec<(FeatureKind, i64)> {
    let mut refs = Vec::new();
    let mut current: Option<FeatureKind> = None;
    for word in words {
        if let Some(kind) = element_word(word) {
            current = Some(kind);
        } else if let Ok(id) = word.parse::<i64>() {
            if let Some(kind) = current {
                refs.push((kind, id));
            }
        } else if !matches!(word.as_str(), "with" | "the" | "id" | "ids" | "of" | "and") {
            current = None;
        }
    }
    refs
}

/// The number right after the first occurrence of `word` in `words`.
fn number_after(words: &[String], word: &str) -> Option<i32> {
    let at = words.iter().position(|w| w == word)?;
    words.get(at + 1)?.parse().ok()
}

impl UploadConflict {
    /// Interpret a 409/410/412 response from the diff upload. Tolerant of
    /// small wording differences between the Rails port and CGImap (an
    /// optional "Precondition failed:" prefix, singular or plural "way(s)",
    /// punctuation); anything unrecognised is `ConflictKind::Other`.
    pub fn parse(status: u16, body: &str) -> UploadConflict {
        let message = body.trim().to_string();
        let lower = message.to_lowercase();
        let all = words(&message);
        // The object is the first element reference in the text, except that
        // "...server had: 4 of Node 12345" is found the same way.
        let object = element_refs(&all).first().copied();

        let kind = if lower.contains("version mismatch") {
            match (number_after(&all, "provided"), number_after(&all, "had")) {
                (Some(provided), Some(server)) => {
                    ConflictKind::VersionMismatch { provided, server }
                }
                _ => ConflictKind::Other,
            }
        } else if lower.contains("already been deleted")
            || lower.contains("already deleted")
            || status == 410
        {
            ConflictKind::AlreadyDeleted
        } else if let Some(at) = ["still used by", "is used in", "still used"]
            .iter()
            .find_map(|p| lower.find(p).map(|i| i + p.len()))
        {
            let by = element_refs(&words(&lower[at..]));
            ConflictKind::StillInUse { by }
        } else if lower.contains("requires the")
            || lower.contains("cannot be saved due to")
            || lower.contains("do not exist, or are not visible")
        {
            ConflictKind::MissingDependency
        } else {
            ConflictKind::Other
        };
        UploadConflict {
            kind,
            object,
            status,
            message,
        }
    }

    /// The named object in plain words ("Node 12345"), or a generic phrase
    /// when the server named none.
    pub fn object_name(&self) -> String {
        match self.object {
            Some((kind, id)) => describe_object(kind, id),
            None => tr!("upload_conflict.something").to_string(),
        }
    }

    /// One plain-language sentence saying what went wrong.
    pub fn describe(&self) -> String {
        let object = self.object_name();
        match &self.kind {
            ConflictKind::VersionMismatch { provided, server } => tr!(
                "upload_conflict.version_mismatch",
                object = object,
                provided = provided,
                server = server
            )
            .to_string(),
            ConflictKind::AlreadyDeleted => {
                tr!("upload_conflict.already_deleted", object = object).to_string()
            }
            ConflictKind::StillInUse { by } => {
                if by.is_empty() {
                    tr!("upload_conflict.still_in_use_unknown", object = object).to_string()
                } else {
                    let by = by
                        .iter()
                        .map(|&(kind, id)| describe_object(kind, id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    tr!("upload_conflict.still_in_use", object = object, by = by).to_string()
                }
            }
            ConflictKind::MissingDependency => {
                tr!("upload_conflict.missing_dependency", object = object).to_string()
            }
            ConflictKind::Other => tr!(
                "upload_conflict.other",
                status = self.status,
                detail = self.message.lines().next().unwrap_or("")
            )
            .to_string(),
        }
    }
}

/// "Node 12345", "Way 5", "Relation 7".
pub fn describe_object(kind: FeatureKind, id: i64) -> String {
    match kind {
        FeatureKind::Node => tr!("upload_conflict.node", id = id),
        FeatureKind::Way => tr!("upload_conflict.way", id = id),
        FeatureKind::Relation => tr!("upload_conflict.relation", id = id),
    }
    .to_string()
}

#[derive(Debug)]
pub enum UploadError {
    /// Transport-level failure (no HTTP response received at all).
    Network(String),
    /// A non-2xx HTTP response not covered by a more specific variant below.
    Http { status: u16, body: String },
    /// 409/410/412 from the server: an edit it can't apply (see
    /// `UploadConflict`). Never retried automatically.
    Conflict(UploadConflict),
    /// The response body wasn't parseable XML/text in the shape we expected.
    Parse(String),
    /// The diff upload got no usable answer (transport failure, 5xx, or an
    /// unreadable success body), so the server may have applied it. Not
    /// retried automatically; the user must check the changeset first.
    OutcomeUnknown {
        changeset_id: u64,
        /// The changeset's page on the OSM website.
        url: String,
        /// What went wrong, as the underlying error reads.
        detail: String,
    },
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Network(msg) => write!(f, "{}", tr!("error.network", detail = msg)),
            UploadError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "{}",
                    tr!("error.api.http", status = status, detail = first_line)
                )
            }
            UploadError::Conflict(conflict) => write!(f, "{}", conflict.describe()),
            UploadError::Parse(msg) => write!(f, "{}", tr!("error.api.parse", detail = msg)),
            UploadError::OutcomeUnknown { url, detail, .. } => write!(
                f,
                "{}",
                tr!("error.api.outcome_unknown", detail = detail, url = url)
            ),
        }
    }
}

impl std::error::Error for UploadError {}

/// A failed upload session: the error, and the changeset that was opened for
/// it (already closed, best effort) if one got that far.
#[derive(Debug)]
pub struct UploadFailure {
    pub error: UploadError,
    pub changeset_id: Option<u64>,
}

/// Per-changeset result of a successful `upload_changes_with` call: for every
/// node/way/relation the server accepted, maps its LOCAL id (the id we sent: a
/// possibly-negative placeholder for a create, the real id for a modify) to its
/// new `(id, version)`. A modify maps to the same id; a create maps to the
/// server-assigned id. Deleted elements need no remap and are not represented.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadResult {
    pub node_id_remap: HashMap<i64, (i64, i32)>,
    pub way_id_remap: HashMap<i64, (i64, i32)>,
    pub relation_id_remap: HashMap<i64, (i64, i32)>,
}

/// Send a single changeset-lifecycle request under `policy`.
///
/// Only the idempotent close is retried (`RetryPolicy::standard`: 3
/// attempts). A transport error or 5xx does not say whether the server acted
/// on the request: it may have committed and then lost the response. For the
/// diff upload a retry would then apply the same osmChange twice, creating
/// every new node/way/relation again on OpenStreetMap (modifies and deletes
/// would only fail on a version conflict). Creating a changeset is not
/// idempotent either (a retry leaves an extra open changeset), and the HTTP
/// layer cannot tell "request never sent" from "response lost", so create and
/// upload are single attempts (`RetryPolicy::none`). Closing twice is
/// harmless (409 "already closed"). 409/410/412 are never retried: the
/// same payload would fail again.
fn call_with_retries(
    client: &dyn HttpClient,
    policy: &RetryPolicy,
    method: Method,
    url: &str,
    token: &str,
    body: Option<String>,
) -> Result<String, UploadError> {
    let request = HttpRequest::with_text(method, url, body)
        .bearer(token)
        .header("Content-Type", "text/xml; charset=utf-8");
    fetch_with_retries(client, &request, policy)?
        .into_string()
        .map_err(|e| UploadError::Network(e.to_string()))
}

impl From<HttpError> for UploadError {
    fn from(e: HttpError) -> Self {
        match HttpFailure::from(e) {
            HttpFailure::Status { status, body } if matches!(status, 409 | 410 | 412) => {
                UploadError::Conflict(UploadConflict::parse(status, &body))
            }
            HttpFailure::Status { status, body } => UploadError::Http { status, body },
            HttpFailure::Transport(msg) => UploadError::Network(msg),
            HttpFailure::Cancelled => UploadError::Network(HttpError::Cancelled.to_string()),
        }
    }
}

/// `PUT {base_url}/api/0.6/changeset/create`. Returns the new changeset id.
///
/// `tags` is the comment alone (`&str`) or the full tag list from
/// `changeset_tags::build_changeset_tags`.
pub fn create_changeset_with(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    tags: impl Into<ChangesetTags>,
) -> Result<u64, UploadError> {
    let url = format!(
        "{}/api/0.6/changeset/create",
        base_url.trim_end_matches('/')
    );
    let xml = build_changeset_create_xml(&tags.into());
    let body = call_with_retries(
        client,
        &RetryPolicy::none(),
        Method::Put,
        &url,
        token,
        Some(xml),
    )?;
    body.trim()
        .parse::<u64>()
        .map_err(|_| UploadError::Parse(format!("invalid changeset id in response: {:?}", body)))
}

fn build_changeset_create_xml(tags: &ChangesetTags) -> String {
    let mut xml = String::from("<osm><changeset>");
    for (key, value) in tags.pairs() {
        xml.push_str(&format!(
            r#"<tag k="{}" v="{}"/>"#,
            xml_escape(key),
            xml_escape(value)
        ));
    }
    xml.push_str("</changeset></osm>");
    xml
}

/// `POST {base_url}/api/0.6/changeset/{id}/upload` with `xml` as the body.
pub fn upload_changes_with(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    changeset_id: u64,
    xml: &str,
) -> Result<UploadResult, UploadError> {
    let url = format!(
        "{}/api/0.6/changeset/{}/upload",
        base_url.trim_end_matches('/'),
        changeset_id
    );
    let body = call_with_retries(
        client,
        &RetryPolicy::none(),
        Method::Post,
        &url,
        token,
        Some(xml.to_string()),
    )?;
    parse_diff_result(&body)
}

/// `PUT {base_url}/api/0.6/changeset/{id}/close`.
pub fn close_changeset_with(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    changeset_id: u64,
) -> Result<(), UploadError> {
    let url = format!(
        "{}/api/0.6/changeset/{}/close",
        base_url.trim_end_matches('/'),
        changeset_id
    );
    call_with_retries(
        client,
        &RetryPolicy::standard(),
        Method::Put,
        &url,
        token,
        None,
    )?;
    Ok(())
}

/// The whole upload: open a changeset, send the diff, close the changeset.
/// `on_phase` is told each stage as it starts (opening is the caller's
/// initial state).
///
/// Whatever goes wrong after the changeset is open — the diff refused,
/// the connection lost — the changeset is closed (best effort, a failure to
/// close is ignored: the report is about the upload) before this returns, so
/// a failed upload never leaves one dangling on the server. The refusal is
/// the error that matters, so it is what comes back.
pub fn run_upload(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    tags: impl Into<ChangesetTags>,
    layers: &[(&str, LayerDiff)],
    on_phase: &mut dyn FnMut(TransferPhase),
) -> Result<(u64, UploadResult), UploadFailure> {
    let changeset_id =
        create_changeset_with(client, base_url, token, tags).map_err(|error| UploadFailure {
            error,
            changeset_id: None,
        })?;

    let xml = build_osm_change_xml(changeset_id, layers);
    let count: usize = layers
        .iter()
        .map(|(_, d)| {
            let (created, modified, deleted) = d.counts();
            created + modified + deleted
        })
        .sum();
    on_phase(TransferPhase::UploadingChanges { count });
    let uploaded = upload_changes_with(client, base_url, token, changeset_id, &xml);

    if uploaded.is_ok() {
        on_phase(TransferPhase::ClosingChangeset);
    }
    // Best effort either way; see the doc comment.
    let _ = close_changeset_with(client, base_url, token, changeset_id);

    match uploaded {
        Ok(result) => Ok((changeset_id, result)),
        Err(error) => Err(UploadFailure {
            error: mark_outcome_unknown(error, base_url, changeset_id),
            changeset_id: Some(changeset_id),
        }),
    }
}

/// Turn a diff-upload failure that leaves the server's state unknown into
/// `OutcomeUnknown`: a transport error, a 5xx (other than 509, which is
/// refused before any work), or a success body we could not read. Anything
/// else (conflicts, 4xx) is a definite refusal and passes through.
fn mark_outcome_unknown(error: UploadError, base_url: &str, changeset_id: u64) -> UploadError {
    let detail = match &error {
        UploadError::Network(_) | UploadError::Parse(_) => error.to_string(),
        UploadError::Http { status, .. } if (500..600).contains(status) && *status != 509 => {
            error.to_string()
        }
        _ => return error,
    };
    UploadError::OutcomeUnknown {
        changeset_id,
        url: format!(
            "{}/changeset/{}",
            crate::auth::oauth_base_for(base_url),
            changeset_id
        ),
        detail,
    }
}

/// XML-escape text usable in both attribute values and element text content
/// (the escaped set — `&` `<` `>` `"` `'` — is a safe superset for both
/// contexts). `&` must be escaped first so we don't double-escape the
/// entities it introduces.
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        // A literal line break or tab inside an attribute value is turned
        // into a plain space by any conforming XML reader (the OSM server,
        // JOSM), so a multi-line `note=*` would change in transit. As
        // character references they arrive intact.
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

/// Serialize the combined diff from every modified layer into one osmChange
/// document. Created nodes/ways use their local (possibly negative) id as
/// the `id` attribute, per the OSM API's convention for referencing
/// not-yet-created elements within the same changeset upload.
///
/// The API applies a changeset's elements in document order and checks
/// references as it goes, so the order matters:
///
/// - `create`: nodes, ways, then relations (a way's `nd` may name a node
///   created here; a relation member may name any of them). Created relations
///   are in dependency order: a relation that is a member of another created
///   relation comes first (a cycle falls back to id order and is left for the
///   API to reject);
/// - `modify`: nodes, then ways, then relations, so a relation member that
///   names a created or modified way/node (by placeholder id when new) finds
///   it already in place;
/// - `delete`: relations, then ways, then nodes (a way still in a relation, or
///   a node still used by a way, can't go first). Relations that lose a
///   deleted element as a member are modified earlier.
pub fn build_osm_change_xml(changeset_id: u64, layers: &[(&str, LayerDiff)]) -> String {
    let mut create_nodes = String::new();
    let mut create_ways = String::new();
    let mut modify_nodes = String::new();
    let mut modify_ways = String::new();
    let mut modify_relations = String::new();
    let mut delete_relations = String::new();
    let mut delete_ways = String::new();
    let mut delete_nodes = String::new();
    let mut created_relations: Vec<&OsmRelation> = Vec::new();

    for (_name, diff) in layers {
        created_relations.extend(&diff.created_relations);
        for &(id, version) in &diff.deleted_relation_ids {
            delete_relations.push_str(&delete_relation_xml(id, version, changeset_id));
        }
        for n in &diff.created_nodes {
            create_nodes.push_str(&node_xml(n, changeset_id));
        }
        for w in &diff.created_ways {
            create_ways.push_str(&way_xml(w, changeset_id));
        }
        for n in &diff.modified_nodes {
            modify_nodes.push_str(&node_xml(n, changeset_id));
        }
        for w in &diff.modified_ways {
            modify_ways.push_str(&way_xml(w, changeset_id));
        }
        for r in &diff.modified_relations {
            modify_relations.push_str(&relation_xml(r, changeset_id));
        }
        for &(id, version) in &diff.deleted_way_ids {
            delete_ways.push_str(&delete_way_xml(id, version, changeset_id));
        }
        for &(id, version) in &diff.deleted_node_ids {
            delete_nodes.push_str(&delete_node_xml(id, version, changeset_id));
        }
    }
    let create_relations: String = order_created_relations(created_relations)
        .into_iter()
        .map(|r| relation_xml(r, changeset_id))
        .collect();
    let create_body = format!("{create_nodes}{create_ways}{create_relations}");
    let modify_body = format!("{modify_nodes}{modify_ways}{modify_relations}");
    let delete_body = format!("{delete_relations}{delete_ways}{delete_nodes}");

    let mut out = String::new();
    out.push_str(&format!(
        r#"<osmChange version="0.6" generator="{}">"#,
        xml_escape(crate::CREATED_BY)
    ));
    if !create_body.is_empty() {
        out.push_str("<create>");
        out.push_str(&create_body);
        out.push_str("</create>");
    }
    if !modify_body.is_empty() {
        out.push_str("<modify>");
        out.push_str(&modify_body);
        out.push_str("</modify>");
    }
    if !delete_body.is_empty() {
        out.push_str("<delete>");
        out.push_str(&delete_body);
        out.push_str("</delete>");
    }
    out.push_str("</osmChange>");
    out
}

/// Order created relations so that a relation comes after every created
/// relation it has as a member (Kahn's algorithm, ties and any leftover cycle
/// in id order). Relations in a cycle can't be satisfied; they are appended in
/// id order and the API reports the problem.
fn order_created_relations(mut relations: Vec<&OsmRelation>) -> Vec<&OsmRelation> {
    use crate::osm::MemberType;
    use std::collections::HashSet;

    relations.sort_by_key(|r| r.id);
    let ids: HashSet<i64> = relations.iter().map(|r| r.id).collect();
    let mut placed: HashSet<i64> = HashSet::new();
    let mut ordered: Vec<&OsmRelation> = Vec::with_capacity(relations.len());
    let mut remaining = relations;
    loop {
        let (ready, blocked): (Vec<_>, Vec<_>) = remaining.into_iter().partition(|r| {
            r.members.iter().all(|m| {
                m.member_type != MemberType::Relation
                    || m.reference == r.id
                    || !ids.contains(&m.reference)
                    || placed.contains(&m.reference)
            })
        });
        if ready.is_empty() {
            ordered.extend(blocked);
            return ordered;
        }
        placed.extend(ready.iter().map(|r| r.id));
        ordered.extend(ready);
        if blocked.is_empty() {
            return ordered;
        }
        remaining = blocked;
    }
}

/// Sorted `(k, v)` pairs for deterministic XML output (tags are stored in a
/// `HashMap`, whose iteration order is otherwise unspecified).
fn sorted_tags(tags: &HashMap<String, String>) -> Vec<(&String, &String)> {
    let mut v: Vec<_> = tags.iter().collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v
}

fn node_xml(n: &OsmNode, changeset_id: u64) -> String {
    let mut s = format!(
        r#"<node id="{}" lat="{}" lon="{}" version="{}" changeset="{}">"#,
        n.id, n.lat, n.lon, n.version, changeset_id
    );
    for (k, v) in sorted_tags(&n.tags) {
        s.push_str(&format!(
            r#"<tag k="{}" v="{}"/>"#,
            xml_escape(k),
            xml_escape(v)
        ));
    }
    s.push_str("</node>");
    s
}

fn way_xml(w: &OsmWay, changeset_id: u64) -> String {
    let mut s = format!(
        r#"<way id="{}" version="{}" changeset="{}">"#,
        w.id, w.version, changeset_id
    );
    for nd in &w.nodes {
        s.push_str(&format!(r#"<nd ref="{}"/>"#, nd));
    }
    for (k, v) in sorted_tags(&w.tags) {
        s.push_str(&format!(
            r#"<tag k="{}" v="{}"/>"#,
            xml_escape(k),
            xml_escape(v)
        ));
    }
    s.push_str("</way>");
    s
}

/// A `<relation>` with its members in order (member order is significant)
/// and tags. A member that refers to a not-yet-uploaded element keeps its
/// negative placeholder id, which the API resolves within the changeset.
fn relation_xml(r: &OsmRelation, changeset_id: u64) -> String {
    let mut s = format!(
        r#"<relation id="{}" version="{}" changeset="{}">"#,
        r.id, r.version, changeset_id
    );
    for m in &r.members {
        s.push_str(&format!(
            r#"<member type="{}" ref="{}" role="{}"/>"#,
            m.member_type,
            m.reference,
            xml_escape(&m.role)
        ));
    }
    for (k, v) in sorted_tags(&r.tags) {
        s.push_str(&format!(
            r#"<tag k="{}" v="{}"/>"#,
            xml_escape(k),
            xml_escape(v)
        ));
    }
    s.push_str("</relation>");
    s
}

fn delete_node_xml(id: i64, version: i32, changeset_id: u64) -> String {
    format!(
        r#"<node id="{}" version="{}" changeset="{}"/>"#,
        id, version, changeset_id
    )
}

fn delete_relation_xml(id: i64, version: i32, changeset_id: u64) -> String {
    format!(
        r#"<relation id="{}" version="{}" changeset="{}"/>"#,
        id, version, changeset_id
    )
}

fn delete_way_xml(id: i64, version: i32, changeset_id: u64) -> String {
    format!(
        r#"<way id="{}" version="{}" changeset="{}"/>"#,
        id, version, changeset_id
    )
}

/// Parse a `<diffResult>` response from `.../changeset/{id}/upload`. Each
/// `<node>`/`<way>`/`<relation>` child carries `old_id` always, and `new_id`/
/// `new_version` when the element was created or modified (both present
/// together); a deleted element is confirmed with just `old_id` and no
/// `new_id`/`new_version`, which we skip since there's nothing to remap.
fn parse_diff_result(xml: &str) -> Result<UploadResult, UploadError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut result = UploadResult::default();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_node = e.name().as_ref() == b"node";
                let is_way = e.name().as_ref() == b"way";
                let is_relation = e.name().as_ref() == b"relation";
                if !is_node && !is_way && !is_relation {
                    buf.clear();
                    continue;
                }

                let kind = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let unreadable = |what: &str| {
                    UploadError::Parse(format!(
                        "the server's diffResult has an unreadable {what} on a <{kind}>"
                    ))
                };
                let mut old_id: Option<&str> = None;
                let mut new_id: Option<&str> = None;
                let mut new_version: Option<&str> = None;
                let attrs: Vec<_> = e
                    .attributes()
                    .collect::<Result<_, _>>()
                    .map_err(|e| UploadError::Parse(e.to_string()))?;
                for attr in &attrs {
                    let key = std::str::from_utf8(attr.key.as_ref())
                        .map_err(|e| UploadError::Parse(e.to_string()))?;
                    let value = std::str::from_utf8(&attr.value)
                        .map_err(|e| UploadError::Parse(e.to_string()))?;
                    match key {
                        "old_id" => old_id = Some(value),
                        "new_id" => new_id = Some(value),
                        "new_version" => new_version = Some(value),
                        _ => {}
                    }
                }

                // The server accepted the upload, so an element we cannot map back
                // would leave a local object without its server id and version, and
                // the next upload would create it a second time: refuse to guess.
                let old_id: i64 = old_id
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| unreadable("old_id"))?;
                let mapped = match (new_id, new_version) {
                    (None, None) if old_id > 0 => None,
                    (None, None) => return Err(unreadable("missing new_id")),
                    (new_id, new_version) => {
                        let new_id: i64 = new_id
                            .and_then(|v| v.parse().ok())
                            .ok_or_else(|| unreadable("new_id"))?;
                        let new_version: i32 = new_version
                            .and_then(|v| v.parse().ok())
                            .ok_or_else(|| unreadable("new_version"))?;
                        Some((new_id, new_version))
                    }
                };

                if let Some((new_id, new_version)) = mapped {
                    let remap = if is_node {
                        &mut result.node_id_remap
                    } else if is_way {
                        &mut result.way_id_remap
                    } else {
                        &mut result.relation_id_remap
                    };
                    remap.insert(old_id, (new_id, new_version));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(UploadError::Parse(e.to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn http_failures_map_to_upload_errors() {
        for status in [409, 410, 412] {
            assert!(
                matches!(
                    UploadError::from(HttpError::Status {
                        status,
                        body: b"conflict".to_vec()
                    }),
                    UploadError::Conflict(_)
                ),
                "{status}"
            );
        }
        assert!(matches!(
            UploadError::from(HttpError::Status {
                status: 500,
                body: b"oops".to_vec()
            }),
            UploadError::Http { status: 500, body } if body == "oops"
        ));
        assert!(matches!(
            UploadError::from(HttpError::Transport("reset".into())),
            UploadError::Network(m) if m == "reset"
        ));
        assert!(matches!(
            UploadError::from(HttpError::Cancelled),
            UploadError::Network(m) if m == "cancelled"
        ));
    }

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn changeset_create_xml_has_created_by_and_comment() {
        let xml = build_changeset_create_xml(&ChangesetTags::from("fix <stuff>"));
        assert!(xml.contains(&format!(
            r#"<tag k="created_by" v="osm-gpui {}"/>"#,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(xml.contains(r#"<tag k="comment" v="fix &lt;stuff&gt;"/>"#));
        // Nothing else is written for a bare comment.
        assert_eq!(xml.matches("<tag ").count(), 2);
    }

    #[test]
    fn changeset_create_xml_writes_every_tag_in_order_and_escapes_values() {
        use crate::changeset_tags::{build_changeset_tags, TagInputs};
        let imagery = vec!["Esri & Co".to_string()];
        let tags = build_changeset_tags(&TagInputs {
            comment: "Fix #a",
            source: "survey <x>",
            review_requested: true,
            imagery: &imagery,
            locale: "de",
        });
        let xml = build_changeset_create_xml(&tags);
        let positions: Vec<usize> = [
            r#"k="created_by""#,
            r#"<tag k="comment" v="Fix #a"/>"#,
            r#"<tag k="source" v="survey &lt;x&gt;"/>"#,
            r#"<tag k="imagery_used" v="Esri &amp; Co"/>"#,
            r#"<tag k="locale" v="de"/>"#,
            r##"<tag k="hashtags" v="#a"/>"##,
            r#"<tag k="review_requested" v="yes"/>"#,
        ]
        .iter()
        .map(|needle| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {xml}"))
        })
        .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{xml}");
    }

    #[test]
    fn empty_diff_produces_no_sections() {
        let diff = LayerDiff::default();
        let xml = build_osm_change_xml(1, &[("L", diff)]);
        assert_eq!(
            xml,
            format!(
                r#"<osmChange version="0.6" generator="osm-gpui {}"></osmChange>"#,
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(!xml.contains("<create>"));
        assert!(!xml.contains("<modify>"));
        assert!(!xml.contains("<delete>"));
    }

    #[test]
    fn create_node_and_way_xml_shape() {
        let mut diff = LayerDiff::default();
        diff.created_nodes.push(OsmNode {
            id: -1,
            lat: 40.5,
            lon: -74.5,
            version: 0,
            tags: tags(&[("amenity", "cafe")]),
        });
        diff.created_ways.push(OsmWay {
            id: -2,
            nodes: vec![-1, 5],
            version: 0,
            tags: tags(&[("highway", "residential")]),
        });

        let xml = build_osm_change_xml(42, &[("L", diff)]);
        assert!(xml.contains("<create>"), "{}", xml);
        assert!(
            xml.contains(r#"<node id="-1" lat="40.5" lon="-74.5" version="0" changeset="42">"#),
            "{}",
            xml
        );
        assert!(xml.contains(r#"<tag k="amenity" v="cafe"/>"#), "{}", xml);
        assert!(
            xml.contains(r#"<way id="-2" version="0" changeset="42">"#),
            "{}",
            xml
        );
        assert!(xml.contains(r#"<nd ref="-1"/>"#), "{}", xml);
        assert!(xml.contains(r#"<nd ref="5"/>"#), "{}", xml);
        assert!(
            xml.contains(r#"<tag k="highway" v="residential"/>"#),
            "{}",
            xml
        );
        assert!(!xml.contains("<modify>"));
        assert!(!xml.contains("<delete>"));
    }

    #[test]
    fn modify_node_xml_shape() {
        let mut diff = LayerDiff::default();
        diff.modified_nodes.push(OsmNode {
            id: 100,
            lat: 1.0,
            lon: 2.0,
            version: 3,
            tags: HashMap::new(),
        });
        let xml = build_osm_change_xml(7, &[("L", diff)]);
        assert!(xml.contains("<modify>"), "{}", xml);
        assert!(
            xml.contains(r#"<node id="100" lat="1" lon="2" version="3" changeset="7">"#),
            "{}",
            xml
        );
        assert!(!xml.contains("<create>"));
        assert!(!xml.contains("<delete>"));
    }

    #[test]
    fn delete_node_and_way_xml_shape() {
        let mut diff = LayerDiff::default();
        diff.deleted_node_ids.push((55, 4));
        diff.deleted_way_ids.push((66, 2));
        let xml = build_osm_change_xml(9, &[("L", diff)]);
        assert!(xml.contains("<delete>"), "{}", xml);
        assert!(
            xml.contains(r#"<node id="55" version="4" changeset="9"/>"#),
            "{}",
            xml
        );
        assert!(
            xml.contains(r#"<way id="66" version="2" changeset="9"/>"#),
            "{}",
            xml
        );
        assert!(!xml.contains("<create>"));
        assert!(!xml.contains("<modify>"));
    }

    #[test]
    fn modify_relation_xml_shape_and_order() {
        use crate::osm::{MemberType, OsmMember};
        let mut diff = LayerDiff::default();
        diff.modified_relations.push(OsmRelation {
            id: 500,
            version: 7,
            tags: tags(&[("name", "A & B"), ("type", "route")]),
            members: vec![
                OsmMember {
                    member_type: MemberType::Way,
                    reference: -3,
                    role: "".into(),
                },
                OsmMember {
                    member_type: MemberType::Node,
                    reference: 12,
                    role: "stop \"1\"".into(),
                },
                OsmMember {
                    member_type: MemberType::Way,
                    reference: 40,
                    role: "forward".into(),
                },
            ],
        });
        diff.created_ways.push(OsmWay {
            id: -3,
            nodes: vec![12, 13],
            version: 0,
            tags: HashMap::new(),
        });
        diff.modified_ways.push(OsmWay {
            id: 40,
            nodes: vec![1, 2],
            version: 2,
            tags: HashMap::new(),
        });
        diff.modified_nodes.push(OsmNode {
            id: 12,
            lat: 0.0,
            lon: 0.0,
            version: 1,
            tags: HashMap::new(),
        });
        // Deleted: a way and a node it used, plus nothing else.
        diff.deleted_way_ids.push((77, 1));
        diff.deleted_node_ids.push((78, 1));

        let xml = build_osm_change_xml(9, &[("L", diff)]);

        let expected_relation = concat!(
            r#"<relation id="500" version="7" changeset="9">"#,
            r#"<member type="way" ref="-3" role=""/>"#,
            r#"<member type="node" ref="12" role="stop &quot;1&quot;"/>"#,
            r#"<member type="way" ref="40" role="forward"/>"#,
            r#"<tag k="name" v="A &amp; B"/>"#,
            r#"<tag k="type" v="route"/>"#,
            r#"</relation>"#,
        );
        assert!(xml.contains(expected_relation), "{xml}");

        let at = |needle: &str| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {xml}"))
        };
        // Created way before any modify; modifies go nodes, ways, relations
        // (inside the one <modify> section); deletes go ways before nodes.
        assert!(at("<create>") < at(r#"<way id="-3""#));
        assert!(at(r#"<way id="-3""#) < at("<modify>"));
        assert!(at("<modify>") < at(r#"<node id="12""#));
        assert!(at(r#"<node id="12""#) < at(r#"<way id="40""#));
        assert!(at(r#"<way id="40""#) < at(r#"<relation id="500""#));
        assert!(at("</modify>") < at("<delete>"));
        assert!(at(r#"<way id="77""#) < at(r#"<node id="78""#));
        assert_eq!(xml.matches("<modify>").count(), 1, "{xml}");
    }

    fn rel(id: i64, members: &[(crate::osm::MemberType, i64)]) -> OsmRelation {
        OsmRelation {
            id,
            version: 0,
            tags: tags(&[("type", "route")]),
            members: members
                .iter()
                .map(|&(member_type, reference)| crate::osm::OsmMember {
                    member_type,
                    reference,
                    role: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn created_relations_follow_their_members_and_deletes_start_with_relations() {
        use crate::osm::MemberType::{Node, Relation, Way};
        let mut diff = LayerDiff::default();
        // -10 contains created way -2 and created relation -11; -11 contains
        // created node -1. Listed with the container first on purpose.
        diff.created_relations
            .push(rel(-10, &[(Way, -2), (Relation, -11), (Way, 40)]));
        diff.created_relations.push(rel(-11, &[(Node, -1)]));
        diff.created_ways.push(OsmWay {
            id: -2,
            nodes: vec![-1, 5],
            version: 0,
            tags: HashMap::new(),
        });
        diff.created_nodes.push(OsmNode {
            id: -1,
            lat: 1.0,
            lon: 2.0,
            version: 0,
            tags: HashMap::new(),
        });
        diff.modified_relations.push(rel(500, &[(Way, 40)]));
        diff.deleted_relation_ids.push((600, 3));
        diff.deleted_way_ids.push((77, 1));
        diff.deleted_node_ids.push((78, 1));

        let xml = build_osm_change_xml(9, &[("L", diff)]);
        let at = |needle: &str| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {xml}"))
        };
        assert!(at(r#"<node id="-1""#) < at(r#"<way id="-2""#));
        assert!(at(r#"<way id="-2""#) < at(r#"<relation id="-11""#));
        assert!(at(r#"<relation id="-11""#) < at(r#"<relation id="-10""#));
        assert!(at(r#"<relation id="-10""#) < at("</create>"));
        assert!(
            xml.contains(concat!(
                r#"<relation id="-10" version="0" changeset="9">"#,
                r#"<member type="way" ref="-2" role=""/>"#,
                r#"<member type="relation" ref="-11" role=""/>"#,
                r#"<member type="way" ref="40" role=""/>"#,
                r#"<tag k="type" v="route"/></relation>"#,
            )),
            "{xml}"
        );
        assert!(at("</create>") < at("<modify>"));
        assert!(at("</modify>") < at("<delete>"));
        assert!(at(r#"<relation id="600" version="3" changeset="9"/>"#) > at("<delete>"));
        assert!(at(r#"<relation id="600""#) < at(r#"<way id="77""#));
        assert!(at(r#"<way id="77""#) < at(r#"<node id="78""#));
    }

    #[test]
    fn created_relation_cycle_falls_back_to_id_order() {
        use crate::osm::MemberType::Relation;
        let a = rel(-20, &[(Relation, -21)]);
        let b = rel(-21, &[(Relation, -20)]);
        let c = rel(-22, &[]);
        let ordered: Vec<i64> = order_created_relations(vec![&a, &b, &c])
            .iter()
            .map(|r| r.id)
            .collect();
        // The independent relation goes first; the cycle follows in id order.
        assert_eq!(ordered, vec![-22, -21, -20]);
    }

    #[test]
    fn parse_diff_result_maps_created_relations() {
        let xml = r#"<diffResult version="0.6">
  <relation old_id="-10" new_id="77" new_version="1"/>
  <relation old_id="-11" new_id="76" new_version="1"/>
  <relation old_id="600"/>
</diffResult>"#;
        let result = parse_diff_result(xml).unwrap();
        assert_eq!(result.relation_id_remap.get(&-10), Some(&(77, 1)));
        assert_eq!(result.relation_id_remap.get(&-11), Some(&(76, 1)));
        assert_eq!(result.relation_id_remap.get(&600), None);
    }

    #[test]
    fn tag_values_are_xml_escaped() {
        let mut diff = LayerDiff::default();
        diff.modified_nodes.push(OsmNode {
            id: 1,
            lat: 0.0,
            lon: 0.0,
            version: 1,
            tags: tags(&[("name", "Fish & Chips <\"Best\" in 'Town'>")]),
        });
        let xml = build_osm_change_xml(1, &[("L", diff)]);
        assert!(
            xml.contains("Fish &amp; Chips &lt;&quot;Best&quot; in &apos;Town&apos;&gt;"),
            "{}",
            xml
        );
        assert!(
            !xml.contains(" & "),
            "raw ampersand leaked into XML: {}",
            xml
        );
    }

    #[test]
    fn changeset_comment_is_escaped() {
        let xml = build_changeset_create_xml(&ChangesetTags::from("Fix <road> & \"stuff\""));
        assert!(
            xml.contains("Fix &lt;road&gt; &amp; &quot;stuff&quot;"),
            "{}",
            xml
        );
    }

    #[test]
    fn multiple_layers_combine_into_one_change_document() {
        let mut diff_a = LayerDiff::default();
        diff_a.created_nodes.push(OsmNode {
            id: -1,
            lat: 0.0,
            lon: 0.0,
            version: 0,
            tags: HashMap::new(),
        });
        let mut diff_b = LayerDiff::default();
        diff_b.deleted_node_ids.push((3, 1));

        let xml = build_osm_change_xml(1, &[("A", diff_a), ("B", diff_b)]);
        assert!(xml.contains(r#"<node id="-1""#), "{}", xml);
        assert!(
            xml.contains(r#"<node id="3" version="1" changeset="1"/>"#),
            "{}",
            xml
        );
    }

    #[test]
    fn parse_diff_result_maps_created_modified_and_skips_deleted() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<diffResult generator="OpenStreetMap server" version="0.6">
  <node old_id="-1" new_id="12345" new_version="1"/>
  <node old_id="45" new_id="45" new_version="3"/>
  <node old_id="99"/>
  <way old_id="-2" new_id="678" new_version="1"/>
  <way old_id="10" version="2"/>
  <relation old_id="500" new_id="500" new_version="8"/>
  <relation old_id="501"/>
</diffResult>"#;

        let result = parse_diff_result(xml).expect("should parse");
        assert_eq!(result.node_id_remap.get(&-1), Some(&(12345, 1)));
        assert_eq!(result.node_id_remap.get(&45), Some(&(45, 3)));
        assert_eq!(
            result.node_id_remap.get(&99),
            None,
            "deleted node has no remap entry"
        );
        assert_eq!(result.way_id_remap.get(&-2), Some(&(678, 1)));
        assert_eq!(
            result.way_id_remap.get(&10),
            None,
            "deleted way has no remap entry"
        );
        assert_eq!(result.relation_id_remap.get(&500), Some(&(500, 8)));
        assert_eq!(result.relation_id_remap.get(&501), None);
    }

    #[test]
    fn parse_diff_result_empty_document_returns_empty_result() {
        let xml = r#"<diffResult generator="x" version="0.6"></diffResult>"#;
        let result = parse_diff_result(xml).expect("should parse");
        assert!(result.node_id_remap.is_empty());
        assert!(result.way_id_remap.is_empty());
    }

    #[test]
    fn parse_diff_result_invalid_xml_is_err() {
        let xml = "<diffResult><node old_id=\"1\"";
        assert!(parse_diff_result(xml).is_err());
    }

    #[test]
    fn parse_diff_result_rejects_a_non_numeric_id() {
        for xml in [
            r#"<diffResult><node old_id="-1" new_id="abc" new_version="1"/></diffResult>"#,
            r#"<diffResult><way old_id="x" new_id="5" new_version="1"/></diffResult>"#,
            r#"<diffResult><node old_id="-1" new_id="5" new_version="one"/></diffResult>"#,
        ] {
            assert!(
                matches!(parse_diff_result(xml), Err(UploadError::Parse(_))),
                "{xml}"
            );
        }
    }

    #[test]
    fn parse_diff_result_rejects_a_create_without_new_id() {
        let xml = r#"<diffResult><node old_id="-3"/></diffResult>"#;
        assert!(matches!(parse_diff_result(xml), Err(UploadError::Parse(_))));
        let xml = r#"<diffResult><way old_id="-3" new_version="1"/></diffResult>"#;
        assert!(matches!(parse_diff_result(xml), Err(UploadError::Parse(_))));
    }

    #[test]
    fn parse_diff_result_accepts_a_well_formed_result() {
        let xml = r#"<diffResult><node old_id="-1" new_id="9" new_version="1"/><node old_id="4"/></diffResult>"#;
        let result = parse_diff_result(xml).unwrap();
        assert_eq!(result.node_id_remap.get(&-1), Some(&(9, 1)));
        assert_eq!(result.node_id_remap.len(), 1);
    }

    #[test]
    fn an_unreadable_diff_result_after_a_successful_upload_is_outcome_unknown() {
        let body = r#"<diffResult><node old_id="-1" new_id="nine" new_version="1"/></diffResult>"#;
        let client = FakeClient::new(vec![ok(200, "42"), ok(200, body), ok(200, "")]);
        let failure = run_upload(
            &client,
            "https://api.openstreetmap.org",
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.changeset_id, Some(42));
        assert!(matches!(
            failure.error,
            UploadError::OutcomeUnknown {
                changeset_id: 42,
                ..
            }
        ));
        assert!(failure
            .error
            .to_string()
            .contains("https://www.openstreetmap.org/changeset/42"));
    }

    fn parsed(status: u16, body: &str) -> UploadConflict {
        UploadConflict::parse(status, body)
    }

    #[test]
    fn version_mismatch_names_the_object_and_both_versions() {
        use FeatureKind::*;
        for (body, object) in [
            (
                "Version mismatch: Provided 3, server had: 4 of Node 12345",
                (Node, 12345),
            ),
            (
                "Version mismatch: Provided 7, server had: 9 of Way 678",
                (Way, 678),
            ),
            (
                "Version mismatch: Provided 1, server had: 2 of Relation 55\n",
                (Relation, 55),
            ),
            // Wording drift: case, punctuation, "Precondition failed" prefix.
            (
                "version mismatch: provided 3; server had 4 of node 12345.",
                (Node, 12345),
            ),
        ] {
            let c = parsed(409, body);
            assert_eq!(c.object, Some(object), "{body}");
            assert!(
                matches!(c.kind, ConflictKind::VersionMismatch { .. }),
                "{body}: {:?}",
                c.kind
            );
        }
        let c = parsed(
            409,
            "Version mismatch: Provided 3, server had: 4 of Node 12345",
        );
        assert_eq!(
            c.kind,
            ConflictKind::VersionMismatch {
                provided: 3,
                server: 4
            }
        );
        assert_eq!(c.status, 409);
        assert!(c.message.starts_with("Version mismatch"));
        assert_eq!(
            c.describe(),
            "Node 12345 was changed on the server since you downloaded it (your version 3, server version 4)"
        );
    }

    #[test]
    fn already_deleted_is_recognised_by_text_and_by_status() {
        let c = parsed(410, "The node with the id 12345 has already been deleted");
        assert_eq!(c.kind, ConflictKind::AlreadyDeleted);
        assert_eq!(c.object, Some((FeatureKind::Node, 12345)));
        assert_eq!(
            c.describe(),
            "Node 12345 has already been deleted on the server"
        );

        let c = parsed(409, "The way with the id 8 has already been deleted");
        assert_eq!(c.kind, ConflictKind::AlreadyDeleted);
        assert_eq!(c.object, Some((FeatureKind::Way, 8)));

        // A bare 410 without a usable message is still "deleted".
        let c = parsed(410, "");
        assert_eq!(c.kind, ConflictKind::AlreadyDeleted);
        assert_eq!(c.object, None);
    }

    #[test]
    fn still_in_use_lists_what_uses_the_object() {
        use FeatureKind::*;
        let c = parsed(
            412,
            "Precondition failed: Node 12345 is still used by ways 1,2.",
        );
        assert_eq!(
            c.kind,
            ConflictKind::StillInUse {
                by: vec![(Way, 1), (Way, 2)]
            }
        );
        assert_eq!(c.object, Some((Node, 12345)));
        assert_eq!(
            c.describe(),
            "Node 12345 can't be deleted because it is still used by Way 1, Way 2"
        );

        let c = parsed(412, "Node 9 is still used by way 7.");
        assert_eq!(c.kind, ConflictKind::StillInUse { by: vec![(Way, 7)] });

        let c = parsed(412, "Way 5 still used by relation 3.");
        assert_eq!(c.object, Some((Way, 5)));
        assert_eq!(
            c.kind,
            ConflictKind::StillInUse {
                by: vec![(Relation, 3)]
            }
        );

        let c = parsed(412, "The relation 7 is used in relation 8.");
        assert_eq!(c.object, Some((Relation, 7)));
        assert_eq!(
            c.kind,
            ConflictKind::StillInUse {
                by: vec![(Relation, 8)]
            }
        );

        let c = parsed(412, "Node 4 is still used by relations 10,11");
        assert_eq!(
            c.kind,
            ConflictKind::StillInUse {
                by: vec![(Relation, 10), (Relation, 11)]
            }
        );
    }

    #[test]
    fn missing_dependency_names_the_referring_object() {
        let c = parsed(
            412,
            "Way 5 requires the nodes with id in (11,12), which either do not exist, or are not visible.",
        );
        assert_eq!(c.kind, ConflictKind::MissingDependency);
        assert_eq!(c.object, Some((FeatureKind::Way, 5)));

        let c = parsed(
            412,
            "Precondition failed: Relation 7 requires the nodes with id in (3), which either do not exist, or are not visible.",
        );
        assert_eq!(c.kind, ConflictKind::MissingDependency);
        assert_eq!(c.object, Some((FeatureKind::Relation, 7)));

        let c = parsed(
            412,
            "Relation with id 7 cannot be saved due to Node with id 3",
        );
        assert_eq!(c.kind, ConflictKind::MissingDependency);
        assert_eq!(c.object, Some((FeatureKind::Relation, 7)));
    }

    #[test]
    fn unrecognised_refusals_are_other_with_the_message_kept() {
        let c = parsed(
            409,
            "The changeset 42 was closed at 2026-01-01 10:00:00 UTC",
        );
        assert_eq!(c.kind, ConflictKind::Other);
        assert_eq!(c.object, None);
        assert_eq!(
            c.describe(),
            "The server refused the upload (409): The changeset 42 was closed at 2026-01-01 10:00:00 UTC"
        );

        let c = parsed(412, "something nobody has seen before");
        assert_eq!(c.kind, ConflictKind::Other);

        // A version mismatch with the numbers mangled is not guessed at.
        let c = parsed(409, "Version mismatch of Node 3");
        assert_eq!(c.kind, ConflictKind::Other);
        assert_eq!(c.object, Some((FeatureKind::Node, 3)));
    }

    use crate::http::fake::{ok, status_err, FakeClient};
    use crate::http::{HttpError, HttpResponse};

    const BASE: &str = "https://osm.test";

    fn one_modified_node_diff() -> Vec<(&'static str, LayerDiff)> {
        let mut diff = LayerDiff::default();
        diff.modified_nodes.push(OsmNode {
            id: 12345,
            lat: 1.0,
            lon: 2.0,
            version: 3,
            tags: HashMap::new(),
        });
        vec![("L", diff)]
    }

    fn request_lines(client: &FakeClient) -> Vec<String> {
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| format!("{:?} {}", r.method, r.url))
            .collect()
    }

    #[test]
    fn the_changeset_is_closed_when_the_server_refuses_the_diff() {
        let client = FakeClient::new(vec![
            ok(200, "42"),
            status_err(
                409,
                "Version mismatch: Provided 3, server had: 4 of Node 12345",
            ),
            ok(200, ""),
        ]);
        let mut phases = Vec::new();
        let failure = run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |p| phases.push(p),
        )
        .unwrap_err();

        assert_eq!(failure.changeset_id, Some(42));
        match failure.error {
            UploadError::Conflict(c) => assert_eq!(
                c.kind,
                ConflictKind::VersionMismatch {
                    provided: 3,
                    server: 4
                }
            ),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(
            request_lines(&client),
            vec![
                "Put https://osm.test/api/0.6/changeset/create",
                "Post https://osm.test/api/0.6/changeset/42/upload",
                "Put https://osm.test/api/0.6/changeset/42/close",
            ]
        );
        assert_eq!(phases, vec![TransferPhase::UploadingChanges { count: 1 }]);
    }

    #[test]
    fn the_changeset_is_closed_for_any_failure_and_the_upload_error_wins() {
        // A non-conflict HTTP error, and a close that itself fails.
        let client = FakeClient::new(vec![
            ok(200, "7"),
            status_err(400, "bad xml"),
            status_err(409, "The changeset 7 was closed at now"),
        ]);
        let failure = run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(
            failure.error,
            UploadError::Http { status: 400, .. }
        ));
        assert_eq!(client.request_count(), 3);
        assert!(request_lines(&client)[2].ends_with("/changeset/7/close"));
    }

    fn assert_outcome_unknown_single_upload(failing: Result<HttpResponse, HttpError>) {
        let client = FakeClient::new(vec![ok(200, "42"), failing, ok(200, "")]);
        let failure = run_upload(
            &client,
            "https://api.openstreetmap.org",
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.changeset_id, Some(42));
        match &failure.error {
            UploadError::OutcomeUnknown {
                changeset_id, url, ..
            } => {
                assert_eq!(*changeset_id, 42);
                assert_eq!(url, "https://www.openstreetmap.org/changeset/42");
            }
            other => panic!("expected OutcomeUnknown, got {other:?}"),
        }
        assert!(failure
            .error
            .to_string()
            .contains("https://www.openstreetmap.org/changeset/42"));
        assert_eq!(
            request_lines(&client),
            vec![
                "Put https://api.openstreetmap.org/api/0.6/changeset/create",
                "Post https://api.openstreetmap.org/api/0.6/changeset/42/upload",
                "Put https://api.openstreetmap.org/api/0.6/changeset/42/close",
            ]
        );
    }

    #[test]
    fn a_transport_error_on_the_diff_upload_is_sent_once_and_outcome_unknown() {
        assert_outcome_unknown_single_upload(crate::http::fake::transport_err("connection reset"));
    }

    #[test]
    fn a_503_on_the_diff_upload_is_sent_once_and_outcome_unknown() {
        assert_outcome_unknown_single_upload(status_err(503, "unavailable"));
    }

    #[test]
    fn a_transport_error_creating_the_changeset_is_not_retried() {
        let client = FakeClient::new(vec![
            crate::http::fake::transport_err("reset"),
            ok(200, "42"),
        ]);
        let failure = run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(failure.error, UploadError::Network(_)));
        assert_eq!(failure.changeset_id, None);
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn close_is_still_retried_on_503() {
        let client = FakeClient::new(vec![
            ok(200, "42"),
            ok(
                200,
                r#"<diffResult><node old_id="12345" new_id="12345" new_version="4"/></diffResult>"#,
            ),
            status_err(503, "busy"),
            ok(200, ""),
        ]);
        run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(client.request_count(), 4);
        assert!(request_lines(&client)[3].ends_with("/changeset/42/close"));
    }

    #[test]
    fn nothing_is_closed_when_no_changeset_was_opened() {
        let client = FakeClient::new(vec![status_err(401, "no")]);
        let failure = run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.changeset_id, None);
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn a_successful_upload_sends_credentials_and_closes_once() {
        let client = FakeClient::new(vec![
            ok(200, "42"),
            ok(
                200,
                r#"<diffResult><node old_id="12345" new_id="12345" new_version="4"/></diffResult>"#,
            ),
            ok(200, ""),
        ]);
        let mut phases = Vec::new();
        let (id, result) = run_upload(
            &client,
            BASE,
            "tok",
            "fix",
            &one_modified_node_diff(),
            &mut |p| phases.push(p),
        )
        .unwrap();
        assert_eq!(id, 42);
        assert_eq!(result.node_id_remap.get(&12345), Some(&(12345, 4)));
        assert_eq!(client.request_count(), 3);
        assert_eq!(
            phases,
            vec![
                TransferPhase::UploadingChanges { count: 1 },
                TransferPhase::ClosingChangeset
            ]
        );
        let requests = client.requests.lock().unwrap();
        for r in requests.iter() {
            assert!(r
                .headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer tok"));
        }
        let sent = match &requests[1].body {
            Some(crate::http::Body::Text(xml)) => xml.clone(),
            other => panic!("expected an xml body, got {other:?}"),
        };
        assert!(
            sent.contains(r#"<node id="12345" lat="1" lon="2" version="3" changeset="42">"#),
            "{sent}"
        );
    }
}
