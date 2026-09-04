//! Auto-deploy the Skyline plugin to the local Eden install on startup.
//!
//! The user wants to only run Eden + Visionary to test.  The plugin NRO at
//! `plugins/slight_replica/target/aarch64-skyline-switch/release/lib_effect_viewer.nro`
//! is built by `bash plugins/slight_replica/scripts/build.sh` (which now also
//! deploys), but if the user just runs `cargo run` for Visionary we still want
//! the deployed copy in `.../eden/load/.../Arcropolis/romfs/skyline/plugins/`
//! to be fresh.  This module does a best-effort, non-blocking check at startup
//! and rebuilds+redeploys if the deployed copy is missing or stale.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Try to auto-deploy in the background. Never blocks the UI and never panics.
pub fn spawn_background_check() {
    std::thread::spawn(|| {
        if let Err(e) = check_and_deploy() {
            eprintln!("[visionary] Plugin auto-deploy skipped: {e}");
        }
    });
}

fn check_and_deploy() -> anyhow::Result<()> {
    // Only attempt if an Eden SD/mod dir can be found — portable installs without
    // a standard layout will just be deployed manually.
    let eden_mod = eden_mod_dir()?;
    let deployed = eden_mod.join("romfs/skyline/plugins/lib_effect_viewer.nro");
    let built = built_nro_path();

    // If the built NRO is missing, try to build it.
    let built_exists = built.is_file();
    let deployed_exists = deployed.is_file();

    let needs_build = if !built_exists {
        true
    } else if !deployed_exists {
        true
    } else {
        // Compare mtimes: if any source file is newer than the deployed NRO, rebuild.
        is_source_newer_than(&deployed)?
    };

    if !needs_build {
        return Ok(());
    }

    if !built_exists || is_source_newer_than(&built)? {
        eprintln!("[visionary] Plugin sources newer than NRO — rebuilding...");
        let status = Command::new("bash")
            .arg("plugins/slight_replica/scripts/build.sh")
            .current_dir(find_workspace_root()?)
            .status()?;
        if !status.success() {
            anyhow::bail!("plugin build failed with {}", status);
        }
    }

    // Now deploy: copy the built NRO to the Eden mod dir.
    // Use the Python helper so stray-copy checks are the same as manual deploy.
    let status = Command::new("python3")
        .arg("plugins/slight_replica/tools/deploy_plugin.py")
        .arg("--emulator")
        .arg("eden")
        .current_dir(find_workspace_root()?)
        .status()?;
    if !status.success() {
        anyhow::bail!("deploy_plugin.py failed with {}", status);
    }
    eprintln!("[visionary] Plugin auto-deployed to {}", deployed.display());
    Ok(())
}

fn built_nro_path() -> PathBuf {
    find_workspace_root()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("plugins/slight_replica/target/aarch64-skyline-switch/release/lib_effect_viewer.nro")
}

fn find_workspace_root() -> anyhow::Result<PathBuf> {
    // Walk up from this file's crate dir until we find Cargo.toml with [workspace] or [package] name visionary.
    let mut cur = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if cur.join("Cargo.toml").is_file() {
            // Heuristic: Visionary's Cargo.toml has a [[bin]] named visionary
            if let Ok(text) = std::fs::read_to_string(cur.join("Cargo.toml")) {
                if text.contains("name = \"visionary\"") {
                    return Ok(cur);
                }
            }
        }
        if !cur.pop() {
            anyhow::bail!("could not find Visionary workspace root");
        }
    }
}

fn eden_mod_dir() -> anyhow::Result<PathBuf> {
    // Mirror host_paths.py logic: VISIONARY_EDEN_MOD_DIR or data_dir/eden/load/...
    if let Some(dir) = std::env::var_os("VISIONARY_EDEN_MOD_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Ok(p);
        }
    }
    let data = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data_dir"))?;
    let p = data.join("eden/load/01006A800016E000/Arcropolis");
    if p.is_dir() {
        Ok(p)
    } else {
        anyhow::bail!("Eden mod dir not found at {}", p.display());
    }
}

fn is_source_newer_than(deployed: &Path) -> anyhow::Result<bool> {
    let deployed_mtime = std::fs::metadata(deployed)?.modified()?;
    let root = find_workspace_root()?;
    let src = root.join("plugins/slight_replica/src");
    let cargo = root.join("plugins/slight_replica/Cargo.toml");
    let mut newest = None::<std::time::SystemTime>;
    for path in [src, cargo] {
        // Walk
        let mut stack = vec![path];
        while let Some(p) = stack.pop() {
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&p) {
                    for e in entries.flatten() {
                        stack.push(e.path());
                    }
                }
            } else if p.extension().is_some_and(|e| e == "rs" || e == "toml") {
                let t = meta.modified()?;
                newest = Some(newest.map_or(t, |prev| prev.max(t)));
            }
        }
    }
    Ok(newest.is_some_and(|t| t > deployed_mtime))
}
