//! Runtime diagnostics — appends to `sd:/slight/diag.txt`, which is host-readable *live*
//! (emulator log buffers can freeze on entering gameplay, so skyline `println!` is unreliable
//! in-match). One session answers: does the per-frame driver fire, is each requested effect
//! captured or dropped (and why), does the tracker stay bounded, and do RPM edits apply.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

const DIAG_FILE: &str = "sd:/slight/diag.txt";

static BUF: Mutex<Vec<String>> = Mutex::new(Vec::new());

// Cumulative counters (since boot).
static SPAWNS: AtomicU64 = AtomicU64::new(0);
static SPAWNS_FIGHTER: AtomicU64 = AtomicU64::new(0); // category == 0
static SPAWNS_FOLLOW: AtomicU64 = AtomicU64::new(0);
static SYNTH: AtomicU64 = AtomicU64::new(0); // handle==0 → synthetic pseudo-handle
static NEWS: AtomicU64 = AtomicU64::new(0);
static DEDUPS: AtomicU64 = AtomicU64::new(0);
static RESHOWS: AtomicU64 = AtomicU64::new(0); // slot reuse with changed hash → re-shown

// Reconcile (per-frame cleanup) cumulative counters.
static REC_GONE: AtomicU64 = AtomicU64::new(0); // is_exist_effect said gone
static REC_DEAD_ACCESSOR: AtomicU64 = AtomicU64::new(0); // owning agent no longer live
static REC_EXPIRED: AtomicU64 = AtomicU64::new(0); // synthetic TTL ran out

// Pending flush + edit-apply counters.
static FLUSHED: AtomicU64 = AtomicU64::new(0);
static EDITS_OK: AtomicU64 = AtomicU64::new(0);
static EDITS_FAIL: AtomicU64 = AtomicU64::new(0);

/// Start a bounded diagnostic session. This file used to append across every game boot and grew
/// into hundreds of megabytes, turning routine effect edits into emulator filesystem work.
pub fn start_session() {
    if let Some(mut buffer) = BUF.try_lock() {
        buffer.clear();
    }
    let _ = std::fs::write(DIAG_FILE, "SLight diagnostic session\n");
}

fn push(line: String) {
    // try_lock, never park: this buffer is written from BOTH the game thread and the
    // server/sender threads, and parked lock-waiters never wake in this environment
    // (the std::thread::sleep bug class). Dropping a diag line under contention is fine;
    // freezing the game thread is not.
    let Some(mut b) = BUF.try_lock() else {
        return;
    };
    if b.len() < 40000 {
        b.push(line);
    }
}

/// Free-form diagnostic line (flushed with the rest by the per-frame driver).
pub fn note(line: impl Into<String>) {
    push(line.into());
}

/// Logged at the very top of `track_spawn`, before dedup — so it captures EVERY effect the
/// game requests, regardless of whether we end up showing it.
pub fn note_spawn(name: &str, is_follow: bool, handle: u32, category: i32, status_kind: i32) {
    SPAWNS.fetch_add(1, Ordering::Relaxed);
    if category == 0 {
        SPAWNS_FIGHTER.fetch_add(1, Ordering::Relaxed);
    }
    if is_follow {
        SPAWNS_FOLLOW.fetch_add(1, Ordering::Relaxed);
    }
    if handle == 0 {
        SYNTH.fetch_add(1, Ordering::Relaxed);
    }
    push(format!(
        "SPAWN {name} follow={} h=0x{handle:x} cat={category} st={status_kind}",
        is_follow as u8
    ));
}

/// Logged after `upsert_spawn` returns, so we can see the new / re-shown / deduped split.
pub fn note_result(is_new: bool, reshow: bool) {
    if is_new {
        if reshow {
            RESHOWS.fetch_add(1, Ordering::Relaxed);
        } else {
            NEWS.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        DEDUPS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-frame reconcile outcome (only counted; a line is emitted with STATS).
pub fn note_reconcile(gone: u64, dead_accessor: u64, expired: u64) {
    REC_GONE.fetch_add(gone, Ordering::Relaxed);
    REC_DEAD_ACCESSOR.fetch_add(dead_accessor, Ordering::Relaxed);
    REC_EXPIRED.fetch_add(expired, Ordering::Relaxed);
}

/// Jobs drained by `pending::process` this tick.
pub fn note_flush(drained: u64) {
    FLUSHED.fetch_add(drained, Ordering::Relaxed);
}

/// One RPM edit processed: parsed id, whether the tracked target was found, whether the live
/// effect still existed, and whether setters were applied.
pub fn note_edit(id: u64, found: bool, exists: bool, applied: bool) {
    if applied {
        EDITS_OK.fetch_add(1, Ordering::Relaxed);
    } else {
        EDITS_FAIL.fetch_add(1, Ordering::Relaxed);
    }
    push(format!(
        "EDIT id={id} found={} exists={} applied={}",
        found as u8, exists as u8, applied as u8
    ));
}

/// Periodic stats line (call from the per-frame driver). `pending_depth` growing = flush
/// starvation; `tracker` growing without bound = the leak.
pub fn note_stats(
    frame: u64,
    tracker_count: usize,
    pending_depth: usize,
    outbox_depth: usize,
    live_fighters: usize,
    live_weapons: usize,
) {
    push(format!(
        "STATS frame={frame} tracker={tracker_count} pend={pending_depth} outbox={outbox_depth} \
         agents={live_fighters}f/{live_weapons}w \
         spawns={} (fighter={} follow={} synth={}) new={} reshow={} dedup={} \
         rec_gone={} rec_dead={} rec_ttl={} flushed={} edits={}ok/{}fail",
        SPAWNS.load(Ordering::Relaxed),
        SPAWNS_FIGHTER.load(Ordering::Relaxed),
        SPAWNS_FOLLOW.load(Ordering::Relaxed),
        SYNTH.load(Ordering::Relaxed),
        NEWS.load(Ordering::Relaxed),
        RESHOWS.load(Ordering::Relaxed),
        DEDUPS.load(Ordering::Relaxed),
        REC_GONE.load(Ordering::Relaxed),
        REC_DEAD_ACCESSOR.load(Ordering::Relaxed),
        REC_EXPIRED.load(Ordering::Relaxed),
        FLUSHED.load(Ordering::Relaxed),
        EDITS_OK.load(Ordering::Relaxed),
        EDITS_FAIL.load(Ordering::Relaxed),
    ));
}

/// Append the buffer to the SD file. Call every N frames (not every frame — file I/O).
pub fn flush() {
    let lines: Vec<String> = {
        // try_lock (see push) — skip this flush cycle rather than risk parking forever.
        let Some(mut b) = BUF.try_lock() else {
            return;
        };
        if b.is_empty() {
            return;
        }
        std::mem::take(&mut *b)
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(DIAG_FILE)
    {
        for l in lines {
            let _ = writeln!(f, "{l}");
        }
    }
}
