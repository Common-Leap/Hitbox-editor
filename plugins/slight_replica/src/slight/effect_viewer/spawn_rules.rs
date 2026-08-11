//! Live spawn rules pushed from the PC eff-editor over TCP: suppress ACMD effect spawns and
//! per-spawn transform overrides (pos/rot/scale), scoped to a motion + motion-frame window so
//! editing ONE spawn of an effect doesn't move every spawn of that effect.
//!
//! The rule list is replaced wholesale on every push (idempotent, like pins). The timing-rule
//! setter may run on the TCP thread so a frame-0 edit is visible before the next ACMD boundary;
//! readers run from game hooks. Use try_lock because parked lock waiters never wake in this
//! environment.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::slight::hitbox_viewer::LuaArg;

/// Re-fire a captured EFFECT spawn at a new motion frame (live retime). `func` is the short
/// sv_animcmd name (e.g. "EFFECT_FOLLOW"); `args` is the captured typed arg vector.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnInject {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArg>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnRule {
    pub eff_hash: u64,
    #[serde(default)]
    pub suppress: bool,
    /// Optional lifetime command this rule suppresses. Stop rules are checked only by the
    /// matching termination hook; they must not make a same-hash spawn disappear.
    #[serde(default)]
    pub stop_func: Option<String>,
    /// Live retime: fire this captured spawn at `inject.frame` (paired with a suppress rule
    /// at the pristine frame). Injection is driven at the native ACMD coroutine boundary.
    #[serde(default)]
    pub inject: Option<SpawnInject>,
    /// Re-fire a colour command at a retimed frame. This is separate from graphic injection so
    /// the ACMD hook dispatches the command with its own argument contract.
    #[serde(default)]
    pub color_inject: Option<SpawnInject>,
    /// Motion this rule is scoped to (hash40 of the motion name). None = any motion.
    #[serde(default)]
    pub motion: Option<u64>,
    /// Motion-frame window; None = match any frame.
    #[serde(default)]
    pub frame_start: Option<f32>,
    #[serde(default)]
    pub frame_end: Option<f32>,
    /// Per-spawn transform overrides (ACMD script-offset space). Applied ONLY inside the
    /// window above, so distinct spawns of the same effect stay independent.
    #[serde(default)]
    pub pos: Option<[f32; 3]>,
    #[serde(default)]
    pub rot: Option<[f32; 3]>,
    #[serde(default)]
    pub scale: Option<f32>,
    /// Per-spawn camera-flat offset — the live form of the spawn's
    /// `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` line. The macro applies it to the last spawned
    /// effect, so it is carried as a pending modifier rather than folded into `pos`.
    #[serde(default)]
    pub camera_offset: Option<f32>,
    /// Per-spawn playback rate — the live form of the spawn's `LAST_EFFECT_SET_RATE`.
    ///
    /// Not part of the argument rewrite: the rate is not an argument of any spawn macro. It is
    /// applied to the handle after the spawn, and the script's own rate line — which runs
    /// afterwards and would otherwise win — is rewritten to match.
    #[serde(default)]
    pub rate: Option<f32>,
    /// Per-spawn tint and opacity — the live forms of `LAST_EFFECT_SET_COLOR` and
    /// `LAST_EFFECT_SET_ALPHA`, handled exactly the way `rate` above is.
    ///
    /// Not the same thing as `color` below, which is a whole-fighter tint keyed on a command
    /// name. These two scope to one spawn of one effect kind, like everything else above them.
    #[serde(default)]
    pub tint: Option<[f32; 3]>,
    /// Per-spawn particle tint — the live form of `LAST_PARTICLE_SET_COLOR`. This is separate
    /// from `tint` because the game primitive targets the last particle, not the last effect.
    #[serde(default)]
    pub particle_tint: Option<[f32; 3]>,
    #[serde(default)]
    pub alpha: Option<f32>,
    /// Per-spawn values for the native dynamic-arity `LAST_EFFECT_SET_SCALE_W` primitive.
    /// Keep one to three values rather than padding the stack to three.
    #[serde(default)]
    pub scale_w: Option<Vec<f32>>,
    /// Live values for a colour command — `FLASH`, `BURN_COLOR`, and the rest of the family.
    ///
    /// These name no effect kind, so for such a rule `eff_hash` is hash40 of the lowercased
    /// COMMAND name instead. Nothing collides: an effect kind is a graphic name like
    /// `sys_atk_smoke`, and no graphic is called `burn_color`. Reusing the field is what lets
    /// the whole matcher above — motion, frame window, suppress — apply to these unchanged.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
    /// Frames the `_FRM` / `_FRAME` forms interpolate over. Separate from `color` because
    /// either can be edited without the other.
    #[serde(default)]
    pub transition: Option<f32>,
    /// Stable content identity for the current full-list replacement. It is deliberately not
    /// serialized: the desktop wire remains unchanged, and the value is rebuilt when rules are
    /// installed so repeated pushes of identical edits preserve the injection latch.
    #[serde(skip)]
    identity: u64,
}

