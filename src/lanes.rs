//! A road's lanes as its tags describe them: how many lanes run each way
//! (`lanes`, `lanes:forward`, `lanes:backward`, `lanes:both_ways`, oneway
//! rules) and each lane's turn arrows (`turn:lanes*`). Read for the Junction
//! inspector and the Lanes editor, and written back by the editor
//! ([`LaneConfig::tag_changes`]).
//!
//! "Forward" is the way's node order. Lanes are listed left to right as the
//! driver going that way sees them, as `turn:lanes` does.

use crate::tags::Tags;
use std::collections::BTreeMap;

/// One lane's `turn:lanes` arrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arrow {
    Reverse,
    SharpLeft,
    Left,
    SlightLeft,
    MergeToLeft,
    Through,
    MergeToRight,
    SlightRight,
    Right,
    SharpRight,
}

impl Arrow {
    /// The arrows the Lanes editor offers, left to right.
    pub const EDITABLE: [Arrow; 10] = [
        Arrow::Reverse,
        Arrow::SharpLeft,
        Arrow::Left,
        Arrow::SlightLeft,
        Arrow::MergeToLeft,
        Arrow::Through,
        Arrow::MergeToRight,
        Arrow::SlightRight,
        Arrow::Right,
        Arrow::SharpRight,
    ];

    pub fn parse(s: &str) -> Option<Arrow> {
        Some(match s.trim() {
            "sharp_left" => Arrow::SharpLeft,
            "left" => Arrow::Left,
            "slight_left" => Arrow::SlightLeft,
            "through" => Arrow::Through,
            "slight_right" => Arrow::SlightRight,
            "right" => Arrow::Right,
            "sharp_right" => Arrow::SharpRight,
            "reverse" => Arrow::Reverse,
            "merge_to_left" => Arrow::MergeToLeft,
            "merge_to_right" => Arrow::MergeToRight,
            _ => return None,
        })
    }

    /// The tag value.
    pub fn as_str(self) -> &'static str {
        match self {
            Arrow::SharpLeft => "sharp_left",
            Arrow::Left => "left",
            Arrow::SlightLeft => "slight_left",
            Arrow::Through => "through",
            Arrow::SlightRight => "slight_right",
            Arrow::Right => "right",
            Arrow::SharpRight => "sharp_right",
            Arrow::Reverse => "reverse",
            Arrow::MergeToLeft => "merge_to_left",
            Arrow::MergeToRight => "merge_to_right",
        }
    }

    /// The direction the arrow head points, in degrees clockwise from
    /// straight ahead.
    pub fn angle(self) -> f32 {
        match self {
            Arrow::SharpLeft => -135.0,
            Arrow::Left => -90.0,
            Arrow::SlightLeft => -45.0,
            Arrow::Through => 0.0,
            Arrow::SlightRight => 45.0,
            Arrow::Right => 90.0,
            Arrow::SharpRight => 135.0,
            Arrow::Reverse => -180.0,
            Arrow::MergeToLeft => -20.0,
            Arrow::MergeToRight => 20.0,
        }
    }
}

/// A lane's arrows; empty for `none` or an empty field.
pub type LaneArrows = Vec<Arrow>;

/// How a lane count was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountSource {
    /// `lanes:<dir>`, or `lanes` on a oneway.
    Tagged,
    /// Half of an even `lanes`, or the number of `turn:lanes` fields.
    Inferred,
    /// Not known: drawn as one lane.
    Unknown,
}

/// The lanes of one travel direction.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneGroup {
    pub count: u32,
    pub source: CountSource,
    /// Per lane, left to right as the driver sees them; `None` without
    /// `turn:lanes`.
    pub arrows: Option<Vec<LaneArrows>>,
    /// The tag `arrows` came from, for messages.
    pub arrows_key: Option<String>,
}

/// Whether a way is a road for cars, whose lanes and junctions matter here.
pub fn is_road(tags: &Tags) -> bool {
    matches!(
        tags.get("highway"),
        Some(
            "motorway"
                | "trunk"
                | "primary"
                | "secondary"
                | "tertiary"
                | "unclassified"
                | "residential"
                | "service"
                | "living_street"
                | "road"
                | "busway"
                | "motorway_link"
                | "trunk_link"
                | "primary_link"
                | "secondary_link"
                | "tertiary_link"
        )
    )
}

/// `Some(true)` oneway with the node order, `Some(false)` against it, `None`
/// two-way.
pub fn oneway(tags: &Tags) -> Option<bool> {
    match tags.get("oneway") {
        Some("yes" | "1" | "true") => return Some(true),
        Some("-1" | "reverse") => return Some(false),
        Some("no" | "0" | "false") => return None,
        _ => {}
    }
    let implied = tags.get("highway") == Some("motorway")
        || matches!(tags.get("junction"), Some("roundabout" | "circular"));
    implied.then_some(true)
}

