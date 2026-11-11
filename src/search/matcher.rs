//! Running a parsed [`Query`] over a layer's [`OsmData`].
//!
//! One pass over the nodes, ways and relations; matching a tag allocates
//! nothing for ASCII text (non-ASCII values are lower-cased on the fly).
//! `child` / `parent` sub-queries are resolved first, each into a set of ids
//! that the main pass then looks objects up in.

use super::{Expr, Pat, Query, ValueTest};
use crate::osm::{MemberType, OsmData, OsmNode, OsmRelation, OsmWay};
use crate::selection::FeatureKind;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// A latitude/longitude rectangle (the map view, for `inview`).
pub use crate::coordinates::GeoBounds;

fn intersects(a: &GeoBounds, b: &GeoBounds) -> bool {
    a.min_lat <= b.max_lat
        && a.max_lat >= b.min_lat
        && a.min_lon <= b.max_lon
        && a.max_lon >= b.min_lon
}

/// What a query may ask about besides the data itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchContext<'a> {
    /// The layer's baseline (as last loaded or uploaded): `modified` is
    /// "differs from this, or is new". Without it only new objects count.
    pub original: Option<&'a OsmData>,
    /// The selected objects, for `selected`.
    pub selected: Option<&'a HashSet<(FeatureKind, i64)>>,
    /// The map view, for `inview`. Without it nothing is in view.
    pub view: Option<GeoBounds>,
    /// Only report objects that are in `selected` ("find in selection").
    pub only_selected: bool,
}

#[derive(Clone, Copy)]
enum Obj<'a> {
    Node(&'a OsmNode),
    Way(&'a OsmWay),
    Relation(&'a OsmRelation),
}

impl<'a> Obj<'a> {
    fn kind(&self) -> FeatureKind {
        match self {
            Obj::Node(_) => FeatureKind::Node,
            Obj::Way(_) => FeatureKind::Way,
            Obj::Relation(_) => FeatureKind::Relation,
        }
    }

    fn id(&self) -> i64 {
        match self {
            Obj::Node(n) => n.id,
            Obj::Way(w) => w.id,
            Obj::Relation(r) => r.id,
        }
    }

    fn tags(&self) -> &'a HashMap<String, String> {
        match self {
            Obj::Node(n) => &n.tags,
            Obj::Way(w) => &w.tags,
            Obj::Relation(r) => &r.tags,
        }
    }

    fn version(&self) -> i32 {
        match self {
            Obj::Node(n) => n.version,
            Obj::Way(w) => w.version,
            Obj::Relation(r) => r.version,
        }
    }
}

#[derive(Default)]
struct IdSets {
    nodes: HashSet<i64>,
    ways: HashSet<i64>,
    relations: HashSet<i64>,
}

impl IdSets {
    fn insert(&mut self, kind: FeatureKind, id: i64) {
        match kind {
            FeatureKind::Node => self.nodes.insert(id),
            FeatureKind::Way => self.ways.insert(id),
            FeatureKind::Relation => self.relations.insert(id),
        };
    }

    fn contains(&self, kind: FeatureKind, id: i64) -> bool {
        match kind {
            FeatureKind::Node => self.nodes.contains(&id),
            FeatureKind::Way => self.ways.contains(&id),
            FeatureKind::Relation => self.relations.contains(&id),
        }
    }
}

struct Env<'a> {
    data: &'a OsmData,
    ctx: &'a SearchContext<'a>,
    original_relations: HashMap<i64, &'a OsmRelation>,
    /// Result of each `child` / `parent` sub-query, by index.
    structural: Vec<IdSets>,
}

/// Every node, way and relation of `data` that matches `query`, nodes first
/// and each kind sorted by id (the order Select All uses).
pub fn search(query: &Query, data: &OsmData, ctx: &SearchContext<'_>) -> Vec<(FeatureKind, i64)> {
    let original_relations = ctx
        .original
        .map(|o| o.relations.iter().map(|r| (r.id, r)).collect())
        .unwrap_or_default();
    let mut env = Env {
        data,
        ctx,
        original_relations,
        structural: Vec::with_capacity(query.structural),
    };

    if query.structural > 0 {
        let mut subs: Vec<(usize, bool, &Expr)> = Vec::new();
        collect_structural(&query.expr, &mut subs);
        subs.sort_by_key(|(index, _, _)| *index);
        for (_, is_child, inner) in subs {
            let sets = env.resolve_structural(is_child, inner);
            env.structural.push(sets);
        }
    }

    let keep = |obj: Obj<'_>| -> bool {
        if ctx.only_selected
            && !ctx
                .selected
                .is_some_and(|s| s.contains(&(obj.kind(), obj.id())))
        {
            return false;
        }
        env.eval(&query.expr, obj)
    };

    let mut node_ids: Vec<i64> = data
        .nodes
        .values()
        .filter(|n| keep(Obj::Node(n)))
        .map(|n| n.id)
        .collect();
    let mut way_ids: Vec<i64> = data
        .ways
        .values()
        .filter(|w| keep(Obj::Way(w)))
        .map(|w| w.id)
        .collect();
    let mut relation_ids: Vec<i64> = data
        .relations
        .iter()
        .filter(|r| keep(Obj::Relation(r)))
        .map(|r| r.id)
        .collect();
    node_ids.sort_unstable();
    way_ids.sort_unstable();
    relation_ids.sort_unstable();

    let mut out = Vec::with_capacity(node_ids.len() + way_ids.len() + relation_ids.len());
    out.extend(node_ids.into_iter().map(|id| (FeatureKind::Node, id)));
    out.extend(way_ids.into_iter().map(|id| (FeatureKind::Way, id)));
    out.extend(
        relation_ids
            .into_iter()
            .map(|id| (FeatureKind::Relation, id)),
    );
    out
}

