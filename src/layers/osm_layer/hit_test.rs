use super::*;

/// How close, in pixels, the pointer must be to a way's line to hover it.
pub(super) const WAY_HIT_TOL: f32 = 6.0;

impl OsmLayer {
    /// The way whose vertices are currently previewed, if any.
    pub fn vertex_preview_way(&self) -> Option<i64> {
        self.view.preview_way
    }

    /// Whether node `node_id` is a vertex of way `way_id`.
    pub(super) fn node_in_way(&self, node_id: i64, way_id: i64) -> bool {
        self.derived
            .way_id_to_index
            .get(&way_id)
            .is_some_and(|idx| {
                self.derived
                    .node_to_ways
                    .get(&node_id)
                    .is_some_and(|ws| ws.contains(idx))
            })
    }

    /// Distance in pixels from `pt` to the polyline of way `way_id`.
    pub(super) fn way_screen_distance(
        &self,
        viewport: &Viewport,
        pt: Point<Pixels>,
        way_id: i64,
    ) -> Option<f32> {
        let verts = &self.derived.way_vertices[*self.derived.way_id_to_index.get(&way_id)?];
        let mut best: Option<f32> = None;
        let mut prev: Option<Point<Pixels>> = None;
        for &(_, mx, my) in verts.iter() {
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            if let Some(p0) = prev {
                let d = point_to_segment_distance(pt, p0, sp);
                best = Some(best.map_or(d, |b| b.min(d)));
            }
            prev = Some(sp);
        }
        best
    }

    /// Whether node `id` is an untagged node that belongs to no way, so it
    /// is drawn like a tagged one. The way lookup only runs for untagged
    /// nodes at zooms where a standalone node would be drawn at all.
    pub(super) fn is_standalone(&self, tagged: bool, id: i64, zoom: f64) -> bool {
        !tagged
            && zoom >= node_markers::TAGGED_MIN_ZOOM
            && self
                .derived
                .node_to_ways
                .get(&id)
                .is_none_or(|ways| ways.is_empty())
    }

