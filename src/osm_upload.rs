//! OSM API 0.6 changeset lifecycle (create/upload/close) and osmChange XML
//! (de)serialization. See https://wiki.openstreetmap.org/wiki/API_v0.6#Changesets.
//!
//! Every network call here is synchronous (`ureq`) and must be run from a
//! background executor, never the UI thread — mirrors `osm_api::fetch_bbox`.

use std::collections::HashMap;
use std::time::Duration;

use crate::layers::diff::LayerDiff;
use crate::osm::{OsmNode, OsmRelation, OsmWay};
use crate::tr;

#[derive(Debug)]
pub enum UploadError {
    /// Transport-level failure (no HTTP response received at all).
    Network(String),
    /// A non-2xx HTTP response not covered by a more specific variant below.
    Http { status: u16, body: String },
    /// 409 Conflict: the version we sent for some node/way/relation doesn't
    /// match what the server has — someone else edited it first. Must NOT be
    /// retried automatically; the user needs to re-download and reconcile.
    Conflict { body: String },
    /// 412 Precondition Failed: e.g. a way/relation in our changeset
    /// references a node/way that's already been deleted server-side. Same
    /// "don't retry, reconcile manually" handling as `Conflict`.
    PreconditionFailed { body: String },
    /// The response body wasn't parseable XML/text in the shape we expected.
    Parse(String),
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
            UploadError::Conflict { body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(f, "{}", tr!("error.upload.conflict", detail = first_line))
            }
            UploadError::PreconditionFailed { body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "{}",
                    tr!("error.upload.precondition_failed", detail = first_line)
                )
            }
            UploadError::Parse(msg) => write!(f, "{}", tr!("error.api.parse", detail = msg)),
        }
    }
}

impl std::error::Error for UploadError {}

/// Per-changeset result of a successful `upload_changes` call: for every
/// node/way the server accepted, maps its LOCAL id (the id we sent — a
/// possibly-negative placeholder for a create, or the real id for a
/// modify) to its new `(id, version)`. For a modify, `new_id == old local
/// id`. For a create, `new_id` is the real server-assigned id. Deleted
/// elements are confirmed by the server but need no remap, so they're not
/// represented here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadResult {
    pub node_id_remap: HashMap<i64, (i64, i32)>,
    pub way_id_remap: HashMap<i64, (i64, i32)>,
    /// Same for relations: a modify maps an id to itself with its new
    /// version, a create maps the placeholder to the server-assigned id.
    pub relation_id_remap: HashMap<i64, (i64, i32)>,
}

/// Max attempts for a single create/upload/close request, including the
/// first try.
const MAX_ATTEMPTS: u32 = 3;
const RETRY_DELAYS: [Duration; MAX_ATTEMPTS as usize - 1] =
    [Duration::from_millis(200), Duration::from_millis(500)];

enum HttpMethod {
    Put,
    Post,
}

/// Send a single changeset-lifecycle request with a small bounded retry.
///
/// Retry reasoning (applies uniformly to create/upload/close): we only ever
/// retry a) a transport-level failure, where by construction no HTTP
/// response was received at all, so the server either never saw the request
/// or never finished handling it — retrying can at worst produce a harmless
/// duplicate (an extra open changeset, a double-close no-op), never data
/// corruption; and b) a retryable HTTP status (429/509/5xx via
/// `crate::is_retryable_status`), which for `POST .../upload` specifically
/// means the server itself reported failure (it did not claim success), so
/// the edit was not applied. We deliberately do NOT retry 409/412 — those
/// have a real, meaningful response indicating one or more elements in the
/// changeset are already out of sync with the server; retrying the exact
/// same payload would just fail again forever, and the right recovery is a
/// manual re-download/reconcile, not an automatic retry.
fn call_with_retries(
    method: HttpMethod,
    url: &str,
    token: &str,
    body: Option<String>,
) -> Result<String, UploadError> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;

        let request = match method {
            HttpMethod::Put => ureq::put(url),
            HttpMethod::Post => ureq::post(url),
        }
        .set("User-Agent", crate::USER_AGENT)
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "text/xml; charset=utf-8")
        .timeout(Duration::from_secs(30));

        let result = match &body {
            Some(b) => request.send_string(b),
            None => request.call(),
        };

        match result {
            Ok(resp) => {
                return resp
                    .into_string()
                    .map_err(|e| UploadError::Network(e.to_string()));
            }
            Err(ureq::Error::Status(status, resp)) => {
                let body_text = resp.into_string().unwrap_or_default();
                if status == 409 {
                    return Err(UploadError::Conflict { body: body_text });
                }
                if status == 412 {
                    return Err(UploadError::PreconditionFailed { body: body_text });
                }
                if attempt < MAX_ATTEMPTS && crate::is_retryable_status(status) {
                    std::thread::sleep(RETRY_DELAYS[(attempt - 1) as usize]);
                    continue;
                }
                return Err(UploadError::Http {
                    status,
                    body: body_text,
                });
            }
            Err(e) => {
                if attempt < MAX_ATTEMPTS {
                    std::thread::sleep(RETRY_DELAYS[(attempt - 1) as usize]);
                    continue;
                }
                return Err(UploadError::Network(e.to_string()));
            }
        }
    }
}

