use super::Facade;

pub struct SkylineHookFacade;

impl Facade for SkylineHookFacade {
    fn name(&self) -> &'static str {
        "Skyline Hook"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Skyline Hook");
        crate::slight::systems::skyline_hook::install();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    fn weapon_frame(&self) -> bool {
        false
    }
}
