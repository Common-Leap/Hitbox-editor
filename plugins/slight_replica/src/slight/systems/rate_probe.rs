//! One-shot measurement for E2: which way the `FT_MOTION_RATE` multiplier goes.
//!
//! **This is a probe, not a feature.** It exists to settle a fact the editor cannot establish
//! offline, and it should be deleted once the answer is in the backlog entry.
//!
//! ## The question
//!
//! `FT_MOTION_RATE(agent, 0.25)` means one of two things and the readings give opposite answers:
//!
//! - *Rate is playback speed*, so `game = motion / rate`. Motion advances by `rate` each game
//!   frame — 0.25 motion frames per game frame, i.e. quarter speed.
//! - *Rate is game frames per motion frame*, so `game = rate × motion`. Motion advances by
//!   `1 / rate` each game frame — **four** motion frames per game frame.
//!
//! Under the first, Kirby's jab 1 hitbox lands on game frame 5; under the second, frame 2.
//!
//! ## Why it can be settled by watching one number
//!
//! Both readings are statements about how far `MotionModule::frame` moves in one game frame. So
//! sampling it on consecutive frames during a rate-carrying move distinguishes them outright, and
//! at rate 0.25 the two predictions differ by 16×. No hitbox timing or script correlation is
//! needed.
//!
//! The plugin already has a standing prediction: `animation_sequencer::at_end_frame` computes
//! `end <= frame + rate`, and `update_predict_checker` steps by `frame + rate * step`. Both are
//! working code that treats rate as *motion frames advanced per game frame* — the first reading.
//! What they do not establish is whether the number `MotionModule::rate` returns is the argument
//! `FT_MOTION_RATE` was given, which is the actual open question. So this logs both.
//!
//! ## What it writes
//!
//! Nothing at all until a rate away from 1.0 is seen, then at most [`MAX_SAMPLES`] lines. Kirby's
//! down smash (`attack_lw4`, `FT_MOTION_RATE(agent, 0.25)`) is the intended probe; it is the
//! largest separation in the corpus.

use smash::app::lua_bind::MotionModule;
use smash::app::BattleObjectModuleAccessor;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Enough samples to see the step and its recovery, few enough to read by eye.
const MAX_SAMPLES: u32 = 40;

/// A rate this close to 1.0 is the default and says nothing.
const RATE_EPSILON: f32 = 0.01;

static SAMPLES: AtomicU32 = AtomicU32::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
static LAST_FRAME: AtomicU64 = AtomicU64::new(0);
static LAST_MOTION: AtomicU64 = AtomicU64::new(0);

/// Per-frame entry point. Resolves the current agent itself rather than taking one.
///
/// Deliberately *not* hung off the animation sequencer's `on_frame`, which is gated on a
/// registered sequencer and on `facade_allowed`. A probe that silently does not run because an
/// unrelated subsystem declined is the failure this whole measurement has already hit twice.
pub fn on_frame() {
    if SAMPLES.load(Ordering::Relaxed) >= MAX_SAMPLES {
        return;
    }
    let Some(rec) = crate::slight::frame_context::current_agent() else {
        return;
    };
    let ptr = unsafe {
        if !smash::app::sv_battle_object::is_active(rec.boid)
            || smash::app::sv_battle_object::is_null(rec.boid)
        {
            return;
        }
        smash::app::sv_battle_object::module_accessor(rec.boid)
    };
    tick(ptr);
}

/// Sample one fighter's motion frame for the E2 measurement.
///
/// Does no I/O — `diag::note` buffers — and does nothing at all once [`MAX_SAMPLES`] lines have
/// been written, so it cannot run away during a long session.
pub fn tick(boma: *mut BattleObjectModuleAccessor) {
    if boma.is_null() || SAMPLES.load(Ordering::Relaxed) >= MAX_SAMPLES {
        return;
    }
    let (motion, frame, rate, whole) = unsafe {
        (
            MotionModule::motion_kind(boma),
            MotionModule::frame(boma),
            MotionModule::rate(boma),
            MotionModule::whole_rate(boma),
        )
    };

    // Arm on the first off-default rate seen, and stay armed: the interesting part is not only
    // the slowed span but the frame it returns to 1.0, because that is where a wrong reading
    // would show a discontinuity rather than a smooth resume.
    let off_default =
        (rate - 1.0).abs() > RATE_EPSILON || (whole - 1.0).abs() > RATE_EPSILON;
    if !ARMED.load(Ordering::Relaxed) {
        if !off_default {
            LAST_FRAME.store(frame.to_bits() as u64, Ordering::Relaxed);
            LAST_MOTION.store(motion, Ordering::Relaxed);
            return;
        }
        ARMED.store(true, Ordering::Relaxed);
        crate::slight::diag::note(
            "RATE probe armed — E2. delta is how far MotionModule::frame moved in ONE game frame.",
        );
        crate::slight::diag::note(
            "RATE  reading A (rate is playback speed): delta == rate. \
             reading B (rate is game frames per motion frame): delta == 1/rate.",
        );
    }

    let prev_motion = LAST_MOTION.swap(motion, Ordering::Relaxed);
    let prev_bits = LAST_FRAME.swap(frame.to_bits() as u64, Ordering::Relaxed);
    let prev = f32::from_bits(prev_bits as u32);
    // A motion change resets the frame counter, so the delta across that boundary is meaningless
    // and would read as a large negative step.
    if prev_motion != motion {
        return;
    }
    let delta = frame - prev;
    let n = SAMPLES.fetch_add(1, Ordering::Relaxed);
    crate::slight::diag::note(format!(
        "RATE {n:02} motion={motion:#x} frame={frame:.4} delta={delta:.4} \
         rate={rate:.4} whole={whole:.4} => A_predicts={rate:.4} B_predicts={:.4}",
        if rate.abs() > f32::EPSILON {
            1.0 / rate
        } else {
            f32::INFINITY
        }
    ));
    if n + 1 == MAX_SAMPLES {
        crate::slight::diag::note(
            "RATE probe done — compare delta against A_predicts and B_predicts above.",
        );
        crate::slight::diag::flush();
    }
}
