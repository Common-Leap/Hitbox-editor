use super::Facade;

pub struct EffectManagerFacade;

impl Facade for EffectManagerFacade {
    fn name(&self) -> &'static str {
        "Effect Manager"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Effect Manager");
    }
    fn global_only(&self) -> bool {
        true
    }
    fn on_frame(&mut self) {
        // Jorge FUN_710009d26c: the Effect Manager only RECONCILES already-tracked effects —
        // it checks EffectModule::is_exist_effect on each stored handle and hides any that no
        // longer exist. Effect DISCOVERY happens via the EffectModule::req* hooks (see
        // effect_viewer), not by scanning effect indices. Single-pass over all effects keyed by
        // the live-accessor map (was O(accessors × effects) per frame).
        let live: std::collections::HashMap<u64, *mut smash::app::BattleObjectModuleAccessor> =
            crate::slight::agents::live_accessors()
                .into_iter()
                .collect();
        // Instance-level cleanup only. Kind tabs persist in RPM (pinned edits survive across
        // spawns); no per-instance Remove is sent.
        let _removed = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
            .lock()
            .reconcile_all(&live);
    }
}
