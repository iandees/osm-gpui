use super::*;

/// How far the casing extends beyond each side of a way's stroke, in pixels.
pub(super) const CASING_WIDTH: f32 = 1.0;

/// Below this zoom the casing is skipped: ways are dense enough that it
/// only adds clutter, and it doubles the way pass's vertex count exactly
/// when the most ways are on screen. It is drawn from zoom 16, where editing
/// starts (`COARSE_BELOW_ZOOM`).
pub(super) const CASING_MIN_ZOOM: f64 = 16.0;

/// Below this zoom direction arrows aren't drawn.
pub(super) const ARROW_MIN_ZOOM: f64 = 16.0;

/// Path length between direction arrows along a way, in pixels.
pub(super) const ARROW_SPACING_PX: f32 = 96.0;

/// A direction arrow is only placed on a segment at least this long, so it
/// never overhangs the segment's ends.
pub(super) const ARROW_MIN_SEGMENT_PX: f32 = 12.0;

/// Half the length of a direction arrow along the way, in pixels.
pub(super) const ARROW_HALF_LENGTH: f32 = 4.5;

/// How far a direction arrow's base extends beyond each side of the way's
/// stroke, in pixels.
pub(super) const ARROW_OVERHANG: f32 = 3.0;

/// Minimum on-screen length (in pixels) a way segment must accumulate before
/// it's emitted as its own quad. Segments shorter than this while zoomed out
/// are skipped (their span just gets absorbed into the next emitted
/// segment), cutting emitted vertices with no visible geometry change. The
/// way's final vertex is always emitted exactly regardless of this
/// threshold, so endpoints/junctions and hit-testing (which reads the
/// undecimated cached geometry, not this render-only path) are unaffected.
pub(super) const MIN_SEGMENT_PX: f32 = 1.0;

/// Below this zoom the way pass draws coarser (see `WayLod`): a longer
/// minimum segment, tiny ways collapsed to one stroke or left out, tiny area
/// fills left out. At and above it (zoom 16, where editing starts) only the
/// one-pixel segment merge applies.
pub(super) const COARSE_BELOW_ZOOM: f64 = 16.0;

/// Below this zoom, ways drawn no wider than `MINOR_WAY_WIDTH` (footpaths,
/// buildings' and land-use outlines, barriers: everything the default
/// stylesheet draws at 1.5 px or less) get no outline. Areas keep their fill.
/// At this scale they are far too small to pick out, and they are most of the
/// vertices of a dense city.
pub(super) const MINOR_WAYS_BELOW_ZOOM: f64 = 15.0;

/// Widest stroke, in pixels, that counts as a minor way.
pub(super) const MINOR_WAY_WIDTH: f32 = 1.5;

/// Douglas-Peucker tolerance at and above `COARSE_BELOW_ZOOM`, in pixels: a
/// vertex that moves the line by under a third of a pixel is not drawn.
pub(super) const TOLERANCE_PX: f32 = 0.35;

/// Douglas-Peucker tolerance below `COARSE_BELOW_ZOOM`, in pixels.
pub(super) const COARSE_TOLERANCE_PX: f32 = 1.0;

/// Minimum merged segment length below `COARSE_BELOW_ZOOM`, in pixels.
pub(super) const COARSE_MIN_SEGMENT_PX: f32 = 2.0;

/// Below `COARSE_BELOW_ZOOM`, a way whose bounding box is under this many
/// pixels across is drawn as a single stroke across the box.
pub(super) const COLLAPSE_BELOW_PX: f32 = 3.0;

/// Below `COARSE_BELOW_ZOOM`, a way whose bounding box is under this many
/// pixels across is not drawn at all.
pub(super) const SKIP_BELOW_PX: f32 = 0.75;

/// Below `COARSE_BELOW_ZOOM`, an area under this many pixels across gets no
/// fill (its outline covers it).
pub(super) const TINY_FILL_BELOW_PX: f32 = 2.0;

/// Decide which segments of an already-projected screen-space polyline to
/// actually emit: skip interior vertices until the pending segment (measured
/// from the last emitted "anchor" point) has accumulated at least `min_px`
/// screen pixels, but always emit a final segment ending at the last point
/// so way endpoints are never rounded away. Returns `(anchor_idx, end_idx)`
/// pairs indexing into `pts`.
///
/// Pure/allocation-light and independent of `Window`/`Path` so it can be
/// unit-tested directly.
#[cfg(test)]
pub(super) fn decimate_segments(pts: &[Point<Pixels>], min_px: f32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    decimate_into(pts, min_px, &mut out);
    out
}

