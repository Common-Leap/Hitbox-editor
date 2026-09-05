//! Pure helpers shared by the console-package and developer export paths.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::acmd::ModProject;
use crate::mod_project::{EffMod, LiveTweak, ModProjectFile, PROJECT_VERSION};

/// A filesystem-, Cargo-, and Rust-friendly project name.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if out.is_empty() {
        out.push_str("visionary_mod");
    }
    if out.as_bytes()[0].is_ascii_digit() {
        out.insert_str(0, "mod_");
    }
    out
}

pub fn plugin_name(project: &ModProjectFile) -> String {
    format!("{}_plugin", slugify(&project.name))
}

/// True when the base EFF must be written. Costume-scoped transplants are the only edit class
/// that belongs exclusively in cXX files; every texture operation is a base-file edit too.
pub fn base_eff_has_content(eff: &EffMod) -> bool {
    !eff.authored.is_empty()
        || !eff.textures.is_empty()
        || !eff.textures_added.is_empty()
        || !eff.textures_removed.is_empty()
        || !eff.rosters.is_empty()
        || !eff.entry_edits.is_empty()
        || eff
            .transplants
            .iter()
            .any(|operation| operation.one_slot_slots.is_empty())
}

/// A generated source project, and what verification had to say about it short of refusing it.
///
/// The warnings are returned rather than logged because this function is the only thing that
/// ever sees them and the user is the only one they are for. Until C6c they were built and
/// dropped on the floor here: verification produced a full loss report on every export and
/// `source_project` read one bit of it — whether anything was fatal. An export that quietly
/// deletes a line the editor could not model was reported to nobody.
pub struct GeneratedSource {
    pub project: ModProject,
    /// Ready-to-show lines, already capped. Empty when the export carried everything.
    pub warnings: Vec<String>,
}

