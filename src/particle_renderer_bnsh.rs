// Helper module for loading BNSH shaders into particle renderer
// Bridges the gap between effect files and GPU shader pipeline

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use crate::effects::{BlendType, EmitterDef, PtclFile};
use crate::bnsh_shader_integration::{
    decode_all_effect_shaders, decode_legacy_stage_pair,
    EffectShaderPair, ShaderStats, MaterialTextureBindings,
};
use crate::shader_registry::ShaderKey;

/// Device limits for BNSH particle pipelines.
///
/// `wgpu::Limits::default()` caps `max_storage_buffers_per_shader_stage` at 8, but NVN
/// particle pairs (e.g. Samus bomb) can merge to 9 set-0 storage buffers after FS remap.
/// Request the adapter's native limits instead.
pub fn wgpu_device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let mut limits = adapter.limits();
    limits.max_sampled_textures_per_shader_stage =
        limits.max_sampled_textures_per_shader_stage.max(32);
    limits
}

/// Depth format for per-draw_path offscreen passes (cleared each path like NVN).
pub const PARTICLE_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Sampled in soft-particle FS (`textureLoad`); must not use [`PARTICLE_DEPTH_FORMAT`]
/// because `Queue::write_texture` cannot target depth formats for CPU init.
pub const SOFT_PARTICLE_DEPTH_SAMPLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// Depth/stencil for offscreen particle passes. Billboards keep depth writes off so alpha
/// blending stays correct; the attachment is primed from mesh depth each path.
pub fn particle_depth_stencil_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: PARTICLE_DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth attachment for per-path offscreen particle passes.
///
/// When `mesh_depth_primed` is true the texture already holds copied mesh depth from
/// [`ssbh_wgpu::SsbhRenderer::copy_mesh_depth_resolved`]; otherwise it is cleared to `1.0`.
pub fn particle_path_depth_attachment<'a>(
    view: &'a wgpu::TextureView,
    mesh_depth_primed: bool,
) -> wgpu::RenderPassDepthStencilAttachment<'a> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: if mesh_depth_primed {
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(1.0)
            },
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

/// Depth test + write for opaque-core particles (within-path occlusion approximation).
pub fn particle_depth_stencil_state_write() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: PARTICLE_DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// GPU pipeline state for one BNSH shader variant (keyed by ShaderKey).
pub struct BnshPipelineState {
    pub vs_module: wgpu::ShaderModule,
    pub fs_module: wgpu::ShaderModule,
    /// Fragment shader with per-pixel alpha-test `discard` for opaque-core depth-write passes.
    pub fs_module_depth_write: wgpu::ShaderModule,
    pub vs_entry: String,
    pub fs_entry: String,
    pub descriptors: Vec<crate::spirv_to_wgsl::DescriptorInfo>,
    pub bind_group_layouts: Vec<wgpu::BindGroupLayout>,
    pub pipeline_layout: wgpu::PipelineLayout,
    pub pipeline_cache: HashMap<BlendType, wgpu::RenderPipeline>,
    /// Same blends as [`Self::pipeline_cache`] but with a depth attachment (offscreen paths).
    pub pipeline_cache_depth: HashMap<BlendType, wgpu::RenderPipeline>,
    /// Opaque-core pass: depth test + write for within-path occlusion.
    pub pipeline_cache_depth_write: HashMap<BlendType, wgpu::RenderPipeline>,
    pub storage_bufs: HashMap<u32, wgpu::Buffer>,
    pub bind_group_cache: HashMap<(usize, usize), wgpu::BindGroup>,
    pub vertex_layout: wgpu::VertexBufferLayout<'static>,
    pub cbuf_slot_usage: HashMap<String, HashSet<u32>>,
    pub extra_tex_slots_needed: [bool; 3],
    pub tex_blend_uniform_needed: bool,
    pub particle_alpha_uniform_needed: bool,
    pub soft_particle_needed: bool,
    pub vs_reflection: Option<crate::bnsh_reflection::ShaderStageReflection>,
    pub fs_reflection: Option<crate::bnsh_reflection::ShaderStageReflection>,
}

