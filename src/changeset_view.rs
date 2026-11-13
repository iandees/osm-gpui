//! Inspecting one changeset: what the map looked like before it, after it, and
//! what it changed (much like osmcha).
//!
//! The server's `GET /api/0.6/changeset/#id/download` lists the elements the
//! changeset created, modified and deleted, as they were *after* it. What they
//! were before comes from each element's previous version (the API's
//! `nodes?nodes=12v3,...` multi-fetch). A way only names its nodes, so the
//! positions of nodes the changeset did not touch are taken from the server's
//! latest copy: right unless a later changeset moved them.
//!
//! Nothing here touches the UI toolkit. The result is three sets of drawable
//! shapes ([`Shapes`]) that `layers::changeset_layer` paints.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::http::{
    fetch_with_retries_progress, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};
use crate::imagery_offset::distance_m;
use crate::osm::{OsmNode, OsmWay};
use crate::tr;

/// Elements asked for in one multi-fetch (the server allows 725).
const MULTI_FETCH_CHUNK: usize = 400;
/// A node that moved less than this many metres did not move.
const MOVE_THRESHOLD_M: f64 = 0.05;

/// What a changeset did to an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Create,
    Modify,
    Delete,
}

/// How a shape is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// New (green).
    Added,
    /// Gone (red).
    Removed,
    /// Present on both sides but different (amber).
    Changed,
}

pub type LatLon = (f64, f64);

/// Everything one layer draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shapes {
    pub points: Vec<(Tint, LatLon)>,
    pub lines: Vec<(Tint, Vec<LatLon>)>,
}

impl Shapes {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.lines.is_empty()
    }

    /// `(min_lat, max_lat, min_lon, max_lon)` of everything, if anything.
    pub fn extent(&self) -> Option<(f64, f64, f64, f64)> {
        let mut it = self
            .points
            .iter()
            .map(|(_, p)| *p)
            .chain(self.lines.iter().flat_map(|(_, l)| l.iter().copied()));
        let first = it.next()?;
        Some(it.fold(
            (first.0, first.0, first.1, first.1),
            |(a, b, c, d), (lat, lon)| (a.min(lat), b.max(lat), c.min(lon), d.max(lon)),
        ))
    }
}

/// Counts for the layer's statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub created: usize,
    pub modified: usize,
    pub deleted: usize,
}

/// A fetched changeset, ready to draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangesetView {
    pub id: i64,
    pub user: Option<String>,
    pub comment: Option<String>,
    pub before: Shapes,
    pub after: Shapes,
    pub changes: Shapes,
    pub nodes: Counts,
    pub ways: Counts,
    /// Relations are counted but not drawn.
    pub relations: Counts,
    /// Nodes whose position had to come from the server's latest copy.
    pub nodes_from_latest: usize,
    /// Way nodes that could not be found at all (deleted later, say).
    pub nodes_missing: usize,
}

impl ChangesetView {
    pub fn extent(&self) -> Option<(f64, f64, f64, f64)> {
        let mut all = self.changes.clone();
        all.points.extend(self.before.points.iter().cloned());
        all.points.extend(self.after.points.iter().cloned());
        all.lines.extend(self.before.lines.iter().cloned());
        all.lines.extend(self.after.lines.iter().cloned());
        all.extent()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChangesetError {
    NotFound,
    Http { status: u16 },
    Network(String),
    Parse(String),
    Cancelled,
}

impl ChangesetError {
    pub fn message(&self) -> String {
        match self {
            ChangesetError::NotFound => tr!("changeset.error.not_found").to_string(),
            ChangesetError::Http { status } => {
                tr!("changeset.error.http", status = status).to_string()
            }
            ChangesetError::Network(detail) => {
                tr!("changeset.error.network", detail = detail).to_string()
            }
            ChangesetError::Parse(detail) => {
                tr!("changeset.error.parse", detail = detail).to_string()
            }
            ChangesetError::Cancelled => tr!("changeset.error.cancelled").to_string(),
        }
    }
}

// ------------------------------------------------------------------ parsing

/// A relation, reduced to what the counts need.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationStub {
    pub id: i64,
}

/// The elements of an `osmChange` or `osm` document, each with the action it
/// sits under (`None` for a plain `osm` document).
#[derive(Debug, Clone, Default)]
pub struct Elements {
    pub nodes: Vec<(Option<Action>, OsmNode)>,
    pub ways: Vec<(Option<Action>, OsmWay)>,
    pub relations: Vec<(Option<Action>, RelationStub)>,
}

enum Building {
    Node(OsmNode, bool),
    Way(OsmWay),
    Relation(RelationStub),
}

fn attrs(e: &BytesStart) -> Result<HashMap<String, String>, ChangesetError> {
    let mut out = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|e| ChangesetError::Parse(e.to_string()))?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let value = a
            .unescape_value()
            .map_err(|e| ChangesetError::Parse(e.to_string()))?
            .into_owned();
        out.insert(key, value);
    }
    Ok(out)
}

