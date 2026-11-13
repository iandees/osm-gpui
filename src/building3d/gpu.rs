//! Offscreen GPU rendering of the 3D view with wgpu.
//!
//! gpui has no hook for custom render passes, so the view is drawn into an
//! offscreen texture on our own wgpu device and read back as BGRA bytes,
//! which the app hands to gpui as an image. At view sizes that is a few
//! milliseconds per frame, and only while the view changes.

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use super::ground::Texture;
use super::mesh::{LineVertex, Mesh, Vertex};
use super::model::Pt;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Background above the horizon / beyond the ground.
pub const SKY: [f64; 3] = [0.80, 0.86, 0.92];
/// Ground colour when there is no imagery.
const BARE_GROUND: [u8; 4] = [196, 200, 186, 255];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    light: [f32; 4],
    eye: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GroundVertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

/// The ground to draw: a texture stretched over a quad.
pub struct GroundInput<'a> {
    /// Identifies the texture's content; the texture is re-uploaded only
    /// when this changes.
    pub key: u64,
    /// `None` for bare ground.
    pub texture: Option<&'a Texture>,
    /// North-west, north-east, south-east, south-west corners (local frame).
    pub corners: [Pt; 4],
}

pub struct FrameInput<'a> {
    /// Device pixels.
    pub width: u32,
    pub height: u32,
    pub view_proj: Mat4,
    pub eye: Vec3,
    /// Geometry that rarely changes (the neighbouring buildings), with a key
    /// identifying it: its GPU buffers are kept and only re-uploaded when the
    /// key changes.
    pub static_mesh: Option<(u64, &'a Mesh)>,
    /// Geometry re-uploaded every frame (the building being edited, guides).
    pub mesh: &'a Mesh,
    pub ground: GroundInput<'a>,
}

/// A mesh's triangles and lines in GPU buffers.
struct MeshBuffers {
    tris: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
    lines: Option<(wgpu::Buffer, u32)>,
}

impl MeshBuffers {
    fn upload(device: &wgpu::Device, mesh: &Mesh) -> Self {
        let buffer = |label: &str, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        Self {
            tris: (!mesh.indices.is_empty()).then(|| {
                (
                    buffer(
                        "vertices",
                        bytemuck::cast_slice(&mesh.vertices),
                        wgpu::BufferUsages::VERTEX,
                    ),
                    buffer(
                        "indices",
                        bytemuck::cast_slice(&mesh.indices),
                        wgpu::BufferUsages::INDEX,
                    ),
                    mesh.indices.len() as u32,
                )
            }),
            lines: (!mesh.lines.is_empty()).then(|| {
                (
                    buffer(
                        "lines",
                        bytemuck::cast_slice(&mesh.lines),
                        wgpu::BufferUsages::VERTEX,
                    ),
                    mesh.lines.len() as u32,
                )
            }),
        }
    }
}

/// A rendered frame: tightly packed BGRA rows.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

struct Targets {
    width: u32,
    height: u32,
    msaa: Option<wgpu::TextureView>,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
}

struct GroundGpu {
    key: u64,
    bind_group: wgpu::BindGroup,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    samples: u32,
    uniform_buf: wgpu::Buffer,
    uniform_bg: wgpu::BindGroup,
    ground_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mesh_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    ground_pipeline: wgpu::RenderPipeline,
    targets: Option<Targets>,
    ground: Option<GroundGpu>,
    /// The static mesh's buffers and the key they were uploaded for.
    static_buffers: Option<(u64, MeshBuffers)>,
}

impl Renderer {
    /// Set up a device. Fails when no GPU adapter is available.
    pub fn new() -> Result<Self, String> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("building3d"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| format!("GPU device: {e}"))?;

