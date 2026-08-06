//! Live eff serving + IN-MATCH reparse (no re-entry).
//!
//! Ported from the original effect viewer's working `live_edit.rs` orchestration. The
//! editor writes each fighter's MERGED eff (transplants + authored edits baked in) plus a
//! manifest to `sd:/effect_viewer/live_eff/`. For each entry:
//!
//!  1. Register an Arcropolis GENERIC (disk) callback for the arc path with max_size =
//!     the SD file's size, so the game loads the merged bytes whenever it (re)loads that
//!     eff. Generic is load-bearing: only `arcrop_register_callback(max_size)` patches
//!     the arc FILE-TABLE size (immediately, even registered mid-game). The `with_path`
//!     stream callback never does (it is for `stream:/` files read via nn::fs), so a
//!     merged eff BIGGER than vanilla was truncated to the vanilla size — the appended
//!     transplant entries never existed in game and their spawns were invisible. A grown
//!     file on a later deploy is simply re-registered with the larger size.
//!  2. Non-eff files are pulled into the resident buffer with
//!     [`resource_reload::replace_loaded_file`].
//!
//! `.eff` files are DELIBERATELY left alone in step 2 — registration is the whole job, and the
//! merged bytes then load through a genuine arc request at the next match entry.
//!
//! There used to be a third step that reparsed a loaded eff in place
//! (`effect_reload::reparse_game_path`, since deleted). It never worked and it was dangerous:
//! `unload_effects`/`load_effects` rebuild the parsed structs from the RESIDENT buffer and
//! never re-request the file, so the callback was never hit (`cb_game=0`) and the reparse
//! faithfully re-parsed the VANILLA bytes. Driving it against a live fighter's effect slot
//! mid-match froze the game outright. Anything that must change DURING a match goes through
//! the carrier instead, whose eff is reloadable by design.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MANIFEST: &str = "sd:/effect_viewer/live_eff/manifest.json";
const DIR: &str = "sd:/effect_viewer/live_eff/";
/// Written into every diag file so on-device logs are attributable to a specific build.
pub const BUILD_TAG: &str = "2026-08-06m-rate-miss";

/// Times [`disk_cb`] served bytes. Split by initiator so we can DECISIVELY answer the
/// one open question of the whole cross-fighter-live effort: does the game's own resource
/// loader pull merged bytes through Arcropolis at fighter LOAD (→ `CB_GAME`), or does only
/// our own `arcrop_load_file` probe ever hit the callback (→ `CB_PROBE`)? `probe()` sets
/// [`PROBING`] around its `load_file` calls; every other hit is the game.
static CB_GAME: AtomicU64 = AtomicU64::new(0);
static CB_PROBE: AtomicU64 = AtomicU64::new(0);
/// True only while [`probe`] is calling `arcrop_load_file`, so [`disk_cb`] can attribute
/// that serve to the probe rather than the game. (The res loader runs on its own thread,
/// but a probe serve is synchronous within our own call, so a plain flag is sufficient —
/// a stray concurrent game serve mis-attributed as a probe is the only race and it only
/// ever UNDERcounts CB_GAME, never invents one.)
static PROBING: AtomicBool = AtomicBool::new(false);

#[derive(serde::Deserialize)]
struct ManifestEntry {
    /// Arc path the game requests, e.g. "effect/fighter/mario/ef_mario.eff".
    path: String,
    /// File name inside `sd:/effect_viewer/live_eff/` holding the merged bytes.
    file: String,
}

/// arc-path hash40 → absolute sd path served for that path (read by the disk callback).
static SERVED: parking_lot::Mutex<Option<HashMap<u64, String>>> = parking_lot::Mutex::new(None);
/// hash → max_size we last registered the generic callback with. Re-registering with a
/// bigger size re-patches the file table (monotonic) when a later deploy grows the file.
static REGISTERED: parking_lot::Mutex<Option<HashMap<u64, usize>>> = parking_lot::Mutex::new(None);

