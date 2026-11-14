use super::*;

/// Per-way axis-aligned bounding box in Web Mercator meters. Used to cull
/// offscreen ways with a cheap min/max compare against the viewport's
/// mercator-space view bounds — no trig per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct WayBbox {
    pub(super) min_x: f64,
    pub(super) max_x: f64,
    pub(super) min_y: f64,
    pub(super) max_y: f64,
}

impl WayBbox {
    /// A box containing nothing, for `extend` to grow.
    pub(super) fn empty() -> Self {
        Self {
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

    /// Conservatively grow this bbox to include `(x, y)`. Never shrinks.
    pub(super) fn extend(&mut self, x: f64, y: f64) {
        if x < self.min_x {
            self.min_x = x;
        }
        if x > self.max_x {
            self.max_x = x;
        }
        if y < self.min_y {
            self.min_y = y;
        }
        if y > self.max_y {
            self.max_y = y;
        }
    }
}

pub(super) fn way_rtree_entry(b: WayBbox, way_id: i64) -> GeomWithData<Rectangle<[f64; 2]>, i64> {
    GeomWithData::new(
        Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
        way_id,
    )
}

/// Pre-projected node coordinates (Web Mercator meters) aligned with the
/// iteration order used by the render loops. Computing this once at
/// `set_osm_data` time eliminates the per-frame `lat_lon_to_mercator` (tan+ln)
/// from every node and way vertex. Also caches each node's resolved style
/// (color/size), resolved once from the stylesheet + tags here instead of
/// every frame in the render loop.
#[derive(Debug, Clone)]
pub(super) struct NodeCache {
    /// node id -> index into `flat`/`styles`. Used by the way-vertex build
    /// pass and by incremental cache updates (`commit_node_moves`).
    pub(super) index_by_id: HashMap<i64, usize>,
    /// Flat list of all nodes as `(id, mercator_x, mercator_y)` for cache-
    /// friendly iteration in the node paint loop.
    pub(super) flat: Vec<(i64, f64, f64)>,
    /// Resolved style per node, aligned with `flat` by index.
    pub(super) styles: Vec<NodeStyle>,
}

pub(super) fn compute_node_cache(data: &OsmData, stylesheet: &Stylesheet) -> NodeCache {
    let mut index_by_id = HashMap::with_capacity(data.nodes.len());
    let mut flat = Vec::with_capacity(data.nodes.len());
    let mut styles = Vec::with_capacity(data.nodes.len());
    for node in data.nodes.values() {
        if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
            let (mx, my) = lat_lon_to_mercator(lat, lon);
            index_by_id.insert(node.id, flat.len());
            flat.push((node.id, mx, my));
            styles.push(stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM));
        }
    }
    NodeCache {
        index_by_id,
        flat,
        styles,
    }
}

pub(super) fn compute_layer_bbox(node_cache: &NodeCache) -> Option<WayBbox> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(_id, mx, my) in &node_cache.flat {
        if mx < min_x {
            min_x = mx;
        }
        if mx > max_x {
            max_x = mx;
        }
        if my < min_y {
            min_y = my;
        }
        if my > max_y {
            max_y = my;
        }
    }
    if min_x.is_finite() {
        Some(WayBbox {
            min_x,
            max_x,
            min_y,
            max_y,
        })
    } else {
        None
    }
}

