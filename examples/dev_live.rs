//! Live test of the upload path against the OpenStreetMap DEVELOPMENT server.
//!
//! ```text
//! cargo run --example dev_live -- login
//! cargo run --example dev_live -- run [--only <scenario>]
//! cargo run --example dev_live -- logout
//! ```
//!
//! No GPUI window is opened. The scenarios build in-memory `OsmLayer`s with
//! edits the way the app does, then call the same functions the app calls to
//! plan (`ChunkQueue`), send (`upload_chunk`), apply (`apply_to_layer`) and
//! resolve (`OsmLayer::adopt_server_element` and friends) an upload. See
//! `CONTRIBUTING.md` for what is written where.
//!
//! Safety: the API base is the constant `osm_gpui::dev_live::API_BASE`, and every
//! request goes through `GuardedClient`, which refuses any other host before
//! sending. The harness never touches the platform keyring, `oauth.json` or
//! `settings.json`; its token lives in `dev-live-token.json` (see
//! `osm_gpui::dev_live`) and is never printed.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use osm_gpui::capabilities::{self, Capabilities};
use osm_gpui::dev_live::{self, GuardedClient, API_BASE};
use osm_gpui::http::{
    fetch_with_retries, HttpClient, HttpError, HttpRequest, HttpResponse, Method, RetryPolicy,
    UreqClient,
};
use osm_gpui::layers::diff::LayerDiff;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::{EditableLayer, LayerId};
use osm_gpui::limits::Limits;
use osm_gpui::osm_api::{self, ServerElement};
use osm_gpui::osm_upload::{ConflictKind, UploadConflict, UploadError, UploadResult};
use osm_gpui::selection::FeatureKind;
use osm_gpui::upload_chunks::{
    apply_to_layer, merge_results, plan_chunks, upload_chunk, ChunkQueue, CompletedChunk,
};
use osm_gpui::{MemberType, OsmData, OsmMember, OsmNode, OsmParser};
use rand::Rng;

/// Chunk limit for the `chunked` scenario (the dev server's own limit is far
/// higher, so a multi-changeset upload is forced with a small one).
const CHUNKED_LIMIT: usize = 25;

type Tags = Vec<(String, String)>;

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// Logs the changeset-lifecycle requests that are sent, and lets the upload
/// loop add its own "applied" notes, so the order of the two can be checked.
struct Recorder<C> {
    inner: C,
    events: Mutex<Vec<String>>,
}

impl<C> Recorder<C> {
    fn note(&self, event: String) {
        self.events.lock().unwrap().push(event);
    }
}

impl<C: HttpClient> HttpClient for Recorder<C> {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        if req.method == Method::Put && req.url.ends_with("/changeset/create") {
            self.note("create".to_string());
        }
        self.inner.request(req)
    }
}

type Client = Recorder<GuardedClient<UreqClient>>;

/// Where this run's test objects go: open ocean, shifted at random so runs do
/// not collide.
#[derive(Clone, Copy)]
struct Area {
    lat: f64,
    lon: f64,
}

impl Area {
    fn random() -> Area {
        let mut rng = rand::thread_rng();
        let round = |x: f64| (x * 1e4).round() / 1e4;
        Area {
            lat: round(-48.0 + rng.gen_range(0.0..0.5)),
            lon: round(-123.0 + rng.gen_range(0.0..0.5)),
        }
    }
}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-7
}

fn changeset_url(id: u64) -> String {
    format!("{API_BASE}/changeset/{id}")
}

/// An upload that did not go through.
struct UploadFailed {
    message: String,
    error: Option<UploadError>,
    /// The changeset that was opened for the failed attempt, if any.
    changeset: Option<u64>,
}

impl UploadFailed {
    fn plain(message: impl Into<String>) -> Self {
        UploadFailed {
            message: message.into(),
            error: None,
            changeset: None,
        }
    }
}

impl From<UploadFailed> for String {
    fn from(f: UploadFailed) -> String {
        match f.changeset {
            Some(id) => format!("upload failed: {} ({})", f.message, changeset_url(id)),
            None => format!("upload failed: {}", f.message),
        }
    }
}

struct Uploaded {
    completed: Vec<CompletedChunk>,
    merged: UploadResult,
}

struct Ctx<'a> {
    rec: &'a Client,
    token_path: &'a std::path::Path,
    scenario: &'static str,
    area: Area,
    /// The server's changeset size limit, as the app reads it.
    max: usize,
    /// Every changeset opened by this scenario, in order.
    changesets: Vec<u64>,
    /// Every server object this scenario saw a layer hold, for cleanup.
    tracked: Vec<(FeatureKind, i64)>,
    notes: Vec<String>,
}

