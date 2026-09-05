//! Emitting the roster into a mod folder.
//!
//! Two rules shape this module, both of them consequences of the project being **sparse**:
//!
//!  * The roster database is rebuilt from *base + overrides* on every export, never from a
//!    stored copy. A project therefore stays correct when the mods underneath it change, and
//!    an export never resurrects a base file from whenever the edit happened to be made.
//!  * Anything that could not be written is **reported by name**. An export that quietly drops
//!    an override the user made is the failure this whole module is arranged to avoid: the
//!    project still contains the edit, the mod does not, and nothing said so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::mod_project::RosterMod;

use super::css::{self, CharaDb};
use super::index::RosterIndex;
use super::names::{self, DetailedNameOverride, NameOverride};
use super::traits::{self, FighterTraits};

/// What an export produced, and what it could not.
#[derive(Debug, Default)]
pub struct RosterExport {
    /// Files written, relative to the mod root.
    pub files: Vec<String>,
    /// Edits that were not written, each phrased so the user can act on it.
    pub warnings: Vec<String>,
}

/// Resolve the project's position and visibility edits to `name_id → disp_order`.
///
/// An entry with no roster database row has nowhere to write its position. That is a real
/// state — an authored character whose row has not been created yet, or an edit left over from
/// a mod that is no longer enabled — so it is returned as an unwritable key rather than
/// dropped.
pub fn resolve_order(
    index: &RosterIndex,
    project: &RosterMod,
) -> (BTreeMap<String, i8>, Vec<String>) {
    let mut order = BTreeMap::new();
    let mut unwritable = Vec::new();
    for (key, position) in &project.order {
        match index.by_key(key).and_then(|entry| entry.name_id.clone()) {
            Some(name_id) => {
                order.insert(name_id, *position);
            }
            None => unwritable.push(key.to_string()),
        }
    }
    // Hidden wins over a position: an entry that is both moved and hidden is not on the
    // select screen, and writing its position would put it back.
    for key in &project.hidden {
        match index.by_key(key).and_then(|entry| entry.name_id.clone()) {
            Some(name_id) => {
                order.insert(name_id, css::OFF_ROSTER);
            }
            None => unwritable.push(key.to_string()),
        }
    }
    unwritable.sort();
    unwritable.dedup();
    (order, unwritable)
}

/// Resolve the project's name edits into `.xmsbt` overrides.
///
/// A name is written per costume slot, which is what lets a slot-backed character carry its
/// own name while the donor keeps its. A multi-skin character writes one override
/// per slot it owns, all carrying the same display text.
pub fn resolve_names(index: &RosterIndex, project: &RosterMod) -> (Vec<NameOverride>, Vec<String>) {
    let mut overrides = Vec::new();
    let mut unwritable = Vec::new();
    for (key, display) in &project.names {
        match index.by_key(key) {
            Some(entry) => match &entry.name_id {
                Some(name_id) => {
                    let slots = entry.backing.all_slots();
                    let slots = if slots.is_empty() {
                        vec![entry.backing.slot().unwrap_or(0)]
                    } else {
                        slots
                    };
                    for slot in slots {
                        overrides.push(NameOverride {
                            name_id: name_id.clone(),
                            slot,
                            display: display.clone(),
                        });
                    }
                }
                None => unwritable.push(key.to_string()),
            },
            None => unwritable.push(key.to_string()),
        }
    }
    overrides.sort_by(|a, b| (&a.name_id, a.slot).cmp(&(&b.name_id, b.slot)));
    unwritable.sort();
    unwritable.dedup();
    (overrides, unwritable)
}

