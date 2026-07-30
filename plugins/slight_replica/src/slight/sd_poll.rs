//! The plugin's one and only SD-card polling tick.
//!
//! Several subsystems watch `sd:/slight/user/` for files a user drops there by hand: excommand
//! scripts, `win_detect.txt`, and the `activate.txt` / `deactivate.txt` debug triggers. Each of
//! those used to be checked on EVERY game frame, and every check normally misses, because the
//! files are not there.
//!
//! On Linux a repeated miss is answered from the negative dentry cache in about a microsecond,
//! so a hundred of them per frame cost nothing and the design looked fine. Windows has no
//! equivalent negative-lookup cache: each miss is a full `NtCreateFile` path parse and
//! directory-index probe, routed through the emulator's sdmc VFS, with Defender's real-time
//! scanner hooking the open. That is tens to hundreds of microseconds each. The same frame that
//! costs ~0.1 ms of filesystem time on Linux cost 5-20 ms on Windows, against a 16.6 ms budget
//! — the entire reason Windows testers saw ~10 fps.
//!
//! So they all live here now, on one throttled tick driven by the per-frame driver's existing
//! 30-frame cadence gate. Hand-dropped files are picked up within about half a second instead
//! of within one frame, which is immaterial for what these are used for.

/// One SD polling pass. Call from the throttled driver tick, never per frame.
///
/// Order matters and must not be shuffled: `refresh_debug_logging` READS `activate.txt` /
/// `deactivate.txt`, and `poll_after_win_triggers` DELETES those same files as one-shot
/// triggers. Probing before consuming is the order these two had when they ran on separate
/// per-frame paths, and it keeps the observable behaviour identical.
pub fn tick() {
    crate::slight::smash_utils::refresh_debug_logging();
    crate::slight::frame_context::poll_after_win_triggers();
    crate::slight::systems::win_screen::poll_sd();
    crate::slight::systems::excommand::poll_sd();
}
