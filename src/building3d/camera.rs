//! The 3D view's orbit camera and ray picking.

use glam::{Mat4, Vec2, Vec3, Vec4};

use super::mesh::{FaceId, PickTri};

/// Vertical field of view.
const FOV_Y: f32 = 45.0 * std::f32::consts::PI / 180.0;
const MIN_PITCH: f32 = 3.0 * std::f32::consts::PI / 180.0;
const MAX_PITCH: f32 = 89.0 * std::f32::consts::PI / 180.0;
const MIN_DISTANCE: f32 = 2.0;
const MAX_DISTANCE: f32 = 3000.0;

/// A camera orbiting `target`: `yaw` turns around the vertical axis (0 looks
/// north, from the south), `pitch` is the angle above the horizon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

impl OrbitCamera {
    /// A camera looking at a building of the given footprint centre and
    /// radius from the south-west, a little above it.
    pub fn framing(center: Vec3, radius: f32, height: f32) -> Self {
        let size = radius.max(height * 0.6).max(5.0);
        Self {
            target: Vec3::new(center.x, center.y, height * 0.35),
            yaw: 35.0_f32.to_radians(),
            pitch: 32.0_f32.to_radians(),
            distance: (size / (FOV_Y / 2.0).tan() * 2.3).clamp(MIN_DISTANCE, MAX_DISTANCE),
        }
    }

    /// Unit vector from the target towards the eye.
    fn back(&self) -> Vec3 {
        Vec3::new(
            -self.pitch.cos() * self.yaw.sin(),
            -self.pitch.cos() * self.yaw.cos(),
            self.pitch.sin(),
        )
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.back() * self.distance
    }

