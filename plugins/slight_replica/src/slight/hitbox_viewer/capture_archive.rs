//! Disk-backed ACMD capture history.
//!
//! The game thread only hands each already-deduplicated capture to this worker. The archive is
//! the complete session history; the plugin's live queues are delivery buffers, not the source of
//! truth. Keeping the file append and replay work off the game thread avoids trading a heap crash
//! for a filesystem stall.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread;

use parking_lot::Mutex as ParkingMutex;

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

/// Hook workers must never enter std mpsc's internal wait path. A fixed queue with `try_lock`
/// makes archive delivery explicitly lossy under contention instead of parking the match.
const MAX_QUEUED: usize = 8192;
static STARTED: AtomicBool = AtomicBool::new(false);
static COMMANDS: ParkingMutex<VecDeque<ArchiveCommand>> = ParkingMutex::new(VecDeque::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);
static REPLAY_REQUESTED: AtomicBool = AtomicBool::new(false);
static REPLAY_READY: AtomicBool = AtomicBool::new(false);
static REPLAY: LazyLock<Mutex<Option<BufReader<File>>>> = LazyLock::new(|| Mutex::new(None));

/// Start one writer for this plugin instance and begin a fresh capture session. A prior session's
/// file is left readable until the next boot, which is useful after a crash; the next boot then
/// starts a clean archive instead of replaying stale run ids into a new editor session.
pub fn init() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::fs::create_dir_all("sd:/slight/user/");
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(ARCHIVE_FILE);

    thread::spawn(writer_loop);
}

pub fn append_line(line: CaptureLine) {
    if !enqueue(ArchiveCommand::Record(ArchiveRecord::Line(line))) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn append_end(end: CaptureEnd) {
    if !enqueue(ArchiveCommand::Record(ArchiveRecord::End(end))) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn clear() {
    REPLAY_REQUESTED.store(false, Ordering::Release);
    REPLAY_READY.store(false, Ordering::Release);
    *REPLAY.lock().unwrap() = None;
    let _ = enqueue(ArchiveCommand::Clear);
}

/// Ask the writer to flush every record accepted before this request, then replay the resulting
/// file. New records arriving after the request continue through the ordinary live queue, so a
/// reconnect cannot lose the tail or require loading the whole archive into memory.
pub fn begin_replay() {
    *REPLAY.lock().unwrap() = None;
    REPLAY_READY.store(false, Ordering::Release);
    REPLAY_REQUESTED.store(true, Ordering::Release);
    let _ = enqueue(ArchiveCommand::PrepareReplay);
}

pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
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

fn enqueue(command: ArchiveCommand) -> bool {
    let Some(mut commands) = COMMANDS.try_lock() else {
        return false;
    };
    if commands.len() >= MAX_QUEUED {
        return false;
    }
    commands.push_back(command);
    true
}

fn sleep_ms(ms: u64) {
    unsafe {
        nnsdk::nn::os::SleepThread(nnsdk::nn::TimeSpan {
            nanoseconds: ms * 1_000_000,
        });
    }
}

fn writer_loop() {
    let mut file: Option<BufWriter<File>> = None;
    loop {
        let batch: Vec<ArchiveCommand> = match COMMANDS.try_lock() {
            Some(mut commands) => {
                let count = commands.len().min(256);
                commands.drain(..count).collect()
            }
            None => Vec::new(),
        };
        if batch.is_empty() {
            sleep_ms(4);
            continue;
        }
        for command in batch {
            process(command, &mut file);
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
