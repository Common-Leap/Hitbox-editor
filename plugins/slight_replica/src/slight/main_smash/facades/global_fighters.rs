use super::Facade;

pub struct GlobalFightersFacade;

impl Facade for GlobalFightersFacade {
    fn name(&self) -> &'static str {
        "Global fighters system"
    }
    fn install(&mut self) {
        skyline::println!("[SLight] Installing facade Global fighters system");
    }
    fn global_only(&self) -> bool {
        true
    }
}
