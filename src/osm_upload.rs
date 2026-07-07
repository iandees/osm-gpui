//! OSM API 0.6 changeset lifecycle (create/upload/close) and osmChange XML
//! (de)serialization. See https://wiki.openstreetmap.org/wiki/API_v0.6#Changesets.
//!
//! Every network call here is synchronous (`ureq`) and must be run from a
//! background executor, never the UI thread — mirrors `osm_api::fetch_bbox`.

use std::collections::HashMap;
use std::time::Duration;

use crate::layers::diff::LayerDiff;
use crate::osm::{OsmNode, OsmWay};

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
            UploadError::Network(msg) => write!(f, "Network error: {}", msg),
            UploadError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(f, "OSM API error {}: {}", status, first_line)
            }
            UploadError::Conflict { body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "Upload conflict (409): another edit changed this data first. \
                     Re-download and reconcile before retrying. {}",
                    first_line
                )
            }
            UploadError::PreconditionFailed { body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "Upload failed (412): something your edit refers to was changed or \
                     deleted first. Re-download and reconcile before retrying. {}",
                    first_line
                )
            }
            UploadError::Parse(msg) => write!(f, "Failed to parse OSM API response: {}", msg),
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
        xml_escape(crate::USER_AGENT),
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
fn xml_escape(s: &str) -> String {
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
pub fn build_osm_change_xml(changeset_id: u64, layers: &[(&str, LayerDiff)]) -> String {
    let mut create_body = String::new();
    let mut modify_body = String::new();
    let mut delete_body = String::new();

    for (_name, diff) in layers {
        for n in &diff.created_nodes {
            create_body.push_str(&node_xml(n, changeset_id));
        }
        for w in &diff.created_ways {
            create_body.push_str(&way_xml(w, changeset_id));
        }
        for n in &diff.modified_nodes {
            modify_body.push_str(&node_xml(n, changeset_id));
        }
        for w in &diff.modified_ways {
            modify_body.push_str(&way_xml(w, changeset_id));
        }
        for &(id, version) in &diff.deleted_node_ids {
            delete_body.push_str(&delete_node_xml(id, version, changeset_id));
        }
        for &(id, version) in &diff.deleted_way_ids {
            delete_body.push_str(&delete_way_xml(id, version, changeset_id));
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        r#"<osmChange version="0.6" generator="{}">"#,
        xml_escape(crate::USER_AGENT)
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

fn delete_node_xml(id: i64, version: i32, changeset_id: u64) -> String {
    format!(
        r#"<node id="{}" version="{}" changeset="{}"/>"#,
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
                if !is_node && !is_way {
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
                    if is_node {
                        result.node_id_remap.insert(old_id, (new_id, new_version));
                    } else {
                        result.way_id_remap.insert(old_id, (new_id, new_version));
                    }
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
    fn empty_diff_produces_no_sections() {
        let diff = LayerDiff::default();
        let xml = build_osm_change_xml(1, &[("L", diff)]);
        assert_eq!(
            xml,
            r#"<osmChange version="0.6" generator="osm-gpui/0.1.0"></osmChange>"#
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
