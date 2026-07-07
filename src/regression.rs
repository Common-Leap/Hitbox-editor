//! Regression / capture-diff harness (Phase 0 of the game-accuracy effort).
//!
//! Renders an effect headlessly at a given frame to a fixed 256×256 RGBA image and diffs it
//! against a golden PNG. The harness is agnostic to where a golden comes from:
//!   * a previously-approved editor render — pure regression, catches unintended changes; or
//!   * a framing-matched real game frame — game-accuracy reference.
//!
//! It reuses the offscreen-render + GPU readback pattern proven in
//! `tests/test_bomb_shader_patch.rs` (`diag_render` / `readback_stats_and_png`). GPU work is
//! serialized in the harness; run the test tiers with `--test-threads=1`.
//!
//! Consumers: `tests/regression_harness.rs` (CI gate) and `examples/regression_shot.rs`
//! (interactive). Both obtain a device via [`create_headless_device`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};

use crate::effects::{EffIndex, ParticleSystem, PtclFile};
use crate::particle_renderer::ParticleRenderer;
use crate::particle_renderer_bnsh::BnshShaderSet;

/// Fixed render-target edge. `256 * 4 = 1024` bytes/row is a multiple of wgpu's 256-byte
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, so texture→buffer readback needs no row padding.
pub const TARGET_SIZE: u32 = 256;

/// Offscreen clear colour (matches the diagnostic renders in the bomb-shader tests).
const CLEAR: wgpu::Color = wgpu::Color { r: 0.05, g: 0.05, b: 0.1, a: 1.0 };

/// sRGB-stored background produced by [`CLEAR`] on an `Rgba8UnormSrgb` target (~(63,63,89)).
/// A pixel is "visible" when it differs from this by more than [`BG_TOLERANCE`] on any channel.
const BG: [u8; 3] = [63, 63, 89];
const BG_TOLERANCE: u8 = 8;

/// Camera + billboard basis for a headless render.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub view_proj: Mat4,
    pub right: Vec3,
    pub up: Vec3,
}

impl Camera {
    /// Look-at camera framing the origin from +Z (matches the `diag_framed_color` test), so a
    /// tens-of-units quad spawned at the origin is centred and visible rather than sub-pixel or
    /// off-screen.
    pub fn framed_origin(distance: f32, fov_y: f32) -> Self {
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, distance), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(fov_y, 1.0, 1.0, 5000.0);
        Self {
            view_proj: proj * view,
            right: view.col(0).truncate(),
            up: view.col(1).truncate(),
        }
    }
}

impl Default for Camera {
    fn default() -> Self {
        // Fighter effects are unit-scale (the samus bomb spans ~2 units): a 220-unit
        // distance rendered them sub-pixel and the visual tier gated near-empty frames.
        // 25 units frames them at tens of pixels. Overridable for eyeballing via
        // REGRESSION_CAM_DIST.
        let dist = std::env::var("REGRESSION_CAM_DIST")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(25.0);
        Self::framed_origin(dist, 0.9)
    }
}

/// Create a headless wgpu device/queue (LowPower adapter). Returns `None` when no GPU/adapter
/// is available so callers can skip gracefully (headless CI without a GPU).
///
/// Mirrors `create_test_device` in the integration tests. Must NOT be called from inside an
/// existing tokio runtime — it spins up its own to block on the async wgpu requests.
pub fn create_headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
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
    let info = adapter.get_info();
    eprintln!(
        "[regression] adapter: {} ({:?}, {:?})",
        info.name, info.device_type, info.backend
    );
    let (device, queue) = rt
        .block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("regression_harness"),
            required_features: wgpu::Features::empty(),
            required_limits: crate::wgpu_device_limits(&adapter),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
    Some((device, queue))
}

/// A parsed effect (`.eff` + PTCL + decoded BNSH), ready to render frames from.
///
/// Borrows the device/queue so a single headless device can drive several effects in turn.
///
/// NOTE: each [`render_frame`](Self::render_frame) builds a **fresh** `ParticleRenderer` and
/// target. That is deliberate: a reused renderer reads uninitialized/stale GPU buffer memory
/// across frames, which is process-dependent (garbage differs per process) and makes output
/// non-reproducible run-to-run — see the "renderer buffer-reuse nondeterminism" bug. Rendering
/// each frame from a fresh renderer is the proven-deterministic path. It is slower (pipelines
/// rebuild per frame) but a regression gate renders only a handful of frames.
pub struct EffectHarness<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    eff: EffIndex,
    ptcl: PtclFile,
    bnsh_set: BnshShaderSet,
    /// Bone the effect is spawned on (identity transform). Defaults to `Trans`.
    pub spawn_bone: String,
}

