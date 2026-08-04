//! Live spawn rules pushed from the PC eff-editor over TCP: suppress ACMD effect spawns and
//! per-spawn transform overrides (pos/rot/scale), scoped to a motion + motion-frame window so
//! editing ONE spawn of an effect doesn't move every spawn of that effect.
//!
//! The rule list is replaced wholesale on every push (idempotent, like pins). Both the
//! setter (TCP poll, game-thread facade) and the readers (ACMD hooks) run on the game
//! thread, so contention is not expected — try_lock anyway per the environment rule that
//! parked lock waiters never wake.

use serde::Deserialize;

use crate::slight::hitbox_viewer::LuaArg;

/// Re-fire a captured EFFECT spawn at a new motion frame (live retime). `func` is the short
/// sv_animcmd name (e.g. "EFFECT_FOLLOW"); `args` is the captured typed arg vector.
#[derive(Clone, Debug, Deserialize)]
pub struct SpawnInject {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArg>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnRule {
    pub eff_hash: u64,
    #[serde(default)]
    pub suppress: bool,
    /// Live retime: fire this captured spawn at `inject.frame` (paired with a suppress rule
    /// at the pristine frame). Injection is driven per-frame from the agent line callback.
    #[serde(default)]
    pub inject: Option<SpawnInject>,
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
    #[serde(default)]
    pub alpha: Option<f32>,
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

pub fn set_rules(rules: Vec<SpawnRule>) {
    let n = rules.len();
    if let Some(mut r) = RULES.try_lock() {
        *r = rules;
        crate::slight::diag::note(format!("spawn rules replaced: {n} rule(s)"));
    } else {
        crate::slight::diag::note("spawn rules: lock contended, push dropped");
    }
}

/// Cheap pre-check so the hooks only pay the MotionModule call for ruled effects.
pub fn any_for(eff_hash: u64) -> bool {
    RULES
        .try_lock()
        .map(|r| r.iter().any(|x| x.eff_hash == eff_hash))
        .unwrap_or(false)
}

/// Any live-retime inject rules present (cheap gate for the per-frame inject engine).
pub fn any_inject() -> bool {
    RULES
        .try_lock()
        .map(|r| r.iter().any(|x| x.inject.is_some()))
        .unwrap_or(false)
}

/// Inject rules scoped to this motion (or any-motion), with their rule index for latching.
pub fn injections_for(motion: u64) -> Vec<(usize, SpawnInject)> {
    let Some(rules) = RULES.try_lock() else {
        return Vec::new();
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.inject.is_some() && r.motion.map(|m| m == motion).unwrap_or(true))
        .map(|(i, r)| (i, r.inject.clone().unwrap()))
        .collect()
}

pub fn suppressed(eff_hash: u64, motion: u64, motion_frame: f32) -> bool {
    let Some(rules) = RULES.try_lock() else {
        return false;
    };
    rules
        .iter()
        .any(|r| r.suppress && r.matches(eff_hash, motion, motion_frame))
}

/// Per-spawn transform for the FIRST non-suppress rule matching this spawn — (pos, rot, scale),
/// each optional. None when no transform rule applies (the spawn keeps the global pin / script).
pub fn transform_for(
    eff_hash: u64,
    motion: u64,
    motion_frame: f32,
) -> Option<(Option<[f32; 3]>, Option<[f32; 3]>, Option<f32>)> {
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
    let rules = RULES.try_lock()?;
    rules
        .iter()
        .find(|r| !r.suppress && r.rate.is_some() && r.matches(eff_hash, motion, motion_frame))
        .and_then(|r| r.rate)
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