/// Resolve per-label name variants (`chr0`/`chr1`/`chr2` independently) into detailed overrides.
pub fn resolve_detailed_names(
    index: &RosterIndex,
    project: &RosterMod,
) -> (Vec<DetailedNameOverride>, Vec<String>) {
    let mut overrides = Vec::new();
    let mut unwritable = Vec::new();
    for (key, variants) in &project.name_variants {
        if variants.is_empty() {
            continue;
        }
        match index.by_key(key) {
            Some(entry) => match &entry.name_id {
                Some(name_id) => {
                    // Fallback is the simple `names` entry if present, else display_name.
                    let fallback = project
                        .names
                        .get(key)
                        .cloned()
                        .or_else(|| Some(entry.display_name.clone()));
                    let slots = entry.backing.all_slots();
                    let slots = if slots.is_empty() {
                        vec![entry.backing.slot().unwrap_or(0)]
                    } else {
                        slots
                    };
                    for slot in slots {
                        overrides.push(DetailedNameOverride {
                            name_id: name_id.clone(),
                            slot,
                            chr0: variants.chr0.clone(),
                            chr1: variants.chr1.clone(),
                            chr2: variants.chr2.clone(),
                            fallback: fallback.clone(),
                        });
                    }
                }
                None => unwritable.push(key.to_string()),
            },
            None => unwritable.push(key.to_string()),
        }
    }
    overrides.sort_by(|a, b| (&a.name_id, a.slot).cmp(&(&b.name_id, b.slot)));
    unwritable.sort();
    unwritable.dedup();
    (overrides, unwritable)
}

/// Resolve per-costume names (`fighter -> slot -> display`) into overrides.
pub fn resolve_per_costume_names(
    index: &RosterIndex,
    project: &RosterMod,
) -> (Vec<NameOverride>, Vec<String>) {
    let mut overrides = Vec::new();
    let mut unwritable = Vec::new();
    for (fighter, slots) in &project.per_costume_names {
        // Find a representative entry for the fighter to get its name_id.
        // Prefer a RosterKey::fighter entry, fall back to any entry with that fighter string.
        let representative = index
            .entries
            .iter()
            .find(|e| {
                e.fighter
                    .as_deref()
                    .map(|f| f.eq_ignore_ascii_case(fighter))
                    .unwrap_or(false)
            })
            .or_else(|| index.by_key(&crate::roster::RosterKey::fighter(fighter)));
        let name_id = match representative.and_then(|e| e.name_id.clone()) {
            Some(id) => id,
            None => {
                for slot in slots.keys() {
                    unwritable.push(format!("{fighter} c{slot:02}"));
                }
                continue;
            }
        };
        for (slot, display) in slots {
            overrides.push(NameOverride {
                name_id: name_id.clone(),
                slot: *slot,
                display: display.clone(),
            });
        }
    }
    overrides.sort_by(|a, b| (&a.name_id, a.slot).cmp(&(&b.name_id, b.slot)));
    unwritable.sort();
    unwritable.dedup();
    (overrides, unwritable)
}

/// Collect every name label the project wants to write, with stale keys reported.
pub fn resolve_all_names(
    index: &RosterIndex,
    project: &RosterMod,
) -> (Vec<(String, String)>, Vec<String>) {
    let (simple, mut unwritable) = resolve_names(index, project);
    let (detailed, unw2) = resolve_detailed_names(index, project);
    unwritable.extend(unw2);
    let (per_costume, unw3) = resolve_per_costume_names(index, project);
    unwritable.extend(unw3);

    // Detailed overrides take precedence over simple ones for the same (name_id,slot,chr).
    // To avoid duplicate labels, build a map label->text where detailed wins.
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for ov in simple {
        for (label, text) in ov.labels() {
            map.entry(label).or_insert(text);
        }
    }
    for ov in &per_costume {
        for (label, text) in ov.labels() {
            map.entry(label).or_insert(text);
        }
    }
    for ov in detailed {
        for (label, text) in ov.labels() {
            map.insert(label, text);
        }
    }
    let mut labels: Vec<(String, String)> = map.into_iter().collect();
    labels.sort_by(|a, b| a.0.cmp(&b.0));
    unwritable.sort();
    unwritable.dedup();
    (labels, unwritable)
}

