/// All data types for Visionary's editor state.
use std::collections::HashMap;
use std::path::PathBuf;

/// The `ATTACK`-family macro assumed when nothing says otherwise — the overwhelming majority
/// of collisions, and what every project written before [`Hitbox::func`] existed contains.
pub fn default_attack_func() -> String {
    "ATTACK".to_string()
}

/// One `ATTACK_FP` argument kept losslessly across source parsing, project export, and live
/// capture. Source-backed values retain their original token so symbolic constants and unusual
/// expressions are not rewritten merely by opening and exporting a project; captured values
/// carry the type the game actually pushed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AttackFpArg {
    /// A Rust source token from a user's or the mirror's ACMD file.
    Source(String),
    Hash(u64),
    Num(f32),
    Int(i64),
    Bool(bool),
    Nil,
}

impl AttackFpArg {
    pub fn source(&self) -> String {
        match self {
            Self::Source(value) => value.clone(),
            Self::Hash(value) => format!("Hash40::new_raw({value:#x})"),
            Self::Num(value) => crate::acmd::num(*value),
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            // The wrapper declares every FP slot as a concrete value, so Nil is not a valid
            // source spelling. A captured Void is retained in the IR for fidelity, and zero is
            // the only compiling scalar fallback if an authoring tool nevertheless supplies it.
            Self::Nil => "0".to_string(),
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Num(value) => Some(*value),
            Self::Int(value) => Some(*value as f32),
            Self::Source(value) => value.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Num(value) => Some(*value as i64),
            Self::Bool(value) => Some(*value as i64),
            Self::Source(value) => value.trim().trim_start_matches('*').parse().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::Int(value) => Some(*value != 0),
            Self::Num(value) => Some(*value != 0.0),
            Self::Source(value) => value.trim().parse().ok(),
            _ => None,
        }
    }

    /// Return the string inside `Hash40::new("…")`, or the source token for a raw hash/value.
    /// This is intentionally not a reverse hash40 lookup: a captured hash has no recoverable
    /// author-facing name.
    pub fn hash_text(&self) -> Option<String> {
        match self {
            Self::Source(value) => {
                let value = value.trim();
                value
                    .strip_prefix("Hash40::new(\"")
                    .and_then(|value| value.strip_suffix("\")"))
                    .map(str::to_string)
                    .or_else(|| {
                        value
                            .strip_prefix("Hash40::new_raw(")
                            .and_then(|value| value.strip_suffix(')'))
                            .map(str::to_string)
                    })
                    .or_else(|| Some(value.trim_start_matches('*').to_string()))
            }
            Self::Hash(value) => Some(format!("{value:#x}")),
            _ => None,
        }
    }
}

/// A single hitbox — used for display, timeline, and viewport rendering.
/// `active_start`/`active_end` are computed from the script structure.
/// When `capsule_end` is `Some`, the hitbox is a capsule; otherwise a sphere.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hitbox {
    /// The `ATTACK`-family macro this row came from, and the one an export writes it back
    /// under. Only meaningful for category 0; grabs and wind areas leave it at the default.
    /// See [`AttackCall::func`].
    #[serde(default = "default_attack_func")]
    pub func: String,
    pub id: u32,
    pub part: u32,
    pub bone_name: String,
    pub damage: f32,
    pub angle: i32,
    pub kb_scaling: i32,
    pub fkb: i32,
    pub kb_base: i32,
    pub size: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_z: f32,
    /// Second endpoint for capsule hitboxes (None = sphere).
    pub capsule_end: Option<[f32; 3]>,
    // ── Hit properties ────────────────────────────────────────────────────
    pub hitlag_mult: f32,
    pub sdi_mult: f32,
    pub setoff_kind: String,
    pub lr_check: String,
    pub is_clang: bool,
    /// Extra attack flag (int, usually 0).
    pub is_add_attack: i32,
    /// Hitbox attribute float (usually 0.0).
    pub hitbox_attr: f32,
    /// Ground/air flag (int, usually 0).
    pub ground_or_air: i32,
    pub is_mtk: bool,
    pub is_shield_disable: bool,
    pub is_reflectable: bool,
    pub is_absorbable: bool,
    pub is_landing_attack: bool,
    // ── Collision masks ───────────────────────────────────────────────────
    pub situation_mask: String,
    pub category_mask: String,
    pub part_mask: String,
    pub no_finish_camera: bool,
    // ── Effect / sound ────────────────────────────────────────────────────
    pub collision_attr: String,
    pub sound_level: String,
    pub sound_attr: String,
    pub attack_region: String,
    // ── Timeline ─────────────────────────────────────────────────────────
    pub active_start: u32,
    pub active_end: u32,
    pub hitbox_type: u32,
    /// Collision family: 0 = attack, 1 = grab (CATCH), 2 = wind (AREA_WIND). Drives the
    /// preview color and which plugin family live edits target. Old projects omit it → 0.
    #[serde(default)]
    pub category: u8,
    /// Exact AREA_WIND payload for category 2. Wind areas are 2D rectangles/circles with a
    /// different parameter layout from ATTACK, so keeping the original command and every float
    /// is required for accurate rendering, live rewriting, retiming, and export.
    #[serde(default)]
    pub wind: Option<WindboxData>,
    /// The `CATCH` arguments category 1 has no editable field for. Keeping them is what lets
    /// a grab read from a script export with the author's own values rather than a
    /// substituted default. Deliberately NOT the whole call: every other argument is an
    /// editable property of the hitbox itself, and duplicating those would let the copy go
    /// stale the moment the user dragged one.
    #[serde(default)]
    pub catch: Option<CatchExtras>,
    /// The `ATTACK_ABS` arguments category 3 has no editable field for — see [`AbsExtras`].
    #[serde(default)]
    pub abs: Option<AbsExtras>,
    /// The `SEARCH` arguments category 4 has no editable field for — see [`SearchExtras`].
    #[serde(default)]
    pub search: Option<SearchExtras>,
    /// Complete `ATTACK_FP` payload. The family has a different 41-slot layout and several
    /// undocumented fields, so it is kept separately from the ordinary `ATTACK` fields. The
    /// editor hides FP geometry and only rewrites the slots whose meaning is established.
    #[serde(default)]
    pub fp: Option<AttackFpExtras>,
}

impl Default for Hitbox {
    fn default() -> Self {
        Self {
            func: default_attack_func(),
            id: 0,
            part: 0,
            bone_name: "top".to_string(),
            damage: 10.0,
            angle: 361,
            kb_scaling: 100,
            fkb: 0,
            kb_base: 50,
            size: 4.5,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_z: 0.0,
            capsule_end: None,
            hitlag_mult: 1.0,
            sdi_mult: 1.0,
            setoff_kind: "ATTACK_SETOFF_KIND_ON".to_string(),
            lr_check: "ATTACK_LR_CHECK_POS".to_string(),
            is_clang: false,
            is_add_attack: 0,
            hitbox_attr: 0.0,
            ground_or_air: 0,
            is_mtk: false,
            is_shield_disable: false,
            is_reflectable: false,
            is_absorbable: false,
            is_landing_attack: true,
            situation_mask: "COLLISION_SITUATION_MASK_GA".to_string(),
            category_mask: "COLLISION_CATEGORY_MASK_ALL".to_string(),
            part_mask: "COLLISION_PART_MASK_ALL".to_string(),
            no_finish_camera: false,
            collision_attr: "collision_attr_normal".to_string(),
            sound_level: "ATTACK_SOUND_LEVEL_M".to_string(),
            sound_attr: "COLLISION_SOUND_ATTR_PUNCH".to_string(),
            attack_region: "ATTACK_REGION_PUNCH".to_string(),
            active_start: 0,
            active_end: 9999,
            hitbox_type: 0,
            category: 0,
            wind: None,
            catch: None,
            abs: None,
            search: None,
            fp: None,
        }
    }
}

/// Lossless payload for one AREA_WIND_2ND family call.
///
/// All four commands share slots 0..7: id, four physics values, X/Y, then radius (radial) or
/// width (rectangle). Rectangle calls add height at slot 8. The `_arg9`/`_arg10` variants add
/// a final lifetime; the shorter variants leave the area alive until `erase_wind`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindboxData {
    pub command: String,
    pub args: Vec<f32>,
}

/// Every `AREA_WIND_2ND` command and the number of arguments it takes.
///
/// Longest name first, because these share a prefix: a scan for `AREA_WIND_2ND` matches an
/// `AREA_WIND_2ND_arg10` call too, and would claim it.
///
/// Unlike `ATTACK`, a wind argument is always a bare float, so there is no argument *shape* to
/// read a call's layout from. The command name **is** the layout, and the arity is part of the
/// name — which is why a call whose length disagrees with its name is refused rather than
/// reinterpreted.
pub const WIND_COMMANDS: [(&str, usize); 4] = [
    ("AREA_WIND_2ND_RAD_arg9", 9),
    ("AREA_WIND_2ND_arg10", 10),
    ("AREA_WIND_2ND_RAD", 8),
    ("AREA_WIND_2ND", 9),
];

/// The wind commands `smash_script::macros` actually declares.
///
/// `sv_animcmd` has all four and the plugin hooks all four, but smash-script never wrapped the
/// plain rectangular `AREA_WIND_2ND` — so `macros::AREA_WIND_2ND(..)` names a function that does
/// not exist, and an export emitting it produces a project that does not build. Parsing still
/// accepts the command; only writing it out is refused.
pub const WIND_MACRO_COMMANDS: [&str; 3] = [
    "AREA_WIND_2ND_RAD",
    "AREA_WIND_2ND_RAD_arg9",
    "AREA_WIND_2ND_arg10",
];

pub fn is_wind_command(name: &str) -> bool {
    WIND_COMMANDS.iter().any(|(command, _)| *command == name)
}

impl WindboxData {
    pub fn expected_arity(&self) -> Option<usize> {
        WIND_COMMANDS
            .iter()
            .find(|(command, _)| *command == self.command)
            .map(|(_, arity)| *arity)
    }

    /// Whether this command can be written as a `macros::` call at all.
    pub fn has_macro_wrapper(&self) -> bool {
        WIND_MACRO_COMMANDS.contains(&self.command.as_str())
    }

    pub fn is_valid(&self) -> bool {
        self.expected_arity() == Some(self.args.len())
    }

    pub fn is_radial(&self) -> bool {
        self.command.contains("_RAD")
    }

    pub fn id(&self) -> u32 {
        self.args.first().copied().unwrap_or(0.0).max(0.0) as u32
    }

    pub fn has_lifetime(&self) -> bool {
        (self.is_radial() && self.args.len() >= 9) || (!self.is_radial() && self.args.len() >= 10)
    }

    pub fn lifetime(&self) -> Option<u32> {
        self.has_lifetime()
            .then(|| self.args.last().copied().unwrap_or(0.0).max(0.0) as u32)
    }

    /// The last frame this area is up, given the frame it came out on.
    ///
    /// [`u32::MAX`] when the command carries no lifetime slot, or carries a zero one: the area
    /// then lives until an `AreaModule::erase_wind`. This is the *only* place that derivation
    /// lives, so the panel, the timeline, and source syncing cannot drift apart on it.
    pub fn end_frame(&self, active_start: u32) -> u32 {
        self.lifetime()
            .filter(|life| *life > 0)
            .map(|life| active_start.saturating_add(life).saturating_sub(1))
            .unwrap_or(u32::MAX)
    }

    pub fn offset(&self) -> [f32; 2] {
        [
            self.args.get(5).copied().unwrap_or(0.0),
            self.args.get(6).copied().unwrap_or(0.0),
        ]
    }

    pub fn radius(&self) -> f32 {
        self.args.get(7).copied().unwrap_or(0.0).abs()
    }

    pub fn dimensions(&self) -> [f32; 2] {
        [
            self.args.get(7).copied().unwrap_or(0.0).abs(),
            self.args.get(8).copied().unwrap_or(0.0).abs(),
        ]
    }

    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        let [x, y] = self.offset();
        let size = if self.is_radial() {
            self.radius()
        } else {
            let [width, height] = self.dimensions();
            width.max(height) * 0.5
        };
        let active_end = self.end_frame(active_start);
        Hitbox {
            id: self.id(),
            bone_name: "top".into(),
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            size,
            offset_x: x,
            offset_y: y,
            offset_z: 0.0,
            active_start,
            active_end,
            category: 2,
            wind: Some(self.clone()),
            catch: None,
            ..Default::default()
        }
    }
}

impl Hitbox {
    /// Back-convert to a CATCH call.
    ///
    /// The status and situation come from the originating call when there was one. A grab
    /// that reached the editor from a live capture has neither — the plugin appends these
    /// same two constants when it injects a grab with no donor, so an exported mod grabs the
    /// way the live preview did.
    pub fn to_catch_call(&self) -> CatchCall {
        let base = self.catch.clone();
        CatchCall {
            id: self.id,
            bone_name: self.bone_name.clone(),
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            status: base
                .as_ref()
                .map(|c| c.status.clone())
                .unwrap_or_else(|| CATCH_DEFAULT_STATUS.to_string()),
            situation: base
                .map(|c| c.situation)
                .unwrap_or_else(|| CATCH_DEFAULT_SITUATION.to_string()),
        }
    }

    /// Back-convert to a SEARCH call.
    ///
    /// The four arguments with no panel control come from the originating call. A detection box
    /// that reached the editor from a live capture has none, so it falls back to the shape the
    /// corpus writes most often — look for something, in any hurtbox state, with the trailing
    /// flag clear.
    pub fn to_search_call(&self) -> SearchCall {
        let extras = self.search.clone().unwrap_or(SearchExtras {
            collision_kind: SEARCH_DEFAULT_COLLISION_KIND.to_string(),
            hit_status: SEARCH_DEFAULT_HIT_STATUS.to_string(),
            unk: 0,
            unk2: false,
        });
        SearchCall {
            id: self.id,
            part: self.part,
            bone_name: self.bone_name.clone(),
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            situation_mask: self.situation_mask.clone(),
            category_mask: self.category_mask.clone(),
            part_mask: self.part_mask.clone(),
            extras,
        }
    }

    /// Back-convert to an ATTACK call (script synthesis for capture-sourced moves).
    pub fn to_attack_call(&self) -> AttackCall {
        AttackCall {
            func: self.func.clone(),
            id: self.id,
            part: self.part,
            bone_name: self.bone_name.clone(),
            damage: self.damage,
            angle: self.angle,
            kb_scaling: self.kb_scaling,
            fkb: self.fkb,
            kb_base: self.kb_base,
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            hitlag_mult: self.hitlag_mult,
            sdi_mult: self.sdi_mult,
            setoff_kind: self.setoff_kind.clone(),
            lr_check: self.lr_check.clone(),
            is_clang: self.is_clang,
            is_add_attack: self.is_add_attack,
            hitbox_attr: self.hitbox_attr,
            ground_or_air: self.ground_or_air,
            is_mtk: self.is_mtk,
            is_shield_disable: self.is_shield_disable,
            is_reflectable: self.is_reflectable,
            is_absorbable: self.is_absorbable,
            is_landing_attack: self.is_landing_attack,
            situation_mask: self.situation_mask.clone(),
            category_mask: self.category_mask.clone(),
            part_mask: self.part_mask.clone(),
            no_finish_camera: self.no_finish_camera,
            collision_attr: self.collision_attr.clone(),
            sound_level: self.sound_level.clone(),
            sound_attr: self.sound_attr.clone(),
            attack_region: self.attack_region.clone(),
        }
    }
}

// ── ACMD script IR ────────────────────────────────────────────────────────────

/// A fully-parsed ATTACK(...) call — every parameter is named.
/// This is the source of truth for export; nothing is lost.
/// The two `CATCH` arguments a [`Hitbox`] has no field for.
///
/// A grab's status kind and situation mask live on the originating call, not in editor state.
/// These are the constants the plugin substitutes when it injects a grab with no donor (see
/// `hitbox_viewer::inject`), so a grab that never came from a script still behaves on export
/// the way it did in the live preview.
pub const CATCH_DEFAULT_STATUS: &str = "FIGHTER_STATUS_KIND_CAPTURE_PULLED";
pub const CATCH_DEFAULT_SITUATION: &str = "COLLISION_SITUATION_MASK_GA";

/// Stand-ins for the two `SEARCH` mask arguments when the box did not come from a script.
///
/// The corpus splits 5/2 on the first and 4/3 on the second, so neither is a safe "the game
/// always writes this" — these are the majority, chosen so a captured box behaves on export
/// the way the common case does, and they are only ever used when there is no donor call.
pub const SEARCH_DEFAULT_COLLISION_KIND: &str = "COLLISION_KIND_MASK_ATTACK";
pub const SEARCH_DEFAULT_HIT_STATUS: &str = "HIT_STATUS_MASK_ALL";

/// The `AFTER_IMAGE_OFF` argument used when the editor ends a trail that no script closed.
///
/// A real choice, not a discovered fact: the four corpus calls split evenly between `0` and
/// `3`, and the argument is undocumented beyond `unk` in `macros.rs`, so there is no majority
/// to follow and no meaning to reason from. `0` is taken as the more literal reading of "the
/// trail stops here". A trail read from a script keeps its own value and never reaches this.
pub const TRAIL_OFF_DEFAULT: f32 = 0.0;

/// The `CATCH` arguments that are not editable properties of a grab box.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatchExtras {
    /// The status the grabbed fighter is put into, e.g. `FIGHTER_STATUS_KIND_CAPTURE_PULLED`.
    pub status: String,
    pub situation: String,
}

/// Collision family id for [`Hitbox::category`]. 0/1/2 are attack, grab and wind.
///
/// `ATTACK_ABS` is category 3: a hit with no volume at all. It applies to an opponent already
/// caught, so it has no bone, no size, and no offsets — the panel hides those rather than
/// showing zeroed controls that would look like a hitbox sitting at the origin.
pub const CAT_ABS: u8 = 3;

/// `SEARCH` is category 4: a detection volume that does not hit anything.
///
/// Geometrically it is the closest thing to a grab box in the game — bone, size, offsets and an
/// optional capsule end — but it deals no damage and causes no hitlag. It tells the script that
/// something is *inside* it, and what the fighter does about that lives in the status code, not
/// in the ACMD. So it draws like a collision and carries none of the attack fields.
pub const CAT_SEARCH: u8 = 4;

/// `ATTACK_FP` is a collision volume with its own slot table and fighter-position semantics.
/// Keeping it distinct from category 0 prevents ordinary ATTACK controls, live rules, and
/// viewport geometry from being applied to the wrong family.
pub const CAT_ATTACK_FP: u8 = 5;

pub fn is_attack_category(category: u8) -> bool {
    category == 0 || category == CAT_ATTACK_FP
}

/// The `SEARCH` arguments that have no counterpart field on a [`Hitbox`].
///
/// Scoped the way [`CatchExtras`] and [`AbsExtras`] are: id, part, bone, size, offsets and the
/// capsule map onto fields the panel already has, and so do all three of the trailing masks —
/// `ground_air` is [`Hitbox::situation_mask`], `collision_category` is
/// [`Hitbox::category_mask`], `collision_parts` is [`Hitbox::part_mask`]. Only the four with no
/// home live here.
///
/// Unlike `ATTACK_ABS`'s unknowns, none of these is invariant across the corpus, so substituting
/// a default for any of them would change what the box detects. The two that are named and
/// meaningful get panel controls; the two that are not are carried verbatim.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchExtras {
    /// What the volume looks for — `COLLISION_KIND_MASK_ATTACK` or `_HIT` in the corpus.
    ///
    /// Editable, against [`crate::param_labels::COLLISION_KIND_MASK`]. A `String` rather than a
    /// decoded value for the reason [`AbsExtras::kind`] is one: an unfamiliar mask is carried
    /// as written rather than snapped to the nearest known name.
    pub collision_kind: String,
    /// Which hurtbox states count as found — `HIT_STATUS_MASK_ALL` or `_NORMAL` in the corpus.
    ///
    /// Editable, against [`crate::param_labels::HIT_STATUS_MASK`] — which is a *different table*
    /// from `HIT_STATUS` and overlaps it numerically. See that table's note.
    pub hit_status: String,
    /// Slot 12, `unk` in `macros.rs`. Undocumented, and **not** invariant: the corpus writes
    /// 0, 1 and 60. Carried verbatim rather than exposed — a control whose meaning is a guess
    /// is worse than no control, and dropping it would change the call.
    pub unk: i64,
    /// The trailing bool, `unk2` in `macros.rs`. `false` in all 7 corpus calls, kept for the
    /// same reason.
    pub unk2: bool,
}

/// A parsed `macros::SEARCH` call — a detection box.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchCall {
    pub id: u32,
    pub part: u32,
    pub bone_name: String,
    pub size: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_z: f32,
    /// Capsule second endpoint — `Some([x,y,z])` or `None` for a spherical search.
    pub capsule_end: Option<[f32; 3]>,
    pub situation_mask: String,
    pub category_mask: String,
    pub part_mask: String,
    pub extras: SearchExtras,
}

impl SearchCall {
    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        Hitbox {
            id: self.id,
            part: self.part,
            bone_name: self.bone_name.clone(),
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            situation_mask: self.situation_mask.clone(),
            category_mask: self.category_mask.clone(),
            part_mask: self.part_mask.clone(),
            // A detection box deals nothing. Zeroing these matches what a grab box does and
            // keeps the attack defaults from showing up in a panel for a box that cannot hit.
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            active_start,
            active_end: u32::MAX,
            category: CAT_SEARCH,
            search: Some(self.extras.clone()),
            ..Default::default()
        }
    }
}

/// The `ATTACK_ABS` arguments that are not editable properties of a [`Hitbox`].
///
/// Everything else in the call — damage, angle, the knockback triple, hitlag, `lr_check`,
/// `collision_attr`, the two sound slots and `attack_region` — maps onto a field the hitbox
/// panel already has, so those are not duplicated here. Copying them would let the copy go
/// stale the moment the user dragged one, which is the reason [`CatchExtras`] is scoped the
/// same way.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AbsExtras {
    /// The absolute kind, e.g. `FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH` — what the hit applies to.
    ///
    /// A string rather than a decoded constant, because the slot takes *fighter-specific*
    /// names: the corpus has `FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL` beside the two common
    /// ones. A closed table would silently rewrite Terry's final smash into someone else's
    /// throw, so unknown names are carried exactly as written.
    pub kind: String,
    /// Slots 9, 11 and 12, in order. Undocumented beyond `unk`/`unk2`/`unk3` in `macros.rs`,
    /// and invariant at `1.0` / `0.0` / `true` across every one of the corpus's 32 calls.
    ///
    /// Carried verbatim rather than exposed. There is no evidence for what they do, and a
    /// control whose meaning is a guess is worse than no control — but dropping them would
    /// change the call, so they are kept.
    pub unknowns: (f32, f32, bool),
}

/// The portions of an `ATTACK_FP` call that do not have a safe editor control.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttackFpExtras {
    /// The 41 arguments after `agent`, in the order declared by `smash-script::ATTACK_FP`.
    pub args: Vec<AttackFpArg>,
}