/// Project one way's member nodes into mercator-space vertices (paired with
/// their node ids, so callers can match against an active `drag_preview`
/// set) and compute its bbox, both from `node_cache` in a single pass. Node
/// ids not present in `node_cache` (invalid coords, or a currently-deleted
/// node) are skipped, same as `compute_way_tables` always did. Shared by
/// `compute_way_tables` (full rebuild), `commit_node_moves` (per-touched-way
/// patch), and `restore_way` (single restored way) so the projection math
/// lives in exactly one place.
pub(super) fn project_way_vertices(
    way: &OsmWay,
    node_cache: &NodeCache,
) -> (WayGeom, Option<WayBbox>) {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut verts = Vec::with_capacity(way.nodes.len());
    for nid in &way.nodes {
        if let Some(&idx) = node_cache.index_by_id.get(nid) {
            let (_, mx, my) = node_cache.flat[idx];
            if mx < min_x {
                min_x = mx;
            }
            if mx > max_x {
                max_x = mx;
            }
            if my < min_y {
                min_y = my;
            }
            if my > max_y {
                max_y = my;
            }
            verts.push((*nid, mx, my));
        }
    }
    let bbox = if verts.is_empty() {
        None
    } else {
        Some(WayBbox {
            min_x,
            max_x,
            min_y,
            max_y,
        })
    };
    (WayGeom::new(verts), bbox)
}

/// A way's projected vertices (`(node id, x, y)`, Web Mercator meters) with
/// each vertex's *significance*: the Douglas-Peucker deviation, in meters, at
/// which the vertex first becomes necessary. Dropping every vertex whose
/// significance is under a tolerance gives exactly the Douglas-Peucker
/// simplification at that tolerance, so the renderer can simplify a way to
/// any zoom with one compare per vertex (and project only the survivors).
///
/// Built only by [`WayGeom::new`], which every path that (re)projects a way
/// goes through (`compute_way_tables`, `commit_node_moves`, `restore_way`,
/// add/replace way), so the significance is always that of the current
/// geometry: an edit replaces the whole `WayGeom` of each way it touches.
/// Derefs to the vertex slice for everything that does not care.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct WayGeom {
    pub(super) verts: Vec<(i64, f64, f64)>,
    pub(super) sig: Vec<f32>,
}

impl std::ops::Deref for WayGeom {
    type Target = [(i64, f64, f64)];
    fn deref(&self) -> &Self::Target {
        &self.verts
    }
}

impl WayGeom {
    pub(super) fn new(verts: Vec<(i64, f64, f64)>) -> Self {
        let sig = vertex_significance(&verts);
        Self { verts, sig }
    }
}