/// `decimate_segments` into a reused buffer (cleared first), so the per-way
/// loop of a frame does not allocate.
pub(super) fn decimate_into(pts: &[Point<Pixels>], min_px: f32, out: &mut Vec<(usize, usize)>) {
    out.clear();
    if pts.len() < 2 {
        return;
    }
    let min_px2 = min_px * min_px;
    let mut anchor = 0usize;
    let last = pts.len() - 1;
    for i in 1..pts.len() {
        let dx = f32::from(pts[i].x - pts[anchor].x);
        let dy = f32::from(pts[i].y - pts[anchor].y);
        let dist2 = dx * dx + dy * dy;
        if i == last || dist2 >= min_px2 {
            out.push((anchor, i));
            anchor = i;
        }
    }
}

/// Push one solid triangle into `path`, creating the path on first use and
/// widening `bounds_min_max`. Same raw-vertex batching approach as
/// raw vertices, no Lyon tessellation.
pub(super) fn push_triangle(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    a: Point<Pixels>,
    b: Point<Pixels>,
    c: Point<Pixels>,
) {
    let (min_x, max_x, min_y, max_y) = bounds_min_max;
    for p in [a, b, c] {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        if x < *min_x {
            *min_x = x;
        }
        if x > *max_x {
            *max_x = x;
        }
        if y < *min_y {
            *min_y = y;
        }
        if y > *max_y {
            *max_y = y;
        }
    }
    let st = point(0., 1.);
    let p = path.get_or_insert_with(|| Path::new(a));
    for xy in [a, b, c] {
        p.vertices.push(PathVertex {
            xy_position: xy,
            st_position: st,
            content_mask: Default::default(),
        });
    }
}

/// The triangle for a direction arrow centered on the segment `p0`-`p1`,
/// pointing from `p0` to `p1` (or the other way if `reverse`): tip first,
/// then the two base corners. `half_length` is measured along the segment
/// and `half_base` across it. `None` for a degenerate segment.
pub(super) fn arrow_triangle(
    p0: Point<Pixels>,
    p1: Point<Pixels>,
    reverse: bool,
    half_length: f32,
    half_base: f32,
) -> Option<[Point<Pixels>; 3]> {
    let (x0, y0) = (f32::from(p0.x), f32::from(p0.y));
    let (x1, y1) = (f32::from(p1.x), f32::from(p1.y));
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < f32::EPSILON {
        return None;
    }
    let sign = if reverse { -1.0 } else { 1.0 };
    let (ux, uy) = (dx / len * sign, dy / len * sign);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (bx, by) = (mx - ux * half_length, my - uy * half_length);
    Some([
        point(px(mx + ux * half_length), px(my + uy * half_length)),
        point(px(bx - uy * half_base), px(by + ux * half_base)),
        point(px(bx + uy * half_base), px(by - ux * half_base)),
    ])
}

/// Which segments of the polyline `pts` get a direction arrow: the index
/// `i` of each chosen segment `pts[i]`-`pts[i + 1]`. One arrow per
/// `ARROW_SPACING_PX` of path length, starting half a spacing in so even a
/// short way gets one, and never on a segment too short to hold it.
pub(super) fn arrow_segments(pts: &[Point<Pixels>]) -> Vec<usize> {
    let mut chosen = Vec::new();
    let mut since_arrow = ARROW_SPACING_PX / 2.0;
    for (i, pair) in pts.windows(2).enumerate() {
        let dx = f32::from(pair[1].x - pair[0].x);
        let dy = f32::from(pair[1].y - pair[0].y);
        let len = (dx * dx + dy * dy).sqrt();
        since_arrow += len;
        if since_arrow >= ARROW_SPACING_PX && len >= ARROW_MIN_SEGMENT_PX {
            since_arrow = 0.0;
            chosen.push(i);
        }
    }
    chosen
}

/// What the previous frame's way pass ended up with: the vertex count of each
/// batch, keyed by kind and colour/width. The next frame reserves that much
/// up front, so the (up to millions of vertices, tens of MB) paths are not
/// grown by doubling and copying while they are filled.
#[derive(Default)]
pub(super) struct VertexHints(pub(super) std::sync::Mutex<HashMap<u64, usize>>);

impl VertexHints {
    pub(super) fn snapshot(&self) -> HashMap<u64, usize> {
        self.0.lock().map(|m| m.clone()).unwrap_or_default()
    }

    pub(super) fn replace(&self, hints: HashMap<u64, usize>) {
        if let Ok(mut m) = self.0.lock() {
            *m = hints;
        }
    }
}

/// Which kind of batch a hint key belongs to.
#[derive(Clone, Copy)]
pub(super) enum BatchKind {
    Fill = 1,
    Overlay = 2,
    Casing = 3,
    Stroke = 4,
}

/// A batch of raw triangles painted in one colour, with the bounds of its
/// vertices accumulated as it grows.
pub(super) struct Batch {
    pub(super) rgba: u32,
    /// Draw-order layer (`z-index`, see [`z_key`]); batches of one kind are
    /// painted in ascending `z`, then first-seen order.
    pub(super) z: i32,
    /// Key of this batch's `VertexHints` entry.
    pub(super) key: u64,
    pub(super) path: Option<Path<Pixels>>,
    /// (min_x, max_x, min_y, max_y), accumulated by `PathSink` and
    /// `push_triangle`.
    pub(super) bounds: (f32, f32, f32, f32),
}

