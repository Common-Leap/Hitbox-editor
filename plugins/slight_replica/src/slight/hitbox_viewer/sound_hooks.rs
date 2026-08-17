//! Live capture and override for the `PLAY_SE` family — D1 step 6.
//!
//! This module lives under `hitbox_viewer` for the same reason the hurtbox hooks do: a sound is
//! not a collision, but the capture stream, the rule store and the lua-argument plumbing all
//! live here, and a second copy of any of them is a second thing to keep in step.
//!
//! **What the editor can do to a sound, and therefore what this applies.** Name-only edits rewrite
//! the captured hash arguments. Structural flat edits pair a frame-scoped suppression of the
//! pristine call with a typed per-frame injection of the edited call, so removing or moving a
//! sound does not leave the original base call playing. Calls whose authored tail is symbolic or
//! otherwise not measurable remain source/export-only and are deliberately not silenced.

use super::{any_rules, read_args_exact, record, HbOverrides, LuaArg, CAT_SOUND, RULES};

/// The family: macro name, how many leading `Hash40` arguments it takes, and whether one more
/// argument follows them.
///
/// **This is a copy of the editor's `acmd::SOUND_FUNCS` and is deliberately spelled the same
/// way**, tuple shape included, so that the drift check on the editor side is a literal
/// comparison rather than arithmetic over two different encodings. That check —
/// `game_link::tests::the_plugin_sound_table_still_matches_the_editors` — is the only thing
/// keeping the two in step, because this crate is not in the workspace and its own `#[test]`s
/// never run.
///
/// Arities are `smash-script`'s, cross-checked against `sv_animcmd`: all twelve are declared in
/// both, so unlike `AREA_WIND_2ND` (A1) there is no member the plugin can hook but an export
/// cannot write.
const SOUND_FUNCS: &[(&str, usize, bool)] = &[
    ("PLAY_SE", 1, false),
    ("PLAY_SE_NO_3D", 1, false),
    ("PLAY_SE_REMAIN", 1, false),
    ("STOP_SE", 1, false),
    ("PLAY_STEP", 1, false),
    ("PLAY_STEP_FLIPPABLE", 2, false),
    ("PLAY_SEQUENCE", 1, false),
    ("PLAY_STATUS", 1, false),
    ("PLAY_LANDING_SE", 1, false),
    ("PLAY_DOWN_SE", 1, false),
    ("PLAY_FLY_VOICE", 2, false),
    ("SET_PLAY_INHIVIT", 1, true),
];

pub(super) fn injection_arg_count(func: &str) -> Option<usize> {
    SOUND_FUNCS
        .iter()
        .find(|(name, _, _)| *name == func)
        .map(|(_, hashes, has_tail)| hashes + usize::from(*has_tail))
}

/// Normalize the hash slots to the representation the native sound macros actually receive.
/// Captured `PLAY_SE` calls carry their `Hash40` values in integer-tagged Lua slots, even though
/// the source signature names them as hashes. Keeping this conversion at the sound boundary lets
/// the editor wire remain semantically typed while preventing a native macro from seeing the
/// wrong Lua value tag during a structural replay.
pub(super) fn normalize_injection_args(args: &mut [LuaArg], func: &str) {
    let Some(hash_count) = SOUND_FUNCS
        .iter()
        .find(|(name, _, _)| *name == func)
        .map(|(_, hashes, _)| *hashes)
    else {
        return;
    };
    for arg in args.iter_mut().take(hash_count) {
        if let LuaArg::Hash(hash) = arg {
            let hash = *hash;
            *arg = LuaArg::Int(hash as i64);
        }
    }
}

/// A slot-0 sound hash, however the lua stack happens to have typed it.
///
/// **The signature says `Hash40` and the stack says `Int`.** Measured live: Kirby's up tilt
/// passes `se_kirby_swing_l` as `L2CValueType::Int` holding `0x10556b83cc`, which is exactly
/// `hash40("se_kirby_swing_l")` — the right value under the wrong type tag. Matching on the tag
/// rejected every sound in the game, and did it silently, because a rule that never matches and
/// a rule that was never sent look identical from both ends.
///
/// This is the trap this project already records one step over: *the same fact needs a different
/// test on each surface*, and on the live wire an int and a hash are both just numbers. A source
/// parser can tell `Hash40::new("…")` from an integer literal; this cannot, and must not try.
///
/// Masked to 40 bits because that is what `LuaArg::Hash` is normalised to — an unmasked `Int`
/// would not compare equal to the editor's key.
fn hash_arg(arg: &LuaArg) -> Option<u64> {
    match arg {
        LuaArg::Hash(h) => Some(*h & 0xff_ffff_ffff),
        LuaArg::Int(i) => Some((*i as u64) & 0xff_ffff_ffff),
        _ => None,
    }
}

