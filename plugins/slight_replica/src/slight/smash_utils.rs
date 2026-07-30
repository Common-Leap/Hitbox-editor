//! Jorge SLight paths and RPM gateway helpers (`smash_utils.rs` @ 8aad775).

use std::sync::atomic::{AtomicBool, Ordering};

pub const DEBUG_LOGGERS: &str = "sd:/slight/debug/loggers/";
pub const ERROR_LOGS: &str = "sd:/slight/user/error_logs/";
pub const DEBUGGABLES_DIR: &str = "sd:/slight/user/debuggables/";
pub const GATEWAY_FILE: &str = "sd:/slight/user/gateway.txt";
pub const CLIENT_ID_FILE: &str = "sd:/slight/user/client_id.txt";
pub const DEBUG_ACTIVATE: &str = "sd:/slight/debug/activate.txt";
pub const DEBUG_DEACTIVATE: &str = "sd:/slight/debug/deactivate.txt";

pub fn ensure_slight_dirs() {
    let _ = std::fs::create_dir_all(DEBUG_LOGGERS);
    let _ = std::fs::create_dir_all(ERROR_LOGS);
    let _ = std::fs::create_dir_all(DEBUGGABLES_DIR);
    let _ = std::fs::create_dir_all("sd:/slight/user/");
    // Seed the cached flag so boot-time callers see the same answer they always did, before
    // the per-match poll tick has run for the first time.
    refresh_debug_logging();
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

/// Re-probe the trigger files. Called from the throttled SD poll tick, not per frame.
pub fn refresh_debug_logging() {
    let on = std::path::Path::new(DEBUG_ACTIVATE).exists()
        && !std::path::Path::new(DEBUG_DEACTIVATE).exists();
    DEBUG_LOGGING.store(on, Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
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
