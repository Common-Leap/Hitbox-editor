use std::collections::HashMap;

use glam::{Mat4, Vec3, Vec4};
use hitbox_editor::effects::{BlendType, EffIndex, Particle, ParticleSystem, PtclFile};
use hitbox_editor::particle_renderer_bnsh::{
    blend_state_for, bnsh_vertex_layout, load_bnsh_shader_modules, BnshPipelineState, BnshShaderSet,
};
use hitbox_editor::spirv_to_wgsl::{
    fragment_input_locations, patch_vertex_wgsl, vertex_return_wires_fs_inputs,
};

const BOMB_KEY: u64 = hitbox_editor::bnsh_shader_integration::BOMB_SHADER_KEY;

/// GPU tests mutate process-global `FX_*` env vars; serialize to avoid parallel flake.
fn gpu_test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn effect_export_file(rel: &str) -> Option<std::path::PathBuf> {
    let root = hitbox_editor::scratch_dirs::effect_export_root()?;
    let path = root.join(rel);
    path.exists().then_some(path)
}

fn bomb_pair_or_local() -> Option<hitbox_editor::bnsh_shader_integration::EffectShaderPair> {
    let (pairs, _) = hitbox_editor::bnsh_shader_integration::decode_effect_export_shaders("samus");
    let pair = pairs.get(&BOMB_KEY)?.clone();
    if pair.vertex.is_some() && pair.fragment.is_some() {
        Some(pair)
    } else {
        None
    }
}

/// Minimal PTCL for GPU tests using the bomb shader from the effect export.
fn synthetic_bomb_ptcl() -> Option<PtclFile> {
    hitbox_editor::bnsh_shader_integration::synthetic_ptcl_from_shader_key(
        BOMB_KEY,
        BlendType::Normal,
    )
    .ok()
}

fn samus_bomb_spawn_handle(eff: &EffIndex) -> String {
    ["samus_atk_bomb", "samus_cshot_bomb"]
        .iter()
        .find(|name| eff.handles.contains_key(**name))
        .map(|s| (*s).to_string())
        .or_else(|| {
            eff.handles
                .keys()
                .find(|k| k.contains("bomb") || k.contains("Bomb"))
                .cloned()
        })
        .unwrap_or_else(|| "samus_atk_bomb".to_string())
}

fn synthetic_bomb_particle(ptcl: &PtclFile) -> Particle {
    let emitter = &ptcl.emitter_sets[0].emitters[0];
    let lifetime = emitter.lifetime.max(1.0);
    let age = lifetime * 0.05;
    let life_t = (age / lifetime).clamp(0.0, 1.0);
    let c0 = hitbox_editor::effects::sample_color_pub(&emitter.color0, life_t);
    let c1 = hitbox_editor::effects::sample_color_pub(&emitter.color1, life_t);
    let a0 = hitbox_editor::effects::sample_color_pub(&emitter.alpha0_keys, life_t)[0];
    let a1 = hitbox_editor::effects::sample_color_pub(&emitter.alpha1_keys, life_t)[0];
    let size = (emitter.scale * emitter.scale_anim.sample(life_t)).max(0.01);
    // GPU fixture camera is far (z≈250); match simulation life/colour but keep quad on-screen.
    let size = size.max(40.0);
    Particle {
        position: Vec3::ZERO,
        velocity: Vec3::ZERO,
        accel_world: Vec3::ZERO,
        age,
        lifetime,
        color: Vec4::new(c0[0], c0[1], c0[2], a0),
        color0_rgb: [c0[0], c0[1], c0[2]],
        color1_rgb: [c1[0], c1[1], c1[2]],
        alpha0_live: a0,
        alpha1_live: a1,
        color_scale_live: emitter.color_scale,
        draw_path: emitter.draw_path,
        pre_draw: false,
        parent_emitter_idx: None,
        inst_start_frame: 0.0,
        inherit: None,
        size,
        rotation: 0.0,
        rotation_speed: 0.0,
        emitter_set_idx: 0,
        emitter_idx: 0,
        local_offset: Vec3::ZERO,
        bone_name: "Trans".to_string(),
        inst_offset: Vec3::ZERO,
        inst_rotation: Vec3::ZERO,
        texture_idx: 0,
        blend_type: emitter.blend_type,
        tex_offset: emitter.tex_offset_uv,
        indirect_tex_offset: [0.0, 0.0],
        tex2_tex_offset: [0.0, 0.0],
        tex_scale_live: [1.0, 1.0],
        tex_scroll_angle: 0.0,
        pat_phase_offset: 0.0,
        pat_fixed_frame: None,
        pat_blend: 0.0,
        pat_next_uv_delta: [0.0, 0.0],
        tex_extra_offsets: [[0.0, 0.0]; 3],
        seed: 0,
        rotation_rand: Vec3::ZERO,
    }
}

fn render_particles_visible(
    ptcl: &PtclFile,
    particles: &[Particle],
    native_fs: bool,
    native_vs_pos: bool,
) -> Option<bool> {
    let _lock = gpu_test_env_lock();
    std::env::set_var("FX_NATIVE_FS", if native_fs { "1" } else { "0" });
    if native_vs_pos {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
    } else {
        std::env::remove_var("FX_NATIVE_VS_POS");
    }
    let bnsh_set = BnshShaderSet::from_ptcl_file(ptcl, "fixture_bomb.ptcl").ok()?;
    if !bnsh_set.all_shaders.contains_key(&BOMB_KEY) {
        return None;
    }
    let (device, queue) = create_test_device()?;
    let mut renderer = hitbox_editor::particle_renderer::ParticleRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &bnsh_set,
    );
    renderer.upload_textures(&device, &queue, ptcl);

    let cam_pos = Vec3::new(0.0, 50.0, 250.0);
    let view = Mat4::look_at_rh(cam_pos, Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 1.0, 1.0, 5000.0);
    let view_proj = proj * view;
    let mv_inv = view.inverse();
    let cam_right = mv_inv.col(0).truncate().normalize();
    let cam_up = mv_inv.col(1).truncate().normalize();

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("particle_render_target"),
        size: wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("particle_render_encoder"),
    });
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.05,
                        g: 0.05,
                        b: 0.1,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &target_view,
        view_proj,
        cam_right,
        cam_up,
        particles,
        &[],
        &ptcl.emitter_sets,
        &ptcl.bfres_models,
    );
    queue.submit(Some(encoder.finish()));
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let max_px = readback_max_pixel(&device, &queue, &target);
    eprintln!(
        "[FIXTURE-RENDER] target_max=({},{},{},{})",
        max_px[0], max_px[1], max_px[2], max_px[3]
    );
    Some(readback_particles_on_transparent(&device, &queue, &target))
}

