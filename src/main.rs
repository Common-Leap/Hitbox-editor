// Eff-editor: effect-file editing + in-game live preview. Effect RENDERING is deliberately
// absent on this branch (see `game-accurate-sim` for the particle renderer).
mod app;
mod data;
mod acmd;
mod renderer;
mod effects;
mod batch_loader;
mod effect_browser;
mod effect_converter;
mod scratch_dirs;
mod shader_registry;
mod combiner;
mod fx_env;
pub(crate) use fx_env::{fx_debug_enabled, fx_native_fs_enabled, fx_native_vs_pos_enabled};
mod sphere_volume_tables;

use ssbh_wgpu;

fn main() -> anyhow::Result<()> {
    // Force Vulkan backend on Linux — avoids silent failures with RADV + wgpu auto-detection
    std::env::set_var("WGPU_BACKEND", "vulkan");
    // Native FS (NVN colour chain + texture enhance) is the default.
    // Set FX_PATCHED_FS=1 or FX_NATIVE_FS=0 for legacy patch_fragment_wgsl.

    scratch_dirs::dev_refresh_storage_on_startup();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SSBU Hitbox Editor")
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
                        label: Some("hitbox_editor"),
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
        "SSBU Hitbox Editor",
        options,
        Box::new(|cc| {
            Ok(Box::new(app::HitboxEditorApp::new(cc)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
