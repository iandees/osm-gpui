use super::*;

/// Areas smaller than this on screen (width, height in px) cannot hold any
/// label text and are not considered.
pub(super) const MIN_AREA_PX: (f32, f32) = (16.0, 11.0);

/// A named area ready for placement: its item plus what painting needs.
pub(super) struct AreaItem {
    pub(super) item: labels::Item,
    pub(super) label: Arc<area_labels::AreaLabel>,
    pub(super) line: Arc<ShapedLine>,
    /// Where the text is centred, Mercator metres.
    pub(super) mx: f64,
    pub(super) my: f64,
}

impl OsmLayer {
    /// Marker radius a tagged node is drawn with at `zoom` (0 when hidden).
    pub(super) fn label_marker_radius(style: NodeStyle, zoom: f64) -> f32 {
        match node_markers::node_form(style.tagged, false, false, zoom) {
            NodeForm::Hidden => 0.0,
            NodeForm::Simple(edge) => edge / 2.0,
            _ => (style.size + 2.0) / 2.0,
        }
    }

    /// The (cached) label of node `id`, resolved from the stylesheet at
    /// `zoom` on first use.
    pub(super) fn node_label_for(
        &self,
        cache: &mut labels::LabelCache,
        id: i64,
        zoom: f64,
    ) -> Option<Arc<labels::NodeLabel>> {
        let labels::LabelCache { nodes, icons, .. } = cache;
        nodes
            .entry(id)
            .or_insert_with(|| {
                let tags = &self.store.osm_data.as_ref()?.nodes.get(&id)?.tags;
                let st = self.store.stylesheet.node_text_style(tags, zoom);
                labels::resolve_node_label(&st, tags, icons, zoom).map(Arc::new)
            })
            .clone()
    }

    /// Identifies what label placement depends on besides the data: the
    /// stylesheet, and the tuning (so changing a knob forces a new
    /// placement).
    pub(super) fn label_sheet_id(&self, tuning: &labels::LabelTuning) -> usize {
        Arc::as_ptr(&self.store.stylesheet) as usize ^ tuning.stamp()
    }

    /// Apply a `LabelBench` script spec to this layer's labels (see
    /// `line_labels::parse_bench`); the next frame places again.
    pub fn set_label_bench(&self, spec: &str) -> Result<(), String> {
        self.render
            .label_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tuning
            .set_bench(spec)
    }

    /// The (cached) label text and style of the area `key` (a closed way or
    /// a tagged multipolygon relation) at `zoom`.
    pub(super) fn area_label_for(
        &self,
        cache: &mut labels::LabelCache,
        key: labels::Key,
        zoom: f64,
    ) -> Option<Arc<area_labels::AreaLabel>> {
        cache.areas.label(key, || {
            let data = self.store.osm_data.as_ref()?;
            if key.0 == labels::KIND_REL {
                let e = self.derived.mp.entries.get(&key.1)?;
                if !e.tagged {
                    // Old-style: the outer way carries the tags and its own
                    // label.
                    return None;
                }
                let st = self.store.stylesheet.way_text_style(&e.tags, true, zoom);
                area_labels::resolve_area_label(&st, &e.tags, zoom)
            } else {
                let way = data.ways.get(&key.1)?;
                if !way.is_closed() {
                    return None;
                }
                let st = self.store.stylesheet.way_text_style(&way.tags, true, zoom);
                area_labels::resolve_area_label(&st, &way.tags, zoom)
            }
        })
    }

