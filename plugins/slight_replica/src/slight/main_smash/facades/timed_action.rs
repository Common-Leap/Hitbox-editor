use super::Facade;

pub struct TimedActionFacade;

impl Facade for TimedActionFacade {
    fn name(&self) -> &'static str {
        "Timed Action system"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Timed Action system");
        crate::slight::systems::timed_action::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::timed_action::on_frame();
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
