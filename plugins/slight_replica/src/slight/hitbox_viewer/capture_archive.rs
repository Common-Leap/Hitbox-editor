//! Disk-backed ACMD capture history.
//!
//! The game thread only hands each already-deduplicated capture to this worker. The archive is
//! the complete session history; the plugin's live queues are delivery buffers, not the source of
//! truth. Keeping the file append and replay work off the game thread avoids trading a heap crash
//! for a filesystem stall.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::thread;

use super::{CaptureEnd, CaptureLine};

/// Host-visible through the emulator's SD root. This is intentionally one append-only JSONL file
/// rather than one file per line: per-message file creation was the filesystem hot path that
/// made the old debug fallback progressively slower during busy matches.
pub const ARCHIVE_FILE: &str = "sd:/slight/user/acmd_captures.jsonl";

#[derive(serde::Serialize)]
#[serde(tag = "type", content = "data")]
enum ArchiveRecord {
    #[serde(rename = "line")]
    Line(CaptureLine),
    #[serde(rename = "end")]
    End(CaptureEnd),
}

enum ArchiveCommand {
    Record(ArchiveRecord),
    Clear,
    PrepareReplay,
}

static WRITER: OnceLock<Sender<ArchiveCommand>> = OnceLock::new();
static REPLAY_REQUESTED: AtomicBool = AtomicBool::new(false);
static REPLAY_READY: AtomicBool = AtomicBool::new(false);
static REPLAY: LazyLock<Mutex<Option<BufReader<File>>>> = LazyLock::new(|| Mutex::new(None));

/// Start one writer for this plugin instance and begin a fresh capture session. A prior session's
/// file is left readable until the next boot, which is useful after a crash; the next boot then
/// starts a clean archive instead of replaying stale run ids into a new editor session.
pub fn init() {
    if WRITER.get().is_some() {
        return;
    }
    let _ = std::fs::create_dir_all("sd:/slight/user/");
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(ARCHIVE_FILE);

    let (tx, rx) = mpsc::channel();
    if WRITER.set(tx).is_err() {
        return;
    }
    thread::spawn(move || writer_loop(rx));
}

pub fn append_line(line: CaptureLine) {
    if let Some(writer) = WRITER.get() {
        let _ = writer.send(ArchiveCommand::Record(ArchiveRecord::Line(line)));
    }
}

pub fn append_end(end: CaptureEnd) {
    if let Some(writer) = WRITER.get() {
        let _ = writer.send(ArchiveCommand::Record(ArchiveRecord::End(end)));
    }
}

pub fn clear() {
    REPLAY_REQUESTED.store(false, Ordering::Release);
    REPLAY_READY.store(false, Ordering::Release);
    *REPLAY.lock().unwrap() = None;
    if let Some(writer) = WRITER.get() {
        let _ = writer.send(ArchiveCommand::Clear);
    }
}

/// Ask the writer to flush every record accepted before this request, then replay the resulting
/// file. New records arriving after the request continue through the ordinary live queue, so a
/// reconnect cannot lose the tail or require loading the whole archive into memory.
pub fn begin_replay() {
    *REPLAY.lock().unwrap() = None;
    REPLAY_READY.store(false, Ordering::Release);
    REPLAY_REQUESTED.store(true, Ordering::Release);
    if let Some(writer) = WRITER.get() {
        let _ = writer.send(ArchiveCommand::PrepareReplay);
    }
}

pub fn replay_active() -> bool {
    REPLAY_REQUESTED.load(Ordering::Acquire) || REPLAY.lock().unwrap().is_some()
}

/// Read at most `max` JSONL records. The returned strings are still archive records; the server
/// validates and wraps them into the normal `AcmdCapture` / `AcmdCaptureEnd` wire messages.
pub fn take_replay(max: usize) -> Vec<String> {
    if REPLAY_REQUESTED.load(Ordering::Acquire) && REPLAY_READY.swap(false, Ordering::AcqRel) {
        let reader = File::open(ARCHIVE_FILE).ok().map(BufReader::new);
        if reader.is_none() {
            REPLAY_REQUESTED.store(false, Ordering::Release);
        }
        *REPLAY.lock().unwrap() = reader;
    }

    let mut replay = REPLAY.lock().unwrap();
    let Some(reader) = replay.as_mut() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(max);
    while out.len() < max {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                *replay = None;
                REPLAY_REQUESTED.store(false, Ordering::Release);
                break;
            }
            Ok(_) => {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
            Err(_) => {
                *replay = None;
                REPLAY_REQUESTED.store(false, Ordering::Release);
                break;
            }
        }
    }
    out
}

fn writer_loop(rx: Receiver<ArchiveCommand>) {
    let mut file: Option<BufWriter<File>> = None;
    while let Ok(first) = rx.recv() {
        process(first, &mut file);
        while let Ok(next) = rx.try_recv() {
            process(next, &mut file);
        }
        if let Some(file) = file.as_mut() {
            let _ = file.flush();
        }
    }
}

fn process(command: ArchiveCommand, file: &mut Option<BufWriter<File>>) {
    match command {
        ArchiveCommand::Record(record) => {
            if file.is_none() {
                *file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ARCHIVE_FILE)
                    .ok()
                    .map(BufWriter::new);
            }
            if let Some(file) = file.as_mut() {
                if serde_json::to_writer(&mut *file, &record).is_ok() {
                    let _ = file.write_all(b"\n");
                }
            }
        }
        ArchiveCommand::Clear => {
            *file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(ARCHIVE_FILE)
                .ok()
                .map(BufWriter::new);
        }
        ArchiveCommand::PrepareReplay => {
            if let Some(file) = file.as_mut() {
                let _ = file.flush();
            }
            REPLAY_READY.store(true, Ordering::Release);
        }
    }
}