fn particle_tex_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("particle_tex_bgl"),
        entries: &hitbox_editor::particle_renderer::emitter_tex_bind_group_layout_entries(),
    })
}

fn particle_extra_tex345_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let mut entries = Vec::with_capacity(7);
    for binding in (0..6).step_by(2) {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: binding + 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 6,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: std::num::NonZeroU64::new(96),
        },
        count: None,
    });
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 7,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: std::num::NonZeroU64::new(64),
        },
        count: None,
    });
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bnsh_extra_tex345_bgl"),
        entries: &entries,
    })
}

fn particle_group2_placeholder_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bnsh_group2_placeholder_bgl"),
        entries: &[],
    })
}

fn particle_soft_particle_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bnsh_soft_particle_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(32),
                },
                count: None,
            },
        ],
    })
}

fn create_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let rt = tokio::runtime::Runtime::new().ok()?;
    let adapter = rt
        .block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
    let (device, queue) = rt
        .block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("bomb_pipeline_test"),
            required_features: wgpu::Features::empty(),
            required_limits: hitbox_editor::wgpu_device_limits(&adapter),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
    Some((device, queue))
}

#[test]
fn test_samus_bomb_shader_links_locations_0_through_5() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair_or_local() else {
        panic!("bomb shader fixture missing and Samus effect not found");
    };

    let vs = pair.vertex.as_ref().expect("vs");
    let fs = pair.fragment.as_ref().expect("fs");
    let mut vs_w = hitbox_editor::spirv_to_wgsl::bytes_to_words(&vs.spirv).unwrap();
    let mut fs_w = hitbox_editor::spirv_to_wgsl::bytes_to_words(&fs.spirv).unwrap();
    let _ = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
    let _ = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
    let _ = hitbox_editor::spirv_patch::nvn_remap_vertex_input_locations(&mut vs_w);
    let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let (vs_wgsl, _) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
        &to_bytes(&vs_w),
        naga::ShaderStage::Vertex,
        "bomb_vs",
    )
    .unwrap();
    let (fs_wgsl, _) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
        &to_bytes(&fs_w),
        naga::ShaderStage::Fragment,
        "bomb_fs",
    )
    .unwrap();

    let fs_locs = fragment_input_locations(&fs_wgsl);
    assert_eq!(
        fs_locs,
        vec![0, 1, 2, 3, 4, 5],
        "real bomb FS uses locations 0-5 only"
    );

    let patched = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
    for loc in &fs_locs {
        assert!(
            patched.contains(&format!("@location({loc})")),
            "patched VS must output location {loc} for bomb FS"
        );
    }
    assert!(
        patched.contains("@location(5) out_attr5_"),
        "patched VS must output location 5 for bomb FS"
    );
    assert!(
        vertex_return_wires_fs_inputs(&patched, &fs_wgsl),
        "patched VS return must wire all bomb FS varyings"
    );
    assert!(
        hitbox_editor::spirv_to_wgsl::vs_has_native_color_chain(&vs_wgsl),
        "bomb VS must keep native NVN colour chain"
    );
    assert!(
        !patched.contains("out_attr0_ = in_attr0_"),
        "must not clobber NVN-computed varyings with raw vertex inputs"
    );
    assert!(
        !patched.contains("out_attr0_ = in_attr1_1"),
        "must not clobber NVN colour with CPU attr1 passthrough"
    );
    assert!(
        !patched.contains("out_attr1_ = in_attr1_1"),
        "must not clobber NVN colour with CPU attr1 passthrough"
    );
    // CPU quad UV (attr2) passthrough is required when the decoded VS omits @location(2).
}

#[test]
fn test_samus_bomb_fs_mrt_clamped_to_location_0() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair_or_local() else {
        panic!("bomb shader fixture missing and Samus effect not found");
    };

    let fs = pair.fragment.as_ref().expect("fs");
    let mut fs_w = hitbox_editor::spirv_to_wgsl::bytes_to_words(&fs.spirv).unwrap();
    let _ = hitbox_editor::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
    let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let (fs_wgsl, _) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
        &to_bytes(&fs_w),
        naga::ShaderStage::Fragment,
        "bomb_fs",
    )
    .unwrap();

    let clamped = hitbox_editor::spirv_to_wgsl::clamp_fragment_output_locations(
        &fs_wgsl,
        hitbox_editor::spirv_to_wgsl::PARTICLE_COMPOSITE_MRT_LOCATIONS,
    );
    assert!(
        clamped.contains("@location(0) frag_color0_"),
        "visible colour must remain at location 0"
    );
    let frag_out_struct = clamped
        .split("struct FragmentOutput {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .unwrap_or("");
    assert!(
        !frag_out_struct.contains("@location(1)"),
        "deferred G-buffer outputs must be trimmed for single-target composite"
    );
    let frag_return = clamped
        .lines()
        .find(|l| l.contains("return FragmentOutput("))
        .expect("return FragmentOutput");
    assert!(
        !frag_return.contains(','),
        "constructor must keep only the primary colour arg: {frag_return}"
    );

    let enhanced = hitbox_editor::spirv_to_wgsl::enhance_native_fragment_wgsl(&clamped);
    assert!(
        enhanced.contains("textureSample(color_tex, color_sampler"),
        "native FS must sample emitter texture into location 0"
    );
    assert!(
        enhanced.contains("_fx_native_in.rgb * _fx_ts.rgb")
            || enhanced.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts"),
        "texture must modulate native frag_color0_ chain, not a secondary MRT"
    );
}

