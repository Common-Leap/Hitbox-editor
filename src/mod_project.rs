// Unified mod project: every edit the toolkit makes — hitboxes/ACMD scripts, effect-call
// (spawn) edits, authored .eff value edits, effect transplants — in one serializable file.
// This file travels WITH exported mods so a mod can be re-opened for further editing.
//
// Two distinct concepts live near each other here and must not be conflated:
//   * TRANSPLANT — copying an emitter set out of a donor .eff into a fighter's .eff.
//     That's the operation; see `TransplantOp`.
//   * ONE-SLOT — scoping an edit so it only applies to specific costume/skin slots
//     (c00, c07, c12, c50 …; NOT limited to 0–7). That's a modifier, expressed as
//     `TransplantOp::one_slot_slots`, and it can ride on top of a transplant.

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
        Self {
            version: PROJECT_VERSION,
            name: "unnamed_mod".into(),
            fighters: HashMap::new(),
        }
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
    /// Emitter sets copied in from a donor eff. Serialized as `one_slot` before the
    /// transplant/one-slot split; the alias keeps pre-split projects loadable.
    #[serde(default, alias = "one_slot")]
    pub transplants: Vec<TransplantOp>,
}

impl EffMod {
    pub fn is_empty(&self) -> bool {
        self.authored.is_empty() && self.transplants.is_empty()
    }
}

/// One emitter's edited authored fields. Names are stored alongside indices; appliers
/// prefer the name match and fall back to the index with a warning (dump-revision drift).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthoredEdit {
    /// The EMITTER SET name (e.g. `P_KirbyDash`) — what `apply_authored` matches against
    /// `ptcl.emitter_list.emitter_sets[].name`.
    pub set_name: String,
    /// The ENTRY / KIND name (e.g. `kirby_dash`) — a DIFFERENT namespace: this is what the
    /// game spawns, what `TransplantOp::src_set_name` is resolved against (`entry_names`),
    /// and what an effect alias hashes. The two are related by
    /// `entries[i].emitter_set_id - 1 == set_idx`, not by position.
    ///
    /// Empty on projects saved before this field existed; callers that need a kind name must
    /// skip such edits rather than fall back to `set_name`, which would name a kind that does
    /// not exist and ship a carrier the game's loader can hang on.
    #[serde(default)]
    pub entry_name: String,
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

/// Suffix used when the editor SUGGESTS a name for a transplanted entry.
///
/// This is a default for the name box only — the user may rename a transplant to anything,
/// so nothing downstream may depend on it. The plugin is told each transplant's real kind
/// mapping explicitly (`EffectAliasWire { from: hash(new_entry_name), to: donor }`), and
/// that is what makes an arbitrary name resolve in game. The plugin's suffix matching is
/// only an additive fallback, and it accepts the historical `_os` spelling too.
pub const TRANSPLANT_SUFFIX: &str = "_tp";

/// Prefix for the RUNTIME-ONLY clone of an edited fighter effect.
///
/// Authored edits cannot be applied to the fighter's own eff in a live match (its reparse
/// rebuilds from the resident buffer and never re-reads the file), so the edited entry is
/// cloned into the live carrier and the original kind is aliased onto the clone. That clone
/// needs a name, and it must live in a namespace the user can never reach:
///
///  * NOT the transplant suffix — transplants take ANY user-chosen name, so a user could
///    type the exact name the editor generated and the two would fight over one alias.
///  * Reserved: [`is_reserved_entry_name`] rejects it in the transplant name box, so the
///    collision is impossible by construction rather than merely unlikely.
///
/// This name never reaches an exported mod. On export the edits are applied to the fighter's
/// OWN eff under its ORIGINAL entry name (`rebuild_eff_bytes` → `apply_authored`); the clone
/// exists purely so the running game can be updated without a reload it does not support.
pub const EDIT_CLONE_PREFIX: &str = "vsnedit_";

/// True if `name` is in a namespace Visionary reserves for its own generated entries.
///
/// Used to keep user-chosen transplant names out of the editor's internal namespace.
pub fn is_reserved_entry_name(name: &str) -> bool {
    name.trim().to_lowercase().starts_with(EDIT_CLONE_PREFIX)
}

