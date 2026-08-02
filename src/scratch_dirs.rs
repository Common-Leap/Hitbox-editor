//! User-local cache and temporary storage used by Visionary.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

// ── The running emulator's SD root ────────────────────────────────────────────

/// Explicit SD root chosen by the user, if any. Set once at startup from the persisted
/// setting; portable emulator installs keep their SD next to the `.exe` rather than under any
/// platform directory, so probing alone cannot find them.
static SD_ROOT_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Record the user's explicit SD root. `None` clears it and returns to probing.
pub fn set_emulator_sd_override(path: Option<PathBuf>) {
    if let Ok(mut slot) = SD_ROOT_OVERRIDE.lock() {
        *slot = path;
    }
}

/// Locate the SD root of the emulator running the game, or `None` if it cannot be found.
///
/// This used to be four copies of `home_dir()/.local/share/eden/sdmc` inlined at the call
/// sites, which is Eden's Linux layout and nothing else's. On Windows `home_dir()` is
/// `C:\Users\<name>`, so that resolved to `C:\Users\<name>\.local\share\eden\sdmc` — a path no
/// emulator uses. The write did not fail, because the caller created the directory first; it
/// succeeded into a junk tree in the user's profile, and the plugin was then told to read a
/// payload that was never anywhere it could see. That is what made every edit crash the game
/// for Windows testers.
///
/// `dirs::data_dir()` is the portable spelling of the same location: `~/.local/share` on
/// Linux (byte-identical to the old hardcoded path), `%APPDATA%` on Windows, and
/// `~/Library/Application Support` on macOS.
pub fn emulator_sd_root() -> Option<PathBuf> {
    // An explicit choice is taken at face value — the user may not have installed any mods
    // yet, so the `ultimate/` check below would reject a perfectly good directory.
    if let Some(path) = std::env::var_os("VISIONARY_SD_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| SD_ROOT_OVERRIDE.lock().ok().and_then(|slot| slot.clone()))
    {
        return path.is_dir().then_some(path);
    }

    sd_root_candidates()
        .into_iter()
        .find(|path| is_emulator_sd_root(path.as_path()))
}

/// The standard SD locations to probe, most likely first. Split out so the path spelling can
/// be asserted in a test without depending on what happens to exist on the machine running it.
fn sd_root_candidates() -> Vec<PathBuf> {
    let data = dirs::data_dir();
    let config = dirs::config_dir();
    [
        data.as_ref().map(|d| d.join("eden").join("sdmc")),
        data.as_ref().map(|d| d.join("yuzu").join("sdmc")),
        config.as_ref().map(|c| c.join("Ryujinx").join("sdcard")),
        data.as_ref().map(|d| d.join("Ryujinx").join("sdcard")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Does this directory look like a real emulator SD root?
///
/// Testing `is_dir()` on the root itself is NOT enough. Anyone who ran a build from before the
/// path fix has a junk `C:\Users\<name>\.local\share\eden\sdmc` sitting in their profile,
/// created by the old code, and it would match. Requiring `ultimate/` — the Arcropolis mods
/// directory, the same test the live-deploy path has always used — distinguishes the real SD
/// from that leftover. Being too strict is the safe direction: callers fall back to sending
/// payloads over the wire, which works everywhere.
fn is_emulator_sd_root(path: &Path) -> bool {
    path.join("ultimate").is_dir()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of routing through `dirs::data_dir()` is that it is the portable
    /// spelling of the path that was hardcoded before — so on Linux the first candidate has to
    /// come out byte-identical to the old `~/.local/share/eden/sdmc`, or this "fix" silently
    /// moves the working setup out from under the developer.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_first_candidate_matches_the_old_hardcoded_path() {
        let Some(home) = dirs::home_dir() else {
            return; // No home directory to compare against; nothing to assert.
        };
        // Only meaningful when XDG_DATA_HOME is unset or default — otherwise `data_dir()`
        // correctly points elsewhere and there is no "old path" to match.
        if std::env::var_os("XDG_DATA_HOME").is_some() {
            return;
        }
        assert_eq!(
            sd_root_candidates().first(),
            Some(&home.join(".local/share/eden/sdmc"))
        );
    }

    #[test]
    fn ryujinx_and_yuzu_layouts_are_probed_too() {
        let candidates = sd_root_candidates();
        let has = |needle: &str| {
            candidates
                .iter()
                .any(|c| c.to_string_lossy().replace('\\', "/").contains(needle))
        };
        assert!(has("eden/sdmc"), "{candidates:?}");
        assert!(has("yuzu/sdmc"), "{candidates:?}");
        assert!(has("Ryujinx/sdcard"), "{candidates:?}");
    }

    /// A bare directory must NOT qualify. Testers who ran the broken build have an empty
    /// `~/.local/share/eden/sdmc` that the old code created; accepting it would send payloads
    /// straight back into the hole this fix exists to close.
    #[test]
    fn a_directory_without_ultimate_is_not_an_sd_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_emulator_sd_root(dir.path()));
        std::fs::create_dir(dir.path().join("ultimate")).unwrap();
        assert!(is_emulator_sd_root(dir.path()));
    }
}