/// How many leading slots of `func` are sounds an override may rewrite.
///
/// An unknown name gets **zero**, not a default of one. This is the bound on every write below,
/// so a name that fell out of the table has to stop being writable rather than quietly keep
/// slot 0 — otherwise removing a member here would leave its rules firing on whatever call
/// matched next.
fn hash_slots(func: &str) -> usize {
    SOUND_FUNCS
        .iter()
        .find(|(name, _, _)| *name == func)
        .map(|(_, hashes, _)| *hashes)
        .unwrap_or(0)
}

/// The suppress/override action for one sound call, if a rule names it.
///
/// **Deliberately not [`super::action_for`], and the reason is the trap this file has paid for
/// twice.** All twelve members share one category and every one of them carries a `Hash40` in
/// slot 0, so a rule written for `PLAY_SE` would apply cleanly and silently to a
/// `PLAY_SE_REMAIN` on the same frame with the same sound — type-correct, and therefore
/// invisible. That is `CAT_ATK_POWER` / `CAT_ATK_SETOFF_MUL` again, except that twelve
/// categories to keep in step across the wire is a worse answer than one extra match. So the
/// rule carries the macro name and it has to agree.
///
/// A rule with no `func` at all matches any member, which is what an editor older than this
/// build sends. That is the documented wire rule — an old field is ignored and the rest applies
/// — read in the only direction that is safe here: too broad rather than silently dead.
fn sound_action(
    motion: u64,
    hash: u64,
    frame: f32,
    func: &str,
) -> Option<(bool, Option<HbOverrides>)> {
    let rules = RULES.lock();
    let hit = rules
        .iter()
        .find(|r| {
            r.inject.is_none()
                && r.matches(CAT_SOUND, motion, hash, frame)
                && r.func.as_deref().map(|f| f == func).unwrap_or(true)
        })
        .map(|r| (r.suppress, r.overrides.clone()));

    // **Say why a rule did not fire.** A live sound edit that does nothing is the failure this
    // family is most exposed to, and every field it can miss on — motion, key, frame window,
    // macro name — is invisible from either side on its own: the editor sent a well-formed rule
    // and the game played the original. Bounded to the first few misses per boot, and
    // `diag::note` buffers rather than doing I/O, so it is safe on an ACMD path.
    if hit.is_none() {
        let seen = MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if seen < MAX_MISS_REPORTS {
            let candidates: Vec<String> = rules
                .iter()
                .filter(|r| r.category == CAT_SOUND && r.inject.is_none())
                .map(|r| {
                    format!(
                        "[motion={:#x} key={:?} frames={:?}..{:?} func={:?}]",
                        r.motion, r.hitbox_id, r.frame_start, r.frame_end, r.func
                    )
                })
                .collect();
            crate::slight::diag::note(format!(
                "SND miss {func} motion={motion:#x} key={hash:#x} frame={frame:.4} — \
                 {} sound rule(s) loaded: {}",
                candidates.len(),
                if candidates.is_empty() {
                    "none (the editor sent nothing for this category)".to_string()
                } else {
                    candidates.join(" ")
                }
            ));
        }
    }
    hit
}

/// Misses reported since the last rule set. Bounded so a move played repeatedly cannot flood.
///
/// **Reset by [`reset_reports`] whenever rules arrive, and that is the whole point.** Budgeting
/// per boot spent all twelve reports on "no rules loaded" during ordinary play — the normal
/// state before any edit — and the rule the user was actually debugging arrived 2400 lines
/// later to a budget that had been empty since the opening seconds. The window worth reporting
/// begins when rules arrive, not when the game boots.
static MISSES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const MAX_MISS_REPORTS: u32 = 12;

