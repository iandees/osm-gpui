//! Checks run on a layer before its edits are uploaded.
//!
//! The validator is pure: it takes a layer's current [`OsmData`] and the
//! [`Changes`] the pending upload would send (objects created or modified in
//! this session) and returns a list of [`Finding`]s. It never looks at the
//! rest of the download, so a mapper is not blamed for problems that were
//! already there:
//!
//! - Per-object checks (tags, way shape, untagged objects, relation shape)
//!   run only on objects that are created or modified.
//! - Checks that depend on neighbours also run on objects directly affected
//!   by a change: a way whose node was moved is checked for crossings, and a
//!   multipolygon with a changed member way is checked for open rings. A
//!   crossing or a duplicate position is reported when at least one side of
//!   it is new, modified or affected.
//! - Deleted objects are not in the data and are never validated.
//!
//! Server size limits (way and relation length, changeset size) are enforced
//! in [`crate::limits`] and not repeated here.
//!
//! Findings are ordered errors first, then warnings, then by check and
//! feature, so the same data always lists the same way.
//!
//! The checks:
//!
//! | id                      | severity | what                                                      |
//! |-------------------------|----------|-----------------------------------------------------------|
//! | `untagged_way`          | warning  | untagged way that is in no relation                       |
//! | `untagged_node`         | warning  | untagged node in no way and no relation                   |
//! | `way_too_short`         | error    | way with fewer than 2 nodes                               |
//! | `closed_way_too_short`  | error    | closed way (first = last) with fewer than 4 node refs     |
//! | `duplicate_way_node`    | error    | the same node twice in a row in a way                     |
//! | `duplicate_nodes`       | warning  | distinct nodes at one position                            |
//! | `crossing_ways`         | warning  | highway/railway/waterway/building crossing without a node |
//! | `area_tags_on_open_way` | warning  | area tags on a way that is not closed                     |
//! | `multipolygon_no_outer` | warning  | multipolygon with no outer member                         |
//! | `multipolygon_role`     | warning  | multipolygon way member with an empty or unknown role     |
//! | `multipolygon_open_ring`| warning  | multipolygon rings that do not close                      |
//! | `relation_empty`        | warning  | relation with no members                                  |
//! | `relation_no_type`      | warning  | relation without a `type` tag                             |
//! | `tag_empty_key`         | error    | tag with an empty key                                     |
//! | `tag_empty_value`       | error    | tag with an empty value                                   |
//! | `tag_too_long`          | error    | key or value over 255 characters                          |
//! | `tag_whitespace`        | warning  | leading or trailing whitespace in a key or value          |
//! | `fixme`                 | warning  | a `fixme` / `FIXME` tag on a changed object               |
//! | `deprecated_tag`        | warning  | a well-known deprecated tag                               |

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{RTree, AABB};

use crate::layers::diff::LayerDiff;
use crate::layers::LayerId;
use crate::multipolygon;
use crate::osm::{MemberType, OsmData, OsmRelation, OsmWay};
use crate::selection::FeatureKind;
use crate::tr;

/// The longest key or value the OSM API accepts, in characters.
pub const MAX_TAG_CHARS: usize = 255;

/// Degrees are rounded to this many steps per degree to decide that two nodes
/// are at the same position (the 1e-7 precision the OSM database stores).
const POSITION_STEPS: f64 = 1e7;

/// How bad a finding is. Errors block the upload, warnings do not.
/// Orders errors before warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
}

/// One validation check. [`Check::id`] is stable across releases and
/// languages (scripts and tests refer to it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    UntaggedWay,
    UntaggedNode,
    WayTooShort,
    ClosedWayTooShort,
    DuplicateWayNode,
    DuplicateNodes,
    CrossingWays,
    AreaTagsOnOpenWay,
    MultipolygonNoOuter,
    MultipolygonRole,
    MultipolygonOpenRing,
    RelationEmpty,
    RelationNoType,
    TagEmptyKey,
    TagEmptyValue,
    TagTooLong,
    TagWhitespace,
    Fixme,
    DeprecatedTag,
}

