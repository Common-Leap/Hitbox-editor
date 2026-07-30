//! Win-screen gating + SD triggers — Jorge guard_stance facade (FUN_71000bf6ac).

pub fn install() {
    skyline::println!("[SLight] Guard stance ready");
}

/// Jorge fighter-frame entry.
///
/// This used to consume the `activate.txt` / `deactivate.txt` one-shot triggers here, on every
/// fighter frame. That poll now runs on the throttled SD tick (`slight::sd_poll`) — it was two
/// failing filesystem deletes per frame, which Linux absorbs and Windows does not.
pub fn begin_fighter_frame() {}

pub fn on_dodge_stance(_boid: u32) {
    skyline::println!("[SLight] Dodge stance not implemented");
}

pub fn clear() {}
