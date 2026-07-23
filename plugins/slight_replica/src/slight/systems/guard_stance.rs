//! Win-screen gating + SD triggers — Jorge guard_stance facade (FUN_71000bf6ac).

pub fn install() {
    skyline::println!("[SLight] Guard stance ready");
}

/// Jorge fighter-frame entry — consume `activate.txt` / `deactivate.txt` one-shot triggers.
pub fn begin_fighter_frame() {
    crate::slight::frame_context::poll_after_win_triggers();
}

pub fn on_dodge_stance(_boid: u32) {
    skyline::println!("[SLight] Dodge stance not implemented");
}

pub fn clear() {}
