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

/// GPU pipeline state for one BNSH shader variant (keyed by ShaderKey).
pub struct BnshPipelineState {
    pub vs_module: wgpu::ShaderModule,
    pub fs_module: wgpu::ShaderModule,
    pub vs_entry: String,
    pub fs_entry: String,
    pub descriptors: Vec<crate::spirv_to_wgsl::DescriptorInfo>,
    pub bind_group_layouts: Vec<wgpu::BindGroupLayout>,
    pub pipeline_layout: wgpu::PipelineLayout,
    pub pipeline_cache: HashMap<BlendType, wgpu::RenderPipeline>,
    pub storage_bufs: HashMap<u32, wgpu::Buffer>,
    pub bind_group_cache: HashMap<(usize, usize), wgpu::BindGroup>,
    pub vertex_layout: wgpu::VertexBufferLayout<'static>,
    pub cbuf_slot_usage: HashMap<String, HashSet<u32>>,
}

/// Loaded WGSL modules + descriptor reflection for one shader pair.
pub struct BnshLoadedModules {
    pub vs_module: wgpu::ShaderModule,
    pub fs_module: wgpu::ShaderModule,
    pub vs_entry: String,
    pub fs_entry: String,
    pub descriptors: Vec<crate::spirv_to_wgsl::DescriptorInfo>,
    pub cbuf_slot_usage: HashMap<String, HashSet<u32>>,
}

/// Shared vertex layout for all BNSH particle quads (12 × vec4<f32> at stride 192).
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
        ]));
        wgpu::VertexBufferLayout {
            array_stride: 192,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: vertex_attributes,
        }
    }).clone()
}

pub fn blend_state_for(blend: BlendType) -> wgpu::BlendState {
    match blend {
        BlendType::Add => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
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
        BlendType::Unknown(_) => wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        },
    }
}

/// Load WGSL shader modules from a decoded BNSH pair.
pub fn load_bnsh_shader_modules(
    device: &wgpu::Device,
    pair: &EffectShaderPair,
    label_tag: &str,
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

    let vs_wgsl = crate::spirv_to_wgsl::patch_vertex_wgsl(vs_wgsl, fs_wgsl);
    let fs_wgsl = if crate::fx_native_fs_enabled() {
        fs_wgsl.clone()
    } else {
        crate::spirv_to_wgsl::patch_fragment_wgsl(fs_wgsl)
    };
    // NVN deferred shaders declare many MRT outputs; WebGPU rejects locations >= 8 and we
    // composite only @location(0). Trim the FragmentOutput struct before pipeline creation.
    let fs_wgsl = crate::spirv_to_wgsl::clamp_fragment_output_locations(
        &fs_wgsl,
        crate::spirv_to_wgsl::PARTICLE_COMPOSITE_MRT_LOCATIONS,
    );
    let fs_wgsl = if crate::fx_native_fs_enabled() {
        crate::spirv_to_wgsl::enhance_native_fragment_wgsl(&fs_wgsl)
    } else {
        fs_wgsl
    };
    let fs_wgsl = if std::env::var("FX_DEBUG_SOLID_FS").is_ok() {
        crate::spirv_to_wgsl::debug_solid_fragment_wgsl(&fs_wgsl)
    } else {
        fs_wgsl
    };

    let cbuf_slot_usage = crate::nvn_chain::cbuf_slot_usage_from_wgsl(&vs_wgsl, &fs_wgsl);
    if label_tag.contains("5740678a2aa5959f") {
        let _ = std::fs::write(
            format!("/tmp/hitbox_patched_vs_{label_tag}.wgsl"),
            &vs_wgsl,
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

    let mut descriptors = vs_descs.clone();
    descriptors.extend(fs_descs.clone());
    descriptors.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));
    descriptors.dedup_by(|a, b| a.set == b.set && a.binding == b.binding);

    BnshLoadedModules {
        vs_module: vs_mod,
        fs_module: fs_mod,
        vs_entry: vs_info.entry_point.clone(),
        fs_entry: fs_info.entry_point.clone(),
        descriptors,
        cbuf_slot_usage,
    }
}