impl Check {
    /// Every check, in the order findings of equal severity are listed.
    pub const ALL: [Check; 19] = [
        Check::UntaggedWay,
        Check::UntaggedNode,
        Check::WayTooShort,
        Check::ClosedWayTooShort,
        Check::DuplicateWayNode,
        Check::DuplicateNodes,
        Check::CrossingWays,
        Check::AreaTagsOnOpenWay,
        Check::MultipolygonNoOuter,
        Check::MultipolygonRole,
        Check::MultipolygonOpenRing,
        Check::RelationEmpty,
        Check::RelationNoType,
        Check::TagEmptyKey,
        Check::TagEmptyValue,
        Check::TagTooLong,
        Check::TagWhitespace,
        Check::Fixme,
        Check::DeprecatedTag,
    ];

    /// The stable id, e.g. `crossing_ways`.
    pub fn id(self) -> &'static str {
        match self {
            Check::UntaggedWay => "untagged_way",
            Check::UntaggedNode => "untagged_node",
            Check::WayTooShort => "way_too_short",
            Check::ClosedWayTooShort => "closed_way_too_short",
            Check::DuplicateWayNode => "duplicate_way_node",
            Check::DuplicateNodes => "duplicate_nodes",
            Check::CrossingWays => "crossing_ways",
            Check::AreaTagsOnOpenWay => "area_tags_on_open_way",
            Check::MultipolygonNoOuter => "multipolygon_no_outer",
            Check::MultipolygonRole => "multipolygon_role",
            Check::MultipolygonOpenRing => "multipolygon_open_ring",
            Check::RelationEmpty => "relation_empty",
            Check::RelationNoType => "relation_no_type",
            Check::TagEmptyKey => "tag_empty_key",
            Check::TagEmptyValue => "tag_empty_value",
            Check::TagTooLong => "tag_too_long",
            Check::TagWhitespace => "tag_whitespace",
            Check::Fixme => "fixme",
            Check::DeprecatedTag => "deprecated_tag",
        }
    }

    /// The check with this id.
    pub fn from_id(id: &str) -> Option<Check> {
        Check::ALL.into_iter().find(|c| c.id() == id)
    }

    pub fn severity(self) -> Severity {
        match self {
            Check::WayTooShort
            | Check::ClosedWayTooShort
            | Check::DuplicateWayNode
            | Check::TagEmptyKey
            | Check::TagEmptyValue
            | Check::TagTooLong => Severity::Error,
            _ => Severity::Warning,
        }
    }
}

/// A node, way or relation a finding is about.
pub type Feature = (FeatureKind, i64);

/// One problem found.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub check: Check,
    /// The problem in the user's language.
    pub message: String,
    /// The objects concerned (at least one), in a stable order.
    pub features: Vec<Feature>,
}

/// The objects a pending upload would create or modify.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub nodes: HashSet<i64>,
    pub ways: HashSet<i64>,
    pub relations: HashSet<i64>,
}

