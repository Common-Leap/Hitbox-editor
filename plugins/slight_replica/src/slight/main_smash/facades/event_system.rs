use super::Facade;

pub struct EventSystemFacade;

impl Facade for EventSystemFacade {
    fn name(&self) -> &'static str {
        "Event System"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Event System");
        crate::slight::systems::event_system::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::event_system::on_frame();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    fn weapon_frame(&self) -> bool {
        false
    }
}
