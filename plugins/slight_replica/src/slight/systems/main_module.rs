//! Fight lifecycle coordinator — Jorge main_module facade.

use std::sync::atomic::{AtomicBool, Ordering};

static INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    skyline::println!("[SLight] Main install");
}

pub fn on_init_fighter(boid: u32) {
    skyline::println!("[SLight] Init fighter {boid}");
    crate::slight::systems::animation_sequencer::create(boid, &format!("fighter_{boid}"));
    crate::slight::systems::fighter_data_space::init_fighter(boid);
    crate::slight::systems::dynamic_memory::ensure_agent(boid);
    crate::slight::systems::damage_manager::init_agent(boid, unsafe {
        smash::app::sv_battle_object::module_accessor(boid)
    });
    crate::slight::systems::event_system::emit(
        crate::slight::systems::event_system::GameEvent::Spawn { boid },
    );
    crate::slight::systems::agent_info::notify_if_after_win(boid);
    skyline::println!("[SLight] After init fighter");
}

pub fn on_reinit_fighter(boid: u32) {
    skyline::println!("[SLight] Reinit fighter {boid}");
    skyline::println!("[SLight] Reinitializing fighter over facade");
    crate::slight::systems::animation_sequencer::remove(boid);
    crate::slight::systems::animation_sequencer::create(boid, &format!("fighter_{boid}"));
    crate::slight::systems::fighter_data_space::init_fighter(boid);
    crate::slight::systems::dynamic_memory::ensure_agent(boid);
    crate::slight::systems::event_system::emit(
        crate::slight::systems::event_system::GameEvent::Respawn { boid },
    );
}

pub fn on_reset_fighter(boid: u32) {
    skyline::println!("[SLight] Started reset fighter");
    crate::slight::systems::animation_sequencer::remove(boid);
    crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .invalidate_boid(boid);
    crate::slight::systems::init_frame::clear_boid(boid);
    crate::slight::systems::event_system::emit(
        crate::slight::systems::event_system::GameEvent::StatusChange { boid, status: 0 },
    );
    // FUN_71000bf23c: DAT_71001e3254 (FIGHT_STARTED) is cleared unconditionally on every reset,
    // but DAT_71001e3238 (IS_AFTER_WIN_SCREEN) is cleared ONLY when the fighter is dead
    // (status_kind == -1) or the match is in training mode.
    crate::slight::agent_extender::reset_fight_started();
    unsafe {
        let ptr = smash::app::sv_battle_object::module_accessor(boid);
        let dead = !ptr.is_null() && smash::app::lua_bind::StatusModule::status_kind(ptr) == -1;
        if dead || smash::app::smashball::is_training_mode() {
            crate::slight::frame_context::clear_after_win("Fighter Reset");
        }
    }
    skyline::println!("[SLight] Ended reset fighter");
}

pub fn on_init_weapon(boid: u32) {
    skyline::println!("[SLight] Initializing weapon {boid}");
    crate::slight::systems::dynamic_memory::ensure_agent(boid);
}

pub fn on_reset_weapon(boid: u32) {
    skyline::println!("[SLight] After resetting weapon");
    crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .invalidate_boid(boid);
    crate::slight::systems::init_frame::clear_boid(boid);
}

pub fn on_uninstall() {
    skyline::println!("[SLight] Start uninstall");
    crate::slight::frame_context::clear_after_win("uninstall");
    skyline::println!("[SLight] SLight framework uninstalled!");
    crate::slight::systems::clear_all();
    crate::slight::frame_context::reset();
    INSTALLED.store(false, Ordering::SeqCst);
}

pub fn clear() {
    INSTALLED.store(false, Ordering::SeqCst);
}
