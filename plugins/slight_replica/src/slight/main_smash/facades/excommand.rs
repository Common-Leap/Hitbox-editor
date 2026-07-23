use super::Facade;

pub struct ExcommandFacade;

impl Facade for ExcommandFacade {
    fn name(&self) -> &'static str {
        "Extras Command System"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Extras Command System");
        crate::slight::systems::excommand::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::excommand::on_frame();
    }
    fn weapon_frame(&self) -> bool {
        false
    }
    fn once_per_frame(&self) -> bool {
        true
    }
}
