//! What happens when an upload goes wrong, and what the editor does with its
//! pending edits afterwards.
//!
//! The app sends each changeset with `upload_chunks::upload_chunk` and applies
//! every accepted one to the layers with `upload_chunks::apply_to_layer`
//! (`MapViewer::upload_to_osm` does this on a background task with a real
//! HTTP client, which a test cannot drive). `upload` below runs the same two
//! calls together through `run_chunked_upload`, against `Server`, a small
//! in-memory stand-in for the OSM API that:
//!
//! - applies a diff atomically (one bad element and nothing changes), checking
//!   versions (409), missing references (412), objects still in use (412) and
//!   the 10,000 element changeset cap;
//! - can be told to fail an upload before it is applied (429, 5xx, 4xx), or
//!   after it was applied (the response is lost: dropped connection, 5xx).

use super::*;
use osm_gpui::http::{Body, HttpClient, HttpError, HttpRequest, HttpResponse, Method};
use osm_gpui::osm_upload::{run_upload, ConflictKind, UploadConflict, UploadError, UploadFailure};
use osm_gpui::upload_chunks::{apply_to_layer, run_chunked_upload, ChunkedUpload, PlanError};
use std::collections::{BTreeSet, HashMap as Map, VecDeque};
use std::sync::Mutex;

const BASE: &str = "https://osm.test";
const CAP: usize = 10_000;

// ---------------------------------------------------------------------------
// A tiny OSM API
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Obj {
    version: i32,
    visible: bool,
    refs: Vec<(char, i64)>,
}

#[derive(Clone, Default)]
struct State {
    nodes: BTreeMap<i64, Obj>,
    ways: BTreeMap<i64, Obj>,
    relations: BTreeMap<i64, Obj>,
    next_id: i64,
}

impl State {
    fn map(&mut self, kind: char) -> &mut BTreeMap<i64, Obj> {
        match kind {
            'n' => &mut self.nodes,
            'w' => &mut self.ways,
            _ => &mut self.relations,
        }
    }
    fn visible(&self, kind: char, id: i64) -> bool {
        let map = match kind {
            'n' => &self.nodes,
            'w' => &self.ways,
            _ => &self.relations,
        };
        map.get(&id).is_some_and(|o| o.visible)
    }
}

/// What the server does to the next diff upload.
#[derive(Clone, Debug)]
enum Fault {
    /// Refuse it before doing anything.
    Reject(u16, &'static str),
    /// Apply it, then lose the connection.
    DropAfterApply,
    /// Apply it, then answer with this status.
    StatusAfterApply(u16),
}

#[derive(Default)]
struct Inner {
    state: State,
    next_changeset: u64,
    open: BTreeSet<u64>,
    closed: Vec<u64>,
    /// One entry per diff upload request: `None` is normal.
    faults: VecDeque<Option<Fault>>,
    /// Elements in each diff upload that arrived (applied or not).
    uploads: Vec<usize>,
}

struct Server {
    inner: Mutex<Inner>,
}

struct Op {
    section: char,
    kind: char,
    id: i64,
    version: i32,
    refs: Vec<(char, i64)>,
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.as_ref() == name.as_bytes()).then(|| String::from_utf8_lossy(&a.value).into_owned())
    })
}