pub const ATTACK_FP_ARGC: usize = 41;

/// A parsed `macros::ATTACK_FP` call. Its slot layout is intentionally independent from
/// [`AttackCall`], even where both calls happen to expose fields with the same meaning.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttackFpCall {
    pub args: Vec<AttackFpArg>,
}

impl AttackFpCall {
    fn arg(&self, slot: usize) -> Option<&AttackFpArg> {
        self.args.get(slot)
    }

    fn int(&self, slot: usize, default: i64) -> i64 {
        self.arg(slot)
            .and_then(AttackFpArg::as_i64)
            .unwrap_or(default)
    }

    fn const_int(&self, slot: usize, table: crate::param_labels::ConstTable, default: i64) -> i64 {
        self.arg(slot)
            .and_then(|arg| {
                arg.as_i64().or_else(|| match arg {
                    AttackFpArg::Source(source) => crate::param_labels::encode_const(table, source),
                    _ => None,
                })
            })
            .unwrap_or(default)
    }

    fn float(&self, slot: usize, default: f32) -> f32 {
        self.arg(slot)
            .and_then(AttackFpArg::as_f32)
            .unwrap_or(default)
    }

    fn flag(&self, slot: usize, default: bool) -> bool {
        self.arg(slot)
            .and_then(AttackFpArg::as_bool)
            .unwrap_or(default)
    }

    fn text(&self, slot: usize, default: &str) -> String {
        self.arg(slot)
            .map(AttackFpArg::source)
            .map(|value| value.trim_start_matches('*').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    fn hash(&self, slot: usize, default: &str) -> String {
        self.arg(slot)
            .and_then(AttackFpArg::hash_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// Convert to a display row. The geometry values remain in the row for lossless project
    /// edits, but the UI and viewport recognize `fp` and suppress those controls/drawing because
    /// this macro's fighter-position semantics are not interchangeable with a bone-local sphere.
    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        Hitbox {
            func: "ATTACK_FP".into(),
            id: self.int(0, 0).max(0) as u32,
            part: self.int(1, 0).max(0) as u32,
            bone_name: self
                .arg(2)
                .and_then(AttackFpArg::hash_text)
                .unwrap_or_default(),
            damage: self.float(3, 0.0),
            angle: self.int(4, 0) as i32,
            kb_scaling: self.int(5, 0) as i32,
            fkb: self.int(6, 0) as i32,
            kb_base: self.int(7, 0) as i32,
            size: self.float(8, 0.0),
            offset_x: self.float(9, 0.0),
            offset_y: self.float(10, 0.0),
            offset_z: self.float(11, 0.0),
            hitlag_mult: self.float(14, 1.0),
            sdi_mult: self.float(15, 1.0),
            is_clang: self.flag(16, false),
            // The FP wrapper calls this `ground_air`, but the real source corpus writes a
            // `COLLISION_SITUATION_MASK_*` constant here rather than a bare integer. Decode the
            // known constants without discarding the original source token held in `fp`.
            ground_or_air: self.const_int(21, crate::param_labels::SITUATION_MASK, 0) as i32,
            is_reflectable: self.flag(29, false),
            is_absorbable: self.flag(30, false),
            lr_check: self.text(34, "ATTACK_LR_CHECK_POS"),
            collision_attr: self.hash(12, "collision_attr_normal"),
            sound_level: self.text(19, "ATTACK_SOUND_LEVEL_M"),
            sound_attr: self.text(20, "COLLISION_SOUND_ATTR_PUNCH"),
            attack_region: self.text(23, "ATTACK_REGION_PUNCH"),
            active_start,
            active_end: u32::MAX,
            category: CAT_ATTACK_FP,
            fp: Some(AttackFpExtras {
                args: self.args.clone(),
            }),
            ..Default::default()
        }
    }
}

impl Hitbox {
    /// Rebuild an `ATTACK_FP` call, retaining every unmodelled slot and applying the editor's
    /// supported fields at this family's own indices.
    pub fn to_attack_fp_call(&self) -> Option<AttackFpCall> {
        let extras = self.fp.as_ref()?;
        let mut args = extras.args.clone();
        if args.len() != ATTACK_FP_ARGC {
            return None;
        }
        let source = |value: String| AttackFpArg::Source(value);
        args[0] = AttackFpArg::Int(self.id as i64);
        args[1] = AttackFpArg::Int(self.part as i64);
        args[3] = AttackFpArg::Num(self.damage);
        args[4] = AttackFpArg::Int(self.angle as i64);
        args[5] = AttackFpArg::Int(self.kb_scaling as i64);
        args[6] = AttackFpArg::Int(self.fkb as i64);
        args[7] = AttackFpArg::Int(self.kb_base as i64);
        args[12] = source(crate::acmd::hash40_expr_for_data(&self.collision_attr));
        args[14] = AttackFpArg::Num(self.hitlag_mult);
        args[15] = AttackFpArg::Num(self.sdi_mult);
        args[16] = AttackFpArg::Bool(self.is_clang);
        args[19] = source(crate::acmd::const_expr(&self.sound_level));
        args[20] = source(crate::acmd::const_expr(&self.sound_attr));
        // Keep a symbolic situation mask exactly as authored while the editor value is
        // unchanged. A source-backed `*COLLISION_SITUATION_MASK_G` is semantically `1`, and
        // rewriting it to `0` merely because `AttackFpArg::as_i64` cannot parse identifiers
        // changes the real call during a project export. A deliberate edit still writes the
        // new numeric value, which is the same representation used by the live wire.
        let original_ground_or_air = args[21].as_i64().or_else(|| match &args[21] {
            AttackFpArg::Source(source) => {
                crate::param_labels::encode_const(crate::param_labels::SITUATION_MASK, source)
            }
            _ => None,
        });
        if original_ground_or_air != Some(self.ground_or_air as i64) {
            args[21] = AttackFpArg::Int(self.ground_or_air as i64);
        }
        args[23] = source(crate::acmd::const_expr(&self.attack_region));
        args[29] = AttackFpArg::Bool(self.is_reflectable);
        args[30] = AttackFpArg::Bool(self.is_absorbable);
        args[34] = source(crate::acmd::const_expr(&self.lr_check));
        Some(AttackFpCall { args })
    }
}

/// A parsed `macros::CATCH` call — a grab box.
///
/// `status` and `situation` have no counterpart on [`Hitbox`], so they are kept here: a grab
/// read from a script exports with the author's own values rather than a substituted default.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatchCall {
    pub id: u32,
    pub bone_name: String,
    pub size: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_z: f32,
    /// Capsule second endpoint — `Some([x,y,z])` or `None` for a spherical grab.
    pub capsule_end: Option<[f32; 3]>,
    /// The status the grabbed fighter is put into, e.g. `FIGHTER_STATUS_KIND_CAPTURE_PULLED`.
    pub status: String,
    pub situation: String,
}

impl CatchCall {
    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        Hitbox {
            id: self.id,
            bone_name: self.bone_name.clone(),
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            // A grab box deals no damage or knockback — the attack-only fields stay zeroed,
            // matching what a live capture builds for one.
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            active_start,
            active_end: u32::MAX,
            category: 1,
            catch: Some(CatchExtras {
                status: self.status.clone(),
                situation: self.situation.clone(),
            }),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttackCall {
    /// Which `ATTACK`-family macro this call is — `ATTACK` or `ATTACK_IGNORE_THROW`.
    ///
    /// They share every argument this struct names, but they are not interchangeable: an
    /// `ATTACK_IGNORE_THROW` hitbox passes through a fighter already being thrown. Emitting
    /// one as the other silently changes what the move does, so the name is carried rather
    /// than assumed. Projects written before this field default to `ATTACK`.
    #[serde(default = "default_attack_func")]
    pub func: String,
    // ── Positional / shape ────────────────────────────────────────────────
    pub id: u32,
    pub part: u32,
    pub bone_name: String,
    pub damage: f32,
    pub angle: i32,
    pub kb_scaling: i32,
    pub fkb: i32,
    pub kb_base: i32,
    pub size: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_z: f32,
    /// Capsule second endpoint — `Some([x,y,z])` or `None` for sphere.
    pub capsule_end: Option<[f32; 3]>,
    // ── Hit properties ────────────────────────────────────────────────────
    pub hitlag_mult: f32,
    pub sdi_mult: f32,
    pub setoff_kind: String,
    pub lr_check: String,
    pub is_clang: bool,
    pub is_add_attack: i32,
    pub hitbox_attr: f32,
    pub ground_or_air: i32,
    pub is_mtk: bool,
    pub is_shield_disable: bool,
    pub is_reflectable: bool,
    pub is_absorbable: bool,
    pub is_landing_attack: bool,
    // ── Collision masks ───────────────────────────────────────────────────
    pub situation_mask: String,
    pub category_mask: String,
    pub part_mask: String,
    pub no_finish_camera: bool,
    // ── Effect / sound ────────────────────────────────────────────────────
    pub collision_attr: String,
    pub sound_level: String,
    pub sound_attr: String,
    pub attack_region: String,
}

impl AttackCall {
    /// Convert to a display Hitbox at the given frame.
    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        Hitbox {
            func: self.func.clone(),
            id: self.id,
            part: self.part,
            bone_name: self.bone_name.clone(),
            damage: self.damage,
            angle: self.angle,
            kb_scaling: self.kb_scaling,
            fkb: self.fkb,
            kb_base: self.kb_base,
            size: self.size,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            offset_z: self.offset_z,
            capsule_end: self.capsule_end,
            hitlag_mult: self.hitlag_mult,
            sdi_mult: self.sdi_mult,
            setoff_kind: self.setoff_kind.clone(),
            lr_check: self.lr_check.clone(),
            is_clang: self.is_clang,
            is_add_attack: self.is_add_attack,
            hitbox_attr: self.hitbox_attr,
            ground_or_air: self.ground_or_air,
            is_mtk: self.is_mtk,
            is_shield_disable: self.is_shield_disable,
            is_reflectable: self.is_reflectable,
            is_absorbable: self.is_absorbable,
            is_landing_attack: self.is_landing_attack,
            situation_mask: self.situation_mask.clone(),
            category_mask: self.category_mask.clone(),
            part_mask: self.part_mask.clone(),
            no_finish_camera: self.no_finish_camera,
            collision_attr: self.collision_attr.clone(),
            sound_level: self.sound_level.clone(),
            sound_attr: self.sound_attr.clone(),
            attack_region: self.attack_region.clone(),
            active_start,
            active_end: u32::MAX,
            hitbox_type: 0,
            category: 0,
            wind: None,
            catch: None,
            abs: None,
            search: None,
            fp: None,
        }
    }
}

/// A parsed `macros::ATTACK_ABS` call.
///
/// Its own struct rather than a variant of [`AttackCall`]: the two share field *names* but not
/// slot indices, and this file's oldest trap is that reusing a layout across families corrupts
/// a different call. Sixteen arguments against `ATTACK`'s thirty-six, in a different order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttackAbsCall {
    pub kind: String,
    pub id: u32,
    pub damage: f32,
    pub angle: i32,
    pub kb_scaling: i32,
    pub fkb: i32,
    pub kb_base: i32,
    pub hitlag_mult: f32,
    pub lr_check: String,
    pub collision_attr: String,
    pub sound_level: String,
    pub sound_attr: String,
    pub attack_region: String,
    /// Slots 9, 11, 12 — see [`AbsExtras::unknowns`].
    pub unknowns: (f32, f32, bool),
}

impl AttackAbsCall {
    /// Convert to a display Hitbox at the given frame.
    ///
    /// Geometry is left at zero and the bone at the empty string, both of which the panel reads
    /// as "not applicable" for this category rather than as a value. A default `"top"` bone
    /// would be worse: it is a real bone, so nothing downstream could tell it from one the
    /// script chose.
    pub fn to_hitbox(&self, active_start: u32) -> Hitbox {
        Hitbox {
            func: "ATTACK_ABS".into(),
            id: self.id,
            bone_name: String::new(),
            damage: self.damage,
            angle: self.angle,
            kb_scaling: self.kb_scaling,
            fkb: self.fkb,
            kb_base: self.kb_base,
            size: 0.0,
            hitlag_mult: self.hitlag_mult,
            lr_check: self.lr_check.clone(),
            collision_attr: self.collision_attr.clone(),
            sound_level: self.sound_level.clone(),
            sound_attr: self.sound_attr.clone(),
            attack_region: self.attack_region.clone(),
            active_start,
            active_end: u32::MAX,
            category: CAT_ABS,
            abs: Some(AbsExtras {
                kind: self.kind.clone(),
                unknowns: self.unknowns,
            }),
            ..Default::default()
        }
    }
}

impl Hitbox {
    /// Rebuild the `ATTACK_ABS` call this row came from, or `None` if it is not one.
    pub fn to_attack_abs_call(&self) -> Option<AttackAbsCall> {
        let abs = self.abs.as_ref()?;
        Some(AttackAbsCall {
            kind: abs.kind.clone(),
            id: self.id,
            damage: self.damage,
            angle: self.angle,
            kb_scaling: self.kb_scaling,
            fkb: self.fkb,
            kb_base: self.kb_base,
            hitlag_mult: self.hitlag_mult,
            lr_check: self.lr_check.clone(),
            collision_attr: self.collision_attr.clone(),
            sound_level: self.sound_level.clone(),
            sound_attr: self.sound_attr.clone(),
            attack_region: self.attack_region.clone(),
            unknowns: abs.unknowns,
        })
    }
}

/// What a hurtbox-state call is aimed at.
///
/// `HIT_NODE` names a bone and `HIT_NO` a numbered group. They take the same *shape* —
/// target then status — but they are not the same family and a bone hash must never be
/// written into a group slot, so the two are distinguished here rather than flattened into
/// one string that the write-back would have to guess the type of.
///
/// [`Whole`](Self::Whole) is the third, and it is the one that does *not* share that shape: see
/// [`takes_target_argument`](Self::takes_target_argument).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum HurtTarget {
    /// `HIT_NODE(agent, Hash40::new("legr"), …)` — one named bone.
    Bone(String),
    /// `HIT_NO(agent, 8, …)` — one numbered hurtbox group.
    Group(i64),
    /// `WHOLE_HIT(agent, *HIT_STATUS_XLU)` — every bone at once, with no target argument.
    ///
    /// Filed under "post-hoc hitbox tuning" in the backlog until 2026-08-04, on the strength of
    /// the word *HIT* in the name. Its argument is a `HIT_STATUS_*`, the same four-state
    /// [`param_labels::HIT_STATUS`](crate::param_labels::HIT_STATUS) the other two take, so it
    /// changes how the fighter *receives* hits and belongs here. Unlike `COL_PRI` there is no
    /// `lua_const` constant to appeal to — no `MA_MSC_CMD_*` name exists for it — so the
    /// signature is what settles it.
    ///
    /// **The all-bones reach is deliberately not modelled.** In the game this call covers the
    /// bones a `HIT_NODE` names, so a later `WHOLE_HIT` arguably ends an open per-bone span.
    /// Spans here end on a later call to the *same* target or on `HIT_RESET_ALL`, and `Whole` is
    /// simply a third target. No vanilla script mixes `WHOLE_HIT` with `HIT_NODE`, `HIT_NO` or
    /// `HIT_RESET_ALL` — all 6 occurrences stand alone — so there is nothing to calibrate a
    /// cross-target rule against, and guessing one would put invented spans on the timeline.
    Whole,
}

impl HurtTarget {
    /// The macro that writes this target. Each target type has exactly one.
    pub fn macro_name(&self) -> &'static str {
        match self {
            HurtTarget::Bone(_) => "HIT_NODE",
            HurtTarget::Group(_) => "HIT_NO",
            HurtTarget::Whole => "WHOLE_HIT",
        }
    }

    /// Whether this target is written as an argument, or is implied by the macro name.
    ///
    /// The one place the three targets differ in *shape*: `HIT_NODE` and `HIT_NO` are
    /// `(target, status)` and `WHOLE_HIT` is `(status)`. Every surface that formats or parses a
    /// hurtbox call has to branch here, so the question is asked once, by name, rather than
    /// spelled as a bare `matches!` at each site.
    pub fn takes_target_argument(&self) -> bool {
        !matches!(self, HurtTarget::Whole)
    }

    /// How the target reads in the panel and the timeline lane label.
    pub fn label(&self) -> String {
        match self {
            HurtTarget::Bone(bone) => bone.clone(),
            HurtTarget::Group(n) => format!("group {n}"),
            HurtTarget::Whole => "whole body".to_string(),
        }
    }
}

/// One resolved stretch of non-default hurtbox state, for the panel and the timeline.
///
/// Produced by [`AcmdScript::to_hurtboxes`]. A row is one call plus the frame at which a later
/// call took it back, which is why this is not simply the parsed statement: the script says
/// "leg becomes intangible" and "leg becomes normal" as two independent lines, and what a
/// modder wants to see is the span between them.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HurtboxState {
    pub target: HurtTarget,
    /// Symbolic `HIT_STATUS_*` name, or a bare number if this build does not know the name.
    pub status: String,
    pub active_start: u32,
    pub active_end: u32,
    /// Which hurtbox statement in the script this span came from — see [`site`](Self::site).
    pub site: usize,
}

/// One resolved stretch of non-default colour-blend priority (`COL_PRI` … `COL_NORMAL`).
///
/// Named for the macro rather than for what it does, which is why it read as body collision for
/// so long — see [`ExcuteStmt::ColPri`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColPriState {
    pub pri: i64,
    pub active_start: u32,
    pub active_end: u32,
    pub site: usize,
}

/// Ordinal of a hurtbox statement among all hurtbox statements, in source order.
///
/// The panel edits values in the script itself rather than rebuilding these calls from a
/// separate list the way collisions are rebuilt, so a span needs to say which statement it came
/// from. A statement inside a `for` produces one span per iteration and every one of them
/// carries the same site — editing any is editing the one line, which is what the source says.
pub type HurtSite = usize;

/// Which post-hoc tuning macro an [`AttackModState`] came from.
///
/// The two members of the "hitbox already out" family that take a hitbox id. They share one
/// argument layout — `(id: u64, value: ToF32)` — so they are one type with a discriminant
/// rather than two `ExcuteStmt` variants: every surface treats them identically apart from the
/// macro name and the label. `lua_const` has no `MA_MSC_CMD_*` constant for either, so the
/// `macros.rs` signature is what places them here.
///
/// The other two macros that read like members are not: `ATK_HIT_ABS` and `ATK_LERP_RATIO` take
/// no id, so there is no hitbox for them to modify. See `TODO.md` B3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttackModKind {
    /// `ATK_POWER` — re-set the damage of a hitbox that is already out.
    Power,
    /// `ATK_SET_SHIELD_SETOFF_MUL` — scale the shield push-off of a hitbox already out.
    ShieldSetoffMul,
}

impl AttackModKind {
    /// Every member, for the panel's picker and for exhaustive tests.
    pub const ALL: [AttackModKind; 2] = [AttackModKind::Power, AttackModKind::ShieldSetoffMul];

    pub fn macro_name(&self) -> &'static str {
        match self {
            AttackModKind::Power => "ATK_POWER",
            AttackModKind::ShieldSetoffMul => "ATK_SET_SHIELD_SETOFF_MUL",
        }
    }

    /// What the value means, for the panel row.
    pub fn label(&self) -> &'static str {
        match self {
            AttackModKind::Power => "damage",
            AttackModKind::ShieldSetoffMul => "shield push-off ×",
        }
    }
}

/// One resolved post-hoc edit to a hitbox that is already out.
///
/// A point event, not a span: unlike a hurtbox state there is no macro that takes it back, so
/// there is no end frame to resolve and inventing one would draw a range the script never wrote.
/// It carries the id it retunes rather than being folded into the parent `ATTACK`, because the
/// export has to re-emit it as its own call at its own frame.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttackModState {
    pub kind: AttackModKind,
    /// The hitbox id this retunes. Not resolved to a hitbox here: a script may tune an id that
    /// no longer has an open hitbox, and reporting that is the panel's job, not this walk's.
    pub id: i64,
    pub value: f32,
    pub frame: u32,
    pub site: AttackModSite,
}

/// Ordinal of an attack-modifier statement among all of them, in source order.
///
/// Deliberately its own numbering space rather than a share of [`HurtSite`]: the two families
/// are scanned by different command tables on the write-back path, and folding them into one
/// counter would make every hurtbox site shift the moment a script gained an `ATK_POWER`.
pub type AttackModSite = usize;