/// Douglas-Peucker significance of every vertex of a polyline or ring (see
/// [`WayGeom`]): the end points get infinity; each other vertex gets the
/// distance (meters) from the chord of the range that was split at it.
/// Iterative (no recursion) so a long coastline cannot overflow the stack.
pub(super) fn vertex_significance(verts: &[(i64, f64, f64)]) -> Vec<f32> {
    let n = verts.len();
    let mut sig = vec![0.0f32; n];
    if n == 0 {
        return sig;
    }
    sig[0] = f32::INFINITY;
    sig[n - 1] = f32::INFINITY;
    let mut stack: Vec<(usize, usize)> = vec![(0, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (_, ax, ay) = verts[lo];
        let (_, bx, by) = verts[hi];
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let mut best = lo;
        let mut best_d2 = -1.0f64;
        for (k, &(_, px_, py_)) in verts.iter().enumerate().take(hi).skip(lo + 1) {
            // Distance to the chord line; to the end point for a closed ring
            // (zero-length chord).
            let d2 = if len2 > 0.0 {
                let cross = dx * (py_ - ay) - dy * (px_ - ax);
                cross * cross / len2
            } else {
                (px_ - ax) * (px_ - ax) + (py_ - ay) * (py_ - ay)
            };
            if d2 > best_d2 {
                best_d2 = d2;
                best = k;
            }
        }
        // Significance can only shrink along a branch of the split tree, so
        // clamp to the parent's (the property that makes thresholding equal
        // to running Douglas-Peucker at that tolerance).
        let parent = {
            let lo_s = sig[lo];
            let hi_s = sig[hi];
            lo_s.min(hi_s)
        };
        sig[best] = (best_d2.sqrt() as f32).min(parent);
        stack.push((lo, best));
        stack.push((best, hi));
    }
    sig
}

/// Per-way tables produced by [`compute_way_tables`]: parallel vectors of
/// way ids, bounding boxes, pre-projected `(node_id, x, y)` vertex lists,
/// resolved styles, and fill triangulations, all indexed identically.
pub(super) type WayTables = (
    Vec<i64>,
    Vec<Option<WayBbox>>,
    Vec<WayGeom>,
    Vec<WayStyle>,
    Vec<Option<Vec<u32>>>,
);

/// Earcut triangle indices for a closed, fill-styled way's projected ring,
/// or `None` when the way shouldn't be filled (open ring, no fill style,
/// degenerate geometry). Indices reference `verts[..verts.len()-1]` — the
/// duplicated closing vertex is excluded. Never panics: earcut failure or
/// an empty result just means no fill. The ring check runs on the
/// *projected* vertex list, not the raw node refs — if a member node failed
/// to resolve, `verts` may be shorter and no longer ring-shaped, and the
/// node-id equality check catches that safely.
pub(super) fn compute_fill_tris(verts: &[(i64, f64, f64)], style: &WayStyle) -> Option<Vec<u32>> {
    style.fill?;
    if verts.len() < 4 || verts.first()?.0 != verts.last()?.0 {
        return None;
    }
    let ring = &verts[..verts.len() - 1];
    let mut flat = Vec::with_capacity(ring.len() * 2);
    for &(_, x, y) in ring {
        flat.push(x);
        flat.push(y);
    }
    let tris = earcutr::earcut(&flat, &[], 2).ok()?;
    if tris.is_empty() {
        return None;
    }
    Some(tris.into_iter().map(|i| i as u32).collect())
}

/// Build per-way bboxes, pre-projected vertex lists, resolved styles, and
/// fill triangulations in a single pass so neither the bbox pass nor the
/// render path has to walk the node HashMap (or the stylesheet, or the
/// tessellator) per vertex/way.
pub(super) fn compute_way_tables(
    data: &OsmData,
    node_cache: &NodeCache,
    stylesheet: &Stylesheet,
    members: &HashSet<i64>,
) -> WayTables {
    let mut ids = Vec::with_capacity(data.ways.len());
    let mut bboxes = Vec::with_capacity(data.ways.len());
    let mut vertices = Vec::with_capacity(data.ways.len());
    let mut styles = Vec::with_capacity(data.ways.len());
    let mut fill_tris = Vec::with_capacity(data.ways.len());
    for way in sorted_ways(data) {
        let (verts, bbox) = project_way_vertices(way, node_cache);
        let style =
            stylesheet.way_style_cached(&way.tags, way.is_closed(), members.contains(&way.id));
        fill_tris.push(compute_fill_tris(&verts, &style));
        ids.push(way.id);
        bboxes.push(bbox);
        vertices.push(verts);
        styles.push(style);
    }
    (ids, bboxes, vertices, styles, fill_tris)
}

/// Recompute and return the resolved style for a single already-mutated
/// feature's tags, for the caller to store into the appropriate cache slot.
/// Shared by `set_tag`/`remove_tag` (identical "look up which cache array
/// entry this feature's style lives at" dispatch, previously duplicated).
/// Free function (not a method) so it can be called while the caller still
/// holds a live `&mut OsmData` borrowed out of `self.osm_data` via
/// `Arc::make_mut` — a `&mut self` method call wouldn't type-check there.
/// That split-borrow role is also why it takes each cache slice as its own
/// parameter rather than a struct.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_style_refresh(
    kind: FeatureKind,
    id: i64,
    data: &OsmData,
    node_cache: &mut NodeCache,
    way_id_to_index: &HashMap<i64, usize>,
    way_vertices: &[WayGeom],
    way_styles: &mut [WayStyle],
    way_fill_tris: &mut [Option<Vec<u32>>],
    stylesheet: &Stylesheet,
    members: &HashSet<i64>,
) {
    match kind {
        FeatureKind::Node => {
            let Some(node) = data.nodes.get(&id) else {
                return;
            };
            if let Some(&idx) = node_cache.index_by_id.get(&id) {
                node_cache.styles[idx] = stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM);
            }
        }
        FeatureKind::Way => {
            let Some(way) = data.ways.get(&id) else {
                return;
            };
            if let Some(&idx) = way_id_to_index.get(&id) {
                let style =
                    stylesheet.way_style_cached(&way.tags, way.is_closed(), members.contains(&id));
                way_fill_tris[idx] = compute_fill_tris(&way_vertices[idx], &style);
                way_styles[idx] = style;
            }
        }
        // A relation has no drawn style of its own.
        FeatureKind::Relation => {}
    }
}

