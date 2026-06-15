//! Scratch directories for CLI tools (EffectConverter, bnsh-decoder, spirv-cross).
//!
//! Defaults to `~/.cache/hitbox-editor/` instead of `/tmp` to avoid tmpfs quota exhaustion.

use std::path::PathBuf;

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
