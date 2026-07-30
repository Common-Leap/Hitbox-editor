//! main_smash — FUN_71000bd7ec facade install + FUN_71000bf6ac frame dispatch.

pub mod facades;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::slight::agents::AgentRecord;
use facades::Facade;

/// Per-kind callback table (key=0 = all fighters, key=kind = specific fighter kind).
/// fight_start callbacks use offset 0x38, frame callbacks offset 0x48 in the original.
type KindCb = fn(u32);
static FIGHT_START_CBS: LazyLock<Mutex<HashMap<i32, Vec<KindCb>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FIGHTER_FRAME_CBS: LazyLock<Mutex<HashMap<i32, Vec<KindCb>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn add_fight_start_callback(kind: i32, cb: KindCb) {
    FIGHT_START_CBS.lock().entry(kind).or_default().push(cb);
}

pub fn add_fighter_frame_callback(kind: i32, cb: KindCb) {
    FIGHTER_FRAME_CBS.lock().entry(kind).or_default().push(cb);
}

fn dispatch_kind_cbs(map: &HashMap<i32, Vec<KindCb>>, boid: u32, kind: i32) {
    if let Some(cbs) = map.get(&0) {
        for &cb in cbs {
            cb(boid);
        }
    }
    if kind != 0 {
        if let Some(cbs) = map.get(&kind) {
            for &cb in cbs {
                cb(boid);
            }
        }
    }
}

static FACADES: LazyLock<Mutex<Vec<Box<dyn Facade>>>> =
    LazyLock::new(|| Mutex::new(facades::build_registry()));

fn log_facade_phase(weapon: bool, phase: &str, name: &str) {
    if !crate::slight::smash_utils::debug_logging_enabled() {
        return;
    }
    let prefix = if weapon {
        match phase {
            "frame" => "Frame of facade for weapons:",
            "post" => "Post-frame of facade for weapons:",
            _ => phase,
        }
    } else {
        match phase {
            "pre" => "Pre-frame of facade for fighters:",
            "frame" => "Frame of facade for fighters:",
            "post" => "Post-frame of facade for fighters:",
            _ => phase,
        }
    };
    skyline::println!("[SLight] {prefix} {name}");
}

fn agent_for_dispatch(rec: &AgentRecord) -> AgentRecord {
    let resolved = crate::slight::frame_context::resolve_work_boid(rec.boid);
    if resolved == rec.boid {
        return rec.clone();
    }
    crate::slight::agents::lookup(resolved).unwrap_or_else(|| {
        let mut agent = rec.clone();
        agent.boid = resolved;
        agent
    })
}

fn facade_applies_to_path(facade: &dyn Facade, weapon: bool) -> bool {
    if weapon {
        facade.weapon_frame()
    } else {
        facade.fighter_frame()
    }
}

fn global_facade_applies(facade: &dyn Facade, weapon: bool) -> bool {
    if weapon {
        facade.weapon_frame()
    } else {
        facade.fighter_frame() || (!facade.fighter_frame() && !facade.weapon_frame())
    }
}

fn skip_after_win_fighter(facade_name: &str, weapon: bool, after_win: bool) -> bool {
    if weapon || !after_win {
        return false;
    }
    facade_name == "Damage manager module" || facade_name == "Animation sequencer system"
}

fn run_facade_frame_hooks(facade: &mut dyn Facade, weapon: bool, run_pre: bool) {
    use crate::slight::systems::overload_timer::time_phase;
    let name = facade.name();
    if run_pre && !weapon {
        log_facade_phase(weapon, "pre", name);
        time_phase("Pre-frame", name, || facade.pre_frame());
    }
    log_facade_phase(weapon, "frame", name);
    time_phase("Frame", name, || facade.on_frame());
    log_facade_phase(weapon, "post", name);
    time_phase("Post-Frame", name, || facade.post_frame());
}

pub fn install_all() {
    skyline::println!("[SLight] Main install");
    skyline::println!("[SLight] Inserting systems");
    for facade in FACADES.lock().iter_mut() {
        facade.install();
    }
    late_install();
    crate::slight::systems::init_frame::install();
    skyline::println!("[SLight] Main module installed");
}

fn late_install() {
    skyline::println!("[SLight] Late install of systems");
    let mut facades = FACADES.lock();
    let start = facades.len();
    facades.extend(facades::build_late_registry());
    for facade in facades.iter_mut().skip(start) {
        facade.install();
    }
}

