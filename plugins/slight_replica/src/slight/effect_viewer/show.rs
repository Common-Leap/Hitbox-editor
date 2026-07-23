//! RPM show/hide — FUN_710005b84c / FUN_71000e82a8.

use super::effect_data::EffectData;

pub fn show_effect(id: u64, name: &str, data: &EffectData) {
    crate::rust_extender::debuggable_server::notify_effect(id, name, data);
    skyline::println!("[SLight] Showing effect: #{id} {name}");
}

pub fn hide_effect(id: u64, rpm_notified: bool) {
    if rpm_notified {
        crate::rust_extender::debuggable_server::remove_effect(id);
    }
    skyline::println!("[SLight] Hiding effect #{id}");
}

pub fn queue_show(id: u64) {
    crate::slight::pending::queue_notify(id);
}

pub fn queue_hide(id: u64, notified: bool) {
    crate::slight::pending::queue_remove(id, notified);
}