impl SpawnRule {
    fn matches(&self, eff_hash: u64, motion: u64, frame: f32) -> bool {
        self.eff_hash == eff_hash
            && self.motion.map(|m| m == motion).unwrap_or(true)
            && self.frame_start.map(|s| frame >= s).unwrap_or(true)
            && self.frame_end.map(|e| frame <= e).unwrap_or(true)
    }
}

static RULES: parking_lot::Mutex<Vec<SpawnRule>> = parking_lot::Mutex::new(Vec::new());
static PENDING_RULES: parking_lot::Mutex<Option<PendingRules>> = parking_lot::Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static APPLIED_GENERATION: AtomicU64 = AtomicU64::new(0);

struct PendingRules {
    generation: u64,
    rules: Vec<SpawnRule>,
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(*byte)
    })
}

fn slot_identity(identity: u64, slot: usize) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&identity.to_le_bytes());
    bytes[8..].copy_from_slice(&(slot as u64).to_le_bytes());
    fingerprint_bytes(&bytes)
}

fn prepare_rules(mut rules: Vec<SpawnRule>) -> Vec<SpawnRule> {
    for rule in &mut rules {
        rule.identity = serde_json::to_vec(rule)
            .map(|bytes| fingerprint_bytes(&bytes))
            .unwrap_or_default();
    }
    rules
}

fn stage_rules(generation: u64, rules: Vec<SpawnRule>) {
    // The pending slot is held only long enough to replace one Vec. Spin rather than park if the
    // game-thread boundary is taking it; a parked waiter is not reliably woken by this runtime.
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
        crate::slight::diag::note("spawn rules staged until the next ACMD boundary");
    }
}

fn try_apply_rules(generation: u64, rules: Vec<SpawnRule>) -> Result<(), Vec<SpawnRule>> {
    let Some(mut current) = RULES.try_lock() else {
        return Err(rules);
    };
    let n = rules.len();
    // The desktop sends the complete sparse list. Compare the serialized rule data before
    // replacing it so identical retransmissions preserve a confirmed live latch while a
    // real edit invalidates every slot from the previous list.
    let changed = serde_json::to_vec(&*current).ok() != serde_json::to_vec(&rules).ok();
    *current = rules;
    APPLIED_GENERATION.store(generation, Ordering::Release);
    crate::slight::diag::note(format!("spawn rules replaced: {n} rule(s)"));
    drop(current);

    // Do not clear a newer update that raced while this update held RULES. Older staged updates
    // are obsolete once this generation is live.
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
        crate::slight::effect_viewer::acmd_hooks::reset_effect_injection_latches();
    }
    Ok(())
}

/// Replace the complete timing list, staging it if a game-thread reader currently owns RULES.
/// A staged update is accepted rather than dropped and is applied before the next ACMD query.
pub fn set_rules(rules: Vec<SpawnRule>) -> bool {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let rules = prepare_rules(rules);
    if let Err(rules) = try_apply_rules(generation, rules) {
        stage_rules(generation, rules);
    };
    true
}

