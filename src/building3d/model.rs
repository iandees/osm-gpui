//! The building being modelled and its surroundings, extracted from OSM data
//! into a local metric frame (x east, y north, metres).
//!
//! A building is either a `type=building` relation (its `outline` and `part`
//! members) or a plain building outline way, whose parts are the
//! `building:part` ways that lie inside it — both forms are valid Simple 3D
//! Buildings tagging. A building with no parts is modelled as one solid: its
//! own outline.

use std::collections::{HashMap, HashSet};

use crate::geometry::LocalProjection;
use crate::osm::{MemberType, OsmData, OsmWay};

use super::tags::{self, Heights, RoofShape};

pub type Pt = (f64, f64);

/// What a 3D session is centred on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildingRoot {
    /// A `type=building` relation.
    Relation(i64),
    /// A closed way: a building outline, or a lone `building:part`.
    Way(i64),
}

/// One extruded footprint: a building part, a part-less building outline,
/// or a neighbouring building drawn for context.
#[derive(Clone, Debug, PartialEq)]
pub struct Solid {
    pub way_id: i64,
    /// The way's node ids, closed (first == last).
    pub node_ids: Vec<i64>,
    /// Vertex positions, one per node id *without* the closing repeat, so
    /// segment `i` runs from `ring[i]` to `ring[(i + 1) % ring.len()]`.
    pub ring: Vec<Pt>,
    pub heights: Heights,
    pub roof: RoofShape,
    pub wall_colour: Option<[f32; 3]>,
    pub roof_colour: Option<[f32; 3]>,
    /// Whether this is a building outline standing in for a part (a building
    /// with no parts), rather than a `building:part`.
    pub is_outline: bool,
}

impl Solid {
    pub fn centroid(&self) -> Pt {
        centroid(&self.ring)
    }
}

/// The building being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Building {
    pub root: BuildingRoot,
    /// The outline way, if there is one (a relation may lack it; a lone part
    /// has none).
    pub outline: Option<Solid>,
    /// The parts drawn in 3D. When the building has no `building:part`s
    /// this is just the outline, with `is_outline` set.
    pub parts: Vec<Solid>,
}

impl Building {
    /// Whether the building has real `building:part`s (as opposed to only
    /// the outline standing in for one).
    pub fn has_parts(&self) -> bool {
        self.parts.iter().any(|p| !p.is_outline)
    }

    pub fn part_index(&self, way_id: i64) -> Option<usize> {
        self.parts.iter().position(|p| p.way_id == way_id)
    }
}

/// Everything the 3D view draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub frame: LocalProjection,
    pub building: Building,
    /// Neighbouring buildings / parts, drawn greyed out for reference.
    pub context: Vec<Solid>,
    /// Half-width of the square of ground around the building that is drawn
    /// (and textured with imagery), in metres.
    pub ground_radius: f64,
    /// Half the size of the building's footprint bounding box, in metres —
    /// what the camera frames initially.
    pub building_radius: f64,
}

fn is_building(way: &OsmWay) -> bool {
    way.tags.get("building").is_some_and(|v| v != "no")
}

fn is_part(way: &OsmWay) -> bool {
    way.tags.get("building:part").is_some_and(|v| v != "no")
}

/// The node positions of a closed way, without the closing repeat. `None`
/// if it isn't closed or a node is missing.
fn way_ring_geo(data: &OsmData, way: &OsmWay) -> Option<Vec<(f64, f64)>> {
    if !way.is_closed() {
        return None;
    }
    way.nodes[..way.nodes.len() - 1]
        .iter()
        .map(|id| data.nodes.get(id).map(|n| (n.lat, n.lon)))
        .collect()
}

fn solid_from_way(
    data: &OsmData,
    frame: &LocalProjection,
    way: &OsmWay,
    is_outline: bool,
) -> Option<Solid> {
    let ring: Vec<Pt> = way_ring_geo(data, way)?
        .into_iter()
        .map(|p| frame.project(p))
        .collect();
    if ring.len() < 3 {
        return None;
    }
    Some(Solid {
        way_id: way.id,
        node_ids: way.nodes.clone(),
        ring,
        heights: tags::heights(&way.tags),
        roof: tags::roof_shape(&way.tags),
        wall_colour: way
            .tags
            .get("building:colour")
            .and_then(|c| tags::parse_colour(c)),
        roof_colour: way
            .tags
            .get("roof:colour")
            .and_then(|c| tags::parse_colour(c)),
        is_outline,
    })
}

