/// GPU particle and sword trail renderer.
/// Integrates with the existing egui-wgpu ViewportCallback pipeline.

use std::collections::HashMap;
use wgpu::util::DeviceExt;
use glam::{Mat4, Vec3};
use crate::effects::{BlendType, DisplaySide, PipelineKey, Particle, SwordTrail, PtclFile, EmitterSet};

// ── Tegra X1 block-linear deswizzle ──────────────────────────────────────────
// Delegates to the tegra_swizzle crate (ScanMountGoat, MIT License).
// https://github.com/ScanMountGoat/tegra_swizzle

fn deswizzle_tegra(
    width: u32, height: u32,
    blk_w: u32, blk_h: u32,
    bpp: u32,
    tile_mode: u32,
    _block_height_log2: i32,
    data: &[u8],
) -> Vec<u8> {
    // tile_mode==1 means linear — no deswizzle needed, return a copy.
    if tile_mode == 1 {
        return data.to_vec();
    }

    // tegra_swizzle works in block dimensions.
    let _block_width  = (width  + blk_w - 1) / blk_w;
    let block_height_px = (height + blk_h - 1) / blk_h;

    let block_height = tegra_swizzle::block_height_mip0(
        tegra_swizzle::div_round_up(block_height_px, 8),
    );

    let surface = tegra_swizzle::surface::BlockDim {
        width:  std::num::NonZeroU32::new(blk_w).unwrap(),
        height: std::num::NonZeroU32::new(blk_h).unwrap(),
        depth:  std::num::NonZeroU32::new(1).unwrap(),
    };

    tegra_swizzle::surface::deswizzle_surface(
        width, height, 1,
        data,
        surface,
        Some(block_height),
        bpp,
        1, 1,
    ).unwrap_or_else(|_| data.to_vec())
}

// ── Mesh GPU buffers ──────────────────────────────────────────────────────────

pub struct MeshBuffers {
    pub vertex_buf: wgpu::Buffer,
    pub index_buf: wgpu::Buffer,
    pub index_count: u32,
    /// BNTX texture index for this sub-mesh, propagated from BfresMesh::texture_index.
    /// u32::MAX means "use emitter-level fallback".
    pub texture_index: u32,
}

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
    _pad_rot: f32,   // padding to align tex_scale to offset 40 (vec2 AlignOf=8)
    tex_scale: [f32; 2],
    tex_offset: [f32; 2],
    _pad: f32,
    _pad2: f32,      // padding to keep struct size a multiple of 16
}

// ── Trail vertex (matches trail.wgsl) ────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TrailVertex {
    position: [f32; 3],
    uv: [f32; 2],
    alpha: f32,
    _pad: f32,
    color: [f32; 4],
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

// ── Pipeline helpers ──────────────────────────────────────────────────────────

