use super::Facade;

pub struct ArticleNotifierFacade;

impl Facade for ArticleNotifierFacade {
    fn name(&self) -> &'static str {
        "Article notifier"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Article notifier");
        crate::slight::systems::article_notifier::install();
    }
    fn on_frame(&mut self) {
        crate::slight::systems::article_notifier::on_weapon_frame();
    }
    fn fighter_frame(&self) -> bool {
        false
    }
    /// `on_weapon_frame` does a global scan of all weapons, so it must run once per weapon-frame
    /// regardless of how many weapons exist — otherwise when the last weapon despawns (0 weapon
    /// agents) the per-agent loop is empty and the despawn is never detected.
    fn global_only(&self) -> bool {
        true
    }
}
