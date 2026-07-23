use super::Facade;

use crate::slight::systems::init_frame::InitAction;

pub struct MainModuleFacade;

impl Facade for MainModuleFacade {
    fn name(&self) -> &'static str {
        "Main module"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Main module");
        crate::slight::systems::main_module::install();
    }
    fn init_frame(&mut self) {
        if let Some(rec) = crate::slight::frame_context::current_agent() {
            if crate::slight::frame_context::should_skip_agent_init(rec.boid) {
                return;
            }
            if rec.category == 0 {
                match crate::slight::systems::init_frame::poll(rec.boid) {
                    InitAction::FirstInit if !crate::slight::agents::has_initialized(rec.boid) => {
                        crate::slight::systems::main_module::on_init_fighter(rec.boid);
                    }
                    InitAction::Reinit => {
                        // Original add_fighter_reset_callback(FUN_71000bf23c) fires for ALL
                        // resets, not only deaths. on_reset_fighter handles the condition inside.
                        crate::slight::systems::main_module::on_reset_fighter(rec.boid);
                        crate::slight::systems::main_module::on_reinit_fighter(rec.boid);
                    }
                    InitAction::None | InitAction::FirstInit => {}
                }
            } else if rec.category == 1 {
                match crate::slight::systems::init_frame::poll(rec.boid) {
                    InitAction::FirstInit if !crate::slight::agents::has_initialized(rec.boid) => {
                        crate::slight::systems::main_module::on_init_weapon(rec.boid);
                    }
                    InitAction::Reinit => {
                        crate::slight::systems::main_module::on_reset_weapon(rec.boid);
                        crate::slight::systems::main_module::on_init_weapon(rec.boid);
                    }
                    InitAction::None | InitAction::FirstInit => {}
                }
            }
        }
    }
    fn per_agent_init(&self) -> bool {
        true
    }
}
