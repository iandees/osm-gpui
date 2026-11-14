use super::*;

impl OsmLayer {
    /// The screen-space offset to apply to `node_id`'s projected position
    /// this frame: the drag preview's delta if the node is part of it, zero
    /// otherwise.
    pub(super) fn drag_preview_offset(&self, node_id: i64) -> Point<Pixels> {
        match &self.view.drag_preview {
            Some((ids, delta)) if ids.contains(&node_id) => *delta,
            _ => Point::default(),
        }
    }

    pub(super) fn render_highlight_of(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
        kind: HighlightKind,
    ) {
        if feature.layer_id != self.id {
            return;
        }
        let Some(ref osm_data) = self.store.osm_data else {
            return;
        };
        let (halo, core) = highlight_colors(kind);

        match feature.kind {
            FeatureKind::Node => {
                let Some(n) = osm_data.nodes.get(&feature.id) else {
                    return;
                };
                let Some((lat, lon)) = validate_coords(n.lat, n.lon) else {
                    return;
                };
                let mut sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) {
                    return;
                }
                sp += self.drag_preview_offset(feature.id);
                // Off screen (a big selection mostly is): nothing to draw.
                const MARGIN: f32 = 30.0;
                if kind == HighlightKind::Selected
                    && (sp.x.as_f32() < -MARGIN
                        || sp.y.as_f32() < -MARGIN
                        || sp.x.as_f32() > bounds.size.width.as_f32() + MARGIN
                        || sp.y.as_f32() > bounds.size.height.as_f32() + MARGIN)
                {
                    return;
                }
                let node_style = self.store.stylesheet.node_style(&n.tags, NODE_STYLE_ZOOM);
                let center = point(sp.x + bounds.origin.x, sp.y + bounds.origin.y);
                // The marker itself, in full: the zoom rule may have hidden
                // it or drawn it small.
                paint_node_marker(window, center, node_style);
                // Two concentric rings around the marker: the core color
                // inside, the halo color outside.
                let core_diameter = node_style.size + 10.0;
                paint_ring(window, center, core_diameter + 4.0, 2.0, halo);
                paint_ring(window, center, core_diameter, 2.0, core);
            }
            FeatureKind::Way => {
                let Some(way) = osm_data.ways.get(&feature.id) else {
                    return;
                };
                if way.nodes.len() < 2 {
                    return;
                }

                let origin_x = bounds.origin.x;
                let origin_y = bounds.origin.y;

                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
                // Parallel to `pts`: each node's marker style, so the
                // vertex-marker repaint below can reuse the same projected
                // screen point without redoing coordinate validation/lookup.
                let mut node_styles: Vec<NodeStyle> = Vec::with_capacity(way.nodes.len());
                for node_id in &way.nodes {
                    if let Some(n) = osm_data.nodes.get(node_id) {
                        if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                            let mut sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                sp += self.drag_preview_offset(*node_id);
                                pts.push(point(sp.x + origin_x, sp.y + origin_y));
                                node_styles.push(
                                    self.store.stylesheet.node_style(&n.tags, NODE_STYLE_ZOOM),
                                );
                            }
                        }
                    }
                }
                if pts.len() < 2 {
                    return;
                }
                // A relation can have hundreds of member ways, most of them
                // off screen at any zoom that shows a few: skip those.
                // A selection of thousands (Edit > Find) does the same.
                if matches!(kind, HighlightKind::Member | HighlightKind::Selected) {
                    let xs = pts.iter().map(|p| p.x.as_f32());
                    let ys = pts.iter().map(|p| p.y.as_f32());
                    let (min_x, max_x) =
                        xs.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                    let (min_y, max_y) =
                        ys.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                    let (left, top) = (origin_x.as_f32(), origin_y.as_f32());
                    let right = left + bounds.size.width.as_f32();
                    let bottom = top + bounds.size.height.as_f32();
                    if max_x < left || min_x > right || max_y < top || min_y > bottom {
                        return;
                    }
                }