/// Return every way in `data.ways` sorted by id. `OsmData.ways` is a
/// `HashMap` (for O(1) lookup by id), whose iteration order is unspecified;
/// every derived cache that must line up index-for-index with another
/// derived cache (`way_bboxes`/`way_vertices`/`way_styles`/`way_id_to_index`/
/// `node_to_ways`, and the `way_index` R-tree build) is built by walking this
/// same sorted-by-id order, so rendering/iteration stays deterministic across
/// runs and reloads of the same data.
pub(super) fn sorted_ways(data: &OsmData) -> Vec<&OsmWay> {
    let mut ways: Vec<&OsmWay> = data.ways.values().collect();
    ways.sort_unstable_by_key(|w| w.id);
    ways
}

/// node id -> OSM way id, for every way that references it. Built once at
/// data-load time so `commit_node_moves` can find exactly which ways need
/// their cached vertex/bbox tables recomputed after a node move, instead of
/// rescanning every way.
pub(super) fn build_node_to_ways(ways: &[&OsmWay]) -> HashMap<i64, HashSet<usize>> {
    let mut map: HashMap<i64, HashSet<usize>> = HashMap::new();
    for (idx, way) in ways.iter().enumerate() {
        for nid in &way.nodes {
            map.entry(*nid).or_default().insert(idx);
        }
    }
    map
}

/// OSM way id -> index into `way_vertices`/`way_bboxes`/`way_styles`. Lets
/// hot paths (indexed hit-testing, incremental move updates) go straight to
/// the cached arrays instead of a linear `Vec::iter().find()` scan.
pub(super) fn build_way_id_index(ways: &[&OsmWay]) -> HashMap<i64, usize> {
    ways.iter()
        .enumerate()
        .map(|(idx, w)| (w.id, idx))
        .collect()
}

/// Bulk-build a point index over every node's mercator position.
pub(super) fn build_node_index(node_cache: &NodeCache) -> RTree<GeomWithData<[f64; 2], i64>> {
    let items: Vec<_> = node_cache
        .flat
        .iter()
        .map(|&(id, mx, my)| GeomWithData::new([mx, my], id))
        .collect();
    RTree::bulk_load(items)
}

/// Bulk-build a bounding-box index over every way's mercator bbox. Ways with
/// no valid bbox (no resolvable nodes) are skipped.
pub(super) fn build_way_index(
    way_bboxes: &[Option<WayBbox>],
    ways: &[&OsmWay],
) -> RTree<GeomWithData<Rectangle<[f64; 2]>, i64>> {
    let items: Vec<_> = way_bboxes
        .iter()
        .zip(ways.iter())
        .filter_map(|(bbox, way)| {
            let b = bbox.as_ref()?;
            Some(GeomWithData::new(
                Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way.id,
            ))
        })
        .collect();
    RTree::bulk_load(items)
}

impl OsmLayer {
    /// Recompute every derived cache and index from `osm_data`.
    pub(super) fn rebuild_caches(&mut self, osm_data: &Arc<OsmData>) {
        self.node_cache = compute_node_cache(osm_data, &self.stylesheet);
        self.member_ways = relation_member_ways(osm_data, &self.stylesheet);
        let (ids, bboxes, verts, styles, fill_tris) = compute_way_tables(
            osm_data,
            &self.node_cache,
            &self.stylesheet,
            &self.member_ways,
        );
        self.way_ids = ids;
        self.way_bboxes = bboxes;
        self.way_vertices = verts;
        self.way_styles = styles;
        self.way_fill_tris = fill_tris;
        self.mp
            .rebuild_all(osm_data, &self.node_cache, &self.stylesheet);
        self.layer_bbox = compute_layer_bbox(&self.node_cache);
        self.node_index = build_node_index(&self.node_cache);
        let ways_sorted = sorted_ways(osm_data);
        self.way_index = build_way_index(&self.way_bboxes, &ways_sorted);
        self.way_id_to_index = build_way_id_index(&ways_sorted);
        self.node_to_ways = build_node_to_ways(&ways_sorted);
        self.next_new_id = compute_next_new_id(osm_data);
        self.next_placeholder_way_id = compute_next_way_id(osm_data);
        self.download_rects = crate::download_bounds::merc_rects(&osm_data.bounds);
    }