    /// The horizontal direction the camera faces.
    pub fn forward_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.sin(), self.yaw.cos(), 0.0)
    }

    /// The camera's right-hand horizontal direction.
    pub fn right_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), -self.yaw.sin(), 0.0)
    }

    pub fn near_far(&self, scene_radius: f32) -> (f32, f32) {
        let near = (self.distance * 0.01).clamp(0.05, 5.0);
        let far = self.distance * 4.0 + scene_radius * 4.0;
        (near, far)
    }

    /// View-projection matrix (wgpu clip space: depth 0..1).
    pub fn view_proj(&self, aspect: f32, scene_radius: f32) -> Mat4 {
        let (near, far) = self.near_far(scene_radius);
        let proj = Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), near, far);
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        proj * view
    }

    /// Turn by a drag of (dx, dy) pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * 0.008;
        self.pitch = (self.pitch + dy * 0.006).clamp(MIN_PITCH, MAX_PITCH);
    }

    /// Zoom in (factor < 1) or out (factor > 1).
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Slide the target over the ground by a drag of (dx, dy) pixels in a
    /// view `height_px` tall, so the ground follows the cursor roughly.
    pub fn pan(&mut self, dx: f32, dy: f32, height_px: f32) {
        let metres_per_px = 2.0 * self.distance * (FOV_Y / 2.0).tan() / height_px.max(1.0);
        self.target += (-self.right_flat() * dx + self.forward_flat() * dy) * metres_per_px;
    }

    /// The world-space ray through pixel `p` of a `size` view.
    pub fn ray(&self, p: Vec2, size: Vec2, scene_radius: f32) -> Ray {
        let inv = self
            .view_proj(size.x / size.y.max(1.0), scene_radius)
            .inverse();
        let ndc = Vec2::new(p.x / size.x * 2.0 - 1.0, 1.0 - p.y / size.y * 2.0);
        let unproject = |z: f32| {
            let v = inv * Vec4::new(ndc.x, ndc.y, z, 1.0);
            v.truncate() / v.w
        };
        let near = unproject(0.0);
        let far = unproject(1.0);
        Ray {
            origin: near,
            dir: (far - near).normalize_or_zero(),
        }
    }

    /// Where world point `w` lands in a `size` view, in pixels; `None` if it
    /// is behind the camera.
    pub fn project(&self, w: Vec3, size: Vec2, scene_radius: f32) -> Option<Vec2> {
        let clip = self.view_proj(size.x / size.y.max(1.0), scene_radius) * w.extend(1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(Vec2::new(
            (ndc.x + 1.0) / 2.0 * size.x,
            (1.0 - ndc.y) / 2.0 * size.y,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

impl Ray {
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }

    /// Where the ray crosses the horizontal plane at height `z`.
    pub fn hit_horizontal(&self, z: f32) -> Option<Vec3> {
        if self.dir.z.abs() < 1e-6 {
            return None;
        }
        let t = (z - self.origin.z) / self.dir.z;
        (t > 0.0).then(|| self.at(t))
    }

    /// Where the ray crosses the plane through `point` with `normal`.
    pub fn hit_plane(&self, point: Vec3, normal: Vec3) -> Option<Vec3> {
        let denom = self.dir.dot(normal);
        if denom.abs() < 1e-6 {
            return None;
        }
        let t = (point - self.origin).dot(normal) / denom;
        (t > 0.0).then(|| self.at(t))
    }

    /// Distance along the ray to where it enters the box (0 if it starts
    /// inside), if it crosses it at all (slab test, slightly padded).
    pub fn hit_box(&self, min: Vec3, max: Vec3) -> Option<f32> {
        const PAD: f32 = 1e-3;
        let (min, max) = (min - Vec3::splat(PAD), max + Vec3::splat(PAD));
        let mut t0 = 0.0f32;
        let mut t1 = f32::INFINITY;
        for axis in 0..3 {
            let (o, d) = (self.origin[axis], self.dir[axis]);
            if d.abs() < 1e-12 {
                if o < min[axis] || o > max[axis] {
                    return None;
                }
                continue;
            }
            let (a, b) = ((min[axis] - o) / d, (max[axis] - o) / d);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }

    /// Möller–Trumbore: distance along the ray to the triangle, if hit.
    pub fn hit_triangle(&self, v: &[Vec3; 3]) -> Option<f32> {
        let e1 = v[1] - v[0];
        let e2 = v[2] - v[0];
        let p = self.dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-9 {
            return None;
        }
        let inv = 1.0 / det;
        let s = self.origin - v[0];
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let w = self.dir.dot(q) * inv;
        if w < 0.0 || u + w > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inv;
        (t > 1e-4).then_some(t)
    }
}

/// What a ray hits first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Face { face: FaceId, point: Vec3 },
    Ground { point: Vec3 },
}

impl Hit {
    pub fn point(&self) -> Vec3 {
        match self {
            Hit::Face { point, .. } | Hit::Ground { point } => *point,
        }
    }
}

/// The nearest face the ray hits, else the ground.
pub fn pick(ray: &Ray, tris: &[PickTri]) -> Option<Hit> {
    let nearest = tris
        .iter()
        .filter_map(|t| ray.hit_triangle(&t.v).map(|d| (d, t.face)))
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match nearest {
        Some((t, face)) => Some(Hit::Face {
            face,
            point: ray.at(t),
        }),
        None => ray.hit_horizontal(0.0).map(|point| Hit::Ground { point }),
    }
}

/// Pick triangles grouped by solid, each group with its bounding box, so a
/// ray only tests the triangles of solids whose box it crosses. Built once
/// per scene; [`PickIndex::pick`] answers exactly as [`pick`] would.
#[derive(Clone, Debug, Default)]
pub struct PickIndex {
    tris: Vec<PickTri>,
    /// (box min, box max, start, end) over `tris`.
    groups: Vec<(Vec3, Vec3, usize, usize)>,
}

impl PickIndex {
    /// Index `tris`, grouping runs of triangles from the same solid (as the
    /// mesher emits them).
    pub fn new(tris: Vec<PickTri>) -> Self {
        let mut groups = Vec::new();
        let mut start = 0;
        while start < tris.len() {
            let solid = tris[start].face.solid;
            let mut end = start;
            let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            while end < tris.len() && tris[end].face.solid == solid {
                for v in tris[end].v {
                    min = min.min(v);
                    max = max.max(v);
                }
                end += 1;
            }
            groups.push((min, max, start, end));
            start = end;
        }
        Self { tris, groups }
    }

    /// The nearest face the ray hits, else the ground.
    pub fn pick(&self, ray: &Ray) -> Option<Hit> {
        let mut best: Option<(f32, FaceId)> = None;
        for &(min, max, start, end) in &self.groups {
            let Some(enter) = ray.hit_box(min, max) else {
                continue;
            };
            if best.is_some_and(|(d, _)| enter > d) {
                continue;
            }
            for t in &self.tris[start..end] {
                if let Some(d) = ray.hit_triangle(&t.v) {
                    if best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, t.face));
                    }
                }
            }
        }
        match best {
            Some((t, face)) => Some(Hit::Face {
                face,
                point: ray.at(t),
            }),
            None => ray.hit_horizontal(0.0).map(|point| Hit::Ground { point }),
        }
    }

    pub fn tris(&self) -> &[PickTri] {
        &self.tris
    }
}