fn parse_turn_lanes(value: &str) -> Vec<LaneArrows> {
    value
        .split('|')
        .map(|lane| lane.split(';').filter_map(Arrow::parse).collect())
        .collect()
}

/// A `turn:lanes` value: lanes joined by `|`, a lane's arrows by `;` in
/// left-to-right order, `none` for a lane without.
pub fn format_turn_lanes(lanes: &[LaneArrows]) -> String {
    lanes
        .iter()
        .map(|lane| {
            if lane.is_empty() {
                "none".to_string()
            } else {
                let mut arrows = lane.clone();
                arrows.sort();
                arrows.dedup();
                arrows
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(";")
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn parse_count(tags: &Tags, key: &str) -> Option<u32> {
    tags.get(key)?.trim().parse().ok()
}

/// The lanes of the way's `forward` (with the node order) or `backward`
/// direction, or `None` if a oneway forbids that direction.
pub fn lane_group(tags: &Tags, forward: bool) -> Option<LaneGroup> {
    let dir = if forward { "forward" } else { "backward" };
    let ow = oneway(tags);
    if ow.is_some_and(|with| with != forward) {
        return None;
    }
    let dir_key = |base: &str| format!("{base}:{dir}");
    let (arrows_key, arrows) = [dir_key("turn:lanes")]
        .into_iter()
        .chain(ow.map(|_| "turn:lanes".to_string()))
        .find_map(|k| tags.get(&k).map(|v| (k.clone(), parse_turn_lanes(v))))
        .unzip();

    let explicit = parse_count(tags, &dir_key("lanes"))
        .or_else(|| ow.and_then(|_| parse_count(tags, "lanes")));
    let (count, source) = if let Some(n) = explicit {
        (n, CountSource::Tagged)
    } else if let Some(n) = parse_count(tags, "lanes")
        .map(|n| n.saturating_sub(parse_count(tags, "lanes:both_ways").unwrap_or(0)))
        .filter(|n| n % 2 == 0 && *n > 0)
    {
        (n / 2, CountSource::Inferred)
    } else if let Some(a) = &arrows {
        (a.len() as u32, CountSource::Inferred)
    } else {
        (1, CountSource::Unknown)
    };
    Some(LaneGroup {
        count: count.max(1),
        source,
        arrows,
        arrows_key,
    })
}

/// `lanes:forward + lanes:backward (+ lanes:both_ways)` against `lanes`, for
/// a way's tags. Returns the message when they disagree.
pub fn lanes_split_issue(tags: &Tags) -> Option<String> {
    let total = parse_count(tags, "lanes")?;
    let f = parse_count(tags, "lanes:forward")?;
    let b = parse_count(tags, "lanes:backward")?;
    let both = parse_count(tags, "lanes:both_ways").unwrap_or(0);
    (f + b + both != total).then(|| {
        format!(
            "lanes={total} but lanes:forward={f} + lanes:backward={b}{} adds up to {}",
            if both > 0 {
                format!(" + lanes:both_ways={both}")
            } else {
                String::new()
            },
            f + b + both
        )
    })
}

/// Which lanes of a road an edit or a diagram row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// Lanes driving with the node order.
    Forward,
    /// Lanes driving against the node order.
    Backward,
    /// Shared centre turn lanes (`:both_ways`).
    BothWays,
}

impl Group {
    /// The key suffix on a two-way road.
    fn suffix(self) -> &'static str {
        match self {
            Group::Forward => "forward",
            Group::Backward => "backward",
            Group::BothWays => "both_ways",
        }
    }
}

/// A per-lane `*:lanes` access tag the editor offers besides `turn:lanes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Bicycle,
    Vehicle,
    Cycleway,
    Bus,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Bicycle,
        Category::Vehicle,
        Category::Cycleway,
        Category::Bus,
    ];

    /// The tag key without a direction.
    pub fn key(self) -> &'static str {
        match self {
            Category::Bicycle => "bicycle:lanes",
            Category::Vehicle => "vehicle:lanes",
            Category::Cycleway => "cycleway:lanes",
            Category::Bus => "psv:lanes",
        }
    }

    /// The values the editor offers.
    pub fn values(self) -> &'static [&'static str] {
        match self {
            Category::Bicycle => &["yes", "designated", "no", "permissive", "dismount"],
            Category::Vehicle => &["yes", "no", "designated", "permissive", "hov"],
            Category::Cycleway => &["no", "lane", "share_busway", "track"],
            Category::Bus => &["yes", "designated", "no", "permissive"],
        }
    }
}