impl Batch {
    pub(super) fn new(kind: BatchKind, rgba: u32, z: i32, hints: &HashMap<u64, usize>) -> Self {
        let key = ((kind as u64) << 60) ^ ((rgba as u64) << 24) ^ (z as u32 as u64);
        // The previous frame's size plus a margin, capped so that a stale
        // hint (after zooming in from far out) cannot reserve gigabytes.
        let path = hints.get(&key).map(|&n| {
            let mut p = Path::new(point(px(0.0), px(0.0)));
            p.vertices.reserve((n + n / 8).min(MAX_HINT_VERTICES));
            p
        });
        Self {
            rgba,
            z,
            key,
            path,
            bounds: (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ),
        }
    }

    /// The finished path with its bounds set, or `None` if nothing was
    /// pushed. Records the final size in `hints`.
    pub(super) fn finish(self, hints: &mut HashMap<u64, usize>) -> Option<(u32, Path<Pixels>)> {
        let mut path = self.path?;
        if path.vertices.is_empty() {
            return None;
        }
        hints.insert(self.key, path.vertices.len());
        let (min_x, max_x, min_y, max_y) = self.bounds;
        path.bounds = Bounds {
            origin: point(px(min_x), px(min_y)),
            size: size(px(max_x - min_x), px(max_y - min_y)),
        };
        Some((self.rgba, path))
    }
}

/// Upper bound of a reserved batch, in vertices (32 bytes each).
pub(super) const MAX_HINT_VERTICES: usize = 8 << 20;

/// The index of the batch for `rgba` in `batches`, creating it on first
/// use. First-encounter order, so paint order is deterministic across frames
/// (a `HashMap`'s iteration order is randomized per process, which caused
/// visible z-order flicker between overlapping fills of different colors).
pub(super) fn batch_index(
    index: &mut HashMap<(i32, u32), usize>,
    batches: &mut Vec<Batch>,
    kind: BatchKind,
    z: i32,
    rgba: u32,
    hints: &HashMap<u64, usize>,
) -> usize {
    *index.entry((z, rgba)).or_insert_with(|| {
        batches.push(Batch::new(kind, rgba, z, hints));
        batches.len() - 1
    })
}

/// A `z-index` as a sortable layer number (eighths, so fractional indices
/// like 0.5 order correctly).
pub(super) fn z_key(z_index: f32) -> i32 {
    (z_index.clamp(-1.0e6, 1.0e6) * 8.0).round() as i32
}

/// Finish `batches` into paths in draw order: ascending `z`, ties by
/// first-seen order (a stable sort), so the order is the same every frame.
pub(super) fn finish_sorted(
    mut batches: Vec<Batch>,
    hints: &mut HashMap<u64, usize>,
) -> Vec<(u32, Path<Pixels>)> {
    batches.sort_by_key(|b| b.z);
    batches
        .into_iter()
        .filter_map(|b| b.finish(hints))
        .collect()
}

/// The way pass of one frame, recorded but not yet painted: area fills, route
/// overlays, the casing and the strokes, each batched into as few paths as
/// there are colours or widths. Building it needs no `Window`, so it can be
/// timed and tested on its own (`examples/way_bench.rs`).
pub struct WayScene {
    pub(super) fills: Vec<(u32, Path<Pixels>)>,
    pub(super) overlays: Vec<(u32, Path<Pixels>)>,
    pub(super) casing: Option<(u32, Path<Pixels>)>,
    /// The casings from `casing-width` and the strokes, in one list in
    /// `z-index` order: at each layer the casings come first, so a higher
    /// layer's casing covers a lower layer's stroke.
    pub(super) lines: Vec<(u32, Path<Pixels>)>,
}

impl WayScene {
    /// Total triangle-list vertices in the scene (6 per stroke segment).
    pub fn vertex_count(&self) -> usize {
        let sum =
            |v: &[(u32, Path<Pixels>)]| -> usize { v.iter().map(|(_, p)| p.vertices.len()).sum() };
        let casing = self.casing.as_ref().map_or(0, |(_, p)| p.vertices.len());
        sum(&self.fills) + sum(&self.overlays) + casing + sum(&self.lines)
    }

    /// Number of paths the scene hands to the GPU. Each is rasterised and
    /// composited separately, over its own bounding box.
    pub fn path_count(&self) -> usize {
        self.fills.len() + self.overlays.len() + self.casing.is_some() as usize + self.lines.len()
    }