    /// The (cached) label geometry of the area `key`: its polygons in
    /// Mercator metres with their poles of inaccessibility. Keyed by a hash
    /// of the vertices, so it is rebuilt exactly when an edit changed them.
    pub(super) fn area_geom_for(
        &self,
        cache: &mut labels::LabelCache,
        key: labels::Key,
    ) -> Option<Arc<area_labels::AreaGeom>> {
        use area_labels::{AreaGeom, Fingerprint};
        let xy = |v: &[(i64, f64, f64)]| -> Vec<(f64, f64)> {
            v.iter().map(|&(_, x, y)| (x, y)).collect()
        };
        if key.0 == labels::KIND_REL {
            let e = self.derived.mp.entries.get(&key.1)?;
            let mut fp = Fingerprint::default();
            for p in &e.polygons {
                for k in 0..p.ring_count() {
                    fp.ring(p.ring(k).iter().map(|&(_, x, y)| (x, y)));
                }
            }
            cache.areas.geom(key, fp, || {
                AreaGeom::build(
                    e.polygons
                        .iter()
                        .map(|p| (0..p.ring_count()).map(|k| xy(p.ring(k))).collect())
                        .collect(),
                )
            })
        } else {
            let idx = *self.derived.way_id_to_index.get(&key.1)?;
            let verts = &self.derived.way_vertices[idx];
            if verts.len() < 4 || verts.first()?.0 != verts.last()?.0 {
                return None;
            }
            let mut fp = Fingerprint::default();
            fp.ring(verts.iter().map(|&(_, x, y)| (x, y)));
            cache
                .areas
                .geom(key, fp, || AreaGeom::build(vec![vec![xy(verts)]]))
        }
    }

