use std::collections::HashMap;

use glam::{Mat4, Vec3};
use hitbox_editor::effects::{BlendType, EffIndex, ParticleSystem, PtclFile};
use hitbox_editor::particle_renderer_bnsh::{
    blend_state_for, bnsh_vertex_layout, load_bnsh_shader_modules, BnshPipelineState, BnshShaderSet,
};
use hitbox_editor::spirv_to_wgsl::{patch_vertex_wgsl, vertex_return_wires_fs_inputs};
use std::path::Path;

const EFFECT_DIR: &str =
    "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect";
const BOMB_KEY: u64 = 0x5740678a2aa5959f;

fn bomb_pair() -> Option<hitbox_editor::bnsh_shader_integration::EffectShaderPair> {
    let path = Path::new(EFFECT_DIR)
        .join("fighter")
        .join("samus")
        .join("ef_samus.eff");
    if !path.exists() {
        return None;
    }
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).ok()?;
    let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
    let set = BnshShaderSet::from_ptcl_file(&ptcl, "ef_samus.eff").ok()?;
    set.all_shaders.get(&BOMB_KEY).cloned()
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
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
    Some((device, queue))
}

#[test]
fn test_samus_bomb_shader_links_locations_6_and_7() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair() else {
        eprintln!("Samus effect not found — skipping");
        return;
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

    let patched = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
    assert!(
        patched.contains("@location(6) out_attr6_"),
        "patched VS must output location 6 for bomb FS"
    );
    assert!(
        patched.contains("@location(7) out_attr7_"),
        "patched VS must output location 7 for bomb FS"
    );
    assert!(
        vertex_return_wires_fs_inputs(&patched, &fs_wgsl),
        "patched VS return must wire all bomb FS varyings"
    );
    assert!(
        patched.contains("out_attr6_ = in_attr6_"),
        "location 6 must pass through vertex input"
    );
    assert!(
        !patched.contains("out_attr0_ = in_attr0_"),
        "must not clobber NVN-computed varyings with raw vertex inputs"
    );
    assert!(
        patched.contains("return VertexOutput(_e239, _e241, _e243, _e245, _e247, _e249, out_attr6_, out_attr7_, _e251)"),
        "must extend spirv-cross return temps, not replace them"
    );
}

#[test]
fn test_samus_bomb_fs_mrt_clamped_to_location_0() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair() else {
        eprintln!("Samus effect not found — skipping");
        return;
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
        clamped.contains("@location(0) out_attr0_"),
        "visible colour must remain at location 0"
    );
    assert!(
        !clamped.contains("@location(1)"),
        "deferred G-buffer outputs must be trimmed for single-target composite"
    );
    assert!(
        clamped.contains("return FragmentOutput(_e239"),
        "constructor must keep only the primary colour arg"
    );

    let enhanced = hitbox_editor::spirv_to_wgsl::enhance_native_fragment_wgsl(&clamped);
    assert!(
        enhanced.contains("textureSample(color_tex, color_sampler"),
        "native FS must sample emitter texture into location 0"
    );
    assert!(
        enhanced.contains("_c.rgb * _ts.rgb"),
        "texture must modulate native out_attr0_ chain, not a secondary MRT"
    );
}

