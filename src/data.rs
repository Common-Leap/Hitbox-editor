/// All data types for Visionary's editor state.
use std::collections::HashMap;
use std::path::PathBuf;

/// The `ATTACK`-family macro assumed when nothing says otherwise — the overwhelming majority
/// of collisions, and what every project written before [`Hitbox::func`] existed contains.
pub fn default_attack_func() -> String {
    "ATTACK".to_string()
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

/// The `CATCH` arguments that are not editable properties of a grab box.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatchExtras {
    /// The status the grabbed fighter is put into, e.g. `FIGHTER_STATUS_KIND_CAPTURE_PULLED`.
    pub status: String,
    pub situation: String,
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
        }
    }
}

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
    Wind(WindboxData),
    EraseWind(u32),
    Clear(u32),
    ClearAll,
    /// GrabModule::clear_all — ends every open grab box, and only grab boxes.
    GrabClearAll,
    /// Any other line we don't interpret — preserved verbatim.
    Raw(String),
}

/// A timing statement in the script.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AcmdStmt {
    Frame(f32),
    Wait(f32),
    WaitLoopClear,
    Excute(Vec<ExcuteStmt>),
    Loop { count: usize, body: Vec<AcmdStmt> },
    Raw(String),
}

/// The parsed ACMD game_ function, preserving full structure for export.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AcmdScript {
    pub stmts: Vec<AcmdStmt>,
}

impl AcmdScript {
    /// Flatten the script into display hitboxes with computed frame ranges.
    pub fn to_hitboxes(&self) -> Vec<Hitbox> {
        let mut hitboxes: Vec<Hitbox> = Vec::new();
        eval_stmts(&self.stmts, 0.0, &mut hitboxes);
        for hb in hitboxes.iter_mut() {
            if hb.active_end == u32::MAX {
                hb.active_end = 9999;
            }
        }
        hitboxes
    }
}