    /// Hand the paths to the window: fills first so outlines stay on top,
    /// then route overlays, the casing and the strokes.
    pub(super) fn paint(self, window: &mut Window) {
        let fs_phase = crate::frame_stats::start();
        for (rgba, path) in self.fills {
            window.paint_path(path, gpui::rgba(rgba));
        }
        crate::frame_stats::lap("osm.fill_paint", fs_phase);
        let fs_phase = crate::frame_stats::start();
        for (rgba, path) in self.overlays {
            window.paint_path(path, gpui::rgba(rgba));
        }
        if let Some((rgba, path)) = self.casing {
            window.paint_path(path, gpui::rgba(rgba));
        }
        for (rgba, path) in self.lines {
            window.paint_path(path, gpui::rgba(rgba));
        }
        crate::frame_stats::lap("osm.ways_paint", fs_phase);
    }
}

/// How much of a way is drawn at the current zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WayDetail {
    /// Not drawn: under a pixel across.
    Skip,
    /// Drawn as one short stroke across its bounding box.
    Collapse,
    /// Every (merged) segment.
    Full,
}

/// The level-of-detail rules of the way pass at one zoom. Pure, so the
/// zoom rule is unit-tested. From `COARSE_BELOW_ZOOM` up (the editing
/// zooms) nothing is simplified beyond the one-pixel segment merge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct WayLod {
    pub(super) coarse: bool,
    /// Whether minor ways (see `MINOR_WAYS_BELOW_ZOOM`) are left out.
    pub(super) drops_minor: bool,
    /// Vertices that deviate less than this many pixels from the line
    /// through their neighbours are dropped (Douglas-Peucker tolerance).
    pub(super) tolerance_px: f32,
    pub(super) min_segment_px: f32,
}

impl WayLod {
    pub(super) fn at(zoom: f64) -> Self {
        if zoom >= COARSE_BELOW_ZOOM {
            Self {
                coarse: false,
                drops_minor: false,
                tolerance_px: TOLERANCE_PX,
                min_segment_px: MIN_SEGMENT_PX,
            }
        } else {
            Self {
                coarse: true,
                drops_minor: zoom < MINOR_WAYS_BELOW_ZOOM,
                tolerance_px: COARSE_TOLERANCE_PX,
                min_segment_px: COARSE_MIN_SEGMENT_PX,
            }
        }
    }

    /// Whether a way stroked `width` pixels wide is outlined at all.
    pub(super) fn strokes(&self, width: f32) -> bool {
        !(self.drops_minor && width <= MINOR_WAY_WIDTH)
    }

    /// What to draw of a way whose bounding box is `extent_px` across at its
    /// longest side.
    pub(super) fn detail(&self, extent_px: f32) -> WayDetail {
        if !self.coarse || extent_px >= COLLAPSE_BELOW_PX {
            WayDetail::Full
        } else if extent_px < SKIP_BELOW_PX {
            WayDetail::Skip
        } else {
            WayDetail::Collapse
        }
    }

    /// Whether an area whose bounding box is `extent_px` across gets a fill:
    /// a fill under two pixels across is hidden by its own outline.
    pub(super) fn fills(&self, extent_px: f32) -> bool {
        !self.coarse || extent_px >= TINY_FILL_BELOW_PX
    }
}

/// Longest side of a mercator bounding box in screen pixels.
#[inline]
pub(super) fn bbox_extent_px(b: &WayBbox, ppm_x: f64, ppm_y: f64) -> f32 {
    ((b.max_x - b.min_x) * ppm_x).max((b.max_y - b.min_y) * ppm_y) as f32
}

