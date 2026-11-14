use super::*;

impl OsmLayer {
    /// Apply `edit` to the node list of way `way_id`, then bring the way's
    /// caches and the multipolygons up to date and mark the layer modified.
    /// `edit` returns `Err`, having changed nothing, to refuse.
    pub(super) fn edit_way_nodes<R>(
        &mut self,
        way_id: i64,
        edit: impl FnOnce(&mut Vec<i64>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        if !self.derived.way_id_to_index.contains_key(&way_id) {
            return Err(EditError::UnknownWay(way_id));
        }
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        let way = data
            .ways
            .get_mut(&way_id)
            .ok_or(EditError::UnknownWay(way_id))?;
        let old = way.nodes.clone();
        let result = edit(&mut way.nodes)?;
        self.apply_change(&Change {
            way_nodes: vec![(way_id, old)],
            ..Change::default()
        });
        Ok(result)
    }

    /// Why `take_node` refused: the node is in a way, or does not exist.
    pub(super) fn why_node_stays(&self, id: i64) -> EditError {
        if self
            .derived
            .node_to_ways
            .get(&id)
            .is_some_and(|w| !w.is_empty())
        {
            EditError::NodeInWay(id)
        } else {
            EditError::UnknownNode(id)
        }
    }

    /// Apply `edit` to the tags of a node, way or relation, then refresh the
    /// cached style that follows from them.
    pub(super) fn edit_tags(
        &mut self,
        kind: FeatureKind,
        id: i64,
        edit: impl FnOnce(&mut HashMap<String, String>),
    ) -> Result<(), EditError> {
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        let tags = match kind {
            FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
            FeatureKind::Way => data.ways.get_mut(&id).map(|w| &mut w.tags),
            FeatureKind::Relation => data
                .relations
                .iter_mut()
                .find(|r| r.id == id)
                .map(|r| &mut r.tags),
        };
        let tags = tags.ok_or_else(|| EditError::unknown(kind, id))?;
        edit(tags);
        // A relation's style (or whether it is a multipolygon at all)
        // follows its tags.
        self.apply_change(&Change {
            retagged: vec![(kind, id)],
            ..Change::default()
        });
        Ok(())
    }

    /// Insert (or overwrite) a node, creating empty data if the layer has
    /// none, and patch the caches. Keeps `next_new_id` below `id` so a later
    /// auto-allocation never collides with an explicitly inserted id (a redo).
    pub(super) fn insert_node(
        &mut self,
        id: i64,
        lat: f64,
        lon: f64,
        tags: HashMap<String, String>,
        version: i32,
        restored: bool,
    ) {
        let node = OsmNode {
            id,
            lat,
            lon,
            version,
            tags,
        };
        let arc = self
            .store
            .osm_data
            .get_or_insert_with(|| Arc::new(OsmData::default()));
        Arc::make_mut(arc).nodes.insert(id, node);
        if id <= self.store.next_new_id {
            self.store.next_new_id = id - 1;
        }
        let mut change = Change::default();
        if restored {
            change.nodes_restored.push(id);
        } else {
            change.nodes_added.push(id);
        }
        self.apply_change(&change);
    }

    /// Remove a node from the data and caches. Refuses (`None`, nothing
    /// changed) if the node is unknown or still in a way: removing it would
    /// leave the way pointing at nothing, so the way goes first.
    pub(super) fn take_node(&mut self, id: i64) -> Option<OsmNode> {
        if self
            .derived
            .node_to_ways
            .get(&id)
            .is_some_and(|ways| !ways.is_empty())
        {
            return None;
        }
        let node = Arc::make_mut(self.store.osm_data.as_mut()?)
            .nodes
            .remove(&id)?;
        self.apply_change(&Change {
            nodes_removed: vec![id],
            ..Change::default()
        });
        Some(node)
    }

    /// Remove a way from the data and caches; its nodes stay. `None`
    /// (nothing changed) if the way is unknown.
    pub(super) fn take_way(&mut self, id: i64) -> Option<OsmWay> {
        if !self.derived.way_id_to_index.contains_key(&id) {
            return None;
        }
        let way = Arc::make_mut(self.store.osm_data.as_mut()?)
            .ways
            .remove(&id)?;
        self.apply_change(&Change {
            ways_removed: vec![id],
            ..Change::default()
        });
        Some(way)
    }

    /// Add `way` to the data and every cache, creating empty data if the
    /// layer has none. Member nodes that are not currently cached (deleted,
    /// not yet restored) contribute no vertex, as in `compute_way_tables`.
    pub(super) fn insert_way(&mut self, way: OsmWay) {
        let id = way.id;
        let arc = self
            .store
            .osm_data
            .get_or_insert_with(|| Arc::new(OsmData::default()));
        Arc::make_mut(arc).ways.insert(id, way);
        self.apply_change(&Change {
            ways_added: vec![id],
            ..Change::default()
        });
    }
}

impl EditableLayer for OsmLayer {
    fn set_highlight(&mut self, features: &[FeatureRef]) {
        self.view.highlight = features.to_vec();
    }