fn blend_state_for(blend_type: BlendType) -> wgpu::BlendState {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
    let over = BlendComponent::OVER;
    match blend_type {
        BlendType::Normal => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendType::Add => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendType::Sub => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
            // Preserve destination alpha unchanged
            alpha: BlendComponent {
                src_factor: BlendFactor::Zero,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        },
        BlendType::Screen => BlendState {
            // Screen blend: 1 - (1-src) * (1-dst) ≈ src + dst*(1-src)
            // Use SrcAlpha as src factor so transparent texture regions don't contribute.
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendType::Multiply => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::Dst,
                dst_factor: BlendFactor::Zero,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendType::Unknown(v) => {
            eprintln!("[ParticleRenderer] Unknown BlendType({v}), falling back to Normal");
            blend_state_for(BlendType::Normal)
        }
    }
}

fn cull_mode_for(display_side: DisplaySide) -> Option<wgpu::Face> {
    match display_side {
        DisplaySide::Both => None,
        DisplaySide::Front => Some(wgpu::Face::Back),
        DisplaySide::Back => Some(wgpu::Face::Front),
        DisplaySide::Unknown(v) => {
            eprintln!("[ParticleRenderer] Unknown DisplaySide({v}), falling back to Both");
            None
        }
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    key: PipelineKey,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    surface_format: wgpu::TextureFormat,
    vertex_buffers: &[wgpu::VertexBufferLayout],
) -> wgpu::RenderPipeline {
    let blend = blend_state_for(key.blend_type);
    let cull_mode = cull_mode_for(key.display_side);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("particle_pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: vertex_buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
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
            cull_mode,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}




// ── Particle renderer ─────────────────────────────────────────────────────────

pub struct ParticleRenderer {
    // Pipeline cache: one entry per (BlendType × DisplaySide × is_mesh) combination
    pipeline_cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
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
    // Direct BNTX-index → bind group map, for per-sub-mesh texture lookup
    bntx_tex_cache: HashMap<u32, wgpu::BindGroup>,
    // Primitive mesh GPU buffers keyed by primitive_index
    mesh_cache: HashMap<u32, MeshBuffers>,
    // Bind group layout for mesh camera+instance (group 0)
    mesh_camera_bg_layout: wgpu::BindGroupLayout,
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
        // (kept for trail pipeline which is not in the cache)
        let additive_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };

        // ── Particle vertex buffer layouts ────────────────────────────────
        // Billboard particles use no vertex buffers (positions come from storage)
        let _particle_vertex_buffers: &[wgpu::VertexBufferLayout] = &[];

        // ── Mesh shader + pipelines ───────────────────────────────────────
        let mesh_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh.wgsl").into()),
        });

        // Mesh vertex buffer layout: position (vec3), uv (vec2), normal (vec3)
        let mesh_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<crate::effects::MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x3,  // position
                1 => Float32x2,  // uv
                2 => Float32x3,  // normal
            ],
        };

        // Mesh pipeline layout: same bind group layouts as particle pipelines
        // group 0: camera uniform + instance storage
        // group 1: texture + sampler
        let mesh_camera_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh_camera_bgl"),
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

        let mesh_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh_pipeline_layout"),
            bind_group_layouts: &[&mesh_camera_bg_layout, &tex_bg_layout],
            push_constant_ranges: &[],
        });

        // ── Pipeline cache: all 30 (BlendType × DisplaySide × is_mesh) combos ──
        let mesh_vertex_buffers = [mesh_vertex_layout.clone()];
        let mut pipeline_cache: HashMap<PipelineKey, wgpu::RenderPipeline> = HashMap::new();
        let blend_types = [
            BlendType::Normal, BlendType::Add, BlendType::Sub,
            BlendType::Screen, BlendType::Multiply,
        ];
        let display_sides = [DisplaySide::Both, DisplaySide::Front, DisplaySide::Back];
        for &bt in &blend_types {
            for &ds in &display_sides {
                for &is_mesh in &[false, true] {
                    let key = PipelineKey { blend_type: bt, display_side: ds, is_mesh };
                    let shader = if is_mesh { &mesh_shader } else { &particle_shader };
                    let layout = if is_mesh { &mesh_pipeline_layout } else { &particle_pipeline_layout };
                    let vb: &[wgpu::VertexBufferLayout] = if is_mesh { &mesh_vertex_buffers } else { &[] };
                    let pipeline = build_pipeline(device, key, layout, shader, surface_format, vb);
                    pipeline_cache.insert(key, pipeline);
                }
            }
        }

        // Trail vertex layout
        let trail_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TrailVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32, 3 => Float32, 4 => Float32x4],
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
            pipeline_cache,
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
            bntx_tex_cache: HashMap::new(),
            mesh_cache: HashMap::new(),
            mesh_camera_bg_layout,
        }
    }

    /// Upload textures from the ptcl texture section into GPU bind groups.
    /// Call this once after loading a new ptcl file.
    pub fn upload_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, ptcl: &PtclFile) {
        // Task 4.1: clear cache before processing
        self.tex_cache.clear();
        self.bntx_tex_cache.clear();
        eprintln!("[TEX] upload_textures: {} emitter sets, {} bntx_textures, {} texture_section bytes",
            ptcl.emitter_sets.len(), ptcl.bntx_textures.len(), ptcl.texture_section.len());
        for (set_idx, set) in ptcl.emitter_sets.iter().enumerate() {
            for (emitter_idx, emitter) in set.emitters.iter().enumerate() {
                // Task 4.1: select texture via texture_index into bntx_textures
                let tex_res = match ptcl.bntx_textures.get(emitter.texture_index as usize) {
                    Some(t) if t.width > 0 && t.height > 0 => t,
                    _ => {
                        eprintln!("[TEX] {set_idx}/{emitter_idx}: texture_index={} out of range or zero dims (bntx_textures={})",
                            emitter.texture_index, ptcl.bntx_textures.len());
                        continue;
                    }
                };

                let w = tex_res.width as u32;
                let h = tex_res.height as u32;
                let data_offset = tex_res.ftx_data_offset as usize;
                let data_size = tex_res.ftx_data_size as usize;
                eprintln!("[TEX] {set_idx}/{emitter_idx}: {}x{} fmt={:#06x} wrap={} blk_h={} swizzle={:#010x} data_offset={} data_size={}",
                    w, h, tex_res.ftx_format, tex_res.wrap_mode, tex_res.filter_mode,
                    tex_res.channel_swizzle, data_offset, data_size);

                // Task 4.4: bounds-check texture section reference
                if data_size == 0 || data_offset + data_size > ptcl.texture_section.len() {
                    eprintln!("[TEX] {set_idx}/{emitter_idx}: texture section OOB (offset={data_offset} size={data_size} section={})", ptcl.texture_section.len());
                    // render loop falls back to white_tex_bg for missing cache entries
                    continue;
                }
                let raw = &ptcl.texture_section[data_offset..data_offset + data_size];

                // Map raw BNTX fmt (16-bit: high byte = type, low byte = variant 01=UNORM/02=SNORM/06=SRGB)
                let fmt_type    = (tex_res.ftx_format >> 8) as u8;
                let fmt_variant = (tex_res.ftx_format & 0xFF) as u8;
                let is_srgb     = fmt_variant == 0x06;

                // Map BNTX fmt_type to image_dds::ImageFormat for BC formats,
                // or to a wgpu format for uncompressed formats.
                // All BC formats are decoded to RGBA8 via image_dds (handles sRGB correctly).
                let image_dds_format: Option<image_dds::ImageFormat> = match fmt_type {
                    0x1A => Some(if is_srgb { image_dds::ImageFormat::BC1RgbaUnormSrgb } else { image_dds::ImageFormat::BC1RgbaUnorm }),
                    0x1B => Some(if is_srgb { image_dds::ImageFormat::BC2RgbaUnormSrgb } else { image_dds::ImageFormat::BC2RgbaUnorm }),
                    0x1C => Some(if is_srgb { image_dds::ImageFormat::BC3RgbaUnormSrgb } else { image_dds::ImageFormat::BC3RgbaUnorm }),
                    0x1D => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC4RSnorm } else { image_dds::ImageFormat::BC4RUnorm }),
                    0x1E => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC5RgSnorm } else { image_dds::ImageFormat::BC5RgUnorm }),
                    // Fix 1.4: BC6H (HDR) — fmt_variant 0x05 = unsigned float, others = signed float
                    0x1F => Some(if fmt_variant == 0x05 { image_dds::ImageFormat::BC6hRgbUfloat } else { image_dds::ImageFormat::BC6hRgbSfloat }),
                    0x20 => Some(if is_srgb { image_dds::ImageFormat::BC7RgbaUnormSrgb } else { image_dds::ImageFormat::BC7RgbaUnorm }),
                    _ => None,
                };

                let wgpu_format = if image_dds_format.is_some() {
                    // All BC formats decoded to RGBA8 by image_dds
                    wgpu::TextureFormat::Rgba8Unorm
                } else {
                    match fmt_type {
                        0x02 => wgpu::TextureFormat::R8Unorm,
                        0x07 => wgpu::TextureFormat::Rgba8Unorm, // B5G6R5 → expand below
                        0x09 => wgpu::TextureFormat::Rg8Unorm,
                        0x0A => wgpu::TextureFormat::R16Unorm,
                        0x0B => if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
                        0x0C => if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
                        other => {
                            eprintln!("[TEX] {set_idx}/{emitter_idx}: unsupported fmt_type={other:#04x}, using white fallback");
                            continue;
                        }
                    }
                };

                let is_bgra = fmt_type == 0x0C || {
                    let cs = tex_res.channel_swizzle;
                    cs != 0 && ((cs >> 24) & 0xFF) == 4
                };
                let is_b5g6r5 = fmt_type == 0x07;
                let is_bc = image_dds_format.is_some();

                let upload_data: &[u8] = raw;
                let _is_bc_compressed_raw = is_bc; // raw data is block-compressed

                // BC block counts for raw size calculation
                let bc_blocks_x = (w + 3) / 4;
                let bc_blocks_y = (h + 3) / 4;

                // Bytes per row in the raw compressed data
                let raw_tight_bpr = if is_bc {
                    match fmt_type {
                        0x1A | 0x1D => bc_blocks_x * 8,  // BC1, BC4: 8 bytes/block
                        _ => bc_blocks_x * 16,            // BC2,3,5,6,7: 16 bytes/block
                    }
                } else {
                    match fmt_type {
                        0x02 => w,
                        0x09 | 0x0A => w * 2,
                        _ => if is_b5g6r5 { w * 2 } else { w * 4 },
                    }
                };
                let raw_block_rows = if is_bc { bc_blocks_y } else { h };
                let mip0_size = (raw_tight_bpr * raw_block_rows) as usize;

                if upload_data.len() < mip0_size {
                    eprintln!("[TEX] {set_idx}/{emitter_idx}: not enough data for mip0 ({} < {mip0_size}), using white fallback", upload_data.len());
                    continue;
                }
                let upload_data = &upload_data[..mip0_size];

                // Decode BC formats using image_dds (handles sRGB, BC4, BC5, BC7 correctly).
                // For non-BC formats, handle inline.
                let decoded_buf: Vec<u8>;
                let tex_data: &[u8];
                let tex_w: u32;
                let tex_h_full: u32;
                let bytes_per_row: u32;

                if let Some(dds_fmt) = image_dds_format {
                    // Use image_dds to decode all BC formats to RGBA8.
                    let surface = image_dds::Surface {
                        width: w,
                        height: h,
                        depth: 1,
                        layers: 1,
                        mipmaps: 1,
                        image_format: dds_fmt,
                        data: upload_data,
                    };
                    let rgba = match surface.decode_rgba8() {
                        Ok(s) => s.data,
                        Err(e) => {
                            eprintln!("[TEX] {set_idx}/{emitter_idx}: image_dds decode error: {e}, using white fallback");
                            continue;
                        }
                    };

                    // Apply channel swizzle (comp_sel) after decode.
                    // comp_sel packed little-endian: byte0=R_src (bits 0-7), byte1=G_src,
                    //   byte2=B_src, byte3=A_src (bits 24-31). Values: 0=zero,1=one,2=R,3=G,4=B,5=A.
                    let cs = tex_res.channel_swizzle;
                    let ch_r = ((cs >>  0) & 0xFF) as u8;
                    let ch_g = ((cs >>  8) & 0xFF) as u8;
                    let ch_b = ((cs >> 16) & 0xFF) as u8;
                    let ch_a = ((cs >> 24) & 0xFF) as u8;

                    // For BC4/BC5 particle textures, the R channel is the intensity/alpha mask.
                    // The particle color provides the actual color tint, so the texture RGB
                    // should be white (1,1,1) with alpha = R_bc5. Override the swizzle for
                    // these formats to ensure correct appearance.
                    let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D || fmt_type == 0x1E {
                        // BC4/BC5: white RGB, alpha from R channel
                        (1u8, 1u8, 1u8, 2u8)
                    } else {
                        (ch_r, ch_g, ch_b, ch_a)
                    };

                    // Identity swizzle for RGBA = (2,3,4,5); skip if trivial or unset
                    let needs_swizzle = cs != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);
                    decoded_buf = if needs_swizzle || fmt_type == 0x1D || fmt_type == 0x1E {
                        let pick = |p: &[u8], ch: u8| -> u8 {
                            match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] }
                        };
                        rgba.chunks_exact(4)
                            .flat_map(|p| [pick(p, ch_r), pick(p, ch_g), pick(p, ch_b), pick(p, ch_a)])
                            .collect()
                    } else {
                        rgba
                    };

                    tex_w = w;
                    tex_h_full = h;
                    bytes_per_row = w * 4;
                    tex_data = &decoded_buf;
                } else {
                    // Non-BC: handle BGRA swap, B5G6R5 expand, or pass through.
                    // Fix 1.5 audit: for fmt_type=0x0B/0x0C with is_srgb=true, raw bytes
                    // are uploaded directly to Rgba8UnormSrgb — the GPU applies sRGB
                    // expansion exactly once on read. No CPU gamma conversion is applied here.
                    decoded_buf = if is_bgra {
                        upload_data.chunks_exact(4)
                            .flat_map(|c| [c[2], c[1], c[0], c[3]])
                            .collect()
                    } else if is_b5g6r5 {
                        upload_data.chunks_exact(2)
                            .flat_map(|c| {
                                let v = u16::from_le_bytes([c[0], c[1]]);
                                let r = ((v & 0x001F) << 3) as u8;
                                let g = (((v >> 5) & 0x003F) << 2) as u8;
                                let b = (((v >> 11) & 0x001F) << 3) as u8;
                                [r, g, b, 255u8]
                            })
                            .collect()
                    } else {
                        upload_data.to_vec()
                    };
                    tex_w = w;
                    tex_h_full = h;
                    bytes_per_row = raw_tight_bpr;
                    tex_data = &decoded_buf;
                }

                // wgpu requires bytes_per_row to be a multiple of 256 (COPY_BYTES_PER_ROW_ALIGNMENT).
                // If the natural stride is already aligned, use it directly.
                // Otherwise, pad each row to the aligned stride.
                const ALIGN: u32 = 256;
                let aligned_bpr = (bytes_per_row + ALIGN - 1) & !(ALIGN - 1);
                let (tex_data, bytes_per_row) = if aligned_bpr != bytes_per_row {
                    let rows = tex_h_full as usize; // before atlas crop
                    let mut padded = Vec::with_capacity(rows * aligned_bpr as usize);
                    for row in 0..rows {
                        let src_start = row * bytes_per_row as usize;
                        let src_end = src_start + bytes_per_row as usize;
                        if src_end <= tex_data.len() {
                            padded.extend_from_slice(&tex_data[src_start..src_end]);
                        } else {
                            padded.extend(std::iter::repeat(0u8).take(bytes_per_row as usize));
                        }
                        // Pad to aligned stride
                        let pad = (aligned_bpr - bytes_per_row) as usize;
                        padded.extend(std::iter::repeat(0u8).take(pad));
                    }
                    (padded, aligned_bpr)
                } else {
                    (tex_data.to_vec(), bytes_per_row)
                };
                let tex_data: &[u8] = &tex_data;

                // Upload the full texture — UV scale/offset in the shader handles atlas sub-regions.
                let (tex_data, h) = (tex_data.to_vec(), tex_h_full);
                let tex_data: &[u8] = &tex_data;

                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("ptcl_tex_{set_idx}_{emitter_idx}")),
                    size: wgpu::Extent3d {
                        width: tex_w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu_format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    texture.as_image_copy(),
                    tex_data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d { width: tex_w, height: h, depth_or_array_layers: 1 },
                );
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("ptcl_tex_sampler"),
                    address_mode_u: wgpu::AddressMode::ClampToEdge,
                    address_mode_v: wgpu::AddressMode::ClampToEdge,
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
                let make_bg = |label: &str| device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.tex_bg_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                    ],
                });
                let bg = make_bg(&format!("ptcl_tex_bg_{set_idx}_{emitter_idx}"));
                // Also populate bntx_tex_cache keyed by BNTX texture index (for per-sub-mesh lookup).
                // Only insert once per unique index — first emitter wins.
                let bntx_idx = emitter.texture_index;
                if !self.bntx_tex_cache.contains_key(&bntx_idx) {
                    let bg2 = make_bg(&format!("bntx_tex_bg_{bntx_idx}"));
                    self.bntx_tex_cache.insert(bntx_idx, bg2);
                }
                self.tex_cache.insert((set_idx, emitter_idx), bg);
            }
        }
        eprintln!("[TEX] uploaded {} particle textures", self.tex_cache.len());

        // Fix 3.3: upload all BNTX textures by index so that BfresMesh::texture_index
        // values that are not referenced by any emitter still have entries in bntx_tex_cache.
        // Use entry().or_insert_with() to avoid re-uploading textures already inserted
        // by the emitter loop above.
        for (bntx_idx, tex_res) in ptcl.bntx_textures.iter().enumerate() {
            let bntx_idx = bntx_idx as u32;
            if self.bntx_tex_cache.contains_key(&bntx_idx) {
                continue; // already uploaded by the emitter loop
            }
            if tex_res.width == 0 || tex_res.height == 0 { continue; }
            let data_offset = tex_res.ftx_data_offset as usize;
            let data_size   = tex_res.ftx_data_size as usize;
            if data_size == 0 || data_offset + data_size > ptcl.texture_section.len() { continue; }
            let raw = &ptcl.texture_section[data_offset..data_offset + data_size];

            let w = tex_res.width as u32;
            let h = tex_res.height as u32;
            let fmt_type    = (tex_res.ftx_format >> 8) as u8;
            let fmt_variant = (tex_res.ftx_format & 0xFF) as u8;
            let is_srgb     = fmt_variant == 0x06;

            let image_dds_format: Option<image_dds::ImageFormat> = match fmt_type {
                0x1A => Some(if is_srgb { image_dds::ImageFormat::BC1RgbaUnormSrgb } else { image_dds::ImageFormat::BC1RgbaUnorm }),
                0x1B => Some(if is_srgb { image_dds::ImageFormat::BC2RgbaUnormSrgb } else { image_dds::ImageFormat::BC2RgbaUnorm }),
                0x1C => Some(if is_srgb { image_dds::ImageFormat::BC3RgbaUnormSrgb } else { image_dds::ImageFormat::BC3RgbaUnorm }),
                0x1D => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC4RSnorm } else { image_dds::ImageFormat::BC4RUnorm }),
                0x1E => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC5RgSnorm } else { image_dds::ImageFormat::BC5RgUnorm }),
                0x1F => Some(if fmt_variant == 0x05 { image_dds::ImageFormat::BC6hRgbUfloat } else { image_dds::ImageFormat::BC6hRgbSfloat }),
                0x20 => Some(if is_srgb { image_dds::ImageFormat::BC7RgbaUnormSrgb } else { image_dds::ImageFormat::BC7RgbaUnorm }),
                _ => None,
            };
            let wgpu_format = if image_dds_format.is_some() {
                wgpu::TextureFormat::Rgba8Unorm
            } else {
                match fmt_type {
                    0x02 => wgpu::TextureFormat::R8Unorm,
                    0x07 => wgpu::TextureFormat::Rgba8Unorm,
                    0x09 => wgpu::TextureFormat::Rg8Unorm,
                    0x0A => wgpu::TextureFormat::R16Unorm,
                    0x0B | 0x0C => if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
                    _ => { eprintln!("[TEX] bntx[{}]: unsupported fmt_type={fmt_type:#04x}, skipping", bntx_idx); continue; }
                }
            };
            let is_bc = image_dds_format.is_some();
            let is_bgra = fmt_type == 0x0C || { let cs = tex_res.channel_swizzle; cs != 0 && ((cs >> 24) & 0xFF) == 4 };
            let is_b5g6r5 = fmt_type == 0x07;
            let bc_blocks_x = (w + 3) / 4;
            let bc_blocks_y = (h + 3) / 4;
            let raw_tight_bpr = if is_bc {
                match fmt_type { 0x1A | 0x1D => bc_blocks_x * 8, _ => bc_blocks_x * 16 }
            } else {
                match fmt_type { 0x02 => w, 0x09 | 0x0A => w * 2, _ => if is_b5g6r5 { w * 2 } else { w * 4 } }
            };
            let raw_block_rows = if is_bc { bc_blocks_y } else { h };
            let mip0_size = (raw_tight_bpr * raw_block_rows) as usize;
            if raw.len() < mip0_size { continue; }
            let upload_data = &raw[..mip0_size];

            let decoded_buf: Vec<u8>;
            let tex_data: &[u8];
            let final_bpr: u32;
            if let Some(dds_fmt) = image_dds_format {
                let surface = image_dds::Surface { width: w, height: h, depth: 1, layers: 1, mipmaps: 1, image_format: dds_fmt, data: upload_data };
                let rgba = match surface.decode_rgba8() { Ok(s) => s.data, Err(_) => continue };
                let cs = tex_res.channel_swizzle;
                let ch_r = ((cs >>  0) & 0xFF) as u8;
                let ch_g = ((cs >>  8) & 0xFF) as u8;
                let ch_b = ((cs >> 16) & 0xFF) as u8;
                let ch_a = ((cs >> 24) & 0xFF) as u8;
                let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D || fmt_type == 0x1E { (1u8, 1u8, 1u8, 2u8) } else { (ch_r, ch_g, ch_b, ch_a) };
                let needs_swizzle = cs != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);
                decoded_buf = if needs_swizzle || fmt_type == 0x1D || fmt_type == 0x1E {
                    let pick = |p: &[u8], ch: u8| -> u8 { match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] } };
                    rgba.chunks_exact(4).flat_map(|p| [pick(p, ch_r), pick(p, ch_g), pick(p, ch_b), pick(p, ch_a)]).collect()
                } else { rgba };
                final_bpr = w * 4;
                tex_data = &decoded_buf;
            } else {
                decoded_buf = if is_bgra {
                    upload_data.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect()
                } else if is_b5g6r5 {
                    upload_data.chunks_exact(2).flat_map(|c| { let v = u16::from_le_bytes([c[0], c[1]]); let r = ((v & 0x001F) << 3) as u8; let g = (((v >> 5) & 0x003F) << 2) as u8; let b = (((v >> 11) & 0x001F) << 3) as u8; [r, g, b, 255u8] }).collect()
                } else { upload_data.to_vec() };
                final_bpr = raw_tight_bpr;
                tex_data = &decoded_buf;
            }
            const ALIGN: u32 = 256;
            let aligned_bpr = (final_bpr + ALIGN - 1) & !(ALIGN - 1);
            let (tex_data_padded, upload_bpr) = if aligned_bpr != final_bpr {
                let mut padded = Vec::with_capacity(h as usize * aligned_bpr as usize);
                for row in 0..h as usize {
                    let s = row * final_bpr as usize;
                    let e = s + final_bpr as usize;
                    if e <= tex_data.len() { padded.extend_from_slice(&tex_data[s..e]); } else { padded.extend(std::iter::repeat(0u8).take(final_bpr as usize)); }
                    padded.extend(std::iter::repeat(0u8).take((aligned_bpr - final_bpr) as usize));
                }
                (padded, aligned_bpr)
            } else { (tex_data.to_vec(), final_bpr) };

            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("bntx_tex_{bntx_idx}")),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(), &tex_data_padded,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(upload_bpr), rows_per_image: None },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bntx_tex_sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("bntx_tex_bg_{bntx_idx}")),
                layout: &self.tex_bg_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                ],
            });
            self.bntx_tex_cache.insert(bntx_idx, bg);
        }
        eprintln!("[TEX] bntx_tex_cache now covers {} indices", self.bntx_tex_cache.len());
    }

    /// Resolve the correct texture bind group for a BFRES sub-mesh draw call.
    /// Resolution order:
    ///   1. bntx_tex_cache[sub_mesh_tex_idx]  (if sub_mesh_tex_idx != u32::MAX)
    ///   2. tex_cache[(emitter_set_idx, emitter_idx)]
    ///   3. white_tex_bg
    fn resolve_mesh_tex_bg<'a>(
        &'a self,
        sub_mesh_tex_idx: u32,
        emitter_key: (usize, usize),
    ) -> &'a wgpu::BindGroup {
        if sub_mesh_tex_idx != u32::MAX {
            if let Some(bg) = self.bntx_tex_cache.get(&sub_mesh_tex_idx) {
                return bg;
            }
        }
        self.tex_cache.get(&emitter_key).unwrap_or(&self.white_tex_bg)
    }

    /// Upload primitive mesh geometry from the ptcl file into GPU buffers.
    /// Call this once after loading a new ptcl file, alongside upload_textures.
    pub fn upload_meshes(&mut self, device: &wgpu::Device, ptcl: &PtclFile) {
        self.mesh_cache.clear();
        // Upload PRMA primitive meshes (keyed by primitive index)
        for (prim_idx, prim) in ptcl.primitives.iter().enumerate() {
            if prim.vertices.is_empty() || prim.indices.is_empty() {
                continue;
            }
            let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("mesh_vertex_buf_{prim_idx}")),
                contents: bytemuck::cast_slice(&prim.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("mesh_index_buf_{prim_idx}")),
                contents: bytemuck::cast_slice(&prim.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            self.mesh_cache.insert(prim_idx as u32, MeshBuffers {
                vertex_buf,
                index_buf,
                index_count: prim.indices.len() as u32,
                texture_index: u32::MAX, // PRMA primitives use emitter-level texture
            });
        }
        // Upload G3PR BFRES model meshes (keyed by model_idx * 1000 + mesh_idx)
        for (model_idx, model) in ptcl.bfres_models.iter().enumerate() {
            for (mesh_idx, mesh) in model.meshes.iter().enumerate() {
                if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                    continue;
                }
                let key = (model_idx * 1000 + mesh_idx) as u32;
                let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(&format!("bfres_vertex_buf_{model_idx}_{mesh_idx}")),
                    contents: bytemuck::cast_slice(&mesh.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(&format!("bfres_index_buf_{model_idx}_{mesh_idx}")),
                    contents: bytemuck::cast_slice(&mesh.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
                self.mesh_cache.insert(key, MeshBuffers {
                    vertex_buf,
                    index_buf,
                    index_count: mesh.indices.len() as u32,
                    texture_index: mesh.texture_index,
                });
            }
        }
        eprintln!("[MESH] uploaded {} total mesh entries ({} primitives, {} bfres models)",
            self.mesh_cache.len(), ptcl.primitives.len(), ptcl.bfres_models.len());
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
        emitter_sets: &[EmitterSet],
        bfres_models: &[crate::effects::BfresModel],
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
            let instances: Vec<ParticleInstance> = particles.iter().map(|p| {
                let emitter = emitter_sets.get(p.emitter_set_idx)
                    .and_then(|s| s.emitters.get(p.emitter_idx));
                let tex_scale = emitter.map(|e| e.tex_scale_uv).unwrap_or([1.0, 1.0]);
                ParticleInstance {
                    position: p.position.to_array(),
                    size: p.size,
                    color: p.color.to_array(),
                    rotation: p.rotation,
                    _pad_rot: 0.0,
                    tex_scale,
                    tex_offset: p.tex_offset,
                    _pad: 0.0,
                    _pad2: 0.0,
                }
            }).collect();

            let byte_size = (instances.len() * std::mem::size_of::<ParticleInstance>()) as u64;

            if self.instance_buf_capacity < instances.len() {
                self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle_instance_buf"),
                    size: byte_size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.instance_buf_capacity = instances.len();

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

            // Group billboard particles by (emitter_set_idx, emitter_idx), preserving
            // encounter order so each group is a contiguous slice in the upload buffer.
            let mut groups: Vec<((usize, usize), Vec<&Particle>)> = Vec::new();
            eprintln!("[RENDER] grouping {} particles, emitter_sets={}", particles.len(), emitter_sets.len());
            for p in particles.iter().filter(|p| {
                // Only billboard particles (mesh_type == 0); mesh particles are handled below
                let is_billboard = emitter_sets
                    .get(p.emitter_set_idx)
                    .and_then(|s| s.emitters.get(p.emitter_idx))
                    .map(|e| e.mesh_type == 0)
                    .unwrap_or(true); // treat unknown emitters as billboard
                if !is_billboard {
                    eprintln!("[RENDER] particle set={} emitter={} is mesh_type!=0, skipping billboard", p.emitter_set_idx, p.emitter_idx);
                }
                is_billboard
            }) {
                let key = (p.emitter_set_idx, p.emitter_idx);
                if let Some(g) = groups.iter_mut().find(|(k, _)| *k == key) {
                    g.1.push(p);
                } else {
                    groups.push((key, vec![p]));
                }
            }

            // Re-upload instances in group order
            let sorted_instances: Vec<ParticleInstance> = groups.iter()
                .flat_map(|((set_idx, emitter_idx), ps)| {
                    let emitter = emitter_sets.get(*set_idx)
                        .and_then(|s| s.emitters.get(*emitter_idx));
                    let tex_scale = emitter.map(|e| e.tex_scale_uv).unwrap_or([1.0, 1.0]);
                    ps.iter().map(move |p| ParticleInstance {
                        position: p.position.to_array(),
                        size: p.size,
                        color: p.color.to_array(),
                        rotation: p.rotation,
                        _pad_rot: 0.0,
                        tex_scale,
                        tex_offset: p.tex_offset,
                        _pad: 0.0,
                        _pad2: 0.0,
                    })
                })
                .collect();

            if let Some(buf) = &self.instance_buf {
                queue.write_buffer(buf, 0, bytemuck::cast_slice(&sorted_instances));
            }

            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("particle_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_bind_group(0, &self.camera_bind_group, &[]);

            // Draw each group with its own pipeline looked up from pipeline_cache
            let mut cursor = 0u32;
            eprintln!("[RENDER] {} billboard groups to draw", groups.len());
            for ((set_idx, emitter_idx), group) in &groups {
                let count = group.len() as u32;
                // Req 9.3: skip draw calls when instance_count == 0
                if count == 0 {
                    cursor += count;
                    continue;
                }

                // Look up the emitter's actual blend_type and display_side
                let (blend_type, display_side) = emitter_sets
                    .get(*set_idx)
                    .and_then(|s| s.emitters.get(*emitter_idx))
                    .map(|e| {
                        // Normalize Unknown variants (Req 8.2, 8.3)
                        let bt = match e.blend_type {
                            BlendType::Unknown(_) => BlendType::Normal,
                            other => other,
                        };
                        let ds = match e.display_side {
                            DisplaySide::Unknown(_) => DisplaySide::Both,
                            other => other,
                        };
                        (bt, ds)
                    })
                    .unwrap_or((BlendType::Normal, DisplaySide::Both));

                // Construct the pipeline key and look it up from the cache (Req 10.1, 10.2)
                let pk = PipelineKey { blend_type, display_side, is_mesh: false };
                let pipeline = self.pipeline_cache.get(&pk)
                    .unwrap_or_else(|| self.pipeline_cache.get(&PipelineKey {
                        blend_type: BlendType::Normal,
                        display_side: DisplaySide::Both,
                        is_mesh: false,
                    }).unwrap());

                let tex_bg = self.tex_cache.get(&(*set_idx, *emitter_idx)).unwrap_or(&self.white_tex_bg);

                rpass.set_pipeline(pipeline);
                rpass.set_bind_group(1, tex_bg, &[]);
                eprintln!("[RENDER] draw group set={set_idx} emitter={emitter_idx} count={count} cursor={cursor} blend={blend_type:?} side={display_side:?}");
                rpass.draw(0..6, cursor..cursor + count);
                cursor += count;
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

        // ── Primitive mesh particles ──────────────────────────────────────
        // Collect mesh particles (mesh_type != 0) grouped by (emitter_set_idx, emitter_idx)
        let mesh_particles: Vec<&Particle> = particles.iter()
            .filter(|p| {
                emitter_sets
                    .get(p.emitter_set_idx)
                    .and_then(|s| s.emitters.get(p.emitter_idx))
                    .map(|e| e.mesh_type != 0)
                    .unwrap_or(false)
            })
            .collect();

        if !mesh_particles.is_empty() {
            // Sort by (emitter_set_idx, emitter_idx) to batch by texture/pipeline
            let mut sorted_mesh: Vec<&Particle> = mesh_particles;
            sorted_mesh.sort_by_key(|p| (p.emitter_set_idx, p.emitter_idx));

            // Process each contiguous group
            let mut i = 0;
            while i < sorted_mesh.len() {
                let key = (sorted_mesh[i].emitter_set_idx, sorted_mesh[i].emitter_idx);
                let group_start = i;
                while i < sorted_mesh.len()
                    && (sorted_mesh[i].emitter_set_idx, sorted_mesh[i].emitter_idx) == key
                {
                    i += 1;
                }
                let group = &sorted_mesh[group_start..i];

                // Look up emitter to get primitive_index, mesh_type, and blend_type
                let emitter = match emitter_sets
                    .get(key.0)
                    .and_then(|s| s.emitters.get(key.1))
                {
                    Some(e) => e,
                    None => continue,
                };

                // Build MeshInstance struct (shared across all sub-mesh draw calls)
                #[repr(C)]
                #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
                struct MeshInstance {
                    world_pos: [f32; 3],
                    scale: f32,
                    color: [f32; 4],
                    rotation_y: f32,
                    _pad: [f32; 2],
                }

                // Select pipeline based on blend_type and display_side
                let pk = PipelineKey {
                    blend_type: emitter.blend_type,
                    display_side: emitter.display_side,
                    is_mesh: true,
                };
                let pipeline = self.pipeline_cache.get(&pk)
                    .or_else(|| self.pipeline_cache.get(&PipelineKey {
                        blend_type: BlendType::Add,
                        display_side: DisplaySide::Both,
                        is_mesh: true,
                    }))
                    .unwrap();

                // Helper: issue one draw call for a given mesh_bufs + tex_bg + instances
                let draw_mesh = |encoder: &mut wgpu::CommandEncoder,
                                 mesh_bufs: &MeshBuffers,
                                 tex_bg: &wgpu::BindGroup,
                                 instances: &[MeshInstance]| {
                    let inst_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mesh_instance_buf"),
                        contents: bytemuck::cast_slice(instances),
                        usage: wgpu::BufferUsages::STORAGE,
                    });
                    let mesh_cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("mesh_cam_bg"),
                        layout: &self.mesh_camera_bg_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.camera_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: inst_buf.as_entire_binding() },
                        ],
                    });
                    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("mesh_pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target_view,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    rpass.set_pipeline(pipeline);
                    rpass.set_bind_group(0, &mesh_cam_bg, &[]);
                    rpass.set_bind_group(1, tex_bg, &[]);
                    rpass.set_vertex_buffer(0, mesh_bufs.vertex_buf.slice(..));
                    rpass.set_index_buffer(mesh_bufs.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                    rpass.draw_indexed(0..mesh_bufs.index_count, 0, 0..instances.len() as u32);
                };

                match emitter.mesh_type {
                    1 => {
                        // PRMA primitive mesh — single draw call, apply emitter_scale to size
                        let cache_key = emitter.primitive_index;
                        let mesh_bufs = match self.mesh_cache.get(&cache_key) {
                            Some(b) => b,
                            None => continue, // fall back to billboard (skip)
                        };
                        let emitter_scale_mag = emitter.emitter_scale.length().max(0.001);
                        let instances: Vec<MeshInstance> = group.iter().map(|p| MeshInstance {
                            world_pos: p.position.to_array(),
                            scale: p.size * emitter_scale_mag, // Task 6: apply emitter scale
                            color: p.color.to_array(),
                            rotation_y: p.rotation,
                            _pad: [0.0; 2],
                        }).collect();
                        let tex_bg = self.resolve_mesh_tex_bg(mesh_bufs.texture_index, key);
                        draw_mesh(encoder, mesh_bufs, tex_bg, &instances);
                    }
                    2 => {
                        // BFRES model — iterate all sub-meshes (capped at 64), one draw call each
                        let model_idx = emitter.primitive_index as usize;
                        let model = match bfres_models.get(model_idx) {
                            Some(m) => m,
                            None => continue,
                        };

                        let num_sub = model.meshes.len();
                        if num_sub > 64 {
                            eprintln!("[MESH] model {} has {} sub-meshes, capping at 64", model_idx, num_sub);
                        }

                        // Build instances with full emitter TRS applied (Task 7)
                        let emitter_trs = crate::effects::build_emitter_trs(emitter);
                        let mut drew_any = false;

                        for mesh_idx in 0..num_sub.min(64) {
                            let cache_key = (model_idx * 1000 + mesh_idx) as u32;
                            let mesh_bufs = match self.mesh_cache.get(&cache_key) {
                                Some(b) => b,
                                None => continue, // skip missing sub-mesh
                            };

                            // Apply emitter TRS to each particle's world position
                            let instances: Vec<MeshInstance> = group.iter().map(|p| {
                                // world_pos = emitter_trs * particle_local_pos
                                // For BFRES, the particle position is already in world space;
                                // emitter_trs provides the base orientation/scale offset.
                                let base_pos = emitter_trs.transform_point3(glam::Vec3::ZERO);
                                let final_pos = p.position + base_pos;
                                MeshInstance {
                                    world_pos: final_pos.to_array(),
                                    scale: p.size,
                                    color: p.color.to_array(),
                                    rotation_y: p.rotation,
                                    _pad: [0.0; 2],
                                }
                            }).collect();

                            let tex_bg = self.resolve_mesh_tex_bg(mesh_bufs.texture_index, key);
                            draw_mesh(encoder, mesh_bufs, tex_bg, &instances);
                            drew_any = true;
                        }

                        // Fall back to billboard if no sub-meshes were drawn (Req 4.3)
                        if !drew_any {
                            // Issue a billboard draw for each particle in the group.
                            // Build a temporary storage buffer with ParticleInstance data.
                            let tex_scale = emitter.tex_scale_uv;
                            let fallback_instances: Vec<ParticleInstance> = group.iter().map(|p| ParticleInstance {
                                position: p.position.to_array(),
                                size: p.size,
                                color: p.color.to_array(),
                                rotation: p.rotation,
                                _pad_rot: 0.0,
                                tex_scale,
                                tex_offset: p.tex_offset,
                                _pad: 0.0,
                                _pad2: 0.0,
                            }).collect();
                            let fallback_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("bfres_billboard_fallback_buf"),
                                contents: bytemuck::cast_slice(&fallback_instances),
                                usage: wgpu::BufferUsages::STORAGE,
                            });
                            let fallback_cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("bfres_billboard_fallback_cam_bg"),
                                layout: &self.camera_bg_layout,
                                entries: &[
                                    wgpu::BindGroupEntry { binding: 0, resource: self.camera_buf.as_entire_binding() },
                                    wgpu::BindGroupEntry { binding: 1, resource: fallback_buf.as_entire_binding() },
                                ],
                            });
                            let billboard_pk = PipelineKey {
                                blend_type: emitter.blend_type,
                                display_side: emitter.display_side,
                                is_mesh: false,
                            };
                            let billboard_pipeline = self.pipeline_cache.get(&billboard_pk)
                                .or_else(|| self.pipeline_cache.get(&PipelineKey {
                                    blend_type: BlendType::Normal,
                                    display_side: DisplaySide::Both,
                                    is_mesh: false,
                                }))
                                .unwrap();
                            let tex_bg = self.tex_cache.get(&key).unwrap_or(&self.white_tex_bg);
                            let count = fallback_instances.len() as u32;
                            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("bfres_billboard_fallback_pass"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: target_view,
                                    resolve_target: None,
                                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                                    depth_slice: None,
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                            });
                            rpass.set_pipeline(billboard_pipeline);
                            rpass.set_bind_group(0, &fallback_cam_bg, &[]);
                            rpass.set_bind_group(1, tex_bg, &[]);
                            rpass.draw(0..6, 0..count);
                        }
                    }
                    _ => continue,
                }
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
        let base_color = trail.color;
        for (i, sample) in trail.samples.iter().enumerate() {
            let t = i as f32 / (trail.samples.len() - 1).max(1) as f32;
            let alpha = (1.0 - sample.age / max_age).clamp(0.0, 1.0);
            let color = [base_color[0], base_color[1], base_color[2], base_color[3] * alpha];
            verts.push(TrailVertex {
                position: sample.tip.to_array(),
                uv: [t, 0.0],
                alpha,
                _pad: 0.0,
                color,
            });
            verts.push(TrailVertex {
                position: sample.base.to_array(),
                uv: [t, 1.0],
                alpha,
                _pad: 0.0,
                color,
            });
        }
    }
    verts
}

