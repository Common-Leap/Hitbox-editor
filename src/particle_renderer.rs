/// GPU particle and sword trail renderer.
/// Integrates with the existing egui-wgpu ViewportCallback pipeline.

use std::collections::HashMap;
use wgpu::util::DeviceExt;
use glam::{Mat4, Vec3};
use crate::effects::{BlendType, Particle, SwordTrail};

// ── Camera uniform (matches particle.wgsl / trail.wgsl) ──────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniforms {
    view_proj: [[f32; 4]; 4],
    cam_right: [f32; 3],
    _pad0: f32,
    cam_up: [f32; 3],
    _pad1: f32,
}

// ── Per-particle instance data (matches particle.wgsl) ───────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleInstance {
    position: [f32; 3],
    size: f32,
    color: [f32; 4],
    rotation: f32,
    _pad: [f32; 3],
}

// ── Trail vertex (matches trail.wgsl) ────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TrailVertex {
    position: [f32; 3],
    uv: [f32; 2],
    alpha: f32,
    _pad: f32,
}

// ── Fallback 1×1 white texture ────────────────────────────────────────────────

fn create_white_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("particle_white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &[255u8, 255, 255, 255],
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("particle_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    (texture, view, sampler)
}

// ── Particle renderer ─────────────────────────────────────────────────────────

pub struct ParticleRenderer {
    // Particle pipeline (additive)
    particle_pipeline_add: wgpu::RenderPipeline,
    // Particle pipeline (alpha blend)
    particle_pipeline_alpha: wgpu::RenderPipeline,
    // Trail pipeline (additive)
    trail_pipeline: wgpu::RenderPipeline,
    // Fullscreen blit pipeline (composites particle_target onto surface)
    blit_pipeline: wgpu::RenderPipeline,
    blit_bg_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    // Cached blit bind group — rebuilt when particle_target changes
    blit_bind_group: Option<wgpu::BindGroup>,
    blit_bind_group_for: bool, // unused sentinel, kept for future use

    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_bg_layout: wgpu::BindGroupLayout,

    // Trail camera bind group (cached, not rebuilt every frame)
    trail_cam_bgl: wgpu::BindGroupLayout,
    trail_cam_bg: wgpu::BindGroup,

    tex_bg_layout: wgpu::BindGroupLayout,
    white_tex_bg: wgpu::BindGroup,

    // Per-frame upload buffers (recreated each frame if needed)
    instance_buf: Option<wgpu::Buffer>,
    instance_buf_capacity: usize,
    trail_vertex_buf: Option<wgpu::Buffer>,
    trail_vertex_buf_capacity: usize,

    // Cached wgpu textures keyed by (emitter_set_idx, emitter_idx)
    tex_cache: HashMap<(usize, usize), wgpu::BindGroup>,
}

impl ParticleRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, surface_format: wgpu::TextureFormat) -> Self {
        // ── Shader modules ────────────────────────────────────────────────
        let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("particle.wgsl").into()),
        });
        let trail_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("trail_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("trail.wgsl").into()),
        });

        // ── Bind group layouts ────────────────────────────────────────────
        let camera_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particle_camera_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let tex_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particle_tex_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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

        // Trail camera layout (no storage buffer — vertices are in vertex buffer)
        let trail_camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("trail_camera_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // ── Camera uniform buffer ─────────────────────────────────────────
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle_camera_buf"),
            size: std::mem::size_of::<CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Placeholder storage buffer (1 particle) for initial bind group
        let placeholder_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle_placeholder_storage"),
            size: std::mem::size_of::<ParticleInstance>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle_camera_bg"),
            layout: &camera_bg_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: camera_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: placeholder_storage.as_entire_binding() },
            ],
        });

        // ── White fallback texture ────────────────────────────────────────
        let (_, white_view, white_sampler) = create_white_texture(device, queue);
        let white_tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle_white_tex_bg"),
            layout: &tex_bg_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&white_sampler) },
            ],
        });

        // ── Pipeline layout ───────────────────────────────────────────────
        let particle_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particle_pipeline_layout"),
            bind_group_layouts: &[&camera_bg_layout, &tex_bg_layout],
            push_constant_ranges: &[],
        });

        let trail_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("trail_pipeline_layout"),
            bind_group_layouts: &[&trail_camera_bgl, &tex_bg_layout],
            push_constant_ranges: &[],
        });

        // ── Blend states ──────────────────────────────────────────────────
        let additive_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let alpha_blend = wgpu::BlendState::ALPHA_BLENDING;

        let make_particle_pipeline = |blend: wgpu::BlendState, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&particle_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &particle_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &particle_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        let particle_pipeline_add = make_particle_pipeline(additive_blend, "particle_pipeline_add");
        let particle_pipeline_alpha = make_particle_pipeline(alpha_blend, "particle_pipeline_alpha");

        // Trail vertex layout
        let trail_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TrailVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32, 3 => Float32],
        };

        let trail_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("trail_pipeline"),
            layout: Some(&trail_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &trail_shader,
                entry_point: Some("vs_main"),
                buffers: &[trail_vertex_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &trail_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(additive_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Cached trail camera bind group ────────────────────────────────
        let trail_cam_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("trail_cam_bgl_cached"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let trail_cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("trail_cam_bg_cached"),
            layout: &trail_cam_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        // ── Fullscreen blit pipeline ──────────────────────────────────────
        // Composites the offscreen particle texture onto the surface render pass.
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit_shader"),
            source: wgpu::ShaderSource::Wgsl(r#"
@group(0) @binding(0) var t_particle: texture_2d<f32>;
@group(0) @binding(1) var s_particle: sampler;

struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    // Fullscreen triangle
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VOut;
    out.pos = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv = uv[vi];
    return out;
}

@fragment fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(t_particle, s_particle, in.uv);
}
"#.into()),
        });

        let blit_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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

        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit_pipeline_layout"),
            bind_group_layouts: &[&blit_bg_layout],
            push_constant_ranges: &[],
        });

        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit_pipeline"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            particle_pipeline_add,
            particle_pipeline_alpha,
            trail_pipeline,
            blit_pipeline,
            blit_bg_layout,
            blit_sampler,
            blit_bind_group: None,
            blit_bind_group_for: false,
            camera_buf,
            camera_bind_group,
            camera_bg_layout,
            trail_cam_bgl,
            trail_cam_bg,
            tex_bg_layout,
            white_tex_bg,
            instance_buf: None,
            instance_buf_capacity: 0,
            trail_vertex_buf: None,
            trail_vertex_buf_capacity: 0,
            tex_cache: HashMap::new(),
        }
    }

    /// Upload camera uniforms and particle instance data, then record draw calls.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        view_proj: Mat4,
        cam_right: Vec3,
        cam_up: Vec3,
        particles: &[Particle],
        trails: &[SwordTrail],
    ) {
        // Upload camera uniforms
        let cam_uniforms = CameraUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            cam_right: cam_right.to_array(),
            _pad0: 0.0,
            cam_up: cam_up.to_array(),
            _pad1: 0.0,
        };
        queue.write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cam_uniforms));

        // ── Particles ─────────────────────────────────────────────────────
        if !particles.is_empty() {
            let instances: Vec<ParticleInstance> = particles.iter().map(|p| ParticleInstance {
                position: p.position.to_array(),
                size: p.size,
                color: p.color.to_array(),
                rotation: p.rotation,
                _pad: [0.0; 3],
            }).collect();

            let byte_size = (instances.len() * std::mem::size_of::<ParticleInstance>()) as u64;

            // Recreate storage buffer if capacity exceeded
            if self.instance_buf_capacity < instances.len() {
                self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle_instance_buf"),
                    size: byte_size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.instance_buf_capacity = instances.len();

                // Rebuild camera bind group with new storage buffer
                let storage_buf = self.instance_buf.as_ref().unwrap();
                self.camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("particle_camera_bg"),
                    layout: &self.camera_bg_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.camera_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: storage_buf.as_entire_binding() },
                    ],
                });
            }

            if let Some(buf) = &self.instance_buf {
                queue.write_buffer(buf, 0, bytemuck::cast_slice(&instances));
            }

            // Separate additive and alpha-blend particles
            let add_count = particles.iter().filter(|p| matches!(p.blend_type, BlendType::Add)).count();
            let alpha_count = particles.len() - add_count;

            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("particle_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_bind_group(0, &self.camera_bind_group, &[]);
            rpass.set_bind_group(1, &self.white_tex_bg, &[]);

            if add_count > 0 {
                rpass.set_pipeline(&self.particle_pipeline_add);
                rpass.draw(0..6, 0..add_count as u32);
            }
            if alpha_count > 0 {
                rpass.set_pipeline(&self.particle_pipeline_alpha);
                rpass.draw(0..6, add_count as u32..(add_count + alpha_count) as u32);
            }
        }

        // ── Sword trails ──────────────────────────────────────────────────
        let trail_verts = build_trail_vertices(trails);
        if !trail_verts.is_empty() {
            let byte_size = (trail_verts.len() * std::mem::size_of::<TrailVertex>()) as u64;
            if self.trail_vertex_buf_capacity < trail_verts.len() {
                self.trail_vertex_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("trail_vertex_buf"),
                    size: byte_size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.trail_vertex_buf_capacity = trail_verts.len();
            }
            if let Some(buf) = &self.trail_vertex_buf {
                queue.write_buffer(buf, 0, bytemuck::cast_slice(&trail_verts));

                let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("trail_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                rpass.set_pipeline(&self.trail_pipeline);
                rpass.set_bind_group(0, &self.trail_cam_bg, &[]);
                rpass.set_bind_group(1, &self.white_tex_bg, &[]);
                rpass.set_vertex_buffer(0, buf.slice(..));
                rpass.draw(0..trail_verts.len() as u32, 0..1);
            }
        }
    }

    /// Pre-build the blit bind group for the given particle target view.
    /// Call this from `prepare()` so `composite()` can be called from `paint()` with `&self`.
    pub fn prepare_composite(&mut self, device: &wgpu::Device, particle_target_view: &wgpu::TextureView) {
        self.blit_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit_bg"),
            layout: &self.blit_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(particle_target_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        }));
    }

    /// Composite the pre-built particle texture onto the surface render pass.
    /// Must call `prepare_composite()` first in `prepare()`.
    pub fn composite(&self, render_pass: &mut wgpu::RenderPass<'static>) {
        if let Some(bg) = &self.blit_bind_group {
            render_pass.set_pipeline(&self.blit_pipeline);
            render_pass.set_bind_group(0, bg, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}

/// Build a triangle-strip ribbon from all active sword trails.
fn build_trail_vertices(trails: &[SwordTrail]) -> Vec<TrailVertex> {
    let mut verts = Vec::new();
    for trail in trails {
        if trail.samples.len() < 2 { continue; }
        let max_age = trail.max_samples as f32;
        for (i, sample) in trail.samples.iter().enumerate() {
            let t = i as f32 / (trail.samples.len() - 1).max(1) as f32;
            let alpha = (1.0 - sample.age / max_age).clamp(0.0, 1.0);
            verts.push(TrailVertex {
                position: sample.tip.to_array(),
                uv: [t, 0.0],
                alpha,
                _pad: 0.0,
            });
            verts.push(TrailVertex {
                position: sample.base.to_array(),
                uv: [t, 1.0],
                alpha,
                _pad: 0.0,
            });
        }
    }
    verts
}