/// Whether "no rules loaded" has been said once already.
///
/// Separate from the budget above because it is not a diagnostic event: with no editor
/// connected it is true for every sound in the session, and it is worth saying once so the
/// silence is explained, not twelve times so nothing else can be.
static SAID_NO_RULES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Give the next rule set a fresh reporting budget.
pub(super) fn reset_reports() {
    MISSES.store(0, std::sync::atomic::Ordering::Relaxed);
    SAID_NO_RULES.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Report an early return, bounded, sharing the miss budget.
fn bail(func: &str, why: &str) {
    let seen = MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if seen < MAX_MISS_REPORTS {
        crate::slight::diag::note(format!("SND bail {func}: {why}"));
    }
}

/// Capture one sound call and apply any rule matching it. `true` means suppress it entirely.
///
/// The rule is keyed on the sound the *script* names, not on the one the editor wants played —
/// matching on the edited value would never fire, which is the same rule the hurtbox path
/// states for its target.
unsafe fn sound_action_for_call(lua_state: u64, func: &'static str, args: &[LuaArg]) -> bool {
    record(lua_state, func, args);
    let seen = super::SOUND_CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if seen == 0 {
        // One line per boot, on the first sound this session. `write_capture_diag` only runs at
        // install and at drain, so its counters are a *boot snapshot* until the editor pulls
        // captures — `recorded=0` there means "nothing has drained yet", not "nothing fired",
        // and reading it the other way is what made D1f's second boot unreadable. This is the
        // one signal that says a hook fired without needing the editor connected at all.
        //
        // `diag::note` buffers rather than writing, so this is safe on an ACMD path, and the
        // one-shot keeps it bounded the way `handle_kill_hash` is bounded.
        crate::slight::diag::note(format!("SND first captured sound: {func}"));
    }
    // **Every early return below reports itself.** The first version of this diagnostic only
    // logged a rule *miss*, and the failure turned out to be upstream of the rule lookup — no
    // `SND miss` line appeared at all, which proved only that the diagnostic did not cover the
    // path that was taken. A branch that bails silently is invisible in exactly the case you are
    // trying to debug.
    if !any_rules() {
        if !SAID_NO_RULES.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::slight::diag::note(format!(
                "SND bail {func}: no rules loaded yet — expected until the editor sends one. \
                 Said once; the report budget is untouched."
            ));
        }
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        bail(func, "battle_object_module_accessor was null");
        return false;
    }
    let Some(key) = args.first().and_then(hash_arg) else {
        bail(
            func,
            &format!("slot 0 carries no hash (all {} args: {args:?})", args.len()),
        );
        return false;
    };
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let Some((suppress, overrides)) = sound_action(motion, key, frame, func) else {
        return false;
    };
    crate::slight::diag::note(format!(
        "SND hit {func} motion={motion:#x} key={key:#x} frame={frame:.4} suppress={suppress}"
    ));
    if suppress {
        return true;
    }
    let Some(ov) = overrides else {
        crate::slight::diag::note("SND     rule matched but carried no overrides");
        return false;
    };
    let Some(hashes) = ov.sound_hashes else {
        return false;
    };

    // Positional into the leading hash slots only, and never past what this member declares.
    // `SET_PLAY_INHIVIT` is why the bound is `hash_slots` rather than `vals.len()`: its slot 1
    // is a duration, and writing a hash there would silence the sound for 0x... frames.
    let writable = hash_slots(func).min(args.len());
    let mut vals = args.to_vec();
    for (idx, hash) in hashes.iter().enumerate().take(writable) {
        // **Written back under the same type the script passed**, which is `Int` in practice.
        // Pushing a `Hash`-tagged value where the game handed us an `Int` changes the shape of
        // the call as well as its value, and there is no reason to: the number is the same
        // either way, and reproducing what was there cannot be wrong.
        vals[idx] = match &vals[idx] {
            LuaArg::Hash(_) => LuaArg::Hash(*hash),
            _ => LuaArg::Int(*hash as i64),
        };
    }
    if vals != args {
        super::rewrite_args(lua_state, &vals);
    }
    false
}

/// Hook one member of the family.
///
/// The arity is fixed per macro rather than read from the stack: `read_args_exact` is what every
/// other family here uses, and a short read would hand slot 0 to `sound_action_for_call` from a
/// call whose real slot 0 is somewhere else.
macro_rules! sound_hook {
    ($hook_name:ident, $target:path, $func:literal, $argc:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let args = read_args_exact(lua_state, $argc);
            if args.len() >= $argc && sound_action_for_call(lua_state, $func, &args) {
                return;
            }
            original!()(lua_state)
        }
    };
}

