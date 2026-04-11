/// All data types for the hitbox editor state.

use std::collections::HashMap;
use std::path::PathBuf;

/// A single hitbox — used for display, timeline, and viewport rendering.
/// `active_start`/`active_end` are computed from the script structure.
/// When `capsule_end` is `Some`, the hitbox is a capsule; otherwise a sphere.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hitbox {
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
}

impl Default for Hitbox {
    fn default() -> Self {
        Self {
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
        }
    }
}

// ── ACMD script IR ────────────────────────────────────────────────────────────

/// A fully-parsed ATTACK(...) call — every parameter is named.
/// This is the source of truth for export; nothing is lost.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttackCall {
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
        }
    }
}

/// One statement inside an is_excute block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ExcuteStmt {
    Attack(AttackCall),
    ClearAll,
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
                            if let Some(existing) = hitboxes.iter_mut()
                                .find(|h| h.id == call.id && h.active_end == u32::MAX)
                            {
                                existing.active_end = (frame as u32).saturating_sub(1);
                            }
                            hitboxes.push(call.to_hitbox(frame as u32));
                        }
                        ExcuteStmt::ClearAll => {
                            let end = frame as u32;
                            for hb in hitboxes.iter_mut() {
                                if hb.active_end == u32::MAX {
                                    hb.active_end = end.saturating_sub(1);
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
    pub hitboxes: Vec<Hitbox>,
}

/// Persistent log of all edits, keyed fighter_name → move_name → record.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EditLog {
    /// fighter_name → move_name → record
    pub entries: HashMap<String, HashMap<String, EditRecord>>,
}

impl EditLog {
    pub fn save(&mut self, fighter: &str, fighter_display: &str, move_name: &str, script: AcmdScript, hitboxes: Vec<Hitbox>) {
        self.entries
            .entry(fighter.to_string())
            .or_default()
            .insert(move_name.to_string(), EditRecord {
                fighter: fighter.to_string(),
                fighter_display: fighter_display.to_string(),
                move_name: move_name.to_string(),
                script,
                hitboxes,
            });
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
        let mut v: Vec<(String, String)> = self.entries.iter()
            .map(|(k, moves)| {
                let display = moves.values().next()
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
        let mut v: Vec<String> = self.entries.get(fighter)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

#[derive(Debug, Clone)]
pub struct FighterEntry {
    pub name: String,
    pub display_name: String,
    pub param_path: PathBuf,
    pub motion_dir: PathBuf,
    pub model_dir: PathBuf,
    pub effect_dir: Option<PathBuf>,
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
    pub show_effects_panel: bool,
    // ── Effect rendering ──────────────────────────────────────────────────
    pub eff_index: Option<crate::effects::EffIndex>,
    pub ptcl: Option<crate::effects::PtclFile>,
    pub particle_system: crate::effects::ParticleSystem,
    pub trail_system: crate::effects::TrailSystem,
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
            current_frame: 0,
            total_frames: 0,
            playing: false,
            status: "Select a data root directory to begin.".to_string(),
            edit_log: EditLog::default(),
            effect_script: EffectScript::default(),
            effects: Vec::new(),
            show_effects_panel: false,
            eff_index: None,
            ptcl: None,
            particle_system: crate::effects::ParticleSystem::default(),
            trail_system: crate::effects::TrailSystem::default(),
        }
    }
}

pub fn fighter_display_name(name: &str) -> String {
    let map: &[(&str, &str)] = &[
        ("bayonetta", "Bayonetta"), ("brave", "Hero"), ("buddy", "Banjo & Kazooie"),
        ("captain", "Captain Falcon"), ("chrom", "Chrom"), ("cloud", "Cloud"),
        ("daisy", "Daisy"), ("dedede", "King Dedede"), ("demon", "Kazuya"),
        ("diddy", "Diddy Kong"), ("dolly", "Terry"), ("donkey", "Donkey Kong"),
        ("duckhunt", "Duck Hunt"), ("edge", "Sephiroth"), ("eflame", "Pyra"),
        ("elight", "Mythra"), ("element", "Aegis"), ("falco", "Falco"), ("fox", "Fox"),
        ("gamewatch", "Mr. Game & Watch"), ("ganon", "Ganondorf"), ("gaogaen", "Incineroar"),
        ("gekkouga", "Greninja"), ("ice_climber", "Ice Climbers"), ("ike", "Ike"),
        ("inkling", "Inkling"), ("jack", "Joker"), ("kamui", "Corrin"), ("ken", "Ken"),
        ("kirby", "Kirby"), ("koopa", "Bowser"), ("koopajr", "Bowser Jr."),
        ("krool", "King K. Rool"), ("link", "Link"), ("littlemac", "Little Mac"),
        ("lucario", "Lucario"), ("lucas", "Lucas"), ("lucina", "Lucina"), ("luigi", "Luigi"),
        ("mario", "Mario"), ("mariod", "Dr. Mario"), ("marth", "Marth"),
        ("metaknight", "Meta Knight"), ("mewtwo", "Mewtwo"), ("miifighter", "Mii Brawler"),
        ("miigunner", "Mii Gunner"), ("miisword", "Mii Swordfighter"),
        ("miiswordsman", "Mii Swordfighter"), ("murabito", "Villager"), ("ness", "Ness"),
        ("packun", "Piranha Plant"), ("pacman", "Pac-Man"), ("palutena", "Palutena"),
        ("peach", "Peach"), ("pfushigisou", "Ivysaur"), ("pichu", "Pichu"),
        ("pickel", "Steve"), ("pikachu", "Pikachu"), ("pikmin", "Olimar"),
        ("pit", "Pit"), ("pitb", "Dark Pit"), ("plizardon", "Charizard"),
        ("purin", "Jigglypuff"), ("pzenigame", "Squirtle"), ("reflet", "Robin"),
        ("richter", "Richter"), ("ridley", "Ridley"), ("robot", "R.O.B."),
        ("rockman", "Mega Man"), ("rosetta", "Rosalina"), ("roy", "Roy"), ("ryu", "Ryu"),
        ("samusd", "Dark Samus"), ("samus", "Samus"), ("sheik", "Sheik"),
        ("shizue", "Isabelle"), ("shulk", "Shulk"), ("simon", "Simon"), ("snake", "Snake"),
        ("sonic", "Sonic"), ("szerosuit", "Zero Suit Samus"), ("tantan", "Min Min"),
        ("toonlink", "Toon Link"), ("trail", "Sora"), ("wario", "Wario"),
        ("wiifit", "Wii Fit Trainer"), ("wolf", "Wolf"), ("yoshi", "Yoshi"),
        ("younglink", "Young Link"), ("zelda", "Zelda"), ("zenigame", "Squirtle"),
    ];
    for (k, v) in map {
        if *k == name { return v.to_string(); }
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
        bone_name: String,
        offset: [f32; 3],
        rotation: [f32; 3],
        scale: f32,
        /// `true` for EFFECT_FOLLOW / EFFECT_FOLLOW_FLIP / EFFECT_FLIP variants.
        follows_bone: bool,
    },
    /// AFTER_IMAGE4_ON / AFTER_IMAGE_ON — sword/weapon trail effects.
    AfterImage {
        effect_name: String,
        bone_name: String,
    },
    /// AFTER_IMAGE_OFF — turns off a sword trail.
    AfterImageOff,
    /// EFFECT_OFF_KIND — terminates a following effect by name.
    EffectOffKind { effect_name: String },
    /// LAST_EFFECT_SET_RATE — modifies the rate of the last spawned effect.
    LastEffectSetRate { rate: f32 },
    /// Any unrecognised line, preserved verbatim.
    Raw(String),
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EffectCall {
    pub effect_name: String,
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
}

impl EffectScript {
    /// Flatten the script into resolved `EffectCall`s with computed frame ranges.
    pub fn to_effect_calls(&self) -> Vec<EffectCall> {
        let mut calls: Vec<EffectCall> = Vec::new();
        eval_effect_stmts(&self.stmts, 0.0, &mut calls);
        calls
    }
}

fn eval_effect_stmts(
    stmts: &[EffectStmt],
    start_frame: f32,
    calls: &mut Vec<EffectCall>,
) -> f32 {
    let mut frame = start_frame;
    for stmt in stmts {
        match stmt {
            EffectStmt::Frame(f) => frame = *f,
            EffectStmt::Wait(w) => frame += w,
            EffectStmt::Raw(_) => {}
            EffectStmt::Excute(macros) => {
                for m in macros {
                    match m {
                        EffectMacro::Effect {
                            effect_name,
                            bone_name,
                            offset,
                            rotation,
                            scale,
                            follows_bone,
                        } => {
                            let active_end = if *follows_bone {
                                9999
                            } else {
                                frame as u32
                            };
                            calls.push(EffectCall {
                                effect_name: effect_name.clone(),
                                bone_name: bone_name.clone(),
                                offset: *offset,
                                rotation: *rotation,
                                scale: *scale,
                                follows_bone: *follows_bone,
                                active_start: frame as u32,
                                active_end,
                            });
                        }
                        EffectMacro::EffectOffKind { effect_name } => {
                            // Close the most recent open following effect with this name.
                            if let Some(call) = calls.iter_mut().rev().find(|c| {
                                &c.effect_name == effect_name && c.active_end == 9999
                            }) {
                                call.active_end = frame as u32;
                            }
                        }
                        EffectMacro::AfterImage { effect_name, bone_name } => {
                            // Sword/weapon trail — active until AfterImageOff
                            calls.push(EffectCall {
                                effect_name: effect_name.clone(),
                                bone_name: bone_name.clone(),
                                offset: [0.0; 3],
                                rotation: [0.0; 3],
                                scale: 1.0,
                                follows_bone: true,
                                active_start: frame as u32,
                                active_end: 9999,
                            });
                        }
                        EffectMacro::AfterImageOff => {
                            // Close the most recent open after-image effect.
                            if let Some(call) = calls.iter_mut().rev().find(|c| c.active_end == 9999) {
                                call.active_end = frame as u32;
                            }
                        }
                        EffectMacro::LastEffectSetRate { .. } | EffectMacro::Raw(_) => {}
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
