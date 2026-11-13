//! The 3D building pane: a docked view, next to the map, of the building
//! picked in 3D mode — its parts extruded to their heights, the buildings
//! around it, and the imagery on the ground — where parts can be pushed,
//! pulled, drawn and split (see `osm_gpui::building3d` for the pure parts).
//!
//! The scene is rebuilt from the layer whenever its data changes (so undo,
//! redo and side-panel tag edits show at once), and rendered offscreen on
//! the GPU only when something visible changed.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use glam::{Vec2, Vec3};
use gpui::{
    canvas, div, prelude::*, px, Bounds, Context, Corners, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, RenderImage, ScrollWheelEvent, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};
use osm_gpui::building3d::camera::{pick, Hit, OrbitCamera};
use osm_gpui::building3d::edit::{self, Tags, VerticalEdit};
use osm_gpui::building3d::ground::{self, Texture, TileBlock};
use osm_gpui::building3d::mesh::{self, FaceKind, MeshStyle, SolidRef};
use osm_gpui::building3d::model::{self, BuildingRoot, Pt, Scene, Solid};
use osm_gpui::building3d::{gpu, tags};
use osm_gpui::extrude::{ExtrudeKind, PlanNode, SegmentNode};
use osm_gpui::imagery_offset::ImageryOffset;
use osm_gpui::layers::{osm_layer::OsmLayer, tile_layer::TileLayer, LayerId};
use osm_gpui::osm::{MemberType, OsmMember};
use osm_gpui::selection::{DeletedFeatureSnapshot, FeatureKind, FeatureRef};
use osm_gpui::tr;

use crate::undo::{NodeMoveUndoEntry, UndoableAction};
use crate::MapViewer;

/// Tile-cache consumer name for the ground imagery.
const TILE_CONSUMER: &str = "building3d";
/// Pointer travel (px) that turns a press into a drag.
const DRAG_THRESHOLD_PX: f32 = 3.0;
pub(crate) const DEFAULT_WIDTH: f32 = 520.0;
const MIN_WIDTH: f32 = 280.0;
/// The map keeps at least this much width when the pane is widened.
const MIN_MAP_WIDTH: f32 = 240.0;
const TOOLBAR_HEIGHT: f32 = 40.0;
/// Height given to a newly drawn part.
const NEW_PART_HEIGHT_M: f64 = 3.0;
/// Deepest imagery zoom requested for the ground.
const MAX_GROUND_ZOOM: u32 = 19;

/// What a press on the 3D view does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    /// Pick parts; drag roofs, undersides and walls; drag elsewhere to orbit.
    Select,
    /// Drag out a rectangle on a roof or the ground to add a part.
    DrawPart,
    /// Click twice on a part's roof to cut it in two along that line.
    Split,
    /// Click on a part's wall to cut it in two at that height.
    SplitHeight,
}

enum Gesture {
    /// Pressed, not yet moved far enough to be a drag.
    Press {
        down: Vec2,
        hit: Option<Hit>,
        button: MouseButton,
        shift: bool,
        pan: bool,
    },
    Orbit {
        last: Vec2,
    },
    Pan {
        last: Vec2,
    },
    Vertical {
        part: usize,
        edit: VerticalEdit,
        anchor: Vec3,
        current: tags::Heights,
    },
    Wall {
        part: usize,
        segment: usize,
        start: Vec3,
        offset: (f64, f64),
        kind: ExtrudeKind,
    },
    Rect {
        base_z: f64,
        start: Pt,
        end: Pt,
        angle: f64,
    },
}

#[derive(Default)]
struct GroundState {
    source_key: Option<String>,
    block: Option<TileBlock>,
    loaded: usize,
    offset: ImageryOffset,
    texture: Option<Texture>,
    key: u64,
}

pub(crate) struct Building3dPane {
    layer_id: LayerId,
    root: BuildingRoot,
    scene: Option<Scene>,
    /// (layer edit revision, undo revision) the scene was built from.
    scene_key: (u64, u64),
    camera: OrbitCamera,
    tool: Tool,
    gesture: Option<Gesture>,
    /// What the pointer is over (for highlighting and tool previews).
    hover: Option<Hit>,
    /// First click of a Split: the part and the point on its roof.
    split_start: Option<(usize, Vec3)>,
    /// The canvas's bounds as of the last paint, in window coordinates.
    canvas_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    image: Option<Arc<RenderImage>>,
    /// What `image` was rendered from; re-rendered when this changes.
    rendered: Option<FrameKey>,
    /// Bumped by anything that changes the picture but isn't in `FrameKey`.
    version: u64,
    ground: GroundState,
    error: Option<SharedString>,
}

#[derive(Clone, PartialEq)]
struct FrameKey {
    width: u32,
    height: u32,
    version: u64,
    scene_key: (u64, u64),
    ground_key: u64,
    selected_part: Option<usize>,
}

impl Building3dPane {
    #[cfg(test)]
    pub(crate) fn root_for_tests(&self) -> BuildingRoot {
        self.root
    }

    fn touch(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    fn part(&self, i: usize) -> Option<&Solid> {
        self.scene.as_ref()?.building.parts.get(i)
    }

    fn canvas_size(&self) -> Option<Vec2> {
        self.canvas_bounds
            .get()
            .map(|b| Vec2::new(b.size.width.as_f32(), b.size.height.as_f32()))
            .filter(|s| s.x > 1.0 && s.y > 1.0)
    }

    /// Window position -> canvas-local pixels.
    fn local(&self, p: gpui::Point<Pixels>) -> Option<Vec2> {
        let b = self.canvas_bounds.get()?;
        Some(Vec2::new(
            (p.x - b.origin.x).as_f32(),
            (p.y - b.origin.y).as_f32(),
        ))
    }

    fn ray(&self, p: Vec2) -> Option<osm_gpui::building3d::camera::Ray> {
        let size = self.canvas_size()?;
        let radius = self.scene.as_ref()?.ground_radius as f32;
        Some(self.camera.ray(p, size, radius))
    }

    fn pick_at(&self, p: Vec2, tris: &[mesh::PickTri]) -> Option<Hit> {
        pick(&self.ray(p)?, tris)
    }

    /// Triangles to pick against: the scene as it stands.
    fn pick_tris(&self) -> Vec<mesh::PickTri> {
        match &self.scene {
            Some(scene) => mesh::build_mesh(scene, &MeshStyle::default(), &[]).tris,
            None => Vec::new(),
        }
    }

    fn frame_camera(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let pts: Vec<Pt> = scene
            .building
            .parts
            .iter()
            .flat_map(|p| p.ring.iter().copied())
            .collect();
        let (min, max) = model::bbox(&pts);
        let top = scene
            .building
            .parts
            .iter()
            .map(|p| p.heights.height)
            .fold(0.0, f64::max);
        let base = OrbitCamera::framing(
            Vec3::new(
                ((min.0 + max.0) / 2.0) as f32,
                ((min.1 + max.1) / 2.0) as f32,
                0.0,
            ),
            scene.building_radius as f32,
            top as f32,
        );
        // Look from a side where the neighbours don't hide the building:
        // check sightlines to its middle and its footprint's corners.
        let blockers: Vec<mesh::PickTri> = mesh::build_mesh(scene, &MeshStyle::default(), &[])
            .tris
            .into_iter()
            .filter(|t| matches!(t.face.solid, SolidRef::Context(_)))
            .collect();
        let mid_z = (top / 2.0) as f32;
        let mut look_at = vec![Vec3::new(base.target.x, base.target.y, mid_z)];
        look_at.extend(
            [
                (min.0, min.1),
                (max.0, min.1),
                (max.0, max.1),
                (min.0, max.1),
            ]
            .map(|(x, y)| Vec3::new(x as f32, y as f32, mid_z)),
        );
        self.camera = osm_gpui::building3d::camera::least_obstructed(base, &look_at, &blockers);
        self.touch();
    }

    /// Heights of every other part's top and bottom, for snapping.
    fn snap_heights(&self, except: usize) -> Vec<f64> {
        let Some(scene) = &self.scene else {
            return Vec::new();
        };
        let mut out = vec![0.0];
        for (i, p) in scene.building.parts.iter().enumerate() {
            if i != except {
                out.push(p.heights.height);
                out.push(p.heights.wall_top());
                out.push(p.heights.min_height);
            }
        }
        for c in &scene.context {
            out.push(c.heights.height);
        }
        out
    }

    /// Snapping distance in metres: a few pixels' worth at the target.
    fn snap_tolerance(&self) -> f64 {
        (self.camera.distance as f64 * 0.012).clamp(0.15, 2.0)
    }

    /// The solid as a drag in progress would leave it.
    fn preview(&self) -> Option<(usize, Solid)> {
        match self.gesture.as_ref()? {
            Gesture::Vertical { part, current, .. } => {
                let mut s = self.part(*part)?.clone();
                s.heights = current.clone();
                Some((*part, s))
            }
            Gesture::Wall {
                part,
                segment,
                offset,
                kind,
                ..
            } => {
                let solid = self.part(*part)?;
                let nodes = segment_nodes(solid, |_| true);
                let plan = osm_gpui::extrude::plan_extrude(&nodes, *segment, *offset, *kind)?;
                let mut s = solid.clone();
                s.ring = plan_ring(&plan, &nodes);
                Some((*part, s))
            }
            _ => None,
        }
    }
}

/// The ring's nodes as the extrude planner wants them, closed; `movable`
/// says which may be moved rather than duplicated.
fn segment_nodes(solid: &Solid, movable: impl Fn(i64) -> bool) -> Vec<SegmentNode> {
    let n = solid.ring.len();
    solid
        .node_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| SegmentNode {
            id,
            pos: solid.ring[i % n],
            movable: movable(id),
        })
        .collect()
}

