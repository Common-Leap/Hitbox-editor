#![feature(proc_macro_hygiene)]
// The replica intentionally retains framework-compatible facade APIs that this standalone plugin
// does not call directly. They are documented and audited in AUDIT.md.
#![allow(dead_code)]

mod rust_extender;
mod slight;

pub const PLUGIN_VERSION: &str = "0.1.0-slight-replica";

/// Jorge entry — `smashline_install` @ 71000013d0.
#[skyline::main(name = "_effect_viewer")]
pub fn smashline_install() {
    skyline::println!("[SLight] Start preinstall");
    slight::smash_utils::ensure_slight_dirs();
    let _ = std::fs::write(
        format!("{}/effect_viewer_boot.txt", slight::smash_utils::ERROR_LOGS),
        format!("lib_effect_viewer v{PLUGIN_VERSION}\n"),
    );

    slight::effect_viewer::install_hooks();
    slight::agent_extender::install();
    slight::main_smash::install_all();

    skyline::println!(
        "[SLight] Mod powered using the SLight framework by Jorge Rico Vivas (replica v{PLUGIN_VERSION})"
    );
}