/// The lanes of one group, as the editor changes them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirLanes {
    /// Every painted lane, including ones closed to motor vehicles.
    pub count: u32,
    /// Per lane; `None` when the group has no `turn:lanes`.
    pub arrows: Option<Vec<LaneArrows>>,
    /// Per lane, one value per [`Category`] (empty = unspecified).
    pub attrs: BTreeMap<Category, Vec<String>>,
}

impl DirLanes {
    /// The value of `cat` for `lane`, or "".
    pub fn attr(&self, cat: Category, lane: usize) -> &str {
        self.attrs
            .get(&cat)
            .and_then(|v| v.get(lane))
            .map_or("", String::as_str)
    }

    /// Lanes that carry motor traffic: those not marked `vehicle=no`. This is
    /// what `lanes=*` counts.
    pub fn vehicle_count(&self) -> u32 {
        (0..self.count as usize)
            .filter(|&i| self.attr(Category::Vehicle, i) != "no")
            .count() as u32
    }

    fn resize(&mut self) {
        let n = self.count as usize;
        if let Some(a) = &mut self.arrows {
            a.resize(n, Vec::new());
        }
        for v in self.attrs.values_mut() {
            v.resize(n, String::new());
        }
    }
}

/// Where cars stand beside the road (`parking:<side>`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParkingSide {
    /// `lane`, `street_side`, ... or "" for none.
    pub kind: String,
    /// `parallel`, `diagonal`, `perpendicular` or "".
    pub orientation: String,
}

/// Parking types the editor offers.
pub const PARKING_KINDS: [&str; 8] = [
    "lane",
    "street_side",
    "half_on_kerb",
    "on_kerb",
    "shoulder",
    "separate",
    "yes",
    "no",
];
/// Parking orientations the editor offers.
pub const PARKING_ORIENTATIONS: [&str; 3] = ["parallel", "diagonal", "perpendicular"];

/// Whether vehicles actually stand beside the road, the only parking an
/// orientation applies to and the only one drawn as a band.
pub fn parking_is_physical(kind: &str) -> bool {
    matches!(
        kind,
        "lane" | "street_side" | "half_on_kerb" | "on_kerb" | "shoulder" | "yes"
    )
}

/// A side of the way, looking along its node order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn name(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

/// A cycle lane against a oneway (`oneway:bicycle=no`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contraflow {
    pub side: Side,
    pub kind: ContraflowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContraflowKind {
    /// `cycleway:<side>=lane`
    Lane,
    /// `cycleway:<side>=track`
    Track,
    /// Only `oneway:bicycle=no`: no separate lane.
    Shared,
}

impl ContraflowKind {
    fn tag(self) -> Option<&'static str> {
        match self {
            ContraflowKind::Lane => Some("lane"),
            ContraflowKind::Track => Some("track"),
            ContraflowKind::Shared => None,
        }
    }
}

/// A way's whole lane layout, as the Lanes editor shows and changes it.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneConfig {
    /// With the node order; `None` when a oneway forbids it.
    pub forward: Option<DirLanes>,
    /// Against the node order; `None` when a oneway forbids it.
    pub backward: Option<DirLanes>,
    /// Shared centre turn lanes (`lanes:both_ways`); `count` 0 for none.
    pub both_ways: DirLanes,
    /// Contraflow cycling on a oneway.
    pub contraflow: Option<Contraflow>,
    pub parking_left: ParkingSide,
    pub parking_right: ParkingSide,
}

/// Lane edits the editor offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneEdit {
    /// One more lane (`add`) or one fewer in a group. A lane is added or
    /// removed on the driver's right.
    Count { group: Group, add: bool },
    /// Turn `arrow` on or off for lane `lane` (left to right) of a group.
    Arrow {
        group: Group,
        lane: usize,
        arrow: Arrow,
    },
    /// Set a lane's access value for `cat`, or clear it (`value` "") when it
    /// already has that value.
    Attr {
        group: Group,
        lane: usize,
        cat: Category,
        value: String,
    },
    /// Set (or with `None`, remove) contraflow cycling.
    Contraflow(Option<Contraflow>),
    /// Set a side's parking; choosing the current type clears it.
    Parking {
        side: Side,
        kind: String,
        orientation: String,
    },
}

fn read_list(tags: &Tags, key: &str, count: usize) -> Option<Vec<String>> {
    tags.get(key).map(|v| {
        let mut l: Vec<String> = v.split('|').map(|s| s.trim().to_string()).collect();
        l.resize(count.max(l.len()), String::new());
        l
    })
}

