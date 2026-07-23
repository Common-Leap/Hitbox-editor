//! Match / frame globals — Jorge DAT_71001e3238, DAT_71001e3290 agent context.

use parking_lot::Mutex;
use smash::app::lua_bind::WorkModule;
use smash::app::sv_battle_object;
use smash::app::utility;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::LazyLock;

use super::agents::AgentRecord;

const INVALID_FOUNDER: i32 = 0x50000000;
const WORK_BOID_VAR: i32 = 0x10000000;

static MATCH_FRAME: AtomicU32 = AtomicU32::new(0);
static WEAPON_FRAME: AtomicU32 = AtomicU32::new(0);
static FIGHT_ACTIVE: AtomicBool = AtomicBool::new(false);
static AFTER_WIN: AtomicBool = AtomicBool::new(false);
static MATCH_TICKS: AtomicU64 = AtomicU64::new(0);

static CURRENT_AGENT: LazyLock<Mutex<Option<AgentRecord>>> = LazyLock::new(|| Mutex::new(None));

pub fn set_fight_active(active: bool) {
    FIGHT_ACTIVE.store(active, Ordering::Relaxed);
    if active {
        clear_after_win("fight start");
    }
}

pub fn is_fight_active() -> bool {
    FIGHT_ACTIVE.load(Ordering::Relaxed)
}

pub fn set_after_win(v: bool) {
    AFTER_WIN.store(v, Ordering::Relaxed);
}

pub fn is_after_win() -> bool {
    AFTER_WIN.load(Ordering::Relaxed)
}

pub fn clear_after_win(reason: &str) {
    if AFTER_WIN.swap(false, Ordering::Relaxed) {
        if reason == "fight start" {
            skyline::println!("[SLight] IS_AFTER_WIN_SCREEN = false, due to fight start");
        } else {
            skyline::println!("[SLight] IS_AFTER_WIN_SCREEN = false, due to {reason}");
        }
    }
}

fn mark_after_win(reason: &str) {
    AFTER_WIN.store(true, Ordering::Relaxed);
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] IS_AFTER_WIN_SCREEN = true, due to {reason}");
    }
}

/// Jorge FUN_71000bf6ac — one-shot SD triggers via FUN_7100124b24 (delete-if-exists).
pub fn poll_after_win_triggers() {
    if is_after_win() {
        if crate::slight::smash_utils::consume_sd_trigger(
            crate::slight::smash_utils::DEBUG_DEACTIVATE,
        ) {
            clear_after_win("deactivate trigger");
        }
    } else if crate::slight::smash_utils::consume_sd_trigger(
        crate::slight::smash_utils::DEBUG_ACTIVATE,
    ) {
        mark_after_win("activate trigger");
    }
}

/// Jorge FUN_7100107404 — resolve owner boid from WorkModule when nested.
pub fn resolve_work_boid(boid: u32) -> u32 {
    unsafe {
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return boid;
        }
        // Full battle object ids throughout — sv_battle_object::* take the full id.
        let id = (*ptr).battle_object_id;
        let mut resolved = id;
        if id >> 0x1c != 0 {
            let owner = WorkModule::get_int(ptr, WORK_BOID_VAR) as u32;
            if sv_battle_object::is_active(owner) {
                resolved = owner;
            }
        }
        resolved
    }
}

/// Jorge FUN_71000be114 — skip fighter init/frame when on win screen article.
pub fn should_skip_fighter_frame(boid: u32) -> bool {
    unsafe {
        let founder = sv_battle_object::get_founder_id(boid);
        if founder == INVALID_FOUNDER {
            return true;
        }
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return true;
        }
        let kind = utility::get_kind(&mut *ptr);
        if kind == -1 {
            return true;
        }
        let resolved = resolve_work_boid(boid);
        if resolved >= 9 {
            return true;
        }
        if sv_battle_object::category(boid) == 6 {
            return super::agents::lookup_by_founder(founder).is_none();
        }
        false
    }
}

pub fn should_skip_agent_init(boid: u32) -> bool {
    should_skip_fighter_frame(boid) && !is_after_win()
}

pub fn begin_fighter_frame() {
    MATCH_FRAME.fetch_add(1, Ordering::Relaxed);
    MATCH_TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn begin_weapon_frame() {
    WEAPON_FRAME.fetch_add(1, Ordering::Relaxed);
}

pub fn match_frame() -> u32 {
    MATCH_FRAME.load(Ordering::Relaxed)
}

pub fn weapon_frame() -> u32 {
    WEAPON_FRAME.load(Ordering::Relaxed)
}

pub fn match_ticks() -> u64 {
    MATCH_TICKS.load(Ordering::Relaxed)
}

pub fn set_current_agent(agent: Option<AgentRecord>) {
    *CURRENT_AGENT.lock() = agent;
}

pub fn current_agent() -> Option<AgentRecord> {
    CURRENT_AGENT.lock().clone()
}

pub fn for_each_live_agent(mut f: impl FnMut(&AgentRecord)) {
    let mut recs = super::agents::all_records();
    recs.sort_by_key(|rec| rec.boid);
    for rec in recs {
        set_current_agent(Some(rec.clone()));
        f(&rec);
    }
    set_current_agent(None);
}

pub fn reset() {
    MATCH_FRAME.store(0, Ordering::Relaxed);
    WEAPON_FRAME.store(0, Ordering::Relaxed);
    MATCH_TICKS.store(0, Ordering::Relaxed);
    FIGHT_ACTIVE.store(false, Ordering::Relaxed);
    clear_after_win("uninstall");
    set_current_agent(None);
}