impl Changes {
    /// The created and modified objects of an upload diff (deleted ones are
    /// left out).
    pub fn from_diff(diff: &LayerDiff) -> Self {
        Changes {
            nodes: diff
                .created_nodes
                .iter()
                .chain(&diff.modified_nodes)
                .map(|n| n.id)
                .collect(),
            ways: diff
                .created_ways
                .iter()
                .chain(&diff.modified_ways)
                .map(|w| w.id)
                .collect(),
            relations: diff
                .created_relations
                .iter()
                .chain(&diff.modified_relations)
                .map(|r| r.id)
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.ways.is_empty() && self.relations.is_empty()
    }
}

/// Validate a layer against the upload diff it would send.
pub fn validate_diff(data: &OsmData, diff: &LayerDiff) -> Vec<Finding> {
    validate(data, &Changes::from_diff(diff))
}

/// Run every check on the objects in `changes` (and what they affect).
pub fn validate(data: &OsmData, changes: &Changes) -> Vec<Finding> {
    if changes.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let cx = Context::new(data, changes);

    let nodes = sorted(&changes.nodes);
    let ways = sorted(&changes.ways);
    let relations = sorted(&changes.relations);

    for &id in &nodes {
        if let Some(node) = data.nodes.get(&id) {
            if node.tags.is_empty() && !cx.node_in_way.contains(&id) && !cx.node_in_relation(id) {
                out.push(finding(
                    Check::UntaggedNode,
                    tr!("validator.msg.untagged_node").to_string(),
                    vec![(FeatureKind::Node, id)],
                ));
            }
            check_tags(FeatureKind::Node, id, &node.tags, &mut out);
        }
    }
    for &id in &ways {
        if let Some(way) = data.ways.get(&id) {
            check_way(way, &cx, &mut out);
            check_tags(FeatureKind::Way, id, &way.tags, &mut out);
        }
    }
    let relation_by_id: HashMap<i64, &OsmRelation> =
        data.relations.iter().map(|r| (r.id, r)).collect();
    for &id in &relations {
        if let Some(relation) = relation_by_id.get(&id) {
            check_relation(relation, &mut out);
            check_tags(FeatureKind::Relation, id, &relation.tags, &mut out);
        }
    }
    for relation in &data.relations {
        if cx.is_multipolygon_to_check(relation) {
            check_multipolygon(data, relation, &mut out);
        }
    }

    check_duplicate_nodes(data, &nodes, &mut out);
    check_crossings(data, &cx, &mut out);

    out.sort_by(compare_findings);
    out
}

fn sorted(ids: &HashSet<i64>) -> Vec<i64> {
    let mut v: Vec<i64> = ids.iter().copied().collect();
    v.sort_unstable();
    v
}

fn finding(check: Check, message: String, features: Vec<Feature>) -> Finding {
    Finding {
        severity: check.severity(),
        check,
        message,
        features,
    }
}

fn kind_rank(kind: FeatureKind) -> u8 {
    match kind {
        FeatureKind::Node => 0,
        FeatureKind::Way => 1,
        FeatureKind::Relation => 2,
    }
}

fn compare_findings(a: &Finding, b: &Finding) -> Ordering {
    let key = |f: &Finding| f.features.first().map(|&(k, id)| (kind_rank(k), id));
    a.severity
        .cmp(&b.severity)
        .then(a.check.cmp(&b.check))
        .then(key(a).cmp(&key(b)))
        .then_with(|| a.message.cmp(&b.message))
}

/// What the checks need to know about how objects relate to each other,
/// gathered in one pass over the ways and relations.
struct Context<'a> {
    changes: &'a Changes,
    /// Changed nodes that are part of some way.
    node_in_way: HashSet<i64>,
    /// Changed nodes that are a relation member.
    node_member: HashSet<i64>,
    /// Changed ways that are a relation member.
    way_member: HashSet<i64>,
    /// Changed ways that are a member of a multipolygon or boundary.
    way_in_area_relation: HashSet<i64>,
    /// Ways to check for crossings and rings: changed ways plus ways that
    /// contain a changed node.
    affected_ways: HashSet<i64>,
}

impl<'a> Context<'a> {
    fn new(data: &'a OsmData, changes: &'a Changes) -> Self {
        let mut node_in_way = HashSet::new();
        let mut affected_ways: HashSet<i64> = changes
            .ways
            .iter()
            .copied()
            .filter(|id| data.ways.contains_key(id))
            .collect();
        if !changes.nodes.is_empty() {
            for way in data.ways.values() {
                let mut touched = false;
                for n in &way.nodes {
                    if changes.nodes.contains(n) {
                        node_in_way.insert(*n);
                        touched = true;
                    }
                }
                if touched {
                    affected_ways.insert(way.id);
                }
            }
        }
        let mut node_member = HashSet::new();
        let mut way_member = HashSet::new();
        let mut way_in_area_relation = HashSet::new();
        for relation in &data.relations {
            let area = multipolygon::is_area_relation(&relation.tags);
            for m in &relation.members {
                match m.member_type {
                    MemberType::Node if changes.nodes.contains(&m.reference) => {
                        node_member.insert(m.reference);
                    }
                    MemberType::Way if changes.ways.contains(&m.reference) => {
                        way_member.insert(m.reference);
                        if area {
                            way_in_area_relation.insert(m.reference);
                        }
                    }
                    _ => {}
                }
            }
        }
        Context {
            changes,
            node_in_way,
            node_member,
            way_member,
            way_in_area_relation,
            affected_ways,
        }
    }

    fn node_in_relation(&self, id: i64) -> bool {
        self.node_member.contains(&id)
    }

    /// A multipolygon is checked when it is changed or a member way is
    /// changed or affected (a moved node).
    fn is_multipolygon_to_check(&self, relation: &OsmRelation) -> bool {
        relation.tags.get("type").map(String::as_str) == Some("multipolygon")
            && (self.changes.relations.contains(&relation.id)
                || relation.members.iter().any(|m| {
                    m.member_type == MemberType::Way && self.affected_ways.contains(&m.reference)
                }))
    }
}

