//! Effect viewer — Jorge facade registry entry; per-frame effect tick dispatch.

use super::Facade;

pub struct EffectViewerFacade;

impl Facade for EffectViewerFacade {
    fn name(&self) -> &'static str {
        "Effect viewer"
    }

    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Effect viewer");
    }

    fn on_frame(&mut self) {
        if crate::slight::frame_context::is_after_win() {
            return;
        }
        let ids: Vec<u64> = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
            .lock()
            .iter()
            .map(|e| e.id)
            .collect();
        crate::slight::effect_viewer::each_frame(&ids);
    }

    fn fighter_frame(&self) -> bool {
        true
    }

    fn weapon_frame(&self) -> bool {
        false
    }

    fn global_only(&self) -> bool {
        true
    }
}