/// Resolve per-entry `ui_chara_db` field overrides (color_num, etc.) into `name_id -> patch`.
pub fn resolve_chara_overrides(
    index: &RosterIndex,
    project: &RosterMod,
) -> (
    BTreeMap<String, crate::mod_project::CharaOverrides>,
    Vec<String>,
) {
    let mut out = BTreeMap::new();
    let mut unwritable = Vec::new();
    for (key, patch) in &project.chara_overrides {
        match index.by_key(key).and_then(|e| e.name_id.clone()) {
            Some(name_id) => {
                out.insert(name_id, patch.clone());
            }
            None => unwritable.push(key.to_string()),
        }
    }
    unwritable.sort();
    unwritable.dedup();
    (out, unwritable)
}

/// Write every fighter's trait edits into one rebuilt `fighter_param.prc`.
///
/// One file holds every fighter in the game, so this rebuilds it from the base once and
/// applies each fighter's sparse edits into it in turn. Writing a file per fighter — or
/// copying the base per fighter — would have each copy overwrite the others' changes.
///
/// Edits whose field is gone from the base file are reported by name. That is the whole
/// difference between a mod that quietly lacks a change and one that says which change it
/// lacks.
pub fn export_params(
    mod_root: &Path,
    ui_root: Option<&Path>,
    params: &BTreeMap<String, crate::mod_project::ParamMod>,
    labels: &std::collections::HashMap<u64, String>,
    report: &mut RosterExport,
) -> Result<()> {
    let with_edits: Vec<(&String, &BTreeMap<String, crate::mod_project::ParamValue>)> = params
        .iter()
        .map(|(fighter, param_mod)| (fighter, traits::edits_for(param_mod)))
        .filter(|(_, edits)| !edits.is_empty())
        .collect();
    if with_edits.is_empty() {
        return Ok(());
    }

    let Some(root) = ui_root else {
        report.warnings.push(format!(
            "{} fighter value change(s) were not written: {} was not found. Dump the \
             fighter/common folder along with the rest of fighter/.",
            with_edits
                .iter()
                .map(|(_, edits)| edits.len())
                .sum::<usize>(),
            traits::FIGHTER_PARAM_PATH
        ));
        return Ok(());
    };
    let source = root.join(traits::FIGHTER_PARAM_PATH);
    let destination = mod_root.join(traits::FIGHTER_PARAM_PATH);

    // Each fighter's row is applied into the *same* file. After the first, the base is the
    // partially edited copy in the destination, which is what keeps the earlier fighters'
    // changes in it.
    let mut base = source.clone();
    let mut wrote = false;
    for (fighter, edits) in with_edits {
        let mut loaded = match FighterTraits::open(&base, fighter, labels) {
            Ok(loaded) => loaded,
            Err(error) => {
                report.warnings.push(format!(
                    "{fighter}: value changes were not written — {error:#}"
                ));
                continue;
            }
        };
        let missing = loaded.apply(edits);
        for key in missing {
            report.warnings.push(format!(
                "{fighter}: the field \"{key}\" no longer exists in the game's values file, so \
                 that change was not written. It is still saved in the project."
            ));
        }
        loaded.save(&destination)?;
        base = destination.clone();
        wrote = true;
    }
    if wrote {
        report.files.push(traits::FIGHTER_PARAM_PATH.to_string());
    }
    Ok(())
}

