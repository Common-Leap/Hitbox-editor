//! Overload time measurer — Jorge "Overload time measurer" facade.
//!
//! Reverse-engineered: the original (`FUN_71000bf6ac`) wraps each facade phase with
//! `nn::os::GetSystemTick()` and reports `"<phase> did an overload of <ticks> for facade <name>"`
//! when a phase exceeds a time budget (binary strings: "Pre-frame () did an overload of ",
//! "Init frame did an overload of  for facade ", "Complete frame of  did an overload of "). It is
//! a per-facade execution-time profiler — NOT a collision timer (the replica's earlier
//! collision-delta interpretation was wrong, and its output was never consumed).

use skyline::nn;

/// System-tick budget above which a facade phase is reported as an overload. The exact original
/// threshold is not recoverable from the decomp; this is a conservative default (SSBU's
/// `GetSystemTick` runs at ~19.2 MHz, so ~10k ticks ≈ 0.5 ms).
const OVERLOAD_THRESHOLD_TICKS: u64 = 10_000;

pub fn install() {}

pub fn clear() {}

/// Run a facade phase `f` under the timer; if it exceeds the budget, log the overload (debug-only,
/// matching the original's behavior).
pub fn time_phase(phase: &str, facade_name: &str, f: impl FnOnce()) {
    let start = unsafe { nn::os::GetSystemTick() };
    f();
    let elapsed = unsafe { nn::os::GetSystemTick() }.wrapping_sub(start);
    if elapsed > OVERLOAD_THRESHOLD_TICKS {
        // Route to diag (host-readable live) — skyline println is invisible under emulators.
        crate::slight::diag::note(format!("OVL {phase} {facade_name} ticks={elapsed}"));
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!(
                "[SLight] {phase} did an overload of {elapsed} for facade {facade_name}"
            );
        }
    }
}
