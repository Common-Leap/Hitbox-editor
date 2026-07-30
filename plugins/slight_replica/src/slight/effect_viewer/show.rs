//! RPM show/hide — FUN_710005b84c / FUN_71000e82a8.
//!
//! Both logs here are gated. `skyline::println!` is not free when nobody is listening: it is a
//! `format!` followed by `skyline_tcp_send_raw`, i.e. a synchronous socket write on the game
//! thread. These two run once per effect shown and once per effect hidden — dozens of times a
//! frame in a busy match — which is a very different cost from the install-time logging the
//! rest of the plugin uses `println!` for. `diag.rs` already notes that emulator log buffers
//! can freeze on entering gameplay; these two sites had simply been missed.

use super::effect_data::EffectData;

pub fn show_effect(id: u64, name: &str, data: &EffectData) {
    crate::rust_extender::debuggable_server::notify_effect(id, name, data);
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Showing effect: #{id} {name}");
    }
}

pub fn hide_effect(id: u64, rpm_notified: bool) {
    if rpm_notified {
        crate::rust_extender::debuggable_server::remove_effect(id);
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Hiding effect #{id}");
    }
}

pub fn queue_show(id: u64) {
    crate::slight::pending::queue_notify(id);
}

pub fn queue_hide(id: u64, notified: bool) {
    crate::slight::pending::queue_remove(id, notified);
}
