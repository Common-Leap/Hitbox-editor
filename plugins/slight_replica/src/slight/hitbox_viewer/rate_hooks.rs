//! Live override for `FT_MOTION_RATE` — E2 — and for the direct `MotionModule` playback setters.
//!
//! The partial-animation setters live here too: `set_rate_partial` because it is a rate, and
//! `set_frame_partial` because it is the same `(boma, part_kind, …)` shape hooked the same way.
//!
//! **One hook covers all three rate macros.** `smash-script` compiles `FT_MOTION_RATE`,
//! `FT_MOTION_RATE_RANGE` and `FT_DESIRED_RATE` down to the same `sv_animcmd::FT_MOTION_RATE`
//! with a single `f32` on the lua stack; the two longer forms just divide first
//! (`game_frames / motion_frames`). So there is nothing to enumerate here and no family table to
//! keep in step with the editor's, unlike the sound hooks.
//!
//! That division is also **independent confirmation of the direction**: the macro that takes
//! motion frames and game frames explicitly passes `game_frames / motion_frames` as the rate, so
//! `game_frames = motion_frames * rate`. A rate below 1.0 makes a move play *faster*. That was
//! measured live before it was read here, and the two agree.
//!
//! **What the editor can do to a rate, and therefore what this applies.** The value only. A rate
//! call's frame is the position it sits at in the script, so moving, adding or removing one is a
//! structural edit that belongs to the export — the same boundary the sound and hurtbox
//! sections draw.

use super::{
    any_rules, read_args_exact, record, record_for_boma, HbOverrides, LuaArg,
    CAT_MOTION_MODULE_SET_FRAME_PARTIAL, CAT_MOTION_MODULE_SET_HELPER_CALCULATION,
    CAT_MOTION_MODULE_SET_RATE, CAT_MOTION_MODULE_SET_RATE_PARTIAL, CAT_MOTION_RATE,
};

/// Reports per rule set, not per boot.
///
/// A per-boot budget is what made D1g's third round unreadable: ordinary play burned it
/// thousands of lines before the rule under test ever arrived. `reset_reports` is called from
/// `set_rules`, so every new rule set gets a fresh window to explain itself in.
static MISSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SAID_NO_RULES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const MISS_BUDGET: usize = 24;

pub(super) fn reset_reports() {
    MISSES.store(0, std::sync::atomic::Ordering::Relaxed);
    SAID_NO_RULES.store(false, std::sync::atomic::Ordering::Relaxed);
}

fn bail(why: &str) {
    if MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < MISS_BUDGET {
        crate::slight::diag::note(format!("RATE bail: {why}"));
    }
}

/// The rate argument, however the lua stack happens to have typed it.
///
/// `lua_args!` pushes an `f32`, so `Num` is what this sees in practice — but an integral rate
/// written `FT_MOTION_RATE(agent, 1)` would arrive as `Int`, and D1g is the standing reminder
/// that matching a live argument on its type tag is how a feature comes to half-work. The
/// number is the same either way.
fn rate_arg(arg: &LuaArg) -> Option<f32> {
    match arg {
        LuaArg::Num(n) => Some(*n),
        LuaArg::Int(i) => Some(*i as f32),
        _ => None,
    }
}

/// Decide what to do with one `FT_MOTION_RATE` call.
///
/// A free function taking the motion and frame rather than a method reading them, so the
/// decision is reachable without a running game — the plugin crate is outside the workspace and
/// cannot host a test, but the editor asserts against this file's *source*.
///
/// **Says why a rule did not fire.** The first version of this returned `None` silently, with
/// "every early return reports itself" written directly above it — and the *rule miss* is the
/// one return that matters, because a live edit doing nothing is the failure this family is most
/// exposed to. A rate rule can miss on the motion or on the frame window, and from outside the
/// two are indistinguishable: both look like "the edit did not apply". This is the third time
/// that shape has cost a game boot on this project.
fn rate_action(motion: u64, frame: f32) -> Option<(bool, Option<HbOverrides>)> {
    let hit = super::action_for(CAT_MOTION_RATE, motion, 0, frame);
    if hit.is_none() && MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < MISS_BUDGET {
        let loaded: Vec<String> = {
            let rules = super::RULES.lock();
            rules
                .iter()
                .filter(|r| r.category == CAT_MOTION_RATE)
                .map(|r| {
                    format!(
                        "[motion={:#x} frames={:?}..{:?}]",
                        r.motion, r.frame_start, r.frame_end
                    )
                })
                .collect()
        };
        crate::slight::diag::note(format!(
            "RATE miss motion={motion:#x} frame={frame:.4} — {} rate rule(s) loaded: {}",
            loaded.len(),
            if loaded.is_empty() {
                "none (the editor sent nothing for this category)".to_string()
            } else {
                loaded.join(" ")
            }
        ));
    }
    hit
}

