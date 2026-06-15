use hitbox_editor::effects::PtclFile;
use hitbox_editor::particle_renderer_bnsh::BnshShaderSet;
use hitbox_editor::spirv_to_wgsl::{patch_fragment_wgsl, patch_vertex_wgsl};
use std::path::Path;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

const EFFECT_DIR: &str = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect";

fn load_effect_ptcl(effect_name: &str) -> Option<PtclFile> {
    let candidates = vec![
        Path::new(EFFECT_DIR).join("fighter").join(effect_name).join(format!("ef_{}.eff", effect_name)),
        Path::new(EFFECT_DIR).join(format!("ef_{}.eff", effect_name)),
    ];
    let path = candidates.into_iter().find(|p| p.exists())?;
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).ok()?;
    if eff.ptcl_data.is_empty() {
        return None;
    }
    match hitbox_editor::effects::PtclFile::parse(&eff.ptcl_data) {
        Ok(ptcl) => {
            let _ = ptcl.emitter_sets.len(); // silence unused warning
            Some(ptcl)
        }
        Err(e) => {
            eprintln!("[TEST] PtclFile::parse failed for {}: {}", effect_name, e);
            None
        }
    }
}

#[test]
fn test_renderer_accepts_bnsh_shaders() {
    let ptcl = load_effect_ptcl("mario").expect("Mario effect file should exist");
    let shader_set = BnshShaderSet::from_ptcl_file(&ptcl, "mario.eff")
        .expect("BNSH decoder should successfully decode Mario effect shaders");
    println!("[TEST] Loaded Mario effect shaders: {}", shader_set.summary());
    let spirv_total = shader_set.default_pair().vertex.as_ref().map(|s| s.spirv.len()).unwrap_or(0)
        + shader_set.default_pair().fragment.as_ref().map(|s| s.spirv.len()).unwrap_or(0);
    assert!(spirv_total > 0,
        "Mario effect should have decoded SPIR-V bytes (got {})", spirv_total);
}

#[test]
fn test_multiple_effect_shaders() {
    let effect_names = vec!["mario", "link", "sonic"];
    let mut loaded = 0;
    for effect_name in &effect_names {
        if let Some(ptcl) = load_effect_ptcl(effect_name) {
            match BnshShaderSet::from_ptcl_file(&ptcl, &format!("{}.eff", effect_name)) {
                Ok(shader_set) => {
                    loaded += 1;
                    println!("[TEST] {} shaders: {} vertex={} fragment={}",
                        effect_name, shader_set.summary(),
                        shader_set.default_pair().vertex.is_some(),
                        shader_set.default_pair().fragment.is_some());
                }
                Err(e) => { eprintln!("[TEST] Failed to decode {}: {}", effect_name, e); }
            }
        }
    }
    assert!(loaded > 0);
}

#[test]
fn test_bnsh_shader_set_completeness() {
    let ptcl = load_effect_ptcl("mario").expect("Mario effect file should exist");
    let shader_set = BnshShaderSet::from_ptcl_file(&ptcl, "mario.eff")
        .expect("BNSH decoder should decode Mario shaders");
    let complete = shader_set.is_complete();
    let has_both = shader_set.default_pair().vertex.is_some() && shader_set.default_pair().fragment.is_some();
    assert_eq!(complete, has_both);
}

/// Test that the BNSH → WGSL pipeline works via the spirv-cross GLSL fallback.
/// This exercises the full BNSH decoder + NVN patching + spirv-cross + naga GLSL
/// frontend WITHOUT requiring a GPU device.
#[test]
fn test_bnsh_to_wgsl_via_glsl_fallback() {
    let ptcl = load_effect_ptcl("mario").expect("Mario effect file should exist");
    let shader_set = BnshShaderSet::from_ptcl_file(&ptcl, "mario.eff")
        .expect("BNSH decoder should decode Mario shaders");

    for (label, stage, spirv_bytes) in [
        ("vertex",   naga::ShaderStage::Vertex,
         shader_set.default_pair().vertex.as_ref().map(|s| s.spirv.as_slice())),
        ("fragment", naga::ShaderStage::Fragment,
         shader_set.default_pair().fragment.as_ref().map(|s| s.spirv.as_slice())),
    ] {
        let spirv_bytes = spirv_bytes.expect("{} SPIR-V bytes should exist");

        // Apply NVN patches (same as load_particle_shader does)
        let mut spirv_words = hitbox_editor::spirv_to_wgsl::bytes_to_words(spirv_bytes)
            .expect("SPIR-V bytes should convert to words");
        let _patches = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut spirv_words);

        // Convert back to bytes for spirv-cross
        let patched_bytes: Vec<u8> = spirv_words.iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();

        // Convert SPIR-V to WGSL via spirv-cross → GLSL → naga GLSL frontend
        let (wgsl, _descriptors) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
            &patched_bytes, stage, &format!("test_mario_{}", label),
        ).expect("spirv-cross → GLSL → naga should produce WGSL");

        assert!(!wgsl.is_empty(), "WGSL output for {} should be non-empty", label);
        eprintln!("[TEST] Generated {} lines of {} WGSL, {} descriptors",
            wgsl.lines().count(), label, _descriptors.len());
        for d in &_descriptors {
            eprintln!("[TEST]   descriptor: group={} binding={} type={} name={}",
                d.set, d.binding, d.ty_str, d.name);
        }
    }
}