impl<'a> EffectHarness<'a> {
    /// Load an `.eff`, parse its embedded PTCL, and decode the BNSH shader set. Returns `None`
    /// on any parse/decode failure.
    pub fn load(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        eff_path: &Path,
    ) -> Option<Self> {
        let eff = EffIndex::from_file(eff_path).ok()?;
        let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
        let filename = eff_path.file_name()?.to_str()?;
        Self::from_parts(device, queue, eff, ptcl, filename)
    }

    /// Like [`Self::load`], but from already-parsed (possibly ef_common-merged) data —
    /// lets the harness reproduce the live app's merged-PTCL state exactly.
    pub fn from_parts(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        eff: EffIndex,
        ptcl: PtclFile,
        source_name: &str,
    ) -> Option<Self> {
        let bnsh_set = BnshShaderSet::from_ptcl_file(&ptcl, source_name).ok()?;
        Some(Self {
            device,
            queue,
            eff,
            ptcl,
            bnsh_set,
            spawn_bone: "Trans".to_string(),
        })
    }

    /// Handles available in the loaded effect (emitter-set aliases).
    pub fn handles(&self) -> impl Iterator<Item = &String> {
        self.eff.handles.keys()
    }

    /// Simulate `handle` from spawn up to and including integer `frame` (fixed dt = 1.0, the
    /// game-accurate 60 Hz cadence), render one frame, and read back the RGBA8 pixels.
    ///
    /// A fresh [`ParticleSystem`] and a fresh renderer/target are used every call, so the result
    /// depends only on `(handle, frame, cam)` — deterministic and independent of prior frames.
    pub fn render_frame(&self, handle: &str, frame: u32, cam: Camera) -> Vec<u8> {
        let mut system = ParticleSystem::default();
        system.spawn_effect(
            handle,
            &self.spawn_bone,
            Vec3::ZERO,
            Vec3::ZERO,
            0.0,
            9999.0,
            &self.eff,
            &self.ptcl,
        );
        let bones: HashMap<String, Mat4> =
            [(self.spawn_bone.clone(), Mat4::IDENTITY)].into_iter().collect();
        for f in 0..=frame {
            system.step(f as f32, &bones, &self.ptcl);
        }
        system.particles.retain(|p| !p.is_dead());

        let mut renderer = ParticleRenderer::new(
            self.device,
            self.queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &self.bnsh_set,
        );
        renderer.upload_textures(self.device, self.queue, &self.ptcl);
        renderer.upload_meshes(self.device, self.queue, &self.ptcl);

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("regression_target"),
            size: wgpu::Extent3d {
                width: TARGET_SIZE,
                height: TARGET_SIZE,
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

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // Explicit clear: `renderer.render` loads the attachment, so it won't clear for us.
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("regression_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        // Full-context prepare (bones + active emitters + frame). The `renderer.render`
        // convenience wrapper passes empties for these, which collapses emitter world
        // transforms for matrix-dependent emitters — additive fire/flare paths rendered
        // nothing through it (mirrors simulation_render_visible_opts in the bomb suite).
        renderer.prepare_particle_frame(
            self.device,
            self.queue,
            cam.view_proj,
            cam.right,
            cam.up,
            cam.view_proj.inverse().col(3).truncate(),
            &system.particles,
            &[],
            &self.ptcl.emitter_sets,
            &self.ptcl.bfres_models,
            &bones,
            &system.active_emitters,
            frame as f32,
        );
        for (i, &path) in renderer.prepared_draw_paths().to_vec().iter().enumerate() {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("regression_particles"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: if i == 0 {
                            wgpu::LoadOp::Clear(CLEAR)
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
                self.device,
                &mut rpass,
                path,
                true,
                crate::particle_renderer::BnshDrawFilter::ExcludeSub,
                crate::particle_renderer::DepthDrawConfig::NONE,
            );
            renderer.draw_prepared_particles_for_path(
                self.device,
                &mut rpass,
                path,
                false,
                crate::particle_renderer::BnshDrawFilter::SubOnly,
                crate::particle_renderer::DepthDrawConfig::NONE,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        let _ = self
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        readback_rgba(self.device, self.queue, &target)
    }
}

/// Pure-CPU determinism probe: simulate `handle` to `frame` and return
/// `(particle_count, FNV-1a hash of every particle's position/velocity/size/rotation/age)`.
///
/// No GPU involved, so this isolates simulation determinism from render determinism. Two
/// separate process invocations must return the same value for a deterministic sim.
pub fn sim_fingerprint_from_file(
    eff_path: &Path,
    handle: &str,
    frame: u32,
    bone: &str,
) -> Option<(usize, u64)> {
    let eff = EffIndex::from_file(eff_path).ok()?;
    let ptcl = PtclFile::parse(&eff.ptcl_data).ok()?;
    let mut system = ParticleSystem::default();
    system.spawn_effect(handle, bone, Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &eff, &ptcl);
    let bones: HashMap<String, Mat4> =
        [(bone.to_string(), Mat4::IDENTITY)].into_iter().collect();
    for f in 0..=frame {
        system.step(f as f32, &bones, &ptcl);
    }
    system.particles.retain(|p| !p.is_dead());

    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |x: f32| {
        for b in x.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for p in &system.particles {
        mix(p.position.x);
        mix(p.position.y);
        mix(p.position.z);
        mix(p.velocity.x);
        mix(p.velocity.y);
        mix(p.velocity.z);
        mix(p.size);
        mix(p.rotation);
        mix(p.age);
    }
    Some((system.particles.len(), h))
}

/// Copy a 256×256 `Rgba8UnormSrgb` texture back to CPU as tightly-packed RGBA8 bytes.
fn readback_rgba(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> Vec<u8> {
    let bytes = (TARGET_SIZE * TARGET_SIZE * 4) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("regression_readback"),
        size: bytes,
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
                bytes_per_row: Some(TARGET_SIZE * 4),
                rows_per_image: Some(TARGET_SIZE),
            },
        },
        wgpu::Extent3d {
            width: TARGET_SIZE,
            height: TARGET_SIZE,
            depth_or_array_layers: 1,
        },
    );
    // The render was already submitted+polled by the caller; submit the copy and wait on it.
    let idx = queue.submit(Some(encoder.finish()));
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: Some(idx),
        timeout: None,
    });
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let data = slice.get_mapped_range();
    let pixels = data.to_vec();
    drop(data);
    readback.unmap();
    pixels
}

/// Per-image comparison metrics.
#[derive(Clone, Copy, Debug)]
pub struct DiffReport {
    pub width: u32,
    pub height: u32,
    /// Mean squared error per channel (R,G,B,A), 0..=65025.
    pub per_channel_mse: [f64; 4],
    /// Root-mean-square error across all RGBA channels, 0..=255.
    pub rmse: f64,
    /// Largest absolute single-channel delta, 0..=255.
    pub max_delta: u8,
    /// Mean absolute channel delta, 0..=255.
    pub mean_delta: f64,
    /// Pixels where any channel differs at all.
    pub changed_px: usize,
    pub visible_actual: usize,
    pub visible_golden: usize,
}

impl DiffReport {
    /// True when the images are within both thresholds (regression pass).
    pub fn within(&self, max_delta: u8, rmse: f64) -> bool {
        self.max_delta <= max_delta && self.rmse <= rmse
    }

    pub fn summary(&self) -> String {
        format!(
            "rmse={:.2} max_delta={} mean_delta={:.2} changed_px={} visible actual/golden={}/{}",
            self.rmse,
            self.max_delta,
            self.mean_delta,
            self.changed_px,
            self.visible_actual,
            self.visible_golden,
        )
    }
}

/// Diff two tightly-packed 256×256 RGBA8 buffers.
pub fn diff_images(actual: &[u8], golden: &[u8]) -> DiffReport {
    let n = (TARGET_SIZE * TARGET_SIZE) as usize;
    assert_eq!(actual.len(), n * 4, "actual buffer must be 256x256 RGBA8");
    assert_eq!(golden.len(), n * 4, "golden buffer must be 256x256 RGBA8");

    let mut sq_sum = [0f64; 4];
    let mut abs_sum = 0f64;
    let mut max_delta = 0u8;
    let mut changed_px = 0usize;
    for (a, g) in actual.chunks_exact(4).zip(golden.chunks_exact(4)) {
        let mut any = false;
        for c in 0..4 {
            let d = a[c].abs_diff(g[c]);
            if d > 0 {
                any = true;
            }
            max_delta = max_delta.max(d);
            let df = d as f64;
            sq_sum[c] += df * df;
            abs_sum += df;
        }
        if any {
            changed_px += 1;
        }
    }
    let per_channel_mse = [
        sq_sum[0] / n as f64,
        sq_sum[1] / n as f64,
        sq_sum[2] / n as f64,
        sq_sum[3] / n as f64,
    ];
    let rmse = (per_channel_mse.iter().sum::<f64>() / 4.0).sqrt();
    let mean_delta = abs_sum / (n * 4) as f64;
    DiffReport {
        width: TARGET_SIZE,
        height: TARGET_SIZE,
        per_channel_mse,
        rmse,
        max_delta,
        mean_delta,
        changed_px,
        visible_actual: visible_pixels(actual),
        visible_golden: visible_pixels(golden),
    }
}

/// Count pixels that differ from the harness background by more than [`BG_TOLERANCE`].
pub fn visible_pixels(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|p| {
            p[0].abs_diff(BG[0]) > BG_TOLERANCE
                || p[1].abs_diff(BG[1]) > BG_TOLERANCE
                || p[2].abs_diff(BG[2]) > BG_TOLERANCE
        })
        .count()
}

/// Save a 256×256 RGBA8 buffer as a PNG.
pub fn save_png(path: &Path, pixels: &[u8]) -> Result<(), image::ImageError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(
        path,
        pixels,
        TARGET_SIZE,
        TARGET_SIZE,
        image::ColorType::Rgba8,
    )
}