fn parse_ops(xml: &str) -> Vec<Op> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut ops: Vec<Op> = Vec::new();
    let mut section = 'c';
    loop {
        match reader.read_event().expect("the client sent valid xml") {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"create" => section = 'c',
                b"modify" => section = 'm',
                b"delete" => section = 'd',
                b"node" | b"way" | b"relation" => ops.push(Op {
                    section,
                    kind: match e.name().as_ref() {
                        b"node" => 'n',
                        b"way" => 'w',
                        _ => 'r',
                    },
                    id: attr(&e, "id").unwrap().parse().unwrap(),
                    version: attr(&e, "version").map_or(0, |v| v.parse().unwrap()),
                    refs: Vec::new(),
                }),
                b"nd" => ops
                    .last_mut()
                    .unwrap()
                    .refs
                    .push(('n', attr(&e, "ref").unwrap().parse().unwrap())),
                b"member" => {
                    let kind = match attr(&e, "type").unwrap().as_str() {
                        "node" => 'n',
                        "way" => 'w',
                        _ => 'r',
                    };
                    ops.last_mut()
                        .unwrap()
                        .refs
                        .push((kind, attr(&e, "ref").unwrap().parse().unwrap()));
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    ops
}

fn word(kind: char) -> &'static str {
    match kind {
        'n' => "Node",
        'w' => "Way",
        _ => "Relation",
    }
}

impl Server {
    /// A server that holds exactly `data`, every element at the version the
    /// test builder gave it (ways and nodes 3, relations 2).
    fn holding(data: &OsmData) -> Server {
        let mut state = State {
            next_id: 1000,
            ..State::default()
        };
        for n in data.nodes.values() {
            state.nodes.insert(
                n.id,
                Obj {
                    version: n.version,
                    visible: true,
                    refs: Vec::new(),
                },
            );
        }
        for w in data.ways.values() {
            state.ways.insert(
                w.id,
                Obj {
                    version: w.version,
                    visible: true,
                    refs: w.nodes.iter().map(|&n| ('n', n)).collect(),
                },
            );
        }
        for r in &data.relations {
            state.relations.insert(
                r.id,
                Obj {
                    version: r.version,
                    visible: true,
                    refs: r
                        .members
                        .iter()
                        .map(|m| {
                            (
                                match m.member_type {
                                    MemberType::Node => 'n',
                                    MemberType::Way => 'w',
                                    MemberType::Relation => 'r',
                                },
                                m.reference,
                            )
                        })
                        .collect(),
                },
            );
        }
        Server {
            inner: Mutex::new(Inner {
                state,
                next_changeset: 42,
                ..Inner::default()
            }),
        }
    }

    fn fail_uploads(&self, faults: Vec<Option<Fault>>) {
        self.inner.lock().unwrap().faults = faults.into();
    }

    /// Someone else saves node `id`: its version goes up.
    fn someone_else_edits_node(&self, id: i64) {
        self.inner
            .lock()
            .unwrap()
            .state
            .nodes
            .get_mut(&id)
            .unwrap()
            .version += 1;
    }
    fn someone_else_deletes_node(&self, id: i64) {
        let mut g = self.inner.lock().unwrap();
        let n = g.state.nodes.get_mut(&id).unwrap();
        n.visible = false;
        n.version += 1;
    }
    fn someone_else_adds_way(&self, id: i64, nodes: &[i64]) {
        self.inner.lock().unwrap().state.ways.insert(
            id,
            Obj {
                version: 1,
                visible: true,
                refs: nodes.iter().map(|&n| ('n', n)).collect(),
            },
        );
    }

    fn node_version(&self, id: i64) -> Option<i32> {
        self.inner
            .lock()
            .unwrap()
            .state
            .nodes
            .get(&id)
            .map(|o| o.version)
    }
    fn visible_nodes(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .state
            .nodes
            .values()
            .filter(|o| o.visible)
            .count()
    }
    fn way_refs(&self, id: i64) -> Vec<i64> {
        self.inner.lock().unwrap().state.ways[&id]
            .refs
            .iter()
            .map(|r| r.1)
            .collect()
    }
    fn uploads(&self) -> Vec<usize> {
        self.inner.lock().unwrap().uploads.clone()
    }
    fn open_changesets(&self) -> usize {
        self.inner.lock().unwrap().open.len()
    }
    fn closed_changesets(&self) -> Vec<u64> {
        self.inner.lock().unwrap().closed.clone()
    }

    /// Apply a whole diff or nothing (the API is transactional).
    fn apply(state: &State, xml: &str) -> Result<(State, String), (u16, String)> {
        let mut st = state.clone();
        let mut placeholder: Map<(char, i64), i64> = Map::new();
        let mut out = String::new();
        let resolve = |ph: &Map<(char, i64), i64>, r: (char, i64)| -> (char, i64) {
            (r.0, ph.get(&r).copied().unwrap_or(r.1))
        };
        for op in parse_ops(xml) {
            let refs: Vec<(char, i64)> =
                op.refs.iter().map(|&r| resolve(&placeholder, r)).collect();
            if op.section != 'd' {
                for &(k, id) in &refs {
                    if !st.visible(k, id) {
                        return Err((
                            412,
                            format!(
                                "Precondition failed: {} {} requires the {}s with id in ({}), which either do not exist, or are not visible.",
                                word(op.kind),
                                op.id,
                                word(k).to_lowercase(),
                                id
                            ),
                        ));
                    }
                }
            }
            match op.section {
                'c' => {
                    if op.id >= 0 {
                        return Err((
                            400,
                            format!("Placeholder IDs must be negative, got {}", op.id),
                        ));
                    }
                    let id = st.next_id;
                    st.next_id += 1;
                    st.map(op.kind).insert(
                        id,
                        Obj {
                            version: 1,
                            visible: true,
                            refs,
                        },
                    );
                    placeholder.insert((op.kind, op.id), id);
                    out.push_str(&format!(
                        r#"<{} old_id="{}" new_id="{id}" new_version="1"/>"#,
                        word(op.kind).to_lowercase(),
                        op.id
                    ));
                }
                'm' => {
                    let name = word(op.kind);
                    let Some(obj) = st.map(op.kind).get_mut(&op.id) else {
                        return Err((404, format!("{name} {} not found", op.id)));
                    };
                    if !obj.visible {
                        return Err((
                            410,
                            format!(
                                "The {} {} has already been deleted",
                                name.to_lowercase(),
                                op.id
                            ),
                        ));
                    }
                    if obj.version != op.version {
                        return Err((
                            409,
                            format!(
                                "Version mismatch: Provided {}, server had: {} of {name} {}",
                                op.version, obj.version, op.id
                            ),
                        ));
                    }
                    obj.version += 1;
                    if op.kind != 'n' {
                        obj.refs = refs;
                    }
                    out.push_str(&format!(
                        r#"<{} old_id="{}" new_id="{}" new_version="{}"/>"#,
                        name.to_lowercase(),
                        op.id,
                        op.id,
                        obj.version
                    ));
                }
                _ => {
                    let name = word(op.kind);
                    let Some(obj) = st.map(op.kind).get(&op.id).cloned() else {
                        return Err((404, format!("{name} {} not found", op.id)));
                    };
                    if !obj.visible {
                        return Err((
                            410,
                            format!(
                                "The {} {} has already been deleted",
                                name.to_lowercase(),
                                op.id
                            ),
                        ));
                    }
                    if obj.version != op.version {
                        return Err((
                            409,
                            format!(
                                "Version mismatch: Provided {}, server had: {} of {name} {}",
                                op.version, obj.version, op.id
                            ),
                        ));
                    }
                    for (user_kind, plural) in [('w', "ways"), ('r', "relations")] {
                        let users: Vec<String> = st
                            .map(user_kind)
                            .iter()
                            .filter(|(_, o)| o.visible && o.refs.contains(&(op.kind, op.id)))
                            .map(|(id, _)| id.to_string())
                            .collect();
                        if !users.is_empty() {
                            return Err((
                                412,
                                format!(
                                    "Precondition failed: {name} {} is still used by {plural} {}.",
                                    op.id,
                                    users.join(",")
                                ),
                            ));
                        }
                    }
                    let obj = st.map(op.kind).get_mut(&op.id).unwrap();
                    obj.visible = false;
                    obj.version += 1;
                    out.push_str(&format!(r#"<{} old_id="{}"/>"#, name.to_lowercase(), op.id));
                }
            }
        }
        Ok((st, format!("<diffResult>{out}</diffResult>")))
    }
}

fn plain(status: u16, body: impl Into<Vec<u8>>) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status,
        body: body.into(),
    })
}