/// Convert every ACMD/effect-call record in a saved project into one buildable Skyline source
/// project. Effect-call deltas are not sufficient by themselves because the generated script
/// replaces the entire original effect script.
pub fn source_project(project: &ModProjectFile) -> Result<Option<GeneratedSource>> {
    let mut acmd_edits = Vec::new();
    let mut effect_edits = Vec::new();
    let mut sound_edits = Vec::new();
    let mut expression_edits = Vec::new();
    let mut tweaks: Vec<LiveTweak> = Vec::new();
    let mut dropped: HashMap<String, Vec<String>> = HashMap::new();
    let mut incomplete = Vec::new();
    let mut exported_effect_names = HashSet::new();
    let mut capture_warnings = Vec::new();
    let mut slot_gates = crate::acmd::SlotGates::new();

    let mut fighters: Vec<_> = project.fighters.iter().collect();
    fighters.sort_by(|a, b| a.0.cmp(b.0));
    for (fighter, edits) in fighters {
        let mut moves: Vec<_> = edits.acmd.iter().collect();
        moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, record) in moves {
            // A move scoped to costumes needs the fighter's own script as the other arm.
            // Without it the generated script would remove this move from every other costume
            // of the fighter, so a missing original is refused rather than exported wide.
            let scopes = record.effective_scopes();
            if !scopes.is_empty() {
                match edits
                    .move_sources
                    .get(move_name)
                    .map(|snapshot| snapshot.body.trim())
                    .filter(|body| !body.is_empty())
                {
                    Some(body) => {
                        slot_gates.insert(
                            (fighter.clone(), move_name.clone()),
                            (scopes.clone(), body.to_string()),
                        );
                    }
                    None => {
                        let slot_list = scopes
                            .iter()
                            .map(|slot| format!("c{slot:02}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        bail!(
                            "{fighter}'s {move_name} is set to costume(s) {slot_list} only, but the \
                             fighter's own version of it was not saved with the project. Exporting \
                             it now would remove {move_name} from every other costume of \
                             {fighter}. Open the move in the editor once so its original is \
                             recorded, then export again."
                        )
                    }
                }
            }
            acmd_edits.push((fighter.clone(), move_name.clone(), record.script.clone()));
        }

        let mut effect_moves: Vec<_> = edits.effect_calls_full.iter().collect();
        effect_moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, calls) in effect_moves {
            let calls = calls
                .iter()
                .cloned()
                .map(crate::data::EffectCall::normalized_timing)
                .collect::<Vec<_>>();
            exported_effect_names.extend(
                calls
                    .iter()
                    .map(|call| call.effect_name.to_ascii_lowercase()),
            );
            effect_edits.push((
                fighter.clone(),
                move_name.clone(),
                calls,
                edits
                    .effect_frame_residue
                    .get(move_name)
                    .cloned()
                    .unwrap_or_default(),
            ));
        }
        for (move_name, lost) in &edits.effect_dropped_lines {
            // Re-keyed to match what the report looks moves up by, and otherwise passed through
            // whole. A note for a move this build ships no function for needs no filtering here:
            // `verify_export` walks the exported moves and asks the map about each, so an orphan
            // note is never consulted. C6c wrote that filter first and removed it once a mutation
            // showed it could not change an outcome.
            dropped.insert(format!("{fighter}/{move_name}"), lost.clone());
        }
        // Sound scripts. An empty script is intentional when the user removed every call; it
        // must be emitted so the generated replacement silences the stock category. A missing
        // map entry, not an empty script, means the category was never edited.
        let mut sound_moves: Vec<_> = edits.sound_scripts.iter().collect();
        sound_moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, script) in sound_moves {
            sound_edits.push((fighter.clone(), move_name.clone(), script.clone()));
        }
        let mut expression_moves: Vec<_> = edits.expression_scripts.iter().collect();
        expression_moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, script) in expression_moves {
            expression_edits.push((fighter.clone(), move_name.clone(), script.clone()));
        }

        // A provenance note is useful only beside a function this export actually ships. A
        // saved project may retain a note for a move whose edit was later removed, and reporting
        // that orphan would make the warning impossible to act on.
        for (move_name, warning) in &edits.capture_branch_warnings {
            let exported = edits.acmd.contains_key(move_name)
                || edits.effect_calls_full.contains_key(move_name)
                || edits.sound_scripts.contains_key(move_name);
            let exported = exported || edits.expression_scripts.contains_key(move_name);
            if exported {
                capture_warnings.push(warning.clone());
            }
        }

        for (move_name, deltas) in &edits.effect_calls {
            if !deltas.is_empty() && !edits.effect_calls_full.contains_key(move_name) {
                incomplete.push(format!("{fighter}/{move_name}"));
            }
        }
        for tweak in &edits.live_tweaks {
            if !tweaks
                .iter()
                .any(|existing| existing.effect_name == tweak.effect_name)
            {
                tweaks.push(tweak.clone());
            }
        }
    }
    if !incomplete.is_empty() {
        incomplete.sort();
        bail!(
            "effect-call edits are missing their complete source list: {}. Open each move and save the project again",
            incomplete.join(", ")
        );
    }
    let mut uncovered_tweaks: Vec<_> = tweaks
        .iter()
        .filter(|tweak| !exported_effect_names.contains(&tweak.effect_name.to_ascii_lowercase()))
        .map(|tweak| tweak.effect_name.clone())
        .collect();
    if !uncovered_tweaks.is_empty() {
        uncovered_tweaks.sort();
        uncovered_tweaks.dedup();
        bail!(
            "live color/speed edits have no captured effect script to attach to: {}. Open or perform a move that uses each effect, then save again",
            uncovered_tweaks.join(", ")
        );
    }
    if acmd_edits.is_empty()
        && effect_edits.is_empty()
        && sound_edits.is_empty()
        && expression_edits.is_empty()
    {
        return Ok(None);
    }
    let built = crate::acmd::build_mod_project_with_slot_gates(
        &acmd_edits,
        &effect_edits,
        &sound_edits,
        &expression_edits,
        &tweaks,
        &plugin_name(project),
        &slot_gates,
    );

    // Nothing reaches disk until the generated code has been read back and matched against the
    // edits it came from. A mod that will not compile, or that ships numbers other than the
    // ones on screen, is worse than an export that stops and says why.
    let report = crate::acmd_verify::verify_export_with_expression(
        &built,
        &acmd_edits,
        &effect_edits,
        &sound_edits,
        &expression_edits,
        &tweaks,
        &dropped,
    );
    if report.has_blockers() {
        bail!(
            "the generated mod source did not pass verification:\n{}",
            report.blocker_summary()
        );
    }
    let mut warnings = report.warning_summary();
    warnings.extend(capture_warnings);
    warnings.sort();
    warnings.dedup();
    Ok(Some(GeneratedSource {
        project: built,
        warnings,
    }))
}

