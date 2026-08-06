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

/// The move being measured. Everything else is ignored outright.
///
/// **The second run of this probe spent its whole budget on `run` and `walk_middle`.** Arming on
/// "any rate away from 1.0" looked reasonable and is not: the engine drives `MotionModule::rate`
/// continuously during locomotion — measured values from 0.73 to 3.17 while simply moving — so
/// an off-default rate is the *normal* state for a walking fighter and says nothing about
/// `FT_MOTION_RATE`. Exactly one of ~300 logged lines was the move the entry is about.
const TARGET_MOTION: &str = "attack_lw4";

static SAMPLES: AtomicU32 = AtomicU32::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
static LAST_FRAME: AtomicU64 = AtomicU64::new(0);
static LAST_MOTION: AtomicU64 = AtomicU64::new(0);
/// The battle object this probe locked onto — the first one seen in [`TARGET_MOTION`].
///
/// **A single shared "previous sample" slot is not enough, and that was the third run's bug.**
/// `current_agent()` alternates between the fighters on screen, so every other `tick` belonged
/// to a different agent and overwrote the slot. Every logged line then reported a boid change
/// and refused to compute a delta — 300 lines of "no delta yet", all of them for `boid=0`,
/// because the *intervening* ticks were the other fighter. Locking to one boid removes the
/// interleaving instead of trying to detect it.
static PROBE_BOID: AtomicU32 = AtomicU32::new(u32::MAX);

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
    tick(rec.boid, ptr);
}

/// Sample one fighter's motion frame for the E2 measurement.
///
/// Does no I/O — `diag::note` buffers — and does nothing at all once [`MAX_SAMPLES`] lines have
/// been written, so it cannot run away during a long session.
pub fn tick(boid: u32, boma: *mut BattleObjectModuleAccessor) {
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

    // Only the move under test. See [`TARGET_MOTION`] for why "any off-default rate" was the
    // wrong trigger — locomotion runs at a continuously varying rate as a matter of course.
    if motion != smash::phx::Hash40::new(TARGET_MOTION).hash {
        return;
    }

    // Lock to the first agent seen performing it, so the other fighter's ticks cannot interleave.
    let locked = PROBE_BOID.load(Ordering::Relaxed);
    if locked == u32::MAX {
        PROBE_BOID.store(boid, Ordering::Relaxed);
    } else if locked != boid {
        return;
    }

    if !ARMED.swap(true, Ordering::Relaxed) {
        crate::slight::diag::note(format!(
            "RATE probe armed on {TARGET_MOTION} boid={boid} — E2. delta is how far \
             MotionModule::frame moved in ONE game frame."
        ));
        crate::slight::diag::note(
            "RATE  reading A (rate is playback speed): delta == the FT_MOTION_RATE argument. \
             reading B (game frames per motion frame): delta == 1/argument.",
        );
        crate::slight::diag::note(
            "RATE  NOTE MotionModule::rate is NOT the macro argument — it reads 1.0 here while \
             the script sets 0.25. The delta is the measurement; rate= is context only.",
        );
    }

    let prev_motion = LAST_MOTION.swap(motion, Ordering::Relaxed);
    let prev_bits = LAST_FRAME.swap(frame.to_bits() as u64, Ordering::Relaxed);
    // Every frame of the move is logged, not only the off-default ones: the whole point is to
    // see the frame advance while the script's rate is in force, and the rate the *macro* set is
    // not visible in `MotionModule::rate` at all.
    if prev_motion != motion {
        crate::slight::diag::note(format!(
            "RATE -- {TARGET_MOTION} started, frame={frame:.4} (no delta yet)"
        ));
        return;
    }
    let prev = f32::from_bits(prev_bits as u32);
    let delta = frame - prev;
    let n = SAMPLES.fetch_add(1, Ordering::Relaxed);
    crate::slight::diag::note(format!(
        "RATE {n:02} boid={boid} motion={motion:#x} frame={frame:.4} delta={delta:.4} \
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