#[test]
fn test_samus_bomb_sub_pipeline_valid_on_gpu() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair_or_local() else {
        panic!("bomb shader fixture missing and Samus effect not found");
    };
    let Some((device, _queue)) = create_test_device() else {
        eprintln!("No GPU — skipping");
        return;
    };

    eprintln!(
        "[BOMB-GPU] vs entry={:?} fs entry={:?}",
        pair.vertex.as_ref().map(|s| s.entry_point.as_str()),
        pair.fragment.as_ref().map(|s| s.entry_point.as_str()),
    );

    let label = format!("{BOMB_KEY:#x}");
    let modules = load_bnsh_shader_modules(
        &device,
        &pair,
        &label,
        hitbox_editor::shader_registry::NativeColorInput::Auto,
        hitbox_editor::shader_registry::ShaderVsProfile::ParticleBillboard,
    );

    let tex_bg_layout = particle_tex_bind_group_layout(&device);
    let extra_tex345_bg_layout = particle_extra_tex345_bind_group_layout(&device);
    let group2_placeholder_bg_layout = particle_group2_placeholder_bind_group_layout(&device);
    let soft_particle_bg_layout = particle_soft_particle_bind_group_layout(&device);
    let state = BnshPipelineState::new(
        &device,
        modules,
        &tex_bg_layout,
        Some(&extra_tex345_bg_layout),
        &group2_placeholder_bg_layout,
        Some(&soft_particle_bg_layout),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &label,
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("bnsh_blend_{label}_Sub")),
        layout: Some(&state.pipeline_layout),
        vertex: wgpu::VertexState {
            module: &state.vs_module,
            entry_point: Some(&state.vs_entry),
            buffers: &[bnsh_vertex_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &state.fs_module,
            entry_point: Some(&state.fs_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(blend_state_for(BlendType::Sub)),
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
    let _ = device.poll(wgpu::PollType::Poll);
    let err = rt.block_on(scope.pop());
    if let Some(e) = err {
        panic!("bomb Sub pipeline invalid: {e:?}");
    }
    let _ = pipeline;
}

#[test]
fn test_samus_attack_shader_depth_pipelines_valid_on_gpu() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let (pairs, _) = hitbox_editor::bnsh_shader_integration::decode_effect_export_shaders("samus");
    let keys = [
        0x620eb49dad22664a_u64,
        0x1214b7abe376cc24_u64,
        0xf83f92d82c51ed75_u64,
    ];
    let Some((device, _queue)) = create_test_device() else {
        eprintln!("No GPU — skipping");
        return;
    };
    let tex_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("particle_tex_bgl"),
        entries: &hitbox_editor::particle_renderer::emitter_tex_bind_group_layout_entries(),
    });
    let extra_tex345_bg_layout = particle_extra_tex345_bind_group_layout(&device);
    let group2_placeholder_bg_layout = particle_group2_placeholder_bind_group_layout(&device);
    let soft_particle_bg_layout = particle_soft_particle_bind_group_layout(&device);
    let rt = tokio::runtime::Runtime::new().unwrap();

    for key in keys {
        let Some(pair) = pairs.get(&key) else {
            continue;
        };
        let label = format!("{key:#x}");
        let modules = load_bnsh_shader_modules(
            &device,
            pair,
            &label,
            hitbox_editor::shader_registry::NativeColorInput::Auto,
            hitbox_editor::shader_registry::ShaderVsProfile::ParticleBillboard,
        );
        let state = BnshPipelineState::new(
            &device,
            modules,
            &tex_bg_layout,
            Some(&extra_tex345_bg_layout),
            &group2_placeholder_bg_layout,
            Some(&soft_particle_bg_layout),
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &label,
        );
        for (depth_write, tag) in [(false, "depth"), (true, "depth_write")] {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("bnsh_blend_{label}_Normal_{tag}")),
                layout: Some(&state.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &state.vs_module,
                    entry_point: Some(&state.vs_entry),
                    buffers: &[bnsh_vertex_layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: if depth_write {
                        &state.fs_module_depth_write
                    } else {
                        &state.fs_module
                    },
                    entry_point: Some(&state.fs_entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        blend: Some(blend_state_for(BlendType::Normal)),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: Some(if depth_write {
                    hitbox_editor::particle_renderer_bnsh::particle_depth_stencil_state_write()
                } else {
                    hitbox_editor::particle_renderer_bnsh::particle_depth_stencil_state()
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            let _ = device.poll(wgpu::PollType::Poll);
            let err = rt.block_on(scope.pop());
            if let Some(e) = err {
                panic!("{label} Normal {tag} pipeline invalid: {e:?}");
            }
        }
    }
}

#[test]
fn test_bomb_fixture_particle_renderer_produces_visible_pixels() {
    let Some(ptcl) = synthetic_bomb_ptcl() else {
        panic!("bomb fixture PTCL build failed");
    };
    let particle = synthetic_bomb_particle(&ptcl);
    let Some(visible) = render_particles_visible(&ptcl, std::slice::from_ref(&particle), true, false)
    else {
        eprintln!("No GPU — skipping");
        return;
    };
    assert!(
        visible,
        "bomb fixture ParticleRenderer draw produced no pixels different from clear color"
    );
}

/// Full Samus effect path (optional local dump). Uses the same native VS+FS chain as the
/// passing simulation test once geometry and colour paths are aligned.
#[test]
fn test_samus_bomb_particle_renderer_produces_visible_pixels() {
    if let Some(ptcl) = synthetic_bomb_ptcl() {
        let particle = synthetic_bomb_particle(&ptcl);
        if let Some(visible) =
            render_particles_visible(&ptcl, std::slice::from_ref(&particle), true, true)
        {
            assert!(
                visible,
                "bomb fixture ParticleRenderer (native VS+FS) produced no visible pixels"
            );
            return;
        }
        eprintln!("No GPU — skipping fixture path");
        return;
    }

    let path = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus");
    let Some(path) = path else {
        eprintln!("Samus effect not found — skipping local integration path");
        return;
    };
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let mut merged = ptcl;
    if let Some(fixture) = synthetic_bomb_ptcl() {
        merged.shader_registry = fixture.shader_registry;
    }
    let bnsh_set = BnshShaderSet::from_ptcl_file(&merged, "ef_samus.eff").expect("bnsh");
    if !bnsh_set.all_shaders.contains_key(&BOMB_KEY) {
        eprintln!("Bomb shader key not in registry — skipping");
        return;
    }

    let Some((device, queue)) = create_test_device() else {
        eprintln!("No GPU — skipping");
        return;
    };

    let mut renderer = hitbox_editor::particle_renderer::ParticleRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &bnsh_set,
    );
    renderer.upload_textures(&device, &queue, &merged);

    let mut emitter_key = None;
    for (set_idx, set) in merged.emitter_sets.iter().enumerate() {
        for (emitter_idx, emitter) in set.emitters.iter().enumerate() {
            if bnsh_set.pipeline_key_for_emitter(emitter) == BOMB_KEY {
                emitter_key = Some((set_idx, emitter_idx));
                break;
            }
        }
        if emitter_key.is_some() {
            break;
        }
    }
    let Some((set_idx, emitter_idx)) = emitter_key else {
        eprintln!("No bomb emitter in samus ptcl — skipping");
        return;
    };
    let emitter = &merged.emitter_sets[set_idx].emitters[emitter_idx];

    let particle = Particle {
        position: Vec3::ZERO,
        velocity: Vec3::ZERO,
        accel_world: Vec3::ZERO,
        age: 0.1,
        lifetime: 2.0,
        color: Vec4::new(1.0, 0.2, 0.1, 1.0),
        color0_rgb: [1.0, 0.2, 0.1],
        color1_rgb: [1.0, 1.0, 1.0],
        alpha0_live: 1.0,
        alpha1_live: 1.0,
        color_scale_live: 1.0,
        draw_path: emitter.draw_path,
        pre_draw: false,
        parent_emitter_idx: None,
        inst_start_frame: 0.0,
        inherit: None,
        size: 50.0,
        rotation: 0.0,
        rotation_speed: 0.0,
        emitter_set_idx: set_idx,
        emitter_idx,
        local_offset: Vec3::ZERO,
        bone_name: "Trans".to_string(),
        inst_offset: Vec3::ZERO,
        inst_rotation: Vec3::ZERO,
        texture_idx: 0,
        blend_type: emitter.blend_type,
        tex_offset: emitter.tex_offset_uv,
        indirect_tex_offset: [0.0, 0.0],
        tex2_tex_offset: [0.0, 0.0],
        tex_scale_live: [1.0, 1.0],
        tex_scroll_angle: 0.0,
        pat_phase_offset: 0.0,
        pat_fixed_frame: None,
        pat_blend: 0.0,
        pat_next_uv_delta: [0.0, 0.0],
        tex_extra_offsets: [[0.0, 0.0]; 3],
        seed: 0,
        rotation_rand: Vec3::ZERO,
    };

    std::env::set_var("FX_NATIVE_FS", "1");
    std::env::set_var("FX_NATIVE_VS_POS", "1");
    let cam_pos = Vec3::new(0.0, 50.0, 250.0);
    let view = Mat4::look_at_rh(cam_pos, Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 1.0, 1.0, 5000.0);
    let view_proj = proj * view;
    let cam_right = Vec3::new(view.col(0).x, view.col(0).y, view.col(0).z);
    let cam_up = Vec3::new(view.col(1).x, view.col(1).y, view.col(1).z);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bomb_render_target"),
        size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("bomb_render_encoder"),
    });
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.1, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }

    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &target_view,
        view_proj,
        cam_right,
        cam_up,
        std::slice::from_ref(&particle),
        &[],
        &merged.emitter_sets,
        &merged.bfres_models,
    );

    queue.submit(Some(encoder.finish()));

    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });

    let max_px = readback_max_pixel(&device, &queue, &target);
    let any_visible = readback_particles_on_transparent(&device, &queue, &target);
    eprintln!(
        "[BOMB-READBACK] target_max=({},{},{},{}) visible={any_visible}",
        max_px[0], max_px[1], max_px[2], max_px[3]
    );
    assert!(
        any_visible,
        "samus bomb ParticleRenderer draw produced no visible pixels on transparent clear"
    );
}

