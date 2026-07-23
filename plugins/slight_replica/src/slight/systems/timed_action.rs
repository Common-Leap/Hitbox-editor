//! Deferred frame actions — Jorge timed_action facade.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::LazyLock;

static QUEUE: LazyLock<Mutex<VecDeque<TimedAction>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));

#[derive(Clone, Debug)]
pub struct TimedAction {
    pub boid: u32,
    pub label: String,
    pub frames_remaining: i32,
}

pub fn install() {
    skyline::println!("[SLight] Timed Action system ready");
}

pub fn schedule(boid: u32, label: &str, frames: i32) {
    if frames <= 0 {
        run_now(boid, label);
        return;
    }
    QUEUE.lock().push_back(TimedAction {
        boid,
        label: label.to_string(),
        frames_remaining: frames,
    });
}

fn run_now(boid: u32, label: &str) {
    match label {
        "reinit_fighter" => crate::slight::systems::main_module::on_reinit_fighter(boid),
        "reset_fighter" => crate::slight::systems::main_module::on_reset_fighter(boid),
        "reset_weapon" => crate::slight::systems::main_module::on_reset_weapon(boid),
        "reinit_weapon" => {
            crate::slight::systems::main_module::on_reset_weapon(boid);
            crate::slight::systems::main_module::on_init_weapon(boid);
        }
        other if crate::slight::smash_utils::debug_logging_enabled() => {
            skyline::println!("[SLight] Timed action {other} for boid {boid}");
        }
        _ => {}
    }
}

pub fn on_frame() {
    let mut q = QUEUE.lock();
    let mut i = 0;
    while i < q.len() {
        q[i].frames_remaining -= 1;
        if q[i].frames_remaining <= 0 {
            let action = q.remove(i).unwrap();
            drop(q);
            run_now(action.boid, &action.label);
            q = QUEUE.lock();
            continue;
        }
        i += 1;
    }
}

pub fn clear() {
    QUEUE.lock().clear();
}
