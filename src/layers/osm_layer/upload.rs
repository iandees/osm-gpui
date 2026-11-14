use super::*;

impl OsmLayer {
    /// The layer's current data and its server baseline, for the autosave to
    /// serialize elsewhere. Cheap: two reference-count bumps, nothing is
    /// copied (an edit made while the other side still holds them pays one
    /// copy-on-write of the data instead). `None` for a layer with no data.
    pub fn autosave_snapshot(&self) -> Option<(Arc<OsmData>, Arc<OsmData>)> {
        let current = self.osm_data.clone()?;
        let original = self
            .original_data
            .clone()
            .unwrap_or_else(|| current.clone());
        Some((current, original))
    }

    /// Run `change` on copies of the baseline and current data and, if it
    /// reports a change, install them: caches rebuilt like `merge_download`
    /// (this is a once-per-conflict path), the baseline kept separate from
    /// the data, and `modified` recomputed from the diff since a resolution
    /// can leave nothing to upload.
    pub(super) fn change_snapshots(
        &mut self,
        change: impl FnOnce(&mut OsmData, &mut OsmData) -> bool,
    ) -> bool {
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return false;
        };
        let (mut current, mut original) = ((*current).clone(), (*original).clone());
        if !change(&mut original, &mut current) {
            return false;
        }
        self.set_osm_data(Arc::new(current));
        self.original_data = Some(Arc::new(original));
        self.modified = !self.diff_for_upload().is_empty();
        self.revision += 1;
        true
    }

    /// Compute the diff between this layer's original (last-synced)
    /// snapshot and its current data — see `diff::diff_osm_data`. Returns
    /// an all-empty `LayerDiff` if there's no data or no original snapshot
    /// captured (the latter shouldn't happen in practice, since both
    /// `new_with_data` and `set_osm_data` always set one, but is handled
    /// defensively by treating everything current as newly created).
    pub fn diff_for_upload(&self) -> LayerDiff {
        let Some(current) = &self.osm_data else {
            return LayerDiff::default();
        };
        match &self.original_data {
            Some(original) => diff_osm_data(original, current),
            None => {
                let empty = OsmData {
                    nodes: HashMap::new(),
                    ways: HashMap::new(),
                    relations: Vec::new(),
                    bounds: Vec::new(),
                };
                diff_osm_data(&empty, current)
            }
        }
    }

    /// Reconcile this layer's data with a successful changeset upload:
    /// remap any locally-created node/way ids (including in a way's `nodes`
    /// list, so a way that referenced a newly-created node's placeholder id
    /// doesn't end up pointing at a now-meaningless id) to their real
    /// server-assigned ids, and update every affected node/way's `version`.
    /// Then rebuilds every derived cache from scratch via `set_osm_data`
    /// (which also resets `original_data` to this reconciled state and
    /// clears `modified`).
    ///
    /// A full rebuild — rather than incrementally patching each cache like
    /// `commit_node_moves` does — is used here deliberately: this runs once
    /// per upload (not a per-frame/per-drag hot path), and a bulk id remap
    /// can touch an unbounded number of nodes/ways/index entries, so the
    /// incremental-patch discipline used elsewhere isn't worth the added
    /// complexity/risk for a one-time reconciliation pass.
    pub fn apply_upload_result(&mut self, result: &UploadResult) {
        let Some(current) = self.osm_data.clone() else {
            return;
        };
        let mut data = (*current).clone();
        Self::remap_osm_data(&mut data, result);
        self.set_osm_data(Arc::new(data));
    }

    /// Reconcile this layer with ONE accepted chunk of an upload that is
    /// still going (or stopped part way), leaving the edits of the chunks not
    /// yet accepted pending: `result`'s ids and versions are applied to the
    /// data as in `apply_upload_result`, and the server baseline is advanced
    /// by exactly the objects in `chunk` (created ones added under their new
    /// ids, modified ones replaced, deleted ones removed), so the next
    /// `diff_for_upload` is what remains to be sent. `chunk` is the diff
    /// that was sent for `result` (placeholder ids as the layer still holds
    /// them for its own new objects).
    pub fn apply_chunk_result(&mut self, result: &UploadResult, chunk: &LayerDiff) {
        let (Some(current), Some(original)) = (self.osm_data.clone(), self.original_data.clone())
        else {
            return;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        Self::remap_osm_data(&mut data, result);

        let new_id = |remap: &HashMap<i64, (i64, i32)>, id: i64| remap.get(&id).map(|&(n, _)| n);
        for n in &chunk.created_nodes {
            if let Some(id) = new_id(&result.node_id_remap, n.id) {
                if let Some(node) = data.nodes.get(&id) {
                    base.nodes.insert(id, node.clone());
                }
            }
        }
        for n in &chunk.modified_nodes {
            if let Some(node) = data.nodes.get(&n.id) {
                base.nodes.insert(n.id, node.clone());
            }
        }
        for w in &chunk.created_ways {
            if let Some(id) = new_id(&result.way_id_remap, w.id) {
                if let Some(way) = data.ways.get(&id) {
                    base.ways.insert(id, way.clone());
                }
            }
        }
        for w in &chunk.modified_ways {
            if let Some(way) = data.ways.get(&w.id) {
                base.ways.insert(w.id, way.clone());
            }
        }
        for r in &chunk.created_relations {
            if let Some(id) = new_id(&result.relation_id_remap, r.id) {
                if let Some(rel) = data.relations.iter().find(|x| x.id == id) {
                    base.relations.retain(|x| x.id != id);
                    base.relations.push(rel.clone());
                }
            }
        }
        for r in &chunk.modified_relations {
            if let Some(rel) = data.relations.iter().find(|x| x.id == r.id) {
                match base.relations.iter_mut().find(|x| x.id == r.id) {
                    Some(slot) => *slot = rel.clone(),
                    None => base.relations.push(rel.clone()),
                }
            }
        }
        for &(id, _) in &chunk.deleted_node_ids {
            if !data.nodes.contains_key(&id) {
                base.nodes.remove(&id);
            }
        }
        for &(id, _) in &chunk.deleted_way_ids {
            if !data.ways.contains_key(&id) {
                base.ways.remove(&id);
            }
        }
        for &(id, _) in &chunk.deleted_relation_ids {
            if !data.relations.iter().any(|x| x.id == id) {
                base.relations.retain(|x| x.id != id);
            }
        }

        self.set_osm_data(Arc::new(data));
        // `set_osm_data` treats its argument as a fresh server baseline;
        // restore the real one so the unsent chunks stay pending.
        self.original_data = Some(Arc::new(base));
        self.modified = !self.diff_for_upload().is_empty();
    }

    pub(super) fn remap_osm_data(data: &mut OsmData, result: &UploadResult) {
        // Remap node ids (covers true creates, where old id != new id, and
        // version-only updates for modified nodes, where old id == new id).
        let mut node_id_map: HashMap<i64, i64> = HashMap::new();
        let mut updates: Vec<(i64, i64, i32)> = Vec::new();
        for (&old_id, &(new_id, new_version)) in &result.node_id_remap {
            if data.nodes.contains_key(&old_id) {
                node_id_map.insert(old_id, new_id);
                updates.push((old_id, new_id, new_version));
            }
        }
        for (old_id, new_id, new_version) in updates {
            if let Some(mut node) = data.nodes.remove(&old_id) {
                node.id = new_id;
                node.version = new_version;
                data.nodes.insert(new_id, node);
            }
        }
        if !node_id_map.is_empty() {
            for way in data.ways.values_mut() {
                for nid in &mut way.nodes {
                    if let Some(&new_id) = node_id_map.get(nid) {
                        *nid = new_id;
                    }
                }
            }
        }

        // Remap way ids/versions (same create-vs-modify unification as
        // nodes). Since a way's id is also its `data.ways` key, this must
        // remove-then-reinsert under the new id rather than mutating
        // `way.id` in place through `values_mut()`, which would leave the
        // entry keyed under the stale id.
        let way_updates: Vec<(i64, i64, i32)> = data
            .ways
            .keys()
            .filter_map(|&old_id| {
                result
                    .way_id_remap
                    .get(&old_id)
                    .map(|&(new_id, new_version)| (old_id, new_id, new_version))
            })
            .collect();
        let mut way_id_map: HashMap<i64, i64> = HashMap::new();
        for (old_id, new_id, new_version) in way_updates {
            if let Some(mut way) = data.ways.remove(&old_id) {
                way.id = new_id;
                way.version = new_version;
                data.ways.insert(new_id, way);
                way_id_map.insert(old_id, new_id);
            }
        }

        // Relations: member refs to created nodes/ways/relations switch from
        // their placeholder ids to the real ones, and a relation the server
        // accepted gets its new id (when created) and version.
        let relation_id_map: HashMap<i64, i64> = data
            .relations
            .iter()
            .filter_map(|r| {
                result
                    .relation_id_remap
                    .get(&r.id)
                    .map(|&(new_id, _)| (r.id, new_id))
            })
            .collect();
        for relation in &mut data.relations {
            for member in &mut relation.members {
                let map = match member.member_type {
                    crate::osm::MemberType::Node => &node_id_map,
                    crate::osm::MemberType::Way => &way_id_map,
                    crate::osm::MemberType::Relation => &relation_id_map,
                };
                if let Some(&new_id) = map.get(&member.reference) {
                    member.reference = new_id;
                }
            }
            if let Some(&(new_id, new_version)) = result.relation_id_remap.get(&relation.id) {
                relation.id = new_id;
                relation.version = new_version;
            }
        }
    }
}
