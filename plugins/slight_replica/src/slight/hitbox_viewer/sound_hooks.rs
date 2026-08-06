//! Live capture and override for the `PLAY_SE` family — D1 step 6.
//!
//! This module lives under `hitbox_viewer` for the same reason the hurtbox hooks do: a sound is
//! not a collision, but the capture stream, the rule store and the lua-argument plumbing all
//! live here, and a second copy of any of them is a second thing to keep in step.
//!
//! **What the editor can do to a sound, and therefore what this applies.** D1d scoped editing to
//! *which sound a call plays* — retiming, adding and deleting are out of scope there because a
//! sound's frame is the block it sits in rather than an argument. So a rule can rewrite the hash
//! arguments, or suppress the call outright (the preview for "mute this"). Nothing here can move
//! a call to another frame, and the editor never asks it to.

use super::{
    any_rules, read_args_exact, record, HbOverrides, LuaArg, CAT_SOUND, RULES,
};

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
fn sound_action(motion: u64, hash: u64, frame: f32, func: &str) -> Option<(bool, Option<HbOverrides>)> {
    let rules = RULES.lock();
    rules
        .iter()
        .find(|r| {
            r.inject.is_none()
                && r.matches(CAT_SOUND, motion, hash, frame)
                && r.func.as_deref().map(|f| f == func).unwrap_or(true)
        })
        .map(|r| (r.suppress, r.overrides.clone()))
}

/// Capture one sound call and apply any rule matching it. `true` means suppress it entirely.
///
/// The rule is keyed on the sound the *script* names, not on the one the editor wants played —
/// matching on the edited value would never fire, which is the same rule the hurtbox path
/// states for its target.
unsafe fn sound_action_for_call(lua_state: u64, func: &'static str, args: &[LuaArg]) -> bool {
    record(lua_state, func, args);
    super::SOUND_CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !any_rules() {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    let key = match args.first() {
        Some(LuaArg::Hash(h)) => *h,
        // Every member declares slot 0 as `Hash40`. Anything else means the call was written by
        // hand with the wrong type, and there is no key to match on — leave it alone rather than
        // coercing a number into a hash and firing a rule meant for a different sound.
        _ => return false,
    };
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let Some((suppress, overrides)) = sound_action(motion, key, frame, func) else {
        return false;
    };
    if suppress {
        return true;
    }
    let Some(ov) = overrides else {
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
        vals[idx] = LuaArg::Hash(*hash);
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
