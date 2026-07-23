use super::Facade;

pub struct DamageManagerFacade;

impl Facade for DamageManagerFacade {
    fn name(&self) -> &'static str {
        "Damage manager module"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Damage manager module");
        crate::slight::systems::damage_manager::install();
    }
    fn init_frame(&mut self) {
        crate::slight::systems::damage_manager::on_init_frame();
    }
    fn per_agent_init(&self) -> bool {
        true
    }
    fn weapon_frame(&self) -> bool {
        false
    }
}