fn refused(status: u16, body: impl Into<String>) -> Result<HttpResponse, HttpError> {
    Err(HttpError::Status {
        status,
        body: body.into().into_bytes(),
    })
}

impl HttpClient for Server {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let path = req.url.strip_prefix(BASE).unwrap().to_string();
        let mut g = self.inner.lock().unwrap();
        if path == "/api/0.6/changeset/create" {
            let id = g.next_changeset;
            g.next_changeset += 1;
            g.open.insert(id);
            return plain(200, id.to_string());
        }
        let rest = path.strip_prefix("/api/0.6/changeset/").unwrap();
        let (id, action) = rest.split_once('/').unwrap();
        let id: u64 = id.parse().unwrap();
        match (req.method, action) {
            (Method::Put, "close") => {
                if !g.open.remove(&id) {
                    return refused(409, format!("The changeset {id} was closed at 2026-01-01"));
                }
                g.closed.push(id);
                plain(200, "")
            }
            (Method::Post, "upload") => {
                let Some(Body::Text(xml)) = req.body else {
                    panic!("an upload carries a text body");
                };
                let ops = parse_ops(&xml).len();
                g.uploads.push(ops);
                let fault = g.faults.pop_front().flatten();
                if let Some(Fault::Reject(status, body)) = &fault {
                    return refused(*status, *body);
                }
                if !g.open.contains(&id) {
                    return refused(409, format!("The changeset {id} was closed at 2026-01-01"));
                }
                if ops > CAP {
                    return refused(
                        409,
                        format!("Changeset size limit exceeded: {ops} changes, at most {CAP}"),
                    );
                }
                match Server::apply(&g.state, &xml) {
                    Err((status, body)) => refused(status, body),
                    Ok((state, answer)) => {
                        g.state = state;
                        match fault {
                            Some(Fault::DropAfterApply) => {
                                Err(HttpError::Transport("connection reset by peer".into()))
                            }
                            Some(Fault::StatusAfterApply(status)) => refused(status, "oops"),
                            _ => plain(200, answer),
                        }
                    }
                }
            }
            other => panic!("unexpected request {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Upload what the layer has pending, the way the app does: in changesets of
/// at most `max` elements, applying each accepted one to the layer before the
/// next is sent.
fn upload(s: &mut Session, server: &Server, max: usize) -> ChunkedUpload {
    let diff = s.diff();
    let layer = s.layer;
    let layers = &mut s.view.layer_manager;
    let history = &mut s.view.undo_stack;
    run_chunked_upload(
        server,
        BASE,
        "tok",
        "a comment",
        &[("Net", diff)],
        max,
        &mut |_, _| {},
        &mut |done| {
            apply_to_layer(layers.find_layer_mut(layer).unwrap(), done);
            // As `MapViewer::upload_to_osm` does after each accepted chunk.
            history.remove_touching(&crate::uploaded_taint(&[layer], &done.chunk));
        },
    )
    .expect("the edits can be planned")
}

/// A tag edit recorded in the undo history, as the tag editor does.
fn edit_tag(s: &mut Session, kind: FeatureKind, id: i64, key: &str, value: &str) {
    let feature = s.feature(kind, id);
    let before = s
        .tags(kind, id)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|t| t.1);
    s.set_tag(kind, id, key, value);
    s.view
        .undo_stack
        .push(crate::undo::UndoableAction::SetTags {
            entries: vec![(feature, key.to_string(), before, Some(value.to_string()))],
        });
}

fn failure_of(up: &ChunkedUpload) -> &UploadFailure {
    up.failure.as_ref().expect("the upload failed")
}

fn conflict_of(up: &ChunkedUpload) -> &UploadConflict {
    match &failure_of(up).error {
        UploadError::Conflict(c) => c,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn ways_net() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5])
        .way(10, &[1, 2], &[("highway", "residential")])
        .way(11, &[4, 5], &[("highway", "service")])
        .done()
}

/// The standard failed-upload aftermath: every changeset the client opened
/// was closed and none is left open on the server.
fn assert_all_changesets_closed(server: &Server, opened: usize) {
    assert_eq!(server.open_changesets(), 0, "a changeset was left open");
    assert_eq!(server.closed_changesets().len(), opened);
}

// ---------------------------------------------------------------------------
// 1. Partial failure
// ---------------------------------------------------------------------------

#[gpui::test]
fn one_rejected_element_rolls_back_the_whole_diff_and_every_edit_stays_pending(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        for id in [1, 2, 3] {
            s.set_tag(N, id, "name", "mine");
        }
        assert_eq!(s.counts(), (0, 3, 0));
        // Someone else saves node 2 after we downloaded it.
        server.someone_else_edits_node(2);

        let up = upload(s, &server, CAP);
        assert!(up.completed.is_empty());
        let conflict = conflict_of(&up);
        assert_eq!(
            conflict.kind,
            ConflictKind::VersionMismatch {
                provided: 3,
                server: 4
            }
        );
        assert_eq!(conflict.object, Some((N, 2)));
        assert!(conflict.describe().contains("Node 2"));
        assert_eq!(failure_of(&up).changeset_id, Some(42));

        // The server applies a diff all or nothing: the two good nodes were
        // not saved either.
        assert_eq!(server.node_version(1), Some(3));
        assert_eq!(server.node_version(2), Some(4));
        assert_eq!(server.node_version(3), Some(3));
        // The changeset was closed, nothing is left open.
        assert_all_changesets_closed(&server, 1);
        // The layer still holds all three edits, with their old versions.
        assert_eq!(s.counts(), (0, 3, 0));
        assert!(s.xml().contains(r#"<node id="2" "#));
        assert!(s.xml().contains(r#"version="3""#));
    });
}

#[gpui::test]
fn a_lost_response_after_the_server_applied_the_diff_is_reported_as_unknown_and_not_resent(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        let before = server.visible_nodes();
        s.view.set_selection(Vec::new());
        s.editable_mut().add_node(40.5, -74.5);
        s.set_tag(N, 1, "name", "mine");
        assert_eq!(s.counts(), (1, 1, 0));
        server.fail_uploads(vec![Some(Fault::DropAfterApply)]);

        let up = upload(s, &server, CAP);
        assert!(up.completed.is_empty());
        match &failure_of(&up).error {
            UploadError::OutcomeUnknown {
                changeset_id, url, ..
            } => {
                assert_eq!(*changeset_id, 42);
                assert!(url.ends_with("/changeset/42"), "{url}");
            }
            other => panic!("expected an unknown outcome, got {other:?}"),
        }
        // The message tells the user where to look.
        assert!(failure_of(&up).error.to_string().contains("/changeset/42"));
        // The diff went out exactly once, the changeset was closed.
        assert_eq!(server.uploads(), vec![2]);
        assert_all_changesets_closed(&server, 1);
        // The server did apply it ...
        assert_eq!(server.visible_nodes(), before + 1);
        assert_eq!(server.node_version(1), Some(4));
        // ... but the layer cannot know, so everything is still pending
        // under the old ids and versions.
        assert_eq!(s.counts(), (1, 1, 0));
        assert!(s.xml().contains(r#"id="-1""#));
    });
}

#[gpui::test]
fn sending_again_after_an_unknown_outcome_duplicates_creates_and_conflicts_on_modifies(
    cx: &mut TestAppContext,
) {
    // The editor never resends a diff by itself (see the test above), because
    // a second copy of an applied diff creates every new element twice. This
    // pins what a manual "Upload" press does in that state: the client has no
    // way to tell, so it is up to the user to check the changeset first.
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        let before = server.visible_nodes();
        s.editable_mut().add_node(40.5, -74.5);
        server.fail_uploads(vec![Some(Fault::DropAfterApply)]);
        let first = upload(s, &server, CAP);
        assert!(matches!(
            failure_of(&first).error,
            UploadError::OutcomeUnknown { .. }
        ));
        assert_eq!(server.visible_nodes(), before + 1);

        let second = upload(s, &server, CAP);
        assert!(second.failure.is_none());
        assert_eq!(
            server.visible_nodes(),
            before + 2,
            "the new node exists twice on the server now"
        );
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_refusal_in_a_later_changeset_keeps_the_accepted_ones_and_retry_sends_only_the_rest(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        let before = server.visible_nodes();
        for i in 0..5 {
            s.editable_mut().add_node(40.5 + i as f64 * 0.001, -74.5);
        }
        assert_eq!(s.counts(), (5, 0, 0));
        // Changesets of 2, 2 and 1. The second is refused with 429 (before
        // anything is applied).
        server.fail_uploads(vec![None, Some(Fault::Reject(429, "slow down"))]);
        let up = upload(s, &server, 2);
        assert_eq!(up.total, 3);
        assert_eq!(up.completed.len(), 1);
        match &failure_of(&up).error {
            UploadError::Http { status: 429, .. } => {}
            other => panic!("expected 429, got {other:?}"),
        }
        // The first changeset is in the layer (its nodes have server ids and
        // are no longer pending); the other three are pending.
        assert_eq!(server.visible_nodes(), before + 2);
        assert_eq!(s.counts(), (3, 0, 0));
        assert_all_changesets_closed(&server, 2);
        assert_eq!(server.uploads(), vec![2, 2]);

        // Retry: only the three remaining nodes go out (2 + 1), nothing is
        // created twice.
        let retry = upload(s, &server, 2);
        assert!(retry.failure.is_none());
        assert_eq!(retry.completed.len(), 2);
        assert_eq!(server.visible_nodes(), before + 5);
        assert_eq!(server.uploads(), vec![2, 2, 2, 1]);
        assert_eq!(s.counts(), (0, 0, 0));
        assert_all_changesets_closed(&server, 4);
    });
}

#[gpui::test]
fn a_lost_response_in_a_later_changeset_leaves_that_chunk_pending_and_the_rest_unsent(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        let before = server.visible_nodes();
        for i in 0..5 {
            s.editable_mut().add_node(40.5 + i as f64 * 0.001, -74.5);
        }
        server.fail_uploads(vec![None, Some(Fault::DropAfterApply)]);
        let up = upload(s, &server, 2);
        assert_eq!(up.completed.len(), 1);
        assert!(matches!(
            failure_of(&up).error,
            UploadError::OutcomeUnknown { .. }
        ));
        // The third changeset was never opened: one request fewer than a
        // full run, and nothing dangling.
        assert_eq!(server.uploads(), vec![2, 2]);
        assert_all_changesets_closed(&server, 2);
        // The server holds four of the five new nodes, the layer thinks it
        // still owes three (the second chunk and the third).
        assert_eq!(server.visible_nodes(), before + 4);
        assert_eq!(s.counts(), (3, 0, 0));
    });
}