/// The relations of `kind` (`type=<kind>`) that have way `way_id` as a member.
fn relations_with_way<'a>(
    data: &'a OsmData,
    way_id: i64,
    kind: &str,
) -> impl Iterator<Item = &'a crate::osm::OsmRelation> + 'a {
    let kind = kind.to_string();
    data.relations.iter().filter(move |r| {
        r.tags.get("type").is_some_and(|t| *t == kind)
            && r.members
                .iter()
                .any(|m| m.member_type == MemberType::Way && m.reference == way_id)
    })
}

/// Which building a click on way `way_id` means: the `type=building`
/// relation it belongs to, else the outline it is (or, for a part, the
/// smallest outline containing it), else the part itself. `None` for a way
/// that is neither a building nor a part.
pub fn resolve_root(data: &OsmData, way_id: i64) -> Option<BuildingRoot> {
    let way = data.ways.get(&way_id)?;
    if let Some(rel) = relations_with_way(data, way_id, "building").next() {
        return Some(BuildingRoot::Relation(rel.id));
    }
    if is_building(way) && way.is_closed() {
        return Some(BuildingRoot::Way(way_id));
    }
    if !is_part(way) || !way.is_closed() {
        return None;
    }
    // A part: the smallest building outline around its centroid.
    let ring = way_ring_geo(data, way)?;
    let frame = LocalProjection::new(ring[0]);
    let local: Vec<Pt> = ring.iter().map(|&p| frame.project(p)).collect();
    let c = centroid(&local);
    let (bb_min, bb_max) = bbox(&local);
    let mut best: Option<(f64, i64)> = None;
    for other in data.ways.values() {
        if other.id == way_id || !is_building(other) || !other.is_closed() {
            continue;
        }
        let Some(other_ring) = way_ring_geo(data, other) else {
            continue;
        };
        let other_local: Vec<Pt> = other_ring.iter().map(|&p| frame.project(p)).collect();
        let (o_min, o_max) = bbox(&other_local);
        if o_min.0 > bb_max.0 || o_max.0 < bb_min.0 || o_min.1 > bb_max.1 || o_max.1 < bb_min.1 {
            continue;
        }
        if point_in_ring(c, &other_local) {
            let area = signed_area(&other_local).abs();
            if best.is_none_or(|(a, _)| area < a) {
                best = Some((area, other.id));
            }
        }
    }
    match best {
        Some((_, outline)) => match relations_with_way(data, outline, "building").next() {
            Some(rel) => Some(BuildingRoot::Relation(rel.id)),
            None => Some(BuildingRoot::Way(outline)),
        },
        None => Some(BuildingRoot::Way(way_id)),
    }
}

/// How far around the building neighbours and imagery are shown: a few
/// building-widths, within sensible bounds.
pub fn ground_radius_for(building_radius: f64) -> f64 {
    (building_radius * 4.0).clamp(80.0, 400.0)
}