impl Ctx<'_> {
    fn client(&self) -> &dyn HttpClient {
        self.rec
    }

    fn comment(&self) -> String {
        format!("osm-gpui automated live test ({})", self.scenario)
    }

    fn track(&mut self, layer: &OsmLayer) {
        let Some(data) = layer.get_osm_data() else {
            return;
        };
        let mut found: Vec<(FeatureKind, i64)> = Vec::new();
        found.extend(data.nodes.keys().map(|&id| (FeatureKind::Node, id)));
        found.extend(data.ways.keys().map(|&id| (FeatureKind::Way, id)));
        found.extend(data.relations.iter().map(|r| (FeatureKind::Relation, r.id)));
        for item in found {
            if item.1 > 0 && !self.tracked.contains(&item) {
                self.tracked.push(item);
            }
        }
    }

    /// Upload everything `layer` has pending, the way the app does: plan the
    /// chunks, then for each one refresh the token, send it in a changeset of
    /// its own, absorb its ids into the queue and apply it to the layer before
    /// the next is handed out. A diff is sent once and never retried.
    fn upload(&mut self, layer: &mut OsmLayer, max: usize) -> Result<Uploaded, UploadFailed> {
        let diff = layer.diff_for_upload();
        let total = elements(&diff);
        if total == 0 {
            return Err(UploadFailed::plain("the layer has nothing pending"));
        }
        let mut queue =
            ChunkQueue::plan(&[&diff], max).map_err(|e| UploadFailed::plain(e.to_string()))?;
        drop(diff);

        let comment = self.comment();
        let mut completed: Vec<CompletedChunk> = Vec::new();
        let mut applied = 0usize;
        while let Some((part, chunk)) = queue.next_chunk() {
            let token = dev_live::fresh_token(self.client(), self.token_path)
                .map_err(UploadFailed::plain)?;
            match upload_chunk(
                self.client(),
                API_BASE,
                &token.access_token,
                &comment,
                part,
                chunk,
                &mut |_, _| {},
            ) {
                Ok(mut done) => {
                    done.last = queue.remaining() == 0;
                    queue.accept(&done.result);
                    self.changesets.push(done.changeset_id);
                    apply_to_layer(layer, &done);
                    self.rec.note(format!("applied {}", done.changeset_id));
                    self.track(layer);

                    applied += elements(&done.chunk);
                    let pending = elements(&layer.diff_for_upload());
                    if pending != total - applied {
                        return Err(UploadFailed {
                            message: format!(
                                "after changeset {} the layer has {} pending elements, expected {}",
                                done.changeset_id,
                                pending,
                                total - applied
                            ),
                            error: None,
                            changeset: Some(done.changeset_id),
                        });
                    }
                    completed.push(done);
                }
                Err(failure) => {
                    if let Some(id) = failure.changeset_id {
                        self.changesets.push(id);
                    }
                    return Err(UploadFailed {
                        message: failure.error.to_string(),
                        changeset: failure.changeset_id,
                        error: Some(failure.error),
                    });
                }
            }
        }
        let merged = merge_results(&completed);
        Ok(Uploaded { completed, merged })
    }

    /// `upload` with the server's own changeset limit.
    fn upload_all(&mut self, layer: &mut OsmLayer) -> Result<Uploaded, String> {
        let max = self.max;
        self.upload(layer, max).map_err(String::from)
    }

    /// Upload expecting the server to refuse with a conflict. Returns the
    /// conflict and the changeset opened for the refused attempt.
    fn upload_conflict(&mut self, layer: &mut OsmLayer) -> Result<(UploadConflict, u64), String> {
        let max = self.max;
        match self.upload(layer, max) {
            Ok(_) => Err("expected the server to refuse the upload, but it was accepted".into()),
            Err(UploadFailed {
                error: Some(UploadError::Conflict(conflict)),
                changeset: Some(id),
                ..
            }) => Ok((conflict, id)),
            Err(other) => Err(format!("expected a conflict, got: {}", String::from(other))),
        }
    }

    fn get_text(&self, path: &str) -> Result<String, String> {
        let req = HttpRequest::get(format!("{API_BASE}{path}"));
        fetch_with_retries(self.client(), &req, &RetryPolicy::standard())
            .map_err(|e| format!("GET {path}: {e}"))?
            .into_string()
            .map_err(|e| format!("GET {path}: {e}"))
    }

    fn fetch(&self, kind: FeatureKind, id: i64, full: bool) -> Result<ServerElement, String> {
        osm_api::fetch_element_with(self.client(), API_BASE, None, kind, id, full)
            .map_err(|e| format!("GET {kind:?} {id}: {e}"))
    }

    fn server_node(&self, id: i64) -> Result<OsmNode, String> {
        match self.fetch(FeatureKind::Node, id, false)? {
            ServerElement::Present(d) => d
                .nodes
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("node {id}: not in the server's answer")),
            ServerElement::Gone => Err(format!("node {id} is deleted on the server")),
        }
    }

    fn server_way(&self, id: i64) -> Result<osm_gpui::OsmWay, String> {
        match self.fetch(FeatureKind::Way, id, false)? {
            ServerElement::Present(d) => d
                .ways
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("way {id}: not in the server's answer")),
            ServerElement::Gone => Err(format!("way {id} is deleted on the server")),
        }
    }

    fn server_relation(&self, id: i64) -> Result<osm_gpui::OsmRelation, String> {
        match self.fetch(FeatureKind::Relation, id, false)? {
            ServerElement::Present(d) => d
                .relations
                .iter()
                .find(|r| r.id == id)
                .cloned()
                .ok_or_else(|| format!("relation {id}: not in the server's answer")),
            ServerElement::Gone => Err(format!("relation {id} is deleted on the server")),
        }
    }

    fn ensure_gone(&self, kind: FeatureKind, id: i64) -> Result<(), String> {
        match self.fetch(kind, id, false)? {
            ServerElement::Gone => Ok(()),
            ServerElement::Present(_) => Err(format!("{kind:?} {id} is still on the server")),
        }
    }

    fn ensure_changeset_closed(&self, id: u64) -> Result<(), String> {
        let body = self.get_text(&format!("/api/0.6/changeset/{id}"))?;
        match dev_live::changeset_is_open(&body) {
            Some(false) => Ok(()),
            Some(true) => Err(format!("changeset {id} is still open")),
            None => Err(format!("changeset {id}: no open flag in the answer")),
        }
    }

    /// Delete whatever this scenario left on the server. Rebuilds the objects
    /// from the server's current copies (so versions are right whoever
    /// changed them last) and uploads their deletion with the real code.
    fn cleanup(&mut self) -> Result<(), String> {
        let mut data = OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        for (kind, id) in self.tracked.clone() {
            if let ServerElement::Present(found) = self.fetch(kind, id, false)? {
                data.nodes.extend(found.nodes);
                data.ways.extend(found.ways);
                data.relations.extend(found.relations);
            }
        }
        if data.nodes.is_empty() && data.ways.is_empty() && data.relations.is_empty() {
            return Ok(());
        }
        let relation_ids: Vec<i64> = data.relations.iter().map(|r| r.id).collect();
        let way_ids: Vec<i64> = data.ways.keys().copied().collect();
        let node_ids: Vec<i64> = data.nodes.keys().copied().collect();
        let mut layer = OsmLayer::new_with_data(LayerId(99), "cleanup", Arc::new(data));
        for id in relation_ids {
            ensure!(
                layer.delete_relation(id).is_some(),
                "cleanup: could not delete relation {id} locally"
            );
        }
        for id in way_ids {
            ensure!(
                layer.delete_feature(FeatureKind::Way, id).is_some(),
                "cleanup: could not delete way {id} locally"
            );
        }
        for id in node_ids {
            ensure!(
                layer.delete_feature(FeatureKind::Node, id).is_some(),
                "cleanup: could not delete node {id} locally"
            );
        }
        self.upload_all(&mut layer).map(|_| ())
    }
}