/// Write the project's roster edits into `mod_root`.
///
/// `ui_root` is the arc root the base roster database is read from. Without one the position
/// and visibility edits cannot be written at all — there is no file to apply them to — and
/// that is reported rather than treated as "nothing to do".
pub fn export_roster(
    mod_root: &Path,
    ui_root: Option<&Path>,
    index: &RosterIndex,
    project: &RosterMod,
) -> Result<RosterExport> {
    let mut report = RosterExport::default();

    let (order, unwritable_order) = resolve_order(index, project);
    for key in unwritable_order {
        report.warnings.push(format!(
            "{key}: no character select entry exists for this character, so its position could \
             not be written. It is still saved in the project."
        ));
    }

    let (chara_patches, unwritable_chara) = resolve_chara_overrides(index, project);
    for key in unwritable_chara {
        report.warnings.push(format!(
            "{key}: no character select entry exists for this character, so its ui_chara_db patch could not be written. It is still saved in the project."
        ));
    }

    let needs_db = !order.is_empty() || !chara_patches.is_empty();

    if needs_db {
        match ui_root {
            Some(root) => {
                let source = root.join(css::CHARA_DB_PATH);
                let mut db = CharaDb::open(&source)
                    .with_context(|| "reading the base character select database")?;
                if !order.is_empty() {
                    let missing = db.apply_order(&order);
                    for name_id in missing {
                        report.warnings.push(format!(
                            "{name_id}: the character select database no longer contains this \
                             character, so its position was not written."
                        ));
                    }
                }
                if !chara_patches.is_empty() {
                    let missing = db.apply_chara_patches(&chara_patches);
                    for name_id in missing {
                        report.warnings.push(format!(
                            "{name_id}: the character select database no longer contains this \
                             character, so its ui_chara_db patch was not written."
                        ));
                    }
                }
                let destination = mod_root.join(css::CHARA_DB_PATH);
                db.save(&destination)?;
                report.files.push(css::CHARA_DB_PATH.to_string());
            }
            None => report.warnings.push(format!(
                "{} position/visibility/ui_chara_db change(s) were not written: no character select \
                 database was found. Dump the game's ui/ folder alongside fighter/ and effect/.",
                order.len() + chara_patches.len()
            )),
        }
    }

    let (all_labels, unwritable_names) = resolve_all_names(index, project);
    for key in unwritable_names {
        report.warnings.push(format!(
            "{key}: no character select entry exists for this character, so its name could not \
             be written. It is still saved in the project."
        ));
    }
    if names::write_xmsbt_labels(mod_root, &all_labels)?.is_some() {
        report.files.push(names::XMSBT_PATH.to_string());
    }

    // UI image overrides
    if !project.ui_images.is_empty() {
        super::ui_images::export_ui_images(mod_root, index, &project.ui_images, &mut report)?;
    }

    Ok(report)
}

/// Copy an authored character's own files into the exported mod folder.
///
/// An exported mod has to be self-contained: the character's model, animations, and effects
/// live in whatever folder the wizard created, which is not somewhere the person installing
/// the mod will have. Copying is what makes the export shippable.
///
/// A character with no files yet is reported rather than skipped, because a mod containing a
/// costume-gated moveset and no model is a character that appears as the donor wearing the
/// donor's clothes, running the wrong moveset — which looks like the gate being broken.
pub fn export_authored_files(
    mod_root: &Path,
    roots: &[PathBuf],
    authored: &[crate::mod_project::AuthoredEntry],
    report: &mut RosterExport,
) -> Result<()> {
    for entry in authored {
        // Every skin ships, not just the primary slot: a multi-skin character
        // whose extra costumes are missing plays the donor's files (or
        // nothing) under a moveset gated to slots that do not exist.
        let all = entry.all_slots();
        let mut copied = 0;
        for relative in super::scaffold::directories_for_slots(&entry.donor, &all) {
            for root in roots {
                copied += copy_tree(&root.join(&relative), &mod_root.join(&relative))?;
            }
        }
        for effect in super::scaffold::effect_files(&entry.donor, &all) {
            for root in roots {
                let source = root.join(&effect);
                if source.is_file() {
                    let destination = mod_root.join(&effect);
                    if let Some(parent) = destination.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&source, &destination)?;
                    copied += 1;
                }
            }
        }
        if copied == 0 {
            report.warnings.push(format!(
                "{}: no model, animation, or effect files were found for costume {} of {}, \
                 so the exported mod contains its moveset but not the character itself.",
                entry.display_name,
                slot_span(&all),
                entry.donor
            ));
        } else {
            report
                .files
                .push(format!("{} ({copied} file(s))", entry.display_name));
        }
    }
    Ok(())
}