#[gpui::test]
fn a_way_split_from_its_new_nodes_by_a_changeset_boundary_names_server_ids(
    cx: &mut TestAppContext,
) {
    let server = Server::holding(&ways_net());
    let node = |id: i64| OsmNode {
        id,
        lat: 1.0,
        lon: 2.0,
        version: 0,
        tags: Default::default(),
    };
    let diff = LayerDiff {
        created_nodes: (1..=4).map(|i| node(-i)).collect(),
        created_ways: vec![OsmWay {
            id: -10,
            nodes: vec![-1, -2, -3, -4],
            version: 0,
            tags: Default::default(),
        }],
        ..Default::default()
    };
    // Three elements per changeset: the way lands in the second one, after
    // its nodes went out in the first.
    let up = run_chunked_upload(
        &server,
        BASE,
        "tok",
        "c",
        &[("l", diff)],
        3,
        &mut |_, _| {},
        &mut |_| {},
    )
    .unwrap();
    assert!(up.failure.is_none());
    assert_eq!(up.total, 2);
    let way = *up.completed[1].result.way_id_remap.get(&-10).unwrap();
    let refs = server.way_refs(way.0);
    assert_eq!(refs.len(), 4);
    assert!(refs.iter().all(|&r| r > 0), "{refs:?}");
    let _ = cx;
}

