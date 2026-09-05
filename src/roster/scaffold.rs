//! Creating a new character as a costume of a donor fighter.
//!
//! What this produces is a costume slot with its own model, animations, effects, name, and
//! **moveset**. What it does not produce is a separate cell on the character select grid —
//! that needs a runtime costume pin, which is `R-55` on the board and blocked for a measured
//! reason recorded in `docs/roster/PLAN.md`. The wizard says so before anything is created.
//!
//! ## Why the scaffold does not copy the donor's files
//!
//! Copying would produce a working character that is identical to the donor, and "working"
//! and "my model was picked up" would then look exactly the same. Creating the directories
//! empty makes the readiness report meaningful: a slot with no model is visibly a slot with
//! no model.
//!
//! ## The slot guard, and the trap inside it
//!
//! A Smashline script **replaces** the fighter's script for every costume, not just ours. So a
//! guard that returns early on other costumes does not "fall through to vanilla" — it silences
//! the move for every other costume of the donor, which is a far worse bug than the one it was
//! meant to avoid and shows up only when someone plays the donor normally.
//!
//! The guard therefore has two arms, and the vanilla arm is mandatory: our costume runs the
//! authored body, every other costume runs the fighter's original body, which Visionary
//! already has because it stores the source of every move it opens. [`wrap_with_slot_guard`]
//! will not produce a function without it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// The directories a new costume slot needs, relative to a mod root.
///
/// `model/body` and `motion/body` are where the user's own files go. The effect file is
/// per-slot too, so a character can have its own particles without touching the donor's.
pub fn directories(donor: &str, slot: u8) -> Vec<String> {
    let donor = donor.to_ascii_lowercase();
    vec![
        format!("fighter/{donor}/model/body/c{slot:02}"),
        format!("fighter/{donor}/motion/body/c{slot:02}"),
        format!("effect/fighter/{donor}"),
    ]
}

/// The per-slot effect file's path, relative to a mod root.
pub fn effect_file(donor: &str, slot: u8) -> String {
    let donor = donor.to_ascii_lowercase();
    format!("effect/fighter/{donor}/ef_{donor}_c{slot:02}.eff")
}

/// What a scaffold created.
#[derive(Debug, Clone)]
pub struct Scaffold {
    pub donor: String,
    pub slot: u8,
    /// Every slot scaffolded, ascending (single-slot scaffolds hold one).
    pub slots: Vec<u8>,
    pub created: Vec<String>,
}

/// Create the directory trees and placement guides for every slot in `slots`.
///
/// One mod root holds the whole range (c08–c15 lives in one folder), so this
/// loops the single-slot layout rather than inventing a second one. `slots`
/// must be non-empty; entries are de-duplicated and the scaffold reports them
/// ascending with the lowest as [`Scaffold::slot`].
pub fn create_many(mod_root: &Path, donor: &str, slots: &[u8]) -> Result<Scaffold> {
    if donor.trim().is_empty() {
        bail!("a new character needs a donor fighter to be a costume of");
    }
    if slots.is_empty() {
        bail!("a new character needs at least one costume slot");
    }
    let mut sorted: Vec<u8> = slots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut created = Vec::new();
    for slot in &sorted {
        for relative in directories(donor, *slot) {
            let path = mod_root.join(&relative);
            std::fs::create_dir_all(&path)?;
            if !created.contains(&relative) {
                created.push(relative);
            }
        }
        let guide = mod_root
            .join(format!(
                "fighter/{}/model/body/c{slot:02}",
                donor.to_ascii_lowercase()
            ))
            .join("PUT_YOUR_MODEL_HERE.txt");
        std::fs::write(&guide, placement_guide(donor, *slot))?;
        created.push(format!(
            "fighter/{}/model/body/c{slot:02}/PUT_YOUR_MODEL_HERE.txt",
            donor.to_ascii_lowercase()
        ));
    }
    Ok(Scaffold {
        donor: donor.to_ascii_lowercase(),
        slot: sorted[0],
        slots: sorted,
        created,
    })
}

/// Directories for every slot in `slots`, de-duplicated (the shared
/// `effect/fighter/<donor>` dir appears once).
pub fn directories_for_slots(donor: &str, slots: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for slot in slots {
        for relative in directories(donor, *slot) {
            if !out.contains(&relative) {
                out.push(relative);
            }
        }
    }
    out
}