/// `c08` for one slot, `c08…c15` for a range — for messages about a
/// character's whole costume block.
fn slot_span(slots: &[u8]) -> String {
    match slots {
        [] => "—".to_string(),
        [only] => format!("c{only:02}"),
        _ => {
            let (Some(first), Some(last)) = (slots.first(), slots.last()) else {
                return "—".to_string();
            };
            format!("c{first:02}…c{last:02}")
        }
    }
}

/// Copy a directory's files, returning how many were copied.
///
/// Visionary's own placement note is skipped: it is a hint for the author, not content, and
/// shipping it inside a released mod would be noise.
fn copy_tree(source: &Path, destination: &Path) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(source) else {
        return Ok(0);
    };
    let mut copied = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            copied += copy_tree(&path, &destination.join(&name))?;
            continue;
        }
        if name
            .to_string_lossy()
            .eq_ignore_ascii_case("PUT_YOUR_MODEL_HERE.txt")
        {
            continue;
        }
        std::fs::create_dir_all(destination)?;
        std::fs::copy(&path, destination.join(&name))?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{FighterEntry, FighterSource};
    use crate::mod_project::AuthoredEntry;
    use crate::roster::css::{fighter_kind_hash, test_db};
    use crate::roster::library::ModLibrary;
    use crate::roster::RosterKey;

    fn fighter(name: &str) -> FighterEntry {
        FighterEntry {
            name: name.to_string(),
            display_name: name.to_uppercase(),
            param_path: PathBuf::new(),
            motion_dir: PathBuf::new(),
            model_dir: PathBuf::new(),
            effect_dir: None,
            slots: vec![0],
            fighter_dir: PathBuf::new(),
            source: FighterSource::DataRoot,
        }
    }

    fn setup() -> (RosterIndex, CharaDb) {
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("link", 2, fighter_kind_hash("link"), true),
        ]);
        let index = RosterIndex::build(
            &[fighter("mario"), fighter("link")],
            &ModLibrary::default(),
            &RosterMod::default(),
            Some(&db),
        );
        (index, db)
    }
    #[test]
    fn positions_resolve_through_the_entrys_database_row() {
        let (index, _) = setup();
        let mut project = RosterMod::default();
        project.order.insert(RosterKey::fighter("mario"), 5);
        let (order, unwritable) = resolve_order(&index, &project);
        assert_eq!(order.get("mario"), Some(&5));
        assert!(unwritable.is_empty());
    }

    /// One portrait per skin: the bare kind lands on the entry's slot and
    /// each suffixed key on its own, so costumes stop sharing one face.
    #[test]
    fn per_slot_portraits_export_to_their_own_slots() {
        use crate::mod_project::UiImageOverride;
        let (index, _) = setup();
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("face.png");
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 100, 50, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::fs::File::create(&png_path).unwrap(),
                image::ImageFormat::Png,
            )
            .unwrap();
        let mut kinds = BTreeMap::new();
        for stored_key in ["chara_1", "chara_1#c02"] {
            kinds.insert(
                stored_key.to_string(),
                UiImageOverride {
                    png_path: png_path.display().to_string(),
                    ..Default::default()
                },
            );
        }
        let overrides = std::collections::BTreeMap::from([(RosterKey::fighter("mario"), kinds)]);
        let mod_root = dir.path().join("mod");
        let mut report = RosterExport::default();
        crate::roster::ui_images::export_ui_images(&mod_root, &index, &overrides, &mut report)
            .unwrap();
        assert!(mod_root
            .join("ui/replace/chara/chara_1/chara_1_mario_00.bntx")
            .is_file());
        assert!(mod_root
            .join("ui/replace/chara/chara_1/chara_1_mario_02.bntx")
            .is_file());
    }

    /// Hidden must win over a position. An entry that is both moved and hidden is not on the
    /// select screen, and writing its position would put it back.
    #[test]
    fn hiding_overrides_a_position_for_the_same_entry() {
        let (index, _) = setup();
        let key = RosterKey::fighter("mario");
        let mut project = RosterMod::default();
        project.order.insert(key.clone(), 5);
        project.hidden.insert(key);
        let (order, _) = resolve_order(&index, &project);
        assert_eq!(order.get("mario"), Some(&css::OFF_ROSTER));
    }

    /// The failure this guards is exactly the one the module exists to avoid: the project
    /// keeps the edit, the mod does not get it, and nothing says so.
    #[test]
    fn an_edit_with_no_database_row_is_reported_by_name_not_dropped() {
        let index = RosterIndex::build(
            &[fighter("mario")],
            &ModLibrary::default(),
            &RosterMod::default(),
            None,
        );
        let mut project = RosterMod::default();
        project.order.insert(RosterKey::fighter("mario"), 5);
        project
            .names
            .insert(RosterKey::fighter("mario"), "Jumpman".into());

        let (order, unwritable) = resolve_order(&index, &project);
        assert!(order.is_empty());
        assert_eq!(unwritable, vec!["mario"]);

        let (overrides, unwritable) = resolve_names(&index, &project);
        assert!(overrides.is_empty());
        assert_eq!(unwritable, vec!["mario"]);
    }

    /// A slot-backed character's name is written against its own slot, or it would rename the
    /// donor's default costume instead of itself.
    #[test]
    fn a_slot_backed_entrys_name_is_written_against_its_slot() {
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("vision", 87, fighter_kind_hash("mario"), true),
        ]);
        let key = RosterKey::slot("mario", 8);
        let mut project = RosterMod::default();
        project.authored.push(AuthoredEntry {
            key: key.clone(),
            donor: "mario".into(),
            slot: 8,
            slots: Vec::new(),
            display_name: "Vision".into(),
            name_id: "vision".into(),
            moveset_scaffolded: true,
            files_root: None,
        });
        project.names.insert(key, "Vision".into());
        let index = RosterIndex::build(
            &[fighter("mario")],
            &ModLibrary::default(),
            &project,
            Some(&db),
        );

        let (overrides, unwritable) = resolve_names(&index, &project);
        assert!(unwritable.is_empty());
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].name_id, "vision");
        assert_eq!(overrides[0].slot, 8);
    }

    /// Rebuilding from base + overrides is what keeps a project correct when the mods
    /// underneath it change. A stored copy would pin whatever base file was installed when the
    /// edit was made.
    #[test]
    fn the_written_database_is_the_base_file_with_only_the_overrides_applied() {
        let dir = tempfile::tempdir().unwrap();
        let ui_root = dir.path().join("ui_root");
        let mod_root = dir.path().join("mod");
        let (index, db) = setup();
        let source = ui_root.join(css::CHARA_DB_PATH);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        db.save(&source).unwrap();

        let mut project = RosterMod::default();
        project.order.insert(RosterKey::fighter("mario"), 5);

        let report = export_roster(&mod_root, Some(&ui_root), &index, &project).unwrap();
        assert_eq!(report.files, vec![css::CHARA_DB_PATH]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        let written = CharaDb::open(&mod_root.join(css::CHARA_DB_PATH)).unwrap();
        assert_eq!(written.row("mario").unwrap().disp_order, 5);
        // Untouched entries keep exactly what the base file had.
        assert_eq!(written.row("link").unwrap().disp_order, 2);
    }

    /// Without a `ui/` dump there is no file to apply positions to. Treating that as "nothing
    /// to do" would produce a silent no-op export.
    #[test]
    fn positions_with_no_base_database_are_reported_rather_than_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (index, _) = setup();
        let mut project = RosterMod::default();
        project.order.insert(RosterKey::fighter("mario"), 5);

        let report = export_roster(dir.path(), None, &index, &project).unwrap();
        assert!(report.files.is_empty());
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("no character select database"));
    }

    #[test]
    fn a_project_with_no_roster_edits_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (index, _) = setup();
        let report = export_roster(dir.path(), None, &index, &RosterMod::default()).unwrap();
        assert!(report.files.is_empty());
        assert!(report.warnings.is_empty());
    }
}

