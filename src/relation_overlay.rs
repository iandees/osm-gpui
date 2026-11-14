//! Map overlays for `type=route` and `type=restriction` relations.
//!
//! Pure data code: given a relation and a way to look up node positions, work
//! out what to draw. The layer caches the results (see `MpCache` in
//! `layers/osm_layer/`) so nothing here runs per frame.
//!
//! * A route colours every member way with a translucent line under the
//!   way's own stroke: the relation's `colour` tag when it parses, otherwise a
//!   colour picked by the `route` value from [`PALETTE`].
//! * A turn restriction is a small sign-like marker at the `via` node (or the
//!   middle of the `via` way, or the node shared by the `from` and `to` ways
//!   when there is no `via`).

use crate::osm::{MemberType, OsmData, OsmRelation};
use std::collections::HashMap;

/// The route colours, in one place: `(route values, colour)`. A route whose
/// `route` value is in none of these lists gets [`FALLBACK_COLOUR`].
pub const PALETTE: &[(&[&str], u32)] = &[
    (&["bicycle", "mtb"], 0x1E6FE8),
    (&["hiking", "foot", "walking"], 0xE8590C),
    (&["bus", "trolleybus", "share_taxi"], 0x9B30D9),
    (
        &[
            "tram",
            "train",
            "subway",
            "light_rail",
            "monorail",
            "railway",
        ],
        0xD6249F,
    ),
    (&["road", "detour"], 0xC9A100),
];

/// Routes of any other kind (ferry, horse, power, ...).
pub const FALLBACK_COLOUR: u32 = 0x14A38B;

/// Opacity of the route overlay line.
pub const ROUTE_ALPHA: f32 = 0.45;

/// `no_*` restrictions: a red ring.
pub const RESTRICTION_NO_COLOUR: u32 = 0xD32F2F;
/// `only_*` restrictions: a blue disc.
pub const RESTRICTION_ONLY_COLOUR: u32 = 0x1565C0;

/// Whether `tags` describe a route that is drawn. `route_master` is not.
pub fn is_route(tags: &HashMap<String, String>) -> bool {
    tags.get("type").map(String::as_str) == Some("route")
}

/// Whether `tags` describe a turn restriction.
pub fn is_restriction(tags: &HashMap<String, String>) -> bool {
    tags.get("type").map(String::as_str) == Some("restriction")
}

/// Parse `#rgb`, `#rrggbb` or a small set of CSS colour names into `0xRRGGBB`.
pub fn parse_colour(text: &str) -> Option<u32> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if !hex.is_ascii() {
            return None;
        }
        return match hex.len() {
            3 => {
                let v = u32::from_str_radix(hex, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
                Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
            }
            6 => u32::from_str_radix(hex, 16).ok(),
            _ => None,
        };
    }
    let named = match t.to_ascii_lowercase().as_str() {
        "red" => 0xFF0000,
        "green" => 0x008000,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "orange" => 0xFFA500,
        "purple" => 0x800080,
        "pink" => 0xFFC0CB,
        "brown" => 0xA52A2A,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "gray" | "grey" => 0x808080,
        "cyan" => 0x00FFFF,
        "magenta" => 0xFF00FF,
        "lime" => 0x00FF00,
        "navy" => 0x000080,
        "teal" => 0x008080,
        "maroon" => 0x800000,
        _ => return None,
    };
    Some(named)
}

/// The overlay colour of a route: its `colour` (or `color`) tag if that
/// parses, else the palette entry for its `route` value.
pub fn route_colour(tags: &HashMap<String, String>) -> u32 {
    if let Some(c) = tags
        .get("colour")
        .or_else(|| tags.get("color"))
        .and_then(|v| parse_colour(v))
    {
        return c;
    }
    let kind = tags.get("route").map(String::as_str).unwrap_or("");
    PALETTE
        .iter()
        .find(|(names, _)| names.contains(&kind))
        .map_or(FALLBACK_COLOUR, |&(_, c)| c)
}

/// A route relation as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteEntry {
    pub colour: u32,
    /// Member way ids, sorted and deduplicated. Includes ways that aren't
    /// loaded; the renderer simply never finds them.
    pub ways: Vec<i64>,
}

/// Build the entry for `relation`, or `None` if it isn't a drawn route.
pub fn build_route(relation: &OsmRelation) -> Option<RouteEntry> {
    if !is_route(&relation.tags) {
        return None;
    }
    let mut ways: Vec<i64> = relation
        .members
        .iter()
        .filter(|m| m.member_type == MemberType::Way)
        .map(|m| m.reference)
        .collect();
    ways.sort_unstable();
    ways.dedup();
    Some(RouteEntry {
        colour: route_colour(&relation.tags),
        ways,
    })
}

