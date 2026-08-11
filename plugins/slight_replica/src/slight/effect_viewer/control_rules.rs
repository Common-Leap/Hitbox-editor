//! Live rules for effect point controls (`EFFECT_DETACH_*` and area toggles).
//!
//! These are deliberately separate from spawn rules. A control has no effect kind to match on
//! and no emitter transform; its identity is the primitive plus the exact captured arguments at
//! one motion-frame window.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::slight::hitbox_viewer::LuaArg;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlInject {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArg>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// WorkModule slot to resolve when injecting `EFFECT_DETACH_KIND_WORK`. The desktop sends this
    /// only for numeric values or symbolic IDs covered by its measured constant table.
    #[serde(default)]
    pub work_slot: Option<i32>,
    /// Stable content identity rebuilt for each full-list replacement. This is not part of the
    /// desktop wire; it lets identical rule pushes preserve a confirmed injection latch.
    #[serde(skip)]
    identity: u64,
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
static PENDING_RULES: parking_lot::Mutex<Option<PendingRules>> = parking_lot::Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static APPLIED_GENERATION: AtomicU64 = AtomicU64::new(0);

struct PendingRules {
    generation: u64,
    rules: Vec<ControlRule>,
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(*byte)
    })
}

fn prepare_rules(mut rules: Vec<ControlRule>) -> Vec<ControlRule> {
    for rule in &mut rules {
        rule.identity = serde_json::to_vec(rule)
            .map(|bytes| fingerprint_bytes(&bytes))
            .unwrap_or_default();
    }
    rules
}

fn stage_rules(generation: u64, rules: Vec<ControlRule>) {
    let mut pending = loop {
        if let Some(guard) = PENDING_RULES.try_lock() {
            break guard;
        }
        core::hint::spin_loop();
    };
    if pending
        .as_ref()
        .map(|current| generation >= current.generation)
        .unwrap_or(true)
    {
        *pending = Some(PendingRules { generation, rules });
        crate::slight::diag::note("effect control rules staged until the next ACMD boundary");
    }
}

fn try_apply_rules(generation: u64, rules: Vec<ControlRule>) -> Result<(), Vec<ControlRule>> {
    let Some(mut current) = RULES.try_lock() else {
        return Err(rules);
    };
    let count = rules.len();
    // Preserve a confirmed latch for identical full-list retransmissions, but invalidate the
    // previous rule generation when any value, timing, or slot changes.
    let changed = serde_json::to_vec(&*current).ok() != serde_json::to_vec(&rules).ok();
    *current = rules;
    APPLIED_GENERATION.store(generation, Ordering::Release);
    crate::slight::diag::note(format!("effect control rules replaced: {count} rule(s)"));
    drop(current);

    if let Some(mut pending) = PENDING_RULES.try_lock() {
        if pending
            .as_ref()
            .map(|current| current.generation <= generation)
            .unwrap_or(false)
        {
            *pending = None;
        }
    }
    if changed {
        crate::slight::effect_viewer::acmd_hooks::reset_control_injection_latches();
    }
    Ok(())
}

/// Replace the complete control list, staging it if a game-thread reader currently owns RULES.
pub fn set_rules(rules: Vec<ControlRule>) -> bool {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let rules = prepare_rules(rules);
    if let Err(rules) = try_apply_rules(generation, rules) {
        stage_rules(generation, rules);
    };
    true
}

/// Apply the newest list staged by a network or game-thread update before a control hook reads it.
pub fn service_pending() {
    let pending = {
        let Some(mut pending) = PENDING_RULES.try_lock() else {
            return;
        };
        pending.take()
    };
    let Some(PendingRules { generation, rules }) = pending else {
        return;
    };
    if generation <= APPLIED_GENERATION.load(Ordering::Acquire) {
        return;
    }
    if let Err(rules) = try_apply_rules(generation, rules) {
        stage_rules(generation, rules);
    }
}

pub fn any_for(func: &str) -> bool {
    service_pending();
    RULES
        .try_lock()
        .map(|rules| rules.iter().any(|rule| rule.func == func))
        .unwrap_or(false)
}

pub fn suppressed(func: &str, args: &[LuaArg], motion: u64, frame: f32) -> bool {
    service_pending();
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
    service_pending();
    RULES
        .try_lock()
        .map(|rules| rules.iter().any(|rule| rule.inject.is_some()))
        .unwrap_or(false)
}

/// Whether this motion has at least one point-control replacement.  This is used while the
/// native motion transition is publishing its new hash and the ACMD boundary may still expose
/// the previous motion.
pub fn has_inject_for(motion: u64) -> bool {
    service_pending();
    RULES
        .try_lock()
        .map(|rules| {
            rules.iter().any(|rule| {
                rule.inject.is_some() && rule.motion.map(|wanted| wanted == motion).unwrap_or(true)
            })
        })
        .unwrap_or(false)
}

pub fn injections_for(motion: u64) -> Vec<(usize, ControlInject, Option<i32>, u64)> {
    service_pending();
    let Some(rules) = RULES.try_lock() else {
        return Vec::new();
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule.inject.is_some() && rule.motion.map(|wanted| wanted == motion).unwrap_or(true)
        })
        .map(|(index, rule)| {
            (
                index,
                rule.inject.clone().unwrap(),
                rule.work_slot,
                rule.identity,
            )
        })
        .collect()
}