    /// The node nearest `screen_pt` within `tol_px`, whether or not its
    /// marker is drawn. Add mode snaps a new point onto it, so a vertex
    /// that is not drawn can still be connected to.
    pub fn snap_to_node(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<i64> {
        self.snap_to_node_where(viewport, screen_pt, tol_px, |_| true)
    }

    /// Like `snap_to_node`, considering only nodes `accept` allows (the
    /// dragged node itself, or a node of an incompatible way, are left out).
    pub fn snap_to_node_where(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        accept: impl Fn(i64) -> bool,
    ) -> Option<i64> {
        self.store.osm_data.as_ref()?;
        let pad = px(tol_px * 2.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);
        let mut best: Option<(f32, i64)> = None;
        for item in self.derived.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            if !accept(id) {
                continue;
            }
            let Some(&idx) = self.derived.node_cache.index_by_id.get(&id) else {
                continue;
            };
            let (_, mx, my) = self.derived.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= tol_px && best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Find the way segment nearest `screen_pt`, within `tol_px`, returning
    /// `(way_id, node_id_a, node_id_b, segment_index)` for its two endpoints
    /// (in the way's node-list order) if within tolerance. Used by Extrude
    /// mode, which needs the segment's endpoints rather than just a
    /// `FeatureRef` to the whole way (unlike `hit_test`).
    pub fn hit_test_segment(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<(i64, i64, i64, usize)> {
        self.hit_test_segment_preferring(viewport, screen_pt, tol_px, None)
    }

    /// Like `hit_test_segment`, but a segment of `prefer_way` within
    /// tolerance wins over a nearer segment of any other way (so overlapping
    /// ways resolve to the selected one).
    pub fn hit_test_segment_preferring(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        prefer_way: Option<i64>,
    ) -> Option<(i64, i64, i64, usize)> {
        self.store.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        // (preferred, distance, way, a, b, index): preferred ways sort first.
        let mut best: Option<(bool, f32, i64, i64, i64, usize)> = None;
        for item in self
            .derived
            .way_index
            .locate_in_envelope_intersecting(envelope)
        {
            let way_id = item.data;
            let Some(&way_idx) = self.derived.way_id_to_index.get(&way_id) else {
                continue;
            };
            let preferred = prefer_way == Some(way_id);
            let verts = &self.derived.way_vertices[way_idx];
            for i in 0..verts.len().saturating_sub(1) {
                let (id_a, ax, ay) = verts[i];
                let (id_b, bx, by) = verts[i + 1];
                let sp_a = viewport.mercator_to_screen(ax, ay);
                let sp_b = viewport.mercator_to_screen(bx, by);
                if !is_point_valid(sp_a) || !is_point_valid(sp_b) {
                    continue;
                }
                let d = point_to_segment_distance(screen_pt, sp_a, sp_b);
                if d <= tol_px
                    && best.as_ref().is_none_or(|&(bp, bd, ..)| {
                        (preferred && !bp) || (preferred == bp && d < bd)
                    })
                {
                    best = Some((preferred, d, way_id, id_a, id_b, i));
                }
            }
        }
        best.map(|(_, _, way_id, a, b, idx)| (way_id, a, b, idx))
    }

    /// Like `hit_test_segment`, but also returns the nearest point on the
    /// winning segment as `(lat, lon)` — the coordinates a snapped node
    /// should adopt. Shares the same rtree query and per-segment loop; see
    /// `hit_test_segment`'s doc comment for the shared parameters.
    pub fn snap_to_way(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<(i64, i64, i64, usize, f64, f64)> {
        self.snap_to_way_where(viewport, screen_pt, tol_px, |_| true)
    }

    /// Like `snap_to_way`, considering only ways `accept` allows (ways of an
    /// incompatible kind, or one that already holds the node, are left out).
    pub fn snap_to_way_where(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
        accept: impl Fn(i64) -> bool,
    ) -> Option<(i64, i64, i64, usize, f64, f64)> {
        self.store.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize, Point<Pixels>)> = None;
        for item in self
            .derived
            .way_index
            .locate_in_envelope_intersecting(envelope)
        {
            let way_id = item.data;
            if !accept(way_id) {
                continue;
            }
            let Some(&way_idx) = self.derived.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.derived.way_vertices[way_idx];
            for i in 0..verts.len().saturating_sub(1) {
                let (id_a, ax, ay) = verts[i];
                let (id_b, bx, by) = verts[i + 1];
                let sp_a = viewport.mercator_to_screen(ax, ay);
                let sp_b = viewport.mercator_to_screen(bx, by);
                if !is_point_valid(sp_a) || !is_point_valid(sp_b) {
                    continue;
                }
                let d = point_to_segment_distance(screen_pt, sp_a, sp_b);
                if d <= tol_px && best.as_ref().is_none_or(|&(bd, ..)| d < bd) {
                    let nearest = nearest_point_on_segment(screen_pt, sp_a, sp_b);
                    best = Some((d, way_id, id_a, id_b, i, nearest));
                }
            }
        }
        best.map(|(_, way_id, a, b, idx, nearest)| {
            let (lat, lon) = viewport.screen_to_geo(nearest);
            (way_id, a, b, idx, lat, lon)
        })
    }
}

impl OsmLayer {
    pub(super) fn hit_test_point(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = node_markers::FULL_HIT_RADIUS;
        const WAY_TOL: f32 = WAY_HIT_TOL;
        // Generous multiplier on the query envelope vs. the exact pixel
        // tolerance above: the R-tree query is only a coarse candidate
        // filter, the refinement loops below do the exact distance check,
        // so over-including candidates costs a little extra refinement work
        // but never causes a missed hit. Mirrors the box-select envelope
        // approach in `hit_test_rect`.
        const ENVELOPE_PAD_FACTOR: f32 = 4.0;

        if self.store.osm_data.is_none() {
            return Vec::new();
        }

        // Phase 0: a turn-restriction marker (drawn only where vertices are).
        // Checked first because the marker sits beside its via node, never
        // on it, so it can't steal a click aimed at the node.
        if viewport.zoom_level() >= VERTEX_MIN_ZOOM {
            let mut marker_hits: Vec<HitCandidate> = Vec::new();
            for (&relation_id, r) in &self.derived.mp.restrictions {
                let sp = viewport.mercator_to_screen(r.x, r.y);
                if !is_point_valid(sp) {
                    continue;
                }
                let c = point(
                    sp.x + px(RESTRICTION_OFFSET_PX.0),
                    sp.y + px(RESTRICTION_OFFSET_PX.1),
                );
                let dist = (c - screen_pt).magnitude() as f32;
                if dist <= RESTRICTION_MARKER_DIAMETER / 2.0 + 1.0 {
                    marker_hits.push(HitCandidate {
                        feature: FeatureRef {
                            layer_id: self.id,
                            kind: FeatureKind::Relation,
                            id: relation_id,
                        },
                        kind: FeatureKind::Relation,
                        dist_px: dist,
                    });
                }
            }
            if !marker_hits.is_empty() {
                return marker_hits;
            }
        }

        let max_tol = NODE_TOL.max(WAY_TOL);
        let pad = px(max_tol * ENVELOPE_PAD_FACTOR);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        // Phase 1: nodes within NODE_TOL. Candidates come from the point
        // R-tree (already built for box-select); refinement reads the
        // cached mercator position and projects with `mercator_to_screen`
        // (no trig) instead of `geo_to_screen`.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        let zoom = viewport.zoom_level();
        let pins = self.pins();
        for item in self.derived.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let Some(&idx) = self.derived.node_cache.index_by_id.get(&id) else {
                continue;
            };
            // A node whose marker is not shown is not a target: the click
            // goes to the way underneath. A simple marker has a smaller
            // radius than a full one.
            let form = self.node_form_at(&pins, idx, id, zoom);
            if !form.is_shown() {
                continue;
            }
            let (_, mx, my) = self.derived.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let dist = (sp - screen_pt).magnitude() as f32;
            if dist <= form.hit_radius() {
                node_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Node,
                        id,
                    },
                    kind: FeatureKind::Node,
                    dist_px: dist,
                });
            }
        }
        if !node_hits.is_empty() {
            return node_hits;
        }

