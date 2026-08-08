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
    /// Deliberately does not consult `effect_dropped_lines`. It is a note about what an export
    /// would lose, not an edit, so a project holding only that has nothing to build and must
    /// still count as empty — otherwise "No edits to export yet" turns into an export that
    /// produces no files.
    pub fn is_empty(&self) -> bool {
        self.fighters.values().all(|f| {
            f.acmd.is_empty()
                && f.effect_calls.values().all(|v| v.is_empty())
                && f.effect_calls_full.values().all(|v| v.is_empty())
                && f.sound_scripts.values().all(|s| s.stmts.is_empty())
                && f.expression_scripts.values().all(|s| s.stmts.is_empty())
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
    /// move name → the lines the effect export throws away, as measured when the move was last
    /// read from a script.
    ///
    /// Stored rather than derived because it cannot be derived later: the export regenerates the
    /// function from `effect_calls_full`, and a line that became no call is in neither the calls
    /// nor the output. This is the only field here that changes no generated code — it exists so
    /// that exporting a reloaded project reports the same losses as the source pane does with the
    /// script open. Moves that lost nothing are left out, and `#[serde(default)]` keeps projects
    /// saved before C6c loadable; both simply report nothing, which is what they did before.
    #[serde(default)]
    pub effect_dropped_lines: HashMap<String, Vec<String>>,
    /// move name → effect lines keyed by the frame they belong to, for frames whose block held
    /// no spawn to attach them to. See [`crate::data::EffectScript::to_effect_calls_and_residue`].
    ///
    /// Stored for the same reason as `effect_dropped_lines` and derived at the same moment, but
    /// unlike that field this one **does** change generated code: the emitter writes these lines
    /// out at their frame. A project saved before E3 has none, so `#[serde(default)]` loads it
    /// and it exports exactly as it did then — the lines are dropped, and the note in
    /// `effect_dropped_lines` written by that older build still says so.
    #[serde(default)]
    pub effect_frame_residue: HashMap<String, std::collections::BTreeMap<u32, Vec<String>>>,
    /// move name → the whole edited `sound_` script (what the exported sound script emits)
    ///
    /// The whole script, not the changed calls, for the reason `effect_calls_full` is whole:
    /// an installed `sound_` function replaces the fighter's own, so a partial one would
    /// silence every call it left out. `#[serde(default)]` keeps projects saved before D1d
    /// loadable — they simply have no sounds, which is what was true when they were written.
    #[serde(default)]
    pub sound_scripts: HashMap<String, crate::data::AcmdScript>,
    /// move name → whole edited `expression_` script. Like `sound_scripts`, installing one
    /// replaces that category for the move, so the complete statement tree is stored.
    #[serde(default)]
    pub expression_scripts: HashMap<String, crate::data::AcmdScript>,
    /// move name → warning recorded when the move was loaded from one live-capture path while
    /// the cached source contained runtime branches. This is provenance, not an edit: it keeps
    /// an export from presenting an observed arm as if it were the complete source.
    #[serde(default)]
    pub capture_branch_warnings: HashMap<String, String>,
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
    /// Pool textures the user replaced with their own image.
    #[serde(default)]
    pub textures: Vec<TextureImport>,
    /// Pool textures the user ADDED, beyond the ones the eff shipped with.
    #[serde(default)]
    pub textures_added: Vec<TextureAddition>,
    /// Names of pool textures the user removed.
    #[serde(default)]
    pub textures_removed: Vec<String>,
    /// Emitter sets whose emitter LIST the user changed — emitters removed, duplicated or
    /// re-ordered. Recorded per set, and only for sets that differ from the source.
    #[serde(default)]
    pub rosters: Vec<EmitterRoster>,
    /// Effect entries whose spawn structure the user changed: which parts play at which frame,
    /// and which external model comes with them.
    #[serde(default)]
    pub entry_edits: Vec<EntryEdit>,
}

impl EffMod {
    pub fn is_empty(&self) -> bool {
        self.authored.is_empty()
            && self.transplants.is_empty()
            && self.textures.is_empty()
            && self.textures_added.is_empty()
            && self.textures_removed.is_empty()
            && self.rosters.is_empty()
            && self.entry_edits.is_empty()
    }
}

/// The emitter list one effect should end up with.
///
/// Declarative rather than a log of add/remove operations: each slot names an emitter of the
/// SOURCE set to clone, so applying the roster twice gives the same result as applying it once,
/// and a source eff that gained an emitter between sessions cannot shift the whole list by one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmitterRoster {
    pub set_name: String,
    /// The entry (kind) name that plays this set, for the UI and for carrier retargeting.
    #[serde(default)]
    pub entry_name: String,
    pub set_idx: usize,
    pub slots: Vec<EmitterSlot>,
}

/// One emitter in the result, and where its data comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmitterSlot {
    /// Flat parent-first index of the SOURCE emitter this slot clones.
    pub source_idx: usize,
    /// Name of that source emitter, preferred over the index when both are available.
    #[serde(default)]
    pub source_name: String,
    /// Name this emitter carries in the result. A duplicate is renamed so that authored edits,
    /// which address emitters by name, can still tell the two apart.
    #[serde(default)]
    pub name: String,
    /// Nesting depth in the result: 0 is a root emitter, 1 a child of the slot above it.
    #[serde(default)]
    pub depth: u8,
}