// ---------------------------------------------------------------------------
// 2. How each failure is reported, and that a retry is safe
// ---------------------------------------------------------------------------

#[gpui::test]
fn a_409_names_the_node_and_both_versions_and_a_retry_with_keep_mine_goes_through(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        edit_tag(s, N, 1, "name", "mine");
        server.someone_else_edits_node(1);

        let up = upload(s, &server, CAP);
        let c = conflict_of(&up);
        assert_eq!(c.status, 409);
        let text = failure_of(&up).error.to_string();
        assert_eq!(text, c.describe());
        assert!(text.contains("Node 1"), "{text}");
        assert!(text.contains('3') && text.contains('4'), "{text}");
        assert_eq!(s.counts(), (0, 1, 0));
        assert_eq!(s.steps(), 1);

        // Sending the same thing again fails the same way and changes
        // nothing: a refusal is always safe to retry.
        let again = upload(s, &server, CAP);
        assert_eq!(conflict_of(&again).kind, c.kind);
        assert_eq!(server.node_version(1), Some(4));

        // "Keep mine": adopt the server's version as the base, keep the edit.
        assert!(s.editable_mut().set_base_version(N, 1, 4));
        assert_eq!(s.counts(), (0, 1, 0));
        assert_eq!(s.steps(), 1, "the undo history is kept");
        let ok = upload(s, &server, CAP);
        assert!(ok.failure.is_none());
        assert_eq!(server.node_version(1), Some(5));
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_412_for_a_node_that_is_gone_names_the_way_that_needs_it(cx: &mut TestAppContext) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.set_tag(W, 10, "name", "mine");
        // Meanwhile someone deleted node 2 (and with it the ways using it
        // were edited; here the server just no longer has the node).
        server.someone_else_deletes_node(2);

        let up = upload(s, &server, CAP);
        let c = conflict_of(&up);
        assert_eq!(c.status, 412);
        assert_eq!(c.kind, ConflictKind::MissingDependency);
        assert_eq!(c.object, Some((W, 10)));
        assert!(failure_of(&up).error.to_string().contains("Way 10"));
        assert_all_changesets_closed(&server, 1);
        assert_eq!(s.counts(), (0, 1, 0));

        // A retry changes nothing on the server and fails the same way.
        let again = upload(s, &server, CAP);
        assert_eq!(conflict_of(&again).kind, ConflictKind::MissingDependency);
        assert_eq!(s.counts(), (0, 1, 0));
    });
}

