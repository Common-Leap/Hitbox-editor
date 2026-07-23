//! Article / weapon spawn notifications — Jorge article_notifier facade.

use parking_lot::Mutex;
use smash::app::lua_bind::WorkModule;
use smash::app::sv_battle_object;
use std::collections::HashSet;
use std::sync::LazyLock;

const WORK_OWNER_VAR: i32 = 0x10000000;

static LIVE: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn install() {
    skyline::println!("[SLight] Article notifier ready");
}

/// Jorge article_notifier — resolve Pokemon Trainer / nested article owner slot.
fn weapon_owner_boid(boid: u32) -> u32 {
    let resolved = crate::slight::frame_context::resolve_work_boid(boid);
    unsafe {
        let founder = sv_battle_object::get_founder_id(boid);
        if let Some(owner) = crate::slight::agents::lookup_by_founder(founder) {
            return owner.boid;
        }
        let ptr = sv_battle_object::module_accessor(boid);
        if !ptr.is_null() {
            let owner = WorkModule::get_int(ptr, WORK_OWNER_VAR) as u32;
            if sv_battle_object::is_active(owner) {
                return owner; // full battle object id
            }
        }
    }
    resolved
}

pub fn on_weapon_frame() {
    // Weapons enter the registry via their smashline line callbacks / effect-spawn hooks;
    // full battle object ids can't be enumerated by scanning 0..512.
    let current: HashSet<u32> = crate::slight::agents::all_records()
        .into_iter()
        .filter(|rec| rec.category == 1)
        .map(|rec| rec.boid)
        .collect();

    let mut live = LIVE.lock();
    for &gone in live.difference(&current) {
        crate::slight::systems::main_module::on_reset_weapon(gone);
        crate::slight::systems::init_frame::clear_boid(gone);
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Article notifier: weapon {gone} despawned");
        }
    }

    for &boid in current.difference(&*live) {
        let owner = weapon_owner_boid(boid);
        if crate::slight::systems::init_frame::facade_allowed("Main module", owner) {
            crate::slight::systems::main_module::on_init_weapon(boid);
            crate::slight::systems::event_system::emit(
                crate::slight::systems::event_system::GameEvent::Spawn { boid },
            );
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!(
                    "[SLight] Article notifier: new weapon boid {boid} (owner {owner})"
                );
            }
        }
    }

    *live = current;
}

pub fn clear() {
    LIVE.lock().clear();
}
