//! Fight start — Jorge late install facade #1 (FUN_71000bd7ec / FUN_71000bb610).

use super::Facade;

pub struct FightStartFacade;

impl Facade for FightStartFacade {
    fn name(&self) -> &'static str {
        "Fight start"
    }

    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Fight start");
        crate::slight::frame_context::clear_after_win("fight start");
    }

    fn fighter_frame(&self) -> bool {
        false
    }

    fn weapon_frame(&self) -> bool {
        false
    }
}

/// Jorge FUN_71000bb610 fight-start path — agent_extender calls once per match.
pub fn on_fight_start() {
    skyline::println!("[SLight] Fight start");
    // Original resets all 10 subsystems before setting fight-active (FUN_71000bb610 lines 389-447).
    // Order matches the system name blob: Dynamic Memory, Skyline Hook, Multipliers,
    // Time counting, Timer system, Global fighters, Fighter Data Space, [tracker],
    // Overload time, Event System, Extras Command.
    crate::slight::systems::dynamic_memory::clear();
    crate::slight::systems::skyline_hook::clear();
    crate::slight::systems::multipliers::clear();
    crate::slight::systems::time_counting::clear();
    crate::slight::systems::timer_system::clear();
    crate::slight::systems::main_module::clear();
    crate::slight::systems::fighter_data_space::clear();
    crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .clear();
    // NOTE: kinds (RPM tabs + pinned edits) are deliberately NOT cleared here — fight_start
    // re-runs after every KO (fighter reset clears FIGHT_STARTED), and wiping kinds mid-match
    // lost the user's pinned edits ("edits revert after a couple hits") and made every
    // in-flight RPM transaction fail. Kinds clear on match teardown/uninstall only.
    crate::slight::systems::overload_timer::clear();
    crate::slight::systems::event_system::clear();
    crate::slight::systems::excommand::clear();
    crate::slight::frame_context::set_fight_active(true);
}