/// The open ring an extrude plan produces.
fn plan_ring(plan: &osm_gpui::extrude::ExtrudePlan, nodes: &[SegmentNode]) -> Vec<Pt> {
    let mut ring: Vec<Pt> = plan
        .nodes
        .iter()
        .map(|n| match *n {
            PlanNode::New(i) => plan.new_nodes[i],
            PlanNode::Existing(id) => plan
                .moved
                .iter()
                .find(|m| m.0 == id)
                .map(|m| m.1)
                .or_else(|| nodes.iter().find(|n| n.id == id).map(|n| n.pos))
                .unwrap_or_default(),
        })
        .collect();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    ring
}

fn to_tags(pairs: Option<Vec<(String, String)>>) -> Tags {
    pairs.unwrap_or_default().into_iter().collect()
}

fn sorted_pairs(t: &Tags) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = t.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    v.sort();
    v
}

type TagEntry = (FeatureRef, String, Option<String>, Option<String>);

impl MapViewer {
    // --- opening and closing ------------------------------------------------

    #[cfg(test)]
    pub(crate) fn building3d_part_index_for_tests(&self, way_id: i64) -> Option<usize> {
        self.building3d
            .as_ref()?
            .scene
            .as_ref()?
            .building
            .part_index(way_id)
    }

    #[cfg(test)]
    pub(crate) fn building3d_part_count_for_tests(&self) -> usize {
        self.building3d
            .as_ref()
            .and_then(|p| p.scene.as_ref())
            .map_or(0, |s| s.building.parts.len())
    }

