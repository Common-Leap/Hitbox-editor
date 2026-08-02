//! Pure helpers shared by the console-package and developer export paths.

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::acmd::{build_mod_project_full, ModProject};
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

/// Convert every ACMD/effect-call record in a saved project into one buildable Skyline source
/// project. Effect-call deltas are not sufficient by themselves because the generated script
/// replaces the entire original effect script.
pub fn source_project(project: &ModProjectFile) -> Result<Option<ModProject>> {
    let mut acmd_edits = Vec::new();
    let mut effect_edits = Vec::new();
    let mut tweaks: Vec<LiveTweak> = Vec::new();
    let mut incomplete = Vec::new();
    let mut exported_effect_names = HashSet::new();

    let mut fighters: Vec<_> = project.fighters.iter().collect();
    fighters.sort_by(|a, b| a.0.cmp(b.0));
    for (fighter, edits) in fighters {
        let mut moves: Vec<_> = edits.acmd.iter().collect();
        moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, record) in moves {
            acmd_edits.push((fighter.clone(), move_name.clone(), record.script.clone()));
        }

        let mut effect_moves: Vec<_> = edits.effect_calls_full.iter().collect();
        effect_moves.sort_by(|a, b| a.0.cmp(b.0));
        for (move_name, calls) in effect_moves {
            exported_effect_names.extend(
                calls
                    .iter()
                    .map(|call| call.effect_name.to_ascii_lowercase()),
            );
            effect_edits.push((fighter.clone(), move_name.clone(), calls.clone()));
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
    if acmd_edits.is_empty() && effect_edits.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_mod_project_full(
        &acmd_edits,
        &effect_edits,
        &tweaks,
        &plugin_name(project),
    )))
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
            .expect("ACMD source edits");
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