/// Changes to what one effect entry SPAWNS — the eff header's side of an effect, as opposed to
/// the particle data the emitters carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryEdit {
    /// The entry (kind) name, e.g. `kirby_dash`. Entries are addressed by name only: the entry
    /// table's order is not stable across a transplant, which appends to it.
    pub entry_name: String,
    /// Replacement primary emitter set. `None` leaves it alone; `Some("")` removes it.
    /// Multi-part entries commonly have no primary set because their content lives in variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter_set: Option<String>,
    /// Replacement part list, each with its own start frame. `None` leaves the entry's parts
    /// alone; `Some(empty)` makes it a plain single-part effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<VariantEdit>>,
    /// Replacement external model. `None` leaves it alone; `Some` with an empty name removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelEdit>,
}

/// One part of a multi-part effect: an emitter set, when it starts, and what it hangs off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantEdit {
    /// Frames after the effect begins before this part comes in.
    pub start_frame: u16,
    /// Emitter set this part plays, by name. Empty means "no set", which the format stores as 0.
    pub set_name: String,
    /// Bone the part attaches to. Empty keeps the effect's own attachment.
    #[serde(default)]
    pub bone: String,
}

/// The model an effect spawns alongside its particles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEdit {
    /// Model name as the eff's model table spells it. Empty removes the model.
    pub name: String,
    /// The model's spawn condition byte.
    #[serde(default)]
    pub flag: u8,
}

/// One pool texture replaced by an image of the user's.
///
/// Keyed by texture NAME, not pool index: the carrier and the exported eff both rebuild their
/// pools (pruning drops everything unreferenced), so an index recorded against the editor's
/// view names a different texture by the time it is applied. Names survive that.
///
/// The PNG is referenced by path rather than embedded, so the project file stays small and a
/// rebuild picks up whatever the image file says now — edit it in your paint program, send
/// again, and the new pixels ship without re-importing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextureImport {
    pub texture_name: String,
    pub png_path: String,
    /// The PNG holds the texture's STORED channels rather than the editable form.
    ///
    /// Recorded per import because the two are not interchangeable: reading an editable image as
    /// raw (or the reverse) puts the shape in the wrong channel, which the game draws as a solid
    /// square. Defaults to false, so a project written before this field existed is read as
    /// editable — the only form there was.
    #[serde(default)]
    pub raw: bool,
}

/// A pool texture the user ADDED, on top of the ones the eff shipped with.
///
/// This is how one effect gets a texture of its own. A pool texture is shared by every emitter
/// that samples it, and the `ef_cmn_*` names are shared by dozens of effects inside a single eff,
/// so editing one to change a single effect changes all of them. Adding a private copy and
/// repointing just that emitter is the only way to alter one effect and leave the rest alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextureAddition {
    /// The new pool texture's name — unique within the eff.
    pub texture_name: String,
    /// The existing pool texture this one is shaped like: it supplies the format, the channel
    /// swizzle and the required dimensions, and the pixels too when `png_path` is empty.
    pub template_name: String,
    /// Pixels from this PNG. Empty means "an exact copy of `template_name`", which is a rename
    /// rather than a re-encode and so loses nothing.
    #[serde(default)]
    pub png_path: String,
    /// The PNG holds stored channels rather than the editable form. See [`TextureImport::raw`].
    #[serde(default)]
    pub raw: bool,
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
///
/// `attrs` is the general case and covers every field of the emitter; it is keyed by the stable
/// ids in [`crate::eff_attrs`]. The named fields below it predate that table and are kept for
/// projects saved before it existed — the editor re-records them as `attrs` the first time it
/// loads such a project, and both paths are applied (named first, so `attrs` wins a tie).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmitterFieldEdits {
    /// Attribute id → new value, e.g. `"emission.rate": 12.5`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attrs: std::collections::BTreeMap<String, crate::eff_attrs::AttrValue>,
    /// Sparse byte edits to emitter child sections. The section index disambiguates repeated
    /// magic values; the magic prevents a changed source file from applying bytes to the wrong
    /// section. Storing only changed offsets keeps projects small and avoids embedding source
    /// subsection blobs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subsections: Vec<SubsectionEdit>,
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
    /// Pool texture NAME this emitter's `sampler0` should read instead of its original one.
    ///
    /// A name rather than the picker's index: the index is against the editor's view of the
    /// merged eff, and every path that ships these bytes rebuilds its own pool — the carrier
    /// prunes everything unreferenced, so indices there mean something else entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubsectionEdit {
    pub index: usize,
    pub magic: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub bytes: std::collections::BTreeMap<usize, u8>,
}