fn num<T: std::str::FromStr>(map: &HashMap<String, String>, key: &str) -> Option<T> {
    map.get(key).and_then(|v| v.parse().ok())
}

fn begin(name: &[u8], map: &HashMap<String, String>) -> Option<Building> {
    let id: i64 = num(map, "id")?;
    let version: i32 = num(map, "version").unwrap_or(0);
    match name {
        b"node" => {
            // A deleted node may carry no position.
            let located = map.contains_key("lat") && map.contains_key("lon");
            Some(Building::Node(
                OsmNode {
                    id,
                    lat: num(map, "lat").unwrap_or(0.0),
                    lon: num(map, "lon").unwrap_or(0.0),
                    version,
                    tags: HashMap::new(),
                },
                located,
            ))
        }
        b"way" => Some(Building::Way(OsmWay {
            id,
            nodes: Vec::new(),
            version,
            tags: HashMap::new(),
        })),
        b"relation" => Some(Building::Relation(RelationStub { id })),
        _ => None,
    }
}

/// Parse an `osmChange` or `osm` document.
pub fn parse_elements(xml: &str) -> Result<Elements, ChangesetError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Elements::default();
    let mut action: Option<Action> = None;
    let mut current: Option<Building> = None;
    let mut saw_root = false;

    fn finish(out: &mut Elements, action: Option<Action>, b: Building) {
        match b {
            // A node without a position is only usable for its tags and id.
            Building::Node(n, located) => {
                let mut n = n;
                if !located {
                    n.lat = f64::NAN;
                    n.lon = f64::NAN;
                }
                out.nodes.push((action, n));
            }
            Building::Way(w) => out.ways.push((action, w)),
            Building::Relation(r) => out.relations.push((action, r)),
        }
    }

    loop {
        let event = reader
            .read_event()
            .map_err(|e| ChangesetError::Parse(e.to_string()))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let self_closing = matches!(event, Event::Empty(_));
                let name = e.name();
                let name = name.as_ref();
                match name {
                    b"osm" | b"osmChange" => saw_root = true,
                    b"create" => action = Some(Action::Create),
                    b"modify" => action = Some(Action::Modify),
                    b"delete" => action = Some(Action::Delete),
                    b"node" | b"way" | b"relation" => {
                        if let Some(b) = begin(name, &attrs(e)?) {
                            if self_closing {
                                finish(&mut out, action, b);
                            } else {
                                current = Some(b);
                            }
                        }
                    }
                    b"tag" => {
                        let m = attrs(e)?;
                        if let (Some(k), Some(v)) = (m.get("k"), m.get("v")) {
                            match current.as_mut() {
                                Some(Building::Node(n, _)) => {
                                    n.tags.insert(k.clone(), v.clone());
                                }
                                Some(Building::Way(w)) => {
                                    w.tags.insert(k.clone(), v.clone());
                                }
                                _ => {}
                            }
                        }
                    }
                    b"nd" => {
                        if let (Some(Building::Way(w)), Some(r)) =
                            (current.as_mut(), num::<i64>(&attrs(e)?, "ref"))
                        {
                            w.nodes.push(r);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match e.name().as_ref() {
                b"node" | b"way" | b"relation" => {
                    if let Some(b) = current.take() {
                        finish(&mut out, action, b);
                    }
                }
                b"create" | b"modify" | b"delete" => action = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err(ChangesetError::Parse("not an OSM document".to_string()));
    }
    Ok(out)
}

/// The user and comment of a `GET /changeset/#id` reply.
pub fn parse_meta(xml: &str) -> (Option<String>, Option<String>) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let (mut user, mut comment) = (None, None);
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let Ok(m) = attrs(&e) else { continue };
                match e.name().as_ref() {
                    b"changeset" => user = m.get("user").cloned(),
                    b"tag" if m.get("k").map(String::as_str) == Some("comment") => {
                        comment = m.get("v").cloned();
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (user, comment)
}

// ----------------------------------------------------------------- assembly

/// What [`assemble`] needs besides the changeset itself.
#[derive(Debug, Default)]
pub struct Context {
    /// Previous versions of the modified and deleted nodes.
    pub old_nodes: HashMap<i64, OsmNode>,
    /// Previous versions of the modified and deleted ways.
    pub old_ways: HashMap<i64, OsmWay>,
    /// Latest server positions of way nodes the changeset did not touch.
    pub other_nodes: HashMap<i64, LatLon>,
}

fn has_pos(n: &OsmNode) -> bool {
    n.lat.is_finite() && n.lon.is_finite()
}

/// The node ids, with the version before the changeset's, that
/// [`assemble`] wants in `Context::old_nodes` / `old_ways`.
/// `(id, version)` of an element.
pub type Versioned = (i64, i32);

pub fn previous_versions(change: &Elements) -> (Vec<Versioned>, Vec<Versioned>) {
    let prev = |action: &Option<Action>, version: i32| {
        matches!(action, Some(Action::Modify | Action::Delete)) && version > 1
    };
    (
        change
            .nodes
            .iter()
            .filter(|(a, n)| prev(a, n.version))
            .map(|(_, n)| (n.id, n.version - 1))
            .collect(),
        change
            .ways
            .iter()
            .filter(|(a, w)| prev(a, w.version))
            .map(|(_, w)| (w.id, w.version - 1))
            .collect(),
    )
}

/// Node ids the ways of the change (new and previous versions) name but whose
/// position is not known from the change or its previous versions.
pub fn unlocated_nodes(change: &Elements, old_nodes: &HashMap<i64, OsmNode>) -> Vec<i64> {
    let known: HashSet<i64> = change
        .nodes
        .iter()
        .filter(|(a, n)| *a != Some(Action::Delete) && has_pos(n))
        .map(|(_, n)| n.id)
        .chain(old_nodes.keys().copied())
        .collect();
    let mut wanted: Vec<i64> = Vec::new();
    let mut seen = HashSet::new();
    for (_, w) in &change.ways {
        for &n in &w.nodes {
            if !known.contains(&n) && seen.insert(n) {
                wanted.push(n);
            }
        }
    }
    wanted
}

/// Like [`unlocated_nodes`] but also over the previous way versions; used
/// once those are known.
pub fn unlocated_nodes_all(
    change: &Elements,
    old_nodes: &HashMap<i64, OsmNode>,
    old_ways: &HashMap<i64, OsmWay>,
) -> Vec<i64> {
    let mut wanted = unlocated_nodes(change, old_nodes);
    let known: HashSet<i64> = change
        .nodes
        .iter()
        .filter(|(a, n)| *a != Some(Action::Delete) && has_pos(n))
        .map(|(_, n)| n.id)
        .chain(old_nodes.keys().copied())
        .collect();
    let mut seen: HashSet<i64> = wanted.iter().copied().collect();
    for w in old_ways.values() {
        for &n in &w.nodes {
            if !known.contains(&n) && seen.insert(n) {
                wanted.push(n);
            }
        }
    }
    wanted
}

fn polyline(
    nodes: &[i64],
    pos: &dyn Fn(i64) -> Option<LatLon>,
    missing: &mut usize,
) -> Option<Vec<LatLon>> {
    let mut line = Vec::with_capacity(nodes.len());
    for &n in nodes {
        match pos(n) {
            Some(p) => line.push(p),
            None => *missing += 1,
        }
    }
    (line.len() >= 2).then_some(line)
}

/// Build the three sets of shapes.
pub fn assemble(id: i64, change: &Elements, ctx: &Context) -> ChangesetView {
    let mut view = ChangesetView {
        id,
        ..ChangesetView::default()
    };

    // Positions after the changeset.
    let mut after_pos: HashMap<i64, LatLon> = HashMap::new();
    for (a, n) in &change.nodes {
        if *a != Some(Action::Delete) && has_pos(n) {
            after_pos.insert(n.id, (n.lat, n.lon));
        }
    }
    // Positions before it: previous versions, else wherever the node is now.
    let mut before_pos: HashMap<i64, LatLon> = HashMap::new();
    for (id, n) in &ctx.old_nodes {
        if has_pos(n) {
            before_pos.insert(*id, (n.lat, n.lon));
        }
    }
    let after_lookup = |n: i64| -> Option<LatLon> {
        after_pos
            .get(&n)
            .or_else(|| ctx.other_nodes.get(&n))
            .copied()
    };

    // Nodes.
    for (a, n) in &change.nodes {
        let counts = &mut view.nodes;
        match a {
            Some(Action::Create) => counts.created += 1,
            Some(Action::Modify) => counts.modified += 1,
            Some(Action::Delete) => counts.deleted += 1,
            None => {}
        }
        let old = ctx.old_nodes.get(&n.id);
        let tagged = !n.tags.is_empty() || old.is_some_and(|o| !o.tags.is_empty());
        match a {
            Some(Action::Create) if has_pos(n) => {
                let p = (n.lat, n.lon);
                if tagged {
                    view.after.points.push((Tint::Added, p));
                    view.changes.points.push((Tint::Added, p));
                }
            }
            Some(Action::Modify) if has_pos(n) => {
                let p = (n.lat, n.lon);
                let old_p = old.filter(|o| has_pos(o)).map(|o| (o.lat, o.lon));
                let moved =
                    old_p.is_some_and(|o| distance_m(o.0, o.1, p.0, p.1) > MOVE_THRESHOLD_M);
                if tagged {
                    view.after.points.push((Tint::Changed, p));
                    if let Some(o) = old_p {
                        view.before.points.push((Tint::Changed, o));
                        view.changes.points.push((Tint::Removed, o));
                    }
                    view.changes.points.push((Tint::Added, p));
                }
                if moved {
                    if let Some(o) = old_p {
                        view.changes.lines.push((Tint::Changed, vec![o, p]));
                    }
                }
            }
            Some(Action::Delete) => {
                if let Some(o) = old.filter(|o| has_pos(o)) {
                    if tagged {
                        let p = (o.lat, o.lon);
                        view.before.points.push((Tint::Removed, p));
                        view.changes.points.push((Tint::Removed, p));
                    }
                }
            }
            _ => {}
        }
    }

    // Ways.
    let mut missing = 0usize;
    for (a, w) in &change.ways {
        let counts = &mut view.ways;
        match a {
            Some(Action::Create) => counts.created += 1,
            Some(Action::Modify) => counts.modified += 1,
            Some(Action::Delete) => counts.deleted += 1,
            None => {}
        }
        let new_line = (*a != Some(Action::Delete))
            .then(|| polyline(&w.nodes, &after_lookup, &mut missing))
            .flatten();
        let old_line = ctx.old_ways.get(&w.id).and_then(|ow| {
            polyline(
                &ow.nodes,
                &|n| {
                    before_pos.get(&n).copied().or_else(|| {
                        after_pos
                            .get(&n)
                            .copied()
                            .or_else(|| ctx.other_nodes.get(&n).copied())
                    })
                },
                &mut missing,
            )
        });
        match a {
            Some(Action::Create) => {
                if let Some(l) = new_line {
                    view.after.lines.push((Tint::Added, l.clone()));
                    view.changes.lines.push((Tint::Added, l));
                }
            }
            Some(Action::Modify) => {
                if let Some(l) = &new_line {
                    view.after.lines.push((Tint::Changed, l.clone()));
                }
                if let Some(l) = &old_line {
                    view.before.lines.push((Tint::Changed, l.clone()));
                }
                // Draw the old line only if it differs from the new one.
                if new_line != old_line {
                    if let Some(l) = old_line {
                        view.changes.lines.push((Tint::Removed, l));
                    }
                    if let Some(l) = new_line {
                        view.changes.lines.push((Tint::Added, l));
                    }
                } else if let Some(l) = new_line {
                    // Tags changed only.
                    view.changes.lines.push((Tint::Changed, l));
                }
            }
            Some(Action::Delete) => {
                if let Some(l) = old_line {
                    view.before.lines.push((Tint::Removed, l.clone()));
                    view.changes.lines.push((Tint::Removed, l));
                }
            }
            None => {}
        }
    }
    let mut latest = HashSet::new();
    for (_, w) in &change.ways {
        for &n in &w.nodes {
            if !after_pos.contains_key(&n) && ctx.other_nodes.contains_key(&n) {
                latest.insert(n);
            }
        }
    }
    view.nodes_from_latest = latest.len();
    view.nodes_missing = missing;

    for (a, _) in &change.relations {
        match a {
            Some(Action::Create) => view.relations.created += 1,
            Some(Action::Modify) => view.relations.modified += 1,
            Some(Action::Delete) => view.relations.deleted += 1,
            None => {}
        }
    }
    view
}

// ------------------------------------------------------------------- fetch

fn get(
    client: &dyn HttpClient,
    url: String,
    cancel: &AtomicBool,
) -> Result<String, ChangesetError> {
    let req = HttpRequest::get(url);
    let resp = fetch_with_retries_progress(client, &req, &RetryPolicy::standard(), &mut |_, _| {
        !cancel.load(Ordering::Relaxed)
    })
    .map_err(|e| match e {
        HttpError::Status { status: 404, .. } => ChangesetError::NotFound,
        HttpError::Status { status, .. } => ChangesetError::Http { status },
        HttpError::Transport(msg) => ChangesetError::Network(msg),
        HttpError::Cancelled => ChangesetError::Cancelled,
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(ChangesetError::Cancelled);
    }
    resp.into_string()
        .map_err(|e| ChangesetError::Network(e.to_string()))
}

fn base(api: &str) -> String {
    format!("{}/api/0.6", api.trim_end_matches('/'))
}

/// `{api}/api/0.6/changeset/123/download`
pub fn download_url(api: &str, id: i64) -> String {
    format!("{}/changeset/{id}/download", base(api))
}

/// `{api}/api/0.6/changeset/123`
pub fn meta_url(api: &str, id: i64) -> String {
    format!("{}/changeset/{id}", base(api))
}

/// Multi-fetch `kind` (`nodes` or `ways`) for the given specs (`12` or
/// `12v3`). A chunk the server refuses (an element in it is gone or hidden)
/// is halved until the refusing elements are down to one each, which are
/// then skipped.
fn multi_fetch(
    client: &dyn HttpClient,
    api: &str,
    kind: &str,
    specs: &[String],
    cancel: &AtomicBool,
    out: &mut Elements,
) -> Result<(), ChangesetError> {
    if specs.is_empty() {
        return Ok(());
    }
    let url = format!("{}/{kind}?{kind}={}", base(api), specs.join(","));
    match get(client, url, cancel) {
        Ok(body) => {
            let e = parse_elements(&body)?;
            out.nodes.extend(e.nodes);
            out.ways.extend(e.ways);
            Ok(())
        }
        Err(ChangesetError::NotFound | ChangesetError::Http { .. }) => {
            if specs.len() == 1 {
                return Ok(());
            }
            let (a, b) = specs.split_at(specs.len() / 2);
            multi_fetch(client, api, kind, a, cancel, out)?;
            multi_fetch(client, api, kind, b, cancel, out)
        }
        Err(e) => Err(e),
    }
}

fn fetch_all(
    client: &dyn HttpClient,
    api: &str,
    kind: &str,
    specs: Vec<String>,
    cancel: &AtomicBool,
) -> Result<Elements, ChangesetError> {
    let mut out = Elements::default();
    for chunk in specs.chunks(MULTI_FETCH_CHUNK) {
        multi_fetch(client, api, kind, chunk, cancel, &mut out)?;
    }
    Ok(out)
}

/// Fetch a changeset and build its view. Anonymous; synchronous, so call it
/// from a worker thread. Setting `cancel` abandons it between requests.
pub fn fetch_changeset_with(
    client: &dyn HttpClient,
    api: &str,
    id: i64,
    cancel: &AtomicBool,
) -> Result<ChangesetView, ChangesetError> {
    let change = parse_elements(&get(client, download_url(api, id), cancel)?)?;

    let (old_node_ids, old_way_ids) = previous_versions(&change);
    let spec = |&(id, v): &(i64, i32)| format!("{id}v{v}");
    let old_nodes: HashMap<i64, OsmNode> = fetch_all(
        client,
        api,
        "nodes",
        old_node_ids.iter().map(spec).collect(),
        cancel,
    )?
    .nodes
    .into_iter()
    .map(|(_, n)| (n.id, n))
    .collect();
    let old_ways: HashMap<i64, OsmWay> = fetch_all(
        client,
        api,
        "ways",
        old_way_ids.iter().map(spec).collect(),
        cancel,
    )?
    .ways
    .into_iter()
    .map(|(_, w)| (w.id, w))
    .collect();

    let wanted = unlocated_nodes_all(&change, &old_nodes, &old_ways);
    let other_nodes: HashMap<i64, LatLon> = fetch_all(
        client,
        api,
        "nodes",
        wanted.iter().map(|n| n.to_string()).collect(),
        cancel,
    )?
    .nodes
    .into_iter()
    .filter(|(_, n)| has_pos(n))
    .map(|(_, n)| (n.id, (n.lat, n.lon)))
    .collect();

    let mut view = assemble(
        id,
        &change,
        &Context {
            old_nodes,
            old_ways,
            other_nodes,
        },
    );
    // The description is nice to have; the changeset is shown without it.
    if let Ok(body) = get(client, meta_url(api, id), cancel) {
        (view.user, view.comment) = parse_meta(&body);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ChangesetError::Cancelled);
    }
    Ok(view)
}

/// `fetch_changeset_with` against the real network.
pub fn fetch_changeset(
    api: &str,
    id: i64,
    cancel: &AtomicBool,
) -> Result<ChangesetView, ChangesetError> {
    fetch_changeset_with(&UreqClient::new(), api, id, cancel)
}

/// A changeset number from what the user typed: `123`, `#123`, or a
/// `.../changeset/123` link.
pub fn parse_changeset_id(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = match t.find("/changeset/") {
        Some(at) => &t[at + "/changeset/".len()..],
        None => t.trim_start_matches('#'),
    };
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    const CHANGE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osmChange version="0.6">
 <create>
  <node id="-1" version="1" lat="1.0" lon="1.0"><tag k="amenity" v="cafe"/></node>
  <node id="5" version="1" lat="2.0" lon="2.0"/>
  <way id="50" version="1"><nd ref="5"/><nd ref="6"/><tag k="highway" v="path"/></way>
 </create>
 <modify>
  <node id="10" version="2" lat="3.001" lon="3.0"><tag k="shop" v="bakery"/></node>
  <way id="60" version="2"><nd ref="7"/><nd ref="10"/><tag k="highway" v="road"/></way>
 </modify>
 <delete>
  <node id="11" version="3" lat="4.0" lon="4.0"/>
  <way id="70" version="2"/>
  <relation id="80" version="2"/>
 </delete>
</osmChange>"#;

    #[test]
    fn parses_actions_tags_and_nodes() {
        let e = parse_elements(CHANGE).unwrap();
        assert_eq!(e.nodes.len(), 4);
        assert_eq!(e.ways.len(), 3);
        assert_eq!(e.relations.len(), 1);
        let (a, n) = &e.nodes[0];
        assert_eq!(*a, Some(Action::Create));
        assert_eq!(n.tags.get("amenity").map(String::as_str), Some("cafe"));
        let (a, w) = &e.ways[1];
        assert_eq!(*a, Some(Action::Modify));
        assert_eq!(w.nodes, vec![7, 10]);
        assert_eq!(e.ways[2].0, Some(Action::Delete));
    }

    #[test]
    fn rejects_non_osm_documents() {
        assert!(matches!(
            parse_elements("<html/>"),
            Err(ChangesetError::Parse(_))
        ));
    }

    #[test]
    fn previous_versions_are_asked_for_modified_and_deleted_only() {
        let e = parse_elements(CHANGE).unwrap();
        let (nodes, ways) = previous_versions(&e);
        assert_eq!(nodes, vec![(10, 1), (11, 2)]);
        assert_eq!(ways, vec![(60, 1), (70, 1)]);
    }

    #[test]
    fn unlocated_nodes_are_those_the_ways_name_but_nothing_places() {
        let e = parse_elements(CHANGE).unwrap();
        assert_eq!(unlocated_nodes(&e, &HashMap::new()), vec![6, 7]);
    }

    fn node(id: i64, lat: f64, lon: f64, version: i32) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version,
            tags: HashMap::new(),
        }
    }

    fn context() -> Context {
        let mut ctx = Context::default();
        ctx.old_nodes.insert(10, node(10, 3.0, 3.0, 1));
        ctx.old_nodes.insert(11, node(11, 4.0, 4.0, 2));
        ctx.old_ways.insert(
            60,
            OsmWay {
                id: 60,
                nodes: vec![7, 10],
                version: 1,
                tags: HashMap::new(),
            },
        );
        ctx.old_ways.insert(
            70,
            OsmWay {
                id: 70,
                nodes: vec![7, 11],
                version: 1,
                tags: HashMap::new(),
            },
        );
        ctx.other_nodes.insert(6, (2.5, 2.5));
        ctx.other_nodes.insert(7, (3.5, 3.5));
        ctx
    }

    #[test]
    fn changes_layer_shows_added_removed_and_changed() {
        let e = parse_elements(CHANGE).unwrap();
        let v = assemble(99, &e, &context());
        assert_eq!(
            v.ways,
            Counts {
                created: 1,
                modified: 1,
                deleted: 1
            }
        );
        assert_eq!(v.relations.deleted, 1);
        // Created way: green, between its two nodes.
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Added, vec![(2.0, 2.0), (2.5, 2.5)])));
        // Modified way: old line red, new line green.
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Removed, vec![(3.5, 3.5), (3.0, 3.0)])));
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Added, vec![(3.5, 3.5), (3.001, 3.0)])));
        // Deleted way: red.
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Removed, vec![(3.5, 3.5), (4.0, 4.0)])));
        // The moved, tagged node: old and new marker and the move itself.
        assert!(v.changes.points.contains(&(Tint::Removed, (3.0, 3.0))));
        assert!(v.changes.points.contains(&(Tint::Added, (3.001, 3.0))));
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Changed, vec![(3.0, 3.0), (3.001, 3.0)])));
        // The created tagged node.
        assert!(v.changes.points.contains(&(Tint::Added, (1.0, 1.0))));
    }

    #[test]
    fn before_has_only_old_geometry_and_after_only_new() {
        let e = parse_elements(CHANGE).unwrap();
        let v = assemble(99, &e, &context());
        assert!(v
            .before
            .lines
            .contains(&(Tint::Changed, vec![(3.5, 3.5), (3.0, 3.0)])));
        assert!(v
            .before
            .lines
            .contains(&(Tint::Removed, vec![(3.5, 3.5), (4.0, 4.0)])));
        assert!(!v.before.points.iter().any(|(_, p)| *p == (1.0, 1.0)));
        assert!(v
            .after
            .lines
            .contains(&(Tint::Added, vec![(2.0, 2.0), (2.5, 2.5)])));
        assert!(!v.after.lines.iter().any(|(t, _)| *t == Tint::Removed));
    }

    #[test]
    fn untagged_vertices_draw_no_marker_but_a_move_is_still_shown() {
        let xml = r#"<osmChange><modify><node id="1" version="2" lat="1.001" lon="1.0"/></modify></osmChange>"#;
        let e = parse_elements(xml).unwrap();
        let mut ctx = Context::default();
        ctx.old_nodes.insert(1, node(1, 1.0, 1.0, 1));
        let v = assemble(1, &e, &ctx);
        assert!(v.changes.points.is_empty());
        assert_eq!(v.changes.lines.len(), 1);
    }

    #[test]
    fn extent_covers_everything() {
        let e = parse_elements(CHANGE).unwrap();
        let v = assemble(99, &e, &context());
        let (min_lat, max_lat, min_lon, max_lon) = v.extent().unwrap();
        assert_eq!((min_lat, max_lat), (1.0, 4.0));
        assert_eq!((min_lon, max_lon), (1.0, 4.0));
    }

    #[test]
    fn meta_gives_user_and_comment() {
        let (user, comment) = parse_meta(
            r#"<osm><changeset id="1" user="alice"><tag k="comment" v="fix &amp; tidy"/></changeset></osm>"#,
        );
        assert_eq!(user.as_deref(), Some("alice"));
        assert_eq!(comment.as_deref(), Some("fix & tidy"));
    }

    #[test]
    fn ids_are_taken_from_numbers_and_links() {
        assert_eq!(parse_changeset_id("123"), Some(123));
        assert_eq!(parse_changeset_id(" #123 "), Some(123));
        assert_eq!(
            parse_changeset_id("https://www.openstreetmap.org/changeset/456/"),
            Some(456)
        );
        assert_eq!(parse_changeset_id("abc"), None);
        assert_eq!(parse_changeset_id("0"), None);
    }

    #[test]
    fn urls_are_built_on_the_api_base() {
        assert_eq!(
            download_url("https://api.openstreetmap.org/", 5),
            "https://api.openstreetmap.org/api/0.6/changeset/5/download"
        );
    }

    #[test]
    fn a_missing_changeset_is_not_found() {
        let client = FakeClient::new(vec![status_err(404, "")]);
        let cancel = AtomicBool::new(false);
        assert_eq!(
            fetch_changeset_with(&client, "https://x", 1, &cancel),
            Err(ChangesetError::NotFound)
        );
    }

    #[test]
    fn fetching_follows_the_documented_requests() {
        let client = FakeClient::new(vec![
            ok(200, CHANGE),
            // Old nodes 10v1, 11v2.
            ok(
                200,
                r#"<osm><node id="10" version="1" lat="3.0" lon="3.0"/><node id="11" version="2" lat="4.0" lon="4.0"/></osm>"#,
            ),
            // Old ways 60v1, 70v1.
            ok(
                200,
                r#"<osm><way id="60" version="1"><nd ref="7"/><nd ref="10"/></way><way id="70" version="1"><nd ref="7"/><nd ref="11"/></way></osm>"#,
            ),
            // Latest nodes 6 and 7.
            ok(
                200,
                r#"<osm><node id="6" version="1" lat="2.5" lon="2.5"/><node id="7" version="1" lat="3.5" lon="3.5"/></osm>"#,
            ),
            // Metadata.
            ok(
                200,
                r#"<osm><changeset id="9" user="bob"><tag k="comment" v="hi"/></changeset></osm>"#,
            ),
        ]);
        let cancel = AtomicBool::new(false);
        let v = fetch_changeset_with(&client, "https://x", 9, &cancel).unwrap();
        assert_eq!(v.user.as_deref(), Some("bob"));
        assert_eq!(v.comment.as_deref(), Some("hi"));
        assert_eq!(v.ways.modified, 1);
        assert!(v
            .changes
            .lines
            .contains(&(Tint::Removed, vec![(3.5, 3.5), (3.0, 3.0)])));
        assert_eq!(v.nodes_from_latest, 2);
    }

    #[test]
    fn a_chunk_with_a_gone_element_is_split_until_the_rest_arrive() {
        let client = FakeClient::new(vec![
            status_err(410, ""),
            ok(
                200,
                r#"<osm><node id="1" version="1" lat="1" lon="1"/></osm>"#,
            ),
            status_err(410, ""),
        ]);
        let cancel = AtomicBool::new(false);
        let mut out = Elements::default();
        multi_fetch(
            &client,
            "https://x",
            "nodes",
            &["1".to_string(), "2".to_string()],
            &cancel,
            &mut out,
        )
        .unwrap();
        assert_eq!(out.nodes.len(), 1);
    }
}