    /// Locate and shape one area's label for the current view. `None` when
    /// it has no geometry, does not fit, or (unless `forced`) fits nowhere.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn locate_area_label(
        &self,
        cache: &mut labels::LabelCache,
        view: &area_labels::AreaView,
        key: labels::Key,
        label: &Arc<area_labels::AreaLabel>,
        forced: bool,
        budget: &mut usize,
        window: &mut Window,
    ) -> Option<(area_labels::Located, Arc<ShapedLine>)> {
        let geom = self.area_geom_for(cache, key)?;
        let line = cache.shapes.get(window, &label.text, label.look.font_px);
        let size = (f32::from(line.width), f32::from(line.ascent + line.descent));
        let scales = area_labels::font_scales(label.look.font_px);
        let at = area_labels::locate(&geom, view, size, &scales, forced, budget)?;
        let line = if at.scale == 1.0 {
            line
        } else {
            cache
                .shapes
                .get(window, &label.text, label.look.font_px * at.scale)
        };
        Some((at, line))
    }

    /// The named areas in view as placement items (`Anchor::Center`, no
    /// marker). Cheap for what is skipped: tiny areas are dropped on their
    /// cached bounding box before any lookup, labels and geometry are cached
    /// across placements, and only the top `MAX_AREA_CANDIDATES` by priority
    /// are located and shaped. An area whose text equals that of a node label
    /// inside it (a building and its own POI node) is dropped: the point
    /// label is the better one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn area_items(
        &self,
        cache: &mut labels::LabelCache,
        viewport: &Viewport,
        cam: labels::CameraSig,
        forced: &HashSet<labels::Key>,
        node_texts: &HashMap<SharedString, Vec<(f64, f64)>>,
        window: &mut Window,
    ) -> Vec<AreaItem> {
        let zoom = cam.zoom;
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let view = area_labels::AreaView::new(vmin_x, vmax_x, vmax_y, cam.w, cam.h);
        let ppm = view.ppm;
        // (key, label, longer side in px)
        let mut found: Vec<(labels::Key, Arc<area_labels::AreaLabel>, f32)> = Vec::new();
        let tiny =
            |w: f64, h: f64| (w * ppm) < MIN_AREA_PX.0 as f64 || (h * ppm) < MIN_AREA_PX.1 as f64;
        let env = AABB::from_corners([vmin_x, vmin_y], [vmax_x, vmax_y]);
        for g in self.derived.way_index.locate_in_envelope_intersecting(env) {
            let id = g.data;
            let key = (labels::KIND_WAY, id);
            let Some(&idx) = self.derived.way_id_to_index.get(&id) else {
                continue;
            };
            let Some(b) = self.derived.way_bboxes[idx] else {
                continue;
            };
            let (bw, bh) = (b.max_x - b.min_x, b.max_y - b.min_y);
            if tiny(bw, bh) && !forced.contains(&key) {
                continue;
            }
            if let Some(l) = self.area_label_for(cache, key, zoom) {
                found.push((key, l, ((bw.max(bh)) * ppm) as f32));
            }
        }
        for (&id, e) in &self.derived.mp.entries {
            let key = (labels::KIND_REL, id);
            let b = &e.bbox;
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let (bw, bh) = (b.max_x - b.min_x, b.max_y - b.min_y);
            if tiny(bw, bh) && !forced.contains(&key) {
                continue;
            }
            if let Some(l) = self.area_label_for(cache, key, zoom) {
                found.push((key, l, ((bw.max(bh)) * ppm) as f32));
            }
        }
        // A member way repeating its relation's name is the same label.
        let rel_text: HashMap<i64, &SharedString> = found
            .iter()
            .filter(|f| f.0 .0 == labels::KIND_REL)
            .map(|f| (f.0 .1, &f.1.text))
            .collect();
        if !rel_text.is_empty() {
            let dup: HashSet<labels::Key> = found
                .iter()
                .filter(|f| f.0 .0 == labels::KIND_WAY)
                .filter(|f| {
                    self.derived
                        .mp
                        .way_to_rels
                        .get(&f.0 .1)
                        .is_some_and(|rels| {
                            rels.iter().any(|r| rel_text.get(r) == Some(&&f.1.text))
                        })
                })
                .map(|f| f.0)
                .collect();
            found.retain(|f| !dup.contains(&f.0));
        }
        let rank = |f: &(labels::Key, Arc<area_labels::AreaLabel>, f32)| {
            (
                forced.contains(&f.0),
                f.1.priority + area_labels::size_bonus(f.2),
            )
        };
        found.sort_by(|a, b| rank(b).cmp(&rank(a)).then(a.0.cmp(&b.0)));
        found.truncate(area_labels::MAX_AREA_CANDIDATES);

        let mut budget = area_labels::CLIP_BUDGET;
        let mut out = Vec::with_capacity(found.len());
        for (key, label, bpx) in found {
            let is_forced = forced.contains(&key);
            let Some((at, line)) =
                self.locate_area_label(cache, &view, key, &label, is_forced, &mut budget, window)
            else {
                continue;
            };
            if !is_forced && Self::names_a_node_inside(node_texts, &label.text, &at, &view) {
                continue;
            }
            let sp = viewport.mercator_to_screen(at.mx, at.my);
            let (w, h) = (f32::from(line.width), f32::from(line.ascent + line.descent));
            out.push(AreaItem {
                item: labels::Item {
                    key,
                    center: (f32::from(sp.x), f32::from(sp.y)),
                    marker_r: 0.0,
                    icon_r: None,
                    text: Some((w, h, labels::Anchor::Center)),
                    priority: label.priority + area_labels::size_bonus(bpx),
                    forced: is_forced,
                },
                label,
                line,
                mx: at.mx,
                my: at.my,
            });
        }
        out
    }

    /// Whether a node label with the same `text` sits near the located
    /// area label (within a text's width of it): then the area label would
    /// only repeat it.
    pub(super) fn names_a_node_inside(
        node_texts: &HashMap<SharedString, Vec<(f64, f64)>>,
        text: &SharedString,
        at: &area_labels::Located,
        view: &area_labels::AreaView,
    ) -> bool {
        let Some(nodes) = node_texts.get(text) else {
            return false;
        };
        let reach = 60.0 / view.ppm;
        nodes
            .iter()
            .any(|&(x, y)| (x - at.mx).abs() < reach && (y - at.my).abs() < reach)
    }

    /// Place the labels and icons of the nodes in view (the expensive half;
    /// run when the camera settles, not every frame).
    pub(super) fn place_labels(
        &self,
        cache: &mut labels::LabelCache,
        viewport: &Viewport,
        cam: labels::CameraSig,
        selected: &HashSet<i64>,
        forced_areas: &HashSet<labels::Key>,
        window: &mut Window,
    ) -> labels::Placement {
        let fs = crate::frame_stats::start();
        let zoom = cam.zoom;
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        // Nodes a little outside the view can still have a label that
        // reaches in.
        let margin = 150.0 / (cam.w as f64 / cam.span_x.max(1e-9));
        let mut cands: Vec<(usize, labels::Item, Arc<labels::NodeLabel>)> = Vec::new();
        let mut fixed: Vec<labels::Rect> = Vec::new();
        for (idx, &(id, mx, my)) in self.derived.node_cache.flat.iter().enumerate() {
            if mx < vmin_x - margin
                || mx > vmax_x + margin
                || my < vmin_y - margin
                || my > vmax_y + margin
            {
                continue;
            }
            let style = self.derived.node_cache.styles[idx];
            if !style.tagged {
                continue;
            }
            let sp = viewport.mercator_to_screen(mx, my);
            if !is_point_valid(sp) {
                continue;
            }
            let c = (f32::from(sp.x), f32::from(sp.y));
            let r = Self::label_marker_radius(style, zoom);
            match self.node_label_for(cache, id, zoom) {
                Some(l) => {
                    let item = labels::Item {
                        key: (labels::KIND_NODE, id),
                        center: c,
                        marker_r: r,
                        icon_r: l.icon.as_ref().map(|i| i.backing() / 2.0),
                        text: None,
                        priority: l.priority,
                        forced: selected.contains(&id),
                    };
                    cands.push((idx, item, l));
                }
                None if r > 0.0 => fixed.push(labels::Rect::around(c.0, c.1, r, r)),
                None => {}
            }
        }
        // Keep the most important candidates; the rest are plain obstacles.
        if cands.len() > labels::MAX_CANDIDATES {
            cands.sort_by(|a, b| {
                b.1.forced
                    .cmp(&a.1.forced)
                    .then(b.1.priority.cmp(&a.1.priority))
                    .then(a.1.key.cmp(&b.1.key))
            });
            for (_, it, _) in cands.drain(labels::MAX_CANDIDATES..) {
                if it.marker_r > 0.0 {
                    fixed.push(labels::Rect::around(
                        it.center.0,
                        it.center.1,
                        it.marker_r,
                        it.marker_r,
                    ));
                }
            }
        }
        // Shape (cached) to learn each text's size.
        let mut lines: Vec<Option<Arc<ShapedLine>>> = Vec::with_capacity(cands.len());
        for (_, it, l) in cands.iter_mut() {
            match &l.text {
                Some(t) => {
                    let line = cache.shapes.get(window, t, l.look.font_px);
                    let w = f32::from(line.width);
                    let h = f32::from(line.ascent + line.descent);
                    it.text = Some((w, h, l.pref));
                    lines.push(Some(line));
                }
                None => lines.push(None),
            }
        }
        let mut items: Vec<labels::Item> = cands.iter().map(|c| c.1.clone()).collect();
        // Named areas (after the nodes, so their item indices follow).
        let n_nodes = items.len();
        let mut node_texts: HashMap<SharedString, Vec<(f64, f64)>> = HashMap::new();
        for (idx, _, l) in &cands {
            if let Some(t) = &l.text {
                let (_, mx, my) = self.derived.node_cache.flat[*idx];
                node_texts.entry(t.clone()).or_default().push((mx, my));
            }
        }
        let areas = self.area_items(cache, viewport, cam, forced_areas, &node_texts, window);
        items.extend(areas.iter().map(|a| a.item.clone()));
        let prev = cache.prev_state();
        let out = labels::place(
            &items,
            &fixed,
            &prev,
            &labels::Config::for_view(cam.w, cam.h),
        );
        let mut placed = Vec::with_capacity(out.len());
        let mut by_id = HashMap::with_capacity(out.len());
        for p in out {
            if p.item >= n_nodes {
                let a = &areas[p.item - n_nodes];
                if let Some((_, r)) = p.text {
                    placed.push(labels::PlacedLabel::new_area(
                        a.item.key.0,
                        a.item.key.1,
                        a.mx,
                        a.my,
                        labels::PlacedText::in_rect(
                            a.line.clone(),
                            &r,
                            a.item.center,
                            a.label.look,
                        ),
                    ));
                }
                continue;
            }
            let (idx, item, l) = &cands[p.item];
            let (id, mx, my) = self.derived.node_cache.flat[*idx];
            let icon = if p.icon {
                l.icon.clone().map(|choice| labels::PlacedIcon {
                    choice,
                    fill: self.derived.node_cache.styles[*idx].color,
                })
            } else {
                None
            };
            let text = match (p.text, &lines[p.item]) {
                (Some((anchor, r)), Some(line)) => Some((
                    labels::PlacedText::in_rect(line.clone(), &r, item.center, l.look),
                    anchor,
                )),
                _ => None,
            };
            by_id.insert(id, placed.len());
            placed.push(labels::PlacedLabel::new(id, mx, my, icon, text));
        }
        let n_text = placed.iter().filter(|l| l.text.is_some()).count();
        crate::frame_stats::lap("labels.place", fs);
        crate::frame_stats::count("labels.candidates", items.len());
        crate::frame_stats::count("labels.placed", placed.len());
        labels::Placement {
            cam,
            revision: self.store.revision,
            sheet: self.label_sheet_id(&cache.tuning),
            labels: placed,
            by_id,
            fade_end: None,
            halo: cache.tuning.halo_for(n_text),
        }
    }

    /// Paint labels and POI icons above everything `render_canvas` and the
    /// highlights drew; `selected` and `hovered` keep their labels even when
    /// collision would drop them. Places when the camera has settled (or the
    /// data changed), otherwise just re-projects the last placement.
    #[allow(clippy::too_many_arguments)]
    pub fn render_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        selected: &[FeatureRef],
        hovered: Option<&FeatureRef>,
        window: &mut Window,
        cx: &App,
    ) {
        let zoom = viewport.zoom_level();
        if self.store.osm_data.is_none() || zoom < labels::LABEL_MIN_ZOOM {
            return;
        }
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        if let Some(lb) = &self.derived.layer_bbox {
            if lb.max_x < vmin_x || lb.min_x > vmax_x || lb.max_y < vmin_y || lb.min_y > vmax_y {
                return;
            }
        }
        let fs = crate::frame_stats::start();
        let cam = labels::CameraSig {
            zoom,
            min_x: vmin_x,
            max_y: vmax_y,
            span_x: vmax_x - vmin_x,
            w: f32::from(viewport.transform.screen_size.width),
            h: f32::from(viewport.transform.screen_size.height),
        };
        let mut cache = self
            .render
            .label_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sheet = self.label_sheet_id(&cache.tuning);
        cache.validate(self.store.revision, sheet, zoom.floor() as i32);
        let now = std::time::Instant::now();
        let step = cache.step(cam, self.store.revision, sheet, now);
        if step == labels::Step::Recompute {
            let ids: HashSet<i64> = selected
                .iter()
                .chain(hovered)
                .filter(|f| f.layer_id == self.id && f.kind == FeatureKind::Node)
                .map(|f| f.id)
                .collect();
            let forced_areas: HashSet<labels::Key> = selected
                .iter()
                .chain(hovered)
                .filter(|f| f.layer_id == self.id)
                .filter_map(|f| match f.kind {
                    FeatureKind::Way => Some((labels::KIND_WAY, f.id)),
                    FeatureKind::Relation => Some((labels::KIND_REL, f.id)),
                    FeatureKind::Node => None,
                })
                .collect();
            let p = self.place_labels(&mut cache, viewport, cam, &ids, &forced_areas, window);
            self.place_line_labels(viewport, cam, &p, &cache.tuning.line_knobs(), now);
            cache.store(p, now);
        }
        // Frames are requested only while something is in flight: waiting to
        // settle, or a fade-in running.
        let line_fade_end = self
            .render
            .line_placed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fade_end;
        let fade_end = cache
            .placement
            .as_ref()
            .and_then(|p| p.fade_end)
            .max(line_fade_end);
        if labels::wants_frame(step, fade_end, now) {
            window.request_animation_frame();
        }
        if step == labels::Step::HideAndWait {
            return;
        }
        // Street names under the point labels and icons they yielded to.
        self.paint_line_labels(
            viewport,
            bounds,
            now,
            cache.tuning.line_knobs().repeat,
            window,
        );
        let ox = bounds.origin.x;
        let oy = bounds.origin.y;
        let (w, h) = (cam.w, cam.h);
        let on_screen = |x: f32, y: f32| x > -300.0 && x < w + 300.0 && y > -100.0 && y < h + 100.0;
        // Taken out of the cache so painting can also borrow it for the
        // forced labels below.
        let Some(placement) = cache.placement.take() else {
            return;
        };
        let halo = placement.halo;
        for l in &placement.labels {
            let drag = if l.kind == labels::KIND_NODE {
                self.drag_preview_offset(l.id)
            } else {
                Point::default()
            };
            let sp = viewport.mercator_to_screen(l.mx, l.my) + drag;
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let at = point(sp.x + ox, sp.y + oy);
            let alpha = labels::fade_alpha(l.born, now);
            if let Some(icon) = &l.icon {
                labels::paint_icon(window, cx, icon, at, alpha);
            }
            if let Some(t) = &l.text {
                labels::paint_text(
                    window,
                    t,
                    point(at.x + px(t.dx), at.y + px(t.dy)),
                    alpha,
                    halo,
                );
            }
        }
        // Selected and hovered nodes keep their label even where the
        // placement dropped it: drawn beside the marker, collision ignored.
        for f in selected.iter().chain(hovered) {
            if f.layer_id != self.id
                || f.kind != FeatureKind::Node
                || placement
                    .by_id
                    .get(&f.id)
                    .is_some_and(|&i| placement.labels[i].text.is_some())
            {
                continue;
            }
            let Some(&idx) = self.derived.node_cache.index_by_id.get(&f.id) else {
                continue;
            };
            let Some(label) = self.node_label_for(&mut cache, f.id, zoom) else {
                continue;
            };
            let Some(text) = &label.text else { continue };
            let (_, mx, my) = self.derived.node_cache.flat[idx];
            let sp = viewport.mercator_to_screen(mx, my) + self.drag_preview_offset(f.id);
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let line = cache.shapes.get(window, text, label.look.font_px);
            let reach = Self::label_marker_radius(self.derived.node_cache.styles[idx], zoom)
                .max((self.derived.node_cache.styles[idx].size + 2.0) / 2.0);
            let r = labels::label_rect(
                label.pref,
                (0.0, 0.0),
                reach,
                3.0,
                f32::from(line.width),
                f32::from(line.ascent + line.descent),
            );
            let t = labels::PlacedText::new(line, r.x0, r.y0, label.look);
            let at = point(sp.x + ox, sp.y + oy);
            labels::paint_text(
                window,
                &t,
                point(at.x + px(t.dx), at.y + px(t.dy)),
                1.0,
                labels::HaloMode::Offsets,
            );
        }
        // Likewise a selected or hovered area whose label was not placed
        // (the selection changed since): shown at its label point, fitted or
        // not, collision ignored.
        let mut forced_area_budget = area_labels::CLIP_BUDGET;
        let view = area_labels::AreaView::new(vmin_x, vmax_x, vmax_y, w, h);
        for f in selected.iter().chain(hovered) {
            let kind = match f.kind {
                FeatureKind::Way => labels::KIND_WAY,
                FeatureKind::Relation => labels::KIND_REL,
                FeatureKind::Node => continue,
            };
            if f.layer_id != self.id
                || placement
                    .labels
                    .iter()
                    .any(|l| l.kind == kind && l.id == f.id && l.text.is_some())
            {
                continue;
            }
            let key = (kind, f.id);
            let Some(label) = self.area_label_for(&mut cache, key, zoom) else {
                continue;
            };
            let Some((at, line)) = self.locate_area_label(
                &mut cache,
                &view,
                key,
                &label,
                true,
                &mut forced_area_budget,
                window,
            ) else {
                continue;
            };
            let sp = viewport.mercator_to_screen(at.mx, at.my);
            if !on_screen(f32::from(sp.x), f32::from(sp.y)) {
                continue;
            }
            let dx = -f32::from(line.width) / 2.0;
            let dy = -f32::from(line.ascent + line.descent) / 2.0;
            let t = labels::PlacedText::new(line, dx, dy, label.look);
            let atp = point(sp.x + ox, sp.y + oy);
            labels::paint_text(
                window,
                &t,
                point(atp.x + px(t.dx), atp.y + px(t.dy)),
                1.0,
                labels::HaloMode::Offsets,
            );
        }
        cache.placement = Some(placement);
        crate::frame_stats::lap("labels.paint", fs);
    }

    /// Ways in view whose MapCSS text style asks for a label along the line
    /// (`text: name; text-position: line`), with the style resolved at
    /// `zoom`. Run when placement is recomputed, not every frame.
    pub(super) fn line_label_candidates(
        &self,
        viewport: &Viewport,
        zoom: f64,
    ) -> Vec<crate::layers::line_labels::Candidate<'_>> {
        use crate::layers::line_labels::{Candidate, LabelStyle};
        let mut cands = Vec::new();
        let Some(data) = self.store.osm_data.as_ref() else {
            return cands;
        };
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        for (i, bbox) in self.derived.way_bboxes.iter().enumerate() {
            let Some(b) = bbox else { continue };
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let way_id = self.derived.way_ids[i];
            let Some(way) = data.ways.get(&way_id) else {
                continue;
            };
            let st = self
                .store
                .stylesheet
                .way_text_style(&way.tags, way.is_closed(), zoom);
            if st.position != Some(TextPosition::Line) {
                continue;
            }
            let Some(text) = st
                .text
                .as_deref()
                .and_then(|k| labels::label_text(k, &way.tags))
            else {
                continue;
            };
            // The road's half-width on screen (stroke plus casing), so the
            // text clears it even when the stylesheet's offset is small.
            let ws = &self.render.way_styles[i];
            let casing = if zoom >= CASING_MIN_ZOOM {
                ws.casing_width.max(CASING_WIDTH)
            } else {
                ws.casing_width
            };
            let half_width = ws.width / 2.0 + casing;
            cands.push(Candidate {
                way_id,
                text,
                verts: &self.derived.way_vertices[i],
                style: LabelStyle::resolve(&st, half_width),
                priority: highway_label_rank(&way.tags),
            });
        }
        cands
    }

    /// Choose the street-name labels for the current camera, yielding to
    /// the point labels and icons in `points`. Stores the result for
    /// `paint_line_labels`.
    pub(super) fn place_line_labels(
        &self,
        viewport: &Viewport,
        cam: labels::CameraSig,
        points: &labels::Placement,
        knobs: &crate::layers::line_labels::BenchKnobs,
        now: std::time::Instant,
    ) {
        let fs = crate::frame_stats::start();
        let mut obstacles: Vec<labels::Rect> = Vec::new();
        for l in &points.labels {
            let sp = viewport.mercator_to_screen(l.mx, l.my);
            let (cx, cy) = (f32::from(sp.x), f32::from(sp.y));
            if let Some(icon) = &l.icon {
                let r = icon.choice.backing() / 2.0 + 1.0;
                obstacles.push(labels::Rect::around(cx, cy, r, r));
            }
            if let Some(t) = &l.text {
                let (w, h) = (
                    f32::from(t.line.width),
                    f32::from(t.line.ascent + t.line.descent),
                );
                obstacles.push(labels::Rect::new(
                    cx + t.dx,
                    cy + t.dy,
                    cx + t.dx + w,
                    cy + t.dy + h,
                ));
            }
        }
        let cands = self.line_label_candidates(viewport, cam.zoom);
        let mut placed_slot = self
            .render
            .line_placed
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev: HashSet<(i64, usize)> = placed_slot
            .placed
            .iter()
            .map(|p| (p.way_id, p.slot))
            .collect();
        let to_screen = |mx: f64, my: f64| {
            let sp = viewport.mercator_to_screen(mx, my);
            (f32::from(sp.x), f32::from(sp.y))
        };
        let (placed, _built) = match self.render.line_labels.lock() {
            Ok(mut cache) => cache.place(
                &cands,
                cam.zoom,
                knobs,
                to_screen,
                (cam.w, cam.h),
                &obstacles,
                &prev,
            ),
            Err(_) => (Vec::new(), 0),
        };
        crate::frame_stats::count("linelabels.candidates", cands.len());
        crate::frame_stats::count("linelabels.placed", placed.len());
        placed_slot.store(placed, cam.zoom, now, labels::ZOOM_KEEP, labels::FADE);
        crate::frame_stats::lap("linelabels.place", fs);
    }

    /// Paint the placed street-name labels: halos first, then glyph fills.
    /// Per frame this only re-projects each label's reference point and
    /// transforms its cached triangles.
    pub(super) fn paint_line_labels(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        now: std::time::Instant,
        repeat: u32,
        window: &mut Window,
    ) {
        use crate::layers::line_labels::{emit_frame, FrameOut};
        if self.view.drag_preview.is_some() {
            return;
        }
        let fs_phase = crate::frame_stats::start();
        let placed = self
            .render
            .line_placed
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if placed.placed.is_empty() {
            return;
        }
        let mut out = FrameOut::default();
        let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        emit_frame(
            &placed.placed,
            |mx, my| {
                let sp = viewport.mercator_to_screen(mx, my);
                (f32::from(sp.x), f32::from(sp.y))
            },
            origin,
            view,
            now,
            &mut out,
        );
        drop(placed);
        crate::frame_stats::lap("linelabels.build", fs_phase);
        let fs_phase = crate::frame_stats::start();
        let (nf, nh) = out.vertex_counts();
        crate::frame_stats::count("linelabels.drawn", out.labels);
        crate::frame_stats::count("linelabels.vertices", nf + nh);
        // `repeat` > 1 is a GPU-load multiplier for benchmarking.
        for (verts, color) in out.halo.iter().chain(out.fill.iter()).map(|(c, v)| (v, *c)) {
            if verts.is_empty() {
                continue;
            }
            let st = point(0., 1.);
            let mut path = Path::new(point(px(verts[0].0), px(verts[0].1)));
            let (mut min_x, mut max_x, mut min_y, mut max_y) = (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            );
            path.vertices.reserve(verts.len());
            for &(x, y) in verts.iter() {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                path.vertices.push(PathVertex {
                    xy_position: point(px(x), px(y)),
                    st_position: st,
                    content_mask: Default::default(),
                });
            }
            path.bounds = Bounds {
                origin: point(px(min_x), px(min_y)),
                size: size(px(max_x - min_x), px(max_y - min_y)),
            };
            for _ in 1..repeat {
                window.paint_path(path.clone(), rgba(color));
            }
            window.paint_path(path, rgba(color));
        }
        crate::frame_stats::lap("linelabels.paint", fs_phase);
    }

    /// Street-name label triangles for the current view with no point labels
    /// in the way: candidates, placement and emission in one call. For
    /// `examples/perf_bench.rs`, which has no window.
    pub fn line_label_frame(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
    ) -> crate::layers::line_labels::FrameOut {
        use crate::layers::line_labels::FrameOut;
        let mut out = FrameOut::default();
        let zoom = viewport.zoom_level();
        let cands = self.line_label_candidates(viewport, zoom);
        let proj = viewport.projector();
        let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if let Ok(mut cache) = self.render.line_labels.lock() {
            cache.frame(
                &cands,
                zoom,
                |mx, my| proj.to_screen(mx, my),
                origin,
                view,
                &mut out,
            );
        }
        out
    }
}