unsafe fn rate_action_for_call(lua_state: u64, args: &[LuaArg]) -> bool {
    // **Captured first, and unconditionally.** Overriding and capturing are two different jobs:
    // a rule only exists once the user has edited something, while a capture is what lets them
    // see the move at all. The first version of this hook did only the override, so a live
    // capture of a rate-carrying move came back with no rate in it — the value was simply
    // missing from a "⟳ Live" load, and the export then wrote a move with no rate call.
    //
    // Every other family here records on the same line for the same reason; this one was
    // written from the override end and never grew the other half.
    record(lua_state, "FT_MOTION_RATE", args);
    // Every early return reports itself. A branch that bails silently is invisible in exactly
    // the case you are debugging it, which is the lesson three rounds of D1g paid for.
    if !any_rules() {
        if !SAID_NO_RULES.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::slight::diag::note(
                "RATE bail: no rules loaded yet — expected until the editor sends one. \
                 Said once; the report budget is untouched.",
            );
        }
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        bail("battle_object_module_accessor was null");
        return false;
    }
    let Some(current) = args.first().and_then(rate_arg) else {
        bail(&format!(
            "slot 0 carries no number (all {} args: {args:?})",
            args.len()
        ));
        return false;
    };
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let Some((suppress, overrides)) = rate_action(motion, frame) else {
        return false;
    };
    crate::slight::diag::note(format!(
        "RATE hit motion={motion:#x} frame={frame:.4} was={current} suppress={suppress}"
    ));
    // Suppressing a rate call leaves the animation at whatever rate is already in force, which
    // is the preview for "delete this line".
    if suppress {
        return true;
    }
    let Some(rate) = overrides.and_then(|ov| ov.motion_rate) else {
        return false;
    };
    // A rate of zero freezes the animation and nothing below the call would ever run. The editor
    // clamps its own widget, but a rule can arrive from a saved project or an older build, and
    // wedging the fighter is a much worse failure than ignoring one edit.
    if !(rate > 0.0) || !rate.is_finite() {
        bail(&format!(
            "refusing rate {rate}, which would stop the animation"
        ));
        return false;
    }
    let mut vals = args.to_vec();
    // Written back under the tag that arrived, for the reason D1g records: reproducing the shape
    // of the call as well as its value cannot be wrong, and assuming a tag can be.
    vals[0] = match &vals[0] {
        LuaArg::Int(_) => LuaArg::Int(rate as i64),
        _ => LuaArg::Num(rate),
    };
    if vals != args {
        super::rewrite_args(lua_state, &vals);
    }
    false
}

#[skyline::hook(replace = smash::app::sv_animcmd::FT_MOTION_RATE)]
unsafe fn hook_ft_motion_rate(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    if args.len() >= 1 && rate_action_for_call(lua_state, &args) {
        return;
    }
    original!()(lua_state)
}

/// Capture and sparsely override the direct `MotionModule::set_rate` setter.
///
/// This is deliberately separate from [`hook_ft_motion_rate`]: `FT_MOTION_RATE` is an
/// sv_animcmd Lua-stack primitive that establishes a rate window, while this direct lua-bind
/// call is a conditional point setter. The rule is keyed by the pristine numeric argument and
/// the motion/frame, so one setter cannot accidentally retune another call on the same move.
#[skyline::hook(replace = smash::app::lua_bind::MotionModule::set_rate)]
unsafe fn hook_motion_module_set_rate(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    rate: f32,
) {
    let args = [LuaArg::Num(rate)];
    record_for_boma(boma, "MotionModule::set_rate", &args);
    if any_rules() && !boma.is_null() {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = super::numeric_point_key("MotionModule::set_rate", &[rate]);
        if let Some((suppress, overrides)) =
            super::action_for(CAT_MOTION_MODULE_SET_RATE, motion, key, frame)
        {
            if suppress {
                return;
            }
            if let Some(replacement) = overrides.and_then(|item| item.motion_module_rate) {
                // A zero or negative rate stops the animation, so ignore unsafe live values;
                // source/export still carry the measured numeric shape exactly.
                if replacement.is_finite() && replacement > 0.0 && replacement != rate {
                    original!()(boma, replacement);
                    return;
                }
            }
        }
    }
    original!()(boma, rate)
}

