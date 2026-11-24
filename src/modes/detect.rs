//! Detect mode: a click on the aerial imagery outlines what is under it. The
//! picture around the pointer is read from the cached imagery tiles, a region
//! of similar colour is grown from the click (`osm_gpui::detect`), and the
//! result is previewed under the pointer: a rectangle for a building, or with
//! Alt held the outline of an area such as trees. A click makes it a way.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gpui::{point, px, Context, CursorStyle, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_mode::EditMode;
use osm_gpui::edit_ops::Recording;
use osm_gpui::segment::{models, Embedding, Segmenter};
use osm_gpui::tags::Tags;

use super::preview::{blue, fill_square, line_width, red, stroke_polyline, PreviewFrame};
use crate::undo::{EditLabel, UndoableAction};
use crate::MapViewer;

/// Half the width of the picture searched around the pointer, in screen
/// pixels: anything bigger than this is not outlined.
const HALF_WINDOW: usize = 160;
/// Half the width of the picture the segmentation model looks at, in screen
/// pixels. The picture is re-made around the pointer when it strays within
/// `AI_MARGIN` of the picture's edge.
const AI_HALF: usize = 320;
const AI_MARGIN: f64 = 100.0;
/// How different a pixel's colour may be from the clicked one and still be
/// part of the same thing.
const TOLERANCE: f64 = 28.0;

/// What a detection outlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A building seen from above: the best-fitting rectangle.
    Building,
    /// An area (trees): the outline of the whole region.
    Area,
}

/// One detection: the outline in map-local screen pixels.
#[derive(Clone)]
struct Detection {
    target: Target,
    outline: Vec<(f64, f64)>,
}

/// The map view an embedding was made for: any pan or zoom invalidates it.
type ViewKey = (u64, u64, u64);

/// The model's picture of part of the map.
struct Picture {
    view: ViewKey,
    /// Where the picture's top-left pixel is on the map, in screen pixels.
    origin: (f64, f64),
    embedding: Embedding,
    /// What the model was shown.
    raster: osm_gpui::detect::Raster,
}

#[derive(Default)]
struct AiState {
    /// Bytes done and expected while the model files download.
    download: Option<(u64, u64)>,
    /// Why the model could not be fetched or loaded (cleared on re-entering
    /// the mode).
    failure: Option<String>,
    loading: bool,
    ready: bool,
    /// The view and centre of the picture being made, so it is made once.
    embedding_for: Option<(ViewKey, (i32, i32))>,
    picture: Option<Arc<Picture>>,
    /// Bumped whenever a new picture lands, to retire cached detections.
    epoch: u64,
}

/// The segmentation model and what it knows, shared with the worker threads
/// that download, load and run it.
#[derive(Clone, Default)]
pub(crate) struct DetectAi {
    state: Arc<Mutex<AiState>>,
    segmenter: Arc<Mutex<Option<Segmenter>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DetectAi {
    /// Forget an earlier failure so entering the mode tries again.
    pub(crate) fn retry(&self) {
        lock(&self.state).failure = None;
    }
}

/// What a detection was made from, so the same one is reused until the
/// pointer, the modifier or the view moves.
#[derive(Clone, PartialEq)]
struct Key {
    at: (i32, i32),
    target: Target,
    view: ViewKey,
    epoch: u64,
    /// The extra points, and whether the pointer counts as an include point.
    prompts: Vec<((i32, i32), bool)>,
    pointer_includes: bool,
}

/// A point the user put on the map to steer the model: it is part of the
/// building (`include`) or it is not.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Prompt {
    at: (f64, f64),
    include: bool,
}

/// Detect mode's gesture state: the points added so far with Shift-click
/// (include) and Cmd/Ctrl-click (exclude), the view they were placed in (they
/// are screen positions, so a pan or zoom drops them), a cache of the last
/// result and the display scale it last painted at (clicks have no window to
/// ask).
#[derive(Default)]
pub(crate) struct DetectState {
    last: RefCell<Option<(Key, Option<Detection>)>>,
    scale_factor: Cell<f32>,
    prompts: RefCell<Vec<Prompt>>,
    prompts_view: Cell<Option<ViewKey>>,
}

