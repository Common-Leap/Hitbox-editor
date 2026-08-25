//! hitbox_viewer — live hitbox editing + ACMD capture.
//!
//! Mirrors effect_viewer's ACMD approach for the ATTACK family:
//!   * the first playback of each fighter-kind + motion pair is CAPTURED (pristine args +
//!     motion + frame) and streamed to the editor as `AcmdCapture` — later playbacks cannot
//!     replace or merge into that snapshot;
//!   * `hitbox_rules` (full-list replace over TCP) modify args at spawn (rewrite), suppress
//!     a hitbox entirely (skip original), or INJECT a synthesized ATTACK at a motion frame
//!     from the per-frame line callback.
//!
//! ATTACK lua arg layout (0-based, from smash_script macros::ATTACK — the push order):
//!   0 id, 1 part, 2 bone(h), 3 damage, 4 angle, 5 kbg, 6 fkb, 7 bkb, 8 size,
//!   9 x, 10 y, 11 z, 12..14 x2/y2/z2 (nil = sphere), 15 hitlag, 16 sdi, 17 clang,
//!   18 facing, 19 set_weight, 20 shield_damage, 21 trip, 22 rehit, 23 reflectable,
//!   24 absorbable, 25 flinchless, 26 disable_hitlag, 27 direct, 28 ground_air,
//!   29 hitbits, 30 collision_part, 31 friendly_fire, 32 effect(h), 33 sfx_level,
//!   34 collision_sound, 35 type.
//!
//! Those are `smash_script`'s parameter names, which are a stale Smash-4-era RE pass. The
//! decompiled Ultimate scripts (and the editor) name the SAME positions differently — this
//! is a naming difference only, the slots line up exactly:
//!   17 ATTACK_SETOFF_KIND, 18 ATTACK_LR_CHECK, 19 is_clang, 20 is_add_attack,
//!   21 hitbox_attr, 22 ground_or_air, 23 is_mtk, 24 is_shield_disable, 25 is_reflectable,
//!   26 is_absorbable, 27 is_landing_attack, 28 COLLISION_SITUATION_MASK,
//!   29 COLLISION_CATEGORY_MASK, 30 COLLISION_PART_MASK, 31 no_finish_camera,
//!   32 collision_attr(h), 33 ATTACK_SOUND_LEVEL, 34 COLLISION_SOUND_ATTR, 35 ATTACK_REGION.
//! Slots 1..35 are rewritable from `HbOverrides` (see `rewrite_attack_args`).

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use parking_lot::Mutex;
use smash::lib::{L2CValue, L2CValueType};

mod capture_archive;
mod rate_hooks;
mod sound_hooks;

// ── Typed lua args (wire + capture form) ─────────────────────────────────────

/// One lua argument, typed — losslessly round-trips capture → editor → inject.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum LuaArg {
    #[serde(rename = "h")]
    Hash(u64),
    #[serde(rename = "n")]
    Num(f32),
    #[serde(rename = "i")]
    Int(i64),
    #[serde(rename = "b")]
    Bool(bool),
    #[serde(rename = "x")]
    Nil,
}

impl LuaArg {
    fn from_l2c(v: &L2CValue) -> Self {
        unsafe {
            match v.val_type {
                L2CValueType::Hash => LuaArg::Hash(v.inner.raw & 0xff_ffff_ffff),
                L2CValueType::Num => LuaArg::Num(v.inner.raw_float),
                L2CValueType::Int => LuaArg::Int(v.inner.raw as i64),
                L2CValueType::Bool => LuaArg::Bool(v.inner.raw & 1 != 0),
                _ => LuaArg::Nil,
            }
        }
    }

    pub(crate) fn to_l2c(&self) -> L2CValue {
        match self {
            LuaArg::Hash(h) => L2CValue::new_hash(*h),
            LuaArg::Num(n) => L2CValue::new_num(*n),
            LuaArg::Int(i) => L2CValue::new_int(*i as u64),
            LuaArg::Bool(b) => L2CValue::new_bool(*b),
            LuaArg::Nil => L2CValue::new_void(),
        }
    }

    fn dedupe_bits(&self) -> u64 {
        match self {
            LuaArg::Hash(h) => 0x1000_0000_0000_0000 ^ h,
            LuaArg::Num(n) => 0x2000_0000_0000_0000 ^ (n.to_bits() as u64),
            LuaArg::Int(i) => 0x3000_0000_0000_0000 ^ (*i as u64),
            LuaArg::Bool(b) => 0x4000_0000_0000_0000 ^ (*b as u64),
            LuaArg::Nil => 0x5000_0000_0000_0000,
        }
    }
}

/// Last path segment of a stringify!'d target: "smash :: app :: sv_animcmd :: EFFECT" →
/// "EFFECT" (slices of 'static strs stay 'static).
pub fn short_func(s: &'static str) -> &'static str {
    s.rsplit("::").next().map(str::trim).unwrap_or(s)
}

/// Read every lua arg with its type (stops at the first Void). Non-destructive.
/// Fine for the EFFECT family (no nil slots); ATTACK needs `read_args_exact` — its capsule
/// slots are pushed as Void for sphere hitboxes and must not truncate the read.
pub unsafe fn read_args_typed(lua_state: u64, max: i32) -> Vec<LuaArg> {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let mut out = Vec::new();
    for i in 1..=max {
        let v: L2CValue = agent.pop_lua_stack(i);
        if matches!(v.val_type, L2CValueType::Void) {
            break;
        }
        out.push(LuaArg::from_l2c(&v));
    }
    out
}

/// Read exactly `n` args (Void slots become Nil) — for fixed-arity calls like ATTACK (36).
pub unsafe fn read_args_exact(lua_state: u64, n: i32) -> Vec<LuaArg> {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    (1..=n)
        .map(|i| {
            let v: L2CValue = agent.pop_lua_stack(i);
            LuaArg::from_l2c(&v)
        })
        .collect()
}

/// ATTACK / ATTACK_IGNORE_THROW arity.
const ATTACK_ARGC: i32 = 36;
/// CATCH (grabbox) arity: id, bone(h), size, x, y, z, x2?, y2?, z2?, status, situation.
const CATCH_ARGC: i32 = 11;
/// `ATTACK_ABS` takes sixteen — a different family, not a short `ATTACK`.
const ATTACK_ABS_ARGC: i32 = 16;
/// `ATTACK_FP` takes 41 arguments after `agent` and has its own layout.
const ATTACK_FP_ARGC: i32 = 41;
/// `SEARCH`: id, part, bone, size, x/y/z, the capsule triple, collision kind, hit status, an
/// undocumented int, then the situation/category/part masks and a trailing flag.
const SEARCH_ARGC: i32 = 17;
/// Max args to probe for the AREA_WIND family (all floats, variable arity 8..10).
const WIND_ARGC_MAX: i32 = 12;

/// Collision family a capture / rule belongs to. Kept in sync with the editor
/// (`Hitbox.category`): 0 = attack, 1 = grab (CATCH), 2 = wind (AREA_WIND).
pub const CAT_ATTACK: u8 = 0;
pub const CAT_GRAB: u8 = 1;
pub const CAT_WIND: u8 = 2;
/// `ATTACK_ABS` — throw/catch damage with no volume. Its own family because the argument
/// layout shares nothing positionally with `ATTACK`, and its rule key is the absolute kind
/// rather than the id, which every vanilla call writes as 0.
pub const CAT_ABS: u8 = 4;
/// `ATTACK_FP` — fighter-position collision with a separate 41-slot layout.
/// **Must equal the editor's `game_link::CAT_ATTACK_FP`.**
pub const CAT_ATTACK_FP: u8 = 12;
/// Hurtbox state (`HIT_NODE` / `HIT_NO` / `WHOLE_HIT`), which is not a collision at all — it
/// changes how the fighter *receives* hits. It rides the same rule pipeline because the matching
/// key is the same shape (motion + target + frame window), but it is deliberately absent from
/// [`is_collision_func`]: that gate exists to note when something is out to be cleared, and a
/// hurtbox state is never ended by `AttackModule::clear_all`.
///
/// The rule's `hitbox_id` carries the bone hash for `HIT_NODE` and the group number for
/// `HIT_NO`, which cannot collide: a hash40 of a real bone name never lands in the low integers
/// the group form uses. The two members with no target of their own — `WHOLE_HIT` and `COL_PRI`
/// — take the top of the range instead, one sentinel each: [`HURT_KEY_WHOLE`] and
/// [`HURT_KEY_COL_PRI`].
pub const CAT_HURT: u8 = 3;

/// `ATK_POWER` — retune the damage of a hitbox that is already out. Keyed by that hitbox's id.
///
/// A category per macro rather than one shared "post-hoc tuning" category keyed by id: the two
/// members can legally name the same id in the same frame window, and one category would then
/// let an `ATK_POWER` rule fire on an `ATK_SET_SHIELD_SETOFF_MUL` call and write damage into a
/// shield multiplier. **Must equal the editor's `game_link::CAT_ATK_POWER`.**
pub const CAT_ATK_POWER: u8 = 5;

/// `ATK_SET_SHIELD_SETOFF_MUL` — scale the shield push-off of a hitbox already out.
/// **Must equal the editor's `game_link::CAT_ATK_SETOFF_MUL`.**
pub const CAT_ATK_SETOFF_MUL: u8 = 6;

/// `SEARCH` — a detection volume. Keyed by its id, like an attack hitbox.
///
/// **Must equal the editor's `game_link::CAT_SEARCH`.** Note that a search box's *display*
/// category in the editor is `4`, which is this file's [`CAT_ABS`]: the two numbering spaces
/// stopped agreeing at `ATTACK_ABS`, and the editor now converts with
/// `game_link::wire_category`. A category on the wire is not a `Hitbox.category`.
pub const CAT_SEARCH: u8 = 7;

/// The `PLAY_SE` family — a sound the script starts or stops. **Must equal the editor's
/// `game_link::CAT_SOUND`.**
///
/// One category for all twelve members, which is the opposite of the choice
/// [`CAT_ATK_POWER`] records, and the reason is worth stating because it looks inconsistent.
/// There the two members' slot 1 meant *different things*, so a misapplied rule wrote damage
/// into a shield multiplier — a wrong value in a real field. Here every member declares a
/// `Hash40` in slot 0 and nothing else is ever written, so a misapplied rule can only ever put
/// a sound where a sound goes. Twelve categories to keep in step across the wire is a worse
/// trade than one name comparison, so the *macro name* travels on the rule instead and
/// `sound_hooks::sound_action` requires it to agree.
///
/// Not in [`is_collision_func`], for the reason `SEARCH` is not: nothing clears a sound.
pub const CAT_SOUND: u8 = 8;

/// `FT_MOTION_RATE` — the animation playback rate, not a collision at all.
///
/// One category for all three rate macros, because `smash-script` compiles
/// `FT_MOTION_RATE_RANGE` and `FT_DESIRED_RATE` into the same `sv_animcmd::FT_MOTION_RATE` call.
/// A rule carries no id: a rate call has nothing to key on but its motion and its frame.
///
/// Not in [`is_collision_func`], for the reason `SEARCH` and `CAT_SOUND` are not: nothing clears
/// a rate.
pub const CAT_MOTION_RATE: u8 = 9;

/// The measured `expression_` camera/rumble primitives. The exact call name is carried on the
/// rule because the members have different argument shapes and one is a direct native binding.
pub const CAT_EXPRESSION: u8 = 10;

/// `REVERSE_LR` — an argument-less facing-direction point in a `game_` script.
pub const CAT_REVERSE_LR: u8 = 11;

/// `SET_SPEED_EX` — a verified three-argument velocity point.
pub const CAT_SPEED_EX: u8 = 13;

/// `SET_SPEED` — a verified direct x/y velocity point.
pub const CAT_SPEED: u8 = 16;

/// `ADD_SPEED_NO_LIMIT` — a verified x/y velocity-addition point.
pub const CAT_ADD_SPEED_NO_LIMIT: u8 = 14;

/// `CORRECT` — a verified numeric ground-correction point.
pub const CAT_CORRECT: u8 = 15;

/// `FT_CATCH_STOP` — a verified two-argument numeric point.
pub const CAT_FT_CATCH_STOP: u8 = 17;

/// `FT_START_ADJUST_MOTION_FRAME_arg1` — a verified one-argument numeric point.
pub const CAT_FT_START_ADJUST_MOTION_FRAME: u8 = 18;

/// `CLR_SPEED` — clear one named kinetic-energy reserve. Must equal the editor's wire category.
pub const CAT_CLR_SPEED: u8 = 19;

/// `SET_AIR` — an argument-less kinetic-state point. Must equal the editor's wire category.
pub const CAT_SET_AIR: u8 = 20;

/// `KineticModule::change_kinetic` — a direct kinetic-type point. Must equal the editor's wire category.
pub const CAT_CHANGE_KINETIC: u8 = 21;

/// `KineticModule::add_speed` — a direct x/y vector point. Must equal the editor's wire category.
pub const CAT_KINETIC_ADD_SPEED: u8 = 22;

/// `KineticModule::suspend_energy` — a direct kinetic-energy point. Must equal the editor's wire category.
pub const CAT_KINETIC_SUSPEND_ENERGY: u8 = 23;

/// `KineticModule::resume_energy` — a direct kinetic-energy point. Must equal the editor's wire category.
pub const CAT_KINETIC_RESUME_ENERGY: u8 = 24;

/// `KineticModule::enable_energy` — a direct kinetic-energy point. Must equal the editor's wire category.
pub const CAT_KINETIC_ENABLE_ENERGY: u8 = 25;

/// `KineticModule::unable_energy` — a direct kinetic-energy point. Must equal the editor's wire category.
pub const CAT_KINETIC_UNABLE_ENERGY: u8 = 26;

/// `KineticModule::clear_speed_all` — an argument-less direct kinetic point. Must equal the editor's wire category.
pub const CAT_KINETIC_CLEAR_SPEED_ALL: u8 = 27;

/// `KineticModule::set_consider_ground_friction` — a direct bool/attribute kinetic point.
/// Must equal the editor's wire category.
pub const CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION: u8 = 28;

/// Direct `MotionModule::set_rate` point. Must equal the editor's wire category.
pub const CAT_MOTION_MODULE_SET_RATE: u8 = 29;

/// Direct `MotionModule::set_helper_calculation` point. Must equal the editor's wire category.
pub const CAT_MOTION_MODULE_SET_HELPER_CALCULATION: u8 = 30;

/// Direct `MotionModule::set_rate_partial` point. Must equal the editor's wire category.
pub const CAT_MOTION_MODULE_SET_RATE_PARTIAL: u8 = 31;

/// Direct `WorkModule::on_flag` / `off_flag` point. Must equal the editor's wire category.
pub const CAT_WORK_FLAG: u8 = 32;

/// Direct `WorkModule::enable_transition_term` / `unable_transition_term` point. Must equal the
/// editor's wire category.
pub const CAT_WORK_TRANSITION_TERM: u8 = 33;

/// Direct `WorkModule::set_int` / `set_float` / `set_int64` point. Must equal the editor's wire
/// category.
pub const CAT_WORK_MODULE_SET: u8 = 34;

/// Direct `WorkModule::inc_int` point. Must equal the editor's wire category.
pub const CAT_WORK_MODULE_INC_INT: u8 = 35;

/// Direct `MotionModule::set_frame_partial` point. Must equal the editor's wire category.
pub const CAT_MOTION_MODULE_SET_FRAME_PARTIAL: u8 = 36;

/// Targetless rule key for `SET_AIR`. Must equal `game_link::KINETIC_KEY_SET_AIR`.
const KINETIC_KEY_SET_AIR: u64 = u64::MAX - 2;

/// Targetless rule key for `KineticModule::clear_speed_all`. Must equal the editor's wire key.
const KINETIC_KEY_CLEAR_SPEED_ALL: u64 = u64::MAX - 3;

// ── Capture (live ACMD stream) ───────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptureLine {
    /// Fighter kind (smash::app::utility::get_kind) of the performing agent.
    pub kind: i32,
    /// MotionModule::motion_kind (hash40 of the motion name, e.g. "attack_air_n").
    pub motion: u64,
    /// MotionModule::frame at call time.
    pub frame: f32,
    /// The sv_animcmd function, e.g. "ATTACK", "EFFECT_FOLLOW".
    pub func: &'static str,
    /// Direct lua-bind hooks do not retain the source ACMD category. The editor must therefore
    /// reject ambiguous `game_`/`expression_` matches for shared binding primitives.
    pub args: Vec<LuaArg>,
    /// Which PLAYBACK of the motion produced this line. See [`next_run`].
    pub run: u32,
    /// True when the call came from the game's COMMON animcmd agent rather than the fighter's
    /// own move script — status feedback such as invincibility flashing or damage burn, which
    /// shares the move's fighter/motion/frame but is not part of the move.
    ///
    /// Tagged rather than dropped. A dropped line is also a lost injection boundary and a lost
    /// donor, which is how filtering status noise and applying live edits ended up fighting each
    /// other; the editor decides what a common line may be used for.
    pub common: bool,
}

/// One completed motion playback: "every capture line this motion is going to produce has
/// been recorded". Streamed to the editor as `AcmdCaptureEnd` so it can adopt a COMPLETE
/// script instead of whatever had arrived a few frames into the move.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CaptureEnd {
    pub kind: i32,
    pub motion: u64,
    /// The playback that finished — pairs with [`CaptureLine::run`].
    pub run: u32,
}