fn readback_max_pixel(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> [u8; 4] {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("max_readback"),
        size: 256 * 256 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("max_readback_encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(256),
            },
        },
        wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let data = slice.get_mapped_range();
    let mut max = [0u8; 4];
    for px in data.chunks(4) {
        for i in 0..4 {
            max[i] = max[i].max(px[i]);
        }
    }
    drop(data);
    readback.unmap();
    max
}

fn readback_particles_on_transparent(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> bool {
    let max = readback_max_pixel(device, queue, target);
    max[0] > 8 || max[1] > 8 || max[2] > 8 || max[3] > 8
}

fn readback_any_visible(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> bool {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sim_readback"),
        size: 256 * 256 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("sim_readback_encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(256),
            },
        },
        wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let data = slice.get_mapped_range();
    let pixels: &[u8] = &data;
    // Target is Rgba8UnormSrgb: linear clear (0.05,0.05,0.1) is STORED as ~(63,63,89).
    let bg = (63u8, 63u8, 89u8);
    let visible = pixels.chunks(4).any(|p| {
        p[0].abs_diff(bg.0) > 8 || p[1].abs_diff(bg.1) > 8 || p[2].abs_diff(bg.2) > 8
    });
    drop(data);
    readback.unmap();
    visible
}

/// Step a real effect simulation and read back pixels from ParticleRenderer.
fn simulation_render_visible(
    effect_rel: &str,
    spawn_handle: &str,
    target_frame: f32,
    native_fs: bool,
) -> Option<(usize, bool)> {
    simulation_render_visible_opts(effect_rel, spawn_handle, target_frame, native_fs, false)
}

fn simulation_render_visible_opts(
    effect_rel: &str,
    spawn_handle: &str,
    target_frame: f32,
    native_fs: bool,
    native_vs_pos: bool,
) -> Option<(usize, bool)> {
    let _lock = gpu_test_env_lock();
    std::env::set_var("FX_NATIVE_FS", if native_fs { "1" } else { "0" });
    if native_vs_pos {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
    } else {
        std::env::remove_var("FX_NATIVE_VS_POS");
    }
    let path = effect_export_file(effect_rel)?;
    let eff = EffIndex::from_file(&path).ok()?;
    let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
    let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, path.file_name()?.to_str()?).ok()?;
    let (device, queue) = create_test_device()?;

    let mut renderer = hitbox_editor::particle_renderer::ParticleRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &bnsh_set,
    );
    renderer.upload_textures(&device, &queue, &ptcl);

    let mut system = ParticleSystem::default();
    system.spawn_effect(
        spawn_handle,
        "Trans",
        Vec3::ZERO,
        Vec3::ZERO,
        0.0,
        9999.0,
        &eff,
        &ptcl,
    );
    let bone_matrices: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)]
        .into_iter()
        .collect();
    system.step(target_frame, &bone_matrices, &ptcl);
    system.particles.retain(|p| !p.is_dead());
    let count = system.particles.len();
    eprintln!(
        "[SIM-RENDER] {spawn_handle} frame={target_frame} native_fs={native_fs} particles={count}"
    );
    if count == 0 {
        return Some((0, false));
    }

    let cam_pos = Vec3::new(0.0, 50.0, 250.0);
    let view = Mat4::look_at_rh(cam_pos, Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 1.0, 1.0, 5000.0);
    let view_proj = proj * view;
    let mv_inv = view.inverse();
    let cam_right = mv_inv.col(0).truncate().normalize();
    let cam_up = mv_inv.col(1).truncate().normalize();

    renderer.prepare_particle_frame(
        &device,
        &queue,
        view_proj,
        cam_right,
        cam_up,
        cam_pos,
        &system.particles,
        &[],
        &ptcl.emitter_sets,
        &ptcl.bfres_models,
        &bone_matrices,
        &system.active_emitters,
        target_frame,
    );
    if renderer.prepared_draw_paths().is_empty() {
        return Some((count, false));
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sim_render_target"),
        size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("sim_render_encoder"),
    });
    let paths: Vec<u32> = renderer.prepared_draw_paths().to_vec();
    for (i, &path) in paths.iter().enumerate() {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&format!("sim_particles_{path}")),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: if i == 0 {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.draw_prepared_particles_for_path(
            &device,
            &mut rpass,
            path,
            true,
            hitbox_editor::particle_renderer::BnshDrawFilter::ExcludeSub,
            hitbox_editor::particle_renderer::DepthDrawConfig::NONE,
        );
        renderer.draw_prepared_particles_for_path(
            &device,
            &mut rpass,
            path,
            false,
            hitbox_editor::particle_renderer::BnshDrawFilter::SubOnly,
            hitbox_editor::particle_renderer::DepthDrawConfig::NONE,
        );
    }
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let max_px = readback_max_pixel(&device, &queue, &target);
    eprintln!(
        "[SIM-RENDER] {spawn_handle} frame={target_frame} native_fs={native_fs} native_vs={native_vs_pos} target_max=({},{},{},{})",
        max_px[0], max_px[1], max_px[2], max_px[3]
    );
    Some((count, readback_particles_on_transparent(&device, &queue, &target)))
}