// ---------------------------------------------------------------- ways

fn check_way(way: &OsmWay, cx: &Context, out: &mut Vec<Finding>) {
    let feature = vec![(FeatureKind::Way, way.id)];
    if way.tags.is_empty() && !cx.way_member.contains(&way.id) {
        out.push(finding(
            Check::UntaggedWay,
            tr!("validator.msg.untagged_way").to_string(),
            feature.clone(),
        ));
    }
    if way.nodes.len() < 2 {
        out.push(finding(
            Check::WayTooShort,
            tr!("validator.msg.way_too_short", count = way.nodes.len()).to_string(),
            feature.clone(),
        ));
    } else if way.nodes.first() == way.nodes.last() && way.nodes.len() < 4 {
        out.push(finding(
            Check::ClosedWayTooShort,
            tr!(
                "validator.msg.closed_way_too_short",
                count = way.nodes.len()
            )
            .to_string(),
            feature.clone(),
        ));
    }
    if let Some(pair) = way.nodes.windows(2).find(|p| p[0] == p[1]) {
        out.push(finding(
            Check::DuplicateWayNode,
            tr!("validator.msg.duplicate_way_node", node = pair[0]).to_string(),
            feature.clone(),
        ));
    }
    if way.nodes.len() >= 2 && !way.is_closed() && !cx.way_in_area_relation.contains(&way.id) {
        if let Some(tag) = area_tag(&way.tags) {
            out.push(finding(
                Check::AreaTagsOnOpenWay,
                tr!("validator.msg.area_tags_on_open_way", tag = tag).to_string(),
                feature,
            ));
        }
    }
}

/// Keys that mark a way as a line however it is also tagged.
const LINE_KEYS: [&str; 6] = [
    "highway",
    "railway",
    "waterway",
    "barrier",
    "aerialway",
    "route",
];

/// Keys that, with most of their values, mean an area. Narrower than the
/// vendored iD table ([`crate::presets::area_keys`]), which also lists keys
/// such as `power` or `waterway` that are drawn as lines when open.
const AREA_KEYS: [&str; 9] = [
    "building",
    "building:part",
    "landuse",
    "amenity",
    "shop",
    "office",
    "natural",
    "leisure",
    "tourism",
];

/// Values of an area key that are lines.
const LINE_VALUES: [(&str, &str); 15] = [
    ("natural", "coastline"),
    ("natural", "cliff"),
    ("natural", "ridge"),
    ("natural", "arete"),
    ("natural", "tree_row"),
    ("natural", "valley"),
    ("natural", "gully"),
    ("leisure", "track"),
    ("leisure", "slipway"),
    ("leisure", "picnic_table"),
    ("tourism", "artwork"),
    ("amenity", "bench"),
    ("building", "no"),
    ("shop", "no"),
    ("tourism", "no"),
];

/// The tag that makes an open way an area (`area=yes`, `building=yes`, ...),
/// formatted `key=value`, or `None` when its tags say nothing of the sort.
/// Uses the same table as presets for closed ways, restricted to the keys that
/// are areas without doubt.
fn area_tag(tags: &HashMap<String, String>) -> Option<String> {
    if !crate::presets::area_keys().closed_way_is_area(tags) {
        return None;
    }
    if tags.get("area").map(String::as_str) == Some("yes") {
        return Some("area=yes".to_string());
    }
    if LINE_KEYS.iter().any(|k| tags.contains_key(*k)) {
        return None;
    }
    let mut keys: Vec<&String> = tags.keys().collect();
    keys.sort();
    keys.into_iter()
        .filter(|k| AREA_KEYS.contains(&k.as_str()))
        .find(|k| !LINE_VALUES.contains(&(k.as_str(), tags[*k].as_str())))
        .map(|k| format!("{k}={}", tags[k]))
}

// ----------------------------------------------------------- relations

fn check_relation(relation: &OsmRelation, out: &mut Vec<Finding>) {
    let feature = vec![(FeatureKind::Relation, relation.id)];
    if relation.members.is_empty() {
        out.push(finding(
            Check::RelationEmpty,
            tr!("validator.msg.relation_empty").to_string(),
            feature.clone(),
        ));
    }
    if !relation.tags.contains_key("type") {
        out.push(finding(
            Check::RelationNoType,
            tr!("validator.msg.relation_no_type").to_string(),
            feature,
        ));
    }
}

