//! Collision / attack hit log facade.
//!
//! Visionary does not patch the game's collision dispatcher. Both the manual body trampoline and
//! the generated binding detour have stopped Eden during ordinary attacks, while the recorded hit
//! queues have no runtime consumer. ACMD capture observes ATTACK commands directly and does not
//! need a post-collision hook.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::LazyLock;

static INSTALLED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static HITS: LazyLock<Mutex<VecDeque<HitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));

#[derive(Clone, Debug)]
pub struct HitRecord {
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub tick: u64,
}

/// Retained for the damage-manager facade's inert collision-record API. No game hook constructs
/// this context during normal operation.
#[derive(Clone, Debug)]
pub struct CollisionContext {
    pub manager: u64,
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub damage: f32,
    pub collision_id: u32,
    pub flags: u32,
    pub tick: u64,
}

pub fn install() {
    let mut installed = INSTALLED.lock();
    if *installed {
        return;
    }

    crate::slight::diag::note("COLLISION_HOOK mode=disabled reason=unused-and-unsafe");
    skyline::println!("[SLight] Skyline Hook: collision detours disabled");
    *installed = true;
}

/// Collision hit notify + damage queues, retained for callers that already own a validated
/// collision context. The plugin does not detour game collision dispatch to call this function.
pub fn notify_log_event_collision_hit(ctx: &CollisionContext) {
    crate::slight::diag::note_collision();
    record_hit(ctx.attacker_boid, ctx.defender_boid, ctx.tick);
    crate::slight::systems::damage_manager::on_collision_hit(ctx);
    if crate::slight::smash_utils::debug_logging_enabled() {
        crate::slight::diag::note(format!(
            "COLLISION attacker={} defender={} damage={:.2} id={} flags={}",
            ctx.attacker_boid, ctx.defender_boid, ctx.damage, ctx.collision_id, ctx.flags
        ));
    }
}

/// Jorge @ 71000dfda8 — after-win attack log hook entry (SD debug path).
pub fn handle_attack(param: u64) {
    if crate::slight::frame_context::is_after_win()
        && crate::slight::smash_utils::debug_logging_enabled()
    {
        skyline::println!("[SLight] handle_attack param={param:#x}");
    }
}

pub fn is_installed() -> bool {
    *INSTALLED.lock()
}

pub fn record_hit(attacker: u32, defender: u32, tick: u64) {
    let mut q = HITS.lock();
    q.push_back(HitRecord {
        attacker_boid: attacker,
        defender_boid: defender,
        tick,
    });
    while q.len() > 64 {
        q.pop_front();
    }
}

pub fn drain_hits() -> Vec<HitRecord> {
    HITS.lock().drain(..).collect()
}

pub fn clear() {
    HITS.lock().clear();
}