/// Build the scene for `root`. `None` if the root no longer exists or has
/// no drawable geometry.
pub fn build_scene(data: &OsmData, root: BuildingRoot) -> Option<Scene> {
    // Collect the root's outline and part ways (by id) first.
    let (outline_id, mut part_ids): (Option<i64>, Vec<i64>) = match root {
        BuildingRoot::Relation(rel_id) => {
            let rel = data.relations.iter().find(|r| r.id == rel_id)?;
            let mut outline = None;
            let mut parts = Vec::new();
            for m in &rel.members {
                if m.member_type != MemberType::Way {
                    continue;
                }
                match m.role.as_str() {
                    "outline" => outline = outline.or(Some(m.reference)),
                    "part" => parts.push(m.reference),
                    _ => {}
                }
            }
            (outline, parts)
        }
        BuildingRoot::Way(way_id) => {
            let way = data.ways.get(&way_id)?;
            if is_building(way) {
                (Some(way_id), Vec::new())
            } else {
                (None, vec![way_id])
            }
        }
    };

    // Origin: the first node of the outline or first part.
    let anchor_way = outline_id
        .or_else(|| part_ids.first().copied())
        .and_then(|id| data.ways.get(&id))?;
    let anchor = data.nodes.get(anchor_way.nodes.first()?)?;
    let frame = LocalProjection::new((anchor.lat, anchor.lon));

    let outline = outline_id
        .and_then(|id| data.ways.get(&id))
        .and_then(|w| solid_from_way(data, &frame, w, true));

    // An outline way with no relation: its parts are the building:part ways
    // inside it.
    if matches!(root, BuildingRoot::Way(_)) {
        if let Some(outline) = &outline {
            let (o_min, o_max) = bbox(&outline.ring);
            for way in data.ways.values() {
                if !is_part(way) || !way.is_closed() || way.id == outline.way_id {
                    continue;
                }
                let Some(ring) = way_ring_geo(data, way) else {
                    continue;
                };
                let local: Vec<Pt> = ring.iter().map(|&p| frame.project(p)).collect();
                let c = centroid(&local);
                if c.0 < o_min.0 || c.0 > o_max.0 || c.1 < o_min.1 || c.1 > o_max.1 {
                    continue;
                }
                if point_in_ring(c, &outline.ring) {
                    part_ids.push(way.id);
                }
            }
            part_ids.sort_unstable();
        }
    }

    let mut parts: Vec<Solid> = part_ids
        .iter()
        .filter_map(|id| data.ways.get(id))
        .filter_map(|w| solid_from_way(data, &frame, w, false))
        .collect();
    if parts.is_empty() {
        parts.push(outline.clone()?);
    }

    let all_pts: Vec<Pt> = outline
        .iter()
        .chain(parts.iter())
        .flat_map(|s| s.ring.iter().copied())
        .collect();
    let (min, max) = bbox(&all_pts);
    let building_radius = ((max.0 - min.0).max(max.1 - min.1) / 2.0).max(5.0);
    let ground_radius = ground_radius_for(building_radius);
    let center = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);

    let mut own: HashSet<i64> = parts.iter().map(|p| p.way_id).collect();
    if let Some(o) = &outline {
        own.insert(o.way_id);
    }
    let context = context_solids(data, &frame, center, ground_radius, &own);

    Some(Scene {
        frame,
        building: Building {
            root,
            outline,
            parts,
        },
        context,
        ground_radius,
        building_radius,
    })
}

/// Neighbouring buildings and parts within `radius` of `center`, excluding
/// `own` ways. As in 3D renderers, an outline that has parts inside it is
/// left out (its parts are drawn instead).
fn context_solids(
    data: &OsmData,
    frame: &LocalProjection,
    center: Pt,
    radius: f64,
    own: &HashSet<i64>,
) -> Vec<Solid> {
    // Rough degree bounds for a cheap first filter.
    let lo = frame.unproject((center.0 - radius, center.1 - radius));
    let hi = frame.unproject((center.0 + radius, center.1 + radius));
    let (min_lat, max_lat) = (lo.0.min(hi.0), lo.0.max(hi.0));
    let (min_lon, max_lon) = (lo.1.min(hi.1), lo.1.max(hi.1));

    let mut outlines = Vec::new();
    let mut parts = Vec::new();
    for way in data.ways.values() {
        if own.contains(&way.id) || !way.is_closed() {
            continue;
        }
        let part = is_part(way);
        if !part && !is_building(way) {
            continue;
        }
        let Some(first) = way.nodes.first().and_then(|id| data.nodes.get(id)) else {
            continue;
        };
        // A building reaching into the area from far away is rare; testing
        // its first node keeps this pass cheap.
        if first.lat < min_lat || first.lat > max_lat || first.lon < min_lon || first.lon > max_lon
        {
            continue;
        }
        if let Some(solid) = solid_from_way(data, frame, way, !part) {
            if part {
                parts.push(solid);
            } else {
                outlines.push(solid);
            }
        }
    }
    // Parts that belong to the building being edited (via a relation that
    // wasn't the root) are already excluded by `own`.
    let part_centroids: Vec<Pt> = parts.iter().map(|p| p.centroid()).collect();
    outlines.retain(|o| {
        let (min, max) = bbox(&o.ring);
        !part_centroids.iter().any(|&c| {
            c.0 >= min.0
                && c.0 <= max.0
                && c.1 >= min.1
                && c.1 <= max.1
                && point_in_ring(c, &o.ring)
        })
    });
    outlines.extend(parts);
    outlines.sort_by_key(|s| s.way_id);
    outlines
}

// --- small planar helpers ---------------------------------------------------

/// Twice-free signed area (positive for counter-clockwise rings).
pub fn signed_area(ring: &[Pt]) -> f64 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        a += p.0 * q.1 - q.0 * p.1;
    }
    a / 2.0
}

