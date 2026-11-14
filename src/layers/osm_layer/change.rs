use super::*;

/// What one edit did to `Store::osm_data`. The mutator changes the data and
/// describes the result here; `OsmLayer::apply_change` then decides which
/// derived tables and render caches that invalidates.
#[derive(Default)]
pub(super) struct Change {
    /// New nodes, to be indexed.
    pub(super) nodes_added: Vec<i64>,
    /// Existing nodes whose position changed.
    pub(super) nodes_moved: Vec<i64>,
    /// Nodes put back by an undo: indexed like added ones, and the
    /// turn restrictions that use them as `via` draw again.
    pub(super) nodes_restored: Vec<i64>,
    /// Nodes already gone from the data.
    pub(super) nodes_removed: Vec<i64>,
    /// New ways, already in the data.
    pub(super) ways_added: Vec<i64>,
    /// Ways already gone from the data.
    pub(super) ways_removed: Vec<i64>,
    /// Ways whose node list was replaced, with the list they had before
    /// (`node_to_ways` needs the difference). Their geometry is recomputed.
    pub(super) way_nodes: Vec<(i64, Vec<i64>)>,
    /// Ways whose node list is unchanged but whose geometry moved with a
    /// node.
    pub(super) ways_reprojected: Vec<i64>,
    /// Features whose tags changed: their cached style is re-resolved.
    pub(super) retagged: Vec<(FeatureKind, i64)>,
    /// Relations created, deleted or given other members: their
    /// multipolygons are reassembled and `member_ways` is recomputed.
    pub(super) relations: Vec<i64>,
}

impl Change {
    pub(super) fn is_empty(&self) -> bool {
        self.nodes_added.is_empty()
            && self.nodes_moved.is_empty()
            && self.nodes_restored.is_empty()
            && self.nodes_removed.is_empty()
            && self.ways_added.is_empty()
            && self.ways_removed.is_empty()
            && self.way_nodes.is_empty()
            && self.ways_reprojected.is_empty()
            && self.retagged.is_empty()
            && self.relations.is_empty()
    }
}

impl OsmLayer {
    /// Patch every derived table and render cache for `change`, which the
    /// caller has already applied to `osm_data`, then mark the layer
    /// modified and bump `revision` once. An empty change does nothing.
    pub(super) fn apply_change(&mut self, change: &Change) {
        if change.is_empty() {
            return;
        }
        self.store.modified = true;
        self.store.revision += 1;
        let Some(data) = self.store.osm_data.clone() else {
            return;
        };

        for &id in &change.nodes_removed {
            self.index_node_removed(id);
        }
        for id in change
            .nodes_moved
            .iter()
            .chain(&change.nodes_added)
            .chain(&change.nodes_restored)
        {
            if let Some(node) = data.nodes.get(id) {
                self.index_node_set(node);
            }
        }

        for &id in &change.ways_removed {
            if let Some(&way_idx) = self.derived.way_id_to_index.get(&id) {
                self.index_way_removed(way_idx, id);
            }
        }
        for id in &change.ways_added {
            if let Some(way) = data.ways.get(id) {
                self.index_way_added(way);
            }
        }
        for (id, old) in &change.way_nodes {
            if let (Some(&way_idx), Some(way)) =
                (self.derived.way_id_to_index.get(id), data.ways.get(id))
            {
                self.index_way_nodes_changed(way_idx, old, &way.nodes);
            }
        }
        for id in change
            .way_nodes
            .iter()
            .map(|(id, _)| id)
            .chain(&change.ways_reprojected)
        {
            if let (Some(&way_idx), Some(way)) =
                (self.derived.way_id_to_index.get(id), data.ways.get(id))
            {
                self.reproject_way(way_idx, way);
            }
        }

        for &(kind, id) in &change.retagged {
            apply_style_refresh(
                kind,
                id,
                &data,
                &mut self.derived.node_cache,
                &self.derived.way_id_to_index,
                &self.derived.way_vertices,
                &mut self.render.way_styles,
                &mut self.render.way_fill_tris,
                &self.store.stylesheet,
                &self.render.member_ways,
            );
        }

        let relations: Vec<i64> = change
            .retagged
            .iter()
            .filter(|(kind, _)| *kind == FeatureKind::Relation)
            .map(|&(_, id)| id)
            .chain(change.relations.iter().copied())
            .collect();
        self.mp_refresh_relations(&relations);
        let ways: Vec<i64> = change
            .ways_added
            .iter()
            .chain(&change.ways_removed)
            .chain(change.way_nodes.iter().map(|(id, _)| id))
            .chain(&change.ways_reprojected)
            .copied()
            .collect();
        self.mp_refresh_ways(&ways);
        let nodes: Vec<i64> = change
            .nodes_moved
            .iter()
            .chain(&change.nodes_restored)
            .chain(&change.nodes_removed)
            .copied()
            .collect();
        self.mp_refresh_nodes(&nodes);
        if !change.relations.is_empty() {
            self.refresh_member_ways();
        }
    }
}
