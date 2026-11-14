use super::*;

/// The ids of the ways that are a member of any relation, for
/// `way:in-relation` rules. Empty (and not worth computing) for a stylesheet
/// with no such rule.
pub(super) fn relation_member_ways(data: &OsmData, stylesheet: &Stylesheet) -> HashSet<i64> {
    if !stylesheet.uses_relation_membership() {
        return HashSet::new();
    }
    data.relations
        .iter()
        .flat_map(|r| &r.members)
        .filter(|m| m.member_type == crate::osm::MemberType::Way)
        .map(|m| m.reference)
        .collect()
}

/// One assembled polygon of a multipolygon relation, projected and
/// triangulated once.
#[derive(Debug, Clone)]
pub(super) struct MpPolygon {
    /// `(node id, x, y)` in Web Mercator meters for the outer ring followed
    /// by each hole, none repeating its first vertex. The node ids let the
    /// renderer apply a drag preview.
    pub(super) verts: Vec<(i64, f64, f64)>,
    /// Start offset of each ring in `verts`, then `verts.len()`: ring `k` is
    /// `verts[ring_starts[k]..ring_starts[k + 1]]`; ring 0 is the outer one.
    pub(super) ring_starts: Vec<usize>,
    /// Earcut triangles (indices into `verts`), outer minus holes. Empty when
    /// the relation's style has no fill.
    pub(super) tris: Vec<u32>,
    pub(super) bbox: WayBbox,
}

impl MpPolygon {
    pub(super) fn ring(&self, k: usize) -> &[(i64, f64, f64)] {
        &self.verts[self.ring_starts[k]..self.ring_starts[k + 1]]
    }
    pub(super) fn ring_count(&self) -> usize {
        self.ring_starts.len() - 1
    }
}

/// A multipolygon relation that assembled into valid rings.
#[derive(Debug, Clone)]
pub(super) struct MpEntry {
    /// Resolved from the relation's tags as for a closed way.
    pub(super) style: WayStyle,
    pub(super) polygons: Vec<MpPolygon>,
    pub(super) bbox: WayBbox,
    /// Area in mercator square meters, holes excluded.
    pub(super) area: f64,
    /// Whether the relation carries tags besides `type`. An untagged
    /// multipolygon is the old style, whose tags live on the outer way: that
    /// way stays what a click selects.
    pub(super) tagged: bool,
    /// The relation's tags, for its area label.
    pub(super) tags: HashMap<String, String>,
}

/// What became of a relation in the cache (tests inspect this).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MpState {
    /// Not a multipolygon/boundary (or not in the data).
    NotArea,
    /// A multipolygon that couldn't be assembled; it is not drawn.
    Invalid(crate::multipolygon::Problem),
    /// Assembled into polygons with this many holes in all.
    Valid { polygons: usize, holes: usize },
}

/// Assembled multipolygon relations, kept in step with the data.
///
/// Built from scratch with the rest of the layer's caches, and then patched
/// per relation: every edit that can change a relation's rings (a member
/// node moved, a member way reshaped, split, joined, added or removed, the
/// member list or the relation's tags changed, and the undo/redo of each)
/// calls [`OsmLayer::mp_refresh_relations`] or [`OsmLayer::mp_refresh_ways`]
/// for just the relations involved. `version` counts those refreshes so a
/// test (or a future consumer) can tell a rebuild happened.
#[derive(Debug, Clone, Default)]
pub(super) struct MpCache {
    pub(super) entries: BTreeMap<i64, MpEntry>,
    pub(super) invalid: BTreeMap<i64, crate::multipolygon::Problem>,
    /// Member way id -> area relations listing it (valid or not).
    pub(super) way_to_rels: HashMap<i64, Vec<i64>>,
    /// Relation id -> its member way ids, to unmap them on refresh.
    pub(super) rel_ways: HashMap<i64, Vec<i64>>,
    /// Route relations (`type=route`) by id.
    pub(super) routes: BTreeMap<i64, crate::relation_overlay::RouteEntry>,
    /// Member way id -> the route relations listing it, ascending. The
    /// first one decides the way's overlay colour.
    pub(super) route_ways: HashMap<i64, Vec<i64>>,
    /// Turn restrictions (`type=restriction`) with a drawable position.
    pub(super) restrictions: BTreeMap<i64, crate::relation_overlay::RestrictionEntry>,
    /// Restriction member node id -> restriction relations, so moving,
    /// deleting or restoring that node refreshes them.
    pub(super) node_to_rels: HashMap<i64, Vec<i64>>,
    /// Restriction id -> its member node ids, to unmap them on refresh.
    pub(super) rel_nodes: HashMap<i64, Vec<i64>>,
    pub(super) version: u64,
}

