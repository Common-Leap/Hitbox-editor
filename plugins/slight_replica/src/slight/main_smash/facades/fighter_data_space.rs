use super::Facade;

pub struct FighterDataSpaceFacade;

impl Facade for FighterDataSpaceFacade {
    fn name(&self) -> &'static str {
        "Fighter Data Space system"
    }

    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Fighter Data Space system");
        crate::slight::systems::fighter_data_space::install();
    }

    fn on_frame(&mut self) {
        crate::slight::systems::fighter_data_space::on_frame();
    }

    fn fighter_frame(&self) -> bool {
        true
    }

    fn weapon_frame(&self) -> bool {
        false
    }
}