/// One statement inside an is_excute block.
// AttackCall is intentionally inline: ATTACK statements dominate these short-lived syntax
// trees, so boxing every normal statement would add allocations to optimize the rare Raw case.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ExcuteStmt {
    Attack(AttackCall),
    /// CATCH — a grab box. Its own family: `CATCH` shares no argument layout with `ATTACK`
    /// and is cleared by `GrabModule`, not `AttackModule`.
    Catch(CatchCall),
    /// ATTACK_ABS — damage applied to an opponent already caught. No volume, no bone.
    AttackAbs(AttackAbsCall),
    /// ATTACK_FP — fighter-position collision with a separate 41-slot payload.
    AttackFp(AttackFpCall),
    /// SEARCH — a detection volume. Its own family for the same reason `CATCH` is: it shares
    /// no argument layout with `ATTACK`, and nothing in a `game_` script takes it back.
    Search(SearchCall),
    Wind(WindboxData),
    EraseWind(u32),
    Clear(u32),
    ClearAll,
    /// GrabModule::clear_all — ends every open grab box, and only grab boxes.
    GrabClearAll,
    /// `HIT_NODE` / `HIT_NO` — set one bone's or one group's hurtbox state.
    ///
    /// Not a collision: this changes how the fighter *receives* hits, so it neither appears in
    /// [`AcmdScript::to_hitboxes`] nor is ended by an `AttackModule::clear_all`. It is ended by
    /// a later call on the same target, or by [`HitResetAll`](Self::HitResetAll).
    HitStatus {
        target: HurtTarget,
        /// Kept as written — symbolic where the script wrote a symbol. Storing the number
        /// instead would export `*HIT_STATUS_XLU` as `2`, which compiles but stops matching
        /// the vanilla text this parser is calibrated against.
        status: String,
    },
    /// `HIT_RESET_ALL` — return every bone and group to its default state at once.
    HitResetAll,
    /// `COL_PRI` — which colour blend wins while several are applied, not a hurtbox state.
    ///
    /// `lua_const` calls it `MA_MSC_CMD_COLOR_BLEND_COL_PRI`, one of six
    /// `MA_MSC_CMD_COLOR_BLEND_*` commands with `FLASH` and `FLASH_FRM`, so it is the `FLASH`
    /// family's priority and has nothing to do with pushboxes. It is parsed here rather than
    /// with the colour commands only because this is where a `game_` script's statements live;
    /// all ten corpus occurrences of the pair are in `effect_` functions, where `COL_NORMAL`
    /// goes through [`COLOR_COMMANDS`] instead and `COL_PRI` rides along verbatim.
    ColPri(i64),
    /// `COL_NORMAL` — clear the colour blend, ending an open `COL_PRI` or `FLASH`.
    ColNormal,
    /// `ATK_POWER` / `ATK_SET_SHIELD_SETOFF_MUL` — retune a hitbox that is already out.
    ///
    /// Not a collision of its own, so it does not appear in [`AcmdScript::to_hitboxes`]: it
    /// edits one the script already opened. The corpus writes it both in the same `is_excute`
    /// block as its `ATTACK` and several frames later, so it is kept as a separate statement at
    /// its own frame rather than folded into the call it modifies.
    AttackMod {
        kind: AttackModKind,
        id: i64,
        value: f32,
    },
    /// One call from the `PLAY_SE` family — a sound the script starts or stops.
    ///
    /// Not a collision and not a spawn: it fires on one frame and the script never takes it
    /// back, so it has no span for [`AcmdScript::to_hitboxes`] to end. `STOP_SE` looks like it
    /// would end one, but it silences a sound started somewhere else entirely — usually by a
    /// different script — so pairing the two inside one move would be guesswork.
    ///
    /// Every one of the corpus's 610 calls lives in a `sound_` function; not one is in a
    /// `game_` or `effect_` one. It is parsed here anyway because a `sound_` script is read by
    /// the same walker a `game_` script is, and this is where that walker's statements live.
    Sound(SoundCall),
    /// One of the measured `expression_` camera/rumble calls.
    ///
    /// The arguments stay as source tokens rather than being decoded to guessed enums. That
    /// keeps named lua constants (`*CAMERA_QUAKE_KIND_L`) and captured numeric values both
    /// compilable and round-trippable while the later live rule surface can operate on their
    /// typed wire representation.
    Expression(ExpressionCall),
    /// `REVERSE_LR` — flip the fighter's facing direction at this point in the move.
    ///
    /// This is deliberately a point statement rather than a state with an editable value:
    /// the macro takes no arguments after `agent`, so the meaningful edits are whether it is
    /// present and which frame it runs on.
    ReverseLr,
    /// `SET_SPEED_EX` — set the x/y velocity of one kinetic-energy reserve.
    SetSpeedEx(SetSpeedExCall),
    /// `SET_SPEED` — set the fighter's x/y velocity directly.
    SetSpeed(SetSpeedCall),
    /// `ADD_SPEED_NO_LIMIT` — add x/y velocity without the kinetic module's normal limit.
    AddSpeedNoLimit(AddSpeedNoLimitCall),
    /// `CORRECT` — change the fighter's ground-correction mode at this point in the move.
    Correct(CorrectCall),
    /// Any other line we don't interpret — preserved verbatim.
    Raw(String),
}

/// A call from the `PLAY_SE` family, as written.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SoundCall {
    /// The macro name without the `macros::` path — `PLAY_SE`, `STOP_SE`, `PLAY_SEQUENCE`, …
    pub func: String,
    /// The `Hash40::new("…")` arguments in order, unwrapped to the bare name.
    ///
    /// A list rather than one name because two members take a pair: `PLAY_STEP_FLIPPABLE`
    /// names the left and right footstep, and `PLAY_FLY_VOICE` two alternative voice clips.
    pub sounds: Vec<String>,
    /// The trailing non-hash argument, verbatim. Only `SET_PLAY_INHIVIT` has one — the frames
    /// for which the named sound is suppressed — and it is kept as text so a `5` stays a `5`
    /// rather than being re-emitted as `5.0`.
    pub tail: Option<String>,
}

/// One of the three expression macros measured in the local script corpus.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExpressionCall {
    /// `macros::RUMBLE_HIT(agent, kind, unk)`.
    RumbleHit { kind: String, unk: String },
    /// `macros::QUAKE(agent, kind)`.
    Quake { kind: String },
    /// `macros::FT_ATTACK_ABS_CAMERA_QUAKE(agent, attack_abs_kind, quake_kind)`.
    FtAttackAbsCameraQuake {
        attack_abs_kind: String,
        quake_kind: String,
    },
}

impl ExpressionCall {
    /// The smash-script macro name without its `macros::` path.
    pub fn func(&self) -> &'static str {
        match self {
            Self::RumbleHit { .. } => "RUMBLE_HIT",
            Self::Quake { .. } => "QUAKE",
            Self::FtAttackAbsCameraQuake { .. } => "FT_ATTACK_ABS_CAMERA_QUAKE",
        }
    }

    /// Source tokens in the order the corresponding `sv_animcmd` primitive receives them.
    pub fn tokens(&self) -> Vec<&str> {
        match self {
            Self::RumbleHit { kind, unk } => vec![kind, unk],
            Self::Quake { kind } => vec![kind],
            Self::FtAttackAbsCameraQuake {
                attack_abs_kind,
                quake_kind,
            } => vec![attack_abs_kind, quake_kind],
        }
    }
}

/// A parsed `macros::SET_SPEED_EX` call.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetSpeedExCall {
    pub speed_x: f32,
    pub speed_y: f32,
    /// The third argument after `agent`, retained as authored so named kinetic constants survive
    /// parse -> edit -> export.
    pub kinetic_kind: String,
}

impl SetSpeedExCall {
    pub const FUNC: &'static str = "SET_SPEED_EX";
}

/// A parsed `macros::SET_SPEED` call.
///
/// The checked-in public corpus uses exactly `(agent, x, y)`. The generated export calls a
/// local helper over the linked `sv_animcmd::SET_SPEED` primitive because the vendored
/// `smash-script` crate exposes no safe Rust macro wrapper for this primitive.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetSpeedCall {
    pub speed_x: f32,
    pub speed_y: f32,
}

impl SetSpeedCall {
    pub const FUNC: &'static str = "SET_SPEED";
}

/// A parsed `macros::ADD_SPEED_NO_LIMIT` call.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AddSpeedNoLimitCall {
    pub speed_x: f32,
    pub speed_y: f32,
}

impl AddSpeedNoLimitCall {
    pub const FUNC: &'static str = "ADD_SPEED_NO_LIMIT";
}

/// A parsed `macros::CORRECT` call.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CorrectCall {
    /// Ground-correction kind, retained as authored so named constants survive export.
    pub kind: String,
}

impl CorrectCall {
    pub const FUNC: &'static str = "CORRECT";
}

/// A resolved expression call at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExpressionEvent {
    pub frame: u32,
    pub call: ExpressionCall,
    /// Source ordinal among expression calls, independent of hitbox/sound sites.
    #[serde(default)]
    pub site: usize,
}

/// A resolved `REVERSE_LR` point event at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReverseLrEvent {
    pub frame: u32,
    /// Source ordinal among reverse-facing calls, independent of every other family.
    #[serde(default)]
    pub site: usize,
}

/// A resolved `SET_SPEED_EX` point event at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetSpeedExEvent {
    pub frame: u32,
    pub call: SetSpeedExCall,
    #[serde(default)]
    pub site: usize,
}

/// A resolved `SET_SPEED` point event at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetSpeedEvent {
    pub frame: u32,
    pub call: SetSpeedCall,
    #[serde(default)]
    pub site: usize,
}

/// A resolved `ADD_SPEED_NO_LIMIT` point event at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AddSpeedNoLimitEvent {
    pub frame: u32,
    pub call: AddSpeedNoLimitCall,
    #[serde(default)]
    pub site: usize,
}

/// A resolved `CORRECT` point event at the one-based game frame it fires on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CorrectEvent {
    pub frame: u32,
    pub call: CorrectCall,
    #[serde(default)]
    pub site: usize,
}

/// A resolved sound with the frame it fires on.
///
/// One-shot by construction: unlike a hitbox or a following effect there is no end frame to
/// compute, because nothing in a `game_` or `sound_` script closes a sound it started.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SoundEvent {
    pub frame: u32,
    pub call: SoundCall,
    /// Which sound call in the script's *source order* this event came from.
    ///
    /// A looped `PLAY_SE` produces one event per iteration and every one of them carries the
    /// same site, because all of them are the one line in the file. Write-back resolves a site
    /// against a textual scan of the source, so this has to be the ordinal a pre-order read of
    /// the text would give — see [`WalkAccum::next_sound_site`].
    ///
    /// Counted separately from the hurtbox and attack-modifier ordinals. They are three
    /// independent spaces over three different sets of macros; sharing a counter would make
    /// every sound site shift the moment a script gained a `HIT_NODE`.
    #[serde(default)]
    pub site: usize,
}

/// A timing statement in the script.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AcmdStmt {
    Frame(f32),
    Wait(f32),
    WaitLoopClear,
    Excute(Vec<ExcuteStmt>),
    Loop {
        count: usize,
        body: Vec<AcmdStmt>,
    },
    /// A block the editor does not model, kept whole: a runtime branch
    /// (`if WorkModule::is_flag(…) {`), its `else {`, a raw `for`.
    ///
    /// It has to be a block rather than a `Raw` opening line because the closing brace is part
    /// of it. Flattening one drops that brace — the exported function then never closes, and
    /// the next function in the file is swallowed by it — and it promotes both arms of a
    /// branch to unconditional. `header` is the opening line, trimmed of its indentation.
    RawBlock {
        header: String,
        body: Vec<AcmdStmt>,
    },
    /// A typed command written at the function's top level, outside any `is_excute` block.
    ///
    /// Fifteen corpus `sound_` scripts end this way — `kirby/WalkMiddle` plays its second
    /// footstep bare — and before D1c every one of those calls was [`Raw`](Self::Raw) and
    /// invisible. It cannot be modelled as a one-statement [`Excute`](Self::Excute) because
    /// emitting that would *add* an `if macros::is_excute(agent) {` the source never wrote,
    /// which is both a behaviour change and a round-trip failure.
    ///
    /// Only sound calls are parsed into this today. A bare `ATTACK` in a `game_` script stays
    /// `Raw`, exactly as it did before, because nothing has measured whether one exists or what
    /// the timeline should do with it.
    Bare(Box<ExcuteStmt>),
    /// `macros::FT_MOTION_RATE(agent, r)` — the animation's playback rate from here on.
    ///
    /// **`r` below 1.0 makes the move play FASTER**, which is the opposite of what the name
    /// suggests and is load-bearing everywhere this value is used. The engine advances the
    /// motion by `1/r` motion frames per game frame, so a span of `n` motion frames takes
    /// `n * r` game frames: kirby's down smash sets `0.25` and crosses 4 motion frames of windup
    /// in a single game frame. Measured live, four moves and four arguments — see E2 in
    /// `TODO.md`.
    ///
    /// This is why the timeline distinguishes script frames from game frames at all. A script
    /// frame is a *motion* frame, which is the number the author writes and the number every
    /// other statement here is keyed to; the game frame is what the player experiences.
    ///
    /// Written at the function's top level, never inside an `is_excute` block, in all 17 corpus
    /// calls.
    MotionRate(f32),
    Raw(String),
}

/// The parsed ACMD game_ function, preserving full structure for export.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AcmdScript {
    pub stmts: Vec<AcmdStmt>,
}

/// One stretch of the timeline over which the playback rate does not change.
///
/// `rate` is the [`AcmdStmt::MotionRate`] argument in force, so `1.0` outside every rate window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateSpan {
    /// First motion frame this rate applies to.
    pub from_motion: f32,
    /// Motion frame the next span starts at, or [`f32::INFINITY`] for the last one.
    pub to_motion: f32,
    /// Game frame `from_motion` lands on.
    pub from_game: f32,
    pub rate: f32,
}

impl AcmdScript {
    /// Count runtime conditional blocks preserved in this script.
    ///
    /// A live capture observes one execution path, while a source script may contain several
    /// arms. This count is intentionally structural: loops are not branches, and a branch nested
    /// inside one is counted once because it is still one source condition. It is used only for
    /// provenance warnings, never to pretend the editor can reconstruct an unobserved arm.
    pub fn branch_count(&self) -> usize {
        fn count(stmts: &[AcmdStmt]) -> usize {
            stmts
                .iter()
                .map(|stmt| match stmt {
                    AcmdStmt::RawBlock { body, .. } => 1 + count(body),
                    AcmdStmt::Loop { body, .. } => count(body),
                    _ => 0,
                })
                .sum()
        }
        count(&self.stmts)
    }

    /// The script's motion-frame → game-frame mapping, one entry per rate change.
    ///
    /// Motion frames are what a script names and what every hitbox range is keyed to; game
    /// frames are what the player experiences. They differ wherever a rate other than `1.0` is
    /// in force: the engine advances the motion by `1/r` motion frames per game frame, so a span
    /// of `n` motion frames takes `n * r` game frames.
    ///
    /// Rate calls sit at the function's top level in all 17 corpus calls, so only the top level
    /// is walked. A rate set inside a runtime branch would apply for real, but whether the
    /// branch is taken is not knowable here, and pretending otherwise would put a confident
    /// wrong number on the timeline; those scripts keep a `Raw` block and are left alone.
    pub fn rate_spans(&self) -> Vec<RateSpan> {
        let mut spans: Vec<RateSpan> = Vec::new();
        let (mut motion, mut game, mut rate) = (0.0f32, 0.0f32, 1.0f32);
        for stmt in &self.stmts {
            match stmt {
                // `frame()` waits *until* a frame and returns immediately if it has already
                // passed, so a target behind the cursor advances nothing rather than rewinding.
                AcmdStmt::Frame(f) => {
                    let advance = (*f - motion).max(0.0);
                    motion += advance;
                    game += advance * rate;
                }
                AcmdStmt::Wait(w) => {
                    let advance = w.max(0.0);
                    motion += advance;
                    game += advance * rate;
                }
                AcmdStmt::MotionRate(r) if *r > 0.0 => {
                    if let Some(last) = spans.last_mut() {
                        last.to_motion = motion;
                    }
                    spans.push(RateSpan {
                        from_motion: motion,
                        to_motion: f32::INFINITY,
                        from_game: game,
                        rate: *r,
                    });
                    rate = *r;
                }
                _ => {}
            }
        }
        spans
    }

    /// The game frame a motion frame lands on, given this script's rate windows.
    ///
    /// Every top-level rate call as `(index into `stmts`, motion frame it lands on, rate)`.
    ///
    /// The index is what an edit writes through, so the panel can change a value without
    /// rebuilding the script — the same shape the hurtbox and hitbox-tuning sections use, and
    /// for the same reason: these statements are carried through the script rather than
    /// regenerated from a list, so the script is the model and there is no second copy.
    pub fn motion_rate_sites(&self) -> Vec<(usize, f32, f32)> {
        let mut sites = Vec::new();
        let mut motion = 0.0f32;
        for (index, stmt) in self.stmts.iter().enumerate() {
            match stmt {
                AcmdStmt::Frame(f) => motion += (*f - motion).max(0.0),
                AcmdStmt::Wait(w) => motion += w.max(0.0),
                AcmdStmt::MotionRate(r) => sites.push((index, motion, *r)),
                _ => {}
            }
        }
        sites
    }

    /// The rate statement at a site from [`motion_rate_sites`](Self::motion_rate_sites).
    ///
    /// Returns `None` if the index is not a rate call, so a stale site from a script that has
    /// since been re-parsed writes nothing rather than overwriting an unrelated statement.
    pub fn motion_rate_mut(&mut self, index: usize) -> Option<&mut f32> {
        match self.stmts.get_mut(index) {
            Some(AcmdStmt::MotionRate(rate)) => Some(rate),
            _ => None,
        }
    }

    /// Equal to `motion` for a script with no rate call, which is every script but ten in the
    /// corpus — so callers can use this unconditionally rather than branching on whether a rate
    /// is present, which is the kind of branch that goes stale.
    pub fn game_frame(&self, motion: f32) -> f32 {
        let spans = self.rate_spans();
        // Before the first rate call the animation runs at 1.0, so the frame is its own answer.
        let Some(first) = spans.first() else {
            return motion;
        };
        if motion <= first.from_motion {
            return motion;
        }
        let span = spans
            .iter()
            .rev()
            .find(|s| motion >= s.from_motion)
            .unwrap_or(first);
        span.from_game + (motion - span.from_motion) * span.rate
    }

    /// Flatten the script into display hitboxes with computed frame ranges.
    pub fn to_hitboxes(&self) -> Vec<Hitbox> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut hurt = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut hurt);
        for hb in hitboxes.iter_mut() {
            if hb.active_end == u32::MAX {
                hb.active_end = 9999;
            }
        }
        hitboxes
    }

    /// Flatten the script into hurtbox-state and collision-priority spans.
    ///
    /// Resolved by the same walk as [`to_hitboxes`](Self::to_hitboxes) rather than a second one
    /// beside it: `frame` / `wait` arithmetic and `for` unrolling decide where these spans start
    /// just as much as where a hitbox does, and two implementations of that would drift.
    pub fn to_hurtboxes(&self) -> (Vec<HurtboxState>, Vec<ColPriState>) {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut hurt = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut hurt);
        // An unterminated span runs to the end of the move, the same `9999` sentinel an
        // uncleared hitbox gets. Scripts routinely set a state and never take it back — the
        // engine resets on the status change — so this is the common case, not an error.
        for state in hurt.states.iter_mut() {
            if state.active_end == u32::MAX {
                state.active_end = 9999;
            }
        }
        for pri in hurt.pris.iter_mut() {
            if pri.active_end == u32::MAX {
                pri.active_end = 9999;
            }
        }
        (hurt.states, hurt.pris)
    }

    /// Flatten the script into post-hoc hitbox modifiers, each at the frame it runs on.
    ///
    /// No end-frame pass like the two above: these are point events. See [`AttackModState`].
    pub fn to_attack_mods(&self) -> Vec<AttackModState> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.mods
    }

    /// Flatten the script into the sounds it plays, each at the frame it fires on.
    ///
    /// Resolved by the same walk as the three above, for the reason [`to_hurtboxes`] gives:
    /// `frame`/`wait` arithmetic and `for` unrolling decide when a sound fires exactly as much
    /// as when a hitbox opens, and a second implementation of that would drift from this one.
    ///
    /// A `for` body is unrolled, so a looped `PLAY_SE` yields one event per iteration. That is
    /// what the game does — each pass plays the sound again — and it is the same treatment a
    /// looped hitbox gets.
    ///
    /// [`to_hurtboxes`]: Self::to_hurtboxes
    pub fn to_sound_events(&self) -> Vec<SoundEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.sounds
    }

    /// Flatten the measured expression calls into frame events.
    ///
    /// This shares the same frame/wait/loop walk as hitboxes and sounds. Expression scripts are
    /// stored in the same `AcmdStmt` tree, so a looped or branched call cannot quietly acquire a
    /// second timing implementation.
    pub fn to_expression_events(&self) -> Vec<ExpressionEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.expressions
    }

    /// Flatten `REVERSE_LR` calls into point events at their one-based game frames.
    pub fn to_reverse_lr_events(&self) -> Vec<ReverseLrEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.reverse_lrs
    }

    /// Flatten `SET_SPEED_EX` calls into editable point events at their one-based game frames.
    pub fn to_speed_ex_events(&self) -> Vec<SetSpeedExEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.speed_exs
    }

    /// Flatten `SET_SPEED` calls into editable point events at their one-based game frames.
    pub fn to_speed_events(&self) -> Vec<SetSpeedEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.speeds
    }

    /// Flatten `ADD_SPEED_NO_LIMIT` calls into editable point events at their one-based game
    /// frames.
    pub fn to_add_speed_no_limit_events(&self) -> Vec<AddSpeedNoLimitEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.add_speed_no_limits
    }

    /// Flatten `CORRECT` calls into editable point events at their one-based game frames.
    pub fn to_correct_events(&self) -> Vec<CorrectEvent> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        let mut acc = WalkAccum::default();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes, &mut acc);
        acc.corrects
    }
}

