// The 3D building view: lit solids, edge lines and the textured ground.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // xyz: direction towards the light.
    light: vec4<f32>,
    // xyz: camera position; w: how far (as a fraction of the distance to
    // the camera) lines are pulled towards it, so edges lying exactly on a
    // face win the depth test.
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct MeshIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_mesh(v: MeshIn) -> MeshOut {
    var out: MeshOut;
    out.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    out.color = v.color;
    out.normal = v.normal;
    return out;
}

@fragment
fn fs_mesh(i: MeshOut) -> @location(0) vec4<f32> {
    let n = normalize(i.normal);
    let l = normalize(u.light.xyz);
    // Two-sided diffuse plus a little sky light from above, so walls facing
    // away from the sun still read as solid.
    let diffuse = max(dot(n, l), 0.0);
    let sky = 0.5 + 0.5 * n.z;
    let shade = 0.42 + 0.43 * diffuse + 0.15 * sky;
    return vec4<f32>(i.color.rgb * shade, 1.0);
}

struct LineIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(v: LineIn) -> LineOut {
    var out: LineOut;
    let pulled = v.pos + (u.eye.xyz - v.pos) * u.eye.w;
    out.clip = u.view_proj * vec4<f32>(pulled, 1.0);
    out.color = v.color;
    return out;
}

@fragment
fn fs_line(i: LineOut) -> @location(0) vec4<f32> {
    return i.color;
}

struct GroundIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct GroundOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(1) @binding(0) var ground_tex: texture_2d<f32>;
@group(1) @binding(1) var ground_sampler: sampler;

@vertex
fn vs_ground(v: GroundIn) -> GroundOut {
    var out: GroundOut;
    out.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    out.uv = v.uv;
    return out;
}

@fragment
fn fs_ground(i: GroundOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(ground_tex, ground_sampler, i.uv).rgb, 1.0);
}