/// Per-slot effect files for every slot in `slots`.
pub fn effect_files(donor: &str, slots: &[u8]) -> Vec<String> {
    slots.iter().map(|slot| effect_file(donor, *slot)).collect()
}

/// The note left in the model directory, naming the files the game expects.
fn placement_guide(donor: &str, slot: u8) -> String {
    let donor = donor.to_ascii_lowercase();
    format!(
        "This is costume slot c{slot:02} of {donor}, created by Visionary.\n\
         \n\
         Put your model here:\n\
         \x20 model.numdlb, model.numshb, model.numatb, model.nusktb, model.numshexb,\n\
         \x20 your .nutexb textures, and model.numatb's materials.\n\
         \n\
         Put your animations in:\n\
         \x20 fighter/{donor}/motion/body/c{slot:02}/\n\
         \x20 as .nuanmb files, plus a motion_list.bin naming them.\n\
         \n\
         Effects for this slot go in:\n\
         \x20 effect/fighter/{donor}/ef_{donor}_c{slot:02}.eff\n\
         \n\
         Visionary does not copy {donor}'s own files in here. That is deliberate: an empty\n\
         slot is visibly empty, whereas a copied one would look identical whether or not your\n\
         files were picked up.\n\
         \n\
         Delete this file once your model is in place.\n"
    )
}

/// Which parts of an authored character are present.
///
/// Reports what is *installed*, and — for the moveset — what is *registered*. Those are
/// different questions: a generated script with no `agent.acmd` line compiles, installs
/// nothing, and plays vanilla, so counting files would report a working moveset that does not
/// exist.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readiness {
    pub model_files: usize,
    pub motion_files: usize,
    pub has_motion_list: bool,
    pub has_effect: bool,
    pub has_name: bool,
    /// Moves the project has authored for this slot.
    pub authored_moves: usize,
    /// Of those, how many the generated plugin will actually install.
    pub registered_moves: usize,
}

impl Readiness {
    /// Human-readable outstanding items, most important first. Empty when nothing is missing.
    pub fn outstanding(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.model_files == 0 {
            out.push("no model files in the slot's model folder".to_string());
        }
        if self.motion_files == 0 {
            out.push("no animations in the slot's motion folder".to_string());
        } else if !self.has_motion_list {
            out.push(
                "animations are present but there is no motion_list.bin naming them, so the \
                 game will not find them"
                    .to_string(),
            );
        }
        if self.authored_moves > 0 && self.registered_moves < self.authored_moves {
            out.push(format!(
                "{} of {} authored move(s) are not registered, so they will compile and then \
                 play the donor's move instead",
                self.authored_moves - self.registered_moves,
                self.authored_moves
            ));
        }
        if !self.has_name {
            out.push("no display name set for this slot".to_string());
        }
        out
    }

    pub fn is_ready(&self) -> bool {
        self.outstanding().is_empty()
    }

    /// The starting-moveset moves this character has not replaced yet.
    ///
    /// Everything not in this list still plays the donor's version, which is the correct
    /// default and not a fault — so this is a checklist, not a set of warnings.
    pub fn remaining_starting_moves<'a>(
        &self,
        authored: &'a std::collections::BTreeSet<String>,
    ) -> Vec<&'a str> {
        MOVESET_TEMPLATE
            .iter()
            .filter(|name| !authored.contains(**name))
            .copied()
            .collect()
    }
}

/// What one costume slot holds, per file kind, plus where to open it.
///
/// `measure` answers "how ready is this authored slot"; this answers "what
/// is in each skin and where do I put more" for any fighter's slot —
/// vanilla's eight, extended slots, or a new character's. Counts union
/// across roots like everything else; directory answers prefer later roots
/// (mods) over the data root, so "open" lands somewhere writable.
#[derive(Debug, Clone, Default)]
pub struct SlotInventory {
    /// Model mesh files (`.numdlb/.numshb/.numatb/.nusktb/.numshexb`).
    pub meshes: usize,
    /// Textures (`.nutexb`) in the model folder.
    pub textures: usize,
    /// Other files in the model folder (materials aside, usually guides).
    pub other_model: usize,
    /// Animations (`.nuanmb`) in the motion folder.
    pub anims: usize,
    pub has_motion_list: bool,
    pub has_effect: bool,
    pub has_portrait: bool,
    /// Existing slot folders, mods preferred — the reveal targets.
    pub model_dir: Option<PathBuf>,
    pub motion_dir: Option<PathBuf>,
}