#[cfg(test)]
mod authored_file_tests {
    use super::*;
    use crate::roster::{new_character::authored_entry_multi, scaffold};

    #[test]
    fn an_authored_characters_own_files_are_copied_into_the_exported_mod() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("character");
        let mod_root = dir.path().join("mod");
        scaffold::create_many(&source, "mario", &[8]).unwrap();
        std::fs::write(
            source.join("fighter/mario/model/body/c08/model.numdlb"),
            b"model",
        )
        .unwrap();
        std::fs::write(
            source.join("fighter/mario/motion/body/c08/attack.nuanmb"),
            b"anim",
        )
        .unwrap();
        std::fs::write(source.join(scaffold::effect_file("mario", 8)), b"eff").unwrap();

        let mut report = RosterExport::default();
        export_authored_files(
            &mod_root,
            std::slice::from_ref(&source),
            &[authored_entry_multi("mario", &[8], "Vision", "vision")],
            &mut report,
        )
        .unwrap();

        assert!(mod_root
            .join("fighter/mario/model/body/c08/model.numdlb")
            .is_file());
        assert!(mod_root
            .join("fighter/mario/motion/body/c08/attack.nuanmb")
            .is_file());
        assert!(mod_root.join(scaffold::effect_file("mario", 8)).is_file());
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        // Visionary's own note is a hint for the author, not mod content.
        assert!(!mod_root
            .join("fighter/mario/model/body/c08/PUT_YOUR_MODEL_HERE.txt")
            .exists());
    }

    /// A multi-skin character used to ship only its primary slot: the other
    /// costumes' movesets were gated to slots with no files, which is a
    /// crash-shaped hole on hardware that does not fall back gracefully.
    #[test]
    fn a_multi_skin_characters_extra_skins_ship_too() {
        use crate::roster::new_character::authored_entry_multi;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("character");
        for slot in [8u8, 15] {
            std::fs::create_dir_all(source.join(format!("fighter/mario/model/body/c{slot:02}")))
                .unwrap();
            std::fs::write(
                source.join(format!("fighter/mario/model/body/c{slot:02}/model.numdlb")),
                b"model",
            )
            .unwrap();
        }
        let mut report = RosterExport::default();
        export_authored_files(
            &dir.path().join("mod"),
            &[source],
            &[authored_entry_multi(
                "mario",
                &[8, 9, 10, 11, 12, 13, 14, 15],
                "Vision",
                "vision",
            )],
            &mut report,
        )
        .unwrap();
        let mod_root = dir.path().join("mod");
        assert!(mod_root
            .join("fighter/mario/model/body/c08/model.numdlb")
            .is_file());
        assert!(
            mod_root
                .join("fighter/mario/model/body/c15/model.numdlb")
                .is_file(),
            "the last skin of the block did not ship"
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    /// A mod with the moveset and no model is a character that appears as the donor running
    /// the wrong moveset, which looks like the costume gate being broken.
    #[test]
    fn an_authored_character_with_no_files_yet_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("character");
        scaffold::create_many(&source, "mario", &[8]).unwrap();
        let mut report = RosterExport::default();
        export_authored_files(
            &dir.path().join("mod"),
            &[source],
            &[authored_entry_multi("mario", &[8], "Vision", "vision")],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("Vision"));
        assert!(report.warnings[0].contains("not the character itself"));
    }
}

