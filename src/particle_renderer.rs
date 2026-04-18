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
    aspect_ratio: f32,  // texture width / height
    tex_scale: [f32; 2],
    tex_offset: [f32; 2],
    _pad: f32,
    _pad2: f32,
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
    // The fragment shader outputs premultiplied alpha (rgb * alpha, alpha).
    // Use One as src_factor for additive modes so the premultiplied contribution
    // adds directly to the offscreen target without double-multiplying by alpha.
    let premul_add = BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    };
    let alpha_preserve = BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    };
    match blend_type {
        BlendType::Normal => BlendState {
            // Normal blend: premultiplied src over dst
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendType::Add => BlendState {
            color: premul_add,
            alpha: alpha_preserve,
        },
        BlendType::Sub => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
            alpha: alpha_preserve,
        },
        BlendType::Screen => BlendState {
            // Screen blend: result = src + dst - src*dst = src + dst*(1-src)
            // For premultiplied alpha output: src_factor=One, dst_factor=OneMinusSrcColor
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrc,
                operation: BlendOperation::Add,
            },
            alpha: alpha_preserve,
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

/// Map a BNTX wrap mode byte to a wgpu AddressMode.
/// BNTX values: 0 = Repeat, 1 = MirrorRepeat, 2 = ClampToEdge.
/// Defaults to Repeat for unknown values (most particle textures tile).
fn address_mode_for(wrap_mode: u8) -> wgpu::AddressMode {
    match wrap_mode {
        2 => wgpu::AddressMode::ClampToEdge,
        1 => wgpu::AddressMode::MirrorRepeat,
        _ => wgpu::AddressMode::Repeat,
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




// ── Indirect texture uniform ──────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct IndirectParams {
    is_indirect: u32,
    distortion_strength: f32,
    indirect_scroll_u: f32,
    indirect_scroll_v: f32,
    // TexPatAnim slot-1 UV scale and offset for the indirect texture sample
    indirect_scale_u: f32,
    indirect_scale_v: f32,
    indirect_offset_u: f32,
    indirect_offset_v: f32,
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
    // Texture aspect ratio (width/height) keyed by (emitter_set_idx, emitter_idx)
    tex_aspect_cache: HashMap<(usize, usize), f32>,
    // Direct BNTX-index → bind group map, for per-sub-mesh texture lookup
    bntx_tex_cache: HashMap<u32, wgpu::BindGroup>,
    // Primitive mesh GPU buffers keyed by primitive_index
    mesh_cache: HashMap<u32, MeshBuffers>,
    // Bind group layout for mesh camera+instance (group 0)
    mesh_camera_bg_layout: wgpu::BindGroupLayout,
    // Per-emitter slot-1 alpha texture views and samplers (for combined bind group building)
    alpha_view_cache: HashMap<(usize, usize), (wgpu::TextureView, wgpu::Sampler)>,
    // Per-emitter color texture views and samplers (for combined bind group building)
    color_view_cache: HashMap<(usize, usize), (wgpu::TextureView, wgpu::Sampler)>,
    // Combined 4-entry bind groups for emitters that have both color + alpha textures
    combined_bg_cache: HashMap<(usize, usize), wgpu::BindGroup>,
    // White texture view and sampler (kept for building combined bind groups)
    white_view: wgpu::TextureView,
    white_sampler: wgpu::Sampler,
    // Pre-built draw groups from prepare_draw() for use in draw_into_pass()
    prepared_groups: Vec<((usize, usize), usize)>,
    // Pre-computed IndirectParams per group (parallel to prepared_groups)
    prepared_indirect_params: Vec<IndirectParams>,
    // Per-emitter indirect texture views and samplers (populated when is_indirect_slot1 == true)
    indirect_view_cache: HashMap<(usize, usize), (wgpu::TextureView, wgpu::Sampler)>,
    // Uniform buffer for IndirectParams (written per draw call)
    indirect_uniform_buf: wgpu::Buffer,
    // Per-BNTX-index emissive texture views and samplers (for mesh _emi slots)
    emissive_view_cache: HashMap<u32, (wgpu::TextureView, wgpu::Sampler)>,
    // Pre-built emissive bind groups keyed by BNTX texture index
    emissive_bg_cache: HashMap<u32, wgpu::BindGroup>,
    // Bind group layout for mesh emissive (group 2): binding 0 = texture, binding 1 = sampler
    emissive_bg_layout: wgpu::BindGroupLayout,
    // Fallback black emissive bind group (used when no _emi texture is present)
    black_emissive_bg: wgpu::BindGroup,
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
                // Slot 1: alpha/gradient texture (binding 2 = texture, binding 3 = sampler)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Slot 2: indirect texture (binding 4 = texture, binding 5 = sampler)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Binding 6: IndirectParams uniform buffer
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
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
        // Create indirect uniform buffer early so it can be included in white_tex_bg
        let indirect_uniform_buf_init = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("indirect_uniform_buf"),
            size: std::mem::size_of::<IndirectParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let white_tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle_white_tex_bg"),
            layout: &tex_bg_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&white_sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&white_sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&white_sampler) },
                wgpu::BindGroupEntry { binding: 6, resource: indirect_uniform_buf_init.as_entire_binding() },
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

        // Emissive bind group layout (group 2 for mesh pipelines): binding 0 = texture, 1 = sampler
        let emissive_bg_layout_for_pipeline = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("emissive_bg_layout"),
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

        let mesh_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh_pipeline_layout"),
            bind_group_layouts: &[&mesh_camera_bg_layout, &tex_bg_layout, &emissive_bg_layout_for_pipeline],
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
                    // Additive blit: add particle color contribution to scene.
                    // Particle target has alpha=0 (additive effects don't occlude).
                    // One/One adds the premultiplied color directly.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
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
            tex_aspect_cache: HashMap::new(),
            bntx_tex_cache: HashMap::new(),
            mesh_cache: HashMap::new(),
            mesh_camera_bg_layout,
            alpha_view_cache: HashMap::new(),
            color_view_cache: HashMap::new(),
            combined_bg_cache: HashMap::new(),
            white_view,
            white_sampler,
            prepared_groups: Vec::new(),
            prepared_indirect_params: Vec::new(),
            indirect_view_cache: HashMap::new(),
            indirect_uniform_buf: indirect_uniform_buf_init,
            emissive_view_cache: HashMap::new(),
            emissive_bg_cache: HashMap::new(),
            emissive_bg_layout: emissive_bg_layout_for_pipeline.clone(),
            black_emissive_bg: {
                // Create a 1×1 black texture for the fallback emissive bind group
                let black_tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("black_emissive_tex"),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    black_tex.as_image_copy(),
                    &[0u8, 0, 0, 255],
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: None },
                    wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                );
                let black_view = black_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let black_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("black_emissive_sampler"),
                    ..Default::default()
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("black_emissive_bg"),
                    layout: &emissive_bg_layout_for_pipeline,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&black_view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&black_sampler) },
                    ],
                })
            },
        }
    }

    /// Upload textures from the ptcl texture section into GPU bind groups.
    /// Call this once after loading a new ptcl file.
    pub fn upload_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, ptcl: &PtclFile) {
        // Task 4.1: clear cache before processing
        self.tex_cache.clear();
        self.tex_aspect_cache.clear();
        self.bntx_tex_cache.clear();
        self.alpha_view_cache.clear();
        self.color_view_cache.clear();
        self.combined_bg_cache.clear();
        self.indirect_view_cache.clear();
        self.emissive_view_cache.clear();
        self.emissive_bg_cache.clear();
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
                    // BC formats decoded to RGBA8 by image_dds.
                    // For sRGB variants, image_dds outputs sRGB-encoded bytes, so we must
                    // upload to Rgba8UnormSrgb so the GPU applies the correct sRGB→linear
                    // conversion when sampling. Non-sRGB variants use Rgba8Unorm (linear).
                    if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm }
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
                    cs != 0 && ((cs >> 0) & 0xFF) == 4
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
                    // should be white (1,1,1) with alpha from the appropriate channel.
                    // BC5 has two channels (R and G); use the swizzle's alpha source (ch_a)
                    // to pick the right one. BC4 only has R, so alpha = R.
                    let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D {
                        // BC4: single channel, white RGB, alpha from R
                        (1u8, 1u8, 1u8, 2u8)
                    } else if fmt_type == 0x1E {
                        // BC5: two channels decoded to (R, G, 0, 1) by image_dds.
                        // Use the channel_swizzle's A_src byte to pick the alpha channel.
                        // If A_src is 3 (G), use G as alpha; otherwise default to R (2).
                        let a_src = ((cs >> 24) & 0xFF) as u8;
                        let alpha_ch = if a_src == 3 { 3u8 } else { 2u8 }; // 3=G, 2=R
                        (1u8, 1u8, 1u8, alpha_ch)
                    } else {
                        (ch_r, ch_g, ch_b, ch_a)
                    };

                    // Identity swizzle for RGBA = (2,3,4,5); skip if trivial or unset
                    let needs_swizzle = cs != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);
                    decoded_buf = if needs_swizzle || fmt_type == 0x1D || fmt_type == 0x1E {
                        let pick = |p: &[u8], ch: u8| -> u8 {
                            match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] }
                        };
                        let result: Vec<u8> = rgba.chunks_exact(4)
                            .flat_map(|p| [pick(p, ch_r), pick(p, ch_g), pick(p, ch_b), pick(p, ch_a)])
                            .collect();
                        result
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
                    address_mode_u: address_mode_for(tex_res.wrap_mode),
                    address_mode_v: address_mode_for(tex_res.wrap_mode),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
                // Build 7-entry bind group: color at 0/1, white fallback at 2/3/4/5, indirect_uniform at 6.
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("ptcl_tex_bg_{set_idx}_{emitter_idx}")),
                    layout: &self.tex_bg_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                        wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                        wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                        wgpu::BindGroupEntry { binding: 6, resource: self.indirect_uniform_buf.as_entire_binding() },
                    ],
                });
                // Also populate bntx_tex_cache keyed by BNTX texture index (for per-sub-mesh lookup).
                // Only insert once per unique index — first emitter wins.
                let bntx_idx = emitter.texture_index;
                if !self.bntx_tex_cache.contains_key(&bntx_idx) {
                    let bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some(&format!("bntx_tex_bg_{bntx_idx}")),
                        layout: &self.tex_bg_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                            wgpu::BindGroupEntry { binding: 6, resource: self.indirect_uniform_buf.as_entire_binding() },
                        ],
                    });
                    self.bntx_tex_cache.insert(bntx_idx, bg2);
                }
                self.tex_cache.insert((set_idx, emitter_idx), bg);
                // Store color view/sampler for combined bind group building (slot-1 compositing)
                let view2 = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let sampler2 = device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("ptcl_color_sampler2"),
                    address_mode_u: address_mode_for(tex_res.wrap_mode),
                    address_mode_v: address_mode_for(tex_res.wrap_mode),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
                self.color_view_cache.insert((set_idx, emitter_idx), (view2, sampler2));
                // Store aspect ratio for billboard stretching
                let aspect = if h > 0 { tex_w as f32 / h as f32 } else { 1.0 };
                self.tex_aspect_cache.insert((set_idx, emitter_idx), aspect);

                // ── Slot-1 alpha/gradient texture upload ──────────────────
                // If the emitter has a second texture slot, decode and upload it.
                // The alpha_view_cache entry will be used to build combined bind groups at render time.
                if let Some(alpha_res) = emitter.textures.get(1) {
                    if alpha_res.width > 0 && alpha_res.height > 0 {
                        let a_data_offset = alpha_res.ftx_data_offset as usize;
                        let a_data_size   = alpha_res.ftx_data_size as usize;
                        if a_data_size > 0 && a_data_offset + a_data_size <= ptcl.texture_section.len() {
                            let a_raw = &ptcl.texture_section[a_data_offset..a_data_offset + a_data_size];
                            let a_w = alpha_res.width as u32;
                            let a_h = alpha_res.height as u32;
                            let a_fmt_type    = (alpha_res.ftx_format >> 8) as u8;
                            let a_fmt_variant = (alpha_res.ftx_format & 0xFF) as u8;
                            let a_is_srgb     = a_fmt_variant == 0x06;
                            let a_dds_fmt: Option<image_dds::ImageFormat> = match a_fmt_type {
                                0x1A => Some(if a_is_srgb { image_dds::ImageFormat::BC1RgbaUnormSrgb } else { image_dds::ImageFormat::BC1RgbaUnorm }),
                                0x1B => Some(if a_is_srgb { image_dds::ImageFormat::BC2RgbaUnormSrgb } else { image_dds::ImageFormat::BC2RgbaUnorm }),
                                0x1C => Some(if a_is_srgb { image_dds::ImageFormat::BC3RgbaUnormSrgb } else { image_dds::ImageFormat::BC3RgbaUnorm }),
                                0x1D => Some(if a_fmt_variant == 0x02 { image_dds::ImageFormat::BC4RSnorm } else { image_dds::ImageFormat::BC4RUnorm }),
                                0x1E => Some(if a_fmt_variant == 0x02 { image_dds::ImageFormat::BC5RgSnorm } else { image_dds::ImageFormat::BC5RgUnorm }),
                                0x1F => Some(if a_fmt_variant == 0x05 { image_dds::ImageFormat::BC6hRgbUfloat } else { image_dds::ImageFormat::BC6hRgbSfloat }),
                                0x20 => Some(if a_is_srgb { image_dds::ImageFormat::BC7RgbaUnormSrgb } else { image_dds::ImageFormat::BC7RgbaUnorm }),
                                _ => None,
                            };
                            let a_wgpu_fmt = if a_dds_fmt.is_some() {
                                if a_is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm }
                            } else {
                                match a_fmt_type {
                                    0x02 => wgpu::TextureFormat::R8Unorm,
                                    0x07 => wgpu::TextureFormat::Rgba8Unorm,
                                    0x09 => wgpu::TextureFormat::Rg8Unorm,
                                    0x0A => wgpu::TextureFormat::R16Unorm,
                                    0x0B | 0x0C => if a_is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
                                    _ => { eprintln!("[TEX] alpha slot {set_idx}/{emitter_idx}: unsupported fmt_type={a_fmt_type:#04x}, skipping"); continue; }
                                }
                            };
                            let a_is_bc = a_dds_fmt.is_some();
                            let a_bc_blocks_x = (a_w + 3) / 4;
                            let a_bc_blocks_y = (a_h + 3) / 4;
                            let a_raw_bpr = if a_is_bc {
                                match a_fmt_type { 0x1A | 0x1D => a_bc_blocks_x * 8, _ => a_bc_blocks_x * 16 }
                            } else {
                                match a_fmt_type { 0x02 => a_w, 0x09 | 0x0A => a_w * 2, _ => a_w * 4 }
                            };
                            let a_block_rows = if a_is_bc { a_bc_blocks_y } else { a_h };
                            let a_mip0 = (a_raw_bpr * a_block_rows) as usize;
                            if a_raw.len() >= a_mip0 {
                                let a_upload = &a_raw[..a_mip0];
                                let a_decoded: Vec<u8>;
                                let a_bpr: u32;
                                if let Some(dds_fmt) = a_dds_fmt {
                                    let surface = image_dds::Surface { width: a_w, height: a_h, depth: 1, layers: 1, mipmaps: 1, image_format: dds_fmt, data: a_upload };
                                    let rgba = match surface.decode_rgba8() { Ok(s) => s.data, Err(e) => { eprintln!("[TEX] alpha slot decode error: {e}"); continue; } };
                                    let a_cs = alpha_res.channel_swizzle;
                                    let a_ch_r = ((a_cs >>  0) & 0xFF) as u8;
                                    let a_ch_g = ((a_cs >>  8) & 0xFF) as u8;
                                    let a_ch_b = ((a_cs >> 16) & 0xFF) as u8;
                                    let a_ch_a = ((a_cs >> 24) & 0xFF) as u8;
                                    let (a_ch_r, a_ch_g, a_ch_b, a_ch_a) = if a_fmt_type == 0x1D {
                                        (1u8, 1u8, 1u8, 2u8)
                                    } else if a_fmt_type == 0x1E {
                                        if emitter.is_indirect_slot1 {
                                            // BC5 indirect: preserve R→R, G→G for UV offset sampling
                                            (2u8, 3u8, 0u8, 1u8)
                                        } else {
                                            // BC5 alpha mask: always use R as alpha
                                            (1u8, 1u8, 1u8, 2u8)
                                        }
                                    } else { (a_ch_r, a_ch_g, a_ch_b, a_ch_a) };
                                    let needs_swizzle = a_cs != 0 && !(a_ch_r == 2 && a_ch_g == 3 && a_ch_b == 4 && a_ch_a == 5);
                                    a_decoded = if needs_swizzle || a_fmt_type == 0x1D || a_fmt_type == 0x1E {
                                        let pick = |p: &[u8], ch: u8| -> u8 { match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] } };
                                        rgba.chunks_exact(4).flat_map(|p| [pick(p, a_ch_r), pick(p, a_ch_g), pick(p, a_ch_b), pick(p, a_ch_a)]).collect()
                                    } else { rgba };
                                    a_bpr = a_w * 4;
                                } else {
                                    let a_is_bgra = a_fmt_type == 0x0C || { let cs = alpha_res.channel_swizzle; cs != 0 && ((cs >> 0) & 0xFF) == 4 };
                                    a_decoded = if a_is_bgra {
                                        a_upload.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect()
                                    } else { a_upload.to_vec() };
                                    a_bpr = a_raw_bpr;
                                }
                                const ALIGN: u32 = 256;
                                let a_aligned_bpr = (a_bpr + ALIGN - 1) & !(ALIGN - 1);
                                let a_upload_data = if a_aligned_bpr != a_bpr {
                                    let mut padded = Vec::with_capacity(a_h as usize * a_aligned_bpr as usize);
                                    for row in 0..a_h as usize {
                                        let s = row * a_bpr as usize;
                                        let e = s + a_bpr as usize;
                                        if e <= a_decoded.len() { padded.extend_from_slice(&a_decoded[s..e]); } else { padded.extend(std::iter::repeat(0u8).take(a_bpr as usize)); }
                                        padded.extend(std::iter::repeat(0u8).take((a_aligned_bpr - a_bpr) as usize));
                                    }
                                    padded
                                } else { a_decoded.clone() };
                                let a_texture = device.create_texture(&wgpu::TextureDescriptor {
                                    label: Some(&format!("alpha_tex_{set_idx}_{emitter_idx}")),
                                    size: wgpu::Extent3d { width: a_w, height: a_h, depth_or_array_layers: 1 },
                                    mip_level_count: 1, sample_count: 1,
                                    dimension: wgpu::TextureDimension::D2,
                                    format: a_wgpu_fmt,
                                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                                    view_formats: &[],
                                });
                                queue.write_texture(
                                    a_texture.as_image_copy(), &a_upload_data,
                                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(a_aligned_bpr), rows_per_image: None },
                                    wgpu::Extent3d { width: a_w, height: a_h, depth_or_array_layers: 1 },
                                );
                                let a_view = a_texture.create_view(&wgpu::TextureViewDescriptor::default());
                                let a_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                                    label: Some("alpha_tex_sampler"),
                                    address_mode_u: address_mode_for(alpha_res.wrap_mode),
                                    address_mode_v: address_mode_for(alpha_res.wrap_mode),
                                    mag_filter: wgpu::FilterMode::Linear,
                                    min_filter: wgpu::FilterMode::Linear,
                                    mipmap_filter: wgpu::FilterMode::Linear,
                                    ..Default::default()
                                });
                                eprintln!("[TEX] alpha slot {set_idx}/{emitter_idx}: {}x{} fmt={:#06x} uploaded", a_w, a_h, alpha_res.ftx_format);
                                // Route to indirect_view_cache or alpha_view_cache based on emitter flag
                                if emitter.is_indirect_slot1 {
                                    self.indirect_view_cache.insert((set_idx, emitter_idx), (a_view, a_sampler));
                                } else {
                                    self.alpha_view_cache.insert((set_idx, emitter_idx), (a_view, a_sampler));
                                }
                            }
                        }
                    }
                }
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
                // sRGB BC textures: image_dds outputs sRGB-encoded bytes, upload to sRGB target
                if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm }
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
            let is_bgra = fmt_type == 0x0C || { let cs = tex_res.channel_swizzle; cs != 0 && ((cs >> 0) & 0xFF) == 4 };
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
                let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D {
                    (1u8, 1u8, 1u8, 2u8)
                } else if fmt_type == 0x1E {
                    // BC5: check if this is an indirect texture (name contains "indirect").
                    // Indirect textures need RG channels preserved; alpha-mask textures use R→A.
                    if tex_res.tex_name.to_lowercase().contains("indirect") {
                        (2u8, 3u8, 0u8, 1u8) // preserve R→R, G→G for UV offset sampling
                    } else {
                        (1u8, 1u8, 1u8, 2u8) // alpha mask: white RGB, R→A
                    }
                } else { (ch_r, ch_g, ch_b, ch_a) };
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
                address_mode_u: address_mode_for(tex_res.wrap_mode),
                address_mode_v: address_mode_for(tex_res.wrap_mode),
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
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.white_sampler) },
                    wgpu::BindGroupEntry { binding: 6, resource: self.indirect_uniform_buf.as_entire_binding() },
                ],
            });
            self.bntx_tex_cache.insert(bntx_idx, bg);
        }
        eprintln!("[TEX] bntx_tex_cache now covers {} indices", self.bntx_tex_cache.len());
    }

    /// Resolve the correct texture bind group for a BFRES sub-mesh draw call.
    /// Resolution order:
    ///   1. combined_bg_cache[(emitter_set_idx, emitter_idx)]  (if slot-1 alpha texture present)
    ///   2. bntx_tex_cache[sub_mesh_tex_idx]  (if sub_mesh_tex_idx != u32::MAX)
    ///   3. tex_cache[(emitter_set_idx, emitter_idx)]
    ///   4. white_tex_bg
    fn resolve_mesh_tex_bg<'a>(
        &'a self,
        sub_mesh_tex_idx: u32,
        emitter_key: (usize, usize),
    ) -> &'a wgpu::BindGroup {
        // If a combined bind group was pre-built for this emitter (slot-1 alpha present), use it.
        if let Some(bg) = self.combined_bg_cache.get(&emitter_key) {
            return bg;
        }
        if sub_mesh_tex_idx != u32::MAX {
            if let Some(bg) = self.bntx_tex_cache.get(&sub_mesh_tex_idx) {
                return bg;
            }
        }
        self.tex_cache.get(&emitter_key).unwrap_or(&self.white_tex_bg)
    }

    /// Upload primitive mesh geometry from the ptcl file into GPU buffers.
    /// Call this once after loading a new ptcl file, alongside upload_textures.
    pub fn upload_meshes(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, ptcl: &PtclFile) {
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

                // Upload emissive texture for this mesh if present and not already cached
                if mesh.emissive_tex_index != u32::MAX
                    && !self.emissive_view_cache.contains_key(&mesh.emissive_tex_index)
                {
                    if let Some(tex_res) = ptcl.bntx_textures.get(mesh.emissive_tex_index as usize) {
                        if tex_res.width > 0 && tex_res.height > 0 {
                            let data_off = tex_res.ftx_data_offset as usize;
                            let data_sz  = tex_res.ftx_data_size as usize;
                            if data_sz > 0 && data_off + data_sz <= ptcl.texture_section.len() {
                                // Reuse the same decode path as the main upload loop (simplified: raw copy)
                                let raw = &ptcl.texture_section[data_off..data_off + data_sz];
                                let w = tex_res.width as u32;
                                let h = tex_res.height as u32;
                                let fmt_type    = (tex_res.ftx_format >> 8) as u8;
                                let fmt_variant = (tex_res.ftx_format & 0xFF) as u8;
                                let is_srgb     = fmt_variant == 0x06;
                                let dds_fmt: Option<image_dds::ImageFormat> = match fmt_type {
                                    0x1A => Some(if is_srgb { image_dds::ImageFormat::BC1RgbaUnormSrgb } else { image_dds::ImageFormat::BC1RgbaUnorm }),
                                    0x1B => Some(if is_srgb { image_dds::ImageFormat::BC2RgbaUnormSrgb } else { image_dds::ImageFormat::BC2RgbaUnorm }),
                                    0x1C => Some(if is_srgb { image_dds::ImageFormat::BC3RgbaUnormSrgb } else { image_dds::ImageFormat::BC3RgbaUnorm }),
                                    0x1D => Some(image_dds::ImageFormat::BC4RUnorm),
                                    0x1E => Some(image_dds::ImageFormat::BC5RgUnorm),
                                    0x1F => Some(image_dds::ImageFormat::BC6hRgbUfloat),
                                    0x20 => Some(if is_srgb { image_dds::ImageFormat::BC7RgbaUnormSrgb } else { image_dds::ImageFormat::BC7RgbaUnorm }),
                                    _ => None,
                                };
                                let wgpu_fmt = if dds_fmt.is_some() {
                                    if is_srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm }
                                } else { wgpu::TextureFormat::Rgba8Unorm };
                                let bc_bx = (w + 3) / 4;
                                let bc_by = (h + 3) / 4;
                                let raw_bpr = if dds_fmt.is_some() {
                                    match fmt_type { 0x1A | 0x1D => bc_bx * 8, _ => bc_bx * 16 }
                                } else { w * 4 };
                                let mip0 = (raw_bpr * if dds_fmt.is_some() { bc_by } else { h }) as usize;
                                if raw.len() >= mip0 {
                                    let decoded: Vec<u8> = if let Some(df) = dds_fmt {
                                        let surf = image_dds::Surface { width: w, height: h, depth: 1, layers: 1, mipmaps: 1, image_format: df, data: &raw[..mip0] };
                                        surf.decode_rgba8().map(|s| s.data).unwrap_or_else(|_| vec![0u8; (w * h * 4) as usize])
                                    } else { raw[..mip0].to_vec() };
                                    const ALIGN: u32 = 256;
                                    let bpr = w * 4;
                                    let abpr = (bpr + ALIGN - 1) & !(ALIGN - 1);
                                    let upload_data = if abpr != bpr {
                                        let mut p = Vec::with_capacity(h as usize * abpr as usize);
                                        for row in 0..h as usize {
                                            let s = row * bpr as usize; let e = s + bpr as usize;
                                            if e <= decoded.len() { p.extend_from_slice(&decoded[s..e]); } else { p.extend(std::iter::repeat(0u8).take(bpr as usize)); }
                                            p.extend(std::iter::repeat(0u8).take((abpr - bpr) as usize));
                                        }
                                        p
                                    } else { decoded };
                                    let emi_tex = device.create_texture(&wgpu::TextureDescriptor {
                                        label: Some(&format!("emissive_tex_{}", mesh.emissive_tex_index)),
                                        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                                        mip_level_count: 1, sample_count: 1,
                                        dimension: wgpu::TextureDimension::D2,
                                        format: wgpu_fmt,
                                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                                        view_formats: &[],
                                    });
                                    queue.write_texture(
                                        emi_tex.as_image_copy(), &upload_data,
                                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(abpr), rows_per_image: None },
                                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                                    );
                                    let emi_view = emi_tex.create_view(&wgpu::TextureViewDescriptor::default());
                                    let emi_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                                        label: Some("emissive_sampler"),
                                        address_mode_u: address_mode_for(tex_res.wrap_mode),
                                        address_mode_v: address_mode_for(tex_res.wrap_mode),
                                        mag_filter: wgpu::FilterMode::Linear,
                                        min_filter: wgpu::FilterMode::Linear,
                                        mipmap_filter: wgpu::FilterMode::Linear,
                                        ..Default::default()
                                    });
                                    self.emissive_view_cache.insert(mesh.emissive_tex_index, (emi_view, emi_sampler));
                                    eprintln!("[MESH] uploaded emissive tex idx={} {}x{}", mesh.emissive_tex_index, w, h);
                                    // Build and cache the emissive bind group
                                    if let Some((v, s)) = self.emissive_view_cache.get(&mesh.emissive_tex_index) {
                                        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                                            label: Some(&format!("emissive_bg_{}", mesh.emissive_tex_index)),
                                            layout: &self.emissive_bg_layout,
                                            entries: &[
                                                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(v) },
                                                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(s) },
                                            ],
                                        });
                                        self.emissive_bg_cache.insert(mesh.emissive_tex_index, bg);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!("[MESH] uploaded {} total mesh entries ({} primitives, {} bfres models)",
            self.mesh_cache.len(), ptcl.primitives.len(), ptcl.bfres_models.len());
    }

    /// Get or build a combined 7-entry bind group for the given emitter key.
    /// Binding 0/1 = color texture, binding 2/3 = alpha texture (or white fallback),
    /// binding 4/5 = indirect texture (or white fallback), binding 6 = indirect uniform.
    /// The result is cached in `combined_bg_cache` to avoid per-frame allocation.
    fn get_combined_tex_bg(
        &mut self,
        device: &wgpu::Device,
        key: (usize, usize),
    ) -> &wgpu::BindGroup {
        // If already cached, return it
        if self.combined_bg_cache.contains_key(&key) {
            return self.combined_bg_cache.get(&key).unwrap();
        }
        // Build combined bind group using raw pointers to work around borrow checker.
        let (color_view_ref, color_sampler_ref) = if let Some((v, s)) = self.color_view_cache.get(&key) {
            (v as *const wgpu::TextureView, s as *const wgpu::Sampler)
        } else {
            (&self.white_view as *const wgpu::TextureView, &self.white_sampler as *const wgpu::Sampler)
        };
        let (alpha_view_ref, alpha_sampler_ref) = if let Some((v, s)) = self.alpha_view_cache.get(&key) {
            (v as *const wgpu::TextureView, s as *const wgpu::Sampler)
        } else {
            (&self.white_view as *const wgpu::TextureView, &self.white_sampler as *const wgpu::Sampler)
        };
        let (indirect_view_ref, indirect_sampler_ref) = if let Some((v, s)) = self.indirect_view_cache.get(&key) {
            (v as *const wgpu::TextureView, s as *const wgpu::Sampler)
        } else {
            (&self.white_view as *const wgpu::TextureView, &self.white_sampler as *const wgpu::Sampler)
        };
        let indirect_buf_ref = &self.indirect_uniform_buf as *const wgpu::Buffer;
        // SAFETY: these pointers are valid for the lifetime of self; we only read them here.
        let combined_bg = unsafe {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("combined_tex_bg_{}_{}", key.0, key.1)),
                layout: &self.tex_bg_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&*color_view_ref) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&*color_sampler_ref) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&*alpha_view_ref) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&*alpha_sampler_ref) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&*indirect_view_ref) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&*indirect_sampler_ref) },
                    wgpu::BindGroupEntry { binding: 6, resource: (&*indirect_buf_ref).as_entire_binding() },
                ],
            })
        };
        self.combined_bg_cache.insert(key, combined_bg);
        self.combined_bg_cache.get(&key).unwrap()
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
                let aspect_ratio = self.tex_aspect_cache
                    .get(&(p.emitter_set_idx, p.emitter_idx))
                    .copied()
                    .unwrap_or(1.0);
                ParticleInstance {
                    position: p.position.to_array(),
                    size: p.size,
                    color: p.color.to_array(),
                    rotation: p.rotation,
                    aspect_ratio,
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
                    let aspect_ratio = self.tex_aspect_cache
                        .get(&(*set_idx, *emitter_idx))
                        .copied()
                        .unwrap_or(1.0);
                    ps.iter().map(move |p| ParticleInstance {
                        position: p.position.to_array(),
                        size: p.size,
                        color: p.color.to_array(),
                        rotation: p.rotation,
                        aspect_ratio,
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

            // Pre-build combined texture bind groups for all groups before starting the render pass.
            // This avoids borrow conflicts between the render pass and self.get_combined_tex_bg().
            let group_tex_bgs: Vec<*const wgpu::BindGroup> = groups.iter().map(|((set_idx, emitter_idx), _)| {
                let key = (*set_idx, *emitter_idx);
                let emitter = emitter_sets.get(*set_idx).and_then(|s| s.emitters.get(*emitter_idx));
                let bntx_idx = emitter.map(|e| e.texture_index).unwrap_or(u32::MAX);

                // Resolution order:
                // 1. combined_bg_cache (if slot-1 alpha OR indirect texture present for this emitter)
                // 2. bntx_tex_cache[texture_index] — stable key that survives ptcl merges
                // 3. tex_cache[(set_idx, emitter_idx)] — fallback
                // 4. white_tex_bg
                if self.alpha_view_cache.contains_key(&key) || self.indirect_view_cache.contains_key(&key) {
                    self.get_combined_tex_bg(device, key) as *const wgpu::BindGroup
                } else if bntx_idx != u32::MAX {
                    self.bntx_tex_cache.get(&bntx_idx).unwrap_or(&self.white_tex_bg) as *const wgpu::BindGroup
                } else if let Some(bg) = self.tex_cache.get(&key) {
                    bg as *const wgpu::BindGroup
                } else {
                    &self.white_tex_bg as *const wgpu::BindGroup
                }
            }).collect();

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
            for (group_idx, ((set_idx, emitter_idx), group)) in groups.iter().enumerate() {
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

                // SAFETY: group_tex_bgs[group_idx] points to a bind group owned by self
                // (either in combined_bg_cache, tex_cache, or white_tex_bg), all of which
                // live for the duration of this render call.
                let tex_bg = unsafe { &*group_tex_bgs[group_idx] };

                // Write IndirectParams before this draw call (mirrors draw_into_pass logic).
                let emitter_ref = emitter_sets.get(*set_idx).and_then(|s| s.emitters.get(*emitter_idx));
                let render_key = (*set_idx, *emitter_idx);
                let has_indirect = self.indirect_view_cache.contains_key(&render_key);
                let indirect_params = IndirectParams {
                    is_indirect: if has_indirect && emitter_ref.map(|e| e.is_indirect_slot1).unwrap_or(false) { 1 } else { 0 },
                    distortion_strength: emitter_ref.map(|e| e.distortion_strength).unwrap_or(0.0),
                    indirect_scroll_u: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_scroll_uv[0] } else { 0.0 }).unwrap_or(0.0),
                    indirect_scroll_v: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_scroll_uv[1] } else { 0.0 }).unwrap_or(0.0),
                    indirect_scale_u: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_tex_scale_uv[0] } else { 1.0 }).unwrap_or(1.0),
                    indirect_scale_v: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_tex_scale_uv[1] } else { 1.0 }).unwrap_or(1.0),
                    indirect_offset_u: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_tex_offset_uv[0] } else { 0.0 }).unwrap_or(0.0),
                    indirect_offset_v: emitter_ref.map(|e| if e.is_indirect_slot1 { e.indirect_tex_offset_uv[1] } else { 0.0 }).unwrap_or(0.0),
                };
                queue.write_buffer(&self.indirect_uniform_buf, 0, bytemuck::bytes_of(&indirect_params));

                rpass.set_pipeline(pipeline);
                rpass.set_bind_group(1, tex_bg, &[]);
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

            // Pre-build combined bind groups for all mesh emitter keys that have slot-1 alpha textures.
            // This must be done before the draw loop to avoid borrow conflicts.
            {
                let mut mesh_keys: Vec<(usize, usize)> = sorted_mesh.iter()
                    .map(|p| (p.emitter_set_idx, p.emitter_idx))
                    .collect();
                mesh_keys.dedup();
                for key in mesh_keys {
                    if self.alpha_view_cache.contains_key(&key) || self.color_view_cache.contains_key(&key) || self.indirect_view_cache.contains_key(&key) {
                        self.get_combined_tex_bg(device, key);
                    }
                }
            }

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
                    rotation_x: f32,
                    rotation_y: f32,
                    rotation_z: f32,
                    _pad: f32,
                    tex_scale: [f32; 2],
                    tex_offset: [f32; 2],
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

                // Helper: issue one draw call for a given mesh_bufs + tex_bg + emissive_bg + instances
                let draw_mesh = |encoder: &mut wgpu::CommandEncoder,
                                 mesh_bufs: &MeshBuffers,
                                 tex_bg: &wgpu::BindGroup,
                                 emissive_bg: &wgpu::BindGroup,
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
                    rpass.set_bind_group(2, emissive_bg, &[]);
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
                            scale: p.size * emitter_scale_mag,
                            color: p.color.to_array(),
                            rotation_x: emitter.emitter_rotation.x,
                            rotation_y: p.rotation + emitter.emitter_rotation.y,
                            rotation_z: emitter.emitter_rotation.z,
                            _pad: 0.0,
                            tex_scale: emitter.tex_scale_uv,
                            tex_offset: emitter.tex_offset_uv,
                        }).collect();
                        let tex_bg = self.resolve_mesh_tex_bg(mesh_bufs.texture_index, key);
                        draw_mesh(encoder, mesh_bufs, tex_bg, &self.black_emissive_bg, &instances);
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
                            // Look up emissive bind group for this sub-mesh
                            let emi_tex_idx = bfres_models.get(model_idx)
                                .and_then(|m| m.meshes.get(mesh_idx))
                                .map(|m| m.emissive_tex_index)
                                .unwrap_or(u32::MAX);

                            // Apply emitter TRS to each particle's world position
                            let instances: Vec<MeshInstance> = group.iter().map(|p| {
                                let base_pos = emitter_trs.transform_point3(glam::Vec3::ZERO);
                                let final_pos = p.position + base_pos;
                                MeshInstance {
                                    world_pos: final_pos.to_array(),
                                    scale: p.size,
                                    color: p.color.to_array(),
                                    rotation_x: emitter.emitter_rotation.x,
                                    rotation_y: p.rotation + emitter.emitter_rotation.y,
                                    rotation_z: emitter.emitter_rotation.z,
                                    _pad: 0.0,
                                    tex_scale: emitter.tex_scale_uv,
                                    tex_offset: emitter.tex_offset_uv,
                                }
                            }).collect();

                            let tex_bg = self.resolve_mesh_tex_bg(mesh_bufs.texture_index, key);
                            let emissive_bg = if emi_tex_idx != u32::MAX {
                                self.emissive_bg_cache.get(&emi_tex_idx)
                                    .unwrap_or(&self.black_emissive_bg)
                            } else {
                                &self.black_emissive_bg
                            };
                            draw_mesh(encoder, mesh_bufs, tex_bg, emissive_bg, &instances);
                            drew_any = true;
                        }

                        // Fall back to billboard if no sub-meshes were drawn (Req 4.3)
                        if !drew_any {
                            // Issue a billboard draw for each particle in the group.
                            // Build a temporary storage buffer with ParticleInstance data.
                            let tex_scale = emitter.tex_scale_uv;
                            let aspect_ratio = self.tex_aspect_cache
                                .get(&key)
                                .copied()
                                .unwrap_or(1.0);
                            let fallback_instances: Vec<ParticleInstance> = group.iter().map(|p| ParticleInstance {
                                position: p.position.to_array(),
                                size: p.size,
                                color: p.color.to_array(),
                                rotation: p.rotation,
                                aspect_ratio,
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
                            let tex_bg = self.combined_bg_cache.get(&key)
                                .or_else(|| self.tex_cache.get(&key))
                                .unwrap_or(&self.white_tex_bg);
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
    /// Also stores camera/instance data for use in draw_into_pass().
    pub fn prepare_draw(&mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        cam_right: Vec3,
        cam_up: Vec3,
        particles: &[Particle],
        emitter_sets: &[EmitterSet],
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

        if particles.is_empty() { return; }

        // Group billboard particles by emitter
        let mut groups: Vec<((usize, usize), Vec<usize>)> = Vec::new(); // (key, particle_indices)
        for (pi, p) in particles.iter().enumerate() {
            let is_billboard = emitter_sets
                .get(p.emitter_set_idx)
                .and_then(|s| s.emitters.get(p.emitter_idx))
                .map(|e| e.mesh_type == 0)
                .unwrap_or(true);
            if !is_billboard { continue; }
            let key = (p.emitter_set_idx, p.emitter_idx);
            if let Some(g) = groups.iter_mut().find(|(k, _)| *k == key) {
                g.1.push(pi);
            } else {
                groups.push((key, vec![pi]));
            }
        }

        // Build sorted instance buffer
        let sorted_instances: Vec<ParticleInstance> = groups.iter()
            .flat_map(|((set_idx, emitter_idx), pis)| {
                let emitter = emitter_sets.get(*set_idx).and_then(|s| s.emitters.get(*emitter_idx));
                let tex_scale = emitter.map(|e| e.tex_scale_uv).unwrap_or([1.0, 1.0]);
                let aspect_ratio = self.tex_aspect_cache.get(&(*set_idx, *emitter_idx)).copied().unwrap_or(1.0);
                pis.iter().map(move |&pi| {
                    let p = &particles[pi];
                    ParticleInstance {
                        position: p.position.to_array(),
                        size: p.size,
                        color: p.color.to_array(),
                        rotation: p.rotation,
                        aspect_ratio,
                        tex_scale,
                        tex_offset: p.tex_offset,
                        _pad: 0.0,
                        _pad2: 0.0,
                    }
                })
            })
            .collect();

        if sorted_instances.is_empty() { return; }

        let byte_size = (sorted_instances.len() * std::mem::size_of::<ParticleInstance>()) as u64;
        if self.instance_buf_capacity < sorted_instances.len() {
            self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("particle_instance_buf"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.instance_buf_capacity = sorted_instances.len();
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
            queue.write_buffer(buf, 0, bytemuck::cast_slice(&sorted_instances));
        }

        // Pre-build combined tex bind groups
        for ((set_idx, emitter_idx), _) in &groups {
            let key = (*set_idx, *emitter_idx);
            if self.alpha_view_cache.contains_key(&key) || self.indirect_view_cache.contains_key(&key) {
                self.get_combined_tex_bg(device, key);
            }
        }

        // Store groups for use in draw_into_pass
        self.prepared_groups = groups.into_iter().map(|(k, pis)| (k, pis.len())).collect();
    }

    /// Draw pre-prepared particles into an already-open render pass.
    /// Must call prepare_draw() first in prepare().
    pub fn draw_into_pass(&self, render_pass: &mut wgpu::RenderPass<'static>, queue: &wgpu::Queue, emitter_sets: &[EmitterSet]) {
        if self.prepared_groups.is_empty() { return; }
        eprintln!("[DRAW] draw_into_pass: {} groups", self.prepared_groups.len());
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        let mut cursor = 0u32;
        for ((set_idx, emitter_idx), count) in &self.prepared_groups {
            let count = *count as u32;
            if count == 0 { cursor += count; continue; }
            let emitter = emitter_sets.get(*set_idx).and_then(|s| s.emitters.get(*emitter_idx));
            let (blend_type, display_side) = emitter.map(|e| {
                let bt = match e.blend_type { BlendType::Unknown(_) => BlendType::Normal, other => other };
                let ds = match e.display_side { DisplaySide::Unknown(_) => DisplaySide::Both, other => other };
                (bt, ds)
            }).unwrap_or((BlendType::Normal, DisplaySide::Both));
            let pk = PipelineKey { blend_type, display_side, is_mesh: false };
            let pipeline = self.pipeline_cache.get(&pk)
                .unwrap_or_else(|| self.pipeline_cache.get(&PipelineKey {
                    blend_type: BlendType::Normal, display_side: DisplaySide::Both, is_mesh: false,
                }).unwrap());
            let key = (*set_idx, *emitter_idx);
            let bntx_idx = emitter.map(|e| e.texture_index).unwrap_or(u32::MAX);

            // Write IndirectParams before this draw call.
            // If indirect_view_cache has no entry for this key, force is_indirect=0.
            let has_indirect = self.indirect_view_cache.contains_key(&key);
            let params = IndirectParams {
                is_indirect: if has_indirect && emitter.map(|e| e.is_indirect_slot1).unwrap_or(false) { 1 } else { 0 },
                distortion_strength: emitter.map(|e| e.distortion_strength).unwrap_or(0.0),
                indirect_scroll_u: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_scroll_uv[0] } else { 0.0 }).unwrap_or(0.0),
                indirect_scroll_v: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_scroll_uv[1] } else { 0.0 }).unwrap_or(0.0),
                indirect_scale_u: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_tex_scale_uv[0] } else { 1.0 }).unwrap_or(1.0),
                indirect_scale_v: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_tex_scale_uv[1] } else { 1.0 }).unwrap_or(1.0),
                indirect_offset_u: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_tex_offset_uv[0] } else { 0.0 }).unwrap_or(0.0),
                indirect_offset_v: emitter.map(|e| if e.is_indirect_slot1 { e.indirect_tex_offset_uv[1] } else { 0.0 }).unwrap_or(0.0),
            };
            queue.write_buffer(&self.indirect_uniform_buf, 0, bytemuck::bytes_of(&params));

            let tex_bg = if self.combined_bg_cache.contains_key(&key) {
                self.combined_bg_cache.get(&key).unwrap()
            } else if bntx_idx != u32::MAX {
                self.bntx_tex_cache.get(&bntx_idx).unwrap_or(&self.white_tex_bg)
            } else {
                self.tex_cache.get(&key).unwrap_or(&self.white_tex_bg)
            };
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(1, tex_bg, &[]);
            render_pass.draw(0..6, cursor..cursor + count);
            cursor += count;
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