/// Reads `base` for a group: the directional key, or on a oneway also the
/// plain one.
fn group_key(tags: &Tags, base: &str, group: Group, oneway: bool) -> Option<String> {
    let k = format!("{base}:{}", group.suffix());
    if tags.get(&k).is_some() {
        return Some(k);
    }
    (oneway && group != Group::BothWays && tags.get(base).is_some()).then(|| base.to_string())
}

fn read_parking(tags: &Tags, side: Side) -> ParkingSide {
    let get = |key: String| tags.get(&key).map(str::to_string);
    let both = |suffix: &str| get(format!("parking:both{suffix}"));
    let own = |suffix: &str| get(format!("parking:{}{suffix}", side.name()));
    ParkingSide {
        kind: own("").or_else(|| both("")).unwrap_or_default(),
        orientation: own(":orientation")
            .or_else(|| both(":orientation"))
            .unwrap_or_default(),
    }
}

impl LaneConfig {
    /// The layout `tags` describe.
    pub fn read(tags: &Tags) -> LaneConfig {
        let ow = oneway(tags);
        let one_way = ow.is_some();
        let group = |g: Group| -> DirLanes {
            let base = match g {
                Group::BothWays => {
                    let n = parse_count(tags, "lanes:both_ways").unwrap_or(0);
                    DirLanes {
                        count: n,
                        ..Default::default()
                    }
                }
                _ => match lane_group(tags, g == Group::Forward) {
                    Some(lg) => DirLanes {
                        count: lg.count,
                        arrows: lg.arrows,
                        attrs: BTreeMap::new(),
                    },
                    None => DirLanes::default(),
                },
            };
            let mut d = base;
            if g == Group::BothWays {
                if let Some(v) = tags.get("turn:lanes:both_ways") {
                    d.arrows = Some(parse_turn_lanes(v));
                }
            }
            for cat in Category::ALL {
                if let Some(k) = group_key(tags, cat.key(), g, one_way) {
                    if let Some(list) = read_list(tags, &k, d.count as usize) {
                        d.attrs.insert(cat, list);
                    }
                }
            }
            // Lanes closed to cars aren't in `lanes`, so the lists can run
            // longer than the tagged count.
            let longest = d
                .attrs
                .values()
                .map(Vec::len)
                .chain(d.arrows.iter().map(Vec::len))
                .max()
                .unwrap_or(0) as u32;
            if g == Group::BothWays || d.attrs.contains_key(&Category::Vehicle) {
                d.count = d.count.max(longest);
            } else if longest > d.count && d.arrows.is_none() {
                d.count = longest;
            }
            d.resize();
            d
        };
        let forward = lane_group(tags, true).map(|_| group(Group::Forward));
        let backward = lane_group(tags, false).map(|_| group(Group::Backward));
        let contraflow =
            (ow == Some(true) && tags.get("oneway:bicycle") == Some("no")).then(|| {
                let side = [Side::Left, Side::Right]
                    .into_iter()
                    .find(|s| {
                        matches!(
                            tags.get(&format!("cycleway:{}", s.name())),
                            Some("lane" | "track")
                        )
                    })
                    .unwrap_or(Side::Left);
                let kind = match tags.get(&format!("cycleway:{}", side.name())) {
                    Some("lane") => ContraflowKind::Lane,
                    Some("track") => ContraflowKind::Track,
                    _ => ContraflowKind::Shared,
                };
                Contraflow { side, kind }
            });
        LaneConfig {
            forward,
            backward,
            both_ways: group(Group::BothWays),
            contraflow,
            parking_left: read_parking(tags, Side::Left),
            parking_right: read_parking(tags, Side::Right),
        }
    }

    /// The lanes of a group.
    pub fn group(&self, g: Group) -> Option<&DirLanes> {
        match g {
            Group::Forward => self.forward.as_ref(),
            Group::Backward => self.backward.as_ref(),
            Group::BothWays => Some(&self.both_ways),
        }
    }

    fn group_mut(&mut self, g: Group) -> Option<&mut DirLanes> {
        match g {
            Group::Forward => self.forward.as_mut(),
            Group::Backward => self.backward.as_mut(),
            Group::BothWays => Some(&mut self.both_ways),
        }
    }

