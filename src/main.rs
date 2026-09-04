// Visionary: character/circle preview, authored effect editing, and live in-game rendering.
mod acmd;
mod acmd_src;
mod acmd_verify;
mod app;
mod app_icon;
mod credits;
mod data;
mod eff_attrs;
mod eff_editor;
mod eff_export;
mod eff_subsections;
mod effect_pool;
mod effects;
mod game_link;
mod mod_export;
mod mod_project;
mod move_kinds;
mod param_labels;
mod renderer;
mod plugin_deploy;
mod roster;
mod scratch_dirs;
mod texture_import;
#[cfg(target_os = "linux")]
mod wayland_icon;

fn main() -> anyhow::Result<()> {
    // Best-effort auto-deploy the Skyline plugin to the local Eden install so the
    // user only has to run Eden + Visionary to test. This is non-blocking and
    // never fails the desktop launch — check stderr for "[visionary] Plugin auto-deploy".
    plugin_deploy::spawn_background_check();

    // RADV can fail silently during wgpu's Linux auto-detection. Keep the working Vulkan
    // default there, while respecting an explicit user choice and leaving Windows to select
    // DirectX/Vulkan normally.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        std::env::set_var("WGPU_BACKEND", "vulkan");
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(app_icon::APP_ID)
            .with_icon(app_icon::viewport_icon())
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
