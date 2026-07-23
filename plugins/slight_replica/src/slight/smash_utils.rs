//! Jorge SLight paths and RPM gateway helpers (`smash_utils.rs` @ 8aad775).

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

pub fn debug_logging_enabled() -> bool {
    std::path::Path::new(DEBUG_ACTIVATE).exists()
        && !std::path::Path::new(DEBUG_DEACTIVATE).exists()
}

pub fn set_debug_logging(on: bool) {
    if on {
        let _ = std::fs::write(DEBUG_ACTIVATE, b"1");
        let _ = std::fs::remove_file(DEBUG_DEACTIVATE);
    } else {
        let _ = std::fs::write(DEBUG_DEACTIVATE, b"1");
    }
}

/// Jorge FUN_7100124b24 — delete SD file if present; returns true when removed.
pub fn consume_sd_trigger(path: &str) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}