        // Phase 2: ways within WAY_TOL. Candidates come from the way-bbox
        // R-tree. Note this uses `locate_in_envelope_intersecting` (not
        // `locate_in_envelope`, which requires the candidate's bbox to be
        // fully CONTAINED in the query envelope — the right semantics for
        // box-select in `hit_test_rect`, but wrong here: a long way's bbox
        // can be much bigger than our small click-envelope even when the
        // click lands right on the way). An intersection is guaranteed
        // whenever the closest point on the way is within `pad` of the
        // click, since pad >= WAY_TOL. Refinement walks the cached
        // (already-projected-to-mercator) vertex list for exactly that way.
        let mut way_hits: Vec<HitCandidate> = Vec::new();
        for item in self
            .derived
            .way_index
            .locate_in_envelope_intersecting(envelope)
        {
            let way_id = item.data;
            let Some(&way_idx) = self.derived.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.derived.way_vertices[way_idx];
            if verts.len() < 2 {
                continue;
            }
            let mut best = f32::INFINITY;
            let mut prev: Option<Point<Pixels>> = None;
            for &(_, mx, my) in verts.iter() {
                let sp = viewport.mercator_to_screen(mx, my);
                if !is_point_valid(sp) {
                    continue;
                }
                if let Some(p0) = prev {
                    let d = point_to_segment_distance(screen_pt, p0, sp);
                    if d < best {
                        best = d;
                    }
                }
                prev = Some(sp);
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Way,
                        id: way_id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: best,
                });
            }
        }
        way_hits
    }

    pub(super) fn hit_test_interior_at(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        if self.store.osm_data.is_none() {
            return Vec::new();
        }

        let (mx, my) = viewport.screen_to_mercator(screen_pt);
        let envelope = AABB::from_corners([mx, my], [mx, my]);

        let mut hits: Vec<HitCandidate> = Vec::new();
        for item in self
            .derived
            .way_index
            .locate_in_envelope_intersecting(envelope)
        {
            let way_id = item.data;
            let Some(&way_idx) = self.derived.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.derived.way_vertices[way_idx];
            // Closed-ring convention: first and last node ref are the same
            // (mirrors `OsmWay::is_closed`).
            if verts.len() < 4 || verts.first().map(|v| v.0) != verts.last().map(|v| v.0) {
                continue;
            }
            let screen_verts: Vec<Point<Pixels>> = verts
                .iter()
                .map(|&(_, vx, vy)| viewport.mercator_to_screen(vx, vy))
                .collect();
            if !screen_verts.iter().all(|p| is_point_valid(*p)) {
                continue;
            }
            if crate::selection::point_in_polygon(screen_pt, &screen_verts) {
                hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Way,
                        id: way_id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: crate::selection::polygon_area(&screen_verts),
                });
            }
        }

        // Multipolygon relations compete with the closed ways on area, holes
        // excluded: the point must be inside an outer ring and in none of its
        // holes. Only tagged relations take part (an untagged, old-style
        // multipolygon leaves the click to its tagged outer way), and the
        // area is scaled by a hair so a relation beats a closed way of
        // exactly the same area, e.g. its own single outer way.
        let (x0, _) = viewport.screen_to_mercator(point(px(0.0), px(0.0)));
        let (x1, _) = viewport.screen_to_mercator(point(px(1.0), px(0.0)));
        let m_per_px = (x1 - x0).abs();
        for (&relation_id, entry) in &self.derived.mp.entries {
            let b = &entry.bbox;
            if !entry.tagged || mx < b.min_x || mx > b.max_x || my < b.min_y || my > b.max_y {
                continue;
            }
            let xy = |v: &(i64, f64, f64)| (v.1, v.2);
            let inside = entry.polygons.iter().any(|p| {
                let pb = &p.bbox;
                mx >= pb.min_x
                    && mx <= pb.max_x
                    && my >= pb.min_y
                    && my <= pb.max_y
                    && crate::multipolygon::point_in_ring(mx, my, p.ring(0), xy)
                    && !(1..p.ring_count())
                        .any(|k| crate::multipolygon::point_in_ring(mx, my, p.ring(k), xy))
            });
            if inside && m_per_px > 0.0 {
                hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_id: self.id,
                        kind: FeatureKind::Relation,
                        id: relation_id,
                    },
                    kind: FeatureKind::Relation,
                    dist_px: (entry.area / (m_per_px * m_per_px)) as f32 * 0.999,
                });
            }
        }
        hits
    }

    pub(super) fn hit_test_rect_in(
        &self,
        viewport: &Viewport,
        rect: Bounds<Pixels>,
    ) -> Vec<FeatureRef> {
        let (x1, y1) = viewport.screen_to_mercator(rect.origin);
        let (x2, y2) = viewport.screen_to_mercator(point(
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ));
        let min_x = x1.min(x2);
        let max_x = x1.max(x2);
        let min_y = y1.min(y2);
        let max_y = y1.max(y2);
        let envelope = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        let mut out = Vec::new();
        // Only nodes whose marker is shown: a box at low zoom selects the
        // ways it encloses and the tagged nodes, not the hidden vertices.
        let zoom = viewport.zoom_level();
        let pins = self.pins();
        for item in self.derived.node_index.locate_in_envelope(envelope) {
            let id = item.data;
            let shown = self
                .derived
                .node_cache
                .index_by_id
                .get(&id)
                .is_some_and(|&idx| self.node_form_at(&pins, idx, id, zoom).is_shown());
            if shown {
                out.push(FeatureRef {
                    layer_id: self.id,
                    kind: FeatureKind::Node,
                    id,
                });
            }
        }
        for item in self.derived.way_index.locate_in_envelope(envelope) {
            out.push(FeatureRef {
                layer_id: self.id,
                kind: FeatureKind::Way,
                id: item.data,
            });
        }
        out
    }
}
