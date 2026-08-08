//! Live rules for effect point controls (`EFFECT_DETACH_*` and area toggles).
//!
//! These are deliberately separate from spawn rules. A control has no effect kind to match on
//! and no emitter transform; its identity is the primitive plus the exact captured arguments at
//! one motion-frame window.

use serde::Deserialize;

use crate::slight::hitbox_viewer::LuaArg;

#[derive(Clone, Debug, Deserialize)]
pub struct ControlInject {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArg>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ControlRule {
    pub motion: Option<u64>,
    pub func: String,
    #[serde(default)]
    pub args: Vec<LuaArg>,
    #[serde(default)]
    pub suppress: bool,
    #[serde(default)]
    pub inject: Option<ControlInject>,
    #[serde(default)]
    pub frame_start: Option<f32>,
    #[serde(default)]
    pub frame_end: Option<f32>,
}

impl ControlRule {
    fn matches(&self, func: &str, args: &[LuaArg], motion: u64, frame: f32) -> bool {
        self.func == func
            && self.motion.map(|m| m == motion).unwrap_or(true)
            && self.args == args
            && self.frame_start.map(|start| frame >= start).unwrap_or(true)
            && self.frame_end.map(|end| frame <= end).unwrap_or(true)
    }
}

static RULES: parking_lot::Mutex<Vec<ControlRule>> = parking_lot::Mutex::new(Vec::new());

pub fn set_rules(rules: Vec<ControlRule>) {
    let count = rules.len();
    if let Some(mut current) = RULES.try_lock() {
        *current = rules;
        crate::slight::diag::note(format!("effect control rules replaced: {count} rule(s)"));
    } else {
        crate::slight::diag::note("effect control rules: lock contended, push dropped");
    }
}

pub fn any_for(func: &str) -> bool {
    RULES
        .try_lock()
        .map(|rules| rules.iter().any(|rule| rule.func == func))
        .unwrap_or(false)
}

pub fn suppressed(func: &str, args: &[LuaArg], motion: u64, frame: f32) -> bool {
    RULES
        .try_lock()
        .map(|rules| {
            rules
                .iter()
                .any(|rule| rule.suppress && rule.matches(func, args, motion, frame))
        })
        .unwrap_or(false)
}

pub fn any_inject() -> bool {
    RULES
        .try_lock()
        .map(|rules| rules.iter().any(|rule| rule.inject.is_some()))
        .unwrap_or(false)
}

pub fn injections_for(motion: u64) -> Vec<(usize, ControlInject)> {
    let Some(rules) = RULES.try_lock() else {
        return Vec::new();
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule.inject.is_some() && rule.motion.map(|wanted| wanted == motion).unwrap_or(true)
        })
        .map(|(index, rule)| (index, rule.inject.clone().unwrap()))
        .collect()
}