/// Loaded WGSL modules + descriptor reflection for one shader pair.
pub struct BnshLoadedModules {
    pub vs_module: wgpu::ShaderModule,
    pub fs_module: wgpu::ShaderModule,
    pub fs_module_depth_write: wgpu::ShaderModule,
    pub vs_entry: String,
    pub fs_entry: String,
    pub descriptors: Vec<crate::spirv_to_wgsl::DescriptorInfo>,
    pub cbuf_slot_usage: HashMap<String, HashSet<u32>>,
    pub extra_tex_slots_needed: [bool; 3],
    pub tex_blend_uniform_needed: bool,
    pub particle_alpha_uniform_needed: bool,
    pub soft_particle_needed: bool,
    pub vs_reflection: Option<crate::bnsh_reflection::ShaderStageReflection>,
    pub fs_reflection: Option<crate::bnsh_reflection::ShaderStageReflection>,
}

/// WGSL after NVN patch + particle simulation wiring (no GPU modules yet).
pub struct PreparedBnshWgsl {
    pub vs_wgsl: String,
    pub fs_wgsl: String,
    pub fs_wgsl_depth_write: String,
    pub uses_native_fs_fragment: bool,
    pub cbuf_slot_usage: HashMap<String, HashSet<u32>>,
    pub extra_tex_slots_needed: [bool; 3],
    pub tex_blend_uniform_needed: bool,
    pub particle_alpha_uniform_needed: bool,
    pub soft_particle_needed: bool,
}

/// Full CPU-side BNSH WGSL pipeline shared by the renderer and integration tests.
pub fn prepare_bnsh_wgsl(
    vs_wgsl: &str,
    fs_wgsl: &str,
    vs_hint: Option<crate::shader_registry::ShaderVsProfile>,
    vs_spirv: Option<&[u8]>,
    fs_spirv: Option<&[u8]>,
    native_color_hint: crate::shader_registry::NativeColorInput,
) -> PreparedBnshWgsl {
    let vs_prefixed = crate::spirv_to_wgsl::wire_vertex_simulation_varyings(vs_wgsl);
    let fs_prefixed = {
        let fs = crate::spirv_to_wgsl::wire_crossfade_fragment_input(fs_wgsl, &vs_prefixed);
        let fs = crate::spirv_to_wgsl::wire_extra_tex_fragment_input(&fs, &vs_prefixed);
        crate::spirv_to_wgsl::wire_quad_uv_fragment_input(&fs, &vs_prefixed)
    };
    let vs_wgsl = crate::spirv_to_wgsl::patch_vertex_wgsl_with_hint(
        &vs_prefixed,
        &fs_prefixed,
        vs_hint,
    );
    let fs_clamped = crate::spirv_to_wgsl::clamp_fragment_output_locations(
        &fs_prefixed,
        crate::spirv_to_wgsl::PARTICLE_COMPOSITE_MRT_LOCATIONS,
    );
    let wgsl_color_hint =
        crate::spirv_to_wgsl::infer_native_color_from_fs_wgsl(&fs_clamped);
    let native_color = native_color_hint.merge(wgsl_color_hint);
    let uses_native_fs =
        crate::spirv_to_wgsl::should_use_native_fs_fragment(&fs_clamped, native_color);
    let fs_wgsl = if uses_native_fs {
        let enhanced = crate::spirv_to_wgsl::enhance_native_fragment_wgsl_with_hint(
            &fs_clamped,
            native_color,
        );
        crate::spirv_to_wgsl::neutralize_fs_cbuf9_life_discard(&enhanced)
    } else {
        crate::spirv_to_wgsl::patch_fragment_wgsl(&fs_clamped)
    };
    let extra_tex_slots_needed = crate::spirv_to_wgsl::native_fs_extra_tex_slots_needed(&fs_wgsl);
    let tex_blend_uniform_needed =
        crate::spirv_to_wgsl::native_fs_tex_blend_uniform_needed(&fs_wgsl);
    let particle_alpha_uniform_needed =
        crate::spirv_to_wgsl::native_fs_particle_alpha_uniform_needed(&fs_wgsl);
    let fs_wgsl = if std::env::var("FX_DEBUG_SOLID_FS").is_ok() {
        crate::spirv_to_wgsl::debug_solid_fragment_wgsl(&fs_wgsl)
    } else {
        fs_wgsl
    };
    let fs_wgsl = crate::spirv_to_wgsl::inject_soft_particle_fs(&fs_wgsl);
    let fs_wgsl_depth_write = crate::spirv_to_wgsl::inject_opaque_core_alpha_test(
        &fs_wgsl,
        crate::spirv_to_wgsl::OPAQUE_CORE_DEPTH_ALPHA_TEST,
    );
    let soft_particle_needed = crate::spirv_to_wgsl::native_fs_soft_particle_needed(&fs_wgsl);
    let cbuf_slot_usage =
        crate::nvn_chain::cbuf_slot_usage_from_shaders(vs_spirv, fs_spirv, &vs_wgsl, &fs_wgsl);
    PreparedBnshWgsl {
        vs_wgsl,
        fs_wgsl,
        fs_wgsl_depth_write,
        uses_native_fs_fragment: uses_native_fs,
        cbuf_slot_usage,
        extra_tex_slots_needed,
        tex_blend_uniform_needed,
        particle_alpha_uniform_needed,
        soft_particle_needed,
    }
}