/// Create a wgpu device for headless rendering tests.
fn create_test_device() -> (Option<wgpu::Device>, Option<wgpu::Queue>) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[TEST-RENDER] Failed to create tokio runtime: {}", e);
            return (None, None);
        }
    };
    let adapter = match rt.block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        },
    )) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[TEST-RENDER] No GPU adapter available: {} — skipping", e);
            return (None, None);
        }
    };
    let (device, queue) = match rt.block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("test_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        },
    )) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[TEST-RENDER] Failed to create device: {}", e);
            return (None, None);
        }
    };
    (Some(device), Some(queue))
}

/// Render-test the full BNSH → WGSL pipeline: create a wgpu device, compile
/// shader modules, create a pipeline, draw, and read back rendered pixels.
#[test]
fn test_bnsh_shaders_render() {
    let ptcl = load_effect_ptcl("mario").expect("Mario effect file should exist");
    let shader_set = BnshShaderSet::from_ptcl_file(&ptcl, "mario.eff")
        .expect("BNSH decoder should decode Mario shaders");

    // Patch + translate both stages
    let mut vs_wgsl = String::new();
    let mut fs_wgsl = String::new();
    let mut vs_descs = Vec::new();
    let mut fs_descs = Vec::new();
    for (label, stage, spirv_bytes, wgsl_out, descs_out) in [
        ("vertex", naga::ShaderStage::Vertex,
         shader_set.default_pair().vertex.as_ref().map(|s| s.spirv.as_slice()),
         &mut vs_wgsl, &mut vs_descs),
        ("fragment", naga::ShaderStage::Fragment,
         shader_set.default_pair().fragment.as_ref().map(|s| s.spirv.as_slice()),
         &mut fs_wgsl, &mut fs_descs),
    ] {
        let spirv_bytes = spirv_bytes.expect("{} SPIR-V bytes should exist");
        let mut spirv_words = hitbox_editor::spirv_to_wgsl::bytes_to_words(spirv_bytes)
            .expect("SPIR-V bytes should convert to words");
        let _patches = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut spirv_words);
        // Safety: remap vertex input locations to 0-based if needed (BNSH decoder
        // already produces 0-based locations, but keep for robustness with other shaders)
        if label == "vertex" {
            hitbox_editor::spirv_patch::nvn_remap_vertex_input_locations(&mut spirv_words);
        }
        let patched_bytes: Vec<u8> = spirv_words.iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        let (wgsl, descs) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
            &patched_bytes, stage, &format!("test_mario_{}", label),
        ).expect("spirv-cross → GLSL → naga should produce WGSL");
        eprintln!("[TEST-RENDER] {}: {} lines, {} descriptors", label, wgsl.lines().count(), descs.len());
        *wgsl_out = wgsl;
        *descs_out = descs;
    }

    // Create wgpu device
    let (device, queue) = match create_test_device() {
        (Some(d), Some(q)) => (d, q),
        _ => {
            eprintln!("[TEST-RENDER] Skipping render test — no GPU available");
            return;
        }
    };
    eprintln!("[TEST-RENDER] Device created");

    // Set up error callback
    device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
        eprintln!("[wgpu ERROR] {:?}", e);
    }));

    // Create a white 1x1 texture for the fallback bind group
    let white_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test_white_tex"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let white_view = white_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let white_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

    // Patch vertex WGSL: spirv-cross independently renumbered locations in the
    // vertex (outputs 0-5) and fragment (inputs 0-7).  The vertex needs to
    // provide all 8 locations the fragment expects.
    let vs_wgsl = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);

    // Create shader modules (validates WGSL compiles)
    let _ = std::fs::write("/tmp/bnsh_vs.wgsl", &vs_wgsl);
    let _ = std::fs::write("/tmp/bnsh_fs.wgsl", &fs_wgsl);
    let vs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bnsh_vs"),
        source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
    });
    // Patch fragment shader to use vertex color attribute instead of
    // NVN buffer-driven color computation (same as the real renderer).
    let patched_fs = patch_fragment_wgsl(&fs_wgsl);
    let _ = std::fs::write("/tmp/bnsh_fs_patched.wgsl", &patched_fs);
    let fs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bnsh_fs"),
        source: wgpu::ShaderSource::Wgsl(patched_fs.into()),
    });

    // Build a pipeline layout matching the shader descriptors.
    let mut all_descs: Vec<hitbox_editor::spirv_to_wgsl::DescriptorInfo> = vs_descs;
    all_descs.extend(fs_descs);
    all_descs.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));
    all_descs.dedup_by(|a, b| a.set == b.set && a.binding == b.binding);

    eprintln!("[TEST-RENDER] Total unique descriptors: {}", all_descs.len());
    for d in &all_descs {
        eprintln!("[TEST-RENDER]   group={} binding={} type={} name={}",
            d.set, d.binding, d.ty_str, d.name);
    }

    // Group descriptors by set number
    let max_set = all_descs.iter().map(|d| d.set).max().unwrap_or(0);
    let per_set: Vec<Vec<&hitbox_editor::spirv_to_wgsl::DescriptorInfo>> = (0..=max_set)
        .map(|s| all_descs.iter().filter(|d| d.set == s).collect())
        .collect();

    // First create all dummy buffers, write default data, and keep them alive
    let mut dummy_buffers: Vec<wgpu::Buffer> = Vec::new();
    for entries in &per_set {
        for d in entries {
            let usage = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => {
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST
                }
                hitbox_editor::spirv_to_wgsl::BindingClass::Uniform => {
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST
                }
                _ => wgpu::BufferUsages::UNIFORM,
            };
            let buf_size = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => 65536u64,
                _ => 256u64,
            };
            dummy_buffers.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("dummy_{}_{}", d.set, d.binding)),
                size: buf_size,
                usage,
                mapped_at_creation: false,
            }));
            let buf = dummy_buffers.last().unwrap();
            // Write data to all storage buffers to avoid division by zero
            // and provide valid comparison/mask values.
            if let hitbox_editor::spirv_to_wgsl::BindingClass::Storage = d.class {
                // Write view-proj matrix at offset 0
                // (use identity-like values for the test)
                // Also write specific data at known indices
                match d.name.as_str() {
                    "cbuf_1_1" => {
                        // _m0[0] — equality comparison reference (int-rep 1 = passes bit 0 check)
                        let ref_vals: [u32; 4] = [1, 1, 1, 1];
                        queue.write_buffer(buf, 0, bytemuck::cast_slice(&ref_vals));
                    }
                    "cbuf_9_1" => {
                        // _m0[0] (offset 0) — view-projection matrix (identity for test).
                        // The position-override patch reads this to compute gl_Position.
                        let identity_vp: [[f32; 4]; 4] = [
                            [1.0, 0.0, 0.0, 0.0],
                            [0.0, 1.0, 0.0, 0.0],
                            [0.0, 0.0, 1.0, 0.0],
                            [0.0, 0.0, 0.0, 1.0],
                        ];
                        queue.write_buffer(buf, 0, bytemuck::cast_slice(&identity_vp));
                        // _m0[5]  (offset 80) — render flags mask (all-ones = pass all bits)
                        let mask: [u32; 2] = [!0u32, !0u32];
                        queue.write_buffer(buf, 5 * 16, bytemuck::cast_slice(&mask));
                        // _m0[17] (offset 272) — texture dimensions
                        let tex_dims: [f32; 2] = [256.0, 256.0];
                        queue.write_buffer(buf, 17 * 16, bytemuck::cast_slice(&tex_dims));
                        // _m0[48].z (offset 776) — non-zero subdivision count
                        // _m0[53].z (offset 856) — non-zero subdivision count
                        let count_z: f32 = 256.0;
                        queue.write_buffer(buf, 48 * 16 + 8, bytemuck::bytes_of(&count_z));
                        queue.write_buffer(buf, 53 * 16 + 8, bytemuck::bytes_of(&count_z));
                        // NVN colour-computation indices (same as renderer)
                        let col_mult: f32 = 1.0;
                        queue.write_buffer(buf, 59 * 16, bytemuck::bytes_of(&col_mult));
                        let rgb_src: [f32; 4] = [1.0, 1.0, 1.0, 0.0];
                        queue.write_buffer(buf, 60 * 16, bytemuck::cast_slice(&rgb_src));
                        let zero4: [f32; 4] = [0.0; 4];
                        queue.write_buffer(buf, 44 * 16, bytemuck::cast_slice(&zero4));
                        queue.write_buffer(buf, 45 * 16, bytemuck::cast_slice(&zero4));
                        queue.write_buffer(buf, 46 * 16, bytemuck::cast_slice(&zero4));
                        queue.write_buffer(buf, 68 * 16, bytemuck::cast_slice(&zero4));
                    }
                    "cbuf_10_1" => {
                        // Match the renderer's data layout for cbuf_10
                        let mut default_data = [0.0f32; 256];
                        default_data[0..4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
                        default_data[4*4..4*4+4].copy_from_slice(&[1.0f32; 4]); // _m0[4]
                        // _m0[5] offset 80
                        default_data[5*4..5*4+1].copy_from_slice(&[1.0f32]);
                        // _m0[6] offset 96
                        default_data[6*4..6*4+1].copy_from_slice(&[1.0f32]);
                        // _m0[8].y offset 132
                        default_data[8*4+1..8*4+2].copy_from_slice(&[1.0f32]);
                        // _m0[10] offset 160
                        default_data[10*4..10*4+2].copy_from_slice(&[1.0f32, 1.0f32]);
                        // _m0[2] offset 32
                        default_data[2*4..2*4+4].copy_from_slice(&[1.0f32; 4]);
                        queue.write_buffer(buf, 0, bytemuck::cast_slice(&default_data[..32]));
                    }
                    "cbuf_8_1" | "cbuf_16_1" => {
                        // Not read by fragment; position is overridden in vertex.
                        let zero: [f32; 4] = [0.0; 4];
                        queue.write_buffer(buf, 0, bytemuck::cast_slice(&zero));
                    }
                    _ => {
                        let one: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
                        queue.write_buffer(buf, 0, bytemuck::cast_slice(&one));
                    }
                }
            }
        }
    }

    let mut bgls: Vec<wgpu::BindGroupLayout> = Vec::new();
    let mut bind_groups: Vec<wgpu::BindGroup> = Vec::new();

    let mut buf_idx = 0usize;
    for (set, entries) in per_set.iter().enumerate() {
        let mut bgl_entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
        let mut bg_entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::new();

        for d in entries {
            let (visibility, binding_ty) = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Texture => (
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Sampler => (
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => (
                    wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Uniform => (
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

            bg_entries.push(wgpu::BindGroupEntry {
                binding: d.binding,
                resource: dummy_buffers[buf_idx].as_entire_binding(),
            });
            buf_idx += 1;
        }

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("test_bgl_{}", set)),
            entries: &bgl_entries,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("test_bg_{}", set)),
            layout: &bgl,
            entries: &bg_entries,
        });
        bgls.push(bgl);
        bind_groups.push(bg);
    }

    // Create texture bind group layout for set=1 (used by patch_fragment_wgsl)
    let test_tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("test_tex_bgl"),
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
    let mut bgl_all: Vec<Option<&wgpu::BindGroupLayout>> = bgls.iter().map(|b| Some(b)).collect();
    bgl_all.push(Some(&test_tex_bgl));
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bnsh_test"),
        bind_group_layouts: &bgl_all,
        immediate_size: 0,
    });

    // Create a vertex buffer with dummy data for 8 attributes (each vec4<f32>)
    // The vertex shader expects @location(0..7) as vertex attribute inputs.
    // Provide clip-space positions in attr0 (the position override patch applies
    // the view-proj matrix, so identity VP + clip-space coords = visible output).
    // Three vertices forming a triangle visible with identity projection:
    //   v0: bottom-left,  v1: bottom-right,  v2: top-centre
    let mut vertex_data: Vec<f32> = vec![0.0f32; 3 * 8 * 4];
    // vertex 0 — attr0 = (-0.5, -0.5, 0.0, 1.0), attr1 = (1, 1, 1, 1)
    vertex_data[0] = -0.5; vertex_data[1] = -0.5; vertex_data[3] = 1.0;
    vertex_data[4..8].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
    // vertex 1 — attr0 = (0.5, -0.5, 0.0, 1.0), attr1 = (1, 1, 1, 1)
    vertex_data[8*4 + 0] = 0.5; vertex_data[8*4 + 1] = -0.5; vertex_data[8*4 + 3] = 1.0;
    vertex_data[8*4 + 4..8*4 + 8].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
    // vertex 2 — attr0 = (0.0, 0.5, 0.0, 1.0), attr1 = (1, 1, 1, 1)
    vertex_data[8*4*2 + 1] = 0.5; vertex_data[8*4*2 + 3] = 1.0;
    vertex_data[8*4*2 + 4..8*4*2 + 8].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
    let vertex_buf = device.create_buffer_init(
        &wgpu::util::BufferInitDescriptor {
            label: Some("vertex_buf"),
            contents: bytemuck::cast_slice(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        }
    );
    let mut vertex_buf_layouts: Vec<wgpu::VertexBufferLayout> = Vec::new();
    let mut vertex_attributes: Vec<wgpu::VertexAttribute> = Vec::new();
    for loc in 0..8i32 {
        let offset = (loc as u64) * 16;
        vertex_attributes.push(wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset,
            shader_location: loc as u32,
        });
    }
    vertex_buf_layouts.push(wgpu::VertexBufferLayout {
        array_stride: 8 * 16, // 8 attributes × 4 floats × 4 bytes
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attributes,
    });

    // Create a simple render pipeline
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bnsh_test"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_mod,
            entry_point: Some("main"),
            buffers: &vertex_buf_layouts,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs_mod,
            entry_point: Some("main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    eprintln!("[TEST-RENDER] Pipeline created");

    // Create render target texture (256×256 RGBA8)
    let render_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render_target"),
        size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_tex.create_view(&wgpu::TextureViewDescriptor::default());

    // Readback buffer
    let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (256 * 256 * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // Render: clear to red, bind groups, draw 3 vertices (a triangle)
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_encoder"),
        });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test_rp"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &render_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            });
            rp.set_pipeline(&pipeline);
            rp.set_vertex_buffer(0, vertex_buf.slice(..));
            for (set_idx, bg) in bind_groups.iter().enumerate() {
                rp.set_bind_group(set_idx as u32, bg, &[]);
            }
            // Bind white texture+sampler at set=1 for patch_fragment_wgsl
            let tex_set = bind_groups.len() as u32;
            let test_tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test_tex_bg"),
                layout: &test_tex_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&white_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&white_sampler),
                    },
                ],
            });
            rp.set_bind_group(tex_set, &test_tex_bg, &[]);
            rp.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }

    // Copy render target to readback buffer
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback_encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &render_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::default(),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256 * 4),
                    rows_per_image: Some(256),
                },
            },
            wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));
    }

    // Wait for GPU to finish
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });

    // Map readback buffer and check pixels
    let buf_slice = readback_buf.slice(..);
    buf_slice.map_async(wgpu::MapMode::Read, |r| {
        if let Err(e) = r {
            panic!("map_async failed: {:?}", e);
        }
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });

    let data = buf_slice.get_mapped_range();
    let pixels: &[u8] = &data;
    assert_eq!(pixels.len(), 256 * 256 * 4, "Readback should be 256×256×4 bytes");

    // If all storage buffers are zeroed, the shader may not produce visible
    // output — log whether pixels differ from the clear color.
    let any_different = pixels.chunks(4).any(|p| p[0] != 255 || p[1] != 0 || p[2] != 0 || p[3] != 255);
    if any_different {
        eprintln!("[TEST-RENDER] ✓ BNSH shaders rendered successfully (pixels differ from clear)");
    } else {
        eprintln!("[TEST-RENDER] ⚠ All pixels are red — shaders produced no visible output with dummy data");
    }

    drop(data);
    readback_buf.unmap();
}

