// Visionary: character/circle preview, authored effect editing, and live in-game rendering.
mod acmd;
mod app;
mod data;
mod eff_editor;
mod eff_export;
mod effect_pool;
mod effects;
mod game_link;
mod mod_project;
mod param_labels;
mod renderer;
mod scratch_dirs;
mod texture_import;

use ssbh_wgpu;

fn main() -> anyhow::Result<()> {
    // Force Vulkan backend on Linux — avoids silent failures with RADV + wgpu auto-detection
    std::env::set_var("WGPU_BACKEND", "vulkan");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Visionary")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew {
                instance_descriptor: wgpu::InstanceDescriptor::new_without_display_handle(),
                display_handle: None,
                power_preference: wgpu::PowerPreference::HighPerformance,
                native_adapter_selector: None,
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
                        label: Some("visionary"),
                        required_features: features,
                        required_limits: adapter.limits(),
                        memory_hints: wgpu::MemoryHints::default(),
                        ..Default::default()
                    }
                }),
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "Visionary",
        options,
        Box::new(|cc| Ok(Box::new(app::VisionaryApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

pub(crate) fn debug_enabled() -> bool {
    std::env::var_os("VISIONARY_DEBUG").is_some()
}