#[cfg(test)]
mod param_tests {
    use super::*;
    use crate::mod_project::{ParamMod, ParamValue};
    use crate::roster::traits;
    use std::collections::HashMap;

    fn labels() -> HashMap<u64, String> {
        ["weight", "jump_squat_frame", "fighter_kind"]
            .iter()
            .map(|name| (hash40::hash40(name).0, (*name).to_string()))
            .collect()
    }

    fn base(dir: &Path) -> PathBuf {
        let path = dir.join(traits::FIGHTER_PARAM_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        prc::save(
            &path,
            &traits::test_file(&[("mario", 98.0, 3), ("link", 104.0, 7), ("fox", 77.0, 3)]),
        )
        .unwrap();
        dir.to_path_buf()
    }

    fn edits(pairs: &[(&str, &str, ParamValue)]) -> BTreeMap<String, ParamMod> {
        let mut out: BTreeMap<String, ParamMod> = BTreeMap::new();
        for (fighter, key, value) in pairs {
            out.entry((*fighter).to_string())
                .or_default()
                .files
                .entry(traits::FIGHTER_PARAM_PATH.to_string())
                .or_default()
                .insert((*key).to_string(), *value);
        }
        out
    }

    /// The one that decides whether this works at all. Every fighter lives in one file, so two
    /// fighters' edits have to land in the same copy — writing per fighter from the base would
    /// have the last one silently erase the others.
    #[test]
    fn two_fighters_edits_land_in_one_file_without_erasing_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let ui_root = base(&dir.path().join("game"));
        let mod_root = dir.path().join("mod");
        let mut report = RosterExport::default();

        export_params(
            &mod_root,
            Some(&ui_root),
            &edits(&[
                ("mario", "weight", ParamValue::Float(120.0)),
                ("link", "jump_squat_frame", ParamValue::I32(2)),
            ]),
            &labels(),
            &mut report,
        )
        .unwrap();

        assert_eq!(report.files, vec![traits::FIGHTER_PARAM_PATH]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        let written = mod_root.join(traits::FIGHTER_PARAM_PATH);
        let mario = traits::FighterTraits::open(&written, "mario", &labels()).unwrap();
        let link = traits::FighterTraits::open(&written, "link", &labels()).unwrap();
        let fox = traits::FighterTraits::open(&written, "fox", &labels()).unwrap();
        assert_eq!(mario.get("weight"), Some(&ParamValue::Float(120.0)));
        assert_eq!(link.get("jump_squat_frame"), Some(&ParamValue::I32(2)));
        // And an untouched fighter keeps exactly what the base had.
        assert_eq!(fox.get("weight"), Some(&ParamValue::Float(77.0)));
        assert_eq!(link.get("weight"), Some(&ParamValue::Float(104.0)));
    }

    #[test]
    fn an_edit_to_a_field_that_no_longer_exists_is_reported_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let ui_root = base(&dir.path().join("game"));
        let mut report = RosterExport::default();
        export_params(
            &dir.path().join("mod"),
            Some(&ui_root),
            &edits(&[("mario", "no_such_field", ParamValue::Float(1.0))]),
            &labels(),
            &mut report,
        )
        .unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("no_such_field"));
        assert!(report.warnings[0].contains("still saved in the project"));
    }

    /// Without the file there is nothing to apply edits to. Treating that as "nothing to do"
    /// would produce a silent no-op.
    #[test]
    fn value_edits_with_no_base_file_are_reported_rather_than_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = RosterExport::default();
        export_params(
            dir.path(),
            None,
            &edits(&[("mario", "weight", ParamValue::Float(120.0))]),
            &labels(),
            &mut report,
        )
        .unwrap();
        assert!(report.files.is_empty());
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("was not found"));
    }

    #[test]
    fn no_value_edits_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let ui_root = base(&dir.path().join("game"));
        let mut report = RosterExport::default();
        export_params(
            &dir.path().join("mod"),
            Some(&ui_root),
            &BTreeMap::new(),
            &labels(),
            &mut report,
        )
        .unwrap();
        assert!(report.files.is_empty());
        assert!(report.warnings.is_empty());
    }
}
