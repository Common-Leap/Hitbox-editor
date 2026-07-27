//! hitbox_viewer — live hitbox editing + ACMD capture.
//!
//! Mirrors effect_viewer's ACMD approach for the ATTACK family:
//!   * every hooked call is CAPTURED (pristine args + motion + frame) and streamed to the
//!     editor as `AcmdCapture` — the editor's "live ACMD" source replacing the GitHub dump;
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
}

/// Everything unique captured this session (re-sent whole on client resync).
static CAPTURE_LOG: LazyLock<Mutex<Vec<CaptureLine>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Dedupe keys for CAPTURE_LOG entries.
static CAPTURE_SEEN: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Not-yet-sent indices into CAPTURE_LOG.
static CAPTURE_PENDING: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));
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

/// Record one ACMD call (pristine, pre-rewrite args). Dedupes on
/// (kind, motion, func, frame, args) so repeated move playback costs nothing.
pub unsafe fn record(lua_state: u64, func: &'static str, args: &[LuaArg]) {
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let kind = smash::app::utility::get_kind(&mut *boma);

    let mut key = fnv(0xcbf29ce484222325, motion);
    key = fnv(key, kind as u64);
    key = fnv(key, frame.to_bits() as u64);
    for b in func.bytes() {
        key = fnv(key, b as u64);
    }
    for a in args {
        key = fnv(key, a.dedupe_bits());
    }
    if !CAPTURE_SEEN.lock().insert(key) {
        return;
    }
    let line = CaptureLine {
        kind,
        motion,
        frame,
        func,
        args: args.to_vec(),
    };
    let mut log = CAPTURE_LOG.lock();
    let idx = log.len();
    log.push(line);
    CAPTURE_PENDING.lock().push(idx);
    CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if func.contains("EFFECT") {
        EFFECT_CAPTURE_RECORDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    CAPTURE_LAST_KIND.store(kind as u64, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_MOTION.store(motion, std::sync::atomic::Ordering::Relaxed);
    CAPTURE_LAST_FRAME.store(frame.to_bits() as u64, std::sync::atomic::Ordering::Relaxed);
}

/// Drain up to `max` unsent capture lines (game thread, per-frame flush).
pub fn take_pending(max: usize) -> Vec<CaptureLine> {
    let mut pending = CAPTURE_PENDING.lock();
    if pending.is_empty() {
        return Vec::new();
    }
    let n = pending.len().min(max);
    let idxs: Vec<usize> = pending.drain(..n).collect();
    drop(pending);
    let log = CAPTURE_LOG.lock();
    let lines: Vec<_> = idxs
        .into_iter()
        .filter_map(|i| log.get(i).cloned())
        .collect();
    drop(log);
    if !lines.is_empty() {
        CAPTURE_DRAINED.fetch_add(lines.len() as u64, std::sync::atomic::Ordering::Relaxed);
        write_capture_diag("drained");
    }
    lines
}

/// Re-queue the whole capture log (new editor client connected).
pub fn requeue_all() {
    let n = CAPTURE_LOG.lock().len();
    *CAPTURE_PENDING.lock() = (0..n).collect();
}

// ── Rules (live modify / suppress / inject) ──────────────────────────────────

/// Sparse per-slot overrides for an existing ATTACK (arg indices in the header comment).
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct HbOverrides {
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
    pub hitlag: Option<f32>,
    pub sdi: Option<f32>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct InjectRule {
    /// Motion frame at which to fire (once per motion playback).
    pub frame: f32,
    /// Complete typed ATTACK arg vector (36 slots).
    pub args: Vec<LuaArg>,
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
    HAVE_RULES.store(!rules.is_empty(), std::sync::atomic::Ordering::Relaxed);
    let n = rules.len();
    if let Some(mut guard) = RULES.try_lock() {
        *guard = rules;
    } else {
        *RULES.lock() = rules;
    }
    crate::slight::diag::note(format!("hitbox_rules set: {n} rule(s)"));
}

fn any_rules() -> bool {
    HAVE_RULES.load(std::sync::atomic::Ordering::Relaxed)
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
fn injections_for(motion: u64) -> Vec<(usize, u8, InjectRule)> {
    let rules = RULES.lock();
    rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.motion == motion && r.inject.is_some())
        .map(|(i, r)| (i, r.category, r.inject.clone().unwrap()))
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
    set_num(3, ov.damage, &mut vals);
    set_int(4, ov.angle, &mut vals);
    set_int(5, ov.kbg, &mut vals);
    set_int(6, ov.fkb, &mut vals);
    set_int(7, ov.bkb, &mut vals);
    set_num(8, ov.size, &mut vals);
    set_num(9, ov.x, &mut vals);
    set_num(10, ov.y, &mut vals);
    set_num(11, ov.z, &mut vals);
    set_num(12, ov.x2, &mut vals);
    set_num(13, ov.y2, &mut vals);
    set_num(14, ov.z2, &mut vals);
    set_num(15, ov.hitlag, &mut vals);
    set_num(16, ov.sdi, &mut vals);

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

// ── CATCH (grabbox) hook ─────────────────────────────────────────────────────
// Arg layout (0-based): 0 id, 1 bone(h), 2 size, 3 x, 4 y, 5 z, 6 x2, 7 y2, 8 z2
// (nil = sphere), 9 status, 10 situation. Only geometry (size + offsets) is rewritten.

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
    set(2, ov.size, &mut vals);
    set(3, ov.x, &mut vals);
    set(4, ov.y, &mut vals);
    set(5, ov.z, &mut vals);
    set(6, ov.x2, &mut vals);
    set(7, ov.y2, &mut vals);
    set(8, ov.z2, &mut vals);

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
// All args are floats; arity varies (8..10). Semantics are undocumented in the
// bindings, so we capture the raw args (editor renders best-effort + shows them)
// and support suppression. Fine-grained rewrite is deferred until the layout is
// confirmed from real captures.

macro_rules! wind_hook {
    ($hook_name:ident, $target:path, $func:literal) => {
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
                        // Wind has no id — match by motion+frame only.
                        if let Some((suppress, _ov)) = action_for(CAT_WIND, motion, u64::MAX, frame)
                        {
                            if suppress {
                                return;
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
    "AREA_WIND_2ND"
);
wind_hook!(
    hook_wind_2nd_rad,
    smash::app::sv_animcmd::AREA_WIND_2ND_RAD,
    "AREA_WIND_2ND_RAD"
);
wind_hook!(
    hook_wind_2nd_rad_arg9,
    smash::app::sv_animcmd::AREA_WIND_2ND_RAD_arg9,
    "AREA_WIND_2ND_RAD_arg9"
);
wind_hook!(
    hook_wind_2nd_arg10,
    smash::app::sv_animcmd::AREA_WIND_2ND_arg10,
    "AREA_WIND_2ND_arg10"
);

// ── Injection (per-frame, from the smashline line callback) ──────────────────

/// (boid, rule idx) → motion frame it last fired at. Refires when the motion loops
/// (frame goes backwards) or the motion changes.
static FIRED: LazyLock<Mutex<HashMap<(u32, usize), (u64, f32)>>> =
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
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let injections = injections_for(motion);
    if injections.is_empty() {
        return;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let boid = (*boma).battle_object_id;

    for (idx, category, inj) in injections {
        let key = (boid, idx);
        let due = frame >= inj.frame;
        let already = {
            let fired = FIRED.lock();
            fired
                .get(&key)
                .map(|(m, f)| *m == motion && frame >= *f)
                .unwrap_or(false)
        };
        if due && !already {
            let mut agent = smash::lib::L2CAgent::new(lua_state);
            agent.clear_lua_stack();
            for a in &inj.args {
                let mut v = a.to_l2c();
                agent.push_lua_stack(&mut v);
            }
            // Fire through the collision family the rule targets. The guard keeps the
            // replay out of the pristine capture (these functions are our own hooks).
            {
                let _g = InjectGuard::new();
                match category {
                    CAT_GRAB => smash::app::sv_animcmd::CATCH(agent.lua_state_agent),
                    CAT_WIND => smash::app::sv_animcmd::AREA_WIND_2ND_arg10(agent.lua_state_agent),
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
        hook_wind_2nd_arg10
    );
    skyline::println!("[SLight] ACMD ATTACK/CATCH/WIND hooks installed (capture + rules)");
}
