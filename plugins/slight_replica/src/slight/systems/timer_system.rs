//! Deferred frame timers — Jorge timer_system facade.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static TIMERS: LazyLock<Mutex<HashMap<u64, Timer>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A registered action a timer triggers on expiry (the original "Triggering actions").
pub type TimerAction = fn(u32);

#[derive(Clone)]
pub struct Timer {
    pub id: u64,
    pub boid: u32,
    pub label: String,
    pub remaining: i32,
    pub repeat: bool,
    /// Action run when the timer fires (the original "Triggering actions" / "Triggering a checker").
    pub action: Option<TimerAction>,
}

pub fn install() {}

pub fn schedule(boid: u32, label: &str, frames: i32, repeat: bool) -> u64 {
    schedule_action(boid, label, frames, repeat, None)
}

/// Schedule a timer that runs `action(boid)` when it fires.
pub fn schedule_action(
    boid: u32,
    label: &str,
    frames: i32,
    repeat: bool,
    action: Option<TimerAction>,
) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    TIMERS.lock().insert(
        id,
        Timer {
            id,
            boid,
            label: label.into(),
            remaining: frames,
            repeat,
            action,
        },
    );
    id
}

pub fn cancel(id: u64) {
    TIMERS.lock().remove(&id);
}

pub fn on_frame() {
    let mut fired = Vec::new();
    {
        let mut map = TIMERS.lock();
        let ids: Vec<u64> = map.keys().copied().collect();
        for id in ids {
            let Some(timer) = map.get_mut(&id) else {
                continue;
            };
            timer.remaining -= 1;
            if timer.remaining <= 0 {
                fired.push(timer.clone());
                if timer.repeat {
                    timer.remaining = timer.remaining.max(1);
                } else {
                    map.remove(&id);
                }
            }
        }
    }
    for t in fired {
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Triggering actions: timer {} ({})", t.id, t.label);
        }
        // The original triggers the timer's registered action/checker on expiry.
        if let Some(action) = t.action {
            action(t.boid);
        }
    }
}

pub fn clear() {
    TIMERS.lock().clear();
}