                // A wide halo stroke under a narrower core stroke.
                let way_style = self.store.stylesheet.way_style(
                    &way.tags,
                    way.is_closed(),
                    viewport.zoom_level(),
                );
                // Drawn with the way pass's own stroke geometry (mitered
                // bends, round ends, a joined closing corner) rather than a
                // tessellated stroke with square ends.
                let mut polyline = way_stroke::Polyline::default();
                polyline.set(pts.iter().map(|p| (p.x.as_f32(), p.y.as_f32())));
                for (extra_width, color) in [(7.0, halo), (3.0, core)] {
                    let mut batch = PathBatch::new();
                    polyline.emit(
                        &mut batch.sink(),
                        (way_style.width + extra_width) / 2.0,
                        way_stroke::Shape::FULL,
                    );
                    if let Some(path) = batch.finish() {
                        window.paint_path(path, rgb(color));
                    }
                }

                // The strokes above cover the way's own direction arrows, so
                // draw them again on top: core-colored, each outlined in the
                // halo color.
                if way_style.direction != Direction::None && viewport.zoom_level() >= ARROW_MIN_ZOOM
                {
                    let reverse = way_style.direction == Direction::Reverse;
                    let half_base = (way_style.width + 3.0) / 2.0 + ARROW_OVERHANG;
                    for (grow, color) in [(1.5, halo), (0.0, core)] {
                        let mut batch = PathBatch::new();
                        for i in arrow_segments(&pts) {
                            if let Some([tip, left, right]) = arrow_triangle(
                                pts[i],
                                pts[i + 1],
                                reverse,
                                ARROW_HALF_LENGTH + 1.0 + 1.5 * grow,
                                half_base + grow,
                            ) {
                                batch.triangle(tip, left, right);
                            }
                        }
                        if let Some(path) = batch.finish() {
                            window.paint_path(path, rgb(color));
                        }
                    }
                }

                // Re-paint the way's vertex markers on top of the highlight
                // strokes above — otherwise they fully cover the node-marker
                // quads painted earlier in `render_canvas`, hiding the way's
                // vertices. This also shows a highlighted way's vertices at
                // zooms where plain vertices are otherwise hidden.
                // Hover and relation members repaint only the markers the
                // zoom rule shows (the ones that can be targeted); a
                // selected way shows all of its vertices.
                let zoom = viewport.zoom_level();
                // The hovered way is the preview way: its vertices that are
                // not otherwise shown get the small preview dot (the
                // stroke above covered the ones painted by `render_canvas`).
                let preview = kind == HighlightKind::Hovered
                    && self.view.preview_way == Some(feature.id)
                    && zoom >= node_markers::PREVIEW_MIN_ZOOM;
                for (p, style) in pts.iter().zip(node_styles.iter()) {
                    if kind == HighlightKind::Selected
                        || node_markers::node_form(style.tagged, false, false, zoom).is_shown()
                    {
                        paint_node_marker(window, *p, *style);
                    } else if preview {
                        paint_preview_marker(window, *p);
                    }
                }
            }
            FeatureKind::Relation => {
                // A relation has no shape of its own: mark each member that
                // is loaded (nested relations' members too, one level).
                let Some(relation) = osm_data.relations.iter().find(|r| r.id == feature.id) else {
                    return;
                };
                // A multipolygon is an area: tint it in the highlight's color
                // so the whole shape reads as selected, under the members.
                if let Some(entry) = self.derived.mp.entries.get(&feature.id) {
                    self.render_area_tint(viewport, bounds, window, entry, core, kind);
                }
                let find = |id: i64| osm_data.relations.iter().find(|r| r.id == id).cloned();
                for (member_type, id) in crate::relations::flatten_members(relation, &find) {
                    let kind = match member_type {
                        crate::osm::MemberType::Node => FeatureKind::Node,
                        crate::osm::MemberType::Way => FeatureKind::Way,
                        crate::osm::MemberType::Relation => continue,
                    };
                    let member = FeatureRef {
                        layer_id: self.id,
                        kind,
                        id,
                    };
                    self.render_highlight(viewport, bounds, window, &member, HighlightKind::Member);
                }
            }
        }
    }
}
