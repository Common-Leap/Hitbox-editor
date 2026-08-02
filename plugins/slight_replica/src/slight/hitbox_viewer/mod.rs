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
/// Max args to probe for the AREA_WIND family (all floats, variable arity 8..10).
const WIND_ARGC_MAX: i32 = 12;

/// Collision family a capture / rule belongs to. Kept in sync with the editor
/// (`Hitbox.category`): 0 = attack, 1 = grab (CATCH), 2 = wind (AREA_WIND).
pub const CAT_ATTACK: u8 = 0;
pub const CAT_GRAB: u8 = 1;
pub const CAT_WIND: u8 = 2;

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
    pub args: Vec<LuaArg>,
    /// Which PLAYBACK of the motion produced this line. See [`next_run`].
    pub run: u32,
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

/// Everything captured this session, one claimed run per (kind, motion).
static CAPTURE_LOG: LazyLock<Mutex<Vec<CaptureLine>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Dedupe keys within each claimed run.
static CAPTURE_SEEN: LazyLock<Mutex<HashMap<u32, HashSet<u64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Not-yet-sent lines. Holds clones so reconnect/resend and network draining never borrow the
/// immutable session log.
static CAPTURE_PENDING: LazyLock<Mutex<Vec<CaptureLine>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// Completed motions not yet sent (drained strictly AFTER the lines they terminate).
static END_PENDING: LazyLock<Mutex<Vec<CaptureEnd>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// One immutable capture owner per fighter-kind + motion. The claim exists as soon as the first
/// line lands, so another battle object cannot interleave a second run while the move is still
/// executing. It remains after completion so later activity cannot supersede the snapshot.
#[derive(Clone, Copy)]
struct CaptureClaim {
    boid: u32,
    run: u32,
}

static CAPTURE_CLAIMS: LazyLock<Mutex<HashMap<(i32, u64), CaptureClaim>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
static MOTION_WATCH: LazyLock<Mutex<HashMap<u32, MotionWatch>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
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
static CAPTURE_DRAINED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_MOTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_KIND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CAPTURE_LAST_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_capture_diag(stage: &str) {
    use std::sync::atomic::Ordering;

    let pending = CAPTURE_PENDING.lock().len();
    let _ = std::fs::write(
        "sd:/effect_viewer_capture.txt",
        format!(
            "stage={stage}\nrecorded={}\neffect_recorded={}\ndrained={}\npending={pending}\nlast_kind={}\nlast_motion={:#x}\nlast_frame={}\n",
            CAPTURE_RECORDED.load(Ordering::Relaxed),
            EFFECT_CAPTURE_RECORDED.load(Ordering::Relaxed),
            CAPTURE_DRAINED.load(Ordering::Relaxed),
            CAPTURE_LAST_KIND.load(Ordering::Relaxed) as i64,
            CAPTURE_LAST_MOTION.load(Ordering::Relaxed),
            f32::from_bits(CAPTURE_LAST_FRAME.load(Ordering::Relaxed) as u32),
        ),
    );
}

/// True while an inject_tick replays a captured line through the (hooked) sv_animcmd
/// functions. The replay re-enters the capture hooks, so without this gate every live
/// retime/inject would be recorded as if the SCRIPT contained it — the editor would then
/// treat the user's own edit as a pristine spawn on the next capture load.
static INJECTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII guard: capture recording is suspended while this is alive.
pub struct InjectGuard;

impl InjectGuard {
    pub fn new() -> Self {
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        InjectGuard
    }
}

impl Drop for InjectGuard {
    fn drop(&mut self) {
        INJECTING.store(false, std::sync::atomic::Ordering::Relaxed);
    }
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
    record_for_boma(boma, func, args);
}