/// Assemble and triangulate one relation. `None` when it isn't an area
/// relation.
pub(super) fn build_mp(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
    relation: &crate::osm::OsmRelation,
) -> Option<Result<MpEntry, crate::multipolygon::Problem>> {
    use crate::multipolygon::{assemble, triangulate, WayMember};
    if !crate::multipolygon::is_area_relation(&relation.tags) {
        return None;
    }
    let members: Vec<WayMember> = relation
        .members
        .iter()
        .filter(|m| m.member_type == crate::osm::MemberType::Way)
        .map(|m| WayMember {
            way_id: m.reference,
            role: &m.role,
            nodes: data.ways.get(&m.reference).map(|w| w.nodes.as_slice()),
        })
        .collect();
    let position = |id: i64| {
        node_cache
            .index_by_id
            .get(&id)
            .map(|&i| (node_cache.flat[i].1, node_cache.flat[i].2))
    };
    let assembled = match assemble(&members, &position) {
        Ok(a) => a,
        Err(problem) => return Some(Err(problem)),
    };
    let style = stylesheet.way_style_cached(&relation.tags, true, false);
    let mut polygons = Vec::with_capacity(assembled.polygons.len());
    let mut total = WayBbox::empty();
    for polygon in &assembled.polygons {
        let mut verts: Vec<(i64, f64, f64)> = Vec::new();
        let mut ring_starts = vec![0];
        let mut bbox = WayBbox::empty();
        for (k, ring) in std::iter::once(&polygon.outer)
            .chain(polygon.inners.iter())
            .enumerate()
        {
            for &id in &ring[..ring.len() - 1] {
                let Some((x, y)) = position(id) else {
                    return Some(Err(crate::multipolygon::Problem::MissingNode(id)));
                };
                verts.push((id, x, y));
                if k == 0 {
                    bbox.extend(x, y);
                }
            }
            ring_starts.push(verts.len());
        }
        total.extend(bbox.min_x, bbox.min_y);
        total.extend(bbox.max_x, bbox.max_y);
        let tris = if style.fill.is_some() {
            let rings: Vec<Vec<(f64, f64)>> = (0..ring_starts.len() - 1)
                .map(|k| {
                    verts[ring_starts[k]..ring_starts[k + 1]]
                        .iter()
                        .map(|&(_, x, y)| (x, y))
                        .collect()
                })
                .collect();
            let refs: Vec<&[(f64, f64)]> = rings.iter().map(|r| r.as_slice()).collect();
            triangulate(&refs).unwrap_or_default()
        } else {
            Vec::new()
        };
        polygons.push(MpPolygon {
            verts,
            ring_starts,
            tris,
            bbox,
        });
    }
    Some(Ok(MpEntry {
        style,
        polygons,
        bbox: total,
        area: assembled.area(),
        tagged: relation.tags.keys().any(|k| k != "type"),
        tags: relation.tags.clone(),
    }))
}

impl MpCache {
    pub(super) fn rebuild_all(
        &mut self,
        data: &OsmData,
        node_cache: &NodeCache,
        stylesheet: &Stylesheet,
    ) {
        let version = self.version + 1;
        *self = MpCache::default();
        self.version = version;
        for relation in &data.relations {
            self.install(relation, build_mp(data, node_cache, stylesheet, relation));
            self.install_overlay(relation, data, node_cache);
        }
    }

    /// The overlay colour of a way that belongs to a route, if any.
    pub(super) fn route_colour_of_way(&self, way_id: i64) -> Option<u32> {
        let rel = self.route_ways.get(&way_id)?.first()?;
        self.routes.get(rel).map(|r| r.colour)
    }

    pub(super) fn remove_overlay(&mut self, id: i64) {
        if let Some(route) = self.routes.remove(&id) {
            for way in route.ways {
                if let Some(rels) = self.route_ways.get_mut(&way) {
                    rels.retain(|&r| r != id);
                    if rels.is_empty() {
                        self.route_ways.remove(&way);
                    }
                }
            }
        }
        self.restrictions.remove(&id);
        if let Some(nodes) = self.rel_nodes.remove(&id) {
            for node in nodes {
                if let Some(rels) = self.node_to_rels.get_mut(&node) {
                    rels.retain(|&r| r != id);
                    if rels.is_empty() {
                        self.node_to_rels.remove(&node);
                    }
                }
            }
        }
    }