/// Gather the `child` / `parent` sub-expressions of `expr`.
fn collect_structural<'e>(expr: &'e Expr, out: &mut Vec<(usize, bool, &'e Expr)>) {
    match expr {
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_structural(item, out);
            }
        }
        Expr::Not(inner) => collect_structural(inner, out),
        Expr::Child { index, expr } => {
            collect_structural(expr, out);
            out.push((*index, true, expr));
        }
        Expr::Parent { index, expr } => {
            collect_structural(expr, out);
            out.push((*index, false, expr));
        }
        _ => {}
    }
}

impl<'a> Env<'a> {
    /// The ids that `child inner` (or `parent inner`) matches. Sub-queries
    /// nested inside `inner` have smaller indices and are already resolved.
    fn resolve_structural(&self, is_child: bool, inner: &Expr) -> IdSets {
        let data = self.data;
        let mut matched = IdSets::default();
        for n in data.nodes.values() {
            if self.eval(inner, Obj::Node(n)) {
                matched.nodes.insert(n.id);
            }
        }
        for w in data.ways.values() {
            if self.eval(inner, Obj::Way(w)) {
                matched.ways.insert(w.id);
            }
        }
        for r in &data.relations {
            if self.eval(inner, Obj::Relation(r)) {
                matched.relations.insert(r.id);
            }
        }

        let mut result = IdSets::default();
        if is_child {
            for id in &matched.ways {
                if let Some(w) = data.ways.get(id) {
                    result.nodes.extend(w.nodes.iter().copied());
                }
            }
            for r in &data.relations {
                if matched.relations.contains(&r.id) {
                    for m in &r.members {
                        result.insert(member_kind(m.member_type), m.reference);
                    }
                }
            }
        } else {
            for w in data.ways.values() {
                if w.nodes.iter().any(|n| matched.nodes.contains(n)) {
                    result.ways.insert(w.id);
                }
            }
            for r in &data.relations {
                if r.members
                    .iter()
                    .any(|m| matched.contains(member_kind(m.member_type), m.reference))
                {
                    result.relations.insert(r.id);
                }
            }
        }
        result
    }

    fn eval(&self, expr: &Expr, obj: Obj<'_>) -> bool {
        match expr {
            Expr::And(items) => items.iter().all(|e| self.eval(e, obj)),
            Expr::Or(items) => items.iter().any(|e| self.eval(e, obj)),
            Expr::Not(inner) => !self.eval(inner, obj),
            Expr::Child { index, .. } | Expr::Parent { index, .. } => self
                .structural
                .get(*index)
                .is_some_and(|s| s.contains(obj.kind(), obj.id())),
            Expr::Tag { key, value } => {
                let tags = obj.tags();
                match value {
                    ValueTest::Present => tags.keys().any(|k| key.matches(k)),
                    ValueTest::Absent => !tags.iter().any(|(k, v)| !v.is_empty() && key.matches(k)),
                    ValueTest::Matches(p) => {
                        tags.iter().any(|(k, v)| key.matches(k) && p.matches(v))
                    }
                }
            }
            Expr::Word(p) => obj.tags().iter().any(|(k, v)| p.matches(k) || p.matches(v)),
            Expr::Type(kind) => obj.kind() == *kind,
            Expr::Modified => obj.id() < 0 || self.differs_from_original(obj),
            Expr::New => obj.id() < 0,
            Expr::Selected => self
                .ctx
                .selected
                .is_some_and(|s| s.contains(&(obj.kind(), obj.id()))),
            Expr::Untagged => obj.tags().is_empty(),
            Expr::Closed => matches!(obj, Obj::Way(w) if w.is_closed()),
            Expr::Incomplete => match obj {
                Obj::Node(_) => false,
                Obj::Way(w) => w.nodes.iter().any(|n| !self.data.nodes.contains_key(n)),
                Obj::Relation(r) => r.members.iter().any(|m| !self.has_member(m)),
            },
            Expr::InView => self.ctx.view.is_some_and(|v| self.in_view(&v, obj, true)),
            Expr::Id(id) => obj.id() == *id,
            Expr::Version(r) => r.contains(obj.version().max(0) as u64),
            Expr::Nodes(r) => matches!(obj, Obj::Way(w) if r.contains(w.nodes.len() as u64)),
            Expr::Tags(r) => r.contains(obj.tags().len() as u64),
            Expr::Members(r) => {
                matches!(obj, Obj::Relation(rel) if r.contains(rel.members.len() as u64))
            }
        }
    }