fn eval_stmts(stmts: &[AcmdStmt], start_frame: f32, hitboxes: &mut Vec<Hitbox>) -> f32 {
    let mut frame = start_frame;
    for stmt in stmts {
        match stmt {
            AcmdStmt::Frame(f) => frame = *f,
            AcmdStmt::Wait(w) => frame += w,
            AcmdStmt::WaitLoopClear | AcmdStmt::Raw(_) => {}
            AcmdStmt::Excute(stmts) => {
                for s in stmts {
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
                        ExcuteStmt::Wind(wind) => {
                            let spawn = script_frame(frame);
                            if let Some(existing) = hitboxes.iter_mut().find(|hitbox| {
                                hitbox.category == 2
                                    && hitbox.id == wind.id()
                                    && hitbox.active_end >= spawn
                            }) {
                                existing.active_end =
                                    spawn.saturating_sub(1).max(existing.active_start);
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
                                hitbox.category == 1
                                    && hitbox.id == call.id
                                    && hitbox.active_end == u32::MAX
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
                                hitbox.category == 0
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
                            for hb in hitboxes.iter_mut().filter(|hitbox| hitbox.category == 0) {
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
                        ExcuteStmt::Raw(_) => {}
                    }
                }
            }
            AcmdStmt::Loop { count, body } => {
                for _ in 0..*count {
                    frame = eval_stmts(body, frame, hitboxes);
                }
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
    /// Hitboxes as loaded (GitHub fetch or live capture) — live hitbox rules diff vs this.
    pub hitboxes_pristine: Vec<Hitbox>,
    /// Provenance of the current move's ACMD data ("", "GitHub", "Live capture").
    pub acmd_source: String,
    /// User edits to effect calls, keyed by "fighter/move" (indices into pristine order).
    pub effect_call_edits: HashMap<String, Vec<EffectCallEdit>>,
    /// Full edited call list snapshot per "fighter/move" — what the mod exporter emits
    /// (a generated effect script replaces the whole move's spawn list).
    pub effect_call_full: HashMap<String, Vec<EffectCall>>,
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
            hitboxes_pristine: Vec::new(),
            acmd_source: String::new(),
            effect_call_edits: HashMap::new(),
            effect_call_full: HashMap::new(),
            selected_effect_call: None,
            show_all_effect_calls: false,
            show_effects_panel: false,
        }
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
        /// The call verbatim. A trail's arguments are textures and per-frame trail
        /// parameters, not a transform, so there is nothing to recompose it from.
        #[serde(default)]
        raw: String,
    },
    /// AFTER_IMAGE_OFF — turns off a sword trail.
    AfterImageOff,
    /// EFFECT_OFF_KIND — terminates a following effect by name.
    EffectOffKind { effect_name: String },
    /// LAST_EFFECT_SET_RATE — modifies the rate of the last spawned effect.
    LastEffectSetRate { rate: f32 },
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
/// All six are declared in smash-script's `macros.rs`, so all six are emittable. Every
/// argument is generic over `ToF32`, which is why they are written with plain `to_string`
/// rather than the decimal-forcing `num` — see the note on [`crate::data::WIND_COMMANDS`].
///
/// `FLASH_SET_DIRECTION` is deliberately absent: `sv_animcmd` has it and the corpus uses it
/// eight times, but smash-script never wrapped it, so modelling it would mean emitting a
/// macro that does not exist. It stays an unmodelled line, as it is today.
pub const COLOR_COMMANDS: &[(&str, bool, bool)] = &[
    ("FLASH", false, true),
    ("FLASH_FRM", true, true),
    ("BURN_COLOR", false, true),
    ("BURN_COLOR_FRAME", true, true),
    ("BURN_COLOR_NORMAL", false, false),
    ("START_INFO_FLASH_EYE", false, false),
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
    Loop { count: usize, body: Vec<EffectStmt> },
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
        let mut calls: Vec<EffectCall> = Vec::new();
        eval_effect_stmts(&self.stmts, 0.0, &mut calls);
        calls
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

fn eval_effect_stmts(stmts: &[EffectStmt], start_frame: f32, calls: &mut Vec<EffectCall>) -> f32 {
    let mut frame = start_frame;
    for stmt in stmts {
        match stmt {
            EffectStmt::Frame(f) => frame = *f,
            EffectStmt::Wait(w) => frame += w,
            EffectStmt::Raw(_) => {}
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
                                rate: None,
                                color: None,
                            });
                            anchor = Some(calls.len() - 1);
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
                                rate: None,
                                color: None,
                            });
                            // Deliberately NOT an anchor. A trail produces an `EffectCall` for
                            // the timeline, but it is drawn by the after-image system rather
                            // than spawned as an effect, so it is not what `LAST_EFFECT_SET_RATE`
                            // would find. Nothing in the corpus puts a rate after a trail, so
                            // refusing costs no coverage and avoids inventing an answer.
                            anchor = None;
                        }
                        EffectMacro::AfterImageOff => {
                            // Close the most recent open after-image effect.
                            if let Some(call) =
                                calls.iter_mut().rev().find(|c| c.active_end == 9999)
                            {
                                call.active_end = script_frame(frame);
                            }
                            anchor = None;
                        }
                        EffectMacro::LastEffectSetRate { rate } => {
                            // `anchor` is left in place: two rate lines in a row both name the
                            // same spawn, and the later one wins, exactly as in game.
                            if let Some(index) = anchor {
                                calls[index].rate = Some(*rate);
                            }
                        }
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
                                rate: None,
                                color: Some(color.clone()),
                            });
                            // Not a spawn, so not a rate anchor — a `LAST_EFFECT_SET_RATE`
                            // below a `FLASH` still belongs to whatever spawned before it, and
                            // the parser refuses to reach past this line to find it.
                            anchor = None;
                        }
                        EffectMacro::Raw(_) => anchor = None,
                    }
                }
            }
            EffectStmt::Loop { count, body } => {
                for _ in 0..*count {
                    frame = eval_effect_stmts(body, frame, calls);
                }
            }
        }
    }
    frame
}

impl EffectScript {
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
}
