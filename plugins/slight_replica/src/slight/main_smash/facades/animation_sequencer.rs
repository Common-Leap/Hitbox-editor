use super::Facade;

pub struct AnimationSequencerFacade;

impl Facade for AnimationSequencerFacade {
    fn name(&self) -> &'static str {
        "Animation sequencer system"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Animation sequencer system");
        crate::slight::systems::animation_sequencer::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::animation_sequencer::on_frame();
    }
    fn post_frame(&mut self) {
        crate::slight::systems::animation_sequencer::on_post_frame();
    }
}
