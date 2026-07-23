use super::Facade;

pub struct MultipliersFacade;

impl Facade for MultipliersFacade {
    fn name(&self) -> &'static str {
        "Multipliers"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Multipliers");
        crate::slight::systems::multipliers::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::multipliers::on_frame();
    }
    fn weapon_frame(&self) -> bool {
        false
    }
    fn once_per_frame(&self) -> bool {
        true
    }
}