/// Model mesh extensions, lowercased.
const MESH_EXTS: &[&str] = &["numdlb", "numshb", "numatb", "nusktb", "numshexb"];

/// Inventory one costume slot: file counts by kind plus reveal targets.
///
/// `name_id` is the roster row's id for the portrait lookup, when the slot
/// has a row (a slot without one simply reports no portrait).
pub fn inventory(roots: &[PathBuf], donor: &str, slot: u8, name_id: Option<&str>) -> SlotInventory {
    let donor = donor.to_ascii_lowercase();
    let mut out = SlotInventory::default();
    // Later roots win, so walk reversed: the first hit is the mod the user
    // would edit, not the dumped game data underneath it.
    let mut rev: Vec<&PathBuf> = roots.iter().collect();
    rev.reverse();
    let model_rel = format!("fighter/{donor}/model/body/c{slot:02}");
    let motion_rel = format!("fighter/{donor}/motion/body/c{slot:02}");
    for root in &rev {
        let dir = root.join(&model_rel);
        if out.model_dir.is_none() && dir.is_dir() {
            out.model_dir = Some(dir.clone());
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if !entry.path().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name == "put_your_model_here.txt" {
                    continue;
                }
                let ext = name.rsplit('.').next().unwrap_or("");
                if MESH_EXTS.contains(&ext) {
                    out.meshes += 1;
                } else if ext == "nutexb" {
                    out.textures += 1;
                } else {
                    out.other_model += 1;
                }
            }
        }
        let dir = root.join(&motion_rel);
        if out.motion_dir.is_none() && dir.is_dir() {
            out.motion_dir = Some(dir.clone());
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if !entry.path().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name == "motion_list.bin" {
                    out.has_motion_list = true;
                } else if name.ends_with(".nuanmb") {
                    out.anims += 1;
                }
            }
        }
    }
    out.has_effect = roots
        .iter()
        .any(|root| root.join(effect_file(&donor, slot)).is_file());
    out.has_portrait = name_id
        .map(|nid| super::icons::find_portrait(roots, nid, slot).is_some())
        .unwrap_or(false);
    out
}

/// Scan the filesystem for what an authored slot has.
pub fn measure(roots: &[PathBuf], donor: &str, slot: u8, has_name: bool) -> Readiness {
    let donor = donor.to_ascii_lowercase();
    let count = |relative: String| -> (usize, bool) {
        let mut files = 0;
        let mut motion_list = false;
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root.join(&relative)) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.path().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                // The placement guide is Visionary's own note, not the user's content.
                if name == "put_your_model_here.txt" {
                    continue;
                }
                if name == "motion_list.bin" {
                    motion_list = true;
                }
                files += 1;
            }
        }
        (files, motion_list)
    };

    let (model_files, _) = count(format!("fighter/{donor}/model/body/c{slot:02}"));
    let (motion_files, has_motion_list) = count(format!("fighter/{donor}/motion/body/c{slot:02}"));
    let effect = effect_file(&donor, slot);
    Readiness {
        model_files,
        motion_files,
        has_motion_list,
        has_effect: roots.iter().any(|root| root.join(&effect).is_file()),
        has_name,
        authored_moves: 0,
        registered_moves: 0,
    }
}

/// How a slot's animations line up with the motion list that names them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimationBinding {
    /// Animations the motion list names that this slot provides.
    pub resolved: Vec<String>,
    /// Animations the motion list names that this slot does not provide.
    ///
    /// Not an error: the game falls back to the donor's base costume for these, which is
    /// usually what an author wants while only some animations are replaced. It is reported so
    /// "my animation is not playing" has an answer.
    pub falls_back_to_donor: Vec<String>,
    /// Files in the slot's motion folder that no motion list entry names.
    ///
    /// These will never play, whatever they are called. This is the failure mode worth
    /// surfacing: the file is present, so nothing looks wrong.
    pub unreferenced_files: Vec<String>,
}

impl AnimationBinding {
    pub fn summary(&self) -> String {
        format!(
            "{} animation(s) come from this costume, {} fall back to the base costume",
            self.resolved.len(),
            self.falls_back_to_donor.len()
        )
    }
}