impl EmitterFieldEdits {
    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty()
            && self.subsections.is_empty()
            && self.emission_rate.is_none()
            && self.lifetime.is_none()
            && self.scale.is_none()
            && self.color_scale.is_none()
            && self.emitter_scale.is_none()
            && self.color0.is_none()
            && self.color1.is_none()
            && self.alpha0.is_none()
            && self.texture_name.is_none()
    }

    /// Number of edited fields (for the edit-tree badges).
    pub fn count(&self) -> usize {
        self.attrs.len()
            + self
                .subsections
                .iter()
                .map(|section| section.bytes.len())
                .sum::<usize>()
            + [
                self.emission_rate.is_some(),
                self.lifetime.is_some(),
                self.scale.is_some(),
                self.color_scale.is_some(),
                self.emitter_scale.is_some(),
                self.color0.is_some(),
                self.color1.is_some(),
                self.alpha0.is_some(),
                self.texture_name.is_some(),
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

    #[test]
    fn kinetic_acmd_points_survive_project_round_trip() {
        use crate::data::{
            AcmdScript, AcmdStmt, ClrSpeedCall, ExcuteStmt, WorkFlagAction, WorkFlagCall,
            WorkModuleSetCall, WorkModuleSetKind, WorkTransitionTermAction, WorkTransitionTermCall,
        };

        let script = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(4.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::ClrSpeed(ClrSpeedCall {
                        kinetic_kind: "*FIGHTER_KINETIC_ENERGY_ID_GRAVITY".into(),
                    }),
                    ExcuteStmt::SetAir,
                    ExcuteStmt::WorkFlag(WorkFlagCall {
                        action: WorkFlagAction::On,
                        flag: "*FIGHTER_STATUS_ATTACK_FLAG_START_SMASH_HOLD".into(),
                    }),
                    ExcuteStmt::WorkTransitionTerm(WorkTransitionTermCall {
                        action: WorkTransitionTermAction::Enable,
                        transition_term: "*FIGHTER_STATUS_TRANSITION_TERM_ID_DASH_TO_RUN".into(),
                    }),
                    ExcuteStmt::WorkModuleSet(WorkModuleSetCall {
                        kind: WorkModuleSetKind::Int,
                        value: "4".into(),
                        slot: "*FIGHTER_STATUS_WORK_INT_NEXT_STEP".into(),
                    }),
                ]),
            ],
        };
        let mut project = ModProjectFile::default();
        project.fighters.insert(
            "mario".into(),
            FighterMod {
                acmd: HashMap::from([(
                    "attack_air_n".into(),
                    EditRecord {
                        fighter: "mario".into(),
                        fighter_display: "Mario".into(),
                        move_name: "attack_air_n".into(),
                        script: script.clone(),
                        hitboxes_pristine: Vec::new(),
                        hitboxes: Vec::new(),
                    },
                )]),
                ..Default::default()
            },
        );

        let written = serde_json::to_string(&project).unwrap();
        let reloaded: ModProjectFile = serde_json::from_str(&written).unwrap();
        let loaded = &reloaded.fighters["mario"].acmd["attack_air_n"].script;
        assert_eq!(loaded.to_clr_speed_events(), script.to_clr_speed_events());
        assert_eq!(loaded.to_set_air_events(), script.to_set_air_events());
        assert_eq!(loaded.to_work_flag_events(), script.to_work_flag_events());
        assert_eq!(
            loaded.to_work_transition_term_events(),
            script.to_work_transition_term_events()
        );
        assert_eq!(
            loaded.to_work_module_set_events(),
            script.to_work_module_set_events()
        );
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
