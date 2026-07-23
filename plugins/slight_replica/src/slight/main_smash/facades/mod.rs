//! Jorge SLight facade registry — 15 facades @ FUN_71000bd7ec install order.

pub mod animation_sequencer;
pub mod article_notifier;
pub mod damage_manager;
pub mod debuggable_server;
pub mod dynamic_memory;
pub mod effect_manager;
pub mod effect_viewer;
pub mod event_system;
pub mod excommand;
pub mod fight_start;
pub mod fighter_data_space;
pub mod gauge_system;
pub mod global_fighters;
pub mod guard_stance;
pub mod last_frame_data;
pub mod main_module;
pub mod multipliers;
pub mod overload_timer;
pub mod skyline_hook;
pub mod time_counting;
pub mod timed_action;
pub mod timer_system;

pub trait Facade: Send {
    fn name(&self) -> &'static str;
    fn install(&mut self);
    fn pre_init_frame(&mut self) {}
    fn init_frame(&mut self) {}
    fn post_init_frame(&mut self) {}
    fn pre_frame(&mut self) {}
    fn on_frame(&mut self) {}
    fn post_frame(&mut self) {}
    fn fighter_frame(&self) -> bool {
        true
    }
    fn weapon_frame(&self) -> bool {
        true
    }
    fn per_agent_init(&self) -> bool {
        false
    }
    fn global_only(&self) -> bool {
        false
    }
    /// Fighter-path facades that scan globally (excommand, multipliers) — once per dispatch pass.
    fn once_per_frame(&self) -> bool {
        false
    }
}

pub fn build_registry() -> Vec<Box<dyn Facade>> {
    vec![
        Box::new(dynamic_memory::DynamicMemoryFacade),
        Box::new(skyline_hook::SkylineHookFacade),
        Box::new(multipliers::MultipliersFacade),
        Box::new(time_counting::TimeCountingFacade),
        Box::new(timer_system::TimerSystemFacade),
        Box::new(global_fighters::GlobalFightersFacade),
        Box::new(fighter_data_space::FighterDataSpaceFacade),
        Box::new(overload_timer::OverloadTimerFacade),
        Box::new(event_system::EventSystemFacade),
        Box::new(excommand::ExcommandFacade),
        Box::new(effect_manager::EffectManagerFacade),
        Box::new(debuggable_server::DebuggableServerFacade),
        Box::new(damage_manager::DamageManagerFacade),
        Box::new(main_module::MainModuleFacade),
        Box::new(animation_sequencer::AnimationSequencerFacade),
        Box::new(effect_viewer::EffectViewerFacade),
    ]
}

/// Jorge "Late install of systems" — fight start, gauge, last-frame snapshots, article notifier.
pub fn build_late_registry() -> Vec<Box<dyn Facade>> {
    vec![
        Box::new(fight_start::FightStartFacade),
        Box::new(gauge_system::GaugeSystemFacade),
        Box::new(last_frame_data::LastFrameDataFacade),
        Box::new(article_notifier::ArticleNotifierFacade),
        Box::new(guard_stance::GuardStanceFacade),
        Box::new(timed_action::TimedActionFacade),
    ]
}
