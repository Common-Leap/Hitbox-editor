use super::Facade;

pub struct GuardStanceFacade;

impl Facade for GuardStanceFacade {
    fn name(&self) -> &'static str {
        "Guard stance"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Guard stance");
        crate::slight::systems::guard_stance::install();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    fn weapon_frame(&self) -> bool {
        false
    }
    fn global_only(&self) -> bool {
        true
    }
}
