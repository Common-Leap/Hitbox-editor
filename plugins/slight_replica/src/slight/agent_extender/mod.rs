//! agent_extender — real smashline-2 callback registration.
//!
//! The original `smashline_install` (@0x71000013d0) registered five smashline-1
//! per-agent callbacks against the external `smashline_hook` plugin:
//!   add_fighter_frame_callback(FUN_71000bf6ac)   -> per-fighter, every frame
//!   add_weapon_frame_callback(FUN_71000c1c58)    -> per-weapon, every frame
//!   add_fighter_init_callback(FUN_71000bb610)    -> per-fighter, on init
//!   add_agent_init_callback(FUN_71000be1cc)      -> per-agent, on init
//!   add_fighter_reset_callback(FUN_71000bf23c)   -> per-fighter, on RESET
//!
//! Dispatch is done with DIRECT skyline hooks on the game's line-system functions — the same
//! mechanism smashline 1 used. An earlier smashline-2 port used the global (None-agent) callback
//! API (`install_state_callback`/`install_line_callback`), but a None-agent callback makes
//! smashline wrap every agent and `panic!("failed to get original scripts")` on agents it never
//! relocated (`create_agent.rs`), crashing ~9s into boot. Hooking the functions directly avoids
//! smashline's agent-wrapping entirely:
//!   sys_line_system_control_fighter (L2CFighterCommon) -> per-fighter, every frame
//!   sys_line_system_control         (L2CFighterBase)   -> per-weapon/agent, every frame
//!   L2CFighter{Common,Base}_RESET                       -> per-fighter/agent reset
//! Init is handled lazily on first frame (idempotent via `agents::has_initialized`).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use parking_lot::Mutex;
use std::sync::LazyLock;

use smash::app::sv_system;
use smash::lua2cpp::L2CFighterBase;
use smash::phx::Hash40;
use smashline::{api, StatusLine};

use crate::slight::agents;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static FIGHT_STARTED: AtomicBool = AtomicBool::new(false);
/// Live fighter (category 0) count, tracked via Initialize/Finalize. When it returns
/// to zero the match has ended → run SLight teardown (replaces the old polling).
static LIVE_FIGHTERS: AtomicI64 = AtomicI64::new(0);

/// Boids whose `Main` line callback has fired since the last frame edge. A repeat means
/// the per-frame status loop has wrapped → a new game frame began. This drives the
/// once-per-frame batch dispatch off smashline's per-agent callbacks.
static FRAME_SEEN: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn install() {
    if INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    // Dispatch via smashline-2 PER-AGENT line callbacks — the modern equivalent of the original's
    // add_fighter_frame_callback / add_weapon_frame_callback. `StatusLine::Main` runs every frame
    // for the agent's current status, so it is the per-frame entry point that drives the whole
    // SLight engine (reconcile + tick + RPM flush + edit-apply via run_one_frame).
    //
    // We register PER-CATEGORY (Some("fighter") / Some("weapon")), NOT global `None`. A `None`
    // callback makes smashline wrap every agent and call `original_scripts` on agents it never
    // relocated → `panic!("failed to get original scripts")` (create_agent.rs) ~9s into boot. The
    // direct lua2cpp line-system hooks we tried instead resolve to NULL in 13.0.4 (those symbols
    // aren't exported), so they never fired and the per-frame engine was dead — which is why only
    // spawn-hooked req_follow effects reached RPM. Init is handled lazily on first frame
    // (idempotent via `has_initialized`); RESET has no smashline-2 event and is dropped (the old
    // direct RESET hooks were null too).
    api::install_line_callback(
        Some(Hash40::new("fighter")),
        StatusLine::Main,
        fighter_line_main as *const (),
    );
    api::install_line_callback(
        Some(Hash40::new("weapon")),
        StatusLine::Main,
        weapon_line_main as *const (),
    );

    skyline::println!("[SLight] dispatch installed (smashline-2 per-agent line callbacks)");
}

/// Per-fighter `StatusLine::Main` callback (every frame for the fighter's current status).
/// Lazy-inits the agent and drives the once-per-frame SLight engine via `handle_frame`.
unsafe extern "C" fn fighter_line_main(agent: &mut L2CFighterBase) {
    let lua_state = agent.agent.lua_state_agent;
    handle_init(lua_state);
    if let Some(boma) = boma_from_lua(lua_state) {
        crate::slight::effect_viewer::effect_reload::pump_auto_carrier(boma);
        crate::slight::effect_viewer::acmd_hooks::pump_carrier_follows(boma);
    }
    // Queued donor eff co-loads must run HERE (game thread) — load_effects from the TCP
    // thread never completes its async resource work.
    crate::slight::effect_viewer::effect_reload::pump_donor_queue();
    // Synchronous live re-read (repoint fighter eff slot at merged bytes + reparse). MUST be
    // on the game thread: it unloads/loads the effect manager and touches the res slot.
    crate::slight::effect_viewer::effect_reload::pump_force_reread();
    // Drive the co-loaded set's per-frame update so its resource state machine advances + its
    // textures get set up (inactive synthetic-handle sets are never ticked otherwise).
    crate::slight::effect_viewer::effect_reload::pump_coload_tick();
    // Live hitbox + effect-retime injection need THIS agent's lua state at its motion frame.
    crate::slight::hitbox_viewer::inject_tick(lua_state);
    crate::slight::effect_viewer::acmd_hooks::inject_tick(lua_state);
    handle_frame(lua_state);
}