/// As [`record`], for hooks that receive a module accessor rather than a lua state —
/// `AttackModule::clear_all` is a lua_bind call, not an sv_animcmd script primitive.
pub unsafe fn record_for_boma(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    func: &'static str,
    args: &[LuaArg],
) {
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
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

    // Resolve the run BEFORE the dedupe key — the key is scoped to it, and a new playback
    // must not be silently folded into the previous one's key set.
    let Some(run) = mark_capture_motion((*boma).battle_object_id, motion, kind, frame) else {
        return;
    };

    // Arm the clear-all gate: a clear is only worth capturing once something is out to clear.
    if is_collision_func(func) {
        note_collision((*boma).battle_object_id, true);
    }

    let mut key = fnv(0xcbf29ce484222325, motion);
    key = fnv(key, kind as u64);
    key = fnv(key, frame.to_bits() as u64);
    for b in func.bytes() {
        key = fnv(key, b as u64);
    }
    for a in args {
        key = fnv(key, a.dedupe_bits());
    }
    if !CAPTURE_SEEN.lock().entry(run).or_default().insert(key) {
        return;
    }
    let line = CaptureLine {
        kind,
        motion,
        frame,
        func,
        args: args.to_vec(),
        run,
    };
    CAPTURE_PENDING.lock().push(line.clone());
    CAPTURE_LOG.lock().push(line);
    CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if func.contains("EFFECT") {
        EFFECT_CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    CAPTURE_LAST_KIND.store(kind as u64, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_MOTION.store(motion, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_FRAME.store(frame.to_bits() as u64, std::sync::atomic::Ordering::Relaxed);
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
    CAPTURE_LOG.lock().clear();
    CAPTURE_SEEN.lock().clear();
    CAPTURE_PENDING.lock().clear();
    END_PENDING.lock().clear();
    CAPTURE_CLAIMS.lock().clear();
    MOTION_WATCH.lock().clear();
    MOTION_WATCH_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::rust_extender::debuggable_server::notify_acmd_capture_cleared();
    crate::slight::diag::note("live ACMD captures cleared");
}

/// Drain up to `max` unsent capture lines (game thread, per-frame flush).
pub fn take_pending(max: usize) -> Vec<CaptureLine> {
    clear_captures_if_requested();
    let mut pending = CAPTURE_PENDING.lock();
    if pending.is_empty() {
        return Vec::new();
    }
    let n = pending.len().min(max);
    let lines: Vec<CaptureLine> = pending.drain(..n).collect();
    drop(pending);
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
    if !CAPTURE_PENDING.lock().is_empty() {
        return Vec::new();
    }
    let mut q = END_PENDING.lock();
    if q.is_empty() {
        return Vec::new();
    }
    let n = q.len().min(max);
    q.drain(..n).collect()
}

/// Re-queue the whole capture log (new editor client connected).
pub fn requeue_all() {
    let log = CAPTURE_LOG.lock().clone();
    *CAPTURE_PENDING.lock() = log;
}

/// Claim the first playback of `(kind, motion)` for one battle object and return its run id.
/// Existing claims are immutable: later performances and other instances are ignored.
///
/// This can be the first thing to observe a motion change (an ACMD script can run before the
/// line callback on the frame a move is cancelled into another), so it also closes out the
/// motion it replaces — otherwise a cancelled move's end marker would be dropped, and its
/// lines would be filed under the incoming motion's run.
fn mark_capture_motion(boid: u32, motion: u64, kind: i32, frame: f32) -> Option<u32> {
    let mut finished: Option<(i32, u64, u32)> = None;
    {
        let mut watch = MOTION_WATCH.lock();
        // Battle object ids are reused across matches; keep the map from growing unbounded.
        if watch.len() > 128 {
            watch.clear();
        }
        let continuing = watch
            .get(&boid)
            .is_some_and(|w| w.motion == motion && !w.ended && frame + 0.5 >= w.frame);
        if continuing {
            let w = watch.get_mut(&boid).expect("continuing implies present");
            w.captured = true;
            w.kind = kind;
            w.frame = frame;
            return Some(w.run);
        }
        if let Some(w) = watch.remove(&boid) {
            if w.captured && !w.ended {
                finished = Some((w.kind, w.motion, w.run));
            }
        }
        MOTION_WATCH_ACTIVE.store(!watch.is_empty(), std::sync::atomic::Ordering::Relaxed);
    }
    if let Some((kind, motion, run)) = finished {
        finish_capture(boid, kind, motion, run);
    }

    let run = {
        let mut claims = CAPTURE_CLAIMS.lock();
        if claims.contains_key(&(kind, motion)) {
            return None;
        }
        let run = next_run();
        claims.insert((kind, motion), CaptureClaim { boid, run });
        run
    };
    let mut watch = MOTION_WATCH.lock();
    watch.insert(
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
    Some(run)
}

/// Does this captured function put a collision out? Kept in step with the editor, which
/// buckets captures the same way (`ATTACK*` / `CATCH` / `AREA_WIND*`).
fn is_collision_func(func: &str) -> bool {
    func.starts_with("ATTACK") && func != "ATTACK_CLEAR_ALL"
        || func == "CATCH"
        || func.starts_with("AREA_WIND") && func != "AREA_WIND_ERASE"
}

/// Note that a collision came out (or was cleared) on `boid`, and report whether a clear is
/// worth recording — i.e. whether anything was actually open to clear.
fn note_collision(boid: u32, open: bool) -> bool {
    let mut watch = MOTION_WATCH.lock();
    let Some(w) = watch.get_mut(&boid) else {
        return false;
    };
    let was_open = w.open_collisions;
    w.open_collisions = open;
    was_open
}

fn push_end(kind: i32, motion: u64, run: u32) {
    let mut q = END_PENDING.lock();
    // Collapse duplicate completion paths for the same claimed run (end frame and motion switch
    // can be observed on adjacent callbacks).
    if q.iter().any(|e| e.run == run) {
        return;
    }
    q.push(CaptureEnd { kind, motion, run });
}

fn finish_capture(boid: u32, kind: i32, motion: u64, run: u32) {
    let owned = CAPTURE_CLAIMS
        .lock()
        .get(&(kind, motion))
        .is_some_and(|claim| claim.boid == boid && claim.run == run);
    if owned {
        push_end(kind, motion, run);
    }
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

    let mut finished: Option<(i32, u64, u32)> = None;
    {
        let mut watch = MOTION_WATCH.lock();
        // Resolve the entry BEFORE querying MotionModule: an object nobody captured from
        // must not pay for motion_kind/frame/end_frame.
        if !watch.contains_key(&boid) {
            return;
        }
        let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let frame = smash::app::lua_bind::MotionModule::frame(boma);
        let end = smash::app::lua_bind::MotionModule::end_frame(boma);
        let Some(w) = watch.get_mut(&boid) else {
            return;
        };
        if w.motion != motion || frame + 0.5 < w.frame {
            // Switched away (or looped back to frame 0): the previous playback is over.
            if w.captured && !w.ended {
                finished = Some((w.kind, w.motion, w.run));
            }
            watch.remove(&boid);
        } else {
            w.frame = frame;
            // `>=` (not `end - rate`) so the marker is never EARLY: a hitbox on the very
            // last frame still gets recorded before the editor is told to adopt.
            if w.captured && !w.ended && end > 0.0 && frame >= end {
                w.ended = true;
                finished = Some((w.kind, w.motion, w.run));
            }
        }
        MOTION_WATCH_ACTIVE.store(!watch.is_empty(), std::sync::atomic::Ordering::Relaxed);
    }
    if let Some((kind, motion, run)) = finished {
        finish_capture(boid, kind, motion, run);
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
    /// ATTACK id this rule targets (None = any id in the motion) — unused for inject.
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
    crate::slight::diag::note(format!("hitbox_rules set: {n} rule(s)"));
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

/// Inject rules for a motion, tagged with the collision family to fire through.
fn injection_fingerprint(motion: u64, category: u8, injection: &InjectRule) -> u64 {
    let mut hash = fnv(0xcbf2_9ce4_8422_2325, motion);
    hash = fnv(hash, category as u64);
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
                injection_fingerprint(rule.motion, rule.category, &injection),
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
    if !boma.is_null() && note_collision((*boma).battle_object_id, false) {
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
    FIRED.lock().retain(|(fired_boid, fingerprint), (fired_motion, _)| {
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
            for a in &args {
                let mut v = a.to_l2c();
                agent.push_lua_stack(&mut v);
            }
            // Fire through the collision family the rule targets. The guard keeps the
            // replay out of the pristine capture (these functions are our own hooks).
            {
                let _g = InjectGuard::new();
                match category {
                    CAT_GRAB => smash::app::sv_animcmd::CATCH(agent.lua_state_agent),
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
                    _ => smash::app::sv_animcmd::ATTACK(agent.lua_state_agent),
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
    write_capture_diag("installed");
    skyline::install_hooks!(
        hook_attack,
        hook_attack_ignore_throw,
        hook_catch,
        hook_wind_2nd,
        hook_wind_2nd_rad,
        hook_wind_2nd_rad_arg9,
        hook_wind_2nd_arg10,
        hook_erase_wind,
        hook_attack_clear_all
    );
    skyline::println!("[SLight] ACMD ATTACK/CATCH/WIND/CLEAR hooks installed (capture + rules)");
}