#[gpui::test]
fn a_412_for_deleting_something_still_in_use_lists_the_user(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &[("highway", "residential")])
        .done();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        // Node 3 is loose for us; someone else has since put it in a new way.
        server.someone_else_adds_way(77, &[3, 4]);
        s.select(&[(N, 3)]);
        assert!(s.act("delete"));
        assert_eq!(s.counts(), (0, 0, 1));

        let up = upload(s, &server, CAP);
        let c = conflict_of(&up);
        assert_eq!(c.status, 412);
        assert_eq!(c.kind, ConflictKind::StillInUse { by: vec![(W, 77)] });
        assert!(failure_of(&up).error.to_string().contains("Way 77"));
        assert_eq!(server.node_version(3), Some(3));
        assert_all_changesets_closed(&server, 1);
        // The delete is still pending, and undo still works.
        assert_eq!(s.counts(), (0, 0, 1));
        s.undo();
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_429_is_a_definite_refusal_with_the_status_shown_and_a_retry_sends_the_edit_once(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.set_tag(N, 1, "name", "mine");
        server.fail_uploads(vec![Some(Fault::Reject(429, "Rate limit exceeded"))]);

        let up = upload(s, &server, CAP);
        match &failure_of(&up).error {
            UploadError::Http { status: 429, body } => assert!(body.contains("Rate limit")),
            other => panic!("expected a plain 429, got {other:?}"),
        }
        let text = failure_of(&up).error.to_string();
        assert!(text.contains("429"), "{text}");
        assert!(text.contains("Rate limit exceeded"), "{text}");
        assert_eq!(
            server.uploads(),
            vec![1],
            "sent once, not retried by itself"
        );
        assert_all_changesets_closed(&server, 1);
        assert_eq!(server.node_version(1), Some(3));
        assert_eq!(s.counts(), (0, 1, 0));

        let ok = upload(s, &server, CAP);
        assert!(ok.failure.is_none());
        assert_eq!(server.node_version(1), Some(4));
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn server_errors_are_unknown_outcomes_except_509_which_is_refused_before_any_work(
    cx: &mut TestAppContext,
) {
    for status in [500u16, 502, 503, 504] {
        let data = ways_net();
        let server = Server::holding(&data);
        with_session(cx, data, move |s| {
            s.set_tag(N, 1, "name", "mine");
            server.fail_uploads(vec![Some(Fault::StatusAfterApply(status))]);
            let up = upload(s, &server, CAP);
            assert!(
                matches!(failure_of(&up).error, UploadError::OutcomeUnknown { .. }),
                "{status}"
            );
            // Applied on the server, still pending here, sent once.
            assert_eq!(server.node_version(1), Some(4), "{status}");
            assert_eq!(s.counts(), (0, 1, 0), "{status}");
            assert_eq!(server.uploads(), vec![1], "{status}");
            assert_all_changesets_closed(&server, 1);
        });
    }
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.set_tag(N, 1, "name", "mine");
        server.fail_uploads(vec![Some(Fault::Reject(509, "Bandwidth Limit Exceeded"))]);
        let up = upload(s, &server, CAP);
        assert!(matches!(
            failure_of(&up).error,
            UploadError::Http { status: 509, .. }
        ));
        assert_eq!(server.node_version(1), Some(3));
        assert_eq!(s.counts(), (0, 1, 0));
    });
}

#[gpui::test]
fn a_changeset_closed_under_us_is_reported_as_a_refusal_with_the_edits_kept(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.set_tag(N, 1, "name", "mine");
        server.fail_uploads(vec![Some(Fault::Reject(
            409,
            "The changeset 42 was closed at 2026-01-01 00:00:00 UTC",
        ))]);
        let up = upload(s, &server, CAP);
        let c = conflict_of(&up);
        assert_eq!(c.kind, ConflictKind::Other);
        assert!(failure_of(&up).error.to_string().contains("409"));
        assert_eq!(s.counts(), (0, 1, 0));
    });
}

// ---------------------------------------------------------------------------
// 3. The 10,000 element changeset cap
// ---------------------------------------------------------------------------

fn big_net(count: i64) -> OsmData {
    let mut b = Build::new();
    for id in 1..=count {
        b = b.node(id, &[]);
    }
    b.done()
}

fn nudge_all(s: &mut Session, count: i64) {
    let moves: Vec<(i64, f64, f64)> = (1..=count)
        .map(|id| {
            let (lat, lon) = s.editable().node_lat_lon(id).unwrap();
            (id, lat + 0.01, lon + 0.01)
        })
        .collect();
    s.editable_mut().commit_node_moves(&moves).unwrap();
}

