//! Stage roster edits onto the emulator SD for a manual reboot.
//!
//! Some edits reach the running game live (effect transplants via the
//! carrier, ACMD live rules). Roster edits do not: new costume slots, models,
//! animations, select-screen order/names, and fighter-wide values are served
//! from the SD card's mod folders, which Arcropolis reads at boot. Testing
//! those means staging the files and rebooting the emulator yourself — this
//! module is the staging half. (It used to relaunch the emulator too, but a
//! dirty kill corrupted emulator state and crashed the next boot; rebooting
//! by hand through the emulator UI is a clean shutdown and stays reliable.)

use std::path::{Path, PathBuf};

/// Folder name of the dev-staging mod under `<sd>/ultimate/mods/`. A fixed
/// name (rather than per-project) keeps one entry in the emulator's mod list;
/// each deploy wipes and rewrites it, so it can never serve stale files.
pub const DEV_MOD_NAME: &str = "visionary_dev";

/// Stage the dev mod: wipe `<sd>/ultimate/mods/visionary_dev/` and let
/// `write` fill it, so a deploy can never serve files from a previous one.
/// Returns the mod dir and whatever the writer reported.
pub fn stage_dev_mod<T>(
    sd: &Path,
    write: impl FnOnce(&Path) -> anyhow::Result<T>,
) -> anyhow::Result<(PathBuf, T)> {
    if !sd.join("ultimate").is_dir() {
        anyhow::bail!(
            "{} does not look like an emulator SD root (no ultimate/). Pick the folder \
             containing your emulator's `ultimate` directory.",
            sd.display()
        );
    }
    let dest = sd.join("ultimate").join("mods").join(DEV_MOD_NAME);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    let out = write(&dest)?;
    Ok((dest, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_wipes_the_previous_deploy() {
        let sd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(sd.path().join("ultimate")).unwrap();
        let stale = sd
            .path()
            .join("ultimate/mods")
            .join(DEV_MOD_NAME)
            .join("old.txt");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, b"stale").unwrap();

        let (dest, files) = stage_dev_mod(sd.path(), |dir| {
            std::fs::write(dir.join("new.prc"), b"new")?;
            Ok(vec!["new.prc".to_string()])
        })
        .unwrap();
        assert!(!stale.exists(), "previous deploy must not survive");
        assert!(dest.join("new.prc").is_file());
        assert_eq!(files, vec!["new.prc"]);
    }

    #[test]
    fn staging_refuses_a_directory_without_ultimate() {
        let dir = tempfile::tempdir().unwrap();
        let err = stage_dev_mod(dir.path(), |_| Ok(Vec::<String>::new())).unwrap_err();
        assert!(
            err.to_string().contains("ultimate"),
            "must say what is missing: {err}"
        );
    }
}