/// Generic (disk) callback: fill the game's load buffer with the merged SD bytes.
extern "C" fn disk_cb(hash: u64, out: *mut u8, capacity: usize, out_size: &mut usize) -> bool {
    let path = match SERVED.lock().as_ref().and_then(|m| m.get(&hash).cloned()) {
        Some(p) => p,
        None => return false,
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            crate::slight::diag::note(format!("live eff read failed ({path}): {e}"));
            return false;
        }
    };
    if bytes.len() > capacity {
        // File grew since registration and no reload re-registered it yet — serving a
        // truncated eff would corrupt the parse, so fall back to vanilla.
        crate::slight::diag::note(format!(
            "live eff too big for registered size ({} > {capacity}): {path} — redeploy",
            bytes.len()
        ));
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
    }
    *out_size = bytes.len();
    let by_probe = PROBING.load(Ordering::Relaxed);
    let (cb_game, cb_probe) = if by_probe {
        (
            CB_GAME.load(Ordering::Relaxed),
            CB_PROBE.fetch_add(1, Ordering::Relaxed) + 1,
        )
    } else {
        (
            CB_GAME.fetch_add(1, Ordering::Relaxed) + 1,
            CB_PROBE.load(Ordering::Relaxed),
        )
    };
    // Proof-of-serve trace: this is the ONLY place that knows a load re-read the file.
    // `cb_game>0` is the decisive proof that the GAME'S loader (not just our probe) pulls
    // merged bytes through Arcropolis — i.e. that cross-fighter transplants load at match entry.
    //
    // Behind the trace opt-in: the game serves this callback from its loading thread, so the
    // two writes below land in the middle of Arcropolis's own read. The same counters are in
    // `effect_viewer_last_reload.txt` and in the probe, both written off that path.
    if !crate::slight::smash_utils::trace_enabled() {
        crate::slight::diag::note(format!(
            "live eff served {} B for {hash:#x} by {}",
            bytes.len(),
            if by_probe { "probe" } else { "GAME" }
        ));
        return true;
    }
    let _ = std::fs::write(
        "sd:/effect_viewer_cb.txt",
        format!(
            "build={BUILD_TAG}\ncb_game={cb_game}\ncb_probe={cb_probe}\nlast_by={}\nlast_hash={hash:#x}\nlast_bytes={}\ncapacity={capacity}\npath={path}\n",
            if by_probe { "probe" } else { "GAME" },
            bytes.len()
        ),
    );
    // A dedicated, append-only proof file for the first game serve — survives later probes.
    if !by_probe {
        let _ = std::fs::write(
            "sd:/effect_viewer_gameserve.txt",
            format!("build={BUILD_TAG}\nGAME read merged bytes\nhash={hash:#x}\nbytes={}\npath={path}\ncb_game={cb_game}\n", bytes.len()),
        );
    }
    crate::slight::diag::note(format!(
        "live eff served {} B for {hash:#x} by {}",
        bytes.len(),
        if by_probe { "probe" } else { "GAME" }
    ));
    true
}

/// Set while a probe's `arcrop_load_file` is in flight so [`disk_cb`] attributes the serve
/// to the probe. Returns a guard that clears the flag on drop.
struct ProbeGuard;
impl ProbeGuard {
    fn new() -> Self {
        PROBING.store(true, Ordering::Relaxed);
        ProbeGuard
    }
}
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        PROBING.store(false, Ordering::Relaxed);
    }
}

fn is_eff_path(game_path: &str) -> bool {
    game_path.ends_with(".eff")
}

/// (Re)read the manifest, (re)register disk callbacks, reparse eff files in place, and
/// respawn live effects. Triggered by the editor over TCP after every deploy.
pub fn reload() {
    reload_inner(true);
}