#[gpui::test]
fn exactly_10000_changes_go_in_one_changeset(cx: &mut TestAppContext) {
    let data = big_net(10_000);
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        nudge_all(s, 10_000);
        assert_eq!(s.counts(), (0, 10_000, 0));
        let up = upload(s, &server, CAP);
        assert!(up.failure.is_none());
        assert_eq!(up.total, 1);
        assert_eq!(server.uploads(), vec![10_000]);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn one_change_over_the_cap_is_split_into_two_changesets_and_the_whole_diff_would_be_refused(
    cx: &mut TestAppContext,
) {
    let data = big_net(10_001);
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        nudge_all(s, 10_001);
        assert_eq!(s.counts(), (0, 10_001, 0));

        // Sent as one document the server refuses it, and the changeset is
        // closed. This is what the split is for.
        let whole =
            run_upload(&server, BASE, "tok", "c", &[("Net", s.diff())], &mut |_| {}).unwrap_err();
        match &whole.error {
            UploadError::Conflict(c) => assert!(c.message.contains("size limit"), "{c:?}"),
            other => panic!("expected the cap to refuse it, got {other:?}"),
        }
        assert_all_changesets_closed(&server, 1);
        assert_eq!(server.node_version(1), Some(3));

        // What the app does: 10,000 then 1, each in its own changeset.
        let up = upload(s, &server, CAP);
        assert!(up.failure.is_none());
        assert_eq!(up.total, 2);
        assert_eq!(server.uploads(), vec![10_001, 10_000, 1]);
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(server.node_version(1), Some(4));
        assert_eq!(server.node_version(10_001), Some(4));
    });
}

#[gpui::test]
fn a_failure_in_the_second_changeset_of_an_oversized_upload_leaves_exactly_the_rest_pending(
    cx: &mut TestAppContext,
) {
    let data = big_net(10_001);
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        nudge_all(s, 10_001);
        server.fail_uploads(vec![None, Some(Fault::Reject(429, "slow"))]);
        let up = upload(s, &server, CAP);
        assert_eq!(up.completed.len(), 1);
        assert_eq!(s.counts(), (0, 1, 0));
        assert_eq!(server.uploads(), vec![10_000, 1]);
        // Retrying sends the one remaining change only.
        let retry = upload(s, &server, CAP);
        assert!(retry.failure.is_none());
        assert_eq!(server.uploads(), vec![10_000, 1, 1]);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn a_cycle_of_new_relations_bigger_than_a_changeset_is_refused_before_anything_is_sent(
    cx: &mut TestAppContext,
) {
    // New relations that contain each other must be created together. The
    // real limit needs 10,001 of them; a limit of 1 shows the same refusal.
    let rel = |id: i64, other: i64| OsmRelation {
        id,
        version: 0,
        tags: Default::default(),
        members: vec![OsmMember {
            member_type: MemberType::Relation,
            reference: other,
            role: String::new(),
        }],
    };
    let diff = LayerDiff {
        created_relations: vec![rel(-1, -2), rel(-2, -1)],
        ..Default::default()
    };
    let server = Server::holding(&ways_net());
    let err = run_chunked_upload(
        &server,
        BASE,
        "tok",
        "c",
        &[("l", diff)],
        1,
        &mut |_, _| {},
        &mut |_| {},
    )
    .map(|_| ())
    .unwrap_err();
    assert_eq!(err, PlanError::CycleTooLarge { size: 2, max: 1 });
    assert!(server.uploads().is_empty());
    assert_eq!(
        server.open_changesets() + server.closed_changesets().len(),
        0
    );
    let _ = cx;
}

// ---------------------------------------------------------------------------
// 4. Undo after an upload, and editing something the server has moved on
// ---------------------------------------------------------------------------

#[gpui::test]
fn undoing_a_tag_edit_after_it_was_uploaded_is_a_new_edit_on_the_new_version(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        edit_tag(s, N, 1, "name", "mine");
        assert!(upload(s, &server, CAP).failure.is_none());
        assert_eq!(server.node_version(1), Some(4));
        assert_eq!(s.counts(), (0, 0, 0));

        // The undo history survives the upload; undoing reverts the tag
        // locally, which is now a pending change against version 4.
        s.undo();
        assert!(s.tags(N, 1).is_empty());
        assert_eq!(s.counts(), (0, 1, 0));
        assert!(s.xml().contains(r#"<node id="1""#));
        assert!(s.xml().contains(r#"version="4""#), "{}", s.xml());
        let ok = upload(s, &server, CAP);
        assert!(ok.failure.is_none());
        assert_eq!(server.node_version(1), Some(5));

        // Redo goes back the same way.
        s.redo();
        assert_eq!(s.tags(N, 1), pairs(&[("name", "mine")]));
        assert!(upload(s, &server, CAP).failure.is_none());
        assert_eq!(server.node_version(1), Some(6));
    });
}

#[gpui::test]
fn undoing_a_split_after_it_was_uploaded_does_not_leave_the_uploaded_half_behind(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &[("highway", "residential")])
        .done();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.select(&[(N, 3)]);
        assert!(s.act("split"));
        assert_eq!(s.new_ways(), vec![-1]);
        assert!(upload(s, &server, CAP).failure.is_none());
        // The new half has its server id now.
        assert!(s.new_ways().is_empty());
        let halves = s.snap().ways.len();
        assert_eq!(halves, 2);

        // The split can no longer be undone: its step named the placeholder
        // -1, and replaying it would restore way 10 whole next to the
        // uploaded half (two ways on the same nodes).
        assert_eq!(s.steps(), 0);
        assert_eq!(s.snap().ways.len(), 2);
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn undoing_a_delete_after_it_was_uploaded_does_not_try_to_modify_a_deleted_object(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2])
        .way(10, &[1, 2], &[("highway", "residential")])
        .done();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        assert!(upload(s, &server, CAP).failure.is_none());
        assert_eq!(s.counts(), (0, 0, 0));

        // The way is gone from the server. Undoing the delete would bring
        // back way 10 and its nodes with their old ids, and the next upload
        // would send them as creates with positive ids, which the server
        // refuses. The step is dropped from the history instead.
        assert_eq!(s.steps(), 0);
        assert!(!s.has_way(10));
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn an_unrelated_change_on_the_server_does_not_block_uploading_other_edits(cx: &mut TestAppContext) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        s.set_tag(N, 1, "name", "mine");
        // Node 5 changed on the server, but we did not touch it, so it is
        // not part of the diff and cannot conflict.
        server.someone_else_edits_node(5);
        assert!(upload(s, &server, CAP).failure.is_none());
        assert_eq!(server.node_version(1), Some(4));
    });
}