/// Order among street names: bigger roads first.
pub(super) fn highway_label_rank(tags: &HashMap<String, String>) -> i32 {
    match tags.get("highway").map(String::as_str) {
        Some("motorway" | "trunk") => 90,
        Some("primary") => 80,
        Some("secondary") => 70,
        Some("tertiary") => 60,
        Some("unclassified" | "residential") => 50,
        Some("service" | "living_street" | "pedestrian") => 40,
        Some(_) => 30,
        None => 20,
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it carries `gpui::test`, which shadows `#[test]`.
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::LayerId;

    #[test]
    fn a_bench_action_changes_the_placement_key_of_that_layer_only() {
        let a = OsmLayer::new(LayerId(1));
        let b = OsmLayer::new(LayerId(2));
        let key = |l: &OsmLayer| {
            let t = l.render.label_cache.lock().unwrap().tuning;
            l.label_sheet_id(&t)
        };
        let before = key(&a);
        a.set_label_bench("on halo=none").unwrap();
        assert_ne!(key(&a), before);
        assert_eq!(key(&b), before, "the other layer is untouched");
        assert_eq!(
            a.render.label_cache.lock().unwrap().tuning.bench.halo,
            crate::layers::line_labels::MeshHalo::None
        );
        assert_eq!(
            b.render.label_cache.lock().unwrap().tuning.bench,
            crate::layers::line_labels::BenchKnobs::default()
        );
        assert!(a.set_label_bench("halo=wat").is_err());
    }
}