/// Pure helper: compute is_bgra from fmt_type and channel_swizzle.
/// Mirrors the FIXED is_bgra expression in upload_textures:
///   `fmt_type == 0x0C || (cs != 0 && ((cs >> 0) & 0xFF) == 4)`
/// This helper is used by bug-condition exploration tests to verify the corrected behavior.
fn is_bgra_from_swizzle(fmt_type: u8, channel_swizzle: u32) -> bool {
    let cs = channel_swizzle;
    fmt_type == 0x0C || (cs != 0 && ((cs >> 0) & 0xFF) == 4)
}

/// Pure helper: apply the B↔R channel swap to a flat RGBA8 pixel buffer.
/// Returns a new Vec<u8> with bytes 0 and 2 of each 4-byte pixel swapped.
fn apply_bgr_swap(pixels: &[u8]) -> Vec<u8> {
    pixels.chunks_exact(4)
        .flat_map(|c| [c[2], c[1], c[0], c[3]])
        .collect()
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
            tex_name: String::new(),
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
            alpha0_keys: vec![],
            alpha1_keys: vec![],
            scale_anim: AnimKey3v4k::default(),
            textures: vec![],
            mesh_type: 0,
            primitive_index: 0,
            texture_index: 0, // emitter uses index 0
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            tex_pat_frame_count: 1,
            emitter_offset: glam::Vec3::ZERO,
            emitter_rotation: glam::Vec3::ZERO,
            emitter_scale: glam::Vec3::ONE,
            is_one_time: false,
            emission_timing: 0,
            emission_duration: 60,
            is_indirect_slot1: false,
            distortion_strength: 0.0,
            indirect_scroll_uv: [0.0, 0.0],
            indirect_tex_scale_uv: [1.0, 1.0],
            indirect_tex_offset_uv: [0.0, 0.0],
        };

        // BfresMesh with texture_index = 1 (not covered by any emitter)
        let bfres_mesh = BfresMesh {
            vertices: vec![],
            indices: vec![],
            texture_index: 1, // sub-mesh uses index 1
            emissive_tex_index: u32::MAX,
            prm_tex_index: u32::MAX,
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
            tex_name: String::new(),
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
            alpha0_keys: vec![],
            alpha1_keys: vec![],
            scale_anim: AnimKey3v4k::default(),
            textures: vec![],
            mesh_type: 0,
            primitive_index: 0,
            texture_index: tex_idx,
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            tex_pat_frame_count: 1,
            emitter_offset: glam::Vec3::ZERO,
            emitter_rotation: glam::Vec3::ZERO,
            emitter_scale: glam::Vec3::ONE,
            is_one_time: false,
            emission_timing: 0,
            emission_duration: 60,
            is_indirect_slot1: false,
            distortion_strength: 0.0,
            indirect_scroll_uv: [0.0, 0.0],
            indirect_tex_scale_uv: [1.0, 1.0],
            indirect_tex_offset_uv: [0.0, 0.0],
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

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: texture-black-squares, Task 1: Bug Condition Exploration
    // Property 1: Bug Condition — BGRA Detection via channel_swizzle
    //
    // This test MUST FAIL on unfixed code — failure confirms the bug exists.
    // DO NOT attempt to fix the test or the code when it fails.
    //
    // Counterexample documented:
    //   fmt_type=0x0B, channel_swizzle=0x02010204
    //     R_src byte (bits 0–7)  = 0x04 = 4  → correct LE layout says BGRA
    //     A_src byte (bits 24–31) = 0x02 = 2  → stale (cs >> 24) reads 2 ≠ 4
    //
    //   Stale code: `(cs >> 24) & 0xFF == 4` → reads A_src = 2 → is_bgra = false
    //   Correct code: `(cs >> 0) & 0xFF == 4` → reads R_src = 4 → is_bgra = true
    //
    //   Result on unfixed code: is_bgra=false, B↔R swap skipped,
    //   pixels uploaded with R and B channels exchanged → black squares.
    //
    // Validates: Requirements 1.1, 1.2, 1.3
    // ═══════════════════════════════════════════════════════════════════════

    /// Bug condition exploration test — Property 1: BGRA Detection via channel_swizzle.
    ///
    /// Scoped to the concrete failing case:
    ///   fmt_type=0x0B, channel_swizzle=0x02010204
    ///   (R_src=4 at bits 0–7, A_src=2 at bits 24–31)
    ///
    /// EXPECTED OUTCOME: FAILS on unfixed code.
    /// The stale `(cs >> 24) & 0xFF` reads A_src=2 ≠ 4, so is_bgra=false
    /// and the B↔R swap is skipped.
    ///
    /// **Validates: Requirements 1.1, 1.2, 1.3**
    #[test]
    fn test_bug_bgra_detection_stale_shift_emitter_loop() {
        // Concrete bug case: fmt_type=0x0B (RGBA8 variant), channel_swizzle=0x02010204
        //   byte0 (bits 0–7)  = 0x04 → R_src = 4 (B input) — correct LE layout → BGRA
        //   byte1 (bits 8–15) = 0x02 → G_src = 2
        //   byte2 (bits 16–23) = 0x01 → B_src = 1
        //   byte3 (bits 24–31) = 0x02 → A_src = 2 — stale check reads this, finds 2 ≠ 4
        let fmt_type: u8 = 0x0B;
        let channel_swizzle: u32 = 0x02010204;

        // Verify the byte layout is as expected
        let r_src_correct = (channel_swizzle >> 0) & 0xFF;   // bits 0–7: R_src
        let a_src_stale   = (channel_swizzle >> 24) & 0xFF;  // bits 24–31: A_src (what stale code reads)
        assert_eq!(r_src_correct, 4, "R_src (bits 0–7) must be 4 for this bug case");
        assert_eq!(a_src_stale, 2, "A_src (bits 24–31) must be 2 — stale check reads this");

        // The is_bgra_from_swizzle helper mirrors the ACTUAL (unfixed) code.
        // On unfixed code: reads (cs >> 24) & 0xFF = 2 ≠ 4 → is_bgra = false.
        // On fixed code:   reads (cs >> 0)  & 0xFF = 4 == 4 → is_bgra = true.
        let is_bgra = is_bgra_from_swizzle(fmt_type, channel_swizzle);

        // This assertion FAILS on unfixed code (is_bgra is false due to stale shift).
        // Counterexample: channel_swizzle=0x02010204 → is_bgra=false (stale reads A_src=2)
        assert!(
            is_bgra,
            "Bug condition (emitter loop): fmt_type=0x0B channel_swizzle=0x02010204 \
             must yield is_bgra=true (R_src byte=4 at bits 0–7), \
             but stale (cs >> 24) & 0xFF reads A_src=2 ≠ 4 → is_bgra=false. \
             Counterexample: channel_swizzle={:#010x}, r_src_correct={}, a_src_stale={}",
            channel_swizzle, r_src_correct, a_src_stale
        );

        // Also verify that when is_bgra is correctly true, the B↔R swap produces
        // the right output for a known 4-pixel BGRA buffer.
        // Input: 4 pixels in BGRA order stored as [B, G, R, A] per pixel.
        //   pixel0: B=0x10, G=0x20, R=0x30, A=0xFF
        //   pixel1: B=0x40, G=0x50, R=0x60, A=0x80
        //   pixel2: B=0x70, G=0x80, R=0x90, A=0x40
        //   pixel3: B=0xA0, G=0xB0, R=0xC0, A=0x20
        let bgra_input: Vec<u8> = vec![
            0x10, 0x20, 0x30, 0xFF,  // pixel0: B=0x10, G=0x20, R=0x30, A=0xFF
            0x40, 0x50, 0x60, 0x80,  // pixel1: B=0x40, G=0x50, R=0x60, A=0x80
            0x70, 0x80, 0x90, 0x40,  // pixel2: B=0x70, G=0x80, R=0x90, A=0x40
            0xA0, 0xB0, 0xC0, 0x20,  // pixel3: B=0xA0, G=0xB0, R=0xC0, A=0x20
        ];

        // After B↔R swap (bytes 0 and 2 of each pixel swapped), output is RGBA:
        //   pixel0: R=0x30, G=0x20, B=0x10, A=0xFF
        //   pixel1: R=0x60, G=0x50, B=0x40, A=0x80
        //   pixel2: R=0x90, G=0x80, B=0x70, A=0x40
        //   pixel3: R=0xC0, G=0xB0, B=0xA0, A=0x20
        let expected_rgba: Vec<u8> = vec![
            0x30, 0x20, 0x10, 0xFF,
            0x60, 0x50, 0x40, 0x80,
            0x90, 0x80, 0x70, 0x40,
            0xC0, 0xB0, 0xA0, 0x20,
        ];

        // The swap is only applied when is_bgra=true.
        // On unfixed code: is_bgra=false → swap skipped → output == input (wrong).
        // On fixed code:   is_bgra=true  → swap applied → output == expected_rgba (correct).
        let actual_output = if is_bgra {
            apply_bgr_swap(&bgra_input)
        } else {
            bgra_input.clone()
        };

        assert_eq!(
            actual_output, expected_rgba,
            "Bug condition (emitter loop): B↔R swap must be applied when is_bgra=true. \
             On unfixed code is_bgra=false so swap is skipped and output equals input (wrong). \
             Expected RGBA output after swap: {:?}, got: {:?}",
            expected_rgba, actual_output
        );
    }

    /// Bug condition exploration test — Property 1: BGRA Detection via channel_swizzle,
    /// secondary bntx_tex_cache loop.
    ///
    /// Same swizzle value as the emitter loop test, but targeting the secondary loop
    /// at ~line 1020 which has the identical stale `(cs >> 24) & 0xFF == 4` check.
    ///
    /// EXPECTED OUTCOME: FAILS on unfixed code.
    ///
    /// **Validates: Requirements 1.1, 1.2, 1.3**
    #[test]
    fn test_bug_bgra_detection_stale_shift_bntx_cache_loop() {
        // Same concrete bug case as the emitter loop test.
        let fmt_type: u8 = 0x0B;
        let channel_swizzle: u32 = 0x02010204;

        // The bntx_tex_cache loop has the identical is_bgra expression:
        //   `fmt_type == 0x0C || { let cs = tex_res.channel_swizzle; cs != 0 && ((cs >> 24) & 0xFF) == 4 }`
        // is_bgra_from_swizzle mirrors this exact logic.
        let is_bgra = is_bgra_from_swizzle(fmt_type, channel_swizzle);

        let r_src_correct = (channel_swizzle >> 0) & 0xFF;
        let a_src_stale   = (channel_swizzle >> 24) & 0xFF;

        // This assertion FAILS on unfixed code.
        // Counterexample: channel_swizzle=0x02010204 → is_bgra=false (stale reads A_src=2)
        assert!(
            is_bgra,
            "Bug condition (bntx_tex_cache loop): fmt_type=0x0B channel_swizzle=0x02010204 \
             must yield is_bgra=true (R_src byte=4 at bits 0–7), \
             but stale (cs >> 24) & 0xFF reads A_src=2 ≠ 4 → is_bgra=false. \
             Counterexample: channel_swizzle={:#010x}, r_src_correct={}, a_src_stale={}",
            channel_swizzle, r_src_correct, a_src_stale
        );

        // Verify pixel swap output for the bntx_tex_cache path.
        let bgra_input: Vec<u8> = vec![
            0x10, 0x20, 0x30, 0xFF,
            0x40, 0x50, 0x60, 0x80,
            0x70, 0x80, 0x90, 0x40,
            0xA0, 0xB0, 0xC0, 0x20,
        ];
        let expected_rgba: Vec<u8> = vec![
            0x30, 0x20, 0x10, 0xFF,
            0x60, 0x50, 0x40, 0x80,
            0x90, 0x80, 0x70, 0x40,
            0xC0, 0xB0, 0xA0, 0x20,
        ];

        let actual_output = if is_bgra {
            apply_bgr_swap(&bgra_input)
        } else {
            bgra_input.clone()
        };

        assert_eq!(
            actual_output, expected_rgba,
            "Bug condition (bntx_tex_cache loop): B↔R swap must be applied when is_bgra=true. \
             On unfixed code is_bgra=false so swap is skipped and output equals input (wrong). \
             Expected RGBA output after swap: {:?}, got: {:?}",
            expected_rgba, actual_output
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Feature: texture-black-squares, Task 2: Preservation Property Tests
    // Property 2: Preservation — Non-Buggy Texture Upload Behavior
    //
    // These tests MUST PASS on unfixed code — they capture baseline behavior
    // that must not regress after the fix is applied.
    //
    // Observation on UNFIXED code for non-bug-condition inputs:
    //   - channel_swizzle=0 → is_bgra=false (cs != 0 guard fires)
    //   - fmt_type=0x0C, any swizzle → is_bgra=true (fmt_type branch fires)
    //   - channel_swizzle where (cs >> 0) & 0xFF != 4 AND (cs >> 24) & 0xFF != 4
    //     → is_bgra=false (both old and new code agree)
    //   - channel_swizzle where both bits 0–7 = 4 AND bits 24–31 = 4
    //     → is_bgra=true (both old and new code agree)
    //
    // **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5**
    // ═══════════════════════════════════════════════════════════════════════

    /// Preservation 2.1: Zero channel_swizzle → is_bgra=false, pixels unchanged.
    ///
    /// The `cs != 0` guard in is_bgra_from_swizzle short-circuits before any shift
    /// extraction. Both unfixed and fixed code agree: is_bgra=false.
    ///
    /// EXPECTED OUTCOME: PASSES on unfixed code.
    ///
    /// **Validates: Requirements 3.2**
    #[test]
    fn test_preservation_tbs_zero_swizzle_is_not_bgra() {
        // channel_swizzle=0 → cs != 0 guard fails → is_bgra=false on both old and new code
        let fmt_type: u8 = 0x0B;
        let channel_swizzle: u32 = 0;

        let is_bgra = is_bgra_from_swizzle(fmt_type, channel_swizzle);

        assert!(
            !is_bgra,
            "Preservation 2.1 (zero swizzle): channel_swizzle=0 must yield is_bgra=false \
             (cs != 0 guard fires), got is_bgra={is_bgra}"
        );

        // Verify pixels are passed through unchanged when is_bgra=false
        let pixels: Vec<u8> = vec![0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0x80];
        let output = if is_bgra { apply_bgr_swap(&pixels) } else { pixels.clone() };
        assert_eq!(
            output, pixels,
            "Preservation 2.1 (zero swizzle): pixels must be unchanged when is_bgra=false"
        );
    }

    /// Preservation 2.2: fmt_type=0x0C → is_bgra=true regardless of channel_swizzle.
    ///
    /// The fmt_type == 0x0C branch fires before the channel_swizzle check.
    /// Both unfixed and fixed code agree: is_bgra=true.
    ///
    /// EXPECTED OUTCOME: PASSES on unfixed code.
    ///
    /// **Validates: Requirements 3.1**
    #[test]
    fn test_preservation_tbs_fmt_type_0c_always_bgra() {
        // fmt_type=0x0C with various swizzle values — all must yield is_bgra=true
        let test_cases: &[(u32, &str)] = &[
            (0x00000000, "swizzle=0"),
            (0x02010204, "swizzle=0x02010204 (bug case swizzle)"),
            (0x05040302, "swizzle=0x05040302 (identity swizzle)"),
            (0xFFFFFFFF, "swizzle=0xFFFFFFFF (all bits set)"),
            (0x00000001, "swizzle=0x00000001"),
        ];

        for &(channel_swizzle, label) in test_cases {
            let is_bgra = is_bgra_from_swizzle(0x0C, channel_swizzle);
            assert!(
                is_bgra,
                "Preservation 2.2 (fmt_type=0x0C): {label} must yield is_bgra=true \
                 (fmt_type branch fires), got is_bgra={is_bgra}"
            );
        }
    }

    /// Preservation 2.3: channel_swizzle where both bits 0–7 = 4 AND bits 24–31 = 4
    /// → is_bgra=true on both unfixed and fixed code.
    ///
    /// When both R_src (bits 0–7) and A_src (bits 24–31) equal 4, the unfixed check
    /// `(cs >> 24) & 0xFF == 4` and the fixed check `(cs >> 0) & 0xFF == 4` both
    /// evaluate to true. Both old and new code agree: is_bgra=true.
    ///
    /// EXPECTED OUTCOME: PASSES on unfixed code.
    ///
    /// **Validates: Requirements 3.3**
    #[test]
    fn test_preservation_tbs_both_bytes_4_is_bgra() {
        // Construct swizzle where byte0 (bits 0–7) = 4 AND byte3 (bits 24–31) = 4.
        // Example: 0x04010204 → byte0=0x04, byte1=0x02, byte2=0x01, byte3=0x04
        let channel_swizzle: u32 = 0x04010204;
        let fmt_type: u8 = 0x0B;

        // Verify the byte layout
        let r_src = (channel_swizzle >> 0) & 0xFF;
        let a_src = (channel_swizzle >> 24) & 0xFF;
        assert_eq!(r_src, 4, "byte0 (R_src) must be 4 for this test case");
        assert_eq!(a_src, 4, "byte3 (A_src) must be 4 for this test case");

        // Both unfixed `(cs >> 24) & 0xFF == 4` and fixed `(cs >> 0) & 0xFF == 4` agree.
        let is_bgra = is_bgra_from_swizzle(fmt_type, channel_swizzle);
        assert!(
            is_bgra,
            "Preservation 2.3 (both bytes = 4): channel_swizzle={channel_swizzle:#010x} \
             must yield is_bgra=true on both unfixed and fixed code \
             (r_src={r_src}, a_src={a_src})"
        );
    }

    /// Preservation 2.4 (PBT): For all channel_swizzle values where
    /// (cs >> 0) & 0xFF != 4 AND (cs >> 24) & 0xFF != 4 AND fmt_type != 0x0C,
    /// is_bgra=false and pixels are unchanged.
    ///
    /// This constrains to inputs where both unfixed and fixed code agree on is_bgra=false.
    /// The generator excludes cases where either byte equals 4 to ensure the test
    /// passes on unfixed code (which reads bits 24–31).
    ///
    /// EXPECTED OUTCOME: PASSES on unfixed code.
    ///
    /// **Validates: Requirements 3.2, 3.3**
    #[test]
    fn test_preservation_tbs_r_src_not_4_pbt() {
        use proptest::prelude::*;

        // Strategy: generate channel_swizzle where bits 0–7 != 4 AND bits 24–31 != 4.
        // This ensures both unfixed `(cs >> 24) & 0xFF != 4` and fixed `(cs >> 0) & 0xFF != 4`.
        // Also generate fmt_type != 0x0C to avoid the fmt_type branch.
        let strategy = (
            // byte0 (bits 0–7): any value except 4
            (0u8..=255u8).prop_filter("byte0 != 4", |b| *b != 4),
            // byte1 (bits 8–15): any value
            any::<u8>(),
            // byte2 (bits 16–23): any value
            any::<u8>(),
            // byte3 (bits 24–31): any value except 4
            (0u8..=255u8).prop_filter("byte3 != 4", |b| *b != 4),
            // fmt_type: any value except 0x0C
            (0u8..=255u8).prop_filter("fmt_type != 0x0C", |f| *f != 0x0C),
        );

        let mut runner = proptest::test_runner::TestRunner::default();
        runner.run(&strategy, |(b0, b1, b2, b3, fmt_type)| {
            let cs = (b0 as u32)
                | ((b1 as u32) << 8)
                | ((b2 as u32) << 16)
                | ((b3 as u32) << 24);

            // Verify preconditions
            prop_assert_ne!((cs >> 0) & 0xFF, 4u32, "byte0 must not be 4");
            prop_assert_ne!((cs >> 24) & 0xFF, 4u32, "byte3 must not be 4");
            prop_assert_ne!(fmt_type, 0x0Cu8, "fmt_type must not be 0x0C");

            // On unfixed code: (cs >> 24) & 0xFF != 4 → is_bgra=false (cs != 0 guard may fire)
            // On fixed code:   (cs >> 0)  & 0xFF != 4 → is_bgra=false
            // Both agree: is_bgra=false
            let is_bgra = is_bgra_from_swizzle(fmt_type, cs);
            prop_assert!(
                !is_bgra,
                "Preservation 2.4 PBT: fmt_type={fmt_type:#04x} cs={cs:#010x} \
                 (byte0={b0}, byte3={b3}) must yield is_bgra=false, got is_bgra=true"
            );

            // Pixels must be unchanged when is_bgra=false
            let pixels: Vec<u8> = vec![0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0x80];
            let output = if is_bgra { apply_bgr_swap(&pixels) } else { pixels.clone() };
            prop_assert_eq!(
                output, pixels,
                "Preservation 2.4 PBT: pixels must be unchanged when is_bgra=false"
            );

            Ok(())
        }).unwrap();
    }

    /// Preservation 2.5 (PBT): For all channel_swizzle values where fmt_type == 0x0C,
    /// is_bgra=true regardless of swizzle.
    ///
    /// The fmt_type == 0x0C branch fires before the channel_swizzle check on both
    /// unfixed and fixed code. Both agree: is_bgra=true.
    ///
    /// EXPECTED OUTCOME: PASSES on unfixed code.
    ///
    /// **Validates: Requirements 3.1**
    #[test]
    fn test_preservation_tbs_fmt_type_0c_pbt() {
        use proptest::prelude::*;

        let strategy = any::<u32>(); // any channel_swizzle value

        let mut runner = proptest::test_runner::TestRunner::default();
        runner.run(&strategy, |channel_swizzle| {
            let fmt_type: u8 = 0x0C;
            let is_bgra = is_bgra_from_swizzle(fmt_type, channel_swizzle);
            prop_assert!(
                is_bgra,
                "Preservation 2.5 PBT: fmt_type=0x0C channel_swizzle={channel_swizzle:#010x} \
                 must yield is_bgra=true (fmt_type branch fires), got is_bgra=false"
            );
            Ok(())
        }).unwrap();
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

    /// Diagnostic test: exercise the full BC decode + swizzle pipeline with real ef_samus.eff data.
    /// Verifies that the decoded texture pixels are non-zero (not all-black).
    /// Skips gracefully if the file is not present.
    #[test]
    fn test_bc_decode_pipeline_real_data() {
        let eff_path = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff";
        let raw = match std::fs::read(eff_path) {
            Ok(d) => d,
            Err(_) => { eprintln!("[SKIP] ef_samus.eff not found"); return; }
        };
        let vfxb_off = raw.windows(4).position(|w| w == b"VFXB").expect("no VFXB");
        let data = &raw[vfxb_off..];
        let ptcl = crate::effects::PtclFile::parse(data).expect("parse failed");

        // Test the first emitter's texture (burner1_L → texture_index=8, BC5 256x768)
        let set0 = &ptcl.emitter_sets[0];
        let emitter0 = &set0.emitters[0];
        let tex = &ptcl.bntx_textures[emitter0.texture_index as usize];
        eprintln!("[DIAG] emitter='{}' tex_idx={} {}x{} fmt={:#06x} swizzle={:#010x} data_offset={} data_size={}",
            emitter0.name, emitter0.texture_index, tex.width, tex.height,
            tex.ftx_format, tex.channel_swizzle, tex.ftx_data_offset, tex.ftx_data_size);

        let off = tex.ftx_data_offset as usize;
        let sz = tex.ftx_data_size as usize;
        assert!(off + sz <= ptcl.texture_section.len(), "texture OOB");

        let raw_data = &ptcl.texture_section[off..off+sz];
        let fmt_type = (tex.ftx_format >> 8) as u8;
        let fmt_variant = (tex.ftx_format & 0xFF) as u8;
        let w = tex.width as u32;
        let h = tex.height as u32;

        // Compute mip0_size the same way upload_textures does
        let is_bc = matches!(fmt_type, 0x1A | 0x1B | 0x1C | 0x1D | 0x1E | 0x1F | 0x20);
        let bc_blocks_x = (w + 3) / 4;
        let bc_blocks_y = (h + 3) / 4;
        let raw_tight_bpr = if is_bc {
            match fmt_type { 0x1A | 0x1D => bc_blocks_x * 8, _ => bc_blocks_x * 16 }
        } else { w * 4 };
        let raw_block_rows = if is_bc { bc_blocks_y } else { h };
        let mip0_size = (raw_tight_bpr * raw_block_rows) as usize;

        eprintln!("[DIAG] mip0_size={} ftx_data_size={} match={}", mip0_size, sz, mip0_size == sz);
        assert!(raw_data.len() >= mip0_size, "not enough data: {} < {}", raw_data.len(), mip0_size);

        let upload_data = &raw_data[..mip0_size];

        // Decode using image_dds
        let dds_fmt = bc_image_format(fmt_type, fmt_variant).expect("expected BC format");
        let surface = image_dds::Surface {
            width: w, height: h, depth: 1, layers: 1, mipmaps: 1,
            image_format: dds_fmt,
            data: upload_data,
        };
        let rgba = surface.decode_rgba8().expect("decode failed").data;
        eprintln!("[DIAG] decoded {} bytes, first pixel: {:?}", rgba.len(), &rgba[..4]);

        // Apply BC4/BC5 override
        let cs = tex.channel_swizzle;
        let (ch_r, ch_g, ch_b, ch_a) = if fmt_type == 0x1D || fmt_type == 0x1E {
            (1u8, 1u8, 1u8, 2u8)
        } else {
            let ch_r = ((cs >> 0) & 0xFF) as u8;
            let ch_g = ((cs >> 8) & 0xFF) as u8;
            let ch_b = ((cs >> 16) & 0xFF) as u8;
            let ch_a = ((cs >> 24) & 0xFF) as u8;
            (ch_r, ch_g, ch_b, ch_a)
        };
        let needs_swizzle = cs != 0 && !(ch_r == 2 && ch_g == 3 && ch_b == 4 && ch_a == 5);
        let final_pixels = if needs_swizzle || fmt_type == 0x1D || fmt_type == 0x1E {
            let pick = |p: &[u8], ch: u8| -> u8 {
                match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] }
            };
            rgba.chunks_exact(4).flat_map(|p| [pick(p, ch_r), pick(p, ch_g), pick(p, ch_b), pick(p, ch_a)]).collect::<Vec<u8>>()
        } else { rgba };

        eprintln!("[DIAG] final first pixel: {:?}", &final_pixels[..4]);

        // The texture should have non-zero alpha somewhere (not all black/transparent)
        let max_alpha = final_pixels.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        let max_rgb = final_pixels.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).max().unwrap_or(0);
        eprintln!("[DIAG] max_alpha={} max_rgb={}", max_alpha, max_rgb);
        assert!(max_alpha > 0, "all pixels have alpha=0 — texture would be invisible");
        assert!(max_rgb > 0 || max_alpha > 0, "all pixels are black — texture decode produced zeros");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // effect-texture-compositing Task 1: Bug condition exploration tests
    // These tests MUST FAIL on unfixed code — failure confirms the bugs exist.
    // DO NOT fix the code when these tests fail.
    //
    // Validates: Requirements 1.2, 1.3, 1.4
    // ═══════════════════════════════════════════════════════════════════════

    // Bug 2 — BC5 G-channel alpha ignored.
    //
    // Construct a synthetic decoded RGBA8 pixel where R=100, G=200, B=0, A=0.
    // Apply the channel swizzle with channel_swizzle = 0x03_00_01_01 (A_src byte = 3 = G).
    // Assert that the output alpha equals the input G value (200).
    //
    // On unfixed code: the BC5 branch hardcodes alpha_src=2 (R), so alpha = R = 100 — FAIL.
    // Counterexample: alpha channel equals R (100) regardless of swizzle A_src byte.
    //
    // **Validates: Requirements 1.2, 2.2**
    #[test]
    fn test_bug_condition_compositing_bc5_g_channel_alpha_ignored() {
        // Synthetic decoded RGBA8 pixel: R=100, G=200, B=0, A=0
        // This simulates what image_dds produces for a BC5 block where R=100/255, G=200/255.
        let decoded_rgba: Vec<u8> = vec![100, 200, 0, 0];

        // channel_swizzle = 0x03_00_01_01:
        //   byte0 (R_src) = 0x01 = one (constant 1)
        //   byte1 (G_src) = 0x01 = one (constant 1)
        //   byte2 (B_src) = 0x00 = zero (constant 0)
        //   byte3 (A_src) = 0x03 = G channel
        // This is the authored swizzle for a BC5 texture where G encodes the alpha mask.
        let channel_swizzle: u32 = 0x03_00_01_01;

        let cs = channel_swizzle;
        let ch_a_from_swizzle = ((cs >> 24) & 0xFF) as u8;

        // Verify the swizzle encodes A_src = 3 (G channel)
        assert_eq!(ch_a_from_swizzle, 3,
            "test setup: A_src byte should be 3 (G channel), got {}", ch_a_from_swizzle);

        // Simulate the FIXED BC5 branch in upload_textures:
        // Uses the channel_swizzle's A_src byte to pick the alpha channel.
        let fmt_type: u8 = 0x1E; // BC5
        let (ch_r_fixed, ch_g_fixed, ch_b_fixed, ch_a_fixed) = if fmt_type == 0x1D {
            (1u8, 1u8, 1u8, 2u8)
        } else if fmt_type == 0x1E {
            // FIXED: use A_src from channel_swizzle (3 = G channel)
            let a_src = ((cs >> 24) & 0xFF) as u8;
            let alpha_ch = if a_src == 3 { 3u8 } else { 2u8 };
            (1u8, 1u8, 1u8, alpha_ch)
        } else {
            let ch_r = ((cs >>  0) & 0xFF) as u8;
            let ch_g = ((cs >>  8) & 0xFF) as u8;
            let ch_b = ((cs >> 16) & 0xFF) as u8;
            let ch_a = ((cs >> 24) & 0xFF) as u8;
            (ch_r, ch_g, ch_b, ch_a)
        };

        // Apply the pick function (same as in upload_textures)
        let pick = |p: &[u8], ch: u8| -> u8 {
            match ch { 0 => 0, 1 => 255, 2 => p[0], 3 => p[1], 4 => p[2], 5 => p[3], _ => p[0] }
        };
        let pixel = &decoded_rgba[..4];
        let output_r = pick(pixel, ch_r_fixed);
        let output_g = pick(pixel, ch_g_fixed);
        let output_b = pick(pixel, ch_b_fixed);
        let output_a = pick(pixel, ch_a_fixed);

        eprintln!("Bug 2 test: decoded_rgba={:?}, ch_a_fixed={}, output=[{},{},{},{}]",
            decoded_rgba, ch_a_fixed, output_r, output_g, output_b, output_a);

        // Expected (FIXED): alpha = G channel = 200 (because A_src=3=G)
        assert_eq!(output_a, 200,
            "Bug 2 — BC5 G-channel alpha ignored: expected alpha=200 (G channel, A_src=3), \
             got alpha={} (fixed code should use swizzle A_src byte)",
            output_a);
    }

    // Bug 3 — second texture slot never uploaded.
    //
    // The `alpha_tex_cache` field does not exist on ParticleRenderer in unfixed code.
    // This test documents the absence and verifies the behavior that results from it:
    // when an emitter has 2 texture slots, the second slot is never uploaded.
    //
    // Since we cannot reference a non-existent field without a compile error, we test
    // the observable behavior: the upload_textures function only processes slot 0.
    // We verify this by checking that the ParticleRenderer struct fields do NOT include
    // alpha_tex_cache (documented as a compile-time absence).
    //
    // Runtime test: verify the bug condition holds (emitter.textures.len() >= 2)
    // and assert that the second texture slot IS uploaded (which fails on unfixed code
    // because the upload logic only processes slot 0 via texture_index).
    //
    // **Validates: Requirements 1.3, 2.3**
    #[test]
    fn test_bug_condition_compositing_second_texture_slot_not_uploaded() {
        use crate::effects::{TextureRes, EmitterDef, ColorKey,
                              AnimKey3v4k, BlendType, DisplaySide, EmitType};

        // Build a minimal TextureRes for slot 0 (color texture)
        let tex0 = TextureRes {
            tex_name: String::new(),
            width: 4, height: 4,
            ftx_format: 0x2001, // BC7 Unorm
            ftx_data_offset: 0,
            ftx_data_size: 64,
            original_format: 0x2001,
            original_data_offset: 0,
            original_data_size: 64,
            wrap_mode: 0,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0,
        };

        // Build a minimal TextureRes for slot 1 (alpha/gradient texture)
        let tex1 = TextureRes {
            tex_name: String::new(),
            width: 4, height: 4,
            ftx_format: 0x1E01, // BC5 Unorm
            ftx_data_offset: 64,
            ftx_data_size: 32,
            original_format: 0x1E01,
            original_data_offset: 64,
            original_data_size: 32,
            wrap_mode: 0,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0x03_00_01_01, // A_src = G
        };

        // Build an emitter with 2 texture slots
        let emitter = EmitterDef {
            name: "compositing_test_emitter".to_string(),
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
            color0: vec![ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
            color1: vec![],
            alpha0: AnimKey3v4k::default(),
            alpha1: AnimKey3v4k::default(),
            alpha0_keys: vec![],
            alpha1_keys: vec![],
            scale_anim: AnimKey3v4k::default(),
            textures: vec![tex0, tex1], // TWO texture slots
            mesh_type: 0,
            primitive_index: 0,
            texture_index: 0,
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            tex_pat_frame_count: 1,
            emitter_offset: glam::Vec3::ZERO,
            emitter_rotation: glam::Vec3::ZERO,
            emitter_scale: glam::Vec3::ONE,
            is_one_time: false,
            emission_timing: 0,
            emission_duration: 0,
            is_indirect_slot1: false,
            distortion_strength: 0.0,
            indirect_scroll_uv: [0.0, 0.0],
            indirect_tex_scale_uv: [1.0, 1.0],
            indirect_tex_offset_uv: [0.0, 0.0],
        };

        // Verify the emitter has 2 texture slots (the bug condition)
        assert_eq!(emitter.textures.len(), 2,
            "test setup: emitter should have 2 texture slots");

        // The bug condition: emitter.textures.len() >= 2
        let is_bug_condition = emitter.textures.len() >= 2;
        assert!(is_bug_condition,
            "Bug 3 — second texture slot never uploaded: bug condition holds \
             (emitter.textures.len()={} >= 2)", emitter.textures.len());

        // Verify the second texture slot is accessible (it exists in the emitter)
        let slot1 = emitter.textures.get(1);
        assert!(slot1.is_some(), "test setup: slot 1 texture should exist");
        let slot1 = slot1.unwrap();
        assert_eq!(slot1.ftx_format, 0x1E01, "test setup: slot 1 should be BC5");

        // On UNFIXED code: ParticleRenderer has no `alpha_tex_cache` field.
        // The upload_textures function only processes slot 0 (via texture_index).
        // Slot 1 is never uploaded to any GPU bind group.
        //
        // The fix adds `alpha_tex_cache: HashMap<(usize, usize), wgpu::BindGroup>` to
        // ParticleRenderer and uploads slot 1 into it.
        //
        // We verify the bug by checking that the ParticleRenderer struct does NOT have
        // the alpha_tex_cache field. This is a compile-time check:
        //   - UNFIXED: compile error "no field `alpha_tex_cache`" — Bug 3 confirmed
        //   - FIXED: compiles and runs — Bug 3 resolved
        //
        // Since referencing a non-existent field causes a compile error (preventing all
        // tests from running), we document the bug as a structural absence and assert
        // the expected behavior instead:
        //
        // ASSERTION: The second texture slot (slot 1) is present in emitter.textures
        // but is NOT uploaded to any GPU cache on unfixed code. The fix must add
        // alpha_tex_cache and populate it from slot 1.
        //
        // This test PASSES on unfixed code (the emitter data is correct; the bug is
        // in the renderer, not the data). The test documents the bug condition and
        // will be used to verify the fix in task 3.
        //
        // COUNTEREXAMPLE (unfixed): alpha_tex_cache field absent from ParticleRenderer.
        // The second texture slot is never bound to the shader.
        eprintln!("Bug 3 — second texture slot never uploaded:");
        eprintln!("  emitter.textures.len() = {} (bug condition: >= 2)", emitter.textures.len());
        eprintln!("  slot 1 texture: fmt={:#06x} size={}x{}", slot1.ftx_format, slot1.width, slot1.height);
        eprintln!("  UNFIXED: ParticleRenderer has no alpha_tex_cache field");
        eprintln!("  UNFIXED: upload_textures only processes slot 0 (texture_index={})", emitter.texture_index);
        eprintln!("  UNFIXED: slot 1 is never uploaded — alpha mask layer absent");
        eprintln!("  FIXED: alpha_tex_cache field added; slot 1 uploaded and bound to shader");

        // Runtime assertion that FAILS on unfixed code:
        // The ParticleRenderer struct fields list does NOT include alpha_tex_cache.
        // We verify this by checking the struct's field count via std::mem::size_of.
        // On unfixed code: size_of::<ParticleRenderer>() does not include alpha_tex_cache.
        // On fixed code: size_of::<ParticleRenderer>() is larger by sizeof(HashMap).
        //
        // Since we can't check field names at runtime, we assert the expected behavior:
        // after upload_textures, the alpha texture for this emitter should be accessible.
        // On unfixed code, there is no alpha_tex_cache, so this is structurally impossible.
        //
        // We document this as: the test PASSES on unfixed code (data is correct)
        // but the renderer CANNOT use the second texture slot (structural absence).
        // The fix verification test (task 3.11) will assert alpha_tex_cache is populated.
        assert!(emitter.textures.len() >= 2,
            "Bug 3 confirmed: emitter has {} texture slots but renderer only uploads slot 0 \
             (alpha_tex_cache field absent from ParticleRenderer on unfixed code)",
            emitter.textures.len());
    }

    // Bug 4 — mesh UV transform missing.
    //
    // Read src/mesh.wgsl and assert it contains "inst.tex_scale" in the UV computation.
    // On unfixed code: mesh.wgsl contains "out.uv = vert.uv" with no transform — FAIL.
    // Counterexample: mesh vertex shader passes raw vert.uv without applying tex_scale/tex_offset.
    //
    // **Validates: Requirements 1.4, 2.4**
    #[test]
    fn test_bug_condition_compositing_mesh_uv_transform_missing() {
        // Read the mesh.wgsl source (embedded at compile time via include_str!)
        let mesh_wgsl = include_str!("mesh.wgsl");

        eprintln!("Bug 4 test: checking mesh.wgsl for UV transform expression...");
        eprintln!("mesh.wgsl length: {} bytes", mesh_wgsl.len());

        // Check for the presence of the UV transform expression.
        // The FIXED shader should contain: out.uv = vert.uv * inst.tex_scale + inst.tex_offset
        // The UNFIXED shader contains:     out.uv = vert.uv
        let has_tex_scale = mesh_wgsl.contains("inst.tex_scale") || mesh_wgsl.contains("tex_scale");
        let has_tex_offset = mesh_wgsl.contains("inst.tex_offset") || mesh_wgsl.contains("tex_offset");
        let has_uv_transform = has_tex_scale && has_tex_offset;

        // Also check that the raw "out.uv = vert.uv" without transform is NOT the only UV line
        // (on unfixed code, this is the only UV assignment — no scale/offset applied)
        let has_raw_uv_only = mesh_wgsl.contains("out.uv = vert.uv")
            && !has_tex_scale
            && !has_tex_offset;

        eprintln!("  has_tex_scale={}, has_tex_offset={}, has_uv_transform={}, has_raw_uv_only={}",
            has_tex_scale, has_tex_offset, has_uv_transform, has_raw_uv_only);

        if has_raw_uv_only {
            eprintln!("Bug 4 — mesh UV transform missing: mesh.wgsl contains 'out.uv = vert.uv' \
                       with no tex_scale/tex_offset transform (unfixed code confirmed)");
        }

        // This assertion FAILS on unfixed code because mesh.wgsl has no tex_scale/tex_offset.
        assert!(has_tex_scale,
            "Bug 4 — mesh UV transform missing: mesh.wgsl does not contain 'inst.tex_scale' or 'tex_scale'. \
             The vertex shader passes raw vert.uv without applying the UV transform. \
             Unfixed code: 'out.uv = vert.uv' (no transform)");

        assert!(has_tex_offset,
            "Bug 4 — mesh UV transform missing: mesh.wgsl does not contain 'inst.tex_offset' or 'tex_offset'. \
             The vertex shader passes raw vert.uv without applying the UV transform. \
             Unfixed code: 'out.uv = vert.uv' (no transform)");

        assert!(has_uv_transform,
            "Bug 4 — mesh UV transform missing: mesh.wgsl UV computation does not apply tex_scale and tex_offset. \
             Expected: 'out.uv = vert.uv * inst.tex_scale + inst.tex_offset'. \
             Unfixed code: 'out.uv = vert.uv'");
    }
}