#[gpui::test]
fn editing_then_undoing_an_element_the_server_changed_uploads_nothing_and_cannot_conflict(
    cx: &mut TestAppContext,
) {
    let data = ways_net();
    let server = Server::holding(&data);
    with_session(cx, data, move |s| {
        edit_tag(s, N, 1, "name", "mine");
        server.someone_else_edits_node(1);
        s.undo();
        assert_eq!(s.counts(), (0, 0, 0));
        let up = upload(s, &server, CAP);
        assert_eq!(up.total, 0);
        assert!(server.uploads().is_empty());
    });
}

#[gpui::test]
fn after_an_unknown_outcome_the_dialog_asks_for_a_check_and_offers_no_plain_retry(
    cx: &mut TestAppContext,
) {
    use gpui::AppContext as _;
    use osm_gpui::upload_chunks::UploadStop;
    use osm_gpui::ui::upload_dialog::{ConfirmKind, FailureKind, UploadDialog};

    cx.update(gpui_component::init);
    let data = ways_net();
    let server = Server::holding(&data);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, window, cx| {
            let layer = view.layer_manager.alloc_id();
            view.layer_manager.add_layer(Box::new(
                osm_gpui::layers::osm_layer::OsmLayer::new_with_data(
                    layer,
                    "Net".to_string(),
                    Arc::new(data),
                ),
            ));
            view.active_layer = Some(layer);
            let dialog = cx.new(|cx| UploadDialog::new(window, cx, Vec::new()));
            let mut s = Session { view, cx, layer };
            s.editable_mut().add_node(40.5, -74.5);
            server.fail_uploads(vec![Some(Fault::DropAfterApply)]);
            let error = upload(&mut s, &server, CAP)
                .failure
                .expect("the upload failed")
                .error;
            let UploadStop::OutcomeUnknown(message) = UploadStop::from_error(&error) else {
                panic!("an unknown outcome must not be reported as a plain failure");
            };
            assert_eq!(message, error.to_string());
            dialog.update(s.cx, |d, cx| {
                d.fail_outcome_unknown(message.clone().into(), cx)
            });
            let dialog = dialog.read(s.cx);
            assert_eq!(dialog.failure_kind(), Some(FailureKind::OutcomeUnknown));
            assert_eq!(
                dialog.confirm_button_kind(),
                ConfirmKind::UploadAgainAfterCheck
            );
            assert_ne!(dialog.confirm_button_kind(), ConfirmKind::Retry);
        })
        .unwrap();
}

#[test]
fn only_a_refusal_and_an_unknown_outcome_get_their_own_stop() {
    use osm_gpui::upload_chunks::UploadStop;
    assert!(matches!(
        UploadStop::from_error(&UploadError::Network("x".into())),
        UploadStop::Failed(_)
    ));
    assert!(matches!(
        UploadStop::from_error(&UploadError::Parse("x".into())),
        UploadStop::Failed(_)
    ));
    assert!(matches!(
        UploadStop::from_error(&UploadError::Http {
            status: 400,
            body: String::new()
        }),
        UploadStop::Failed(_)
    ));
    assert!(matches!(
        UploadStop::from_error(&UploadError::OutcomeUnknown {
            changeset_id: 1,
            url: "u".into(),
            detail: "d".into()
        }),
        UploadStop::OutcomeUnknown(_)
    ));
    assert!(matches!(
        UploadStop::from_error(&UploadError::Conflict(UploadConflict {
            kind: ConflictKind::Other,
            object: None,
            status: 409,
            message: String::new()
        })),
        UploadStop::Refused(_)
    ));
}
