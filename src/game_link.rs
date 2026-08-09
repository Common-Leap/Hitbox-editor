// Game link: TCP client to the slight_replica plugin (127.0.0.1:7878).
// Speaks the plugin's `<TCP_MESSAGE>{json}</TCP_MESSAGE>` framing (formerly RPM's role):
//  - inbound  `{"header":"Notify","body":"{\"Notify\":{id,name,value_in_json}}"}` = live
//    effect-kind tabs (id = hash40 of the effect name, value = RpmEffectData JSON)
//  - outbound `{"id":<hash>,"newValue":"<sparse JSON>"}` = an edit; only controls the
//    user actually changed are sent and pinned.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const PLUGIN_ADDR: &str = "127.0.0.1:7878";

// ── Wire structs (must match slight_replica effect_data.rs exactly) ──────────

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Point3D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Color {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rainbow {
    pub color: Color,
    pub movement_state: f32,
}

/// One live effect kind as the plugin reports it. `rainbow.color` and `speed` are runtime
/// MULTIPLIERS (the game has no getters for authored color); `pos`/`rot` are in ACMD script
/// offset space for script-spawned effects; `scale` is the spawn size argument.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpmEffectData {
    pub index: u32,
    pub effect_name: String,
    pub bone_name: String,
    pub is_follow: bool,
    pub visible: bool,
    pub scale: f32,
    pub frame: f32,
    pub pos: Point3D,
    pub rot: Point3D,
    pub speed: f32,
    pub rainbow: Rainbow,
}

impl Default for RpmEffectData {
    fn default() -> Self {
        Self {
            index: 0,
            effect_name: "0x0".into(),
            bone_name: "0x0".into(),
            is_follow: false,
            visible: true,
            scale: 1.0,
            frame: 0.0,
            pos: Point3D::default(),
            rot: Point3D::default(),
            speed: 1.0,
            rainbow: Rainbow::default(),
        }
    }
}

/// Wire form of a plugin spawn rule (matches slight_replica spawn_rules::SpawnRule).
/// `motion` + frame window scope the rule to ONE spawn so editing one spawn of an effect
/// doesn't move every spawn; `pos`/`rot`/`scale` are the per-spawn transform override.
#[derive(Clone, Debug, Serialize)]
pub struct SpawnRuleWire {
    pub eff_hash: u64,
    pub suppress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<u64>,
    pub frame_start: Option<f32>,
    pub frame_end: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rot: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Per-spawn playback rate — the live counterpart of the spawn's `LAST_EFFECT_SET_RATE`.
    ///
    /// Sent only when the user changed it, so an untouched spawn keeps whatever its script
    /// asks for. Separate from `scale` because it is not part of the spawn's argument list at
    /// all: the plugin applies it to the handle after the spawn, and rewrites the script's own
    /// rate line if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    /// Per-spawn camera-flat offset — the live counterpart of the spawn's
    /// `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` line. It is a separate modifier because it is
    /// not part of the spawn's three-dimensional position arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_offset: Option<f32>,
    /// Per-spawn tint and opacity — the live counterparts of `LAST_EFFECT_SET_COLOR` and
    /// `LAST_EFFECT_SET_ALPHA`, applied and rewritten exactly the way `rate` is.
    ///
    /// Not to be confused with `color` below, which is a whole-fighter `FLASH` / `BURN_COLOR`
    /// payload keyed on a command name rather than an effect kind. These two are scoped to one
    /// spawn of one effect, like everything else above them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 3]>,
    /// Per-spawn particle tint — the live counterpart of `LAST_PARTICLE_SET_COLOR`. It is
    /// separate from `tint` because the game primitive targets the last particle, not the last
    /// effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub particle_tint: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    /// Per-spawn values for the native dynamic-arity `LAST_EFFECT_SET_SCALE_W` modifier. The
    /// vector preserves the authored one-to-three-value Lua stack shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_w: Option<Vec<f32>>,
    /// Live values for a colour command. For such a rule `eff_hash` is hash40 of the
    /// lowercased command name — `burn_color`, not an effect kind — because these macros name
    /// no effect at all; see `SpawnRule::color` in the plugin for why that field is reused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 4]>,
    /// Frames a `_FRM` / `_FRAME` command interpolates over. Sent apart from `color` so
    /// retiming a ramp does not have to restate its colour, and vice versa.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<f32>,
    /// Live retime: re-fire a captured spawn at a new frame (paired with a suppress rule at
    /// the pristine frame). Omitted for plain transform/suppress rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<SpawnInjectWire>,
}

/// Wire form of plugin `spawn_rules::SpawnInject` — a captured EFFECT spawn to replay.
#[derive(Clone, Debug, Serialize)]
pub struct SpawnInjectWire {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArgWire>,
}

/// One live effect point-control rule. Matching includes the captured argument vector so two
/// `ENABLE_AREA` calls on one frame cannot suppress each other accidentally.
#[derive(Clone, Debug, Serialize)]
pub struct EffectControlRuleWire {
    pub motion: Option<u64>,
    pub func: String,
    pub args: Vec<LuaArgWire>,
    pub suppress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_start: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_end: Option<f32>,
    /// Replacement WorkModule slot for `EFFECT_DETACH_KIND_WORK` injection. The desktop sends this
    /// only for numeric values or symbolic IDs covered by its measured constant table; the
    /// primitive itself receives the resolved effect handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_slot: Option<i32>,
    /// Re-fire the captured point at a new motion frame after suppressing its pristine call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<EffectControlInjectWire>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectControlInjectWire {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArgWire>,
}

/// Wire form of plugin `spawn_rules::EffectAlias` — live transplant kind substitution:
/// a copy/replaced entry that doesn't exist in the running game spawns as its donor.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectAliasWire {
    /// Requested kind (hash40 of the copy / replaced entry name, lowercase).
    pub from: u64,
    /// Kind that exists in the loaded eff resources (the donor).
    pub to: u64,
    /// Costume slots (c00…) the alias applies to; empty = all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<u8>,
}

/// Wire form of the plugin's `effect_reload::DonorSpec` (field names must match).
#[derive(Clone, PartialEq, serde::Serialize)]
pub struct DonorEffWire {
    /// Target fighter's eff arc path (lowercase), e.g. "effect/fighter/kirby/ef_kirby.eff".
    pub target: String,
    /// Donor eff arc paths to co-load whenever the target's effects load.
    pub donors: Vec<String>,
}

/// A stripped donor eff (only the referenced effects + their resources), base64-encoded,
/// that the plugin injects as resident data for a live cross-character transplant.
#[derive(Clone, PartialEq, serde::Serialize)]
pub struct DonorBytesWire {
    /// Donor eff arc path (lowercase), e.g. "effect/assist/alucard/ef_alucard.eff".
    pub path: String,
    /// base64(stripped ef bytes). Empty when [`Self::file`] carries the payload instead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub b64: String,
    /// sd:-relative file holding the payload, which the plugin reads directly.
    ///
    /// The emulator's sdmc is a directory on this machine, so multi-MB payloads go to disk
    /// rather than through base64 in a JSON frame over a socket read in 8 KB chunks. `b64`
    /// remains the fallback for when that directory cannot be found.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
}

// ── Live ACMD capture + hitbox rules (wire forms match slight_replica hitbox_viewer) ──

/// One typed lua argument (plugin `LuaArg`): losslessly round-trips capture → edit → inject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum LuaArgWire {
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

impl LuaArgWire {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            LuaArgWire::Num(n) => Some(*n),
            LuaArgWire::Int(i) => Some(*i as f32),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            LuaArgWire::Int(i) => Some(*i),
            LuaArgWire::Num(n) => Some(*n as i64),
            LuaArgWire::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_hash(&self) -> Option<u64> {
        match self {
            LuaArgWire::Hash(h) => Some(*h),
            LuaArgWire::Int(i) => Some(*i as u64),
            _ => None,
        }
    }

    /// Render this argument as Rust source, for exports that replay a captured call.
    ///
    /// `None` for `Nil`: lua's nil has no single Rust spelling, and a spawn whose tail
    /// contains one has to fall back to a known-good macro rather than emit a guess.
    ///
    /// Floats go through the emitter's own renderer, not a fixed decimal count: a captured
    /// tail is replayed verbatim, so rounding one here changes the move with nothing left to
    /// compare against — the export verifier sees the already-rounded string on both sides.
    pub fn to_source_arg(&self) -> Option<String> {
        Some(match self {
            LuaArgWire::Hash(h) => format!("Hash40::new_raw({h:#x})"),
            LuaArgWire::Num(n) => crate::acmd::num(*n),
            LuaArgWire::Int(i) => i.to_string(),
            LuaArgWire::Bool(b) => b.to_string(),
            LuaArgWire::Nil => return None,
        })
    }
}

