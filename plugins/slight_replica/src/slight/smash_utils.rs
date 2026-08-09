//! Jorge SLight paths and RPM gateway helpers (`smash_utils.rs` @ 8aad775).

use std::sync::atomic::{AtomicBool, Ordering};

pub const DEBUG_LOGGERS: &str = "sd:/slight/debug/loggers/";
pub const ERROR_LOGS: &str = "sd:/slight/user/error_logs/";
pub const DEBUGGABLES_DIR: &str = "sd:/slight/user/debuggables/";
pub const GATEWAY_FILE: &str = "sd:/slight/user/gateway.txt";
pub const CLIENT_ID_FILE: &str = "sd:/slight/user/client_id.txt";
pub const DEBUG_ACTIVATE: &str = "sd:/slight/debug/activate.txt";
pub const DEBUG_DEACTIVATE: &str = "sd:/slight/debug/deactivate.txt";
/// One-shot opt-in for the otherwise-disabled inline collision trampoline test.
pub const DEBUG_INLINE_COLLISION: &str = "sd:/slight/debug/inline_collision_hook.txt";
/// Opt-in for the effect-loader research trace — see [`trace_enabled`].
pub const DEBUG_TRACE: &str = "sd:/slight/debug/trace.txt";
/// Whitespace/comma-separated subsystem names to leave uninstalled — see [`subsystem_disabled`].
pub const DEBUG_OFF: &str = "sd:/slight/debug/off.txt";

/// Guards [`ensure_slight_dirs`] against running more than once. See that function.
static DIRS_ENSURED: AtomicBool = AtomicBool::new(false);

/// Create the plugin's SD directories and seed the cached debug flags. **Boot only** — every
/// call after the first returns without touching the filesystem.
///
/// The guard is not an optimisation, it is the fix for R2's headline finding.
/// `poll_transactions` called this on **every frame** as a defensive prelude to its `read_dir`,
/// and each call is four `create_dir_all` (a `stat` per path component, all of them long since
/// present), three `exists` probes through `refresh_debug_logging`, and a `read_to_string` of
/// `off.txt`, which normally does not exist. Nine filesystem operations per frame on the game
/// thread, of which nine were redundant — and the whole point of `slight::sd_poll` is that this
/// costs 20-200 µs each on Windows against a 16.6 ms budget.
///
/// Nothing recreates a directory a user deletes mid-session, and nothing should: `read_dir` on a
/// missing directory already returns empty, which is the same answer it gives for the empty
/// directory this used to guarantee. The only caller that needs one to *exist* is the one
/// writing into it, and those create their own.
pub fn ensure_slight_dirs() {
    if DIRS_ENSURED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::fs::create_dir_all(DEBUG_LOGGERS);
    let _ = std::fs::create_dir_all(ERROR_LOGS);
    let _ = std::fs::create_dir_all(DEBUGGABLES_DIR);
    let _ = std::fs::create_dir_all("sd:/slight/user/");
    // Seed the cached flag so boot-time callers see the same answer they always did, before
    // the per-match poll tick has run for the first time.
    refresh_debug_logging();
    load_disabled_subsystems();
}

/// Subsystems named in [`DEBUG_OFF`] at boot. Read once, before anything installs.
static DISABLED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Read [`DEBUG_OFF`] once. Splitting on any non-alphanumeric run means a file written as
/// `acmd, hitbox` or one name per line both work — the user is editing this by hand on a
/// device, so the format has to be forgiving.
fn load_disabled_subsystems() {
    let names: Vec<String> = std::fs::read_to_string(DEBUG_OFF)
        .unwrap_or_default()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let _ = DISABLED.set(names);
}

/// True when the user asked for this subsystem to stay uninstalled by naming it in
/// [`DEBUG_OFF`] before boot.
///
/// This exists to bisect a hang. The plugin installs seven independent groups of hooks into
/// the game, several of them inside the resource loader, and when one of them wedges a match
/// load there is no output to work from — the diag buffer only flushes from the per-frame
/// driver, which never runs. Rebuilding and redeploying to test each group costs minutes per
/// attempt; naming it in a file costs a reboot. Recognised names are listed in the plugin
/// README.
pub fn subsystem_disabled(name: &str) -> bool {
    DISABLED
        .get()
        .is_some_and(|d| d.iter().any(|n| n == name))
}