        let supports = |format: wgpu::TextureFormat| {
            adapter
                .get_texture_format_features(format)
                .flags
                .sample_count_supported(4)
        };
        let samples = if supports(COLOR_FORMAT) && supports(DEPTH_FORMAT) {
            4
        } else {
            1
        };

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let ground_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ground"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ground"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });

        let plain_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("plain"),
            bind_group_layouts: &[Some(&uniform_layout)],
            immediate_size: 0,
        });
        let ground_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ground"),
                bind_group_layouts: &[Some(&uniform_layout), Some(&ground_layout)],
                immediate_size: 0,
            });

        let depth = |write: bool| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let multisample = wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        };
        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        vs: &str,
                        fs: &str,
                        buffers: &[wgpu::VertexBufferLayout],
                        topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(COLOR_FORMAT.into())],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    // Faces are lit two-sided and winding isn't normalised.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth(true)),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };

        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
        let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let ground_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];
        let mesh_pipeline = pipeline(
            "mesh",
            &plain_layout,
            "vs_mesh",
            "fs_mesh",
            &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &mesh_attrs,
            }],
            wgpu::PrimitiveTopology::TriangleList,
        );
        let line_pipeline = pipeline(
            "lines",
            &plain_layout,
            "vs_line",
            "fs_line",
            &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &line_attrs,
            }],
            wgpu::PrimitiveTopology::LineList,
        );
        let ground_pipeline = pipeline(
            "ground",
            &ground_pipeline_layout,
            "vs_ground",
            "fs_ground",
            &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GroundVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ground_attrs,
            }],
            wgpu::PrimitiveTopology::TriangleList,
        );

        Ok(Self {
            device,
            queue,
            samples,
            uniform_buf,
            uniform_bg,
            ground_layout,
            sampler,
            mesh_pipeline,
            line_pipeline,
            ground_pipeline,
            targets: None,
            ground: None,
            static_buffers: None,
        })
    }

    fn ensure_targets(&mut self, width: u32, height: u32) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = |label: &str, format, samples, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let resolve = texture(
            "color",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let msaa = (self.samples > 1).then(|| {
            texture(
                "color-msaa",
                COLOR_FORMAT,
                self.samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&Default::default())
        });
        let depth = texture(
            "depth",
            DEPTH_FORMAT,
            self.samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let padded_bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let resolve_view = resolve.create_view(&Default::default());
        self.targets = Some(Targets {
            width,
            height,
            msaa,
            resolve,
            resolve_view,
            depth,
            readback,
            padded_bytes_per_row,
        });
    }

    fn ensure_ground(&mut self, ground: &GroundInput) {
        if self.ground.as_ref().is_some_and(|g| g.key == ground.key) {
            return;
        }
        let bare = Texture {
            width: 1,
            height: 1,
            levels: vec![BARE_GROUND.to_vec()],
        };
        let tex = ground.texture.unwrap_or(&bare);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ground"),
            size: wgpu::Extent3d {
                width: tex.width,
                height: tex.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: tex.levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h) = (tex.width, tex.height);
        for (level, data) in tex.levels.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        let view = texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ground"),
            layout: &self.ground_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.ground = Some(GroundGpu {
            key: ground.key,
            bind_group,
        });
    }

    /// Draw one frame and read it back.
    pub fn render(&mut self, input: &FrameInput) -> Result<Frame, String> {
        let (width, height) = (input.width.max(1), input.height.max(1));
        self.ensure_targets(width, height);
        self.ensure_ground(&input.ground);

        let light = Vec3::new(-0.45, -0.6, 0.75).normalize();
        let uniforms = Uniforms {
            view_proj: input.view_proj.to_cols_array_2d(),
            light: [light.x, light.y, light.z, 0.0],
            eye: [input.eye.x, input.eye.y, input.eye.z, 0.002],
        };
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let c = input.ground.corners;
        let gv = |p: Pt, u: f32, v: f32| GroundVertex {
            pos: [p.0 as f32, p.1 as f32, 0.0],
            uv: [u, v],
        };
        let ground_vertices = [
            gv(c[0], 0.0, 0.0),
            gv(c[1], 1.0, 0.0),
            gv(c[2], 1.0, 1.0),
            gv(c[0], 0.0, 0.0),
            gv(c[2], 1.0, 1.0),
            gv(c[3], 0.0, 1.0),
        ];
        let buffer = |label: &str, contents: &[u8], usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let ground_buf = buffer(
            "ground",
            bytemuck::cast_slice(&ground_vertices),
            wgpu::BufferUsages::VERTEX,
        );
        match input.static_mesh {
            Some((key, mesh)) => {
                if self.static_buffers.as_ref().is_none_or(|(k, _)| *k != key) {
                    self.static_buffers = Some((key, MeshBuffers::upload(&self.device, mesh)));
                }
            }
            None => self.static_buffers = None,
        }
        let dynamic = MeshBuffers::upload(&self.device, input.mesh);

        let targets = self.targets.as_ref().expect("ensured above");
        let ground = self.ground.as_ref().expect("ensured above");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let (view, resolve_target) = match &targets.msaa {
                Some(msaa) => (msaa, Some(&targets.resolve_view)),
                None => (&targets.resolve_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("building3d"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: SKY[0],
                            g: SKY[1],
                            b: SKY[2],
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.uniform_bg, &[]);

            pass.set_pipeline(&self.ground_pipeline);
            pass.set_bind_group(1, &ground.bind_group, &[]);
            pass.set_vertex_buffer(0, ground_buf.slice(..));
            pass.draw(0..6, 0..1);

            let meshes = self
                .static_buffers
                .as_ref()
                .map(|(_, b)| b)
                .into_iter()
                .chain(std::iter::once(&dynamic));
            for buffers in meshes {
                if let Some((vb, ib, count)) = &buffers.tris {
                    pass.set_pipeline(&self.mesh_pipeline);
                    pass.set_vertex_buffer(0, vb.slice(..));
                    pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..*count, 0, 0..1);
                }
                if let Some((lb, count)) = &buffers.lines {
                    pass.set_pipeline(&self.line_pipeline);
                    pass.set_vertex_buffer(0, lb.slice(..));
                    pass.draw(0..*count, 0..1);
                }
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &targets.resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &targets.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(targets.padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = targets.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("GPU poll: {e}"))?;
        rx.recv()
            .map_err(|e| format!("GPU readback: {e}"))?
            .map_err(|e| format!("GPU readback: {e}"))?;
        let row = (width * 4) as usize;
        let mut bgra = Vec::with_capacity(row * height as usize);
        {
            let mapped = slice.get_mapped_range();
            for y in 0..height as usize {
                let start = y * targets.padded_bytes_per_row as usize;
                bgra.extend_from_slice(&mapped[start..start + row]);
            }
        }
        targets.readback.unmap();
        Ok(Frame {
            width,
            height,
            bgra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::camera::OrbitCamera;
    use super::super::mesh::{build_mesh, MeshStyle};
    use super::super::model::test_data::*;
    use super::super::model::{build_scene, BuildingRoot};
    use super::*;

    /// Renders a box and checks the middle of the frame shows it. Skipped
    /// (passes) on machines without a usable GPU, such as most CI runners.
    #[test]
    fn renders_a_building() {
        let Ok(mut renderer) = Renderer::new() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut b = DataBuilder::new();
        b.ring(
            10,
            &rect(-10.0, -10.0, 10.0, 10.0),
            &[("building", "yes"), ("height", "15")],
        );
        let scene = build_scene(&b.data, BuildingRoot::Way(10)).unwrap();
        let mesh = build_mesh(&scene, &MeshStyle::default(), &[]);
        let cam = OrbitCamera::framing(Vec3::ZERO, 10.0, 15.0);
        let (w, h) = (160, 120);
        let frame = renderer.render(&FrameInput {
            width: w,
            height: h,
            view_proj: cam.view_proj(w as f32 / h as f32, 100.0),
            eye: cam.eye(),
            static_mesh: None,
            mesh: &mesh,
            ground: GroundInput {
                key: 1,
                texture: None,
                corners: [(-50.0, 50.0), (50.0, 50.0), (50.0, -50.0), (-50.0, -50.0)],
            },
        });
        // A software adapter on a GPU-less CI runner may set up but not draw.
        let frame = match frame {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("GPU render failed ({e}); skipping");
                return;
            }
        };
        assert_eq!(frame.bgra.len(), (w * h * 4) as usize);
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [frame.bgra[i + 2], frame.bgra[i + 1], frame.bgra[i]]
        };
        let sky = [
            (SKY[0] * 255.0).round() as u8,
            (SKY[1] * 255.0).round() as u8,
            (SKY[2] * 255.0).round() as u8,
        ];
        let center = px(w / 2, h / 2);
        assert_ne!(center, sky, "the building should cover the centre");
        // The top-left corner is sky (the camera looks down at the box).
        let corner = px(0, 0);
        assert!(
            corner.iter().zip(sky).all(|(a, b)| a.abs_diff(b) <= 2),
            "{corner:?} vs {sky:?}"
        );
    }
}