    /// Apply `edit`; `false` (and nothing changed) when it can't be: a
    /// direction the road doesn't have, removing a direction's last lane, or
    /// a contraflow lane on a road that isn't a oneway.
    pub fn apply(&mut self, edit: LaneEdit) -> bool {
        match edit {
            LaneEdit::Count { group, add } => {
                let min = if group == Group::BothWays { 0 } else { 1 };
                let Some(d) = self.group_mut(group) else {
                    return false;
                };
                if add {
                    d.count += 1;
                } else if d.count > min {
                    d.count -= 1;
                } else {
                    return false;
                }
                d.resize();
                true
            }
            LaneEdit::Arrow { group, lane, arrow } => {
                let Some(d) = self.group_mut(group) else {
                    return false;
                };
                if lane >= d.count as usize {
                    return false;
                }
                let count = d.count as usize;
                let arrows = d.arrows.get_or_insert_with(|| vec![Vec::new(); count]);
                let l = &mut arrows[lane];
                match l.iter().position(|&a| a == arrow) {
                    Some(i) => {
                        l.remove(i);
                    }
                    None => {
                        l.push(arrow);
                        l.sort();
                    }
                }
                true
            }
            LaneEdit::Attr {
                group,
                lane,
                cat,
                value,
            } => {
                let Some(d) = self.group_mut(group) else {
                    return false;
                };
                if lane >= d.count as usize {
                    return false;
                }
                let n = d.count as usize;
                let list = d.attrs.entry(cat).or_insert_with(|| vec![String::new(); n]);
                list[lane] = if list[lane] == value {
                    String::new()
                } else {
                    value
                };
                true
            }
            LaneEdit::Contraflow(c) => {
                // Only a oneway has a contraflow.
                if c.is_some() && self.forward.is_some() == self.backward.is_some() {
                    return false;
                }
                self.contraflow = c;
                true
            }
            LaneEdit::Parking {
                side,
                kind,
                orientation,
            } => {
                let p = match side {
                    Side::Left => &mut self.parking_left,
                    Side::Right => &mut self.parking_right,
                };
                if !kind.is_empty() {
                    *p = if p.kind == kind {
                        ParkingSide::default()
                    } else {
                        ParkingSide {
                            orientation: if parking_is_physical(&kind) {
                                p.orientation.clone()
                            } else {
                                String::new()
                            },
                            kind,
                        }
                    };
                } else if parking_is_physical(&p.kind) {
                    p.orientation = if p.orientation == orientation {
                        String::new()
                    } else {
                        orientation
                    };
                }
                true
            }
        }
    }

