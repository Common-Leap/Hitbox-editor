//! Per-effect frame tick — Jorge FUN_71000d33ac FrameChecker.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Debug)]
struct FrameState {
    real_range: (f32, f32),
    counter: f32,
    checked_first: bool,
    passed_frame: bool,
    stop_treatment: bool,
    slow_treatment: bool,
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            real_range: (0.0, f32::MAX),
            counter: 0.0,
            checked_first: false,
            passed_frame: false,
            stop_treatment: false,
            slow_treatment: false,
        }
    }
}

static FRAME_STATES: LazyLock<Mutex<HashMap<u64, FrameState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn set_real_range(effect_id: u64, min: f32, max: f32) {
    FRAME_STATES.lock().entry(effect_id).or_default().real_range = (min, max);
}

pub fn set_stop_treatment(effect_id: u64, stop: bool) {
    if let Some(s) = FRAME_STATES.lock().get_mut(&effect_id) {
        s.stop_treatment = stop;
    }
}

pub fn set_slow_treatment(effect_id: u64, slow: bool) {
    if let Some(s) = FRAME_STATES.lock().get_mut(&effect_id) {
        s.slow_treatment = slow;
    }
}

pub fn counter(effect_id: u64) -> f32 {
    FRAME_STATES
        .lock()
        .get(&effect_id)
        .map(|s| s.counter)
        .unwrap_or(0.0)
}

pub fn tick_effect(effect_id: u64, should_advance: bool) {
    let mut states = FRAME_STATES.lock();
    let state = states.entry(effect_id).or_default();
    if !state.checked_first {
        state.checked_first = true;
        state.counter = state.real_range.0;
        return;
    }
    if state.stop_treatment {
        return;
    }
    if should_advance {
        let step = if state.slow_treatment { 0.5 } else { 1.0 };
        state.counter += step;
        state.passed_frame = true;
        if state.counter < state.real_range.0 {
            state.counter = state.real_range.0;
        }
        if state.real_range.1.is_finite() && state.counter >= state.real_range.1 {
            state.counter = state.real_range.1;
            state.stop_treatment = true;
        }
    }
}

pub fn clear_all() {
    FRAME_STATES.lock().clear();
}