/// Shared vertex layout for all BNSH particle quads (13 × vec4<f32> at stride 208).
pub fn bnsh_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    static LAYOUT: std::sync::OnceLock<wgpu::VertexBufferLayout<'static>> =
        std::sync::OnceLock::new();
    LAYOUT.get_or_init(|| {
        let vertex_attributes: &'static [wgpu::VertexAttribute] = Box::leak(Box::new([
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 64, shader_location: 4 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 80, shader_location: 5 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 96, shader_location: 6 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 112, shader_location: 7 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 128, shader_location: 8 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 144, shader_location: 9 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 160, shader_location: 10 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 176, shader_location: 11 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 192, shader_location: 12 },
        ]));
        wgpu::VertexBufferLayout {
            array_stride: 208,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: vertex_attributes,
        }
    }).clone()
}

pub fn blend_state_for(blend: BlendType) -> wgpu::BlendState {
    match blend {
        BlendType::Add => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
            // Additive light must not occlude what's behind it: the HDR composite blits
            // the layer over the scene with (One, OneMinusSrcAlpha), and accumulating
            // alpha (One,One) drove layer alpha to 1 in hot regions — fire ERASED the
            // model/stage behind it instead of adding over them. Keep dst alpha.
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        },
        BlendType::Sub => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::ReverseSubtract },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::ReverseSubtract },
        },
        BlendType::Screen => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrc, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        },
        BlendType::Multiply => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::Src, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add },
        },
        BlendType::Normal => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        },
        BlendType::Unknown(raw) => {
            // Fall back to Normal, but surface the raw NVN blend value so unmapped modes can be
            // identified and added above (instead of silently rendering as Normal).
            eprintln!("[blend] unmapped NVN blend type {raw:?}; falling back to Normal");
            wgpu::BlendState {
                color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
                alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            }
        }
    }
}

