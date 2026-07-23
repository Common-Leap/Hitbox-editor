//! FrameChecker / match timing — Jorge time_counting facade.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

use smash::app::lua_bind::{SlowModule, StopModule};
use smash::app::sv_battle_object;

static AGENT_CHECKERS: LazyLock<Mutex<HashMap<u32, FrameChecker>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, Default)]
pub struct FrameChecker {
    pub real_range: f32,
    pub count: u32,
    pub checked_first_frame: bool,
    pub passed_a_frame: bool,
    pub stop_treatment: bool,
    pub slow_treatment: bool,
}

pub fn install() {}

pub fn checker_for(boid: u32) -> FrameChecker {
    AGENT_CHECKERS.lock().entry(boid).or_default().clone()
}

pub fn on_pre_frame() {
    let frame = crate::slight::frame_context::match_frame();
    if frame == 1 && crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Time counting — first fighter frame");
    }
}

pub fn on_post_frame() {
    let Some(rec) = crate::slight::frame_context::current_agent() else {
        return;
    };
    // Classify the counting "situation" from the game's own stop/slow modules: hitstop
    // (StopModule::is_stop) freezes the frame (stop_treatment); slow-mo (SlowModule::is_slow)
    // is a slow frame (slow_treatment). The original counts "real" frames net of these.
    let (stop, slow) = unsafe {
        let ptr = sv_battle_object::module_accessor(rec.boid);
        if ptr.is_null() {
            return;
        }
        (StopModule::is_stop(ptr), SlowModule::is_slow(ptr))
    };

    let mut map = AGENT_CHECKERS.lock();
    let chk = map.entry(rec.boid).or_default();
    chk.stop_treatment = stop;
    chk.slow_treatment = slow;
    if !chk.checked_first_frame {
        chk.checked_first_frame = true;
        return;
    }
    if stop {
        return;
    }
    let step = if slow { 0.5 } else { 1.0 };
    chk.count = chk.count.saturating_add(1);
    chk.real_range += step;
    chk.passed_a_frame = true;
}

pub fn clear() {
    AGENT_CHECKERS.lock().clear();
}