/// Apply the newest rule list staged by a network or game-thread update. This is called before
/// ACMD suppression and injection checks, so the first coroutine boundary sees the complete edit.
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

/// Cheap pre-check so the hooks only pay the MotionModule call for ruled effects.
pub fn any_for(eff_hash: u64) -> bool {
    service_pending();
    RULES
        .try_lock()
        .map(|r| r.iter().any(|x| x.eff_hash == eff_hash))
        .unwrap_or(false)
}

/// Any live-retime inject rules present (cheap gate for the per-frame inject engine).
pub fn any_inject() -> bool {
    service_pending();
    RULES
        .try_lock()
        .map(|r| {
            r.iter()
                .any(|x| x.inject.is_some() || x.color_inject.is_some())
        })
        .unwrap_or(false)
}

/// Whether this motion has at least one replacement rule.  The ACMD boundary can briefly report
/// the previous motion while the native motion transition is still starting; callers use this
/// as a conservative test before honoring a just-requested motion hash.
pub fn has_inject_for(motion: u64) -> bool {
    service_pending();
    RULES
        .try_lock()
        .map(|rules| {
            rules.iter().any(|rule| {
                (rule.inject.is_some() || rule.color_inject.is_some())
                    && rule.motion.map(|wanted| wanted == motion).unwrap_or(true)
            })
        })
        .unwrap_or(false)
}

/// Inject rules scoped to this motion (or any-motion), with their rule index for latching.
pub fn injections_for(motion: u64) -> Vec<(usize, SpawnInject, u64)> {
    service_pending();
    let Some(rules) = RULES.try_lock() else {
        return Vec::new();
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            (r.inject.is_some() || r.color_inject.is_some())
                && r.motion.map(|m| m == motion).unwrap_or(true)
        })
        .flat_map(|(i, r)| {
            [r.inject.clone(), r.color_inject.clone()]
                .into_iter()
                .enumerate()
                .filter_map(move |(slot, inject)| {
                    inject.map(|inject| (i * 2 + slot, inject, slot_identity(r.identity, slot)))
                })
        })
        .collect()
}

/// Return the replacement injections associated with an authored suppression at this exact
/// motion/frame. The desktop wire intentionally has no schema-only pairing field: normal
/// retimes emit the suppression and replacement adjacent to one another, while colour retimes
/// carry both fields on one rule. Keeping that relationship here lets the runtime distinguish a
/// deliberately disabled authored call from a replacement that arrived too late.
pub fn replacement_injections_for_suppression(
    eff_hash: u64,
    motion: u64,
    frame: f32,
) -> Vec<(usize, SpawnInject, u64)> {
    service_pending();
    let Some(rules) = RULES.try_lock() else {
        return Vec::new();
    };
    let mut replacements = Vec::new();

    let mut push = |index: usize, rule: &SpawnRule, slot: usize, injection: &SpawnInject| {
        let identity = slot_identity(rule.identity, slot);
        if !replacements
            .iter()
            .any(|(existing, _, _)| *existing == index * 2 + slot)
        {
            replacements.push((index * 2 + slot, injection.clone(), identity));
        }
    };

    for (index, rule) in rules.iter().enumerate() {
        if !rule.suppress || !rule.matches(eff_hash, motion, frame) {
            continue;
        }

        // Colour retimes keep suppression and replacement on the same rule.
        if let Some(injection) = &rule.color_inject {
            push(index, rule, 1, injection);
        }
        if let Some(injection) = &rule.inject {
            push(index, rule, 0, injection);
        }

        // Ordinary and swapped effect retimes place the authored suppression next to its
        // replacement. Stop at another suppression so an unrelated later call cannot be paired
        // across a disabled or independently retimed call.
        for direction in [1isize, -1isize] {
            let mut cursor = index as isize + direction;
            while cursor >= 0 && (cursor as usize) < rules.len() {
                let candidate = &rules[cursor as usize];
                if cursor as usize != index
                    && candidate.suppress
                    && candidate
                        .motion
                        .map(|wanted| wanted == motion)
                        .unwrap_or(true)
                {
                    break;
                }
                if candidate
                    .motion
                    .map(|wanted| wanted == motion)
                    .unwrap_or(true)
                {
                    if let Some(injection) = &candidate.inject {
                        push(cursor as usize, candidate, 0, injection);
                    }
                    if let Some(injection) = &candidate.color_inject {
                        push(cursor as usize, candidate, 1, injection);
                    }
                }
                cursor += direction;
            }
        }
    }
    replacements
}