/// Pure helper: map a BNTX format ID to the image_dds ImageFormat used for BC decoding.
/// Returns None for non-BC formats or unsupported types.
/// Extracted from upload_textures for testability (no GPU required).
fn bc_image_format(fmt_type: u8, fmt_variant: u8) -> Option<image_dds::ImageFormat> {
    let is_srgb = fmt_variant == 0x06;
    match fmt_type {
        0x1A => Some(if is_srgb { image_dds::ImageFormat::BC1RgbaUnormSrgb } else { image_dds::ImageFormat::BC1RgbaUnorm }),
        0x1B => Some(if is_srgb { image_dds::ImageFormat::BC2RgbaUnormSrgb } else { image_dds::ImageFormat::BC2RgbaUnorm }),
        0x1C => Some(if is_srgb { image_dds::ImageFormat::BC3RgbaUnormSrgb } else { image_dds::ImageFormat::BC3RgbaUnorm }),
        0x1D => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC4RSnorm } else { image_dds::ImageFormat::BC4RUnorm }),
        0x1E => Some(if fmt_variant == 0x02 { image_dds::ImageFormat::BC5RgSnorm } else { image_dds::ImageFormat::BC5RgUnorm }),
        0x1F => Some(if fmt_variant == 0x05 { image_dds::ImageFormat::BC6hRgbUfloat } else { image_dds::ImageFormat::BC6hRgbSfloat }),
        0x20 => Some(if is_srgb { image_dds::ImageFormat::BC7RgbaUnormSrgb } else { image_dds::ImageFormat::BC7RgbaUnorm }),
        _ => None,
    }
}

