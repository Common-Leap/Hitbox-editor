//! Archive extraction for mod import.
//!
//! Compiled Smash mods are distributed as `.zip` and `.7z` far more often than as loose
//! folders. Extracting them inside the tool matters for correctness, not just convenience:
//! hand-extraction is where a user ends up pointing the library at the wrong nesting level,
//! and a mod rooted one directory too high provides nothing while looking perfectly healthy.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Archive extensions this module handles, lowercase and without the dot.
pub const SUPPORTED: &[&str] = &["zip", "7z"];

pub fn is_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| SUPPORTED.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// A filesystem-safe directory name for an archive, used as its stable cache slug.
pub fn slug_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "mod".to_string());
    crate::mod_export::slugify(&stem)
}

/// Extract `archive` into the library cache and return the directory.
///
/// The destination is stable per archive slug and is **cleared first**, so re-importing the
/// same mod overwrites rather than merging into a stale extraction. Merging is the subtler
/// failure: a file the mod author removed in a newer release would survive in the cache and
/// keep being served to the game.
pub fn extract(archive: &Path) -> Result<PathBuf> {
    let destination = super::library::extraction_root(&slug_for(archive));
    if destination.exists() {
        std::fs::remove_dir_all(&destination)
            .with_context(|| format!("clearing {}", destination.display()))?;
    }
    std::fs::create_dir_all(&destination)
        .with_context(|| format!("creating {}", destination.display()))?;

    let extension = archive
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "zip" => extract_zip(archive, &destination)?,
        "7z" => extract_7z(archive, &destination)?,
        other => bail!("unsupported archive type .{other} — extract it and import the folder"),
    }
    Ok(destination)
}

fn extract_zip(archive: &Path, destination: &Path) -> Result<()> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {} as a zip", archive.display()))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            // `enclosed_name` is None exactly for entries that would escape the destination.
            continue;
        };
        let Some(target) = safe_join(destination, relative.as_ref()) else {
            continue;
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)
            .with_context(|| format!("writing {}", target.display()))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn extract_7z(archive: &Path, destination: &Path) -> Result<()> {
    sevenz_rust2::decompress_file(archive, destination)
        .with_context(|| format!("extracting {}", archive.display()))?;
    Ok(())
}

/// Join a relative archive path under `base`, refusing anything that escapes it.
///
/// Archive entries are attacker-controlled text, and a `../` in one is how an extraction
/// writes outside its destination. Both extractors route through this rather than trusting
/// their own path handling.
fn safe_join(base: &Path, relative: &Path) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (out != base).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_paths_are_refused_and_ordinary_ones_are_not() {
        let base = Path::new("/cache/mod");
        assert_eq!(
            safe_join(base, Path::new("fighter/mario/a.numdlb")),
            Some(PathBuf::from("/cache/mod/fighter/mario/a.numdlb"))
        );
        assert_eq!(safe_join(base, Path::new("../escape")), None);
        assert_eq!(safe_join(base, Path::new("/absolute")), None);
        assert_eq!(safe_join(base, Path::new("a/../../b")), None);
        // An entry naming the destination itself would make `create_dir_all(parent)` write
        // above it.
        assert_eq!(safe_join(base, Path::new("")), None);
    }

    #[test]
    fn archive_detection_is_case_insensitive_and_narrow() {
        assert!(is_archive(Path::new("Mod.ZIP")));
        assert!(is_archive(Path::new("mod.7z")));
        assert!(!is_archive(Path::new("mod.rar")));
        assert!(!is_archive(Path::new("fighter")));
    }
}
