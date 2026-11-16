//! Reverting a changeset: reading what it did, and deciding what undoing it
//! takes.
//!
//! Pure data in, pure data out — the fetching lives in `osm_api` and the
//! applying in the viewer. `OsmParser` cannot read this data: the API reports
//! a deleted object as `visible="false"` with no `lat`/`lon`, which the
//! parser rejects, and an osmChange file's `<create>`/`<modify>`/`<delete>`
//! sections matter here and are flattened away there.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::edit_ops::{EditOp, Recording};
use crate::ids::{NodeId, RelationId, WayId};
use crate::layers::{EditError, EditableLayer, LayerId};
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::selection::{FeatureKind, FeatureRef, PurgedFeatures};
use crate::tags::Tags;

/// The `<create>`, `<modify>` or `<delete>` block of an osmChange file an
/// entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Create,
    Modify,
    Delete,
}

/// A node, way or relation as it stood at one version.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Node(OsmNode),
    Way(OsmWay),
    Relation(OsmRelation),
}

/// One version of one object, as the API reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub kind: FeatureKind,
    pub id: i64,
    pub version: i32,
    /// What the object was at this version; `None` when it was deleted then.
    pub object: Option<Object>,
}

impl Entry {
    pub fn is_visible(&self) -> bool {
        self.object.is_some()
    }

    pub fn key(&self) -> (FeatureKind, i64) {
        (self.kind, self.id)
    }
}

/// Why a changeset could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn bad(what: impl std::fmt::Display) -> ParseError {
    ParseError(what.to_string())
}