/// `PUT {base_url}/api/0.6/changeset/create`. Returns the new changeset id.
pub fn create_changeset(base_url: &str, token: &str, comment: &str) -> Result<u64, UploadError> {
    let url = format!(
        "{}/api/0.6/changeset/create",
        base_url.trim_end_matches('/')
    );
    let xml = build_changeset_create_xml(comment);
    let body = call_with_retries(HttpMethod::Put, &url, token, Some(xml))?;
    body.trim()
        .parse::<u64>()
        .map_err(|_| UploadError::Parse(format!("invalid changeset id in response: {:?}", body)))
}

fn build_changeset_create_xml(comment: &str) -> String {
    format!(
        r#"<osm><changeset><tag k="created_by" v="{}"/><tag k="comment" v="{}"/></changeset></osm>"#,
        xml_escape(crate::CREATED_BY),
        xml_escape(comment),
    )
}

/// `POST {base_url}/api/0.6/changeset/{id}/upload` with `xml` as the body.
pub fn upload_changes(
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
    let body = call_with_retries(HttpMethod::Post, &url, token, Some(xml.to_string()))?;
    parse_diff_result(&body)
}

/// `PUT {base_url}/api/0.6/changeset/{id}/close`.
pub fn close_changeset(base_url: &str, token: &str, changeset_id: u64) -> Result<(), UploadError> {
    let url = format!(
        "{}/api/0.6/changeset/{}/close",
        base_url.trim_end_matches('/'),
        changeset_id
    );
    call_with_retries(HttpMethod::Put, &url, token, None)?;
    Ok(())
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
/// `<node>`/`<way>` child carries `old_id` always, and `new_id`/
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

                let mut old_id: Option<i64> = None;
                let mut new_id: Option<i64> = None;
                let mut new_version: Option<i32> = None;
                for attr in e.attributes() {
                    let attr = attr.map_err(|e| UploadError::Parse(e.to_string()))?;
                    let key = std::str::from_utf8(attr.key.as_ref())
                        .map_err(|e| UploadError::Parse(e.to_string()))?;
                    let value = std::str::from_utf8(&attr.value)
                        .map_err(|e| UploadError::Parse(e.to_string()))?;
                    match key {
                        "old_id" => old_id = value.parse().ok(),
                        "new_id" => new_id = value.parse().ok(),
                        "new_version" => new_version = value.parse().ok(),
                        _ => {}
                    }
                }

                if let (Some(old_id), Some(new_id), Some(new_version)) =
                    (old_id, new_id, new_version)
                {
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

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn changeset_create_xml_has_created_by_and_comment() {
        let xml = build_changeset_create_xml("fix <stuff>");
        assert!(xml.contains(&format!(
            r#"<tag k="created_by" v="osm-gpui {}"/>"#,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(xml.contains(r#"<tag k="comment" v="fix &lt;stuff&gt;"/>"#));
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
        let xml = build_changeset_create_xml("Fix <road> & \"stuff\"");
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
}