fn run_facade_init(facade: &mut dyn Facade, boid: u32) {
    if !crate::slight::systems::init_frame::facade_allowed(facade.name(), boid) {
        return;
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Initializing frame on facade {}", facade.name());
    }
    use crate::slight::systems::overload_timer::time_phase;
    let name = facade.name();
    crate::slight::systems::init_frame::run_pre_init(name, boid);
    time_phase("Pre-init frame", name, || facade.pre_init_frame());
    time_phase("Init frame", name, || facade.init_frame());
    time_phase("Post-init frame", name, || facade.post_init_frame());
    crate::slight::systems::init_frame::run_post_init(name, boid);
}

fn dispatch_frame(weapon: bool, after_win: bool) {
    crate::slight::systems::init_frame::on_frame();

    let target_category = if weapon { 1 } else { 0 };
    // Single registry snapshot (one lock), sorted by boid for deterministic dispatch order —
    // replaces the old 512-iteration lookup() loop (512 lock/clone cycles per frame per path).
    let mut all = crate::slight::agents::all_records();
    all.sort_by_key(|rec| rec.boid);
    let agents: Vec<AgentRecord> = all
        .into_iter()
        .filter(|rec| rec.category == target_category)
        .filter(|rec| {
            !(after_win
                && !weapon
                && crate::slight::frame_context::should_skip_fighter_frame(rec.boid))
        })
        .collect();

    let mut first_agent = true;
    for rec in &agents {
        let agent = agent_for_dispatch(rec);
        crate::slight::frame_context::set_current_agent(Some(agent.clone()));

        let mut facades = FACADES.lock();
        for facade in facades.iter_mut() {
            if facade.global_only() || !facade_applies_to_path(facade.as_ref(), weapon) {
                continue;
            }
            if skip_after_win_fighter(facade.name(), weapon, after_win) {
                continue;
            }

            if facade.per_agent_init() {
                run_facade_init(facade.as_mut(), agent.boid);
            }

            let run_hooks = !facade.once_per_frame() || first_agent;
            if run_hooks {
                run_facade_frame_hooks(facade.as_mut(), weapon, true);
            }
        }
        drop(facades);

        // Per-kind callback dispatch (original FUN_71000bf6ac lines 1034-1150, offset 0x48).
        if !weapon {
            let kind = agent.kind;
            let map = FIGHTER_FRAME_CBS.lock();
            dispatch_kind_cbs(&map, agent.boid, kind);
        }

        first_agent = false;
    }
    crate::slight::frame_context::set_current_agent(None);

    {
        let mut facades = FACADES.lock();
        for facade in facades.iter_mut() {
            if !facade.global_only() || !global_facade_applies(facade.as_ref(), weapon) {
                continue;
            }
            if skip_after_win_fighter(facade.name(), weapon, after_win) {
                continue;
            }
            run_facade_frame_hooks(facade.as_mut(), weapon, !weapon);
        }
    }

    if !weapon {
        let mut facades = FACADES.lock();
        for facade in facades.iter_mut() {
            if facade.fighter_frame() || facade.weapon_frame() || facade.global_only() {
                continue;
            }
            facade.on_frame();
        }
    }
}

pub fn on_fighter_frame() {
    // The `activate.txt` / `deactivate.txt` one-shot trigger poll moved to the throttled SD
    // tick (`slight::sd_poll`) — it is a delete-if-exists on two paths that normally do not
    // exist, and one per frame is exactly the kind of failing lookup Windows charges for.
    crate::slight::systems::guard_stance::begin_fighter_frame();
    let after_win = crate::slight::frame_context::is_after_win();
    if !after_win {
        crate::slight::frame_context::begin_fighter_frame();
    }
    dispatch_frame(false, after_win);
}

/// Jorge FUN_71000bb610 — first active fighter tick after match entry.
pub fn on_fight_start() {
    facades::fight_start::on_fight_start();
    // Per-kind fight-start callbacks (original FUN_71000bb610 lines 898-977, offset 0x38).
    let map = FIGHT_START_CBS.lock();
    if !map.is_empty() {
        let mut recs = crate::slight::agents::all_records();
        recs.sort_by_key(|rec| rec.boid);
        for rec in recs.iter().filter(|rec| rec.category == 0) {
            dispatch_kind_cbs(&map, rec.boid, rec.kind);
        }
    }
}

pub fn on_weapon_frame() {
    if crate::slight::frame_context::is_after_win() {
        return;
    }
    crate::slight::frame_context::begin_weapon_frame();
    dispatch_frame(true, false);
}

pub fn uninstall() {
    crate::slight::systems::main_module::on_uninstall();
    crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .clear();
    crate::slight::effect_viewer::kinds::clear();
    crate::slight::effect_viewer::frame_tick::clear_all();
    crate::slight::agents::clear_all();
}