/// `base` turned to whichever of eight compass directions (starting from
/// its own) sees the most of `look_at` without `blockers` (the neighbouring
/// buildings) in the way, so a tall neighbour doesn't fill the view. Ties
/// keep the earliest direction.
pub fn least_obstructed(base: OrbitCamera, look_at: &[Vec3], blockers: &[PickTri]) -> OrbitCamera {
    let blocked = |cam: &OrbitCamera| {
        let eye = cam.eye();
        look_at
            .iter()
            .filter(|&&p| {
                let to = p - eye;
                let dist = to.length();
                let ray = Ray {
                    origin: eye,
                    dir: to / dist.max(1e-6),
                };
                blockers
                    .iter()
                    .any(|t| ray.hit_triangle(&t.v).is_some_and(|d| d < dist - 0.01))
            })
            .count()
    };
    let mut best = (usize::MAX, base);
    for k in 0..8 {
        let mut cam = base;
        cam.yaw = base.yaw + k as f32 * std::f32::consts::FRAC_PI_4;
        let n = blocked(&cam);
        if n < best.0 {
            best = (n, cam);
            if n == 0 {
                break;
            }
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::super::mesh::{FaceKind, SolidRef};
    use super::*;

    #[test]
    fn centre_ray_points_at_target() {
        let cam = OrbitCamera::framing(Vec3::new(10.0, 20.0, 0.0), 15.0, 10.0);
        let size = Vec2::new(800.0, 600.0);
        let ray = cam.ray(size / 2.0, size, 200.0);
        let to_target = (cam.target - ray.origin).normalize();
        assert!(ray.dir.dot(to_target) > 0.9999);
        let p = cam.project(cam.target, size, 200.0).unwrap();
        assert!((p - size / 2.0).length() < 0.5);
    }

    #[test]
    fn project_and_ray_round_trip() {
        let cam = OrbitCamera::framing(Vec3::ZERO, 15.0, 10.0);
        let size = Vec2::new(640.0, 480.0);
        let w = Vec3::new(3.0, -4.0, 0.0);
        let p = cam.project(w, size, 200.0).unwrap();
        let hit = cam.ray(p, size, 200.0).hit_horizontal(0.0).unwrap();
        assert!((hit - w).length() < 0.01, "{hit:?}");
    }

    #[test]
    fn framing_turns_away_from_a_blocking_neighbour() {
        use super::super::mesh::{FaceKind, SolidRef};
        let base = OrbitCamera::framing(Vec3::ZERO, 10.0, 5.0);
        // A tall wall between the default (south-west) eye and the target.
        let eye = base.eye();
        let mid = (eye + base.target) / 2.0;
        let side = Vec3::new(-eye.y, eye.x, 0.0).normalize() * 50.0;
        let face = FaceId {
            solid: SolidRef::Context(0),
            kind: FaceKind::Wall(0),
        };
        let wall = |a: Vec3, b: Vec3, c: Vec3| PickTri { v: [a, b, c], face };
        let (lo, hi) = (
            Vec3::new(mid.x, mid.y, -1.0),
            Vec3::new(mid.x, mid.y, 200.0),
        );
        let blockers = [
            wall(lo - side, lo + side, hi + side),
            wall(lo - side, hi + side, hi - side),
        ];
        let targets = [base.target];
        let cam = least_obstructed(base, &targets, &blockers);
        assert_ne!(cam.yaw, base.yaw);
        assert_eq!(cam.pitch, base.pitch);
        assert_eq!(cam.distance, base.distance);
        // With nothing in the way the camera is left alone.
        assert_eq!(least_obstructed(base, &targets, &[]), base);
    }

    #[test]
    fn pick_index_agrees_with_brute_force() {
        use super::super::mesh::build_mesh;
        use super::super::model::test_data::*;
        use super::super::model::{build_scene, BuildingRoot};
        let mut b = DataBuilder::new();
        b.ring(
            10,
            &rect(0.0, 0.0, 20.0, 12.0),
            &[("building", "yes"), ("height", "15")],
        );
        for i in -3..=3 {
            for j in -3..=3 {
                if (i, j) != (0, 0) {
                    let (x, y) = (i as f64 * 25.0, j as f64 * 25.0);
                    let h = format!("{}", 5 + (i + j + 6) * 2);
                    b.ring(
                        100 + (i * 10 + j) as i64 + 50,
                        &rect(x, y, x + 10.0, y + 8.0),
                        &[("building", "yes"), ("height", &h)],
                    );
                }
            }
        }
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let tris = build_mesh(&scene, &Default::default(), &[]).tris;
        let index = PickIndex::new(tris.clone());
        assert!(index.groups.len() > 10);
        let cam = OrbitCamera::framing(Vec3::new(10.0, 6.0, 0.0), 30.0, 15.0);
        let size = Vec2::new(640.0, 480.0);
        let mut faces = 0;
        for y in (0..480).step_by(23) {
            for x in (0..640).step_by(19) {
                let ray = cam.ray(Vec2::new(x as f32, y as f32), size, 200.0);
                let (a, b) = (index.pick(&ray), pick(&ray, &tris));
                assert_eq!(
                    a.map(|h| matches!(h, Hit::Face { .. })),
                    b.map(|h| matches!(h, Hit::Face { .. }))
                );
                if let (
                    Some(Hit::Face {
                        face: fa,
                        point: pa,
                    }),
                    Some(Hit::Face { point: pb, .. }),
                ) = (a, b)
                {
                    assert!((pa - pb).length() < 1e-3, "{fa:?}");
                    faces += 1;
                }
            }
        }
        assert!(faces > 50, "{faces}");
    }

    #[test]
    fn ray_box() {
        let ray = Ray {
            origin: Vec3::new(-5.0, 0.5, 0.5),
            dir: Vec3::X,
        };
        assert_eq!(
            ray.hit_box(Vec3::ZERO, Vec3::ONE).map(|t| t.round()),
            Some(5.0)
        );
        assert_eq!(
            ray.hit_box(Vec3::new(0.0, 2.0, 0.0), Vec3::new(1.0, 3.0, 1.0)),
            None
        );
        let inside = Ray {
            origin: Vec3::splat(0.5),
            dir: Vec3::Z,
        };
        assert_eq!(inside.hit_box(Vec3::ZERO, Vec3::ONE), Some(0.0));
    }

    #[test]
    fn yaw_zero_looks_north() {
        let mut cam = OrbitCamera::framing(Vec3::ZERO, 10.0, 0.0);
        cam.yaw = 0.0;
        assert!(cam.eye().y < 0.0);
        assert!(cam.forward_flat().y > 0.99);
        assert!(cam.right_flat().x > 0.99);
    }

    #[test]
    fn pitch_and_distance_are_clamped() {
        let mut cam = OrbitCamera::framing(Vec3::ZERO, 10.0, 0.0);
        cam.orbit(0.0, 1e6);
        assert!(cam.pitch <= MAX_PITCH);
        cam.orbit(0.0, -1e6);
        assert!(cam.pitch >= MIN_PITCH);
        cam.zoom(1e-9);
        assert_eq!(cam.distance, MIN_DISTANCE);
    }

    #[test]
    fn pick_prefers_nearest_face_then_ground() {
        let face = |kind| FaceId {
            solid: SolidRef::Part(0),
            kind,
        };
        let tri = |z: f32, kind| PickTri {
            v: [
                Vec3::new(-1.0, -1.0, z),
                Vec3::new(1.0, -1.0, z),
                Vec3::new(0.0, 1.0, z),
            ],
            face: face(kind),
        };
        let tris = [tri(2.0, FaceKind::Bottom), tri(5.0, FaceKind::Roof)];
        let ray = Ray {
            origin: Vec3::new(0.0, 0.0, 10.0),
            dir: -Vec3::Z,
        };
        match pick(&ray, &tris) {
            Some(Hit::Face { face: f, point }) => {
                assert_eq!(f.kind, FaceKind::Roof);
                assert!((point.z - 5.0).abs() < 1e-5);
            }
            other => panic!("{other:?}"),
        }
        let miss = Ray {
            origin: Vec3::new(10.0, 10.0, 10.0),
            dir: -Vec3::Z,
        };
        assert!(matches!(pick(&miss, &tris), Some(Hit::Ground { .. })));
        let up = Ray {
            origin: Vec3::new(10.0, 10.0, 10.0),
            dir: Vec3::Z,
        };
        assert_eq!(pick(&up, &tris), None);
    }

    #[test]
    fn pan_moves_target_horizontally() {
        let mut cam = OrbitCamera::framing(Vec3::ZERO, 10.0, 0.0);
        let z = cam.target.z;
        cam.pan(100.0, 0.0, 600.0);
        assert_eq!(cam.target.z, z);
        assert!(cam.target.length() > 0.0);
    }
}