    pub(super) fn grow_layer_bbox(&mut self, b: WayBbox) {
        match &mut self.layer_bbox {
            Some(lb) => {
                lb.extend(b.min_x, b.min_y);
                lb.extend(b.max_x, b.max_y);
            }
            None => self.layer_bbox = Some(b),
        }
    }

    /// Make the caches and indices show `node` at its current position and
    /// style, or drop it from them if its coordinates are invalid (as
    /// `compute_node_cache` never includes such nodes). `layer_bbox` only
    /// grows.
    pub(super) fn index_node_set(&mut self, node: &OsmNode) {
        let id = node.id;
        let Some((lat, lon)) = validate_coords(node.lat, node.lon) else {
            self.index_node_removed(id);
            return;
        };
        let (mx, my) = lat_lon_to_mercator(lat, lon);
        let style = self.stylesheet.node_style(&node.tags, NODE_STYLE_ZOOM);
        if let Some(&idx) = self.node_cache.index_by_id.get(&id) {
            let (_, old_mx, old_my) = self.node_cache.flat[idx];
            self.node_index
                .remove(&GeomWithData::new([old_mx, old_my], id));
            self.node_cache.flat[idx] = (id, mx, my);
            self.node_cache.styles[idx] = style;
        } else {
            let idx = self.node_cache.flat.len();
            self.node_cache.flat.push((id, mx, my));
            self.node_cache.styles.push(style);
            self.node_cache.index_by_id.insert(id, idx);
        }
        self.node_index.insert(GeomWithData::new([mx, my], id));
        self.grow_layer_bbox(WayBbox {
            min_x: mx,
            max_x: mx,
            min_y: my,
            max_y: my,
        });
    }

    /// Drop a node from the position caches and indices, including its
    /// `node_to_ways` entry.
    pub(super) fn index_node_removed(&mut self, id: i64) {
        if let Some(idx) = self.node_cache.index_by_id.remove(&id) {
            let (_, mx, my) = self.node_cache.flat[idx];
            self.node_index.remove(&GeomWithData::new([mx, my], id));
            self.node_cache.flat.remove(idx);
            self.node_cache.styles.remove(idx);
            // Removing from the middle shifts every later index.
            for v in self.node_cache.index_by_id.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
        }
        self.node_to_ways.remove(&id);
    }

    pub(super) fn way_style_of(&self, way: &OsmWay) -> WayStyle {
        self.stylesheet.way_style_cached(
            &way.tags,
            way.is_closed(),
            self.member_ways.contains(&way.id),
        )
    }