/// Same GPU path as the editor viewport `paint` callback: scene clear, then direct
/// per-path particle draws (ExcludeSub + SubOnly) onto the viewport target.
fn editor_viewport_direct_render_visible(
    effect_rel: &str,
    spawn_handle: &str,
    target_frame: f32,
    native_fs: bool,
    native_vs_pos: bool,
) -> Option<(usize, bool)> {
    let _lock = gpu_test_env_lock();
    std::env::set_var("FX_NATIVE_FS", if native_fs { "1" } else { "0" });
    if native_vs_pos {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
    } else {
        std::env::remove_var("FX_NATIVE_VS_POS");
    }
    let path = effect_export_file(effect_rel).or_else(|| {
        hitbox_editor::scratch_dirs::resolve_fighter_eff(
            effect_rel.rsplit('/').next().unwrap_or(effect_rel)
                .trim_start_matches("ef_")
                .trim_end_matches(".eff"),
        )
    })?;
    let eff = EffIndex::from_file(&path).ok()?;
    let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
    let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, path.file_name()?.to_str()?).ok()?;
    let (device, queue) = create_test_device()?;

    let mut renderer = hitbox_editor::particle_renderer::ParticleRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &bnsh_set,
    );
    renderer.upload_textures(&device, &queue, &ptcl);
    renderer.upload_meshes(&device, &queue, &ptcl);

    let mut system = ParticleSystem::default();
    system.spawn_effect(
        spawn_handle,
        "Trans",
        Vec3::ZERO,
        Vec3::ZERO,
        0.0,
        9999.0,
        &eff,
        &ptcl,
    );
    let bone_matrices: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)]
        .into_iter()
        .collect();
    system.step(target_frame, &bone_matrices, &ptcl);
    system.particles.retain(|p| !p.is_dead());
    let count = system.particles.len();
    if std::env::var("FX_VIEWPORT_LOG").is_ok() && count > 0 {
        for (i, p) in system.particles.iter().take(4).enumerate() {
            eprintln!(
                "[EDITOR-VIEWPORT] particle[{i}] pos=({:.2},{:.2},{:.2}) size={:.3}",
                p.position.x, p.position.y, p.position.z, p.size
            );
        }
    }
    if count == 0 {
        return Some((0, false));
    }

    let cam_pos = Vec3::new(0.0, 50.0, 250.0);
    let view = Mat4::look_at_rh(cam_pos, Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 1.0, 1.0, 5000.0);
    let view_proj = proj * view;
    let mv_inv = view.inverse();
    let cam_right = mv_inv.col(0).truncate().normalize();
    let cam_up = mv_inv.col(1).truncate().normalize();

    renderer.prepare_particle_frame(
        &device,
        &queue,
        view_proj,
        cam_right,
        cam_up,
        cam_pos,
        &system.particles,
        &[],
        &ptcl.emitter_sets,
        &ptcl.bfres_models,
        &bone_matrices,
        &system.active_emitters,
        target_frame,
    );
    if renderer.prepared_draw_paths().is_empty() {
        return Some((count, false));
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("editor_viewport_target"),
        size: wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("editor_viewport_encoder"),
    });

    {
        let _mesh_bg = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("editor_scene_mesh"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.05,
                        g: 0.05,
                        b: 0.1,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    let paths: Vec<u32> = renderer.prepared_draw_paths().to_vec();
    for &path in &paths {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&format!("editor_viewport_particles_{path}")),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.draw_prepared_particles_for_path(
            &device,
            &mut rpass,
            path,
            true,
            hitbox_editor::particle_renderer::BnshDrawFilter::ExcludeSub,
            hitbox_editor::particle_renderer::DepthDrawConfig::NONE,
        );
        renderer.draw_prepared_particles_for_path(
            &device,
            &mut rpass,
            path,
            false,
            hitbox_editor::particle_renderer::BnshDrawFilter::SubOnly,
            hitbox_editor::particle_renderer::DepthDrawConfig::NONE,
        );
    }
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    if std::env::var("FX_VIEWPORT_LOG").is_ok() {
        let final_max = readback_max_pixel(&device, &queue, &target);
        eprintln!(
            "[EDITOR-VIEWPORT-READBACK] paths={} final_max=({},{},{},{})",
            paths.len(),
            final_max[0], final_max[1], final_max[2], final_max[3],
        );
    }
    Some((count, readback_any_visible(&device, &queue, &target)))
}

