use super::Facade;

pub struct TimeCountingFacade;

impl Facade for TimeCountingFacade {
    fn name(&self) -> &'static str {
        "Time counting"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Time counting");
        crate::slight::systems::time_counting::install();
    }
    fn pre_frame(&mut self) {
        crate::slight::systems::time_counting::on_pre_frame();
    }
    fn post_frame(&mut self) {
        crate::slight::systems::time_counting::on_post_frame();
    }
}
