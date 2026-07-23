//! Motion/status init gate + per-facade pre/init/post chains — Jorge FUN_71000ed20c.

use parking_lot::Mutex;
use smash::app::lua_bind::{MotionModule, StatusModule};
use smash::app::sv_battle_object;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitAction {
    None,
    FirstInit,
    Reinit,
}

#[derive(Clone, Copy, Debug, Default)]
struct FrameState {
    motion: u64,
    status: i32,
    initialized: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct FacadeGate {
    motion: u64,
    status: i32,
}

type InitHook = fn(u32);

static STATES: LazyLock<Mutex<HashMap<u32, FrameState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FACADE_GATES: LazyLock<Mutex<HashMap<&'static str, FacadeGate>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PRE_INIT_HOOKS: LazyLock<Mutex<HashMap<&'static str, Vec<InitHook>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static POST_INIT_HOOKS: LazyLock<Mutex<HashMap<&'static str, Vec<InitHook>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn read_live(boid: u32) -> Option<(u64, i32)> {
    unsafe {
        if !sv_battle_object::is_active(boid) || sv_battle_object::is_null(boid) {
            return None;
        }
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return None;
        }
        Some((
            MotionModule::motion_kind(ptr),
            StatusModule::status_kind(ptr),
        ))
    }
}

/// Returns whether main_module should run first init or reinit for this agent.
pub fn poll(boid: u32) -> InitAction {
    let Some((motion, status)) = read_live(boid) else {
        return InitAction::None;
    };
    let mut states = STATES.lock();
    let entry = states.entry(boid).or_default();
    if !entry.initialized {
        entry.motion = motion;
        entry.status = status;
        entry.initialized = true;
        return InitAction::FirstInit;
    }
    if entry.motion != motion || entry.status != status {
        entry.motion = motion;
        entry.status = status;
        return InitAction::Reinit;
    }
    InitAction::None
}

pub fn install() {
    load_defaults();
    skyline::println!("[SLight] Init frame gates ready");
}

pub fn on_frame() {}

fn load_defaults() {
    // Jorge FUN_71000ed20c — per-facade motion/status rows (0 = match any).
    set_facade_gate("Animation sequencer system", 0, 0);
    set_facade_gate("Damage manager module", 0, 0);
    set_facade_gate("Main module", 0, 0);
    set_facade_gate("Effect viewer", 0, 0);
}

/// Jorge FUN_71000ed20c per-facade motion/status table entry (offset 0x3e motion, 0x3b/0x1dc status).
pub fn set_facade_gate(facade: &'static str, motion: u64, status: i32) {
    FACADE_GATES
        .lock()
        .insert(facade, FacadeGate { motion, status });
}

pub fn register_pre_init(facade: &'static str, hook: InitHook) {
    PRE_INIT_HOOKS.lock().entry(facade).or_default().push(hook);
}

pub fn register_post_init(facade: &'static str, hook: InitHook) {
    POST_INIT_HOOKS.lock().entry(facade).or_default().push(hook);
}

pub fn facade_allowed(facade: &str, boid: u32) -> bool {
    let gate = FACADE_GATES.lock().get(facade).copied().unwrap_or_default();
    matches_gate(boid, gate.motion, gate.status)
}

/// Jorge table gate — when expected motion is unset (0), match on status only.
fn matches_gate(boid: u32, expected_motion: u64, expected_status: i32) -> bool {
    let Some((motion, status)) = read_live(boid) else {
        return false;
    };
    if expected_motion == 0 {
        expected_status == 0 || status == expected_status
    } else if motion != expected_motion {
        false
    } else {
        expected_status == 0 || status == expected_status
    }
}

fn run_hooks(hooks: &[InitHook], boid: u32) {
    for hook in hooks {
        hook(boid);
    }
}

/// Run registered pre-init hooks for a facade (FUN_71000ed20c offset 0x2c chain).
pub fn run_pre_init(facade: &str, boid: u32) {
    let hooks = PRE_INIT_HOOKS
        .lock()
        .get(facade)
        .cloned()
        .unwrap_or_default();
    run_hooks(&hooks, boid);
}

/// Run registered post-init hooks for a facade (FUN_71000ed20c offset 0x35/0x40 chain).
pub fn run_post_init(facade: &str, boid: u32) {
    let hooks = POST_INIT_HOOKS
        .lock()
        .get(facade)
        .cloned()
        .unwrap_or_default();
    run_hooks(&hooks, boid);
}

pub fn clear_boid(boid: u32) {
    STATES.lock().remove(&boid);
}

pub fn clear() {
    STATES.lock().clear();
    FACADE_GATES.lock().clear();
    PRE_INIT_HOOKS.lock().clear();
    POST_INIT_HOOKS.lock().clear();
}