pub fn suppressed(eff_hash: u64, motion: u64, motion_frame: f32) -> bool {
    service_pending();
    let Some(rules) = RULES.try_lock() else {
        return false;
    };
    rules
        .iter()
        .any(|r| r.stop_func.is_none() && r.suppress && r.matches(eff_hash, motion, motion_frame))
}

/// Whether a pristine lifetime command should be suppressed for a retimed replacement. The
/// command name is part of the identity because `EFFECT_OFF_KIND` is keyed by effect hash while
/// `AFTER_IMAGE_OFF` is keyed by the command itself (its argument has no trail texture hash).
pub fn stop_suppressed(func: &str, eff_hash: u64, motion: u64, motion_frame: f32) -> bool {
    service_pending();
    let Some(rules) = RULES.try_lock() else {
        return false;
    };
    rules.iter().any(|r| {
        r.suppress
            && r.stop_func.as_deref() == Some(func)
            && r.matches(eff_hash, motion, motion_frame)
    })
}

/// Per-spawn transform for the FIRST non-suppress rule matching this spawn — (pos, rot, scale),
/// each optional. None when no transform rule applies (the spawn keeps the global pin / script).
pub fn transform_for(
    eff_hash: u64,
    motion: u64,
    motion_frame: f32,
) -> Option<(Option<[f32; 3]>, Option<[f32; 3]>, Option<f32>)> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| {
            !r.suppress
                && (r.pos.is_some() || r.rot.is_some() || r.scale.is_some())
                && r.matches(eff_hash, motion, motion_frame)
        })
        .map(|r| (r.pos, r.rot, r.scale))
}

/// Per-spawn playback rate for the FIRST non-suppress rule matching this spawn.
///
/// Looked up separately from [`transform_for`] because the two travel independently: a spawn
/// can be retuned without being moved, and moved without being retuned.
pub fn rate_for(eff_hash: u64, motion: u64, motion_frame: f32) -> Option<f32> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| !r.suppress && r.rate.is_some() && r.matches(eff_hash, motion, motion_frame))
        .and_then(|r| r.rate)
}

/// Per-spawn camera-flat offset for the FIRST non-suppress rule matching this spawn.
pub fn camera_offset_for(eff_hash: u64, motion: u64, motion_frame: f32) -> Option<f32> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| {
            !r.suppress && r.camera_offset.is_some() && r.matches(eff_hash, motion, motion_frame)
        })
        .and_then(|r| r.camera_offset)
}

/// Per-spawn tint and opacity for the FIRST non-suppress rule matching this spawn.
///
/// One lookup for both, unlike the rate above, because both are applied at the same moment to
/// the same handle — but each half stays optional, so recolouring a spawn does not also assert
/// an opacity the editor never set.
pub fn tint_for(
    eff_hash: u64,
    motion: u64,
    motion_frame: f32,
) -> Option<(Option<[f32; 3]>, Option<f32>)> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| {
            !r.suppress
                && (r.tint.is_some() || r.alpha.is_some())
                && r.matches(eff_hash, motion, motion_frame)
        })
        .map(|r| (r.tint, r.alpha))
}

/// Per-spawn particle tint for the FIRST non-suppress rule matching this spawn.
pub fn particle_tint_for(eff_hash: u64, motion: u64, motion_frame: f32) -> Option<[f32; 3]> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| {
            !r.suppress && r.particle_tint.is_some() && r.matches(eff_hash, motion, motion_frame)
        })
        .and_then(|r| r.particle_tint)
}

