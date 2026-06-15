use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use glam::{Mat4, Vec3};
use hitbox_editor::effects::{EffIndex, PtclFile, ParticleSystem, BlendType, SwordTrail, TrailSystem};
use hitbox_editor::particle_renderer::ParticleRenderer;
use hitbox_editor::particle_renderer_bnsh::BnshShaderSet;
use wgpu::util::DeviceExt;

const EFFECT_DIR: &str = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect";

fn load_effect(effect_name: &str) -> (EffIndex, PtclFile) {
    let candidates = vec![
        Path::new(EFFECT_DIR).join("fighter").join(effect_name).join(format!("ef_{}.eff", effect_name)),
        Path::new(EFFECT_DIR).join(format!("ef_{}.eff", effect_name)),
    ];
    let path = candidates.into_iter().find(|p| p.exists())
        .unwrap_or_else(|| panic!("Effect file not found for '{}' in {:?}", effect_name, EFFECT_DIR));
    let eff = EffIndex::from_file(&path)
        .unwrap_or_else(|e| panic!("Failed to parse .eff: {e}"));
    let ptcl = PtclFile::parse(&eff.ptcl_data)
        .unwrap_or_else(|e| panic!("Failed to parse .ptcl: {e}"));
    eprintln!("Loaded '{}': {} emitter sets, {} bntx textures, {} primitives, {} bfres models",
        effect_name, ptcl.emitter_sets.len(), ptcl.bntx_textures.len(),
        ptcl.primitives.len(), ptcl.bfres_models.len());
    (eff, ptcl)
}

/// Pick a spawn handle for the demo viewer: exact name first, then shortest
/// `{effect}_*` handle with the earliest emission timing.
fn pick_demo_handle(effect_name: &str, eff_index: &EffIndex, ptcl: &PtclFile) -> String {
    if eff_index.handles.contains_key(effect_name) {
        return effect_name.to_string();
    }
    let lower = effect_name.to_lowercase();
    if eff_index.handles.contains_key(&lower) {
        return lower;
    }
    let mut candidates: Vec<(&String, i32)> = eff_index
        .handles
        .iter()
        .filter(|(k, _)| {
            let kl = k.to_lowercase();
            kl == lower || kl.starts_with(&format!("{lower}_"))
        })
        .map(|(k, &set_idx)| {
            let timing = ptcl
                .emitter_sets
                .get(set_idx as usize)
                .and_then(|s| s.emitters.first())
                .map(|e| e.emission_timing as i32)
                .unwrap_or(9999);
            (k, timing)
        })
        .collect();
    candidates.sort_by_key(|(k, timing)| (*timing, k.len()));
    if let Some((k, timing)) = candidates.first() {
        eprintln!("Auto-matched handle '{k}' (emission_timing={timing})");
        return (*k).clone();
    }
    let keys: Vec<&String> = eff_index.handles.keys().take(15).collect();
    eprintln!("Available handles (first 15): {keys:?}");
    panic!("No effect handle matches '{effect_name}'. Pass the handle name as the second argument.");
}