/// Stable rule key for one captured expression call. It includes the primitive name and every
/// pristine Lua argument, so two equal camera calls on the same frame do not share a live rule.
/// The plugin mirrors this small hash in its expression hooks.
pub fn expression_key(func: &str, args: &[LuaArgWire]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in func.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    for arg in args {
        let value = match arg {
            LuaArgWire::Hash(h) => 0x1000_0000_0000_0000 ^ h,
            LuaArgWire::Num(n) => 0x2000_0000_0000_0000 ^ n.to_bits() as u64,
            LuaArgWire::Int(i) => 0x3000_0000_0000_0000 ^ *i as u64,
            LuaArgWire::Bool(b) => 0x4000_0000_0000_0000 ^ *b as u64,
            LuaArgWire::Nil => 0x5000_0000_0000_0000,
        };
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    hash
}

/// Stable key for a point call whose payload is a fixed list of numeric `ToF32` arguments.
///
/// The plugin derives the same key from the Lua stack before applying an override. Keeping the
/// function name in the hash prevents two numeric point families from sharing a rule by accident.
pub fn numeric_point_key(func: &str, args: &[f32]) -> u64 {
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

/// Stable key for a point call whose payload is a fixed list of exact integer arguments.
///
/// This is deliberately separate from [`numeric_point_key`]: a WorkModule `set_int64` value
/// commonly carries a hash40 and must not be narrowed through `f32` before the desktop and
/// plugin derive the same rule identity.
pub fn integer_point_key(func: &str, args: &[i64]) -> u64 {
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

/// One captured ACMD call, as streamed by the plugin (`AcmdCapture`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CaptureLine {
    /// Fighter kind of the performing agent.
    pub kind: i32,
    /// hash40 of the motion name (e.g. "attack_air_n").
    pub motion: u64,
    /// Motion frame at call time.
    pub frame: f32,
    /// sv_animcmd function name ("ATTACK", "EFFECT_FOLLOW", …).
    pub func: String,
    /// The primitive hook's source ACMD category is not available here. In particular, direct
    /// MotionModule bindings can be called by either `game_` or `expression_` scripts.
    pub args: Vec<LuaArgWire>,
    /// Which single playback of the motion produced this line. Current plugins lock one run per
    /// fighter-kind + motion until captures are explicitly cleared. Defaults to 0 for older
    /// plugins that predate run ownership.
    #[serde(default)]
    pub run: u32,
}

/// Rule key for `COL_PRI`, which is per fighter rather than per target.
///
/// The hurtbox category matches a rule on motion + key + frame, and this call has no target to
/// build a key from. `u64::MAX` is safe as a stand-in because no bone hash and no group number
/// reaches it. **Must equal the plugin's value** — see `hitbox_viewer::HURT_KEY_COL_PRI`.
pub const HURT_KEY_COL_PRI: u64 = u64::MAX;

/// Rule key for `WHOLE_HIT`, the other targetless member of the hurtbox category.
///
/// Deliberately *not* [`HURT_KEY_COL_PRI`]. Sharing one sentinel across two targetless macros
/// would let a `COL_PRI` rule fire on a `WHOLE_HIT` in the same frame window and write a
/// priority into a status slot — the cross-family corruption this codebase keeps rediscovering,
/// arrived at from the other direction. **Must equal the plugin's value.**
pub const HURT_KEY_WHOLE: u64 = u64::MAX - 1;

/// Rule key for the targetless `SET_AIR` kinetic point.
///
/// Kept separate from the hurtbox sentinels so a structural kinetic rule cannot match a
/// `WHOLE_HIT` or `COL_PRI` call in an older/current plugin. **Must equal the plugin's value.**
pub const KINETIC_KEY_SET_AIR: u64 = u64::MAX - 2;

/// Rule key for the targetless direct `KineticModule::clear_speed_all` kinetic point.
pub const KINETIC_KEY_CLEAR_SPEED_ALL: u64 = u64::MAX - 3;

/// Wire category for `ATTACK_ABS`. **Must equal the plugin's `CAT_ABS`.**
///
/// Deliberately *not* [`crate::data::CAT_ABS`], which is `3`. These are two different numbering
/// spaces that happen to agree for attack, grab and wind: the editor's numbers only cover things
/// that are [`crate::data::Hitbox`]es, while the wire's also carry hurtbox state, which took `3`
/// first. They were assumed identical, so every live `ATTACK_ABS` edit went out as category `3`
/// and reached the plugin's hurtbox hook instead — B1's live surface has never worked.
pub const CAT_ABS: u8 = 4;

/// Wire category for `SEARCH`. **Must equal the plugin's value.**
pub const CAT_SEARCH: u8 = 7;

/// Wire category for `ATTACK_FP`. **Must equal the plugin's `CAT_ATTACK_FP`.**
///
/// The value is kept outside the display-category range because the plugin's wire categories
/// already reserve 3 for hurtbox state and 4 for `ATTACK_ABS`.
pub const CAT_ATTACK_FP: u8 = 12;

/// Translate a [`crate::data::Hitbox`]'s display category into the one the plugin matches on.
///
/// Every place that puts a collision on the wire goes through this. Passing the display number
/// straight out is what aimed throw-damage rules at the hurtbox hook, and the two spaces diverge
/// further with each family added — so the conversion is explicit rather than an identity that
/// happens to be right for the first three.
pub fn wire_category(display: u8) -> u8 {
    match display {
        crate::data::CAT_ABS => CAT_ABS,
        crate::data::CAT_SEARCH => CAT_SEARCH,
        crate::data::CAT_ATTACK_FP => CAT_ATTACK_FP,
        // Attack, grab and wind agree in both spaces.
        other => other,
    }
}

/// Wire category for `ATK_POWER`. **Must equal the plugin's value.**
///
/// The two post-hoc modifiers get a category each rather than sharing one keyed by hitbox id.
/// They can legally name the same id in the same frame window — kirby/Attack100Sub tunes id 0
/// and a move may retune the same box two ways — so one shared category would let an
/// `ATK_POWER` rule fire on an `ATK_SET_SHIELD_SETOFF_MUL` call and write damage into a shield
/// multiplier. Separating them makes that unrepresentable instead of merely unlikely, which is
/// the lesson `HURT_KEY_WHOLE` records from the other direction.
pub const CAT_ATK_POWER: u8 = 5;

/// Wire category for `ATK_SET_SHIELD_SETOFF_MUL`. **Must equal the plugin's value.**
pub const CAT_ATK_SETOFF_MUL: u8 = 6;

/// Wire category for the whole `PLAY_SE` family. **Must equal the plugin's value.**
///
/// One category for all twelve members, which reads as inconsistent beside the two above and
/// is a different situation. There, slot 1 meant a different thing in each member, so a
/// misapplied rule wrote damage into a shield multiplier — a wrong value in a real field.
/// Here every member declares a `Hash40` in slot 0 and nothing else is ever written, so the
/// worst a misapplied rule can do is put a sound where a sound goes. The macro name travels on
/// [`HitboxRuleWire::func`] to close even that.
pub const CAT_SOUND: u8 = 8;

/// Wire category for `FT_MOTION_RATE`. **Must equal the plugin's value.**
///
/// One category covers all three rate macros because `smash-script` compiles
/// `FT_MOTION_RATE_RANGE` and `FT_DESIRED_RATE` down to the same `sv_animcmd::FT_MOTION_RATE`,
/// so the plugin cannot tell them apart and does not try. The editor only models the plain form;
/// a rule keys on the motion and frame alone, because a rate call has no id.
pub const CAT_MOTION_RATE: u8 = 9;

/// Wire category for the measured `expression_` camera/rumble primitives. The function name
/// travels on [`HitboxRuleWire::func`] because the three members have different argument shapes.
pub const CAT_EXPRESSION: u8 = 10;

/// Wire category for the argument-less `REVERSE_LR` facing-direction point.
pub const CAT_REVERSE_LR: u8 = 11;

/// Wire category for the verified three-argument `SET_SPEED_EX` velocity point.
pub const CAT_SPEED_EX: u8 = 13;

/// Wire category for the verified `(x, y)` `SET_SPEED` velocity point.
pub const CAT_SPEED: u8 = 16;

/// Wire category for the verified `ADD_SPEED_NO_LIMIT` x/y velocity point.
pub const CAT_ADD_SPEED_NO_LIMIT: u8 = 14;

/// Wire category for the verified `CORRECT` ground-correction point.
pub const CAT_CORRECT: u8 = 15;

/// Wire category for the measured two-argument `FT_CATCH_STOP` point.
pub const CAT_FT_CATCH_STOP: u8 = 17;

/// Wire category for the measured one-argument motion-frame adjustment point.
pub const CAT_FT_START_ADJUST_MOTION_FRAME: u8 = 18;

/// Wire category for the measured `CLR_SPEED` kinetic point.
pub const CAT_CLR_SPEED: u8 = 19;

/// Wire category for the measured argument-less `SET_AIR` kinetic point.
pub const CAT_SET_AIR: u8 = 20;

/// Wire category for the direct `KineticModule::change_kinetic` kinetic point.
pub const CAT_CHANGE_KINETIC: u8 = 21;

/// Wire category for the direct `KineticModule::add_speed` x/y vector point.
pub const CAT_KINETIC_ADD_SPEED: u8 = 22;

/// Wire category for direct `KineticModule::suspend_energy` points.
pub const CAT_KINETIC_SUSPEND_ENERGY: u8 = 23;

/// Wire category for direct `KineticModule::resume_energy` points.
pub const CAT_KINETIC_RESUME_ENERGY: u8 = 24;

/// Wire category for direct `KineticModule::enable_energy` points.
pub const CAT_KINETIC_ENABLE_ENERGY: u8 = 25;

/// Wire category for direct `KineticModule::unable_energy` points.
pub const CAT_KINETIC_UNABLE_ENERGY: u8 = 26;

/// Wire category for direct `KineticModule::clear_speed_all` points.
pub const CAT_KINETIC_CLEAR_SPEED_ALL: u8 = 27;

/// Wire category for direct `KineticModule::set_consider_ground_friction` points.
pub const CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION: u8 = 28;

/// Wire category for direct `MotionModule::set_rate` point overrides.
pub const CAT_MOTION_MODULE_SET_RATE: u8 = 29;

/// Wire category for direct `MotionModule::set_helper_calculation` point overrides.
pub const CAT_MOTION_MODULE_SET_HELPER_CALCULATION: u8 = 30;

/// Wire category for direct `MotionModule::set_rate_partial` point overrides.
pub const CAT_MOTION_MODULE_SET_RATE_PARTIAL: u8 = 31;

/// Wire category for direct `WorkModule::on_flag` / `off_flag` point overrides.
pub const CAT_WORK_FLAG: u8 = 32;

/// Wire category for direct WorkModule transition-term and transition-term-group point overrides.
pub const CAT_WORK_TRANSITION_TERM: u8 = 33;

/// Wire category for direct `WorkModule::set_int` / `set_float` / `set_int64` point overrides.
pub const CAT_WORK_MODULE_SET: u8 = 34;

/// Wire category for direct `WorkModule::inc_int` point overrides.
pub const CAT_WORK_MODULE_INC_INT: u8 = 35;

/// The wire category a modifier's rules go out under.
pub fn attack_mod_category(kind: crate::data::AttackModKind) -> u8 {
    match kind {
        crate::data::AttackModKind::Power => CAT_ATK_POWER,
        crate::data::AttackModKind::ShieldSetoffMul => CAT_ATK_SETOFF_MUL,
    }
}

/// Sparse ATTACK-arg overrides (plugin `HbOverrides`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct HbOverridesWire {
    /// Complete AREA_WIND argument vector when a wind payload changes. Wind calls do not share
    /// ATTACK's slot layout, so the plugin swaps this exact typed vector into the original hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_args: Option<Vec<LuaArgWire>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damage: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub angle: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kbg: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fkb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bkb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x2: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y2: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z2: Option<f32>,
    /// Explicit capsule state. `Some(false)` is distinct from an absent x2/y2/z2 override:
    /// it replaces the source call's numeric second endpoint with three Lua nils.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capsule: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitlag: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdi: Option<f32>,
    // ── Attribute slots 17..35 ───────────────────────────────────────────────
    //
    // Sent as the NUMBERS the lua stack wants (the editor holds them as symbolic names);
    // `collision_attr` is a hash40. `None` means "leave the game's own value alone", so a
    // hitbox whose attributes were never resolved to a known constant is not clobbered.
    // Plugins predating these fields ignore them, so an old plugin build still applies the
    // geometry/damage overrides exactly as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setoff: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lr_check: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clang: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_attack: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitbox_attr: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_or_air: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtk: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shield_disable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflectable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absorbable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_attack: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub situation_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_finish_camera: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision_attr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_attr: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attack_region: Option<i64>,
    // ── Hurtbox state (category 3 only) ──────────────────────────────────────
    //
    // Skipped when absent like everything above, which is what keeps a plugin build predating
    // this family working: it deserialises the rule, finds no field it knows, and applies
    // nothing rather than failing the whole message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_status: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_target: Option<LuaArgWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col_pri: Option<i64>,
    // ── Post-hoc hitbox tuning (category 5 only) ─────────────────────────────
    //
    // `ATK_POWER` and `ATK_SET_SHIELD_SETOFF_MUL` share one `(id, value)` layout, so one pair of
    // fields covers both; the rule's own category and key say which call it is aimed at. Skipped
    // when absent like everything above, so a plugin build predating this family ignores them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atk_mod_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atk_mod_value: Option<f32>,
    // ── Sound (category 8 only) ──────────────────────────────────────────────
    //
    // The hashes to play instead, positional into the call's leading `Hash40` slots. A list
    // because `PLAY_STEP_FLIPPABLE` and `PLAY_FLY_VOICE` each name a pair, which is why
    // `SoundCall::sounds` is a list too.
    //
    // The plugin bounds the write by the member's own declared hash-slot count rather than by
    // this vector's length, so a longer list cannot reach `SET_PLAY_INHIVIT`'s trailing
    // duration argument. Sending a shorter one leaves the remaining slots as the script wrote
    // them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_hashes: Option<Vec<u64>>,
    /// Replacement `FT_MOTION_RATE` argument.
    ///
    /// **Below 1.0 plays FASTER**: `game_frames = motion_frames * rate`. The plugin refuses a
    /// value that is not finite and positive rather than trusting this end, because a zero rate
    /// freezes the animation and nothing below the call would run again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_rate: Option<f32>,
    /// Replacement `SET_SPEED_EX` or `ADD_SPEED_NO_LIMIT` x/y velocity components.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_y: Option<f32>,
    /// Replacement numeric `CORRECT` kind. Named source constants remain source-owned when a
    /// live capture cannot prove their numeric value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correct_kind: Option<i64>,
    /// Replacement `FT_CATCH_STOP` numeric `ToF32` arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ft_catch_stop_arg1: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ft_catch_stop_arg2: Option<f32>,
    /// Replacement `FT_START_ADJUST_MOTION_FRAME_arg1` numeric value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ft_start_adjust_motion_frame_value: Option<f32>,
    /// Replacement direct `MotionModule::set_rate` value. This is separate from
    /// `motion_rate`, which belongs to the `FT_MOTION_RATE` ACMD primitive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_module_rate: Option<f32>,
    /// Replacement direct `MotionModule::set_helper_calculation` boolean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_module_helper_calculation: Option<bool>,
    /// Replacement direct `MotionModule::set_rate_partial` rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_module_rate_partial: Option<f32>,
    /// Replacement numeric flag for direct `WorkModule::on_flag` / `off_flag` calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_flag: Option<i64>,
    /// Replacement numeric transition term for direct WorkModule transition-term calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_transition_term: Option<i64>,
    /// Replacement integer value for a direct `WorkModule::set_int` call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_module_set_int_value: Option<i64>,
    /// Replacement exact 64-bit value for a direct `WorkModule::set_int64` call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_module_set_int64_value: Option<i64>,
    /// Replacement WorkModule slot for a direct `WorkModule::inc_int` call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_module_inc_int_slot: Option<i64>,
    /// Replacement float value for a direct `WorkModule::set_float` call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_module_set_float_value: Option<f32>,
    /// Replacement WorkModule slot for either direct value setter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_module_set_slot: Option<i64>,
    /// Replacement numeric kinetic-energy kind for `CLR_SPEED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clr_speed_kinetic_kind: Option<i64>,
    /// Replacement numeric kinetic type for `KineticModule::change_kinetic`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_kinetic_type: Option<i64>,
    /// Replacement numeric energy ID for direct suspend/resume kinetic calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinetic_energy_id: Option<i64>,
    /// Replacement bool for `KineticModule::set_consider_ground_friction`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinetic_ground_friction: Option<bool>,
    /// Replacement resolved reserve attribute for `set_consider_ground_friction`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinetic_ground_friction_energy: Option<i64>,
    /// Complete replacement argument vector for a measured expression primitive. The plugin
    /// preserves the captured Lua types while swapping these values into the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_args: Option<Vec<LuaArgWire>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InjectRuleWire {
    pub frame: f32,
    pub args: Vec<LuaArgWire>,
    /// Exact AREA_WIND family function for wind injection. Attack/grab injections omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// One live hitbox rule (plugin `HitboxRule`); the full list replaces on every send.
fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

/// `frame_start`/`frame_end` scope suppress/override to ONE hit so multi-hit moves (which
/// reuse the same id across frames) stay independent.
#[derive(Clone, Debug, Serialize)]
pub struct HitboxRuleWire {
    pub motion: u64,
    /// Rule family: 0 attack, 1 grab, 2 wind, and 11 `REVERSE_LR`. Omitted when 0 so old
    /// plugins default to the attack family.
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub category: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitbox_id: Option<u64>,
    pub suppress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_start: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_end: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<HbOverridesWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<InjectRuleWire>,
    /// The exact ACMD call this rule is for, when the category alone does not identify it.
    ///
    /// [`CAT_SOUND`] and [`CAT_EXPRESSION`] set it. Sound members share one category and every
    /// one of them carries a `Hash40` in slot 0, so a rule for `PLAY_SE` would otherwise apply
    /// cleanly and silently to a `PLAY_SE_REMAIN` on the same frame naming the same sound.
    /// Expression members have different argument shapes, so their macro name is equally part
    /// of the match. Twelve sound categories plus three expression categories to keep in step
    /// across the wire is a worse trade than one name for each family.
    ///
    /// A plugin predating this field ignores it and matches on the category alone — too broad
    /// rather than silently dead, which is the right way round for a preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub func: Option<String>,
}

// ── Shared live-override store ───────────────────────────────────────────────
//
// ONE per-kind runtime form that both the Effects panel and the Eff Editor game panel read
// and write, flushed to the plugin by a single debounced sender. (They used to keep separate
// forms with separate debounces and stomped each other's sends for the same kind id.)

const OVERRIDE_DEBOUNCE_MS: u128 = 200;

// This store carries ONLY user-set kind-level tweaks (color× / speed×, which apply to every
// spawn of the effect — that is what the user is asking for when they drag those fields).
// It used to carry a second class of entry, "modifiers derived from authored .eff edits",
// which also pushed `scale`. That derivation was removed: authored values are per emitter
// and this wire message is per kind, so it recoloured whole effects when one emitter was
// edited. Authored edits now go through the eff rebuild + hot-reload path (app::
// apply_authored_eff_live), which is per-emitter exact.

#[derive(Clone, Debug)]
pub struct LiveOverride {
    pub form: RpmEffectData,
    dirty_at: Option<Instant>,
    /// The USER set this entry's color×/speed — only these export as LAST_EFFECT_SET_*
    /// tweaks and persist in the project.
    user_tweaked: bool,
}

impl LiveOverride {
    fn new(form: RpmEffectData) -> Self {
        Self {
            form,
            dirty_at: None,
            user_tweaked: false,
        }
    }
}

/// Are these two capture lines the same script line, at the resolution the editor works in?
///
/// Only ever true within one run. This guards against the plugin re-sending its immutable log
/// on reconnect.
///
/// Frames are compared as the ROUNDED integer the editor displays rather than as raw floats.
/// The float is `MotionModule::frame` at call time and differs in the low bits between
/// performances; that only matters for a resend within a run, but comparing what is actually
/// displayed is the honest test either way.
fn same_capture(a: &CaptureLine, b: &CaptureLine) -> bool {
    a.run == b.run
        && a.kind == b.kind
        && a.motion == b.motion
        && a.func == b.func
        && a.frame.max(0.0).round() == b.frame.max(0.0).round()
        && a.args == b.args
}

#[derive(Default)]
pub struct LiveOverrides {
    entries: BTreeMap<u64, LiveOverride>,
}

/// Kind-level color×/speed× multipliers.
///
/// These predate the live carrier. They were once the only way to change how an effect looked
/// at runtime, and they are whole-effect by construction — one multiplier tints every emitter
/// of every spawn, and cannot express a per-key or color1 edit at all. Authored eff edits now
/// do that exactly, so the eff editor no longer offers a second, cruder way to recolour the
/// same effect.
///
/// The store stays because the multipliers still ARRIVE from two places the user did not type
/// them into: a project's saved `live_tweaks` (restored and re-sent on load) and pins already
/// set in the running game (importable, or clearable, from the pin-sync prompt). Both paths
/// go through `set_form`/`restore_tweak`, and `flush_due` sends them.
impl LiveOverrides {
    /// Adopt a form already reported by the game. Importing the game's current pins does not
    /// need to echo the complete form back; doing so could race a new spawn observation.
    pub fn set_form(&mut self, hash: u64, form: RpmEffectData) {
        let e = self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(form.clone()));
        e.form = form;
        e.dirty_at = None;
    }

    /// Send every entry whose debounce has elapsed. Returns how many were sent.
    /// `include_scale` is false: size is a per-spawn value pushed through the spawn rules,
    /// never inferred from this kind-level form. Transform fields are omitted for the same
    /// reason — ACMD position/rotation are per spawn.
    pub fn flush_due(&mut self, link: &GameLink) -> usize {
        let mut sent = 0;
        for (hash, e) in self.entries.iter_mut() {
            if let Some(t) = e.dirty_at {
                if t.elapsed().as_millis() > OVERRIDE_DEBOUNCE_MS {
                    e.dirty_at = None;
                    link.send_modifier_edit(*hash, &e.form, false);
                    sent += 1;
                }
            }
        }
        sent
    }

    /// True while a debounced send is pending (keep repainting so it fires).
    pub fn any_dirty(&self) -> bool {
        self.entries.values().any(|e| e.dirty_at.is_some())
    }

    /// Flag the entry's color×/speed as USER-set (exports + persists as a live tweak).
    pub fn mark_tweak(&mut self, hash: u64) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.dirty_at = Some(Instant::now());
            e.user_tweaked = true;
        }
    }

    /// All user-set tweak entries: (hash, form) — export/persist as LiveTweaks.
    pub fn tweaked(&self) -> Vec<(u64, RpmEffectData)> {
        self.entries
            .iter()
            .filter(|(_, e)| e.user_tweaked)
            .map(|(h, e)| (*h, e.form.clone()))
            .collect()
    }

    /// Revert a user tweak: color/speed back to identity, unflag, and re-send.
    pub fn clear_tweak(&mut self, hash: u64) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.form.rainbow = Rainbow::default();
            e.form.speed = 1.0;
            e.user_tweaked = false;
            e.dirty_at = Some(Instant::now());
        }
    }

    /// Restore a tweak from a loaded project: sets color/speed, flags user_tweaked,
    /// and schedules a send.
    pub fn restore_tweak(&mut self, hash: u64, init: RpmEffectData) {
        let e = self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(init.clone()));
        e.form.rainbow = init.rainbow;
        e.form.speed = init.speed;
        if e.form.effect_name == "0x0" {
            e.form.effect_name = init.effect_name;
        }
        e.user_tweaked = true;
        e.dirty_at = Some(Instant::now());
    }
}

// ── Link state ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Disconnected,
    Connecting,
    Connected,
}

/// Sparse pins-only form the plugin reports alongside merged values (newer plugins) —
/// mirrors slight_replica kinds::Pinned. `Some` fields are active user overrides in-game.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PinsWire {
    pub scale: Option<f32>,
    pub rate: Option<f32>,
    pub pos: Option<Point3D>,
    pub rot: Option<Point3D>,
    pub visible: Option<bool>,
    pub frame: Option<f32>,
    pub color: Option<Color>,
    pub movement_state: Option<f32>,
}

