//! Scratch directories for CLI tools (EffectConverter, bnsh-decoder, spirv-cross).
//!
//! WGSL shader dumps and diagnostic PNGs go under `{manifest}/tmp/` (override with
//! `HITBOX_WORKSHOP_TMP`) instead of `/tmp` to avoid tmpfs quota exhaustion.
//! Other cache data defaults to `{target}/hitbox-editor-cache/`. Override with `HITBOX_EFFECT_TMP`.

use std::path::{Path, PathBuf};

/// Root for all editor scratch/cache data. Override with `HITBOX_EFFECT_TMP`.
pub fn app_storage_root() -> PathBuf {
    std::env::var("HITBOX_EFFECT_TMP")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(default_app_storage_root)
}

fn default_app_storage_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir).join("hitbox-editor-cache");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("hitbox-editor-cache")
}

/// Root for WGSL dumps, diagnostic PNGs, and other workshop-local temp artifacts.
/// Defaults to `{CARGO_MANIFEST_DIR}/tmp`. Override with `HITBOX_WORKSHOP_TMP`.
pub fn workshop_tmp_root() -> PathBuf {
    std::env::var("HITBOX_WORKSHOP_TMP")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp"))
}

/// Ensure [`workshop_tmp_root`] exists and return `{root}/{filename}`.
pub fn workshop_tmp_path(filename: &str) -> PathBuf {
    let root = workshop_tmp_root();
    let _ = std::fs::create_dir_all(&root);
    root.join(filename)
}

/// Write a debug WGSL dump under [`workshop_tmp_root`].
pub fn write_workshop_wgsl_dump(filename: &str, contents: &str) {
    let path = workshop_tmp_path(filename);
    if let Err(e) = std::fs::write(&path, contents) {
        eprintln!("[DUMP] failed to write {}: {e}", path.display());
    }
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

/// EffectConverter PTCL dump cache (`{target}/hitbox-editor-cache/ptcl-dumps/`).
pub fn effect_dump_cache_root() -> PathBuf {
    app_storage_root().join("ptcl-dumps")
}

/// Deterministic SPIR-V→WGSL memoization cache (`{target}/hitbox-editor-cache/wgsl-cache/`).
/// naga's GLSL→WGSL stage is nondeterministic across process launches (std HashMap seed), so
/// generated WGSL is cached by content hash to make rendering reproducible. See the
/// `renderer-nondeterminism` note.
pub fn wgsl_cache_root() -> PathBuf {
    app_storage_root().join("wgsl-cache")
}

/// Deterministic BNSH→SPIR-V decode cache (`{target}/hitbox-editor-cache/bnsh-decode-cache/`).
/// The external bnsh-decoder CLI produces different SPIR-V across process launches for identical
/// input; results are cached by BNSH content hash so decoding is reproducible. See the
/// `renderer-nondeterminism` note.
pub fn bnsh_decode_cache_root() -> PathBuf {
    app_storage_root().join("bnsh-decode-cache")
}

/// Debug builds only: delete PTCL dump cache and EffectConverter scratch temps on each run
/// so effects are re-converted from scratch. Set `HITBOX_KEEP_CACHE=1` to skip.
pub fn dev_refresh_storage_on_startup() {
    #[cfg(not(debug_assertions))]
    return;

    if std::env::var("HITBOX_KEEP_CACHE").is_ok() {
        eprintln!("[CACHE] HITBOX_KEEP_CACHE set — keeping existing cache");
        return;
    }

    let root = app_storage_root();
    eprintln!("[CACHE] dev refresh: storage root {}", root.display());

    for sub in ["ptcl-dumps", "scratch"] {
        let path = root.join(sub);
        if !path.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => eprintln!("[CACHE] cleared {}", path.display()),
            Err(e) => eprintln!("[CACHE] failed to clear {}: {e}", path.display()),
        }
    }
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

/// Locate `ef_{name}.eff` under the configured effect export directory. Accepts fighter
/// names and the shared system files (`common` → `system/common/ef_common.eff`).
pub fn resolve_fighter_eff(fighter: &str) -> Option<PathBuf> {
    let root = effect_export_root()?;
    [
        root.join("fighter").join(fighter).join(format!("ef_{fighter}.eff")),
        root.join("system").join(fighter).join(format!("ef_{fighter}.eff")),
        root.join(format!("ef_{fighter}.eff")),
    ]
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
