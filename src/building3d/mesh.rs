//! Turning a [`Scene`] into triangles (for the GPU and for picking) and edge
//! lines. World space is the scene's local frame: x east, y north, z up, in
//! metres.

use glam::Vec3;

use super::model::{centroid, signed_area, Pt, Scene, Solid};
use super::tags::{Heights, RoofShape};

/// Which solid a face belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SolidRef {
    /// `scene.building.parts[i]`.
    Part(usize),
    /// `scene.context[i]`.
    Context(usize),
}

/// Which face of a solid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaceKind {
    Roof,
    Bottom,
    /// The wall over ring segment `i` (from `ring[i]` to `ring[i + 1]`).
    Wall(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceId {
    pub solid: SolidRef,
    pub kind: FaceKind,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// A triangle kept CPU-side for ray picking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickTri {
    pub v: [Vec3; 3],
    pub face: FaceId,
}

#[derive(Default, Clone, Debug)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Pairs of vertices (a line list).
    pub lines: Vec<LineVertex>,
    pub tris: Vec<PickTri>,
}

/// How to colour the scene this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshStyle {
    /// The part that is selected (tinted).
    pub selected_part: Option<usize>,
    /// The face under the cursor (highlighted).
    pub hovered: Option<FaceId>,
}

pub const PART_WALL: [f32; 3] = [0.87, 0.85, 0.81];
pub const PART_ROOF: [f32; 3] = [0.74, 0.71, 0.68];
pub const CONTEXT_WALL: [f32; 3] = [0.64, 0.65, 0.68];
pub const CONTEXT_ROOF: [f32; 3] = [0.56, 0.57, 0.60];
const SELECT_TINT: [f32; 3] = [0.30, 0.56, 1.0];
const HOVER_TINT: [f32; 3] = [1.0, 0.86, 0.35];
const EDGE: [f32; 4] = [0.18, 0.18, 0.2, 1.0];
const EDGE_CONTEXT: [f32; 4] = [0.35, 0.36, 0.4, 1.0];
const EDGE_SELECTED: [f32; 4] = [0.1, 0.38, 0.95, 1.0];
/// The building outline's footprint when it has parts (it isn't drawn as a
/// solid then, only traced on the ground).
const EDGE_OUTLINE: [f32; 4] = [0.95, 0.55, 0.12, 1.0];

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Build the mesh for `scene`, with `overrides` replacing the geometry of
/// the given parts (a drag in progress) without touching the scene.
pub fn build_mesh(scene: &Scene, style: &MeshStyle, overrides: &[(usize, Solid)]) -> Mesh {
    let mut mesh = build_context_mesh(scene);
    add_building(&mut mesh, scene, style, overrides);
    mesh
}

/// The neighbouring buildings alone. They never change look while a scene
/// is open (only the building's own parts are highlighted), so this is built
/// once per scene and kept, on the GPU too.
pub fn build_context_mesh(scene: &Scene) -> Mesh {
    let mut mesh = Mesh::default();
    for (i, solid) in scene.context.iter().enumerate() {
        add_solid(
            &mut mesh,
            solid,
            Some(SolidRef::Context(i)),
            solid
                .wall_colour
                .map_or(CONTEXT_WALL, |c| mix(c, CONTEXT_WALL, 0.6)),
            solid
                .roof_colour
                .map_or(CONTEXT_ROOF, |c| mix(c, CONTEXT_ROOF, 0.6)),
            EDGE_CONTEXT,
            None,
        );
    }
    mesh
}

/// The building being edited: its parts (with `overrides` standing in for
/// parts being dragged) and its outline, styled for this frame. Small, so
/// rebuilt every frame.
pub fn build_building_mesh(scene: &Scene, style: &MeshStyle, overrides: &[(usize, Solid)]) -> Mesh {
    let mut mesh = Mesh::default();
    add_building(&mut mesh, scene, style, overrides);
    mesh
}

fn add_building(mesh: &mut Mesh, scene: &Scene, style: &MeshStyle, overrides: &[(usize, Solid)]) {
    for (i, part) in scene.building.parts.iter().enumerate() {
        let solid = overrides
            .iter()
            .find(|(j, _)| *j == i)
            .map_or(part, |(_, s)| s);
        let selected = style.selected_part == Some(i);
        let mut wall = solid.wall_colour.unwrap_or(PART_WALL);
        let mut roof = solid.roof_colour.unwrap_or(PART_ROOF);
        if selected {
            wall = mix(wall, SELECT_TINT, 0.45);
            roof = mix(roof, SELECT_TINT, 0.45);
        }
        let edge = if selected { EDGE_SELECTED } else { EDGE };
        add_solid(
            mesh,
            solid,
            Some(SolidRef::Part(i)),
            wall,
            roof,
            edge,
            style.hovered,
        );
    }
    // A building with parts: trace the outline on the ground.
    if scene.building.has_parts() {
        if let Some(outline) = &scene.building.outline {
            let n = outline.ring.len();
            for i in 0..n {
                let (p, q) = (outline.ring[i], outline.ring[(i + 1) % n]);
                push_line(mesh, v3(p, 0.05), v3(q, 0.05), EDGE_OUTLINE);
            }
        }
    }
}