    /// The tag changes that make `tags` describe this layout: `(key, new
    /// value or None to remove)`, only keys whose value changes. On a oneway
    /// the plain `lanes` / `turn:lanes` hold the open direction; a two-way
    /// road gets `lanes:forward` / `lanes:backward` unless both are equal and
    /// weren't tagged, and `turn:lanes:forward` / `:backward`. A group
    /// whose lanes all lack a value has no such tag. `lanes` counts only
    /// lanes not marked `vehicle=no`.
    pub fn tag_changes(&self, tags: &Tags) -> Vec<(String, Option<String>)> {
        let mut want: Vec<(String, Option<String>)> = Vec::new();
        let mut put = |k: String, v: Option<String>| want.push((k, v));
        let arrows_value = |d: &DirLanes| {
            d.arrows
                .as_ref()
                .filter(|a| d.count > 0 && a.iter().any(|l| !l.is_empty()))
                .map(|a| format_turn_lanes(a))
        };
        let attr_value = |d: &DirLanes, cat: Category| {
            let list = d.attrs.get(&cat)?;
            (d.count > 0 && list.iter().any(|v| !v.is_empty())).then(|| list.join("|"))
        };
        let two_way = self.forward.is_some() && self.backward.is_some();
        let base = LaneConfig::read(tags);
        let lanes_changed = (&self.forward, &self.backward, &self.both_ways)
            != (&base.forward, &base.backward, &base.both_ways);
        match (&self.forward, &self.backward) {
            _ if !lanes_changed => {}
            (Some(d), None) | (None, Some(d)) => {
                put("lanes".into(), Some(d.vehicle_count().to_string()));
                put("turn:lanes".into(), arrows_value(d));
                for cat in Category::ALL {
                    put(cat.key().into(), attr_value(d, cat));
                }
                for g in ["forward", "backward", "both_ways"] {
                    put(format!("lanes:{g}"), None);
                    put(format!("turn:lanes:{g}"), None);
                    for cat in Category::ALL {
                        put(format!("{}:{g}", cat.key()), None);
                    }
                }
            }
            (Some(f), Some(b)) => {
                let bw = &self.both_ways;
                let (fc, bc, wc) = (f.vehicle_count(), b.vehicle_count(), bw.vehicle_count());
                put("lanes".into(), Some((fc + bc + wc).to_string()));
                let was_split =
                    tags.get("lanes:forward").is_some() || tags.get("lanes:backward").is_some();
                let split = was_split || fc != bc || wc > 0;
                put("lanes:forward".into(), split.then(|| fc.to_string()));
                put("lanes:backward".into(), split.then(|| bc.to_string()));
                put("lanes:both_ways".into(), (wc > 0).then(|| wc.to_string()));
                // A plain `*:lanes` on a two-way road is replaced once any
                // directional value is written.
                let mut series: Vec<(&str, Box<dyn Fn(&DirLanes) -> Option<String> + '_>)> =
                    vec![("turn:lanes", Box::new(&arrows_value))];
                for cat in Category::ALL {
                    series.push((cat.key(), Box::new(move |d| attr_value(d, cat))));
                }
                for (base, value) in &series {
                    let vals = [(&f, "forward"), (&b, "backward"), (&bw, "both_ways")]
                        .map(|(d, g)| (g, value(d)));
                    let existing = ["forward", "backward", "both_ways"]
                        .iter()
                        .any(|g| tags.get(&format!("{base}:{g}")).is_some());
                    if existing || vals.iter().any(|(_, v)| v.is_some()) {
                        put((*base).into(), None);
                    }
                    for (g, v) in vals {
                        put(format!("{base}:{g}"), v);
                    }
                }
            }
            (None, None) => {}
        }
        // Contraflow cycling, on a oneway with the node order only.
        if oneway(tags) == Some(true) && !two_way {
            let (cf, prev) = (self.contraflow, LaneConfig::read(tags).contraflow);
            put("oneway:bicycle".into(), cf.map(|_| "no".to_string()));
            for side in [Side::Left, Side::Right] {
                let tag_of =
                    |c: Option<Contraflow>| c.filter(|c| c.side == side).and_then(|c| c.kind.tag());
                let (mine, was) = (tag_of(cf), tag_of(prev));
                if mine.is_some() || was.is_some() {
                    let k = format!("cycleway:{}", side.name());
                    put(format!("{k}:oneway"), mine.map(|_| "-1".to_string()));
                    put(k, mine.map(str::to_string));
                }
            }
        }
        // Parking.
        let (l, r) = (&self.parking_left, &self.parking_right);
        let side_tags = |side: &str, p: &ParkingSide| -> Vec<(String, Option<String>)> {
            vec![
                (
                    format!("parking:{side}"),
                    (!p.kind.is_empty()).then(|| p.kind.clone()),
                ),
                (
                    format!("parking:{side}:orientation"),
                    (!p.orientation.is_empty() && parking_is_physical(&p.kind))
                        .then(|| p.orientation.clone()),
                ),
            ]
        };
        let blank = vec![
            ("parking:both".to_string(), None),
            ("parking:both:orientation".to_string(), None),
            ("parking:left".to_string(), None),
            ("parking:left:orientation".to_string(), None),
            ("parking:right".to_string(), None),
            ("parking:right:orientation".to_string(), None),
        ];
        let mut parking = blank;
        let mut set = |v: Vec<(String, Option<String>)>| {
            for (k, val) in v {
                if let Some(e) = parking.iter_mut().find(|(ek, _)| *ek == k) {
                    e.1 = val;
                }
            }
        };
        // `both` is the shorthand for two identical sides.
        if !l.kind.is_empty() && l == r {
            set(side_tags("both", l));
        } else {
            set(side_tags("left", l));
            set(side_tags("right", r));
        }
        want.extend(parking);
        // Later entries win; keep the last value per key.
        let mut out: Vec<(String, Option<String>)> = Vec::new();
        for (k, v) in want {
            match out.iter_mut().find(|(ek, _)| *ek == k) {
                Some(e) => e.1 = v,
                None => out.push((k, v)),
            }
        }
        out.into_iter()
            .filter(|(k, v)| tags.get(k) != v.as_deref())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(
        tags: &[(&str, &str)],
        edit: impl FnOnce(&mut LaneConfig),
    ) -> Vec<(String, Option<String>)> {
        let tags = Tags::from(tags);
        let mut c = LaneConfig::read(&tags);
        edit(&mut c);
        let mut out = c.tag_changes(&tags);
        out.sort();
        out
    }

    fn set(k: &str, v: &str) -> (String, Option<String>) {
        (k.to_string(), Some(v.to_string()))
    }

    fn unset(k: &str) -> (String, Option<String>) {
        (k.to_string(), None)
    }

    #[test]
    fn lanes_split_mismatch() {
        let t = Tags::from([
            ("lanes", "4"),
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
        ]);
        assert!(lanes_split_issue(&t).is_some());
        let t = Tags::from([
            ("lanes", "3"),
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
        ]);
        assert!(lanes_split_issue(&t).is_none());
    }

    #[test]
    fn reading_and_writing_back_unchanged_is_no_change() {
        for tags in [
            &[("highway", "primary"), ("lanes", "2")][..],
            &[
                ("highway", "primary"),
                ("lanes", "3"),
                ("lanes:forward", "2"),
                ("lanes:backward", "1"),
                ("turn:lanes:forward", "left|through;right"),
            ][..],
            &[
                ("highway", "primary"),
                ("oneway", "yes"),
                ("lanes", "2"),
                ("turn:lanes", "left|through"),
            ][..],
        ] {
            assert_eq!(changes(tags, |_| {}), [], "{tags:?}");
        }
    }

    #[test]
    fn adding_a_lane_one_way_splits_the_counts() {
        let c = changes(&[("highway", "primary"), ("lanes", "2")], |c| {
            assert!(c.apply(LaneEdit::Count {
                group: Group::Forward,
                add: true
            }));
        });
        assert_eq!(
            c,
            [
                set("lanes", "3"),
                set("lanes:backward", "1"),
                set("lanes:forward", "2")
            ]
        );
    }

    #[test]
    fn arrows_are_toggled_per_lane_and_written_in_order() {
        let c = changes(
            &[
                ("highway", "primary"),
                ("lanes:forward", "2"),
                ("lanes:backward", "1"),
                ("lanes", "3"),
            ],
            |c| {
                for (lane, arrow) in [(0, Arrow::Left), (1, Arrow::Right), (1, Arrow::Through)] {
                    assert!(c.apply(LaneEdit::Arrow {
                        group: Group::Forward,
                        lane,
                        arrow
                    }));
                }
            },
        );
        assert_eq!(c, [set("turn:lanes:forward", "left|through;right")]);

        // Turning the last arrow off removes the tag.
        let c = changes(
            &[
                ("highway", "primary"),
                ("oneway", "yes"),
                ("lanes", "1"),
                ("turn:lanes", "left"),
            ],
            |c| {
                c.apply(LaneEdit::Arrow {
                    group: Group::Forward,
                    lane: 0,
                    arrow: Arrow::Left,
                });
            },
        );
        assert_eq!(c, [unset("turn:lanes")]);
    }

    #[test]
    fn removing_a_lane_drops_its_arrows_and_keeps_one() {
        let c = changes(
            &[
                ("highway", "primary"),
                ("oneway", "yes"),
                ("lanes", "2"),
                ("turn:lanes", "left|through"),
            ],
            |c| {
                assert!(c.apply(LaneEdit::Count {
                    group: Group::Forward,
                    add: false
                }));
                assert!(!c.apply(LaneEdit::Count {
                    group: Group::Forward,
                    add: false
                }));
                // A oneway has no backward lanes to change.
                assert!(!c.apply(LaneEdit::Count {
                    group: Group::Backward,
                    add: true
                }));
            },
        );
        assert_eq!(c, [set("lanes", "1"), set("turn:lanes", "left")]);
    }

    #[test]
    fn a_two_way_plain_turn_lanes_is_replaced_by_the_directional_keys() {
        let c = changes(
            &[
                ("highway", "primary"),
                ("lanes", "2"),
                ("turn:lanes", "left"),
            ],
            |c| {
                c.apply(LaneEdit::Arrow {
                    group: Group::Backward,
                    lane: 0,
                    arrow: Arrow::Through,
                });
            },
        );
        assert_eq!(
            c,
            [unset("turn:lanes"), set("turn:lanes:backward", "through")]
        );
    }

    #[test]
    fn a_bike_only_lane_is_painted_but_not_counted_in_lanes() {
        let tags = &[
            ("highway", "primary"),
            ("oneway", "yes"),
            ("lanes", "2"),
            ("vehicle:lanes", "yes|yes|no"),
            ("bicycle:lanes", "yes|yes|designated"),
        ];
        let c = LaneConfig::read(&Tags::from(tags));
        assert_eq!(c.forward.as_ref().unwrap().count, 3);
        assert_eq!(changes(tags, |_| {}), []);
        // Opening the bike lane to cars makes it count.
        let c = changes(tags, |c| {
            c.apply(LaneEdit::Attr {
                group: Group::Forward,
                lane: 2,
                cat: Category::Vehicle,
                value: "no".into(),
            });
        });
        assert_eq!(c, [set("lanes", "3"), set("vehicle:lanes", "yes|yes|")]);
    }

    #[test]
    fn access_values_toggle_and_use_directional_keys_on_a_two_way_road() {
        let c = changes(&[("highway", "primary"), ("lanes", "2")], |c| {
            for g in [Group::Forward, Group::Backward] {
                c.apply(LaneEdit::Attr {
                    group: g,
                    lane: 0,
                    cat: Category::Bus,
                    value: "designated".into(),
                });
            }
            // Choosing the same value again clears it.
            c.apply(LaneEdit::Attr {
                group: Group::Backward,
                lane: 0,
                cat: Category::Bus,
                value: "designated".into(),
            });
        });
        assert_eq!(c, [set("psv:lanes:forward", "designated")]);
    }

    #[test]
    fn shared_centre_lanes_have_their_own_count_and_arrows() {
        let c = changes(
            &[
                ("highway", "primary"),
                ("lanes", "2"),
                ("lanes:forward", "1"),
                ("lanes:backward", "1"),
            ],
            |c| {
                assert!(c.apply(LaneEdit::Count {
                    group: Group::BothWays,
                    add: true
                }));
                assert!(c.apply(LaneEdit::Arrow {
                    group: Group::BothWays,
                    lane: 0,
                    arrow: Arrow::Left
                }));
            },
        );
        assert_eq!(
            c,
            [
                set("lanes", "3"),
                set("lanes:both_ways", "1"),
                set("turn:lanes:both_ways", "left"),
            ]
        );
        let tags = &[
            ("highway", "primary"),
            ("lanes", "3"),
            ("lanes:forward", "1"),
            ("lanes:backward", "1"),
            ("lanes:both_ways", "1"),
            ("turn:lanes:both_ways", "left"),
        ];
        assert_eq!(changes(tags, |_| {}), []);
        // Removing it drops its tags.
        let c = changes(tags, |c| {
            c.apply(LaneEdit::Count {
                group: Group::BothWays,
                add: false,
            });
        });
        assert!(c.contains(&unset("lanes:both_ways")));
        assert!(c.contains(&unset("turn:lanes:both_ways")));
        assert!(c.contains(&set("lanes", "2")));
    }

    #[test]
    fn merge_arrows_round_trip() {
        let tags = &[
            ("highway", "primary"),
            ("oneway", "yes"),
            ("lanes", "2"),
            ("turn:lanes", "through|merge_to_left"),
        ];
        assert_eq!(changes(tags, |_| {}), []);
    }

    #[test]
    fn contraflow_cycling_on_a_oneway() {
        let base = &[("highway", "residential"), ("oneway", "yes")][..];
        let cf = |side, kind| LaneEdit::Contraflow(Some(Contraflow { side, kind }));
        let c = changes(base, |c| {
            assert!(c.apply(cf(Side::Left, ContraflowKind::Lane)));
        });
        assert_eq!(
            c,
            [
                set("cycleway:left", "lane"),
                set("cycleway:left:oneway", "-1"),
                set("oneway:bicycle", "no"),
            ]
        );
        let c = changes(base, |c| {
            c.apply(cf(Side::Right, ContraflowKind::Shared));
        });
        assert_eq!(c, [set("oneway:bicycle", "no")]);
        let tagged = &[
            ("highway", "residential"),
            ("oneway", "yes"),
            ("oneway:bicycle", "no"),
            ("cycleway:right", "track"),
            ("cycleway:right:oneway", "-1"),
        ];
        assert_eq!(changes(tagged, |_| {}), []);
        let c = changes(tagged, |c| {
            c.apply(LaneEdit::Contraflow(None));
        });
        assert_eq!(
            c,
            [
                unset("cycleway:right"),
                unset("cycleway:right:oneway"),
                unset("oneway:bicycle")
            ]
        );
        // Not on a two-way road.
        let mut two = LaneConfig::read(&Tags::from([("highway", "residential")]));
        assert!(!two.apply(cf(Side::Left, ContraflowKind::Lane)));
    }

    #[test]
    fn parking_collapses_to_both_and_orients_only_physical_parking() {
        let edit = |side, kind: &str, orientation: &str| LaneEdit::Parking {
            side,
            kind: kind.into(),
            orientation: orientation.into(),
        };
        let c = changes(&[("highway", "residential")], |c| {
            c.apply(edit(Side::Left, "lane", ""));
            c.apply(edit(Side::Right, "lane", ""));
            c.apply(edit(Side::Left, "", "parallel"));
            c.apply(edit(Side::Right, "", "parallel"));
        });
        assert_eq!(
            c,
            [
                set("parking:both", "lane"),
                set("parking:both:orientation", "parallel")
            ]
        );
        // Splitting the sides rewrites `both`.
        let c = changes(
            &[("highway", "residential"), ("parking:both", "lane")],
            |c| {
                c.apply(edit(Side::Right, "no", ""));
            },
        );
        assert_eq!(
            c,
            [
                unset("parking:both"),
                set("parking:left", "lane"),
                set("parking:right", "no")
            ]
        );
        // `no` takes no orientation.
        let mut cfg = LaneConfig::read(&Tags::from([("parking:left", "no")]));
        cfg.apply(edit(Side::Left, "", "diagonal"));
        assert_eq!(cfg.parking_left.orientation, "");
    }
}