/// The colour of a point marked inside the building.
fn green() -> gpui::Rgba {
    gpui::rgb(0x22c55e)
}

fn target_for(alt: bool) -> Target {
    if alt {
        Target::Area
    } else {
        Target::Building
    }
}

impl MapViewer {
    /// The Detect state, while in Detect mode.
    fn detect_state(&self) -> Option<&DetectState> {
        self.mode_state.detect()
    }

    /// The outline a click at `cursor` would make now, with the picture
    /// around it read from the topmost visible imagery layer.
    fn detection_at(&self, cursor: Point<Pixels>) -> Option<Detection> {
        let state = self.detect_state()?;
        let target = target_for(self.pointer.last_modifiers.alt);
        self.forget_stale_prompts(state);
        let prompts = state.prompts.borrow().clone();
        let pointer_includes = self.pointer_includes(&prompts);
        let key = Key {
            at: (cursor.x.to_f64() as i32, cursor.y.to_f64() as i32),
            target,
            view: self.view_key(),
            epoch: lock(&self.detect_ai.state).epoch,
            prompts: prompts
                .iter()
                .map(|p| ((p.at.0 as i32, p.at.1 as i32), p.include))
                .collect(),
            pointer_includes,
        };
        if let Some((k, d)) = state.last.borrow().as_ref() {
            if *k == key {
                return d.clone();
            }
        }
        let detection = if lock(&self.detect_ai.state).ready {
            self.ai_detection(cursor, target, &prompts, pointer_includes)
        } else {
            self.detect_uncached(cursor, target, state.scale_factor.get())
        };
        *state.last.borrow_mut() = Some((key, detection.clone()));
        detection
    }

