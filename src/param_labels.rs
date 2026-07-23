//! Runtime param-label download from ultimate-research/param-labels.
//!
//! Replaces the old requirement of a `ParamLabels.csv` / `Labels.txt` inside the game
//! data (export) folder: at startup a background thread loads the cached copy from app
//! storage instantly, then checks GitHub for updates via ETag and re-downloads only when
//! the repo actually changed. Results arrive over an mpsc channel and are merged into
//! `state.labels` (hash40 → label).

use std::collections::HashMap;
use std::path::PathBuf;

const CSV_URL: &str =
    "https://raw.githubusercontent.com/ultimate-research/param-labels/master/ParamLabels.csv";

pub enum Msg {
    /// A full label map is ready (from cache or a fresh download).
    Loaded { labels: HashMap<u64, String> },
    /// Progress/result note for the status bar.
    Status(String),
}

fn cache_csv() -> PathBuf {
    crate::scratch_dirs::app_storage_root().join("ParamLabels.csv")
}

fn cache_etag() -> PathBuf {
    crate::scratch_dirs::app_storage_root().join("ParamLabels.etag")
}

/// Parse `0xHASH,label` lines (the repo format; hash may also be bare hex).
pub fn parse_csv(content: &str) -> HashMap<u64, String> {
    let mut labels = HashMap::new();
    for line in content.lines() {
        let mut parts = line.splitn(2, ',');
        if let (Some(hex), Some(label)) = (parts.next(), parts.next()) {
            let hex = hex.trim();
            let hex = hex.strip_prefix("0x").unwrap_or(hex);
            if let Ok(val) = u64::from_str_radix(hex, 16) {
                let label = label.trim();
                if !label.is_empty() {
                    labels.insert(val, label.to_string());
                }
            }
        }
    }
    labels
}

/// Start the background load + update check; poll the receiver each UI frame.
pub fn spawn_fetch() -> std::sync::mpsc::Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 1. Serve the cached copy immediately (startup never waits on the network).
        let cached = std::fs::read_to_string(cache_csv()).ok();
        if let Some(content) = &cached {
            let labels = parse_csv(content);
            if !labels.is_empty() {
                let n = labels.len();
                let _ = tx.send(Msg::Loaded { labels });
                let _ = tx.send(Msg::Status(format!("Param labels: {n} cached")));
            }
        }

        // 2. Update check: conditional GET — GitHub answers 304 when nothing changed.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let Ok(client) = client else { return };
        let mut req = client.get(CSV_URL);
        if cached.is_some() {
            if let Ok(etag) = std::fs::read_to_string(cache_etag()) {
                let etag = etag.trim();
                if !etag.is_empty() {
                    req = req.header(reqwest::header::IF_NONE_MATCH, etag);
                }
            }
        }
        match req.send() {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                let _ = tx.send(Msg::Status("Param labels are up to date".into()));
            }
            Ok(resp) if resp.status().is_success() => {
                let etag = resp
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                match resp.text() {
                    Ok(content) => {
                        let labels = parse_csv(&content);
                        if labels.is_empty() {
                            let _ = tx.send(Msg::Status(
                                "Param labels download parsed to 0 labels — kept cache".into(),
                            ));
                            return;
                        }
                        let _ = std::fs::write(cache_csv(), &content);
                        match etag {
                            Some(e) => {
                                let _ = std::fs::write(cache_etag(), e);
                            }
                            None => {
                                let _ = std::fs::remove_file(cache_etag());
                            }
                        }
                        let n = labels.len();
                        let _ = tx.send(Msg::Loaded { labels });
                        let _ = tx.send(Msg::Status(format!(
                            "Param labels updated from GitHub ({n} labels)"
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Status(format!("Param labels read failed: {e}")));
                    }
                }
            }
            Ok(resp) => {
                let _ = tx.send(Msg::Status(format!(
                    "Param labels download failed: HTTP {}",
                    resp.status()
                )));
            }
            Err(e) => {
                // Offline is fine — the cache (if any) is already serving.
                let _ = tx.send(Msg::Status(format!("Param labels: offline ({e})")));
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_repo_csv_lines() {
        let csv = "0x0000000000,\n0x00aa8bcd06,attack_air_n\n0x112233,with,comma\nnothex,line\n";
        let labels = super::parse_csv(csv);
        assert_eq!(
            labels.get(&0x00aa8bcd06).map(String::as_str),
            Some("attack_air_n")
        );
        // Label keeps everything after the first comma.
        assert_eq!(
            labels.get(&0x112233).map(String::as_str),
            Some("with,comma")
        );
        // Empty labels and unparsable hashes are skipped.
        assert_eq!(labels.len(), 2);
    }
}