fn elements(diff: &LayerDiff) -> usize {
    let (created, modified, deleted) = diff.counts();
    created + modified + deleted
}

fn new_layer(id: u64) -> OsmLayer {
    let empty = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    OsmLayer::new_with_data(LayerId(id), "live test", Arc::new(empty))
}

fn tag_map(tags: &[(String, String)]) -> HashMap<String, String> {
    tags.iter().cloned().collect()
}

/// Tag values that exercise XML escaping: every special character, text that
/// looks like an entity (to catch double escaping) and non-ASCII text.
fn nasty_tags(what: &str) -> Tags {
    vec![
        (
            "name".to_string(),
            format!("{what} & Co <\"Caf\u{e9}\"> \u{2014} \u{65e5}\u{672c}\u{8a9e} \u{2615}"),
        ),
        (
            "note".to_string(),
            "literal &amp; and &lt;b&gt; and &#38; stay text".to_string(),
        ),
        (
            "description".to_string(),
            "a > b && c < d, \"double\" and 'single'".to_string(),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The shared fixture: two nodes, a way over them and a relation holding the way
// ---------------------------------------------------------------------------

struct Fixture {
    n1: i64,
    n2: i64,
    way: i64,
    rel: i64,
    n1_tags: Tags,
    n2_tags: Tags,
    way_tags: Tags,
    rel_tags: Tags,
    role: String,
    n1_pos: (f64, f64),
    n2_pos: (f64, f64),
}

/// Server ids of a fixture after its upload.
struct Ids {
    n1: i64,
    n2: i64,
    way: i64,
    rel: i64,
}

fn build_fixture(layer: &mut OsmLayer, area: Area) -> Result<Fixture, String> {
    let n1_pos = (area.lat, area.lon);
    let n2_pos = (area.lat + 0.0001, area.lon + 0.0001);
    let n1_tags = nasty_tags("node one");
    let n2_tags = nasty_tags("node two");
    let way_tags = {
        let mut t = nasty_tags("way");
        t.push(("highway".to_string(), "path".to_string()));
        t
    };
    let rel_tags = {
        let mut t = nasty_tags("relation");
        t.push(("type".to_string(), "route".to_string()));
        t.push(("route".to_string(), "ferry".to_string()));
        t
    };
    let role = "main & <ferry> \"x\"".to_string();

    let n1 = layer.add_node(n1_pos.0, n1_pos.1);
    let n2 = layer.add_node(n2_pos.0, n2_pos.1);
    for (k, v) in &n1_tags {
        layer.set_tag(FeatureKind::Node, n1, k, v);
    }
    for (k, v) in &n2_tags {
        layer.set_tag(FeatureKind::Node, n2, k, v);
    }
    let way = layer.add_way(vec![n1, n2], way_tags.clone());
    let rel = layer
        .create_relation(
            rel_tags.clone(),
            vec![OsmMember {
                member_type: MemberType::Way,
                reference: way,
                role: role.clone(),
            }],
        )
        .ok_or("could not create the relation locally")?;
    Ok(Fixture {
        n1,
        n2,
        way,
        rel,
        n1_tags,
        n2_tags,
        way_tags,
        rel_tags,
        role,
        n1_pos,
        n2_pos,
    })
}

/// Check what the upload of a fresh fixture did locally: the placeholders
/// became positive server ids at version 1 and nothing is pending.
fn check_fixture_applied(layer: &OsmLayer, fx: &Fixture, up: &Uploaded) -> Result<Ids, String> {
    let get = |map: &HashMap<i64, (i64, i32)>, placeholder: i64, what: &str| {
        map.get(&placeholder)
            .copied()
            .ok_or_else(|| format!("the diff result has no answer for {what} {placeholder}"))
    };
    let (n1, v1) = get(&up.merged.node_id_remap, fx.n1, "node")?;
    let (n2, v2) = get(&up.merged.node_id_remap, fx.n2, "node")?;
    let (way, vw) = get(&up.merged.way_id_remap, fx.way, "way")?;
    let (rel, vr) = get(&up.merged.relation_id_remap, fx.rel, "relation")?;
    for (what, id, version) in [
        ("node 1", n1, v1),
        ("node 2", n2, v2),
        ("way", way, vw),
        ("relation", rel, vr),
    ] {
        ensure!(id > 0, "{what} got id {id}, expected a positive server id");
        ensure!(
            version == 1,
            "{what} came back as version {version}, expected 1"
        );
    }

    let data = layer.get_osm_data().ok_or("the layer lost its data")?;
    ensure!(
        data.nodes.keys().all(|&id| id > 0)
            && data.ways.keys().all(|&id| id > 0)
            && data.relations.iter().all(|r| r.id > 0),
        "the layer still holds a local (negative) id after the upload"
    );
    ensure!(
        data.nodes.len() == 2 && data.ways.len() == 1 && data.relations.len() == 1,
        "the layer holds {} nodes, {} ways, {} relations, expected 2, 1, 1",
        data.nodes.len(),
        data.ways.len(),
        data.relations.len()
    );
    for (what, version) in [
        ("node 1", data.nodes.get(&n1).map(|n| n.version)),
        ("node 2", data.nodes.get(&n2).map(|n| n.version)),
        ("way", data.ways.get(&way).map(|w| w.version)),
        (
            "relation",
            data.relations
                .iter()
                .find(|r| r.id == rel)
                .map(|r| r.version),
        ),
    ] {
        ensure!(
            version == Some(1),
            "{what} is at version {version:?} in the layer, expected Some(1)"
        );
    }
    ensure!(
        data.ways[&way].nodes == vec![n1, n2],
        "the layer's way has nodes {:?}, expected {:?}",
        data.ways[&way].nodes,
        vec![n1, n2]
    );
    let rel_local = data.relations.iter().find(|r| r.id == rel).unwrap();
    ensure!(
        rel_local.members.len() == 1
            && rel_local.members[0].member_type == MemberType::Way
            && rel_local.members[0].reference == way,
        "the layer's relation members were not remapped: {:?}",
        rel_local.members
    );
    ensure!(
        elements(&layer.diff_for_upload()) == 0,
        "the layer still has pending edits after the upload"
    );
    Ok(Ids { n1, n2, way, rel })
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Scenario `roundtrip`: create two nodes, a way and a relation with tricky tag
/// text; compare the server's copies with what was sent.
fn roundtrip(ctx: &mut Ctx) -> Result<(), String> {
    let mut layer = new_layer(1);
    let fx = build_fixture(&mut layer, ctx.area)?;
    let up = ctx.upload_all(&mut layer)?;
    ensure!(
        up.completed.len() == 1,
        "expected one changeset, got {}",
        up.completed.len()
    );
    let ids = check_fixture_applied(&layer, &fx, &up)?;

    let n1 = ctx.server_node(ids.n1)?;
    let n2 = ctx.server_node(ids.n2)?;
    let way = ctx.server_way(ids.way)?;
    let rel = ctx.server_relation(ids.rel)?;
    for (what, version) in [
        ("node 1", n1.version),
        ("node 2", n2.version),
        ("way", way.version),
        ("relation", rel.version),
    ] {
        ensure!(
            version == 1,
            "server has {what} at version {version}, expected 1"
        );
    }
    ensure!(
        close(n1.lat, fx.n1_pos.0) && close(n1.lon, fx.n1_pos.1),
        "node 1 is at ({}, {}) on the server, sent ({}, {})",
        n1.lat,
        n1.lon,
        fx.n1_pos.0,
        fx.n1_pos.1
    );
    ensure!(
        close(n2.lat, fx.n2_pos.0) && close(n2.lon, fx.n2_pos.1),
        "node 2 is at ({}, {}) on the server, sent ({}, {})",
        n2.lat,
        n2.lon,
        fx.n2_pos.0,
        fx.n2_pos.1
    );
    for (what, got, sent) in [
        ("node 1", &n1.tags, &fx.n1_tags),
        ("node 2", &n2.tags, &fx.n2_tags),
        ("way", &way.tags, &fx.way_tags),
        ("relation", &rel.tags, &fx.rel_tags),
    ] {
        ensure!(
            *got == tag_map(sent),
            "{what} tags differ (double escaping?)\n  sent: {:?}\n  got:  {:?}",
            tag_map(sent),
            got
        );
    }
    ensure!(
        way.nodes == vec![ids.n1, ids.n2],
        "server way nodes {:?}, expected {:?}",
        way.nodes,
        vec![ids.n1, ids.n2]
    );
    ensure!(
        rel.members.len() == 1
            && rel.members[0].member_type == MemberType::Way
            && rel.members[0].reference == ids.way
            && rel.members[0].role == fx.role,
        "server relation members {:?}, expected the way {} with role {:?}",
        rel.members,
        ids.way,
        fx.role
    );
    ctx.notes.push(format!(
        "node {} node {} way {} relation {}",
        ids.n1, ids.n2, ids.way, ids.rel
    ));
    Ok(())
}

/// Scenario `modify_delete`: modify a way's tag and move a node, then delete everything.
fn modify_delete(ctx: &mut Ctx) -> Result<(), String> {
    let mut layer = new_layer(1);
    let fx = build_fixture(&mut layer, ctx.area)?;
    let up = ctx.upload_all(&mut layer)?;
    let ids = check_fixture_applied(&layer, &fx, &up)?;

    let new_name = "changed & <again> \u{e9}".to_string();
    let moved = (fx.n1_pos.0 + 0.001, fx.n1_pos.1 + 0.001);
    layer.set_tag(FeatureKind::Way, ids.way, "name", &new_name);
    layer.commit_node_moves(&[(ids.n1, moved.0, moved.1)]);
    let counts = layer.diff_for_upload().counts();
    ensure!(
        counts == (0, 2, 0),
        "expected 2 modifications (the way, node 1), the layer has {counts:?}"
    );
    ctx.upload_all(&mut layer)?;

    let data = layer.get_osm_data().ok_or("the layer lost its data")?;
    let local = [
        ("node 1", data.nodes[&ids.n1].version, 2),
        ("node 2", data.nodes[&ids.n2].version, 1),
        ("way", data.ways[&ids.way].version, 2),
        (
            "relation",
            data.relations
                .iter()
                .find(|r| r.id == ids.rel)
                .unwrap()
                .version,
            1,
        ),
    ];
    for (what, got, want) in local {
        ensure!(
            got == want,
            "layer has {what} at version {got}, expected {want}"
        );
    }
    let n1 = ctx.server_node(ids.n1)?;
    let n2 = ctx.server_node(ids.n2)?;
    let way = ctx.server_way(ids.way)?;
    let rel = ctx.server_relation(ids.rel)?;
    ensure!(
        (n1.version, n2.version, way.version, rel.version) == (2, 1, 2, 1),
        "server versions (node 1, node 2, way, relation) are {:?}, expected (2, 1, 2, 1)",
        (n1.version, n2.version, way.version, rel.version)
    );
    ensure!(
        close(n1.lat, moved.0) && close(n1.lon, moved.1),
        "node 1 is at ({}, {}) on the server, expected ({}, {})",
        n1.lat,
        n1.lon,
        moved.0,
        moved.1
    );
    ensure!(
        way.tags.get("name") == Some(&new_name),
        "server way name is {:?}, expected {:?}",
        way.tags.get("name"),
        new_name
    );

    // Delete it all (relation, then way, then nodes, as the app's user would).
    ensure!(
        layer.delete_relation(ids.rel).is_some(),
        "could not delete the relation locally"
    );
    ensure!(
        layer.delete_feature(FeatureKind::Way, ids.way).is_some(),
        "could not delete the way locally"
    );
    for id in [ids.n1, ids.n2] {
        ensure!(
            layer.delete_feature(FeatureKind::Node, id).is_some(),
            "could not delete node {id} locally"
        );
    }
    let counts = layer.diff_for_upload().counts();
    ensure!(counts == (0, 0, 4), "expected 4 deletions, got {counts:?}");
    ctx.upload_all(&mut layer)?;
    let data = layer.get_osm_data().ok_or("the layer lost its data")?;
    ensure!(
        data.nodes.is_empty() && data.ways.is_empty() && data.relations.is_empty(),
        "the layer still holds objects after deleting them"
    );

    ctx.ensure_gone(FeatureKind::Relation, ids.rel)?;
    ctx.ensure_gone(FeatureKind::Way, ids.way)?;
    ctx.ensure_gone(FeatureKind::Node, ids.n1)?;
    ctx.ensure_gone(FeatureKind::Node, ids.n2)?;
    let history = ctx.get_text(&format!("/api/0.6/node/{}/history", ids.n1))?;
    ensure!(
        history.contains("visible=\"false\""),
        "node {}'s history has no deleted version",
        ids.n1
    );
    Ok(())
}

/// A second, independent client of the server: a layer that starts from what
/// `layer` holds now and uploads its own edits.
fn second_party(layer: &OsmLayer) -> Result<OsmLayer, String> {
    let data = layer.get_osm_data().ok_or("the layer lost its data")?;
    Ok(OsmLayer::new_with_data(LayerId(2), "second party", data))
}

/// Scenario `conflict_version`: refused with the right kind and object,
/// changeset closed, then both resolutions.
fn conflict_version(ctx: &mut Ctx) -> Result<(), String> {
    let mut layer = new_layer(1);
    let id = layer.add_node(ctx.area.lat, ctx.area.lon);
    layer.set_tag(FeatureKind::Node, id, "name", "conflict & <test>");
    let up = ctx.upload_all(&mut layer)?;
    let (id, version) = up.merged.node_id_remap[&id];
    ensure!(
        id > 0 && version == 1,
        "created node came back as {id} v{version}"
    );

    // Someone else saves it behind the layer's back.
    let mut other = second_party(&layer)?;
    other.set_tag(FeatureKind::Node, id, "name", "theirs v2");
    ctx.upload_all(&mut other)?;
    ensure!(
        ctx.server_node(id)?.version == 2,
        "second party's edit did not reach v2"
    );

    // Our copy is stale (v1): the edit is refused.
    layer.set_tag(FeatureKind::Node, id, "name", "mine");
    let (conflict, changeset) = ctx.upload_conflict(&mut layer)?;
    ensure!(
        conflict.kind
            == ConflictKind::VersionMismatch {
                provided: 1,
                server: 2
            }
            && conflict.object == Some((FeatureKind::Node, id)),
        "unexpected conflict: {:?} on {:?} ({})",
        conflict.kind,
        conflict.object,
        conflict.message
    );
    ctx.notes
        .push(format!("version mismatch was HTTP {}", conflict.status));
    ctx.ensure_changeset_closed(changeset)?;

    // Use theirs: the local copy becomes the server's.
    let server = ctx.fetch(FeatureKind::Node, id, true)?;
    ensure!(
        layer.adopt_server_element(FeatureKind::Node, id, server),
        "use theirs changed nothing"
    );
    let local = layer
        .get_osm_data()
        .and_then(|d| d.nodes.get(&id).cloned())
        .ok_or("the node vanished from the layer")?;
    ensure!(
        local == ctx.server_node(id)?,
        "after use theirs the local node {local:?} differs from the server's"
    );
    ensure!(
        elements(&layer.diff_for_upload()) == 0,
        "use theirs left an edit pending"
    );

    // Same setup again, now resolved with keep mine.
    other.set_tag(FeatureKind::Node, id, "name", "theirs v3");
    ctx.upload_all(&mut other)?;
    layer.set_tag(FeatureKind::Node, id, "name", "mine again");
    let (conflict, changeset) = ctx.upload_conflict(&mut layer)?;
    ensure!(
        conflict.kind
            == ConflictKind::VersionMismatch {
                provided: 2,
                server: 3
            }
            && conflict.object == Some((FeatureKind::Node, id)),
        "unexpected second conflict: {:?} on {:?} ({})",
        conflict.kind,
        conflict.object,
        conflict.message
    );
    ctx.ensure_changeset_closed(changeset)?;

    let server = ctx.fetch(FeatureKind::Node, id, false)?;
    let server_version = server
        .version(FeatureKind::Node, id)
        .ok_or("the server's node has no version")?;
    ensure!(
        server_version == 3,
        "server node is at v{server_version}, expected 3"
    );
    ensure!(
        layer.set_base_version(FeatureKind::Node, id, server_version),
        "keep mine did not know the node"
    );
    ctx.upload_all(&mut layer)?;
    let local = layer.get_osm_data().and_then(|d| d.nodes.get(&id).cloned());
    ensure!(
        local.as_ref().map(|n| n.version) == Some(4),
        "after keep mine the local node is {local:?}, expected version 4"
    );
    let server = ctx.server_node(id)?;
    ensure!(
        server.version == 4 && server.tags.get("name").map(String::as_str) == Some("mine again"),
        "server node after keep mine is v{} {:?}, expected v4 \"mine again\"",
        server.version,
        server.tags.get("name")
    );
    Ok(())
}

/// Scenario `conflict_deleted`: the object is deleted on the server before the stale edit arrives.
fn conflict_deleted(ctx: &mut Ctx) -> Result<(), String> {
    let mut layer = new_layer(1);
    let a = layer.add_node(ctx.area.lat, ctx.area.lon);
    let b = layer.add_node(ctx.area.lat + 0.0001, ctx.area.lon);
    let up = ctx.upload_all(&mut layer)?;
    let (a, _) = up.merged.node_id_remap[&a];
    let (b, _) = up.merged.node_id_remap[&b];

    // The server loses both nodes behind the layer's back.
    let mut other = second_party(&layer)?;
    for id in [a, b] {
        ensure!(
            other.delete_feature(FeatureKind::Node, id).is_some(),
            "second party could not delete node {id}"
        );
    }
    ctx.upload_all(&mut other)?;
    ctx.ensure_gone(FeatureKind::Node, a)?;

    // A stale modify of a deleted object.
    layer.set_tag(FeatureKind::Node, a, "name", "edited after deletion");
    let (conflict, changeset) = ctx.upload_conflict(&mut layer)?;
    ensure!(
        conflict.kind == ConflictKind::AlreadyDeleted
            && conflict.object == Some((FeatureKind::Node, a)),
        "unexpected conflict: {:?} on {:?} ({})",
        conflict.kind,
        conflict.object,
        conflict.message
    );
    ctx.notes.push(format!(
        "modify of a deleted node was HTTP {}",
        conflict.status
    ));
    ctx.ensure_changeset_closed(changeset)?;
    // The app offers no choice here; the server's answer (deleted) is
    // adopted, which removes the object and the edit.
    let server = ctx.fetch(FeatureKind::Node, a, true)?;
    ensure!(
        server == ServerElement::Gone,
        "the server still has node {a}"
    );
    ensure!(
        layer.adopt_server_element(FeatureKind::Node, a, server),
        "adopting the deletion changed nothing"
    );
    ensure!(
        elements(&layer.diff_for_upload()) == 0,
        "an edit is still pending after adopting the deletion"
    );

    // Both sides deleted node b: "Drop my change".
    ensure!(
        layer.delete_feature(FeatureKind::Node, b).is_some(),
        "could not delete node {b} locally"
    );
    let (conflict, changeset) = ctx.upload_conflict(&mut layer)?;
    ensure!(
        conflict.kind == ConflictKind::AlreadyDeleted
            && conflict.object == Some((FeatureKind::Node, b)),
        "unexpected conflict on the double delete: {:?} on {:?} ({})",
        conflict.kind,
        conflict.object,
        conflict.message
    );
    ctx.notes.push(format!(
        "delete of a deleted node was HTTP {}",
        conflict.status
    ));
    ctx.ensure_changeset_closed(changeset)?;
    ensure!(
        layer.is_locally_deleted(FeatureKind::Node, b),
        "the layer does not see node {b} as locally deleted"
    );
    ensure!(
        layer.forget_local_delete(FeatureKind::Node, b),
        "drop my change did nothing"
    );
    ensure!(
        elements(&layer.diff_for_upload()) == 0,
        "an edit is still pending after drop my change"
    );
    Ok(())
}

/// Scenario `chunked`: a multi-changeset upload with a small chunk limit.
fn chunked(ctx: &mut Ctx) -> Result<(), String> {
    const NODES: usize = 60;
    let mut layer = new_layer(1);
    let base = ctx.area;
    let nodes: Vec<i64> = (0..NODES)
        .map(|i| {
            layer.add_node(
                base.lat + 0.0001 * (i / 10) as f64,
                base.lon + 0.0001 * (i % 10) as f64,
            )
        })
        .collect();
    // Way 1 uses nodes 0..20. Way 2 reuses nodes 10..20 and adds nodes
    // 40..45; the planner puts nodes right before the first way that uses
    // them, so the second way lands in the second changeset while most of
    // its nodes were created in the first.
    let first: Vec<i64> = nodes[0..20].to_vec();
    let mut second: Vec<i64> = nodes[10..20].to_vec();
    second.extend_from_slice(&nodes[40..45]);
    let way_tags = |what: &str| {
        let mut t = nasty_tags(what);
        t.push(("highway".to_string(), "path".to_string()));
        t
    };
    let way1 = layer.add_way(first.clone(), way_tags("way one"));
    let way2 = layer.add_way(second.clone(), way_tags("way two"));
    let before = layer.get_osm_data().ok_or("the layer lost its data")?;

    // The plan must really depend on ids from an earlier changeset.
    let diff = layer.diff_for_upload();
    let planned = plan_chunks(&[&diff], CHUNKED_LIMIT).map_err(|e| e.to_string())?;
    ensure!(
        planned.len() == 3,
        "expected 3 planned changesets for {} elements at limit {CHUNKED_LIMIT}, got {}",
        elements(&diff),
        planned.len()
    );
    let in_first: Vec<i64> = planned[0].created_nodes.iter().map(|n| n.id).collect();
    let cross = planned[1]
        .created_ways
        .iter()
        .any(|w| w.nodes.iter().any(|n| in_first.contains(n)));
    ensure!(
        cross,
        "no way in the second changeset uses a node of the first; the scenario no longer tests the id map"
    );
    drop(diff);

    let up = ctx
        .upload(&mut layer, CHUNKED_LIMIT)
        .map_err(String::from)?;
    ensure!(
        up.completed.len() == 3,
        "expected 3 changesets, got {}",
        up.completed.len()
    );
    let ids: Vec<u64> = up.completed.iter().map(|c| c.changeset_id).collect();
    ensure!(
        ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
        "changesets are not distinct: {ids:?}"
    );

    // Each chunk was applied to the layer before the next changeset opened.
    let events: Vec<String> = ctx
        .rec
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| *e == "create" || e.starts_with("applied "))
        .cloned()
        .collect();
    let want: Vec<String> = ids
        .iter()
        .flat_map(|id| ["create".to_string(), format!("applied {id}")])
        .collect();
    ensure!(
        events == want,
        "request order was {events:?}, expected {want:?}"
    );
    for id in &ids {
        ctx.ensure_changeset_closed(*id)?;
    }

    // Everything is on the server with the right references.
    let remap = &up.merged.node_id_remap;
    let real = |p: i64| {
        remap
            .get(&p)
            .map(|&(n, _)| n)
            .ok_or(format!("node {p} was not answered"))
    };
    let node_ids: Vec<i64> = nodes.iter().map(|&p| real(p)).collect::<Result<_, _>>()?;
    ensure!(
        node_ids.iter().all(|&id| id > 0),
        "a node kept a local id: {node_ids:?}"
    );
    let list = |ids: &[i64]| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let body = ctx.get_text(&format!("/api/0.6/nodes?nodes={}", list(&node_ids)))?;
    let on_server = OsmParser::new()
        .parse_str(&body)
        .map_err(|e| e.to_string())?;
    for id in &node_ids {
        ensure!(
            on_server.nodes.contains_key(id),
            "node {id} is not on the server"
        );
    }
    for (placeholder, what) in [(way1, "way one"), (way2, "way two")] {
        let (way_id, version) = up
            .merged
            .way_id_remap
            .get(&placeholder)
            .copied()
            .ok_or(format!("{what} was not answered"))?;
        ensure!(
            way_id > 0 && version == 1,
            "{what} came back as {way_id} v{version}"
        );
        let expected: Vec<i64> = before.ways[&placeholder]
            .nodes
            .iter()
            .map(|&p| real(p))
            .collect::<Result<_, _>>()?;
        let server = ctx.server_way(way_id)?;
        ensure!(
            server.nodes == expected,
            "{what} on the server has nodes {:?}, expected {expected:?}",
            server.nodes
        );
        let local = layer
            .get_osm_data()
            .and_then(|d| d.ways.get(&way_id).cloned())
            .ok_or(format!("{what} is missing from the layer"))?;
        ensure!(
            local.nodes == expected,
            "{what} in the layer has nodes {:?}, expected {expected:?}",
            local.nodes
        );
    }
    ensure!(
        elements(&layer.diff_for_upload()) == 0,
        "the layer still has pending edits after the last changeset"
    );

    // Clean up with one more upload (one changeset at the server's limit).
    let data = layer.get_osm_data().ok_or("the layer lost its data")?;
    let way_ids: Vec<i64> = data.ways.keys().copied().collect();
    for id in way_ids {
        ensure!(
            layer.delete_feature(FeatureKind::Way, id).is_some(),
            "could not delete way {id} locally"
        );
    }
    for id in &node_ids {
        ensure!(
            layer.delete_feature(FeatureKind::Node, *id).is_some(),
            "could not delete node {id} locally"
        );
    }
    ctx.upload_all(&mut layer)?;
    ctx.ensure_gone(FeatureKind::Node, node_ids[0])?;
    ctx.ensure_gone(FeatureKind::Node, node_ids[NODES - 1])?;
    ctx.notes.push(format!(
        "{} elements in {} changesets",
        NODES + 2,
        ids.len()
    ));
    Ok(())
}

/// Scenario `capabilities`: the server's capabilities document.
fn capabilities_scenario(ctx: &mut Ctx) -> Result<(), String> {
    let body = ctx.get_text("/api/capabilities")?;
    let parsed = Capabilities::parse(&body)?;
    let fetched = capabilities::fetch_with(ctx.client(), API_BASE)?;
    ensure!(
        parsed == fetched,
        "parsing the raw document and fetching it give different limits"
    );
    for element in ["<area", "<waynodes", "<changesets"] {
        ensure!(
            body.contains(element),
            "the capabilities document has no {element} element"
        );
    }
    ensure!(
        parsed.area_maximum.is_finite() && parsed.area_maximum.is_sign_positive(),
        "area maximum is {}",
        parsed.area_maximum
    );
    ensure!(parsed.waynodes_maximum > 0, "way nodes maximum is 0");
    ensure!(
        parsed.relationmembers_maximum > 0,
        "relation members maximum is 0"
    );
    ensure!(
        parsed.changeset_maximum_elements > 0,
        "changeset elements maximum is 0"
    );
    ensure!(
        !parsed.is_offline(),
        "the server reports itself offline: {parsed:?}"
    );
    let reported = if body.contains("<relationmembers") {
        ""
    } else {
        " (relation members not reported, default shown)"
    };
    ctx.notes.push(format!(
        "area {} sq. deg., way nodes {}, relation members {}{}, changeset elements {}",
        parsed.area_maximum,
        parsed.waynodes_maximum,
        parsed.relationmembers_maximum,
        reported,
        parsed.changeset_maximum_elements
    ));
    Ok(())
}

type ScenarioFn = fn(&mut Ctx) -> Result<(), String>;

const SCENARIOS: &[(&str, ScenarioFn)] = &[
    ("roundtrip", roundtrip),
    ("modify_delete", modify_delete),
    ("conflict_version", conflict_version),
    ("conflict_deleted", conflict_deleted),
    ("chunked", chunked),
    ("capabilities", capabilities_scenario),
];

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

fn help() -> String {
    let names: Vec<&str> = SCENARIOS.iter().map(|(n, _)| *n).collect();
    format!(
        "dev_live: run the app's upload code against the OSM DEVELOPMENT server\n\
         ({API_BASE}). It cannot talk to any other host.\n\
         \n\
         USAGE:\n\
         \x20   cargo run --example dev_live -- <command>\n\
         \n\
         COMMANDS:\n\
         \x20   login                 sign in to the dev server in your browser and save the\n\
         \x20                         token to the harness's own file\n\
         \x20   logout                delete that file\n\
         \x20   run [--only <name>]   run the scenarios (all, or one), print PASS/FAIL for each;\n\
         \x20                         exit status 1 if any failed\n\
         \x20   --help                this text\n\
         \n\
         SCENARIOS: {}\n\
         \n\
         The token file is {}.\n",
        names.join(", "),
        dev_live::token_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<no config directory>".to_string()),
    )
}

fn make_client() -> Client {
    Recorder {
        inner: GuardedClient::new(UreqClient::new()),
        events: Mutex::new(Vec::new()),
    }
}

fn cmd_login() -> Result<(), String> {
    let path = dev_live::token_path().ok_or("no config directory on this system")?;
    let client = make_client();
    let token = dev_live::login(&client, &path)?;
    println!("Logged in to the dev server as {}", token.display_name);
    println!("Token saved to {}", path.display());
    Ok(())
}

fn cmd_logout() -> Result<(), String> {
    let path = dev_live::token_path().ok_or("no config directory on this system")?;
    match dev_live::delete_token_at(&path) {
        Ok(true) => println!("Deleted {}", path.display()),
        Ok(false) => println!("No token file at {}", path.display()),
        Err(e) => return Err(format!("could not delete {}: {e}", path.display())),
    }
    Ok(())
}

fn cmd_run(only: Option<&str>) -> Result<bool, String> {
    if let Some(name) = only {
        if !SCENARIOS.iter().any(|(n, _)| *n == name) {
            let names: Vec<&str> = SCENARIOS.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "unknown scenario {name:?}; choose one of: {}",
                names.join(", ")
            ));
        }
    }
    let path = dev_live::token_path().ok_or("no config directory on this system")?;
    let client = make_client();
    // Fails with a "run login first" message when there is no token file.
    let token = dev_live::fresh_token(&client, &path)?;
    println!(
        "Running against {API_BASE} as {} (token file {})",
        token.display_name,
        path.display()
    );

    // The same way the app learns the limits it uploads under.
    capabilities::refresh_blocking(&client, API_BASE, std::time::Instant::now());
    let max = Limits::for_server(API_BASE).changeset_elements;
    let area = Area::random();
    println!(
        "Test area around lat {}, lon {}; changeset limit {max}",
        area.lat, area.lon
    );

    let mut all_passed = true;
    for (name, run) in SCENARIOS {
        if only.is_some_and(|o| o != *name) {
            continue;
        }
        client.events.lock().unwrap().clear();
        let mut ctx = Ctx {
            rec: &client,
            token_path: &path,
            scenario: name,
            area,
            max,
            changesets: Vec::new(),
            tracked: Vec::new(),
            notes: Vec::new(),
        };
        let outcome =
            std::panic::catch_unwind(AssertUnwindSafe(|| run(&mut ctx))).unwrap_or_else(|panic| {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                Err(format!("panic: {what}"))
            });
        let cleanup = std::panic::catch_unwind(AssertUnwindSafe(|| ctx.cleanup()))
            .unwrap_or_else(|_| Err("cleanup panicked".to_string()));

        let mut failure = outcome.err();
        if let Err(e) = cleanup {
            let msg = format!("cleanup failed (objects may remain on the dev server): {e}");
            failure = Some(match failure {
                Some(f) => format!("{f}; {msg}"),
                None => msg,
            });
        }
        let changesets = if ctx.changesets.is_empty() {
            "none".to_string()
        } else {
            ctx.changesets
                .iter()
                .map(|&id| changeset_url(id))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let notes = if ctx.notes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", ctx.notes.join("; "))
        };
        match failure {
            None => println!("PASS {name}{notes} changesets: {changesets}"),
            Some(why) => {
                all_passed = false;
                println!("FAIL {name}: {why}{notes} changesets: {changesets}");
            }
        }
    }
    Ok(all_passed)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["--help"] | ["-h"] | ["help"] => {
            print!("{}", help());
            return ExitCode::SUCCESS;
        }
        ["login"] => cmd_login().map(|()| true),
        ["logout"] => cmd_logout().map(|()| true),
        ["run"] => cmd_run(None),
        ["run", "--only", name] => cmd_run(Some(name)),
        _ => {
            eprint!("{}", help());
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