/// Load WGSL shader modules from a decoded BNSH pair.
pub fn load_bnsh_shader_modules(
    device: &wgpu::Device,
    pair: &EffectShaderPair,
    label_tag: &str,
    native_color_hint: crate::shader_registry::NativeColorInput,
    registry_vs_profile: crate::shader_registry::ShaderVsProfile,
) -> BnshLoadedModules {
    let vs_info = pair.vertex.as_ref().expect("BNSH vertex shader required");
    let fs_info = pair.fragment.as_ref().expect("BNSH fragment shader required");

    eprintln!(
        "[ParticleRenderer] Loading BNSH pair '{}': vs={} bytes entry='{}', fs={} bytes entry='{}'",
        label_tag, vs_info.spirv.len(), vs_info.entry_point, fs_info.spirv.len(), fs_info.entry_point
    );

    let mut vs_w = crate::spirv_to_wgsl::bytes_to_words(&vs_info.spirv)
        .expect("Failed to parse vertex SPIR-V bytes");
    let mut fs_w = crate::spirv_to_wgsl::bytes_to_words(&fs_info.spirv)
        .expect("Failed to parse fragment SPIR-V bytes");

    let vs_patches = crate::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
    let fs_patches = crate::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
    if !vs_patches.is_empty() || !fs_patches.is_empty() {
        eprintln!("[ParticleRenderer] NVN patches ({}): VS[{}] FS[{}]",
            label_tag, vs_patches.join(", "), fs_patches.join(", "));
    }

    let remapped = crate::spirv_patch::nvn_remap_vertex_input_locations(&mut vs_w);
    if remapped > 0 {
        eprintln!("[ParticleRenderer] Remapped {} vertex input locations ({})", remapped, label_tag);
    }

    let to_bytes = |w: &[u32]| -> Vec<u8> {
        w.iter().flat_map(|&x| x.to_le_bytes()).collect()
    };
    let (ref vs_wgsl, vs_descs) = crate::spirv_to_wgsl::spirv_to_wgsl(
        &to_bytes(&vs_w), naga::ShaderStage::Vertex, &format!("particle_bnsh_vs_{label_tag}"),
    ).expect("Failed to convert vertex SPIR-V to WGSL");
    let (ref fs_wgsl, fs_descs) = crate::spirv_to_wgsl::spirv_to_wgsl(
        &to_bytes(&fs_w), naga::ShaderStage::Fragment, &format!("particle_bnsh_fs_{label_tag}"),
    ).expect("Failed to convert fragment SPIR-V to WGSL");

    let vs_hint = {
        let vs_prefixed = crate::spirv_to_wgsl::wire_vertex_simulation_varyings(vs_wgsl);
        let wired_for_hint =
            crate::spirv_to_wgsl::wire_billboard_vertex_inputs(&vs_prefixed);
        let mut hint = registry_vs_profile;
        if let Some(reflection) = vs_info.reflection.as_ref() {
            hint = hint.merge(crate::shader_registry::vs_profile_from_reflection(reflection));
        }
        hint = hint.merge(crate::spirv_to_wgsl::classify_vs_profile(&wired_for_hint));
        if hint == crate::shader_registry::ShaderVsProfile::Unknown {
            None
        } else {
            Some(hint)
        }
    };
    if std::env::var("FX_VS_BRANCH_DEBUG").is_ok() {
        eprintln!(
            "[VS-BRANCH] pair '{}': registry_profile={:?} reflection={} final_hint={:?}",
            label_tag,
            registry_vs_profile,
            vs_info.reflection.is_some(),
            vs_hint
        );
    }
    let prepared = prepare_bnsh_wgsl(
        vs_wgsl,
        fs_wgsl,
        vs_hint,
        Some(&to_bytes(&vs_w)),
        Some(&to_bytes(&fs_w)),
        native_color_hint,
    );
    let mut vs_wgsl = prepared.vs_wgsl;
    let mut fs_wgsl = prepared.fs_wgsl;
    let mut fs_wgsl_depth_write = prepared.fs_wgsl_depth_write;
    crate::scratch_dirs::write_workshop_wgsl_dump(
        &format!("hitbox_prepared_vs_{label_tag}.wgsl"),
        &vs_wgsl,
    );
    crate::scratch_dirs::write_workshop_wgsl_dump(
        &format!("hitbox_prepared_fs_{label_tag}.wgsl"),
        &fs_wgsl,
    );
    if prepared.uses_native_fs_fragment {
        fs_wgsl = crate::spirv_to_wgsl::strip_fs_wgsl_conflicting_with_vs(
            &fs_wgsl,
            &vs_descs,
            &fs_descs,
        );
        fs_wgsl_depth_write = crate::spirv_to_wgsl::strip_fs_wgsl_conflicting_with_vs(
            &fs_wgsl_depth_write,
            &vs_descs,
            &fs_descs,
        );
        crate::spirv_to_wgsl::validate_wgsl_shader(
            &fs_wgsl,
            &format!("particle_bnsh_fs_{label_tag}"),
        )
        .unwrap_or_else(|e| {
            let dump = crate::scratch_dirs::workshop_tmp_path(&format!(
                "hitbox_stripped_fs_{label_tag}.wgsl"
            ));
            crate::scratch_dirs::write_workshop_wgsl_dump(
                &format!("hitbox_stripped_fs_{label_tag}.wgsl"),
                &fs_wgsl,
            );
            eprintln!("[BNSH] Wrote stripped FS WGSL to {} for debugging", dump.display());
            panic!("BNSH fragment WGSL failed validation after native FS strip: {e}");
        });
        crate::spirv_to_wgsl::validate_wgsl_shader(
            &fs_wgsl_depth_write,
            &format!("particle_bnsh_fs_depth_write_{label_tag}"),
        )
        .unwrap_or_else(|e| {
            let dump = crate::scratch_dirs::workshop_tmp_path(&format!(
                "hitbox_stripped_fs_depth_{label_tag}.wgsl"
            ));
            crate::scratch_dirs::write_workshop_wgsl_dump(
                &format!("hitbox_stripped_fs_depth_{label_tag}.wgsl"),
                &fs_wgsl_depth_write,
            );
            eprintln!(
                "[BNSH] Wrote stripped depth FS WGSL to {} for debugging",
                dump.display()
            );
            panic!(
                "BNSH depth-write fragment WGSL failed validation after native FS strip: {e}"
            );
        });
    }
    let (fs_wgsl, fs_wgsl_depth_write, fs_descs) =
        crate::spirv_to_wgsl::remap_fs_storage_bindings_for_vs_pair(
            &fs_wgsl,
            &fs_wgsl_depth_write,
            &vs_descs,
            &fs_descs,
        );
    let extra_tex_slots_needed = prepared.extra_tex_slots_needed;
    let tex_blend_uniform_needed = prepared.tex_blend_uniform_needed;
    let particle_alpha_uniform_needed = prepared.particle_alpha_uniform_needed;
    let soft_particle_needed = prepared.soft_particle_needed;
    let cbuf_slot_usage = prepared.cbuf_slot_usage;
    if label_tag.contains("5740678a2aa5959f") {
        crate::scratch_dirs::write_workshop_wgsl_dump(
            &format!("hitbox_patched_vs_{label_tag}.wgsl"),
            &vs_wgsl,
        );
        crate::scratch_dirs::write_workshop_wgsl_dump(
            &format!("hitbox_patched_fs_{label_tag}.wgsl"),
            &fs_wgsl,
        );
    }

    let vs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("particle_bnsh_vs_{label_tag}")),
        source: wgpu::ShaderSource::Wgsl(vs_wgsl.clone().into()),
    });
    let fs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("particle_bnsh_fs_{label_tag}")),
        source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
    });
    let fs_mod_depth_write = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("particle_bnsh_fs_depth_write_{label_tag}")),
        source: wgpu::ShaderSource::Wgsl(fs_wgsl_depth_write.into()),
    });

    let descriptors =
        crate::spirv_to_wgsl::merge_stage_pipeline_descriptors(&vs_descs, &fs_descs);

    BnshLoadedModules {
        vs_module: vs_mod,
        fs_module: fs_mod,
        fs_module_depth_write: fs_mod_depth_write,
        vs_entry: vs_info.entry_point.clone(),
        fs_entry: fs_info.entry_point.clone(),
        descriptors,
        cbuf_slot_usage,
        extra_tex_slots_needed,
        tex_blend_uniform_needed,
        particle_alpha_uniform_needed,
        soft_particle_needed,
        vs_reflection: vs_info.reflection.clone(),
        fs_reflection: fs_info.reflection.clone(),
    }
}