fn main() {
    let effect_name = std::env::args().nth(1).unwrap_or_else(|| "mario".to_string());
    let handle_name = std::env::args().nth(2).unwrap_or_default();

    let (eff_index, ptcl) = load_effect(&effect_name);

    // Pick a handle: prefer explicit arg, else best demo match for this effect file.
    let spawn_name = if !handle_name.is_empty() {
        eprintln!("Using explicit handle: '{handle_name}'");
        handle_name
    } else {
        pick_demo_handle(&effect_name, &eff_index, &ptcl)
    };

    // --- winit window ---
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
            builder.build().expect("Failed to build event loop")
        }
        #[cfg(not(target_os = "linux"))]
        EventLoopBuilder::new().build().expect("Failed to build event loop")
    };

    let window = std::sync::Arc::new(
        event_loop.create_window(
            Window::default_attributes()
                .with_title(format!("Effect Viewer — {}", effect_name))
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
        ).expect("Failed to create window")
    );

    // --- wgpu ---
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let surface = instance.create_surface(window.clone()).expect("Failed to create surface");

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let adapter = rt.block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, force_fallback_adapter: false, compatible_surface: Some(&surface) },
    )).expect("No GPU adapter");

    let surface_format = surface.get_capabilities(&adapter).formats[0];
    let (device, queue) = rt.block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("viewer_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        },
    )).expect("Failed to create device");

    surface.configure(&device, &wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: 1280,
        height: 720,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    });

    // --- BNSH shaders ---
    let bnsh_shaders = BnshShaderSet::from_ptcl_file(&ptcl, &effect_name)
        .unwrap_or_else(|e| panic!("Failed to decode BNSH shaders: {e}"));

    // --- particle renderer ---
    let mut renderer = ParticleRenderer::new(&device, &queue, surface_format, &bnsh_shaders);
    renderer.upload_textures(&device, &queue, &ptcl);
    renderer.upload_meshes(&device, &queue, &ptcl);
    eprintln!("Textures and meshes uploaded");

    // --- particle system ---
    let mut particle_system = ParticleSystem::default();
    particle_system.spawn_effect(&spawn_name, "Trans", Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &eff_index, &ptcl);
    eprintln!("Effect '{spawn_name}' spawned from '{effect_name}'");

    let warmup_frame = 65.0f32;
    let bone_matrices_warm: HashMap<String, Mat4> = {
        let mut m = HashMap::new();
        m.insert("Trans".to_string(), Mat4::IDENTITY);
        m.insert("top".to_string(), Mat4::IDENTITY);
        m
    };
    particle_system.step(warmup_frame, &bone_matrices_warm, &ptcl);
    particle_system.particles.retain(|p| !p.is_dead());
    eprintln!(
        "Warm-up sim at frame {warmup_frame}: {} particles, {} emitters",
        particle_system.particles.len(),
        particle_system.active_emitters.len()
    );

    // Pre-warm only the emitter sets that actually produced particles, so the first drawn
    // frame doesn't stall compiling shaders (warming the whole file builds dozens of variants).
    {
        let mut warm_sets: Vec<usize> = particle_system
            .particles
            .iter()
            .map(|p| p.emitter_set_idx)
            .collect();
        warm_sets.sort_unstable();
        warm_sets.dedup();
        for set_idx in warm_sets {
            if let Some(set) = ptcl.emitter_sets.get(set_idx) {
                renderer.warm_bnsh_pipelines(&device, std::slice::from_ref(set));
            }
        }
    }

    let mut trail_system = TrailSystem::default();

    // --- frame timing ---
    let start = Instant::now();
    let mut frame_counter: f32 = warmup_frame;
    let mut last_frame_time = start;

    // --- event loop ---
    use winit::event::Event;
    use winit::event_loop::ControlFlow;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        match event {
            Event::WindowEvent { event: winit::event::WindowEvent::CloseRequested, .. } => {
                target.exit();
            }
            Event::WindowEvent { event: winit::event::WindowEvent::Resized(size), .. } => {
                surface.configure(&device, &wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: surface_format,
                    width: size.width.max(1),
                    height: size.height.max(1),
                    present_mode: wgpu::PresentMode::Fifo,
                    desired_maximum_frame_latency: 2,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: vec![],
                });
            }
            Event::AboutToWait => {
                window.request_redraw();
            }
            Event::WindowEvent { event: winit::event::WindowEvent::RedrawRequested, .. } => {
                let now = Instant::now();
                let dt = (now - last_frame_time).as_secs_f32().min(0.05);
                last_frame_time = now;

                let elapsed = (now - start).as_secs_f32();
                frame_counter += dt * 60.0 * 0.50;

                // Step simulation
                let bone_matrices: HashMap<String, Mat4> = {
                    let mut m = HashMap::new();
                    m.insert("Trans".to_string(), Mat4::IDENTITY);
                    m.insert("top".to_string(), Mat4::IDENTITY);
                    m
                };

                if frame_counter >= 0.0 {
                    let particle_count_before = particle_system.particles.len();
                    let emitter_count_before = particle_system.active_emitters.len();
                    particle_system.step(frame_counter, &bone_matrices, &ptcl);

                    // Update trails
                    for trail in &mut trail_system.trails {
                        trail.record(&bone_matrices);
                    }

                    if particle_system.particles.len() != particle_count_before || emitter_count_before != particle_system.active_emitters.len() {
                        let pc = particle_system.particles.len();
                        let ec = particle_system.active_emitters.len();
                        if pc > 0 || ec > 0 {
                            let fps = 1.0 / dt.max(0.001);
                            eprintln!("Frame {frame_counter:.1}: {pc} particles, {ec} emitters, {fps:.0} fps");
                        }
                    }
                }

                // Match editor default camera (translation + Y rotation, not look-at).
                let model_view = Mat4::from_translation(Vec3::new(0.0, -8.0, -60.0))
                    * Mat4::from_euler(
                        glam::EulerRot::XYZ,
                        0.0,
                        std::f32::consts::FRAC_PI_2,
                        0.0,
                    );
                let projection = Mat4::perspective_rh(
                    30f32.to_radians(),
                    1280.0 / 720.0,
                    1.0,
                    400_000.0,
                );
                let view_proj = projection * model_view;
                let cam_right = model_view.inverse().col(0).truncate().normalize();
                let cam_up = model_view.inverse().col(1).truncate().normalize();

                // Remove dead particles
                particle_system.particles.retain(|p| !p.is_dead());

                // Loop effect when done
                if particle_system.particles.is_empty() && particle_system.active_emitters.is_empty() {
                    frame_counter = warmup_frame;
                    particle_system.reset();
                    particle_system.spawn_effect(&spawn_name, "Trans", Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &eff_index, &ptcl);
                    particle_system.step(warmup_frame, &bone_matrices, &ptcl);
                    particle_system.particles.retain(|p| !p.is_dead());
                    eprintln!("--- loop ---");
                }

                // Get surface texture
                let frame = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    wgpu::CurrentSurfaceTexture::Timeout => return,
                    other => {
                        eprintln!("Surface error: {:?}", other);
                        return;
                    }
                };
                let view_tex = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

                // Render
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame_encoder"),
                });

                {
                    let _rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("frame_clear_rp"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view_tex,
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
                    &device, &queue, &mut encoder, &view_tex,
                    view_proj, cam_right, cam_up,
                    &particle_system.particles,
                    &trail_system.trails,
                    &ptcl.emitter_sets,
                    &ptcl.bfres_models,
                );

                queue.submit(Some(encoder.finish()));
                frame.present();
            }
            _ => {}
        }
    }).expect("Event loop failed");
}