fn check_multipolygon(data: &OsmData, relation: &OsmRelation, out: &mut Vec<Finding>) {
    let ways: Vec<_> = relation
        .members
        .iter()
        .filter(|m| m.member_type == MemberType::Way)
        .collect();
    if ways.is_empty() {
        return; // an empty relation is reported on its own
    }
    let feature = vec![(FeatureKind::Relation, relation.id)];
    if ways
        .iter()
        .any(|m| !matches!(m.role.as_str(), "outer" | "inner"))
    {
        out.push(finding(
            Check::MultipolygonRole,
            tr!("validator.msg.multipolygon_role").to_string(),
            feature.clone(),
        ));
    }
    // An empty role counts as outer for rendering, so only a relation with
    // neither is missing its outer ring.
    if !ways.iter().any(|m| matches!(m.role.as_str(), "outer" | "")) {
        out.push(finding(
            Check::MultipolygonNoOuter,
            tr!("validator.msg.multipolygon_no_outer").to_string(),
            feature,
        ));
        return;
    }
    let members: Vec<multipolygon::WayMember> = ways
        .iter()
        .map(|m| multipolygon::WayMember {
            way_id: m.reference,
            role: &m.role,
            nodes: data.ways.get(&m.reference).map(|w| w.nodes.as_slice()),
        })
        .collect();
    let coord = |id: i64| data.nodes.get(&id).map(|n| (n.lon, n.lat));
    if multipolygon::assemble(&members, &coord) == Err(multipolygon::Problem::Unclosed) {
        out.push(finding(
            Check::MultipolygonOpenRing,
            tr!("validator.msg.multipolygon_open_ring").to_string(),
            feature,
        ));
    }
}

// ---------------------------------------------------------------- tags

/// Tags well known to be replaced: `(key, value, what to use instead)`.
const DEPRECATED: [(&str, &str, &str); 14] = [
    ("highway", "ford", "ford=yes"),
    ("landuse", "farm", "landuse=farmland"),
    ("amenity", "public_building", "building=public"),
    ("building", "entrance", "entrance=*"),
    ("power", "sub_station", "power=substation"),
    ("natural", "waterfall", "waterway=waterfall"),
    ("highway", "unsurfaced", "highway=track"),
    ("amenity", "swimming_pool", "leisure=swimming_pool"),
    ("leisure", "hackerspace", "amenity=hackerspace"),
    ("waterway", "riverbank", "natural=water + water=river"),
    ("barrier", "wire_fence", "barrier=fence + fence_type=wire"),
    ("shop", "fishmonger", "shop=seafood"),
    ("amenity", "sauna", "leisure=sauna"),
    (
        "tourism",
        "bed_and_breakfast",
        "tourism=guest_house + guest_house=bed_and_breakfast",
    ),
];

/// A key or value cut to a length that fits in a message.
fn short(text: &str) -> String {
    const MAX: usize = 40;
    if text.chars().count() > MAX {
        let mut s: String = text.chars().take(MAX).collect();
        s.push('…');
        s
    } else {
        text.to_string()
    }
}

fn check_tags(kind: FeatureKind, id: i64, tags: &HashMap<String, String>, out: &mut Vec<Finding>) {
    let feature = vec![(kind, id)];
    let mut entries: Vec<(&String, &String)> = tags.iter().collect();
    entries.sort();
    // One finding per check and object, naming the first tag at fault.
    let mut seen: HashSet<Check> = HashSet::new();
    let mut found: Vec<Finding> = Vec::new();
    let mut deprecated: Vec<Finding> = Vec::new();
    let mut add = |check: Check, message: String| {
        if seen.insert(check) {
            found.push(finding(check, message, feature.clone()));
        }
    };
    for (key, value) in entries {
        if key.is_empty() {
            add(
                Check::TagEmptyKey,
                tr!("validator.msg.tag_empty_key", value = short(value)).to_string(),
            );
        } else if value.is_empty() {
            add(
                Check::TagEmptyValue,
                tr!("validator.msg.tag_empty_value", key = short(key)).to_string(),
            );
        }
        if key.chars().count() > MAX_TAG_CHARS || value.chars().count() > MAX_TAG_CHARS {
            add(
                Check::TagTooLong,
                tr!(
                    "validator.msg.tag_too_long",
                    key = short(key),
                    max = MAX_TAG_CHARS
                )
                .to_string(),
            );
        }
        if key.trim() != key || value.trim() != value {
            add(
                Check::TagWhitespace,
                tr!("validator.msg.tag_whitespace", key = short(key.trim())).to_string(),
            );
        }
        if key == "fixme" || key == "FIXME" {
            add(
                Check::Fixme,
                tr!(
                    "validator.msg.fixme",
                    key = short(key),
                    value = short(value)
                )
                .to_string(),
            );
        }
        if let Some((k, v, replacement)) =
            DEPRECATED.iter().find(|(k, v, _)| key == k && value == v)
        {
            // Several deprecated tags on one object are each reported.
            deprecated.push(finding(
                Check::DeprecatedTag,
                tr!(
                    "validator.msg.deprecated_tag",
                    old = format!("{k}={v}"),
                    new = replacement
                )
                .to_string(),
                feature.clone(),
            ));
        }
    }
    out.extend(found);
    out.extend(deprecated);
}

