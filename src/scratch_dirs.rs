//! User-local cache and temporary storage used by Visionary.

use std::path::{Path, PathBuf};

/// Scratch .eff the editor writes NEXT TO a fighter's source eff: the source with every
/// recorded transplant merged in, used as the eff editor's pristine baseline and as the
/// bytes deployed to the running game.
pub const TRANSPLANT_PREVIEW_FILE: &str = "_transplant_preview.eff";

/// The pre-rename name of [`TRANSPLANT_PREVIEW_FILE`]. Kept so stale files written by
/// older builds are still hidden from the source lists and still deleted alongside the
/// current one, instead of being orphaned next to the user's game dump forever.
pub const LEGACY_TRANSPLANT_PREVIEW_FILE: &str = "_oneslot_preview.eff";

/// True for either the current or the legacy transplant-preview file name. Callers match
/// on a substring because the check also runs against full relative paths.
pub fn is_transplant_preview_name(name: &str) -> bool {
    name.contains("_transplant_preview") || name.contains("_oneslot_preview")
}

/// Delete the transplant preview next to `source_eff`, current and legacy names both.
pub fn remove_transplant_previews(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(TRANSPLANT_PREVIEW_FILE));
    let _ = std::fs::remove_file(dir.join(LEGACY_TRANSPLANT_PREVIEW_FILE));
}

/// Persistent cache root. `VISIONARY_CACHE_DIR` overrides the platform cache directory.
pub fn app_storage_root() -> PathBuf {
    std::env::var_os("VISIONARY_CACHE_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::cache_dir().map(|path| path.join("visionary")))
        .unwrap_or_else(|| std::env::temp_dir().join("visionary"))
}

/// Unique temporary directory for EffectLibrary operations that need filesystem scratch space.
pub fn app_scratch_dir(prefix: &str) -> anyhow::Result<tempfile::TempDir> {
    let base = app_storage_root().join("scratch");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to create scratch directory in {}: {error} (set VISIONARY_CACHE_DIR to another disk)",
                base.display()
            )
        })
}
