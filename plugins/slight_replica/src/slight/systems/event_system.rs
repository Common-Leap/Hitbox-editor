//! Match lifecycle events — Jorge event_system facade.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

static EVENTS: LazyLock<Mutex<VecDeque<GameEvent>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(64)));
static SEEN_FINAL: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static SEEN_WIN: LazyLock<Mutex<HashSet<i32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// `(fighter_kind, EventKind) -> actions`. `fighter_kind == -1` = all kinds. This is the original
/// "Event System" facade's per-fighter-kind event→action registry ("Event map of fighter kind ",
/// "Pushing action for Event  of kind "); the registrations persist across matches.
static EVENT_REGISTRY: LazyLock<Mutex<HashMap<(i32, EventKind), Vec<EventAction>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
pub enum GameEvent {
    RealAnimationChange { boid: u32 },
    Spawn { boid: u32 },
    Win { entry_id: i32 },
    Kills { boid: u32 },
    Die { boid: u32 },
    Suicide { boid: u32 },
    Respawn { boid: u32 },
    StatusChange { boid: u32, status: i32 },
    StartsFinalAnim { boid: u32 },
    Loops { boid: u32 },
    Point { host: u16, port: u16 },
}

/// Event type (data-less) used as a registration key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EventKind {
    RealAnimationChange,
    Spawn,
    Win,
    Kills,
    Die,
    Suicide,
    Respawn,
    StatusChange,
    StartsFinalAnim,
    Loops,
    Point,
}

/// A registered reaction to an event (the original lets other mods register these per kind).
pub type EventAction = fn(&GameEvent);

impl GameEvent {
    fn event_kind(&self) -> EventKind {
        match self {
            GameEvent::RealAnimationChange { .. } => EventKind::RealAnimationChange,
            GameEvent::Spawn { .. } => EventKind::Spawn,
            GameEvent::Win { .. } => EventKind::Win,
            GameEvent::Kills { .. } => EventKind::Kills,
            GameEvent::Die { .. } => EventKind::Die,
            GameEvent::Suicide { .. } => EventKind::Suicide,
            GameEvent::Respawn { .. } => EventKind::Respawn,
            GameEvent::StatusChange { .. } => EventKind::StatusChange,
            GameEvent::StartsFinalAnim { .. } => EventKind::StartsFinalAnim,
            GameEvent::Loops { .. } => EventKind::Loops,
            GameEvent::Point { .. } => EventKind::Point,
        }
    }

    fn fighter_kind(&self) -> Option<i32> {
        let boid = match self {
            GameEvent::RealAnimationChange { boid }
            | GameEvent::Spawn { boid }
            | GameEvent::Kills { boid }
            | GameEvent::Die { boid }
            | GameEvent::Suicide { boid }
            | GameEvent::Respawn { boid }
            | GameEvent::StatusChange { boid, .. }
            | GameEvent::StartsFinalAnim { boid }
            | GameEvent::Loops { boid } => *boid,
            GameEvent::Win { .. } | GameEvent::Point { .. } => return None,
        };
        crate::slight::agents::lookup(boid).map(|a| a.kind)
    }
}

/// Register `action` to run whenever `event` fires for fighters of `fighter_kind` (`-1` = all
/// kinds). The original "Event System" facade exposes this so other mods can hook events per kind.
pub fn register_action(fighter_kind: i32, event: EventKind, action: EventAction) {
    let mut reg = EVENT_REGISTRY.lock();
    let key = (fighter_kind, event);
    if crate::slight::smash_utils::debug_logging_enabled() {
        if !reg.contains_key(&key) {
            skyline::println!(
                "[SLight] Event map of fighter kind {fighter_kind} didn't exist, now creating"
            );
        }
        skyline::println!("[SLight] Pushing action for Event {event:?} of kind {fighter_kind}");
    }
    reg.entry(key).or_default().push(action);
}