    /// Cache the route / restriction drawing of `relation`, if it is one.
    /// A restriction is also mapped from its member ways (through
    /// `way_to_rels`/`rel_ways`, like area relations) and member nodes, so
    /// the edits that move them refresh it. A route only lists way ids, so it
    /// needs no geometry refresh.
    pub(super) fn install_overlay(
        &mut self,
        relation: &crate::osm::OsmRelation,
        data: &OsmData,
        node_cache: &NodeCache,
    ) {
        if let Some(route) = crate::relation_overlay::build_route(relation) {
            for &w in &route.ways {
                let rels = self.route_ways.entry(w).or_default();
                if let Err(at) = rels.binary_search(&relation.id) {
                    rels.insert(at, relation.id);
                }
            }
            self.routes.insert(relation.id, route);
            return;
        }
        if !crate::relation_overlay::is_restriction(&relation.tags) {
            return;
        }
        // Map every member, drawable or not, so the edit that supplies a
        // missing via (an undo that restores it) refreshes the restriction.
        let mut ways: Vec<i64> = Vec::new();
        let mut nodes: Vec<i64> = Vec::new();
        for m in &relation.members {
            match m.member_type {
                crate::osm::MemberType::Way => ways.push(m.reference),
                crate::osm::MemberType::Node => nodes.push(m.reference),
                crate::osm::MemberType::Relation => {}
            }
        }
        ways.sort_unstable();
        ways.dedup();
        nodes.sort_unstable();
        nodes.dedup();
        for &w in &ways {
            self.way_to_rels.entry(w).or_default().push(relation.id);
        }
        for &n in &nodes {
            self.node_to_rels.entry(n).or_default().push(relation.id);
        }
        self.rel_ways.insert(relation.id, ways);
        self.rel_nodes.insert(relation.id, nodes);
        let position = |id: i64| {
            node_cache
                .index_by_id
                .get(&id)
                .map(|&i| (node_cache.flat[i].1, node_cache.flat[i].2))
        };
        if let Some(r) = crate::relation_overlay::build_restriction(data, relation, &position) {
            self.restrictions.insert(relation.id, r);
        }
    }

    /// Recompute just these relations (dropping any that are gone or no
    /// longer area relations).
    pub(super) fn refresh(
        &mut self,
        ids: &[i64],
        data: &OsmData,
        node_cache: &NodeCache,
        stylesheet: &Stylesheet,
    ) {
        if ids.is_empty() {
            return;
        }
        self.version += 1;
        for &id in ids {
            self.entries.remove(&id);
            self.invalid.remove(&id);
            self.remove_overlay(id);
            if let Some(ways) = self.rel_ways.remove(&id) {
                for way in ways {
                    if let Some(rels) = self.way_to_rels.get_mut(&way) {
                        rels.retain(|&r| r != id);
                        if rels.is_empty() {
                            self.way_to_rels.remove(&way);
                        }
                    }
                }
            }
            if let Some(relation) = data.relations.iter().find(|r| r.id == id) {
                self.install(relation, build_mp(data, node_cache, stylesheet, relation));
                self.install_overlay(relation, data, node_cache);
            }
        }
    }

    pub(super) fn install(
        &mut self,
        relation: &crate::osm::OsmRelation,
        built: Option<Result<MpEntry, crate::multipolygon::Problem>>,
    ) {
        let Some(built) = built else {
            return;
        };
        let mut ways: Vec<i64> = relation
            .members
            .iter()
            .filter(|m| m.member_type == crate::osm::MemberType::Way)
            .map(|m| m.reference)
            .collect();
        ways.sort_unstable();
        ways.dedup();
        for &w in &ways {
            self.way_to_rels.entry(w).or_default().push(relation.id);
        }
        self.rel_ways.insert(relation.id, ways);
        match built {
            Ok(entry) => {
                self.entries.insert(relation.id, entry);
            }
            Err(problem) => {
                self.invalid.insert(relation.id, problem);
            }
        }
    }
}

impl OsmLayer {
    /// Bring `member_ways` and the styles of the ways whose membership
    /// changed up to date after a relation was created, deleted or its
    /// members replaced. Does nothing for a stylesheet with no
    /// `way:in-relation` rules.
    pub(super) fn refresh_member_ways(&mut self) {
        if !self.stylesheet.uses_relation_membership() {
            return;
        }
        let Some(data) = self.osm_data.clone() else {
            return;
        };
        let now = relation_member_ways(&data, &self.stylesheet);
        let changed: Vec<i64> = now
            .symmetric_difference(&self.member_ways)
            .copied()
            .collect();
        self.member_ways = now;
        for id in changed {
            let (Some(&idx), Some(way)) = (self.way_id_to_index.get(&id), data.ways.get(&id))
            else {
                continue;
            };
            let style = self.stylesheet.way_style_cached(
                &way.tags,
                way.is_closed(),
                self.member_ways.contains(&id),
            );
            self.way_fill_tris[idx] = compute_fill_tris(&self.way_vertices[idx], &style);
            self.way_styles[idx] = style;
        }
    }