impl AcmdScript {
    /// The hurtbox statement a span's [`site`](HurtboxState::site) refers to.
    ///
    /// Pre-order over the source with each `for` body entered exactly once, which is the
    /// definition [`WalkAccum::next_site`] is written to reproduce. Editing through this is
    /// what makes a panel change reach the export: these statements are carried through the
    /// script rather than rebuilt from a list, so the script *is* the model.
    pub fn hurt_stmt_mut(&mut self, site: HurtSite) -> Option<&mut ExcuteStmt> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: HurtSite,
            seen: &mut usize,
        ) -> Option<&'a mut ExcuteStmt> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for s in inner.iter_mut().filter(|s| is_hurt_stmt(s)) {
                            if *seen == site {
                                return Some(s);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The statement an [`AttackModState::site`] refers to, by the same rule as
    /// [`hurt_stmt_mut`](Self::hurt_stmt_mut) but over its own numbering space.
    pub fn attack_mod_stmt_mut(&mut self, site: AttackModSite) -> Option<&mut ExcuteStmt> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: AttackModSite,
            seen: &mut usize,
        ) -> Option<&'a mut ExcuteStmt> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for s in inner.iter_mut().filter(|s| is_attack_mod_stmt(s)) {
                            if *seen == site {
                                return Some(s);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The sound call a [`SoundEvent::site`] refers to, for writing an edit back into the IR.
    ///
    /// Walks the same shapes [`count_sound_stmts`] counts, in the same order — including `Bare`,
    /// which the two functions above genuinely do not have to handle: no hurtbox or attack
    /// modifier is ever written outside an `is_excute` block. `RawBlock` used to be listed here
    /// as a second such exemption and was not one — the two above skipped it while `eval_stmts`
    /// descended into it, so for those families a site inside a runtime branch resolved to the
    /// call *after* the branch. If these ever disagree about what takes a site, an edit lands on
    /// the wrong call and produces a script that is still perfectly well-formed, which is why the
    /// corpus oracle checks the macro name at the resolved site rather than only that a site
    /// resolves.
    pub fn sound_stmt_mut(&mut self, site: usize) -> Option<&mut ExcuteStmt> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut ExcuteStmt> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for s in inner.iter_mut().filter(|s| is_sound_stmt(s)) {
                            if *seen == site {
                                return Some(s);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) if is_sound_stmt(inner) => {
                        if *seen == site {
                            return Some(inner.as_mut());
                        }
                        *seen += 1;
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The expression call an [`ExpressionEvent::site`] refers to, in source order.
    pub fn expression_stmt_mut(&mut self, site: usize) -> Option<&mut ExpressionCall> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut ExpressionCall> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for expression in inner.iter_mut().filter_map(|stmt| match stmt {
                            ExcuteStmt::Expression(call) => Some(call),
                            _ => None,
                        }) {
                            if *seen == site {
                                return Some(expression);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) => {
                        if let ExcuteStmt::Expression(call) = inner.as_mut() {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The `SET_SPEED_EX` call an event site's ordinal refers to, in source order.
    pub fn speed_ex_stmt_mut(&mut self, site: usize) -> Option<&mut SetSpeedExCall> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut SetSpeedExCall> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for call in inner.iter_mut().filter_map(|stmt| match stmt {
                            ExcuteStmt::SetSpeedEx(call) => Some(call),
                            _ => None,
                        }) {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) => {
                        if let ExcuteStmt::SetSpeedEx(call) = inner.as_mut() {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The `SET_SPEED` call an event site's ordinal refers to, in source order.
    pub fn set_speed_stmt_mut(&mut self, site: usize) -> Option<&mut SetSpeedCall> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut SetSpeedCall> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for call in inner.iter_mut().filter_map(|stmt| match stmt {
                            ExcuteStmt::SetSpeed(call) => Some(call),
                            _ => None,
                        }) {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) => {
                        if let ExcuteStmt::SetSpeed(call) = inner.as_mut() {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The `ADD_SPEED_NO_LIMIT` call an event site's ordinal refers to, in source order.
    pub fn add_speed_no_limit_stmt_mut(&mut self, site: usize) -> Option<&mut AddSpeedNoLimitCall> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut AddSpeedNoLimitCall> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for call in inner.iter_mut().filter_map(|stmt| match stmt {
                            ExcuteStmt::AddSpeedNoLimit(call) => Some(call),
                            _ => None,
                        }) {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) => {
                        if let ExcuteStmt::AddSpeedNoLimit(call) = inner.as_mut() {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// The `CORRECT` call an event site's ordinal refers to, in source order.
    pub fn correct_stmt_mut(&mut self, site: usize) -> Option<&mut CorrectCall> {
        fn walk<'a>(
            stmts: &'a mut [AcmdStmt],
            site: usize,
            seen: &mut usize,
        ) -> Option<&'a mut CorrectCall> {
            for stmt in stmts {
                match stmt {
                    AcmdStmt::Excute(inner) => {
                        for call in inner.iter_mut().filter_map(|stmt| match stmt {
                            ExcuteStmt::Correct(call) => Some(call),
                            _ => None,
                        }) {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) => {
                        if let ExcuteStmt::Correct(call) = inner.as_mut() {
                            if *seen == site {
                                return Some(call);
                            }
                            *seen += 1;
                        }
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if let Some(found) = walk(body, site, seen) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&mut self.stmts, site, &mut 0)
    }

    /// Remove the `REVERSE_LR` source statement at an ordinal from
    /// [`to_reverse_lr_events`](Self::to_reverse_lr_events).
    ///
    /// The ordinal is source-based, not an execution count: a call inside a loop is one source
    /// statement even when it produces several point events. Removing a looped call therefore
    /// removes the source line rather than only one unrolled occurrence.
    pub fn remove_reverse_lr(&mut self, site: usize) -> bool {
        fn walk(stmts: &mut Vec<AcmdStmt>, site: usize, seen: &mut usize) -> bool {
            let mut index = 0;
            while index < stmts.len() {
                match &mut stmts[index] {
                    AcmdStmt::Excute(inner) => {
                        let mut inner_index = 0;
                        while inner_index < inner.len() {
                            if matches!(inner[inner_index], ExcuteStmt::ReverseLr) {
                                if *seen == site {
                                    inner.remove(inner_index);
                                    if inner.is_empty() {
                                        stmts.remove(index);
                                    }
                                    return true;
                                }
                                *seen += 1;
                            }
                            inner_index += 1;
                        }
                    }
                    AcmdStmt::Bare(inner) if matches!(inner.as_ref(), ExcuteStmt::ReverseLr) => {
                        if *seen == site {
                            stmts.remove(index);
                            return true;
                        }
                        *seen += 1;
                    }
                    AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                        if walk(body, site, seen) {
                            return true;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            false
        }

        walk(&mut self.stmts, site, &mut 0)
    }

    /// Add a `REVERSE_LR` call on a top-level frame, preserving an existing execute block when
    /// one already names that frame. If no such block exists, a new absolute frame/block pair is
    /// inserted before the next later top-level frame.
    ///
    /// The first editor pass intentionally does not invent a call inside a runtime branch or a
    /// loop. Those structures are retained for export, but their execution context is not a safe
    /// place for a new unconditional point event.
    pub fn insert_reverse_lr_at_frame(&mut self, frame: u32) -> bool {
        let target = frame.max(1) as f32;
        for index in 0..self.stmts.len() {
            if !matches!(self.stmts[index], AcmdStmt::Frame(value) if script_frame(value) == frame)
            {
                continue;
            }
            if let Some(AcmdStmt::Excute(inner)) = self.stmts.get_mut(index + 1) {
                inner.push(ExcuteStmt::ReverseLr);
                return true;
            }
            self.stmts
                .insert(index + 1, AcmdStmt::Excute(vec![ExcuteStmt::ReverseLr]));
            return true;
        }

        let insert_at = self
            .stmts
            .iter()
            .position(|stmt| matches!(stmt, AcmdStmt::Frame(value) if *value > target))
            .unwrap_or(self.stmts.len());
        self.stmts.insert(insert_at, AcmdStmt::Frame(target));
        self.stmts
            .insert(insert_at + 1, AcmdStmt::Excute(vec![ExcuteStmt::ReverseLr]));
        true
    }
}

/// Everything but hitboxes that one walk of a script resolves.
///
/// Named for the walk rather than for hurtboxes because it carries two independent families now.
/// They are gathered together, and not by a second walk each, because `frame` / `wait` arithmetic
/// and `for` unrolling decide where all of them land — two implementations of that would drift.
#[derive(Default)]
struct WalkAccum {
    states: Vec<HurtboxState>,
    pris: Vec<ColPriState>,
    mods: Vec<AttackModState>,
    sounds: Vec<SoundEvent>,
    expressions: Vec<ExpressionEvent>,
    reverse_lrs: Vec<ReverseLrEvent>,
    speed_exs: Vec<SetSpeedExEvent>,
    speeds: Vec<SetSpeedEvent>,
    add_speed_no_limits: Vec<AddSpeedNoLimitEvent>,
    corrects: Vec<CorrectEvent>,
    /// Site to hand to the next hurtbox statement encountered.
    ///
    /// A *source* ordinal, not an execution counter: [`eval_stmts`] unrolls `for` bodies, and
    /// every iteration of a looped `HIT_NODE` is the same line in the file, so all of them must
    /// come back with the same site or an edit would land on whichever iteration was clicked.
    next_site: usize,
    /// Site for the next attack-modifier statement, counted separately — see [`AttackModSite`].
    next_mod_site: usize,
    /// Site for the next sound call, counted separately again — see [`SoundEvent::site`].
    next_sound_site: usize,
    /// Site for the next expression call, independent of every other family.
    next_expression_site: usize,
    /// Site for the next `REVERSE_LR`, independent of every other point-event family.
    next_reverse_lr_site: usize,
    /// Site for the next `SET_SPEED_EX`, independent of every other point-event family.
    next_speed_ex_site: usize,
    /// Site for the next `SET_SPEED`, independent of every other point-event family.
    next_speed_site: usize,
    /// Site for the next `ADD_SPEED_NO_LIMIT`, independent of every other point-event family.
    next_add_speed_no_limit_site: usize,
    /// Site for the next `CORRECT`, independent of every other point-event family.
    next_correct_site: usize,
}

/// Hurtbox statements in a subtree, counted in source order.
///
/// Used to step [`WalkAccum::next_site`] over a `for` body whose count is zero, so that a
/// statement *after* an empty loop still gets the ordinal a plain pre-order walk of the source
/// would give it. Without this the two definitions of "site" diverge exactly when a loop runs
/// no iterations, and the editor would resolve a site to the wrong line.
///
/// Counts [`AcmdStmt::RawBlock`] for the reason [`count_sound_stmts`] gives — [`eval_stmts`]
/// descends into a runtime branch, so a hurtbox inside one takes a site. This arm was missing
/// until B6, and the corpus could not have found it: **no** hurtbox statement anywhere in the
/// cache sits inside a raw block, against 26 sounds that do. It is reachable from a user's own
/// script, where an `if` around a `HIT_NODE` is nothing unusual.
fn count_hurt_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner.iter().filter(|s| is_hurt_stmt(s)).count(),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => count_hurt_stmts(body),
            _ => 0,
        })
        .sum()
}

/// Does this statement consume a hurtbox site?
///
/// `HIT_RESET_ALL` and `COL_NORMAL` take one even though they have no editable argument, so
/// that a site stays the ordinal of the statement rather than of the *editable* statement —
/// the second is a rule that changes meaning the moment a field is added.
fn is_hurt_stmt(stmt: &ExcuteStmt) -> bool {
    matches!(
        stmt,
        ExcuteStmt::HitStatus { .. }
            | ExcuteStmt::HitResetAll
            | ExcuteStmt::ColPri(_)
            | ExcuteStmt::ColNormal
    )
}

/// Does this statement consume an attack-modifier site?
fn is_attack_mod_stmt(stmt: &ExcuteStmt) -> bool {
    matches!(stmt, ExcuteStmt::AttackMod { .. })
}

/// Sound calls in a subtree, counted in source order.
///
/// Serves [`WalkAccum::next_sound_site`] the way [`count_hurt_stmts`] serves `next_site`, with
/// one difference that is not cosmetic: it counts [`AcmdStmt::Bare`] as well as
/// [`AcmdStmt::Excute`]. Fifteen corpus scripts write a sound outside every `is_excute` block,
/// so a count that saw only wrapped calls would under-count a body and mis-number every site
/// after the loop containing it.
///
/// [`AcmdStmt::RawBlock`] is counted for the same reason: [`eval_stmts`] walks a raw block's
/// body, so a sound inside one takes a site, and a count that disagreed with the walk is the
/// bug this function exists to prevent.
fn count_sound_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner.iter().filter(|s| is_sound_stmt(s)).count(),
            AcmdStmt::Bare(inner) => usize::from(is_sound_stmt(inner)),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_sound_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// Does this statement consume a sound site?
fn is_sound_stmt(stmt: &ExcuteStmt) -> bool {
    matches!(stmt, ExcuteStmt::Sound(_))
}

/// Does this statement consume an expression site?
fn is_expression_stmt(stmt: &ExcuteStmt) -> bool {
    matches!(stmt, ExcuteStmt::Expression(_))
}

/// Expression calls in a subtree, counted in source order for loop/site resolution.
fn count_expression_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner.iter().filter(|s| is_expression_stmt(s)).count(),
            AcmdStmt::Bare(inner) => usize::from(is_expression_stmt(inner)),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_expression_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// `REVERSE_LR` calls in a subtree, counted in source order for loop/site resolution.
fn count_reverse_lr_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner
                .iter()
                .filter(|s| matches!(s, ExcuteStmt::ReverseLr))
                .count(),
            AcmdStmt::Bare(inner) => usize::from(matches!(inner.as_ref(), ExcuteStmt::ReverseLr)),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_reverse_lr_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// `SET_SPEED_EX` calls in a subtree, counted in source order for loop/site resolution.
fn count_speed_ex_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner
                .iter()
                .filter(|s| matches!(s, ExcuteStmt::SetSpeedEx(_)))
                .count(),
            AcmdStmt::Bare(inner) => {
                usize::from(matches!(inner.as_ref(), ExcuteStmt::SetSpeedEx(_)))
            }
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_speed_ex_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// `SET_SPEED` calls in a subtree, counted in source order for loop/site resolution.
fn count_speed_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner
                .iter()
                .filter(|s| matches!(s, ExcuteStmt::SetSpeed(_)))
                .count(),
            AcmdStmt::Bare(inner) => usize::from(matches!(inner.as_ref(), ExcuteStmt::SetSpeed(_))),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_speed_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// `ADD_SPEED_NO_LIMIT` calls in a subtree, counted in source order for loop/site resolution.
fn count_add_speed_no_limit_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner
                .iter()
                .filter(|s| matches!(s, ExcuteStmt::AddSpeedNoLimit(_)))
                .count(),
            AcmdStmt::Bare(inner) => {
                usize::from(matches!(inner.as_ref(), ExcuteStmt::AddSpeedNoLimit(_)))
            }
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_add_speed_no_limit_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// `CORRECT` calls in a subtree, counted in source order for loop/site resolution.
fn count_correct_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner
                .iter()
                .filter(|s| matches!(s, ExcuteStmt::Correct(_)))
                .count(),
            AcmdStmt::Bare(inner) => usize::from(matches!(inner.as_ref(), ExcuteStmt::Correct(_))),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_correct_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

/// Attack-modifier statements in a subtree, counted in source order.
///
/// The [`count_hurt_stmts`] argument applies unchanged, including its `RawBlock` arm and the
/// reason that arm cannot be justified from the corpus: a zero-iteration `for` still has to step
/// the cursor over its body, or a statement after it resolves to the wrong line.
fn count_attack_mod_stmts(stmts: &[AcmdStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => inner.iter().filter(|s| is_attack_mod_stmt(s)).count(),
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                count_attack_mod_stmts(body)
            }
            _ => 0,
        })
        .sum()
}

impl WalkAccum {
    fn take_site(&mut self) -> usize {
        let site = self.next_site;
        self.next_site += 1;
        site
    }

    /// End every open span on `target` at `frame - 1`, then open a new one.
    ///
    /// `.max(active_start)` for the reason the id-scoped hitbox clear does it: a state set and
    /// replaced on the very next frame held for that one frame, not for none.
    fn set_status(&mut self, target: &HurtTarget, status: &str, frame: u32, site: usize) {
        let end = frame.saturating_sub(1);
        for open in self
            .states
            .iter_mut()
            .filter(|s| &s.target == target && s.active_end == u32::MAX)
        {
            open.active_end = end.max(open.active_start);
        }
        self.states.push(HurtboxState {
            target: target.clone(),
            status: status.to_string(),
            active_start: frame,
            active_end: u32::MAX,
            site,
        });
    }

    /// `HIT_RESET_ALL` — close every open hurtbox span, whatever its target.
    ///
    /// Deliberately does not touch [`pris`](Self::pris): resetting hit *status* is not the same
    /// call as restoring body collision, and folding them would invent an end frame the script
    /// never wrote.
    fn reset_all(&mut self, frame: u32) {
        let end = frame.saturating_sub(1);
        for open in self.states.iter_mut().filter(|s| s.active_end == u32::MAX) {
            open.active_end = end.max(open.active_start);
        }
    }

    fn set_pri(&mut self, pri: i64, frame: u32, site: usize) {
        self.close_pri(frame);
        self.pris.push(ColPriState {
            pri,
            active_start: frame,
            active_end: u32::MAX,
            site,
        });
    }

    fn close_pri(&mut self, frame: u32) {
        let end = frame.saturating_sub(1);
        for open in self.pris.iter_mut().filter(|p| p.active_end == u32::MAX) {
            open.active_end = end.max(open.active_start);
        }
    }

    fn take_mod_site(&mut self) -> usize {
        let site = self.next_mod_site;
        self.next_mod_site += 1;
        site
    }

    fn take_sound_site(&mut self) -> usize {
        let site = self.next_sound_site;
        self.next_sound_site += 1;
        site
    }

    fn take_expression_site(&mut self) -> usize {
        let site = self.next_expression_site;
        self.next_expression_site += 1;
        site
    }

    fn take_reverse_lr_site(&mut self) -> usize {
        let site = self.next_reverse_lr_site;
        self.next_reverse_lr_site += 1;
        site
    }

    fn take_speed_ex_site(&mut self) -> usize {
        let site = self.next_speed_ex_site;
        self.next_speed_ex_site += 1;
        site
    }

    fn take_speed_site(&mut self) -> usize {
        let site = self.next_speed_site;
        self.next_speed_site += 1;
        site
    }

    fn take_add_speed_no_limit_site(&mut self) -> usize {
        let site = self.next_add_speed_no_limit_site;
        self.next_add_speed_no_limit_site += 1;
        site
    }

    fn take_correct_site(&mut self) -> usize {
        let site = self.next_correct_site;
        self.next_correct_site += 1;
        site
    }

    /// Record a modifier. There is nothing to close: these are point events, and no macro takes
    /// one back, so a span here would be invented rather than read.
    fn add_mod(&mut self, kind: AttackModKind, id: i64, value: f32, frame: u32, site: usize) {
        self.mods.push(AttackModState {
            kind,
            id,
            value,
            frame,
            site,
        });
    }
}

/// Run one statement from inside an `is_excute` block, or one written bare beside it.
///
/// Split out of [`eval_stmts`] when [`AcmdStmt::Bare`] arrived so both routes share one
/// implementation. A second copy for the bare case would be a rule about what a command does
/// that depends on whether the author wrapped it, which is not a rule the game has.
fn eval_excute_stmt(s: &ExcuteStmt, frame: f32, hitboxes: &mut Vec<Hitbox>, hurt: &mut WalkAccum) {
    match s {
        ExcuteStmt::Attack(call) => {
            if let Some(existing) = hitboxes
                .iter_mut()
                .find(|h| h.id == call.id && h.active_end == u32::MAX)
            {
                existing.active_end = (script_frame(frame)).saturating_sub(1);
            }
            hitboxes.push(call.to_hitbox(script_frame(frame)));
        }
        ExcuteStmt::AttackFp(call) => {
            let spawn = script_frame(frame);
            if let Some(existing) = hitboxes.iter_mut().find(|h| {
                is_attack_category(h.category)
                    && h.id == call.int(0, 0).max(0) as u32
                    && h.active_end == u32::MAX
            }) {
                existing.active_end = spawn.saturating_sub(1).max(existing.active_start);
            }
            hitboxes.push(call.to_hitbox(spawn));
        }
        ExcuteStmt::Wind(wind) => {
            let spawn = script_frame(frame);
            if let Some(existing) = hitboxes.iter_mut().find(|hitbox| {
                hitbox.category == 2 && hitbox.id == wind.id() && hitbox.active_end >= spawn
            }) {
                existing.active_end = spawn.saturating_sub(1).max(existing.active_start);
            }
            hitboxes.push(wind.to_hitbox(spawn));
        }
        ExcuteStmt::EraseWind(id) => {
            let end = script_frame(frame).saturating_sub(1);
            for hitbox in hitboxes.iter_mut().filter(|hitbox| {
                hitbox.category == 2
                    && hitbox.id == *id
                    && hitbox.active_start <= end.saturating_add(1)
                    && hitbox.active_end >= end
            }) {
                hitbox.active_end = end.max(hitbox.active_start);
            }
        }
        ExcuteStmt::Catch(call) => {
            let spawn = script_frame(frame);
            // Reusing a grab id replaces the open one, the same way ATTACK does.
            if let Some(existing) = hitboxes.iter_mut().find(|hitbox| {
                hitbox.category == 1 && hitbox.id == call.id && hitbox.active_end == u32::MAX
            }) {
                existing.active_end = spawn.saturating_sub(1);
            }
            hitboxes.push(call.to_hitbox(spawn));
        }
        // AttackModule clears attack hitboxes only — a grab box survives it and
        // is ended by GrabModule::clear_all instead.
        ExcuteStmt::Clear(id) => {
            let end = script_frame(frame).saturating_sub(1);
            for hitbox in hitboxes.iter_mut().filter(|hitbox| {
                is_attack_category(hitbox.category)
                    && hitbox.id == *id
                    && hitbox.active_end == u32::MAX
            }) {
                hitbox.active_end = end.max(hitbox.active_start);
            }
        }
        // `.max(active_start)` for the same reason the id-scoped clear does it:
        // a collision that comes out and is cleared on the next `wait` is out
        // for that one frame, not for none. Without the clamp a hitbox spawned
        // before any `frame()` call ends up ending the frame before it starts,
        // and the timeline draws nothing at all.
        ExcuteStmt::ClearAll => {
            let end = script_frame(frame).saturating_sub(1);
            for hb in hitboxes
                .iter_mut()
                .filter(|hitbox| is_attack_category(hitbox.category))
            {
                if hb.active_end == u32::MAX {
                    hb.active_end = end.max(hb.active_start);
                }
            }
        }
        ExcuteStmt::GrabClearAll => {
            let end = script_frame(frame).saturating_sub(1);
            for hb in hitboxes.iter_mut().filter(|hitbox| hitbox.category == 1) {
                if hb.active_end == u32::MAX {
                    hb.active_end = end.max(hb.active_start);
                }
            }
        }
        // Identity is (id, kind), NOT id alone. Every one of the corpus's 32
        // calls writes id `0`, and kirby/ThrowF puts two in a single block —
        // one `..._THROW` and one `..._CATCH` — which are both live at once and
        // say what happens on two different outcomes. Matching on id would
        // have ended the first the instant the second was read.
        //
        // Deliberately not ended by `AttackModule::clear_all`: only 2 of the 24
        // scripts using this macro contain one at all, and there is no evidence
        // it applies. An uncleared call runs to the end of the move, which is
        // the same `9999` an uncleared hitbox gets — better than inventing a
        // frame the script never wrote.
        ExcuteStmt::AttackAbs(call) => {
            let spawn = script_frame(frame);
            if let Some(existing) = hitboxes.iter_mut().find(|h| {
                h.category == CAT_ABS
                    && h.id == call.id
                    && h.abs.as_ref().is_some_and(|a| a.kind == call.kind)
                    && h.active_end == u32::MAX
            }) {
                existing.active_end = spawn.saturating_sub(1).max(existing.active_start);
            }
            hitboxes.push(call.to_hitbox(spawn));
        }
        // Deliberately not ended by any clear. `AttackModule::clear_all` does
        // not touch a search volume, and none of the 7 corpus scripts contains
        // a clear of any kind for one — the two that do end (kirby's inhale)
        // end it from the status code, which no ACMD script can see. So an
        // unclosed search runs to the end of the move, the same `9999` an
        // unclosed `ATTACK_ABS` gets, rather than a frame nothing ever wrote.
        //
        // Reusing an id still replaces the open box, matching every other
        // family. Scoped to `CAT_SEARCH`: kirby/SpecialNStart opens a `CATCH`,
        // a `SEARCH` and an `ATTACK_ABS` that all three carry id 0 in one
        // block, so a match on id alone would close two of them here.
        ExcuteStmt::Search(call) => {
            let spawn = script_frame(frame);
            if let Some(existing) = hitboxes.iter_mut().find(|hitbox| {
                hitbox.category == CAT_SEARCH
                    && hitbox.id == call.id
                    && hitbox.active_end == u32::MAX
            }) {
                existing.active_end = spawn.saturating_sub(1).max(existing.active_start);
            }
            hitboxes.push(call.to_hitbox(spawn));
        }
        ExcuteStmt::HitStatus { target, status } => {
            let site = hurt.take_site();
            hurt.set_status(target, status, script_frame(frame), site);
        }
        ExcuteStmt::HitResetAll => {
            hurt.take_site();
            hurt.reset_all(script_frame(frame));
        }
        ExcuteStmt::ColPri(pri) => {
            let site = hurt.take_site();
            hurt.set_pri(*pri, script_frame(frame), site);
        }
        ExcuteStmt::ColNormal => {
            hurt.take_site();
            hurt.close_pri(script_frame(frame));
        }
        ExcuteStmt::AttackMod { kind, id, value } => {
            let site = hurt.take_mod_site();
            hurt.add_mod(*kind, *id, *value, script_frame(frame), site);
        }
        ExcuteStmt::Sound(call) => {
            let site = hurt.take_sound_site();
            hurt.sounds.push(SoundEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::Expression(call) => {
            let site = hurt.take_expression_site();
            hurt.expressions.push(ExpressionEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::ReverseLr => {
            let site = hurt.take_reverse_lr_site();
            hurt.reverse_lrs.push(ReverseLrEvent {
                frame: script_frame(frame),
                site,
            });
        }
        ExcuteStmt::SetSpeedEx(call) => {
            let site = hurt.take_speed_ex_site();
            hurt.speed_exs.push(SetSpeedExEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::SetSpeed(call) => {
            let site = hurt.take_speed_site();
            hurt.speeds.push(SetSpeedEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::AddSpeedNoLimit(call) => {
            let site = hurt.take_add_speed_no_limit_site();
            hurt.add_speed_no_limits.push(AddSpeedNoLimitEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::Correct(call) => {
            let site = hurt.take_correct_site();
            hurt.corrects.push(CorrectEvent {
                frame: script_frame(frame),
                call: call.clone(),
                site,
            });
        }
        ExcuteStmt::Raw(_) => {}
    }
}

fn eval_stmts(
    stmts: &[AcmdStmt],
    start_frame: f32,
    hitboxes: &mut Vec<Hitbox>,
    hurt: &mut WalkAccum,
) -> f32 {
    let mut frame = start_frame;
    for stmt in stmts {
        match stmt {
            AcmdStmt::Frame(f) => frame = *f,
            AcmdStmt::Wait(w) => frame += w,
            // `MotionRate` is deliberately inert here. This walk resolves the frames a script
            // *names*, which are motion frames, and every hitbox range it produces is keyed to
            // them. Rate converts motion frames to game frames, which is a separate mapping —
            // see [`game_frame_spans`] — and applying it here would move every hitbox to a
            // frame its own source does not mention.
            AcmdStmt::WaitLoopClear | AcmdStmt::Raw(_) | AcmdStmt::MotionRate(_) => {}
            AcmdStmt::Excute(stmts) => {
                for s in stmts {
                    eval_excute_stmt(s, frame, hitboxes, hurt);
                }
            }
            // A bare command runs where it is written, on the frame the cursor is on — the
            // `is_excute` wrapper decides whether a command runs at all, never when.
            AcmdStmt::Bare(s) => eval_excute_stmt(s, frame, hitboxes, hurt),
            AcmdStmt::Loop { count, body } => {
                // Rewind the site cursor for every iteration so all of them agree, then step it
                // over the body once regardless of how many iterations actually ran.
                let site_at_entry = hurt.next_site;
                let mod_site_at_entry = hurt.next_mod_site;
                let sound_site_at_entry = hurt.next_sound_site;
                let expression_site_at_entry = hurt.next_expression_site;
                let reverse_lr_site_at_entry = hurt.next_reverse_lr_site;
                let speed_ex_site_at_entry = hurt.next_speed_ex_site;
                let speed_site_at_entry = hurt.next_speed_site;
                let add_speed_no_limit_site_at_entry = hurt.next_add_speed_no_limit_site;
                let correct_site_at_entry = hurt.next_correct_site;
                for _ in 0..*count {
                    hurt.next_site = site_at_entry;
                    hurt.next_mod_site = mod_site_at_entry;
                    hurt.next_sound_site = sound_site_at_entry;
                    hurt.next_expression_site = expression_site_at_entry;
                    hurt.next_reverse_lr_site = reverse_lr_site_at_entry;
                    hurt.next_speed_ex_site = speed_ex_site_at_entry;
                    hurt.next_speed_site = speed_site_at_entry;
                    hurt.next_add_speed_no_limit_site = add_speed_no_limit_site_at_entry;
                    hurt.next_correct_site = correct_site_at_entry;
                    frame = eval_stmts(body, frame, hitboxes, hurt);
                }
                hurt.next_site = site_at_entry + count_hurt_stmts(body);
                hurt.next_mod_site = mod_site_at_entry + count_attack_mod_stmts(body);
                hurt.next_sound_site = sound_site_at_entry + count_sound_stmts(body);
                hurt.next_expression_site = expression_site_at_entry + count_expression_stmts(body);
                hurt.next_reverse_lr_site = reverse_lr_site_at_entry + count_reverse_lr_stmts(body);
                hurt.next_speed_ex_site = speed_ex_site_at_entry + count_speed_ex_stmts(body);
                hurt.next_speed_site = speed_site_at_entry + count_speed_stmts(body);
                hurt.next_add_speed_no_limit_site =
                    add_speed_no_limit_site_at_entry + count_add_speed_no_limit_stmts(body);
                hurt.next_correct_site = correct_site_at_entry + count_correct_stmts(body);
            }
            // Walked as though the branch always runs, which is what happened before it was a
            // block at all: its lines used to be parsed as siblings of the branch, so a hitbox
            // inside an `if` has always shown in the editor unconditionally. Eighteen `game_`
            // scripts in the corpus place an `ATTACK` this way. Keeping that behaviour is the
            // point — the fix here is to the *brace*, not to the condition, and changing both
            // at once would leave neither tested.
            AcmdStmt::RawBlock { body, .. } => {
                frame = eval_stmts(body, frame, hitboxes, hurt);
            }
        }
    }
    frame
}

// ── Fighter / App state ───────────────────────────────────────────────────────

/// A saved edit for one fighter+move combination.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EditRecord {
    pub fighter: String,
    pub fighter_display: String,
    pub move_name: String,
    pub script: AcmdScript,
    /// Pristine hitboxes used to derive sparse live rules after a project is reopened.
    #[serde(default)]
    pub hitboxes_pristine: Vec<Hitbox>,
    pub hitboxes: Vec<Hitbox>,
}

/// Persistent log of all edits, keyed fighter_name → move_name → record.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EditLog {
    /// fighter_name → move_name → record
    pub entries: HashMap<String, HashMap<String, EditRecord>>,
}

impl EditLog {
    pub fn save(
        &mut self,
        fighter: &str,
        fighter_display: &str,
        move_name: &str,
        script: AcmdScript,
        hitboxes_pristine: Vec<Hitbox>,
        hitboxes: Vec<Hitbox>,
    ) {
        self.entries.entry(fighter.to_string()).or_default().insert(
            move_name.to_string(),
            EditRecord {
                fighter: fighter.to_string(),
                fighter_display: fighter_display.to_string(),
                move_name: move_name.to_string(),
                script,
                hitboxes_pristine,
                hitboxes,
            },
        );
    }

    pub fn remove_move(&mut self, fighter: &str, move_name: &str) {
        if let Some(moves) = self.entries.get_mut(fighter) {
            moves.remove(move_name);
            if moves.is_empty() {
                self.entries.remove(fighter);
            }
        }
    }

    pub fn remove_fighter(&mut self, fighter: &str) {
        self.entries.remove(fighter);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sorted list of (fighter_name, fighter_display) pairs.
    pub fn fighters_sorted(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(k, moves)| {
                let display = moves
                    .values()
                    .next()
                    .map(|r| r.fighter_display.clone())
                    .unwrap_or_else(|| k.clone());
                (k.clone(), display)
            })
            .collect();
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    }

    /// Sorted move names for a fighter.
    pub fn moves_for(&self, fighter: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .entries
            .get(fighter)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// Where a fighter was indexed from. Modded fighters (added-character mods) come from a
/// user-added mod root and may be missing pieces a vanilla dump always has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FighterSource {
    /// Indexed out of the main game-data root.
    DataRoot,
    /// Indexed out of an extra mod root the user pointed the tool at.
    ModRoot,
}

#[derive(Debug, Clone)]
pub struct FighterEntry {
    pub name: String,
    pub display_name: String,
    #[allow(dead_code)]
    pub param_path: PathBuf,
    pub motion_dir: PathBuf,
    pub model_dir: PathBuf,
    pub effect_dir: Option<PathBuf>,
    /// Costume slots that actually exist on disk for this fighter, ascending. Vanilla
    /// fighters yield 0..=7; mods add c08+ and community slot packs go well past that.
    pub slots: Vec<u8>,
    /// The directory this fighter's own files live in (`<root>/fighter/<name>`), so slot
    /// rescans and modded-fighter lookups don't have to re-derive it from the data root.
    pub fighter_dir: PathBuf,
    pub source: FighterSource,
}

impl FighterEntry {
    /// Lowest existing costume slot — the one whose model/motion dirs represent the
    /// fighter. Vanilla is always c00, but a mod may ship only c08+.
    pub fn base_slot(&self) -> u8 {
        self.slots.first().copied().unwrap_or(0)
    }

    /// True when this fighter is not part of the vanilla roster.
    pub fn is_modded(&self) -> bool {
        self.source == FighterSource::ModRoot || !VANILLA_FIGHTERS.contains(&self.name.as_str())
    }
}

pub struct AppState {
    pub data_root: Option<PathBuf>,
    pub fighters: Vec<FighterEntry>,
    pub labels: HashMap<u64, String>,
    pub selected_fighter: Option<usize>,
    pub selected_move: Option<MoveEntry>,
    pub hitboxes: Vec<Hitbox>,
    pub script: AcmdScript,
    pub current_frame: u32,
    pub total_frames: u32,
    pub playing: bool,
    pub status: String,
    pub edit_log: EditLog,
    pub effect_script: EffectScript,
    pub effects: Vec<EffectCall>,
    /// Pristine copy of `effects` as parsed from ACMD, before user edits — "orig" ghosts.
    pub effects_pristine: Vec<EffectCall>,
    /// The current move's `sound_` function, kept whole.
    ///
    /// Held apart from [`script`](Self::script), which is the `game_` function: they are two
    /// different functions in the file, and an edit to one must not re-emit the other. An edit
    /// goes through [`AcmdScript::sound_stmt_mut`] into this, exactly as a hurtbox edit goes
    /// into `script` — so the script stays the one source of truth and the list below is only
    /// ever a view of it.
    pub sound_script: AcmdScript,
    /// The current move's sounds: one event per call, at the frame it fires.
    ///
    /// A *view* of [`sound_script`](Self::sound_script), refreshed from it after every edit
    /// rather than mutated in place — nothing writes here that did not write to the script
    /// first. Cached rather than re-derived per frame because five call sites on the draw path
    /// read it, and each walk unrolls the script's loops.
    pub sounds: Vec<SoundEvent>,
    /// The same list as loaded, before any edit — what write-back diffs against.
    pub sounds_pristine: Vec<SoundEvent>,
    /// The current move's `expression_` function, kept whole alongside `sound_script`.
    pub expression_script: AcmdScript,
    /// Resolved camera/rumble events shown by the expression lane.
    pub expressions: Vec<ExpressionEvent>,
    /// The same expression events as loaded, before edits.
    pub expressions_pristine: Vec<ExpressionEvent>,
    /// Hitboxes as loaded (GitHub fetch or live capture) — live hitbox rules diff vs this.
    pub hitboxes_pristine: Vec<Hitbox>,
    /// Hurtbox spans as loaded, for source syncing to diff against.
    ///
    /// Unlike hitboxes there is no edited copy beside this one: hurtbox statements are carried
    /// through [`script`](Self::script) rather than rebuilt from a list, so the script itself is
    /// the edited model and the current spans are always `script.to_hurtboxes()`.
    pub hurtboxes_pristine: (Vec<HurtboxState>, Vec<ColPriState>),
    /// Post-hoc hitbox modifiers as loaded, on the same terms as `hurtboxes_pristine`: the
    /// script is the edited model, so the current list is always `script.to_attack_mods()`.
    pub attack_mods_pristine: Vec<AttackModState>,
    /// `REVERSE_LR` point events as loaded, for sparse live suppression/injection rules.
    pub reverse_lr_pristine: Vec<ReverseLrEvent>,
    /// `SET_SPEED_EX` point events as loaded, for sparse live velocity rules and source syncing.
    pub speed_ex_pristine: Vec<SetSpeedExEvent>,
    /// `SET_SPEED` point events as loaded, for sparse live velocity rules and source syncing.
    pub speed_pristine: Vec<SetSpeedEvent>,
    /// `ADD_SPEED_NO_LIMIT` point events as loaded, for sparse live velocity rules and source
    /// syncing.
    pub add_speed_no_limit_pristine: Vec<AddSpeedNoLimitEvent>,
    /// `CORRECT` point events as loaded, for sparse live correction rules and source syncing.
    pub correct_pristine: Vec<CorrectEvent>,
    /// Provenance of the current move's ACMD data ("", "GitHub", "Live capture").
    pub acmd_source: String,
    /// "fighter/move" → the warning captured when a live performance observed only one arm of
    /// a branched source script. Kept separately from `acmd_source` so a later project export can
    /// carry the warning even after the move is no longer open.
    pub capture_branch_warnings: HashMap<String, String>,
    /// The text every panel above was built from: the project's own functions where it has
    /// them, the mirror's for the categories it does not.
    ///
    /// Kept because write-back needs it to *create* a category the project lacks. The function
    /// it writes has to be the mirror's own text rather than something regenerated from the IR,
    /// so the text has to survive the parse that produced the IR.
    pub loaded_body: String,
    /// User edits to effect calls, keyed by "fighter/move" (indices into pristine order).
    pub effect_call_edits: HashMap<String, Vec<EffectCallEdit>>,
    /// Full edited call list snapshot per "fighter/move" — what the mod exporter emits
    /// (a generated effect script replaces the whole move's spawn list).
    pub effect_call_full: HashMap<String, Vec<EffectCall>>,
    /// What the effect export would throw away, per "fighter/move" — the companion to
    /// `effect_call_full` and written at the same moments, because the script both are derived
    /// from is in hand only then.
    ///
    /// A move is present only if it was parsed from a script *and* lost something. Absent
    /// therefore means "captured live, or lost nothing"; the two are indistinguishable here and
    /// deliberately so, since neither produces a finding.
    pub effect_dropped_lines: HashMap<String, Vec<String>>,
    /// "fighter/move" → the effect lines that belong to a *frame* rather than to any one call.
    ///
    /// The exact sibling of `effect_dropped_lines` above, written at the same moment from the
    /// same parse, and stored for the same reason: it cannot be derived later, because the
    /// export regenerates the function from `effect_call_full` and these lines are in neither
    /// the calls nor — before E3 — the output.
    ///
    /// The difference is what happens to it. A dropped line is *reported*; one of these is
    /// *emitted*, at the frame it is keyed by. So this one does change generated code, and a
    /// move missing from here exports exactly as it did before E3 — the lines vanish. That is
    /// why it is threaded all the way to the emitter rather than consulted at the end.
    pub effect_frame_residue: HashMap<String, std::collections::BTreeMap<u32, Vec<String>>>,
    /// Edited `sound_` script per "fighter/move" — what the mod exporter emits, on the same
    /// terms as `effect_call_full`: whole scripts, because an installed one replaces the
    /// fighter's own. A move is in here only once its sounds have actually been changed.
    pub sound_script_edits: HashMap<String, AcmdScript>,
    /// Edited `expression_` script per "fighter/move" — the complete category script the
    /// generated plugin installs.
    pub expression_script_edits: HashMap<String, AcmdScript>,
    pub selected_effect_call: Option<usize>,
    /// Effects panel: show every call, not just the ones active on the current frame.
    pub show_all_effect_calls: bool,
    pub show_effects_panel: bool,
}

#[derive(Debug, Clone)]
pub struct MoveEntry {
    pub name: String,
    pub hash: u64,
    pub frame_count: u32,
    pub anim_path: Option<PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            data_root: None,
            fighters: Vec::new(),
            labels: HashMap::new(),
            selected_fighter: None,
            selected_move: None,
            hitboxes: Vec::new(),
            script: AcmdScript::default(),
            // ACMD/game frames are one-based. Animation sampling converts this to its
            // zero-based frame index at the renderer boundary.
            current_frame: 1,
            total_frames: 0,
            playing: false,
            status: "Select a data root directory to begin.".to_string(),
            edit_log: EditLog::default(),
            effect_script: EffectScript::default(),
            effects: Vec::new(),
            effects_pristine: Vec::new(),
            sound_script: AcmdScript::default(),
            sounds: Vec::new(),
            sounds_pristine: Vec::new(),
            expression_script: AcmdScript::default(),
            expressions: Vec::new(),
            expressions_pristine: Vec::new(),
            hitboxes_pristine: Vec::new(),
            hurtboxes_pristine: (Vec::new(), Vec::new()),
            attack_mods_pristine: Vec::new(),
            reverse_lr_pristine: Vec::new(),
            speed_ex_pristine: Vec::new(),
            speed_pristine: Vec::new(),
            add_speed_no_limit_pristine: Vec::new(),
            correct_pristine: Vec::new(),
            acmd_source: String::new(),
            capture_branch_warnings: HashMap::new(),
            loaded_body: String::new(),
            effect_call_edits: HashMap::new(),
            effect_call_full: HashMap::new(),
            effect_dropped_lines: HashMap::new(),
            effect_frame_residue: HashMap::new(),
            sound_script_edits: HashMap::new(),
            expression_script_edits: HashMap::new(),
            selected_effect_call: None,
            show_all_effect_calls: false,
            show_effects_panel: false,
        }
    }
}

impl AppState {
    /// Install a freshly loaded script and re-baseline the hurtbox spans it resolves to.
    ///
    /// A method rather than two assignments at each of the four call sites, so the baseline
    /// cannot drift out of step with the script it is the baseline *of* — which would make
    /// source syncing diff a move's hurtboxes against a different move's.
    pub fn set_script(&mut self, script: AcmdScript) {
        self.hurtboxes_pristine = script.to_hurtboxes();
        self.attack_mods_pristine = script.to_attack_mods();
        self.reverse_lr_pristine = script.to_reverse_lr_events();
        self.speed_ex_pristine = script.to_speed_ex_events();
        self.speed_pristine = script.to_speed_events();
        self.add_speed_no_limit_pristine = script.to_add_speed_no_limit_events();
        self.correct_pristine = script.to_correct_events();
        self.script = script;
    }
}

// ── Costume slot discovery ───────────────────────────────────────────────────
//
// Vanilla fighters have exactly 8 costume slots (c00–c07), but slot-add mods routinely go
// past that and community slot packs use large indices. Nothing here assumes a count: slots
// are whatever the filesystem says exists. The slot index type is `u8` throughout, which is
// the game's own domain — the runtime colour index is a small integer and Arcropolis-style
// slot mods top out at c255 — so indices above 255 are rejected as "not a costume dir"
// rather than silently truncating into a valid-looking slot.

/// Vanilla costume slot count. Used only as the fallback when a fighter's directories cannot
/// be scanned, so a tool run without a readable data root behaves exactly as it always did.
pub const VANILLA_SLOT_COUNT: u8 = 8;

/// The vanilla 0..=7 slot list — the fallback when discovery finds nothing.
pub fn default_slots() -> Vec<u8> {
    (0..VANILLA_SLOT_COUNT).collect()
}

/// `"c00" → 0`, `"c08" → 8`, `"c113" → 113`. Rejects anything that is not `c` followed by
/// only digits, and anything that would not fit a costume index (`> 255`).
///
/// Note `"c0"` and `"c000"` both parse: mods are inconsistent about zero padding, and the
/// game resolves by index, not by the literal directory string.
pub fn parse_costume_dir(name: &str) -> Option<u8> {
    let digits = name.strip_prefix('c')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Parse into u32 first so "c9999" is rejected as out-of-range rather than wrapping.
    digits
        .parse::<u32>()
        .ok()
        .and_then(|v| u8::try_from(v).ok())
}

/// Costume slots a model part (`fighter/<name>/model/<part>`) actually ships, ascending.
///
/// Falls back to the vanilla 0..=7 list when the directory holds no recognisable `cNN`
/// subdirectory, matching the rest of the discovery code.
pub fn part_costume_slots(part_dir: &std::path::Path) -> Vec<u8> {
    let mut found: Vec<u8> = std::fs::read_dir(part_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| parse_costume_dir(e.file_name().to_str()?))
        .collect();
    if found.is_empty() {
        return default_slots();
    }
    found.sort_unstable();
    found
}

/// Locate a model part's `model.nusktb`, preferring `preferred_slot` and otherwise taking the
/// lowest slot the part actually has.
///
/// Weapon parts routinely carry a different slot set from the body, and a modded fighter may
/// ship no `c00` at all — hardcoding `c00` silently dropped that fighter's weapons.
pub fn find_part_skel(part_dir: &std::path::Path, preferred_slot: u8) -> Option<PathBuf> {
    let preferred = part_dir
        .join(format!("c{preferred_slot:02}"))
        .join("model.nusktb");
    if preferred.exists() {
        return Some(preferred);
    }
    part_costume_slots(part_dir)
        .iter()
        .map(|s| part_dir.join(format!("c{s:02}")).join("model.nusktb"))
        .find(|p| p.exists())
}

/// Slot index encoded in an eff file name, e.g. `("ef_mario_c08", "mario") → 8`.
/// The base `ef_mario` (no suffix) is not slot-scoped and yields `None`.
pub fn slot_from_eff_stem(stem: &str, fighter: &str) -> Option<u8> {
    let rest = stem.strip_prefix(&format!("ef_{fighter}_")).or_else(|| {
        stem.strip_prefix("ef_")
            .and_then(|s| s.split_once('_').map(|x| x.1))
    })?;
    parse_costume_dir(rest)
}

/// Costume slot indices found directly under `dir` (each `cNN` subdirectory).
fn slots_in_costume_parent(dir: &std::path::Path) -> Vec<u8> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| parse_costume_dir(e.file_name().to_str()?))
        .collect()
}

/// Every costume slot that exists for `fighter`, unioned across all `roots` (the game-data
/// root plus any mod roots), ascending and deduplicated.
///
/// Four independent signals are unioned, because a slot mod may add only some of them:
///   * `<root>/fighter/<f>/model/<part>/cNN`   — the usual definition of a costume slot
///   * `<root>/fighter/<f>/motion/<part>/cNN`  — motion-only slots
///   * `<root>/effect/fighter/<f>/ef_<f>_cNN.eff` — one-slot effect files
///   * `<root>/fighter/<f>/<other>/cNN`        — e.g. sound/camera slot dirs
///
/// Returns an empty vec when nothing is found; callers decide whether to fall back to the
/// vanilla 0..=7 (see [`default_slots`]).
pub fn discover_costume_slots(roots: &[PathBuf], fighter: &str) -> Vec<u8> {
    let mut found: Vec<u8> = Vec::new();
    for root in roots {
        let fighter_dir = root.join("fighter").join(fighter);
        // model/ and motion/ hold per-part subdirs (body, sword, …), each containing cNN.
        for group in ["model", "motion"] {
            let group_dir = fighter_dir.join(group);
            if let Ok(parts) = std::fs::read_dir(&group_dir) {
                for part in parts.flatten() {
                    if part.path().is_dir() {
                        found.extend(slots_in_costume_parent(&part.path()));
                    }
                }
            }
        }
        // Some layouts (sound, camera, and hand-made mod folders) put cNN one level up.
        found.extend(slots_in_costume_parent(&fighter_dir));

        // Slot-scoped eff files: effect/fighter/<f>/ef_<f>_cNN.eff
        let effect_dir = root.join("effect").join("fighter").join(fighter);
        if let Ok(entries) = std::fs::read_dir(&effect_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("eff") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Some(slot) = slot_from_eff_stem(stem, fighter) {
                        found.push(slot);
                    }
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// [`discover_costume_slots`] with the vanilla 0..=7 fallback applied — what the UI and the
/// exporter use, so an unscannable fighter still offers the slots it always did.
pub fn costume_slots_or_default(roots: &[PathBuf], fighter: &str) -> Vec<u8> {
    let found = discover_costume_slots(roots, fighter);
    if found.is_empty() {
        default_slots()
    } else {
        found
    }
}

/// A fighter directory is loadable when it carries a param prc (the historical gate, kept so
/// no fighter that used to be indexed can disappear) OR the motion data the move list is
/// actually built from.
///
/// The motion arm is what added-character mods need: they frequently ship without a param
/// prc, and gating on param alone made them invisible to the fighter list. The slot list is
/// consulted rather than assuming c00, because a mod may ship only c08+.
pub fn fighter_dir_is_loadable(fighter_dir: &std::path::Path, slots: &[u8]) -> bool {
    if fighter_dir.join("param").join("vl.prc").exists()
        || fighter_dir.join("param").join("fighter_param.prc").exists()
    {
        return true;
    }
    let candidates: Vec<u8> = if slots.is_empty() {
        default_slots()
    } else {
        slots.to_vec()
    };
    candidates.iter().any(|slot| {
        fighter_dir
            .join("motion")
            .join("body")
            .join(format!("c{slot:02}"))
            .join("motion_list.bin")
            .exists()
    })
}

/// Every fighter directory name shipped by the vanilla game, including the sub-fighters and
/// bosses the roster list filters out. Anything outside this set is an added-character mod.
/// Used only to LABEL fighters as modded — indexing itself is purely directory-driven, so a
/// name missing here still loads.
pub const VANILLA_FIGHTERS: &[&str] = &[
    "bayonetta",
    "brave",
    "buddy",
    "captain",
    "chrom",
    "cloud",
    "common",
    "crazy",
    "daisy",
    "dedede",
    "demon",
    "diddy",
    "dolly",
    "donkey",
    "duckhunt",
    "edge",
    "eflame",
    "elight",
    "element",
    "falco",
    "fox",
    "gamewatch",
    "ganon",
    "gaogaen",
    "gekkouga",
    "ice_climber",
    "ike",
    "inkling",
    "jack",
    "kamui",
    "ken",
    "kirby",
    "koopa",
    "koopag",
    "koopajr",
    "krool",
    "link",
    "littlemac",
    "lucario",
    "lucas",
    "lucina",
    "luigi",
    "mario",
    "mariod",
    "marth",
    "master",
    "metaknight",
    "mewtwo",
    "miienemyf",
    "miienemyg",
    "miienemys",
    "miifighter",
    "miigunner",
    "miisword",
    "miiswordsman",
    "murabito",
    "nana",
    "ness",
    "packun",
    "pacman",
    "palutena",
    "peach",
    "pfushigisou",
    "pichu",
    "pickel",
    "pikachu",
    "pikmin",
    "pit",
    "pitb",
    "plizardon",
    "popo",
    "ptrainer",
    "ptrainer_low",
    "purin",
    "pzenigame",
    "reflet",
    "richter",
    "ridley",
    "robot",
    "rockman",
    "rosetta",
    "roy",
    "ryu",
    "samus",
    "samusd",
    "sheik",
    "shizue",
    "shulk",
    "simon",
    "snake",
    "sonic",
    "szerosuit",
    "tantan",
    "toonlink",
    "trail",
    "wario",
    "wiifit",
    "wolf",
    "yoshi",
    "younglink",
    "zelda",
    "zenigame",
];

pub fn fighter_display_name(name: &str) -> String {
    let map: &[(&str, &str)] = &[
        ("bayonetta", "Bayonetta"),
        ("brave", "Hero"),
        ("buddy", "Banjo & Kazooie"),
        ("captain", "Captain Falcon"),
        ("chrom", "Chrom"),
        ("cloud", "Cloud"),
        ("daisy", "Daisy"),
        ("dedede", "King Dedede"),
        ("demon", "Kazuya"),
        ("diddy", "Diddy Kong"),
        ("dolly", "Terry"),
        ("donkey", "Donkey Kong"),
        ("duckhunt", "Duck Hunt"),
        ("edge", "Sephiroth"),
        ("eflame", "Pyra"),
        ("elight", "Mythra"),
        ("element", "Aegis"),
        ("falco", "Falco"),
        ("fox", "Fox"),
        ("gamewatch", "Mr. Game & Watch"),
        ("ganon", "Ganondorf"),
        ("gaogaen", "Incineroar"),
        ("gekkouga", "Greninja"),
        ("ice_climber", "Ice Climbers"),
        ("ike", "Ike"),
        ("inkling", "Inkling"),
        ("jack", "Joker"),
        ("kamui", "Corrin"),
        ("ken", "Ken"),
        ("kirby", "Kirby"),
        ("koopa", "Bowser"),
        ("koopajr", "Bowser Jr."),
        ("krool", "King K. Rool"),
        ("link", "Link"),
        ("littlemac", "Little Mac"),
        ("lucario", "Lucario"),
        ("lucas", "Lucas"),
        ("lucina", "Lucina"),
        ("luigi", "Luigi"),
        ("mario", "Mario"),
        ("mariod", "Dr. Mario"),
        ("marth", "Marth"),
        ("metaknight", "Meta Knight"),
        ("mewtwo", "Mewtwo"),
        ("miifighter", "Mii Brawler"),
        ("miigunner", "Mii Gunner"),
        ("miisword", "Mii Swordfighter"),
        ("miiswordsman", "Mii Swordfighter"),
        ("murabito", "Villager"),
        ("ness", "Ness"),
        ("packun", "Piranha Plant"),
        ("pacman", "Pac-Man"),
        ("palutena", "Palutena"),
        ("peach", "Peach"),
        ("pfushigisou", "Ivysaur"),
        ("pichu", "Pichu"),
        ("pickel", "Steve"),
        ("pikachu", "Pikachu"),
        ("pikmin", "Olimar"),
        ("pit", "Pit"),
        ("pitb", "Dark Pit"),
        ("plizardon", "Charizard"),
        ("purin", "Jigglypuff"),
        ("pzenigame", "Squirtle"),
        ("reflet", "Robin"),
        ("richter", "Richter"),
        ("ridley", "Ridley"),
        ("robot", "R.O.B."),
        ("rockman", "Mega Man"),
        ("rosetta", "Rosalina"),
        ("roy", "Roy"),
        ("ryu", "Ryu"),
        ("samusd", "Dark Samus"),
        ("samus", "Samus"),
        ("sheik", "Sheik"),
        ("shizue", "Isabelle"),
        ("shulk", "Shulk"),
        ("simon", "Simon"),
        ("snake", "Snake"),
        ("sonic", "Sonic"),
        ("szerosuit", "Zero Suit Samus"),
        ("tantan", "Min Min"),
        ("toonlink", "Toon Link"),
        ("trail", "Sora"),
        ("wario", "Wario"),
        ("wiifit", "Wii Fit Trainer"),
        ("wolf", "Wolf"),
        ("yoshi", "Yoshi"),
        ("younglink", "Young Link"),
        ("zelda", "Zelda"),
        ("zenigame", "Squirtle"),
    ];
    for (k, v) in map {
        if *k == name {
            return v.to_string();
        }
    }
    let mut c = name.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// ── Effect script IR ──────────────────────────────────────────────────────────

/// A single effect macro call inside an is_excute block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EffectMacro {
    /// EFFECT / EFFECT_FOLLOW / EFFECT_FOLLOW_FLIP / EFFECT_FLIP /
    /// FOOT_EFFECT / LANDING_EFFECT — all share the same data shape.
    Effect {
        effect_name: String,
        /// Second graphic used by FLIP variants (left/right or facing alternatives).
        #[serde(default)]
        effect_name_alt: Option<String>,
        /// Exact sv_animcmd spawn function, retained for live replay and editing.
        #[serde(default = "default_effect_spawn_func")]
        spawn_func: String,
        bone_name: String,
        offset: [f32; 3],
        rotation: [f32; 3],
        scale: f32,
        /// `true` for EFFECT_FOLLOW / EFFECT_FOLLOW_FLIP / EFFECT_FLIP variants.
        follows_bone: bool,
        /// Every argument after `scale`, verbatim from the source call.
        ///
        /// The spawn families differ only past the shared transform block: alpha, colour,
        /// random ranges, contact flags. Keeping them as text is what lets an export
        /// reproduce the caller's own macro instead of falling back to plain `EFFECT`,
        /// without this code having to know all two dozen signatures.
        #[serde(default)]
        extra_args: Vec<String>,
    },
    /// AFTER_IMAGE4_ON / AFTER_IMAGE_ON — sword/weapon trail effects.
    AfterImage {
        effect_name: String,
        bone_name: String,
        /// The trail's second edge joint. See [`EffectCall::trail_bone2`] for why it is
        /// separate from `bone_name` and why it is optional.
        #[serde(default)]
        bone_name2: Option<String>,
        /// The call verbatim. A trail's arguments are textures and per-frame trail
        /// parameters, not a transform, so there is nothing to recompose it from.
        #[serde(default)]
        raw: String,
    },
    /// AFTER_IMAGE_OFF — turns off a sword trail.
    ///
    /// Carries its one argument. `macros::AFTER_IMAGE_OFF` is declared
    /// `<F: ToF32>(agent, unk: F)`, so a call written without it does not compile — which is
    /// exactly what the export used to emit. The value is undocumented beyond `unk`, and the
    /// corpus writes `0` twice and `3` twice, so it is carried rather than normalised.
    AfterImageOff { arg: f32 },
    /// EFFECT_OFF_KIND — terminates a following effect by name.
    EffectOffKind { effect_name: String },
    /// LAST_EFFECT_SET_RATE — modifies the rate of the last spawned effect.
    LastEffectSetRate { rate: f32 },
    /// LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT — offsets the last spawned effect along the
    /// camera-flat axis. The wrapper takes one numeric value after `agent`.
    LastEffectSetOffsetToCameraFlat { offset: f32 },
    /// LAST_EFFECT_SET_COLOR — retints the last spawned effect.
    ///
    /// Three arguments in every one of the corpus's 65 calls, and no alpha among them: opacity
    /// is [`LastEffectSetAlpha`](Self::LastEffectSetAlpha), a separate line and a separate
    /// decision. Keeping them apart is what lets a script that sets only one export only one.
    LastEffectSetColor { rgb: [f32; 3] },
    /// LAST_PARTICLE_SET_COLOR — retints the last spawned particle.
    ///
    /// This is deliberately separate from [`LastEffectSetColor`](Self::LastEffectSetColor):
    /// the game exposes two different "last" targets, and a particle colour must not be
    /// exported as an effect colour merely because both carry three floats.
    LastParticleSetColor { rgb: [f32; 3] },
    /// LAST_EFFECT_SET_ALPHA — sets the opacity of the last spawned effect.
    LastEffectSetAlpha { alpha: f32 },
    /// FLASH / BURN_COLOR and their relatives — see [`ColorCall`].
    Color { command: String, color: ColorCall },
    /// Any unrecognised line, preserved verbatim.
    Raw(String),
}

/// The colour payload of a `FLASH` / `BURN_COLOR` command.
///
/// These tint the fighter's model or the screen flash; they are not spawns and have no
/// graphic, joint, or transform. The command name itself lives in
/// [`EffectCall::spawn_func`], which is what every other macro-name comparison in the editor
/// already reads, so keeping a second copy of it here would be one more thing to hold in step.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorCall {
    /// Frames to interpolate over, for the `_FRM` / `_FRAME` forms. `None` for the rest, and
    /// that difference is the command's, not the user's: a `FLASH` has no such slot to write
    /// into, so a transition on one is a change of command rather than of value.
    #[serde(default)]
    pub transition: Option<f32>,
    /// Red, green, blue, and blend strength. `None` for the two commands that take no
    /// arguments at all — `BURN_COLOR_NORMAL` and `START_INFO_FLASH_EYE`.
    ///
    /// Not clamped to 0..=1. The corpus writes `BURN_COLOR(agent, 2, 0.059, 0.008, 0)`, whose
    /// red is deliberately over-bright; clamping it on the way in would dim every burn in the
    /// game by the act of loading it.
    #[serde(default)]
    pub rgba: Option<[f32; 4]>,
}

/// The model- and screen-colour commands, as `(command, takes a transition length, takes a
/// colour)`.
///
/// All seven are declared in smash-script's `macros.rs`, so all seven are emittable. Every
/// argument is generic over `ToF32`, which is why they are written with plain `to_string`
/// rather than the decimal-forcing `num` — see the note on [`crate::data::WIND_COMMANDS`].
///
/// `COL_NORMAL` belongs here and not with the hurtbox statements it is currently filed under in
/// [`ExcuteStmt`]: `lua_const` names it `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL`, one of six
/// `MA_MSC_CMD_COLOR_BLEND_*` commands with `FLASH` and `FLASH_FRM`. It is the exact sibling of
/// `BURN_COLOR_NORMAL` above — the argument-free reset for the other half of the family.
///
/// `COL_PRI` is the seventh member of that family and is deliberately **not** here. It takes a
/// single integer priority, which is neither a transition length nor a colour, so it would need
/// a third payload shape in [`ColorCall`] for the two calls the corpus makes. Both of those sit
/// in an `is_excute` block with a `FLASH`, so the export already carries them verbatim as that
/// call's `leading` and nothing is lost by leaving them there.
///
/// `FLASH_SET_DIRECTION` is deliberately absent for a different reason: `sv_animcmd` has it and
/// the corpus uses it eight times, but smash-script never wrapped it, so modelling it would mean
/// emitting a macro that does not exist. It stays an unmodelled line, as it is today.
pub const COLOR_COMMANDS: &[(&str, bool, bool)] = &[
    ("FLASH", false, true),
    ("FLASH_FRM", true, true),
    ("BURN_COLOR", false, true),
    ("BURN_COLOR_FRAME", true, true),
    ("BURN_COLOR_NORMAL", false, false),
    ("START_INFO_FLASH_EYE", false, false),
    ("COL_NORMAL", false, false),
];

/// `(takes a transition length, takes a colour)` for a colour command, or `None` if the name
/// is not one.
pub fn color_command_layout(name: &str) -> Option<(bool, bool)> {
    COLOR_COMMANDS
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, transition, rgba)| (*transition, *rgba))
}

pub fn is_color_command(name: &str) -> bool {
    color_command_layout(name).is_some()
}

/// Trail commands the corpus writes as a raw `effect(*MA_MSC_CMD_…, …)` call rather than
/// through a `macros::` wrapper, mapped to the name the rest of the editor knows them by.
///
/// C2 assumed sword trails were `macros::AFTER_IMAGE4_ON`/`_arg29`/`AFTER_IMAGE_ON`, and all
/// three have zero corpus calls. The four real trail-ON calls — kirby `SpecialHi2` and
/// `SpecialAirHi2`, twice each — are `effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)`. There is
/// no `macros::AFTER_IMAGE3_ON` to call: `lua_const` declares the command, smash-script never
/// wrapped it, so the game is reached through the command id and an export must re-emit that
/// same form.
///
/// **The command id sits in the argument slot `agent` occupies in a `macros::` call**, so slot
/// numbering is shared with the wrapper form and `acmd::TRAIL_GRAPHIC_SLOT` and
/// `TRAIL_JOINT_SLOT` address both. That is measured against the real calls, not assumed — see
/// the round-trip tests in `acmd.rs`.
///
/// **This table is the single source of truth for two sets that must not drift.** The effect
/// parser produces an `EffectMacro::AfterImage` for exactly these commands, and
/// `acmd_src::scan_macro_sites` produces a rewritable site for exactly these commands. A
/// command in one and not the other shifts every later call ordinal onto the wrong line of the
/// user's source, which is silent and writes edits into an unrelated call.
///
/// `MA_MSC_CMD_EFFECT_AFTER_IMAGE2_ON` is declared by `lua_const` and deliberately absent: it
/// has zero corpus calls, so its argument layout is unverified, and adding it here would claim
/// slot 1 is a texture and slot 4 a joint on the strength of nothing. It stays an unmodelled
/// line and rides through the export verbatim, which is what it does today.
pub const RAW_TRAIL_COMMANDS: &[(&str, &str)] =
    &[("MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON", "AFTER_IMAGE3_ON")];

/// The trail site name for a raw command id, accepting it with or without the leading `*`.
pub fn raw_trail_command(id: &str) -> Option<&'static str> {
    let id = id.trim().trim_start_matches('*').trim();
    RAW_TRAIL_COMMANDS
        .iter()
        .find(|(command, _)| *command == id)
        .map(|(_, name)| *name)
}

/// The argument slot each part of a colour call occupies, counting `agent` as slot 0.
///
/// One layout for the whole family: the transition length comes first where there is one, and
/// the four colour components follow. That uniformity is measured, not assumed —
/// `BURN_COLOR(agent, 2, 0.059, 0.008, 0)` and
/// `BURN_COLOR_FRAME(agent, 12, 2, 0.059, 0.008, 0)` are the same four values with a length
/// pushed in front.
pub fn color_slots(has_transition: bool) -> (Option<usize>, [usize; 4]) {
    if has_transition {
        (Some(1), [2, 3, 4, 5])
    } else {
        (None, [1, 2, 3, 4])
    }
}

/// A timing statement in an effect_ script — mirrors `AcmdStmt`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EffectStmt {
    Frame(f32),
    Wait(f32),
    Excute(Vec<EffectMacro>),
    Loop {
        count: usize,
        body: Vec<EffectStmt>,
    },
    /// A block this parser has no typed form for — in practice `if <costume check> {`,
    /// `if !WorkModule::is_flag(…) {`, and `else {`.
    ///
    /// The body is nested rather than flattened into the enclosing list. Flattening is what
    /// the parser used to do, and it had two costs: the header and its closing brace were
    /// dropped on export, and both arms of an `if`/`else` came out as siblings, so a move that
    /// spawned one graphic facing left and another facing right exported as spawning *both*.
    ///
    /// `header` is verbatim source, reproduced and never interpreted. It is not a condition
    /// this code can evaluate — the dumps spell costume checks as raw addresses
    /// (`if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 3)){`) — and it is not reliably
    /// even the construct it looks like: see [`crate::acmd::parse_effect_stmts`] on `else`.
    Cond {
        header: String,
        body: Vec<EffectStmt>,
    },
    Raw(String),
}

/// The parsed effect_ function, preserving full structure.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EffectScript {
    pub stmts: Vec<EffectStmt>,
}

/// A resolved effect event with computed active frame range.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EffectCall {
    pub effect_name: String,
    /// Second graphic for FLIP variants. `None` for single-graphic spawn functions.
    #[serde(default)]
    pub effect_name_alt: Option<String>,
    /// Exact ACMD spawn function (`EFFECT`, `FOOT_EFFECT`, `EFFECT_FOLLOW_ALPHA`, ...).
    #[serde(default = "default_effect_spawn_func")]
    pub spawn_func: String,
    pub bone_name: String,
    pub offset: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
    /// `true` when the effect follows the bone (EFFECT_FOLLOW variants).
    pub follows_bone: bool,
    pub active_start: u32,
    /// For one-shot effects this equals `active_start`.
    /// For following effects this is set to 9999 until an EFFECT_OFF_KIND closes it.
    pub active_end: u32,
    /// Soft-removed by the user (kept in place so edit indices stay stable).
    #[serde(default)]
    pub disabled: bool,
    /// Arguments after `scale` in the originating call, verbatim (see
    /// [`EffectMacro::Effect::extra_args`]).
    ///
    /// `None` means "not known" — a call the user added from scratch, a live capture whose
    /// tail could not be spelled in Rust, or a project saved before this field existed. It
    /// is NOT the same as `Some(vec![])`: several spawn macros genuinely end at `scale`, and
    /// conflating the two downgraded them to plain `EFFECT_FOLLOW` on export.
    #[serde(default)]
    pub extra_args: Option<Vec<String>>,
    /// Set for spawns that cannot be recomposed from a transform (currently the
    /// AFTER_IMAGE trail macros); exports re-emit this line as-is.
    #[serde(default)]
    pub raw_line: Option<String>,
    /// The argument of the `AFTER_IMAGE_OFF` that closed this trail, when a script wrote one.
    ///
    /// `None` means the editor is ending the trail itself — a retimed or newly-added one — and
    /// the export supplies [`TRAIL_OFF_DEFAULT`]. Keeping the author's value matters because
    /// the corpus does not agree on it: two calls write `0` and two write `3`.
    #[serde(default)]
    pub trail_off: Option<f32>,
    /// The trail's second joint — `trail_bone2`, argument 8 — when the call's layout is known.
    ///
    /// A trail is a ribbon stretched between *two* edges, each an offset from a named joint, so
    /// [`bone_name`](Self::bone_name) alone does not place it. All four vanilla calls name the
    /// same joint twice and separate the edges by offset — `haver` at `(0, 3, 0.25)` and `haver`
    /// again at `(0, 26, 0.5)`, the base and tip of Kirby's cutter — which is exactly why this
    /// has to be its own field rather than a second read of the first: **the corpus cannot tell
    /// the two slots apart.** Reading slot 8 where slot 4 was meant returns the right string on
    /// every vanilla call and only diverges on a mod that points the edges at different bones.
    ///
    /// `None` is "this call has no second joint the editor can vouch for", which covers both a
    /// non-trail and the `macros::AFTER_IMAGE4_ON` / `AFTER_IMAGE_ON` spellings — neither is
    /// declared by `smash-script` and neither appears in the corpus, so nothing says what sits
    /// at slot 8 of a call that could not have been written. The panel shows no second joint
    /// there rather than one guessed from position.
    #[serde(default)]
    pub trail_bone2: Option<String>,
    /// Playback rate from a `LAST_EFFECT_SET_RATE` line following this spawn.
    ///
    /// `None` means the script sets no rate and the export writes no line — which is not the
    /// same as `Some(1.0)`, an explicit rate that happens to be the default. The value belongs
    /// to the spawn rather than standing on its own because `LAST_EFFECT_SET_RATE` takes no
    /// effect kind: it modifies whatever was spawned last, so it is a property *of* that
    /// spawn. Binding it here is also what makes disabling or reordering a spawn carry its
    /// rate along instead of leaving the line behind to land on someone else's effect.
    #[serde(default)]
    pub rate: Option<f32>,
    /// Camera-flat offset from a `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` line following this
    /// spawn. It is kept separate from [`offset`](Self::offset): the latter is the spawn's
    /// three-dimensional bone-relative position, while this macro adjusts the rendered effect
    /// after it has been created.
    #[serde(default)]
    pub camera_offset: Option<f32>,
    /// Tint from a `LAST_EFFECT_SET_COLOR` line following this spawn, as red, green, blue.
    ///
    /// Bound to the spawn for the same reason [`rate`](Self::rate) is, and with the same
    /// `None` vs `Some(default)` rule: no line at all is not the same export as a line setting
    /// white. Distinct from [`color`](Self::color), which is the payload of a `FLASH` /
    /// `BURN_COLOR` command — that one tints the *fighter*, this one tints one spawned effect.
    #[serde(default)]
    pub tint: Option<[f32; 3]>,
    /// Tint from a `LAST_PARTICLE_SET_COLOR` line following this spawn, as red, green, blue.
    ///
    /// Kept distinct from [`tint`](Self::tint): the primitive targets the last particle rather
    /// than the last effect, so aliasing the fields would change which renderer object is edited.
    #[serde(default)]
    pub particle_tint: Option<[f32; 3]>,
    /// Opacity from a `LAST_EFFECT_SET_ALPHA` line following this spawn.
    ///
    /// Its own field rather than a fourth component of [`tint`](Self::tint), because the two
    /// are separate macros: a script that sets colour and not alpha must export exactly that,
    /// and folding them together would make every recolour also write an opacity the script
    /// never asked for.
    #[serde(default)]
    pub alpha: Option<f32>,
    /// Set when this entry is a colour command rather than a spawn — `FLASH`, `BURN_COLOR`,
    /// and the rest of [`COLOR_COMMANDS`], with `spawn_func` naming which one.
    ///
    /// These share the effect list rather than getting one of their own because everything the
    /// list already does — reordering, disabling, undo, project save, write-back ordinals,
    /// export grouping by frame — is exactly what they need too, and a parallel list would be
    /// a second copy of all of it to keep in step. The cost is that the fields above are
    /// meaningless here: there is no graphic, joint, transform, or end frame, and every site
    /// that reads one must check this field first. `active_end` is set equal to `active_start`
    /// so nothing tries to close a colour command with an `EFFECT_OFF_KIND`.
    #[serde(default)]
    pub color: Option<ColorCall>,
    /// Verbatim header of the conditional this spawn sat inside, or `None` at top level.
    ///
    /// The export re-wraps the spawn's `is_excute` block in this, which is the difference
    /// between a directional move exporting as "spawn the left graphic when facing left" and
    /// exporting as "spawn both". Reproduced, never parsed — see [`EffectStmt::Cond`].
    #[serde(default)]
    pub guard: Option<String>,
    /// Lines that preceded this spawn inside its frame block and that no `EffectCall` could
    /// represent, kept verbatim with their own `if`/`is_excute` wrapper already in place.
    #[serde(default)]
    pub leading: Vec<String>,
    /// The same, for lines that *followed* this spawn.
    ///
    /// Position is load-bearing rather than cosmetic, which is why these hang off a call
    /// instead of off the frame. Every one of the 64 costume tints in the corpus is a
    /// `LAST_EFFECT_SET_COLOR` that recolours whatever spawned most recently, and dolly's
    /// `SpecialHiCommand` puts three spawns and 24 such tints in one frame. Emitting the
    /// spawns first and the tints after — the obvious thing, if residue were anchored to a
    /// frame — would land all 24 on the third spawn and recolour it eight times.
    ///
    /// Deliberately not modelled into [`tint`](Self::tint): that is one field and these are
    /// eight alternatives, so binding them would keep only costume 7's and apply it to all.
    #[serde(default)]
    pub trailing: Vec<String>,
}

fn default_effect_spawn_func() -> String {
    String::new()
}

/// One user edit to a move's effect-call list. `index` refers to the PRISTINE
/// `to_effect_calls()` order; added calls live past the pristine length.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EffectCallEdit {
    pub index: usize,
    pub op: EffectCallOp,
    /// Snapshot of the pristine call this edit targets (None for adds / older saves).
    /// Lets a reloaded pristine list re-anchor the edit when indices shift, and lets
    /// capture loading tell "user's retimed/renamed spawn" apart from a script spawn.
    #[serde(default)]
    pub pristine: Option<EffectCall>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EffectCallOp {
    /// Replace the call at `index` with new values.
    Modify(EffectCall),
    /// A user-added call (index is its position in the edited list).
    Add(EffectCall),
    /// Soft-remove the call at `index`.
    Remove,
}

impl EffectScript {
    /// Flatten the script into resolved `EffectCall`s with computed frame ranges.
    pub fn to_effect_calls(&self) -> Vec<EffectCall> {
        self.to_effect_calls_and_residue().0
    }

    /// The resolved calls, plus the lines that belong to a frame rather than to any one call.
    ///
    /// The second half exists because a modifier this parser understands but cannot attach is
    /// *worse* than one it never understood: an unrecognised line is reported as dropped by
    /// [`crate::acmd::unexportable_effect_lines`], while a recognised one that binds to nothing
    /// used to be discarded here and left no trace anywhere. That is the trap C1 walked
    /// into — modelling `LAST_EFFECT_SET_COLOR` moved 32 of the corpus's 65 calls out of the
    /// dropped-line report without moving them into the export, because they sit inside
    /// costume-gated `if` blocks that separate them from their spawn.
    ///
    /// Returned from the same walk that resolves the calls, rather than computed beside it, so
    /// there is exactly one implementation of the rule deciding what a modifier binds to.
    ///
    /// C6 carried a modifier that binds to no spawn but shares a frame block with one, as that
    /// call's [`EffectCall::leading`] or [`EffectCall::trailing`]. E3 took the remainder: lines
    /// whose frame holds no spawn at all now come back here keyed by frame, and the emitter
    /// opens a block for them. **Nothing is dropped by this walk any more**, which is why it no
    /// longer returns a loss list — what [`crate::acmd::unexportable_effect_lines`] reports is
    /// now decided entirely from the statement tree.
    ///
    /// The caller has to pass the second half to
    /// [`crate::acmd::emit_effect_move_fn`](../acmd/fn.emit_effect_move_fn.html); there is no
    /// default. Dropping it on the floor is silent and is exactly the bug this replaced.
    pub fn to_effect_calls_and_residue(
        &self,
    ) -> (
        Vec<EffectCall>,
        std::collections::BTreeMap<u32, Vec<String>>,
    ) {
        let mut calls: Vec<EffectCall> = Vec::new();
        let mut walk = EffectWalk::default();
        let end = eval_effect_stmts(&self.stmts, 0.0, &mut calls, &mut walk);
        walk.end_frame(end);
        (calls, walk.frame_residue)
    }
}

/// Walk state for [`eval_effect_stmts`] that is not the frame cursor.
#[derive(Default)]
struct EffectWalk {
    /// Verbatim header of the enclosing [`EffectStmt::Cond`].
    ///
    /// One value rather than a stack: no effect function in the corpus nests a non-`is_excute`
    /// conditional more than one deep, so a stack would be untested machinery. A nested guard
    /// would overwrite this and lose the outer one, so [`EffectStmt::Cond`] refuses to descend
    /// into a second level and leaves it to be reported as unexportable instead — a named loss
    /// beats a half-applied condition.
    guard: Option<String>,
    /// Residue seen since the last spawn in this frame, wrapped and waiting for a call to
    /// attach to.
    pending: Vec<String>,
    /// Index of the most recent spawn in this frame block, if any.
    last_spawn: Option<usize>,
    /// Residue from a frame that turned out to contain no spawn at all, keyed by the frame it
    /// was written at, already wrapped and ready to emit. See [`EffectWalk::end_frame`].
    ///
    /// Before E3 this channel did not exist and its contents were reported as deleted, which
    /// they were.
    frame_residue: std::collections::BTreeMap<u32, Vec<String>>,
}

impl EffectWalk {
    /// Record one line that no `EffectCall` can represent, wrapped so it can be re-emitted.
    ///
    /// Attaches to the spawn above it when there is one, because these lines are overwhelmingly
    /// `LAST_EFFECT_SET_*` and those name "whatever spawned most recently" — see
    /// [`EffectCall::trailing`] for what anchoring them to the frame instead would do.
    /// The `is_excute` wrapper is regenerated here rather than carried from the source, because
    /// the block is re-emitted standalone at frame level — outside the `is_excute` the emitter
    /// writes for the spawns. Reusing the source's own wrapper would mean carrying the brace
    /// that closes it too, and dropping the wrapper entirely would leave the line running on
    /// every frame the coroutine sits on rather than once.
    fn residue(&mut self, line: &str, calls: &mut [EffectCall]) {
        let mut block = Vec::new();
        if let Some(guard) = &self.guard {
            block.push(guard.clone());
        }
        block.push("if macros::is_excute(agent) {".to_string());
        block.push(line.to_string());
        block.push("}".to_string());
        if self.guard.is_some() {
            block.push("}".to_string());
        }
        match self.last_spawn {
            Some(index) => calls[index].trailing.extend(block),
            None => self.pending.extend(block),
        }
    }

    /// Hand the pending residue to the call about to be pushed, which is now its home.
    fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }

    /// Close the frame block at `frame`: nothing may attach across a frame boundary.
    ///
    /// Carrying residue to a spawn at a *different* frame would not preserve the line, it would
    /// retime it, and that is still refused. But there is a third option this used to skip:
    /// keep the lines at the frame they were written at and emit them there with no call to hang
    /// from. They arrive from [`Self::residue`] already wrapped in their own
    /// `if macros::is_excute(agent) { … }`, so a frame that produced nothing else can still be
    /// written out — the emitter has always been able to open a bare block, it just had no way
    /// to be told a frame existed unless a call sat on it.
    ///
    /// This is the whole of E3's measured defect. `dolly/FinalAirEnd`'s frame 40 is two
    /// `CANCEL_FILL_SCREEN` calls and nothing else, so there was no spawn in the frame to become
    /// their `leading`, and the export deleted both. Every other line in the corpus's residue
    /// channel shares a frame with a spawn and was already carried by C6.
    ///
    /// Anything still pending after this is a genuine loss and keeps being reported.
    fn end_frame(&mut self, frame: f32) {
        if !self.pending.is_empty() {
            self.frame_residue
                .entry(script_frame(frame))
                .or_default()
                .append(&mut self.pending);
        }
        self.last_spawn = None;
    }
}

/// ACMD script frame → the one-based game frame the editor shows.
///
/// ROUNDS. This used to truncate (`frame as u32`), while the live-capture path rounded, so the
/// two sources disagreed by a frame on any non-integral value. Frame zero is only the ACMD
/// coroutine's initial state; in the game-facing timeline it is frame 1.
pub(crate) fn script_frame(frame: f32) -> u32 {
    (frame.max(0.0).round() as u32).max(1)
}

/// The plugin reports `MotionModule::frame`, whose first pose is zero. The editor, ACMD source,
/// exports, and user-facing timeline all name that same instant game frame 1.
pub(crate) fn motion_to_script_frame(frame: f32) -> u32 {
    (frame.max(0.0).round() as u32).saturating_add(1)
}

/// Convert a one-based game/ACMD frame to the zero-based motion and animation frame index.
pub(crate) fn script_to_motion_frame(frame: u32) -> f32 {
    frame.saturating_sub(1) as f32
}

fn eval_effect_stmts(
    stmts: &[EffectStmt],
    start_frame: f32,
    calls: &mut Vec<EffectCall>,
    walk: &mut EffectWalk,
) -> f32 {
    let mut frame = start_frame;
    for stmt in stmts {
        match stmt {
            // `end_frame` is handed the frame being *left*, not the one being entered — the
            // pending residue was written under the old cursor and has to be emitted there.
            EffectStmt::Frame(f) => {
                walk.end_frame(frame);
                frame = *f;
            }
            EffectStmt::Wait(w) => {
                walk.end_frame(frame);
                frame += w;
            }
            // Statement-level lines with no typed form: `wait_loop_sync_mot`, bare
            // `EffectModule::` calls, `methodlib::L2CAgent::pop…`. Deliberately NOT carried.
            //
            // The regenerated function states every frame absolutely, with its own `frame()`
            // calls computed from this walk. A carried `wait_loop_sync_mot` would advance the
            // coroutine a second time on top of that and shift every effect after it — the
            // export would compile and play wrong, which is worse than the current honest
            // deletion. `unexportable_effect_lines` still names each one.
            EffectStmt::Raw(_) => {}
            EffectStmt::Cond { header, body } => {
                // One level only. A guard inside a guard would overwrite `walk.guard` and
                // export the inner condition while silently discarding the outer, which is a
                // wrong export rather than a lossy one. Nothing in the corpus nests, so this
                // arm has no measured cost; the body is still walked so its spawns reach the
                // timeline, they just carry the outer guard.
                let outer = walk.guard.clone();
                if outer.is_none() {
                    walk.guard = Some(header.clone());
                }
                frame = eval_effect_stmts(body, frame, calls, walk);
                walk.guard = outer;
            }
            EffectStmt::Excute(macros) => {
                // Which call, if any, the macro immediately above produced — the anchor a
                // `LAST_EFFECT_SET_RATE` binds to. It is deliberately cleared by every macro
                // that spawns nothing, including `Raw`, so a rate can only ever attach to a
                // spawn this code actually understands.
                //
                // At runtime the game's "last effect" persists across frame blocks and across
                // lines this parser does not recognise, so a stricter rule than the game's
                // costs coverage in principle. It costs none in practice: all 27
                // `LAST_EFFECT_SET_RATE` calls in the local corpus sit directly beneath a
                // recognised spawn in the same block. Guessing the other way would attach a
                // rate to a spawn several lines up and export it silently onto that one.
                let mut anchor: Option<usize> = None;
                for m in macros {
                    match m {
                        EffectMacro::Effect {
                            effect_name,
                            effect_name_alt,
                            spawn_func,
                            bone_name,
                            offset,
                            rotation,
                            scale,
                            follows_bone,
                            extra_args,
                        } => {
                            let active_end = if *follows_bone {
                                9999
                            } else {
                                script_frame(frame)
                            };
                            calls.push(EffectCall {
                                effect_name: effect_name.clone(),
                                effect_name_alt: effect_name_alt.clone(),
                                spawn_func: spawn_func.clone(),
                                bone_name: bone_name.clone(),
                                offset: *offset,
                                rotation: *rotation,
                                scale: *scale,
                                follows_bone: *follows_bone,
                                active_start: script_frame(frame),
                                active_end,
                                disabled: false,
                                extra_args: Some(extra_args.clone()),
                                raw_line: None,
                                trail_off: None,
                                trail_bone2: None,
                                rate: None,
                                camera_offset: None,
                                tint: None,
                                particle_tint: None,
                                alpha: None,
                                color: None,
                                guard: walk.guard.clone(),
                                leading: walk.take_pending(),
                                trailing: Vec::new(),
                            });
                            anchor = Some(calls.len() - 1);
                            walk.last_spawn = anchor;
                        }
                        EffectMacro::EffectOffKind { effect_name } => {
                            // EffectModule::kill_kind closes every live instance of this kind.
                            for call in calls.iter_mut().filter(|call| {
                                &call.effect_name == effect_name && call.active_end == 9999
                            }) {
                                call.active_end = script_frame(frame);
                            }
                            anchor = None;
                        }
                        EffectMacro::AfterImage {
                            effect_name,
                            bone_name,
                            bone_name2,
                            raw,
                        } => {
                            // Sword/weapon trail — active until AfterImageOff
                            calls.push(EffectCall {
                                effect_name: effect_name.clone(),
                                effect_name_alt: None,
                                spawn_func: "AFTER_IMAGE_ON".into(),
                                bone_name: bone_name.clone(),
                                offset: [0.0; 3],
                                rotation: [0.0; 3],
                                scale: 1.0,
                                follows_bone: true,
                                active_start: script_frame(frame),
                                active_end: 9999,
                                disabled: false,
                                extra_args: None,
                                raw_line: (!raw.is_empty()).then(|| raw.clone()),
                                trail_off: None,
                                trail_bone2: bone_name2.clone(),
                                rate: None,
                                camera_offset: None,
                                tint: None,
                                particle_tint: None,
                                alpha: None,
                                color: None,
                                guard: walk.guard.clone(),
                                leading: walk.take_pending(),
                                trailing: Vec::new(),
                            });
                            // A residue anchor even though it is not a rate anchor: the two
                            // questions are different. `LAST_EFFECT_SET_RATE` must not bind to a
                            // trail because it would not bind to one in game, but a carried line
                            // only needs *a* call at this frame to keep its position relative to.
                            walk.last_spawn = Some(calls.len() - 1);
                            // Deliberately NOT an anchor. A trail produces an `EffectCall` for
                            // the timeline, but it is drawn by the after-image system rather
                            // than spawned as an effect, so it is not what `LAST_EFFECT_SET_RATE`
                            // would find. Nothing in the corpus puts a rate after a trail, so
                            // refusing costs no coverage and avoids inventing an answer.
                            anchor = None;
                        }
                        EffectMacro::AfterImageOff { arg } => {
                            // Close the most recent open *trail*, keeping the argument this call
                            // wrote so the export can write it back.
                            //
                            // Not merely the most recent open call. A trail routinely runs while
                            // an ordinary spawn from the same block is still open — kirby
                            // `SpecialHi2` starts a trail and an `EFFECT_FOLLOW` in one
                            // `is_excute` — and `rev()` then picks the follow. That closed the
                            // wrong call twice over: the `AFTER_IMAGE_OFF` vanished, so the
                            // exported trail ran forever, *and* the follow acquired an end frame
                            // it never had, so the export invented an `EFFECT_OFF_KIND` killing
                            // an effect the script leaves running. Invisible until C2 made trails
                            // parse, because with no trail call the wrong answer was the only
                            // answer.
                            match calls
                                .iter_mut()
                                .rev()
                                .find(|c| c.active_end == 9999 && c.spawn_func == "AFTER_IMAGE_ON")
                            {
                                Some(call) => {
                                    call.active_end = script_frame(frame);
                                    call.trail_off = Some(*arg);
                                }
                                // A trail this script did not start. Kirby `SpecialHi4` and
                                // `SpecialAirHi4` end the trail `SpecialHi2` began — the two
                                // halves of one move, in separate scripts — so there is no call
                                // here to close and no way to make one. Dropping the line would
                                // leave the trail running, so it is carried verbatim.
                                None => walk.residue(
                                    &format!(
                                        "macros::AFTER_IMAGE_OFF(agent, {});",
                                        crate::acmd::attack_mod_num(*arg)
                                    ),
                                    calls,
                                ),
                            }
                            anchor = None;
                        }
                        EffectMacro::LastEffectSetRate { rate } => {
                            // `anchor` is left in place: two rate lines in a row both name the
                            // same spawn, and the later one wins, exactly as in game. The same
                            // is true across modifiers — a colour line after a rate line still
                            // names the spawn above both — which is why none of these modifiers
                            // arms clears it.
                            match anchor {
                                Some(index) => calls[index].rate = Some(*rate),
                                None => walk.residue(
                                    &format!("macros::LAST_EFFECT_SET_RATE(agent, {rate});"),
                                    calls,
                                ),
                            }
                        }
                        EffectMacro::LastEffectSetOffsetToCameraFlat { offset } => match anchor {
                            Some(index) => calls[index].camera_offset = Some(*offset),
                            None => walk.residue(
                                &format!(
                                    "macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, {offset});"
                                ),
                                calls,
                            ),
                        },
                        EffectMacro::LastEffectSetColor { rgb } => match anchor {
                            Some(index) => calls[index].tint = Some(*rgb),
                            // The 64 costume tints land here, and carrying them verbatim is the
                            // whole of C6's colour half. They cannot become `tint` — see
                            // [`EffectCall::trailing`] — so they are reproduced as written.
                            None => walk.residue(
                                &format!(
                                    "macros::LAST_EFFECT_SET_COLOR(agent, {}, {}, {});",
                                    rgb[0], rgb[1], rgb[2]
                                ),
                                calls,
                            ),
                        },
                        EffectMacro::LastParticleSetColor { rgb } => match anchor {
                            Some(index) => calls[index].particle_tint = Some(*rgb),
                            None => walk.residue(
                                &format!(
                                    "macros::LAST_PARTICLE_SET_COLOR(agent, {}, {}, {});",
                                    rgb[0], rgb[1], rgb[2]
                                ),
                                calls,
                            ),
                        },
                        EffectMacro::LastEffectSetAlpha { alpha } => match anchor {
                            Some(index) => calls[index].alpha = Some(*alpha),
                            None => walk.residue(
                                &format!("macros::LAST_EFFECT_SET_ALPHA(agent, {alpha});"),
                                calls,
                            ),
                        },
                        EffectMacro::Color { command, color } => {
                            calls.push(EffectCall {
                                effect_name: String::new(),
                                effect_name_alt: None,
                                spawn_func: command.clone(),
                                bone_name: String::new(),
                                offset: [0.0; 3],
                                rotation: [0.0; 3],
                                scale: 1.0,
                                follows_bone: false,
                                active_start: script_frame(frame),
                                // Not 9999, and not the transition length either. A colour
                                // command is one instant event: `BURN_COLOR_FRAME` schedules an
                                // interpolation the game runs on its own, with no closing call
                                // anywhere for an end frame to mean.
                                active_end: script_frame(frame),
                                disabled: false,
                                extra_args: None,
                                raw_line: None,
                                trail_off: None,
                                trail_bone2: None,
                                rate: None,
                                camera_offset: None,
                                tint: None,
                                particle_tint: None,
                                alpha: None,
                                color: Some(color.clone()),
                                guard: walk.guard.clone(),
                                leading: walk.take_pending(),
                                trailing: Vec::new(),
                            });
                            // Not a spawn, so not a rate anchor — a `LAST_EFFECT_SET_RATE`
                            // below a `FLASH` still belongs to whatever spawned before it, and
                            // the parser refuses to reach past this line to find it. It *is* a
                            // residue anchor, for the reason given on the after-image arm.
                            walk.last_spawn = Some(calls.len() - 1);
                            anchor = None;
                        }
                        // An effect macro with no typed form. Carried verbatim, unlike its
                        // statement-level twin above, because inside an `is_excute` block a line
                        // cannot be a timing primitive — everything here acts on effects.
                        EffectMacro::Raw(line) => {
                            walk.residue(line, calls);
                            anchor = None;
                        }
                    }
                }
            }
            EffectStmt::Loop { count, body } => {
                // Every iteration re-runs the SAME text, so the pending residue is rewound to
                // where the loop started each time. The calls are genuinely unrolled — four
                // iterations spawn four effects, and each carries its own copy of the lines that
                // followed it, which is right.
                //
                // `frame_residue` is deliberately NOT rewound, and that is a change of meaning
                // from the loss report this replaced. That report named lines of *source*, and
                // one line named four times was three lies, so it was truncated back. This is
                // output: an iteration that reaches a frame of its own needs its own copy there,
                // exactly as its spawns do. Iterations that land on the same frame append, which
                // matches what the game runs — every corpus loop advances the frame, so this is
                // reasoning about a shape the corpus does not contain rather than a measurement.
                let pending_at_entry = walk.pending.clone();
                for _ in 0..*count {
                    walk.pending.clone_from(&pending_at_entry);
                    frame = eval_effect_stmts(body, frame, calls, walk);
                }
            }
        }
    }
    frame
}

impl EffectScript {
    /// Count runtime conditional blocks in the effect source.
    ///
    /// `Cond` preserves a branch that a live capture cannot identify after the fact. Loops are
    /// deliberately not branches; only the source conditions that can hide an effect matter to
    /// the capture-provenance warning.
    pub fn branch_count(&self) -> usize {
        fn count(stmts: &[EffectStmt]) -> usize {
            stmts
                .iter()
                .map(|stmt| match stmt {
                    EffectStmt::Cond { body, .. } => 1 + count(body),
                    EffectStmt::Loop { body, .. } => count(body),
                    _ => 0,
                })
                .sum()
        }
        count(&self.stmts)
    }

    /// For each call [`to_effect_calls`](Self::to_effect_calls) produces, the ordinal of the
    /// spawn macro in the script text that produced it.
    ///
    /// The two are not one-to-one: a macro inside a `for` runs once per iteration, so several
    /// calls share one ordinal. Writing an edit back to source needs to know which text the
    /// call came from, and this is the only thing that connects them — so it MUST visit the
    /// statement tree exactly the way `eval_effect_stmts` does. Keep them in step.
    pub fn call_macro_ordinals(&self) -> Vec<usize> {
        fn walk(stmts: &[EffectStmt], next: &mut usize, out: &mut Vec<usize>) {
            for stmt in stmts {
                match stmt {
                    EffectStmt::Excute(macros) => {
                        for m in macros {
                            // Every macro that produces an `EffectCall` is numbered, and only
                            // those — the ordinals index the call list, so a macro counted here
                            // that produces no call (or a call produced by a macro not counted
                            // here) shifts every later call onto the wrong line of source.
                            if matches!(
                                m,
                                EffectMacro::Effect { .. }
                                    | EffectMacro::AfterImage { .. }
                                    | EffectMacro::Color { .. }
                            ) {
                                out.push(*next);
                                *next += 1;
                            }
                        }
                    }
                    EffectStmt::Loop { count, body } => {
                        // Every iteration re-runs the SAME text: restore the counter so the
                        // second pass lands on the same ordinals as the first.
                        let start = *next;
                        for _ in 0..*count {
                            *next = start;
                            walk(body, next, out);
                        }
                    }
                    // Descended into, not skipped. `eval_effect_stmts` resolves the spawns
                    // inside a conditional into real calls, so this walk has to number them or
                    // every later call would be attributed to the wrong line of source.
                    EffectStmt::Cond { body, .. } => walk(body, next, out),
                    EffectStmt::Frame(_) | EffectStmt::Wait(_) | EffectStmt::Raw(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.stmts, &mut 0, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kirby's down smash, which is the clearest case in the corpus: it sets `0.25`, crosses
    /// four motion frames of windup, then restores `1.0` and plays one more.
    ///
    /// ```text
    /// FT_MOTION_RATE(agent, 0.25);   frame(4.0);   FT_MOTION_RATE(agent, 1.0);   frame(5.0);
    /// ```
    ///
    /// Four motion frames at `0.25` are **one** game frame, so the whole windup is gone by the
    /// time the player's second frame starts. The numbers below are the measured live behaviour
    /// (E2), not a reading of the macro's name.
    fn down_smash() -> AcmdScript {
        AcmdScript {
            stmts: vec![
                AcmdStmt::MotionRate(0.25),
                AcmdStmt::Frame(4.0),
                AcmdStmt::MotionRate(1.0),
                AcmdStmt::Frame(5.0),
            ],
        }
    }

    #[test]
    fn a_rate_window_compresses_motion_frames_into_fewer_game_frames() {
        let script = down_smash();
        assert_eq!(script.game_frame(0.0), 0.0);
        assert_eq!(
            script.game_frame(2.0),
            0.5,
            "halfway through the 0.25 window"
        );
        assert_eq!(
            script.game_frame(4.0),
            1.0,
            "4 motion frames at 0.25 = 1 game"
        );
        assert_eq!(script.game_frame(5.0), 2.0, "then 1 motion frame at 1.0");
    }

    /// The direction, asserted on its own so that inverting the arithmetic fails a test whose
    /// name says what broke.
    ///
    /// **A rate below 1.0 makes a move play FASTER**, which is the opposite of what "rate"
    /// reads like and the thing most likely to be "corrected" by someone later.
    #[test]
    fn a_rate_below_one_makes_the_move_finish_sooner_not_later() {
        let slow_looking = down_smash();
        let plain = AcmdScript {
            stmts: vec![AcmdStmt::Frame(4.0)],
        };
        assert!(
            slow_looking.game_frame(4.0) < plain.game_frame(4.0),
            "0.25 must reach motion frame 4 in fewer game frames than 1.0, got {} vs {}",
            slow_looking.game_frame(4.0),
            plain.game_frame(4.0),
        );
    }

    #[test]
    fn reverse_lr_points_keep_their_sites_while_being_removed_and_inserted() {
        let mut script = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::ReverseLr]),
                AcmdStmt::Frame(8.0),
                AcmdStmt::Excute(vec![ExcuteStmt::ReverseLr]),
            ],
        };
        assert_eq!(
            script.to_reverse_lr_events(),
            vec![
                ReverseLrEvent { frame: 3, site: 0 },
                ReverseLrEvent { frame: 8, site: 1 },
            ]
        );

        assert!(script.remove_reverse_lr(0));
        assert_eq!(
            script.to_reverse_lr_events(),
            vec![ReverseLrEvent { frame: 8, site: 0 }]
        );
        assert!(script.insert_reverse_lr_at_frame(5));
        assert_eq!(
            script.to_reverse_lr_events(),
            vec![
                ReverseLrEvent { frame: 5, site: 0 },
                ReverseLrEvent { frame: 8, site: 1 },
            ]
        );
    }

    /// A script with no rate call must map every frame to itself — so callers can use
    /// `game_frame` unconditionally instead of branching on whether a rate is present, which is
    /// the kind of branch that goes stale when a family arrives.
    #[test]
    fn a_script_with_no_rate_call_maps_every_frame_to_itself() {
        let script = AcmdScript {
            stmts: vec![AcmdStmt::Frame(3.0), AcmdStmt::Wait(4.0)],
        };
        assert!(script.rate_spans().is_empty());
        for frame in [0.0, 1.0, 3.0, 7.0, 40.0] {
            assert_eq!(script.game_frame(frame), frame);
        }
    }

    /// `frame()` waits *until* a frame and returns immediately if it has already passed, so a
    /// target behind the cursor must advance neither clock. Kirby's stone special really does
    /// this — `frame(14.0)` then `frame(2.0)`.
    ///
    /// **A rate change has to come after the rewind for this to test anything.** The obvious
    /// version — assert the backwards frame still maps somewhere sane — passes with the clamp
    /// deleted, because with a single rate window `game_frame` recomputes from that span's own
    /// origin and never reads the running clock. The corrupted clock is only visible where the
    /// *next* span starts, so the script below opens one. Caught by mutation, not by review.
    #[test]
    fn a_frame_target_already_passed_does_not_rewind_the_clock_the_next_span_starts_from() {
        let script = AcmdScript {
            stmts: vec![
                AcmdStmt::MotionRate(0.5),
                AcmdStmt::Frame(14.0), // 14 motion frames at 0.5 = 7 game frames
                AcmdStmt::Frame(2.0),  // already passed: advances nothing
                AcmdStmt::MotionRate(1.0), // so this span starts at motion 14, game 7
                AcmdStmt::Frame(20.0), // 6 more motion frames, now at 1.0
            ],
        };
        assert_eq!(script.game_frame(14.0), 7.0);
        assert_eq!(
            script.game_frame(20.0),
            13.0,
            "the second rate window must start from motion 14 / game 7, not from the \
             rewound frame(2.0)"
        );
    }

    fn touch(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn weapon_skel_falls_back_when_the_preferred_slot_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("sword");
        // A mod that ships no c00 at all — the case that used to render with no weapons.
        touch(&part.join("c03").join("model.nusktb"));
        touch(&part.join("c07").join("model.nusktb"));

        // Preferred slot present → used as-is.
        assert_eq!(
            find_part_skel(&part, 7),
            Some(part.join("c07").join("model.nusktb"))
        );
        // Preferred slot absent → lowest slot that actually exists, not a hardcoded c00.
        assert_eq!(
            find_part_skel(&part, 0),
            Some(part.join("c03").join("model.nusktb"))
        );
        assert_eq!(
            find_part_skel(&part, 5),
            Some(part.join("c03").join("model.nusktb"))
        );
    }

    #[test]
    fn part_with_no_skeleton_at_all_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("shield");
        std::fs::create_dir_all(part.join("c00")).unwrap();
        assert_eq!(find_part_skel(&part, 0), None);
        assert_eq!(
            find_part_skel(tmp.path().join("missing").as_path(), 0),
            None
        );
    }

    #[test]
    fn part_costume_slots_are_sorted_and_default_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("hammer");
        touch(&part.join("c11").join("model.nusktb"));
        touch(&part.join("c02").join("model.nusktb"));
        assert_eq!(part_costume_slots(&part), vec![2, 11]);

        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(part_costume_slots(&bare), default_slots());
    }

    #[test]
    fn costume_dir_names_parse_past_the_vanilla_eight() {
        assert_eq!(parse_costume_dir("c00"), Some(0));
        assert_eq!(parse_costume_dir("c07"), Some(7));
        // The whole point: slots outside the vanilla 8.
        assert_eq!(parse_costume_dir("c08"), Some(8));
        assert_eq!(parse_costume_dir("c99"), Some(99));
        assert_eq!(parse_costume_dir("c113"), Some(113));
        assert_eq!(parse_costume_dir("c255"), Some(255));
        // Inconsistent padding is tolerated (the game resolves by index).
        assert_eq!(parse_costume_dir("c0"), Some(0));
        assert_eq!(parse_costume_dir("c008"), Some(8));
    }

    #[test]
    fn non_costume_dirs_and_out_of_range_are_rejected() {
        assert_eq!(parse_costume_dir("body"), None);
        assert_eq!(parse_costume_dir("sword"), None);
        assert_eq!(parse_costume_dir("c"), None);
        assert_eq!(parse_costume_dir("cXX"), None);
        assert_eq!(parse_costume_dir("c0a"), None);
        assert_eq!(parse_costume_dir("00"), None);
        // Must NOT wrap into a plausible-looking slot.
        assert_eq!(parse_costume_dir("c256"), None);
        assert_eq!(parse_costume_dir("c9999"), None);
    }

    #[test]
    fn eff_stem_slot_suffix_parses() {
        assert_eq!(slot_from_eff_stem("ef_mario_c08", "mario"), Some(8));
        assert_eq!(slot_from_eff_stem("ef_mario_c00", "mario"), Some(0));
        assert_eq!(slot_from_eff_stem("ef_mario_c127", "mario"), Some(127));
        // The base file is not slot-scoped.
        assert_eq!(slot_from_eff_stem("ef_mario", "mario"), None);
        // A fighter whose name itself contains underscores still resolves.
        assert_eq!(
            slot_from_eff_stem("ef_ice_climber_c03", "ice_climber"),
            Some(3)
        );
        assert_eq!(slot_from_eff_stem("ef_ice_climber", "ice_climber"), None);
    }

    #[test]
    fn discovery_finds_vanilla_eight_and_nothing_more() {
        let root = tempfile::tempdir().unwrap();
        for slot in 0..8u8 {
            let dir = root
                .path()
                .join("fighter/mario/model/body")
                .join(format!("c{slot:02}"));
            std::fs::create_dir_all(&dir).unwrap();
        }
        let slots = discover_costume_slots(&[root.path().to_path_buf()], "mario");
        assert_eq!(slots, (0..8u8).collect::<Vec<_>>());
    }

    #[test]
    fn discovery_picks_up_extra_and_large_slots() {
        let root = tempfile::tempdir().unwrap();
        for slot in [0u8, 1, 8, 12, 200] {
            std::fs::create_dir_all(
                root.path()
                    .join("fighter/mario/model/body")
                    .join(format!("c{slot:02}")),
            )
            .unwrap();
        }
        let slots = discover_costume_slots(&[root.path().to_path_buf()], "mario");
        assert_eq!(slots, vec![0, 1, 8, 12, 200]);
    }

    #[test]
    fn discovery_unions_model_motion_eff_and_multiple_roots() {
        let game = tempfile::tempdir().unwrap();
        let modroot = tempfile::tempdir().unwrap();
        // Vanilla-ish game root: model c00/c01.
        for slot in [0u8, 1] {
            std::fs::create_dir_all(
                game.path()
                    .join("fighter/mario/model/body")
                    .join(format!("c{slot:02}")),
            )
            .unwrap();
        }
        // Motion-only slot in the game root.
        std::fs::create_dir_all(game.path().join("fighter/mario/motion/body/c05")).unwrap();
        // Mod root adds a model slot and a one-slot eff for a different slot.
        std::fs::create_dir_all(modroot.path().join("fighter/mario/model/body/c09")).unwrap();
        touch(&modroot.path().join("effect/fighter/mario/ef_mario_c42.eff"));
        // The base eff must not register as a slot.
        touch(&modroot.path().join("effect/fighter/mario/ef_mario.eff"));

        let slots = discover_costume_slots(
            &[game.path().to_path_buf(), modroot.path().to_path_buf()],
            "mario",
        );
        assert_eq!(slots, vec![0, 1, 5, 9, 42]);
    }

    #[test]
    fn discovery_is_empty_for_unknown_fighter_and_falls_back_to_vanilla() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("fighter/mario/model/body/c00")).unwrap();
        let roots = [root.path().to_path_buf()];
        assert!(discover_costume_slots(&roots, "nosuchfighter").is_empty());
        // Callers that need a list still get the historical vanilla behaviour.
        assert_eq!(
            costume_slots_or_default(&roots, "nosuchfighter"),
            default_slots()
        );
        assert_eq!(costume_slots_or_default(&roots, "mario"), vec![0]);
    }

    #[test]
    fn loadable_check_accepts_a_modded_fighter_without_param() {
        let root = tempfile::tempdir().unwrap();
        let fighter_dir = root.path().join("fighter/mychar");
        // No param/ at all — an added-character mod that only ships motion + model.
        touch(&fighter_dir.join("motion/body/c00/motion_list.bin"));
        assert!(fighter_dir_is_loadable(&fighter_dir, &[0]));
        // Slot list is consulted, so a mod whose only slot is c08 still resolves.
        let alt = root.path().join("fighter/other");
        touch(&alt.join("motion/body/c08/motion_list.bin"));
        assert!(fighter_dir_is_loadable(&alt, &[8]));
        // c00 is not assumed to exist.
        assert!(!fighter_dir_is_loadable(&alt, &[0]));
    }

    #[test]
    fn loadable_check_rejects_a_dir_with_neither_param_nor_motion() {
        let root = tempfile::tempdir().unwrap();
        let fighter_dir = root.path().join("fighter/empty");
        // An empty param/ directory is not a param prc.
        std::fs::create_dir_all(fighter_dir.join("param")).unwrap();
        assert!(!fighter_dir_is_loadable(&fighter_dir, &[]));
    }

    #[test]
    fn loadable_check_still_accepts_the_historical_param_only_layout() {
        // Strict superset of the old gate: anything that indexed before still indexes,
        // so vanilla fighters cannot disappear from the roster.
        let root = tempfile::tempdir().unwrap();
        let vl = root.path().join("fighter/mario");
        touch(&vl.join("param/vl.prc"));
        assert!(fighter_dir_is_loadable(&vl, &[]));

        let fp = root.path().join("fighter/other");
        touch(&fp.join("param/fighter_param.prc"));
        assert!(fighter_dir_is_loadable(&fp, &[]));
    }

    #[test]
    fn modded_fighters_are_distinguished_from_vanilla_ones() {
        assert!(VANILLA_FIGHTERS.contains(&"mario"));
        assert!(VANILLA_FIGHTERS.contains(&"ice_climber"));
        assert!(!VANILLA_FIGHTERS.contains(&"waluigi"));
    }

    /// GitHub ACMD names game frames directly, while live capture reports the corresponding
    /// zero-based motion frame. Equal in-game events must resolve to the same editor frame.
    #[test]
    fn script_and_motion_frames_name_the_same_game_frame() {
        for script in [1.0, 1.4, 1.5, 2.0, 5.999_998, 6.0, 6.000_002, 6.5, 10.75] {
            let motion = script - 1.0;
            assert_eq!(
                script_frame(script),
                motion_to_script_frame(motion),
                "script frame {script} and motion frame {motion} describe the same game instant"
            );
        }
        assert_eq!(script_frame(0.0), 1);
        assert_eq!(script_frame(-3.0), 1);
        assert_eq!(motion_to_script_frame(0.0), 1);
        assert_eq!(script_to_motion_frame(1), 0.0);
    }

    /// `frame()` is absolute and `wait()` is relative — mixing them up silently shifts every
    /// spawn after the first `wait`, which is exactly the shape of a "timings are wrong"
    /// report. Kirby's aerial neutral is the real case: frame 10, then three waits.
    #[test]
    fn wait_is_relative_and_frame_is_absolute() {
        let script = EffectScript {
            stmts: vec![
                EffectStmt::Frame(10.0),
                EffectStmt::Excute(vec![EffectMacro::Raw("x".into())]),
                EffectStmt::Wait(3.0),
                EffectStmt::Excute(vec![EffectMacro::Effect {
                    effect_name: "a".into(),
                    effect_name_alt: None,
                    spawn_func: "EFFECT".into(),
                    bone_name: "top".into(),
                    offset: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: 1.0,
                    follows_bone: false,
                    extra_args: Vec::new(),
                }]),
                EffectStmt::Wait(5.0),
                EffectStmt::Excute(vec![EffectMacro::Effect {
                    effect_name: "b".into(),
                    effect_name_alt: None,
                    spawn_func: "EFFECT".into(),
                    bone_name: "top".into(),
                    offset: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: 1.0,
                    follows_bone: false,
                    extra_args: Vec::new(),
                }]),
                // An absolute frame AFTER waits must not be treated as another wait.
                EffectStmt::Frame(20.0),
                EffectStmt::Excute(vec![EffectMacro::Effect {
                    effect_name: "c".into(),
                    effect_name_alt: None,
                    spawn_func: "EFFECT".into(),
                    bone_name: "top".into(),
                    offset: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: 1.0,
                    follows_bone: false,
                    extra_args: Vec::new(),
                }]),
            ],
        };
        let calls = script.to_effect_calls();
        let frames: Vec<u32> = calls.iter().map(|c| c.active_start).collect();
        assert_eq!(frames, vec![13, 18, 20]);
    }

    #[test]
    fn set_speed_ex_is_a_timed_point_with_an_editable_source_site() {
        let mut script = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::SetSpeedEx(SetSpeedExCall {
                    speed_x: 0.0,
                    speed_y: -2.5,
                    kinetic_kind: "*KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN".into(),
                })]),
                AcmdStmt::Wait(2.0),
                AcmdStmt::Excute(vec![ExcuteStmt::SetSpeedEx(SetSpeedExCall {
                    speed_x: 1.0,
                    speed_y: 0.5,
                    kinetic_kind: "*KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN".into(),
                })]),
            ],
        };
        assert_eq!(
            script
                .to_speed_ex_events()
                .iter()
                .map(|event| (event.frame, event.site))
                .collect::<Vec<_>>(),
            vec![(3, 0), (5, 1)]
        );
        let call = script.speed_ex_stmt_mut(1).expect("second speed point");
        call.speed_y = 1.25;
        assert_eq!(script.to_speed_ex_events()[1].call.speed_y, 1.25);
        assert_eq!(
            script.to_speed_ex_events()[1].call.kinetic_kind,
            "*KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN"
        );
    }

    #[test]
    fn speed_addition_and_correction_points_keep_independent_sites() {
        let mut script = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::AddSpeedNoLimit(AddSpeedNoLimitCall {
                        speed_x: 0.0,
                        speed_y: -2.5,
                    }),
                    ExcuteStmt::Correct(CorrectCall {
                        kind: "*GROUND_CORRECT_KIND_GROUND".into(),
                    }),
                ]),
                AcmdStmt::Wait(2.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::AddSpeedNoLimit(AddSpeedNoLimitCall {
                        speed_x: 1.0,
                        speed_y: 0.5,
                    }),
                    ExcuteStmt::Correct(CorrectCall { kind: "1".into() }),
                ]),
            ],
        };
        assert_eq!(
            script
                .to_add_speed_no_limit_events()
                .iter()
                .map(|event| (event.frame, event.site))
                .collect::<Vec<_>>(),
            vec![(3, 0), (5, 1)]
        );
        assert_eq!(
            script
                .to_correct_events()
                .iter()
                .map(|event| (event.frame, event.site))
                .collect::<Vec<_>>(),
            vec![(3, 0), (5, 1)]
        );
        script.add_speed_no_limit_stmt_mut(1).unwrap().speed_y = 1.25;
        script.correct_stmt_mut(1).unwrap().kind = "2".into();
        assert_eq!(script.to_add_speed_no_limit_events()[1].call.speed_y, 1.25);
        assert_eq!(script.to_correct_events()[1].call.kind, "2");
    }

    #[test]
    fn direct_speed_points_keep_their_own_site_cursor() {
        let mut script = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(2.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::SetSpeed(SetSpeedCall {
                        speed_x: 0.0,
                        speed_y: -2.0,
                    }),
                    ExcuteStmt::SetSpeedEx(SetSpeedExCall {
                        speed_x: 1.0,
                        speed_y: 0.5,
                        kinetic_kind: "0".into(),
                    }),
                ]),
                AcmdStmt::Wait(2.0),
                AcmdStmt::Excute(vec![ExcuteStmt::SetSpeed(SetSpeedCall {
                    speed_x: 2.0,
                    speed_y: 1.0,
                })]),
            ],
        };
        assert_eq!(
            script
                .to_speed_events()
                .iter()
                .map(|event| (event.frame, event.site))
                .collect::<Vec<_>>(),
            vec![(2, 0), (4, 1)]
        );
        assert_eq!(script.to_speed_ex_events()[0].site, 0);
        script.set_speed_stmt_mut(1).unwrap().speed_x = 3.5;
        assert_eq!(script.to_speed_events()[1].call.speed_x, 3.5);
    }
}
