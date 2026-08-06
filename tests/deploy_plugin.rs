//! The plugin deploy script's double-plugin guard, exercised end to end.
//!
//! Skyline loads *every* file in `romfs:/skyline/plugins/` as a plugin, extension ignored, so a
//! `lib_effect_viewer.nro.bak` left beside the real one runs a second full copy of the plugin:
//! double ACMD hooks, double per-frame drivers, two servers racing for :7878. The visible symptom
//! is a hard 60→30 fps drop entering training mode, which reads as a performance regression in
//! whatever was last touched. It cost about six rounds of misdiagnosis once, because it also
//! invalidates every A/B test run while it is there.
//!
//! `deploy_plugin.py` now refuses to deploy over one. This is where that is checked, rather than
//! in the Python file itself, because `cargo test` is the gate this project actually runs — a
//! test suite nothing invokes is a comment.
//!
//! The tests drive the real script in a throwaway directory. Nothing here mocks the filesystem,
//! so what passes is what a user's mod folder will do.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The path constant the script searches candidate files for. Copied here on purpose: the script
/// hardcodes these bytes, and [`the_marker_the_script_greps_for_is_still_in_the_plugin_source`]
/// checks all three copies still agree.
const PLUGIN_MARKER: &str = "sd:/slight/diag.txt";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    repo_root().join("plugins/slight_replica/tools/deploy_plugin.py")
}

/// A fresh, empty mod root under the system temp directory, plus its plugins directory. Each
/// test owns a distinct name so the suite is safe to run in parallel.
fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("visionary-deploy-test/{name}"));
    let _ = fs::remove_dir_all(&root);
    let plugins = root.join("romfs/skyline/plugins");
    fs::create_dir_all(&plugins).expect("create fixture plugins dir");
    (root, plugins)
}