/// Compare a slot's animation files against the motion list that names them.
///
/// Matching is by hash40, not by text. A motion list stores hashes, and `Hash40`'s text form
/// is the raw hex unless a global label table happens to be loaded — comparing those against
/// filenames matches nothing, silently, and reports every animation as missing.
///
/// `motion_list` is the donor's list — the one the game reads for this fighter — and
/// `slot_dir` is the costume's own motion folder. `labels` is used only to name the
/// animations that fall back, since their names exist nowhere else.
pub fn bind_animations(
    motion_list: &Path,
    slot_dir: &Path,
    labels: &std::collections::HashMap<u64, String>,
) -> Result<AnimationBinding> {
    let list = motion_lib::open(motion_list)
        .map_err(|error| anyhow::anyhow!("reading {}: {error:?}", motion_list.display()))?;

    let mut named: std::collections::BTreeSet<u64> = Default::default();
    for motion in list.list.values() {
        for animation in &motion.animations {
            named.insert(animation.name.0);
        }
    }

    // hash40 of the filename stem → the stem, which is the name to show for a file that is
    // present. A slot may legitimately hold two files whose stems differ only in case.
    let mut present: std::collections::BTreeMap<u64, String> = Default::default();
    if let Ok(entries) = std::fs::read_dir(slot_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if let Some(stem) = name.strip_suffix(".nuanmb") {
                present.insert(hash40::hash40(stem).0, stem.to_string());
            }
        }
    }

    let describe = |hash: u64| {
        labels
            .get(&hash)
            .cloned()
            .unwrap_or_else(|| format!("{hash:#018x}"))
    };

    let mut binding = AnimationBinding::default();
    for (hash, stem) in &present {
        if named.contains(hash) {
            binding.resolved.push(stem.clone());
        } else {
            binding.unreferenced_files.push(stem.clone());
        }
    }
    for hash in &named {
        if !present.contains_key(hash) {
            binding.falls_back_to_donor.push(describe(*hash));
        }
    }
    binding.resolved.sort();
    binding.falls_back_to_donor.sort();
    binding.unreferenced_files.sort();
    Ok(binding)
}

/// The moves worth replacing first, as ACMD script names.
///
/// Not generated code. A costume-backed character is already playable the moment its model is
/// in place, because it runs the donor's moveset until a move is replaced — so scaffolding
/// copies of the donor's scripts would add nothing but 24 functions that do exactly what the
/// absence of them does. What is useful is knowing which moves make a character feel like its
/// own, which is what this list is: the ground and air normals, the smashes, the throws, and
/// the four specials.
pub const MOVESET_TEMPLATE: &[&str] = &[
    "attack_11",
    "attack_12",
    "attack_13",
    "attack_dash",
    "attack_s3_s",
    "attack_hi3",
    "attack_lw3",
    "attack_s4_s",
    "attack_hi4",
    "attack_lw4",
    "attack_air_n",
    "attack_air_f",
    "attack_air_b",
    "attack_air_hi",
    "attack_air_lw",
    "catch",
    "throw_f",
    "throw_b",
    "throw_hi",
    "throw_lw",
    "special_n",
    "special_s",
    "special_hi",
    "special_lw",
];