    /// 3D mode's map click: open (or switch) the 3D pane on the building
    /// under the cursor, selecting the building (its relation when it has
    /// one). A click on a part of the building already open selects that
    /// part instead.
    pub(crate) fn handle_building3d_map_click(
        &mut self,
        screen_pt: gpui::Point<Pixels>,
        _click_count: usize,
    ) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        let hit = osm_gpui::selection::resolve_hits(per_layer)
            .filter(|h| h.kind == FeatureKind::Way)
            .or_else(|| {
                // Inside a building, not just on its outline.
                let interior = self
                    .layer_manager
                    .hit_test_interior_all(&self.viewport, screen_pt);
                osm_gpui::selection::resolve_hits(interior)
            })
            .filter(|h| h.kind == FeatureKind::Way);
        let Some(hit) = hit else {
            self.set_status(tr!("building3d.status.click_a_building"));
            return;
        };
        let root = self
            .layer_manager
            .find_layer(hit.layer_id)
            .and_then(|l| l.as_any().downcast_ref::<OsmLayer>())
            .and_then(|l| l.get_osm_data())
            .and_then(|data| model::resolve_root(&data, hit.id));
        let Some(root) = root else {
            self.set_status(tr!("building3d.status.not_a_building"));
            return;
        };
        // Already open on this building: a click on one of its parts picks it.
        if let Some(pane) = &self.building3d {
            if pane.layer_id == hit.layer_id
                && pane.root == root
                && pane.scene.as_ref().is_some_and(|s| {
                    s.building
                        .part_index(hit.id)
                        .is_some_and(|i| !s.building.parts[i].is_outline)
                })
            {
                self.set_selection(vec![hit]);
                return;
            }
        }
        self.open_building3d(hit.layer_id, root);
    }

    pub(crate) fn open_building3d(&mut self, layer_id: LayerId, root: BuildingRoot) {
        let scene = self.build_building3d_scene(layer_id, root);
        let Some(scene) = scene else {
            self.set_status(tr!("building3d.status.not_a_building"));
            return;
        };
        let reuse_bounds = self
            .building3d
            .as_ref()
            .map(|p| p.canvas_bounds.clone())
            .unwrap_or_default();
        let old_image = self.building3d.as_mut().and_then(|p| p.image.take());
        if let Some(image) = old_image {
            self.building3d_stale_images.push(image);
        }
        let mut pane = Building3dPane {
            layer_id,
            root,
            scene_key: self.building3d_scene_key(layer_id),
            scene: Some(scene),
            camera: OrbitCamera::framing(Vec3::ZERO, 10.0, 0.0),
            tool: Tool::Select,
            gesture: None,
            hover: None,
            split_start: None,
            canvas_bounds: reuse_bounds,
            image: None,
            rendered: None,
            version: 0,
            ground: GroundState::default(),
            error: None,
        };
        pane.frame_camera();
        self.building3d = Some(pane);
        self.select_building3d_root();
    }

    /// Select the open building as a whole.
    fn select_building3d_root(&mut self) {
        let Some(pane) = &self.building3d else {
            return;
        };
        let feature = match pane.root {
            BuildingRoot::Relation(id) => FeatureRef {
                layer_id: pane.layer_id,
                kind: FeatureKind::Relation,
                id,
            },
            BuildingRoot::Way(id) => FeatureRef {
                layer_id: pane.layer_id,
                kind: FeatureKind::Way,
                id,
            },
        };
        self.set_selection(vec![feature]);
    }

    pub(crate) fn close_building3d(&mut self, _window: &mut Window) {
        if let Some(mut pane) = self.building3d.take() {
            if let Some(image) = pane.image.take() {
                self.building3d_stale_images.push(image);
            }
            osm_gpui::tile_cache::clear_extra_wanted_tiles(TILE_CONSUMER);
        }
    }

    /// Escape in 3D mode: cancel a gesture or tool, then close the pane.
    /// `false` when there was nothing to back out of (leave the mode).
    pub(crate) fn building3d_escape(&mut self, window: &mut Window) -> bool {
        let Some(pane) = self.building3d.as_mut() else {
            return false;
        };
        if pane.gesture.take().is_some() || pane.split_start.take().is_some() {
            pane.touch();
            return true;
        }
        if pane.tool != Tool::Select {
            pane.tool = Tool::Select;
            pane.touch();
            return true;
        }
        self.close_building3d(window);
        true
    }

    fn building3d_scene_key(&self, layer_id: LayerId) -> (u64, u64) {
        let layer_rev = self
            .layer_manager
            .find_layer(layer_id)
            .map_or(0, |l| l.edit_revision());
        (layer_rev, self.undo_stack.revision)
    }

    /// Build the scene for `root` from the layer's current data. The data
    /// snapshot is dropped before returning so later edits don't have to
    /// copy it.
    fn build_building3d_scene(&self, layer_id: LayerId, root: BuildingRoot) -> Option<Scene> {
        let data = self
            .layer_manager
            .find_layer(layer_id)?
            .as_any()
            .downcast_ref::<OsmLayer>()?
            .get_osm_data()?;
        model::build_scene(&data, root)
    }

    /// Width the pane takes from the map, 0 when closed.
    pub(crate) fn building3d_width(&self) -> f32 {
        if self.building3d.is_some() {
            self.building3d_pane_width
        } else {
            0.0
        }
    }

    // --- per-frame sync -----------------------------------------------------

    /// Called every render: rebuild the scene when the data changed, keep the
    /// ground imagery loaded, and re-render the 3D image when anything
    /// visible changed.
    pub(crate) fn building3d_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for image in self.building3d_stale_images.drain(..) {
            let _ = window.drop_image(image);
        }
        let Some(layer_id) = self.building3d.as_ref().map(|p| p.layer_id) else {
            return;
        };
        // Rebuild on data changes.
        let key = self.building3d_scene_key(layer_id);
        if self.building3d.as_ref().is_some_and(|p| p.scene_key != key) {
            let root = self.building3d.as_ref().unwrap().root;
            match self.build_building3d_scene(layer_id, root) {
                Some(scene) => {
                    let pane = self.building3d.as_mut().unwrap();
                    pane.scene = Some(scene);
                    pane.scene_key = key;
                    pane.gesture = None;
                    pane.hover = None;
                    pane.split_start = None;
                }
                None => {
                    // The building is gone (deleted, or its creation undone).
                    self.close_building3d(window);
                    self.set_status(tr!("building3d.status.gone"));
                    return;
                }
            }
        }
        self.building3d_sync_ground(window, cx);
        self.building3d_render_image(window);
    }

    /// Fetch the topmost visible imagery layer's tiles under the scene and
    /// stitch them into the ground texture as they arrive.
    fn building3d_sync_ground(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let imagery = self
            .layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|l| l.is_visible())
            .find_map(|l| {
                let tiles = l.as_any().downcast_ref::<TileLayer>()?;
                Some((
                    tiles.url_template().to_string(),
                    tiles.source_key().to_string(),
                    tiles.max_zoom(),
                    l.imagery_offset().unwrap_or(ImageryOffset::ZERO),
                ))
            });
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let Some(scene) = &pane.scene else {
            return;
        };
        let Some((template, source_key, max_zoom, offset)) = imagery else {
            if pane.ground.source_key.take().is_some() || pane.ground.texture.is_some() {
                pane.ground = GroundState::default();
                osm_gpui::tile_cache::clear_extra_wanted_tiles(TILE_CONSUMER);
            }
            return;
        };
        let pts: Vec<Pt> = scene
            .building
            .parts
            .iter()
            .flat_map(|p| p.ring.iter().copied())
            .collect();
        let (min, max) = model::bbox(&pts);
        let center = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
        let block = ground::tile_block(
            &scene.frame,
            center,
            scene.ground_radius,
            max_zoom.unwrap_or(MAX_GROUND_ZOOM).min(MAX_GROUND_ZOOM),
        );
        let urls: Vec<(osm_gpui::tiles::TileCoord, String)> = block
            .tiles()
            .into_iter()
            .map(|t| (t, osm_gpui::tiles::url_from_template(&template, &t)))
            .collect();
        let source_changed = pane.ground.source_key.as_deref() != Some(source_key.as_str())
            || pane.ground.block != Some(block);
        if source_changed {
            let wanted: HashSet<String> = urls.iter().map(|(_, u)| u.clone()).collect();
            osm_gpui::tile_cache::clear_extra_wanted_tiles(TILE_CONSUMER);
            for key in
                osm_gpui::tile_cache::set_extra_wanted_tiles(TILE_CONSUMER, &source_key, wanted)
            {
                cx.remove_asset::<osm_gpui::tile_cache::TileAsset>(&key);
            }
        }
        let mut images: HashMap<osm_gpui::tiles::TileCoord, Arc<RenderImage>> = HashMap::new();
        for (coord, url) in &urls {
            let key = osm_gpui::tile_cache::TileAssetKey {
                url: url.clone(),
                source_key: source_key.clone(),
            };
            if let Some(Ok(image)) = window.use_asset::<osm_gpui::tile_cache::TileAsset>(&key, cx) {
                images.insert(*coord, image);
            }
        }
        let pane = self.building3d.as_mut().unwrap();
        let loaded = images.len();
        if !source_changed && loaded == pane.ground.loaded && pane.ground.offset == offset {
            return;
        }
        let texture = ground::stitch(&block, |c| {
            images.get(&c).and_then(|img| {
                let size = img.size(0);
                Some((img.as_bytes(0)?, size.width.0 as u32, size.height.0 as u32))
            })
        });
        pane.ground = GroundState {
            source_key: Some(source_key),
            block: Some(block),
            loaded,
            offset,
            texture: Some(texture),
            key: pane.ground.key.wrapping_add(1),
        };
    }

    fn building3d_selected_part(&self) -> Option<usize> {
        let pane = self.building3d.as_ref()?;
        let [only] = self.selected.as_slice() else {
            return None;
        };
        if only.layer_id != pane.layer_id || only.kind != FeatureKind::Way {
            return None;
        }
        let i = pane.scene.as_ref()?.building.part_index(only.id)?;
        // The outline standing in for the parts is "the building", not a
        // part.
        (!pane.part(i)?.is_outline).then_some(i)
    }

    fn building3d_render_image(&mut self, window: &mut Window) {
        let selected_part = self.building3d_selected_part();
        let scale = window.scale_factor();
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let Some(size) = pane.canvas_size() else {
            return;
        };
        let (width, height) = (
            (size.x * scale).round() as u32,
            (size.y * scale).round() as u32,
        );
        let key = FrameKey {
            width,
            height,
            version: pane.version,
            scene_key: pane.scene_key,
            ground_key: pane.ground.key,
            selected_part,
        };
        if pane.rendered.as_ref() == Some(&key) {
            return;
        }
        let Some(scene) = &pane.scene else {
            return;
        };
        let renderer = self
            .building3d_renderer
            .get_or_insert_with(gpu::Renderer::new);
        let renderer = match renderer {
            Ok(r) => r,
            Err(e) => {
                pane.error = Some(tr!("building3d.no_gpu", error = e.clone()));
                pane.rendered = Some(key);
                return;
            }
        };

        // While a drag is underway the grabbed solid follows it; outside a
        // drag the face under the cursor is highlighted.
        let preview = pane.preview();
        let style = MeshStyle {
            selected_part: preview.as_ref().map(|(i, _)| *i).or(selected_part),
            hovered: if pane.gesture.is_none() {
                match pane.hover {
                    Some(Hit::Face { face, .. }) if matches!(face.solid, SolidRef::Part(_)) => {
                        Some(face)
                    }
                    _ => None,
                }
            } else {
                None
            },
        };
        let overrides: Vec<(usize, Solid)> = preview.into_iter().collect();
        let mut m = mesh::build_mesh(scene, &style, &overrides);
        building3d_guides(pane, scene, &mut m);

        let corners = match (&pane.ground.block, &pane.ground.texture) {
            (Some(block), Some(_)) => block.corners(&scene.frame, pane.ground.offset),
            _ => {
                let r = scene.ground_radius;
                let c = pane.camera.target;
                let (x, y) = (c.x as f64, c.y as f64);
                [
                    (x - r, y + r),
                    (x + r, y + r),
                    (x + r, y - r),
                    (x - r, y - r),
                ]
            }
        };
        let aspect = size.x / size.y;
        let radius = scene.ground_radius as f32;
        let input = gpu::FrameInput {
            width,
            height,
            view_proj: pane.camera.view_proj(aspect, radius),
            eye: pane.camera.eye(),
            mesh: &m,
            ground: gpu::GroundInput {
                key: pane.ground.key,
                texture: pane.ground.texture.as_ref(),
                corners,
            },
        };
        match renderer.render(&input) {
            Ok(frame) => {
                let Some(buffer) =
                    image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                else {
                    return;
                };
                let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                    buffer
                )]));
                if let Some(old) = pane.image.replace(image) {
                    self.building3d_stale_images.push(old);
                }
                pane.error = None;
            }
            Err(e) => pane.error = Some(tr!("building3d.no_gpu", error = e)),
        }
        pane.rendered = Some(key);
    }

    // --- input --------------------------------------------------------------

    fn building3d_mouse_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let tris = match &self.building3d {
            Some(p) => p.pick_tris(),
            None => return,
        };
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let Some(p) = pane.local(ev.position) else {
            return;
        };
        let hit = pane.pick_at(p, &tris);
        pane.gesture = Some(Gesture::Press {
            down: p,
            hit,
            button: ev.button,
            shift: ev.modifiers.shift,
            pan: ev.button != MouseButton::Left || ev.modifiers.platform || ev.modifiers.alt,
        });
        cx.notify();
    }

    /// Pointer movement over the pane, or anywhere while a gesture is on.
    fn building3d_mouse_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let alt = ev.modifiers.alt;
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let Some(p) = pane.local(ev.position) else {
            return;
        };
        match pane.gesture.take() {
            None => {
                // Hover: highlight and tool previews.
                let tris = pane.pick_tris();
                let hit = pane.pick_at(p, &tris);
                if hit != pane.hover {
                    pane.hover = hit;
                    pane.touch();
                    cx.notify();
                }
            }
            Some(Gesture::Press {
                down,
                hit,
                button,
                shift,
                pan,
            }) => {
                if (p - down).length() < DRAG_THRESHOLD_PX {
                    pane.gesture = Some(Gesture::Press {
                        down,
                        hit,
                        button,
                        shift,
                        pan,
                    });
                    return;
                }
                pane.gesture = Some(start_drag(pane, down, hit, shift, pan));
                pane.hover = None;
                // Apply this first movement too.
                drag_to(pane, p, alt);
                pane.touch();
                cx.notify();
            }
            Some(g) => {
                pane.gesture = Some(g);
                drag_to(pane, p, alt);
                pane.touch();
                cx.notify();
            }
        }
    }

    fn building3d_mouse_up(&mut self, ev: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let Some(gesture) = pane.gesture.take() else {
            return;
        };
        pane.touch();
        match gesture {
            Gesture::Press { hit, button, .. } => {
                if button == MouseButton::Left {
                    self.building3d_click(hit, ev.click_count);
                }
            }
            Gesture::Orbit { .. } | Gesture::Pan { .. } => {}
            Gesture::Vertical { part, current, .. } => {
                self.building3d_commit_heights(part, current);
            }
            Gesture::Wall {
                part,
                segment,
                offset,
                kind,
                ..
            } => self.building3d_commit_wall(part, segment, offset, kind),
            Gesture::Rect {
                base_z,
                start,
                end,
                angle,
            } => {
                let corners = edit::rectangle(start, end, angle);
                if model::signed_area(&corners).abs() >= 0.5 {
                    self.building3d_commit_new_part(corners, base_z);
                }
                if let Some(pane) = self.building3d.as_mut() {
                    pane.tool = Tool::Select;
                }
            }
        }
        cx.notify();
    }

    fn building3d_scroll(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let dy = ev.delta.pixel_delta(px(16.0)).y.as_f32();
        pane.camera.zoom((-dy * 0.004).exp());
        pane.touch();
        cx.notify();
    }

    /// A click (press and release without a drag) on the 3D view.
    fn building3d_click(&mut self, hit: Option<Hit>, click_count: usize) {
        let Some(pane) = self.building3d.as_mut() else {
            return;
        };
        let tool = pane.tool;
        let part_face = |hit: &Option<Hit>| match hit {
            Some(Hit::Face { face, point }) => match face.solid {
                SolidRef::Part(i) => Some((i, face.kind, *point)),
                _ => None,
            },
            _ => None,
        };
        match tool {
            Tool::Select | Tool::DrawPart => match hit {
                Some(Hit::Face { face, .. }) => match face.solid {
                    SolidRef::Part(i) => {
                        let Some(solid) = pane.part(i) else {
                            return;
                        };
                        let (way, is_outline) = (solid.way_id, solid.is_outline);
                        let layer_id = pane.layer_id;
                        if is_outline {
                            self.select_building3d_root();
                        } else {
                            self.set_selection(vec![FeatureRef {
                                layer_id,
                                kind: FeatureKind::Way,
                                id: way,
                            }]);
                        }
                    }
                    SolidRef::Context(i) => {
                        let Some(way) = pane
                            .scene
                            .as_ref()
                            .and_then(|s| s.context.get(i))
                            .map(|s| s.way_id)
                        else {
                            return;
                        };
                        if click_count >= 2 {
                            // Double-click a neighbour: model that one.
                            let layer_id = pane.layer_id;
                            let root = self
                                .layer_manager
                                .find_layer(layer_id)
                                .and_then(|l| l.as_any().downcast_ref::<OsmLayer>())
                                .and_then(|l| l.get_osm_data())
                                .and_then(|d| model::resolve_root(&d, way));
                            if let Some(root) = root {
                                self.open_building3d(layer_id, root);
                            }
                        } else {
                            self.set_status(tr!("building3d.status.neighbour"));
                        }
                    }
                },
                _ => self.select_building3d_root(),
            },
            Tool::Split => {
                match pane.split_start {
                    None => {
                        let Some((i, FaceKind::Roof, point)) = part_face(&hit) else {
                            self.set_status(tr!("building3d.status.split_click_roof"));
                            return;
                        };
                        pane.split_start = Some((i, point));
                    }
                    Some((part, start)) => {
                        // The cut line runs over the roof plane of the first
                        // click.
                        let end = match hit {
                            Some(h) => h.point(),
                            None => return,
                        };
                        pane.split_start = None;
                        pane.tool = Tool::Select;
                        self.building3d_commit_split_line(
                            part,
                            (start.x as f64, start.y as f64),
                            (end.x as f64, end.y as f64),
                        );
                    }
                }
            }
            Tool::SplitHeight => {
                let Some((i, FaceKind::Wall(_), point)) = part_face(&hit) else {
                    self.set_status(tr!("building3d.status.split_height_click_wall"));
                    return;
                };
                let at = edit::snap_height(
                    point.z as f64,
                    &pane.snap_heights(i),
                    pane.snap_tolerance(),
                    true,
                );
                pane.tool = Tool::Select;
                self.building3d_commit_split_height(i, at);
            }
        }
    }

    // --- committing edits -----------------------------------------------------

    fn building3d_context(&self) -> Option<(LayerId, BuildingRoot, Scene)> {
        let pane = self.building3d.as_ref()?;
        Some((pane.layer_id, pane.root, pane.scene.clone()?))
    }

    fn way_tags(&self, layer_id: LayerId, way_id: i64) -> Tags {
        to_tags(
            self.layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
                .and_then(|e| {
                    e.feature_tags(&FeatureRef {
                        layer_id,
                        kind: FeatureKind::Way,
                        id: way_id,
                    })
                }),
        )
    }

    /// Apply tag rewrites to existing ways, returning the undo entries.
    fn building3d_apply_tags(
        &mut self,
        layer_id: LayerId,
        changes: &[(i64, Tags)],
    ) -> Vec<TagEntry> {
        let mut entries: Vec<TagEntry> = Vec::new();
        for (way, after) in changes {
            let before = self.way_tags(layer_id, *way);
            let feature = FeatureRef {
                layer_id,
                kind: FeatureKind::Way,
                id: *way,
            };
            for (k, b, a) in edit::diff_tags(&before, after) {
                entries.push((feature, k, b, a));
            }
        }
        if entries.is_empty() {
            return entries;
        }
        let indexed = self.current_tag_index_layers();
        if let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        {
            for (f, k, _, after) in &entries {
                match after {
                    Some(v) => editable.set_tag(f.kind, f.id, k, v),
                    None => editable.remove_tag(f.kind, f.id, k),
                }
            }
        }
        self.note_tag_changes(&indexed, &entries);
        entries
    }

    /// Add `new_parts` to the building relation (role `part`), if the
    /// building is one.
    fn building3d_add_to_relation(
        &mut self,
        layer_id: LayerId,
        root: BuildingRoot,
        new_parts: &[i64],
        actions: &mut Vec<UndoableAction>,
    ) {
        let BuildingRoot::Relation(rel_id) = root else {
            return;
        };
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let Some(rel) = editable.relation(rel_id) else {
            return;
        };
        let before = rel.members.clone();
        let mut after = before.clone();
        for &id in new_parts {
            after.push(OsmMember {
                member_type: MemberType::Way,
                reference: id,
                role: "part".to_string(),
            });
        }
        editable.set_relation_members(rel_id, after.clone());
        actions.push(UndoableAction::SetRelationMembers {
            layer: layer_id,
            relation_id: rel_id,
            before,
            after,
        });
    }

    /// Create a closed way over existing `nodes` with `tags`.
    fn building3d_create_way(
        &mut self,
        layer_id: LayerId,
        nodes: Vec<i64>,
        tags: &Tags,
        actions: &mut Vec<UndoableAction>,
    ) -> Option<i64> {
        let editable = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())?;
        let pairs = sorted_pairs(tags);
        let id = editable.add_way(nodes.clone(), pairs.clone());
        actions.push(UndoableAction::CreateWay {
            layer: layer_id,
            snapshot: DeletedFeatureSnapshot {
                kind: FeatureKind::Way,
                id,
                tags: pairs,
                way_nodes: nodes,
                node_lat_lon: None,
                version: 0,
            },
        });
        Some(id)
    }

    fn building3d_create_node(
        &mut self,
        layer_id: LayerId,
        (lat, lon): (f64, f64),
        actions: &mut Vec<UndoableAction>,
    ) -> Option<i64> {
        let editable = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())?;
        let id = editable.add_node(lat, lon);
        actions.push(UndoableAction::CreateNode {
            layer: layer_id,
            id,
            lat,
            lon,
        });
        Some(id)
    }

    fn building3d_push(
        &mut self,
        label: SharedString,
        actions: Vec<UndoableAction>,
        tag_entries: Vec<TagEntry>,
    ) {
        let mut actions = actions;
        if !tag_entries.is_empty() {
            actions.push(UndoableAction::SetTags {
                entries: tag_entries,
            });
        }
        if actions.is_empty() {
            return;
        }
        self.undo_stack.push(UndoableAction::Batch {
            label: label.to_string(),
            actions,
        });
    }

    /// Select way `id` (a part) of `layer_id`.
    fn building3d_select_way(&mut self, layer_id: LayerId, id: i64) {
        self.set_selection(vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id,
        }]);
    }

    pub(crate) fn building3d_commit_heights(&mut self, part: usize, new: tags::Heights) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, _, scene)) = self.building3d_context() else {
            return;
        };
        let Some(solid) = scene.building.parts.get(part) else {
            return;
        };
        if solid.heights == new {
            return;
        }
        let before = self.way_tags(layer_id, solid.way_id);
        let after = edit::retag_heights(&before, &solid.heights, &new);
        let entries = self.building3d_apply_tags(layer_id, &[(solid.way_id, after)]);
        self.building3d_push(tr!("building3d.history.height"), Vec::new(), entries);
        if !solid.is_outline {
            self.building3d_select_way(layer_id, solid.way_id);
        }
        self.set_status(tr!(
            "building3d.status.height_set",
            min = tags::format_height(new.min_height),
            max = tags::format_height(new.height)
        ));
    }

    pub(crate) fn building3d_commit_wall(
        &mut self,
        part: usize,
        segment: usize,
        offset: (f64, f64),
        kind: ExtrudeKind,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, _, scene)) = self.building3d_context() else {
            return;
        };
        let Some(solid) = scene.building.parts.get(part).cloned() else {
            return;
        };
        let movable = {
            let Some(editable) = self
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
            else {
                return;
            };
            let way_id = solid.way_id;
            move |id: i64| {
                editable.ways_using_node(id).iter().all(|&w| w == way_id)
                    && editable.relations_containing_node(id).is_empty()
                    && editable
                        .feature_tags(&FeatureRef {
                            layer_id,
                            kind: FeatureKind::Node,
                            id,
                        })
                        .is_none_or(|t| t.is_empty())
            }
        };
        let nodes = segment_nodes(&solid, movable);
        let Some(plan) = osm_gpui::extrude::plan_extrude(&nodes, segment, offset, kind) else {
            return;
        };
        if !plan.new_nodes.is_empty()
            && self.refuse_way_growth(layer_id, solid.way_id, plan.new_nodes.len())
        {
            return;
        }
        let frame = scene.frame;
        let mut actions = Vec::new();
        let mut new_ids = Vec::new();
        for &p in &plan.new_nodes {
            let Some(id) = self.building3d_create_node(layer_id, frame.unproject(p), &mut actions)
            else {
                return;
            };
            new_ids.push(id);
        }
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let after: Vec<i64> = plan
            .nodes
            .iter()
            .map(|n| match *n {
                PlanNode::Existing(id) => id,
                PlanNode::New(i) => new_ids[i],
            })
            .collect();
        if after != solid.node_ids {
            editable.set_way_nodes(solid.way_id, after.clone());
            actions.push(UndoableAction::SetWayNodes {
                layer: layer_id,
                way_id: solid.way_id,
                before: solid.node_ids.clone(),
                after,
            });
        }
        if !plan.moved.is_empty() {
            let mut entries: Vec<NodeMoveUndoEntry> = Vec::new();
            let mut moves = Vec::new();
            for &(id, p) in &plan.moved {
                let (lat, lon) = frame.unproject(p);
                if let Some(old) = editable.node_lat_lon(id) {
                    entries.push((id, old, (lat, lon)));
                    moves.push((id, lat, lon));
                }
            }
            editable.commit_node_moves(&moves);
            actions.push(UndoableAction::MoveNodes {
                select: None,
                per_layer: vec![(layer_id, entries)],
            });
        }
        self.building3d_push(tr!("building3d.history.wall"), actions, Vec::new());
        if !solid.is_outline {
            self.building3d_select_way(layer_id, solid.way_id);
        }
    }

    /// Insert each new node into every way (other than `skip`) that has the
    /// segment it was cut into, so neighbouring parts and the outline stay
    /// joined where they share that wall.
    fn building3d_insert_on_shared_edges(
        &mut self,
        layer_id: LayerId,
        inserts: &[(i64, i64, i64)],
        skip: Option<i64>,
        actions: &mut Vec<UndoableAction>,
    ) {
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let mut changed: Vec<(i64, Vec<i64>, Vec<i64>)> = Vec::new();
        for &(u, v, new_id) in inserts {
            let ways: Vec<i64> = editable
                .ways_using_node(u)
                .into_iter()
                .filter(|w| Some(*w) != skip && editable.ways_using_node(v).contains(w))
                .collect();
            for w in ways {
                let idx = match changed.iter().position(|c| c.0 == w) {
                    Some(i) => i,
                    None => {
                        let Some(nodes) = editable.way_node_ids(w) else {
                            continue;
                        };
                        changed.push((w, nodes.clone(), nodes));
                        changed.len() - 1
                    }
                };
                let list = &mut changed[idx].2;
                if let Some(pos) = list
                    .windows(2)
                    .position(|p| (p[0] == u && p[1] == v) || (p[0] == v && p[1] == u))
                {
                    list.insert(pos + 1, new_id);
                }
            }
        }
        for (w, before, after) in changed {
            if before != after {
                editable.set_way_nodes(w, after.clone());
                actions.push(UndoableAction::SetWayNodes {
                    layer: layer_id,
                    way_id: w,
                    before,
                    after,
                });
            }
        }
    }

    pub(crate) fn building3d_commit_split_line(&mut self, part: usize, p: Pt, q: Pt) {
        use osm_gpui::building3d::edit::{RingVertex, SplitError};
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, root, scene)) = self.building3d_context() else {
            return;
        };
        let Some(solid) = scene.building.parts.get(part).cloned() else {
            return;
        };
        let split = match edit::split_ring(&solid.ring, p, q) {
            Ok(s) => s,
            Err(SplitError::NoCrossing) => {
                self.set_status(tr!("building3d.status.split_misses"));
                return;
            }
            Err(SplitError::TooManyCrossings) => {
                self.set_status(tr!("building3d.status.split_too_many"));
                return;
            }
        };
        let mut actions = Vec::new();
        let mut new_ids = Vec::new();
        let mut inserts = Vec::new();
        for &(pt, seg) in &split.new_points {
            let Some(id) =
                self.building3d_create_node(layer_id, scene.frame.unproject(pt), &mut actions)
            else {
                return;
            };
            new_ids.push(id);
            inserts.push((solid.node_ids[seg], solid.node_ids[seg + 1], id));
        }
        let ids = |piece: &[RingVertex]| -> Vec<i64> {
            let mut v: Vec<i64> = piece
                .iter()
                .map(|r| match *r {
                    RingVertex::Existing(i) => solid.node_ids[i],
                    RingVertex::New(k) => new_ids[k],
                })
                .collect();
            v.push(v[0]);
            v
        };
        let (a, b) = (ids(&split.a), ids(&split.b));
        let tags_before = self.way_tags(layer_id, solid.way_id);
        let mut created = Vec::new();
        let selected = if solid.is_outline {
            // A building without parts: the outline gets the cut nodes and
            // the two halves become its first parts.
            self.building3d_insert_on_shared_edges(layer_id, &inserts, None, &mut actions);
            let part_tags = edit::part_tags_from(&tags_before, true);
            for nodes in [a, b] {
                if let Some(id) =
                    self.building3d_create_way(layer_id, nodes, &part_tags, &mut actions)
                {
                    created.push(id);
                }
            }
            created.first().copied()
        } else {
            self.building3d_insert_on_shared_edges(
                layer_id,
                &inserts,
                Some(solid.way_id),
                &mut actions,
            );
            if let Some(editable) = self
                .layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
            {
                editable.set_way_nodes(solid.way_id, a.clone());
            }
            actions.push(UndoableAction::SetWayNodes {
                layer: layer_id,
                way_id: solid.way_id,
                before: solid.node_ids.clone(),
                after: a,
            });
            if let Some(id) = self.building3d_create_way(layer_id, b, &tags_before, &mut actions) {
                created.push(id);
            }
            Some(solid.way_id)
        };
        self.building3d_add_to_relation(layer_id, root, &created, &mut actions);
        self.building3d_push(tr!("building3d.history.split"), actions, Vec::new());
        if let Some(id) = selected {
            self.building3d_select_way(layer_id, id);
        }
    }

    pub(crate) fn building3d_commit_split_height(&mut self, part: usize, at: f64) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, root, scene)) = self.building3d_context() else {
            return;
        };
        let Some(solid) = scene.building.parts.get(part).cloned() else {
            return;
        };
        let tags_before = self.way_tags(layer_id, solid.way_id);
        let base = edit::part_tags_from(&tags_before, solid.is_outline);
        let Some((lower, upper)) = edit::split_at_height(&base, &solid.heights, at) else {
            self.set_status(tr!("building3d.status.split_height_outside"));
            return;
        };
        let mut actions = Vec::new();
        let mut entries = Vec::new();
        let mut created = Vec::new();
        if solid.is_outline {
            for t in [&lower, &upper] {
                if let Some(id) =
                    self.building3d_create_way(layer_id, solid.node_ids.clone(), t, &mut actions)
                {
                    created.push(id);
                }
            }
        } else {
            entries = self.building3d_apply_tags(layer_id, &[(solid.way_id, lower)]);
            if let Some(id) =
                self.building3d_create_way(layer_id, solid.node_ids.clone(), &upper, &mut actions)
            {
                created.push(id);
            }
        }
        self.building3d_add_to_relation(layer_id, root, &created, &mut actions);
        self.building3d_push(tr!("building3d.history.split_height"), actions, entries);
        if let Some(&id) = created.last() {
            self.building3d_select_way(layer_id, id);
        }
    }

    pub(crate) fn building3d_commit_new_part(&mut self, corners: [Pt; 4], base_z: f64) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, root, scene)) = self.building3d_context() else {
            return;
        };
        let mut actions = Vec::new();
        let mut created = Vec::new();
        // The first part of a building without parts: the rest of the
        // building becomes a part too, or it would stop being drawn.
        if !scene.building.has_parts() {
            if let Some(outline) = &scene.building.outline {
                let base = edit::part_tags_from(&self.way_tags(layer_id, outline.way_id), true);
                if let Some(id) = self.building3d_create_way(
                    layer_id,
                    outline.node_ids.clone(),
                    &base,
                    &mut actions,
                ) {
                    created.push(id);
                }
            }
        }
        let mut nodes = Vec::new();
        for c in corners {
            let Some(id) =
                self.building3d_create_node(layer_id, scene.frame.unproject(c), &mut actions)
            else {
                return;
            };
            nodes.push(id);
        }
        nodes.push(nodes[0]);
        let mut t: Tags = Tags::new();
        t.insert("building:part".into(), "yes".into());
        t.insert(
            "height".into(),
            tags::format_height(base_z + NEW_PART_HEIGHT_M),
        );
        if base_z > 0.05 {
            t.insert("min_height".into(), tags::format_height(base_z));
        }
        let Some(new_id) = self.building3d_create_way(layer_id, nodes, &t, &mut actions) else {
            return;
        };
        created.push(new_id);
        self.building3d_add_to_relation(layer_id, root, &created, &mut actions);
        self.building3d_push(tr!("building3d.history.new_part"), actions, Vec::new());
        self.building3d_select_way(layer_id, new_id);
        self.set_status(tr!("building3d.status.part_added"));
    }

    // --- pane resize ----------------------------------------------------------

    fn start_building3d_resize(&mut self, x: f32) {
        self.building3d_resize = Some((x, self.building3d_pane_width));
    }

    pub(crate) fn building3d_resizing(&self) -> bool {
        self.building3d_resize.is_some()
    }

    pub(crate) fn drag_building3d_resize(
        &mut self,
        ev: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_x, start_w)) = self.building3d_resize else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.building3d_resize = None;
            return;
        }
        let max = (window.viewport_size().width.as_f32()
            - self.side_panel_width
            - Self::MODE_PANEL_WIDTH
            - MIN_MAP_WIDTH)
            .max(MIN_WIDTH);
        // The pane sits right of the map: dragging its left edge left widens it.
        self.building3d_pane_width =
            (start_w + start_x - ev.position.x.as_f32()).clamp(MIN_WIDTH, max);
        if let Some(pane) = self.building3d.as_mut() {
            pane.touch();
        }
        cx.notify();
    }

    pub(crate) fn finish_building3d_resize(&mut self) {
        self.building3d_resize = None;
    }

    pub(crate) fn building3d_gesture_active(&self) -> bool {
        self.building3d
            .as_ref()
            .is_some_and(|p| p.gesture.is_some())
    }

    /// Root-level handlers while a 3D drag is underway, so it keeps
    /// tracking the pointer beyond the pane.
    pub(crate) fn building3d_root_handlers(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        // Over the pane its own handlers already have the event (mouse-ups
        // bubble here too, which is harmless: the gesture is gone by then).
        root.on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
            if !this.building3d_over_canvas(ev.position) {
                this.building3d_mouse_move(ev, cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
        )
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
        )
        .on_mouse_up(
            MouseButton::Middle,
            cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
        )
    }

    /// Whether window point `p` is over the 3D canvas.
    fn building3d_over_canvas(&self, p: gpui::Point<Pixels>) -> bool {
        self.building3d
            .as_ref()
            .and_then(|pane| pane.canvas_bounds.get())
            .is_some_and(|b| b.contains(&p))
    }

    // --- rendering the pane ---------------------------------------------------

    fn building3d_set_tool(&mut self, tool: Tool) {
        if let Some(pane) = self.building3d.as_mut() {
            pane.tool = tool;
            pane.split_start = None;
            pane.gesture = None;
            pane.touch();
        }
    }

    pub(crate) fn render_building3d_pane(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let selected_part = self.building3d_selected_part();
        let pane = self.building3d.as_ref()?;
        let border = cx.theme().border;
        let background = cx.theme().background;
        let tool = pane.tool;
        let bounds_cell = pane.canvas_bounds.clone();
        let image = pane.image.clone();
        let error = pane.error.clone();
        let has_parts = pane.scene.as_ref().is_some_and(|s| s.building.has_parts());

        let tool_button = |id: &'static str,
                           t: Tool,
                           label: SharedString,
                           tip: SharedString,
                           cx: &mut Context<Self>| {
            let mut b = Button::new(id).small().label(label).tooltip(tip);
            if tool == t {
                b = b.primary();
            } else {
                b = b.ghost();
            }
            b.on_click(cx.listener(move |this, _, _, cx| {
                this.building3d_set_tool(t);
                cx.notify();
            }))
        };

        let info: SharedString = match selected_part.and_then(|i| pane.part(i).map(|p| (i, p))) {
            Some((i, p)) => tr!(
                "building3d.info.part",
                n = i + 1,
                count = pane.scene.as_ref().map_or(0, |s| s.building.parts.len()),
                min = tags::format_height(p.heights.min_height),
                max = tags::format_height(p.heights.height)
            ),
            None => match pane.scene.as_ref() {
                Some(s) if has_parts => {
                    osm_gpui::tr_count!("building3d.info.building_parts", s.building.parts.len())
                }
                Some(s) => tr!(
                    "building3d.info.building_no_parts",
                    max = tags::format_height(
                        s.building.parts.first().map_or(0.0, |p| p.heights.height)
                    )
                ),
                None => SharedString::default(),
            },
        };
        let hint: SharedString = match (tool, pane.split_start.is_some()) {
            (Tool::Select, _) => tr!("building3d.hint.select"),
            (Tool::DrawPart, _) => tr!("building3d.hint.draw"),
            (Tool::Split, false) => tr!("building3d.hint.split_first"),
            (Tool::Split, true) => tr!("building3d.hint.split_second"),
            (Tool::SplitHeight, _) => tr!("building3d.hint.split_height"),
        };
        let label = self.building3d_overlay_label();

        let toolbar = h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(tool_button(
                "b3d-select",
                Tool::Select,
                tr!("building3d.tool.select"),
                tr!("building3d.tool.select_tip"),
                cx,
            ))
            .child(tool_button(
                "b3d-draw",
                Tool::DrawPart,
                tr!("building3d.tool.draw"),
                tr!("building3d.tool.draw_tip"),
                cx,
            ))
            .child(tool_button(
                "b3d-split",
                Tool::Split,
                tr!("building3d.tool.split"),
                tr!("building3d.tool.split_tip"),
                cx,
            ))
            .child(tool_button(
                "b3d-split-height",
                Tool::SplitHeight,
                tr!("building3d.tool.split_height"),
                tr!("building3d.tool.split_height_tip"),
                cx,
            ))
            .child(div().flex_1())
            .child(
                Button::new("b3d-delete")
                    .small()
                    .ghost()
                    .label(tr!("building3d.delete_part"))
                    .disabled(selected_part.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.run_selection_action("delete", cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("b3d-reset-view")
                    .small()
                    .ghost()
                    .label(tr!("building3d.reset_view"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(pane) = this.building3d.as_mut() {
                            pane.frame_camera();
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("b3d-close")
                    .small()
                    .ghost()
                    .label("✕")
                    .tooltip(tr!("building3d.close"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_building3d(window);
                        cx.notify();
                    })),
            );

        let view_canvas = canvas(
            move |bounds, window, _| {
                if bounds_cell.get() != Some(bounds) {
                    bounds_cell.set(Some(bounds));
                    // Render at the new size on the next frame.
                    window.refresh();
                }
            },
            move |bounds, _, window, _| {
                if let Some(image) = image {
                    let _ = window.paint_image(bounds, Corners::default(), image, 0, false);
                }
            },
        )
        .size_full();

        let surface = div()
            .id("building3d-view")
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .bg(gpui::rgb(0xcbdbea))
            .cursor(match tool {
                Tool::Select => gpui::CursorStyle::Arrow,
                _ => gpui::CursorStyle::Crosshair,
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.building3d_mouse_down(ev, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.building3d_mouse_down(ev, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.building3d_mouse_down(ev, cx)),
            )
            // Moves and releases are handled here as well as at the root
            // (which only exists from the frame after a press): a quick click
            // can deliver its press and release in the same frame.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                this.building3d_mouse_move(ev, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| this.building3d_mouse_up(ev, cx)),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    if let Some(pane) = this.building3d.as_mut() {
                        if pane.hover.take().is_some() {
                            pane.touch();
                            cx.notify();
                        }
                    }
                }
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                this.building3d_scroll(ev, cx);
            }))
            .child(view_canvas)
            .children(label.map(|(pos, text)| {
                div()
                    .absolute()
                    .left(px(pos.x + 12.0))
                    .top(px(pos.y - 28.0))
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .bg(gpui::rgba(0x1e2430dd))
                    .text_color(gpui::rgb(0xffffff))
                    .text_sm()
                    .child(text)
            }))
            .children(error.map(|e| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .text_color(gpui::rgb(0x40242a))
                    .child(e)
            }))
            .child(
                div()
                    .absolute()
                    .left_2()
                    .right_2()
                    .bottom_2()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(gpui::rgba(0xffffffcc))
                            .text_color(gpui::rgb(0x1e2430))
                            .text_sm()
                            .child(info),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(gpui::rgba(0xffffffaa))
                            .text_color(gpui::rgb(0x3a4250))
                            .text_xs()
                            .child(hint),
                    ),
            );

        let handle = div()
            .id("building3d-resize")
            .w(px(5.0))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .bg(border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    this.start_building3d_resize(ev.position.x.as_f32());
                    cx.notify();
                }),
            );

        Some(
            h_flex()
                .w(px(self.building3d_pane_width))
                .h_full()
                .flex_none()
                .child(handle)
                .child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .bg(background)
                        .child(toolbar)
                        .child(surface),
                )
                .into_any_element(),
        )
    }

    /// Text shown next to the cursor while dragging (the height being set,
    /// the size of the part being drawn), with where to show it.
    fn building3d_overlay_label(&self) -> Option<(Vec2, SharedString)> {
        let pane = self.building3d.as_ref()?;
        let size = pane.canvas_size()?;
        let scene = pane.scene.as_ref()?;
        let radius = scene.ground_radius as f32;
        let project = |w: Vec3| pane.camera.project(w, size, radius);
        match pane.gesture.as_ref()? {
            Gesture::Vertical {
                anchor,
                current,
                edit,
                ..
            } => {
                let z = match edit {
                    VerticalEdit::Bottom | VerticalEdit::Lift => current.min_height,
                    VerticalEdit::Top => current.height,
                };
                let p = project(Vec3::new(anchor.x, anchor.y, z as f32))?;
                let text = match edit {
                    VerticalEdit::Top => format!("{} m", tags::format_height(current.height)),
                    _ => format!(
                        "{}–{} m",
                        tags::format_height(current.min_height),
                        tags::format_height(current.height)
                    ),
                };
                Some((p, text.into()))
            }
            Gesture::Rect {
                start,
                end,
                angle,
                base_z,
            } => {
                let (u, v) = ((angle.cos(), angle.sin()), (-angle.sin(), angle.cos()));
                let d = (end.0 - start.0, end.1 - start.1);
                let (du, dv) = ((d.0 * u.0 + d.1 * u.1).abs(), (d.0 * v.0 + d.1 * v.1).abs());
                let p = project(Vec3::new(end.0 as f32, end.1 as f32, *base_z as f32))?;
                Some((
                    p,
                    format!(
                        "{} × {} m",
                        tags::format_height(du),
                        tags::format_height(dv)
                    )
                    .into(),
                ))
            }
            Gesture::Wall { offset, .. } => {
                let hover = pane.hover?;
                let p = project(hover.point())?;
                Some((
                    p,
                    format!("{} m", tags::format_height(offset.0.hypot(offset.1))).into(),
                ))
            }
            _ => None,
        }
    }
}