/// Area centroid of a simple polygon (vertex mean for a degenerate one).
pub fn centroid(ring: &[Pt]) -> Pt {
    let n = ring.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let area = signed_area(ring);
    if area.abs() < 1e-9 {
        let s = ring
            .iter()
            .fold((0.0, 0.0), |acc, p| (acc.0 + p.0, acc.1 + p.1));
        return (s.0 / n as f64, s.1 / n as f64);
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let cross = p.0 * q.1 - q.0 * p.1;
        cx += (p.0 + q.0) * cross;
        cy += (p.1 + q.1) * cross;
    }
    (cx / (6.0 * area), cy / (6.0 * area))
}

pub fn bbox(pts: &[Pt]) -> (Pt, Pt) {
    let mut min = (f64::INFINITY, f64::INFINITY);
    let mut max = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in pts {
        min = (min.0.min(p.0), min.1.min(p.1));
        max = (max.0.max(p.0), max.1.max(p.1));
    }
    (min, max)
}

/// Even-odd point-in-polygon test.
pub fn point_in_ring(p: Pt, ring: &[Pt]) -> bool {
    let n = ring.len();
    let mut inside = false;
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Node-id -> position lookup for the vertices of every solid in the scene,
/// used when planning edits that move shared nodes.
pub fn node_positions(scene: &Scene) -> HashMap<i64, Pt> {
    let mut out = HashMap::new();
    let b = &scene.building;
    for s in b.outline.iter().chain(b.parts.iter()) {
        for (id, p) in s.node_ids.iter().zip(s.ring.iter()) {
            out.insert(*id, *p);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod test_data {
    use super::*;
    use crate::osm::{OsmMember, OsmNode, OsmRelation};

    /// Builds small OSM datasets around (0, 0) with node positions given in
    /// metres east/north.
    pub struct DataBuilder {
        pub data: OsmData,
        frame: LocalProjection,
        next_node: i64,
    }

    impl DataBuilder {
        pub fn new() -> Self {
            Self {
                data: OsmData {
                    nodes: HashMap::new(),
                    ways: HashMap::new(),
                    relations: Vec::new(),
                    bounds: Vec::new(),
                },
                frame: LocalProjection::new((0.0, 0.0)),
                next_node: 1,
            }
        }

        pub fn node(&mut self, x: f64, y: f64) -> i64 {
            let id = self.next_node;
            self.next_node += 1;
            let (lat, lon) = self.frame.unproject((x, y));
            self.data.nodes.insert(
                id,
                OsmNode {
                    id,
                    lat,
                    lon,
                    version: 1,
                    tags: HashMap::new(),
                },
            );
            id
        }

        /// A closed way through the given points with the given tags.
        pub fn ring(&mut self, way_id: i64, pts: &[(f64, f64)], tags: &[(&str, &str)]) -> i64 {
            let mut nodes: Vec<i64> = pts.iter().map(|&(x, y)| self.node(x, y)).collect();
            nodes.push(nodes[0]);
            self.way(way_id, nodes, tags)
        }

        pub fn way(&mut self, way_id: i64, nodes: Vec<i64>, tags: &[(&str, &str)]) -> i64 {
            self.data.ways.insert(
                way_id,
                OsmWay {
                    id: way_id,
                    nodes,
                    version: 1,
                    tags: tags
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                },
            );
            way_id
        }

        pub fn relation(&mut self, id: i64, members: &[(i64, &str)], tags: &[(&str, &str)]) {
            self.data.relations.push(OsmRelation {
                id,
                version: 1,
                members: members
                    .iter()
                    .map(|&(r, role)| OsmMember {
                        member_type: MemberType::Way,
                        reference: r,
                        role: role.to_string(),
                    })
                    .collect(),
                tags: tags
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            });
        }
    }

    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> [(f64, f64); 4] {
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }
}

#[cfg(test)]
mod tests {
    use super::test_data::*;
    use super::*;

    #[test]
    fn plain_building_is_its_own_single_part() {
        let mut b = DataBuilder::new();
        b.ring(
            10,
            &rect(0.0, 0.0, 20.0, 10.0),
            &[("building", "yes"), ("height", "9")],
        );
        assert_eq!(resolve_root(&b.data, 10), Some(BuildingRoot::Way(10)));
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        assert_eq!(scene.building.parts.len(), 1);
        assert!(scene.building.parts[0].is_outline);
        assert!(!scene.building.has_parts());
        assert_eq!(scene.building.parts[0].heights.height, 9.0);
        assert_eq!(scene.building.parts[0].ring.len(), 4);
        assert!((scene.building.parts[0].ring[1].0 - 20.0).abs() < 1e-6);
    }

    #[test]
    fn outline_finds_parts_inside_it() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 20.0, 10.0), &[("building", "yes")]);
        b.ring(11, &rect(0.0, 0.0, 10.0, 10.0), &[("building:part", "yes")]);
        b.ring(
            12,
            &rect(10.0, 0.0, 20.0, 10.0),
            &[("building:part", "yes")],
        );
        // A part elsewhere.
        b.ring(
            13,
            &rect(50.0, 0.0, 60.0, 10.0),
            &[("building:part", "yes")],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let ids: Vec<i64> = scene.building.parts.iter().map(|p| p.way_id).collect();
        assert_eq!(ids, vec![11, 12]);
        assert!(scene.building.has_parts());
        // The far part is context.
        assert!(scene.context.iter().any(|c| c.way_id == 13));
        assert!(!scene.context.iter().any(|c| c.way_id == 10));
    }

    #[test]
    fn clicking_a_part_resolves_to_its_outline() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 20.0, 10.0), &[("building", "yes")]);
        b.ring(11, &rect(1.0, 1.0, 9.0, 9.0), &[("building:part", "yes")]);
        assert_eq!(resolve_root(&b.data, 11), Some(BuildingRoot::Way(10)));
    }

    #[test]
    fn lone_part_is_its_own_root() {
        let mut b = DataBuilder::new();
        b.ring(11, &rect(1.0, 1.0, 9.0, 9.0), &[("building:part", "yes")]);
        assert_eq!(resolve_root(&b.data, 11), Some(BuildingRoot::Way(11)));
        let scene = build_scene(&b.data, BuildingRoot::Way(11)).unwrap();
        assert_eq!(scene.building.parts.len(), 1);
        assert!(!scene.building.parts[0].is_outline);
        assert!(scene.building.outline.is_none());
    }

    #[test]
    fn building_relation_wins() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 20.0, 10.0), &[("building", "yes")]);
        b.ring(11, &rect(0.0, 0.0, 10.0, 10.0), &[("building:part", "yes")]);
        b.ring(
            12,
            &rect(10.0, 0.0, 20.0, 10.0),
            &[("building:part", "yes")],
        );
        b.relation(
            100,
            &[(10, "outline"), (11, "part")],
            &[("type", "building")],
        );
        assert_eq!(resolve_root(&b.data, 10), Some(BuildingRoot::Relation(100)));
        assert_eq!(resolve_root(&b.data, 11), Some(BuildingRoot::Relation(100)));
        // 12 isn't a member, but its outline is.
        assert_eq!(resolve_root(&b.data, 12), Some(BuildingRoot::Relation(100)));
        let scene = build_scene(&b.data, BuildingRoot::Relation(100)).unwrap();
        // Only member parts count for a relation.
        let ids: Vec<i64> = scene.building.parts.iter().map(|p| p.way_id).collect();
        assert_eq!(ids, vec![11]);
        assert_eq!(scene.building.outline.as_ref().unwrap().way_id, 10);
    }

    #[test]
    fn non_buildings_do_not_resolve() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 20.0, 10.0), &[("landuse", "grass")]);
        assert_eq!(resolve_root(&b.data, 10), None);
        assert_eq!(resolve_root(&b.data, 999), None);
    }

    #[test]
    fn context_skips_outlines_that_have_parts() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 10.0, 10.0), &[("building", "yes")]);
        b.ring(20, &rect(30.0, 0.0, 50.0, 10.0), &[("building", "yes")]);
        b.ring(
            21,
            &rect(30.0, 0.0, 40.0, 10.0),
            &[("building:part", "yes")],
        );
        b.ring(30, &rect(60.0, 0.0, 70.0, 10.0), &[("building", "yes")]);
        // Far beyond the ground radius.
        b.ring(40, &rect(5000.0, 0.0, 5010.0, 10.0), &[("building", "yes")]);
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let ids: Vec<i64> = scene.context.iter().map(|c| c.way_id).collect();
        assert_eq!(ids, vec![21, 30]);
    }

    #[test]
    fn polygon_helpers() {
        let sq = rect(0.0, 0.0, 2.0, 2.0);
        assert_eq!(signed_area(&sq), 4.0);
        assert_eq!(centroid(&sq), (1.0, 1.0));
        assert!(point_in_ring((1.0, 1.0), &sq));
        assert!(!point_in_ring((3.0, 1.0), &sq));
    }
}