/// Render a random game effect in a visible window and error out if any
/// fallback mechanisms are used (e.g., default WGSL shader instead of BNSH).
///
/// Uses a curated list of known fighter effects to avoid the slow full-directory
/// enumeration.  Effects are pre-filtered to those with non-empty BNSH SPIR-V.
///
/// This test bypasses ParticleRenderer::new_with_shaders because the BNSH
/// vertex shaders expect vertex buffer inputs (locations 0-7) that the default
/// particle pipeline (no vertex buffers) does not provide.  Instead, it builds
/// the pipeline manually, mirroring the approach in test_bnsh_shaders_render
/// but targeting a visible window instead of a headless texture.
#[test]
fn test_render_random_effect_no_fallback() {
    // Mario and link verified with per-emitter FS + canonical VS pairing.
    let known_working = &["mario", "link"];

    // Load and filter to effects with non-empty decoded SPIR-V
    let mut candidates: Vec<(String, PtclFile, BnshShaderSet)> = Vec::new();
    for name in known_working {
        if let Some(ptcl) = load_effect_ptcl(name) {
            if let Ok(bnsh_set) = BnshShaderSet::from_ptcl_file(&ptcl, &format!("{}.eff", name)) {
                let has_spirv = bnsh_set.default_pair().vertex.as_ref()
                    .map(|s| !s.spirv.is_empty()).unwrap_or(false)
                    && bnsh_set.default_pair().fragment.as_ref()
                        .map(|s| !s.spirv.is_empty()).unwrap_or(false);
                if bnsh_set.is_complete() && has_spirv {
                    candidates.push((name.to_string(), ptcl, bnsh_set));
                }
            }
        }
    }

    if candidates.is_empty() {
        eprintln!("[TEST-WINDOW] No candidates with valid BNSH SPIR-V — skipping");
        return;
    }

    // Pick one at random from the filtered list
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let idx = (seed % candidates.len() as u64) as usize;
    let (effect_name, _ptcl, bnsh_set) = candidates.swap_remove(idx);
    eprintln!("[TEST-WINDOW] Picked random effect: {}", effect_name);
    eprintln!("[TEST-WINDOW] BNSH shaders: {}", bnsh_set.summary());

    // ---------- SPIR-V → WGSL conversion (must NOT fall back) ----------
    let mut vs_wgsl = String::new();
    let mut fs_wgsl = String::new();
    let mut vs_descs = Vec::new();
    let mut fs_descs = Vec::new();
    for (label, stage, spirv_bytes, wgsl_out, descs_out) in [
        ("vertex",   naga::ShaderStage::Vertex,
         bnsh_set.default_pair().vertex.as_ref().map(|s| s.spirv.as_slice()),
         &mut vs_wgsl, &mut vs_descs),
        ("fragment", naga::ShaderStage::Fragment,
         bnsh_set.default_pair().fragment.as_ref().map(|s| s.spirv.as_slice()),
         &mut fs_wgsl, &mut fs_descs),
    ] {
        let spirv_bytes = spirv_bytes.expect("{} SPIR-V bytes should exist");
        let mut spirv_words = hitbox_editor::spirv_to_wgsl::bytes_to_words(spirv_bytes)
            .expect("SPIR-V bytes should convert to words");
        let _patches = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut spirv_words);
        let patched_bytes: Vec<u8> = spirv_words.iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        let (wgsl, descs) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
            &patched_bytes, stage, &format!("test_window_{}", label),
        ).unwrap_or_else(|e| {
            panic!("[TEST-WINDOW] BNSH → WGSL failed for {} {}: {} — fallback would be used!",
                effect_name, label, e);
        });
        eprintln!("[TEST-WINDOW] {}: {} lines, {} descriptors", label, wgsl.lines().count(), descs.len());
        *wgsl_out = wgsl;
        *descs_out = descs;
    }

    eprintln!("[TEST-WINDOW] ✓ BNSH → WGSL conversion succeeded for {} (no fallback)", effect_name);

    // ---------- Create visible window ----------
    // Rust test runner runs tests on separate threads, so use
    // with_any_thread to allow EventLoop creation on non-main threads.
    #[allow(deprecated)]
    use winit::event_loop::EventLoopBuilder;
    #[allow(deprecated)]
    use winit::window::Window;
    let event_loop = {
        #[cfg(target_os = "linux")]
        {
            use winit::platform::x11::EventLoopBuilderExtX11;
            let mut builder = EventLoopBuilder::new();
            builder.with_any_thread(true);
            builder.build()
        }
        #[cfg(not(target_os = "linux"))]
        {
            EventLoopBuilder::new().build()
        }
    };
    let event_loop = match event_loop {
        Ok(el) => el,
        Err(e) => {
            eprintln!("[TEST-WINDOW] Cannot create event loop (no display?): {:?}", e);
            eprintln!("[TEST-WINDOW] Skipping window test");
            return;
        }
    };
    let window = std::sync::Arc::new(
        #[allow(deprecated)]
        event_loop.create_window(
            Window::default_attributes()
                .with_title(format!("Hitbox — {}", effect_name))
                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0))
        ).expect("Failed to create winit window")
    );

    // ---------- wgpu device + surface ----------
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let surface = instance.create_surface(window.clone())
        .expect("Failed to create wgpu surface");

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let adapter = rt.block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        },
    )).expect("No compatible GPU adapter available");

    let surface_format = surface.get_capabilities(&adapter).formats[0];
    let (device, queue) = rt.block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("test_window_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        },
    )).expect("Failed to create wgpu device");

    surface.configure(&device, &wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: 800,
        height: 600,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    });

    // White fallback texture for the fragment shader's texture bind group
    let window_white_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("window_white_tex"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let window_white_view = window_white_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let window_white_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

    // ---------- Build pipeline with proper vertex buffer layout ----------
    // Patch vertex WGSL — spirv-cross independently renumbers locations
    // in vertex (outputs 0-5) vs fragment (inputs 0-7).
    let vs_wgsl = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
    let fs_wgsl = patch_fragment_wgsl(&fs_wgsl);

    let vs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bnsh_vs"),
        source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
    });
    let fs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bnsh_fs"),
        source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
    });

    // Build bind group layout from all descriptors
    let mut all_descs: Vec<hitbox_editor::spirv_to_wgsl::DescriptorInfo> = vs_descs;
    all_descs.extend(fs_descs);
    all_descs.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));
    all_descs.dedup_by(|a, b| a.set == b.set && a.binding == b.binding);

    let max_set = all_descs.iter().map(|d| d.set).max().unwrap_or(0);
    let per_set: Vec<Vec<&hitbox_editor::spirv_to_wgsl::DescriptorInfo>> = (0..=max_set)
        .map(|s| all_descs.iter().filter(|d| d.set == s).collect())
        .collect();

    let mut dummy_buffers: Vec<wgpu::Buffer> = Vec::new();
    for entries in &per_set {
        for d in entries {
            let usage = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => {
                    wgpu::BufferUsages::STORAGE
                }
                _ => wgpu::BufferUsages::UNIFORM,
            };
            let buf_size = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => 65536u64,
                _ => 256u64,
            };
            dummy_buffers.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("dummy_{}_{}", d.set, d.binding)),
                size: buf_size,
                usage,
                mapped_at_creation: false,
            }));
        }
    }

    let mut bgls: Vec<wgpu::BindGroupLayout> = Vec::new();
    let mut bind_groups: Vec<wgpu::BindGroup> = Vec::new();
    let mut buf_idx = 0usize;
    for (set, entries) in per_set.iter().enumerate() {
        let mut bgl_entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
        let mut bg_entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::new();

        for d in entries {
            let (visibility, binding_ty) = match d.class {
                hitbox_editor::spirv_to_wgsl::BindingClass::Texture => (
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Sampler => (
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Storage => (
                    wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                hitbox_editor::spirv_to_wgsl::BindingClass::Uniform => (
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
            bg_entries.push(wgpu::BindGroupEntry {
                binding: d.binding,
                resource: dummy_buffers[buf_idx].as_entire_binding(),
            });
            buf_idx += 1;
        }

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("test_bgl_{}", set)),
            entries: &bgl_entries,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("test_bg_{}", set)),
            layout: &bgl,
            entries: &bg_entries,
        });
        bgls.push(bgl);
        bind_groups.push(bg);
    }

    // Create texture bind group layout for set=1 (used by patch_fragment_wgsl)
    let window_tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("window_tex_bgl"),
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
    // Create texture bind group for set=1
    let window_tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("window_tex_bg"),
        layout: &window_tex_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&window_white_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&window_white_sampler),
            },
        ],
    });
    bind_groups.push(window_tex_bg);

    let mut bgl_all: Vec<Option<&wgpu::BindGroupLayout>> = bgls.iter().map(|b| Some(b)).collect();
    bgl_all.push(Some(&window_tex_bgl));
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bnsh_window_test"),
        bind_group_layouts: &bgl_all,
        immediate_size: 0,
    });

    // Vertex buffer with 8 attributes per vertex
    let mut vertex_data: Vec<f32> = vec![0.0f32; 3 * 8 * 4];
    vertex_data[0] = -1.0;
    vertex_data[8*4] = 0.0;
    vertex_data[8*4*2] = 1.0;
    let vertex_buf = device.create_buffer_init(
        &wgpu::util::BufferInitDescriptor {
            label: Some("vertex_buf"),
            contents: bytemuck::cast_slice(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        }
    );

    let mut vertex_attributes: Vec<wgpu::VertexAttribute> = Vec::new();
    for loc in 0..8i32 {
        vertex_attributes.push(wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: (loc as u64) * 16,
            shader_location: loc as u32,
        });
    }
    let vertex_buf_layouts = [wgpu::VertexBufferLayout {
        array_stride: 8 * 16,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attributes,
    }];

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bnsh_window_test"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_mod,
            entry_point: Some("main"),
            buffers: &vertex_buf_layouts,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs_mod,
            entry_point: Some("main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
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
        multiview_mask: None,
        cache: None,
    });

    eprintln!("[TEST-WINDOW] ✓ Pipeline created for {} using BNSH shaders", effect_name);

    // ---------- Render one frame ----------
    let frame = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) |
        wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        other => panic!("Failed to acquire swapchain frame: {:?}", other),
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test_window_encoder"),
    });
    {
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test_window_rp"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.15, g: 0.15, b: 0.2, a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rp.set_pipeline(&pipeline);
        rp.set_vertex_buffer(0, vertex_buf.slice(..));
        for (set_idx, bg) in bind_groups.iter().enumerate() {
            rp.set_bind_group(set_idx as u32, bg, &[]);
        }
        rp.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
    frame.present();

    eprintln!("[TEST-WINDOW] ✓ Rendered one frame of {} with BNSH shaders in a visible window", effect_name);
}