impl PinsWire {
    pub fn any(&self) -> bool {
        self.scale.is_some()
            || self.rate.is_some()
            || self.pos.is_some()
            || self.rot.is_some()
            || self.visible.is_some()
            || self.frame.is_some()
            || self.color.is_some()
            || self.movement_state.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct LiveKind {
    pub name: String,
    /// Latest values from the plugin (merged observed + pins).
    pub data: RpmEffectData,
    /// First values seen for this kind this connection — the pristine spawn baseline
    /// edits are computed against so repeated edits don't compound.
    pub first: RpmEffectData,
    /// Active in-game pins (None on older plugins or when nothing is pinned).
    pub pins: Option<PinsWire>,
    pub updates: u64,
    pub last_update: Instant,
}

struct Shared {
    status: LinkStatus,
    client_id: Option<u64>,
    kinds: BTreeMap<u64, LiveKind>,
    /// Live ACMD capture log, keyed by motion hash (deduped; survives reconnects).
    captures: BTreeMap<u64, Vec<CaptureLine>>,
    /// First run observed for each fighter-kind + motion. Later runs are rejected even if an
    /// old or mismatched plugin tries to send them.
    capture_claimed_runs: BTreeMap<(i32, u64), u32>,
    /// Bumped on every new capture line — lets the app cheaply notice new data.
    captures_seq: u64,
    /// Live-carrier readiness reported by the plugin: 0 = none, 1 = staged/building, 2 = live.
    carrier_state: u8,
    /// The carrier battle object exists and is active — the real "can spawn now" signal.
    carrier_spawned: bool,
    /// Kinds the current carrier can serve — distinguishes "not up yet" from "up but does
    /// not know this kind".
    carrier_kinds: usize,
    /// Bumped on every CarrierStatus so callers can tell "no report yet" from "reported 0".
    carrier_seq: u64,
    /// Donor-bytes generation the LIVE carrier was built from. A send is only complete once
    /// this exceeds whatever was live when the send started — otherwise the previous carrier,
    /// still up and still reporting ready, reads as instant success.
    carrier_gen: u64,
    /// Why the plugin rejected the last carrier push, if it did. Taken by the app, which turns
    /// it into a status line and stops waiting — a rejected push never advances `carrier_gen`,
    /// so without this the editor waits for a carrier that is never coming.
    carrier_error: Option<String>,
    /// (fighter kind, motion hash) → number of completed playbacks the plugin reported.
    /// A bump means "every line that motion produces has now been streamed".
    capture_ends: BTreeMap<(i32, u64), u64>,
    /// Exact completed runs. Unlike the compatibility counter above, this cannot be advanced by
    /// another instance or a later playback of the same motion.
    capture_completed_runs: BTreeSet<(i32, u64, u32)>,
    outbox: Vec<String>,
    last_error: Option<String>,
    frames_rx: u64,
    edits_tx: u64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            status: LinkStatus::Disconnected,
            client_id: None,
            kinds: BTreeMap::new(),
            captures: BTreeMap::new(),
            capture_claimed_runs: BTreeMap::new(),
            captures_seq: 0,
            carrier_state: 0,
            carrier_spawned: false,
            carrier_kinds: 0,
            carrier_seq: 0,
            carrier_gen: 0,
            carrier_error: None,
            capture_ends: BTreeMap::new(),
            capture_completed_runs: BTreeSet::new(),
            outbox: Vec::new(),
            last_error: None,
            frames_rx: 0,
            edits_tx: 0,
        }
    }
}

pub struct GameLink {
    shared: Arc<Mutex<Shared>>,
    started: AtomicBool,
}

impl Default for GameLink {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            started: AtomicBool::new(false),
        }
    }
}

impl GameLink {
    /// Spawn the connection thread (idempotent). Called lazily when the eff editor opens.
    pub fn ensure_started(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        std::thread::Builder::new()
            .name("game-link".into())
            .spawn(move || link_thread(shared))
            .expect("spawn game-link thread");
    }

    pub fn status(&self) -> LinkStatus {
        self.shared
            .lock()
            .map(|s| s.status)
            .unwrap_or(LinkStatus::Disconnected)
    }