fn v3(p: (f64, f64), z: f64) -> Vec3 {
    Vec3::new(p.0 as f32, p.1 as f32, z as f32)
}

fn push_line(mesh: &mut Mesh, a: Vec3, b: Vec3, color: [f32; 4]) {
    mesh.lines.push(LineVertex {
        pos: a.into(),
        color,
    });
    mesh.lines.push(LineVertex {
        pos: b.into(),
        color,
    });
}

fn push_tri(mesh: &mut Mesh, v: [Vec3; 3], normal: Vec3, color: [f32; 3], face: Option<FaceId>) {
    let base = mesh.vertices.len() as u32;
    for p in v {
        mesh.vertices.push(Vertex {
            pos: p.into(),
            normal: normal.into(),
            color: [color[0], color[1], color[2], 1.0],
        });
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
    if let Some(face) = face {
        mesh.tris.push(PickTri { v, face });
    }
}

/// Triangulate a ring (indices into it). Falls back to a fan for rings
/// earcut rejects.
pub fn triangulate(ring: &[(f64, f64)]) -> Vec<[usize; 3]> {
    let flat: Vec<f64> = ring.iter().flat_map(|p| [p.0, p.1]).collect();
    match earcutr::earcut(&flat, &[], 2) {
        Ok(idx) if !idx.is_empty() => idx.as_chunks::<3>().0.to_vec(),
        _ => (1..ring.len().saturating_sub(1))
            .map(|i| [0, i, i + 1])
            .collect(),
    }
}

/// Draw `solid` as a preview of a part about to be made: tinted, outlined,
/// and not pickable.
pub fn add_preview_solid(mesh: &mut Mesh, solid: &Solid) {
    let wall = mix(PART_WALL, SELECT_TINT, 0.6);
    let roof = mix(PART_ROOF, SELECT_TINT, 0.6);
    add_solid(mesh, solid, None, wall, roof, EDGE_SELECTED, None);
}

/// A highlighted guide line (split line, split height, ...).
pub fn add_guide_line(mesh: &mut Mesh, a: Vec3, b: Vec3) {
    push_line(mesh, a, b, [0.98, 0.72, 0.08, 1.0]);
}

/// A gabled roof planned over a footprint.
#[derive(Clone, Debug, PartialEq)]
pub struct Gable {
    /// The wall outline: the ring's vertices plus the ridge's two ends, each
    /// with the height the wall reaches there and the ring segment it is on.
    pub walls: Vec<(Pt, f64, usize)>,
    /// The two roof slopes, as planar polygons.
    pub slopes: [Vec<Vec3>; 2],
    /// The ridge's two ends.
    pub ridge: (Vec3, Vec3),
}

/// Plan a gabled roof over `ring` rising from `wall_top` by `roof_height`:
/// the ridge runs through the middle of the footprint, along its longest
/// side (or across it), and each slope falls to the eaves on its side.
/// `None` when the ridge would cut the footprint into more than two pieces
/// (a concave footprint); the caller draws something simpler.
pub fn gable(ring: &[Pt], wall_top: f64, roof_height: f64, across: bool) -> Option<Gable> {
    use super::edit::{dominant_angle, split_ring, RingVertex};
    let mut angle = dominant_angle(ring);
    if across {
        angle += std::f64::consts::FRAC_PI_2;
    }
    let (d, nrm) = ((angle.cos(), angle.sin()), (-angle.sin(), angle.cos()));
    let side = |p: Pt| p.0 * nrm.0 + p.1 * nrm.1;
    let (lo, hi) = ring
        .iter()
        .map(|&p| side(p))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), s| {
            (a.min(s), b.max(s))
        });
    let (mid, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
    if half < 1e-6 {
        return None;
    }
    let z = |p: Pt| wall_top + roof_height * (1.0 - ((side(p) - mid).abs() / half).min(1.0));
    let c = (nrm.0 * mid, nrm.1 * mid);
    let split = split_ring(ring, c, (c.0 + d.0, c.1 + d.1)).ok()?;
    let mut walls = Vec::with_capacity(ring.len() + 2);
    for (i, &p) in ring.iter().enumerate() {
        walls.push((p, z(p), i));
        for &(q, seg) in &split.new_points {
            if seg == i {
                walls.push((q, z(q), i));
            }
        }
    }
    let point = |r: &RingVertex| match *r {
        RingVertex::Existing(i) => ring[i],
        RingVertex::New(k) => split.new_points[k].0,
    };
    let lift = |piece: &[RingVertex]| -> Vec<Vec3> {
        piece
            .iter()
            .map(|r| {
                let p = point(r);
                v3(p, z(p))
            })
            .collect()
    };
    let (a, b) = (lift(&split.a), lift(&split.b));
    let ridge = (a[0], *a.last().unwrap());
    Some(Gable {
        walls,
        slopes: [a, b],
        ridge,
    })
}

