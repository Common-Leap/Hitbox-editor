//! Agent damage snapshots — Jorge damage_manager facade.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

use smash::app::lua_bind::DamageModule;

static SNAPSHOTS: LazyLock<Mutex<HashMap<u32, f32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INITIALIZED: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// 0x1c-byte collision record pushed by FUN_71000d210c.
#[derive(Clone, Debug)]
pub struct CollisionHitRecord {
    pub meta: u64,
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub collision_id: u32,
    pub attacker_kind: u32,
    pub defender_kind: u32,
}

// FUN_71000d210c records each hit into THREE ring buffers on the damage-manager state
// (DAT_71001e3290): +0x2b0 attacker-side, +0x2c8 article-overload (via founder), +0x2e0
// defender-side.
static COLLISION_QUEUE: LazyLock<Mutex<VecDeque<CollisionHitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));
static OVERLOAD_QUEUE: LazyLock<Mutex<VecDeque<CollisionHitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));
static DEFENDER_QUEUE: LazyLock<Mutex<VecDeque<CollisionHitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));

pub fn install() {}

pub fn init_agent(boid: u32, module_accessor: *mut smash::app::BattleObjectModuleAccessor) {
    if module_accessor.is_null() {
        return;
    }
    if !INITIALIZED.lock().insert(boid) {
        return;
    }
    let damage = unsafe { DamageModule::damage(module_accessor, 0) };
    SNAPSHOTS.lock().insert(boid, damage);
    crate::slight::systems::fighter_data_space::set_damage(boid, damage);
    skyline::println!("[SLight] Initializing fighter over facade — boid {boid} damage {damage}");
}

pub fn snapshot(boid: u32) -> Option<f32> {
    SNAPSHOTS.lock().get(&boid).copied()
}

pub fn on_init_frame() {
    if let Some(rec) = crate::slight::frame_context::current_agent() {
        if rec.category != 0 {
            return;
        }
        if !crate::slight::systems::init_frame::facade_allowed("Damage manager module", rec.boid) {
            return;
        }
        unsafe {
            let ptr = smash::app::sv_battle_object::module_accessor(rec.boid);
            if !ptr.is_null() {
                init_agent(rec.boid, ptr);
            }
        }
    }
}

/// Jorge FUN_71000d210c — push collision context into the damage queue.
pub fn on_collision_hit(ctx: &crate::slight::systems::skyline_hook::CollisionContext) {
    let attacker_kind = agent_kind(ctx.attacker_boid);
    let defender_kind = agent_kind(ctx.defender_boid);
    let rec = CollisionHitRecord {
        meta: ctx.manager,
        attacker_boid: ctx.attacker_boid,
        defender_boid: ctx.defender_boid,
        collision_id: ctx.collision_id,
        attacker_kind,
        defender_kind,
    };
    // +0x2b0 attacker-side and +0x2e0 defender-side both get the hit record.
    push_collision(&COLLISION_QUEUE, rec.clone());
    push_collision(&DEFENDER_QUEUE, rec.clone());
    // FUN_71000d210c: for an article attacker (category 6) the overload is attributed to the
    // article's FOUNDER (owner fighter) — resolved via FUN_7100107404 + FUN_71000db2b4 — not to
    // the article itself.
    let attacker_category = unsafe { smash::app::sv_battle_object::category(ctx.attacker_boid) };
    if attacker_category == 6 {
        let founder_id = unsafe { smash::app::sv_battle_object::get_founder_id(ctx.attacker_boid) };
        if let Some(owner) = crate::slight::agents::lookup_by_founder(founder_id) {
            let mut owner_rec = rec;
            owner_rec.attacker_boid = owner.boid;
            owner_rec.attacker_kind = owner.kind as u32;
            push_collision(&OVERLOAD_QUEUE, owner_rec);
        }
    }
}

fn push_collision(queue: &LazyLock<Mutex<VecDeque<CollisionHitRecord>>>, rec: CollisionHitRecord) {
    let mut q = queue.lock();
    q.push_back(rec);
    while q.len() > 64 {
        q.pop_front();
    }
}

fn agent_kind(boid: u32) -> u32 {
    crate::slight::agents::lookup(boid)
        .map(|a| a.kind as u32)
        .unwrap_or(0)
}

pub fn drain_collision_hits() -> Vec<CollisionHitRecord> {
    COLLISION_QUEUE.lock().drain(..).collect()
}

pub fn drain_overload_hits() -> Vec<CollisionHitRecord> {
    OVERLOAD_QUEUE.lock().drain(..).collect()
}

pub fn drain_defender_hits() -> Vec<CollisionHitRecord> {
    DEFENDER_QUEUE.lock().drain(..).collect()
}

pub fn clear() {
    SNAPSHOTS.lock().clear();
    INITIALIZED.lock().clear();
    COLLISION_QUEUE.lock().clear();
    OVERLOAD_QUEUE.lock().clear();
    DEFENDER_QUEUE.lock().clear();
}
