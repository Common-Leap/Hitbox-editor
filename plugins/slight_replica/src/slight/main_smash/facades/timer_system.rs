use super::Facade;

pub struct TimerSystemFacade;

impl Facade for TimerSystemFacade {
    fn name(&self) -> &'static str {
        "Timer system"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Timer system");
        crate::slight::systems::timer_system::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::timer_system::on_frame();
    }
    fn once_per_frame(&self) -> bool {
        true
    }
}