/// Capture and sparsely override the direct helper-calculation toggle.
#[skyline::hook(replace = smash::app::lua_bind::MotionModule::set_helper_calculation)]
unsafe fn hook_motion_module_set_helper_calculation(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    enabled: bool,
) {
    let args = [LuaArg::Bool(enabled)];
    record_for_boma(boma, "MotionModule::set_helper_calculation", &args);
    if any_rules() && !boma.is_null() {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = super::numeric_point_key(
            "MotionModule::set_helper_calculation",
            &[if enabled { 1.0 } else { 0.0 }],
        );
        if let Some((suppress, overrides)) =
            super::action_for(CAT_MOTION_MODULE_SET_HELPER_CALCULATION, motion, key, frame)
        {
            if suppress {
                return;
            }
            if let Some(replacement) =
                overrides.and_then(|item| item.motion_module_helper_calculation)
            {
                if replacement != enabled {
                    original!()(boma, replacement);
                    return;
                }
            }
        }
    }
    original!()(boma, enabled)
}

/// Capture and sparsely override the direct partial-animation rate setter.
#[skyline::hook(replace = smash::app::lua_bind::MotionModule::set_rate_partial)]
unsafe fn hook_motion_module_set_rate_partial(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    part_kind: i32,
    rate: f32,
) {
    let args = [LuaArg::Int(part_kind as i64), LuaArg::Num(rate)];
    record_for_boma(boma, "MotionModule::set_rate_partial", &args);
    if any_rules() && !boma.is_null() {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key =
            super::numeric_point_key("MotionModule::set_rate_partial", &[part_kind as f32, rate]);
        if let Some((suppress, overrides)) =
            super::action_for(CAT_MOTION_MODULE_SET_RATE_PARTIAL, motion, key, frame)
        {
            if suppress {
                return;
            }
            if let Some(replacement) = overrides.and_then(|item| item.motion_module_rate_partial) {
                if replacement.is_finite() && replacement > 0.0 && replacement != rate {
                    original!()(boma, part_kind, replacement);
                    return;
                }
            }
        }
    }
    original!()(boma, part_kind, rate)
}

/// Capture and sparsely override the direct partial-animation frame setter.
///
/// The native binding always carries `sync`, even for the three-argument source form the corpus
/// writes: the game's own Lua reader counts its arguments and passes `true` when the call is
/// short. So the capture always has a boolean to report, and a replacement always has one to
/// send — there is no "absent" case to represent on this side of the wire.
#[skyline::hook(replace = smash::app::lua_bind::MotionModule::set_frame_partial)]
unsafe fn hook_motion_module_set_frame_partial(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    part_kind: i32,
    frame_value: f32,
    sync: bool,
) {
    let args = [
        LuaArg::Int(part_kind as i64),
        LuaArg::Num(frame_value),
        LuaArg::Bool(sync),
    ];
    record_for_boma(boma, "MotionModule::set_frame_partial", &args);
    if any_rules() && !boma.is_null() {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        // Keyed on the part kind and the pristine seek frame only. `sync` stays out of the key
        // so an edit that flips it still matches the call it was staged against.
        let key = super::numeric_point_key(
            "MotionModule::set_frame_partial",
            &[part_kind as f32, frame_value],
        );
        if let Some((suppress, overrides)) =
            super::action_for(CAT_MOTION_MODULE_SET_FRAME_PARTIAL, motion, key, frame)
        {
            if suppress {
                return;
            }
            if let Some(item) = overrides {
                // A saved rule or an older editor can still send an invalid float. Keep the
                // native call safe even when the sync flag itself is the only valid edit.
                let replacement = item
                    .motion_module_frame_partial
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .unwrap_or(frame_value);
                let replacement_sync = item.motion_module_frame_partial_sync.unwrap_or(sync);
                if replacement != frame_value || replacement_sync != sync {
                    original!()(boma, part_kind, replacement, replacement_sync);
                    return;
                }
            }
        }
    }
    original!()(boma, part_kind, frame_value, sync)
}

/// Set once the hook is in. Read by `write_capture_diag`.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn installed() -> bool {
    INSTALLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn install() {
    skyline::install_hooks!(
        hook_ft_motion_rate,
        hook_motion_module_set_rate,
        hook_motion_module_set_helper_calculation,
        hook_motion_module_set_rate_partial,
        hook_motion_module_set_frame_partial
    );
    INSTALLED.store(true, std::sync::atomic::Ordering::Relaxed);
    // `diag::note`, not `skyline::println!` — the two go to different places and only one of
    // them is a file anybody reads afterwards. Telling someone to grep diag.txt for a banner
    // that was never written there cost a game boot once already.
    crate::slight::diag::note(
        "ACMD RATE hooks installed (FT_MOTION_RATE, MotionModule::set_rate, MotionModule::set_helper_calculation, MotionModule::set_rate_partial, MotionModule::set_frame_partial)",
    );
}

// No `#[cfg(test)]` module: this crate is not in the workspace, so `cargo test` never builds it
// and a `#[test]` here would be a comment. The editor asserts against this file's source
// instead — see `game_link::tests`.