impl BnshPipelineState {
    pub fn new(
        device: &wgpu::Device,
        modules: BnshLoadedModules,
        tex_bg_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        label_tag: &str,
    ) -> Self {
        let max_set = modules.descriptors.iter().map(|d| d.set).max().unwrap_or(0);
        let per_set: Vec<Vec<&crate::spirv_to_wgsl::DescriptorInfo>> = (0..=max_set)
            .map(|s| modules.descriptors.iter().filter(|d| d.set == s).collect())
            .collect();

        let mut bind_group_layouts: Vec<wgpu::BindGroupLayout> = Vec::new();
        for entries in &per_set {
            let mut bgl_entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
            for d in entries {
                let (visibility, binding_ty) = match d.class {
                    crate::spirv_to_wgsl::BindingClass::Texture => (
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                    ),
                    crate::spirv_to_wgsl::BindingClass::Sampler => (
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    ),
                    crate::spirv_to_wgsl::BindingClass::Storage => (
                        wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    crate::spirv_to_wgsl::BindingClass::Uniform => (
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
            let label = format!("bnsh_bgl_{label_tag}_{}", bind_group_layouts.len());
            bind_group_layouts.push(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(&label),
                entries: &bgl_entries,
            }));
        }

        let mut bgl_all = Vec::with_capacity(bind_group_layouts.len() + 1);
        bgl_all.extend(bind_group_layouts.iter().map(|b| Some(b)));
        // @group(1) emitter texture: patched FS and enhance_native_fragment_wgsl both sample it.
        bgl_all.push(Some(tex_bg_layout));
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
            vs_entry: modules.vs_entry,
            fs_entry: modules.fs_entry,
            descriptors: modules.descriptors,
            bind_group_layouts,
            pipeline_layout,
            pipeline_cache,
            storage_bufs: HashMap::new(),
            bind_group_cache: HashMap::new(),
            vertex_layout,
            cbuf_slot_usage: modules.cbuf_slot_usage,
        }
    }

    /// Get or lazily create a render pipeline for the given blend mode.
    pub fn pipeline_for_blend(
        &mut self,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        blend: BlendType,
        label_tag: &str,
    ) -> &wgpu::RenderPipeline {
        if !self.pipeline_cache.contains_key(&blend) {
            let blend_state = blend_state_for(blend);
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("bnsh_blend_{label_tag}_{blend:?}")),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.vs_module,
                    entry_point: Some(&self.vs_entry),
                    buffers: &[self.vertex_layout.clone()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.fs_module,
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
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            self.pipeline_cache.insert(blend, pipeline);
        }
        self.pipeline_cache.get(&blend).unwrap()
    }
}

/// Metadata about loaded BNSH shaders for rendering
#[derive(Debug, Clone)]
pub struct BnshShaderSet {
    /// All decoded shader pairs keyed by content hash (from embedded Shader.bnsh).
    pub all_shaders: HashMap<ShaderKey, EffectShaderPair>,
    pub default_key: ShaderKey,
    /// shader_index → key mapping from effect ShaderReferences.
    pub library_indices: HashMap<i32, ShaderKey>,
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
        let default_key = ptcl.shader_registry.default_key();
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
        let material_bindings = MaterialTextureBindings::from_ptcl_file(ptcl);

        eprintln!(
            "[BNSH Shader] Loaded {} shader variant(s), default={:#x}, {} samplers",
            all_shaders.len(),
            default_key,
            stats.total_samplers()
        );

        Ok(BnshShaderSet {
            all_shaders,
            default_key,
            library_indices,
            material_bindings,
            stats,
            source_name: source_name.to_string(),
        })
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

    #[allow(dead_code)]
    pub fn apply_material_bindings(&self) -> HashMap<String, u32> {
        let pair = self.default_pair();
        let mut all_bindings = HashMap::new();
        if let Some(fs) = &pair.fragment {
            if let Some(ref refl) = fs.reflection {
                let resolved = self.material_bindings.resolve_with_reflection(refl);
                all_bindings.extend(resolved);
            }
        }
        if let Some(vs) = &pair.vertex {
            if let Some(ref refl) = vs.reflection {
                let resolved = self.material_bindings.resolve_with_reflection(refl);
                all_bindings.extend(resolved);
            }
        }
        all_bindings
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
            material_bindings: crate::bnsh_shader_integration::MaterialTextureBindings::default(),
            stats: crate::bnsh_shader_integration::ShaderStats::default(),
            source_name: "test.eff".to_string(),
        };

        assert_eq!(set.summary(), "no shaders");
        assert!(!set.is_complete());
    }
}
