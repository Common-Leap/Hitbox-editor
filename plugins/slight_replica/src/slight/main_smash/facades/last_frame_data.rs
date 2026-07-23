use super::Facade;

pub struct LastFrameDataFacade;

impl Facade for LastFrameDataFacade {
    fn name(&self) -> &'static str {
        "Last frame data"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Last frame data");
        crate::slight::systems::last_frame_data::install();
    }
    fn post_frame(&mut self) {
        crate::slight::systems::last_frame_data::on_post_frame();
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
