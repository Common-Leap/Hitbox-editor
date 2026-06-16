//! Scratch directories for CLI tools (EffectConverter, bnsh-decoder, spirv-cross).
//!
//! Defaults to `~/.cache/hitbox-editor/` instead of `/tmp` to avoid tmpfs quota exhaustion.

use std::path::{Path, PathBuf};

/// Root for all editor scratch/cache data. Override with `HITBOX_EFFECT_TMP`.
pub fn app_storage_root() -> PathBuf {
    std::env::var("HITBOX_EFFECT_TMP")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::cache_dir().map(|d| d.join("hitbox-editor")))
        .unwrap_or_else(|| PathBuf::from(".hitbox-editor-cache"))
}

/// Unique temp dir under `{app_storage_root}/scratch/{prefix}-*`.
pub fn app_scratch_dir(prefix: &str) -> anyhow::Result<tempfile::TempDir> {
    let base = app_storage_root().join("scratch");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to create scratch dir in {}: {e} (set HITBOX_EFFECT_TMP to a disk with free space)",
                base.display()
            )
        })
}

pub fn is_disk_quota_error(err: &dyn std::error::Error) -> bool {
    let s = err.to_string();
    s.contains("Disk quota")
        || s.contains("No space left")
        || s.contains("os error 122")
        || s.contains("os error 28")
}

/// EffectConverter PTCL dump cache (`~/.cache/hitbox-editor/ptcl-dumps/`).
pub fn effect_dump_cache_root() -> PathBuf {
    app_storage_root().join("ptcl-dumps")
}

/// Read a path saved by the desktop app under `~/.config/ssbu_hitbox_editor/`.
pub fn load_persisted_config_path(key: &str) -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    let dest = base.join("ssbu_hitbox_editor").join(key);
    let s = std::fs::read_to_string(&dest).ok()?;
    let p = PathBuf::from(s.trim());
    if p.exists() { Some(p) } else { None }
}

/// Arc/explorer export root (`…/export` with `fighter/`, `effect/`, …).
///
/// Resolution order: `HITBOX_DATA_ROOT`, then editor `data_root` config.
pub fn game_data_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("HITBOX_DATA_ROOT") {
        let p = PathBuf::from(raw.trim());
        if p.is_dir() {
            return Some(p);
        }
    }
    load_persisted_config_path("data_root")
}

/// Game effect export root (`…/export/effect`).
///
/// Resolution order: `HITBOX_EFFECT_EXPORT`, then `{data_root}/effect` from the editor config.
pub fn effect_export_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("HITBOX_EFFECT_EXPORT") {
        let p = PathBuf::from(raw.trim());
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(raw) = std::env::var("HITBOX_DATA_ROOT") {
        let effect = PathBuf::from(raw.trim()).join("effect");
        if effect.is_dir() {
            return Some(effect);
        }
    }
    let data_root = load_persisted_config_path("data_root")?;
    let effect = data_root.join("effect");
    if effect.is_dir() {
        Some(effect)
    } else {
        None
    }
}

/// Locate `ef_{fighter}.eff` under the configured effect export directory.
pub fn resolve_fighter_eff(fighter: &str) -> Option<PathBuf> {
    let root = effect_export_root()?;
    [root.join("fighter").join(fighter).join(format!("ef_{fighter}.eff")), root.join(format!("ef_{fighter}.eff"))]
        .into_iter()
        .find(|p| p.exists())
}

/// Walk `root` recursively and invoke `f` for every regular file matching `name`.
pub fn walk_files_named(root: &Path, name: &str, f: &mut impl FnMut(&Path)) {
    if !root.is_dir() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_files_named(&path, name, f);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            f(&path);
        }
    }
}