/// Emit an authored function, the fighter's own function, and a dispatcher between them.
///
/// The dispatcher keeps the **registered** name, so the `agent.acmd` line the exporter already
/// writes needs no change. The authored body moves to `<name>_costume` and the fighter's own
/// script becomes `<name>_original`.
///
/// `authored_source` and `original_source` are whole `unsafe extern "C" fn …` definitions. Both
/// are renamed by replacing their declared name, which is exact rather than a substring
/// rewrite: the name appears once, in the declaration.
///
/// Emit an authored function, the fighter's own function, and a dispatcher between them,
/// gated to every slot in `slots`.
///
/// The dispatcher keeps the **registered** name, so the `agent.acmd` line the exporter already
/// writes needs no change. The authored body moves to `<name>_costume` and the fighter's own
/// script becomes `<name>_original`. A multi-skin character shares one moveset across all its
/// costumes, so the gate is an OR over its slots (`color == 8 || color == 9 …`).
///
/// `authored_source` and `original_source` are whole `unsafe extern "C" fn …` definitions. Both
/// are renamed by replacing their declared name, which is exact rather than a substring
/// rewrite: the name appears once, in the declaration.
///
/// The original arm is mandatory in the same way it is in [`wrap_with_slot_guard`]: without it
/// this move would be removed from every other costume of the donor.
pub fn costume_gated_source_multi(
    registered_name: &str,
    slots: &[u8],
    authored_source: &str,
    original_source: &str,
) -> String {
    let mut sorted: Vec<u8> = slots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    debug_assert!(!sorted.is_empty(), "a costume gate needs at least one slot");
    let condition = sorted
        .iter()
        .map(|slot| format!("color == {slot}"))
        .collect::<Vec<_>>()
        .join(" || ");
    let authored_name = format!("{registered_name}_costume");
    let original_name = format!("{registered_name}_original");
    format!(
        "{authored}\n{original}\n\
         unsafe extern \"C\" fn {registered_name}(agent: &mut L2CAgentBase) {{\n\
         \x20   // Costume gate, generated by Visionary. This script replaces the move for EVERY\n\
         \x20   // costume of this fighter, so the else arm runs the fighter's own script —\n\
         \x20   // without it, every other costume would lose this move entirely.\n\
         \x20   let color = smash::app::lua_bind::WorkModule::get_int(\n\
         \x20       agent.module_accessor,\n\
         \x20       *smash::lib::lua_const::FIGHTER_INSTANCE_WORK_ID_INT_COLOR,\n\
         \x20   );\n\
         \x20   if {condition} {{\n\
         \x20       {authored_name}(agent);\n\
         \x20   }} else {{\n\
         \x20       {original_name}(agent);\n\
         \x20   }}\n\
         }}\n",
        authored = rename_function(authored_source, registered_name, &authored_name),
        original = rename_declared_function(original_source, &original_name),
        registered_name = registered_name,
        condition = condition,
        authored_name = authored_name,
        original_name = original_name,
    )
}

/// The same function source as the costume arm of a gate names it.
///
/// Used by the export verifier so that "did this body reach disk" is still asked of a gated
/// move, rather than the check being skipped for it.
pub fn costume_arm_source(source: &str) -> String {
    let Some(name) = declared_name(source) else {
        return source.to_string();
    };
    rename_function(source, &name, &format!("{name}_costume"))
}

/// The name of the first function a source declares.
fn declared_name(source: &str) -> Option<String> {
    let start = source.find("fn ")? + 3;
    let open = source[start..].find('(')?;
    Some(source[start..start + open].trim().to_string())
}

/// Rename a function whose current name is known.
fn rename_function(source: &str, from: &str, to: &str) -> String {
    source.replacen(&format!("fn {from}("), &format!("fn {to}("), 1)
}