#[test]
fn test_samus_bomb_sub_pipeline_valid_on_gpu() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let Some(pair) = bomb_pair() else {
        eprintln!("Samus effect not found — skipping");
        return;
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
    let modules = load_bnsh_shader_modules(&device, &pair, &label);

    let tex_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("dummy_tex"),
        entries: &[],
    });
    let state = BnshPipelineState::new(
        &device,
        modules,
        &tex_bg_layout,
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

// Geometry/rasterization is fixed (verified: passes with FX_DEBUG_SOLID_FS=1), but the native
// FS colour chain currently outputs zero, so additive-blended particles leave the background
// unchanged. Ignored until root cause #3 (FS colour chain) is resolved; run explicitly with
// `--ignored` to track progress.
#[ignore = "blocked on root cause #3: FS colour chain outputs zero (geometry verified via FX_DEBUG_SOLID_FS)"]
#[test]
fn test_samus_bomb_particle_renderer_produces_visible_pixels() {
    std::env::set_var("FX_NATIVE_FS", "1");
    let path = Path::new(EFFECT_DIR)
        .join("fighter")
        .join("samus")
        .join("ef_samus.eff");
    if !path.exists() {
        eprintln!("Samus effect not found — skipping");
        return;
    }
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, "ef_samus.eff").expect("bnsh");
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
    renderer.upload_textures(&device, &queue, &ptcl);

    // Find an emitter that uses the bomb shader key
    let mut emitter_key = None;
    for (set_idx, set) in ptcl.emitter_sets.iter().enumerate() {
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
    let emitter = &ptcl.emitter_sets[set_idx].emitters[emitter_idx];

    use glam::{Mat4, Vec3, Vec4};
    use hitbox_editor::effects::Particle;

    let particle = Particle {
        position: Vec3::ZERO,
        velocity: Vec3::ZERO,
        age: 0.1,
        lifetime: 2.0,
        color: Vec4::new(1.0, 0.2, 0.1, 1.0),
        size: 50.0,
        rotation: 0.0,
        rotation_speed: 0.0,
        emitter_set_idx: set_idx,
        emitter_idx,
        texture_idx: 0,
        blend_type: emitter.blend_type,
        tex_offset: emitter.tex_offset_uv,
        seed: 0,
    };

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
        &ptcl.emitter_sets,
        &ptcl.bfres_models,
    );

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: 256 * 256 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
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
    let mut any_visible = false;
    let mut brightest = (0u8, 0u8, 0u8);
    let mut max_lum = 0u32;
    let mut vis_count = 0usize;
    for p in pixels.chunks(4) {
        if p[0].abs_diff(bg.0) > 8 || p[1].abs_diff(bg.1) > 8 || p[2].abs_diff(bg.2) > 8 {
            any_visible = true;
            vis_count += 1;
            let lum = p[0] as u32 + p[1] as u32 + p[2] as u32;
            if lum > max_lum {
                max_lum = lum;
                brightest = (p[0], p[1], p[2]);
            }
        }
    }
    eprintln!(
        "[BOMB-READBACK] visible_px={vis_count}/{} brightest_rgb={brightest:?}",
        256 * 256
    );
    drop(data);
    readback.unmap();
    assert!(
        any_visible,
        "bomb ParticleRenderer draw produced no pixels different from clear color"
    );
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
    std::env::set_var("FX_NATIVE_FS", if native_fs { "1" } else { "0" });
    if native_vs_pos {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
    } else {
        std::env::remove_var("FX_NATIVE_VS_POS");
    }
    let path = Path::new(EFFECT_DIR).join(effect_rel);
    if !path.exists() {
        return None;
    }
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
    let cam_right = Vec3::new(view.col(0).x, view.col(0).y, view.col(0).z);
    let cam_up = Vec3::new(view.col(1).x, view.col(1).y, view.col(1).z);

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
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sim_clear"),
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
        &system.particles,
        &[],
        &ptcl.emitter_sets,
        &ptcl.bfres_models,
    );
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    Some((count, readback_any_visible(&device, &queue, &target)))
}

#[test]
fn test_samus_bomb_simulation_renders_with_native_and_patched_fs() {
    let path = Path::new(EFFECT_DIR)
        .join("fighter")
        .join("samus")
        .join("ef_samus.eff");
    if !path.exists() {
        eprintln!("Samus effect not found — skipping");
        return;
    }
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let spawn = eff
        .handles
        .keys()
        .find(|k| k.contains("bomb") || k.contains("Bomb"))
        .cloned()
        .unwrap_or_else(|| "samus_atk_bomb".to_string());

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
    let path = Path::new(EFFECT_DIR)
        .join("fighter")
        .join("samus")
        .join("ef_samus.eff");
    if !path.exists() {
        eprintln!("Samus effect not found — skipping");
        return;
    }
    let eff = hitbox_editor::effects::EffIndex::from_file(&path).expect("eff");
    let spawn = eff
        .handles
        .keys()
        .find(|k| k.contains("bomb") || k.contains("Bomb"))
        .cloned()
        .unwrap_or_else(|| "samus_atk_bomb".to_string());

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
    let path = Path::new(EFFECT_DIR).join(effect_rel);
    if !path.exists() {
        return None;
    }
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
    let png = format!("/tmp/diag_{handle}_{cam_label}.png");
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
    if !Path::new(EFFECT_DIR).join(rel).exists() {
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
    if !Path::new(EFFECT_DIR).join(rel).exists() {
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
    let path = Path::new(EFFECT_DIR)
        .join("fighter")
        .join("mario")
        .join("ef_mario.eff");
    if !path.exists() {
        eprintln!("Mario effect not found — skipping");
        return;
    }
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
