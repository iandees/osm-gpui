//! A road's lanes as its tags describe them: how many lanes run each way
//! (`lanes`, `lanes:forward`, `lanes:backward`, `lanes:both_ways`, oneway
//! rules) and each lane's turn arrows (`turn:lanes*`). Read for the Junction
//! inspector and the Lanes editor, and written back by the editor
//! ([`LaneConfig::tag_changes`]).
//!
//! "Forward" is the way's node order. Lanes are listed left to right as the
//! driver going that way sees them, as `turn:lanes` does.

use crate::tags::Tags;

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
    pub const EDITABLE: [Arrow; 8] = [
        Arrow::Reverse,
        Arrow::SharpLeft,
        Arrow::Left,
        Arrow::SlightLeft,
        Arrow::Through,
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

/// The lanes of one direction, as the editor changes them.
#[derive(Debug, Clone, PartialEq)]
pub struct DirLanes {
    pub count: u32,
    /// Per lane; `None` when the direction has no `turn:lanes`.
    pub arrows: Option<Vec<LaneArrows>>,
}

/// A way's whole lane layout, as the Lanes editor shows and changes it.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneConfig {
    /// With the node order; `None` when a oneway forbids it.
    pub forward: Option<DirLanes>,
    /// Against the node order; `None` when a oneway forbids it.
    pub backward: Option<DirLanes>,
    /// Shared centre turn lanes (`lanes:both_ways`).
    pub both_ways: u32,
}

/// Lane edits the editor offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEdit {
    /// One more lane (`true`) or one fewer in a direction (`forward`). A
    /// lane is added or removed on the driver's right.
    Count { forward: bool, add: bool },
    /// Turn `arrow` on or off for lane `lane` (left to right) of a direction.
    Arrow {
        forward: bool,
        lane: usize,
        arrow: Arrow,
    },
}

impl LaneConfig {
    /// The layout `tags` describe.
    pub fn read(tags: &Tags) -> LaneConfig {
        let dir = |forward| {
            lane_group(tags, forward).map(|g| DirLanes {
                count: g.count,
                arrows: g.arrows.map(|mut a| {
                    a.resize(g.count as usize, Vec::new());
                    a
                }),
            })
        };
        LaneConfig {
            forward: dir(true),
            backward: dir(false),
            both_ways: parse_count(tags, "lanes:both_ways").unwrap_or(0),
        }
    }

    fn dir_mut(&mut self, forward: bool) -> Option<&mut DirLanes> {
        if forward {
            self.forward.as_mut()
        } else {
            self.backward.as_mut()
        }
    }

    /// Apply `edit`; `false` (and nothing changed) when it can't be: a
    /// direction the road doesn't have, or removing its last lane.
    pub fn apply(&mut self, edit: LaneEdit) -> bool {
        match edit {
            LaneEdit::Count { forward, add } => {
                let Some(d) = self.dir_mut(forward) else {
                    return false;
                };
                if add {
                    d.count += 1;
                } else if d.count > 1 {
                    d.count -= 1;
                } else {
                    return false;
                }
                if let Some(a) = &mut d.arrows {
                    a.resize(d.count as usize, Vec::new());
                }
                true
            }
            LaneEdit::Arrow {
                forward,
                lane,
                arrow,
            } => {
                let Some(d) = self.dir_mut(forward) else {
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
        }
    }

    /// The tag changes that make `tags` describe this layout: `(key, new
    /// value or None to remove)`, only keys whose value changes. On a oneway
    /// the plain `lanes` / `turn:lanes` hold the open direction; a two-way
    /// road gets `lanes:forward` / `lanes:backward` unless both are equal and
    /// weren't tagged, and `turn:lanes:forward` / `:backward`. A direction
    /// whose lanes all lack arrows has no `turn:lanes`.
    pub fn tag_changes(&self, tags: &Tags) -> Vec<(String, Option<String>)> {
        let mut want: Vec<(&str, Option<String>)> = Vec::new();
        let arrows_value = |d: &DirLanes| {
            d.arrows
                .as_ref()
                .filter(|a| a.iter().any(|l| !l.is_empty()))
                .map(|a| format_turn_lanes(a))
        };
        match (&self.forward, &self.backward) {
            (Some(d), None) | (None, Some(d)) => {
                want.push(("lanes", Some(d.count.to_string())));
                want.push(("turn:lanes", arrows_value(d)));
                for key in [
                    "lanes:forward",
                    "lanes:backward",
                    "lanes:both_ways",
                    "turn:lanes:forward",
                    "turn:lanes:backward",
                ] {
                    want.push((key, None));
                }
            }
            (Some(f), Some(b)) => {
                let total = f.count + b.count + self.both_ways;
                want.push(("lanes", Some(total.to_string())));
                let was_split =
                    tags.get("lanes:forward").is_some() || tags.get("lanes:backward").is_some();
                let split = was_split || f.count != b.count || self.both_ways > 0;
                want.push(("lanes:forward", split.then(|| f.count.to_string())));
                want.push(("lanes:backward", split.then(|| b.count.to_string())));
                want.push((
                    "lanes:both_ways",
                    (self.both_ways > 0).then(|| self.both_ways.to_string()),
                ));
                let (tf, tb) = (arrows_value(f), arrows_value(b));
                // A plain turn:lanes on a two-way road is replaced once
                // either direction is written.
                if tf.is_some() || tb.is_some() || tags.get("turn:lanes:forward").is_some() {
                    want.push(("turn:lanes", None));
                }
                want.push(("turn:lanes:forward", tf));
                want.push(("turn:lanes:backward", tb));
            }
            (None, None) => {}
        }
        want.into_iter()
            .filter(|(k, v)| tags.get(k) != v.as_deref())
            .map(|(k, v)| (k.to_string(), v))
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
                forward: true,
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
                        forward: true,
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
                    forward: true,
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
                    forward: true,
                    add: false
                }));
                assert!(!c.apply(LaneEdit::Count {
                    forward: true,
                    add: false
                }));
                // A oneway has no backward lanes to change.
                assert!(!c.apply(LaneEdit::Count {
                    forward: false,
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
                    forward: false,
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
}