/// Bytes that look like a build of this plugin to the script: anything containing the marker.
fn plugin_bytes(tag: &str) -> Vec<u8> {
    format!("\0ELF-ish padding\0{PLUGIN_MARKER}\0build={tag}\0").into_bytes()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Lay down the three runtime dependencies the script expects, so its "MISSING dependency"
/// reporting is not what a test ends up measuring.
fn write_dependencies(plugins: &Path) {
    write(&plugins.join("libarcropolis.nro"), b"arcropolis");
    write(&plugins.join("libnro_hook.nro"), b"nro_hook");
    write(
        &plugins.join("libsmashline_plugin.nro"),
        b"smashline_install_state_callback",
    );
}

fn run_deploy(mod_dir: &Path, nro: &Path, extra: &[&str]) -> Output {
    let mut cmd = Command::new("python3");
    cmd.arg(script())
        .arg("--emulator")
        .arg("eden")
        .arg("--mod-dir")
        .arg(mod_dir)
        .arg("--nro")
        .arg(nro)
        .args(extra);
    cmd.output().expect(
        "run deploy_plugin.py — python3 is a hard dependency of the plugin build and deploy \
         scripts (scripts/build.sh execs it), so a missing interpreter is a real failure here, \
         not a reason to skip",
    )
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The paired positive for the two "did not deploy" assertions below.
///
/// Without this, "the installed .nro is absent" would pass for a fixture that could never have
/// produced one — a broken script path, a Python syntax error, a wrong argument name all look
/// exactly like a successful refusal.
#[test]
fn a_clean_plugins_directory_deploys() {
    let (root, plugins) = fixture("clean");
    write_dependencies(&plugins);
    let nro = root.join("lib_effect_viewer.nro");
    write(&nro, &plugin_bytes("new"));

    let out = run_deploy(&root, &nro, &[]);
    assert!(
        out.status.success(),
        "expected success, got:\n{}",
        combined(&out)
    );
    assert_eq!(
        fs::read(plugins.join("lib_effect_viewer.nro")).unwrap(),
        plugin_bytes("new"),
        "a clean directory must actually receive the build"
    );
}

#[test]
fn a_stray_copy_of_the_plugin_refuses_the_deploy_and_changes_nothing() {
    let (root, plugins) = fixture("stray");
    write_dependencies(&plugins);
    let stray = plugins.join("lib_effect_viewer.nro.bak");
    write(&stray, &plugin_bytes("old"));
    let nro = root.join("lib_effect_viewer.nro");
    write(&nro, &plugin_bytes("new"));

    let out = run_deploy(&root, &nro, &[]);
    let text = combined(&out);
    assert!(!out.status.success(), "expected a refusal, got:\n{text}");
    assert!(
        text.contains("lib_effect_viewer.nro.bak"),
        "the refusal must name the file to remove, or it is another mystery:\n{text}"
    );
    assert!(
        !plugins.join("lib_effect_viewer.nro").exists(),
        "a refusal must be a no-op — it is safe to re-run only if it deployed nothing"
    );
    assert_eq!(
        fs::read(&stray).unwrap(),
        plugin_bytes("old"),
        "and it must not have deleted the stray it refused over"
    );
}

/// The name is the one thing that varies. `.bak`, `.old`, a hand-renamed known-good build — the
/// script matches on content for exactly this reason, so the test refuses to only try `.bak`.
#[test]
fn a_stray_is_found_under_any_name() {
    for name in ["lib_effect_viewer.old", "keep-this-one.nro", "backup.txt"] {
        let (root, plugins) = fixture("renamed");
        write_dependencies(&plugins);
        write(&plugins.join(name), &plugin_bytes("old"));
        let nro = root.join("lib_effect_viewer.nro");
        write(&nro, &plugin_bytes("new"));

        let out = run_deploy(&root, &nro, &[]);
        let text = combined(&out);
        assert!(
            !out.status.success(),
            "{name} should have been caught:\n{text}"
        );
        assert!(text.contains(name), "{name} should be named in:\n{text}");
    }
}

/// An unrelated Skyline plugin in the same directory is normal and supported. Refusing to deploy
/// over one would make the guard something users route around, at which point it protects nobody.
#[test]
fn an_unrelated_plugin_does_not_block_the_deploy() {
    let (root, plugins) = fixture("unrelated");
    write_dependencies(&plugins);
    write(
        &plugins.join("libtraining_modpack.nro"),
        b"some other plugin",
    );
    let nro = root.join("lib_effect_viewer.nro");
    write(&nro, &plugin_bytes("new"));

    let out = run_deploy(&root, &nro, &[]);
    let text = combined(&out);
    assert!(out.status.success(), "expected success, got:\n{text}");
    assert!(
        plugins.join("libtraining_modpack.nro").exists(),
        "and it must still be there afterwards"
    );
}

#[test]
fn remove_strays_deletes_the_second_copy_and_then_deploys() {
    let (root, plugins) = fixture("remove");
    write_dependencies(&plugins);
    let stray = plugins.join("lib_effect_viewer.nro.bak");
    write(&stray, &plugin_bytes("old"));
    let nro = root.join("lib_effect_viewer.nro");
    write(&nro, &plugin_bytes("new"));

    let out = run_deploy(&root, &nro, &["--remove-strays"]);
    let text = combined(&out);
    assert!(out.status.success(), "expected success, got:\n{text}");
    assert!(!stray.exists(), "the stray should be gone:\n{text}");
    assert_eq!(
        fs::read(plugins.join("lib_effect_viewer.nro")).unwrap(),
        plugin_bytes("new")
    );
}

/// The guard is only as good as the marker, and the marker is a string literal duplicated in three
/// places that no compiler relates to each other.
///
/// If the plugin ever stops writing `sd:/slight/diag.txt` — renamed file, path built by `format!`
/// instead of a constant — every test above still passes, because they synthesise their own
/// fixture bytes containing the marker. The scan would then match nothing real and the guard would
/// go quietly dead. This is the assertion that fails instead.
///
/// **What it does not prove.** That the string survives into the linked `.nro`. It does today
/// (verified by grepping a release build), and it is used as a `&str` constant in two `std::fs`
/// calls so the linker has no reason to drop it — but a built plugin does not exist in a fresh
/// clone, and a check that skips when the artefact is missing would pass vacuously in exactly the
/// situation where it matters. Source is the strongest thing available unconditionally.
#[test]
fn the_marker_the_script_greps_for_is_still_in_the_plugin_source() {
    let diag = repo_root().join("plugins/slight_replica/src/slight/diag.rs");
    let source = fs::read_to_string(&diag).expect("read the plugin's diag module");
    assert!(
        source.contains(&format!("\"{PLUGIN_MARKER}\"")),
        "{} no longer contains the literal \"{PLUGIN_MARKER}\" that deploy_plugin.py scans \
         candidate files for. Either restore it or change PLUGIN_MARKER in the script, this \
         test, and the plugin together — the guard silently matches nothing otherwise.",
        diag.display()
    );

    let script_source = fs::read_to_string(script()).expect("read deploy_plugin.py");
    assert!(
        script_source.contains(&format!("b\"{PLUGIN_MARKER}\"")),
        "deploy_plugin.py's PLUGIN_MARKER has drifted from this test's copy of it"
    );
}