impl BnshPipelineState {
    pub fn new(
        device: &wgpu::Device,
        modules: BnshLoadedModules,
        tex_bg_layout: &wgpu::BindGroupLayout,
        extra_tex345_bg_layout: Option<&wgpu::BindGroupLayout>,
        group2_placeholder_bg_layout: &wgpu::BindGroupLayout,
        soft_particle_bg_layout: Option<&wgpu::BindGroupLayout>,
        surface_format: wgpu::TextureFormat,
        label_tag: &str,
    ) -> Self {
        use crate::spirv_to_wgsl::{BindingClass, DescriptorInfo};

        fn push_native_bgl(
            device: &wgpu::Device,
            bind_group_layouts: &mut Vec<wgpu::BindGroupLayout>,
            label: &str,
            entries: &[&DescriptorInfo],
        ) {
            let mut bgl_entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
            for d in entries {
                let (visibility, binding_ty) = match d.class {
                    BindingClass::Texture => (
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                    ),
                    BindingClass::Sampler => (
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    ),
                    BindingClass::Storage => (
                        wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    BindingClass::Uniform => (
                        wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                };
                bgl_entries.push(wgpu::BindGroupLayoutEntry {
                    binding: d.binding,
                    visibility,
                    ty: binding_ty,
                    count: None,
                });
            }
            if bgl_entries.is_empty() {
                return;
            }
            bind_group_layouts.push(device.create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some(label),
                    entries: &bgl_entries,
                },
            ));
        }

        let mut bind_group_layouts: Vec<wgpu::BindGroupLayout> = Vec::new();
        // `@group(1..3)` are editor-injected; keep decoded set 0 intact (cbufs + any native set-0
        // textures spirv-cross did not strip). Drop FS texture/sampler descriptors on set >= 1 so
        // `tex_bg_layout` stays at pipeline bind index 1.
        let set0_entries: Vec<&DescriptorInfo> = modules
            .descriptors
            .iter()
            .filter(|d| d.set == 0)
            .collect();
        push_native_bgl(
            device,
            &mut bind_group_layouts,
            &format!("bnsh_bgl_{label_tag}_0"),
            &set0_entries,
        );

        let mut bgl_all = Vec::with_capacity(bind_group_layouts.len() + 2);
        bgl_all.extend(bind_group_layouts.iter().map(|b| Some(b)));
        // @group(1) emitter texture: patched FS and enhance_native_fragment_wgsl both sample it.
        bgl_all.push(Some(tex_bg_layout));
        let needs_group2 = modules.extra_tex_slots_needed.iter().any(|&b| b)
            || modules.tex_blend_uniform_needed
            || modules.particle_alpha_uniform_needed;
        if needs_group2 {
            bgl_all.push(extra_tex345_bg_layout);
        } else if modules.soft_particle_needed {
            bgl_all.push(Some(group2_placeholder_bg_layout));
        }
        if modules.soft_particle_needed {
            bgl_all.push(soft_particle_bg_layout);
        }
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("bnsh_pipeline_layout_{label_tag}")),
            bind_group_layouts: &bgl_all,
            immediate_size: 0,
        });

        let vertex_layout = bnsh_vertex_layout();
        let mut pipeline_cache: HashMap<BlendType, wgpu::RenderPipeline> = HashMap::new();
        let normal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("bnsh_particle_normal_{label_tag}")),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &modules.vs_module,
                entry_point: Some(&modules.vs_entry),
                buffers: &[vertex_layout.clone()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &modules.fs_module,
                entry_point: Some(&modules.fs_entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(blend_state_for(BlendType::Normal)),
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
            multiview_mask: None,
            cache: None,
        });
        pipeline_cache.insert(BlendType::Normal, normal_pipeline);

        eprintln!(
            "[ParticleRenderer] BNSH pipeline '{}' ready: {} bind group layouts, {} descriptors",
            label_tag, bind_group_layouts.len(), modules.descriptors.len()
        );

        Self {
            vs_module: modules.vs_module,
            fs_module: modules.fs_module,
            fs_module_depth_write: modules.fs_module_depth_write,
            vs_entry: modules.vs_entry,
            fs_entry: modules.fs_entry,
            descriptors: modules.descriptors,
            bind_group_layouts,
            pipeline_layout,
            pipeline_cache,
            pipeline_cache_depth: HashMap::new(),
            pipeline_cache_depth_write: HashMap::new(),
            storage_bufs: HashMap::new(),
            bind_group_cache: HashMap::new(),
            vertex_layout,
            cbuf_slot_usage: modules.cbuf_slot_usage,
            extra_tex_slots_needed: modules.extra_tex_slots_needed,
            tex_blend_uniform_needed: modules.tex_blend_uniform_needed,
            particle_alpha_uniform_needed: modules.particle_alpha_uniform_needed,
            soft_particle_needed: modules.soft_particle_needed,
            vs_reflection: modules.vs_reflection,
            fs_reflection: modules.fs_reflection,
        }
    }

    /// Get or lazily create a render pipeline for the given blend mode.
    pub fn pipeline_for_blend(
        &mut self,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        blend: BlendType,
        label_tag: &str,
        use_depth: bool,
        depth_write: bool,
    ) -> &wgpu::RenderPipeline {
        let cache = if use_depth {
            if depth_write {
                &mut self.pipeline_cache_depth_write
            } else {
                &mut self.pipeline_cache_depth
            }
        } else {
            &mut self.pipeline_cache
        };
        if !cache.contains_key(&blend) {
            let blend_state = blend_state_for(blend);
            let depth_stencil = if use_depth {
                Some(if depth_write {
                    particle_depth_stencil_state_write()
                } else {
                    particle_depth_stencil_state()
                })
            } else {
                None
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!(
                    "bnsh_blend_{label_tag}_{blend:?}{}{}",
                    if use_depth { "_depth" } else { "" },
                    if depth_write { "_write" } else { "" }
                )),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.vs_module,
                    entry_point: Some(&self.vs_entry),
                    buffers: &[self.vertex_layout.clone()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: if depth_write {
                        &self.fs_module_depth_write
                    } else {
                        &self.fs_module
                    },
                    entry_point: Some(&self.fs_entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(blend_state),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            cache.insert(blend, pipeline);
        }
        cache.get(&blend).unwrap()
    }
}

/// Metadata about loaded BNSH shaders for rendering
#[derive(Debug, Clone)]
pub struct BnshShaderSet {
    /// All decoded shader pairs keyed by PTCL registry hash (embedded Shader.bnsh).
    pub all_shaders: HashMap<ShaderKey, EffectShaderPair>,
    pub default_key: ShaderKey,
    /// shader_index → key mapping from effect ShaderReferences.
    pub library_indices: HashMap<i32, ShaderKey>,
    /// Per-shader native colour input hints aggregated from emitters.
    pub native_color_by_key: HashMap<ShaderKey, crate::shader_registry::NativeColorInput>,
    /// Per-shader VS profile hints from PTCL registry metadata.
    pub vs_profile_by_key: HashMap<ShaderKey, crate::shader_registry::ShaderVsProfile>,
    pub material_bindings: MaterialTextureBindings,
    #[allow(dead_code)]
    pub stats: ShaderStats,
    pub source_name: String,
}

impl BnshShaderSet {
    pub fn from_ptcl_file(ptcl: &PtclFile, source_name: &str) -> Result<Self> {
        eprintln!(
            "[BNSH Shader] Loading shaders from {} ({} unique BNSH in registry)",
            source_name,
            ptcl.shader_registry.len()
        );

        let effect_vs = decode_legacy_stage_pair(ptcl);
        let mut all_shaders = decode_all_effect_shaders(ptcl)?;
        let mut default_key = ptcl.shader_registry.default_key();
        if default_key == 0 {
            default_key = all_shaders
                .iter()
                .find(|(_, p)| p.vertex.is_some() && p.fragment.is_some())
                .map(|(&k, _)| k)
                .unwrap_or(0);
        }
        let library_indices = ptcl.shader_registry.library_indices().clone();

        if default_key != 0 {
            if let Some(pair) = all_shaders.get(&default_key) {
                if pair.vertex.is_none() || pair.fragment.is_none() {
                    if effect_vs.vertex.is_some() && effect_vs.fragment.is_some() {
                        all_shaders.insert(default_key, effect_vs.clone());
                    }
                }
            } else if effect_vs.vertex.is_some() && effect_vs.fragment.is_some() {
                all_shaders.insert(default_key, effect_vs.clone());
            }
        }

        let shader_pair = all_shaders
            .get(&default_key)
            .cloned()
            .filter(|p| p.vertex.is_some() && p.fragment.is_some())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no complete default shader {default_key:#x} in {}",
                    source_name
                )
            })?;

        let stats = crate::bnsh_shader_integration::get_shader_stats(&shader_pair);
        let fs_samplers = crate::bnsh_shader_integration::fragment_sampler_count(&shader_pair);

        eprintln!(
            "[BNSH Shader] Loaded {} shader variant(s), default={:#x}, {} FS samplers (reflection)",
            all_shaders.len(),
            default_key,
            fs_samplers
        );

        Ok(BnshShaderSet {
            all_shaders,
            default_key,
            library_indices,
            native_color_by_key: ptcl.shader_registry.native_color_inputs().clone(),
            vs_profile_by_key: ptcl.shader_registry.vs_profiles().clone(),
            material_bindings: MaterialTextureBindings::default(),
            stats,
            source_name: source_name.to_string(),
        })
    }

    pub fn native_color_for_key(&self, key: ShaderKey) -> crate::shader_registry::NativeColorInput {
        self.native_color_by_key
            .get(&key)
            .copied()
            .unwrap_or(crate::shader_registry::NativeColorInput::Auto)
    }

    pub fn vs_profile_for_key(&self, key: ShaderKey) -> crate::shader_registry::ShaderVsProfile {
        self.vs_profile_by_key
            .get(&key)
            .copied()
            .unwrap_or_default()
    }

    pub fn default_pair(&self) -> &EffectShaderPair {
        self.all_shaders.get(&self.default_key).unwrap_or_else(|| {
            panic!(
                "default shader {:#x} missing in {}",
                self.default_key, self.source_name
            )
        })
    }

    /// Per-emitter shader pair from the registry (effect VS + per-key FS when blob is FS-only).
    pub fn pair_for_emitter(&self, emitter: &EmitterDef) -> &EffectShaderPair {
        let key = self.resolve_key(emitter);
        self.all_shaders.get(&key).unwrap_or_else(|| {
            panic!(
                "emitter shader {key:#x} not in registry for {}",
                self.source_name
            )
        })
    }

    /// Registry shader key for GPU pipeline cache lookup (one BNSH pair per registry entry).
    pub fn pipeline_key_for_emitter(&self, emitter: &EmitterDef) -> ShaderKey {
        self.resolve_key(emitter)
    }

    pub fn resolve_key(&self, emitter: &EmitterDef) -> ShaderKey {
        if emitter.shader_key != 0 {
            return emitter.shader_key;
        }
        if emitter.shader_index >= 0 {
            if let Some(&key) = self.library_indices.get(&emitter.shader_index) {
                return key;
            }
        }
        self.default_key
    }

    pub fn is_complete(&self) -> bool {
        self.default_pair().vertex.is_some() && self.default_pair().fragment.is_some()
    }

    pub fn summary(&self) -> String {
        if self.all_shaders.len() > 1 {
            format!(
                "{} shader variants, default: {}",
                self.all_shaders.len(),
                self.shader_pair_summary()
            )
        } else {
            self.shader_pair_summary()
        }
    }

    fn shader_pair_summary(&self) -> String {
        let pair = self.default_pair();
        let mut parts = Vec::new();
        if let Some(vs) = &pair.vertex {
            parts.push(format!("vertex({} SPIR-V words)", vs.spirv.len() / 4));
        }
        if let Some(fs) = &pair.fragment {
            parts.push(format!("fragment({} SPIR-V words)", fs.spirv.len() / 4));
        }
        if parts.is_empty() {
            "no shaders".to_string()
        } else {
            parts.join(" + ")
        }
    }

    pub fn apply_material_bindings(&self) -> HashMap<String, u32> {
        HashMap::new()
    }
}

pub fn load_shaders_from_files(effect_files: &[(&str, &PtclFile)]) -> Vec<(String, Result<BnshShaderSet>)> {
    effect_files
        .iter()
        .map(|(name, ptcl)| (name.to_string(), BnshShaderSet::from_ptcl_file(ptcl, name)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bnsh_shader_integration::EffectShaderPair;

    #[test]
    fn test_shader_summary_generation() {
        let pair = crate::bnsh_shader_integration::EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };

        let set = BnshShaderSet {
            all_shaders: HashMap::from([(0u64, pair.clone())]),
            default_key: 0,
            library_indices: HashMap::new(),
            native_color_by_key: HashMap::new(),
            vs_profile_by_key: HashMap::new(),
            material_bindings: crate::bnsh_shader_integration::MaterialTextureBindings::default(),
            stats: crate::bnsh_shader_integration::ShaderStats::default(),
            source_name: "test.eff".to_string(),
        };

        assert_eq!(set.summary(), "no shaders");
        assert!(!set.is_complete());
    }
}