/// Rename whatever function a source defines first, to `to`.
///
/// The fighter's own script comes from the archive or from a live capture and is named after
/// the move, which is not always the name the exporter chose. Rewriting the declared name
/// whatever it is avoids depending on those agreeing.
fn rename_declared_function(source: &str, to: &str) -> String {
    match declared_name(source) {
        Some(name) => rename_function(source, &name, to),
        None => source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scaffold_creates_the_slots_own_directories_and_leaves_a_guide() {
        let dir = tempfile::tempdir().unwrap();
        let scaffold = create_many(dir.path(), "Mario", &[8]).unwrap();
        assert_eq!(scaffold.donor, "mario");
        assert!(dir.path().join("fighter/mario/model/body/c08").is_dir());
        assert!(dir.path().join("fighter/mario/motion/body/c08").is_dir());
        assert!(dir.path().join("effect/fighter/mario").is_dir());
        let guide = dir
            .path()
            .join("fighter/mario/model/body/c08/PUT_YOUR_MODEL_HERE.txt");
        assert!(guide.is_file());
        let text = std::fs::read_to_string(guide).unwrap();
        assert!(text.contains("motion/body/c08"));
        assert!(text.contains("ef_mario_c08.eff"));
    }

    /// Copying the donor's files would make "working" and "my model was picked up"
    /// indistinguishable.
    #[test]
    fn the_scaffold_copies_nothing_from_the_donor() {
        let dir = tempfile::tempdir().unwrap();
        create_many(dir.path(), "mario", &[8]).unwrap();
        let model = dir.path().join("fighter/mario/model/body/c08");
        let files: Vec<String> = std::fs::read_dir(&model)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(files, vec!["PUT_YOUR_MODEL_HERE.txt"]);
    }

    #[test]
    fn readiness_reports_an_empty_slot_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        create_many(dir.path(), "mario", &[8]).unwrap();
        let readiness = measure(&[dir.path().to_path_buf()], "mario", 8, false);
        assert_eq!(
            readiness.model_files, 0,
            "the guide file was counted as content"
        );
        assert_eq!(readiness.motion_files, 0);
        assert!(!readiness.is_ready());
        assert!(readiness
            .outstanding()
            .iter()
            .any(|note| note.contains("no model")));
    }

    /// Animations with no motion_list.bin are invisible to the game, which looks like the
    /// animations not being picked up at all.
    #[test]
    fn animations_without_a_motion_list_are_called_out() {
        let dir = tempfile::tempdir().unwrap();
        create_many(dir.path(), "mario", &[8]).unwrap();
        std::fs::write(
            dir.path()
                .join("fighter/mario/motion/body/c08/attack.nuanmb"),
            b"",
        )
        .unwrap();
        let readiness = measure(&[dir.path().to_path_buf()], "mario", 8, false);
        assert_eq!(readiness.motion_files, 1);
        assert!(!readiness.has_motion_list);
        assert!(readiness
            .outstanding()
            .iter()
            .any(|note| note.contains("motion_list.bin")));
    }

    /// The dossier splits the model folder into meshes vs textures so a slot
    /// with a model but no textures (untextured grey) reads differently from
    /// an empty one — and the guide file counts as neither.
    #[test]
    fn inventory_counts_meshes_textures_and_anims_separately() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("fighter/mario/model/body/c08");
        let motion = dir.path().join("fighter/mario/motion/body/c08");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::create_dir_all(&motion).unwrap();
        std::fs::write(model.join("model.numdlb"), b"mesh").unwrap();
        std::fs::write(model.join("face.nutexb"), b"tex").unwrap();
        std::fs::write(model.join("PUT_YOUR_MODEL_HERE.txt"), b"guide").unwrap();
        std::fs::write(motion.join("attack.nuanmb"), b"anim").unwrap();
        std::fs::write(motion.join("motion_list.bin"), b"list").unwrap();
        let inv = inventory(&[dir.path().to_path_buf()], "mario", 8, None);
        assert_eq!(inv.meshes, 1);
        assert_eq!(inv.textures, 1);
        assert_eq!(inv.other_model, 0);
        assert_eq!(inv.anims, 1);
        assert!(inv.has_motion_list);
        assert!(!inv.has_effect);
        assert!(!inv.has_portrait);
        assert_eq!(inv.model_dir, Some(model));
        assert_eq!(inv.motion_dir, Some(motion));
    }

    /// Reveal targets prefer mod roots: opening the dumped game data invites
    /// editing files the export will never ship.
    #[test]
    fn inventory_reveal_targets_prefer_mod_roots() {
        let base = tempfile::tempdir().unwrap();
        let modded = tempfile::tempdir().unwrap();
        for root in [base.path(), modded.path()] {
            std::fs::create_dir_all(root.join("fighter/mario/model/body/c00")).unwrap();
        }
        let inv = inventory(
            &[base.path().to_path_buf(), modded.path().to_path_buf()],
            "mario",
            0,
            None,
        );
        assert_eq!(
            inv.model_dir,
            Some(modded.path().join("fighter/mario/model/body/c00"))
        );
    }

    /// A created function with no registration compiles, installs nothing, and plays vanilla.
    /// Counting files rather than registrations would report a moveset that does not exist.
    #[test]
    fn unregistered_moves_are_reported_as_not_installed() {
        let readiness = Readiness {
            authored_moves: 5,
            registered_moves: 3,
            ..Default::default()
        };
        assert!(readiness
            .outstanding()
            .iter()
            .any(|note| note.contains("2 of 5 authored move(s) are not registered")));
    }

    const AUTHORED: &str = "unsafe extern \"C\" fn game_attack11(agent: &mut L2CAgentBase) {\n    frame(agent.lua_state_agent, 4.0);\n}\n";
    /// The archive spells the fighter's own script after the move, which is not always the
    /// name the exporter picked — hence renaming whatever is declared rather than a known name.
    const ORIGINAL: &str = "unsafe extern \"C\" fn game_attack_11(agent: &mut L2CAgentBase) {\n    frame(agent.lua_state_agent, 2.0);\n}\n";

    #[test]
    fn a_costume_gate_keeps_the_registered_name_and_renames_both_arms() {
        let generated = costume_gated_source_multi("game_attack11", &[8], AUTHORED, ORIGINAL);
        assert!(generated.contains("fn game_attack11_costume(agent"));
        assert!(generated.contains("fn game_attack11_original(agent"));
        assert!(generated.contains("unsafe extern \"C\" fn game_attack11(agent"));
        assert!(generated.contains("if color == 8 {"));
        assert!(generated.contains("game_attack11_costume(agent);"));
        assert!(generated.contains("game_attack11_original(agent);"));
        // The original's own name must be gone, or the crate has two functions defining it.
        assert!(!generated.contains("fn game_attack_11("));
    }

    /// The dispatcher is what the exporter registers, so it must exist exactly once under the
    /// registered name. Two definitions, or none, both fail to build.
    #[test]
    fn exactly_one_function_carries_the_registered_name() {
        let generated = costume_gated_source_multi("game_attack11", &[8], AUTHORED, ORIGINAL);
        assert_eq!(generated.matches("fn game_attack11(").count(), 1);
    }

    #[test]
    fn a_costume_gated_function_set_parses_as_rust() {
        let generated = costume_gated_source_multi("game_attack11", &[8], AUTHORED, ORIGINAL);
        let source = format!("mod generated {{ use smash::lib::lua_const::*; {generated} }}");
        if let Err(error) = syn::parse_file(&source) {
            panic!("costume-gated source does not parse: {error}\n\n{generated}");
        }
    }

    /// A file that no motion list names will never play, and nothing about the folder looks
    /// wrong. That is the case worth reporting; a missing one merely falls back.
    #[test]
    fn animation_binding_separates_fallbacks_from_files_nothing_names() {
        use motion_lib::mlist::{Animation, MList, Motion};

        let dir = tempfile::tempdir().unwrap();
        let mut list = MList::default();
        for name in ["attack_11", "attack_12"] {
            list.list.insert(
                hash40::hash40(name),
                Motion {
                    animations: vec![Animation {
                        name: hash40::hash40(name),
                        unk: 0,
                    }],
                    ..Default::default()
                },
            );
        }
        let list_path = dir.path().join("motion_list.bin");
        motion_lib::save(&list_path, &list).unwrap();

        let slot = dir.path().join("c08");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("attack_11.nuanmb"), b"").unwrap();
        std::fs::write(slot.join("typo_attack13.nuanmb"), b"").unwrap();

        let labels: std::collections::HashMap<u64, String> = ["attack_11", "attack_12"]
            .iter()
            .map(|name| (hash40::hash40(name).0, (*name).to_string()))
            .collect();
        let binding = bind_animations(&list_path, &slot, &labels).unwrap();
        assert_eq!(binding.resolved, vec!["attack_11"]);
        assert_eq!(binding.falls_back_to_donor, vec!["attack_12"]);
        assert_eq!(binding.unreferenced_files, vec!["typo_attack13"]);
    }

    #[test]
    fn the_starting_moveset_covers_recovery_and_has_no_duplicates() {
        assert!(MOVESET_TEMPLATE.contains(&"special_hi"));
        let unique: std::collections::BTreeSet<&&str> = MOVESET_TEMPLATE.iter().collect();
        assert_eq!(
            unique.len(),
            MOVESET_TEMPLATE.len(),
            "duplicate script name"
        );
    }

    /// A move not in the checklist is not a fault — it plays the donor's version, which is the
    /// correct default. The list shrinks as moves are replaced.
    #[test]
    fn the_starting_move_checklist_shrinks_as_moves_are_replaced() {
        let readiness = Readiness::default();
        let none = std::collections::BTreeSet::new();
        assert_eq!(
            readiness.remaining_starting_moves(&none).len(),
            MOVESET_TEMPLATE.len()
        );
        let some: std::collections::BTreeSet<String> = ["attack_11", "special_hi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let remaining = readiness.remaining_starting_moves(&some);
        assert_eq!(remaining.len(), MOVESET_TEMPLATE.len() - 2);
        assert!(!remaining.contains(&"special_hi"));
    }
}