/// Load a PNG as a tightly-packed 256×256 RGBA8 buffer, or `None` if missing / wrong size.
pub fn load_png_rgba(path: &Path) -> Option<Vec<u8>> {
    let img = image::open(path).ok()?.to_rgba8();
    if img.width() != TARGET_SIZE || img.height() != TARGET_SIZE {
        eprintln!(
            "[regression] golden {} is {}×{}, expected {TARGET_SIZE}×{TARGET_SIZE}",
            path.display(),
            img.width(),
            img.height()
        );
        return None;
    }
    Some(img.into_raw())
}

/// Root of committed/synced golden images: `<crate>/tests/goldens`.
pub fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

/// Golden path for one effect/frame: `tests/goldens/<effect>/frame_NN.png`.
pub fn golden_path(effect: &str, frame: u32) -> PathBuf {
    goldens_dir().join(effect).join(format!("frame_{frame:02}.png"))
}

/// Write side-by-side diagnostic artifacts (`actual.png`, `golden.png`, `heatmap.png`,
/// `report.txt`) under `dir`. The heatmap is the amplified per-pixel max-channel abs-diff.
pub fn write_diff_artifacts(
    dir: &Path,
    actual: &[u8],
    golden: &[u8],
    report: &DiffReport,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let _ = save_png(&dir.join("actual.png"), actual);
    let _ = save_png(&dir.join("golden.png"), golden);

    let mut heat = vec![0u8; actual.len()];
    for ((a, g), h) in actual
        .chunks_exact(4)
        .zip(golden.chunks_exact(4))
        .zip(heat.chunks_exact_mut(4))
    {
        let d = a[0]
            .abs_diff(g[0])
            .max(a[1].abs_diff(g[1]))
            .max(a[2].abs_diff(g[2]))
            .max(a[3].abs_diff(g[3]));
        let amp = (d as u16 * 4).min(255) as u8;
        h[0] = amp;
        h[1] = amp;
        h[2] = amp;
        h[3] = 255;
    }
    let _ = save_png(&dir.join("heatmap.png"), &heat);

    std::fs::write(dir.join("report.txt"), report.summary())?;
    Ok(())
}
