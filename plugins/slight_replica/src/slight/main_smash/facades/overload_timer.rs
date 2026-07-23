use super::Facade;

pub struct OverloadTimerFacade;

impl Facade for OverloadTimerFacade {
    fn name(&self) -> &'static str {
        "Overload time measurer"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Overload time measurer");
        crate::slight::systems::overload_timer::install();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    fn weapon_frame(&self) -> bool {
        false
    }
}
