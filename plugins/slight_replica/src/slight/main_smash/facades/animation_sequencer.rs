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
        // E2's one-shot rate measurement. Rides this facade because it needs the same
        // per-frame cadence and the same agent, but it resolves its own accessor rather than
        // reusing the sequencer's — see `rate_probe::on_frame`. Delete with the probe.
        crate::slight::systems::rate_probe::on_frame();
        crate::slight::systems::animation_sequencer::on_frame();
    }
    fn post_frame(&mut self) {
        crate::slight::systems::animation_sequencer::on_post_frame();
    }
}