fn reload_inner(apply_live: bool) {
    let text = match std::fs::read_to_string(MANIFEST) {
        Ok(t) => t,
        Err(_) => {
            if let Some(mut s) = SERVED.try_lock() {
                if s.as_ref().map(|m| !m.is_empty()).unwrap_or(false) {
                    *s = Some(HashMap::new());
                    crate::slight::diag::note("live eff manifest removed — serving nothing");
                }
            }
            return;
        }
    };
    let entries: Vec<ManifestEntry> = match serde_json::from_str(&text) {
        Ok(e) => e,
        Err(e) => {
            crate::slight::diag::note(format!("live eff manifest parse error: {e}"));
            return;
        }
    };

    let arcrop_ok = crate::slight::effect_viewer::arcrop::init();
    let (mut registered, mut refreshed) = (0usize, 0usize);
    // Always 0 now that eff reparsing is gone; kept in the diag line so a run can be seen to
    // have taken the safe path rather than leaving it to be inferred.
    let reparsed = 0usize;
    let mut served: HashMap<u64, String> = HashMap::new();
    let mut present: Vec<(String, u64)> = Vec::new();

    for entry in &entries {
        let path = entry.path.to_lowercase();
        let hash = smash::hash40(&path);
        let file = format!("{DIR}{}", entry.file);
        let size = match std::fs::metadata(&file) {
            Ok(m) if m.is_file() => m.len() as usize,
            _ => {
                crate::slight::diag::note(format!("live eff missing file, skipped: {file}"));
                continue;
            }
        };
        served.insert(hash, file.clone());
        present.push((path.clone(), hash));

        // Register (or grow) the generic callback: max_size patches the arc file table,
        // so register whenever the current file is bigger than what we last registered.
        // Same-size content updates need nothing — the callback re-reads the SD file.
        let need = {
            let mut reg = REGISTERED.lock();
            let reg = reg.get_or_insert_with(HashMap::new);
            match reg.get(&hash) {
                Some(&old) if old >= size => None,
                _ => {
                    reg.insert(hash, size);
                    Some(size)
                }
            }
        };
        if let Some(size) = need {
            if crate::slight::effect_viewer::arcrop::register_disk(hash, size, disk_cb) {
                registered += 1;
            } else {
                crate::slight::diag::note(format!("live eff register failed: {path}"));
                REGISTERED
                    .lock()
                    .get_or_insert_with(HashMap::new)
                    .remove(&hash);
            }
        }
    }

    // SERVED must be current BEFORE any reparse: the reparse makes the game re-request
    // the file THROUGH our disk callback, which reads SERVED. (This ordering being
    // wrong was one reason transplants came back invisible — the first reparse after a
    // deploy re-loaded the VANILLA bytes.)
    *SERVED.lock() = Some(served.clone());

    // Apply in place while the fighter is loaded — NEVER at boot (apply_live=false):
    // the game's effect manager / arc tables don't exist yet.
    //
    // `.eff` files are DELIBERATELY not reparsed here.
    //
    // Reparsing means `unload_effects` + `load_effects` on a live fighter's effect slot, and
    // that hangs the game — a same-fighter transplant reached this path and froze on the spot
    // (`apply_live=true reparsed=1` in effect_viewer_last_reload.txt). It never worked anyway:
    // the reparse rebuilds the emitter structs from the RESIDENT buffer without re-requesting
    // the file, so it re-parsed the same vanilla bytes and edits only appeared after a reboot.
    //
    // Refreshing SERVED (above) is the whole job here: the merged bytes then load through a
    // genuine arc request at the next match entry. Anything that has to change MID-match goes
    // through the carrier, whose eff is reloadable by design.
    if apply_live {
        for (path, hash) in &present {
            if is_eff_path(path) {
                continue;
            }
            if crate::slight::effect_viewer::resource_reload::replace_loaded_file(*hash) {
                refreshed += 1;
            }
        }
    }

    let cleared = if apply_live && reparsed > 0 {
        clear_live_effects()
    } else {
        0
    };

    let reg_known = REGISTERED.lock().as_ref().map(|r| r.len()).unwrap_or(0);
    let cb_game = CB_GAME.load(Ordering::Relaxed);
    let cb_probe = CB_PROBE.load(Ordering::Relaxed);
    crate::slight::diag::note(format!(
        "live eff reload — arcrop={arcrop_ok} files={} registered={registered} reg_known={reg_known} cb_game={cb_game} cb_probe={cb_probe} reparsed={reparsed} refreshed={refreshed} cleared={cleared}",
        served.len()
    ));
    skyline::println!(
        "[SLight] live eff: {} file(s), {registered} registered, {reparsed} reparsed, {refreshed} refreshed, {cleared} respawned (arcrop={arcrop_ok})",
        served.len()
    );
    let _ = std::fs::write(
        "sd:/effect_viewer_last_reload.txt",
        format!(
            "build={BUILD_TAG}\napply_live={apply_live}\narcrop={arcrop_ok}\nfiles={}\nregistered={registered}\nreg_known={reg_known}\ncb_game={cb_game}\ncb_probe={cb_probe}\nreparsed={reparsed}\nrefreshed={refreshed}\ncleared={cleared}\n{}\n",
            served.len(),
            crate::slight::effect_viewer::effect_reload::debug_line(),
        ),
    );
}

/// The SD path currently served for `hash` (the merged eff bytes), if any. Lets the
/// synchronous live re-read source merged bytes straight off disk (no arc / no thread).
pub fn served_path(hash: u64) -> Option<String> {
    SERVED.lock().as_ref().and_then(|m| m.get(&hash).cloned())
}

/// Kill every tracked live effect and re-request it so on-screen particles respawn from
/// the freshly re-parsed eff data (adapted to slight_replica's tracker).
pub(crate) fn respawn_live_effects() -> usize {
    clear_live_effects()
}