    /// Save what the model saw for a click, and what came of it, into a new
    /// folder under `dir` (set `OSM_GPUI_DETECT_DEBUG` to a folder to switch
    /// this on): the picture, each candidate mask, and the outline. So a
    /// wrong outline can be reproduced from exactly the input the app had.
    fn dump_detection(
        &self,
        dir: &std::path::Path,
        cursor: Point<Pixels>,
        detection: Option<&Detection>,
    ) {
        let Some(picture) = lock(&self.detect_ai.state).picture.clone() else {
            eprintln!("detect debug: no picture yet (model not ready)");
            return;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let out = dir.join(format!("detect-{stamp}"));
        if std::fs::create_dir_all(&out).is_err() {
            return;
        }
        let (w, h) = (picture.embedding.width, picture.embedding.height);
        let mut img = image::RgbImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let p = picture.raster.at(x, y).unwrap_or([0, 0, 0]);
                img.put_pixel(x as u32, y as u32, image::Rgb(p));
            }
        }
        let _ = img.save(out.join("picture.png"));
        let local = (
            cursor.x.to_f64() - picture.origin.0,
            cursor.y.to_f64() - picture.origin.1,
        );
        let prompt_note = self.detect_state().map_or(String::new(), |st| {
            st.prompts
                .borrow()
                .iter()
                .map(|p| {
                    format!(
                        "prompt {} at {:.1} {:.1}\n",
                        if p.include { "include" } else { "exclude" },
                        p.at.0 - picture.origin.0,
                        p.at.1 - picture.origin.1
                    )
                })
                .collect()
        });
        let mut notes = prompt_note;
        notes += &format!(
            "click (picture pixels): {:.1} {:.1}\npicture {w}x{h}\nalt: {}\n",
            local.0, local.1, self.pointer.last_modifiers.alt
        );
        let prompts: Vec<Prompt> = self
            .detect_state()
            .map_or(Vec::new(), |st| st.prompts.borrow().clone());
        let (include, exclude) =
            steering_points(&picture, &prompts, local, self.pointer_includes(&prompts));
        if let Ok(mut guard) = self.detect_ai.segmenter.try_lock() {
            if let Some(Ok(candidates)) = guard
                .as_mut()
                .map(|s| s.candidates_for(&picture.embedding, &include, &exclude))
            {
                for (k, c) in candidates.iter().enumerate() {
                    notes += &format!("candidate {k}: confidence {:.3}\n", c.confidence);
                    let mut mask = image::GrayImage::new(w as u32, h as u32);
                    for (i, &on) in c.mask.iter().enumerate() {
                        mask.put_pixel(
                            (i % w) as u32,
                            (i / w) as u32,
                            image::Luma([if on { 255 } else { 0 }]),
                        );
                    }
                    let _ = mask.save(out.join(format!("candidate{k}.png")));
                }
            }
        }
        match detection {
            Some(d) => {
                notes += &format!("outline ({} points, picture pixels):\n", d.outline.len());
                for (x, y) in &d.outline {
                    notes += &format!(
                        "  {:.1} {:.1}\n",
                        x - picture.origin.0,
                        y - picture.origin.1
                    );
                }
            }
            None => notes += "no outline\n",
        }
        let _ = std::fs::write(out.join("notes.txt"), notes);
        eprintln!("detect debug: saved to {}", out.display());
    }

    /// Drop the added points once the view has moved under them.
    fn forget_stale_prompts(&self, state: &DetectState) {
        let view = self.view_key();
        if state.prompts_view.get() != Some(view) {
            state.prompts.borrow_mut().clear();
            state.prompts_view.set(Some(view));
        }
    }

    /// Whether the pointer stands for a point inside the building: it does
    /// unless Cmd or Ctrl is held and there is already a point inside to
    /// exclude from.
    fn pointer_includes(&self, prompts: &[Prompt]) -> bool {
        let m = self.pointer.last_modifiers;
        !((m.control || m.platform) && prompts.iter().any(|p| p.include))
    }

    fn view_key(&self) -> ViewKey {
        let centre = self.viewport.center();
        (
            self.viewport.zoom_level().to_bits(),
            centre.lat.to_bits(),
            centre.lon.to_bits(),
        )
    }

    /// The outline of the object under `cursor` according to the model, from
    /// the picture made for this view. `None` while that picture is still
    /// being made or the pointer is off it.
    fn ai_detection(
        &self,
        cursor: Point<Pixels>,
        target: Target,
        prompts: &[Prompt],
        pointer_includes: bool,
    ) -> Option<Detection> {
        let picture = lock(&self.detect_ai.state).picture.clone()?;
        if picture.view != self.view_key() {
            return None;
        }
        let local = (
            cursor.x.to_f64() - picture.origin.0,
            cursor.y.to_f64() - picture.origin.1,
        );
        let (w, h) = (picture.embedding.width, picture.embedding.height);
        if local.0 < 0.0 || local.1 < 0.0 || local.0 >= w as f64 || local.1 >= h as f64 {
            return None;
        }
        // A picture being made holds the model; do not wait for it.
        let mut guard = self.detect_ai.segmenter.try_lock().ok()?;
        let segmenter = guard.as_mut()?;
        // The points steering the model, in picture pixels: those added so
        // far, and the pointer.
        let to_local = |at: (f64, f64)| (at.0 - picture.origin.0, at.1 - picture.origin.1);
        let inside = |p: &(f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < w as f64 && p.1 < h as f64;
        let mut include: Vec<(f64, f64)> = Vec::new();
        let mut exclude: Vec<(f64, f64)> = Vec::new();
        for p in prompts {
            let at = to_local(p.at);
            if inside(&at) {
                if p.include {
                    include.push(at)
                } else {
                    exclude.push(at)
                }
            }
        }
        if pointer_includes {
            include.push(local);
        } else {
            exclude.push(local);
        }
        let candidates = segmenter
            .candidates_for(&picture.embedding, &include, &exclude)
            .ok()?;
        let views: Vec<_> = candidates
            .iter()
            .map(|c| osm_gpui::detect::MaskCandidate {
                mask: &c.mask,
                confidence: c.confidence,
            })
            .collect();
        // The outline is of what holds the last point included.
        let anchor = include.last().copied().unwrap_or(local);
        let seed = (anchor.0 as usize, anchor.1 as usize);
        let outline = match target {
            Target::Building => {
                let embedding = &picture.embedding;
                let mut probe = |point: osm_gpui::detect::Pt| {
                    segmenter
                        .candidates_at(embedding, point)
                        .map(|found| {
                            found
                                .into_iter()
                                .map(|c| osm_gpui::detect::OwnedCandidate {
                                    mask: c.mask,
                                    confidence: c.confidence,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                osm_gpui::detect::building_from_candidates(
                    &views,
                    &picture.raster,
                    seed,
                    &include,
                    &mut probe,
                )?
            }
            Target::Area => osm_gpui::detect::area_from_candidates(&views, &picture.raster, seed)?,
        };
        Some(Detection {
            target,
            outline: outline
                .into_iter()
                .map(|(x, y)| (x + picture.origin.0, y + picture.origin.1))
                .collect(),
        })
    }

    /// Move the model along: fetch its files if they are missing, load them,
    /// and make a picture around the pointer for it to look at. Called as the
    /// pointer moves in Detect mode; each step runs on a worker thread and
    /// calls back here when it finishes.
    pub(crate) fn detect_prepare(&mut self, cx: &mut Context<Self>) {
        let ai = self.detect_ai.clone();
        let dir = models::dir();
        let mut st = lock(&ai.state);
        if st.failure.is_some() || st.download.is_some() || st.loading {
            return;
        }
        if !models::present(&dir) {
            st.download = Some((0, 0));
            drop(st);
            self.set_status(crate::tr!("mode.detect.downloading", percent = 0));
            let (work, status) = (ai.clone(), ai.clone());
            run_worker(
                cx,
                move || {
                    let progress_ai = work.clone();
                    let result = models::download(
                        osm_gpui::http::default_client().as_ref(),
                        &dir,
                        &mut |done, total| lock(&progress_ai.state).download = Some((done, total)),
                        &|| true,
                    );
                    let mut st = lock(&work.state);
                    st.download = None;
                    if let Err(e) = result {
                        st.failure = Some(e);
                    }
                },
                move || {
                    let st = lock(&status.state);
                    match (&st.download, &st.failure) {
                        (Some((done, total)), _) if *total > 0 => Some(
                            crate::tr!("mode.detect.downloading", percent = done * 100 / total)
                                .to_string(),
                        ),
                        (None, Some(e)) => {
                            Some(crate::tr!("mode.detect.download_failed", error = e).to_string())
                        }
                        _ => None,
                    }
                },
            );
            return;
        }
        if !st.ready {
            st.loading = true;
            drop(st);
            let work = ai.clone();
            run_worker(
                cx,
                move || {
                    let loaded = Segmenter::load(&dir);
                    let mut st = lock(&work.state);
                    st.loading = false;
                    match loaded {
                        Ok(segmenter) => {
                            *lock(&work.segmenter) = Some(segmenter);
                            st.ready = true;
                        }
                        Err(e) => st.failure = Some(e),
                    }
                },
                {
                    let status = ai.clone();
                    move || {
                        lock(&status.state)
                            .failure
                            .as_ref()
                            .map(|e| crate::tr!("mode.detect.load_failed", error = e).to_string())
                    }
                },
            );
            return;
        }
        // Ready: look at the part of the map around the pointer.
        let Some(cursor) = self.pointer.last_mouse_pos else {
            return;
        };
        let view = self.view_key();
        let covered = st.picture.as_ref().is_some_and(|p| {
            let (x, y) = (
                cursor.x.to_f64() - p.origin.0,
                cursor.y.to_f64() - p.origin.1,
            );
            let size = (2 * AI_HALF) as f64;
            p.view == view
                && (AI_MARGIN..size - AI_MARGIN).contains(&x)
                && (AI_MARGIN..size - AI_MARGIN).contains(&y)
        });
        let centre = (cursor.x.to_f64() as i32, cursor.y.to_f64() as i32);
        let reach = AI_HALF as f64 - AI_MARGIN;
        let pending = st.embedding_for.is_some_and(|(v, c)| {
            v == view
                && (f64::from(c.0) - f64::from(centre.0)).abs() < reach
                && (f64::from(c.1) - f64::from(centre.1)).abs() < reach
        });
        if covered || pending {
            return;
        }
        let scale_factor = self
            .detect_state()
            .map_or(1.0, |s| s.scale_factor.get().max(1.0));
        let Some(job) = self
            .layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|l| l.is_visible())
            .find_map(|l| l.as_imagery())
            .and_then(|i| i.raster_job(&self.viewport, scale_factor, cursor, AI_HALF))
        else {
            return;
        };
        st.embedding_for = Some((view, centre));
        drop(st);
        let work = ai.clone();
        run_worker(
            cx,
            move || {
                let raster = job();
                let embedded = lock(&work.segmenter)
                    .as_mut()
                    .map(|segmenter| segmenter.embed(&raster));
                let mut st = lock(&work.state);
                match embedded {
                    Some(Ok(embedding)) => {
                        st.picture = Some(Arc::new(Picture {
                            view,
                            origin: (
                                f64::from(centre.0) - AI_HALF as f64,
                                f64::from(centre.1) - AI_HALF as f64,
                            ),
                            embedding,
                            raster,
                        }));
                        st.epoch += 1;
                    }
                    Some(Err(e)) => st.failure = Some(e),
                    None => {}
                }
            },
            || None,
        );
    }

    fn detect_uncached(
        &self,
        cursor: Point<Pixels>,
        target: Target,
        scale_factor: f32,
    ) -> Option<Detection> {
        let scale_factor = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let imagery = self
            .layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|l| l.is_visible())
            .find_map(|l| l.as_imagery())?;
        let raster = imagery.raster_job(&self.viewport, scale_factor, cursor, HALF_WINDOW)?();
        let seed = (HALF_WINDOW, HALF_WINDOW);
        let outline = match target {
            Target::Building => {
                osm_gpui::detect::detect_rectangle(&raster, seed, TOLERANCE)?.to_vec()
            }
            Target::Area => osm_gpui::detect::detect_area(&raster, seed, TOLERANCE)?,
        };
        // Back from picture pixels to map-local screen pixels.
        let (ox, oy) = (
            cursor.x.to_f64() - HALF_WINDOW as f64,
            cursor.y.to_f64() - HALF_WINDOW as f64,
        );
        Some(Detection {
            target,
            outline: outline.into_iter().map(|(x, y)| (x + ox, y + oy)).collect(),
        })
    }

    /// Make the outline a way: four nodes and a `building=yes` way for a
    /// rectangle, or a `natural=wood` way for an area. One undo step.
    fn commit_detection(&mut self, detection: &Detection) {
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let at = |&(x, y): &(f64, f64)| {
            self.viewport
                .screen_to_geo(point(px(x as f32), px(y as f32)))
        };
        let positions: Vec<LatLon> = detection.outline.iter().map(at).collect();
        match detection.target {
            Target::Building => match <[LatLon; 4]>::try_from(positions) {
                Ok(corners) => self.commit_building_corners(layer_id, corners),
                // A U, an L or another building that is not a rectangle.
                Err(positions) => self.commit_polygon(
                    layer_id,
                    positions,
                    Tags::from([("building", "yes")]),
                    EditLabel::CreatedBuilding,
                ),
            },
            Target::Area => self.commit_polygon(
                layer_id,
                positions,
                Tags::from([("natural", "wood")]),
                EditLabel::Text(crate::tr!("mode.detect.created_area").to_string()),
            ),
        }
    }

    /// Create a node per corner and a closed way through them, with `tags`,
    /// as one undo step, and select the way.
    fn commit_polygon(
        &mut self,
        layer_id: osm_gpui::layers::LayerId,
        positions: Vec<LatLon>,
        tags: Tags,
        label: EditLabel,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let mut rec = Recording::new(layer_id, editable);
        let mut nodes: Vec<_> = positions.iter().map(|&p| rec.add_node(p)).collect();
        nodes.push(nodes[0]);
        let way_id = rec.add_way(nodes, tags);
        let way = osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id.get(),
        };
        self.undo_stack
            .push(UndoableAction::edit(layer_id, label, rec.into_ops()).selecting(vec![way]));
        self.set_selection(vec![way]);
    }
}

/// The points steering the model, in picture pixels: the ones added so far
/// that lie on the picture, and the pointer (`local`) as one more inside the
/// building, or as one outside it.
fn steering_points(
    picture: &Picture,
    prompts: &[Prompt],
    local: (f64, f64),
    pointer_includes: bool,
) -> (Vec<osm_gpui::detect::Pt>, Vec<osm_gpui::detect::Pt>) {
    let (w, h) = (
        picture.embedding.width as f64,
        picture.embedding.height as f64,
    );
    let (mut include, mut exclude) = (Vec::new(), Vec::new());
    for p in prompts {
        let at = (p.at.0 - picture.origin.0, p.at.1 - picture.origin.1);
        if at.0 >= 0.0 && at.1 >= 0.0 && at.0 < w && at.1 < h {
            if p.include {
                include.push(at);
            } else {
                exclude.push(at);
            }
        }
    }
    if pointer_includes {
        include.push(local);
    } else {
        exclude.push(local);
    }
    (include, exclude)
}

/// Run `work` on its own thread and, until it finishes, poll to show the
/// message `status` gives; when it does finish, carry on with
/// `detect_prepare` and repaint.
fn run_worker(
    cx: &mut Context<MapViewer>,
    work: impl FnOnce() + Send + 'static,
    status: impl Fn() -> Option<String> + Send + 'static,
) {
    let finished = Arc::new(AtomicBool::new(false));
    let flag = finished.clone();
    std::thread::spawn(move || {
        work();
        flag.store(true, Ordering::Release);
    });
    cx.spawn(async move |this, cx| loop {
        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
        let done = finished.load(Ordering::Acquire);
        let message = status();
        let alive = this
            .update(cx, |this, cx| {
                if let Some(message) = message {
                    this.set_status(message);
                }
                if done {
                    this.detect_prepare(cx);
                }
                cx.notify();
            })
            .is_ok();
        if done || !alive {
            break;
        }
    })
    .detach();
}

/// A click makes the previewed outline into a way; with nothing detected it
/// says why.
pub(crate) fn click(view: &mut MapViewer, screen_pt: Point<Pixels>, cx: &mut Context<MapViewer>) {
    if view.active_layer.is_none() {
        return;
    }
    let at = (screen_pt.x.to_f64(), screen_pt.y.to_f64());
    let modifiers = view.pointer.last_modifiers;
    // Shift-click marks a point inside the building and Cmd/Ctrl-click one
    // outside it; the outline is made on a plain click.
    if let Some(state) = view.detect_state() {
        let has_include = state.prompts.borrow().iter().any(|p| p.include);
        let added = if modifiers.shift {
            Some(true)
        } else if (modifiers.control || modifiers.platform) && has_include {
            Some(false)
        } else {
            None
        };
        if let Some(include) = added {
            view.forget_stale_prompts(state);
            state.prompts.borrow_mut().push(Prompt { at, include });
            cx.notify();
            return;
        }
    }
    let detection = view.detection_at(screen_pt);
    if let Some(dir) = std::env::var_os("OSM_GPUI_DETECT_DEBUG") {
        view.dump_detection(std::path::Path::new(&dir), screen_pt, detection.as_ref());
    }
    match detection {
        Some(detection) => view.commit_detection(&detection),
        None => view.set_status(crate::tr!("mode.detect.nothing_found")),
    }
    if let Some(state) = view.detect_state() {
        state.prompts.borrow_mut().clear();
    }
    cx.notify();
}

/// Escape drops the points added so far, and then leaves the mode.
pub(super) fn escape(view: &mut MapViewer) {
    if let Some(state) = view.detect_state() {
        if !state.prompts.borrow().is_empty() {
            state.prompts.borrow_mut().clear();
            return;
        }
    }
    view.enter_mode(EditMode::Select);
}

pub(super) fn cursor() -> CursorStyle {
    CursorStyle::Crosshair
}

/// The outline a click would make.
pub(super) fn paint_preview(view: &MapViewer, frame: &PreviewFrame<'_>, window: &mut Window) {
    if let Some(state) = view.detect_state() {
        state.scale_factor.set(window.scale_factor());
    }
    if let Some(state) = view.detect_state() {
        for p in state.prompts.borrow().iter() {
            let colour = if p.include { green() } else { red() };
            fill_square(window, frame.pixels(p.at), px(7.0), colour);
        }
    }
    let Some(detection) = view
        .pointer
        .last_mouse_pos
        .and_then(|c| view.detection_at(c))
    else {
        return;
    };
    let ring = detection
        .outline
        .iter()
        .chain(detection.outline.first())
        .map(|p| frame.pixels(*p));
    stroke_polyline(window, ring, line_width(), blue());
    for p in &detection.outline {
        fill_square(window, frame.pixels(*p), px(4.0), blue());
    }
}