#[test]
fn test_editor_viewport_direct_draw_samus_bomb_visible() {
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        eprintln!("Samus effect not found — skipping");
        return;
    };
    let eff = EffIndex::from_file(&path).expect("eff");
    let spawn = samus_bomb_spawn_handle(&eff);

    let Some((count, native_vis)) = editor_viewport_direct_render_visible(
        "fighter/samus/ef_samus.eff",
        &spawn,
        30.0,
        true,
        true,
    ) else {
        eprintln!("No GPU or effect — skipping");
        return;
    };
    let patched_vis = editor_viewport_direct_render_visible(
        "fighter/samus/ef_samus.eff",
        &spawn,
        30.0,
        false,
        true,
    )
    .map(|(_, v)| v)
    .unwrap_or(false);
    eprintln!(
        "[EDITOR-VIEWPORT] samus bomb frame=30 particles={count} native={native_vis} patched={patched_vis}"
    );
    if count == 0 {
        eprintln!("No particles at frame 30 — simulation may need another frame");
        return;
    }
    assert!(
        native_vis || patched_vis,
        "editor viewport direct draw produced no visible pixels at frame 30 (particles={count}, native={native_vis}, patched={patched_vis})"
    );
}

#[test]
fn diag_samus_patched_vs_includes_billboard_override() {
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        return;
    };
    std::env::remove_var("FX_NATIVE_VS_POS");
    let eff = EffIndex::from_file(&path).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, path.file_name().unwrap().to_str().unwrap())
        .expect("bnsh");
    let mut checked = 0usize;
    for set in &ptcl.emitter_sets {
        for emitter in &set.emitters {
            let pair = bnsh_set.pair_for_emitter(emitter);
            let Some(vs_info) = pair.vertex.as_ref() else {
                continue;
            };
            let Some(fs_info) = pair.fragment.as_ref() else {
                continue;
            };
            let vs_spirv = vs_info.spirv.as_slice();
            let fs_spirv = fs_info.spirv.as_slice();
            let (vs_wgsl, _) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
                vs_spirv,
                naga::ShaderStage::Vertex,
                "diag_vs",
            )
            .expect("vs wgsl");
            let (fs_wgsl, _) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
                fs_spirv,
                naga::ShaderStage::Fragment,
                "diag_fs",
            )
            .expect("fs wgsl");
            let vs_prefixed = hitbox_editor::spirv_to_wgsl::wire_vertex_simulation_varyings(&vs_wgsl);
            let fs_prefixed =
                hitbox_editor::spirv_to_wgsl::wire_extra_tex_fragment_input(
                    &hitbox_editor::spirv_to_wgsl::wire_crossfade_fragment_input(&fs_wgsl, &vs_prefixed),
                    &vs_prefixed,
                );
            let patched = hitbox_editor::spirv_to_wgsl::patch_vertex_wgsl_with_hint(
                &vs_prefixed,
                &fs_prefixed,
                None,
            );
            let usage = hitbox_editor::nvn_chain::cbuf_slot_usage_from_shaders(
                Some(vs_spirv),
                Some(fs_spirv),
                &patched,
                &fs_prefixed,
            );
            let billboard = hitbox_editor::spirv_to_wgsl::billboard_particle_vs(&patched);
            let hybrid = patched.contains("_vp0 * _world.x");
            let c9 = usage.get("cbuf_9_1_").cloned().unwrap_or_default();
            let c8 = usage.get("cbuf_8_1_").cloned().unwrap_or_default();
            eprintln!(
                "[DIAG-VS] key={:#x} blend={:?} billboard={billboard} hybrid={hybrid} c9={c9:?} c8={c8:?}",
                bnsh_set.pipeline_key_for_emitter(emitter),
                emitter.blend_type,
            );
            checked += 1;
            if checked >= 8 {
                return;
            }
        }
    }
}