/// Run ids identify ONE playback of a motion by one battle object.
///
/// Without them the editor could not tell two performances apart. Its capture store is keyed
/// by motion and appends, so performing a move under different conditions — grounded then
/// aerial, a cancel, a branch that only fires sometimes — left the UNION of those runs in the
/// bucket and the editor showed spawns that never occur together.
///
/// Ids are global and strictly increasing. A `(fighter kind, motion)` is claimed by the first
/// battle object that produces a capture line and is never assigned another run until captures
/// are explicitly cleared. This keeps another instance, replay, cancel follow-up, or reconnect
/// from replacing a completed snapshot.
static NEXT_RUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn next_run() -> u32 {
    NEXT_RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One immutable capture owner per fighter-kind + motion. The claim exists as soon as the first
/// line lands, so another battle object cannot interleave a second run while the move is still
/// executing. It remains after completion so later activity cannot supersede the snapshot.
#[derive(Clone, Copy)]
struct CaptureClaim {
    boid: u32,
    run: u32,
    /// Whether this run's playback reached `end_frame` — the move genuinely finished, as opposed
    /// to being suspended part-way by a charge hold.
    ///
    /// **This is what separates resuming a capture from starting a new one, and both mistakes
    /// are visible.** Never resuming loses everything after a smash attack's charge (R11). Always
    /// resuming piles every later performance into one snapshot — and they do not collapse into
    /// each other, because a charged smash releases at a different motion frame each time, so the
    /// dedupe key differs and the timeline fills with duplicates (R13).
    ended: bool,
    /// Prevent duplicate completion records when the end-frame and motion-switch observers fire
    /// on adjacent callbacks after the pending end has already been drained.
    end_sent: bool,
}

/// Bounded report budget for the claim-collision drop in `mark_capture_motion`.
static CLAIM_DROPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set by the TCP server thread and consumed on the game thread before the next capture drain.
static CLEAR_CAPTURES_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// What one battle object is currently playing, for end-of-motion detection.
#[derive(Clone, Copy)]
struct MotionWatch {
    motion: u64,
    kind: i32,
    /// Last MotionModule::frame seen — a decrease means the motion restarted.
    frame: f32,
    /// A non-duplicate capture line was recorded during THIS playback, so an end marker
    /// is worth sending (otherwise every idle/walk motion would emit one every second).
    captured: bool,
    /// End already announced for this playback.
    ended: bool,
    /// Run id of this playback.
    run: u32,
    /// A collision (ATTACK / CATCH / AREA_WIND) has come out since the last clear.
    ///
    /// Gates clear-all capture. `AttackModule::clear_all` is called by the engine constantly —
    /// on state changes, every frame in some situations — and recording all of that would
    /// bury the script's own clears. Only a clear that actually ENDS something is a script
    /// event worth streaming.
    open_collisions: bool,
}

/// boid → current motion playback. Bounded by the live battle-object count.
/// Capture ownership, deduplication, completion, and delivery are one transaction.
///
/// Hooks can run from more than one game worker. Parking one worker on a mutex is not safe in
/// Skyline: the waiter may never be scheduled again even after the holder releases it. Keeping
/// these related maps under one mutex also prevents the old partial-update cases where a claim
/// was installed but its motion watch was not. Every game-facing access uses `try_lock`; losing
/// one contended sample is recoverable, while parking the game thread freezes the match.
#[derive(Default)]
struct CaptureState {
    seen: HashMap<u32, HashSet<u64>>,
    pending: Vec<CaptureLine>,
    ends: Vec<CaptureEnd>,
    claims: HashMap<(i32, u64), CaptureClaim>,
    watch: HashMap<u32, MotionWatch>,
}

static CAPTURE_STATE: LazyLock<Mutex<CaptureState>> =
    LazyLock::new(|| Mutex::new(CaptureState::default()));

/// Capture operations skipped instead of parking a game worker under contention.
static CAPTURE_CONTENTION_DROPS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Compact, lock-free capture accounting keyed exactly like the capture cache: fighter kind +
/// motion. Hook workers only touch atomics; the low-frequency network facade snapshots changed
/// rows for the desktop. This must never acquire `CAPTURE_STATE`, especially on the failure path
/// whose whole purpose is reporting that `CAPTURE_STATE` was contended.
const CAPTURE_DEBUG_SLOTS: usize = 256;
const CAPTURE_DEBUG_RESERVED: u64 = 1;

struct CaptureDebugSlot {
    fingerprint: std::sync::atomic::AtomicU64,
    kind: std::sync::atomic::AtomicI32,
    motion: std::sync::atomic::AtomicU64,
    attempted: std::sync::atomic::AtomicU64,
    succeeded: std::sync::atomic::AtomicU64,
    contention: std::sync::atomic::AtomicU64,
    claim_conflicts: std::sync::atomic::AtomicU64,
    duplicates: std::sync::atomic::AtomicU64,
    last_frame: std::sync::atomic::AtomicU32,
    last_sent: std::sync::atomic::AtomicU64,
}

impl CaptureDebugSlot {
    const fn new() -> Self {
        Self {
            fingerprint: std::sync::atomic::AtomicU64::new(0),
            kind: std::sync::atomic::AtomicI32::new(0),
            motion: std::sync::atomic::AtomicU64::new(0),
            attempted: std::sync::atomic::AtomicU64::new(0),
            succeeded: std::sync::atomic::AtomicU64::new(0),
            contention: std::sync::atomic::AtomicU64::new(0),
            claim_conflicts: std::sync::atomic::AtomicU64::new(0),
            duplicates: std::sync::atomic::AtomicU64::new(0),
            last_frame: std::sync::atomic::AtomicU32::new(0),
            last_sent: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

static CAPTURE_DEBUG: [CaptureDebugSlot; CAPTURE_DEBUG_SLOTS] =
    [const { CaptureDebugSlot::new() }; CAPTURE_DEBUG_SLOTS];

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CaptureDebugSnapshot {
    pub kind: i32,
    pub motion: u64,
    pub attempted: u64,
    pub succeeded: u64,
    pub contention: u64,
    pub claim_conflicts: u64,
    pub duplicates: u64,
    pub last_frame: f32,
}

fn capture_debug_fingerprint(kind: i32, motion: u64) -> u64 {
    // Reserve 0 for empty and 1 while a writer publishes the stable key fields.
    let fingerprint = fnv(fnv(0xcbf29ce484222325, kind as u64), motion);
    if fingerprint <= CAPTURE_DEBUG_RESERVED {
        fingerprint + 2
    } else {
        fingerprint
    }
}

fn capture_debug_attempt(kind: i32, motion: u64, frame: f32) -> Option<&'static CaptureDebugSlot> {
    use std::sync::atomic::Ordering;

    let fingerprint = capture_debug_fingerprint(kind, motion);
    let start = fingerprint as usize % CAPTURE_DEBUG_SLOTS;
    for offset in 0..CAPTURE_DEBUG_SLOTS {
        let slot = &CAPTURE_DEBUG[(start + offset) % CAPTURE_DEBUG_SLOTS];
        let found = slot.fingerprint.load(Ordering::Acquire);
        if found == fingerprint
            && slot.kind.load(Ordering::Relaxed) == kind
            && slot.motion.load(Ordering::Relaxed) == motion
        {
            slot.attempted.fetch_add(1, Ordering::Relaxed);
            slot.last_frame.store(frame.to_bits(), Ordering::Relaxed);
            return Some(slot);
        }
        if found == 0
            && slot
                .fingerprint
                .compare_exchange(
                    0,
                    CAPTURE_DEBUG_RESERVED,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            slot.kind.store(kind, Ordering::Relaxed);
            slot.motion.store(motion, Ordering::Relaxed);
            slot.last_frame.store(frame.to_bits(), Ordering::Relaxed);
            slot.attempted.store(1, Ordering::Relaxed);
            slot.fingerprint.store(fingerprint, Ordering::Release);
            return Some(slot);
        }
    }
    None
}

/// Snapshot changed rows for the desktop. This runs on the throttled facade, never a hook stack.
pub fn take_capture_debug(max: usize) -> Vec<CaptureDebugSnapshot> {
    use std::sync::atomic::Ordering;

    let mut rows = Vec::new();
    for slot in &CAPTURE_DEBUG {
        if rows.len() >= max {
            break;
        }
        if slot.fingerprint.load(Ordering::Acquire) <= CAPTURE_DEBUG_RESERVED {
            continue;
        }
        let attempted = slot.attempted.load(Ordering::Relaxed);
        if attempted == 0 || attempted == slot.last_sent.load(Ordering::Relaxed) {
            continue;
        }
        rows.push(CaptureDebugSnapshot {
            kind: slot.kind.load(Ordering::Relaxed),
            motion: slot.motion.load(Ordering::Relaxed),
            attempted,
            succeeded: slot.succeeded.load(Ordering::Relaxed),
            contention: slot.contention.load(Ordering::Relaxed),
            claim_conflicts: slot.claim_conflicts.load(Ordering::Relaxed),
            duplicates: slot.duplicates.load(Ordering::Relaxed),
            last_frame: f32::from_bits(slot.last_frame.load(Ordering::Relaxed)),
        });
        slot.last_sent.store(attempted, Ordering::Relaxed);
    }
    rows
}

fn reset_capture_debug_sent() {
    for slot in &CAPTURE_DEBUG {
        slot.last_sent
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

fn clear_capture_debug() {
    use std::sync::atomic::Ordering;

    for slot in &CAPTURE_DEBUG {
        slot.attempted.store(0, Ordering::Relaxed);
        slot.succeeded.store(0, Ordering::Relaxed);
        slot.contention.store(0, Ordering::Relaxed);
        slot.claim_conflicts.store(0, Ordering::Relaxed);
        slot.duplicates.store(0, Ordering::Relaxed);
        slot.last_frame.store(0, Ordering::Relaxed);
        slot.last_sent.store(0, Ordering::Relaxed);
        slot.fingerprint.store(0, Ordering::Release);
    }
}

fn try_capture_state() -> Option<parking_lot::MutexGuard<'static, CaptureState>> {
    let state = CAPTURE_STATE.try_lock();
    if state.is_none() {
        CAPTURE_CONTENTION_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    state
}
/// Mirrors `!MOTION_WATCH.is_empty()` so the per-frame watch can bail on one relaxed load.
///
/// `capture_tick` runs for EVERY fighter and article on EVERY frame. Until something is
/// actually captured the map is empty, so without this the whole roster paid a lock plus
/// several `MotionModule` calls per frame to look up an entry that was never there.
static MOTION_WATCH_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Small on-SD counters for distinguishing capture failures from editor/network failures.
static CAPTURE_RECORDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static EFFECT_CAPTURE_RECORDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Sound-family lines recorded, broken out because D1f's first boot could not be read without
/// it: the aggregate counters cannot tell "the hooks are installed and this move has no sounds"
/// from "the hooks never installed". Both are the same number, and they need opposite answers.
static SOUND_CAPTURE_RECORDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_DRAINED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_MOTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_KIND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_capture_diag(stage: &str) {
    use std::sync::atomic::Ordering;

    let pending = try_capture_state()
        .map(|state| state.pending.len())
        .unwrap_or_default();
    let _ = std::fs::write(
        "sd:/effect_viewer_capture.txt",
        format!(
            "stage={stage}\nrecorded={}\neffect_recorded={}\nsound_recorded={}\nsound_hooks={}\nrate_hook={}\ndrained={}\ncontention_drops={}\narchive_drops={}\npending={pending}\nlast_kind={}\nlast_motion={:#x}\nlast_frame={}\n",
            CAPTURE_RECORDED.load(Ordering::Relaxed),
            EFFECT_CAPTURE_RECORDED.load(Ordering::Relaxed),
            SOUND_CAPTURE_RECORDED.load(Ordering::Relaxed),
            // Installed, not merely compiled in. `sound_recorded=0` is ambiguous on its own —
            // a move with no sounds reads the same as a family that never hooked — so the
            // install itself is reported beside the count.
            sound_hooks::installed(),
            rate_hooks::installed(),
            CAPTURE_DRAINED.load(Ordering::Relaxed),
            CAPTURE_CONTENTION_DROPS.load(Ordering::Relaxed),
            capture_archive::dropped(),
            CAPTURE_LAST_KIND.load(Ordering::Relaxed) as i64,
            CAPTURE_LAST_MOTION.load(Ordering::Relaxed),
            f32::from_bits(CAPTURE_LAST_FRAME.load(Ordering::Relaxed) as u32),
        ),
    );
}

/// Lock-free scopes use a fingerprint of the native Switch thread plus the scoped identity.
/// Direct module hooks can run concurrently for different fighters, so neither ACMD execution
/// nor replacement replay may be represented by a process-wide flag.
const HOOK_SCOPE_SLOT_COUNT: usize = 32;
const HOOK_SCOPE_DEPTH_SHIFT: u32 = 56;
const HOOK_SCOPE_KEY_MASK: u64 = (1_u64 << HOOK_SCOPE_DEPTH_SHIFT) - 1;
const HOOK_SCOPE_MAX_DEPTH: u8 = u8::MAX;

fn hook_scope_state(key: u64, depth: u8) -> u64 {
    ((depth as u64) << HOOK_SCOPE_DEPTH_SHIFT) | (key & HOOK_SCOPE_KEY_MASK)
}

fn hook_scope_depth(state: u64) -> u8 {
    (state >> HOOK_SCOPE_DEPTH_SHIFT) as u8
}

fn current_thread_key(identity: u64) -> u64 {
    // `GetCurrentThread` returns the stable nn::os ThreadType for the calling native worker.
    // Hashing that pointer with the scoped identity keeps the atomic entry to one machine word.
    let thread = unsafe { nnsdk::nn::os::GetCurrentThread() } as usize as u64;
    let mut hash = 0xcbf29ce484222325_u64;
    for value in [thread, identity] {
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    let key = hash & HOOK_SCOPE_KEY_MASK;
    // Zero denotes an empty slot, so keep the vanishingly rare zero hash representable.
    if key == 0 {
        1
    } else {
        key
    }
}

fn enter_hook_scope(
    slots: &[std::sync::atomic::AtomicU64; HOOK_SCOPE_SLOT_COUNT],
    key: u64,
) -> Option<usize> {
    use std::sync::atomic::Ordering;

    // Prefer the current thread's existing slot so nested dispatches share a depth counter.
    for (index, slot) in slots.iter().enumerate() {
        let mut observed = slot.load(Ordering::Acquire);
        loop {
            let depth = hook_scope_depth(observed);
            if depth == 0 || observed & HOOK_SCOPE_KEY_MASK != key || depth == HOOK_SCOPE_MAX_DEPTH
            {
                break;
            }
            let next = hook_scope_state(key, depth + 1);
            match slot.compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(index),
                Err(actual) => observed = actual,
            }
        }
    }

    for (index, slot) in slots.iter().enumerate() {
        if slot
            .compare_exchange(
                0,
                hook_scope_state(key, 1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            return Some(index);
        }
    }
    None
}

fn leave_hook_scope(
    slots: &[std::sync::atomic::AtomicU64; HOOK_SCOPE_SLOT_COUNT],
    index: usize,
    key: u64,
) {
    use std::sync::atomic::Ordering;

    let slot = &slots[index];
    let mut observed = slot.load(Ordering::Acquire);
    loop {
        let depth = hook_scope_depth(observed);
        if depth == 0 || observed & HOOK_SCOPE_KEY_MASK != key {
            return;
        }
        let next = if depth == 1 {
            0
        } else {
            hook_scope_state(key, depth - 1)
        };
        match slot.compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(actual) => observed = actual,
        }
    }
}

fn hook_scope_is_active(
    slots: &[std::sync::atomic::AtomicU64; HOOK_SCOPE_SLOT_COUNT],
    key: u64,
) -> bool {
    slots.iter().any(|slot| {
        let state = slot.load(std::sync::atomic::Ordering::Acquire);
        hook_scope_depth(state) != 0 && state & HOOK_SCOPE_KEY_MASK == key
    })
}

/// Replacement dispatches re-enter hooked sv_animcmd functions. This per-thread scope prevents
/// those calls from being captured as authored script lines without suppressing another game
/// worker that happens to execute at the same time.
static INJECTION_SLOTS: [std::sync::atomic::AtomicU64; HOOK_SCOPE_SLOT_COUNT] =
    [const { std::sync::atomic::AtomicU64::new(0) }; HOOK_SCOPE_SLOT_COUNT];
const INJECTION_SCOPE_IDENTITY: u64 = 0x494e_4a45_4354_494f;

/// Whether the current ACMD hook call is the replacement side of a live injection.
///
/// This is deliberately separate from the injection-attempt latch in the effect viewer. The
/// guard only answers the capture/suppression question: replacement calls must not be captured as
/// authored script lines, and an authored suppression rule must not suppress the replacement
/// itself.
pub fn is_injecting() -> bool {
    hook_scope_is_active(
        &INJECTION_SLOTS,
        current_thread_key(INJECTION_SCOPE_IDENTITY),
    )
}

/// RAII guard: capture recording is suspended while this is alive.
pub struct InjectGuard {
    slot: Option<usize>,
    key: u64,
}

impl InjectGuard {
    pub fn new() -> Self {
        let key = current_thread_key(INJECTION_SCOPE_IDENTITY);
        Self {
            slot: enter_hook_scope(&INJECTION_SLOTS, key),
            key,
        }
    }
}

impl Drop for InjectGuard {
    fn drop(&mut self) {
        if let Some(index) = self.slot {
            leave_hook_scope(&INJECTION_SLOTS, index, self.key);
        }
    }
}

/// A direct rule is still narrowed by fighter motion, command, argument identity, and frame.
/// Coroutine start/resume calls cannot provide an additional execution-scope gate: on the
/// pinned runtime they return before the authored body later reaches its direct bindings.
pub(super) unsafe fn any_direct_rules(boma: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    !boma.is_null() && any_rules()
}

fn fnv(mut h: u64, v: u64) -> u64 {
    for b in v.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Record one ACMD call (pristine, pre-rewrite args).
///
/// Dedupe is within the one claimed playback. Once a fighter-kind + motion has a claim, every
/// other battle object and every later playback is ignored until an explicit capture clear.
pub unsafe fn record(lua_state: u64, func: &'static str, args: &[LuaArg]) {
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    let common = crate::slight::effect_viewer::acmd_hooks::is_common_effect_lua_state(lua_state);
    record_for_boma_inner(boma, func, args, common);
}

/// As [`record`], for hooks that receive a module accessor rather than a lua state —
/// `AttackModule::clear_all` is a lua_bind call, not an sv_animcmd script primitive.
pub unsafe fn record_for_boma(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    func: &'static str,
    args: &[LuaArg],
) {
    // A lua_bind hook has no agent to attribute, so it is never common by construction.
    record_for_boma_inner(boma, func, args, false);
}

unsafe fn record_for_boma_inner(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    func: &'static str,
    args: &[LuaArg],
    common: bool,
) {
    if is_injecting() {
        return;
    }
    if boma.is_null() {
        return;
    }
    // Fighter move snapshots must never absorb article/weapon/item scripts. `get_kind` values
    // are category-local, so an article kind can numerically equal the selected fighter kind;
    // filtering only by kind on the editor side cannot repair that collision after the fact.
    if smash::app::utility::get_category(&mut *boma)
        != *smash::lib::lua_const::BATTLE_OBJECT_CATEGORY_FIGHTER
    {
        return;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let kind = smash::app::utility::get_kind(&mut *boma);
    let boid = (*boma).battle_object_id;
    let debug = capture_debug_attempt(kind, motion, frame);

    let mut key = fnv(0xcbf29ce484222325, motion);
    key = fnv(key, kind as u64);
    key = fnv(key, frame.to_bits() as u64);
    for b in func.bytes() {
        key = fnv(key, b as u64);
    }
    for a in args {
        key = fnv(key, a.dedupe_bits());
    }
    // Part of the identity: the common agent's tint at the same frame with the same arguments is
    // a DIFFERENT line from the move's own, and must not dedupe the move's line away.
    key = fnv(key, common as u64);

    let (line, completed) = {
        // Never park a game worker here. The captured call still reaches its original hook when
        // another worker owns the state; only this diagnostic/cache sample is skipped.
        let Some(mut state) = try_capture_state() else {
            if let Some(debug) = debug {
                debug
                    .contention
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return;
        };

        // Resolve the run BEFORE dedupe — the key is scoped to it, and a new playback must not
        // be silently folded into the previous one's key set.
        let (run, completed) = mark_capture_motion(&mut state, boid, motion, kind, frame, func);
        if let Some(run) = run {
            // Arm the clear-all gate: a clear is only worth capturing once something is out to
            // clear.
            if is_collision_func(func) {
                note_collision(&mut state, boid, true);
            }

            if state.seen.entry(run).or_default().insert(key) {
                let line = CaptureLine {
                    kind,
                    motion,
                    frame,
                    func,
                    args: args.to_vec(),
                    run,
                    common,
                };
                // The archive owns the complete session history. The pending queue is only needed
                // while there is somewhere to stream it; avoiding a second owned copy is important
                // on the no-editor path, which is also the path that can run for a long match.
                if capture_stream_wanted() {
                    state.pending.push(line.clone());
                }
                if let Some(debug) = debug {
                    debug
                        .succeeded
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                (Some(line), completed)
            } else {
                if let Some(debug) = debug {
                    debug
                        .duplicates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                (None, completed)
            }
        } else {
            // A previous watch may have completed just before this call lost a claim to another
            // object. Preserve that completion even though the incoming line cannot be owned.
            if let Some(debug) = debug {
                debug
                    .claim_conflicts
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            (None, completed)
        }
    };

    if let Some(end) = completed {
        capture_archive::append_end(end);
    }
    let Some(line) = line else {
        return;
    };
    capture_archive::append_line(line);
    CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if func.contains("EFFECT") {
        EFFECT_CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    CAPTURE_LAST_KIND.store(kind as u64, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_MOTION.store(motion, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_FRAME.store(frame.to_bits() as u64, std::sync::atomic::Ordering::Relaxed);
}

fn capture_stream_wanted() -> bool {
    crate::rust_extender::net::simple_server::has_client()
        || crate::slight::smash_utils::debug_logging_enabled()
}

/// Ask the game thread to clear capture snapshots and ownership claims. The TCP server thread
/// must not take these locks directly; parking it against a game-thread holder can freeze Skyline.
pub fn request_clear_captures() {
    CLEAR_CAPTURES_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
}

fn clear_captures_if_requested() {
    if !CLEAR_CAPTURES_REQUESTED.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let Some(mut state) = try_capture_state() else {
        // A hook owns the state for a short transaction. Retry next tick instead of parking.
        CLEAR_CAPTURES_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
        return;
    };
    state.seen.clear();
    state.pending.clear();
    state.ends.clear();
    state.claims.clear();
    state.watch.clear();
    drop(state);
    clear_capture_debug();
    MOTION_WATCH_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
    capture_archive::clear();
    // A fresh capture window is a fresh thing to debug. The archive is cleared in the same
    // game-thread transaction as the in-memory ownership state.
    CLAIM_DROPS.store(0, std::sync::atomic::Ordering::Relaxed);
    crate::rust_extender::debuggable_server::notify_acmd_capture_cleared();
    crate::slight::diag::note("live ACMD captures cleared");
}

/// Drain up to `max` unsent capture lines (game thread, per-frame flush).
pub fn take_pending(max: usize) -> Vec<CaptureLine> {
    clear_captures_if_requested();
    let Some(mut state) = try_capture_state() else {
        return Vec::new();
    };
    if state.pending.is_empty() {
        return Vec::new();
    }
    let n = state.pending.len().min(max);
    let lines: Vec<CaptureLine> = state.pending.drain(..n).collect();
    drop(state);
    if !lines.is_empty() {
        CAPTURE_DRAINED.fetch_add(lines.len() as u64, std::sync::atomic::Ordering::Relaxed);
        write_capture_diag("drained");
    }
    lines
}

/// Drain completed-motion markers — but ONLY once every capture line recorded before them
/// has already been handed out, so the editor never sees "motion finished" ahead of the
/// lines that motion produced (the facade drains at most 32 lines per notify tick).
pub fn take_pending_ends(max: usize) -> Vec<CaptureEnd> {
    if capture_archive::replay_active() {
        return Vec::new();
    }
    let Some(mut state) = try_capture_state() else {
        return Vec::new();
    };
    if !state.pending.is_empty() || state.ends.is_empty() {
        return Vec::new();
    }
    let n = state.ends.len().min(max);
    state.ends.drain(..n).collect()
}

/// Replay the complete disk-backed capture archive (new editor client connected).
pub fn requeue_all() {
    capture_archive::begin_replay();
    reset_capture_debug_sent();
}

/// Drain a small archive replay batch for the facade. Keeping this separate from the live typed
/// queue means reconnecting never loads the complete history into plugin memory.
pub fn take_archive_replay(max: usize) -> Vec<String> {
    capture_archive::take_replay(max)
}

pub fn archive_replay_active() -> bool {
    capture_archive::replay_active()
}

/// Claim the first playback of `(kind, motion)` for one battle object and return its run id.
/// Existing claims are immutable: later performances and other instances are ignored.
///
/// This can be the first thing to observe a motion change (an ACMD script can run before the
/// line callback on the frame a move is cancelled into another), so it also closes out the
/// motion it replaces — otherwise a cancelled move's end marker would be dropped, and its
/// lines would be filed under the incoming motion's run.
fn mark_capture_motion(
    state: &mut CaptureState,
    boid: u32,
    motion: u64,
    kind: i32,
    frame: f32,
    // Only for the drop report below — a claim collision is far easier to read when it names
    // the call that was discarded.
    func: &'static str,
) -> (Option<u32>, Option<CaptureEnd>) {
    let mut finished: Option<(i32, u64, u32)> = None;
    // Battle object ids are reused across matches; keep the map from growing unbounded.
    if state.watch.len() > 128 {
        state.watch.clear();
    }
    let continuing = state
        .watch
        .get(&boid)
        .is_some_and(|w| w.motion == motion && !w.ended && frame + 0.5 >= w.frame);
    if continuing {
        let w = state
            .watch
            .get_mut(&boid)
            .expect("continuing implies present");
        w.captured = true;
        w.kind = kind;
        w.frame = frame;
        return (Some(w.run), None);
    }
    if let Some(w) = state.watch.remove(&boid) {
        if w.captured && !w.ended {
            finished = Some((w.kind, w.motion, w.run));
        }
    }
    MOTION_WATCH_ACTIVE.store(
        !state.watch.is_empty(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let completed =
        finished.and_then(|(kind, motion, run)| finish_capture(state, boid, kind, motion, run));

    // Copied out so the refusal arm can read the holder while the insert arms take a mutable
    // borrow.
    let held = state.claims.get(&(kind, motion)).copied();
    let run = match held {
        // **The same object returning to a motion it already owns — resume its run.**
        //
        // This is what a charged smash attack does. `attack_lw4` sets
        // `START_SMASH_HOLD` on frame 5, and the hold either rewinds the motion frame or
        // parks in a separate motion; either way the tests above read it as "the playback
        // ended". The claim then blocked a new run, so **everything after the charge was
        // discarded**: the `ATTACK` on frame 10, the `ATK_POWER` on frame 15, and the
        // sounds. Only `FT_MOTION_RATE` on frames 0 and 4 survived, because it runs before
        // the hold — which is exactly the pattern that was reported, three times, as
        // "hitboxes and tuning are missing but the GitHub script has them".
        //
        // A tilt has no hold and never hit this, which is why the same fetch looked correct
        // on one move and broken on the next.
        //
        // **Only while that playback is still going.** The first version of this resumed on
        // `boid` alone, reasoning that the dedupe key (motion, frame, func, args) would fold
        // a genuine repeat into the lines already held. **That is false for exactly the move
        // this fix exists for:** a charged smash releases at a different motion frame every
        // time, so the frame in the key differs, nothing collapses, and each extra
        // performance stacks another copy of every effect onto the timeline. Reported
        // immediately as "doing both directions adds a whole bunch of junk effects".
        //
        // So a finished playback starts a fresh run instead. The editor reads only the
        // newest run for a motion (`latest_run_for`), so the last complete performance wins
        // and a capture is always one performance — which is also the only thing a script
        // can faithfully represent.
        Some(h) if h.boid == boid && !h.ended => h.run,
        // Same object, previous playback finished: a new performance, so a new run.
        Some(h) if h.boid == boid => {
            let run = next_run();
            state.claims.insert(
                (kind, motion),
                CaptureClaim {
                    boid,
                    run,
                    ended: false,
                    end_sent: false,
                },
            );
            run
        }
        Some(h) => {
            // The remaining drop, now genuinely rare. Bounded, and it names the call so a
            // whole family going missing can never again be silent.
            if CLAIM_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 32 {
                crate::slight::diag::note(format!(
                    "CAP drop {func} motion={motion:#x} frame={frame:.2} boid={boid} — \
                         claimed by another object boid={} run={}; not in any capture",
                    h.boid, h.run
                ));
            }
            return (None, completed);
        }
        None => {
            let run = next_run();
            state.claims.insert(
                (kind, motion),
                CaptureClaim {
                    boid,
                    run,
                    ended: false,
                    end_sent: false,
                },
            );
            run
        }
    };
    state.watch.insert(
        boid,
        MotionWatch {
            motion,
            kind,
            frame,
            captured: true,
            ended: false,
            run,
            open_collisions: false,
        },
    );
    MOTION_WATCH_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    (Some(run), completed)
}

/// Does this captured function put a collision out? Kept in step with the editor, which
/// buckets captures the same way (`ATTACK*` / `CATCH` / `AREA_WIND*`).
fn is_collision_func(func: &str) -> bool {
    // Name equality is intentional. `ATTACK_ABS` has no volume, and prefix bucketing would make
    // a different ATTACK-family layout arm the ordinary clear gate by accident.
    matches!(
        func,
        "ATTACK" | "ATTACK_IGNORE_THROW" | "ATTACK_FP" | "CATCH"
    ) || matches!(
        func,
        "AREA_WIND_2ND" | "AREA_WIND_2ND_RAD" | "AREA_WIND_2ND_arg10" | "AREA_WIND_2ND_RAD_arg9"
    )
}

/// Note that a collision came out (or was cleared) on `boid`, and report whether a clear is
/// worth recording — i.e. whether anything was actually open to clear.
fn note_collision(state: &mut CaptureState, boid: u32, open: bool) -> bool {
    let Some(w) = state.watch.get_mut(&boid) else {
        return false;
    };
    let was_open = w.open_collisions;
    w.open_collisions = open;
    was_open
}

fn close_collision_if_open(boid: u32) -> bool {
    let Some(mut state) = try_capture_state() else {
        return false;
    };
    note_collision(&mut state, boid, false)
}

fn push_end(state: &mut CaptureState, kind: i32, motion: u64, run: u32) -> Option<CaptureEnd> {
    // Collapse duplicate completion paths for the same claimed run (end frame and motion switch
    // can be observed on adjacent callbacks).
    if state.ends.iter().any(|e| e.run == run) {
        return None;
    }
    let end = CaptureEnd { kind, motion, run };
    state.ends.push(end);
    Some(end)
}

fn finish_capture(
    state: &mut CaptureState,
    boid: u32,
    kind: i32,
    motion: u64,
    run: u32,
) -> Option<CaptureEnd> {
    let (naturally_ended, should_emit) = {
        let Some(claim) = state.claims.get_mut(&(kind, motion)) else {
            return None;
        };
        if claim.boid != boid || claim.run != run {
            return None;
        }
        let should_emit = !claim.end_sent;
        let naturally_ended = claim.ended;
        claim.end_sent = true;
        (naturally_ended, should_emit)
    };
    let end = should_emit
        .then(|| push_end(state, kind, motion, run))
        .flatten();
    // A naturally completed run can never receive another authored line. Drop only its
    // in-memory dedupe set; an interrupted run may resume on the same motion and still needs its
    // keys. The complete line history is already in the disk archive either way.
    if naturally_ended {
        state.seen.remove(&run);
    }
    end
}

/// Per-agent, per-frame end-of-motion watch (agent_extender line callback).
///
/// "The move finished" is taken from the game itself rather than a timer: a motion is over
/// once `MotionModule::frame` reaches `MotionModule::end_frame` (the animation ran out, which
/// is exactly when the ACMD script stops executing), or once the agent switches to a different
/// motion / restarts the same one (cancel, interrupt, looping jab). Only motions that actually
/// recorded a new capture line emit a marker.
pub unsafe fn capture_tick(lua_state: u64) {
    // Fast path FIRST: this runs for every fighter and article every frame, and until a
    // capture has actually been recorded there is nothing to watch. One relaxed atomic
    // load, no lock and no game calls, is the difference between free and a per-agent
    // per-frame cost the whole roster pays.
    if !MOTION_WATCH_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    let boid = (*boma).battle_object_id;

    // Resolve the entry before querying MotionModule, then release the capture state. Native
    // getters can themselves enter hooked game code on some fighters; calling them while the
    // state is held would turn that re-entry into a self-deadlock.
    let watched = {
        let Some(state) = try_capture_state() else {
            return;
        };
        state.watch.get(&boid).copied()
    };
    let Some(watched) = watched else {
        return;
    };
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let end = smash::app::lua_bind::MotionModule::end_frame(boma);

    let mut finished: Option<(i32, u64, u32)> = None;
    // Set only when the playback reached `end_frame` — a charge hold must not look like this.
    let mut ended_naturally: Option<(i32, u64, u32)> = None;
    let completed = {
        let Some(mut state) = try_capture_state() else {
            return;
        };
        // Another worker may have advanced this object between the snapshot and this update.
        // Never apply stale native values to the replacement watch.
        let Some(current) = state.watch.get(&boid).copied() else {
            return;
        };
        if current.run != watched.run || current.motion != watched.motion {
            return;
        }
        if current.motion != motion || frame + 0.5 < current.frame {
            // Switched away (or looped back to frame 0): the previous playback is over.
            if current.captured && !current.ended {
                finished = Some((current.kind, current.motion, current.run));
            }
            state.watch.remove(&boid);
        } else {
            let w = state
                .watch
                .get_mut(&boid)
                .expect("the run identity was checked above");
            w.frame = frame;
            // `>=` (not `end - rate`) so the marker is never EARLY: a hitbox on the very
            // last frame still gets recorded before the editor is told to adopt.
            if w.captured && !w.ended && end > 0.0 && frame >= end {
                w.ended = true;
                ended_naturally = Some((w.kind, w.motion, w.run));
                finished = Some((w.kind, w.motion, w.run));
            }
        }
        MOTION_WATCH_ACTIVE.store(
            !state.watch.is_empty(),
            std::sync::atomic::Ordering::Relaxed,
        );

        // The claim outlives the watch entry, and that is the point: the entry is dropped the
        // moment the motion frame steps backwards, so only the claim can distinguish a finished
        // playback from one suspended by a charge hold.
        if let Some((kind, motion, run)) = ended_naturally {
            if let Some(claim) = state.claims.get_mut(&(kind, motion)) {
                if claim.run == run {
                    claim.ended = true;
                }
            }
        }
        finished.and_then(|(kind, motion, run)| finish_capture(&mut state, boid, kind, motion, run))
    };
    if let Some(end) = completed {
        capture_archive::append_end(end);
    }
}

// ── Rules (live modify / suppress / inject) ──────────────────────────────────

/// Sparse per-slot overrides for an existing ATTACK (arg indices in the header comment).
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct HbOverrides {
    /// Complete typed AREA_WIND payload. Wind areas do not use ATTACK's slot layout.
    pub wind_args: Option<Vec<LuaArg>>,
    pub part: Option<i64>,
    pub bone: Option<u64>,
    pub damage: Option<f32>,
    pub angle: Option<i64>,
    pub kbg: Option<i64>,
    pub fkb: Option<i64>,
    pub bkb: Option<i64>,
    pub size: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub z: Option<f32>,
    pub x2: Option<f32>,
    pub y2: Option<f32>,
    pub z2: Option<f32>,
    /// Explicit sphere/capsule state. `None` preserves compatibility with older editors;
    /// `Some(false)` clears an existing capsule's second endpoint back to Lua nils.
    pub capsule: Option<bool>,
    pub hitlag: Option<f32>,
    pub sdi: Option<f32>,
    // ── Attribute slots 17..35 ───────────────────────────────────────────────
    // Arrive as the raw lua numbers (the editor holds them as symbolic names and encodes
    // them on the way out); `collision_attr` is a hash40. Absent = leave the script's value.
    pub setoff: Option<i64>,
    pub lr_check: Option<i64>,
    pub clang: Option<bool>,
    pub add_attack: Option<i64>,
    pub hitbox_attr: Option<f32>,
    pub ground_or_air: Option<i64>,
    pub mtk: Option<bool>,
    pub shield_disable: Option<bool>,
    pub reflectable: Option<bool>,
    pub absorbable: Option<bool>,
    pub landing_attack: Option<bool>,
    pub situation_mask: Option<i64>,
    pub category_mask: Option<i64>,
    pub part_mask: Option<i64>,
    pub no_finish_camera: Option<bool>,
    pub collision_attr: Option<u64>,
    pub sound_level: Option<i64>,
    pub sound_attr: Option<i64>,
    pub attack_region: Option<i64>,
    // ── Hurtbox state (CAT_HURT only) ────────────────────────────────────────
    /// `HIT_STATUS_*` as a raw lua number, for `HIT_NODE` / `HIT_NO`.
    pub hit_status: Option<i64>,
    /// The bone hash or group number to retarget to. Rewriting this is what lets the editor
    /// preview "make the knee intangible instead of the shin" without a rebuild.
    pub hit_target: Option<LuaArg>,
    /// `COL_PRI`'s priority number.
    pub col_pri: Option<i64>,
    /// `DAMAGE_NO_REACTION` mode/value for the fighter-wide damage-reaction hook.
    pub hurt_condition_mode: Option<i64>,
    pub hurt_condition_value: Option<f32>,
    // ── Post-hoc hitbox tuning (CAT_ATK_POWER / CAT_ATK_SETOFF_MUL only) ──────
    /// The hitbox id the modifier names — slot 0 for both members.
    pub atk_mod_id: Option<i64>,
    /// The modifier's value — slot 1 for both members. A float on the wire because the slot is
    /// declared `ToF32`; the editor's exporter puts the vanilla integer spelling back.
    pub atk_mod_value: Option<f32>,
    // ── Sound (CAT_SOUND only) ───────────────────────────────────────────────
    /// The sound hashes to play instead, positional into the call's leading `Hash40` slots.
    ///
    /// A list rather than one hash because two members take a pair — `PLAY_STEP_FLIPPABLE`
    /// names the left and right footstep, `PLAY_FLY_VOICE` two alternative clips — and the
    /// editor holds them as a list for the same reason. Applied only as far as the member
    /// actually declares hash slots; see `sound_hooks::hash_slots`.
    pub sound_hashes: Option<Vec<u64>>,
    /// Replacement `FT_MOTION_RATE` argument.
    ///
    /// **Below 1.0 plays FASTER** — `game_frames = motion_frames * rate`. Rejected by the hook
    /// if it is not finite and positive, because a zero rate freezes the animation and nothing
    /// below the call would ever run.
    pub motion_rate: Option<f32>,
    /// Replacement `SET_SPEED_EX` or `ADD_SPEED_NO_LIMIT` x/y velocity components.
    pub speed_x: Option<f32>,
    pub speed_y: Option<f32>,
    /// Replacement numeric `CORRECT` kind.
    pub correct_kind: Option<i64>,
    /// Replacement `FT_CATCH_STOP` numeric `ToF32` arguments.
    pub ft_catch_stop_arg1: Option<f32>,
    pub ft_catch_stop_arg2: Option<f32>,
    /// Replacement `FT_START_ADJUST_MOTION_FRAME_arg1` numeric value.
    pub ft_start_adjust_motion_frame_value: Option<f32>,
    /// Replacement direct `MotionModule::set_rate` value. Kept separate from the
    /// `FT_MOTION_RATE` field because the two hooks have different call semantics.
    pub motion_module_rate: Option<f32>,
    /// Replacement direct `MotionModule::set_helper_calculation` boolean.
    pub motion_module_helper_calculation: Option<bool>,
    /// Replacement direct `MotionModule::set_rate_partial` rate.
    pub motion_module_rate_partial: Option<f32>,
    /// Replacement direct `MotionModule::set_frame_partial` seek frame.
    pub motion_module_frame_partial: Option<f32>,
    /// Replacement direct `MotionModule::set_frame_partial` sync flag. The native binding always
    /// carries this boolean even when the authored source omitted it.
    pub motion_module_frame_partial_sync: Option<bool>,
    /// Replacement numeric flag for direct `WorkModule::on_flag` / `off_flag` calls.
    pub work_flag: Option<i64>,
    /// Replacement numeric transition term for direct WorkModule transition-term calls.
    pub work_transition_term: Option<i64>,
    /// Replacement integer value for a direct `WorkModule::set_int` call.
    pub work_module_set_int_value: Option<i64>,
    /// Replacement exact 64-bit value for a direct `WorkModule::set_int64` call.
    pub work_module_set_int64_value: Option<i64>,
    /// Replacement WorkModule slot for a direct `WorkModule::inc_int` call.
    pub work_module_inc_int_slot: Option<i64>,
    /// Replacement float value for a direct `WorkModule::set_float` call.
    pub work_module_set_float_value: Option<f32>,
    /// Replacement WorkModule slot for either direct value setter.
    pub work_module_set_slot: Option<i64>,
    /// Replacement numeric kinetic-energy kind for `CLR_SPEED`.
    pub clr_speed_kinetic_kind: Option<i64>,
    /// Replacement numeric kinetic type for `KineticModule::change_kinetic`.
    pub change_kinetic_type: Option<i64>,
    /// Replacement numeric energy ID for direct suspend/resume kinetic calls.
    pub kinetic_energy_id: Option<i64>,
    /// Replacement bool for `KineticModule::set_consider_ground_friction`.
    pub kinetic_ground_friction: Option<bool>,
    /// Replacement resolved reserve attribute for `set_consider_ground_friction`.
    pub kinetic_ground_friction_energy: Option<i64>,
    /// Complete replacement argument vector for a measured expression primitive.
    pub expression_args: Option<Vec<LuaArg>>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct InjectRule {
    /// Motion frame at which to fire (once per motion playback).
    pub frame: f32,
    /// Complete typed argument vector for the selected collision family.
    pub args: Vec<LuaArg>,
    /// Exact AREA_WIND family function. Omitted for attack/grab injections and old editors.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct HitboxRule {
    /// hash40 of the motion name ("attack_air_n").
    pub motion: u64,
    /// Collision family this rule targets (0 attack / 1 grab / 2 wind). Old editors omit
    /// it → defaults to attack, preserving prior behavior.
    #[serde(default)]
    pub category: u8,
    /// ATTACK id this rule targets (None = any id in the motion). For injected sound calls this
    /// is also an optional per-call discriminator, because two identical sounds may share a
    /// frame and must both fire.
    #[serde(default)]
    pub hitbox_id: Option<u64>,
    #[serde(default)]
    pub suppress: bool,
    /// Motion-frame window for suppress/override; None = any frame. Frame scoping keeps
    /// multi-hit moves (which reuse the same id across frames) independent.
    #[serde(default)]
    pub frame_start: Option<f32>,
    #[serde(default)]
    pub frame_end: Option<f32>,
    #[serde(default)]
    pub overrides: Option<HbOverrides>,
    #[serde(default)]
    pub inject: Option<InjectRule>,
    /// The exact ACMD macro this rule is for, when the category alone does not identify it.
    ///
    /// [`CAT_SOUND`] and [`CAT_EXPRESSION`] send it. Sound members share a category and all of
    /// them carry a `Hash40` in slot 0, so without this a rule for one applies silently to
    /// another. Expression members have different argument shapes, so their macro name is part
    /// of the match for the same reason. Every other family either has a category per macro or
    /// an argument layout that makes a cross-member write impossible.
    ///
    /// `None` matches any member, which is what an editor older than this build sends. Read
    /// that way round on purpose: too broad is recoverable, silently dead is not.
    #[serde(default)]
    pub func: Option<String>,
}

impl HitboxRule {
    fn matches(&self, category: u8, motion: u64, id: u64, frame: f32) -> bool {
        self.category == category
            && self.motion == motion
            && self.hitbox_id.map(|h| h == id).unwrap_or(true)
            && self.frame_start.map(|s| frame >= s).unwrap_or(true)
            && self.frame_end.map(|e| frame <= e).unwrap_or(true)
    }
}

static RULES: LazyLock<Mutex<Vec<HitboxRule>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static HAVE_RULES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Full-list replace (same semantics as effect spawn rules).
pub fn set_rules(rules: Vec<HitboxRule>) {
    let n = rules.len();
    {
        let mut guard = RULES.lock();
        *guard = rules;
    }
    HAVE_RULES.store(n != 0, std::sync::atomic::Ordering::Release);
    // Categories, not just a count. "2 rule(s)" is true whether they are two hitbox rules or a
    // hitbox rule and a sound rule, and those need different answers when a live edit does
    // nothing — one says the editor never sent the family, the other says it sent it and the
    // plugin did not match it.
    let by_cat: Vec<String> = {
        let guard = RULES.lock();
        let mut cats: Vec<u8> = guard.iter().map(|r| r.category).collect();
        cats.sort_unstable();
        cats.dedup();
        cats.iter()
            .map(|c| {
                let count = guard.iter().filter(|r| r.category == *c).count();
                let name = match *c {
                    CAT_ATTACK => "attack",
                    CAT_GRAB => "grab",
                    CAT_WIND => "wind",
                    CAT_HURT => "hurt",
                    CAT_ABS => "abs",
                    CAT_ATK_POWER => "atk_power",
                    CAT_ATK_SETOFF_MUL => "atk_setoff",
                    CAT_SEARCH => "search",
                    CAT_ATTACK_FP => "attack_fp",
                    CAT_SOUND => "sound",
                    CAT_EXPRESSION => "expression",
                    CAT_REVERSE_LR => "reverse_lr",
                    CAT_SPEED_EX => "speed_ex",
                    CAT_SPEED => "speed",
                    CAT_ADD_SPEED_NO_LIMIT => "add_speed_no_limit",
                    CAT_CORRECT => "correct",
                    CAT_FT_CATCH_STOP => "ft_catch_stop",
                    CAT_FT_START_ADJUST_MOTION_FRAME => "ft_start_adjust_motion_frame",
                    CAT_CLR_SPEED => "clr_speed",
                    CAT_SET_AIR => "set_air",
                    CAT_CHANGE_KINETIC => "change_kinetic",
                    CAT_KINETIC_ADD_SPEED => "kinetic_add_speed",
                    CAT_KINETIC_SUSPEND_ENERGY => "kinetic_suspend_energy",
                    CAT_KINETIC_RESUME_ENERGY => "kinetic_resume_energy",
                    CAT_KINETIC_ENABLE_ENERGY => "kinetic_enable_energy",
                    CAT_KINETIC_UNABLE_ENERGY => "kinetic_unable_energy",
                    CAT_KINETIC_CLEAR_SPEED_ALL => "kinetic_clear_speed_all",
                    CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION => {
                        "kinetic_set_consider_ground_friction"
                    }
                    CAT_MOTION_MODULE_SET_RATE => "motion_module_set_rate",
                    CAT_MOTION_MODULE_SET_HELPER_CALCULATION => {
                        "motion_module_set_helper_calculation"
                    }
                    CAT_MOTION_MODULE_SET_RATE_PARTIAL => "motion_module_set_rate_partial",
                    CAT_MOTION_MODULE_SET_FRAME_PARTIAL => "motion_module_set_frame_partial",
                    CAT_WORK_FLAG => "work_flag",
                    CAT_WORK_TRANSITION_TERM => "work_transition_term",
                    CAT_WORK_MODULE_SET => "work_module_set",
                    CAT_WORK_MODULE_INC_INT => "work_module_inc_int",
                    _ => "unknown",
                };
                format!("{name}={count}")
            })
            .collect()
    };
    // A new rule set is a new thing to debug, so the sound path gets its reporting budget back.
    // Without this the budget is spent long before the rule under test ever arrives.
    sound_hooks::reset_reports();
    rate_hooks::reset_reports();
    crate::slight::diag::note(format!(
        "hitbox_rules set: {n} rule(s) [{}]",
        by_cat.join(" ")
    ));
}

fn any_rules() -> bool {
    HAVE_RULES.load(std::sync::atomic::Ordering::Acquire)
}

/// The suppress/override action matching (motion, id, frame), if any. Frame-scoped so one
/// hit of a multi-hit move (same id, different frame) can be edited without touching the rest.
fn action_for(
    category: u8,
    motion: u64,
    id: u64,
    frame: f32,
) -> Option<(bool, Option<HbOverrides>)> {
    let rules = RULES.lock();
    rules
        .iter()
        .find(|r| r.inject.is_none() && r.matches(category, motion, id, frame))
        .map(|r| (r.suppress, r.overrides.clone()))
}

/// As [`action_for`], but with the exact direct-call name as a second discriminator. Shared
/// `CAT_WORK_FLAG` rules must not let an `on_flag` edit reach an `off_flag` call with the same
/// numeric flag and frame.
fn action_for_func(
    category: u8,
    motion: u64,
    id: u64,
    frame: f32,
    func: &str,
) -> Option<(bool, Option<HbOverrides>)> {
    let rules = RULES.lock();
    rules
        .iter()
        .find(|r| {
            r.inject.is_none()
                && r.matches(category, motion, id, frame)
                && r.func.as_deref().is_none_or(|candidate| candidate == func)
        })
        .map(|r| (r.suppress, r.overrides.clone()))
}

/// Stable key for one pristine expression call. Keep in lockstep with
/// `game_link::expression_key`: the editor can derive the key from a live capture, while this
/// side derives it from the actual Lua stack just before the game's primitive runs.
fn expression_key(func: &str, args: &[LuaArg]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in func.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    for arg in args {
        let value = arg.dedupe_bits();
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    hash
}

/// Stable key for a fixed list of numeric `ToF32` arguments. Keep this in lockstep with
/// `game_link::numeric_point_key`: the editor keys a rule from the pristine capture and this
/// side keys it from the Lua stack immediately before the primitive runs.
fn numeric_point_key(func: &str, args: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in func.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    for arg in args {
        for byte in arg.to_bits().to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    hash
}

/// Stable key for exact integer point arguments. Keep this in lockstep with
/// `game_link::integer_point_key`; `set_int64` values can be hash40s and must never pass through
/// `f32`.
fn integer_point_key(func: &str, args: &[i64]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in func.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    for arg in args {
        for byte in arg.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    hash
}

fn numeric_arg_f32(arg: &LuaArg) -> Option<f32> {
    match arg {
        LuaArg::Int(value) => Some(*value as f32),
        LuaArg::Num(value) => value.is_finite().then_some(*value),
        _ => None,
    }
}

fn expression_action(
    motion: u64,
    frame: f32,
    func: &str,
    args: &[LuaArg],
) -> Option<(bool, Option<HbOverrides>)> {
    let key = expression_key(func, args);
    let rules = RULES.lock();
    rules
        .iter()
        .find(|rule| {
            rule.inject.is_none()
                && rule.category == CAT_EXPRESSION
                && rule.func.as_deref() == Some(func)
                && rule.matches(CAT_EXPRESSION, motion, key, frame)
        })
        .map(|rule| (rule.suppress, rule.overrides.clone()))
}

unsafe fn rewrite_expression_args(
    lua_state: u64,
    overrides: &HbOverrides,
    args: &[LuaArg],
    expected: usize,
) {
    let Some(replacement) = overrides
        .expression_args
        .as_ref()
        .filter(|replacement| replacement.len() == expected)
    else {
        return;
    };
    if replacement == args {
        return;
    }
    rewrite_args(lua_state, replacement);
}

macro_rules! expression_hook {
    ($hook_name:ident, $target:path, $func:literal, $arity:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let args = read_args_exact(lua_state, $arity);
            record(lua_state, $func, &args);
            // A replacement is already the fully typed call we deliberately chose. Do not run
            // it back through authored expression rules: a second rumble with the same payload
            // on the same frame can otherwise match a suppression rule and disappear.
            if any_rules() && !is_injecting() {
                let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                    as *mut smash::app::BattleObjectModuleAccessor;
                if !boma.is_null() {
                    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                    let frame = smash::app::lua_bind::MotionModule::frame(boma);
                    if let Some((suppress, overrides)) =
                        expression_action(motion, frame, $func, &args)
                    {
                        if suppress {
                            return;
                        }
                        if let Some(overrides) = overrides {
                            rewrite_expression_args(lua_state, &overrides, &args, $arity);
                        }
                    }
                }
            }
            original!()(lua_state)
        }
    };
}

expression_hook!(
    hook_rumble_hit,
    smash::app::sv_animcmd::RUMBLE_HIT,
    "RUMBLE_HIT",
    2
);

#[skyline::hook(replace = smash::app::sv_animcmd::REVERSE_LR)]
unsafe fn hook_reverse_lr(lua_state: u64) {
    record(lua_state, "REVERSE_LR", &[]);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            if let Some((suppress, _)) = action_for(CAT_REVERSE_LR, motion, 0, frame) {
                if suppress {
                    return;
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the measured `CLR_SPEED(agent, kinetic_id)` point.
///
/// The runtime hook sees the resolved numeric kinetic ID; the editor keeps the authored source
/// token separately and only sends a numeric replacement when the live capture can prove it.
#[skyline::hook(replace = smash::app::sv_kinetic_energy::clear_speed)]
unsafe fn hook_clr_speed(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    record(lua_state, "CLR_SPEED", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            let Some(kind) = args.first().and_then(|arg| match arg {
                LuaArg::Int(value) => Some(*value as f32),
                LuaArg::Num(value) => Some(*value),
                _ => None,
            }) else {
                original!()(lua_state);
                return;
            };
            let key = numeric_point_key("CLR_SPEED", &[kind]);
            if let Some((suppress, overrides)) = action_for(CAT_CLR_SPEED, motion, key, frame) {
                if suppress {
                    return;
                }
                if let Some(replacement) = overrides.and_then(|item| item.clr_speed_kinetic_kind) {
                    let mut values = args.clone();
                    if let Some(slot) = values.first_mut() {
                        *slot = LuaArg::Int(replacement);
                        if values != args {
                            rewrite_args(lua_state, &values);
                        }
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely suppress the measured argument-less `SET_AIR` point.
#[skyline::hook(replace = smash::app::sv_animcmd::SET_AIR)]
unsafe fn hook_set_air(lua_state: u64) {
    record(lua_state, "SET_AIR", &[]);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            if let Some((suppress, _)) = action_for(CAT_SET_AIR, motion, KINETIC_KEY_SET_AIR, frame)
            {
                if suppress {
                    return;
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely suppress the measured argument-less direct kinetic clear point.
#[skyline::hook(replace = smash::app::lua_bind::KineticModule::clear_speed_all)]
unsafe fn hook_kinetic_clear_speed_all(boma: *mut smash::app::BattleObjectModuleAccessor) -> u64 {
    record_for_boma(boma, "KineticModule::clear_speed_all", &[]);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        if let Some((suppress, _)) = action_for(
            CAT_KINETIC_CLEAR_SPEED_ALL,
            motion,
            KINETIC_KEY_CLEAR_SPEED_ALL,
            frame,
        ) {
            if suppress {
                return 0;
            }
        }
    }
    original!()(boma)
}

/// Capture and sparsely override the verified direct ground-friction toggle. The authored
/// reserve attribute is usually a named lua constant, so the live rule keys its resolved
/// integer value while source/export keep the original token.
#[skyline::hook(replace = smash::app::lua_bind::KineticModule::set_consider_ground_friction)]
unsafe fn hook_kinetic_set_consider_ground_friction(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    consider_ground_friction: bool,
    kinetic_energy_attribute: i32,
) {
    let args = [
        LuaArg::Bool(consider_ground_friction),
        LuaArg::Int(kinetic_energy_attribute as i64),
    ];
    record_for_boma(boma, "KineticModule::set_consider_ground_friction", &args);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = numeric_point_key(
            "KineticModule::set_consider_ground_friction",
            &[
                if consider_ground_friction { 1.0 } else { 0.0 },
                kinetic_energy_attribute as f32,
            ],
        );
        if let Some((suppress, overrides)) =
            action_for(CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION, motion, key, frame)
        {
            if suppress {
                return;
            }
            if let Some(overrides) = overrides {
                let replacement_friction = overrides
                    .kinetic_ground_friction
                    .unwrap_or(consider_ground_friction);
                let replacement_attribute = overrides
                    .kinetic_ground_friction_energy
                    .map(|value| value as i32)
                    .unwrap_or(kinetic_energy_attribute);
                if replacement_friction != consider_ground_friction
                    || replacement_attribute != kinetic_energy_attribute
                {
                    original!()(boma, replacement_friction, replacement_attribute);
                    return;
                }
            }
        }
    }
    original!()(boma, consider_ground_friction, kinetic_energy_attribute)
}

/// Capture and sparsely override the verified direct kinetic-type change.
///
/// Unlike the `sv_animcmd` kinetic points above, this primitive receives the module accessor
/// directly. The authored source token is not available at runtime, so the editor keys the
/// rule by the resolved numeric type and only sends a numeric replacement when capture proves it.
#[skyline::hook(replace = smash::app::lua_bind::KineticModule::change_kinetic)]
unsafe fn hook_change_kinetic(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    kinetic_type: i32,
) -> i32 {
    record_for_boma(
        boma,
        "KineticModule::change_kinetic",
        &[LuaArg::Int(kinetic_type as i64)],
    );
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = numeric_point_key("KineticModule::change_kinetic", &[kinetic_type as f32]);
        if let Some((suppress, overrides)) = action_for(CAT_CHANGE_KINETIC, motion, key, frame) {
            if suppress {
                // The call's return value is ignored by the measured ACMD source. Zero is the
                // neutral success-like result used for a suppressed primitive in this hook.
                return 0;
            }
            if let Some(replacement) = overrides.and_then(|item| item.change_kinetic_type) {
                return original!()(boma, replacement as i32);
            }
        }
    }
    original!()(boma, kinetic_type)
}

/// Capture and sparsely override the measured direct kinetic-energy toggles. The source keeps the
/// authored energy-ID token, while the live hook keys the resolved numeric ID and only sends a
/// numeric replacement when capture proves it.
macro_rules! kinetic_energy_hook {
    ($hook_name:ident, $target:path, $category:expr, $func:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(
            boma: *mut smash::app::BattleObjectModuleAccessor,
            kinetic_energy_id: i32,
        ) -> u64 {
            record_for_boma(boma, $func, &[LuaArg::Int(kinetic_energy_id as i64)]);
            if any_direct_rules(boma) {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                let key = numeric_point_key($func, &[kinetic_energy_id as f32]);
                if let Some((suppress, overrides)) = action_for($category, motion, key, frame) {
                    if suppress {
                        return 0;
                    }
                    if let Some(replacement) = overrides.and_then(|item| item.kinetic_energy_id) {
                        return original!()(boma, replacement as i32);
                    }
                }
            }
            original!()(boma, kinetic_energy_id)
        }
    };
}

kinetic_energy_hook!(
    hook_kinetic_suspend_energy,
    smash::app::lua_bind::KineticModule::suspend_energy,
    CAT_KINETIC_SUSPEND_ENERGY,
    "KineticModule::suspend_energy"
);
kinetic_energy_hook!(
    hook_kinetic_resume_energy,
    smash::app::lua_bind::KineticModule::resume_energy,
    CAT_KINETIC_RESUME_ENERGY,
    "KineticModule::resume_energy"
);
kinetic_energy_hook!(
    hook_kinetic_enable_energy,
    smash::app::lua_bind::KineticModule::enable_energy,
    CAT_KINETIC_ENABLE_ENERGY,
    "KineticModule::enable_energy"
);
kinetic_energy_hook!(
    hook_kinetic_unable_energy,
    smash::app::lua_bind::KineticModule::unable_energy,
    CAT_KINETIC_UNABLE_ENERGY,
    "KineticModule::unable_energy"
);

/// Capture and sparsely override the verified direct kinetic-vector addition.
///
/// The editor models the measured source shape's x/y components and requires a zero z component
/// on source/export. Runtime capture keeps the full vector in the wire line; an edit changes only
/// x/y and leaves the game's original z untouched until that component has its own evidence-backed
/// editor field.
#[skyline::hook(replace = smash::app::lua_bind::KineticModule::add_speed)]
unsafe fn hook_kinetic_add_speed(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    speed: *const smash::phx::Vector3f,
) -> u64 {
    let Some(vector) = speed.as_ref().copied() else {
        record_for_boma(boma, "KineticModule::add_speed", &[]);
        return original!()(boma, speed);
    };
    let args = [
        LuaArg::Num(vector.x),
        LuaArg::Num(vector.y),
        LuaArg::Num(vector.z),
    ];
    record_for_boma(boma, "KineticModule::add_speed", &args);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = numeric_point_key("KineticModule::add_speed", &[vector.x, vector.y, vector.z]);
        if let Some((suppress, overrides)) = action_for(CAT_KINETIC_ADD_SPEED, motion, key, frame) {
            if suppress {
                return 0;
            }
            if let Some(overrides) = overrides {
                let mut replacement = vector;
                if let Some(value) = overrides.speed_x {
                    replacement.x = value;
                }
                if let Some(value) = overrides.speed_y {
                    replacement.y = value;
                }
                if replacement.x != vector.x
                    || replacement.y != vector.y
                    || replacement.z != vector.z
                {
                    return original!()(boma, &replacement);
                }
            }
        }
    }
    original!()(boma, speed)
}

/// Capture and sparsely override the verified direct WorkModule flag operations.
///
/// `on_flag` and `off_flag` share one wire category but keep their exact function name in the
/// rule. The numeric flag is the runtime identity; the editor retains named source tokens for
/// export and source sync.
macro_rules! work_flag_hook {
    ($hook_name:ident, $target:path, $func:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(boma: *mut smash::app::BattleObjectModuleAccessor, flag: i32) {
            record_for_boma(boma, $func, &[LuaArg::Int(flag as i64)]);
            if any_direct_rules(boma) {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                let key = numeric_point_key($func, &[flag as f32]);
                if let Some((suppress, overrides)) =
                    action_for_func(CAT_WORK_FLAG, motion, key, frame, $func)
                {
                    if suppress {
                        return;
                    }
                    if let Some(replacement) = overrides.and_then(|item| item.work_flag) {
                        if replacement as i32 != flag {
                            original!()(boma, replacement as i32);
                            return;
                        }
                    }
                }
            }
            original!()(boma, flag)
        }
    };
}

work_flag_hook!(
    hook_work_module_on_flag,
    smash::app::lua_bind::WorkModule::on_flag,
    "WorkModule::on_flag"
);
work_flag_hook!(
    hook_work_module_off_flag,
    smash::app::lua_bind::WorkModule::off_flag,
    "WorkModule::off_flag"
);

/// Capture and sparsely override the verified direct WorkModule transition-term operations.
///
/// The operation name remains the rule discriminator; the numeric transition term is the
/// runtime identity while the editor retains the authored source token for export and sync.
macro_rules! work_transition_term_hook {
    ($hook_name:ident, $target:path, $func:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(
            boma: *mut smash::app::BattleObjectModuleAccessor,
            transition_term: i32,
        ) {
            record_for_boma(boma, $func, &[LuaArg::Int(transition_term as i64)]);
            if any_direct_rules(boma) {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                let key = numeric_point_key($func, &[transition_term as f32]);
                if let Some((suppress, overrides)) =
                    action_for_func(CAT_WORK_TRANSITION_TERM, motion, key, frame, $func)
                {
                    if suppress {
                        return;
                    }
                    if let Some(replacement) = overrides.and_then(|item| item.work_transition_term)
                    {
                        if replacement as i32 != transition_term {
                            original!()(boma, replacement as i32);
                            return;
                        }
                    }
                }
            }
            original!()(boma, transition_term)
        }
    };
}

work_transition_term_hook!(
    hook_work_module_enable_transition_term,
    smash::app::lua_bind::WorkModule::enable_transition_term,
    "WorkModule::enable_transition_term"
);
work_transition_term_hook!(
    hook_work_module_unable_transition_term,
    smash::app::lua_bind::WorkModule::unable_transition_term,
    "WorkModule::unable_transition_term"
);
work_transition_term_hook!(
    hook_work_module_enable_transition_term_group,
    smash::app::lua_bind::WorkModule::enable_transition_term_group,
    "WorkModule::enable_transition_term_group"
);
work_transition_term_hook!(
    hook_work_module_unable_transition_term_group_ex,
    smash::app::lua_bind::WorkModule::unable_transition_term_group_ex,
    "WorkModule::unable_transition_term_group_ex"
);

/// Capture and sparsely override the measured direct `WorkModule::inc_int` operation. The slot
/// is the complete runtime identity because the operation has no value argument.
#[skyline::hook(replace = smash::app::lua_bind::WorkModule::inc_int)]
unsafe fn hook_work_module_inc_int(boma: *mut smash::app::BattleObjectModuleAccessor, slot: i32) {
    record_for_boma(boma, "WorkModule::inc_int", &[LuaArg::Int(slot as i64)]);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = integer_point_key("WorkModule::inc_int", &[slot as i64]);
        if let Some((suppress, overrides)) = action_for_func(
            CAT_WORK_MODULE_INC_INT,
            motion,
            key,
            frame,
            "WorkModule::inc_int",
        ) {
            if suppress {
                return;
            }
            if let Some(replacement) = overrides
                .and_then(|item| item.work_module_inc_int_slot)
                .and_then(|item| i32::try_from(item).ok())
            {
                if replacement != slot {
                    original!()(boma, replacement);
                    return;
                }
            }
        }
    }
    original!()(boma, slot)
}

/// Capture and sparsely override the verified direct WorkModule value setters.
///
/// The slot and value are both part of the numeric identity. The editor keeps their authored
/// source tokens, while a live rule can replace only the numeric values proven by a capture.
#[skyline::hook(replace = smash::app::lua_bind::WorkModule::set_int)]
unsafe fn hook_work_module_set_int(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    value: i32,
    slot: i32,
) {
    let args = [LuaArg::Int(value as i64), LuaArg::Int(slot as i64)];
    record_for_boma(boma, "WorkModule::set_int", &args);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = numeric_point_key("WorkModule::set_int", &[value as f32, slot as f32]);
        if let Some((suppress, overrides)) = action_for_func(
            CAT_WORK_MODULE_SET,
            motion,
            key,
            frame,
            "WorkModule::set_int",
        ) {
            if suppress {
                return;
            }
            if let Some(overrides) = overrides {
                let replacement_value = overrides
                    .work_module_set_int_value
                    .and_then(|item| i32::try_from(item).ok())
                    .unwrap_or(value);
                let replacement_slot = overrides
                    .work_module_set_slot
                    .and_then(|item| i32::try_from(item).ok())
                    .unwrap_or(slot);
                if replacement_value != value || replacement_slot != slot {
                    original!()(boma, replacement_value, replacement_slot);
                    return;
                }
            }
        }
    }
    original!()(boma, value, slot)
}

/// Capture and sparsely override the verified direct `WorkModule::set_int64` shape. The exact
/// integer key is separate from the older setter key because hash40 payloads must not be narrowed
/// through `f32`.
#[skyline::hook(replace = smash::app::lua_bind::WorkModule::set_int64)]
unsafe fn hook_work_module_set_int64(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    value: i64,
    slot: i32,
) {
    let args = [LuaArg::Int(value), LuaArg::Int(slot as i64)];
    record_for_boma(boma, "WorkModule::set_int64", &args);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = integer_point_key("WorkModule::set_int64", &[value, slot as i64]);
        if let Some((suppress, overrides)) = action_for_func(
            CAT_WORK_MODULE_SET,
            motion,
            key,
            frame,
            "WorkModule::set_int64",
        ) {
            if suppress {
                return;
            }
            if let Some(overrides) = overrides {
                let replacement_value = overrides.work_module_set_int64_value.unwrap_or(value);
                let replacement_slot = overrides
                    .work_module_set_slot
                    .and_then(|item| i32::try_from(item).ok())
                    .unwrap_or(slot);
                if replacement_value != value || replacement_slot != slot {
                    original!()(boma, replacement_value, replacement_slot);
                    return;
                }
            }
        }
    }
    original!()(boma, value, slot)
}

#[skyline::hook(replace = smash::app::lua_bind::WorkModule::set_float)]
unsafe fn hook_work_module_set_float(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    value: f32,
    slot: i32,
) {
    let args = [LuaArg::Num(value), LuaArg::Int(slot as i64)];
    record_for_boma(boma, "WorkModule::set_float", &args);
    if any_direct_rules(boma) {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let key = numeric_point_key("WorkModule::set_float", &[value, slot as f32]);
        if let Some((suppress, overrides)) = action_for_func(
            CAT_WORK_MODULE_SET,
            motion,
            key,
            frame,
            "WorkModule::set_float",
        ) {
            if suppress {
                return;
            }
            if let Some(overrides) = overrides {
                let replacement_value = overrides
                    .work_module_set_float_value
                    .filter(|item| item.is_finite())
                    .unwrap_or(value);
                let replacement_slot = overrides
                    .work_module_set_slot
                    .and_then(|item| i32::try_from(item).ok())
                    .unwrap_or(slot);
                if replacement_value != value || replacement_slot != slot {
                    original!()(boma, replacement_value, replacement_slot);
                    return;
                }
            }
        }
    }
    original!()(boma, value, slot)
}

/// Capture and sparsely override the verified `SET_SPEED_EX` shape.
///
/// The third argument is the kinetic-energy kind and is the rule key when the editor has a
/// numeric capture. A rule with no key remains useful for a source-backed move with one call at
/// a frame, while the editor refuses that fallback when several calls share the frame.
#[skyline::hook(replace = smash::app::sv_animcmd::SET_SPEED_EX)]
unsafe fn hook_set_speed_ex(lua_state: u64) {
    let args = read_args_exact(lua_state, 3);
    record(lua_state, "SET_SPEED_EX", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            let kind = match args.get(2) {
                Some(LuaArg::Int(value)) => *value as u64,
                Some(LuaArg::Num(value)) => *value as u64,
                _ => u64::MAX,
            };
            if let Some((suppress, overrides)) = action_for(CAT_SPEED_EX, motion, kind, frame) {
                if suppress {
                    return;
                }
                if let Some(overrides) = overrides {
                    let mut values = args.clone();
                    let set_speed = |slot: usize, value: Option<f32>, values: &mut Vec<LuaArg>| {
                        let Some(value) = value else { return };
                        if values.get(slot).is_some() {
                            // Velocity arguments are not integer keys. Keep fractional edits as
                            // Lua numbers even if a decompiled call happened to arrive as an
                            // integer value.
                            values[slot] = LuaArg::Num(value);
                        }
                    };
                    set_speed(0, overrides.speed_x, &mut values);
                    set_speed(1, overrides.speed_y, &mut values);
                    if values != args {
                        rewrite_args(lua_state, &values);
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the verified direct `SET_SPEED` shape.
#[skyline::hook(replace = smash::app::sv_animcmd::SET_SPEED)]
unsafe fn hook_set_speed(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    record(lua_state, "SET_SPEED", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            if let Some((suppress, overrides)) = action_for(CAT_SPEED, motion, 0, frame) {
                if suppress {
                    return;
                }
                if let Some(overrides) = overrides {
                    let mut values = args.clone();
                    let set_speed = |slot: usize, value: Option<f32>, values: &mut Vec<LuaArg>| {
                        let Some(value) = value else { return };
                        if values.get(slot).is_some() {
                            // SET_SPEED's two arguments are velocities, not integer keys. Keep
                            // fractional edits as Lua numbers even if a decompiled call happened
                            // to arrive as an integer value.
                            values[slot] = LuaArg::Num(value);
                        }
                    };
                    set_speed(0, overrides.speed_x, &mut values);
                    set_speed(1, overrides.speed_y, &mut values);
                    if values != args {
                        rewrite_args(lua_state, &values);
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the verified `ADD_SPEED_NO_LIMIT` shape.
#[skyline::hook(replace = smash::app::sv_animcmd::ADD_SPEED_NO_LIMIT)]
unsafe fn hook_add_speed_no_limit(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    record(lua_state, "ADD_SPEED_NO_LIMIT", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            if let Some((suppress, overrides)) =
                action_for(CAT_ADD_SPEED_NO_LIMIT, motion, 0, frame)
            {
                if suppress {
                    return;
                }
                if let Some(overrides) = overrides {
                    let mut values = args.clone();
                    let set_speed = |slot: usize, value: Option<f32>, values: &mut Vec<LuaArg>| {
                        let Some(value) = value else { return };
                        let Some(current) = values.get(slot).cloned() else {
                            return;
                        };
                        values[slot] = match current {
                            LuaArg::Int(_) => LuaArg::Int(value as i64),
                            _ => LuaArg::Num(value),
                        };
                    };
                    set_speed(0, overrides.speed_x, &mut values);
                    set_speed(1, overrides.speed_y, &mut values);
                    if values != args {
                        rewrite_args(lua_state, &values);
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the verified `CORRECT` shape.
#[skyline::hook(replace = smash::app::sv_animcmd::CORRECT)]
unsafe fn hook_correct(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    record(lua_state, "CORRECT", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            let kind = match args.first() {
                Some(LuaArg::Int(value)) => *value as u64,
                Some(LuaArg::Num(value)) => *value as u64,
                _ => u64::MAX,
            };
            if let Some((suppress, overrides)) = action_for(CAT_CORRECT, motion, kind, frame) {
                if suppress {
                    return;
                }
                if let Some(overrides) = overrides {
                    if let Some(replacement) = overrides.correct_kind {
                        let mut values = args.clone();
                        if !values.is_empty() {
                            values[0] = LuaArg::Int(replacement);
                            if values != args {
                                rewrite_args(lua_state, &values);
                            }
                        }
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the verified two-argument `FT_CATCH_STOP` shape.
///
/// The arguments are both `ToF32`, so the pristine numeric pair is the live rule key. This keeps
/// two catch-stop calls on one frame independent without assigning an unverified semantic name to
/// either slot.
#[skyline::hook(replace = smash::app::sv_animcmd::FT_CATCH_STOP)]
unsafe fn hook_ft_catch_stop(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    record(lua_state, "FT_CATCH_STOP", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            let numeric = args
                .iter()
                .map(|arg| match arg {
                    LuaArg::Int(value) => Some(*value as f32),
                    LuaArg::Num(value) => Some(*value),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(numeric) = numeric {
                let key = numeric_point_key("FT_CATCH_STOP", &numeric);
                if let Some((suppress, overrides)) =
                    action_for(CAT_FT_CATCH_STOP, motion, key, frame)
                {
                    if suppress {
                        return;
                    }
                    if let Some(overrides) = overrides {
                        let mut values = args.clone();
                        if let Some(value) = overrides.ft_catch_stop_arg1 {
                            values[0] = LuaArg::Num(value);
                        }
                        if let Some(value) = overrides.ft_catch_stop_arg2 {
                            values[1] = LuaArg::Num(value);
                        }
                        if values != args {
                            rewrite_args(lua_state, &values);
                        }
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// Capture and sparsely override the verified one-argument motion-frame adjustment shape.
///
/// The numeric payload is the live rule key. This keeps the editor from assigning an unverified
/// semantic name to the value while still allowing a source-backed point to be retuned.
#[skyline::hook(replace = smash::app::sv_animcmd::FT_START_ADJUST_MOTION_FRAME_arg1)]
unsafe fn hook_ft_start_adjust_motion_frame(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    record(lua_state, "FT_START_ADJUST_MOTION_FRAME_arg1", &args);
    if any_rules() {
        let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        if !boma.is_null() {
            let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
            let frame = smash::app::lua_bind::MotionModule::frame(boma);
            let Some(value) = args.first().and_then(|arg| match arg {
                LuaArg::Int(value) => Some(*value as f32),
                LuaArg::Num(value) => Some(*value),
                _ => None,
            }) else {
                original!()(lua_state);
                return;
            };
            let key = numeric_point_key("FT_START_ADJUST_MOTION_FRAME_arg1", &[value]);
            if let Some((suppress, overrides)) =
                action_for(CAT_FT_START_ADJUST_MOTION_FRAME, motion, key, frame)
            {
                if suppress {
                    return;
                }
                if let Some(value) =
                    overrides.and_then(|item| item.ft_start_adjust_motion_frame_value)
                {
                    let mut values = args.clone();
                    if let Some(slot) = values.first_mut() {
                        *slot = LuaArg::Num(value);
                        if values != args {
                            rewrite_args(lua_state, &values);
                        }
                    }
                }
            }
        }
    }
    original!()(lua_state)
}
expression_hook!(hook_quake, smash::app::sv_animcmd::QUAKE, "QUAKE", 1);
expression_hook!(
    hook_ft_attack_abs_camera_quake,
    smash::app::sv_animcmd::FT_ATTACK_ABS_CAMERA_QUAKE,
    "FT_ATTACK_ABS_CAMERA_QUAKE",
    2
);

/// Convert the editor's four captured payload slots back to the native direct binding. The
/// receiver is not part of this vector: it is the boma passed to the hook itself. A source token
/// such as `*BATTLE_OBJECT_ID_INVALID as u32` is therefore represented by the pristine captured
/// integer before an edit, while changed values must arrive in the concrete native types below.
fn control_set_rumble_native_args(args: &[LuaArg]) -> Option<(smash::phx::Hash40, i32, bool, u32)> {
    let [kind, duration, looped, target] = args else {
        return None;
    };
    let kind = match kind {
        LuaArg::Hash(value) => smash::phx::Hash40::new_raw(*value),
        LuaArg::Int(value) if *value >= 0 => smash::phx::Hash40::new_raw(*value as u64),
        _ => return None,
    };
    let duration = match duration {
        LuaArg::Int(value) => i32::try_from(*value).ok()?,
        _ => return None,
    };
    let looped = match looped {
        LuaArg::Bool(value) => *value,
        _ => return None,
    };
    let target = match target {
        LuaArg::Int(value) => u32::try_from(*value).ok()?,
        _ => return None,
    };
    Some((kind, duration, looped, target))
}

/// Capture and sparsely override the measured direct `ControlModule::set_rumble` binding.
///
/// Unlike the three `sv_animcmd` expression hooks above, this primitive never touches the Lua
/// stack: it receives the module accessor and concrete native arguments directly. Recording the
/// same four payload types keeps the editor's expression key and the live rule key identical.
#[skyline::hook(replace = smash::app::lua_bind::ControlModule::set_rumble)]
unsafe fn hook_control_set_rumble(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    kind: smash::phx::Hash40,
    duration: i32,
    looped: bool,
    target: u32,
) {
    let args = [
        LuaArg::Hash(kind.hash),
        LuaArg::Int(duration as i64),
        LuaArg::Bool(looped),
        LuaArg::Int(target as i64),
    ];
    record_for_boma(boma, "ControlModule::set_rumble", &args);
    // Direct rumble injections enter this hook through the same native binding. They must pass
    // straight to the original call; applying authored rules again can suppress or rewrite a
    // replacement that happens to share a frame and payload with another call.
    if any_direct_rules(boma) && !is_injecting() {
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        if let Some((suppress, overrides)) =
            expression_action(motion, frame, "ControlModule::set_rumble", &args)
        {
            if suppress {
                return;
            }
            if let Some(overrides) = overrides {
                if let Some(replacement) = overrides
                    .expression_args
                    .as_deref()
                    .and_then(control_set_rumble_native_args)
                {
                    if replacement.0 != kind
                        || replacement.1 != duration
                        || replacement.2 != looped
                        || replacement.3 != target
                    {
                        original!()(
                            boma,
                            replacement.0,
                            replacement.1,
                            replacement.2,
                            replacement.3,
                        );
                        return;
                    }
                }
            }
        }
    }
    original!()(boma, kind, duration, looped, target)
}

/// Inject rules for a motion, tagged with the collision family to fire through.
fn injection_fingerprint(
    motion: u64,
    category: u8,
    injection_key: Option<u64>,
    injection: &InjectRule,
) -> u64 {
    let mut hash = fnv(0xcbf2_9ce4_8422_2325, motion);
    hash = fnv(hash, category as u64);
    // `hitbox_id` is a per-call discriminator for sound and expression additions. Keep it out of
    // the other families' fingerprints so their established injection deduplication remains
    // unchanged.
    if category == CAT_SOUND || category == CAT_EXPRESSION {
        hash = fnv(hash, injection_key.unwrap_or_default());
    }
    hash = fnv(hash, injection.frame.to_bits() as u64);
    for byte in injection.command.as_deref().unwrap_or_default().bytes() {
        hash = fnv(hash, byte as u64);
    }
    for arg in &injection.args {
        hash = fnv(hash, arg.dedupe_bits());
    }
    hash
}

fn injections_for(motion: u64) -> Vec<(u64, u8, InjectRule)> {
    let rules = RULES.lock();
    rules
        .iter()
        .filter(|r| r.motion == motion && r.inject.is_some())
        .map(|rule| {
            let injection = rule.inject.clone().unwrap();
            (
                injection_fingerprint(rule.motion, rule.category, rule.hitbox_id, &injection),
                rule.category,
                injection,
            )
        })
        .collect()
}

// ── ATTACK hooks ─────────────────────────────────────────────────────────────

/// Arg slot rewrite map for HbOverrides (index, value, type).
unsafe fn rewrite_attack_args(lua_state: u64, ov: &HbOverrides, args: &[LuaArg]) {
    let mut vals: Vec<LuaArg> = args.to_vec();
    let set_num = |idx: usize, v: Option<f32>, vals: &mut Vec<LuaArg>| {
        if let Some(v) = v {
            if idx < vals.len() {
                vals[idx] = LuaArg::Num(v);
            }
        }
    };
    let set_int = |idx: usize, v: Option<i64>, vals: &mut Vec<LuaArg>| {
        if let Some(v) = v {
            if idx < vals.len() {
                vals[idx] = LuaArg::Int(v);
            }
        }
    };
    set_int(1, ov.part, &mut vals);
    if let Some(bone) = ov.bone {
        if vals.len() > 2 {
            vals[2] = LuaArg::Hash(bone);
        }
    }
    set_num(3, ov.damage, &mut vals);
    set_int(4, ov.angle, &mut vals);
    set_int(5, ov.kbg, &mut vals);
    set_int(6, ov.fkb, &mut vals);
    set_int(7, ov.bkb, &mut vals);
    set_num(8, ov.size, &mut vals);
    set_num(9, ov.x, &mut vals);
    set_num(10, ov.y, &mut vals);
    set_num(11, ov.z, &mut vals);
    if ov.capsule == Some(false) {
        for idx in 12..=14 {
            if idx < vals.len() {
                vals[idx] = LuaArg::Nil;
            }
        }
    } else {
        set_num(12, ov.x2, &mut vals);
        set_num(13, ov.y2, &mut vals);
        set_num(14, ov.z2, &mut vals);
    }
    set_num(15, ov.hitlag, &mut vals);
    set_num(16, ov.sdi, &mut vals);

    // Attribute slots keep the SCRIPT's own lua type and only take a new value. Scripts vary
    // between pushing a given slot as Int, Num or Bool, and the game reads a wrongly-typed
    // slot as garbage. Because overrides are sparse, an unexpected/sentinel source type is
    // replaced only when the user explicitly edited that slot.
    let set_scalar = |idx: usize, v: Option<i64>, vals: &mut Vec<LuaArg>| {
        let Some(v) = v else { return };
        if idx >= vals.len() {
            return;
        }
        let next = match vals[idx] {
            LuaArg::Int(_) => LuaArg::Int(v),
            LuaArg::Num(_) => LuaArg::Num(v as f32),
            LuaArg::Bool(_) => LuaArg::Bool(v != 0),
            // Overrides are sparse now, so reaching a sentinel or unexpected source type means
            // the user explicitly changed this slot. Use the declared editor type rather than
            // silently ignoring the edit.
            _ => LuaArg::Int(v),
        };
        vals[idx] = next;
    };
    let set_flag = |idx: usize, v: Option<bool>, vals: &mut Vec<LuaArg>| {
        let Some(v) = v else { return };
        if idx >= vals.len() {
            return;
        }
        let next = match vals[idx] {
            LuaArg::Bool(_) => LuaArg::Bool(v),
            LuaArg::Int(_) => LuaArg::Int(v as i64),
            LuaArg::Num(_) => LuaArg::Num(if v { 1.0 } else { 0.0 }),
            _ => LuaArg::Bool(v),
        };
        vals[idx] = next;
    };
    let set_scalar_f = |idx: usize, v: Option<f32>, vals: &mut Vec<LuaArg>| {
        let Some(v) = v else { return };
        if idx >= vals.len() {
            return;
        }
        let next = match vals[idx] {
            LuaArg::Num(_) => LuaArg::Num(v),
            LuaArg::Int(_) => LuaArg::Int(v as i64),
            _ => LuaArg::Num(v),
        };
        vals[idx] = next;
    };
    set_scalar(17, ov.setoff, &mut vals);
    set_scalar(18, ov.lr_check, &mut vals);
    set_flag(19, ov.clang, &mut vals);
    set_scalar(20, ov.add_attack, &mut vals);
    set_scalar_f(21, ov.hitbox_attr, &mut vals);
    set_scalar(22, ov.ground_or_air, &mut vals);
    set_flag(23, ov.mtk, &mut vals);
    set_flag(24, ov.shield_disable, &mut vals);
    set_flag(25, ov.reflectable, &mut vals);
    set_flag(26, ov.absorbable, &mut vals);
    set_flag(27, ov.landing_attack, &mut vals);
    set_scalar(28, ov.situation_mask, &mut vals);
    set_scalar(29, ov.category_mask, &mut vals);
    set_scalar(30, ov.part_mask, &mut vals);
    set_flag(31, ov.no_finish_camera, &mut vals);
    if let Some(h) = ov.collision_attr {
        if vals.len() > 32 {
            vals[32] = LuaArg::Hash(h);
        }
    }
    set_scalar(33, ov.sound_level, &mut vals);
    set_scalar(34, ov.sound_attr, &mut vals);
    set_scalar(35, ov.attack_region, &mut vals);

    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for v in &vals {
        let mut l2c = v.to_l2c();
        agent.push_lua_stack(&mut l2c);
    }
}

/// Rewrite the established `ATTACK_FP` fields at their own slot indices. Geometry and the
/// undocumented tail stay exactly as the captured call supplied them.
unsafe fn rewrite_attack_fp_args(lua_state: u64, ov: &HbOverrides, args: &[LuaArg]) {
    let mut vals: Vec<LuaArg> = args.to_vec();
    let set_num = |idx: usize, value: Option<f32>, vals: &mut Vec<LuaArg>| {
        let Some(value) = value else { return };
        if idx >= vals.len() {
            return;
        }
        vals[idx] = match vals[idx] {
            LuaArg::Int(_) => LuaArg::Int(value as i64),
            _ => LuaArg::Num(value),
        };
    };
    let set_int = |idx: usize, value: Option<i64>, vals: &mut Vec<LuaArg>| {
        let Some(value) = value else { return };
        if idx >= vals.len() {
            return;
        }
        vals[idx] = match vals[idx] {
            LuaArg::Num(_) => LuaArg::Num(value as f32),
            LuaArg::Bool(_) => LuaArg::Bool(value != 0),
            _ => LuaArg::Int(value),
        };
    };
    let set_flag = |idx: usize, value: Option<bool>, vals: &mut Vec<LuaArg>| {
        let Some(value) = value else { return };
        if idx >= vals.len() {
            return;
        }
        vals[idx] = match vals[idx] {
            LuaArg::Int(_) => LuaArg::Int(value as i64),
            LuaArg::Num(_) => LuaArg::Num(if value { 1.0 } else { 0.0 }),
            _ => LuaArg::Bool(value),
        };
    };
    set_int(1, ov.part, &mut vals);
    set_num(3, ov.damage, &mut vals);
    set_int(4, ov.angle, &mut vals);
    set_int(5, ov.kbg, &mut vals);
    set_int(6, ov.fkb, &mut vals);
    set_int(7, ov.bkb, &mut vals);
    set_num(14, ov.hitlag, &mut vals);
    set_num(15, ov.sdi, &mut vals);
    set_flag(16, ov.clang, &mut vals);
    set_int(19, ov.sound_level, &mut vals);
    set_int(20, ov.sound_attr, &mut vals);
    set_int(21, ov.ground_or_air, &mut vals);
    set_int(23, ov.attack_region, &mut vals);
    set_flag(29, ov.reflectable, &mut vals);
    set_flag(30, ov.absorbable, &mut vals);
    set_int(34, ov.lr_check, &mut vals);
    if let Some(hash) = ov.collision_attr {
        if vals.len() > 12 {
            vals[12] = LuaArg::Hash(hash);
        }
    }
    rewrite_args(lua_state, &vals);
}

macro_rules! attack_hook {
    ($hook_name:ident, $target:path, $func:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            // Read (non-destructively) BEFORE original consumes the stack. Fixed arity:
            // sphere hitboxes push Void capsule slots mid-args.
            let args = read_args_exact(lua_state, ATTACK_ARGC);
            if args.len() >= 12 {
                // Capture the PRISTINE script line (even when suppressed/modified).
                record(lua_state, $func, &args);

                if any_rules() {
                    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                        as *mut smash::app::BattleObjectModuleAccessor;
                    if !boma.is_null() {
                        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                        let frame = smash::app::lua_bind::MotionModule::frame(boma);
                        let id = match args.first() {
                            Some(LuaArg::Int(i)) => *i as u64,
                            _ => u64::MAX,
                        };
                        if let Some((suppress, overrides)) =
                            action_for(CAT_ATTACK, motion, id, frame)
                        {
                            if suppress {
                                return; // hitbox never comes out
                            }
                            if let Some(ov) = overrides {
                                rewrite_attack_args(lua_state, &ov, &args);
                            }
                        }
                    }
                }
            }
            original!()(lua_state);
        }
    };
}

attack_hook!(hook_attack, smash::app::sv_animcmd::ATTACK, "ATTACK");
attack_hook!(
    hook_attack_ignore_throw,
    smash::app::sv_animcmd::ATTACK_IGNORE_THROW,
    "ATTACK_IGNORE_THROW"
);

/// `ATTACK_FP` has the same collision lifecycle as ATTACK but a different 41-slot payload.
#[skyline::hook(replace = smash::app::sv_animcmd::ATTACK_FP)]
unsafe fn hook_attack_fp(lua_state: u64) {
    let args = read_args_exact(lua_state, ATTACK_FP_ARGC);
    if args.len() >= ATTACK_FP_ARGC as usize {
        record(lua_state, "ATTACK_FP", &args);
        if any_rules() {
            let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                as *mut smash::app::BattleObjectModuleAccessor;
            if !boma.is_null() {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                let id = match args.first() {
                    Some(LuaArg::Int(value)) => *value as u64,
                    Some(LuaArg::Num(value)) => *value as u64,
                    _ => u64::MAX,
                };
                if let Some((suppress, overrides)) = action_for(CAT_ATTACK_FP, motion, id, frame) {
                    if suppress {
                        return;
                    }
                    if let Some(overrides) = overrides {
                        rewrite_attack_fp_args(lua_state, &overrides, &args);
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

/// `AttackModule::clear_all` — when hitboxes STOP.
///
/// The editor had no source for this at all: a captured hitbox was given a hardcoded two-frame
/// lifetime, so every live-fetched hitbox ended far earlier than the script says. Scripts end
/// their hitboxes with `AttackModule::clear_all`, which is a lua_bind call rather than an
/// sv_animcmd primitive — so it needs its own hook rather than riding `attack_hook!`.
///
/// The engine calls this constantly (state changes, and every frame in some situations), so
/// only a clear that actually ends something is recorded. Without that gate the capture stream
/// would be mostly clears.
#[skyline::hook(replace = smash::app::lua_bind::AttackModule::clear_all)]
unsafe fn hook_attack_clear_all(boma: *mut smash::app::BattleObjectModuleAccessor) {
    if !boma.is_null() && close_collision_if_open((*boma).battle_object_id) {
        record_for_boma(boma, "ATTACK_CLEAR_ALL", &[]);
    }
    original!()(boma)
}

// ── CATCH (grabbox) hook ─────────────────────────────────────────────────────
// Arg layout (0-based): 0 id, 1 bone(h), 2 size, 3 x, 4 y, 5 z, 6 x2, 7 y2, 8 z2
// (nil = sphere), 9 status, 10 situation. Bone and geometry are rewritable.

/// Rewrite grab geometry (size at slot 2, offsets 3..8) from HbOverrides.
unsafe fn rewrite_grab_args(lua_state: u64, ov: &HbOverrides, args: &[LuaArg]) {
    let mut vals: Vec<LuaArg> = args.to_vec();
    let set = |idx: usize, v: Option<f32>, vals: &mut Vec<LuaArg>| {
        if let Some(v) = v {
            if idx < vals.len() {
                vals[idx] = LuaArg::Num(v);
            }
        }
    };
    if let Some(bone) = ov.bone {
        if vals.len() > 1 {
            vals[1] = LuaArg::Hash(bone);
        }
    }
    set(2, ov.size, &mut vals);
    set(3, ov.x, &mut vals);
    set(4, ov.y, &mut vals);
    set(5, ov.z, &mut vals);
    if ov.capsule == Some(false) {
        for idx in 6..=8 {
            if idx < vals.len() {
                vals[idx] = LuaArg::Nil;
            }
        }
    } else {
        set(6, ov.x2, &mut vals);
        set(7, ov.y2, &mut vals);
        set(8, ov.z2, &mut vals);
    }

    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for v in &vals {
        let mut l2c = v.to_l2c();
        agent.push_lua_stack(&mut l2c);
    }
}

#[skyline::hook(replace = smash::app::sv_animcmd::CATCH)]
unsafe fn hook_catch(lua_state: u64) {
    let args = read_args_exact(lua_state, CATCH_ARGC);
    if args.len() >= 6 {
        record(lua_state, "CATCH", &args);
        if any_rules() {
            let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                as *mut smash::app::BattleObjectModuleAccessor;
            if !boma.is_null() {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                // CATCH's id may arrive as Int OR Num depending on how the script pushed it —
                // accept both so grab rules actually match (Int-only missed float ids).
                let id = match args.first() {
                    Some(LuaArg::Int(i)) => *i as u64,
                    Some(LuaArg::Num(n)) => *n as u64,
                    _ => u64::MAX,
                };
                if let Some((suppress, overrides)) = action_for(CAT_GRAB, motion, id, frame) {
                    crate::slight::diag::note(format!(
                        "grab rule hit (motion {motion:#x} id {id} frame {frame:.1} suppress {suppress})"
                    ));
                    if suppress {
                        return;
                    }
                    if let Some(ov) = overrides {
                        rewrite_grab_args(lua_state, &ov, &args);
                    }
                }
            }
        }
    }
    original!()(lua_state);
}

// ── SEARCH (detection volume) hook ───────────────────────────────────────────

/// Rewrite a `SEARCH` call's geometry in place.
///
/// The capsule slots are only touched when the live call actually has them. A vanilla script
/// pushes the 14-argument shape, where slot 7 is `collision_kind` — writing an endpoint there
/// would leave the box looking for something the editor never asked for. This is the same trap
/// the parser and the write-back each have their own guard for; here the signal is the argument
/// count, because that is what a live stack carries.
unsafe fn rewrite_search_args(lua_state: u64, ov: &HbOverrides, args: &[LuaArg]) {
    let mut vals: Vec<LuaArg> = args.to_vec();
    let has_capsule_slots = vals.len() >= SEARCH_ARGC as usize;
    let set = |idx: usize, v: Option<f32>, vals: &mut Vec<LuaArg>| {
        if let Some(v) = v {
            if idx < vals.len() {
                vals[idx] = LuaArg::Num(v);
            }
        }
    };
    if let Some(bone) = ov.bone {
        if vals.len() > 2 {
            vals[2] = LuaArg::Hash(bone);
        }
    }
    set(3, ov.size, &mut vals);
    set(4, ov.x, &mut vals);
    set(5, ov.y, &mut vals);
    set(6, ov.z, &mut vals);
    if has_capsule_slots {
        if ov.capsule == Some(false) {
            for idx in 7..=9 {
                vals[idx] = LuaArg::Nil;
            }
        } else {
            set(7, ov.x2, &mut vals);
            set(8, ov.y2, &mut vals);
            set(9, ov.z2, &mut vals);
        }
    }

    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for v in &vals {
        let mut l2c = v.to_l2c();
        agent.push_lua_stack(&mut l2c);
    }
}

#[skyline::hook(replace = smash::app::sv_animcmd::SEARCH)]
unsafe fn hook_search(lua_state: u64) {
    let args = read_args_exact(lua_state, SEARCH_ARGC);
    if args.len() >= 8 {
        record(lua_state, "SEARCH", &args);
        if any_rules() {
            let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                as *mut smash::app::BattleObjectModuleAccessor;
            if !boma.is_null() {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                // Same Int-or-Num tolerance `CATCH` needs: how the id arrives depends on how
                // the script pushed it, and an Int-only read misses float ids entirely.
                let id = match args.first() {
                    Some(LuaArg::Int(i)) => *i as u64,
                    Some(LuaArg::Num(n)) => *n as u64,
                    _ => u64::MAX,
                };
                if let Some((suppress, overrides)) = action_for(CAT_SEARCH, motion, id, frame) {
                    crate::slight::diag::note(format!(
                        "search rule hit (motion {motion:#x} id {id} frame {frame:.1} suppress {suppress})"
                    ));
                    if suppress {
                        return;
                    }
                    if let Some(ov) = overrides {
                        rewrite_search_args(lua_state, &ov, &args);
                    }
                }
            }
        }
    }
    original!()(lua_state);
}

// ── WIND (AREA_WIND family) hooks ────────────────────────────────────────────
// Arity and layout are exact: id, four wind-physics values, object-relative X/Y, then
// radius (RAD) or width/height (rectangle), with an optional final lifetime.

unsafe fn rewrite_wind_args(lua_state: u64, args: &[LuaArg]) {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for arg in args {
        let mut value = arg.to_l2c();
        agent.push_lua_stack(&mut value);
    }
}

macro_rules! wind_hook {
    ($hook_name:ident, $target:path, $func:literal, $arity:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let args = read_args_typed(lua_state, WIND_ARGC_MAX);
            if !args.is_empty() {
                record(lua_state, $func, &args);
                if any_rules() {
                    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                        as *mut smash::app::BattleObjectModuleAccessor;
                    if !boma.is_null() {
                        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                        let frame = smash::app::lua_bind::MotionModule::frame(boma);
                        let id = match args.first() {
                            Some(LuaArg::Int(id)) => *id as u64,
                            Some(LuaArg::Num(id)) => *id as u64,
                            _ => u64::MAX,
                        };
                        if let Some((suppress, overrides)) = action_for(CAT_WIND, motion, id, frame)
                        {
                            if suppress {
                                return;
                            }
                            if let Some(wind_args) = overrides
                                .and_then(|value| value.wind_args)
                                .filter(|args| args.len() == $arity)
                            {
                                rewrite_wind_args(lua_state, &wind_args);
                            }
                        }
                    }
                }
            }
            original!()(lua_state);
        }
    };
}

wind_hook!(
    hook_wind_2nd,
    smash::app::sv_animcmd::AREA_WIND_2ND,
    "AREA_WIND_2ND",
    9
);

#[skyline::hook(replace = smash::app::lua_bind::AreaModule::erase_wind)]
unsafe fn hook_erase_wind(boma: *mut smash::app::BattleObjectModuleAccessor, id: i32) -> u64 {
    record_for_boma(boma, "AREA_WIND_ERASE", &[LuaArg::Int(id as i64)]);
    original!()(boma, id)
}
wind_hook!(
    hook_wind_2nd_rad,
    smash::app::sv_animcmd::AREA_WIND_2ND_RAD,
    "AREA_WIND_2ND_RAD",
    8
);
wind_hook!(
    hook_wind_2nd_rad_arg9,
    smash::app::sv_animcmd::AREA_WIND_2ND_RAD_arg9,
    "AREA_WIND_2ND_RAD_arg9",
    9
);
wind_hook!(
    hook_wind_2nd_arg10,
    smash::app::sv_animcmd::AREA_WIND_2ND_arg10,
    "AREA_WIND_2ND_arg10",
    10
);

/// `ATTACK_ABS` — damage applied to an opponent already caught.
///
/// Recorded so a throw captured live shows its damage, and rewritable through the same
/// `HbOverrides` the attack hook uses — but through this family's *own* slot numbers. The
/// layout has 16 arguments to `ATTACK`'s 36 and orders them differently, so `rewrite_attack_args`
/// is deliberately not reused.
///
/// The rule key is the absolute kind rather than the id: every vanilla call writes id 0, and
/// kirby/ThrowF issues two in one block that differ only by kind.
#[skyline::hook(replace = smash::app::sv_animcmd::ATTACK_ABS)]
unsafe fn hook_attack_abs(lua_state: u64) {
    let args = read_args_exact(lua_state, ATTACK_ABS_ARGC);
    if args.len() >= ATTACK_ABS_ARGC as usize {
        record(lua_state, "ATTACK_ABS", &args);
        if any_rules() {
            let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                as *mut smash::app::BattleObjectModuleAccessor;
            if !boma.is_null() {
                let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
                let frame = smash::app::lua_bind::MotionModule::frame(boma);
                let kind = match args.first() {
                    Some(LuaArg::Int(k)) => *k as u64,
                    Some(LuaArg::Num(k)) => *k as u64,
                    _ => u64::MAX,
                };
                if let Some((suppress, overrides)) = action_for(CAT_ABS, motion, kind, frame) {
                    if suppress {
                        return;
                    }
                    if let Some(ov) = overrides {
                        let mut vals = args.to_vec();
                        // Slot numbers are this family's own. `unk`/`unk2`/`unk3` (8, 10, 11
                        // zero-based) are never written — the editor exposes no control for
                        // them and they are invariant in every vanilla call.
                        let set_i = |i: usize, v: Option<i64>, vals: &mut Vec<LuaArg>| {
                            if let (Some(v), true) = (v, i < vals.len()) {
                                vals[i] = LuaArg::Int(v);
                            }
                        };
                        let set_f = |i: usize, v: Option<f32>, vals: &mut Vec<LuaArg>| {
                            if let (Some(v), true) = (v, i < vals.len()) {
                                vals[i] = LuaArg::Num(v);
                            }
                        };
                        set_f(2, ov.damage, &mut vals);
                        set_i(3, ov.angle, &mut vals);
                        set_i(4, ov.kbg, &mut vals);
                        set_i(5, ov.fkb, &mut vals);
                        set_i(6, ov.bkb, &mut vals);
                        set_f(7, ov.hitlag, &mut vals);
                        set_i(9, ov.lr_check, &mut vals);
                        if let (Some(attr), true) = (ov.collision_attr, vals.len() > 12) {
                            vals[12] = LuaArg::Hash(attr);
                        }
                        set_i(13, ov.sound_level, &mut vals);
                        set_i(14, ov.sound_attr, &mut vals);
                        set_i(15, ov.attack_region, &mut vals);
                        if vals != args {
                            rewrite_args(lua_state, &vals);
                        }
                    }
                }
            }
        }
    }
    original!()(lua_state)
}

// ── Hurtbox state hooks ──────────────────────────────────────────────────────
//
// `HIT_NODE(bone, status)` and `HIT_NO(group, status)` share a shape but not a family: slot 0
// is a hash in one and an integer in the other. They are hooked separately for that reason,
// and a rule's target is pushed back as the typed value it arrived as rather than coerced.

/// Push a whole argument vector back onto the lua stack, replacing what the script wrote.
unsafe fn rewrite_args(lua_state: u64, args: &[LuaArg]) {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for arg in args {
        let mut value = arg.to_l2c();
        agent.push_lua_stack(&mut value);
    }
}

/// Rule key for `COL_PRI`, which is per fighter rather than per target.
///
/// Not a value any bone hash or group number reaches. Must equal the editor's
/// `game_link::HURT_KEY_COL_PRI`.
const HURT_KEY_COL_PRI: u64 = u64::MAX;

/// Rule key for `WHOLE_HIT`, the other member with no target of its own.
///
/// Distinct from [`HURT_KEY_COL_PRI`] on purpose: one shared sentinel would let a rule written
/// for either macro match the other in the same frame window. Must equal the editor's
/// `game_link::HURT_KEY_WHOLE`.
const HURT_KEY_WHOLE: u64 = u64::MAX - 1;

/// Rule key for `damage!(…, MA_MSC_DAMAGE_DAMAGE_NO_REACTION, …)`. Keep distinct from
/// `WHOLE_HIT` so a fighter-wide armor rule can never rewrite a whole-body hit-status slot.
const HURT_KEY_DAMAGE_REACTION: u64 = u64::MAX - 3;

/// Which slots a hurtbox macro's arguments occupy.
///
/// The three shapes in [`CAT_HURT`] do not agree, and the override application below is
/// positional, so the shape is passed in rather than re-derived from `func` at each use.
#[derive(Clone, Copy)]
enum HurtShape {
    /// `HIT_NODE` / `HIT_NO` — target in slot 0, status in slot 1.
    Targeted,
    /// `WHOLE_HIT` — status in slot 0 and no target slot at all.
    WholeBody,
    /// `COL_PRI` — priority in slot 0 and no status.
    Priority,
    /// `damage!(…, MA_MSC_DAMAGE_DAMAGE_NO_REACTION, mode, value)` — mode/value in Lua slots 1/2
    /// after the command token in slot 0.
    DamageReaction,
}

/// Capture one hurtbox call and apply any rule matching it, returning `true` to suppress.
///
/// `target_key` is what the rule matches on — the bone hash or the group number — so a move
/// that makes four bones intangible can have one of them changed without touching the rest.
/// The targetless members pass a sentinel instead, one each.
unsafe fn hurt_action(
    lua_state: u64,
    func: &'static str,
    args: &[LuaArg],
    target_key: u64,
    shape: HurtShape,
) -> bool {
    record(lua_state, func, args);
    if !any_rules() {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    // A module accessor can reach this hook for articles, weapons, and other agents as well as
    // fighters. Their motion hashes are not a safe namespace for editor hurtbox rules, and an
    // article call must never suppress or rewrite a selected fighter's HIT_NODE/HIT_NO state.
    if smash::app::utility::get_category(&mut *boma)
        != *smash::lib::lua_const::BATTLE_OBJECT_CATEGORY_FIGHTER
    {
        return false;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    // Hurtbox targets share one category, and `WHOLE_HIT`/`HIT_NODE`/`HIT_NO` do not
    // have interchangeable argument layouts.  Match the command as well as the target
    // sentinel so a same-frame COL_PRI or WHOLE_HIT rule can never rewrite a status slot.
    let Some((suppress, overrides)) = action_for_func(CAT_HURT, motion, target_key, frame, func)
    else {
        return false;
    };
    if suppress {
        return true;
    }
    let Some(ov) = overrides else {
        return false;
    };
    let mut vals = args.to_vec();
    // Each override is applied only to the slot its own shape puts it in, and only where that
    // slot exists — so a rule meant for one macro can never lengthen a call of another, nor
    // write into a slot that means something else here. `WHOLE_HIT` is why this is a match
    // rather than three independent bounds checks: its status is in slot 0, which is the
    // *target* slot for the targeted pair and the *priority* slot for `COL_PRI`.
    let slot = |vals: &mut Vec<LuaArg>, i: usize, v: LuaArg| {
        if i < vals.len() {
            vals[i] = v;
        }
    };
    match shape {
        HurtShape::Targeted => {
            if let Some(target) = ov.hit_target.clone() {
                slot(&mut vals, 0, target);
            }
            if let Some(status) = ov.hit_status {
                slot(&mut vals, 1, LuaArg::Int(status));
            }
        }
        HurtShape::WholeBody => {
            // No `hit_target` arm: this macro has no target argument, and the editor does not
            // send one for it.
            if let Some(status) = ov.hit_status {
                slot(&mut vals, 0, LuaArg::Int(status));
            }
        }
        HurtShape::Priority => {
            if let Some(pri) = ov.col_pri {
                slot(&mut vals, 0, LuaArg::Int(pri));
            }
        }
        HurtShape::DamageReaction => {
            if let Some(mode) = ov.hurt_condition_mode {
                slot(&mut vals, 1, LuaArg::Int(mode));
            }
            if let Some(value) = ov.hurt_condition_value {
                slot(&mut vals, 2, LuaArg::Num(value));
            }
        }
    }
    if vals != args {
        rewrite_args(lua_state, &vals);
    }
    false
}

#[skyline::hook(replace = smash::app::sv_animcmd::HIT_NODE)]
unsafe fn hook_hit_node(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    if args.len() >= 2 {
        let bone = match args.first() {
            Some(LuaArg::Hash(h)) => *h,
            Some(LuaArg::Int(h)) => *h as u64,
            Some(LuaArg::Num(h)) => *h as u64,
            _ => u64::MAX,
        };
        if hurt_action(lua_state, "HIT_NODE", &args, bone, HurtShape::Targeted) {
            return;
        }
    }
    original!()(lua_state)
}

#[skyline::hook(replace = smash::app::sv_animcmd::HIT_NO)]
unsafe fn hook_hit_no(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    if args.len() >= 2 {
        let group = match args.first() {
            Some(LuaArg::Int(n)) => *n as u64,
            Some(LuaArg::Num(n)) => *n as u64,
            _ => u64::MAX,
        };
        if hurt_action(lua_state, "HIT_NO", &args, group, HurtShape::Targeted) {
            return;
        }
    }
    original!()(lua_state)
}

#[skyline::hook(replace = smash::app::sv_animcmd::COL_PRI)]
unsafe fn hook_col_pri(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    if !args.is_empty() {
        // `COL_PRI` is per fighter rather than per target, so every rule for it shares one key.
        if hurt_action(
            lua_state,
            "COL_PRI",
            &args,
            HURT_KEY_COL_PRI,
            HurtShape::Priority,
        ) {
            return;
        }
    }
    original!()(lua_state)
}

/// `WHOLE_HIT(status)` — every bone's hurtbox state at once.
///
/// Belongs to this family rather than to the attack hooks despite the `HIT` in its name: the
/// single argument is a `HIT_STATUS_*`, so it changes how the fighter *receives* hits. Like
/// `COL_PRI` it has no target of its own and so matches on a sentinel key.
#[skyline::hook(replace = smash::app::sv_animcmd::WHOLE_HIT)]
unsafe fn hook_whole_hit(lua_state: u64) {
    let args = read_args_exact(lua_state, 1);
    if !args.is_empty() {
        if hurt_action(
            lua_state,
            "WHOLE_HIT",
            &args,
            HURT_KEY_WHOLE,
            HurtShape::WholeBody,
        ) {
            return;
        }
    }
    original!()(lua_state)
}

/// `damage!(agent, MA_MSC_DAMAGE_DAMAGE_NO_REACTION, mode, value)` is a module-access call
/// rather than an `sv_animcmd` function. It still runs with the same Lua state and motion/frame
/// context, so the hurtbox rule lane can apply a frame-scoped armor override without changing the
/// game's authored script or the status geometry.
#[skyline::hook(replace = smash::app::sv_module_access::damage)]
unsafe fn hook_damage_no_reaction(lua_state: u64) {
    let args = read_args_exact(lua_state, 3);
    if args.len() >= 3
        && matches!(args.first(), Some(LuaArg::Int(0)) | Some(LuaArg::Num(0.0)))
        && hurt_action(
            lua_state,
            "DAMAGE_NO_REACTION",
            &args,
            HURT_KEY_DAMAGE_REACTION,
            HurtShape::DamageReaction,
        )
    {
        return;
    }
    original!()(lua_state)
}

/// Capture one post-hoc tuning call and apply any rule matching it, returning `true` to suppress.
///
/// Both members are `(id, value)`, so one function serves both; `category` is what separates
/// them, and the key is the hitbox id the call names. Not folded into [`hurt_action`] even
/// though the shape work looks similar: these write different fields into different slots, and
/// sharing that function is exactly how the `WHOLE_HIT` slot-0 corruption got in.
unsafe fn attack_mod_action(
    lua_state: u64,
    func: &'static str,
    args: &[LuaArg],
    category: u8,
) -> bool {
    record(lua_state, func, args);
    if !any_rules() {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    let id = match args.first() {
        Some(LuaArg::Int(n)) => *n as u64,
        Some(LuaArg::Num(n)) => *n as u64,
        _ => return false,
    };
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let Some((suppress, overrides)) = action_for(category, motion, id, frame) else {
        return false;
    };
    if suppress {
        return true;
    }
    let Some(ov) = overrides else {
        return false;
    };
    let mut vals = args.to_vec();
    // Slot 0 is the id and slot 1 the value, for both members — the order `macros.rs` declares.
    if let Some(new_id) = ov.atk_mod_id {
        if !vals.is_empty() {
            vals[0] = LuaArg::Int(new_id);
        }
    }
    if let Some(value) = ov.atk_mod_value {
        if vals.len() > 1 {
            // Written as a number, not an int: the slot is `ToF32`, and a fractional multiplier
            // is a value the editor can produce even though no vanilla call writes one.
            vals[1] = LuaArg::Num(value);
        }
    }
    if vals != args {
        rewrite_args(lua_state, &vals);
    }
    false
}

#[skyline::hook(replace = smash::app::sv_animcmd::ATK_POWER)]
unsafe fn hook_atk_power(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    if args.len() >= 2 && attack_mod_action(lua_state, "ATK_POWER", &args, CAT_ATK_POWER) {
        return;
    }
    original!()(lua_state)
}

#[skyline::hook(replace = smash::app::sv_animcmd::ATK_SET_SHIELD_SETOFF_MUL)]
unsafe fn hook_atk_set_shield_setoff_mul(lua_state: u64) {
    let args = read_args_exact(lua_state, 2);
    if args.len() >= 2
        && attack_mod_action(
            lua_state,
            "ATK_SET_SHIELD_SETOFF_MUL",
            &args,
            CAT_ATK_SETOFF_MUL,
        )
    {
        return;
    }
    original!()(lua_state)
}

/// The two argument-less members. Recorded so the editor can see where a move gives its
/// hurtboxes back — without them a captured state would run to the end of the timeline.
#[skyline::hook(replace = smash::app::sv_animcmd::HIT_RESET_ALL)]
unsafe fn hook_hit_reset_all(lua_state: u64) {
    record(lua_state, "HIT_RESET_ALL", &[]);
    original!()(lua_state)
}

// `COL_NORMAL` was hooked here, next to `COL_PRI`, back when both read as body collision. It is
// a colour-blend command — `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL` — so its hook now lives with the
// rest of that family in `effect_viewer::acmd_hooks`, which records it exactly as this did and
// additionally honours suppression. Only one hook may replace a given symbol, so this is a move
// rather than an addition. `COL_PRI` stays: it carries an editable value this side writes.

// ── Injection (per-frame, from the smashline line callback) ──────────────────

/// (boid, exact injection fingerprint) → motion/frame it last fired at. Editing a payload gives
/// it a new fingerprint and applies it immediately; unchanged injections remain one-shot.
static FIRED: LazyLock<Mutex<HashMap<(u32, u64), (u64, f32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Called every frame per agent (agent_extender line callback) with the AGENT'S lua state.
pub unsafe fn inject_tick(lua_state: u64) {
    if !any_rules() {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    // Rules describe the selected fighter's move. Article/weapon motion hashes are not a safe
    // namespace and can collide with fighter motions, so never inject editor boxes into them.
    if smash::app::utility::get_category(&mut *boma)
        != *smash::lib::lua_const::BATTLE_OBJECT_CATEGORY_FIGHTER
    {
        return;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let injections = injections_for(motion);
    if injections.is_empty() {
        return;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let boid = (*boma).battle_object_id;
    let live_fingerprints: HashSet<u64> = injections
        .iter()
        .map(|(fingerprint, _, _)| *fingerprint)
        .collect();
    FIRED
        .lock()
        .retain(|(fired_boid, fingerprint), (fired_motion, _)| {
            *fired_boid != boid
                || *fired_motion != motion
                || live_fingerprints.contains(fingerprint)
        });

    for (fingerprint, category, inj) in injections {
        let key = (boid, fingerprint);
        let due = frame >= inj.frame;
        let already = {
            let fired = FIRED.lock();
            fired
                .get(&key)
                .map(|(m, f)| *m == motion && frame >= *f)
                .unwrap_or(false)
        };
        if due && !already {
            let lifecycle_command = match (category, inj.command.as_deref()) {
                (CAT_GRAB, Some("GRAB_CLEAR_ALL")) => {
                    let _g = InjectGuard::new();
                    smash::app::lua_bind::GrabModule::clear_all(boma);
                    true
                }
                (CAT_WIND, Some("AREA_WIND_ERASE")) => {
                    let id = match inj.args.first() {
                        Some(LuaArg::Int(id)) => *id as i32,
                        Some(LuaArg::Num(id)) => *id as i32,
                        _ => -1,
                    };
                    if id >= 0 {
                        let _g = InjectGuard::new();
                        smash::app::lua_bind::AreaModule::erase_wind(boma, id);
                    }
                    true
                }
                _ => false,
            };
            if lifecycle_command {
                FIRED.lock().insert(key, (motion, frame));
                crate::slight::diag::note(format!(
                    "ended collision cat {category} (motion {motion:#x} frame {frame:.1})"
                ));
                continue;
            }
            let mut agent = smash::lib::L2CAgent::new(lua_state);
            agent.clear_lua_stack();
            let mut args = inj.args.clone();
            if category == CAT_SOUND {
                let Some(command) = inj.command.as_deref() else {
                    crate::slight::diag::note("rejected sound injection without a command");
                    continue;
                };
                let Some(expected) = sound_hooks::injection_arg_count(command) else {
                    crate::slight::diag::note(format!(
                        "rejected sound injection with unknown command {command}"
                    ));
                    continue;
                };
                if args.len() != expected {
                    crate::slight::diag::note(format!(
                        "rejected sound injection for {command} with {} args (expected {expected})",
                        args.len()
                    ));
                    continue;
                }
                sound_hooks::normalize_injection_args(&mut args, command);
            }
            if category == CAT_EXPRESSION {
                let Some(command) = inj.command.as_deref() else {
                    crate::slight::diag::note("rejected expression injection without a command");
                    continue;
                };
                let valid = match command {
                    "RUMBLE_HIT" => {
                        matches!(args.as_slice(), [LuaArg::Hash(_), LuaArg::Int(_)])
                    }
                    "QUAKE" => matches!(args.as_slice(), [LuaArg::Int(_)]),
                    "FT_ATTACK_ABS_CAMERA_QUAKE" => {
                        matches!(args.as_slice(), [LuaArg::Int(_), LuaArg::Int(_)])
                    }
                    "ControlModule::set_rumble" => control_set_rumble_native_args(&args).is_some(),
                    _ => false,
                };
                if !valid {
                    crate::slight::diag::note(format!(
                        "rejected expression injection for {command} with an invalid typed argument vector"
                    ));
                    continue;
                }
            }
            if category == CAT_REVERSE_LR && inj.command.as_deref() != Some("REVERSE_LR") {
                crate::slight::diag::note(
                    "rejected reverse_lr injection without REVERSE_LR command",
                );
                continue;
            }
            if category == CAT_FT_CATCH_STOP {
                crate::slight::diag::note(
                    "rejected FT_CATCH_STOP injection: this slice supports value overrides only",
                );
                continue;
            }
            if category == CAT_FT_START_ADJUST_MOTION_FRAME {
                crate::slight::diag::note(
                    "rejected FT_START_ADJUST_MOTION_FRAME_arg1 injection: this slice supports value overrides only",
                );
                continue;
            }
            if category == CAT_CLR_SPEED {
                crate::slight::diag::note(
                    "rejected CLR_SPEED injection: this slice supports numeric value overrides only",
                );
                continue;
            }
            if category == CAT_SET_AIR
                && (!args.is_empty() || inj.command.as_deref() != Some("SET_AIR"))
            {
                crate::slight::diag::note("rejected SET_AIR injection with wrong command or args");
                continue;
            }
            if category == CAT_KINETIC_CLEAR_SPEED_ALL
                && (!args.is_empty()
                    || inj.command.as_deref() != Some("KineticModule::clear_speed_all"))
            {
                crate::slight::diag::note(
                    "rejected kinetic clear_speed_all injection with wrong command or args",
                );
                continue;
            }
            if category == CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION
                && (args.len() != 2
                    || inj.command.as_deref()
                        != Some("KineticModule::set_consider_ground_friction"))
            {
                crate::slight::diag::note(
                    "rejected set_consider_ground_friction injection with wrong command or args",
                );
                continue;
            }
            if category == CAT_CHANGE_KINETIC
                && (args.len() != 1
                    || inj.command.as_deref() != Some("KineticModule::change_kinetic"))
            {
                crate::slight::diag::note(
                    "rejected change_kinetic injection with wrong command or args",
                );
                continue;
            }
            if category == CAT_KINETIC_ADD_SPEED
                && (args.len() != 3 || inj.command.as_deref() != Some("KineticModule::add_speed"))
            {
                crate::slight::diag::note(
                    "rejected kinetic add_speed injection with wrong command or args",
                );
                continue;
            }
            if category == CAT_GRAB && args.len() == 9 {
                args.push(LuaArg::Int(
                    *smash::lib::lua_const::FIGHTER_STATUS_KIND_CAPTURE_PULLED as i64,
                ));
                args.push(LuaArg::Int(
                    *smash::lib::lua_const::COLLISION_SITUATION_MASK_GA as i64,
                ));
            }
            if category == CAT_GRAB && args.len() != CATCH_ARGC as usize {
                crate::slight::diag::note(format!(
                    "rejected grab injection with {} args (expected {CATCH_ARGC})",
                    args.len()
                ));
                continue;
            }
            // The editor always sends the full arity for a search box. Firing a short one
            // would leave the trailing masks reading whatever the last call left behind, so
            // a mismatch is refused rather than padded with guesses.
            if category == CAT_SEARCH && args.len() != SEARCH_ARGC as usize {
                crate::slight::diag::note(format!(
                    "rejected search injection with {} args (expected {SEARCH_ARGC})",
                    args.len()
                ));
                continue;
            }
            if category == CAT_ATTACK_FP
                && (args.len() != ATTACK_FP_ARGC as usize
                    || inj.command.as_deref() != Some("ATTACK_FP"))
            {
                crate::slight::diag::note(format!(
                    "rejected ATTACK_FP injection with {} args or wrong command",
                    args.len()
                ));
                continue;
            }
            if category == CAT_HURT {
                let valid = match inj.command.as_deref() {
                    Some("HIT_NODE") | Some("HIT_NO") => args.len() == 2,
                    Some("WHOLE_HIT") => args.len() == 1,
                    Some("COL_PRI") => args.len() == 1,
                    Some("DAMAGE_NO_REACTION") => args.len() == 3,
                    _ => false,
                };
                if !valid {
                    crate::slight::diag::note(
                        "rejected hurtbox injection with wrong command or argument count",
                    );
                    continue;
                }
            }
            if !(category == CAT_EXPRESSION
                && inj.command.as_deref() == Some("ControlModule::set_rumble"))
            {
                for a in &args {
                    let mut v = a.to_l2c();
                    agent.push_lua_stack(&mut v);
                }
            }
            // Fire through the collision family the rule targets. The guard keeps the
            // replay out of the pristine capture (these functions are our own hooks).
            {
                let _g = InjectGuard::new();
                match category {
                    CAT_SOUND => {
                        let Some(command) = inj.command.as_deref() else {
                            crate::slight::diag::note(
                                "rejected sound injection without a command after validation",
                            );
                            continue;
                        };
                        if !sound_hooks::inject(agent.lua_state_agent, command) {
                            crate::slight::diag::note(format!(
                                "rejected sound injection with unknown command {command}"
                            ));
                            continue;
                        }
                    }
                    CAT_REVERSE_LR => smash::app::sv_animcmd::REVERSE_LR(agent.lua_state_agent),
                    CAT_SET_AIR => smash::app::sv_animcmd::SET_AIR(agent.lua_state_agent),
                    CAT_KINETIC_CLEAR_SPEED_ALL => {
                        smash::app::lua_bind::KineticModule::clear_speed_all(boma);
                    }
                    CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION => {
                        let consider_ground_friction = match args.first() {
                            Some(LuaArg::Bool(value)) => *value,
                            Some(LuaArg::Int(value)) => *value != 0,
                            _ => {
                                crate::slight::diag::note(
                                    "rejected set_consider_ground_friction injection with a non-bool toggle",
                                );
                                continue;
                            }
                        };
                        let Some(kinetic_energy_attribute) = args
                            .get(1)
                            .and_then(numeric_arg_f32)
                            .map(|value| value as i32)
                        else {
                            crate::slight::diag::note(
                                "rejected set_consider_ground_friction injection with a non-numeric attribute",
                            );
                            continue;
                        };
                        smash::app::lua_bind::KineticModule::set_consider_ground_friction(
                            boma,
                            consider_ground_friction,
                            kinetic_energy_attribute,
                        );
                    }
                    CAT_CHANGE_KINETIC => {
                        let kinetic_type = match args.first() {
                            Some(LuaArg::Int(value)) => *value as i32,
                            Some(LuaArg::Num(value)) => *value as i32,
                            _ => {
                                crate::slight::diag::note(
                                    "rejected change_kinetic injection with a non-numeric type",
                                );
                                continue;
                            }
                        };
                        smash::app::lua_bind::KineticModule::change_kinetic(boma, kinetic_type);
                    }
                    CAT_KINETIC_ADD_SPEED => {
                        let values = match args.as_slice() {
                            [x, y, z] => {
                                Some((numeric_arg_f32(x), numeric_arg_f32(y), numeric_arg_f32(z)))
                            }
                            _ => None,
                        };
                        let Some((Some(x), Some(y), Some(z))) = values else {
                            crate::slight::diag::note(
                                "rejected kinetic add_speed injection with non-numeric vector",
                            );
                            continue;
                        };
                        let vector = smash::phx::Vector3f { x, y, z };
                        smash::app::lua_bind::KineticModule::add_speed(boma, &vector);
                    }
                    CAT_EXPRESSION => match inj.command.as_deref() {
                        Some("RUMBLE_HIT") => {
                            smash::app::sv_animcmd::RUMBLE_HIT(agent.lua_state_agent)
                        }
                        Some("QUAKE") => smash::app::sv_animcmd::QUAKE(agent.lua_state_agent),
                        Some("FT_ATTACK_ABS_CAMERA_QUAKE") => {
                            smash::app::sv_animcmd::FT_ATTACK_ABS_CAMERA_QUAKE(
                                agent.lua_state_agent,
                            )
                        }
                        Some("ControlModule::set_rumble") => {
                            let Some((kind, duration, looped, target)) =
                                control_set_rumble_native_args(&args)
                            else {
                                crate::slight::diag::note(
                                    "rejected direct expression injection with invalid native args",
                                );
                                continue;
                            };
                            smash::app::lua_bind::ControlModule::set_rumble(
                                boma, kind, duration, looped, target,
                            );
                        }
                        _ => {
                            crate::slight::diag::note(
                                "rejected expression injection with an unknown command",
                            );
                            continue;
                        }
                    },
                    CAT_GRAB => smash::app::sv_animcmd::CATCH(agent.lua_state_agent),
                    CAT_SEARCH => smash::app::sv_animcmd::SEARCH(agent.lua_state_agent),
                    CAT_ATTACK_FP => smash::app::sv_animcmd::ATTACK_FP(agent.lua_state_agent),
                    CAT_HURT => match inj.command.as_deref() {
                        Some("HIT_NODE") => smash::app::sv_animcmd::HIT_NODE(agent.lua_state_agent),
                        Some("HIT_NO") => smash::app::sv_animcmd::HIT_NO(agent.lua_state_agent),
                        Some("WHOLE_HIT") => {
                            smash::app::sv_animcmd::WHOLE_HIT(agent.lua_state_agent)
                        }
                        Some("COL_PRI") => smash::app::sv_animcmd::COL_PRI(agent.lua_state_agent),
                        Some("DAMAGE_NO_REACTION") => {
                            smash::app::sv_module_access::damage(agent.lua_state_agent)
                        }
                        _ => {
                            crate::slight::diag::note(
                                "rejected hurtbox injection with an unknown command",
                            );
                            continue;
                        }
                    },
                    CAT_WIND => match inj.command.as_deref() {
                        Some("AREA_WIND_2ND_RAD") => {
                            smash::app::sv_animcmd::AREA_WIND_2ND_RAD(agent.lua_state_agent)
                        }
                        Some("AREA_WIND_2ND_RAD_arg9") => {
                            smash::app::sv_animcmd::AREA_WIND_2ND_RAD_arg9(agent.lua_state_agent)
                        }
                        Some("AREA_WIND_2ND") => {
                            smash::app::sv_animcmd::AREA_WIND_2ND(agent.lua_state_agent)
                        }
                        _ => smash::app::sv_animcmd::AREA_WIND_2ND_arg10(agent.lua_state_agent),
                    },
                    // The ATTACK family shares one argument layout, so the stack built
                    // above is right for either member and only the function to fire
                    // differs. An editor that names none of them means plain ATTACK —
                    // which is what this arm did before the family was modelled.
                    _ => match inj.command.as_deref() {
                        Some("ATTACK_IGNORE_THROW") => {
                            smash::app::sv_animcmd::ATTACK_IGNORE_THROW(agent.lua_state_agent)
                        }
                        _ => smash::app::sv_animcmd::ATTACK(agent.lua_state_agent),
                    },
                }
            }
            agent.clear_lua_stack();
            FIRED.lock().insert(key, (motion, frame));
            crate::slight::diag::note(format!(
                "injected collision cat {category} (motion {motion:#x} frame {frame:.1})"
            ));
        }
        // Reset the latch when the motion restarts (frame went backwards) or changed.
        if !due {
            let mut fired = FIRED.lock();
            if let Some((m, f)) = fired.get(&key).copied() {
                if m != motion || frame < f {
                    fired.remove(&key);
                }
            }
        }
    }
}

pub fn install() {
    capture_archive::init();
    skyline::install_hooks!(
        hook_attack,
        hook_attack_ignore_throw,
        hook_attack_fp,
        hook_catch,
        hook_search,
        hook_wind_2nd,
        hook_wind_2nd_rad,
        hook_wind_2nd_rad_arg9,
        hook_wind_2nd_arg10,
        hook_erase_wind,
        hook_attack_clear_all,
        hook_attack_abs,
        hook_hit_node,
        hook_hit_no,
        hook_whole_hit,
        hook_damage_no_reaction,
        hook_col_pri,
        hook_hit_reset_all,
        hook_atk_power,
        hook_atk_set_shield_setoff_mul,
        hook_rumble_hit,
        hook_quake,
        hook_ft_attack_abs_camera_quake,
        hook_control_set_rumble,
        hook_reverse_lr,
        hook_set_speed_ex,
        hook_set_speed,
        hook_add_speed_no_limit,
        hook_correct,
        hook_ft_catch_stop,
        hook_ft_start_adjust_motion_frame,
        hook_clr_speed,
        hook_set_air,
        hook_kinetic_clear_speed_all,
        hook_kinetic_set_consider_ground_friction,
        hook_change_kinetic,
        hook_kinetic_suspend_energy,
        hook_kinetic_resume_energy,
        hook_kinetic_enable_energy,
        hook_kinetic_unable_energy,
        hook_kinetic_add_speed,
        hook_work_module_on_flag,
        hook_work_module_off_flag,
        hook_work_module_enable_transition_term,
        hook_work_module_unable_transition_term,
        hook_work_module_enable_transition_term_group,
        hook_work_module_unable_transition_term_group_ex,
        hook_work_module_inc_int,
        hook_work_module_set_int,
        hook_work_module_set_float,
        hook_work_module_set_int64
    );
    // Installed separately rather than folded into the list above: `install_hooks!` takes a
    // fixed list, and the sound family is twelve more names for a surface that has nothing to
    // do with collisions. Its own banner also makes "did the sound hooks load" answerable from
    // the log without counting names.
    sound_hooks::install();
    rate_hooks::install();
    // **After the hooks, not before.** This used to be the first statement in the function, which
    // was harmless while it only reported a stage — and became a lie the moment it started
    // reporting `sound_hooks`, because that flag is set by the call two lines up. It read `false`
    // on every boot no matter what, which is the worst possible answer: a flag that exists to
    // distinguish "did not install" from "installed but silent", stuck on the first.
    write_capture_diag("installed");
    skyline::println!(
        "[SLight] ACMD ATTACK/CATCH/SEARCH/WIND/CLEAR/HURT/ATKMOD hooks installed (capture + rules)"
    );
}

#[cfg(test)]
mod injection_guard_tests {
    use super::{is_injecting, InjectGuard};

    #[test]
    fn nested_guards_keep_replacement_context_active() {
        assert!(!is_injecting());
        let outer = InjectGuard::new();
        assert!(is_injecting());
        let inner = InjectGuard::new();
        assert!(is_injecting());
        drop(inner);
        assert!(is_injecting());
        drop(outer);
        assert!(!is_injecting());
    }
}
