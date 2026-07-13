// Unified mod project: every edit the toolkit makes — hitboxes/ACMD scripts, effect-call
// (spawn) edits, authored .eff value edits, one-slot ops — in one serializable file.
// This file travels WITH exported mods so a mod can be re-opened for further editing.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::data::{EditRecord, EffectCallEdit};

pub const PROJECT_FILE_NAME: &str = "modproject.json";
pub const PROJECT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModProjectFile {
    pub version: u32,
    pub name: String,
    /// fighter name (e.g. "mario") → all edits for that fighter
    #[serde(default)]
    pub fighters: HashMap<String, FighterMod>,
}

impl Default for ModProjectFile {
    fn default() -> Self {
        Self { version: PROJECT_VERSION, name: "unnamed_mod".into(), fighters: HashMap::new() }
    }
}

impl ModProjectFile {
    pub fn is_empty(&self) -> bool {
        self.fighters.values().all(|f| {
            f.acmd.is_empty()
                && f.effect_calls.values().all(|v| v.is_empty())
                && f.eff.as_ref().map(|e| e.is_empty()).unwrap_or(true)
                && f.live_tweaks.is_empty()
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FighterMod {
    #[serde(default)]
    pub display: String,
    /// move name → hitbox/script edit (the hitbox editor's existing edit-log record)
    #[serde(default)]
    pub acmd: HashMap<String, EditRecord>,
    /// move name → effect-call (spawn) edits
    #[serde(default)]
    pub effect_calls: HashMap<String, Vec<EffectCallEdit>>,
    /// move name → full edited spawn list (what the exported effect script emits)
    #[serde(default)]
    pub effect_calls_full: HashMap<String, Vec<crate::data::EffectCall>>,
    /// authored .eff edits for this fighter's effect file
    #[serde(default)]
    pub eff: Option<EffMod>,
    /// User-set runtime color/speed multipliers, exported as LAST_EFFECT_SET_* lines in
    /// the generated effect scripts and re-applied live on project load.
    #[serde(default)]
    pub live_tweaks: Vec<LiveTweak>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffMod {
    /// Source .eff path relative to the dump export root,
    /// e.g. "effect/fighter/mario/ef_mario.eff".
    pub source_rel: String,
    #[serde(default)]
    pub authored: Vec<AuthoredEdit>,
    #[serde(default)]
    pub one_slot: Vec<OneSlotOp>,
}

impl EffMod {
    pub fn is_empty(&self) -> bool {
        self.authored.is_empty() && self.one_slot.is_empty()
    }
}

/// One emitter's edited authored fields. Names are stored alongside indices; appliers
/// prefer the name match and fall back to the index with a warning (dump-revision drift).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthoredEdit {
    pub set_name: String,
    pub set_idx: usize,
    pub emitter_name: String,
    pub emitter_idx: usize,
    pub fields: EmitterFieldEdits,
}

/// Absolute new values (pristine values are re-derivable from the source eff).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmitterFieldEdits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_rate: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter_scale: Option<[f32; 3]>,
    /// Color key rows: [r, g, b, time]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color0: Option<Vec<[f32; 4]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color1: Option<Vec<[f32; 4]>>,
    /// Alpha key rows: [value, time]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha0: Option<Vec<[f32; 2]>>,
}

impl EmitterFieldEdits {
    pub fn is_empty(&self) -> bool {
        self.emission_rate.is_none()
            && self.lifetime.is_none()
            && self.scale.is_none()
            && self.color_scale.is_none()
            && self.emitter_scale.is_none()
            && self.color0.is_none()
            && self.color1.is_none()
            && self.alpha0.is_none()
    }

    /// Number of edited fields (for the edit-tree badges).
    pub fn count(&self) -> usize {
        [
            self.emission_rate.is_some(),
            self.lifetime.is_some(),
            self.scale.is_some(),
            self.color_scale.is_some(),
            self.emitter_scale.is_some(),
            self.color0.is_some(),
            self.color1.is_some(),
            self.alpha0.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }
}

/// A user-set runtime multiplier on one effect kind (from the live-override color×/speed
/// controls). Exports as LAST_EFFECT_SET_COLOR / LAST_EFFECT_SET_RATE after each spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveTweak {
    pub effect_name: String,
    /// [r, g, b, a] multiplier (alpha currently display-only in the export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

/// Copy an emitter set from a donor eff into this fighter's eff under a new entry name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneSlotOp {
    pub new_entry_name: String,
    /// Donor .eff relative to the export root ("" = same file as the target).
    pub src_file_rel: String,
    pub src_set_name: String,
    pub src_set_idx: usize,
}

/// "effect/fighter/mario/ef_mario.eff" → "mario"; falls back to the file stem.
pub fn fighter_from_source_rel(source_rel: &str) -> String {
    let parts: Vec<&str> = source_rel.split('/').collect();
    if let Some(pos) = parts.iter().position(|p| *p == "fighter") {
        if let Some(name) = parts.get(pos + 1) {
            return (*name).to_string();
        }
    }
    std::path::Path::new(source_rel)
        .file_stem()
        .map(|s| s.to_string_lossy().trim_start_matches("ef_").to_string())
        .unwrap_or_else(|| source_rel.to_string())
}
