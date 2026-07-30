//! Win / results screen detection — Jorge event_system final anim + win status (SD-tunable).

use parking_lot::Mutex;
use std::sync::LazyLock;

pub const WIN_DETECT_FILE: &str = "sd:/slight/user/win_detect.txt";

#[derive(Clone, Debug)]
struct WinDetectConfig {
    final_motions: Vec<u64>,
    win_statuses: Vec<i32>,
    file_mtime: Option<u64>,
}

impl Default for WinDetectConfig {
    fn default() -> Self {
        Self {
            // Smash 13.0.x defaults — override via win_detect.txt per game version.
            final_motions: vec![0x62, 0x63],
            win_statuses: vec![0x24],
            file_mtime: None,
        }
    }
}

static CONFIG: LazyLock<Mutex<WinDetectConfig>> =
    LazyLock::new(|| Mutex::new(WinDetectConfig::default()));

pub fn install() {
    poll_sd();
}

pub fn is_final_motion(motion: u64) -> bool {
    CONFIG.lock().final_motions.contains(&motion)
}

pub fn is_win_status(status: i32) -> bool {
    CONFIG.lock().win_statuses.contains(&status)
}

/// Re-read `win_detect.txt` if it changed. Driven by the throttled SD tick
/// (`slight::sd_poll`) — this used to run every frame, and the file normally does not exist,
/// so it was one failing stat per frame on the game thread.
pub fn poll_sd() {
    let path = std::path::Path::new(WIN_DETECT_FILE);
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let Some(modified) = modified else {
        return;
    };
    {
        let mut cfg = CONFIG.lock();
        if cfg.file_mtime == Some(modified) {
            return;
        }
        cfg.file_mtime = Some(modified);
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut motions = Vec::new();
    let mut statuses = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        match key.as_str() {
            "final_motion" | "final_motions" => {
                motions.extend(parse_hex_list(val));
            }
            "win_status" | "win_statuses" => {
                statuses.extend(parse_int_list(val));
            }
            _ => {}
        }
    }
    let mut cfg = CONFIG.lock();
    if !motions.is_empty() {
        cfg.final_motions = motions;
    }
    if !statuses.is_empty() {
        cfg.win_statuses = statuses;
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!(
            "[SLight] Reloaded win_detect: motions={:?} statuses={:?}",
            cfg.final_motions,
            cfg.win_statuses
        );
    }
}

fn parse_hex_list(s: &str) -> Vec<u64> {
    s.split(',')
        .filter_map(|p| {
            let p = p.trim();
            if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).ok()
            } else {
                p.parse::<u64>().ok()
            }
        })
        .collect()
}

fn parse_int_list(s: &str) -> Vec<i32> {
    s.split(',')
        .filter_map(|p| {
            let p = p.trim();
            if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
                i32::from_str_radix(h, 16).ok()
            } else {
                p.parse::<i32>().ok()
            }
        })
        .collect()
}

pub fn clear() {
    *CONFIG.lock() = WinDetectConfig::default();
}
