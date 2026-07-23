use super::Facade;

pub struct GaugeSystemFacade;

impl Facade for GaugeSystemFacade {
    fn name(&self) -> &'static str {
        "Gauge system"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Gauge system");
        crate::slight::systems::gauge_system::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::gauge_system::on_frame();
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