/// The attributes of an element, decoded, as `(key, value)` pairs.
fn attributes(e: &BytesStart) -> Result<Vec<(String, String)>, ParseError> {
    e.attributes()
        .map(|attr| {
            let attr = attr.map_err(bad)?;
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            let value = attr.unescape_value().map_err(bad)?.into_owned();
            Ok((key, value))
        })
        .collect()
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn number<T: std::str::FromStr>(
    attrs: &[(String, String)],
    key: &str,
    on: &str,
) -> Result<T, ParseError> {
    attr(attrs, key)
        .ok_or_else(|| bad(format!("{on} has no {key}")))?
        .parse()
        .map_err(|_| bad(format!("{on} has a bad {key}")))
}

/// An element being read: its header and what its children add to it.
struct Open {
    kind: FeatureKind,
    id: i64,
    version: i32,
    visible: bool,
    lat_lon: Option<(f64, f64)>,
    tags: Tags,
    nodes: Vec<NodeId>,
    members: Vec<OsmMember>,
}

impl Open {
    fn start(kind: FeatureKind, e: &BytesStart) -> Result<Open, ParseError> {
        let attrs = attributes(e)?;
        let noun = match kind {
            FeatureKind::Node => "node",
            FeatureKind::Way => "way",
            FeatureKind::Relation => "relation",
        };
        let id: i64 = number(&attrs, "id", noun)?;
        let on = format!("{noun} {id}");
        // Objects from older servers carry no `visible`; absent means visible.
        let visible = attr(&attrs, "visible") != Some("false");
        let lat_lon = match (attr(&attrs, "lat"), attr(&attrs, "lon")) {
            (Some(_), Some(_)) => Some((number(&attrs, "lat", &on)?, number(&attrs, "lon", &on)?)),
            _ => None,
        };
        Ok(Open {
            kind,
            id,
            version: number(&attrs, "version", &on)?,
            visible,
            lat_lon,
            tags: Tags::new(),
            nodes: Vec::new(),
            members: Vec::new(),
        })
    }

    fn child(&mut self, name: &[u8], e: &BytesStart) -> Result<(), ParseError> {
        let attrs = attributes(e)?;
        match name {
            b"tag" => {
                if let (Some(k), Some(v)) = (attr(&attrs, "k"), attr(&attrs, "v")) {
                    self.tags.insert(k.to_string(), v.to_string());
                }
            }
            b"nd" => {
                let on = format!("way {}", self.id);
                self.nodes.push(NodeId(number(&attrs, "ref", &on)?));
            }
            b"member" => {
                let on = format!("relation {}", self.id);
                let member_type = attr(&attrs, "type")
                    .ok_or_else(|| bad(format!("{on} has a member without a type")))?
                    .parse::<MemberType>()
                    .map_err(bad)?;
                self.members.push(OsmMember {
                    member_type,
                    reference: number(&attrs, "ref", &on)?,
                    role: attr(&attrs, "role").unwrap_or("").to_string(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<Entry, ParseError> {
        let Open {
            kind,
            id,
            version,
            visible,
            lat_lon,
            tags,
            nodes,
            members,
        } = self;
        let object = if !visible {
            None
        } else {
            Some(match kind {
                FeatureKind::Node => {
                    let (lat, lon) = lat_lon.ok_or_else(|| {
                        bad(format!("node {id} version {version} has no position"))
                    })?;
                    Object::Node(OsmNode {
                        id: NodeId(id),
                        lat,
                        lon,
                        version,
                        tags,
                    })
                }
                FeatureKind::Way => Object::Way(OsmWay {
                    id: WayId(id),
                    nodes,
                    version,
                    tags,
                }),
                FeatureKind::Relation => Object::Relation(OsmRelation {
                    id: RelationId(id),
                    members,
                    version,
                    tags,
                }),
            })
        };
        Ok(Entry {
            kind,
            id,
            version,
            object,
        })
    }
}

fn element_kind(name: &[u8]) -> Option<FeatureKind> {
    match name {
        b"node" => Some(FeatureKind::Node),
        b"way" => Some(FeatureKind::Way),
        b"relation" => Some(FeatureKind::Relation),
        _ => None,
    }
}

fn section_of(name: &[u8]) -> Option<Section> {
    match name {
        b"create" => Some(Section::Create),
        b"modify" => Some(Section::Modify),
        b"delete" => Some(Section::Delete),
        _ => None,
    }
}

/// Every object version in `xml`, in file order, with the osmChange section it
/// sat in (`None` outside any, as in a plain `<osm>` response).
pub fn parse_entries(xml: &str) -> Result<Vec<(Option<Section>, Entry)>, ParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut section: Option<Section> = None;
    let mut open: Option<Open> = None;
    loop {
        match reader.read_event().map_err(bad)? {
            Event::Start(e) => {
                let name = e.name();
                let name = name.as_ref();
                if let Some(s) = section_of(name) {
                    section = Some(s);
                } else if let Some(kind) = element_kind(name) {
                    open = Some(Open::start(kind, &e)?);
                } else if let Some(o) = open.as_mut() {
                    o.child(name, &e)?;
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                let name = name.as_ref();
                if let Some(kind) = element_kind(name) {
                    out.push((section, Open::start(kind, &e)?.finish()?));
                } else if let Some(o) = open.as_mut() {
                    o.child(name, &e)?;
                }
            }
            Event::End(e) => {
                let name = e.name();
                let name = name.as_ref();
                if section_of(name).is_some() {
                    section = None;
                } else if element_kind(name).is_some() {
                    if let Some(o) = open.take() {
                        out.push((section, o.finish()?));
                    }
                }
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

/// What a changeset did to one object, start to finish.
#[derive(Debug, Clone, PartialEq)]
pub struct Touched {
    pub kind: FeatureKind,
    pub id: i64,
    /// The first version the changeset wrote. One less is what to go back to
    /// (nothing, when this is 1: the changeset created the object).
    pub first_version: i32,
    /// The last version the changeset wrote: what the object must still be
    /// for a revert to be safe.
    pub last: Entry,
}

impl Touched {
    /// The version just before the changeset, when there was one.
    pub fn version_before(&self) -> Option<i32> {
        (self.first_version > 1).then_some(self.first_version - 1)
    }
}

/// The objects a changeset touched, collapsed to one per object (a changeset
/// may write several versions of the same one), nodes first, then ways, then
/// relations, each by id.
pub fn touched_objects(entries: &[(Option<Section>, Entry)]) -> Vec<Touched> {
    let mut by_object: HashMap<(FeatureKind, i64), Touched> = HashMap::new();
    for (_, entry) in entries {
        by_object
            .entry(entry.key())
            .and_modify(|t| {
                t.first_version = t.first_version.min(entry.version);
                if entry.version > t.last.version {
                    t.last = entry.clone();
                }
            })
            .or_insert_with(|| Touched {
                kind: entry.kind,
                id: entry.id,
                first_version: entry.version,
                last: entry.clone(),
            });
    }
    let mut touched: Vec<Touched> = by_object.into_values().collect();
    let rank = |k: FeatureKind| match k {
        FeatureKind::Node => 0,
        FeatureKind::Way => 1,
        FeatureKind::Relation => 2,
    };
    touched.sort_by_key(|t| (rank(t.kind), t.id));
    touched
}

/// One step of undoing a changeset.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The changeset created this; delete it. `current` is what it is now.
    Delete { current: Object },
    /// The changeset changed this; put it back as `before` (an object whose
    /// `version` is the current one). `current` is what it is now.
    Restore { before: Object, current: Object },
    /// The changeset deleted this; bring it back as `before`, carrying the
    /// deleted version's number.
    Undelete { before: Object },
}

impl Step {
    pub fn key(&self) -> (FeatureKind, i64) {
        let object = match self {
            Step::Delete { current } | Step::Restore { current, .. } => current,
            Step::Undelete { before } => before,
        };
        match object {
            Object::Node(n) => (FeatureKind::Node, n.id.get()),
            Object::Way(w) => (FeatureKind::Way, w.id.get()),
            Object::Relation(r) => (FeatureKind::Relation, r.id.get()),
        }
    }
}

/// Why an object is left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Someone changed (or deleted, or undeleted) it after the changeset.
    ChangedSince { current_version: i32 },
    /// The server no longer has the object's current state, or its state
    /// before the changeset.
    Unavailable,
}

/// The outcome of planning a revert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// What to do, nodes then ways then relations. Apply undeletes and
    /// restores in this order and deletes in reverse (see `deletes_first`).
    pub steps: Vec<Step>,
    /// Objects left alone, and why.
    pub skipped: Vec<(FeatureKind, i64, Conflict)>,
    /// Objects the changeset left exactly as they began (created and deleted
    /// within it): nothing to do.
    pub neutral: usize,
}

/// Decide how to undo what `touched` records.
///
/// `current` holds each object as the server has it now and `before` each
/// object at `Touched::version_before`. An object is only reverted while it is
/// still at the changeset's last version; otherwise a later edit would be
/// silently undone along with it.
pub fn plan(
    touched: &[Touched],
    current: &HashMap<(FeatureKind, i64), Entry>,
    before: &HashMap<(FeatureKind, i64), Entry>,
) -> Plan {
    let mut plan = Plan::default();
    for t in touched {
        let key = (t.kind, t.id);
        let skip = |plan: &mut Plan, why| plan.skipped.push((t.kind, t.id, why));

        // Created and deleted again within the changeset: net nothing.
        if t.version_before().is_none() && !t.last.is_visible() {
            plan.neutral += 1;
            continue;
        }
        let Some(now) = current.get(&key) else {
            skip(&mut plan, Conflict::Unavailable);
            continue;
        };
        if now.version != t.last.version {
            skip(
                &mut plan,
                Conflict::ChangedSince {
                    current_version: now.version,
                },
            );
            continue;
        }

        let previous = match t.version_before() {
            None => None,
            Some(_) => match before.get(&key) {
                Some(entry) => Some(entry),
                None => {
                    skip(&mut plan, Conflict::Unavailable);
                    continue;
                }
            },
        };
        // What the object was before the changeset, if it existed then.
        let was = previous.and_then(|p| p.object.clone());

        match (was, &now.object) {
            // It exists now and did not before: the changeset made it.
            (None, Some(current)) => plan.steps.push(Step::Delete {
                current: current.clone(),
            }),
            // It exists now and did before: put the old content back, under
            // the current version so the upload names the right base.
            (Some(before), Some(current)) => {
                let before = with_version(before, now.version);
                if before_matches(&before, current) {
                    plan.neutral += 1;
                } else {
                    plan.steps.push(Step::Restore {
                        before,
                        current: current.clone(),
                    });
                }
            }
            // It is deleted now and was not before: nothing to bring back.
            (None, None) => plan.neutral += 1,
            // The changeset deleted it: bring it back.
            (Some(before), None) => plan.steps.push(Step::Undelete {
                before: with_version(before, now.version),
            }),
        }
    }
    plan
}

/// `object` with its version replaced.
fn with_version(object: Object, version: i32) -> Object {
    match object {
        Object::Node(mut n) => {
            n.version = version;
            Object::Node(n)
        }
        Object::Way(mut w) => {
            w.version = version;
            Object::Way(w)
        }
        Object::Relation(mut r) => {
            r.version = version;
            Object::Relation(r)
        }
    }
}

/// Whether two objects of the same version say the same thing.
fn before_matches(a: &Object, b: &Object) -> bool {
    a == b
}

/// The changeset number in what the user typed: `123`, `#123`, `changeset 123`
/// or a page address such as `https://www.openstreetmap.org/changeset/123`.
pub fn parse_changeset_id(text: &str) -> Option<i64> {
    let text = text.trim();
    let after = match text.find("changeset") {
        Some(at) => &text[at + "changeset".len()..],
        None => text,
    };
    let after = after.trim_start_matches(|c: char| c == '/' || c == '#' || c.is_whitespace());
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    // Anything but a separator after the number (`123abc`) is not an id.
    let rest = &after[digits.len()..];
    if !(rest.is_empty()
        || rest.starts_with(['/', '?', '#'])
        || rest.starts_with(char::is_whitespace))
    {
        return None;
    }
    digits.parse().ok().filter(|&id| id > 0)
}

/// A revert ready to apply: the plan, and the server's current objects it
/// needs in the layer first.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub changeset: i64,
    pub plan: Plan,
    /// The objects the plan deletes or restores as the server has them now,
    /// and the nodes of any way involved, so the layer can hold them all.
    pub server: OsmData,
}

fn insert_object(data: &mut OsmData, object: &Object) {
    match object {
        Object::Node(n) => {
            data.nodes.insert(n.id, n.clone());
        }
        Object::Way(w) => {
            data.ways.insert(w.id, w.clone());
        }
        Object::Relation(r) => {
            data.relations.retain(|x| x.id != r.id);
            data.relations.push(r.clone());
        }
    }
}

const KINDS: [FeatureKind; 3] = [FeatureKind::Node, FeatureKind::Way, FeatureKind::Relation];

/// Fetch everything needed to revert `changeset` and work out what to do.
/// Synchronous — call from a worker thread. All requests go through
/// `osm_api`, so they share its retries, token handling and errors.
pub fn prepare(
    client: &dyn crate::http::HttpClient,
    base_url: &str,
    token: Option<&str>,
    changeset: i64,
) -> Result<Prepared, crate::osm_api::OsmApiError> {
    use crate::osm_api::{fetch_changeset_with, fetch_objects_with};

    let entries = fetch_changeset_with(client, base_url, token, changeset)?;
    let touched = touched_objects(&entries);

    let mut current: HashMap<(FeatureKind, i64), Entry> = HashMap::new();
    let mut before: HashMap<(FeatureKind, i64), Entry> = HashMap::new();
    for kind in KINDS {
        let now: Vec<(i64, Option<i32>)> = touched
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| (t.id, None))
            .collect();
        if !now.is_empty() {
            for e in fetch_objects_with(client, base_url, token, kind, &now)? {
                current.insert(e.key(), e);
            }
        }
        // A created-and-deleted object has no earlier version to look up and
        // is neutral whatever the server says now.
        let was: Vec<(i64, Option<i32>)> = touched
            .iter()
            .filter(|t| t.kind == kind)
            .filter_map(|t| Some((t.id, Some(t.version_before()?))))
            .collect();
        if !was.is_empty() {
            for e in fetch_objects_with(client, base_url, token, kind, &was)? {
                before.insert(e.key(), e);
            }
        }
    }

    let plan = plan(&touched, &current, &before);

    let mut server = OsmData::default();
    for step in &plan.steps {
        match step {
            Step::Delete { current } | Step::Restore { current, .. } => {
                insert_object(&mut server, current)
            }
            Step::Undelete { .. } => {}
        }
    }
    // Every way the layer will hold needs its nodes, or it has no shape. The
    // ones the plan itself supplies (restored or undeleted nodes) don't count.
    let mut supplied: std::collections::HashSet<NodeId> = server.nodes.keys().copied().collect();
    let mut wanted: Vec<NodeId> = Vec::new();
    let mut want = |w: &OsmWay| wanted.extend(w.nodes.iter().copied());
    server.ways.values().for_each(&mut want);
    for step in &plan.steps {
        match step {
            Step::Restore {
                before: Object::Way(w),
                ..
            }
            | Step::Undelete {
                before: Object::Way(w),
            } => want(w),
            Step::Restore {
                before: Object::Node(n),
                ..
            }
            | Step::Undelete {
                before: Object::Node(n),
            } => {
                supplied.insert(n.id);
            }
            _ => {}
        }
    }
    wanted.sort_unstable();
    wanted.dedup();
    let missing: Vec<(i64, Option<i32>)> = wanted
        .into_iter()
        .filter(|id| !supplied.contains(id))
        .map(|id| (id.get(), None))
        .collect();
    if !missing.is_empty() {
        for e in fetch_objects_with(client, base_url, token, FeatureKind::Node, &missing)? {
            if let Some(object) = &e.object {
                insert_object(&mut server, object);
            }
        }
    }
    Ok(Prepared {
        changeset,
        plan,
        server,
    })
}

/// Why an object was left alone when the revert was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Someone else changed it since the changeset (see `Conflict`).
    Conflict(Conflict),
    /// It has unuploaded edits here, which a revert must not overwrite.
    EditedLocally,
    /// The layer refused the edit (a node a way still uses, a way a relation
    /// still lists, …).
    Refused(String),
}

/// What applying a revert did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub reverted: usize,
    pub neutral: usize,
    pub skipped: Vec<(FeatureKind, i64, SkipReason)>,
}

impl Report {
    /// One line per object left alone, the first `limit` of them, then a
    /// count of the rest. Empty when nothing was left alone.
    pub fn skipped_text(&self, limit: usize) -> String {
        let mut lines: Vec<String> = self
            .skipped
            .iter()
            .take(limit)
            .map(|(kind, id, why)| {
                let kind = match kind {
                    FeatureKind::Node => crate::tr!("revert.kind.node"),
                    FeatureKind::Way => crate::tr!("revert.kind.way"),
                    FeatureKind::Relation => crate::tr!("revert.kind.relation"),
                };
                match why {
                    SkipReason::Conflict(Conflict::ChangedSince { current_version }) => {
                        crate::tr!(
                            "revert.skipped.changed_since",
                            kind = kind,
                            id = id,
                            version = current_version
                        )
                    }
                    SkipReason::Conflict(Conflict::Unavailable) => {
                        crate::tr!("revert.skipped.unavailable", kind = kind, id = id)
                    }
                    SkipReason::EditedLocally => {
                        crate::tr!("revert.skipped.edited_locally", kind = kind, id = id)
                    }
                    SkipReason::Refused(detail) => {
                        crate::tr!(
                            "revert.skipped.refused",
                            kind = kind,
                            id = id,
                            detail = detail
                        )
                    }
                }
                .to_string()
            })
            .collect();
        if self.skipped.len() > limit {
            lines.push(crate::tr!("revert.more", count = self.skipped.len() - limit).to_string());
        }
        lines.join("\n")
    }
}

fn retag(
    rec: &mut Recording<'_>,
    kind: FeatureKind,
    id: i64,
    want: &Tags,
) -> Result<(), EditError> {
    let feature = FeatureRef {
        layer_id: rec.layer_id(),
        kind,
        id,
    };
    let have = rec
        .layer()
        .feature_tags(&feature)
        .ok_or_else(|| EditError::unknown(kind, id))?;
    for (key, _) in have.iter() {
        if want.get(key).is_none() {
            rec.remove_tag(kind, id, key)?;
        }
    }
    for (key, value) in want.iter() {
        if have.get(key) != Some(value) {
            rec.set_tag(kind, id, key, value)?;
        }
    }
    Ok(())
}

fn restore(rec: &mut Recording<'_>, before: &Object) -> Result<(), EditError> {
    match before {
        Object::Node(n) => {
            if rec.layer().node_lat_lon(n.id) != Some((n.lat, n.lon)) {
                rec.commit_node_moves(&[(n.id, n.lat, n.lon)])?;
            }
            retag(rec, FeatureKind::Node, n.id.get(), &n.tags)
        }
        Object::Way(w) => {
            if rec.layer().way_node_ids(w.id).as_ref() != Some(&w.nodes) {
                rec.set_way_nodes(w.id, w.nodes.clone())?;
            }
            retag(rec, FeatureKind::Way, w.id.get(), &w.tags)
        }
        Object::Relation(r) => {
            if rec.layer().relation(r.id).map(|x| x.members) != Some(r.members.clone()) {
                rec.set_relation_members(r.id, r.members.clone())?;
            }
            retag(rec, FeatureKind::Relation, r.id.get(), &r.tags)
        }
    }
}

/// Apply `prepared` to `layer`: bring the server's current objects in, then
/// make the edits that undo the changeset. Returns the edits, for one undo
/// step, and what was done and left alone.
///
/// Objects with unuploaded edits are left alone. Undeleted objects return as
/// edits of the existing ones, not new ones (see `EditOp::Undelete`).
pub fn apply(
    layer_id: LayerId,
    layer: &mut dyn EditableLayer,
    prepared: &Prepared,
) -> (Vec<EditOp>, Report) {
    let mut report = Report {
        neutral: prepared.plan.neutral,
        skipped: prepared
            .plan
            .skipped
            .iter()
            .map(|(k, id, c)| (*k, *id, SkipReason::Conflict(c.clone())))
            .collect(),
        ..Report::default()
    };

    // Asked before the download is merged in, which can change the answer.
    let steps: Vec<&Step> = prepared
        .plan
        .steps
        .iter()
        .filter(|step| {
            let (kind, id) = step.key();
            let local = layer.has_pending_edit(kind, id);
            if local {
                report.skipped.push((kind, id, SkipReason::EditedLocally));
            }
            !local
        })
        .collect();

    layer.merge_download(prepared.server.clone());

    let mut rec = Recording::new(layer_id, layer);

    let mut undeleted = PurgedFeatures::default();
    for step in &steps {
        if let Step::Undelete { before } = step {
            match before {
                Object::Node(n) => undeleted.nodes.push(n.clone()),
                Object::Way(w) => undeleted.ways.push(w.clone()),
                Object::Relation(r) => undeleted.relations.push((r.clone(), 0, 0)),
            }
        }
    }
    report.reverted += undeleted.nodes.len() + undeleted.ways.len() + undeleted.relations.len();
    rec.undelete_objects(undeleted);

    for step in &steps {
        if let Step::Restore { before, .. } = step {
            match restore(&mut rec, before) {
                Ok(()) => report.reverted += 1,
                Err(e) => {
                    let (kind, id) = step.key();
                    report
                        .skipped
                        .push((kind, id, SkipReason::Refused(e.to_string())));
                }
            }
        }
    }

    // Relations, then ways, then nodes: each lets go of what it holds first.
    for step in steps.iter().rev() {
        if let Step::Delete { .. } = step {
            let (kind, id) = step.key();
            let done = match kind {
                FeatureKind::Relation => rec.delete_relation(RelationId(id)).map(drop),
                _ => rec.delete_feature(kind, id).map(drop),
            };
            match done {
                Ok(()) => report.reverted += 1,
                Err(e) => report
                    .skipped
                    .push((kind, id, SkipReason::Refused(e.to_string()))),
            }
        }
    }
    (rec.into_ops(), report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osmChange version="0.6">
 <create>
  <node id="10" visible="true" version="1" changeset="5" lat="1.5" lon="2.5">
   <tag k="name" v="A &amp; B"/>
  </node>
  <way id="20" visible="true" version="1" changeset="5">
   <nd ref="10"/>
   <nd ref="11"/>
   <tag k="highway" v="path"/>
  </way>
 </create>
 <modify>
  <node id="11" visible="true" version="4" changeset="5" lat="3.0" lon="4.0"/>
  <relation id="30" visible="true" version="2" changeset="5">
   <member type="way" ref="20" role="outer"/>
   <tag k="type" v="multipolygon"/>
  </relation>
 </modify>
 <delete>
  <node id="12" visible="false" version="3" changeset="5"/>
 </delete>
</osmChange>"#;

    fn parsed() -> Vec<(Option<Section>, Entry)> {
        parse_entries(CHANGE).unwrap()
    }

    fn node(id: i64, version: i32, lat: f64, tags: &[(&str, &str)]) -> Object {
        let mut t = Tags::new();
        for (k, v) in tags {
            t.insert(k.to_string(), v.to_string());
        }
        Object::Node(OsmNode {
            id: NodeId(id),
            lat,
            lon: 0.0,
            version,
            tags: t,
        })
    }

    fn entry(id: i64, version: i32, object: Option<Object>) -> Entry {
        Entry {
            kind: FeatureKind::Node,
            id,
            version,
            object,
        }
    }

    fn map(entries: Vec<Entry>) -> HashMap<(FeatureKind, i64), Entry> {
        entries.into_iter().map(|e| (e.key(), e)).collect()
    }

    #[test]
    fn changeset_ids_are_read_from_numbers_and_page_addresses() {
        for text in [
            "123",
            " 123 ",
            "#123",
            "changeset 123",
            "changeset/123",
            "https://www.openstreetmap.org/changeset/123",
            "https://www.openstreetmap.org/changeset/123/",
            "https://www.openstreetmap.org/changeset/123?foo=1",
            "https://www.openstreetmap.org/changeset/123#map=10/1/2",
        ] {
            assert_eq!(parse_changeset_id(text), Some(123), "{text}");
        }
        for text in [
            "",
            "abc",
            "0",
            "-5",
            "12ab",
            "https://www.openstreetmap.org/",
        ] {
            assert_eq!(parse_changeset_id(text), None, "{text}");
        }
    }

    #[test]
    fn prepare_fetches_the_changeset_then_current_then_previous_versions() {
        use crate::http::fake::{ok, FakeClient};
        let change = r#"<osmChange><modify>
            <node id="1" visible="true" version="3" lat="9" lon="9"><tag k="a" v="new"/></node>
            </modify></osmChange>"#;
        let now = r#"<osm><node id="1" visible="true" version="3" lat="9" lon="9">
            <tag k="a" v="new"/></node></osm>"#;
        let was = r#"<osm><node id="1" visible="true" version="2" lat="1" lon="1">
            <tag k="a" v="old"/></node></osm>"#;
        let client = FakeClient::new(vec![ok(200, change), ok(200, now), ok(200, was)]);

        let prepared = prepare(&client, "https://osm.test", None, 42).unwrap();

        let urls: Vec<String> = client
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://osm.test/api/0.6/changeset/42/download",
                "https://osm.test/api/0.6/nodes?nodes=1",
                "https://osm.test/api/0.6/nodes?nodes=1v2",
            ]
        );
        assert_eq!(prepared.changeset, 42);
        assert_eq!(prepared.plan.steps.len(), 1);
        assert!(matches!(prepared.plan.steps[0], Step::Restore { .. }));
        assert!(prepared.server.nodes.contains_key(&NodeId(1)));
    }

    #[test]
    fn prepare_fetches_the_nodes_a_restored_way_needs() {
        use crate::http::fake::{ok, FakeClient};
        let change = r#"<osmChange><modify>
            <way id="5" visible="true" version="2"><nd ref="1"/></way></modify></osmChange>"#;
        let now = r#"<osm><way id="5" visible="true" version="2"><nd ref="1"/></way></osm>"#;
        let was =
            r#"<osm><way id="5" visible="true" version="1"><nd ref="1"/><nd ref="2"/></way></osm>"#;
        let nodes = r#"<osm><node id="1" visible="true" version="1" lat="0" lon="0"/>
            <node id="2" visible="true" version="1" lat="1" lon="1"/></osm>"#;
        let client = FakeClient::new(vec![
            ok(200, change),
            ok(200, now),
            ok(200, was),
            ok(200, nodes),
        ]);

        let prepared = prepare(&client, "https://osm.test", None, 1).unwrap();

        let last = client.requests.lock().unwrap().last().unwrap().url.clone();
        assert_eq!(last, "https://osm.test/api/0.6/nodes?nodes=1,2");
        assert_eq!(prepared.server.nodes.len(), 2);
        assert!(prepared.server.ways.contains_key(&WayId(5)));
    }

    #[test]
    fn prepare_reports_a_missing_changeset() {
        use crate::http::fake::{status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(404, "")]);
        assert!(prepare(&client, "https://osm.test", None, 1).is_err());
    }

    #[test]
    fn osm_change_sections_and_contents_are_read() {
        let entries = parsed();
        assert_eq!(entries.len(), 5);
        let sections: Vec<_> = entries.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            sections,
            vec![
                Some(Section::Create),
                Some(Section::Create),
                Some(Section::Modify),
                Some(Section::Modify),
                Some(Section::Delete),
            ]
        );
        let Some(Object::Node(n)) = &entries[0].1.object else {
            panic!("first entry should be a node");
        };
        assert_eq!((n.lat, n.lon, n.version), (1.5, 2.5, 1));
        assert_eq!(n.tags.get("name"), Some("A & B"));
        let Some(Object::Way(w)) = &entries[1].1.object else {
            panic!("second entry should be a way");
        };
        assert_eq!(w.nodes, vec![NodeId(10), NodeId(11)]);
        let Some(Object::Relation(r)) = &entries[3].1.object else {
            panic!("fourth entry should be a relation");
        };
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.members[0].role, "outer");
    }

    #[test]
    fn a_deleted_object_has_no_content_and_needs_no_position() {
        let entries = parsed();
        let (section, deleted) = &entries[4];
        assert_eq!(*section, Some(Section::Delete));
        assert_eq!((deleted.id, deleted.version), (12, 3));
        assert!(!deleted.is_visible());
    }

    #[test]
    fn a_plain_osm_response_with_deleted_objects_parses() {
        let xml = r#"<osm><node id="1" visible="true" version="2" lat="1" lon="2"/>
            <node id="3" visible="false" version="1"/></osm>"#;
        let entries = parse_entries(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(s, _)| s.is_none()));
        assert!(entries[0].1.is_visible());
        assert!(!entries[1].1.is_visible());
    }

    #[test]
    fn a_visible_node_without_a_position_is_an_error() {
        let xml = r#"<osm><node id="1" visible="true" version="2"/></osm>"#;
        assert!(parse_entries(xml).is_err());
    }

    #[test]
    fn several_versions_of_one_object_collapse() {
        let entries = vec![
            (
                Some(Section::Create),
                entry(1, 1, Some(node(1, 1, 0.0, &[]))),
            ),
            (
                Some(Section::Modify),
                entry(1, 2, Some(node(1, 2, 1.0, &[]))),
            ),
            (
                Some(Section::Modify),
                entry(2, 5, Some(node(2, 5, 0.0, &[]))),
            ),
        ];
        let touched = touched_objects(&entries);
        assert_eq!(touched.len(), 2);
        assert_eq!((touched[0].id, touched[0].first_version), (1, 1));
        assert_eq!(touched[0].last.version, 2);
        assert_eq!(touched[0].version_before(), None);
        assert_eq!(touched[1].version_before(), Some(4));
    }

    #[test]
    fn touched_objects_come_nodes_then_ways_then_relations() {
        let touched = touched_objects(&parsed());
        let kinds: Vec<_> = touched.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FeatureKind::Node,
                FeatureKind::Node,
                FeatureKind::Node,
                FeatureKind::Way,
                FeatureKind::Relation,
            ]
        );
    }

    fn touched_one(first: i32, last: Entry) -> Vec<Touched> {
        vec![Touched {
            kind: last.kind,
            id: last.id,
            first_version: first,
            last,
        }]
    }

    #[test]
    fn a_created_object_is_deleted() {
        let last = entry(1, 1, Some(node(1, 1, 0.0, &[])));
        let current = map(vec![last.clone()]);
        let plan = plan(&touched_one(1, last), &current, &HashMap::new());
        assert_eq!(
            plan.steps,
            vec![Step::Delete {
                current: node(1, 1, 0.0, &[])
            }]
        );
    }

    #[test]
    fn a_modified_object_goes_back_under_the_current_version() {
        let last = entry(1, 3, Some(node(1, 3, 9.0, &[("a", "new")])));
        let current = map(vec![last.clone()]);
        let before = map(vec![entry(1, 2, Some(node(1, 2, 1.0, &[("a", "old")])))]);
        let plan = plan(&touched_one(3, last), &current, &before);
        assert_eq!(
            plan.steps,
            vec![Step::Restore {
                before: node(1, 3, 1.0, &[("a", "old")]),
                current: node(1, 3, 9.0, &[("a", "new")]),
            }]
        );
    }

    #[test]
    fn a_deleted_object_comes_back_under_the_deleted_version() {
        let last = entry(1, 3, None);
        let current = map(vec![last.clone()]);
        let before = map(vec![entry(1, 2, Some(node(1, 2, 1.0, &[("a", "old")])))]);
        let plan = plan(&touched_one(3, last), &current, &before);
        assert_eq!(
            plan.steps,
            vec![Step::Undelete {
                before: node(1, 3, 1.0, &[("a", "old")])
            }]
        );
    }

    #[test]
    fn an_object_edited_since_is_skipped_not_reverted() {
        let last = entry(1, 3, Some(node(1, 3, 9.0, &[])));
        let current = map(vec![entry(1, 4, Some(node(1, 4, 7.0, &[])))]);
        let before = map(vec![entry(1, 2, Some(node(1, 2, 1.0, &[])))]);
        let plan = plan(&touched_one(3, last), &current, &before);
        assert!(plan.steps.is_empty());
        assert_eq!(
            plan.skipped,
            vec![(
                FeatureKind::Node,
                1,
                Conflict::ChangedSince { current_version: 4 }
            )]
        );
    }

    #[test]
    fn an_object_deleted_since_is_skipped() {
        let last = entry(1, 1, Some(node(1, 1, 0.0, &[])));
        let current = map(vec![entry(1, 2, None)]);
        let plan = plan(&touched_one(1, last), &current, &HashMap::new());
        assert!(plan.steps.is_empty());
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn created_then_deleted_within_the_changeset_is_nothing_to_do() {
        let last = entry(1, 2, None);
        let plan = plan(&touched_one(1, last), &HashMap::new(), &HashMap::new());
        assert!(plan.steps.is_empty());
        assert!(plan.skipped.is_empty());
        assert_eq!(plan.neutral, 1);
    }

    #[test]
    fn an_object_missing_from_the_server_is_unavailable() {
        let last = entry(1, 3, Some(node(1, 3, 9.0, &[])));
        let plan = plan(&touched_one(3, last), &HashMap::new(), &HashMap::new());
        assert_eq!(
            plan.skipped,
            vec![(FeatureKind::Node, 1, Conflict::Unavailable)]
        );
    }

    #[test]
    fn a_missing_previous_version_is_unavailable() {
        let last = entry(1, 3, Some(node(1, 3, 9.0, &[])));
        let current = map(vec![last.clone()]);
        let plan = plan(&touched_one(3, last), &current, &HashMap::new());
        assert_eq!(
            plan.skipped,
            vec![(FeatureKind::Node, 1, Conflict::Unavailable)]
        );
    }

    #[test]
    fn an_edit_that_changed_nothing_visible_is_neutral() {
        let last = entry(1, 3, Some(node(1, 3, 1.0, &[])));
        let current = map(vec![last.clone()]);
        let before = map(vec![entry(1, 2, Some(node(1, 2, 1.0, &[])))]);
        let plan = plan(&touched_one(3, last), &current, &before);
        assert!(plan.steps.is_empty());
        assert_eq!(plan.neutral, 1);
    }
}