/// Materialize a generated Cargo project at exactly `root` (without adding another surprise
/// project-name directory).
pub fn write_source_project(project: &ModProject, root: &Path) -> Result<()> {
    for file in &project.files {
        let destination = root.join(&file.rel_path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&destination, &file.contents)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    Ok(())
}

fn portable_component(value: &str) -> String {
    let value = slugify(value);
    if value.len() > 80 {
        value[..80].to_string()
    } else {
        value
    }
}

fn copy_asset(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_file() {
        bail!("texture image does not exist: {}", source.display());
    }
    if let (Ok(source), Ok(destination)) = (source.canonicalize(), destination.canonicalize()) {
        if source == destination {
            return Ok(());
        }
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination).with_context(|| {
        format!(
            "copying texture image {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn valid_relative(path: &Path) -> bool {
    path.components().all(|part| {
        !matches!(
            part,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// Copy every external texture beside the project and rewrite its path to be relative. The
/// returned project is the one that should be serialized.
pub fn make_portable_project(
    project: &ModProjectFile,
    project_dir: &Path,
    asset_dir_name: &str,
) -> Result<ModProjectFile> {
    let mut portable = project.clone();
    let mut used_destinations = HashSet::new();
    let asset_relative = PathBuf::from(asset_dir_name).join("textures");
    if !valid_relative(&asset_relative) {
        bail!("asset directory must be relative");
    }

    let mut fighters: Vec<_> = portable.fighters.iter_mut().collect();
    fighters.sort_by(|a, b| a.0.cmp(b.0));
    for (fighter, edits) in fighters {
        let Some(eff) = &mut edits.eff else { continue };
        for (index, texture) in eff.textures.iter_mut().enumerate() {
            if texture.png_path.is_empty() {
                continue;
            }
            let source = PathBuf::from(&texture.png_path);
            let extension = source
                .extension()
                .and_then(|part| part.to_str())
                .unwrap_or("png");
            let relative = asset_relative
                .join(portable_component(fighter))
                .join(format!(
                    "replace_{index}_{}.{}",
                    portable_component(&texture.texture_name),
                    extension
                ));
            if !used_destinations.insert(relative.clone()) {
                bail!(
                    "duplicate project asset destination: {}",
                    relative.display()
                );
            }
            copy_asset(&source, &project_dir.join(&relative))?;
            texture.png_path = relative.to_string_lossy().replace('\\', "/");
        }
        for (index, texture) in eff.textures_added.iter_mut().enumerate() {
            if texture.png_path.is_empty() {
                continue;
            }
            let source = PathBuf::from(&texture.png_path);
            let extension = source
                .extension()
                .and_then(|part| part.to_str())
                .unwrap_or("png");
            let relative = asset_relative
                .join(portable_component(fighter))
                .join(format!(
                    "add_{index}_{}.{}",
                    portable_component(&texture.texture_name),
                    extension
                ));
            if !used_destinations.insert(relative.clone()) {
                bail!(
                    "duplicate project asset destination: {}",
                    relative.display()
                );
            }
            copy_asset(&source, &project_dir.join(&relative))?;
            texture.png_path = relative.to_string_lossy().replace('\\', "/");
        }
    }
    // Roster UI image overrides (portraits / stock icons) — same portable asset handling.
    let asset_ui = PathBuf::from(asset_dir_name).join("roster_ui");
    for (roster_key, kinds) in portable.roster.ui_images.iter_mut() {
        for (kind, ov) in kinds.iter_mut() {
            if ov.png_path.is_empty() {
                continue;
            }
            let source = PathBuf::from(&ov.png_path);
            let extension = source
                .extension()
                .and_then(|part| part.to_str())
                .unwrap_or("png");
            let stem = source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image");
            let relative = asset_ui
                .join(portable_component(&roster_key.to_string()))
                .join(format!(
                    "{}_{}.{}",
                    portable_component(kind),
                    portable_component(stem),
                    extension
                ));
            if !used_destinations.insert(relative.clone()) {
                bail!(
                    "duplicate project asset destination: {}",
                    relative.display()
                );
            }
            copy_asset(&source, &project_dir.join(&relative))?;
            ov.png_path = relative.to_string_lossy().replace('\\', "/");
        }
    }
    Ok(portable)
}

pub fn write_portable_project(
    project: &ModProjectFile,
    path: &Path,
    asset_dir_name: &str,
) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let portable = make_portable_project(project, parent, asset_dir_name)?;
    let json = serde_json::to_string_pretty(&portable)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

/// Resolve portable texture paths against the loaded project's directory for all in-memory
/// rebuild and live-send code, which expects readable filesystem paths.
pub fn resolve_project_assets(project: &mut ModProjectFile, project_path: &Path) -> Result<()> {
    let parent = project_path.parent().unwrap_or_else(|| Path::new("."));
    for edits in project.fighters.values_mut() {
        let Some(eff) = &mut edits.eff else { continue };
        for path in eff
            .textures
            .iter_mut()
            .map(|texture| &mut texture.png_path)
            .chain(
                eff.textures_added
                    .iter_mut()
                    .map(|texture| &mut texture.png_path),
            )
        {
            if path.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(&*path);
            if candidate.is_relative() {
                if !valid_relative(&candidate) {
                    bail!("project texture path escapes its project folder: {path}");
                }
                *path = parent.join(candidate).to_string_lossy().to_string();
            }
        }
    }
    for kinds in project.roster.ui_images.values_mut() {
        for ov in kinds.values_mut() {
            if ov.png_path.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(&ov.png_path);
            if candidate.is_relative() {
                if !valid_relative(&candidate) {
                    bail!(
                        "project roster UI image path escapes its project folder: {}",
                        ov.png_path
                    );
                }
                ov.png_path = parent.join(candidate).to_string_lossy().to_string();
            }
        }
    }
    Ok(())
}

pub fn validate_project(project: &ModProjectFile) -> Result<()> {
    if project.version > PROJECT_VERSION {
        bail!(
            "project version {} is newer than this Visionary build supports (version {})",
            project.version,
            PROJECT_VERSION
        );
    }
    if project.name.trim().is_empty() {
        bail!("project name is empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{EffectCallEdit, EffectCallOp};
    use crate::mod_project::{EffMod, FighterMod, TextureAddition, TextureImport};

    #[test]
    fn slug_is_safe_and_never_empty() {
        assert_eq!(slugify(" Kirby, but GOOD! "), "kirby_but_good");
        assert_eq!(slugify("123"), "mod_123");
        assert_eq!(slugify("--"), "visionary_mod");
    }

    #[test]
    fn every_base_eff_edit_class_requests_a_base_file() {
        let mut eff = EffMod {
            source_rel: "effect/fighter/kirby/ef_kirby.eff".into(),
            ..Default::default()
        };
        eff.textures_removed.push("old_texture".into());
        assert!(base_eff_has_content(&eff));
        eff.textures_removed.clear();
        eff.textures.push(TextureImport {
            texture_name: "replaced".into(),
            png_path: "image.png".into(),
            raw: false,
        });
        assert!(base_eff_has_content(&eff));
        eff.textures.clear();
        eff.textures_added.push(TextureAddition {
            texture_name: "new".into(),
            template_name: "old".into(),
            png_path: String::new(),
            raw: false,
        });
        assert!(base_eff_has_content(&eff));
    }

    #[test]
    fn portable_project_copies_and_resolves_texture_assets() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("outside.png");
        std::fs::write(&input, b"png fixture").unwrap();
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "portable".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "kirby".into(),
            FighterMod {
                eff: Some(EffMod {
                    source_rel: "effect/fighter/kirby/ef_kirby.eff".into(),
                    textures: vec![TextureImport {
                        texture_name: "ef_test".into(),
                        png_path: input.to_string_lossy().to_string(),
                        raw: false,
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let project_path = temp.path().join("bundle/modproject.json");
        write_portable_project(&project, &project_path, "assets").unwrap();
        let mut loaded: ModProjectFile =
            serde_json::from_str(&std::fs::read_to_string(&project_path).unwrap()).unwrap();
        let stored = &loaded.fighters["kirby"].eff.as_ref().unwrap().textures[0].png_path;
        assert!(Path::new(stored).is_relative(), "{stored}");
        assert!(project_path.parent().unwrap().join(stored).is_file());
        resolve_project_assets(&mut loaded, &project_path).unwrap();
        let resolved = &loaded.fighters["kirby"].eff.as_ref().unwrap().textures[0].png_path;
        assert!(Path::new(resolved).is_absolute(), "{resolved}");
        assert!(Path::new(resolved).is_file());
    }

    #[test]
    fn incomplete_effect_edits_fail_instead_of_silently_disappearing() {
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "incomplete".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "kirby".into(),
            FighterMod {
                effect_calls: std::collections::HashMap::from([(
                    "attack_dash".into(),
                    vec![EffectCallEdit {
                        index: 0,
                        op: EffectCallOp::Remove,
                        pristine: None,
                    }],
                )]),
                ..Default::default()
            },
        );
        let error = match source_project(&project) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("incomplete effect edits unexpectedly exported"),
        };
        assert!(error.contains("kirby/attack_dash"), "{error}");
    }

    /// C6c, end to end: the losses a move suffers on the way through the export survive being
    /// saved, reloaded, and exported again.
    ///
    /// The fixture is lifted from the corpus rather than written here, because a hand-made
    /// "lossy" script only proves that this test can construct one. `Run` is a real
    /// script that really loses a line, and the assertion below is that a user who saves that
    /// project, closes Visionary, reopens it and exports without ever opening the move again is
    /// still told so — which before C6c they were not, twice over: the project had nowhere to
    /// keep the list, and the export threw its whole report away bar the blockers.
    ///
    /// **Repointed from `SpecialHi2` by C2, which is the guard below doing its job.** Modelling
    /// the raw-command sword trail made `SpecialHi2` lossless, so it stopped exercising anything
    /// and said so instead of passing quietly. `Run` loses a `wait_loop_sync_mot`, which
    /// the `EffectStmt::Raw` arm drops on purpose and has no plan to ever carry — a deliberately
    /// unmodelled line is a steadier fixture than one that is merely unmodelled yet.
    #[test]
    fn a_reloaded_project_still_reports_the_lines_its_export_deletes() {
        let script_path = crate::scratch_dirs::app_storage_root()
            .join("script-cache")
            .join("kirby")
            .join("Run.txt");
        let Ok(body) = std::fs::read_to_string(&script_path) else {
            return;
        };
        let parsed = crate::acmd::parse_effect_script(&body);
        let calls = parsed.to_effect_calls();
        let lost = crate::acmd::unexportable_effect_lines(&parsed);
        // Guard the oracle with what it claims to test. If this move ever stops losing a line —
        // because the family got modelled, which is the good outcome — this test is measuring
        // nothing and should be repointed at a move that still does, not deleted.
        assert!(
            !calls.is_empty() && !lost.is_empty(),
            "Run no longer exercises a loss; repoint this test at a move that does"
        );

        let build = |dropped: HashMap<String, Vec<String>>| {
            let mut project = ModProjectFile {
                version: PROJECT_VERSION,
                name: "loss_report".into(),
                ..Default::default()
            };
            project.fighters.insert(
                "kirby".into(),
                FighterMod {
                    effect_calls_full: HashMap::from([("run".into(), calls.clone())]),
                    effect_dropped_lines: dropped,
                    ..Default::default()
                },
            );
            // Through JSON, because that is the trip being tested. Asserting on the struct in
            // memory would pass even if the new field never reached the file.
            let json = serde_json::to_string(&project).unwrap();
            let reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
            source_project(&reloaded)
                .unwrap()
                .expect("effect edits should export")
                .warnings
                .join("\n")
        };

        let said = build(HashMap::from([("run".into(), lost.clone())]));
        let first = lost[0].trim();
        assert!(
            said.contains(first),
            "the export never mentioned the line it deleted ({first}):\n{said}"
        );

        // The control, and the only thing that shows the stored list is what carried it: the
        // calls, the generated code and the verification are identical here.
        let silent = build(HashMap::new());
        assert!(
            !silent.contains(first),
            "the loss was reported without the saved list, so this test proves nothing about \
             it:\n{silent}"
        );

        // A note for a move this build ships no function for. Visionary never saves that pair —
        // the note only goes out beside the calls — but these files travel with mods and get
        // hand-edited, and a warning about a move that is not in the export is a warning the
        // user cannot act on. Enforced by the report only ever asking about moves it is
        // exporting, so this holds no matter what the gathering above passes along.
        let orphan = build(HashMap::from([("some_other_move".into(), lost.clone())]));
        assert!(
            !orphan.contains(first),
            "a loss note was reported for a move with no exported function:\n{orphan}"
        );
    }

    /// The saved residue has to survive the file, or E3 only works with the script open.
    ///
    /// `effect_frame_residue` is the one saved field that changes generated code. A dropped-line
    /// note is a remark; these lines are *written*, so a project that reloads without them
    /// exports exactly what the pre-E3 build did — the lines vanish, and the note that used to
    /// warn about them is gone too, because this build no longer produces one.
    ///
    /// Mutation that made this test necessary: `source_project` passing `Default::default()`
    /// instead of reading the field. Everything else in the suite stayed green, including the
    /// corpus ratchet, because every other test builds the residue and the calls from the same
    /// parse and never goes through a file.
    #[test]
    fn saved_frame_residue_reaches_the_generated_source() {
        // `dolly/FinalAirEnd`'s two frames, with its `FILL_SCREEN_MODEL_COLOR` left out. That
        // line is carried verbatim and the dump spells it `EffectScreenLayer:*GROUND`, which is
        // not valid Rust, so the real file is stopped by export verification long before it
        // reaches this — the designed behaviour, and a different test's subject. The
        // `CANCEL_FILL_SCREEN` lines are verbatim from that script.
        let body = r#"unsafe extern "C" fn effect_finalairend(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("dolly_buster_ground"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
    }
    frame(agent.lua_state_agent, 40.0);
    if macros::is_excute(agent) {
        macros::CANCEL_FILL_SCREEN(agent, 1, 30);
        macros::CANCEL_FILL_SCREEN(agent, 0, 30);
    }
}
"#;
        let (calls, residue) = crate::acmd::parse_effect_script(body).to_effect_calls_and_residue();
        assert!(
            !calls.is_empty() && !residue.is_empty(),
            "the premise: a call list plus a frame that owns lines of its own"
        );

        let build = |residue: HashMap<String, std::collections::BTreeMap<u32, Vec<String>>>| {
            let mut project = ModProjectFile {
                version: PROJECT_VERSION,
                name: "residue_export".into(),
                ..Default::default()
            };
            project.fighters.insert(
                "dolly".into(),
                FighterMod {
                    effect_calls_full: HashMap::from([("finalairend".into(), calls.clone())]),
                    effect_frame_residue: residue,
                    ..Default::default()
                },
            );
            // Through JSON, because that is the trip being tested. Asserting on the struct in
            // memory would pass even if the new field never reached the file.
            let json = serde_json::to_string(&project).unwrap();
            let reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
            let generated = source_project(&reloaded)
                .unwrap()
                .expect("effect edits should export");
            generated
                .project
                .files
                .iter()
                .map(|f| f.contents.clone())
                .collect::<Vec<_>>()
                .join("\n")
        };

        let carried = build(HashMap::from([("finalairend".into(), residue.clone())]));
        assert!(
            carried.contains("CANCEL_FILL_SCREEN(agent, 1, 30)")
                && carried.contains("CANCEL_FILL_SCREEN(agent, 0, 30)"),
            "the saved residue never reached the generated script:\n{carried}"
        );

        // The control. Without it, "the lines are present" would pass for a build that got them
        // from somewhere else entirely — the calls, say — and prove nothing about the field.
        let dropped = build(HashMap::new());
        assert!(
            !dropped.contains("CANCEL_FILL_SCREEN"),
            "the lines appeared without the saved residue, so this test proves nothing about \
             it:\n{dropped}"
        );
    }

    /// A loss note is a remark about an export, not an edit to be exported.
    ///
    /// If it counted, a project holding nothing else would stop reporting "no edits yet" and
    /// start producing an export with no files in it.
    #[test]
    fn a_project_holding_only_a_loss_note_is_still_empty() {
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "note_only".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "kirby".into(),
            FighterMod {
                effect_dropped_lines: HashMap::from([(
                    "special_hi2".into(),
                    vec!["methodlib::L2CAgent::pop();".into()],
                )]),
                ..Default::default()
            },
        );
        assert!(project.is_empty());
        assert!(source_project(&project).unwrap().is_none());
    }

    #[test]
    fn a_live_capture_branch_warning_survives_a_project_round_trip() {
        let body = r#"
unsafe extern "C" fn effect_x(agent: &mut L2CAgentBase) {
    if WorkModule::is_flag(agent.module_accessor, *FLAG) {
        if macros::is_excute(agent) {
            macros::EFFECT(agent, Hash40::new("sys_flash"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
        }
    }
}
"#;
        let calls = crate::acmd::parse_effect_script(body).to_effect_calls();
        assert!(
            !calls.is_empty(),
            "the fixture must ship an effect function"
        );
        let note = "Live capture is partial for kirby/effect_x: one observed path came from a cached source with 1 conditional branch(es). Export will write only the observed calls as unconditional code; inspect the source before shipping".to_string();
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "capture_provenance".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "kirby".into(),
            FighterMod {
                effect_calls_full: HashMap::from([(String::from("effect_x"), calls)]),
                capture_branch_warnings: HashMap::from([(String::from("effect_x"), note.clone())]),
                ..Default::default()
            },
        );

        let json = serde_json::to_string(&project).unwrap();
        let reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
        let generated = source_project(&reloaded)
            .unwrap()
            .expect("the captured effect should still export");
        assert!(
            generated.warnings.contains(&note),
            "{:?}",
            generated.warnings
        );

        let orphan = FighterMod {
            capture_branch_warnings: HashMap::from([(String::from("not_exported"), note)]),
            ..Default::default()
        };
        let mut only_note = ModProjectFile {
            version: PROJECT_VERSION,
            name: "note_only".into(),
            ..Default::default()
        };
        only_note.fighters.insert("kirby".into(), orphan);
        assert!(source_project(&only_note).unwrap().is_none());
    }

    /// Expression scripts are a first-class export surface, not just a panel representation:
    /// they must survive JSON persistence, appear in the fighter source, install under the
    /// expression script name, and pass the same read-back verifier as the other ACMD families.
    #[test]
    fn an_expression_script_survives_a_saved_project_export() {
        let source = r#"
unsafe extern "C" fn expression_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::RUMBLE_HIT(agent, Hash40::new("rbkind_attackm"), 0);
        macros::QUAKE(agent, *CAMERA_QUAKE_KIND_M);
        ControlModule::set_rumble(agent.module_accessor, Hash40::new("rbkind_attackm"), 3, false, *BATTLE_OBJECT_ID_INVALID as u32);
    }
}
"#;
        let script = crate::acmd::parse_expression_script(source);
        assert_eq!(script.to_expression_events().len(), 3);

        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "expression_export".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "mario".into(),
            FighterMod {
                expression_scripts: std::collections::HashMap::from([(
                    "attack_air_n".into(),
                    script,
                )]),
                ..Default::default()
            },
        );

        let json = serde_json::to_string(&project).unwrap();
        let reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
        let generated = source_project(&reloaded)
            .unwrap()
            .expect("expression script should export");
        assert!(generated.warnings.is_empty(), "{:?}", generated.warnings);
        let acmd = generated
            .project
            .files
            .iter()
            .find(|file| file.rel_path == "src/mario/acmd.rs")
            .map(|file| file.contents.as_str())
            .expect("generated fighter ACMD");
        assert!(acmd.contains("unsafe extern \"C\" fn expression_attackairn"));
        assert!(acmd.contains("macros::RUMBLE_HIT(agent"));
        assert!(acmd.contains("macros::QUAKE(agent"));
        assert!(acmd.contains(
            "ControlModule::set_rumble(agent.module_accessor, Hash40::new(\"rbkind_attackm\"), 3, false, *BATTLE_OBJECT_ID_INVALID as u32);"
        ));
        assert!(acmd.contains(
            "agent.acmd(\"expression_attackairn\", expression_attackairn, smashline::Priority::Default);"
        ));
    }

    /// End to end: a script the user actually wrote → parsed → carried through a saved
    /// project → generated plugin source. The macros they called have to come out the other
    /// side, because a mod that silently swaps `EFFECT_FOLLOW_FLIP` for `EFFECT` does not
    /// behave like the move they were editing.
    #[test]
    fn a_moves_own_spawn_macros_survive_the_whole_export_path() {
        const SOURCE: &str = r#"
unsafe extern "C" fn effect_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("mario_hit_l"), Hash40::new("mario_hit_r"), Hash40::new("haver"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);
        macros::EFFECT_ALPHA(agent, Hash40::new("sys_smoke"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.6);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::EFFECT_OFF_KIND(agent, Hash40::new("mario_hit_l"), false, true);
    }
}
"#;
        let calls = crate::acmd::parse_effect_script(SOURCE).to_effect_calls();
        assert_eq!(calls.len(), 2);

        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "fidelity".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "mario".into(),
            FighterMod {
                effect_calls_full: std::collections::HashMap::from([(
                    "attack_air_n".into(),
                    calls.clone(),
                )]),
                ..Default::default()
            },
        );
        // Through a real save/load, so the new fields have to actually serialize.
        let json = serde_json::to_string(&project).unwrap();
        let project: ModProjectFile = serde_json::from_str(&json).unwrap();

        let generated = source_project(&project)
            .unwrap()
            .expect("source project")
            .project;
        let acmd = generated
            .files
            .iter()
            .find(|f| f.rel_path == "src/mario/acmd.rs")
            .map(|f| f.contents.as_str())
            .expect("generated fighter ACMD");

        assert!(
            acmd.contains(
                r#"macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("mario_hit_l"), Hash40::new("mario_hit_r"), Hash40::new("haver"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);"#
            ),
            "the flipped follow spawn and its second graphic must survive:\n{acmd}"
        );
        assert!(
            acmd.contains(
                r#"macros::EFFECT_ALPHA(agent, Hash40::new("sys_smoke"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.6);"#
            ),
            "EFFECT_ALPHA's alpha argument must survive:\n{acmd}"
        );
        assert!(
            acmd.contains(
                r#"macros::EFFECT_OFF_KIND(agent, Hash40::new("mario_hit_l"), false, true);"#
            ),
            "the follow effect's end must still close it:\n{acmd}"
        );
    }

    #[test]
    fn uncovered_live_tweak_fails_instead_of_exporting_no_code() {
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "tweak_only".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "kirby".into(),
            FighterMod {
                live_tweaks: vec![LiveTweak {
                    effect_name: "kirby_dash".into(),
                    color: Some([2.0, 1.0, 1.0, 1.0]),
                    speed: None,
                }],
                ..Default::default()
            },
        );
        let error = match source_project(&project) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("uncovered live tweak unexpectedly exported"),
        };
        assert!(error.contains("kirby_dash"), "{error}");
    }

    #[test]
    fn empty_sound_and_expression_scripts_are_explicit_project_replacements() {
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "clear_categories".into(),
            ..Default::default()
        };
        project.fighters.insert(
            "mario".into(),
            FighterMod {
                sound_scripts: HashMap::from([("attack_air_n".into(), Default::default())]),
                expression_scripts: HashMap::from([("attack_air_n".into(), Default::default())]),
                ..Default::default()
            },
        );

        assert!(
            !project.is_empty(),
            "an explicit empty replacement is still an edit"
        );
        let round_tripped: ModProjectFile =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        let generated = source_project(&round_tripped)
            .unwrap()
            .expect("empty category replacements should generate source");
        let acmd = generated
            .project
            .files
            .iter()
            .find(|file| file.rel_path == "src/mario/acmd.rs")
            .map(|file| file.contents.as_str())
            .expect("generated fighter source");
        assert!(
            acmd.contains("unsafe extern \"C\" fn sound_attackairn"),
            "{acmd}"
        );
        assert!(
            acmd.contains("unsafe extern \"C\" fn expression_attackairn"),
            "{acmd}"
        );
        assert!(acmd.contains("agent.acmd(\"sound_attackairn\""), "{acmd}");
        assert!(
            acmd.contains("agent.acmd(\"expression_attackairn\""),
            "{acmd}"
        );
    }

    /// Audit a real editor-produced project against a real ArcExplorer dump. This verifies that
    /// every serialized EFF edit class can be rebuilt and that the source project + portable
    /// project paths are both materialized. Normal test runs remain machine-independent.
    #[test]
    fn saved_project_exports_when_requested() {
        let (Ok(project_path), Ok(data_root)) = (
            std::env::var("VISIONARY_TEST_PROJECT"),
            std::env::var("VISIONARY_TEST_DATA_ROOT"),
        ) else {
            return;
        };
        let mut project: ModProjectFile = serde_json::from_str(
            &std::fs::read_to_string(&project_path).expect("read saved project"),
        )
        .expect("deserialize saved project");
        validate_project(&project).unwrap();
        resolve_project_assets(&mut project, Path::new(&project_path)).unwrap();

        let output = crate::scratch_dirs::app_storage_root()
            .join("saved-project-export")
            .join(slugify(&project.name));
        let layout = crate::scratch_dirs::app_scratch_dir("saved-project-layout")
            .expect("create isolated export layout");
        let package = layout
            .path()
            .join("mod_folder")
            .join(slugify(&project.name));
        let project_export = layout.path().join("project_export");
        write_portable_project(&project, &project_export.join("modproject.json"), "assets")
            .unwrap();
        let source = source_project(&project)
            .unwrap()
            .expect("ACMD source edits")
            .project;
        let source_root = output.join("acmd_source");
        write_source_project(&source, &source_root).unwrap();

        let data_root = PathBuf::from(data_root);
        let mut rebuilt_count = 0;
        for edits in project.fighters.values() {
            let Some(eff) = &edits.eff else { continue };
            let original = std::fs::read(data_root.join(&eff.source_rel)).expect("read source EFF");
            if base_eff_has_content(eff) {
                let rebuilt = crate::eff_export::rebuild_eff_bytes_for_slot(
                    &original,
                    eff,
                    Some(&data_root),
                    None,
                )
                .expect("rebuild complete EFF edit set");
                assert_ne!(rebuilt, original, "the exported EFF was unchanged");
                effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt EFF");
                let destination = package.join(&eff.source_rel);
                std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                std::fs::write(destination, rebuilt).unwrap();
                rebuilt_count += 1;
            }
            let mut slots: Vec<_> = eff
                .transplants
                .iter()
                .flat_map(|operation| operation.one_slot_slots.iter().copied())
                .collect();
            slots.sort();
            slots.dedup();
            for slot in slots {
                let rebuilt = crate::eff_export::rebuild_eff_bytes_for_slot(
                    &original,
                    eff,
                    Some(&data_root),
                    Some(slot),
                )
                .expect("rebuild one-slot EFF");
                effect_library::NamcoEffectFile::load(&rebuilt).expect("parse one-slot EFF");
                let relative = Path::new(&eff.source_rel);
                let stem = relative
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("ef");
                let file_name = format!("{stem}_c{slot:02}.eff");
                let relative = relative
                    .parent()
                    .map(|parent| parent.join(&file_name))
                    .unwrap_or_else(|| PathBuf::from(file_name));
                let destination = package.join(relative);
                std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                std::fs::write(destination, rebuilt).unwrap();
                rebuilt_count += 1;
            }
        }
        assert!(rebuilt_count > 0, "project contained no exported EFF files");
        std::fs::write(
            package.join("info.toml"),
            "display_name = \"Export audit\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert!(package.join("info.toml").is_file());
        assert!(package.join("effect/fighter/kirby/ef_kirby.eff").is_file());

        let build = std::process::Command::new("cargo")
            .args(["skyline", "build", "--release"])
            .current_dir(&source_root)
            .output()
            .expect("cargo-skyline not runnable");
        assert!(
            build.status.success(),
            "generated source failed to build:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr),
        );
        let plugin = plugin_name(&project);
        let built_nro = source_root
            .join("target")
            .join("aarch64-skyline-switch")
            .join("release")
            .join(format!("lib{plugin}.nro"));
        let installed_nro = package.join("plugin.nro");
        std::fs::create_dir_all(installed_nro.parent().unwrap()).unwrap();
        std::fs::copy(&built_nro, &installed_nro).expect("install generated NRO in package");
        assert!(installed_nro.is_file());
        assert!(project_export.join("modproject.json").is_file());
        assert!(project_export.join("assets/textures/kirby").is_dir());
        assert!(!package.join("modproject.json").exists());
        assert!(!package.join("assets").exists());
        assert!(!package.join("atmosphere").exists());
        assert!(!package.join("ultimate").exists());
        assert!(!package.join("romfs").exists());
    }
}

/// Costume-scoped moves: the export half of the character an authored slot gets.
///
/// A Smashline `game_` script replaces the move for **every** costume of the fighter, so a
/// scoped move that shipped without the fighter's own script as its other arm would delete
/// that move from the donor and every other costume of it. These pin both directions.
#[cfg(test)]
mod slot_gate_tests {
    use super::*;
    use crate::data::EditRecord;
    use crate::mod_project::{FighterMod, MoveSourceSnapshot};

    const ORIGINAL: &str = "unsafe extern \"C\" fn game_attack11(agent: &mut L2CAgentBase) {\n    frame(agent.lua_state_agent, 2.0);\n}\n";

    fn project(slot: Option<u8>, original: Option<&str>) -> ModProjectFile {
        let record = EditRecord {
            fighter: "mario".into(),
            fighter_display: "Mario".into(),
            move_name: "attack_11".into(),
            script: crate::data::AcmdScript::default(),
            hitboxes_pristine: Vec::new(),
            hitboxes: Vec::new(),
            slot_scope: slot,
            slot_scopes: Vec::new(),
        };
        let mut fighter = FighterMod {
            acmd: HashMap::from([("attack_11".to_string(), record)]),
            ..Default::default()
        };
        if let Some(original) = original {
            fighter.move_sources.insert(
                "attack_11".to_string(),
                MoveSourceSnapshot {
                    body: original.to_string(),
                    captures: Vec::new(),
                },
            );
        }
        let mut project = ModProjectFile {
            name: "gate_test".into(),
            ..Default::default()
        };
        project.fighters.insert("mario".into(), fighter);
        project
    }

    fn generated(project: &ModProjectFile) -> String {
        let source = source_project(project)
            .expect("export should succeed")
            .expect("a project with one edit generates source");
        source
            .project
            .files
            .iter()
            .find(|file| file.rel_path.ends_with("acmd.rs"))
            .expect("a per-fighter acmd.rs")
            .contents
            .clone()
    }

    #[test]
    fn a_costume_scoped_move_is_exported_behind_a_costume_gate() {
        let source = generated(&project(Some(8), Some(ORIGINAL)));
        assert!(
            source.contains("FIGHTER_INSTANCE_WORK_ID_INT_COLOR"),
            "{source}"
        );
        assert!(source.contains("if color == 8 {"), "{source}");
        assert!(source.contains("_costume(agent);"), "{source}");
        assert!(source.contains("_original(agent);"), "{source}");
    }

    /// The paired positive for the refusal below: with everything present the same project
    /// exports, so the refusal is about the missing original and not about the fixture.
    #[test]
    fn an_unscoped_move_is_exported_plainly() {
        let source = generated(&project(None, None));
        assert!(
            !source.contains("FIGHTER_INSTANCE_WORK_ID_INT_COLOR"),
            "{source}"
        );
        assert!(!source.contains("_costume(agent);"), "{source}");
    }

    /// Exporting this would silently remove the move from every other costume of the donor.
    /// Refusing, with a message that says what to do, is the only safe answer.
    #[test]
    fn a_costume_scoped_move_with_no_original_is_refused_rather_than_exported_wide() {
        let message = match source_project(&project(Some(8), None)) {
            Ok(_) => panic!("a scoped move with no original must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("every other costume"), "{message}");
        assert!(message.contains("attack_11"), "{message}");
    }

    /// An empty stored body is the same problem as a missing one, and is what a project saved
    /// from a move that never loaded its source actually contains.
    #[test]
    fn an_empty_stored_original_counts_as_missing() {
        assert!(source_project(&project(Some(8), Some("   \n"))).is_err());
    }
}