#[test]
fn test_bomb_fixture_gpu_native_and_patched_fs_visible() {
    let Some(ptcl) = synthetic_bomb_ptcl() else {
        panic!("bomb fixture PTCL build failed");
    };
    let particle = synthetic_bomb_particle(&ptcl);
    let Some(native_vis) = render_particles_visible(&ptcl, std::slice::from_ref(&particle), true, true)
    else {
        eprintln!("No GPU — skipping");
        return;
    };
    let Some(patched_vis) =
        render_particles_visible(&ptcl, std::slice::from_ref(&particle), false, false)
    else {
        return;
    };
    eprintln!(
        "[FIXTURE-GPU] bomb native_vis={native_vis} patched_vis={patched_vis}"
    );
    assert!(
        native_vis || patched_vis,
        "bomb fixture produced no visible pixels (native={native_vis}, patched={patched_vis})"
    );
}

#[test]
fn test_samus_bomb_simulation_renders_with_native_and_patched_fs() {
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        eprintln!("Samus effect not found — skipping");
        return;
    };
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let spawn = samus_bomb_spawn_handle(&eff);

    let Some((count, native_vis)) =
        simulation_render_visible("fighter/samus/ef_samus.eff", &spawn, 30.0, true)
    else {
        eprintln!("No GPU or effect — skipping");
        return;
    };
    eprintln!("[SIM-RENDER] samus bomb native_fs=1 visible={native_vis} particles={count}");
    if count == 0 {
        eprintln!("No particles at frame 30 — try different frame/handle");
        return;
    }

    let Some((_, patched_vis)) =
        simulation_render_visible("fighter/samus/ef_samus.eff", &spawn, 30.0, false)
    else {
        return;
    };
    eprintln!("[SIM-RENDER] samus bomb native_fs=0 visible={patched_vis}");

    assert!(
        native_vis || patched_vis,
        "samus bomb simulation produced no visible pixels (native={native_vis}, patched={patched_vis})"
    );
}

/// Native NVN position chain (no billboard override) + native FS colour chain.
#[test]
fn test_samus_bomb_native_vs_and_fs_renders_pixels() {
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        eprintln!("Samus effect not found — skipping");
        return;
    };
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let spawn = samus_bomb_spawn_handle(&eff);

    let Some((count, visible)) = simulation_render_visible_opts(
        "fighter/samus/ef_samus.eff",
        &spawn,
        30.0,
        true,
        true,
    ) else {
        eprintln!("No GPU or effect — skipping");
        return;
    };
    eprintln!(
        "[SIM-RENDER] samus bomb native_vs+native_fs particles={count} visible={visible}"
    );
    if count == 0 {
        eprintln!("No particles at frame 30 — try different frame/handle");
        return;
    }
    assert!(
        visible,
        "samus bomb with FX_NATIVE_VS_POS=1 and FX_NATIVE_FS=1 produced no visible pixels"
    );
}

/// DIAGNOSTIC: render a handle under an arbitrary camera, report particle world bounds,
/// clip-space NDC of each particle center, and whether any non-background pixels appear.
fn diag_render(
    effect_rel: &str,
    handle: &str,
    frame: f32,
    cam_label: &str,
    view_proj: Mat4,
    cam_right: Vec3,
    cam_up: Vec3,
) -> Option<bool> {
    std::env::set_var("FX_NATIVE_FS", "1");
    let path = effect_export_file(effect_rel)?;
    let eff = EffIndex::from_file(&path).ok()?;
    let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
    let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, path.file_name()?.to_str()?).ok()?;
    let (device, queue) = create_test_device()?;
    let mut renderer = hitbox_editor::particle_renderer::ParticleRenderer::new(
        &device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, &bnsh_set,
    );
    renderer.upload_textures(&device, &queue, &ptcl);
    renderer.upload_meshes(&device, &queue, &ptcl);

    let mut system = ParticleSystem::default();
    system.spawn_effect(handle, "Trans", Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &eff, &ptcl);
    let bones: HashMap<String, Mat4> =
        [("Trans".to_string(), Mat4::IDENTITY)].into_iter().collect();
    system.step(frame, &bones, &ptcl);
    system.particles.retain(|p| !p.is_dead());
    let count = system.particles.len();
    if count == 0 {
        eprintln!("[DIAG] {handle} cam={cam_label}: 0 particles at frame {frame}");
        return Some(false);
    }

    // World bounds + clip-space of each particle center.
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut on_screen = 0;
    for p in &system.particles {
        min = min.min(p.position);
        max = max.max(p.position);
        let clip = view_proj * p.position.extend(1.0);
        if clip.w.abs() > 1e-6 {
            let ndc = clip.truncate() / clip.w;
            let visible = clip.w > 0.0
                && ndc.x >= -1.0 && ndc.x <= 1.0
                && ndc.y >= -1.0 && ndc.y <= 1.0
                && ndc.z >= 0.0 && ndc.z <= 1.0;
            if visible {
                on_screen += 1;
            }
        }
    }
    let sample = &system.particles[0];
    // Report the blend mode + texture presence of each emitter that produced particles.
    let mut emitters_seen: Vec<(usize, usize)> = system
        .particles
        .iter()
        .map(|p| (p.emitter_set_idx, p.emitter_idx))
        .collect();
    emitters_seen.sort_unstable();
    emitters_seen.dedup();
    for (si, ei) in &emitters_seen {
        if let Some(em) = ptcl.emitter_sets.get(*si).and_then(|s| s.emitters.get(*ei)) {
            eprintln!(
                "[DIAG]   emitter ({si},{ei}) blend={:?} tex_index={} num_textures={}",
                em.blend_type, em.texture_index, em.textures.len()
            );
        }
    }
    eprintln!(
        "[DIAG] {handle} cam={cam_label}: {count} particles, world min={min:?} max={max:?}, \
         on_screen_centers={on_screen}/{count}, p0.pos={:?} size={} color={:?}",
        sample.position, sample.size, sample.color
    );

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("diag_target"),
        size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("diag_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.1, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    renderer.render(
        &device, &queue, &mut encoder, &target_view,
        view_proj, cam_right, cam_up,
        &system.particles, &[], &ptcl.emitter_sets, &ptcl.bfres_models,
    );
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    let png = hitbox_editor::scratch_dirs::workshop_tmp_path(&format!(
        "diag_{handle}_{cam_label}.png"
    ));
    let png = png.to_string_lossy().into_owned();
    let (vis, stats) = readback_stats_and_png(&device, &queue, &target, &png);
    eprintln!("[DIAG] {handle} cam={cam_label}: rendered visible={vis} {stats} -> {png}");
    Some(vis)
}

