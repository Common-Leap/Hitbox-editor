use super::Facade;

pub struct DynamicMemoryFacade;

impl Facade for DynamicMemoryFacade {
    fn name(&self) -> &'static str {
        "Dynamic Memory"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Dynamic Memory");
        crate::slight::systems::dynamic_memory::install();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    fn weapon_frame(&self) -> bool {
        false
    }
}