/// Run any registered per-kind actions for a fired event (all-kinds `-1` first, then the fighter's
/// specific kind).
fn run_registered_actions(event: &GameEvent) {
    let ek = event.event_kind();
    let mut to_run: Vec<EventAction> = Vec::new();
    {
        let reg = EVENT_REGISTRY.lock();
        if let Some(actions) = reg.get(&(-1, ek)) {
            to_run.extend(actions.iter().copied());
        }
        if let Some(fk) = event.fighter_kind() {
            if let Some(actions) = reg.get(&(fk, ek)) {
                to_run.extend(actions.iter().copied());
            }
        }
    }
    for action in to_run {
        action(event);
    }
}

pub fn install() {
    skyline::println!("[SLight] Installing Event System");
    crate::slight::systems::win_screen::install();
    skyline::println!("[SLight] Event system installed");
}

pub fn emit(event: GameEvent) {
    EVENTS.lock().push_back(event);
    while EVENTS.lock().len() > 128 {
        EVENTS.lock().pop_front();
    }
}

pub fn drain() -> Vec<GameEvent> {
    EVENTS.lock().drain(..).collect()
}

fn dispatch(event: GameEvent) {
    match &event {
        GameEvent::Win { entry_id } => {
            crate::slight::frame_context::set_after_win(true);
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!(
                    "[SLight] IS_AFTER_WIN_SCREEN = true, due to check is after win screen and discovered on Win entry {entry_id}"
                );
            }
        }
        GameEvent::StartsFinalAnim { boid } => {
            crate::slight::frame_context::set_after_win(true);
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!(
                    "[SLight] IS_AFTER_WIN_SCREEN = true, due to StartsFinalAnim on boid {boid}"
                );
            }
        }
        GameEvent::RealAnimationChange { boid } => {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] RealAnimationChange boid {boid}");
            }
        }
        GameEvent::Respawn { boid } => {
            // Notification-only. Scheduling a reinit here created an INFINITE feedback loop:
            // on_reinit_fighter emits Respawn → this scheduled another reinit → every frame,
            // forever, one loop added per KO (the "game slows down the more happens" bug —
            // ~10ms/frame of sequencer teardown/rebuild by mid-match). The reinit itself is
            // already performed by the init_frame poll (main_module facade) and gauge_system.
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Respawn event for boid {boid}");
            }
        }
        GameEvent::Die { boid } | GameEvent::Suicide { boid } => {
            crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                .lock()
                .invalidate_boid(*boid);
        }
        _ => {}
    }
    // Then run any per-kind actions registered through the Event System framework.
    run_registered_actions(&event);
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Event: {event:?}");
    }
}

pub fn on_frame() {
    // `win_screen`'s SD config re-read moved to the throttled tick (`slight::sd_poll`).
    for ev in drain() {
        dispatch(ev);
    }
    scan_final_animations();
}

fn scan_final_animations() {
    use smash::app::lua_bind::{MotionModule, StatusModule};
    use smash::app::sv_battle_object;

    for boid in 0..8u32 {
        unsafe {
            if !sv_battle_object::is_active(boid) || sv_battle_object::category(boid) != 0 {
                continue;
            }
            let ptr = sv_battle_object::module_accessor(boid);
            if ptr.is_null() {
                continue;
            }
            let motion = MotionModule::motion_kind(ptr);
            if crate::slight::systems::win_screen::is_final_motion(motion) {
                let mut seen = SEEN_FINAL.lock();
                if seen.insert(boid) {
                    drop(seen);
                    emit(GameEvent::StartsFinalAnim { boid });
                }
            }
            let status = StatusModule::status_kind(ptr);
            if crate::slight::systems::win_screen::is_win_status(status) {
                let entry = sv_battle_object::entry_id(boid);
                let mut seen = SEEN_WIN.lock();
                if seen.insert(entry) {
                    drop(seen);
                    emit(GameEvent::Win { entry_id: entry });
                }
            }
        }
    }
}

pub fn clear() {
    EVENTS.lock().clear();
    SEEN_FINAL.lock().clear();
    SEEN_WIN.lock().clear();
    crate::slight::systems::win_screen::clear();
}