/// Read back a 256x256 target, report visible-pixel count + brightest pixel, save PNG.
fn readback_stats_and_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    png_path: &str,
) -> (bool, String) {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("diag_readback"),
        size: 256 * 256 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(256),
            },
        },
        wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    let data = slice.get_mapped_range();
    let pixels: Vec<u8> = data.to_vec();
    drop(data);
    readback.unmap();

    // The target is sRGB; linear clear (0.05,0.05,0.1) is STORED as ~(63,63,89).
    let bg = (63u8, 63u8, 89u8);
    let mut visible_count = 0usize;
    let mut brightest = (0u8, 0u8, 0u8);
    let mut max_lum = 0u32;
    for p in pixels.chunks(4) {
        if p[0].abs_diff(bg.0) > 8 || p[1].abs_diff(bg.1) > 8 || p[2].abs_diff(bg.2) > 8 {
            visible_count += 1;
            let lum = p[0] as u32 + p[1] as u32 + p[2] as u32;
            if lum > max_lum {
                max_lum = lum;
                brightest = (p[0], p[1], p[2]);
            }
        }
    }
    let _ = image::save_buffer(
        png_path,
        &pixels,
        256,
        256,
        image::ColorType::Rgba8,
    );
    let total = 256 * 256;
    let stats = format!(
        "visible_px={visible_count}/{total} ({:.1}%) brightest_rgb={brightest:?}",
        100.0 * visible_count as f32 / total as f32
    );
    (visible_count > 0, stats)
}

#[test]
fn diag_viewer_vs_lookat_camera() {
    let rel = "fighter/mario/ef_mario.eff";
    if effect_export_file(rel).is_none() {
        eprintln!("Mario effect not found — skipping");
        return;
    }

    // Viewer camera (examples/effect_viewer.rs).
    let model_view = Mat4::from_translation(Vec3::new(0.0, -8.0, -60.0))
        * Mat4::from_euler(glam::EulerRot::XYZ, 0.0, std::f32::consts::FRAC_PI_2, 0.0);
    let projection = Mat4::perspective_rh(30f32.to_radians(), 256.0 / 256.0, 1.0, 400_000.0);
    let viewer_vp = projection * model_view;
    let viewer_right = model_view.inverse().col(0).truncate().normalize();
    let viewer_up = model_view.inverse().col(1).truncate().normalize();

    // Known-good look_at camera (passing test).
    let view = Mat4::look_at_rh(Vec3::new(0.0, 50.0, 250.0), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 1.0, 1.0, 5000.0);
    let look_vp = proj * view;
    let look_right = view.col(0).truncate();
    let look_up = view.col(1).truncate();

    for handle in ["mario_pump_hit", "mario_fb_bullet_l"] {
        diag_render(rel, handle, 65.0, "viewer", viewer_vp, viewer_right, viewer_up);
        diag_render(rel, handle, 65.0, "lookat", look_vp, look_right, look_up);
    }
}

/// Render a handle with a camera framed to the particle extent, so the actual quad shape and
/// color are visible (not full-screen or sub-pixel). Dumps PNGs for both native and patched FS.
#[test]
fn diag_framed_color() {
    let rel = "fighter/mario/ef_mario.eff";
    if effect_export_file(rel).is_none() {
        eprintln!("Mario effect not found — skipping");
        return;
    }
    // Frame: camera on +Z looking at origin; distance chosen to fit a ~tens-of-units quad.
    let dist = 220.0f32;
    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, dist), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(0.9, 1.0, 1.0, 5000.0);
    let vp = proj * view;
    let right = view.col(0).truncate();
    let up = view.col(1).truncate();
    // NOTE: `fx_native_fs_enabled()` is cached (OnceLock). Native FS is the default;
    // set FX_PATCHED_FS=1 or FX_NATIVE_FS=0 before the first shader load for patched FS.
    let label = if hitbox_editor::fx_native_fs_enabled() { "framed_native" } else { "framed_patched" };
    for handle in ["mario_pump_hit", "mario_fb_bullet_l", "mario_appeal"] {
        diag_render(rel, handle, 65.0, label, vp, right, up);
    }
}

#[test]
fn test_mario_fb_bullet_simulation_renders_pixels() {
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("mario") else {
        eprintln!("Mario effect not found — skipping");
        return;
    };
    for &(frame, native_fs) in &[(65.0f32, true), (65.0, false)] {
        let Some((count, visible)) =
            simulation_render_visible("fighter/mario/ef_mario.eff", "mario_fb_bullet_l", frame, native_fs)
        else {
            eprintln!("No GPU — skipping");
            return;
        };
        eprintln!(
            "[SIM-RENDER] mario_fb_bullet_l frame={frame} native_fs={native_fs} particles={count} visible={visible}"
        );
    }
}