// ----------------------------------------------------- duplicate nodes

fn position_key(lat: f64, lon: f64) -> (i64, i64) {
    (
        (lat * POSITION_STEPS).round() as i64,
        (lon * POSITION_STEPS).round() as i64,
    )
}

/// Groups of distinct nodes at one position where at least one is changed.
/// A hash of the changed nodes' positions is matched against every node once,
/// so the cost is linear in the size of the data.
fn check_duplicate_nodes(data: &OsmData, changed: &[i64], out: &mut Vec<Finding>) {
    let mut at: HashMap<(i64, i64), Vec<i64>> = HashMap::with_capacity(changed.len());
    for id in changed {
        if let Some(n) = data.nodes.get(id) {
            at.entry(position_key(n.lat, n.lon)).or_default().push(*id);
        }
    }
    if at.is_empty() {
        return;
    }
    let changed_set: HashSet<i64> = changed.iter().copied().collect();
    for n in data.nodes.values() {
        if changed_set.contains(&n.id) {
            continue;
        }
        if let Some(group) = at.get_mut(&position_key(n.lat, n.lon)) {
            group.push(n.id);
        }
    }
    for mut group in at.into_values() {
        if group.len() < 2 {
            continue;
        }
        group.sort_unstable();
        out.push(finding(
            Check::DuplicateNodes,
            tr!("validator.msg.duplicate_nodes", count = group.len()).to_string(),
            group
                .into_iter()
                .map(|id| (FeatureKind::Node, id))
                .collect(),
        ));
    }
}

// ------------------------------------------------------------ crossings

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossable {
    Highway,
    Railway,
    Waterway,
    Building,
}

impl Crossable {
    fn name(self) -> String {
        match self {
            Crossable::Highway => tr!("validator.kind.highway"),
            Crossable::Railway => tr!("validator.kind.railway"),
            Crossable::Waterway => tr!("validator.kind.waterway"),
            Crossable::Building => tr!("validator.kind.building"),
        }
        .to_string()
    }
}

/// Whether a crossing between these kinds is worth a warning.
fn crossing_matters(a: Crossable, b: Crossable) -> bool {
    use Crossable::*;
    matches!(
        (a, b),
        (Highway, Highway)
            | (Highway, Railway)
            | (Railway, Highway)
            | (Highway, Waterway)
            | (Waterway, Highway)
            | (Building, Highway)
            | (Highway, Building)
            | (Building, Building)
    )
}

fn has_value(tags: &HashMap<String, String>, key: &str) -> bool {
    tags.get(key).is_some_and(|v| v != "no")
}

/// What a way is for crossing purposes, if it is a linear (or building)
/// feature the check covers. Roads, tracks and rails that are not built
/// yet or no longer used are left out, as are area highways and trams (which
/// run in the road).
fn crossable(tags: &HashMap<String, String>) -> Option<Crossable> {
    if has_value(tags, "building") {
        return Some(Crossable::Building);
    }
    if tags.get("area").map(String::as_str) == Some("yes") {
        return None;
    }
    if let Some(v) = tags.get("highway") {
        let ignored = [
            "proposed",
            "construction",
            "abandoned",
            "razed",
            "disused",
            "platform",
            "services",
            "rest_area",
            "corridor",
            "elevator",
            "ford",
        ];
        return (!ignored.contains(&v.as_str())).then_some(Crossable::Highway);
    }
    if let Some(v) = tags.get("railway") {
        let rails = [
            "rail",
            "light_rail",
            "narrow_gauge",
            "preserved",
            "monorail",
            "funicular",
            "miniature",
        ];
        return rails.contains(&v.as_str()).then_some(Crossable::Railway);
    }
    if let Some(v) = tags.get("waterway") {
        let streams = ["river", "stream", "canal", "drain", "ditch", "brook"];
        return streams.contains(&v.as_str()).then_some(Crossable::Waterway);
    }
    None
}