    fn update_vertex_preview(
        &mut self,
        viewport: &Viewport,
        pt: Point<Pixels>,
        hovered: Option<&FeatureRef>,
        enabled: bool,
    ) -> bool {
        let next = if !enabled
            || self.store.osm_data.is_none()
            || viewport.zoom_level() < node_markers::PREVIEW_MIN_ZOOM
        {
            None
        } else {
            match hovered {
                Some(h) if h.layer_id == self.id => match h.kind {
                    FeatureKind::Way => self
                        .derived
                        .way_id_to_index
                        .contains_key(&h.id)
                        .then_some(h.id),
                    FeatureKind::Node => {
                        self.view.preview_way.filter(|&w| self.node_in_way(h.id, w))
                    }
                    FeatureKind::Relation => None,
                },
                Some(_) => None,
                None => self.view.preview_way.filter(|&w| {
                    self.way_screen_distance(viewport, pt, w)
                        .is_some_and(|d| d <= WAY_HIT_TOL + node_markers::PREVIEW_HIT_RADIUS)
                }),
            }
        };
        let changed = next != self.view.preview_way;
        self.view.preview_way = next;
        changed
    }

    fn set_drag_preview(&mut self, node_ids: &HashSet<i64>, delta: Point<Pixels>) {
        self.view.drag_preview = Some((node_ids.clone(), delta));
    }

    fn clear_drag_preview(&mut self) {
        self.view.drag_preview = None;
    }

    fn node_lat_lon(&self, node_id: i64) -> Option<(f64, f64)> {
        let data = self.store.osm_data.as_ref()?;
        let n = data.nodes.get(&node_id)?;
        Some((n.lat, n.lon))
    }

    fn way_node_ids(&self, way_id: i64) -> Option<Vec<i64>> {
        let data = self.store.osm_data.as_ref()?;
        let way = data.ways.get(&way_id)?;
        Some(way.nodes.clone())
    }

    fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) -> Result<(), EditError> {
        if moves.is_empty() {
            return Ok(());
        }
        let fs_phase = crate::frame_stats::start();
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        if let Some(&(id, _, _)) = moves.iter().find(|m| !data.nodes.contains_key(&m.0)) {
            return Err(EditError::UnknownNode(id));
        }
        let mut moved_ids: Vec<i64> = Vec::with_capacity(moves.len());
        for &(id, lat, lon) in moves {
            if let Some(node) = data.nodes.get_mut(&id) {
                node.lat = lat;
                node.lon = lon;
                moved_ids.push(id);
            }
        }

        // Only ways through a moved node can have a stale vertex list or bbox.
        let mut touched_ways: HashSet<usize> = HashSet::new();
        for &id in &moved_ids {
            if let Some(way_idxs) = self.derived.node_to_ways.get(&id) {
                touched_ways.extend(way_idxs.iter().copied());
            }
        }
        let ways_reprojected: Vec<i64> = touched_ways
            .iter()
            .map(|&i| self.derived.way_ids[i])
            .collect();
        self.apply_change(&Change {
            nodes_moved: moved_ids,
            ways_reprojected,
            ..Change::default()
        });
        crate::frame_stats::lap("edit.commit_node_moves_total", fs_phase);
        Ok(())
    }