/// Kill every tracked live effect and re-request it so on-screen particles respawn from
/// the freshly re-parsed eff data (adapted to slight_replica's tracker).
fn clear_live_effects() -> usize {
    use smash::app::lua_bind::EffectModule;
    use smash::phx;

    struct Snap {
        mod_addr: u64,
        handle: u32,
        effect_hash: u64,
        bone_hash: u64,
        pos: phx::Vector3f,
        rot: phx::Vector3f,
        size: f32,
    }

    let snaps: Vec<Snap> = {
        let tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
        tracker
            .iter()
            .filter(|e| !e.synthetic) // synthetic = no real handle to kill/re-req
            .map(|e| Snap {
                mod_addr: e.module_accessor_addr,
                handle: e.handle,
                effect_hash: e.effect_hash,
                bone_hash: e.bone_hash,
                pos: phx::Vector3f {
                    x: e.data.pos.x,
                    y: e.data.pos.y,
                    z: e.data.pos.z,
                },
                rot: phx::Vector3f {
                    x: e.data.rot.x,
                    y: e.data.rot.y,
                    z: e.data.rot.z,
                },
                size: e.data.scale,
            })
            .collect()
    };

    unsafe {
        for s in &snaps {
            let ma = s.mod_addr as *mut smash::app::BattleObjectModuleAccessor;
            if !ma.is_null() && EffectModule::is_exist_effect(ma, s.handle) {
                EffectModule::kill(ma, s.handle, true, true);
            }
        }
    }

    unsafe {
        for s in &snaps {
            let ma = s.mod_addr as *mut smash::app::BattleObjectModuleAccessor;
            if ma.is_null() {
                continue;
            }
            let eff = phx::Hash40 {
                hash: s.effect_hash,
            };
            if s.bone_hash != 0 {
                let bone = phx::Hash40 { hash: s.bone_hash };
                EffectModule::req_on_joint(
                    ma, eff, bone, &s.pos, &s.rot, s.size, &s.pos, &s.rot, false, 0, 0, 0,
                );
            } else {
                EffectModule::req(ma, eff, &s.pos, &s.rot, s.size, 0, 0, false, 0);
            }
        }
    }

    snaps.len()
}

/// Answer the two open questions of the serving chain, per served file:
///  * `api_bytes` — what `arcrop_load_file` returns THROUGH Arcropolis's own loader
///    stack. == the SD file size ⇒ the api registration works and any failure is in the
///    game's load path; == vanilla size (or 0) ⇒ the registration itself isn't serving.
///  * `resident` — decomp size + head + whether the IN-MEMORY loaded eff contains an
///    `_os\0` entry name, i.e. whether any (re)load actually brought in merged bytes.
///
/// Triggered from TCP (`live_eff_probe`) so it can run seconds AFTER a reparse, once the
/// async res loads settled. Results go to `sd:/effect_viewer_probe.txt`.
pub fn probe() -> String {
    let served: Vec<(u64, String)> = SERVED
        .lock()
        .as_ref()
        .map(|m| m.iter().map(|(h, p)| (*h, p.clone())).collect())
        .unwrap_or_default();
    let mut out = format!("build={BUILD_TAG}\nfiles={}\n", served.len());
    for (hash, sd_path) in &served {
        let sd_size = std::fs::metadata(sd_path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let mut buf = vec![0u8; sd_size + 0x1000];
        let api_bytes = {
            let _g = ProbeGuard::new();
            crate::slight::effect_viewer::arcrop::load_file(*hash, &mut buf)
        };
        let resident =
            crate::slight::effect_viewer::resource_reload::resident_probe(*hash, b"_os\0");
        out.push_str(&format!(
            "hash={hash:#x}\n  sd_size={sd_size}\n  api_bytes={:?}\n  resident={}\n",
            api_bytes,
            match resident {
                Some((size, head, found_os)) => format!(
                    "size={size} head={} merged_entry_present={found_os}",
                    head.iter().map(|b| format!("{b:02x}")).collect::<String>()
                ),
                None => "not-loaded".into(),
            }
        ));
    }
    out.push_str(&format!(
        "cb_game={} cb_probe={}\n",
        CB_GAME.load(Ordering::Relaxed),
        CB_PROBE.load(Ordering::Relaxed)
    ));
    out.push_str(&format!(
        "{}\n{}",
        crate::slight::effect_viewer::effect_reload::donor_debug_line(),
        crate::slight::effect_viewer::effect_reload::slots_debug(),
    ));
    let _ = std::fs::write("sd:/effect_viewer_probe.txt", &out);
    crate::slight::diag::note(format!("live eff probe:\n{out}"));
    out
}

/// Install the effect-manager load/unload hooks (must run once at plugin init, before a
/// match loads any fighter eff) and register manifest callbacks. Registration-only at
/// boot: reparse/buffer-patch would touch game singletons that don't exist yet.
pub fn install() {
    if !crate::slight::smash_utils::subsystem_disabled("reload") {
        crate::slight::effect_viewer::effect_reload::install_hooks();
    }
    if !crate::slight::smash_utils::subsystem_disabled("liveeff") {
        reload_inner(false);
    }
}