struct WayGeom {
    id: i64,
    kind: Crossable,
    layer: i32,
    /// On a bridge, in a tunnel or covered: crossing is expected.
    separated: bool,
    /// A road through a ford may cross a waterway.
    ford: bool,
    nodes: Vec<i64>,
    pts: Vec<(f64, f64)>,
    /// `[min_x, min_y, max_x, max_y]` in (lon, lat).
    bbox: [f64; 4],
}

fn way_geom(way: &OsmWay, data: &OsmData) -> Option<WayGeom> {
    let kind = crossable(&way.tags)?;
    let mut pts = Vec::with_capacity(way.nodes.len());
    for id in &way.nodes {
        let n = data.nodes.get(id)?; // partly downloaded: can't tell
        pts.push((n.lon, n.lat));
    }
    if pts.len() < 2 {
        return None;
    }
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(x, y) in &pts {
        bbox = [
            bbox[0].min(x),
            bbox[1].min(y),
            bbox[2].max(x),
            bbox[3].max(y),
        ];
    }
    let tags = &way.tags;
    Some(WayGeom {
        id: way.id,
        kind,
        layer: tags
            .get("layer")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0),
        separated: has_value(tags, "bridge")
            || has_value(tags, "tunnel")
            || tags.get("covered").map(String::as_str) == Some("yes"),
        ford: tags.get("ford").map(String::as_str) == Some("yes"),
        nodes: way.nodes.clone(),
        pts,
        bbox,
    })
}

fn boxes_overlap(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn orient(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Whether two ways have a pair of segments that cross strictly inside both,
/// where the two segments share no node.
fn ways_cross(a: &WayGeom, b: &WayGeom) -> bool {
    for i in 0..a.pts.len() - 1 {
        let (p1, p2) = (a.pts[i], a.pts[i + 1]);
        let sa = [
            p1.0.min(p2.0),
            p1.1.min(p2.1),
            p1.0.max(p2.0),
            p1.1.max(p2.1),
        ];
        if !boxes_overlap(&sa, &b.bbox) {
            continue;
        }
        for j in 0..b.pts.len() - 1 {
            let (q1, q2) = (b.pts[j], b.pts[j + 1]);
            let sb = [
                q1.0.min(q2.0),
                q1.1.min(q2.1),
                q1.0.max(q2.0),
                q1.1.max(q2.1),
            ];
            if !boxes_overlap(&sa, &sb) {
                continue;
            }
            let (a1, a2, b1, b2) = (a.nodes[i], a.nodes[i + 1], b.nodes[j], b.nodes[j + 1]);
            if a1 == b1 || a1 == b2 || a2 == b1 || a2 == b2 {
                continue;
            }
            let (o1, o2) = (orient(p1, p2, q1), orient(p1, p2, q2));
            let (o3, o4) = (orient(q1, q2, p1), orient(q1, q2, p2));
            if o1 * o2 < 0.0 && o3 * o4 < 0.0 {
                return true;
            }
        }
    }
    false
}

type BoxEntry = GeomWithData<Rectangle<[f64; 2]>, usize>;

fn check_crossings(data: &OsmData, cx: &Context, out: &mut Vec<Finding>) {
    if cx.affected_ways.is_empty() {
        return;
    }
    // Bounding boxes of every way the check covers, so changed ways can be
    // matched against ones nobody touched. (Cheap: tags are tested first.)
    let mut geoms: Vec<WayGeom> = data
        .ways
        .values()
        .filter_map(|w| way_geom(w, data))
        .collect();
    geoms.sort_by_key(|g| g.id);
    let entries: Vec<BoxEntry> = geoms
        .iter()
        .enumerate()
        .map(|(i, g)| {
            GeomWithData::new(
                Rectangle::from_corners([g.bbox[0], g.bbox[1]], [g.bbox[2], g.bbox[3]]),
                i,
            )
        })
        .collect();
    let tree = RTree::bulk_load(entries);

    let mut reported: HashSet<(i64, i64)> = HashSet::new();
    for (i, g) in geoms.iter().enumerate() {
        if !cx.affected_ways.contains(&g.id) || g.separated {
            continue;
        }
        let env = AABB::from_corners([g.bbox[0], g.bbox[1]], [g.bbox[2], g.bbox[3]]);
        let mut others: Vec<usize> = tree
            .locate_in_envelope_intersecting(env)
            .map(|e| e.data)
            .filter(|&j| j != i)
            .collect();
        others.sort_unstable();
        for j in others {
            let h = &geoms[j];
            if h.separated
                || h.layer != g.layer
                || !crossing_matters(g.kind, h.kind)
                || ((g.ford || h.ford)
                    && matches!(
                        (g.kind, h.kind),
                        (Crossable::Highway, Crossable::Waterway)
                            | (Crossable::Waterway, Crossable::Highway)
                    ))
            {
                continue;
            }
            let pair = (g.id.min(h.id), g.id.max(h.id));
            if reported.contains(&pair) || !ways_cross(g, h) {
                continue;
            }
            reported.insert(pair);
            let (first, second) = if g.id <= h.id { (g, h) } else { (h, g) };
            out.push(finding(
                Check::CrossingWays,
                tr!(
                    "validator.msg.crossing_ways",
                    a = first.kind.name(),
                    b = second.kind.name()
                )
                .to_string(),
                vec![(FeatureKind::Way, first.id), (FeatureKind::Way, second.id)],
            ));
        }
    }
}

// -------------------------------------------------------------- report

/// A finding together with the layer it was found in and a name for each
/// object concerned, ready to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub layer: LayerId,
    pub finding: Finding,
    /// What to call the objects, e.g. `Way 12 (Main Street)`.
    pub label: String,
}

/// The findings of every layer with pending changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Errors first, then warnings (each layer's order is kept).
    pub entries: Vec<Entry>,
    /// Whether anything was pending, so "no problems" can say what it checked.
    pub checked_changes: bool,
}