    fn set_tag(
        &mut self,
        kind: FeatureKind,
        id: i64,
        key: &str,
        value: &str,
    ) -> Result<(), EditError> {
        self.edit_tags(kind, id, |tags| {
            tags.insert(key.to_string(), value.to_string());
        })
    }

    fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) -> Result<(), EditError> {
        self.edit_tags(kind, id, |tags| {
            tags.remove(key);
        })
    }

    fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Result<i64, EditError> {
        let Some(id) = id else {
            return Ok(self.add_node(lat, lon));
        };
        if self
            .store
            .osm_data
            .as_ref()
            .is_some_and(|d| d.nodes.contains_key(&id))
        {
            return Err(EditError::IdInUse(FeatureKind::Node, id));
        }
        self.insert_node(id, lat, lon, HashMap::new(), 0, false);
        Ok(id)
    }

    fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        let id = self.store.next_new_id;
        self.store.next_new_id -= 1;
        self.insert_node(id, lat, lon, HashMap::new(), 0, false);
        id
    }

    fn delete_feature(
        &mut self,
        kind: FeatureKind,
        id: i64,
    ) -> Result<DeletedFeatureSnapshot, EditError> {
        match kind {
            FeatureKind::Node => {
                let node = self.take_node(id).ok_or_else(|| self.why_node_stays(id))?;
                Ok(DeletedFeatureSnapshot {
                    kind,
                    id,
                    tags: node.tags.into_iter().collect(),
                    way_nodes: Vec::new(),
                    node_lat_lon: Some((node.lat, node.lon)),
                    version: node.version,
                })
            }
            FeatureKind::Way => {
                let way = self.take_way(id).ok_or(EditError::UnknownWay(id))?;
                Ok(DeletedFeatureSnapshot {
                    kind,
                    id,
                    tags: way.tags.into_iter().collect(),
                    way_nodes: way.nodes,
                    node_lat_lon: None,
                    version: way.version,
                })
            }
            // A relation's snapshot is a `DeletedRelationSnapshot`.
            FeatureKind::Relation => Err(EditError::NotNodeOrWay(kind)),
        }
    }

    fn restore_feature(&mut self, snapshot: DeletedFeatureSnapshot) -> Result<(), EditError> {
        let id = snapshot.id;
        match snapshot.kind {
            FeatureKind::Node => {
                let (lat, lon) = snapshot
                    .node_lat_lon
                    .ok_or(EditError::MissingCoordinates(id))?;
                if self
                    .store
                    .osm_data
                    .as_ref()
                    .is_some_and(|d| d.nodes.contains_key(&id))
                {
                    return Err(EditError::IdInUse(FeatureKind::Node, id));
                }
                self.insert_node(
                    id,
                    lat,
                    lon,
                    snapshot.tags.into_iter().collect(),
                    snapshot.version,
                    true,
                );
                Ok(())
            }
            FeatureKind::Way => {
                if self.derived.way_id_to_index.contains_key(&id) {
                    return Err(EditError::IdInUse(FeatureKind::Way, id));
                }
                self.insert_way(OsmWay {
                    id,
                    nodes: snapshot.way_nodes,
                    version: snapshot.version,
                    tags: snapshot.tags.into_iter().collect(),
                });
                Ok(())
            }
            kind @ FeatureKind::Relation => Err(EditError::NotNodeOrWay(kind)),
        }
    }

    fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        let id = self.store.next_placeholder_way_id;
        self.store.next_placeholder_way_id -= 1;
        self.insert_way(OsmWay {
            id,
            nodes: node_ids,
            version: 0,
            tags: tags.into_iter().collect(),
        });
        id
    }

    fn extend_way(&mut self, way_id: i64, node_id: i64) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            nodes.push(node_id);
            Ok(())
        })
    }

    fn insert_node_into_way(
        &mut self,
        way_id: i64,
        index: usize,
        lat: f64,
        lon: f64,
    ) -> Result<i64, EditError> {
        // Check before creating the node, so a refusal leaves no orphan.
        let len = self
            .way_node_ids(way_id)
            .ok_or(EditError::UnknownWay(way_id))?
            .len();
        if index > len {
            return Err(EditError::IndexOutOfRange { way_id, index, len });
        }
        let new_id = self.add_node(lat, lon);
        self.edit_way_nodes(way_id, |nodes| {
            nodes.insert(index, new_id);
            Ok(new_id)
        })
    }

    fn remove_node(&mut self, node_id: i64) -> Result<(), EditError> {
        self.take_node(node_id)
            .map(drop)
            .ok_or_else(|| self.why_node_stays(node_id))
    }

    fn remove_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.take_way(way_id)
            .map(drop)
            .ok_or(EditError::UnknownWay(way_id))
    }

    fn remove_node_from_way(&mut self, way_id: i64, index: usize) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            if index >= nodes.len() {
                return Err(EditError::IndexOutOfRange {
                    way_id,
                    index,
                    len: nodes.len(),
                });
            }
            nodes.remove(index);
            Ok(())
        })
    }

    fn reverse_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            nodes.reverse();
            Ok(())
        })
    }

    fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> Result<(), EditError> {
        self.edit_way_nodes(way_id, |nodes| {
            *nodes = node_ids;
            Ok(())
        })
    }

    fn ways_using_node(&self, node_id: i64) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .derived
            .node_to_ways
            .get(&node_id)
            .into_iter()
            .flatten()
            .filter_map(|&idx| self.derived.way_ids.get(idx).copied())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn relations_containing_way(&self, way_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Way, way_id)
    }
    fn relations_containing_node(&self, node_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Node, node_id)
    }

    fn relation(&self, relation_id: i64) -> Option<crate::osm::OsmRelation> {
        self.store
            .osm_data
            .as_ref()?
            .relations
            .iter()
            .find(|r| r.id == relation_id)
            .cloned()
    }

    fn relation_ids(&self) -> Vec<i64> {
        self.store
            .osm_data
            .as_ref()
            .map(|d| d.relations.iter().map(|r| r.id).collect())
            .unwrap_or_default()
    }

    fn set_relation_members(
        &mut self,
        relation_id: i64,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<Vec<crate::osm::OsmMember>, EditError> {
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        let relation = data
            .relations
            .iter_mut()
            .find(|r| r.id == relation_id)
            .ok_or(EditError::UnknownRelation(relation_id))?;
        let previous = std::mem::replace(&mut relation.members, members);
        self.apply_change(&Change {
            relations: vec![relation_id],
            ..Change::default()
        });
        Ok(previous)
    }

    fn relations(&self) -> Vec<crate::osm::OsmRelation> {
        self.store
            .osm_data
            .as_ref()
            .map(|d| d.relations.clone())
            .unwrap_or_default()
    }

    fn relations_containing_relation(&self, relation_id: i64) -> Vec<i64> {
        self.relations_containing(crate::osm::MemberType::Relation, relation_id)
    }

    fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<crate::osm::OsmMember>,
    ) -> Result<i64, EditError> {
        let lowest_relation = self
            .store
            .osm_data
            .as_ref()
            .ok_or(EditError::NoData)?
            .relations
            .iter()
            .map(|r| r.id)
            .min()
            .unwrap_or(0);
        let id = self
            .store
            .next_new_id
            .min(self.store.next_placeholder_way_id)
            .min(lowest_relation.min(0) - 1);
        self.store.next_new_id = id - 1;
        self.store.next_placeholder_way_id = id - 1;
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        data.relations.push(crate::osm::OsmRelation {
            id,
            members,
            version: 0,
            tags: tags.into_iter().collect(),
        });
        self.apply_change(&Change {
            relations: vec![id],
            ..Change::default()
        });
        Ok(id)
    }

    fn delete_relation(&mut self, relation_id: i64) -> Result<DeletedRelationSnapshot, EditError> {
        let data = self.store.osm_data.as_ref().ok_or(EditError::NoData)?;
        if data.relations.iter().any(|r| {
            r.id != relation_id
                && r.members.iter().any(|m| {
                    m.member_type == crate::osm::MemberType::Relation && m.reference == relation_id
                })
        }) {
            return Err(EditError::RelationInUse(relation_id));
        }
        let index = data
            .relations
            .iter()
            .position(|r| r.id == relation_id)
            .ok_or(EditError::UnknownRelation(relation_id))?;
        let data = Arc::make_mut(self.store.osm_data.as_mut().ok_or(EditError::NoData)?);
        let relation = data.relations.remove(index);
        self.apply_change(&Change {
            relations: vec![relation_id],
            ..Change::default()
        });
        Ok(DeletedRelationSnapshot { relation, index })
    }

    fn restore_relation(&mut self, snapshot: DeletedRelationSnapshot) -> Result<(), EditError> {
        let arc = self.store.osm_data.as_mut().ok_or(EditError::NoData)?;
        if arc.relations.iter().any(|r| r.id == snapshot.relation.id) {
            return Err(EditError::IdInUse(
                FeatureKind::Relation,
                snapshot.relation.id,
            ));
        }
        let data = Arc::make_mut(arc);
        let id = snapshot.relation.id;
        let at = snapshot.index.min(data.relations.len());
        data.relations.insert(at, snapshot.relation);
        if id <= self.store.next_new_id {
            self.store.next_new_id = id - 1;
        }
        if id <= self.store.next_placeholder_way_id {
            self.store.next_placeholder_way_id = id - 1;
        }
        self.apply_change(&Change {
            relations: vec![id],
            ..Change::default()
        });
        Ok(())
    }

    fn has_pending_edit(&self, kind: FeatureKind, id: i64) -> bool {
        let (Some(current), Some(original)) = (&self.store.osm_data, &self.store.original_data)
        else {
            return self.store.osm_data.is_some();
        };
        match kind {
            FeatureKind::Node => match (current.nodes.get(&id), original.nodes.get(&id)) {
                (Some(a), Some(b)) => a != b,
                (Some(_), None) => true,
                _ => false,
            },
            FeatureKind::Way => match (current.ways.get(&id), original.ways.get(&id)) {
                (Some(a), Some(b)) => a != b,
                (Some(_), None) => true,
                _ => false,
            },
            FeatureKind::Relation => {
                let find = |d: &OsmData| d.relations.iter().find(|r| r.id == id).cloned();
                match (find(current), find(original)) {
                    (Some(a), Some(b)) => a != b,
                    (Some(_), None) => true,
                    _ => false,
                }
            }
        }
    }

    fn purge_features(&mut self, nodes: &[i64], ways: &[i64], relations: &[i64]) -> PurgedFeatures {
        let mut purged = PurgedFeatures::default();
        let (Some(current), Some(original)) = (
            self.store.osm_data.clone(),
            self.store.original_data.clone(),
        ) else {
            return purged;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        for &id in nodes {
            base.nodes.remove(&id);
            if let Some(n) = data.nodes.remove(&id) {
                purged.nodes.push(n);
            }
        }
        for &id in ways {
            base.ways.remove(&id);
            if let Some(w) = data.ways.remove(&id) {
                purged.ways.push(w);
            }
        }
        // Indices are read before anything is removed, so they are positions in
        // the untouched lists and `restore_purged` can re-insert in ascending
        // order.
        let doomed: HashSet<i64> = relations.iter().copied().collect();
        for (i, r) in data.relations.iter().enumerate() {
            if doomed.contains(&r.id) {
                let base_index = base
                    .relations
                    .iter()
                    .position(|b| b.id == r.id)
                    .unwrap_or(i);
                purged.relations.push((r.clone(), i, base_index));
            }
        }
        data.relations.retain(|r| !doomed.contains(&r.id));
        base.relations.retain(|r| !doomed.contains(&r.id));
        if purged.is_empty() {
            return purged;
        }
        let data = Arc::new(data);
        self.rebuild_caches(&data);
        self.store.osm_data = Some(data);
        self.store.original_data = Some(Arc::new(base));
        self.store.revision += 1;
        purged
    }

    fn restore_purged(&mut self, purged: PurgedFeatures) {
        let (Some(current), Some(original)) = (
            self.store.osm_data.clone(),
            self.store.original_data.clone(),
        ) else {
            return;
        };
        let mut data = (*current).clone();
        let mut base = (*original).clone();
        for n in purged.nodes {
            base.nodes.insert(n.id, n.clone());
            data.nodes.insert(n.id, n);
        }
        for w in purged.ways {
            base.ways.insert(w.id, w.clone());
            data.ways.insert(w.id, w);
        }
        // Lowest index first, so each insert lands where it used to be.
        let mut relations = purged.relations;
        relations.sort_by_key(|(_, i, _)| *i);
        for (r, i, _) in &relations {
            let at = (*i).min(data.relations.len());
            data.relations.insert(at, r.clone());
        }
        relations.sort_by_key(|(_, _, j)| *j);
        for (r, _, j) in relations {
            let at = j.min(base.relations.len());
            base.relations.insert(at, r);
        }
        let data = Arc::new(data);
        self.rebuild_caches(&data);
        self.store.osm_data = Some(data);
        self.store.original_data = Some(Arc::new(base));
        self.store.revision += 1;
    }

    fn hit_test(&self, viewport: &Viewport, screen_pt: Point<Pixels>) -> Vec<HitCandidate> {
        self.hit_test_point(viewport, screen_pt)
    }

    fn hit_test_interior(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        self.hit_test_interior_at(viewport, screen_pt)
    }

    fn hit_test_rect(&self, viewport: &Viewport, rect: Bounds<Pixels>) -> Vec<FeatureRef> {
        self.hit_test_rect_in(viewport, rect)
    }

    fn merge_download(&mut self, incoming: OsmData) -> MergeStats {
        let empty = || OsmData {
            nodes: HashMap::new(),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        let mut current = self
            .store
            .osm_data
            .as_deref()
            .cloned()
            .unwrap_or_else(empty);
        let mut original = self
            .store
            .original_data
            .as_deref()
            .cloned()
            .unwrap_or_else(empty);

        let stats = merge::merge_download(&mut original, &mut current, incoming);

        let modified = self.store.modified;
        self.set_osm_data(Arc::new(current));
        // `set_osm_data` treats its argument as a fresh server baseline;
        // restore the real one so pending edits stay pending.
        self.store.original_data = Some(Arc::new(original));
        self.store.modified = modified;
        self.store.revision += 1;
        stats
    }

    fn has_feature(&self, kind: FeatureKind, id: i64) -> bool {
        let Some(data) = self.store.osm_data.as_ref() else {
            return false;
        };
        match kind {
            FeatureKind::Node => data.nodes.contains_key(&id),
            FeatureKind::Way => data.ways.contains_key(&id),
            FeatureKind::Relation => data.relations.iter().any(|r| r.id == id),
        }
    }

    fn is_locally_deleted(&self, kind: FeatureKind, id: i64) -> bool {
        let Some(original) = self.store.original_data.as_ref() else {
            return false;
        };
        let known = match kind {
            FeatureKind::Node => original.nodes.contains_key(&id),
            FeatureKind::Way => original.ways.contains_key(&id),
            FeatureKind::Relation => original.relations.iter().any(|r| r.id == id),
        };
        known && !EditableLayer::has_feature(self, kind, id)
    }

    fn feature_version(&self, kind: FeatureKind, id: i64) -> Option<i32> {
        let data = self.store.osm_data.as_ref()?;
        match kind {
            FeatureKind::Node => data.nodes.get(&id).map(|n| n.version),
            FeatureKind::Way => data.ways.get(&id).map(|w| w.version),
            FeatureKind::Relation => data
                .relations
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.version),
        }
    }

    fn set_base_version(&mut self, kind: FeatureKind, id: i64, version: i32) -> bool {
        let (Some(current), Some(original)) = (
            self.store.osm_data.as_mut(),
            self.store.original_data.as_mut(),
        ) else {
            return false;
        };
        let changed = resolve::set_base_version(
            Arc::make_mut(original),
            Arc::make_mut(current),
            kind,
            id,
            version,
        );
        if changed {
            self.store.revision += 1;
        }
        changed
    }

    fn adopt_server_element(
        &mut self,
        kind: FeatureKind,
        id: i64,
        server: crate::osm_api::ServerElement,
    ) -> bool {
        self.change_snapshots(|original, current| {
            resolve::adopt_server_element(original, current, kind, id, server)
        })
    }

    fn forget_local_delete(&mut self, kind: FeatureKind, id: i64) -> bool {
        self.change_snapshots(|original, current| {
            resolve::forget_local_delete(original, current, kind, id)
        })
    }

    fn all_features(&self) -> Vec<FeatureRef> {
        let Some(data) = self.store.osm_data.as_ref() else {
            return Vec::new();
        };
        let mut node_ids: Vec<i64> = data.nodes.keys().copied().collect();
        node_ids.sort_unstable();
        let mut way_ids: Vec<i64> = data.ways.keys().copied().collect();
        way_ids.sort_unstable();
        let refs = |kind: FeatureKind, ids: Vec<i64>| {
            let layer_id = self.id;
            ids.into_iter()
                .map(move |id| FeatureRef { layer_id, kind, id })
        };
        let mut relation_ids: Vec<i64> = data.relations.iter().map(|r| r.id).collect();
        relation_ids.sort_unstable();
        refs(FeatureKind::Node, node_ids)
            .chain(refs(FeatureKind::Way, way_ids))
            .chain(refs(FeatureKind::Relation, relation_ids))
            .collect()
    }

    fn search_features(
        &self,
        query: &crate::search::Query,
        selected: &std::collections::HashSet<(FeatureKind, i64)>,
        only_selected: bool,
        view: Option<crate::search::GeoBounds>,
    ) -> Vec<FeatureRef> {
        let Some(data) = self.store.osm_data.as_deref() else {
            return Vec::new();
        };
        let ctx = crate::search::SearchContext {
            original: self.store.original_data.as_deref(),
            selected: Some(selected),
            view,
            only_selected,
        };
        crate::search::search(query, data, &ctx)
            .into_iter()
            .map(|(kind, id)| FeatureRef {
                layer_id: self.id,
                kind,
                id,
            })
            .collect()
    }

    fn feature_tags(&self, feature: &FeatureRef) -> Option<Vec<(String, String)>> {
        if feature.layer_id != self.id {
            return None;
        }
        let data = self.store.osm_data.as_ref()?;
        let tags = match feature.kind {
            FeatureKind::Node => {
                let n = data.nodes.get(&feature.id)?;
                n.tags.clone()
            }
            FeatureKind::Way => {
                let w = data.ways.get(&feature.id)?;
                w.tags.clone()
            }
            FeatureKind::Relation => {
                let r = data.relations.iter().find(|r| r.id == feature.id)?;
                r.tags.clone()
            }
        };
        let mut kv: Vec<(String, String)> = tags.into_iter().collect();
        kv.sort_by(|a, b| a.0.cmp(&b.0));
        Some(kv)
    }

    fn feature_geometry(
        &self,
        feature: &FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Option<crate::presets::Geometry> {
        if feature.layer_id != self.id {
            return None;
        }
        let data = self.store.osm_data.as_ref()?;
        if feature.kind == FeatureKind::Node {
            // Same answer as `classify_geometry`, from the node-to-ways
            // index instead of a scan of every way: a relation lists many
            // nodes and this runs for each of them every frame.
            data.nodes.get(&feature.id)?;
            let used = self
                .derived
                .node_to_ways
                .get(&feature.id)
                .is_some_and(|ways| !ways.is_empty());
            return Some(if used {
                crate::presets::Geometry::Vertex
            } else {
                crate::presets::Geometry::Point
            });
        }
        crate::presets::classify_geometry(data, feature.kind, feature.id, area_keys)
    }

    fn feature_geometry_candidates(
        &self,
        feature: &FeatureRef,
        area_keys: &crate::presets::AreaKeys,
    ) -> Vec<crate::presets::Geometry> {
        if feature.layer_id == self.id && feature.kind == FeatureKind::Way {
            if let Some(way) = self
                .store
                .osm_data
                .as_ref()
                .and_then(|d| d.ways.get(&feature.id))
            {
                return crate::presets::way_candidate_geometries(
                    &way.tags,
                    way.is_closed(),
                    area_keys,
                );
            }
        }
        self.feature_geometry(feature, area_keys)
            .into_iter()
            .collect()
    }

    fn feature_is_closed_way(&self, feature: &FeatureRef) -> bool {
        feature.layer_id == self.id
            && feature.kind == FeatureKind::Way
            && self
                .store
                .osm_data
                .as_ref()
                .and_then(|d| d.ways.get(&feature.id))
                .is_some_and(|w| w.is_closed())
    }

    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
        kind: HighlightKind,
    ) {
        self.render_highlight_of(viewport, bounds, window, feature, kind)
    }
}