    /// Reassemble and retriangulate these relations (and drop any that are
    /// gone or no longer multipolygons). Every edit that can change a
    /// relation's rings ends in one of the `mp_refresh_*` calls.
    pub(super) fn mp_refresh_relations(&mut self, relation_ids: &[i64]) {
        let Some(data) = self.osm_data.clone() else {
            return;
        };
        let fs_phase = crate::frame_stats::start();
        self.mp
            .refresh(relation_ids, &data, &self.node_cache, &self.stylesheet);
        if !relation_ids.is_empty() {
            crate::frame_stats::lap("cache.mp_refresh", fs_phase);
        }
    }

    /// Refresh the multipolygon relations that list any of these ways. Cheap
    /// (a map lookup) when none does, which is the common case.
    pub(super) fn mp_refresh_ways(&mut self, way_ids: &[i64]) {
        let mut relations: Vec<i64> = way_ids
            .iter()
            .filter_map(|w| self.mp.way_to_rels.get(w))
            .flatten()
            .copied()
            .collect();
        relations.sort_unstable();
        relations.dedup();
        self.mp_refresh_relations(&relations);
    }

    /// The route overlay colour of a way, as the renderer sees it.
    #[cfg(test)]
    pub(crate) fn route_overlay_colour(&self, way_id: i64) -> Option<u32> {
        self.mp.route_colour_of_way(way_id)
    }

    /// The drawn restriction markers: `(relation id, mercator x, y, only)`.
    #[cfg(test)]
    pub(crate) fn restriction_markers(&self) -> Vec<(i64, f64, f64, bool)> {
        self.mp
            .restrictions
            .iter()
            .map(|(&id, r)| (id, r.x, r.y, r.only))
            .collect()
    }

    /// Refresh the turn restrictions whose `via` node is one of these.
    pub(super) fn mp_refresh_nodes(&mut self, node_ids: &[i64]) {
        if self.mp.node_to_rels.is_empty() {
            return;
        }
        let mut relations: Vec<i64> = node_ids
            .iter()
            .filter_map(|n| self.mp.node_to_rels.get(n))
            .flatten()
            .copied()
            .collect();
        relations.sort_unstable();
        relations.dedup();
        self.mp_refresh_relations(&relations);
    }

    /// How many times the multipolygon cache has been rebuilt or patched.
    #[cfg(test)]
    pub(crate) fn multipolygon_version(&self) -> u64 {
        self.mp.version
    }

    /// The cached mercator position of `node_id` in the relation's polygons,
    /// and the relation's total fill triangle count.
    #[cfg(test)]
    pub(crate) fn multipolygon_geometry(
        &self,
        relation_id: i64,
        node_id: i64,
    ) -> Option<((f64, f64), usize)> {
        let entry = self.mp.entries.get(&relation_id)?;
        let tris = entry.polygons.iter().map(|p| p.tris.len() / 3).sum();
        let pos = entry
            .polygons
            .iter()
            .flat_map(|p| p.verts.iter())
            .find(|v| v.0 == node_id)
            .map(|v| (v.1, v.2))?;
        Some((pos, tris))
    }

    /// What became of this relation in the multipolygon cache.
    #[cfg(test)]
    pub(crate) fn multipolygon_state(&self, relation_id: i64) -> MpState {
        if let Some(entry) = self.mp.entries.get(&relation_id) {
            MpState::Valid {
                polygons: entry.polygons.len(),
                holes: entry.polygons.iter().map(|p| p.ring_count() - 1).sum(),
            }
        } else if let Some(&problem) = self.mp.invalid.get(&relation_id) {
            MpState::Invalid(problem)
        } else {
            MpState::NotArea
        }
    }

    pub(super) fn relations_containing(
        &self,
        member_type: crate::osm::MemberType,
        id: i64,
    ) -> Vec<i64> {
        self.osm_data
            .as_ref()
            .map(|d| {
                d.relations
                    .iter()
                    .filter(|r| {
                        r.members
                            .iter()
                            .any(|m| m.member_type == member_type && m.reference == id)
                    })
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default()
    }
}