/// The subsystems left uninstalled this boot, for the boot log.
pub fn disabled_subsystems() -> String {
    match DISABLED.get() {
        Some(d) if !d.is_empty() => d.join(","),
        _ => "none".to_string(),
    }
}

pub fn rpm_listen_port() -> u16 {
    let Ok(text) = std::fs::read_to_string(GATEWAY_FILE) else {
        return 7878;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Original regex: (?P<ip>\d+\.\d+\.\d+\.\d+)(:(?P<port>\d+))?
        let (addr, port_str) = match line.rsplit_once(':') {
            Some((a, p)) => (a, Some(p)),
            None => (line, None),
        };
        if !is_dotted_quad(addr) {
            skyline::println!("[SLight] Didn't got ip from {line}");
            continue;
        }
        if let Some(ps) = port_str {
            match ps.parse::<u32>() {
                Ok(p) if p > 0 && p <= 65535 => return p as u16,
                _ => {
                    skyline::println!(
                        "[SLight] Port {ps} is not in range 0-, the port will be ignored and only the address {addr} will be used"
                    );
                    return 7878;
                }
            }
        }
        return 7878;
    }
    7878
}

fn is_dotted_quad(s: &str) -> bool {
    let mut parts = s.split('.');
    let valid = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), Some(d), None) if valid(a) && valid(b) && valid(c) && valid(d)
    )
}

/// Last probed state of the debug-logging trigger files.
///
/// This used to stat both files on every call, and `debug_logging_enabled` guards 40+ call
/// sites — six inside `event_system::on_frame`, sixteen in `animation_sequencer` (several in
/// per-agent loops), more in `article_notifier`'s per-weapon scan. Both lookups normally MISS.
/// Linux answers a repeated miss out of the negative dentry cache for about a microsecond, so
/// the cost is invisible there. Windows has no negative-lookup cache: each miss is a full
/// path parse and directory probe through the emulator's sdmc VFS, with Defender hooking the
/// open — tens to hundreds of microseconds. At 50+ probes per frame that is most of a 16.6 ms
/// budget, which is why Windows testers saw ~10 fps and Linux saw nothing.
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

/// Last probed state of the research-trace trigger file.
///
/// The effect-loader trace writes a line to the SD card from inside the game's own resource
/// hooks — `ensure_dir_loaded` and `load_effects` run on the loading thread, while ARCropolis
/// is servicing that same thread's reads. Each line is a separate open/write/close, thousands
/// of them while a match loads, and every one of them re-enters `nn::fs` underneath the
/// loader. That is fine on a dev machine watching a specific bug and ruinous everywhere else:
/// a heavy mod's fighter can stall or never finish loading (issue #3). Off unless asked for.
static TRACE: AtomicBool = AtomicBool::new(false);

/// Re-probe the trigger files. Called from the throttled SD poll tick, not per frame.
pub fn refresh_debug_logging() {
    let on = std::path::Path::new(DEBUG_ACTIVATE).exists()
        && !std::path::Path::new(DEBUG_DEACTIVATE).exists();
    DEBUG_LOGGING.store(on, Ordering::Relaxed);
    TRACE.store(std::path::Path::new(DEBUG_TRACE).exists(), Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

/// True when the user opted into the effect-loader research trace by creating
/// [`DEBUG_TRACE`]. The observation-only hooks it needs are installed at boot, so the file
/// has to exist before the game starts; creating it later enables the log writes alone.
pub fn trace_enabled() -> bool {
    TRACE.load(Ordering::Relaxed)
}

pub fn set_debug_logging(on: bool) {
    if on {
        let _ = std::fs::write(DEBUG_ACTIVATE, b"1");
        let _ = std::fs::remove_file(DEBUG_DEACTIVATE);
    } else {
        let _ = std::fs::write(DEBUG_DEACTIVATE, b"1");
    }
    // Write through: an in-process toggle must take effect now, not at the next poll tick.
    DEBUG_LOGGING.store(on, Ordering::Relaxed);
}

/// Jorge FUN_7100124b24 — delete SD file if present; returns true when removed.
pub fn consume_sd_trigger(path: &str) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}