/// `sref` is `None` for a solid that isn't pickable (a preview).
fn add_solid(
    mesh: &mut Mesh,
    solid: &Solid,
    sref: Option<SolidRef>,
    wall_colour: [f32; 3],
    roof_colour: [f32; 3],
    edge: [f32; 4],
    hovered: Option<FaceId>,
) {
    let ring = &solid.ring;
    let n = ring.len();
    if n < 3 {
        return;
    }
    let Heights { min_height, .. } = solid.heights;
    let top = solid.heights.wall_top();
    let height = solid.heights.height;
    let ccw = signed_area(ring) >= 0.0;
    let face_id = |kind: FaceKind| sref.map(|solid| FaceId { solid, kind });
    let tint = |kind: FaceKind, c: [f32; 3]| {
        if hovered.is_some() && hovered == face_id(kind) {
            mix(c, HOVER_TINT, 0.45)
        } else {
            c
        }
    };

    let pitched = !solid.roof.is_flat() && solid.heights.roof_height > 1e-6;
    let gable = match solid.roof {
        RoofShape::Gabled { across } if pitched => {
            gable(ring, top, solid.heights.roof_height, across)
        }
        _ => None,
    };
    // The wall outline: each vertex with the height its wall reaches there
    // and the ring segment it starts. A gable adds the ridge's ends, where
    // the gable-end walls peak.
    let walls: Vec<(Pt, f64, usize)> = match &gable {
        Some(g) => g.walls.clone(),
        None => ring.iter().enumerate().map(|(i, &p)| (p, top, i)).collect(),
    };

    // Walls.
    let m = walls.len();
    for k in 0..m {
        let ((p, p_top, i), (q, q_top, _)) = (walls[k], walls[(k + 1) % m]);
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len = dx.hypot(dy);
        if len < 1e-9 {
            continue;
        }
        let mut normal = Vec3::new((dy / len) as f32, (-dx / len) as f32, 0.0);
        if !ccw {
            normal = -normal;
        }
        let kind = FaceKind::Wall(i);
        let face = face_id(kind);
        let c = tint(kind, wall_colour);
        let (a0, b0, b1, a1) = (
            v3(p, min_height),
            v3(q, min_height),
            v3(q, q_top),
            v3(p, p_top),
        );
        push_tri(mesh, [a0, b0, b1], normal, c, face);
        push_tri(mesh, [a0, b1, a1], normal, c, face);
        push_line(mesh, a0, b0, edge);
        push_line(mesh, a1, b1, edge);
        // Vertical edges at real corners only, not at a gable's ridge point.
        if (p_top - top).abs() < 1e-9 || k == 0 || walls[k - 1].2 != i {
            push_line(mesh, a0, a1, edge);
        }
    }

    let roof_face = face_id(FaceKind::Roof);
    let rc = tint(FaceKind::Roof, roof_colour);
    if let Some(g) = &gable {
        for slope in &g.slopes {
            let flat: Vec<Pt> = slope.iter().map(|v| (v.x as f64, v.y as f64)).collect();
            for t in triangulate(&flat) {
                let tri = [slope[t[0]], slope[t[1]], slope[t[2]]];
                let mut normal = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
                if normal.z < 0.0 {
                    normal = -normal;
                }
                push_tri(mesh, tri, normal, rc, roof_face);
            }
        }
        push_line(mesh, g.ridge.0, g.ridge.1, edge);
    } else if !pitched {
        for t in triangulate(ring) {
            push_tri(
                mesh,
                [
                    v3(ring[t[0]], top),
                    v3(ring[t[1]], top),
                    v3(ring[t[2]], top),
                ],
                Vec3::Z,
                rc,
                roof_face,
            );
        }
    } else {
        // Pyramid: every wall top edge rises to an apex over the centroid.
        let apex = v3(centroid(ring), height);
        for i in 0..n {
            let (a, b) = (v3(ring[i], top), v3(ring[(i + 1) % n], top));
            let mut normal = (b - a).cross(apex - a).normalize_or_zero();
            if normal.z < 0.0 {
                normal = -normal;
            }
            push_tri(mesh, [a, b, apex], normal, rc, roof_face);
            push_line(mesh, a, apex, edge);
        }
    }

    if min_height > 0.05 {
        let bottom = face_id(FaceKind::Bottom);
        let bc = tint(FaceKind::Bottom, mix(wall_colour, [0.0, 0.0, 0.0], 0.25));
        for t in triangulate(ring) {
            push_tri(
                mesh,
                [
                    v3(ring[t[0]], min_height),
                    v3(ring[t[1]], min_height),
                    v3(ring[t[2]], min_height),
                ],
                -Vec3::Z,
                bc,
                bottom,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::test_data::*;
    use super::super::model::{build_scene, BuildingRoot};
    use super::*;

    #[test]
    fn box_has_walls_roof_and_edges() {
        let mut b = DataBuilder::new();
        b.ring(
            10,
            &rect(0.0, 0.0, 10.0, 10.0),
            &[("building", "yes"), ("height", "6")],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let mesh = build_mesh(&scene, &MeshStyle::default(), &[]);
        // 4 walls x 2 tris + 2 roof tris, no bottom (on the ground).
        assert_eq!(mesh.tris.len(), 10);
        assert_eq!(mesh.indices.len(), 30);
        assert_eq!(mesh.vertices.len(), 30);
        let walls: Vec<_> = mesh
            .tris
            .iter()
            .filter_map(|t| match t.face.kind {
                FaceKind::Wall(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(walls, vec![0, 0, 1, 1, 2, 2, 3, 3]);
        let top = mesh
            .tris
            .iter()
            .filter(|t| t.face.kind == FaceKind::Roof)
            .flat_map(|t| t.v)
            .map(|v| v.z)
            .fold(f32::MIN, f32::max);
        assert!((top - 6.0).abs() < 1e-5);
        assert_eq!(mesh.lines.len() % 2, 0);
    }

    #[test]
    fn wall_normals_point_outward_for_either_winding() {
        for reversed in [false, true] {
            let mut pts = rect(0.0, 0.0, 10.0, 10.0).to_vec();
            if reversed {
                pts.reverse();
            }
            let mut b = DataBuilder::new();
            b.ring(10, &pts, &[("building", "yes")]);
            let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
            let mesh = build_mesh(&scene, &MeshStyle::default(), &[]);
            // The frame is anchored at the first node, so use the centroid.
            let c = scene.building.parts[0].centroid();
            for (tri, chunk) in mesh.tris.iter().zip(mesh.vertices.chunks(3)) {
                if let FaceKind::Wall(_) = tri.face.kind {
                    let mid = (tri.v[0] + tri.v[1] + tri.v[2]) / 3.0;
                    let out = Vec3::new(mid.x - c.0 as f32, mid.y - c.1 as f32, 0.0);
                    let n = Vec3::from(chunk[0].normal);
                    assert!(n.dot(out) > 0.0, "reversed={reversed}");
                }
            }
        }
    }

    #[test]
    fn floating_part_gets_a_bottom_and_pitched_roof_an_apex() {
        let mut b = DataBuilder::new();
        b.ring(
            11,
            &rect(0.0, 0.0, 10.0, 10.0),
            &[
                ("building:part", "yes"),
                ("min_height", "3"),
                ("height", "9"),
                ("roof:shape", "pyramidal"),
                ("roof:height", "2"),
            ],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(11)).unwrap();
        let mesh = build_mesh(&scene, &MeshStyle::default(), &[]);
        assert!(mesh.tris.iter().any(|t| t.face.kind == FaceKind::Bottom));
        let roof: Vec<_> = mesh
            .tris
            .iter()
            .filter(|t| t.face.kind == FaceKind::Roof)
            .collect();
        assert_eq!(roof.len(), 4);
        let apex = roof[0].v[2];
        assert!((apex.z - 9.0).abs() < 1e-5);
        assert!((apex.x - 5.0).abs() < 1e-4 && (apex.y - 5.0).abs() < 1e-4);
    }

    #[test]
    fn gable_over_a_rectangle() {
        // 20 x 10, ridge along x at y = 5, eaves at 6, ridge at 9.
        let ring = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (0.0, 10.0)];
        let g = gable(&ring, 6.0, 3.0, false).unwrap();
        // Corners at the eaves; the gable ends gain a ridge point each.
        assert_eq!(g.walls.len(), 6);
        let peaks: Vec<_> = g
            .walls
            .iter()
            .filter(|w| (w.1 - 9.0).abs() < 1e-9)
            .collect();
        assert_eq!(peaks.len(), 2);
        for w in &peaks {
            assert!((w.0 .1 - 5.0).abs() < 1e-9);
            assert!(w.0 .0.abs() < 1e-9 || (w.0 .0 - 20.0).abs() < 1e-9);
        }
        assert!(g
            .walls
            .iter()
            .filter(|w| (w.1 - 9.0).abs() > 1e-9)
            .all(|w| (w.1 - 6.0).abs() < 1e-9));
        assert!((g.ridge.0.z - 9.0).abs() < 1e-5 && (g.ridge.1.z - 9.0).abs() < 1e-5);
        assert!((g.ridge.0 - g.ridge.1).length() > 19.9);
        // Each slope is planar: every vertex on the plane of its first three.
        for slope in &g.slopes {
            let n = (slope[1] - slope[0]).cross(slope[2] - slope[0]).normalize();
            for v in slope {
                assert!((v - slope[0]).dot(n).abs() < 1e-4);
            }
        }
        // Across: the ridge runs along y instead.
        let g = gable(&ring, 6.0, 3.0, true).unwrap();
        assert!((g.ridge.0.x - 10.0).abs() < 1e-4 && (g.ridge.1.x - 10.0).abs() < 1e-4);
    }

    #[test]
    fn gabled_part_meshes_with_peaked_gable_ends() {
        let mut b = DataBuilder::new();
        b.ring(
            11,
            &rect(0.0, 0.0, 20.0, 10.0),
            &[
                ("building:part", "yes"),
                ("height", "9"),
                ("roof:shape", "gabled"),
                ("roof:height", "3"),
            ],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(11)).unwrap();
        let mesh = build_mesh(&scene, &MeshStyle::default(), &[]);
        let roof: Vec<_> = mesh
            .tris
            .iter()
            .filter(|t| t.face.kind == FaceKind::Roof)
            .collect();
        assert_eq!(roof.len(), 4, "two quads");
        let top = mesh
            .vertices
            .iter()
            .map(|v| v.pos[2])
            .fold(f32::MIN, f32::max);
        assert!((top - 9.0).abs() < 1e-5);
        // The gable-end walls reach the ridge; their pick faces keep the
        // original segment index.
        let peak_walls: Vec<_> = mesh
            .tris
            .iter()
            .filter(|t| matches!(t.face.kind, FaceKind::Wall(_)))
            .filter(|t| t.v.iter().any(|v| (v.z - 9.0).abs() < 1e-5))
            .map(|t| t.face.kind)
            .collect();
        assert!(peak_walls.contains(&FaceKind::Wall(1)));
        assert!(peak_walls.contains(&FaceKind::Wall(3)));
        // A concave footprint falls back to a pyramid rather than failing.
        let u = vec![
            (0.0, 0.0),
            (30.0, 0.0),
            (30.0, 20.0),
            (20.0, 20.0),
            (20.0, 5.0),
            (10.0, 5.0),
            (10.0, 20.0),
            (0.0, 20.0),
        ];
        assert!(gable(&u, 6.0, 3.0, true).is_none() || gable(&u, 6.0, 3.0, false).is_none());
    }

    #[test]
    fn selected_and_hovered_parts_are_tinted() {
        let mut b = DataBuilder::new();
        b.ring(10, &rect(0.0, 0.0, 10.0, 10.0), &[("building", "yes")]);
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let plain = build_mesh(&scene, &MeshStyle::default(), &[]);
        let selected = build_mesh(
            &scene,
            &MeshStyle {
                selected_part: Some(0),
                hovered: None,
            },
            &[],
        );
        assert_ne!(plain.vertices[0].color, selected.vertices[0].color);
    }

    #[test]
    fn overrides_replace_part_geometry() {
        let mut b = DataBuilder::new();
        b.ring(
            10,
            &rect(0.0, 0.0, 10.0, 10.0),
            &[("building", "yes"), ("height", "6")],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let mut taller = scene.building.parts[0].clone();
        taller.heights.height = 20.0;
        let mesh = build_mesh(&scene, &MeshStyle::default(), &[(0, taller)]);
        let top = mesh
            .vertices
            .iter()
            .map(|v| v.pos[2])
            .fold(f32::MIN, f32::max);
        assert!((top - 20.0).abs() < 1e-5);
    }
}