impl OsmLayer {
    /// Record the way pass (fills, route overlays, casing, strokes) for this
    /// frame: cull in Mercator space, project, merge sub-pixel segments and
    /// batch everything into one path per colour/width. `bounds` is the
    /// canvas rectangle.
    ///
    /// Nothing is cached across frames except what the edit paths already
    /// maintain (`way_vertices`, `way_bboxes`, `way_styles`, `way_fill_tris`,
    /// the `MpCache`), so no edit can leave this stale.
    pub fn build_way_scene(&self, viewport: &Viewport, bounds: Bounds<Pixels>) -> WayScene {
        let origin_x = bounds.origin.x;
        let origin_y = bounds.origin.y;
        let (ox, oy) = (f32::from(origin_x), f32::from(origin_y));
        let (vmin_x, vmax_x, vmin_y, vmax_y) = viewport.mercator_view_bounds();
        let proj = viewport.projector();
        let (ppm_x, ppm_y) = proj.pixels_per_meter();
        let zoom = viewport.zoom_level();
        let lod = WayLod::at(zoom);
        // Zoom-range rules: cached styles are zoom-agnostic and indexed by
        // signature into this per-zoom-bucket table (rebuilt only when the
        // bucket changes). `None` for a stylesheet with no zoom selectors.
        let zoom_table = self
            .store
            .stylesheet
            .is_zoom_dependent()
            .then(|| self.store.stylesheet.zoom_styles(zoom));
        let at_zoom = |s: WayStyle| -> WayStyle {
            match zoom_table.as_ref().and_then(|t| t.get(s.sig as usize)) {
                Some(z) => WayStyle {
                    direction: s.direction,
                    ..*z
                },
                None => s,
            }
        };
        // While nodes are being dragged the cached bounding boxes are stale,
        // so draw every way in full.
        let dragging = self.view.drag_preview.is_some();
        let hints = self.render.vertex_hints.snapshot();
        // The simplification tolerance in Web Mercator meters (nothing is
        // dropped while dragging, for the same reason).
        let tolerance_m = if dragging {
            0.0
        } else {
            (lod.tolerance_px as f64 / ppm_x) as f32
        };

        // Area fills: painted before strokes so outlines and nodes stay on
        // top. Triangulation is cached (`way_fill_tris`, computed at
        // load/edit time); per-frame work is projection + batched
        // raw-triangle emission, grouped by RGBA color.
        let mut fill_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut fills: Vec<Batch> = Vec::new();
        let mut fill_pts: Vec<Point<Pixels>> = Vec::new();

        // Multipolygon relations first, so a closed way's own fill (an
        // island, a building inside a forest) paints over them.
        let screen_rect = (
            ox,
            oy,
            f32::from(origin_x + bounds.size.width),
            f32::from(origin_y + bounds.size.height),
        );
        let fs_phase = crate::frame_stats::start();
        for entry in self.derived.mp.entries.values() {
            let fill_style = at_zoom(entry.style);
            let Some(fill) = fill_style.fill else {
                continue;
            };
            let b = &entry.bbox;
            if b.max_x < vmin_x || b.min_x > vmax_x || b.max_y < vmin_y || b.min_y > vmax_y {
                continue;
            }
            let alpha = (fill.opacity * 255.0).round() as u32;
            let group_idx = batch_index(
                &mut fill_index,
                &mut fills,
                BatchKind::Fill,
                z_key(fill_style.z_index),
                (fill.color << 8) | alpha,
                &hints,
            );
            let group = &mut fills[group_idx];
            for polygon in &entry.polygons {
                let pb = &polygon.bbox;
                if pb.max_x < vmin_x || pb.min_x > vmax_x || pb.max_y < vmin_y || pb.min_y > vmax_y
                {
                    continue;
                }
                if !dragging && !lod.fills(bbox_extent_px(pb, ppm_x, ppm_y)) {
                    continue;
                }
                self.push_polygon_fill(
                    polygon,
                    viewport,
                    (origin_x, origin_y),
                    screen_rect,
                    &mut fill_pts,
                    &mut group.path,
                    &mut group.bounds,
                );
            }
        }
        crate::frame_stats::lap("osm.mp_fill_build", fs_phase);

        let fs_phase = crate::frame_stats::start();
        for (i, tris) in self.render.way_fill_tris.iter().enumerate() {
            let Some(tris) = tris else { continue };
            let fill_style = at_zoom(self.render.way_styles[i]);
            let Some(fill) = fill_style.fill else {
                continue;
            };
            let bbox = match self.derived.way_bboxes.get(i).and_then(|b| b.as_ref()) {
                Some(b) => b,
                None => continue,
            };
            if bbox.max_x < vmin_x
                || bbox.min_x > vmax_x
                || bbox.max_y < vmin_y
                || bbox.min_y > vmax_y
            {
                continue;
            }
            if !dragging && !lod.fills(bbox_extent_px(bbox, ppm_x, ppm_y)) {
                continue;
            }

            let verts = &self.derived.way_vertices[i];
            let ring_len = verts.len() - 1; // closing duplicate excluded
            fill_pts.clear();
            for &(node_id, mx, my) in &verts[..ring_len] {
                let (x, y) = proj.to_screen(mx, my);
                if !(x.is_finite() && y.is_finite()) {
                    break;
                }
                let sp = point(px(x), px(y)) + self.drag_preview_offset(node_id);
                fill_pts.push(point(sp.x + origin_x, sp.y + origin_y));
            }
            // Any invalid projection breaks index alignment with the cached
            // triangle indices — skip the fill (the stroke still draws).
            if fill_pts.len() != ring_len {
                continue;
            }

            let alpha = (fill.opacity * 255.0).round() as u32;
            let group_idx = batch_index(
                &mut fill_index,
                &mut fills,
                BatchKind::Fill,
                z_key(fill_style.z_index),
                (fill.color << 8) | alpha,
                &hints,
            );
            let group = &mut fills[group_idx];
            for t in tris.as_chunks::<3>().0 {
                push_triangle(
                    &mut group.path,
                    &mut group.bounds,
                    fill_pts[t[0] as usize],
                    fill_pts[t[1] as usize],
                    fill_pts[t[2] as usize],
                );
            }
        }
        crate::frame_stats::lap("osm.way_fill_build", fs_phase);

        // Ways: bbox-cull in Mercator space, then group visible ways by
        // `(color, width)` style, one `paint_path` call per group. Vertex lookup is a single contiguous slice per way
        // (`way_vertices[i]`) — no HashMap indirection.
        //
        // Each segment is emitted as two raw triangles (an oriented quad)
        // pushed directly into the group's `Path`, bypassing
        // `PathBuilder::stroke`'s Lyon tessellation pass entirely — that
        // tessellator dominated frame time, since it reruns from scratch
        // every repaint (e.g. every mouse-move while panning). Joins and end caps are built by hand in `way_stroke`
        // (mitered where possible, which costs no extra vertices) for
        // strokes wide enough to show them, at the editing zooms only.
        //
        // One path per stroke *colour*, whatever the width: every extra path
        // is rasterised and composited over its whole bounding box by the
        // GPU (a dozen near-screen-sized paths cost more than a million
        // vertices), and a width is only the half-width of each segment's
        // quad.
        let mut stroke_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut strokes: Vec<Batch> = Vec::new();
        // Consecutive ways mostly share a style, so remember the last lookup.
        let mut last_stroke: Option<((i32, u32), usize)> = None;

        // Route overlays: a translucent line under each route member way,
        // one path per colour. Looked up per visible way from the cache
        // (`MpCache::route_ways`), never by scanning relations.
        let has_routes = !self.derived.mp.route_ways.is_empty();
        let mut overlay_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut overlays: Vec<Batch> = Vec::new();
        let overlay_alpha = (crate::relation_overlay::ROUTE_ALPHA * 255.0).round() as u32;

        // The casing: one dark path under every way's stroke (and direction
        // arrows), slightly wider than the stroke, so thin light lines stay
        // readable over light imagery. Built in the same pass as the strokes
        // from the same projected points, and painted before any of them.
        let draw_casing = zoom >= CASING_MIN_ZOOM;
        let draw_arrows = zoom >= ARROW_MIN_ZOOM;
        let mut casing = Batch::new(BatchKind::Casing, (CASING_COLOR << 8) | 0xFF, 0, &hints);
        // Casings asked for by the stylesheet (`casing-width`), one per colour.
        let mut styled_casing_index: HashMap<(i32, u32), usize> = HashMap::new();
        let mut styled_casings: Vec<Batch> = Vec::new();

        // Reused scratch buffers, so the (potentially tens of thousands of)
        // visible ways of a frame don't each allocate.
        let mut scratch_pts: Vec<Point<Pixels>> = Vec::new();
        let mut segs: Vec<(usize, usize)> = Vec::new();
        let mut polyline = way_stroke::Polyline::default();

        let mut ways_in_view = 0usize;
        let mut ways_skipped = 0usize;
        let mut ways_collapsed = 0usize;

        let fs_phase = crate::frame_stats::start();
        for (i, verts) in self.derived.way_vertices.iter().enumerate() {
            if verts.len() < 2 {
                continue;
            }
            let bbox = match self.derived.way_bboxes.get(i).and_then(|b| b.as_ref()) {
                Some(b) => b,
                None => continue,
            };
            if bbox.max_x < vmin_x
                || bbox.min_x > vmax_x
                || bbox.max_y < vmin_y
                || bbox.min_y > vmax_y
            {
                continue;
            }
            ways_in_view += 1;

            let detail = if dragging {
                WayDetail::Full
            } else {
                lod.detail(bbox_extent_px(bbox, ppm_x, ppm_y))
            };
            if detail == WayDetail::Skip {
                ways_skipped += 1;
                continue;
            }

            // Style is resolved once per way at data-load time
            // (`compute_way_tables`/`commit_node_moves`/`set_stylesheet`),
            // not here — no stylesheet lookup or tags HashMap access in this
            // per-frame loop.
            let style = at_zoom(self.render.way_styles[i]);
            if !dragging && !lod.strokes(style.width) {
                ways_skipped += 1;
                continue;
            }
            let half_width = style.width / 2.0;
            let z = z_key(style.z_index);
            let stroke_rgba = (style.color << 8) | (style.opacity * 255.0).round() as u32;
            let stroke_idx = match last_stroke {
                Some(((lz, c), idx)) if lz == z && c == stroke_rgba => idx,
                _ => {
                    let idx = batch_index(
                        &mut stroke_index,
                        &mut strokes,
                        BatchKind::Stroke,
                        z,
                        stroke_rgba,
                        &hints,
                    );
                    last_stroke = Some(((z, stroke_rgba), idx));
                    idx
                }
            };
            // Dashes only where a segment is long enough to show them; a
            // collapsed way is a single stroke across its bounding box.
            let dashes = style
                .dashes
                .filter(|_| detail == WayDetail::Full && !lod.coarse);
            let stroke = &mut strokes[stroke_idx];

            // An explicit casing replaces the default dark one.
            let styled_casing_idx = (style.casing_width > 0.0).then(|| {
                batch_index(
                    &mut styled_casing_index,
                    &mut styled_casings,
                    BatchKind::Casing,
                    z,
                    (style.casing_color << 8) | 0xFF,
                    &hints,
                )
            });
            let default_casing = draw_casing && styled_casing_idx.is_none();

            let overlay_idx = if has_routes {
                self.derived
                    .mp
                    .route_colour_of_way(self.derived.way_ids[i])
                    .map(|colour| {
                        batch_index(
                            &mut overlay_index,
                            &mut overlays,
                            BatchKind::Overlay,
                            0,
                            (colour << 8) | overlay_alpha,
                            &hints,
                        )
                    })
            } else {
                None
            };

            scratch_pts.clear();
            let min_segment_px = if detail == WayDetail::Collapse {
                // Two corners of the bounding box: one stroke, whatever its
                // length.
                ways_collapsed += 1;
                for (mx, my) in [(bbox.min_x, bbox.min_y), (bbox.max_x, bbox.max_y)] {
                    let (x, y) = proj.to_screen(mx, my);
                    scratch_pts.push(point(px(x + ox), px(y + oy)));
                }
                0.0
            } else {
                // Only the vertices significant at this zoom (Douglas-
                // Peucker, see `WayGeom`) are projected at all.
                for (&(node_id, mx, my), &sig) in verts.verts.iter().zip(&verts.sig) {
                    if sig < tolerance_m {
                        continue;
                    }
                    let (x, y) = proj.to_screen(mx, my);
                    if !(x.is_finite() && y.is_finite()) {
                        continue;
                    }
                    let sp = point(px(x), px(y)) + self.drag_preview_offset(node_id);
                    scratch_pts.push(point(sp.x + origin_x, sp.y + origin_y));
                }
                lod.min_segment_px
            };
            if scratch_pts.len() < 2 {
                continue;
            }

            // Decimate: skip segments under the minimum screen length while
            // zoomed out, but always emit a segment ending at the way's
            // final projected vertex so junctions/endpoints are never
            // rounded away. This is purely a rendering optimization —
            // `way_vertices` (read by hit-testing and selection highlight)
            // is untouched.
            decimate_into(&scratch_pts, min_segment_px, &mut segs);

            // The merged polyline (the anchors of `segs`, in order), stroked
            // as one: with joins at its bends and caps at its ends when the
            // stroke is wide enough to show them (see `way_stroke`), never
            // in the coarse zoom path or for a collapsed way.
            // Ways a few pixels across (most buildings at zoom 16) are too
            // small for a bend to show, and are the bulk of the cost.
            let shaped_lod = !lod.coarse
                && detail == WayDetail::Full
                && (dragging
                    || bbox_extent_px(bbox, ppm_x, ppm_y) >= way_stroke::SHAPE_MIN_EXTENT_PX);
            let casing_extent = if styled_casing_idx.is_some() {
                style.casing_width
            } else {
                CASING_WIDTH
            };
            let (casing_hw, overlay_hw) = (
                half_width + casing_extent,
                half_width + casing_extent + ROUTE_OVERLAY_EXTRA,
            );
            let widest_hw = if overlay_idx.is_some() {
                overlay_hw
            } else if default_casing || styled_casing_idx.is_some() {
                casing_hw
            } else {
                half_width
            };
            let with_joins = shaped_lod && widest_hw * 2.0 >= way_stroke::JOIN_MIN_WIDTH_PX;
            let caps = with_joins && style.width >= way_stroke::CAP_MIN_WIDTH_PX;
            let shape_of = |hw: f32| way_stroke::Shape {
                joins: with_joins && hw * 2.0 >= way_stroke::JOIN_MIN_WIDTH_PX,
                caps,
            };
            if with_joins {
                let anchors = std::iter::once(scratch_pts[segs[0].0])
                    .chain(segs.iter().map(|&(_, b)| scratch_pts[b]))
                    .map(|p| (f32::from(p.x), f32::from(p.y)));
                polyline.set(anchors);
                let overlay_layer = overlay_idx.map(|oi| {
                    let og = &mut overlays[oi];
                    Layer {
                        sink: PathSink {
                            path: &mut og.path,
                            bounds: &mut og.bounds,
                        },
                        hw: overlay_hw,
                        shape: shape_of(overlay_hw),
                    }
                });
                let casing_layer = default_casing.then(|| Layer {
                    sink: PathSink {
                        path: &mut casing.path,
                        bounds: &mut casing.bounds,
                    },
                    hw: casing_hw,
                    shape: shape_of(casing_hw),
                });
                let styled_layer = styled_casing_idx.map(|ci| {
                    let cb = &mut styled_casings[ci];
                    Layer {
                        sink: PathSink {
                            path: &mut cb.path,
                            bounds: &mut cb.bounds,
                        },
                        hw: casing_hw,
                        shape: shape_of(casing_hw),
                    }
                });
                let stroke_layer = Layer {
                    sink: PathSink {
                        path: &mut stroke.path,
                        bounds: &mut stroke.bounds,
                    },
                    hw: half_width,
                    shape: shape_of(half_width),
                };
                polyline.emit_layers(&mut [
                    overlay_layer,
                    casing_layer,
                    styled_layer,
                    dashes.is_none().then_some(stroke_layer),
                ]);
            } else {
                // Plain rectangles; no joins at this width.
                for &(a, b) in &segs {
                    let pa = (f32::from(scratch_pts[a].x), f32::from(scratch_pts[a].y));
                    let pb = (f32::from(scratch_pts[b].x), f32::from(scratch_pts[b].y));
                    if let Some(oi) = overlay_idx {
                        let og = &mut overlays[oi];
                        let mut sink = PathSink {
                            path: &mut og.path,
                            bounds: &mut og.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, overlay_hw);
                    }
                    if default_casing {
                        let mut sink = PathSink {
                            path: &mut casing.path,
                            bounds: &mut casing.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, casing_hw);
                    }
                    if let Some(ci) = styled_casing_idx {
                        let cb = &mut styled_casings[ci];
                        let mut sink = PathSink {
                            path: &mut cb.path,
                            bounds: &mut cb.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, casing_hw);
                    }
                    if dashes.is_none() {
                        let mut sink = PathSink {
                            path: &mut stroke.path,
                            bounds: &mut stroke.bounds,
                        };
                        way_stroke::emit_segment(&mut sink, pa, pb, half_width);
                    }
                }
            }
            if let (Some((dash, gap)), Some(first)) = (dashes, segs.first()) {
                let anchors: Vec<(f32, f32)> = std::iter::once(scratch_pts[first.0])
                    .chain(segs.iter().map(|&(_, b)| scratch_pts[b]))
                    .map(|p| (f32::from(p.x), f32::from(p.y)))
                    .collect();
                let mut sink = PathSink {
                    path: &mut stroke.path,
                    bounds: &mut stroke.bounds,
                };
                way_stroke::emit_dashed(&mut sink, &anchors, dash, gap, half_width);
            }

            let arrows = draw_arrows && style.direction != Direction::None;
            let reverse = style.direction == Direction::Reverse;
            // Start half a spacing in, so even a short way gets an arrow.
            let mut since_arrow = ARROW_SPACING_PX / 2.0;
            for &(a, b) in &segs {
                let (p0, p1) = (scratch_pts[a], scratch_pts[b]);
                if !arrows {
                    continue;
                }
                let dx = f32::from(p1.x - p0.x);
                let dy = f32::from(p1.y - p0.y);
                let len = (dx * dx + dy * dy).sqrt();
                since_arrow += len;
                if since_arrow < ARROW_SPACING_PX || len < ARROW_MIN_SEGMENT_PX {
                    continue;
                }
                since_arrow = 0.0;
                let half_base = half_width + ARROW_OVERHANG;
                if let Some([tip, left, right]) =
                    arrow_triangle(p0, p1, reverse, ARROW_HALF_LENGTH, half_base)
                {
                    push_triangle(&mut stroke.path, &mut stroke.bounds, tip, left, right);
                }
                if default_casing {
                    if let Some([tip, left, right]) = arrow_triangle(
                        p0,
                        p1,
                        reverse,
                        ARROW_HALF_LENGTH + 2.0 * CASING_WIDTH,
                        half_base + 1.5 * CASING_WIDTH,
                    ) {
                        push_triangle(&mut casing.path, &mut casing.bounds, tip, left, right);
                    }
                }
            }
        }
        crate::frame_stats::count("count.ways_in_view", ways_in_view);
        crate::frame_stats::count("count.ways_skipped", ways_skipped);
        crate::frame_stats::count("count.ways_collapsed", ways_collapsed);
        crate::frame_stats::lap("osm.ways_build", fs_phase);

        let mut next_hints = HashMap::new();
        let scene = WayScene {
            fills: finish_sorted(fills, &mut next_hints),
            overlays: overlays
                .into_iter()
                .filter_map(|b| b.finish(&mut next_hints))
                .collect(),
            casing: casing.finish(&mut next_hints),
            lines: {
                // Per z-index layer: casings, then strokes. Stable, so equal
                // layers keep first-seen order.
                let mut all: Vec<(i32, u8, Batch)> = styled_casings
                    .into_iter()
                    .map(|b| (b.z, 0, b))
                    .chain(strokes.into_iter().map(|b| (b.z, 1, b)))
                    .collect();
                all.sort_by_key(|&(z, order, _)| (z, order));
                all.into_iter()
                    .filter_map(|(_, _, b)| b.finish(&mut next_hints))
                    .collect()
            },
        };
        self.render.vertex_hints.replace(next_hints);
        crate::frame_stats::count("count.way_path_vertices", scene.vertex_count());
        crate::frame_stats::count("count.way_paths", scene.path_count());
        scene
    }
}