/// Start the drag a press turned into.
fn start_drag(
    pane: &Building3dPane,
    down: Vec2,
    hit: Option<Hit>,
    shift: bool,
    pan: bool,
) -> Gesture {
    if pan {
        return Gesture::Pan { last: down };
    }
    let part_hit = match hit {
        Some(Hit::Face { face, point }) => match face.solid {
            SolidRef::Part(i) => Some((i, face.kind, point)),
            _ => None,
        },
        _ => None,
    };
    match pane.tool {
        Tool::Select => {
            if let Some((i, kind, point)) = part_hit {
                if let Some(solid) = pane.part(i) {
                    match kind {
                        FaceKind::Roof | FaceKind::Bottom => {
                            let edit = match (kind, shift) {
                                (FaceKind::Roof, true) => VerticalEdit::Lift,
                                (FaceKind::Roof, false) => VerticalEdit::Top,
                                _ => VerticalEdit::Bottom,
                            };
                            return Gesture::Vertical {
                                part: i,
                                edit,
                                anchor: point,
                                current: solid.heights.clone(),
                            };
                        }
                        FaceKind::Wall(segment) => {
                            return Gesture::Wall {
                                part: i,
                                segment,
                                start: point,
                                offset: (0.0, 0.0),
                                kind: if shift {
                                    ExtrudeKind::Move
                                } else {
                                    ExtrudeKind::Reshape
                                },
                            };
                        }
                    }
                }
            }
            Gesture::Orbit { last: down }
        }
        Tool::DrawPart => {
            let (base_z, start, ring) = match (part_hit, hit) {
                (Some((i, FaceKind::Roof, point)), _) => {
                    let Some(solid) = pane.part(i) else {
                        return Gesture::Orbit { last: down };
                    };
                    (solid.heights.wall_top(), point, solid.ring.clone())
                }
                (_, Some(Hit::Ground { point })) => {
                    let ring = pane
                        .scene
                        .as_ref()
                        .and_then(|s| s.building.outline.as_ref().or(s.building.parts.first()))
                        .map(|s| s.ring.clone())
                        .unwrap_or_default();
                    (0.0, point, ring)
                }
                _ => return Gesture::Orbit { last: down },
            };
            let start = (start.x as f64, start.y as f64);
            Gesture::Rect {
                base_z,
                start,
                end: start,
                angle: edit::dominant_angle(&ring),
            }
        }
        Tool::Split | Tool::SplitHeight => Gesture::Orbit { last: down },
    }
}