/// Per-weapon/agent `StatusLine::Main` callback. Same job as the fighter one (the frame edge is
/// driven by whichever agents tick; `FRAME_SEEN` collapses them to one `run_one_frame` per frame).
unsafe extern "C" fn weapon_line_main(agent: &mut L2CFighterBase) {
    let lua_state = agent.agent.lua_state_agent;
    handle_init(lua_state);
    handle_frame(lua_state);
}

/// Reset FIGHT_STARTED so on_fight_start() re-runs. Called from the RESET path
/// (original FUN_71000bf23c clears DAT_71001e3254 unconditionally).
pub fn reset_fight_started() {
    FIGHT_STARTED.store(false, Ordering::Relaxed);
}

/// Resolve the BattleObjectModuleAccessor for a smashline agent from its lua state.
unsafe fn boma_from_lua(lua_state: u64) -> Option<*mut smash::app::BattleObjectModuleAccessor> {
    if lua_state == 0 {
        return None;
    }
    let boma = sv_system::battle_object_module_accessor(lua_state) as *mut _;
    if (boma as *const u8).is_null() {
        None
    } else {
        Some(boma)
    }
}

/// Resolve the battle object id (boid) for a smashline agent.
unsafe fn boid_from_lua(lua_state: u64) -> Option<u32> {
    let boma = boma_from_lua(lua_state)?;
    // Full battle object id — see agents::boid_from_module.
    Some((*boma).battle_object_id)
}

/// Track a fighter/weapon agent (original add_fighter_init_callback / add_agent_init_callback).
/// Called lazily from the per-frame line-system hooks the first time an agent is seen;
/// idempotent via `has_initialized`.
unsafe fn handle_init(lua_state: u64) {
    let Some(boma) = boma_from_lua(lua_state) else {
        return;
    };
    let Some(rec) = agents::upsert_module(boma) else {
        return;
    };
    let boid = rec.boid;

    if agents::has_initialized(boid) {
        return;
    }

    if rec.category == 0 {
        LIVE_FIGHTERS.fetch_add(1, Ordering::Relaxed);
        crate::slight::effect_viewer::init_fighter(boid);
    } else {
        crate::slight::systems::main_module::on_init_weapon(boid);
    }
}

/// Once-per-frame SLight dispatch. The line-system control function fires every frame for each
/// live agent; the first call of each new game frame (FRAME_SEEN wrap) drives run_one_frame.
unsafe fn handle_frame(lua_state: u64) {
    let Some(boid) = boid_from_lua(lua_state) else {
        return;
    };
    let new_frame = {
        let mut seen = FRAME_SEEN.lock();
        if seen.insert(boid) {
            false
        } else {
            seen.clear();
            seen.insert(boid);
            true
        }
    };
    if new_frame {
        run_one_frame();
    }
}

/// One game frame of SLight processing — the once-per-frame body the original ran inside
/// the first fighter's frame callback (FUN_71000bf6ac fight-start gate + facade chain).
fn run_one_frame() {
    agents::refresh_all();

    let after_win = crate::slight::frame_context::is_after_win();
    if !after_win && !FIGHT_STARTED.swap(true, Ordering::Relaxed) {
        crate::slight::main_smash::on_fight_start();
        skyline::println!(
            "[SLight] fight start fired — fight_active={}",
            crate::slight::frame_context::is_fight_active()
        );
    }

    crate::slight::main_smash::on_fighter_frame();
    crate::slight::main_smash::on_weapon_frame();

    // DIAG heartbeat: periodic stats + buffer flush (file I/O only every 30 frames). The
    // STATS `frame=` field IS the driver heartbeat — if it never appears (or freezes) during
    // a match, the smashline-2 StatusLine::Main driver is dead and every per-frame pipeline
    // (reconcile, edit-poll, pending flush) is dead with it.
    let n = ROF_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 30 == 0 {
        let tracker_count = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
            .lock()
            .count();
        let (fighters, weapons) =
            crate::slight::agents::all_records()
                .iter()
                .fold((0usize, 0usize), |(f, w), rec| {
                    if rec.category == 0 {
                        (f + 1, w)
                    } else {
                        (f, w + 1)
                    }
                });
        crate::slight::diag::note_stats(
            n,
            tracker_count,
            crate::slight::pending::depth(),
            crate::rust_extender::net::simple_server::outbox_depth(),
            fighters,
            weapons,
        );
        crate::slight::diag::flush();
    }
}

/// DIAG: count of run_one_frame invocations (the per-frame driver heartbeat).
static ROF_CALLS: AtomicU64 = AtomicU64::new(0);

/// True once the per-frame driver has ticked at least once — i.e. the game is running and
/// all boot-time init (including skyline's nn::socket::Initialize) is guaranteed complete.
pub fn driver_has_ticked() -> bool {
    ROF_CALLS.load(Ordering::Relaxed) > 0
}

fn teardown_match() {
    FIGHT_STARTED.store(false, Ordering::Relaxed);
    FRAME_SEEN.lock().clear();
    crate::slight::pending::process();
    crate::rust_extender::debuggable_server::remove_all();
    crate::slight::main_smash::uninstall();
}

// Per-frame dispatch is driven by the smashline-2 `StatusLine::Main` line callbacks registered in
// `install()` (`fighter_line_main` / `weapon_line_main`). The previous direct lua2cpp line-system
// and RESET hooks are gone: those symbols resolve to NULL in 13.0.4 so the hooks never fired.