/// A TRANSPLANT: copy an emitter set from a donor eff into this fighter's eff, either
/// under a new entry name or replacing an existing entry in place.
///
/// `one_slot_slots` is the (optional) ONE-SLOT scoping layered on top of the transplant —
/// the transplant is the operation, the slot list narrows which costumes see it.
///
/// Was named `OneSlotOp` before the transplant/one-slot split. The struct name is invisible
/// to serde, but the renamed `one_slot_slots` field carries a `serde(alias)` for its old
/// on-disk name (`slots`) so projects saved before the split still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransplantOp {
    pub new_entry_name: String,
    /// Donor .eff relative to the export root ("" = same file as the target).
    pub src_file_rel: String,
    pub src_set_name: String,
    pub src_set_idx: usize,
    /// ONE-SLOT scoping: costume slots this transplant applies to (0 = c00 …). Slot numbers
    /// are real costume indices and are NOT limited to 0–7 — added-costume mods use much
    /// larger ones. Empty = every costume: the transplant lands in the base
    /// ef_<fighter>.eff; otherwise it lands in ef_<fighter>_cXX.eff files, one per slot.
    /// Serialized as `slots` before the transplant/one-slot split.
    #[serde(default, alias = "slots", skip_serializing_if = "Vec::is_empty")]
    pub one_slot_slots: Vec<u8>,
    /// When set, the donor REPLACES this existing entry's emitter set(s) in place — every
    /// use switches, with no ACMD redirect needed — instead of being appended as a new
    /// entry. This is how a one-slot-scoped transplant is normally authored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_entry: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `modproject.json` exactly as builds before the transplant/one-slot rename wrote it:
    /// `EffMod.one_slot` holding `OneSlotOp`s whose costume scoping lived in `slots`.
    /// Both were renamed (`transplants` / `one_slot_slots`); the `serde(alias)`es are the
    /// only thing keeping the user's existing saved projects loadable, so this pins them.
    const LEGACY_PROJECT_JSON: &str = r#"{
      "version": 1,
      "name": "legacy_mod",
      "fighters": {
        "kirby": {
          "display": "Kirby",
          "acmd": {},
          "effect_calls": {},
          "effect_calls_full": {},
          "eff": {
            "source_rel": "effect/fighter/kirby/ef_kirby.eff",
            "authored": [],
            "one_slot": [
              {
                "new_entry_name": "alucard_backdash_os",
                "src_file_rel": "effect/assist/alucard/ef_alucard.eff",
                "src_set_name": "alucard_backdash",
                "src_set_idx": 3
              },
              {
                "new_entry_name": "kirby_swing_for_kirby_appeal",
                "src_file_rel": "",
                "src_set_name": "kirby_swing",
                "src_set_idx": 7,
                "slots": [0, 7, 12, 50],
                "replace_entry": "kirby_appeal"
              }
            ]
          },
          "live_tweaks": []
        }
      }
    }"#;

    #[test]
    fn legacy_project_file_still_deserializes() {
        let project: ModProjectFile =
            serde_json::from_str(LEGACY_PROJECT_JSON).expect("pre-rename project must still load");
        let eff = project.fighters["kirby"]
            .eff
            .as_ref()
            .expect("eff mod present");

        // `one_slot` → `transplants`: both ops survive, in order.
        assert_eq!(eff.transplants.len(), 2, "legacy `one_slot` array dropped");
        assert!(!eff.is_empty());
        assert_eq!(eff.transplants[0].new_entry_name, "alucard_backdash_os");
        assert_eq!(
            eff.transplants[0].src_file_rel,
            "effect/assist/alucard/ef_alucard.eff"
        );
        assert_eq!(eff.transplants[0].src_set_idx, 3);
        // Absent `slots` means "every costume" — no one-slot scoping.
        assert!(eff.transplants[0].one_slot_slots.is_empty());
        assert!(eff.transplants[0].replace_entry.is_none());

        // `slots` → `one_slot_slots`: the one-slot scoping on the second (replace) op,
        // including slots well past the vanilla c00–c07.
        assert_eq!(eff.transplants[1].one_slot_slots, vec![0, 7, 12, 50]);
        assert_eq!(
            eff.transplants[1].replace_entry.as_deref(),
            Some("kirby_appeal")
        );
    }

    #[test]
    fn legacy_project_round_trips_through_the_new_field_names() {
        let loaded: ModProjectFile = serde_json::from_str(LEGACY_PROJECT_JSON).unwrap();
        let written = serde_json::to_string(&loaded).unwrap();

        // New saves use the new on-disk names…
        assert!(written.contains("\"transplants\""), "{written}");
        assert!(written.contains("\"one_slot_slots\""), "{written}");
        assert!(!written.contains("\"one_slot\":"), "{written}");

        // …and reloading them yields the same project.
        let reloaded: ModProjectFile = serde_json::from_str(&written).unwrap();
        let a = reloaded.fighters["kirby"].eff.as_ref().unwrap();
        let b = loaded.fighters["kirby"].eff.as_ref().unwrap();
        assert_eq!(a.transplants.len(), b.transplants.len());
        for (x, y) in a.transplants.iter().zip(&b.transplants) {
            assert_eq!(x.new_entry_name, y.new_entry_name);
            assert_eq!(x.src_file_rel, y.src_file_rel);
            assert_eq!(x.src_set_name, y.src_set_name);
            assert_eq!(x.src_set_idx, y.src_set_idx);
            assert_eq!(x.one_slot_slots, y.one_slot_slots);
            assert_eq!(x.replace_entry, y.replace_entry);
        }
    }

    /// The one-slot side must not assume the vanilla 8 costumes anywhere in the format.
    #[test]
    fn one_slot_scoping_survives_slot_numbers_past_the_vanilla_eight() {
        let op = TransplantOp {
            new_entry_name: "donor_os".into(),
            src_file_rel: String::new(),
            src_set_name: "donor".into(),
            src_set_idx: 0,
            one_slot_slots: vec![8, 15, 16, 99, 255],
            replace_entry: Some("target".into()),
        };
        let json = serde_json::to_string(&op).unwrap();
        let back: TransplantOp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.one_slot_slots, vec![8, 15, 16, 99, 255]);
    }

    /// The plugin hardcodes this prefix (`acmd_hooks::EDIT_CLONE_PREFIX`) because it cannot
    /// depend on this crate. At spawn time it is the only signal that tells an authored edit's
    /// redirect apart from a transplant's — and they need opposite fallback behaviour when the
    /// carrier is not up yet. If the two ever drift, transplants silently render nothing.
    #[test]
    fn edit_clone_prefix_matches_the_plugin_copy() {
        let plugin_src =
            include_str!("../plugins/slight_replica/src/slight/effect_viewer/acmd_hooks.rs");
        let declared = plugin_src
            .lines()
            .find_map(|l| l.trim().strip_prefix("const EDIT_CLONE_PREFIX: &str = "))
            .map(|l| l.trim_end_matches(';').trim_matches('"'))
            .expect("the plugin no longer declares EDIT_CLONE_PREFIX");
        assert_eq!(
            declared, EDIT_CLONE_PREFIX,
            "editor and plugin disagree on the reserved edit-clone prefix"
        );
    }
}