    fn has_member(&self, m: &crate::osm::OsmMember) -> bool {
        match m.member_type {
            MemberType::Node => self.data.nodes.contains_key(&m.reference),
            MemberType::Way => self.data.ways.contains_key(&m.reference),
            MemberType::Relation => self.data.relations.iter().any(|r| r.id == m.reference),
        }
    }

    /// Whether the object differs from the baseline (or is missing there).
    fn differs_from_original(&self, obj: Obj<'_>) -> bool {
        let Some(original) = self.ctx.original else {
            return false;
        };
        match obj {
            Obj::Node(n) => original
                .nodes
                .get(&n.id)
                .is_none_or(|o| o.lat != n.lat || o.lon != n.lon || o.tags != n.tags),
            Obj::Way(w) => original
                .ways
                .get(&w.id)
                .is_none_or(|o| o.nodes != w.nodes || o.tags != w.tags),
            Obj::Relation(r) => self
                .original_relations
                .get(&r.id)
                .is_none_or(|o| o.members != r.members || o.tags != r.tags),
        }
    }

    /// `inview`: a node inside the view, a way whose bounding box meets it,
    /// a relation with such a member (one level deep).
    fn in_view(&self, view: &GeoBounds, obj: Obj<'_>, recurse: bool) -> bool {
        match obj {
            Obj::Node(n) => view.contains(n.lat, n.lon),
            Obj::Way(w) => {
                let mut bbox: Option<GeoBounds> = None;
                for id in &w.nodes {
                    let Some(n) = self.data.nodes.get(id) else {
                        continue;
                    };
                    if view.contains(n.lat, n.lon) {
                        return true;
                    }
                    let b = bbox.get_or_insert(GeoBounds {
                        min_lat: n.lat,
                        max_lat: n.lat,
                        min_lon: n.lon,
                        max_lon: n.lon,
                    });
                    b.min_lat = b.min_lat.min(n.lat);
                    b.max_lat = b.max_lat.max(n.lat);
                    b.min_lon = b.min_lon.min(n.lon);
                    b.max_lon = b.max_lon.max(n.lon);
                }
                bbox.is_some_and(|b| intersects(view, &b))
            }
            Obj::Relation(r) => {
                recurse
                    && r.members.iter().any(|m| match m.member_type {
                        MemberType::Node => self
                            .data
                            .nodes
                            .get(&m.reference)
                            .is_some_and(|n| self.in_view(view, Obj::Node(n), false)),
                        MemberType::Way => self
                            .data
                            .ways
                            .get(&m.reference)
                            .is_some_and(|w| self.in_view(view, Obj::Way(w), false)),
                        MemberType::Relation => false,
                    })
            }
        }
    }
}

fn member_kind(t: MemberType) -> FeatureKind {
    match t {
        MemberType::Node => FeatureKind::Node,
        MemberType::Way => FeatureKind::Way,
        MemberType::Relation => FeatureKind::Relation,
    }
}

// ---------------------------------------------------------------------------
// Text matching. Patterns are lower-cased when parsed; the text under test is
// compared with ASCII case folding, after lower-casing it first when it holds
// non-ASCII characters.

fn lower(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.to_lowercase())
    }
}

/// Position of `needle` in `hay` at or after `from`, comparing ASCII case-
/// insensitively.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len())
        .find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

impl Pat {
    pub(crate) fn matches(&self, text: &str) -> bool {
        let text = lower(text);
        let hay = text.as_bytes();
        match self {
            Pat::Exact(p) => hay.eq_ignore_ascii_case(p.as_bytes()),
            Pat::Contains(p) => find_from(hay, p.as_bytes(), 0).is_some(),
            Pat::Glob { parts, lead, trail } => glob_match(hay, parts, *lead, *trail),
        }
    }
}

fn glob_match(hay: &[u8], parts: &[String], lead: bool, trail: bool) -> bool {
    let n = parts.len();
    if n == 0 {
        // Only stars (or nothing): everything matches, except an anchored
        // empty pattern, which only the empty text matches.
        return lead || trail || hay.is_empty();
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let p = part.as_bytes();
        if i == 0 && !lead {
            if hay.len() < p.len() || !hay[..p.len()].eq_ignore_ascii_case(p) {
                return false;
            }
            pos = p.len();
            if n == 1 && !trail {
                return hay.len() == p.len();
            }
        } else if i == n - 1 && !trail {
            if hay.len() < p.len() || hay.len() - p.len() < pos {
                return false;
            }
            return hay[hay.len() - p.len()..].eq_ignore_ascii_case(p);
        } else {
            match find_from(hay, p, pos) {
                Some(at) => pos = at + p.len(),
                None => return false,
            }
        }
    }
    true
}