/// Per-spawn dynamic-arity scale-W values for the FIRST non-suppress rule matching this spawn.
pub fn scale_w_for(eff_hash: u64, motion: u64, motion_frame: f32) -> Option<Vec<f32>> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| !r.suppress && r.scale_w.is_some() && r.matches(eff_hash, motion, motion_frame))
        .and_then(|r| {
            r.scale_w
                .as_ref()
                .filter(|values| {
                    (1..=3).contains(&values.len()) && values.iter().all(|value| value.is_finite())
                })
                .cloned()
        })
}

/// Live colour and interpolation length for the FIRST non-suppress rule matching this colour
/// command, where `cmd_hash` is hash40 of the lowercased command name.
///
/// Returns `None` when no rule applies, so the script's own arguments are left alone; the two
/// halves are separately optional so retiming a ramp does not have to restate its colour.
pub fn color_for(
    cmd_hash: u64,
    motion: u64,
    motion_frame: f32,
) -> Option<(Option<[f32; 4]>, Option<f32>)> {
    service_pending();
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| {
            !r.suppress
                && (r.color.is_some() || r.transition.is_some())
                && r.matches(cmd_hash, motion, motion_frame)
        })
        .map(|r| (r.color, r.transition))
}

// ── Effect kind aliases (live transplant parity) ─────────────────────────────
//
// A transplanted COPY (new eff entry) or a costume REPLACEMENT doesn't exist in the
// running game's loaded eff resources until the mod is exported and the game restarts —
// spawning its hash does nothing. But a fresh copy is content-identical to its donor,
// so the live equivalent is: rewrite the requested kind to the donor's at the EFFECT
// hook, while every rule/pin/override still keys on the REQUESTED (copy) hash. The
// live game then matches what the export will produce.

/// requested kind (`from`) → kind that exists live (`to`), optionally costume-gated.
#[derive(Clone, Debug, Deserialize)]
pub struct EffectAlias {
    pub from: u64,
    pub to: u64,
    /// Costume slots the alias is active on; empty = all costumes. NOT limited to the
    /// vanilla c00–c07: slot-add mods use higher indices, and `u8` spans the whole colour
    /// index range the runtime can report.
    #[serde(default)]
    pub slots: Vec<u8>,
}

static ALIASES: parking_lot::Mutex<Vec<EffectAlias>> = parking_lot::Mutex::new(Vec::new());

/// Full-list replace (idempotent, like rules). Empty clears all aliases.
pub fn set_aliases(aliases: Vec<EffectAlias>) {
    let n = aliases.len();
    if let Some(mut a) = ALIASES.try_lock() {
        *a = aliases;
        crate::slight::diag::note(format!("effect aliases replaced: {n} alias(es)"));
    } else {
        crate::slight::diag::note("effect aliases: lock contended, push dropped");
    }
}

/// Cheap gate so the hooks skip the costume lookup when no aliases exist.
pub fn any_alias() -> bool {
    ALIASES.try_lock().map(|a| !a.is_empty()).unwrap_or(false)
}

/// The live substitute for a requested kind (`costume` = fighter color index, -1 when
/// unknown — slot-gated aliases then do NOT match).
pub fn alias_for(from: u64, costume: i32) -> Option<u64> {
    let aliases = ALIASES.try_lock()?;
    // `try_from`, not `as`: a costume index outside u8 would WRAP and could then falsely
    // match a low-numbered slot (colour 264 aliasing onto a c08-scoped entry). Out of range
    // means "no slot-gated alias applies", same as the unknown-costume (-1) case.
    let slot = u8::try_from(costume).ok();
    aliases
        .iter()
        .find(|a| {
            a.from == from && (a.slots.is_empty() || slot.is_some_and(|s| a.slots.contains(&s)))
        })
        .map(|a| a.to)
}
