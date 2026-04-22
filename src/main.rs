mod app;
mod data;
mod acmd;
mod renderer;
mod effects;
mod particle_renderer;
mod particle_renderer_bnsh;
mod shader_cache;
mod batch_loader;
mod bnsh_ffi;
mod bnsh_reflection;
mod effect_browser;
mod shader_integration;
mod bnsh_shader_integration;
mod spirv_to_wgsl;

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
mod end_to_end_integration_test;

use ssbh_wgpu;

fn main() -> anyhow::Result<()> {
    // Force Vulkan backend on Linux — avoids silent failures with RADV + wgpu auto-detection
    std::env::set_var("WGPU_BACKEND", "vulkan");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SSBU Hitbox Editor")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew {
                device_descriptor: std::sync::Arc::new(|adapter| {
                    // Only request ssbh_wgpu features if the adapter supports them.
                    // This prevents a blank window on GPUs/drivers that lack BC compression etc.
                    let supported = adapter.features();
                    let wanted = ssbh_wgpu::REQUIRED_FEATURES;
                    let features = if supported.contains(wanted) {
                        wanted
                    } else {
                        eprintln!(
                            "Warning: GPU does not support all ssbh_wgpu features. \
                             Missing: {:?}. 3D rendering will be disabled.",
                            wanted - supported
                        );
                        wgpu::Features::empty()
                    };
                    wgpu::DeviceDescriptor {
                        label: Some("hitbox_editor"),
                        required_features: features,
                        required_limits: wgpu::Limits::default(),
                        memory_hints: wgpu::MemoryHints::default(),
                        ..Default::default()
                    }
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "SSBU Hitbox Editor",
        options,
        Box::new(|cc| {
            Ok(Box::new(app::HitboxEditorApp::new(cc)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
