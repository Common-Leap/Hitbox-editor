//! Show a file or folder in the OS file manager.
//!
//! The roster editors point at real files constantly — a scaffolded model
//! folder, a motion directory, a picked PNG — and "copy this path into your
//! file manager" is not a workflow. This opens it directly, on all three
//! desktop OSes, with no extra dependency.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The nearest path that actually exists: `path` itself, else the first
/// ancestor that does. A scaffolded folder that has not been created yet
/// still reveals usefully — its parent — instead of erroring.
pub fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

/// Open `path` (or its nearest existing ancestor) in the OS file manager.
///
/// Spawns and detaches: the editor does not wait for the window to close.
/// Fails when nothing in the path exists (e.g. a relative path with no
/// existent root) or the opener itself cannot start.
pub fn reveal(path: &Path) -> Result<()> {
    let target = nearest_existing(path)
        .with_context(|| format!("nothing in {} exists yet", path.display()))?;
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(&target);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(&target);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(&target);
        command
    };
    command
        .spawn()
        .with_context(|| format!("opening {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_path_reveals_itself() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(nearest_existing(dir.path()), Some(dir.path().to_path_buf()));
    }

    /// A not-yet-created scaffold folder reveals its parent, so "open files"
    /// works before the user has put anything there.
    #[test]
    fn a_missing_path_reveals_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("c08");
        assert_eq!(nearest_existing(&missing), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn a_path_with_no_existent_root_reveals_nothing() {
        let missing: PathBuf = ["definitely", "not", "here", "c08"].iter().collect();
        // Relative to the test's cwd this must not exist; if it somehow does,
        // the assertion documents a broken test environment, not a bug.
        if !missing.exists() {
            assert_eq!(nearest_existing(&missing), None);
        }
    }
}