/// Follow the pointer at `p` with the gesture in progress.
fn drag_to(pane: &mut Building3dPane, p: Vec2, no_snap: bool) {
    let ray = pane.ray(p);
    let tolerance = pane.snap_tolerance();
    let height_px = pane.canvas_size().map_or(600.0, |s| s.y);
    let snaps = match &pane.gesture {
        Some(Gesture::Vertical { part, .. }) => pane.snap_heights(*part),
        _ => Vec::new(),
    };
    let vertex_snaps: Vec<Pt> = pane
        .scene
        .as_ref()
        .map(|s| {
            s.building
                .outline
                .iter()
                .chain(s.building.parts.iter())
                .flat_map(|p| p.ring.iter().copied())
                .collect()
        })
        .unwrap_or_default();
    let camera = pane.camera;
    match pane.gesture.as_mut() {
        Some(Gesture::Orbit { last }) => {
            let d = p - *last;
            pane.camera.orbit(-d.x, d.y);
            *last = p;
        }
        Some(Gesture::Pan { last }) => {
            let d = p - *last;
            pane.camera.pan(d.x, d.y, height_px);
            *last = p;
        }
        Some(Gesture::Vertical {
            edit: kind,
            anchor,
            current,
            part,
        }) => {
            let Some(ray) = ray else { return };
            let Some(z) = edit::drag_height(&ray, *anchor, &camera) else {
                return;
            };
            let Some(original) = pane
                .scene
                .as_ref()
                .and_then(|s| s.building.parts.get(*part))
                .map(|s| s.heights.clone())
            else {
                return;
            };
            // The grabbed face follows the cursor: move its height by how
            // far the cursor moved from the grab point.
            let grabbed = match kind {
                VerticalEdit::Top | VerticalEdit::Lift => {
                    if *kind == VerticalEdit::Top {
                        original.height
                    } else {
                        original.min_height
                    }
                }
                VerticalEdit::Bottom => original.min_height,
            };
            let raw = grabbed + (z - anchor.z) as f64;
            let target = edit::snap_height(raw, &snaps, tolerance, !no_snap);
            *current = edit::apply_vertical(&original, *kind, target);
        }
        Some(Gesture::Wall { start, offset, .. }) => {
            let Some(ray) = ray else { return };
            let Some(hit) = ray.hit_horizontal(start.z) else {
                return;
            };
            let raw = ((hit.x - start.x) as f64, (hit.y - start.y) as f64);
            // Whole decimetres along the drag.
            let len = raw.0.hypot(raw.1);
            *offset = if no_snap || len < 1e-6 {
                raw
            } else {
                let snapped = (len * 10.0).round() / 10.0;
                (raw.0 / len * snapped, raw.1 / len * snapped)
            };
            pane.hover = Some(Hit::Ground { point: hit });
        }
        Some(Gesture::Rect { base_z, end, .. }) => {
            let Some(ray) = ray else { return };
            let Some(hit) = ray.hit_horizontal(*base_z as f32) else {
                return;
            };
            let raw = (hit.x as f64, hit.y as f64);
            *end = if no_snap {
                raw
            } else {
                edit::snap_point(raw, &vertex_snaps, tolerance)
            };
        }
        _ => {}
    }
}