/// A turn restriction as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct RestrictionEntry {
    /// Mercator position of the marker.
    pub x: f64,
    pub y: f64,
    /// `only_*` (blue disc) rather than `no_*` (red ring).
    pub only: bool,
    /// Node ids whose movement can move the marker (the `via` node).
    pub via_node: Option<i64>,
    /// Way ids whose reshaping can move the marker.
    pub ways: Vec<i64>,
}

/// The restriction value: `restriction`, else the first `restriction:<mode>`
/// key (in key order, so deterministic).
fn restriction_value(tags: &HashMap<String, String>) -> Option<&str> {
    if let Some(v) = tags.get("restriction") {
        return Some(v);
    }
    let mut keys: Vec<&String> = tags
        .keys()
        .filter(|k| k.starts_with("restriction:"))
        .collect();
    keys.sort();
    keys.first().and_then(|k| tags.get(*k)).map(String::as_str)
}

/// Build the entry for `relation`. `position` maps a node id to its mercator
/// position. `None` when it isn't a restriction, its type is missing, or its
/// `via` isn't available.
pub fn build_restriction(
    data: &OsmData,
    relation: &OsmRelation,
    position: &dyn Fn(i64) -> Option<(f64, f64)>,
) -> Option<RestrictionEntry> {
    if !is_restriction(&relation.tags) {
        return None;
    }
    let value = restriction_value(&relation.tags)?;
    let only = value.starts_with("only_");

    fn by<'a>(
        relation: &'a OsmRelation,
        role: &'a str,
        ty: MemberType,
    ) -> impl Iterator<Item = &'a crate::osm::OsmMember> + 'a {
        relation
            .members
            .iter()
            .filter(move |m| m.role == role && m.member_type == ty)
    }
    let by_role = |role: &'static str, ty: MemberType| by(relation, role, ty);
    let way_nodes = |id: i64| data.ways.get(&id).map(|w| w.nodes.as_slice());

    let mut ways: Vec<i64> = relation
        .members
        .iter()
        .filter(|m| m.member_type == MemberType::Way)
        .map(|m| m.reference)
        .collect();
    ways.sort_unstable();
    ways.dedup();

    let has_via = relation.members.iter().any(|m| m.role == "via");
    let mut via_node = None;
    let (x, y) = if let Some(m) = by_role("via", MemberType::Node).next() {
        via_node = Some(m.reference);
        position(m.reference)?
    } else if let Some(m) = by_role("via", MemberType::Way).next() {
        let nodes = way_nodes(m.reference)?;
        let (a, b) = (position(*nodes.first()?)?, position(*nodes.last()?)?);
        ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
    } else if has_via {
        // A via of a kind we don't draw (a relation).
        return None;
    } else {
        // No via: the node the from and to ways share.
        let from = way_nodes(by_role("from", MemberType::Way).next()?.reference)?;
        let to = way_nodes(by_role("to", MemberType::Way).next()?.reference)?;
        let shared = from.iter().find(|n| to.contains(n))?;
        via_node = Some(*shared);
        position(*shared)?
    };
    Some(RestrictionEntry {
        x,
        y,
        only,
        via_node,
        ways,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{OsmMember, OsmWay};

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn member(ty: MemberType, id: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type: ty,
            reference: id,
            role: role.to_string(),
        }
    }

    fn relation(id: i64, t: &[(&str, &str)], members: Vec<OsmMember>) -> OsmRelation {
        OsmRelation {
            id,
            members,
            version: 1,
            tags: tags(t),
        }
    }

    fn data_with(ways: &[(i64, &[i64])]) -> OsmData {
        let mut d = OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        for (id, nodes) in ways {
            d.ways.insert(
                *id,
                OsmWay {
                    id: *id,
                    nodes: nodes.to_vec(),
                    version: 1,
                    tags: HashMap::new(),
                },
            );
        }
        d
    }

    fn pos(id: i64) -> Option<(f64, f64)> {
        (id < 100).then_some((id as f64, id as f64 * 10.0))
    }

    #[test]
    fn parses_hex_and_names() {
        assert_eq!(parse_colour("#ff8000"), Some(0xFF8000));
        assert_eq!(parse_colour("#f80"), Some(0xFF8800));
        assert_eq!(parse_colour(" Red "), Some(0xFF0000));
        assert_eq!(parse_colour("#12"), None);
        assert_eq!(parse_colour("#gggggg"), None);
        assert_eq!(parse_colour("chartreuse-ish"), None);
        assert_eq!(parse_colour("#é"), None);
    }

    #[test]
    fn route_colour_prefers_valid_tag_then_palette() {
        assert_eq!(
            route_colour(&tags(&[("route", "bus"), ("colour", "#112233")])),
            0x112233
        );
        // A bad colour falls back to the route value.
        assert_eq!(
            route_colour(&tags(&[("route", "bicycle"), ("colour", "nope")])),
            0x1E6FE8
        );
        assert_eq!(route_colour(&tags(&[("route", "subway")])), 0xD6249F);
        assert_eq!(route_colour(&tags(&[("route", "ferry")])), FALLBACK_COLOUR);
        assert_eq!(route_colour(&tags(&[])), FALLBACK_COLOUR);
    }

    #[test]
    fn route_lists_member_ways_even_if_missing() {
        let r = relation(
            1,
            &[("type", "route"), ("route", "bus")],
            vec![
                member(MemberType::Way, 9, ""),
                member(MemberType::Way, 3, "forward"),
                member(MemberType::Way, 9, ""),
                member(MemberType::Node, 4, "stop"),
            ],
        );
        let e = build_route(&r).unwrap();
        assert_eq!(e.ways, vec![3, 9]);
        assert_eq!(e.colour, 0x9B30D9);
    }

    #[test]
    fn route_master_and_other_types_are_not_drawn() {
        assert!(build_route(&relation(1, &[("type", "route_master")], vec![])).is_none());
        assert!(build_route(&relation(1, &[("type", "multipolygon")], vec![])).is_none());
        assert!(build_route(&relation(1, &[], vec![])).is_none());
    }

    #[test]
    fn restriction_at_via_node() {
        let d = data_with(&[(1, &[10, 11]), (2, &[11, 12])]);
        let r = relation(
            5,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            vec![
                member(MemberType::Way, 1, "from"),
                member(MemberType::Node, 11, "via"),
                member(MemberType::Way, 2, "to"),
            ],
        );
        let e = build_restriction(&d, &r, &pos).unwrap();
        assert_eq!((e.x, e.y), (11.0, 110.0));
        assert!(!e.only);
        assert_eq!(e.via_node, Some(11));
        assert_eq!(e.ways, vec![1, 2]);
    }

    #[test]
    fn only_restriction_and_mode_keyed_variant() {
        let d = data_with(&[(1, &[10, 11]), (2, &[11, 12])]);
        let members = vec![
            member(MemberType::Way, 1, "from"),
            member(MemberType::Node, 11, "via"),
            member(MemberType::Way, 2, "to"),
        ];
        let r = relation(
            5,
            &[
                ("type", "restriction"),
                ("restriction:hgv", "only_straight_on"),
            ],
            members,
        );
        assert!(build_restriction(&d, &r, &pos).unwrap().only);
    }

    #[test]
    fn restriction_via_way_uses_midpoint() {
        let d = data_with(&[(1, &[10, 11]), (2, &[20, 30]), (3, &[30, 31])]);
        let r = relation(
            5,
            &[("type", "restriction"), ("restriction", "no_u_turn")],
            vec![
                member(MemberType::Way, 1, "from"),
                member(MemberType::Way, 2, "via"),
                member(MemberType::Way, 3, "to"),
            ],
        );
        let e = build_restriction(&d, &r, &pos).unwrap();
        assert_eq!((e.x, e.y), (25.0, 250.0));
        assert_eq!(e.via_node, None);
    }

    #[test]
    fn restriction_without_via_uses_shared_node() {
        let d = data_with(&[(1, &[10, 11]), (2, &[11, 12])]);
        let r = relation(
            5,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            vec![
                member(MemberType::Way, 1, "from"),
                member(MemberType::Way, 2, "to"),
            ],
        );
        let e = build_restriction(&d, &r, &pos).unwrap();
        assert_eq!((e.x, e.y), (11.0, 110.0));
    }

    #[test]
    fn incomplete_restrictions_draw_nothing() {
        let d = data_with(&[(1, &[10, 11])]);
        let t: &[(&str, &str)] = &[("type", "restriction"), ("restriction", "no_left_turn")];
        // via node not downloaded (no position)
        let r = relation(5, t, vec![member(MemberType::Node, 500, "via")]);
        assert!(build_restriction(&d, &r, &pos).is_none());
        // via way not downloaded
        let r = relation(5, t, vec![member(MemberType::Way, 77, "via")]);
        assert!(build_restriction(&d, &r, &pos).is_none());
        // no via and no ways at all
        let r = relation(5, t, vec![]);
        assert!(build_restriction(&d, &r, &pos).is_none());
        // no restriction tag
        let r = relation(
            5,
            &[("type", "restriction")],
            vec![member(MemberType::Node, 11, "via")],
        );
        assert!(build_restriction(&d, &r, &pos).is_none());
    }
}