impl Report {
    /// Add one layer's findings, naming their objects from `data`.
    pub fn add_layer(&mut self, layer: LayerId, data: &OsmData, diff: &LayerDiff) {
        if diff.is_empty() {
            return;
        }
        self.checked_changes = true;
        for finding in validate_diff(data, diff) {
            let label = label_for(data, &finding.features);
            self.entries.push(Entry {
                layer,
                finding,
                label,
            });
        }
        self.entries.sort_by_key(|e| e.finding.severity);
    }

    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.entries
            .iter()
            .filter(|e| e.finding.severity == severity)
            .count()
    }

    pub fn has_errors(&self) -> bool {
        self.errors() > 0
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many findings a check produced.
    pub fn count_of(&self, check: Check) -> usize {
        self.entries
            .iter()
            .filter(|e| e.finding.check == check)
            .count()
    }
}

/// Name one object: `Node 5`, or `Way 12 (Main Street)` when it has a name.
pub fn feature_label(data: &OsmData, kind: FeatureKind, id: i64) -> String {
    let kind_name = match kind {
        FeatureKind::Node => tr!("validator.feature.node"),
        FeatureKind::Way => tr!("validator.feature.way"),
        FeatureKind::Relation => tr!("validator.feature.relation"),
    };
    let tags = match kind {
        FeatureKind::Node => data.nodes.get(&id).map(|n| &n.tags),
        FeatureKind::Way => data.ways.get(&id).map(|w| &w.tags),
        FeatureKind::Relation => data.relations.iter().find(|r| r.id == id).map(|r| &r.tags),
    };
    let name = tags
        .and_then(|t| t.get("name"))
        .map(|n| n.trim())
        .filter(|n| !n.is_empty());
    match name {
        Some(name) => tr!(
            "validator.feature.named",
            kind = kind_name,
            id = id,
            name = short(name)
        )
        .to_string(),
        None => tr!("validator.feature.plain", kind = kind_name, id = id).to_string(),
    }
}

/// Name the objects of a finding: the first two, then how many more.
fn label_for(data: &OsmData, features: &[Feature]) -> String {
    let mut parts: Vec<String> = features
        .iter()
        .take(2)
        .map(|&(kind, id)| feature_label(data, kind, id))
        .collect();
    if features.len() > 2 {
        parts.push(tr!("validator.feature.more", count = features.len() - 2).to_string());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests;