/// Tool previews drawn into the mesh: the rectangle being drawn, the split
/// line, the split height.
fn building3d_guides(pane: &Building3dPane, scene: &Scene, m: &mut mesh::Mesh) {
    if let Some(Gesture::Rect {
        base_z,
        start,
        end,
        angle,
    }) = &pane.gesture
    {
        let corners = edit::rectangle(*start, *end, *angle);
        if model::signed_area(&corners).abs() > 1e-3 {
            mesh::add_preview_solid(
                m,
                &Solid {
                    way_id: 0,
                    node_ids: Vec::new(),
                    ring: corners.to_vec(),
                    heights: tags::Heights {
                        min_height: *base_z,
                        height: base_z + 0.3,
                        roof_height: 0.0,
                    },
                    roof: tags::RoofShape::Flat,
                    wall_colour: None,
                    roof_colour: None,
                    is_outline: false,
                },
            );
        }
    }
    match (pane.tool, &pane.split_start, &pane.hover) {
        (Tool::Split, Some((_, start)), Some(hover)) => {
            let end = hover.point();
            let a = *start + Vec3::Z * 0.05;
            let b = Vec3::new(end.x, end.y, start.z + 0.05);
            // Long enough to show the whole cut.
            let dir = (b - a).normalize_or_zero();
            let reach = scene.building_radius as f32 * 2.5;
            mesh::add_guide_line(m, a - dir * reach, a + dir * reach);
        }
        (Tool::SplitHeight, _, Some(Hit::Face { face, point })) => {
            if let (SolidRef::Part(i), FaceKind::Wall(_)) = (face.solid, face.kind) {
                if let Some(solid) = scene.building.parts.get(i) {
                    let z = point.z;
                    let n = solid.ring.len();
                    for k in 0..n {
                        let (p, q) = (solid.ring[k], solid.ring[(k + 1) % n]);
                        mesh::add_guide_line(
                            m,
                            Vec3::new(p.0 as f32, p.1 as f32, z),
                            Vec3::new(q.0 as f32, q.1 as f32, z),
                        );
                    }
                }
            }
        }
        _ => {}
    }
}