    /// Recompute the vertices, bbox, style and fill of the way at `way_idx`
    /// from `way` (a snapshot, so no borrow of `osm_data` is held) and the
    /// current node cache, replacing its R-tree entry. Callers refresh the
    /// multipolygons afterwards.
    pub(super) fn reproject_way(&mut self, way_idx: usize, way: &OsmWay) {
        if let Some(old) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&way_rtree_entry(old, way.id));
        }
        let (verts, bbox) = project_way_vertices(way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(way_rtree_entry(b, way.id));
            self.grow_layer_bbox(b);
        }
        let style = self.way_style_of(way);
        self.way_fill_tris[way_idx] = compute_fill_tris(&verts, &style);
        self.way_vertices[way_idx] = verts;
        self.way_bboxes[way_idx] = bbox;
        self.way_styles[way_idx] = style;
    }

    /// Append a way to every index-aligned cache and the indices. Callers
    /// refresh the multipolygons afterwards.
    pub(super) fn index_way_added(&mut self, way: &OsmWay) {
        let (verts, bbox) = project_way_vertices(way, &self.node_cache);
        if let Some(b) = bbox {
            self.way_index.insert(way_rtree_entry(b, way.id));
            self.grow_layer_bbox(b);
        }
        let way_idx = self.way_vertices.len();
        self.way_id_to_index.insert(way.id, way_idx);
        for nid in &way.nodes {
            self.node_to_ways.entry(*nid).or_default().insert(way_idx);
        }
        let style = self.way_style_of(way);
        self.way_ids.push(way.id);
        self.way_fill_tris.push(compute_fill_tris(&verts, &style));
        self.way_vertices.push(verts);
        self.way_bboxes.push(bbox);
        self.way_styles.push(style);
    }

    /// Remove the way at `way_idx` from every index-aligned cache and the
    /// indices, shifting the indices of the ways after it.
    pub(super) fn index_way_removed(&mut self, way_idx: usize, way_id: i64) {
        if let Some(old) = self.way_bboxes.get(way_idx).copied().flatten() {
            self.way_index.remove(&way_rtree_entry(old, way_id));
        }
        self.way_ids.remove(way_idx);
        self.way_vertices.remove(way_idx);
        self.way_bboxes.remove(way_idx);
        self.way_styles.remove(way_idx);
        self.way_fill_tris.remove(way_idx);
        self.way_id_to_index.remove(&way_id);
        for v in self.way_id_to_index.values_mut() {
            if *v > way_idx {
                *v -= 1;
            }
        }
        for ways in self.node_to_ways.values_mut() {
            *ways = ways
                .iter()
                .filter(|&&w| w != way_idx)
                .map(|&w| if w > way_idx { w - 1 } else { w })
                .collect();
        }
        self.node_to_ways.retain(|_, ways| !ways.is_empty());
    }

    /// Update `node_to_ways` for the way at `way_idx` after its node list
    /// went from `old` to `new`.
    pub(super) fn index_way_nodes_changed(&mut self, way_idx: usize, old: &[i64], new: &[i64]) {
        let kept: HashSet<i64> = new.iter().copied().collect();
        for dropped in old.iter().filter(|n| !kept.contains(n)) {
            if let Some(ways) = self.node_to_ways.get_mut(dropped) {
                ways.remove(&way_idx);
                if ways.is_empty() {
                    self.node_to_ways.remove(dropped);
                }
            }
        }
        for &added in new {
            self.node_to_ways.entry(added).or_default().insert(way_idx);
        }
    }

    /// Replace the stylesheet used for per-feature styling. Re-resolves
    /// every cached per-feature style against the new stylesheet (a full
    /// pass over the data, but this is a rare/explicit action, not a
    /// per-frame or per-drag hot path).
    pub fn set_stylesheet(&mut self, stylesheet: Arc<Stylesheet>) {
        self.stylesheet = stylesheet;
        self.revision += 1;
        if let Some(data) = self.osm_data.clone() {
            self.node_cache = compute_node_cache(&data, &self.stylesheet);
            self.member_ways = relation_member_ways(&data, &self.stylesheet);
            let (ids, bboxes, verts, styles, fill_tris) =
                compute_way_tables(&data, &self.node_cache, &self.stylesheet, &self.member_ways);
            self.way_ids = ids;
            self.way_bboxes = bboxes;
            self.way_vertices = verts;
            self.way_styles = styles;
            self.way_fill_tris = fill_tris;
            self.mp
                .rebuild_all(&data, &self.node_cache, &self.stylesheet);
            self.layer_bbox = compute_layer_bbox(&self.node_cache);
            self.node_index = build_node_index(&self.node_cache);
            self.way_index = build_way_index(&self.way_bboxes, &sorted_ways(&data));
        }
    }
}