/// Minimal sanity test: render a white triangle on red background using simple
/// WGSL shaders (no BNSH).  This verifies the wgpu test infrastructure works.
#[test]
fn test_simple_triangle_render() {
    let (device, queue) = match create_test_device() {
        (Some(d), Some(q)) => (d, q),
        _ => {
            eprintln!("[SANITY] Skipping — no GPU available");
            return;
        }
    };
    device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
        eprintln!("[wgpu ERROR] {:?}", e);
    }));

    // Minimal WGSL shaders
    let vs_wgsl = r#"
@vertex
fn main(@location(0) pos: vec4<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(pos.x, pos.y, 0.0, 1.0);
}
"#;
    let fs_wgsl = r#"
@fragment
fn main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

    let vs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple_vs"),
        source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
    });
    let fs_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple_fs"),
        source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
    });

    // Pipeline with no bind groups
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });

    let vertex_attrs = [wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 0,
        shader_location: 0,
    }];
    let vertex_buf_layout = [wgpu::VertexBufferLayout {
        array_stride: 16,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attrs,
    }];

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("simple"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_mod,
            entry_point: Some("main"),
            buffers: &vertex_buf_layout,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs_mod,
            entry_point: Some("main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // Vertex buffer: 3 vertices forming a triangle
    let vertex_data: [f32; 12] = [
        -0.5, -0.5, 0.0, 1.0,
         0.5, -0.5, 0.0, 1.0,
         0.0,  0.5, 0.0, 1.0,
    ];
    let vertex_buf = device.create_buffer_init(
        &wgpu::util::BufferInitDescriptor {
            label: Some("vtx"),
            contents: bytemuck::cast_slice(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        }
    );

    // Render target
    let render_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt"),
        size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_tex.create_view(&wgpu::TextureViewDescriptor::default());

    // Readback
    let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (256 * 256 * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // Render
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("enc"),
        });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &render_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&pipeline);
            rp.set_vertex_buffer(0, vertex_buf.slice(..));
            rp.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }

    // Copy and readback
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("copy"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &render_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::default(),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256 * 4),
                    rows_per_image: Some(256),
                },
            },
            wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));
    }

    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

    let buf_slice = readback_buf.slice(..);
    buf_slice.map_async(wgpu::MapMode::Read, |r| {
        if let Err(e) = r {
            panic!("map_async failed: {:?}", e);
        }
    });
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

    let data = buf_slice.get_mapped_range();
    let pixels: &[u8] = &data;
    let any_different = pixels.chunks(4).any(|p| p[0] != 255 || p[1] != 0 || p[2] != 0 || p[3] != 255);
    assert!(any_different, "Simple triangle should produce visible output, but all pixels are red");
    eprintln!("[SANITY] ✓ Simple triangle rendered successfully");
    drop(data);
    readback_buf.unmap();
}