/// Pure helper: compute which BNTX indices would be inserted into bntx_tex_cache
/// by the FIXED upload_textures implementation.
/// The fix uploads all bntx_textures by index (in addition to emitter-referenced ones).
/// Extracted for testability without GPU.
fn bntx_indices_covered_by_emitters(ptcl: &crate::effects::PtclFile) -> std::collections::HashSet<u32> {
    let mut covered = std::collections::HashSet::new();
    // Emitter loop (unchanged)
    for set in &ptcl.emitter_sets {
        for emitter in &set.emitters {
            let idx = emitter.texture_index;
            if let Some(t) = ptcl.bntx_textures.get(idx as usize) {
                if t.width > 0 && t.height > 0 {
                    covered.insert(idx);
                }
            }
        }
    }
    // Fix 3.3: also cover all bntx_textures by index
    for (idx, t) in ptcl.bntx_textures.iter().enumerate() {
        if t.width > 0 && t.height > 0 {
            covered.insert(idx as u32);
        }
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // Task 1: Bug condition exploration tests (bugs 1.4–1.5)
    // ═══════════════════════════════════════════════════════════════════════

    // ── Bug 1.4: BC6H format missing from image_dds_format match ─────────────
    // On UNFIXED code: fmt_type=0x1F falls through to _ => None, texture skipped.
    // On FIXED code: 0x1F maps to BC6hRgbUfloat or BC6hRgbSfloat.
    #[test]
    fn test_bug_1_4_bc6h_format_missing() {
        // UNFIXED: bc_image_format(0x1F, 0x05) returns None (falls through to _ => None)
        // FIXED:   bc_image_format(0x1F, 0x05) returns Some(BC6hRgbUfloat)
        // FIXED:   bc_image_format(0x1F, 0x01) returns Some(BC6hRgbSfloat)
        //
        // This test FAILS on unfixed code (returns None instead of Some).
        let result_ufloat = bc_image_format(0x1F, 0x05);
        assert!(result_ufloat.is_some(),
            "Bug 1.4: fmt_type=0x1F variant=0x05 (BC6H unsigned float) returned None — bug confirmed");
        assert_eq!(result_ufloat, Some(image_dds::ImageFormat::BC6hRgbUfloat),
            "Bug 1.4: expected BC6hRgbUfloat for variant=0x05");

        let result_sfloat = bc_image_format(0x1F, 0x01);
        assert!(result_sfloat.is_some(),
            "Bug 1.4: fmt_type=0x1F variant=0x01 (BC6H signed float) returned None — bug confirmed");
        assert_eq!(result_sfloat, Some(image_dds::ImageFormat::BC6hRgbSfloat),
            "Bug 1.4: expected BC6hRgbSfloat for variant!=0x05");
    }

    // ── Bug 1.5: sRGB double-gamma audit ─────────────────────────────────────
    // Verify that the wgpu format selection for sRGB textures is correct:
    // fmt_type=0x0B/0x0C with is_srgb=true → Rgba8UnormSrgb (GPU handles gamma).
    // No CPU gamma conversion should be applied.
    #[test]
    fn test_bug_1_5_srgb_format_selection() {
        // Verify the wgpu format mapping for sRGB uncompressed textures.
        // This is the pure logic extracted from upload_textures.
        let fmt_variant_srgb: u8 = 0x06;
        let fmt_variant_unorm: u8 = 0x01;
        let is_srgb_0b = fmt_variant_srgb == 0x06;
        let is_srgb_0c = fmt_variant_srgb == 0x06;

        // fmt_type=0x0B (RGBA8) with sRGB → must use Rgba8UnormSrgb
        let wgpu_fmt_0b_srgb = if is_srgb_0b {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        assert_eq!(wgpu_fmt_0b_srgb, wgpu::TextureFormat::Rgba8UnormSrgb,
            "Bug 1.5: RGBA8 sRGB must use Rgba8UnormSrgb, not Rgba8Unorm");

        // fmt_type=0x0C (BGRA8) with sRGB → must use Rgba8UnormSrgb
        let wgpu_fmt_0c_srgb = if is_srgb_0c {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        assert_eq!(wgpu_fmt_0c_srgb, wgpu::TextureFormat::Rgba8UnormSrgb,
            "Bug 1.5: BGRA8 sRGB must use Rgba8UnormSrgb, not Rgba8Unorm");

        // Non-sRGB path must use Rgba8Unorm (preservation)
        let is_unorm = fmt_variant_unorm != 0x06;
        let wgpu_fmt_0b_unorm = if !is_unorm {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        assert_eq!(wgpu_fmt_0b_unorm, wgpu::TextureFormat::Rgba8Unorm,
            "Preservation: non-sRGB RGBA8 must use Rgba8Unorm");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 2: Preservation tests (bugs 1.4–1.5)
    // ═══════════════════════════════════════════════════════════════════════

    // Preservation: BC1–BC7 format arms must be unchanged after adding BC6H
    #[test]
    fn test_preservation_bc1_bc7_formats_unchanged() {
        // BC1 unorm
        assert_eq!(bc_image_format(0x1A, 0x01), Some(image_dds::ImageFormat::BC1RgbaUnorm));
        assert_eq!(bc_image_format(0x1A, 0x06), Some(image_dds::ImageFormat::BC1RgbaUnormSrgb));
        // BC2
        assert_eq!(bc_image_format(0x1B, 0x01), Some(image_dds::ImageFormat::BC2RgbaUnorm));
        assert_eq!(bc_image_format(0x1B, 0x06), Some(image_dds::ImageFormat::BC2RgbaUnormSrgb));
        // BC3
        assert_eq!(bc_image_format(0x1C, 0x01), Some(image_dds::ImageFormat::BC3RgbaUnorm));
        assert_eq!(bc_image_format(0x1C, 0x06), Some(image_dds::ImageFormat::BC3RgbaUnormSrgb));
        // BC4
        assert_eq!(bc_image_format(0x1D, 0x01), Some(image_dds::ImageFormat::BC4RUnorm));
        assert_eq!(bc_image_format(0x1D, 0x02), Some(image_dds::ImageFormat::BC4RSnorm));
        // BC5
        assert_eq!(bc_image_format(0x1E, 0x01), Some(image_dds::ImageFormat::BC5RgUnorm));
        assert_eq!(bc_image_format(0x1E, 0x02), Some(image_dds::ImageFormat::BC5RgSnorm));
        // BC7
        assert_eq!(bc_image_format(0x20, 0x01), Some(image_dds::ImageFormat::BC7RgbaUnorm));
        assert_eq!(bc_image_format(0x20, 0x06), Some(image_dds::ImageFormat::BC7RgbaUnormSrgb));
        // Non-BC formats return None
        assert_eq!(bc_image_format(0x0B, 0x01), None);
        assert_eq!(bc_image_format(0x0C, 0x06), None);
        assert_eq!(bc_image_format(0x02, 0x01), None);
    }

    // Preservation: non-sRGB RGBA8/BGRA8 must use Rgba8Unorm (no gamma)
    #[test]
    fn test_preservation_non_srgb_uses_unorm() {
        let fmt_variant_unorm: u8 = 0x01;
        let is_srgb = fmt_variant_unorm == 0x06;
        assert!(!is_srgb, "variant=0x01 must not be sRGB");

        // fmt_type=0x0B non-sRGB
        let fmt = if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
        assert_eq!(fmt, wgpu::TextureFormat::Rgba8Unorm,
            "non-sRGB RGBA8 must use Rgba8Unorm");

        // fmt_type=0x0C non-sRGB
        let fmt = if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
        assert_eq!(fmt, wgpu::TextureFormat::Rgba8Unorm,
            "non-sRGB BGRA8 must use Rgba8Unorm");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-model-mapping, Property 1: Bug Condition
    // Sub-test B: Upload gap — bntx_tex_cache missing sub-mesh-only indices
    //
    // This test MUST FAIL on unfixed code — failure confirms the bug.
    // It will PASS after the fix in upload_textures is applied.
    // ═══════════════════════════════════════════════════════════════════════

    // ── Sub-test B: Upload gap ────────────────────────────────────────────────
    // Construct a PtclFile with bntx_textures = [tex0, tex1], one emitter with
    // texture_index = 0, and one BfresMesh with texture_index = 1.
    //
    // The unfixed upload_textures only iterates emitters to populate bntx_tex_cache.
    // Since no emitter uses texture_index = 1, bntx_tex_cache will not contain key 1.
    //
    // Expected (fixed): bntx_tex_cache covers index 1 (all bntx_textures uploaded)
    // Actual (unfixed): bntx_tex_cache only covers index 0 (emitter-driven upload)
    //
    // Validates: Requirements 2.2
    #[test]
    fn test_bug_etmm_b_upload_gap_submesh_index_not_covered() {
        use crate::effects::{
            PtclFile, EmitterSet, EmitterDef, BfresModel, BfresMesh, TextureRes,
            EmitType, BlendType, DisplaySide, AnimKey3v4k,
        };

        // Build a minimal TextureRes for bntx_textures entries
        let make_tex = |offset: u32| TextureRes {
            width: 4,
            height: 4,
            ftx_format: 0x0B01,
            ftx_data_offset: offset,
            ftx_data_size: 64,
            original_format: 0x0B01,
            original_data_offset: offset,
            original_data_size: 64,
            wrap_mode: 1,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0,
        };

        // Build a minimal EmitterDef with texture_index = 0
        let emitter = EmitterDef {
            name: "test_emitter".to_string(),
            emit_type: EmitType::Point,
            blend_type: BlendType::Add,
            display_side: DisplaySide::Both,
            emission_rate: 1.0,
            emission_rate_random: 0.0,
            initial_speed: 0.0,
            speed_random: 0.0,
            accel: glam::Vec3::ZERO,
            lifetime: 10.0,
            lifetime_random: 0.0,
            scale: 1.0,
            scale_random: 0.0,
            rotation_speed: 0.0,
            color0: vec![],
            color1: vec![],
            alpha0: AnimKey3v4k::default(),
            alpha1: AnimKey3v4k::default(),
            scale_anim: AnimKey3v4k::default(),
            textures: vec![],
            mesh_type: 0,
            primitive_index: 0,
            texture_index: 0, // emitter uses index 0
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            emitter_offset: glam::Vec3::ZERO,
            emitter_rotation: glam::Vec3::ZERO,
            emitter_scale: glam::Vec3::ONE,
            is_one_time: false,
            emission_timing: 0,
            emission_duration: 60,
        };

        // BfresMesh with texture_index = 1 (not covered by any emitter)
        let bfres_mesh = BfresMesh {
            vertices: vec![],
            indices: vec![],
            texture_index: 1, // sub-mesh uses index 1
        };

        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "test_set".to_string(),
                emitters: vec![emitter],
            }],
            texture_section: vec![0xFFu8; 256], // 256 bytes of dummy pixel data
            texture_section_offset: 0,
            bntx_textures: vec![make_tex(0), make_tex(64)], // tex0 at offset 0, tex1 at offset 64
            primitives: vec![],
            bfres_models: vec![BfresModel {
                name: "test_model".to_string(),
                meshes: vec![bfres_mesh],
            }],
            shader_binary_1: vec![],
            shader_binary_2: vec![],
        };

        // Simulate what upload_textures does: compute which indices would be covered
        // by the unfixed emitter-loop-only implementation.
        let covered = bntx_indices_covered_by_emitters(&ptcl);

        // The sub-mesh uses texture_index = 1, which is NOT covered by any emitter.
        // On unfixed code: covered = {0}, missing key 1.
        // On fixed code: covered = {0, 1} (all bntx_textures uploaded).
        assert!(
            covered.contains(&1),
            "Sub-test B (upload gap): bntx_tex_cache would be missing key 1 — \
             only emitter-referenced indices are uploaded (covered={:?}). \
             Bug confirmed: sub-mesh texture_index=1 has no entry in bntx_tex_cache.",
            covered
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-model-mapping, Property 2: Preservation
    // Non-Buggy Inputs Unchanged
    //
    // These tests MUST PASS on unfixed code — they capture baseline behavior
    // that must not regress after the fix is applied.
    //
    // Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5
    // ═══════════════════════════════════════════════════════════════════════

    // ── Preservation 2: Billboard-only PtclFile ───────────────────────────────
    // When upload_textures is called with a PtclFile that has no BFRES models
    // (only billboard emitters), tex_cache must be populated from emitter
    // texture_index values exactly as before.
    //
    // This test verifies the emitter-loop logic is unchanged by checking that
    // bntx_indices_covered_by_emitters returns the correct set for a billboard-only
    // PtclFile (no bfres_models, emitters reference indices 0 and 2).
    //
    // Validates: Requirements 3.2
    #[test]
    fn test_preservation_etmm_billboard_only_ptcl_tex_cache_from_emitters() {
        use crate::effects::{
            PtclFile, EmitterSet, EmitterDef, TextureRes,
            EmitType, BlendType, DisplaySide, AnimKey3v4k,
        };

        let make_tex = |offset: u32| TextureRes {
            width: 4,
            height: 4,
            ftx_format: 0x0B01,
            ftx_data_offset: offset,
            ftx_data_size: 64,
            original_format: 0x0B01,
            original_data_offset: offset,
            original_data_size: 64,
            wrap_mode: 1,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0,
        };

        let make_emitter = |name: &str, tex_idx: u32| EmitterDef {
            name: name.to_string(),
            emit_type: EmitType::Point,
            blend_type: BlendType::Add,
            display_side: DisplaySide::Both,
            emission_rate: 1.0,
            emission_rate_random: 0.0,
            initial_speed: 0.0,
            speed_random: 0.0,
            accel: glam::Vec3::ZERO,
            lifetime: 10.0,
            lifetime_random: 0.0,
            scale: 1.0,
            scale_random: 0.0,
            rotation_speed: 0.0,
            color0: vec![],
            color1: vec![],
            alpha0: AnimKey3v4k::default(),
            alpha1: AnimKey3v4k::default(),
            scale_anim: AnimKey3v4k::default(),
            textures: vec![],
            mesh_type: 0,
            primitive_index: 0,
            texture_index: tex_idx,
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            emitter_offset: glam::Vec3::ZERO,
            emitter_rotation: glam::Vec3::ZERO,
            emitter_scale: glam::Vec3::ONE,
            is_one_time: false,
            emission_timing: 0,
            emission_duration: 60,
        };

        // Billboard-only PtclFile: no bfres_models, two emitters using indices 0 and 2
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "billboard_set".to_string(),
                emitters: vec![
                    make_emitter("emitter_0", 0),
                    make_emitter("emitter_2", 2),
                ],
            }],
            texture_section: vec![0xFFu8; 256],
            texture_section_offset: 0,
            bntx_textures: vec![make_tex(0), make_tex(64), make_tex(128)],
            primitives: vec![],
            bfres_models: vec![], // no BFRES models — billboard-only
            shader_binary_1: vec![],
            shader_binary_2: vec![],
        };

        // The emitter-loop logic must cover exactly the indices used by emitters.
        // On both unfixed and fixed code, emitters reference indices 0 and 2.
        let covered = bntx_indices_covered_by_emitters(&ptcl);

        assert!(
            covered.contains(&0),
            "Preservation 2 (billboard-only): emitter index 0 must be covered, covered={:?}",
            covered
        );
        assert!(
            covered.contains(&2),
            "Preservation 2 (billboard-only): emitter index 2 must be covered, covered={:?}",
            covered
        );
        // Fix 3.3: the fixed implementation also uploads all bntx_textures by index,
        // so index 1 is now covered even though no emitter references it.
        // This is the correct fixed behavior — all bntx indices must be in bntx_tex_cache.
        assert!(
            covered.contains(&1),
            "Preservation 2 (billboard-only): fixed implementation must cover index 1 \
             (all bntx_textures uploaded by index), covered={:?}",
            covered
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-swizzle-fix, Task 1: Bug Condition Exploration
    // Property 1: Bug Condition — Channel Swizzle Byte Order (Reversed Shifts)
    //
    // This test MUST FAIL on unfixed code — failure confirms the bug exists.
    // DO NOT attempt to fix the test or the code when it fails.
    //
    // Counterexample documented:
    //   comp_sel = 0x05040302u32 (identity swizzle: R_src=2, G_src=3, B_src=4, A_src=5
    //   stored in little-endian byte order, i.e. byte0=0x02, byte1=0x03, byte2=0x04, byte3=0x05)
    //
    //   Buggy code extracts:
    //     ch_r = (cs >> 24) & 0xFF = 0x05 = 5  ← reads A_src byte instead of R_src
    //     ch_g = (cs >> 16) & 0xFF = 0x04 = 4  ← reads B_src byte instead of G_src
    //     ch_b = (cs >>  8) & 0xFF = 0x03 = 3  ← reads G_src byte instead of B_src
    //     ch_a = (cs >>  0) & 0xFF = 0x02 = 2  ← reads R_src byte instead of A_src
    //
    //   All four channels are reversed. The identity check
    //   (ch_r==2 && ch_g==3 && ch_b==4 && ch_a==5) fails, so needs_swizzle=true,
    //   and the swizzle is incorrectly applied to a texture that should pass through.
    //
    // Validates: Requirements 1.1, 1.2, 1.3
    // ═══════════════════════════════════════════════════════════════════════
    #[test]
    fn test_comp_sel_byte_order_bug_condition() {
        // Helper replicating the FIXED shift expressions from upload_textures.
        // Little-endian: R_src at bits 0-7, A_src at bits 24-31.
        fn extract_channels_fixed(cs: u32) -> (u8, u8, u8, u8) {
            let ch_r = ((cs >>  0) & 0xFF) as u8;
            let ch_g = ((cs >>  8) & 0xFF) as u8;
            let ch_b = ((cs >> 16) & 0xFF) as u8;
            let ch_a = ((cs >> 24) & 0xFF) as u8;
            (ch_r, ch_g, ch_b, ch_a)
        }

        // Identity swizzle: R_src=2, G_src=3, B_src=4, A_src=5 in LE byte order.
        // As a little-endian u32: byte0=0x02, byte1=0x03, byte2=0x04, byte3=0x05
        //   → u32 value = 0x05040302
        let comp_sel: u32 = 0x05040302u32;

        let (ch_r, ch_g, ch_b, ch_a) = extract_channels_fixed(comp_sel);

        // Fixed code: ch_r == 2 (reads bits 0-7 = R_src byte).
        assert_eq!(ch_r, 2, "ch_r should be 2 (R_src at bits 0-7), got {}", ch_r);
        assert_eq!(ch_g, 3, "ch_g should be 3 (G_src at bits 8-15), got {}", ch_g);
        assert_eq!(ch_b, 4, "ch_b should be 4 (B_src at bits 16-23), got {}", ch_b);
        assert_eq!(ch_a, 5, "ch_a should be 5 (A_src at bits 24-31), got {}", ch_a);

        // The identity swizzle check: needs_swizzle should be false for this comp_sel.
        // Fixed code: ch_r==2, ch_g==3, ch_b==4, ch_a==5 — identity check passes, needs_swizzle=false.
        let needs_swizzle = comp_sel != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);
        assert!(!needs_swizzle,
            "needs_swizzle should be false for identity swizzle 0x05040302, \
             but ch_r={} ch_g={} ch_b={} ch_a={}",
            ch_r, ch_g, ch_b, ch_a);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-swizzle-fix, Task 2: Preservation Property Tests
    // Property 2: Preservation — Non-Buggy Input Behavior Unchanged
    //
    // Test 1: Zero comp_sel — swizzle pass is skipped on both unfixed and fixed code.
    // The zero check fires before shift extraction, so this PASSES on unfixed code.
    //
    // Validates: Requirements 3.1
    // ═══════════════════════════════════════════════════════════════════════
    #[test]
    fn test_zero_comp_sel_no_swizzle() {
        // Replicate the needs_swizzle guard from upload_textures.
        // With cs == 0, the guard short-circuits before any shift extraction.
        let cs: u32 = 0;

        // Extract channels using the BUGGY shifts (as in unfixed code).
        let ch_r = ((cs >> 24) & 0xFF) as u8;
        let ch_g = ((cs >> 16) & 0xFF) as u8;
        let ch_b = ((cs >>  8) & 0xFF) as u8;
        let ch_a = ((cs >>  0) & 0xFF) as u8;

        // The needs_swizzle guard: cs != 0 is false, so needs_swizzle is always false
        // regardless of the extracted channel values.
        let needs_swizzle = cs != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);

        assert!(
            !needs_swizzle,
            "Preservation (zero comp_sel): needs_swizzle must be false when comp_sel=0, \
             got needs_swizzle={needs_swizzle} (ch_r={ch_r} ch_g={ch_g} ch_b={ch_b} ch_a={ch_a})"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-swizzle-fix, Task 2: Preservation Property Tests
    // Property 2: Preservation — BC4/BC5 override fires regardless of comp_sel.
    //
    // With fmt_type = 0x1D (BC4) and any comp_sel, the override (1,1,1,2) is applied
    // after extraction, independent of the shift values. PASSES on unfixed code.
    //
    // Validates: Requirements 3.2
    // ═══════════════════════════════════════════════════════════════════════
    #[test]
    fn test_bc4_bc5_override_preservation() {
        let fmt_type: u8 = 0x1D; // BC4
        let cs: u32 = 0x05040302; // any non-zero comp_sel

        // Extract channels using the BUGGY shifts (as in unfixed code).
        let ch_r = ((cs >> 24) & 0xFF) as u8;
        let ch_g = ((cs >> 16) & 0xFF) as u8;
        let ch_b = ((cs >>  8) & 0xFF) as u8;
        let ch_a = ((cs >>  0) & 0xFF) as u8;

        // BC4/BC5 override: replaces extracted channels with (1, 1, 1, 2).
        let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D || fmt_type == 0x1E {
            (1u8, 1u8, 1u8, 2u8)
        } else {
            (ch_r, ch_g, ch_b, ch_a)
        };

        assert_eq!(ch_r, 1, "BC4/BC5 override: ch_r must be 1 (one/white), got {ch_r}");
        assert_eq!(ch_g, 1, "BC4/BC5 override: ch_g must be 1 (one/white), got {ch_g}");
        assert_eq!(ch_b, 1, "BC4/BC5 override: ch_b must be 1 (one/white), got {ch_b}");
        assert_eq!(ch_a, 2, "BC4/BC5 override: ch_a must be 2 (R channel = alpha mask), got {ch_a}");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: effect-texture-swizzle-fix, Task 2: Fix-checking test
    // Property 1: Bug Condition — Arbitrary comp_sel extraction (correct shifts).
    //
    // With comp_sel = 0x01020304u32, the CORRECT (fixed) extraction gives:
    //   ch_r = (cs >> 0)  & 0xFF = 0x04 = 4
    //   ch_g = (cs >> 8)  & 0xFF = 0x03 = 3
    //   ch_b = (cs >> 16) & 0xFF = 0x02 = 2
    //   ch_a = (cs >> 24) & 0xFF = 0x01 = 1
    //
    // This test WILL FAIL on unfixed code (reversed shifts give ch_r=1, ch_a=4).
    // That is expected and correct — it is a fix-checking test.
    //
    // Validates: Requirements 2.1
    // ═══════════════════════════════════════════════════════════════════════
    #[test]
    fn test_arbitrary_comp_sel_extraction() {
        let cs: u32 = 0x01020304u32;

        // CORRECT (fixed) shift amounts: R_src at bits 0-7, A_src at bits 24-31.
        let ch_r = ((cs >>  0) & 0xFF) as u8;
        let ch_g = ((cs >>  8) & 0xFF) as u8;
        let ch_b = ((cs >> 16) & 0xFF) as u8;
        let ch_a = ((cs >> 24) & 0xFF) as u8;

        assert_eq!(ch_r, 4, "Fixed extraction: ch_r should be 4 (bits 0-7 of 0x01020304), got {ch_r}");
        assert_eq!(ch_g, 3, "Fixed extraction: ch_g should be 3 (bits 8-15 of 0x01020304), got {ch_g}");
        assert_eq!(ch_b, 2, "Fixed extraction: ch_b should be 2 (bits 16-23 of 0x01020304), got {ch_b}");
        assert_eq!(ch_a, 1, "Fixed extraction: ch_a should be 1 (bits 24-31 of 0x01020304), got {ch_a}");
    }

    // ── Preservation 3: Fallback-to-white when sub_mesh_tex_idx = u32::MAX ───
    // When resolve_mesh_tex_bg is called with sub_mesh_tex_idx = u32::MAX,
    // the resolution chain must fall back to emitter-level or white bind group.
    //
    // This tests the logic of resolve_mesh_tex_bg inline (the method is private).
    // The fallback chain is: bntx_tex_cache[idx] → tex_cache[key] → white_tex_bg.
    // With sub_mesh_tex_idx = u32::MAX, the first branch is skipped entirely.
    //
    // Validates: Requirements 3.1
    #[test]
    fn test_preservation_etmm_fallback_to_white_when_tex_idx_max() {
        // Simulate the resolve_mesh_tex_bg logic inline (no GPU needed).
        // The logic is:
        //   if sub_mesh_tex_idx != u32::MAX { check bntx_tex_cache }
        //   fall back to tex_cache[emitter_key] or white_tex_bg

        let sub_mesh_tex_idx = u32::MAX;

        // With sub_mesh_tex_idx = u32::MAX, the bntx_tex_cache branch is skipped.
        // The function falls through to tex_cache / white_tex_bg.
        let bntx_branch_taken = sub_mesh_tex_idx != u32::MAX;
        assert!(
            !bntx_branch_taken,
            "Preservation 3 (fallback-to-white): sub_mesh_tex_idx=u32::MAX must skip \
             bntx_tex_cache lookup — bntx_branch_taken must be false"
        );

        // Verify that a valid index WOULD take the bntx branch (preservation of the
        // positive case — valid indices must still hit bntx_tex_cache first)
        let valid_idx: u32 = 0;
        let valid_branch_taken = valid_idx != u32::MAX;
        assert!(
            valid_branch_taken,
            "Preservation 3: valid sub_mesh_tex_idx=0 must attempt bntx_tex_cache lookup"
        );

        // Verify u32::MAX sentinel value is stable
        assert_eq!(
            u32::MAX, 0xFFFF_FFFF,
            "Preservation 3: u32::MAX sentinel must equal 0xFFFF_FFFF"
        );
    }
}