    pub fn last_error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.last_error.clone())
    }

    pub fn stats(&self) -> (u64, u64) {
        self.shared
            .lock()
            .map(|s| (s.frames_rx, s.edits_tx))
            .unwrap_or((0, 0))
    }

    /// Snapshot of all live kinds (id = hash40 of effect name).
    pub fn kinds(&self) -> Vec<(u64, LiveKind)> {
        self.shared
            .lock()
            .map(|s| s.kinds.iter().map(|(k, v)| (*k, v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn kind(&self, id: u64) -> Option<LiveKind> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.kinds.get(&id).cloned())
    }

    pub fn is_live(&self, id: u64) -> bool {
        self.shared
            .lock()
            .map(|s| s.kinds.contains_key(&id))
            .unwrap_or(false)
    }

    /// Replace the plugin's live spawn-rule list (suppress/retime ACMD effect spawns).
    /// Send the FULL current rule set every time — an empty slice clears all rules.
    pub fn send_spawn_rules(&self, rules: &[SpawnRuleWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "spawn_rules": rules }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Replace the live effect point-control rules (detach and area toggles).
    pub fn send_effect_control_rules(&self, rules: &[EffectControlRuleWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({
            "effect_control_rules": rules
        })) else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Replace the plugin's live transplant alias list (copy/replaced kind → donor kind,
    /// optionally costume-gated). Full-list replace; empty clears all aliases.
    pub fn send_effect_aliases(&self, aliases: &[EffectAliasWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "effect_aliases": aliases }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Cross-fighter donor eff files the plugin co-loads with each target fighter's
    /// effects (smashline-transplant mechanism), so donor content is spawnable live.
    pub fn send_donor_effs(&self, specs: &[DonorEffWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "donor_effs": specs })) else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Stripped donor eff bytes for the plugin to inject as resident data (live
    /// cross-character transplant). Sent whenever the referenced donor set changes.
    pub fn send_donor_bytes(&self, donors: &[DonorBytesWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "donor_bytes": donors }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Custom names (transplant copies) so the plugin resolves their hashes for display
    /// instead of falling back to hex.
    pub fn send_effect_names(&self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "effect_names": names }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Client id assigned by the plugin for this connection (changes on reconnect).
    pub fn client_id(&self) -> Option<u64> {
        self.shared.lock().ok().and_then(|s| s.client_id)
    }

    /// Kinds the plugin reports active user pins for (fresh-session desync detection).
    pub fn pinned_kinds(&self) -> Vec<(u64, LiveKind)> {
        self.shared
            .lock()
            .map(|s| {
                s.kinds
                    .iter()
                    .filter(|(_, v)| v.pins.as_ref().map(|p| p.any()).unwrap_or(false))
                    .map(|(k, v)| (*k, v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ask the plugin to clear ALL its pinned edits (incl. the SD save) and re-notify
    /// pristine values.
    pub fn send_reset_pins(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"reset_pins\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Tell the plugin the live-eff manifest / merged files on the Eden SD changed —
    /// it refreshes its Arcropolis file-provider registrations.
    pub fn send_live_eff_reload(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"live_eff_reload\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Ask the plugin to write `sd:/effect_viewer_probe.txt` (serving-chain diagnosis).
    pub fn send_live_eff_probe(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"live_eff_probe\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Lines from the MOST RECENT playback of `motion` by `kind` — one performance, never a
    /// merge of several.
    ///
    /// The store keeps more than one run so that a performance still streaming in cannot
    /// replace the last complete one mid-arrival; this is what picks between them. Sorted into
    /// script time, which within a single run is also the order the game executed.
    pub fn latest_run_for(&self, motion: u64, kind: Option<i32>) -> Vec<CaptureLine> {
        let Ok(s) = self.shared.lock() else {
            return Vec::new();
        };
        let Some(lines) = s.captures.get(&motion) else {
            return Vec::new();
        };
        let matching = |l: &&CaptureLine| kind.map(|k| l.kind == k).unwrap_or(true);
        let Some(latest) = lines.iter().filter(matching).map(|l| l.run).max() else {
            return Vec::new();
        };
        let mut v: Vec<CaptureLine> = lines
            .iter()
            .filter(|l| matching(l) && l.run == latest)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            a.frame
                .partial_cmp(&b.frame)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Forget every captured line locally and ask the plugin to release its per-move ownership
    /// claims. A game-thread acknowledgement clears any old line already in flight.
    pub fn clear_captures(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.captures.clear();
            s.capture_claimed_runs.clear();
            s.capture_ends.clear();
            s.capture_completed_runs.clear();
            s.captures_seq += 1;
            s.outbox
                .push("<TCP_MESSAGE>{\"command\":\"clear_acmd_captures\"}</TCP_MESSAGE>".into());
            s.edits_tx += 1;
        }
    }

    /// Every captured line across ALL motions, tagged with its motion hash. Used to discover
    /// every place an effect is used (each move performed live contributes its motion's lines).
    pub fn all_captures(&self) -> Vec<(u64, CaptureLine)> {
        self.shared
            .lock()
            .ok()
            .map(|s| {
                s.captures
                    .iter()
                    .flat_map(|(m, lines)| lines.iter().map(move |l| (*m, l.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Monotonic counter of received capture lines (cheap "anything new?" check).
    /// Live-carrier readiness: `(state, kinds, reports_seen)`.
    ///
    /// `state` is 0 none / 1 staged / 2 live. `reports_seen` is 0 with plugin builds that
    /// predate the `CarrierStatus` notify, so callers can degrade gracefully rather than
    /// waiting forever for a signal that will never arrive.
    pub fn carrier_status(&self) -> (u8, usize, u64, bool) {
        self.shared
            .lock()
            .map(|s| {
                (
                    s.carrier_state,
                    s.carrier_kinds,
                    s.carrier_seq,
                    s.carrier_spawned,
                )
            })
            .unwrap_or((0, 0, 0, false))
    }

    /// Donor-bytes generation of the carrier currently live in game.
    pub fn carrier_gen(&self) -> u64 {
        self.shared.lock().map(|s| s.carrier_gen).unwrap_or(0)
    }

    /// Take the reason the plugin rejected the last carrier push, clearing it.
    pub fn take_carrier_error(&self) -> Option<String> {
        self.shared
            .lock()
            .ok()
            .and_then(|mut s| s.carrier_error.take())
    }

    pub fn captures_seq(&self) -> u64 {
        self.shared.lock().map(|s| s.captures_seq).unwrap_or(0)
    }

    /// How many lines the CURRENT playback of one motion has produced so far. Cheap "did more
    /// arrive?" check that avoids cloning the bucket.
    ///
    /// Counts the claimed run only. The count rises while that one move plays and then remains
    /// frozen; unrelated or later runs are rejected before they reach this store.
    pub fn captures_count(&self, motion: u64, kind: Option<i32>) -> usize {
        self.shared
            .lock()
            .ok()
            .and_then(|s| {
                let lines = s.captures.get(&motion)?;
                let matching = |l: &&CaptureLine| kind.map(|k| l.kind == k).unwrap_or(true);
                let latest = lines.iter().filter(matching).map(|l| l.run).max()?;
                Some(
                    lines
                        .iter()
                        .filter(|l| matching(l) && l.run == latest)
                        .count(),
                )
            })
            .unwrap_or(0)
    }

    pub fn latest_run_id(&self, motion: u64, kind: Option<i32>) -> Option<u32> {
        self.shared.lock().ok().and_then(|s| {
            s.captures
                .get(&motion)?
                .iter()
                .filter(|line| kind.map(|value| line.kind == value).unwrap_or(true))
                .map(|line| line.run)
                .max()
        })
    }

    pub fn capture_run_complete(&self, kind: Option<i32>, motion: u64, run: u32) -> bool {
        self.shared.lock().is_ok_and(|s| match kind {
            Some(kind) => s.capture_completed_runs.contains(&(kind, motion, run)),
            None => s
                .capture_completed_runs
                .iter()
                .any(|(_, captured_motion, captured_run)| {
                    *captured_motion == motion && *captured_run == run
                }),
        })
    }

    /// How many times the plugin has reported this motion finishing (i.e. "its script has
    /// been streamed in full"). `kind = None` sums every fighter that played the motion.
    /// Stays 0 with plugin builds predating the `AcmdCaptureEnd` notify.
    #[cfg(test)]
    pub fn capture_end_count(&self, motion: u64, kind: Option<i32>) -> u64 {
        self.shared
            .lock()
            .map(|s| match kind {
                Some(k) => s.capture_ends.get(&(k, motion)).copied().unwrap_or(0),
                None => s
                    .capture_ends
                    .iter()
                    .filter(|((_, m), _)| *m == motion)
                    .map(|(_, n)| *n)
                    .sum(),
            })
            .unwrap_or(0)
    }

    /// Replace the plugin's live hitbox-rule list (modify/suppress/inject ATTACKs).
    /// Always the FULL set — an empty slice clears all rules.
    pub fn send_hitbox_rules(&self, rules: &[HitboxRuleWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "hitbox_rules": rules }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Queue a sparse kind-modifier edit. Color/speed controls must not upload the live
    /// form's position/rotation: those values are ACMD script offsets, while a carrier-owned
    /// transplant is physically world-space, so accidentally pinning them moves it to the
    /// stage origin. Authored EFF modifiers may additionally include kind-global scale.
    pub fn send_modifier_edit(&self, id: u64, data: &RpmEffectData, include_scale: bool) {
        let mut value = serde_json::json!({
            "speed": data.speed,
            "rainbow": {
                "color": data.rainbow.color,
            },
        });
        if include_scale {
            value["scale"] = serde_json::json!(data.scale);
        }
        let payload = serde_json::json!({ "id": id, "newValue": value.to_string() });
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }
}

// ── Connection thread ─────────────────────────────────────────────────────────

fn link_thread(shared: Arc<Mutex<Shared>>) {
    loop {
        {
            let mut s = shared.lock().unwrap();
            s.status = LinkStatus::Connecting;
        }
        let addr: std::net::SocketAddr = PLUGIN_ADDR.parse().unwrap();
        match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Ok(stream) => {
                {
                    let mut s = shared.lock().unwrap();
                    s.status = LinkStatus::Connected;
                    s.last_error = None;
                }
                let reason = serve_connection(&shared, stream);
                let mut s = shared.lock().unwrap();
                s.status = LinkStatus::Disconnected;
                s.last_error = reason;
            }
            Err(e) => {
                let mut s = shared.lock().unwrap();
                s.status = LinkStatus::Disconnected;
                s.last_error = Some(format!("connect: {e}"));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn serve_connection(shared: &Arc<Mutex<Shared>>, mut stream: TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = stream.set_nodelay(true);
    let mut buf = String::new();
    let mut chunk = [0u8; 8192];

    loop {
        // Outbound edits first — they're latency-sensitive.
        let pending: Vec<String> = {
            let mut s = shared.lock().unwrap();
            std::mem::take(&mut s.outbox)
        };
        for msg in pending {
            if let Err(e) = stream.write_all(msg.as_bytes()) {
                return Some(format!("send: {e}"));
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) => return Some("closed by plugin".into()),
            Ok(n) => {
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                for payload in extract_frames(&mut buf) {
                    handle_frame(shared, &payload);
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Some(format!("recv: {e}")),
        }
    }
}

fn extract_frames(buf: &mut String) -> Vec<String> {
    const OPEN: &str = "<TCP_MESSAGE>";
    const CLOSE: &str = "</TCP_MESSAGE>";
    let mut out = Vec::new();
    while let (Some(s), Some(e)) = (buf.find(OPEN), buf.find(CLOSE)) {
        if e < s {
            // Torn close tag before an open — drop the garbage prefix.
            *buf = buf[e + CLOSE.len()..].to_string();
            continue;
        }
        let payload = buf[s + OPEN.len()..e].trim().to_string();
        *buf = buf[e + CLOSE.len()..].to_string();
        if !payload.is_empty() {
            out.push(payload);
        }
    }
    if buf.len() > 1 << 20 {
        buf.clear();
    }
    out
}

fn handle_frame(shared: &Arc<Mutex<Shared>>, payload: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let Some(header) = v.get("header").and_then(|h| h.as_str()) else {
        return;
    };
    // The plugin serializes `body` as a JSON *string*; tolerate an object too.
    let body: serde_json::Value = match v.get("body") {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
        Some(other) => other.clone(),
        None => serde_json::Value::Null,
    };

    let mut s = shared.lock().unwrap();
    s.frames_rx += 1;
    match header {
        "Notify" => {
            let Some(n) = body.get("Notify") else { return };
            let Some(id) = n.get("id").and_then(|i| i.as_u64()) else {
                return;
            };
            let name = n
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string();
            let data: RpmEffectData = match n.get("value_in_json") {
                Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
                    Ok(d) => d,
                    Err(_) => return,
                },
                Some(obj) => match serde_json::from_value(obj.clone()) {
                    Ok(d) => d,
                    Err(_) => return,
                },
                None => return,
            };
            // Sparse pins-only form (newer plugins) — user overrides already active in-game.
            let pins: Option<PinsWire> = match n.get("pinned_in_json") {
                Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
                Some(serde_json::Value::Null) | None => None,
                Some(obj) => serde_json::from_value(obj.clone()).ok(),
            };
            match s.kinds.get_mut(&id) {
                Some(k) => {
                    k.name = name;
                    k.data = data;
                    k.pins = pins;
                    k.updates += 1;
                    k.last_update = Instant::now();
                }
                None => {
                    s.kinds.insert(
                        id,
                        LiveKind {
                            name,
                            first: data.clone(),
                            data,
                            pins,
                            updates: 1,
                            last_update: Instant::now(),
                        },
                    );
                }
            }
        }
        "AcmdCapture" => {
            let Some(c) = body.get("AcmdCapture") else {
                return;
            };
            let Ok(line) = serde_json::from_value::<CaptureLine>(c.clone()) else {
                return;
            };
            let claim = s
                .capture_claimed_runs
                .entry((line.kind, line.motion))
                .or_insert(line.run);
            if *claim != line.run {
                return;
            }
            let bucket = s.captures.entry(line.motion).or_default();
            // The plugin dedupes per session and a reconnect re-sends the whole log, so exact
            // duplicates have to be dropped here. Exact equality is not enough on its own:
            // `frame` is the raw `MotionModule::frame` float, and two performances of the same
            // move rarely produce bit-identical floats, so the SAME spawn arrived twice at
            // 5.0 and 5.0000019 and the editor showed the effect twice. Dedupe on what the
            // editor can actually distinguish — the whole line at frame resolution.
            if !bucket.iter().any(|existing| same_capture(existing, &line)) {
                bucket.push(line);
                s.captures_seq += 1;
            }
        }
        "AcmdCaptureEnd" => {
            // "That motion just finished playing" — every line it produces has already been
            // delivered (the plugin holds these back until the line backlog drains).
            let Some(e) = body.get("AcmdCaptureEnd") else {
                return;
            };
            let (Some(kind), Some(motion)) = (
                e.get("kind").and_then(|k| k.as_i64()),
                e.get("motion").and_then(|m| m.as_u64()),
            ) else {
                return;
            };
            let run = e.get("run").and_then(|value| value.as_u64()).unwrap_or(0) as u32;
            if s.capture_claimed_runs.get(&(kind as i32, motion)).copied() != Some(run) {
                return;
            }
            if s.capture_completed_runs.insert((kind as i32, motion, run)) {
                *s.capture_ends.entry((kind as i32, motion)).or_insert(0) += 1;
            }
        }
        "AcmdCaptureCleared" => {
            // The acknowledgement is emitted only after the game thread has discarded its
            // pending queue and ownership claims. Clear again here so no pre-command line that
            // was already in flight can repopulate the editor after the user requested reset.
            s.captures.clear();
            s.capture_claimed_runs.clear();
            s.capture_ends.clear();
            s.capture_completed_runs.clear();
            s.captures_seq += 1;
        }
        "CarrierStatus" => {
            // How far along the game is in taking the carrier we pushed. 0 = none staged,
            // 1 = staged/building, 2 = live and serving. The editor uses this to keep its
            // "sending" state up until the game has ACTUALLY taken the edit, instead of
            // clearing the moment the bytes left the socket.
            let Some(c) = body.get("CarrierStatus") else {
                return;
            };
            s.carrier_state = c.get("state").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            // The battle object actually existing is what `spawn_via_carrier` requires;
            // `state` alone can be 2 while nothing can spawn yet.
            s.carrier_spawned = c.get("spawned").and_then(|v| v.as_bool()).unwrap_or(false);
            s.carrier_kinds = c.get("kinds").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            s.carrier_gen = c.get("gen").and_then(|v| v.as_u64()).unwrap_or(0);
            s.carrier_seq += 1;
        }
        "CarrierError" => {
            s.carrier_error = body
                .get("CarrierError")
                .and_then(|c| c.get("reason"))
                .and_then(|r| r.as_str())
                .map(str::to_owned);
        }
        "Remove" => {
            if let Some(id) = body
                .get("Remove")
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_u64())
            {
                s.kinds.remove(&id);
            }
        }
        "RemoveAll" => s.kinds.clear(),
        "GiveClientId" => {
            s.client_id = body
                .get("GiveClientId")
                .and_then(|g| g.get("client_id"))
                .and_then(|c| c.as_u64());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-exact replica of the plugin's `emit()`: body is a JSON-escaped *string*.
    fn plugin_frame(header: &str, body: &serde_json::Value) -> String {
        let body_str = serde_json::to_string(body).unwrap();
        let body_esc = serde_json::to_string(&body_str).unwrap();
        format!("<TCP_MESSAGE>{{\"header\":\"{header}\",\"body\":{body_esc}}}</TCP_MESSAGE>")
    }

    #[test]
    fn parses_notify_remove_and_torn_frames() {
        let shared = Arc::new(Mutex::new(Shared::default()));

        let data = RpmEffectData {
            effect_name: "sys_flyroll_smoke".into(),
            scale: 1.2,
            ..Default::default()
        };
        let value_in_json = serde_json::to_string(&data).unwrap();
        let notify = plugin_frame(
            "Notify",
            &serde_json::json!({
                "Notify": { "id": 0x1154cb72bfu64, "name": "sys_flyroll_smoke", "value_in_json": value_in_json }
            }),
        );
        let handshake = "<TCP_MESSAGE>{\"header\":\"RemoveAll\",\"body\":\"{}\"}</TCP_MESSAGE>";

        // Feed the stream in torn chunks like a real socket would.
        let stream = format!("{handshake}{notify}");
        let (a, b) = stream.split_at(stream.len() / 2);
        let mut buf = String::new();
        buf.push_str(a);
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        buf.push_str(b);
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }

        let s = shared.lock().unwrap();
        let kind = s.kinds.get(&0x1154cb72bf).expect("kind parsed");
        assert_eq!(kind.name, "sys_flyroll_smoke");
        assert!((kind.data.scale - 1.2).abs() < 1e-6);
        assert_eq!(kind.first.scale, kind.data.scale);
        drop(s);

        let remove = plugin_frame(
            "Remove",
            &serde_json::json!({ "Remove": { "id": 0x1154cb72bfu64 } }),
        );
        let mut buf = remove;
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        assert!(shared.lock().unwrap().kinds.is_empty());
    }

    #[test]
    fn acmd_capture_parses_from_plugin_emit_form() {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        // Exactly what the plugin serializes: CaptureLine with tagged LuaArgs.
        let capture = plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": 0,
                    "motion": 0x1234u64,
                    "frame": 3.0,
                    "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        );
        let mut buf = capture;
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        let s = shared.lock().unwrap();
        let lines = s.captures.get(&0x1234).expect("capture stored");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].func, "ATTACK");
        assert_eq!(lines[0].args[2].as_hash(), Some(0x031ed91fca));
        assert_eq!(lines[0].args[3].as_f32(), Some(8.0));
        assert_eq!(lines[0].args[12], LuaArgWire::Nil);
        assert_eq!(s.captures_seq, 1);
        drop(s);

        // Re-delivery (reconnect resend) must not duplicate.
        let s2 = shared.clone();
        let mut buf2 = plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": 0, "motion": 0x1234u64, "frame": 3.0, "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        );
        for payload in extract_frames(&mut buf2) {
            handle_frame(&s2, &payload);
        }
        assert_eq!(s2.lock().unwrap().captures.get(&0x1234).unwrap().len(), 1);
    }

    /// The same spawn seen on a second performance arrives with a slightly different
    /// `MotionModule::frame` float. Exact equality treated those as two spawns, and the editor
    /// then showed the effect twice for a move performed twice.
    #[test]
    fn a_replayed_capture_line_is_not_a_second_spawn() {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let line = |frame: f64| {
            plugin_frame(
                "AcmdCapture",
                &serde_json::json!({
                    "AcmdCapture": {
                        "kind": 8, "motion": 0x1234u64, "frame": frame,
                        "func": "EFFECT_FOLLOW",
                        "args": [{"t":"h","v":0x1u64}, {"t":"h","v":0x2u64}]
                    }
                }),
            )
        };
        for frame in [5.0, 5.000_002, 4.999_998] {
            let mut buf = line(frame);
            for payload in extract_frames(&mut buf) {
                handle_frame(&shared, &payload);
            }
        }
        assert_eq!(
            shared.lock().unwrap().captures.get(&0x1234).unwrap().len(),
            1,
            "one spawn observed three times is still one spawn"
        );

        // A genuinely different frame is still its own spawn — multi-hit moves depend on it.
        let mut buf = line(9.0);
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        assert_eq!(
            shared.lock().unwrap().captures.get(&0x1234).unwrap().len(),
            2
        );
    }

    #[test]
    fn clearing_captures_empties_every_motion() {
        let link = GameLink::default();
        {
            let mut s = link.shared.lock().unwrap();
            s.captures.entry(0x1234).or_default().push(CaptureLine {
                kind: 8,
                motion: 0x1234,
                frame: 1.0,
                func: "ATTACK".into(),
                args: Vec::new(),
                run: 1,
            });
            s.capture_ends.insert((8, 0x1234), 1);
        }
        assert_eq!(link.latest_run_for(0x1234, None).len(), 1);
        link.clear_captures();
        assert!(link.latest_run_for(0x1234, None).is_empty());
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 0);
        let s = link.shared.lock().unwrap();
        assert!(s
            .outbox
            .last()
            .is_some_and(|frame| frame.contains("\"command\":\"clear_acmd_captures\"")));
        drop(s);

        // A line already in flight may arrive after the local clear. The plugin's game-thread
        // acknowledgement is the ordering barrier that removes it again.
        feed(&link, &[capture_frame(8, 0x1234, 9.0)]);
        assert_eq!(link.latest_run_for(0x1234, Some(8)).len(), 1);
        feed(
            &link,
            &[plugin_frame(
                "AcmdCaptureCleared",
                &serde_json::json!({ "AcmdCaptureCleared": true }),
            )],
        );
        assert!(link.latest_run_for(0x1234, Some(8)).is_empty());
    }

    /// A capture line for a fighter kind, in the plugin's exact emit form, with no `run`
    /// field — the shape a plugin build predating run ids sends.
    fn capture_frame(kind: i32, motion: u64, frame: f32) -> String {
        plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": kind, "motion": motion, "frame": frame, "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        )
    }

    /// The same helper with an explicit run id — one performance of the move.
    fn capture_run(kind: i32, motion: u64, frame: f32, run: u32, effect: u64) -> String {
        plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": kind, "motion": motion, "frame": frame,
                    "func": "EFFECT_FOLLOW", "run": run,
                    "args": [{"t":"h","v":effect}, {"t":"h","v":0x2u64}]
                }
            }),
        )
    }

    fn feed(link: &GameLink, payloads: &[String]) {
        for p in payloads {
            let mut buf = p.clone();
            for payload in extract_frames(&mut buf) {
                handle_frame(&link.shared, &payload);
            }
        }
    }

    /// The first playback owns the snapshot. Later runs cannot merge into or replace it, even
    /// when a mismatched/older plugin still sends them.
    #[test]
    fn only_the_first_performance_of_a_move_is_accepted() {
        let link = GameLink::default();
        // Run 7: two spawns. Run 9: a different second spawn (the other branch).
        feed(
            &link,
            &[
                capture_run(8, 0x1234, 3.0, 7, 0xAA),
                capture_run(8, 0x1234, 9.0, 7, 0xBB),
                capture_run(8, 0x1234, 3.0, 9, 0xAA),
                capture_run(8, 0x1234, 9.0, 9, 0xCC),
            ],
        );

        let loaded = link.latest_run_for(0x1234, Some(8));
        assert_eq!(loaded.len(), 2, "one performance, not the union of two");
        assert!(loaded.iter().all(|l| l.run == 7));
        let effects: Vec<u64> = loaded
            .iter()
            .filter_map(|l| l.args.first().and_then(|a| a.as_hash()))
            .collect();
        assert_eq!(
            effects,
            vec![0xAA, 0xBB],
            "later activity must not replace the locked branch"
        );

        // The settle window watches the claimed run only.
        assert_eq!(link.captures_count(0x1234, Some(8)), 2);
    }

    /// Another fighter playing the same motion allocates its own runs. Picking "the newest run
    /// overall" would then hand the editor someone else's script — so the kind filter has to
    /// come first.
    #[test]
    fn a_newer_run_from_another_fighter_does_not_win() {
        let link = GameLink::default();
        feed(
            &link,
            &[
                capture_run(8, 0x1234, 3.0, 4, 0xAA),
                // Higher run id, different fighter kind.
                capture_run(9, 0x1234, 3.0, 12, 0xFF),
            ],
        );
        let loaded = link.latest_run_for(0x1234, Some(8));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].args[0].as_hash(), Some(0xAA));
    }

    /// Repeated later runs are rejected at ingestion rather than merely hidden at load time.
    #[test]
    fn later_runs_never_enter_the_capture_store() {
        let link = GameLink::default();
        for run in 1..=6u32 {
            feed(&link, &[capture_run(8, 0x1234, 3.0, run, 0xAA)]);
        }
        let s = link.shared.lock().unwrap();
        let runs: std::collections::BTreeSet<u32> = s
            .captures
            .get(&0x1234)
            .unwrap()
            .iter()
            .map(|l| l.run)
            .collect();
        assert_eq!(
            runs.into_iter().collect::<Vec<_>>(),
            vec![1],
            "only the first claimed run is retained"
        );
    }

    /// A plugin build without run ids sends no `run` field at all. Everything then shares run
    /// 0 and behaves exactly as it did before — one bucket, no crash, no empty load.
    #[test]
    fn captures_without_run_ids_still_load() {
        let link = GameLink::default();
        feed(
            &link,
            &[capture_frame(8, 0x1234, 3.0), capture_frame(8, 0x1234, 9.0)],
        );
        assert_eq!(link.latest_run_for(0x1234, Some(8)).len(), 2);
    }

    /// `AcmdCaptureEnd` is the "this move finished, its script is fully streamed" signal the
    /// deferred auto-fetch keys off. It must be counted per (fighter kind, motion) and must
    /// NOT land in the capture buckets.
    #[test]
    fn acmd_capture_end_counts_per_kind_and_motion() {
        let link = GameLink::default();
        let end = |kind: i32, motion: u64, run: u32| {
            plugin_frame(
                "AcmdCaptureEnd",
                &serde_json::json!({
                    "AcmdCaptureEnd": { "kind": kind, "motion": motion, "run": run }
                }),
            )
        };

        assert_eq!(link.capture_end_count(0x1234, Some(8)), 0);
        feed(&link, &[capture_frame(8, 0x1234, 3.0)]);
        // An end marker from another playback cannot complete the claimed run.
        feed(&link, &[end(8, 0x1234, 9)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 0);
        feed(&link, &[end(8, 0x1234, 0)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 1);
        assert!(link.capture_run_complete(Some(8), 0x1234, 0));
        // The marker is not a capture line.
        assert_eq!(link.latest_run_for(0x1234, None).len(), 1);

        // An end from a different fighter cannot complete this fighter's snapshot.
        feed(&link, &[end(9, 0x1234, 12)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 1);
        // No line claimed kind 9/run 12, so its unrelated end is ignored.
        assert_eq!(link.capture_end_count(0x1234, Some(9)), 0);
        assert_eq!(link.capture_end_count(0x1234, None), 1);

        // A duplicate end for the same run cannot complete the snapshot twice.
        feed(&link, &[end(8, 0x1234, 0)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 1);
        // Unrelated motions stay at zero.
        assert_eq!(link.capture_end_count(0x5678, Some(8)), 0);
    }

    /// The settle window uses `captures_count` to tell "more lines arrived" from "some other
    /// move produced a line", so it has to be kind-filtered and per-motion.
    #[test]
    fn captures_count_filters_by_kind_and_motion() {
        let link = GameLink::default();
        feed(
            &link,
            &[
                capture_frame(8, 0x1234, 3.0),
                capture_frame(8, 0x1234, 5.0),
                capture_frame(9, 0x1234, 3.0),
                capture_frame(8, 0x5678, 3.0),
            ],
        );
        assert_eq!(link.captures_count(0x1234, Some(8)), 2);
        assert_eq!(link.captures_count(0x1234, Some(9)), 1);
        assert_eq!(link.captures_count(0x1234, None), 3);
        assert_eq!(link.captures_count(0x5678, Some(8)), 1);
        assert_eq!(link.captures_count(0xdead, Some(8)), 0);
    }

    #[test]
    fn outbound_hitbox_rules_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: 0,
                hitbox_id: Some(1),
                suppress: false,
                frame_start: Some(6.5),
                frame_end: Some(8.5),
                overrides: Some(HbOverridesWire {
                    damage: Some(12.0),
                    ..Default::default()
                }),
                inject: None,
                func: None,
            },
            HitboxRuleWire {
                motion: 0x99,
                category: 1,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(InjectRuleWire {
                    frame: 5.0,
                    args: vec![LuaArgWire::Int(2), LuaArgWire::Hash(0xabc), LuaArgWire::Nil],
                    command: None,
                }),
                func: None,
            },
            HitboxRuleWire {
                motion: 0x99,
                category: 2,
                hitbox_id: Some(3),
                suppress: false,
                frame_start: Some(11.5),
                frame_end: Some(12.5),
                overrides: Some(HbOverridesWire {
                    wind_args: Some(vec![LuaArgWire::Num(3.0), LuaArgWire::Num(1.0)]),
                    ..Default::default()
                }),
                inject: Some(InjectRuleWire {
                    frame: 12.0,
                    args: vec![LuaArgWire::Num(3.0), LuaArgWire::Num(1.0)],
                    command: Some("AREA_WIND_2ND".into()),
                }),
                func: None,
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = v.get("hitbox_rules").and_then(|r| r.as_array()).unwrap();
        // Field names the plugin's serde(Deserialize) expects.
        assert_eq!(rules[0]["motion"].as_u64(), Some(0x99));
        assert_eq!(rules[0]["hitbox_id"].as_u64(), Some(1));
        assert_eq!(rules[0]["overrides"]["damage"].as_f64(), Some(12.0));
        // Frame window scopes the override to one hit (multi-hit independence).
        assert_eq!(rules[0]["frame_start"].as_f64(), Some(6.5));
        assert_eq!(rules[0]["frame_end"].as_f64(), Some(8.5));
        assert!(rules[0].get("inject").is_none());
        // category 0 (attack) is omitted so old plugins default it; grab (1) is sent.
        assert!(rules[0].get("category").is_none());
        assert_eq!(rules[1]["category"].as_u64(), Some(1));
        // Inject rule carries no frame window (it fires at its own frame).
        assert!(rules[1].get("frame_start").is_none());
        assert_eq!(rules[1]["inject"]["frame"].as_f64(), Some(5.0));
        assert_eq!(rules[1]["inject"]["args"][0]["t"].as_str(), Some("i"));
        assert_eq!(rules[1]["inject"]["args"][1]["t"].as_str(), Some("h"));
        assert_eq!(rules[1]["inject"]["args"][2]["t"].as_str(), Some("x"));
        assert_eq!(rules[2]["category"].as_u64(), Some(2));
        assert_eq!(rules[2]["overrides"]["wind_args"][0]["v"], 3.0);
        assert_eq!(
            rules[2]["inject"]["command"].as_str(),
            Some("AREA_WIND_2ND")
        );
    }

    /// Attribute edits (Hit Properties / Collision Masks / Effect-Sound) used to be dropped
    /// on the floor: the override payload had no slot for them, so picking a value in the UI
    /// changed nothing in game. Guard both that they are SENT and that the JSON keys are the
    /// ones the plugin's `HbOverrides` deserializes.
    #[test]
    fn outbound_attribute_overrides_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: 0,
            hitbox_id: Some(0),
            suppress: false,
            frame_start: None,
            frame_end: None,
            overrides: Some(HbOverridesWire {
                part: Some(2),
                bone: Some(0x112233),
                capsule: Some(false),
                setoff: Some(1),
                lr_check: Some(3),
                clang: Some(true),
                add_attack: Some(0),
                hitbox_attr: Some(0.0),
                ground_or_air: Some(2),
                mtk: Some(false),
                shield_disable: Some(true),
                reflectable: Some(false),
                absorbable: Some(true),
                landing_attack: Some(false),
                situation_mask: Some(3),
                category_mask: Some(0x3F),
                part_mask: Some(0x1F),
                no_finish_camera: Some(true),
                collision_attr: Some(0x15a2c502b3),
                sound_level: Some(1),
                sound_attr: Some(1),
                attack_region: Some(4),
                ..Default::default()
            }),
            inject: None,
            func: None,
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let ov = &v["hitbox_rules"][0]["overrides"];
        assert_eq!(ov["part"].as_i64(), Some(2));
        assert_eq!(ov["bone"].as_u64(), Some(0x112233));
        assert_eq!(ov["capsule"].as_bool(), Some(false));
        assert_eq!(ov["setoff"].as_i64(), Some(1));
        assert_eq!(ov["lr_check"].as_i64(), Some(3));
        assert_eq!(ov["clang"].as_bool(), Some(true));
        assert_eq!(ov["add_attack"].as_i64(), Some(0));
        assert_eq!(ov["hitbox_attr"].as_f64(), Some(0.0));
        assert_eq!(ov["ground_or_air"].as_i64(), Some(2));
        assert_eq!(ov["mtk"].as_bool(), Some(false));
        assert_eq!(ov["shield_disable"].as_bool(), Some(true));
        assert_eq!(ov["reflectable"].as_bool(), Some(false));
        assert_eq!(ov["absorbable"].as_bool(), Some(true));
        assert_eq!(ov["landing_attack"].as_bool(), Some(false));
        assert_eq!(ov["situation_mask"].as_i64(), Some(3));
        assert_eq!(ov["category_mask"].as_i64(), Some(0x3F));
        assert_eq!(ov["part_mask"].as_i64(), Some(0x1F));
        assert_eq!(ov["no_finish_camera"].as_bool(), Some(true));
        assert_eq!(ov["collision_attr"].as_u64(), Some(0x15a2c502b3));
        assert_eq!(ov["sound_level"].as_i64(), Some(1));
        assert_eq!(ov["sound_attr"].as_i64(), Some(1));
        assert_eq!(ov["attack_region"].as_i64(), Some(4));
        // Unresolved slots stay absent so the plugin keeps the script's own value.
        let bare = serde_json::to_value(HbOverridesWire::default()).unwrap();
        assert!(bare.as_object().unwrap().is_empty());
    }

    #[test]
    fn outbound_effect_aliases_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_effect_aliases(&[
            EffectAliasWire {
                from: 0x111,
                to: 0x222,
                slots: vec![],
            },
            EffectAliasWire {
                from: 0x333,
                to: 0x444,
                slots: vec![1, 3],
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        // Field names the plugin's spawn_rules::EffectAlias serde(Deserialize) expects.
        let aliases = v.get("effect_aliases").and_then(|a| a.as_array()).unwrap();
        assert_eq!(aliases[0]["from"].as_u64(), Some(0x111));
        assert_eq!(aliases[0]["to"].as_u64(), Some(0x222));
        // Empty slots (all costumes) omitted so the plugin's serde(default) fills it.
        assert!(aliases[0].get("slots").is_none());
        assert_eq!(aliases[1]["slots"][0].as_u64(), Some(1));
        assert_eq!(aliases[1]["slots"][1].as_u64(), Some(3));
    }

    #[test]
    fn outbound_live_tweak_omits_spawn_transform_fields() {
        let link = GameLink::default();
        let mut data = RpmEffectData {
            pos: Point3D {
                x: 6.0,
                y: -2.0,
                z: 1.5,
            },
            rot: Point3D {
                x: 0.0,
                y: 110.0,
                z: 0.0,
            },
            scale: 0.7,
            speed: 1.25,
            ..Default::default()
        };
        data.rainbow.color.red = 0.5;
        link.send_modifier_edit(0x1311e844a4, &data, false);

        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let outer: serde_json::Value = serde_json::from_str(inner).unwrap();
        let edit: serde_json::Value =
            serde_json::from_str(outer["newValue"].as_str().unwrap()).unwrap();

        assert_eq!(edit["speed"].as_f64(), Some(1.25));
        assert_eq!(edit["rainbow"]["color"]["red"].as_f64(), Some(0.5));
        assert!(edit.get("pos").is_none());
        assert!(edit.get("rot").is_none());
        assert!(edit.get("scale").is_none());
    }

    #[test]
    fn outbound_authored_modifiers_only_add_scale() {
        let link = GameLink::default();
        let mut data = RpmEffectData {
            scale: 1.75,
            ..Default::default()
        };
        data.pos.x = 99.0;
        link.send_modifier_edit(0x109297479a, &data, true);

        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let outer: serde_json::Value = serde_json::from_str(inner).unwrap();
        let edit: serde_json::Value =
            serde_json::from_str(outer["newValue"].as_str().unwrap()).unwrap();

        assert_eq!(edit["scale"].as_f64(), Some(1.75));
        assert!(edit.get("pos").is_none());
        assert!(edit.get("rot").is_none());
    }

    // ── The sound family's two-sided constants ───────────────────────────────
    //
    // The plugin crate is not a member of this workspace, so `cargo test` never builds it and a
    // `#[test]` written over there would be a comment that looks like a gate. These read the
    // plugin's source as text instead. That is weaker than compiling against it — it pins the
    // source, not the linked `.nro` — but it is the only mechanism that runs, and the failure it
    // guards against (the two tables drifting) is silent in every other check: a rule with the
    // wrong arity still serialises, still sends, and still deserialises.

    fn plugin_sound_hooks_source() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer/sound_hooks.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// The plugin's copy of the sound arity table still says what the editor's says.
    ///
    /// Deliberately spelled the same way on both sides so this is a literal comparison. If they
    /// diverge, the plugin bounds an override by the wrong number of hash slots: too few and a
    /// rename silently only takes on the first sound of a pair, too many and it writes a hash40
    /// into `SET_PLAY_INHIVIT`'s duration argument.
    #[test]
    fn the_plugin_sound_table_still_matches_the_editors() {
        let source = plugin_sound_hooks_source();
        // Split on the `= &[`, not on the declaration: the first `]` after `const SOUND_FUNCS`
        // closes the *type* — `&[(&str, usize, bool)]` — and slicing there leaves an empty
        // table that every `contains` then fails against for the wrong reason.
        let table = source
            .split_once("const SOUND_FUNCS")
            .expect("plugin still declares SOUND_FUNCS")
            .1
            .split_once("= &[")
            .expect("the table is a slice literal")
            .1
            .split_once("];")
            .expect("the table is closed")
            .0;
        for (func, hashes, has_tail) in crate::acmd::SOUND_FUNCS {
            let row = format!("(\"{func}\", {hashes}, {has_tail})");
            assert!(
                table.contains(&row),
                "plugin's SOUND_FUNCS is missing or disagrees on {row}"
            );
        }
        // The other direction: a member the plugin hooks but the editor does not model would
        // capture into a stream nothing reads, and take rules the editor can never send.
        let plugin_rows = table.matches("(\"").count();
        assert_eq!(
            plugin_rows,
            crate::acmd::SOUND_FUNCS.len(),
            "the two tables have different lengths"
        );
    }

    /// Every hook reads exactly as many lua arguments as its macro declares.
    ///
    /// The arity is a literal in each `sound_hook!` invocation, separate from the table above,
    /// so the two can disagree. They must not: `read_args_exact` short by one hands slot 0 of
    /// the wrong value to the rule matcher, and long by one reads whatever the stack happens to
    /// hold past the call. Neither shows up as an error anywhere — the rule simply stops
    /// matching, or matches the wrong call.
    #[test]
    fn every_sound_hook_reads_its_macros_declared_arity() {
        let source = plugin_sound_hooks_source();
        let mut checked = 0;
        for (func, hashes, has_tail) in crate::acmd::SOUND_FUNCS {
            let argc = hashes + usize::from(*has_tail);
            let needle = format!("\"{func}\",\n    {argc}\n);");
            let one_line = format!("\"{func}\", {argc});");
            assert!(
                source.contains(&needle) || source.contains(&one_line),
                "no sound_hook! for {func} at arity {argc}"
            );
            checked += 1;
        }
        // Guard against the whole loop passing because the table went empty.
        assert_eq!(checked, 12, "the sound family is 12 members");
    }

    /// A charged smash attack must not lose everything after the charge.
    ///
    /// `mark_capture_motion` refuses to open a second run for a `(kind, motion)` that is already
    /// claimed, and a claim lives until the editor clears captures. A smash attack's hold reads
    /// as "the playback ended" — the motion frame rewinds, or the fighter parks in another
    /// motion — so every call after the charge landed on that refusal and was discarded:
    /// `attack_lw4` kept its two `FT_MOTION_RATE` calls from frames 0 and 4 and lost the
    /// `ATTACK` on 10, the `ATK_POWER` on 15 and its sounds. Tilts have no hold and were fine,
    /// which is what made it look like a per-family bug for three rounds.
    ///
    /// This pins the *source*, not the linked `.nro`, and it is a weak check for a strong
    /// property — but the property is otherwise invisible from this crate, and the failure it
    /// guards is silent by construction: a dropped capture line looks exactly like a call the
    /// game never made.
    #[test]
    fn the_plugin_resumes_a_capture_claim_held_by_the_same_object() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins/slight_replica/src/slight/hitbox_viewer/mod.rs"),
        )
        .expect("read the plugin's hitbox_viewer");
        assert!(
            source.contains("Some(h) if h.boid == boid && !h.ended => h.run,"),
            "the same-object resume arm is gone — a charged smash attack will lose every call \
             after its hold, silently"
        );
        // **`!h.ended` is the other half, and each half alone is a bug that was shipped.**
        // Resuming on `boid` alone piles every later performance into one snapshot: a charged
        // smash releases at a different motion frame each time, so the frame in the dedupe key
        // differs, nothing collapses, and the timeline fills with duplicate effects.
        assert!(
            source.contains("ended: bool") && source.contains("claim.ended = true"),
            "the claim no longer records whether a playback finished — captures will accumulate \
             across performances instead of the newest run winning"
        );
        // The third: a claim held by a *different* object is a real conflict and must still be
        // refused, or one fighter's capture absorbs another's.
        assert!(
            source.contains("claimed by another object"),
            "the cross-object refusal is gone — captures from two objects can now merge"
        );
    }

    /// The collision hook's Rust ABI must stay aligned with the 13.0.4 native target.
    ///
    /// The plugin is a separate Skyline crate, so the desktop workspace cannot type-check it as
    /// part of `cargo test`. The plugin release build catches compiler-level drift, while this
    /// source assertion makes the important native argument classes visible to the desktop gate:
    /// the manager receiver, two object IDs, a float hit value, an i32 collision ID, and a bool.
    #[test]
    fn the_plugin_collision_hook_keeps_the_1304_native_abi() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/systems/skyline_hook.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for needle in [
            "type CollisionHitTrampoline = unsafe extern \"C\" fn(",
            "*mut smash::app::FighterManager",
            "damage: f32",
            "collision_id: i32",
            "flags: bool",
            "    bool,\n) -> u64;",
            "#[skyline::hook(\n    replace = smash::app::lua_bind::FighterManager::notify_log_event_collision_hit\n)]",
            "skyline::install_hook!(fallback_collision_hit_hook);",
            "let mut match_offset = None;",
            "if match_offset.is_some()",
            "body pattern missing or ambiguous",
            "COLLISION_HOOK mode=wrapper-fallback coverage=partial",
            "COLLISION attacker=",
            "orig(",
        ] {
            assert!(
                source.contains(needle),
                "collision hook ABI guard is missing {needle:?}"
            );
        }
        assert!(
            !source.contains("type CollisionHitHook =") && !source.contains("param_1: u32"),
            "the obsolete six-integer collision hook ABI is still present"
        );

        let disabled_start = source
            .find("if !ENABLE_INLINE_COLLISION_HOOK")
            .expect("collision hook must keep an explicit inline-hook gate");
        let scan_start = source[disabled_start..]
            .find("\n    let offset = match scan_collision_pattern()")
            .map(|offset| disabled_start + offset)
            .expect("collision hook inline gate must precede pattern scanning");
        let disabled_block = &source[disabled_start..scan_start];
        for needle in [
            "inline collision hook disabled (Eden-unsafe); installing wrapper fallback (partial coverage)",
            "skyline::install_hook!(fallback_collision_hit_hook);",
            "*INSTALLED.lock() = true;",
        ] {
            assert!(
                disabled_block.contains(needle),
                "disabled collision hook path is missing {needle:?}"
            );
        }
    }

    /// The rate category and override field the editor sends are the ones the plugin reads.
    ///
    /// Same mechanism and same weakness as the sound checks above: this pins the plugin's
    /// source, not the linked `.nro`. It is worth having because every other check is blind
    /// here — a rate rule under the wrong category still serialises, still sends, and still
    /// deserialises, and simply matches nothing. That is precisely the shape that cost D1g four
    /// game restarts.
    #[test]
    fn the_motion_rate_category_and_override_match_the_plugin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        assert!(
            module.contains(&format!(
                "pub const CAT_MOTION_RATE: u8 = {CAT_MOTION_RATE};"
            )),
            "plugin's CAT_MOTION_RATE disagrees with the editor's {CAT_MOTION_RATE}"
        );
        // One flat category space shared by every family: a collision routes rate rules into
        // another family's hook, where they would be applied to the wrong argument.
        for other in [
            CAT_ABS,
            CAT_ATK_POWER,
            CAT_ATK_SETOFF_MUL,
            CAT_SEARCH,
            CAT_SOUND,
        ] {
            assert_ne!(
                CAT_MOTION_RATE, other,
                "CAT_MOTION_RATE collides with another family"
            );
        }
        assert!(
            module.contains("pub motion_rate: Option<f32>,"),
            "the plugin's HbOverrides has no motion_rate field to deserialise into"
        );

        let hooks = std::fs::read_to_string(root.join("rate_hooks.rs")).expect("read rate_hooks");
        assert!(
            hooks.contains("replace = smash::app::sv_animcmd::FT_MOTION_RATE"),
            "the plugin no longer hooks sv_animcmd::FT_MOTION_RATE"
        );
        // The guard that stops a bad rule wedging the fighter: a zero or negative rate freezes
        // the animation and nothing below the call runs again.
        assert!(
            hooks.contains("rate > 0.0") && hooks.contains("is_finite"),
            "the plugin no longer refuses a non-positive rate"
        );
    }

    /// A rate rule serialises under the field names the plugin deserialises.
    #[test]
    fn outbound_motion_rate_rules_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_MOTION_RATE,
            hitbox_id: None,
            suppress: false,
            frame_start: Some(8.5),
            frame_end: Some(9.5),
            overrides: Some(HbOverridesWire {
                motion_rate: Some(0.6),
                ..Default::default()
            }),
            inject: None,
            func: None,
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &v["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_MOTION_RATE as u64));
        assert_eq!(
            rule["overrides"]["motion_rate"].as_f64().map(|f| f as f32),
            Some(0.6),
            "the rate did not go out under the plugin's field name"
        );
        // A rate rule keys on motion and frame alone. The plugin passes 0 as the id, so a
        // serialised id would make the rule match nothing at all.
        assert!(
            rule["hitbox_id"].is_null(),
            "a rate rule must not carry an id: {rule}"
        );
    }

    #[test]
    fn outbound_camera_flat_offset_rules_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_spawn_rules(&[SpawnRuleWire {
            eff_hash: 0x99,
            suppress: false,
            motion: Some(0x1234),
            frame_start: Some(5.0),
            frame_end: Some(5.0),
            pos: None,
            rot: None,
            scale: None,
            rate: None,
            camera_offset: Some(0.4),
            tint: None,
            particle_tint: Some([0.1, 1.2, 0.3]),
            alpha: None,
            scale_w: Some(vec![0.75]),
            color: None,
            transition: None,
            inject: None,
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let offset = v["spawn_rules"][0]["camera_offset"]
            .as_f64()
            .map(|value| value as f32);
        assert_eq!(
            offset.map(|value| (value - 0.4).abs() < f32::EPSILON),
            Some(true),
            "the camera-flat value must use the plugin's field name"
        );
        assert_eq!(
            v["spawn_rules"][0]["particle_tint"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_f64)
                        .map(|value| value as f32)
                        .collect::<Vec<_>>()
                }),
            Some(vec![0.1, 1.2, 0.3]),
            "the particle tint must use its own wire field"
        );
        assert_eq!(
            v["spawn_rules"][0]["scale_w"].as_array().map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_f64)
                    .map(|value| value as f32)
                    .collect::<Vec<_>>()
            }),
            Some(vec![0.75]),
            "the dynamic scale-W values must use their own wire field"
        );

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/effect_viewer");
        let rules = std::fs::read_to_string(root.join("spawn_rules.rs"))
            .expect("read the plugin's spawn rules");
        assert!(
            rules.contains("pub camera_offset: Option<f32>,")
                && rules.contains("pub fn camera_offset_for")
                && rules.contains("pub particle_tint: Option<[f32; 3]>")
                && rules.contains("pub fn particle_tint_for")
                && rules.contains("pub scale_w: Option<Vec<f32>>")
                && rules.contains("pub fn scale_w_for"),
            "the plugin no longer exposes the camera-flat rule field and lookup"
        );
        let hooks = std::fs::read_to_string(root.join("acmd_hooks.rs"))
            .expect("read the plugin's ACMD hooks");
        assert!(
            hooks.contains(
                "replace = smash::app::sv_animcmd::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT"
            ) && hooks.contains("hook_last_effect_set_offset_to_camera_flat")
                && hooks.contains("apply_pending_camera_offset")
                && hooks.contains("LAST_PARTICLE_SET_COLOR")
                && hooks.contains("apply_pending_particle_tint")
                && hooks.contains("LAST_EFFECT_SET_SCALE_W")
                && hooks.contains("apply_pending_scale_w"),
            "the plugin no longer rewrites the camera-flat modifier hook"
        );
    }

    #[test]
    fn outbound_effect_control_rules_keep_exact_args_and_injection() {
        let link = GameLink::default();
        link.send_effect_control_rules(&[
            EffectControlRuleWire {
                motion: Some(0x99),
                func: "ENABLE_AREA".into(),
                args: vec![LuaArgWire::Int(2)],
                suppress: true,
                frame_start: Some(7.5),
                frame_end: Some(8.5),
                work_slot: None,
                inject: Some(EffectControlInjectWire {
                    frame: 12.0,
                    func: "ENABLE_AREA".into(),
                    args: vec![LuaArgWire::Int(3)],
                }),
            },
            EffectControlRuleWire {
                motion: Some(0x99),
                func: "EFFECT_DETACH_KIND_WORK".into(),
                args: vec![LuaArgWire::Int(0x1234), LuaArgWire::Int(2)],
                suppress: true,
                frame_start: Some(7.5),
                frame_end: Some(8.5),
                work_slot: Some(17),
                inject: Some(EffectControlInjectWire {
                    frame: 12.0,
                    func: "EFFECT_DETACH_KIND_WORK".into(),
                    args: vec![LuaArgWire::Int(0x5678), LuaArgWire::Int(3)],
                }),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let area = &value["effect_control_rules"][0];
        assert_eq!(area["func"].as_str(), Some("ENABLE_AREA"));
        assert_eq!(area["args"][0]["t"].as_str(), Some("i"));
        assert_eq!(area["args"][0]["v"].as_i64(), Some(2));
        assert_eq!(area["inject"]["frame"].as_f64(), Some(12.0));
        assert_eq!(area["inject"]["args"][0]["v"].as_i64(), Some(3));

        let work = &value["effect_control_rules"][1];
        assert_eq!(work["func"].as_str(), Some("EFFECT_DETACH_KIND_WORK"));
        assert_eq!(work["args"][0]["t"].as_str(), Some("i"));
        assert_eq!(work["args"][0]["v"].as_i64(), Some(0x1234));
        assert_eq!(work["work_slot"].as_i64(), Some(17));
        assert_eq!(work["inject"]["frame"].as_f64(), Some(12.0));
        assert_eq!(work["inject"]["args"][0]["v"].as_i64(), Some(0x5678));

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/effect_viewer");
        let rules = std::fs::read_to_string(root.join("control_rules.rs"))
            .expect("read the plugin's effect control rules");
        let hooks = std::fs::read_to_string(root.join("acmd_hooks.rs"))
            .expect("read the plugin's ACMD hooks");
        assert!(
            rules.contains("pub struct ControlRule")
                && rules.contains("pub struct ControlInject")
                && rules.contains("pub work_slot: Option<i32>")
                && rules.contains("self.args == args")
                && hooks.contains("WorkModule::get_int64(boma, slot)")
                && hooks.contains("args[0] = crate::slight::hitbox_viewer::LuaArg::Int")
                && hooks.contains("effect control work-slot injection rejected"),
            "the plugin must match controls by their exact captured argument vector"
        );
    }

    /// The wire category the editor sends is the one the plugin matches on.
    #[test]
    fn the_sound_wire_category_matches_the_plugin() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer/mod.rs");
        let source = std::fs::read_to_string(path).expect("read the plugin's hitbox_viewer");
        assert!(
            source.contains(&format!("pub const CAT_SOUND: u8 = {CAT_SOUND};")),
            "plugin's CAT_SOUND disagrees with the editor's {CAT_SOUND}"
        );
        // The categories are one flat space shared by every family, so a new one colliding with
        // an existing one would route sound rules into another family's hook.
        for other in [CAT_ATK_POWER, CAT_ATK_SETOFF_MUL, CAT_ABS, CAT_SEARCH] {
            assert_ne!(CAT_SOUND, other, "CAT_SOUND collides with another family");
        }
    }

    /// A sound rule serialises under the field names the plugin deserialises.
    #[test]
    fn outbound_sound_rules_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_SOUND,
            hitbox_id: Some(0xabc),
            suppress: false,
            frame_start: Some(5.5),
            frame_end: Some(6.5),
            overrides: Some(HbOverridesWire {
                sound_hashes: Some(vec![0xdef, 0x123]),
                ..Default::default()
            }),
            inject: None,
            func: Some("PLAY_STEP_FLIPPABLE".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &v["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_SOUND as u64));
        assert_eq!(rule["func"].as_str(), Some("PLAY_STEP_FLIPPABLE"));
        let hashes = rule["overrides"]["sound_hashes"].as_array().unwrap();
        assert_eq!(hashes[0].as_u64(), Some(0xdef));
        assert_eq!(hashes[1].as_u64(), Some(0x123));
    }

    /// Expression rules use the plugin's distinct category, macro discriminator, and complete
    /// typed argument vector. The three pieces have to travel together: the category selects the
    /// hook family, `func` selects its exact primitive, and the vector is what gets pushed back
    /// onto the Lua stack.
    #[test]
    fn outbound_expression_rules_match_plugin_field_names() {
        let link = GameLink::default();
        let pristine = vec![LuaArgWire::Int(5)];
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_EXPRESSION,
            hitbox_id: Some(expression_key("QUAKE", &pristine)),
            suppress: false,
            frame_start: Some(8.5),
            frame_end: Some(8.5),
            overrides: Some(HbOverridesWire {
                expression_args: Some(vec![LuaArgWire::Int(7)]),
                ..Default::default()
            }),
            inject: None,
            func: Some("QUAKE".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &v["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_EXPRESSION as u64));
        assert_eq!(rule["func"].as_str(), Some("QUAKE"));
        assert_eq!(
            rule["hitbox_id"].as_u64(),
            Some(expression_key("QUAKE", &pristine))
        );
        assert_eq!(
            rule["overrides"]["expression_args"][0]["t"].as_str(),
            Some("i")
        );
        assert_eq!(
            rule["overrides"]["expression_args"][0]["v"].as_i64(),
            Some(7)
        );
    }

    /// The editor and plugin agree on the expression category, field, and every measured hook
    /// name, including the direct native rumble binding.
    #[test]
    fn the_expression_category_and_hooks_match_the_plugin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        assert!(module.contains(&format!("pub const CAT_EXPRESSION: u8 = {CAT_EXPRESSION};")));
        assert!(module.contains("pub expression_args: Option<Vec<LuaArg>>,"));
        for func in [
            "RUMBLE_HIT",
            "QUAKE",
            "FT_ATTACK_ABS_CAMERA_QUAKE",
            "ControlModule::set_rumble",
        ] {
            assert!(
                module.contains(&format!("\"{func}\"")),
                "missing {func} hook"
            );
        }
        for other in [0, CAT_SOUND, CAT_MOTION_RATE, CAT_SPEED_EX, CAT_SPEED] {
            assert_ne!(
                CAT_EXPRESSION, other,
                "CAT_EXPRESSION collides with another family"
            );
        }
    }

    /// The editor and plugin agree on the separate point-event category and its hook. Keeping it
    /// out of the collision categories matters: a facing rule has no hitbox id or payload to
    /// apply, and routing it through ATTACK would turn an orientation edit into a collision edit.
    #[test]
    fn the_reverse_lr_category_and_hook_match_the_plugin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        assert!(module.contains(&format!("pub const CAT_REVERSE_LR: u8 = {CAT_REVERSE_LR};")));
        assert!(module.contains("replace = smash::app::sv_animcmd::REVERSE_LR"));
        assert!(module.contains("hook_reverse_lr"));
        assert!(module.contains("CAT_REVERSE_LR =>"));
        for other in [
            0,
            CAT_ABS,
            CAT_SEARCH,
            CAT_ATK_POWER,
            CAT_ATK_SETOFF_MUL,
            CAT_SOUND,
            CAT_MOTION_RATE,
            CAT_EXPRESSION,
            CAT_SPEED_EX,
            CAT_SPEED,
        ] {
            assert_ne!(
                CAT_REVERSE_LR, other,
                "CAT_REVERSE_LR collides with {other}"
            );
        }
    }

    #[test]
    fn the_set_speed_ex_category_and_hook_match_the_plugin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        assert!(module.contains(&format!("pub const CAT_SPEED_EX: u8 = {CAT_SPEED_EX};")));
        assert!(module.contains("SET_SPEED_EX"));
        assert!(module.contains("hook_set_speed_ex"));
        assert!(module.contains("pub speed_x: Option<f32>"));
        assert!(module.contains("pub speed_y: Option<f32>"));
        for other in [
            0,
            CAT_SEARCH,
            CAT_SOUND,
            CAT_MOTION_RATE,
            CAT_EXPRESSION,
            CAT_REVERSE_LR,
        ] {
            assert_ne!(CAT_SPEED_EX, other, "CAT_SPEED_EX collides with {other}");
        }
    }

    #[test]
    fn the_speed_addition_and_correction_categories_match_the_plugin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        assert!(module.contains(&format!(
            "pub const CAT_ADD_SPEED_NO_LIMIT: u8 = {CAT_ADD_SPEED_NO_LIMIT};"
        )));
        assert!(module.contains(&format!("pub const CAT_CORRECT: u8 = {CAT_CORRECT};")));
        assert!(module.contains("replace = smash::app::sv_animcmd::ADD_SPEED_NO_LIMIT"));
        assert!(module.contains("replace = smash::app::sv_animcmd::CORRECT"));
        assert!(module.contains("hook_add_speed_no_limit"));
        assert!(module.contains("hook_correct"));
        assert!(module.contains("pub correct_kind: Option<i64>"));
        assert!(module.contains(&format!("pub const CAT_SPEED: u8 = {CAT_SPEED};")));
        assert!(module.contains("replace = smash::app::sv_animcmd::SET_SPEED"));
        assert!(module.contains("hook_set_speed"));
        assert!(module.contains(&format!(
            "pub const CAT_FT_CATCH_STOP: u8 = {CAT_FT_CATCH_STOP};"
        )));
        assert!(module.contains("replace = smash::app::sv_animcmd::FT_CATCH_STOP"));
        assert!(module.contains("hook_ft_catch_stop"));
        assert!(module.contains("pub ft_catch_stop_arg1: Option<f32>"));
        assert!(module.contains("pub ft_catch_stop_arg2: Option<f32>"));
        assert!(module.contains(&format!(
            "pub const CAT_FT_START_ADJUST_MOTION_FRAME: u8 = {CAT_FT_START_ADJUST_MOTION_FRAME};"
        )));
        assert!(
            module.contains("replace = smash::app::sv_animcmd::FT_START_ADJUST_MOTION_FRAME_arg1")
        );
        assert!(module.contains("hook_ft_start_adjust_motion_frame"));
        assert!(module.contains("pub ft_start_adjust_motion_frame_value: Option<f32>"));
        for (category, other) in [
            (CAT_ADD_SPEED_NO_LIMIT, CAT_SPEED_EX),
            (CAT_CORRECT, CAT_SPEED_EX),
            (CAT_SPEED, CAT_SPEED_EX),
            (CAT_ADD_SPEED_NO_LIMIT, CAT_CORRECT),
            (CAT_SPEED, CAT_ADD_SPEED_NO_LIMIT),
            (CAT_SPEED, CAT_CORRECT),
            (CAT_FT_CATCH_STOP, CAT_SPEED_EX),
            (CAT_FT_CATCH_STOP, CAT_SPEED),
            (CAT_FT_CATCH_STOP, CAT_ADD_SPEED_NO_LIMIT),
            (CAT_FT_CATCH_STOP, CAT_CORRECT),
            (CAT_FT_START_ADJUST_MOTION_FRAME, CAT_FT_CATCH_STOP),
            (CAT_FT_START_ADJUST_MOTION_FRAME, CAT_SPEED_EX),
            (CAT_FT_START_ADJUST_MOTION_FRAME, CAT_SPEED),
            (CAT_FT_START_ADJUST_MOTION_FRAME, CAT_ADD_SPEED_NO_LIMIT),
            (CAT_FT_START_ADJUST_MOTION_FRAME, CAT_CORRECT),
        ] {
            assert_ne!(category, other, "new point categories must not collide");
        }
    }

    #[test]
    fn outbound_ft_start_adjust_motion_frame_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let key = numeric_point_key("FT_START_ADJUST_MOTION_FRAME_arg1", &[0.85]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_FT_START_ADJUST_MOTION_FRAME,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(17.0),
            frame_end: Some(17.0),
            overrides: Some(HbOverridesWire {
                ft_start_adjust_motion_frame_value: Some(1.25),
                ..Default::default()
            }),
            inject: None,
            func: Some("FT_START_ADJUST_MOTION_FRAME_arg1".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_FT_START_ADJUST_MOTION_FRAME as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(
            rule["func"].as_str(),
            Some("FT_START_ADJUST_MOTION_FRAME_arg1")
        );
        assert_eq!(
            rule["overrides"]["ft_start_adjust_motion_frame_value"].as_f64(),
            Some(1.25)
        );
    }

    #[test]
    fn kinetic_point_categories_and_overrides_match_plugin_wire_fields() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/slight_replica/src/slight/hitbox_viewer");
        let module = std::fs::read_to_string(root.join("mod.rs")).expect("read hitbox_viewer");
        let rate_hooks =
            std::fs::read_to_string(root.join("rate_hooks.rs")).expect("read rate hooks");
        assert!(module.contains(&format!("pub const CAT_CLR_SPEED: u8 = {CAT_CLR_SPEED};")));
        assert!(module.contains(&format!("pub const CAT_SET_AIR: u8 = {CAT_SET_AIR};")));
        assert!(module.contains(&format!(
            "pub const CAT_CHANGE_KINETIC: u8 = {CAT_CHANGE_KINETIC};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_ADD_SPEED: u8 = {CAT_KINETIC_ADD_SPEED};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_SUSPEND_ENERGY: u8 = {CAT_KINETIC_SUSPEND_ENERGY};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_RESUME_ENERGY: u8 = {CAT_KINETIC_RESUME_ENERGY};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_ENABLE_ENERGY: u8 = {CAT_KINETIC_ENABLE_ENERGY};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_UNABLE_ENERGY: u8 = {CAT_KINETIC_UNABLE_ENERGY};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_CLEAR_SPEED_ALL: u8 = {CAT_KINETIC_CLEAR_SPEED_ALL};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION: u8 = {CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_MOTION_MODULE_SET_RATE: u8 = {CAT_MOTION_MODULE_SET_RATE};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_MOTION_MODULE_SET_HELPER_CALCULATION: u8 = {CAT_MOTION_MODULE_SET_HELPER_CALCULATION};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_MOTION_MODULE_SET_RATE_PARTIAL: u8 = {CAT_MOTION_MODULE_SET_RATE_PARTIAL};"
        )));
        assert!(module.contains(&format!("pub const CAT_WORK_FLAG: u8 = {CAT_WORK_FLAG};")));
        assert!(module.contains(&format!(
            "pub const CAT_WORK_TRANSITION_TERM: u8 = {CAT_WORK_TRANSITION_TERM};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_WORK_MODULE_SET: u8 = {CAT_WORK_MODULE_SET};"
        )));
        assert!(module.contains(&format!(
            "pub const CAT_WORK_MODULE_INC_INT: u8 = {CAT_WORK_MODULE_INC_INT};"
        )));
        assert!(module.contains("pub clr_speed_kinetic_kind: Option<i64>"));
        assert!(module.contains("pub change_kinetic_type: Option<i64>"));
        assert!(module.contains("pub kinetic_energy_id: Option<i64>"));
        assert!(module.contains("pub kinetic_ground_friction: Option<bool>"));
        assert!(module.contains("pub kinetic_ground_friction_energy: Option<i64>"));
        assert!(module.contains("pub work_flag: Option<i64>"));
        assert!(module.contains("pub work_transition_term: Option<i64>"));
        assert!(module.contains("pub work_module_inc_int_slot: Option<i64>"));
        assert!(module.contains("pub work_module_set_int_value: Option<i64>"));
        assert!(module.contains("pub work_module_set_int64_value: Option<i64>"));
        assert!(module.contains("pub work_module_set_float_value: Option<f32>"));
        assert!(module.contains("pub work_module_set_slot: Option<i64>"));
        assert!(module.contains("pub motion_module_rate: Option<f32>"));
        assert!(module.contains("pub motion_module_helper_calculation: Option<bool>"));
        assert!(module.contains("pub motion_module_rate_partial: Option<f32>"));
        assert!(module.contains("replace = smash::app::sv_kinetic_energy::clear_speed"));
        assert!(module.contains("replace = smash::app::sv_animcmd::SET_AIR"));
        assert!(module.contains("replace = smash::app::lua_bind::KineticModule::change_kinetic"));
        assert!(module.contains("replace = smash::app::lua_bind::KineticModule::add_speed"));
        assert!(module.contains("hook_kinetic_add_speed"));
        assert!(module.contains("hook_kinetic_suspend_energy"));
        assert!(module.contains("KineticModule::suspend_energy,"));
        assert!(module.contains("hook_kinetic_resume_energy"));
        assert!(module.contains("KineticModule::resume_energy,"));
        assert!(module.contains("hook_kinetic_enable_energy"));
        assert!(module.contains("KineticModule::enable_energy,"));
        assert!(module.contains("hook_kinetic_unable_energy"));
        assert!(module.contains("KineticModule::unable_energy,"));
        assert!(module.contains("replace = smash::app::lua_bind::KineticModule::clear_speed_all"));
        assert!(module.contains("hook_kinetic_clear_speed_all"));
        assert!(module.contains("smash::app::lua_bind::WorkModule::on_flag,"));
        assert!(module.contains("smash::app::lua_bind::WorkModule::off_flag,"));
        assert!(module.contains("hook_work_module_on_flag"));
        assert!(module.contains("hook_work_module_off_flag"));
        assert!(module.contains("smash::app::lua_bind::WorkModule::enable_transition_term,"));
        assert!(module.contains("smash::app::lua_bind::WorkModule::unable_transition_term,"));
        assert!(module.contains("hook_work_module_enable_transition_term"));
        assert!(module.contains("hook_work_module_unable_transition_term"));
        assert!(module.contains("smash::app::lua_bind::WorkModule::enable_transition_term_group,"));
        assert!(
            module.contains("smash::app::lua_bind::WorkModule::unable_transition_term_group_ex,")
        );
        assert!(module.contains("hook_work_module_enable_transition_term_group"));
        assert!(module.contains("hook_work_module_unable_transition_term_group_ex"));
        assert!(module.contains("replace = smash::app::lua_bind::WorkModule::inc_int"));
        assert!(module.contains("hook_work_module_inc_int"));
        assert!(module.contains("replace = smash::app::lua_bind::WorkModule::set_int)"));
        assert!(module.contains("replace = smash::app::lua_bind::WorkModule::set_float)"));
        assert!(module.contains("replace = smash::app::lua_bind::WorkModule::set_int64)"));
        assert!(module.contains("hook_work_module_set_int"));
        assert!(module.contains("hook_work_module_set_float"));
        assert!(module.contains("hook_work_module_set_int64"));
        assert!(rate_hooks.contains("replace = smash::app::lua_bind::MotionModule::set_rate"));
        assert!(rate_hooks.contains("hook_motion_module_set_rate"));
        assert!(rate_hooks
            .contains("replace = smash::app::lua_bind::MotionModule::set_helper_calculation"));
        assert!(rate_hooks.contains("hook_motion_module_set_helper_calculation"));
        assert!(
            rate_hooks.contains("replace = smash::app::lua_bind::MotionModule::set_rate_partial")
        );
        assert!(rate_hooks.contains("hook_motion_module_set_rate_partial"));
        for (category, other) in [
            (CAT_CLR_SPEED, CAT_FT_START_ADJUST_MOTION_FRAME),
            (CAT_SET_AIR, CAT_CLR_SPEED),
            (CAT_SET_AIR, CAT_SPEED),
            (CAT_SET_AIR, CAT_REVERSE_LR),
            (CAT_CHANGE_KINETIC, CAT_SET_AIR),
            (CAT_CHANGE_KINETIC, CAT_CLR_SPEED),
            (CAT_KINETIC_ADD_SPEED, CAT_CHANGE_KINETIC),
            (CAT_KINETIC_ADD_SPEED, CAT_SET_AIR),
            (CAT_KINETIC_ADD_SPEED, CAT_SPEED),
            (CAT_KINETIC_SUSPEND_ENERGY, CAT_KINETIC_ADD_SPEED),
            (CAT_KINETIC_RESUME_ENERGY, CAT_KINETIC_SUSPEND_ENERGY),
            (CAT_KINETIC_RESUME_ENERGY, CAT_CHANGE_KINETIC),
            (CAT_KINETIC_ENABLE_ENERGY, CAT_KINETIC_RESUME_ENERGY),
            (CAT_KINETIC_UNABLE_ENERGY, CAT_KINETIC_ENABLE_ENERGY),
            (CAT_KINETIC_CLEAR_SPEED_ALL, CAT_KINETIC_UNABLE_ENERGY),
            (
                CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION,
                CAT_KINETIC_CLEAR_SPEED_ALL,
            ),
            (
                CAT_MOTION_MODULE_SET_RATE,
                CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION,
            ),
            (
                CAT_MOTION_MODULE_SET_HELPER_CALCULATION,
                CAT_MOTION_MODULE_SET_RATE,
            ),
            (
                CAT_MOTION_MODULE_SET_RATE_PARTIAL,
                CAT_MOTION_MODULE_SET_HELPER_CALCULATION,
            ),
            (CAT_WORK_FLAG, CAT_MOTION_MODULE_SET_RATE_PARTIAL),
            (CAT_WORK_TRANSITION_TERM, CAT_WORK_FLAG),
            (CAT_WORK_MODULE_INC_INT, CAT_WORK_TRANSITION_TERM),
            (CAT_WORK_MODULE_SET, CAT_WORK_MODULE_INC_INT),
        ] {
            assert_ne!(category, other, "kinetic category collision");
        }
        assert!(module.contains("const KINETIC_KEY_SET_AIR: u64 = u64::MAX - 2;"));
        assert!(module.contains("const KINETIC_KEY_CLEAR_SPEED_ALL: u64 = u64::MAX - 3;"));

        let link = GameLink::default();
        let clr_key = numeric_point_key("CLR_SPEED", &[7.0]);
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_CLR_SPEED,
                hitbox_id: Some(clr_key),
                suppress: false,
                frame_start: Some(3.5),
                frame_end: Some(4.5),
                overrides: Some(HbOverridesWire {
                    clr_speed_kinetic_kind: Some(9),
                    ..Default::default()
                }),
                inject: None,
                func: Some("CLR_SPEED".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_SET_AIR,
                hitbox_id: Some(KINETIC_KEY_SET_AIR),
                suppress: true,
                frame_start: Some(7.5),
                frame_end: Some(8.5),
                overrides: None,
                inject: None,
                func: Some("SET_AIR".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_CHANGE_KINETIC,
                hitbox_id: Some(numeric_point_key("KineticModule::change_kinetic", &[4.0])),
                suppress: false,
                frame_start: Some(9.5),
                frame_end: Some(10.5),
                overrides: Some(HbOverridesWire {
                    change_kinetic_type: Some(5),
                    ..Default::default()
                }),
                inject: None,
                func: Some("KineticModule::change_kinetic".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_ADD_SPEED,
                hitbox_id: Some(numeric_point_key(
                    "KineticModule::add_speed",
                    &[0.5, -1.25, 0.0],
                )),
                suppress: false,
                frame_start: Some(11.5),
                frame_end: Some(11.5),
                overrides: Some(HbOverridesWire {
                    speed_x: Some(1.5),
                    speed_y: Some(2.25),
                    ..Default::default()
                }),
                inject: None,
                func: Some("KineticModule::add_speed".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_WORK_FLAG,
                hitbox_id: Some(numeric_point_key("WorkModule::on_flag", &[7.0])),
                suppress: false,
                frame_start: Some(13.5),
                frame_end: Some(13.5),
                overrides: Some(HbOverridesWire {
                    work_flag: Some(9),
                    ..Default::default()
                }),
                inject: None,
                func: Some("WorkModule::on_flag".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(rules[0]["category"].as_u64(), Some(CAT_CLR_SPEED as u64));
        assert_eq!(rules[0]["hitbox_id"].as_u64(), Some(clr_key));
        assert_eq!(rules[0]["func"].as_str(), Some("CLR_SPEED"));
        assert_eq!(
            rules[0]["overrides"]["clr_speed_kinetic_kind"].as_i64(),
            Some(9)
        );
        assert_eq!(rules[1]["category"].as_u64(), Some(CAT_SET_AIR as u64));
        assert_eq!(rules[1]["hitbox_id"].as_u64(), Some(KINETIC_KEY_SET_AIR));
        assert_eq!(rules[1]["func"].as_str(), Some("SET_AIR"));
        assert_eq!(
            rules[2]["category"].as_u64(),
            Some(CAT_CHANGE_KINETIC as u64)
        );
        assert_eq!(
            rules[2]["overrides"]["change_kinetic_type"].as_i64(),
            Some(5)
        );
        assert_eq!(
            rules[2]["func"].as_str(),
            Some("KineticModule::change_kinetic")
        );
        assert_eq!(
            rules[3]["category"].as_u64(),
            Some(CAT_KINETIC_ADD_SPEED as u64)
        );
        assert_eq!(rules[3]["func"].as_str(), Some("KineticModule::add_speed"));
        assert_eq!(rules[3]["overrides"]["speed_x"].as_f64(), Some(1.5));
        assert_eq!(rules[3]["overrides"]["speed_y"].as_f64(), Some(2.25));
        assert_eq!(rules[4]["category"].as_u64(), Some(CAT_WORK_FLAG as u64));
        assert_eq!(
            rules[4]["hitbox_id"].as_u64(),
            Some(numeric_point_key("WorkModule::on_flag", &[7.0]))
        );
        assert_eq!(rules[4]["func"].as_str(), Some("WorkModule::on_flag"));
        assert_eq!(rules[4]["overrides"]["work_flag"].as_i64(), Some(9));
    }

    #[test]
    fn outbound_work_transition_term_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let key = numeric_point_key("WorkModule::enable_transition_term", &[7.0]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_WORK_TRANSITION_TERM,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(13.5),
            frame_end: Some(13.5),
            overrides: Some(HbOverridesWire {
                work_transition_term: Some(9),
                ..Default::default()
            }),
            inject: None,
            func: Some("WorkModule::enable_transition_term".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_WORK_TRANSITION_TERM as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(
            rule["func"].as_str(),
            Some("WorkModule::enable_transition_term")
        );
        assert_eq!(rule["overrides"]["work_transition_term"].as_i64(), Some(9));
    }

    #[test]
    fn outbound_work_module_set_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let key = numeric_point_key("WorkModule::set_int", &[1.0, 7.0]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_WORK_MODULE_SET,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(13.5),
            frame_end: Some(13.5),
            overrides: Some(HbOverridesWire {
                work_module_set_int_value: Some(3),
                work_module_set_slot: Some(9),
                ..Default::default()
            }),
            inject: None,
            func: Some("WorkModule::set_int".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_WORK_MODULE_SET as u64));
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(rule["func"].as_str(), Some("WorkModule::set_int"));
        assert_eq!(
            rule["overrides"]["work_module_set_int_value"].as_i64(),
            Some(3)
        );
        assert_eq!(rule["overrides"]["work_module_set_slot"].as_i64(), Some(9));
    }

    #[test]
    fn outbound_work_module_set_int64_rules_preserve_exact_integer_wire_fields() {
        let link = GameLink::default();
        let value = 0x1000_0000_0001;
        let slot = 7;
        let key = integer_point_key("WorkModule::set_int64", &[value, slot]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_WORK_MODULE_SET,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(13.5),
            frame_end: Some(13.5),
            overrides: Some(HbOverridesWire {
                work_module_set_int64_value: Some(0x1000_0000_0003),
                work_module_set_slot: Some(9),
                ..Default::default()
            }),
            inject: None,
            func: Some("WorkModule::set_int64".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value_json: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value_json["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_WORK_MODULE_SET as u64));
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(rule["func"].as_str(), Some("WorkModule::set_int64"));
        assert_eq!(
            rule["overrides"]["work_module_set_int64_value"].as_i64(),
            Some(0x1000_0000_0003)
        );
        assert_eq!(rule["overrides"]["work_module_set_slot"].as_i64(), Some(9));
    }

    #[test]
    fn outbound_work_module_inc_int_rules_preserve_exact_integer_wire_fields() {
        let link = GameLink::default();
        let slot = 7;
        let key = integer_point_key("WorkModule::inc_int", &[slot]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_WORK_MODULE_INC_INT,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(13.5),
            frame_end: Some(13.5),
            overrides: Some(HbOverridesWire {
                work_module_inc_int_slot: Some(9),
                ..Default::default()
            }),
            inject: None,
            func: Some("WorkModule::inc_int".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_WORK_MODULE_INC_INT as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(rule["func"].as_str(), Some("WorkModule::inc_int"));
        assert_eq!(
            rule["overrides"]["work_module_inc_int_slot"].as_i64(),
            Some(9)
        );
    }

    #[test]
    fn outbound_motion_module_set_rate_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let pristine_rate = 0.8;
        let key = numeric_point_key("MotionModule::set_rate", &[pristine_rate]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_MOTION_MODULE_SET_RATE,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(12.5),
            frame_end: Some(12.5),
            overrides: Some(HbOverridesWire {
                motion_module_rate: Some(1.25),
                ..Default::default()
            }),
            inject: None,
            func: Some("MotionModule::set_rate".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_MOTION_MODULE_SET_RATE as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(rule["func"].as_str(), Some("MotionModule::set_rate"));
        assert_eq!(rule["overrides"]["motion_module_rate"].as_f64(), Some(1.25));
    }

    #[test]
    fn outbound_motion_module_set_helper_calculation_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let pristine_enabled = false;
        let key = numeric_point_key(
            "MotionModule::set_helper_calculation",
            &[if pristine_enabled { 1.0 } else { 0.0 }],
        );
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_MOTION_MODULE_SET_HELPER_CALCULATION,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(12.5),
            frame_end: Some(12.5),
            overrides: Some(HbOverridesWire {
                motion_module_helper_calculation: Some(true),
                ..Default::default()
            }),
            inject: None,
            func: Some("MotionModule::set_helper_calculation".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_MOTION_MODULE_SET_HELPER_CALCULATION as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(
            rule["func"].as_str(),
            Some("MotionModule::set_helper_calculation")
        );
        assert_eq!(
            rule["overrides"]["motion_module_helper_calculation"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn outbound_motion_module_set_rate_partial_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let pristine_part = 2.0;
        let pristine_rate = 0.8;
        let key = numeric_point_key(
            "MotionModule::set_rate_partial",
            &[pristine_part, pristine_rate],
        );
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_MOTION_MODULE_SET_RATE_PARTIAL,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(12.5),
            frame_end: Some(12.5),
            overrides: Some(HbOverridesWire {
                motion_module_rate_partial: Some(1.25),
                ..Default::default()
            }),
            inject: None,
            func: Some("MotionModule::set_rate_partial".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(
            rule["category"].as_u64(),
            Some(CAT_MOTION_MODULE_SET_RATE_PARTIAL as u64)
        );
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(
            rule["func"].as_str(),
            Some("MotionModule::set_rate_partial")
        );
        assert_eq!(
            rule["overrides"]["motion_module_rate_partial"].as_f64(),
            Some(1.25)
        );
    }

    #[test]
    fn outbound_kinetic_energy_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_SUSPEND_ENERGY,
                hitbox_id: Some(numeric_point_key("KineticModule::suspend_energy", &[2.0])),
                suppress: false,
                frame_start: Some(4.5),
                frame_end: Some(4.5),
                overrides: Some(HbOverridesWire {
                    kinetic_energy_id: Some(3),
                    ..Default::default()
                }),
                inject: None,
                func: Some("KineticModule::suspend_energy".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_RESUME_ENERGY,
                hitbox_id: Some(numeric_point_key("KineticModule::resume_energy", &[2.0])),
                suppress: true,
                frame_start: Some(9.5),
                frame_end: Some(9.5),
                overrides: None,
                inject: None,
                func: Some("KineticModule::resume_energy".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(
            rules[0]["category"].as_u64(),
            Some(CAT_KINETIC_SUSPEND_ENERGY as u64)
        );
        assert_eq!(
            rules[0]["func"].as_str(),
            Some("KineticModule::suspend_energy")
        );
        assert_eq!(rules[0]["overrides"]["kinetic_energy_id"].as_i64(), Some(3));
        assert_eq!(
            rules[1]["category"].as_u64(),
            Some(CAT_KINETIC_RESUME_ENERGY as u64)
        );
        assert_eq!(
            rules[1]["func"].as_str(),
            Some("KineticModule::resume_energy")
        );
        assert_eq!(rules[1]["suppress"].as_bool(), Some(true));
    }

    #[test]
    fn outbound_enable_unable_energy_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_ENABLE_ENERGY,
                hitbox_id: Some(numeric_point_key("KineticModule::enable_energy", &[3.0])),
                suppress: false,
                frame_start: Some(4.5),
                frame_end: Some(4.5),
                overrides: Some(HbOverridesWire {
                    kinetic_energy_id: Some(4),
                    ..Default::default()
                }),
                inject: None,
                func: Some("KineticModule::enable_energy".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_UNABLE_ENERGY,
                hitbox_id: Some(numeric_point_key("KineticModule::unable_energy", &[5.0])),
                suppress: true,
                frame_start: Some(9.5),
                frame_end: Some(9.5),
                overrides: None,
                inject: None,
                func: Some("KineticModule::unable_energy".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(
            rules[0]["category"].as_u64(),
            Some(CAT_KINETIC_ENABLE_ENERGY as u64)
        );
        assert_eq!(
            rules[0]["func"].as_str(),
            Some("KineticModule::enable_energy")
        );
        assert_eq!(rules[0]["overrides"]["kinetic_energy_id"].as_i64(), Some(4));
        assert_eq!(
            rules[1]["category"].as_u64(),
            Some(CAT_KINETIC_UNABLE_ENERGY as u64)
        );
        assert_eq!(
            rules[1]["func"].as_str(),
            Some("KineticModule::unable_energy")
        );
        assert_eq!(rules[1]["suppress"].as_bool(), Some(true));
    }

    #[test]
    fn outbound_kinetic_clear_speed_all_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_CLEAR_SPEED_ALL,
                hitbox_id: Some(KINETIC_KEY_CLEAR_SPEED_ALL),
                suppress: true,
                frame_start: Some(4.0),
                frame_end: Some(4.0),
                overrides: None,
                inject: None,
                func: Some("KineticModule::clear_speed_all".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_CLEAR_SPEED_ALL,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(InjectRuleWire {
                    frame: 8.0,
                    args: Vec::new(),
                    command: Some("KineticModule::clear_speed_all".into()),
                }),
                func: Some("KineticModule::clear_speed_all".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(
            rules[0]["category"].as_u64(),
            Some(CAT_KINETIC_CLEAR_SPEED_ALL as u64)
        );
        assert_eq!(
            rules[0]["hitbox_id"].as_u64(),
            Some(KINETIC_KEY_CLEAR_SPEED_ALL)
        );
        assert_eq!(
            rules[0]["func"].as_str(),
            Some("KineticModule::clear_speed_all")
        );
        assert_eq!(rules[0]["suppress"].as_bool(), Some(true));
        assert_eq!(
            rules[1]["inject"]["command"].as_str(),
            Some("KineticModule::clear_speed_all")
        );
        assert!(rules[1]["inject"]["args"].as_array().unwrap().is_empty());
    }

    #[test]
    fn outbound_ground_friction_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let key = numeric_point_key("KineticModule::set_consider_ground_friction", &[0.0, 7.0]);
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION,
                hitbox_id: Some(key),
                suppress: false,
                frame_start: Some(4.0),
                frame_end: Some(4.0),
                overrides: Some(HbOverridesWire {
                    kinetic_ground_friction: Some(true),
                    kinetic_ground_friction_energy: Some(9),
                    ..Default::default()
                }),
                inject: None,
                func: Some("KineticModule::set_consider_ground_friction".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(InjectRuleWire {
                    frame: 6.0,
                    args: vec![LuaArgWire::Bool(false), LuaArgWire::Int(5)],
                    command: Some("KineticModule::set_consider_ground_friction".into()),
                }),
                func: Some("KineticModule::set_consider_ground_friction".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(
            rules[0]["category"].as_u64(),
            Some(CAT_KINETIC_SET_CONSIDER_GROUND_FRICTION as u64)
        );
        assert_eq!(rules[0]["hitbox_id"].as_u64(), Some(key));
        assert_eq!(
            rules[0]["func"].as_str(),
            Some("KineticModule::set_consider_ground_friction")
        );
        assert_eq!(
            rules[0]["overrides"]["kinetic_ground_friction"].as_bool(),
            Some(true)
        );
        assert_eq!(
            rules[0]["overrides"]["kinetic_ground_friction_energy"].as_i64(),
            Some(9)
        );
        assert_eq!(rules[1]["inject"]["frame"].as_f64(), Some(6.0));
        assert_eq!(
            rules[1]["inject"]["command"].as_str(),
            Some("KineticModule::set_consider_ground_friction")
        );
        assert_eq!(rules[1]["inject"]["args"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn outbound_ft_catch_stop_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        let key = numeric_point_key("FT_CATCH_STOP", &[6.0, 1.0]);
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_FT_CATCH_STOP,
            hitbox_id: Some(key),
            suppress: false,
            frame_start: Some(6.0),
            frame_end: Some(6.0),
            overrides: Some(HbOverridesWire {
                ft_catch_stop_arg1: Some(7.5),
                ft_catch_stop_arg2: Some(0.25),
                ..Default::default()
            }),
            inject: None,
            func: Some("FT_CATCH_STOP".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_FT_CATCH_STOP as u64));
        assert_eq!(rule["hitbox_id"].as_u64(), Some(key));
        assert_eq!(rule["func"].as_str(), Some("FT_CATCH_STOP"));
        assert_eq!(rule["overrides"]["ft_catch_stop_arg1"].as_f64(), Some(7.5));
        assert_eq!(rule["overrides"]["ft_catch_stop_arg2"].as_f64(), Some(0.25));
    }

    #[test]
    fn outbound_set_speed_ex_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_SPEED_EX,
            hitbox_id: Some(7),
            suppress: false,
            frame_start: Some(4.5),
            frame_end: Some(5.5),
            overrides: Some(HbOverridesWire {
                speed_x: Some(1.5),
                speed_y: Some(-3.8),
                ..Default::default()
            }),
            inject: None,
            func: Some("SET_SPEED_EX".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_SPEED_EX as u64));
        assert_eq!(rule["hitbox_id"].as_u64(), Some(7));
        assert_eq!(rule["func"].as_str(), Some("SET_SPEED_EX"));
        assert_eq!(rule["overrides"]["speed_x"].as_f64(), Some(1.5));
        assert!((rule["overrides"]["speed_y"].as_f64().unwrap() + 3.8).abs() < 1e-6);
    }

    #[test]
    fn outbound_set_speed_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_SPEED,
            hitbox_id: None,
            suppress: false,
            frame_start: Some(4.5),
            frame_end: Some(4.5),
            overrides: Some(HbOverridesWire {
                speed_x: Some(1.5),
                speed_y: Some(-3.8),
                ..Default::default()
            }),
            inject: None,
            func: Some("SET_SPEED".into()),
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rule = &value["hitbox_rules"][0];
        assert_eq!(rule["category"].as_u64(), Some(CAT_SPEED as u64));
        assert_eq!(rule["hitbox_id"], serde_json::Value::Null);
        assert_eq!(rule["func"].as_str(), Some("SET_SPEED"));
        assert_eq!(rule["overrides"]["speed_x"].as_f64(), Some(1.5));
        assert!((rule["overrides"]["speed_y"].as_f64().unwrap() + 3.8).abs() < 1e-6);
    }

    #[test]
    fn outbound_speed_addition_and_correction_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_ADD_SPEED_NO_LIMIT,
                hitbox_id: None,
                suppress: false,
                frame_start: Some(4.0),
                frame_end: Some(4.0),
                overrides: Some(HbOverridesWire {
                    speed_x: Some(1.5),
                    speed_y: Some(-3.8),
                    ..Default::default()
                }),
                inject: None,
                func: Some("ADD_SPEED_NO_LIMIT".into()),
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_CORRECT,
                hitbox_id: Some(1),
                suppress: false,
                frame_start: Some(4.0),
                frame_end: Some(4.0),
                overrides: Some(HbOverridesWire {
                    correct_kind: Some(2),
                    ..Default::default()
                }),
                inject: None,
                func: Some("CORRECT".into()),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(
            rules[0]["category"].as_u64(),
            Some(CAT_ADD_SPEED_NO_LIMIT as u64)
        );
        assert_eq!(rules[0]["overrides"]["speed_x"].as_f64(), Some(1.5));
        assert_eq!(rules[0]["func"].as_str(), Some("ADD_SPEED_NO_LIMIT"));
        assert_eq!(rules[1]["category"].as_u64(), Some(CAT_CORRECT as u64));
        assert_eq!(rules[1]["hitbox_id"].as_u64(), Some(1));
        assert_eq!(rules[1]["overrides"]["correct_kind"].as_i64(), Some(2));
        assert_eq!(rules[1]["func"].as_str(), Some("CORRECT"));
    }

    /// Suppression and zero-argument injection use the same wire lane. The command discriminator
    /// is explicit on injection so a future point-event category cannot accidentally fire the
    /// wrong no-argument animcmd primitive.
    #[test]
    fn outbound_reverse_lr_rules_match_plugin_wire_fields() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_REVERSE_LR,
                hitbox_id: Some(0),
                suppress: true,
                frame_start: Some(1.5),
                frame_end: Some(2.5),
                overrides: None,
                inject: None,
                func: None,
            },
            HitboxRuleWire {
                motion: 0x99,
                category: CAT_REVERSE_LR,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(InjectRuleWire {
                    frame: 3.0,
                    args: Vec::new(),
                    command: Some("REVERSE_LR".into()),
                }),
                func: None,
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let value: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = value["hitbox_rules"].as_array().unwrap();
        assert_eq!(rules[0]["category"].as_u64(), Some(CAT_REVERSE_LR as u64));
        assert_eq!(rules[0]["hitbox_id"].as_u64(), Some(0));
        assert_eq!(rules[0]["suppress"].as_bool(), Some(true));
        assert_eq!(rules[1]["category"].as_u64(), Some(CAT_REVERSE_LR as u64));
        assert_eq!(rules[1]["inject"]["frame"].as_f64(), Some(3.0));
        assert_eq!(rules[1]["inject"]["args"].as_array().unwrap().len(), 0);
        assert_eq!(rules[1]["inject"]["command"].as_str(), Some("REVERSE_LR"));
    }

    /// A rule for any other family carries no `func`, and the field is omitted entirely.
    ///
    /// The plugin reads an absent `func` as "match any member of this category", which is only
    /// safe because no other family needs the discrimination. If some other rule started
    /// emitting `func: null` or an empty string, an old plugin build would still ignore it but a
    /// current one would compare against a name no macro has and match nothing — a live edit
    /// that silently does nothing, which is the failure B5b cost two tasks to find.
    #[test]
    fn non_sound_and_non_expression_rules_omit_a_macro_name() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: CAT_ATK_POWER,
            hitbox_id: Some(0),
            suppress: false,
            frame_start: None,
            frame_end: None,
            overrides: Some(HbOverridesWire {
                atk_mod_value: Some(12.0),
                ..Default::default()
            }),
            inject: None,
            func: None,
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        assert!(v["hitbox_rules"][0].get("func").is_none());
        assert!(v["hitbox_rules"][0]["overrides"]
            .get("sound_hashes")
            .is_none());
    }

    /// A sound hash arrives from the game typed as `Int`, and every reader has to accept it.
    ///
    /// **Measured live 2026-08-06.** Kirby's up tilt passes `se_kirby_swing_l` as
    /// `L2CValueType::Int` holding `0x10556b83cc`, which is exactly `hash40("se_kirby_swing_l")`
    /// — the right value under the wrong type tag, because on the lua stack a hash and an int
    /// are both just numbers.
    ///
    /// This asymmetry is what made the bug so hard to see: `as_hash` here already accepted
    /// `Int`, so **live capture of sounds worked**, while the plugin's rule matcher required
    /// `LuaArg::Hash` and rejected every sound in the game. Two readers of one stream with
    /// different tolerance — the feature half-worked, which reads as "nearly right" rather than
    /// "wrong in one place".
    #[test]
    fn a_sound_hash_is_read_whether_it_arrives_as_a_hash_or_an_int() {
        let value = 0x10_556b_83ccu64;
        assert_eq!(LuaArgWire::Hash(value).as_hash(), Some(value));
        assert_eq!(LuaArgWire::Int(value as i64).as_hash(), Some(value));
        // And the plugin's side of the same stream agrees. Source text, because that crate is
        // not in this workspace — see `the_plugin_sound_table_still_matches_the_editors`.
        let source = plugin_sound_hooks_source();
        let body = source
            .split_once("fn hash_arg(")
            .expect("plugin still has hash_arg")
            .1;
        let body = &body[..body.find("\n}").unwrap_or(body.len())];
        assert!(
            body.contains("LuaArg::Int"),
            "the plugin's key reader must accept an Int-tagged hash, or live sound edits go \
             silently dead while live sound capture keeps working: {body}"
        );
    }
}