/// Replay one complete sound call from the per-frame structural injector. The caller has already
/// pushed the typed arguments onto the current Lua stack; dispatching through the native macro is
/// what keeps the call's ordinary sound-bank and 3D/stop semantics intact.
pub(super) unsafe fn inject(lua_state: u64, func: &str) -> bool {
    match func {
        "PLAY_SE" => smash::app::sv_animcmd::PLAY_SE(lua_state),
        "PLAY_SE_NO_3D" => smash::app::sv_animcmd::PLAY_SE_NO_3D(lua_state),
        "PLAY_SE_REMAIN" => smash::app::sv_animcmd::PLAY_SE_REMAIN(lua_state),
        "STOP_SE" => smash::app::sv_animcmd::STOP_SE(lua_state),
        "PLAY_STEP" => smash::app::sv_animcmd::PLAY_STEP(lua_state),
        "PLAY_STEP_FLIPPABLE" => smash::app::sv_animcmd::PLAY_STEP_FLIPPABLE(lua_state),
        "PLAY_SEQUENCE" => smash::app::sv_animcmd::PLAY_SEQUENCE(lua_state),
        "PLAY_STATUS" => smash::app::sv_animcmd::PLAY_STATUS(lua_state),
        "PLAY_LANDING_SE" => smash::app::sv_animcmd::PLAY_LANDING_SE(lua_state),
        "PLAY_DOWN_SE" => smash::app::sv_animcmd::PLAY_DOWN_SE(lua_state),
        "PLAY_FLY_VOICE" => smash::app::sv_animcmd::PLAY_FLY_VOICE(lua_state),
        "SET_PLAY_INHIVIT" => smash::app::sv_animcmd::SET_PLAY_INHIVIT(lua_state),
        _ => return false,
    }
    true
}

sound_hook!(hook_play_se, smash::app::sv_animcmd::PLAY_SE, "PLAY_SE", 1);
sound_hook!(
    hook_play_se_no_3d,
    smash::app::sv_animcmd::PLAY_SE_NO_3D,
    "PLAY_SE_NO_3D",
    1
);
sound_hook!(
    hook_play_se_remain,
    smash::app::sv_animcmd::PLAY_SE_REMAIN,
    "PLAY_SE_REMAIN",
    1
);
sound_hook!(hook_stop_se, smash::app::sv_animcmd::STOP_SE, "STOP_SE", 1);
sound_hook!(
    hook_play_step,
    smash::app::sv_animcmd::PLAY_STEP,
    "PLAY_STEP",
    1
);
sound_hook!(
    hook_play_sequence,
    smash::app::sv_animcmd::PLAY_SEQUENCE,
    "PLAY_SEQUENCE",
    1
);
sound_hook!(
    hook_play_status,
    smash::app::sv_animcmd::PLAY_STATUS,
    "PLAY_STATUS",
    1
);
sound_hook!(
    hook_play_landing_se,
    smash::app::sv_animcmd::PLAY_LANDING_SE,
    "PLAY_LANDING_SE",
    1
);
sound_hook!(
    hook_play_down_se,
    smash::app::sv_animcmd::PLAY_DOWN_SE,
    "PLAY_DOWN_SE",
    1
);
sound_hook!(
    hook_play_step_flippable,
    smash::app::sv_animcmd::PLAY_STEP_FLIPPABLE,
    "PLAY_STEP_FLIPPABLE",
    2
);
sound_hook!(
    hook_play_fly_voice,
    smash::app::sv_animcmd::PLAY_FLY_VOICE,
    "PLAY_FLY_VOICE",
    2
);
sound_hook!(
    hook_set_play_inhivit,
    smash::app::sv_animcmd::SET_PLAY_INHIVIT,
    "SET_PLAY_INHIVIT",
    2
);

/// Set once the twelve hooks are in. Read by `write_capture_diag`.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn installed() -> bool {
    INSTALLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn install() {
    skyline::install_hooks!(
        hook_play_se,
        hook_play_se_no_3d,
        hook_play_se_remain,
        hook_stop_se,
        hook_play_step,
        hook_play_sequence,
        hook_play_status,
        hook_play_landing_se,
        hook_play_down_se,
        hook_play_step_flippable,
        hook_play_fly_voice,
        hook_set_play_inhivit
    );
    INSTALLED.store(true, std::sync::atomic::Ordering::Relaxed);
    // **`diag::note`, not `skyline::println!`.** The two go to different places and only one of
    // them is a file anybody reads afterwards: `println!` reaches the Skyline log, while
    // `sd:/slight/diag.txt` is written by `diag`. D1f's first boot was checked by grepping
    // diag.txt for the `println!` banner beside this one, found nothing, and proved nothing —
    // the hooks may well have been fine. A banner nobody can find is not a banner.
    crate::slight::diag::note("ACMD SOUND hooks installed (12 macros, capture + rules)");
    skyline::println!("[SLight] ACMD SOUND hooks installed (capture + rules)");
}

// No `#[cfg(test)]` module here on purpose. This crate is not a member of the repo's workspace
// — `cargo test` at the root never builds it, and building it on the host fails on aarch64
// inline assembly elsewhere in the plugin. A test written here would be a comment that looks
// like a gate. Everything about this file that can be checked off the source is checked from
// `src/game_link.rs`, which is where the suite actually runs.
