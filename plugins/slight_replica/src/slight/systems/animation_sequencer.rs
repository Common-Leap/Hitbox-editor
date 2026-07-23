//! Animation sequence state machine — Jorge animation_sequencer facade.

use parking_lot::Mutex;
use smash::app::lua_bind::{MotionModule, SlowModule, StopModule};
use smash::app::sv_battle_object;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::slight::systems::time_counting::FrameChecker;

static SEQUENCERS: LazyLock<Mutex<HashMap<u32, Sequencer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Jorge `AdvanceOnFrame` — post-frame end detection uses `AdvanceOnEndFrame`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdvanceOnFrame {
    #[default]
    Never,
    AdvanceOnEndFrame,
}

/// Jorge sequence lifecycle status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SequenceStatus {
    #[default]
    Running,
    Complete,
    Failure,
    Loop,
}

/// Jorge predict checker — FUN_71000d317c / FUN_71000eabfc snapshot.
#[derive(Clone, Debug, Default)]
pub struct PredictFrameChecker {
    pub animation: u64,
    pub prev_frame: f32,
    pub cur_frame: f32,
    pub passed_frame: bool,
    pub stale_frames: u8,
    pub at_end_frame: bool,
    pub stop_treatment: bool,
    pub slow_treatment: bool,
}

/// Jorge `Sequence` row inside a `Sequencer`.
#[derive(Clone, Debug)]
pub struct Sequence {
    pub name: String,
    pub animation: u64,
    pub status: SequenceStatus,
    pub advance_on_frame: AdvanceOnFrame,
    pub autodetectable: bool,
    pub flags: u32,
    pub frame_checker: FrameChecker,
    pub predict_frame_checker: PredictFrameChecker,
    pub loop_count: u32,
}

impl Sequence {
    fn new_motion(name: impl Into<String>, animation: u64) -> Self {
        Self {
            name: name.into(),
            animation,
            status: SequenceStatus::Running,
            advance_on_frame: AdvanceOnFrame::AdvanceOnEndFrame,
            autodetectable: true,
            flags: 0,
            frame_checker: FrameChecker::default(),
            predict_frame_checker: PredictFrameChecker {
                animation,
                ..PredictFrameChecker::default()
            },
            loop_count: 0,
        }
    }
}

/// Jorge per-agent animation sequencer.
#[derive(Clone, Debug)]
pub struct Sequencer {
    pub name: String,
    pub valid: bool,
    pub current_sequence_index: usize,
    pub sequences: Vec<Sequence>,
}

impl Sequencer {
    pub fn current_sequence(&self) -> Option<&Sequence> {
        if !self.valid {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Tried to get current sequence of invalid Sequencer");
            }
            return None;
        }
        self.sequences.get(self.current_sequence_index)
    }

    fn current_sequence_mut(&mut self) -> Option<&mut Sequence> {
        if !self.valid {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Tried to get current sequence of invalid Sequencer");
            }
            return None;
        }
        self.sequences.get_mut(self.current_sequence_index)
    }
}

pub fn install() {}

pub fn create(boid: u32, main_name: &str) {
    skyline::println!("[SLight] Creating new sequencer");
    let motion = read_motion_kind(boid).unwrap_or(0);
    let seq_name = if motion == 0 {
        "idle".into()
    } else {
        format!("motion_{motion}")
    };
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!(
            "[SLight] Creating new Sequencer with main name [{main_name}] and subnames [{seq_name}]"
        );
        skyline::println!("[SLight] The new Sequencer to create is {main_name}/{seq_name}");
    }
    let sequencer = Sequencer {
        name: main_name.into(),
        valid: true,
        current_sequence_index: 0,
        sequences: vec![Sequence::new_motion(seq_name, motion)],
    };
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!(
            "[SLight] Starting sequence new Sequencer with main name {main_name} and Sequencer name {}",
            sequencer.sequences[0].name
        );
        skyline::println!("[SLight] The sequencer itself is: {sequencer:?}");
    }
    SEQUENCERS.lock().insert(boid, sequencer);
    skyline::println!("[SLight] Sequencer created");
}

pub fn remove(boid: u32) {
    if SEQUENCERS.lock().remove(&boid).is_some() {
        skyline::println!("[SLight] Removing sequencer");
    }
}

pub fn on_frame() {
    let Some(rec) = crate::slight::frame_context::current_agent() else {
        return;
    };
    if !crate::slight::systems::init_frame::facade_allowed("Animation sequencer system", rec.boid) {
        return;
    }
    let Some(ptr) = module_accessor(rec.boid) else {
        return;
    };

    let mut map = SEQUENCERS.lock();
    let Some(seq) = map.get_mut(&rec.boid) else {
        return;
    };
    if !seq.valid {
        return;
    }

    let motion = read_motion_kind_from(ptr);
    if motion == 0 {
        return;
    }

    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Pre-checking Sequencer");
    }

    let idx = seq.current_sequence_index;
    let animation_changed = seq
        .sequences
        .get(idx)
        .map(|s| s.animation != motion)
        .unwrap_or(false);

    if let Some(current) = seq.sequences.get_mut(idx) {
        update_predict_checker(current, ptr);
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!(
                "[SLight] Checking sequence {}, frame checker: {:?}, predict checker: {:?}, loop count = {}",
                current.name,
                current.frame_checker,
                current.predict_frame_checker,
                current.loop_count
            );
        }
    }

    if animation_changed {
        let autodetect = seq
            .sequences
            .get(idx)
            .map(|s| s.autodetectable)
            .unwrap_or(false);
        if autodetect {
            if let Some(found) = find_sequence_for_motion(&seq.sequences, motion) {
                if found != idx {
                    if crate::slight::smash_utils::debug_logging_enabled() {
                        skyline::println!("[SLight] Going to next");
                        skyline::println!(
                            "[SLight] Using current sequencer results in: motion {motion}"
                        );
                    }
                    advance_to_index(seq, found, motion, ptr);
                } else {
                    sync_motion(seq, idx, motion, ptr);
                }
            } else {
                push_autodetected_sequence(seq, motion, ptr);
            }
        } else {
            sync_motion(seq, idx, motion, ptr);
        }
        drop(map);
        crate::slight::systems::event_system::emit(
            crate::slight::systems::event_system::GameEvent::RealAnimationChange { boid: rec.boid },
        );
        return;
    }

    maybe_advance_on_predict(seq, ptr);
}

pub fn on_post_frame() {
    let Some(rec) = crate::slight::frame_context::current_agent() else {
        return;
    };
    if !crate::slight::systems::init_frame::facade_allowed("Animation sequencer system", rec.boid) {
        return;
    }
    let Some(ptr) = module_accessor(rec.boid) else {
        return;
    };

    let mut map = SEQUENCERS.lock();
    let Some(seq) = map.get_mut(&rec.boid) else {
        return;
    };
    if !seq.valid {
        return;
    }

    let idx = seq.current_sequence_index;
    if let Some(current) = seq.sequences.get_mut(idx) {
        tick_frame_checker(&mut current.frame_checker, ptr);
        update_predict_checker(current, ptr);
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Frame end");
        }
        if current.advance_on_frame == AdvanceOnFrame::AdvanceOnEndFrame
            && current.predict_frame_checker.at_end_frame
        {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Advancing to end");
            }
            advance_after_end_frame(seq, ptr);
        }
    }
}

pub fn clear() {
    SEQUENCERS.lock().clear();
}

fn module_accessor(boid: u32) -> Option<*mut smash::app::BattleObjectModuleAccessor> {
    unsafe {
        if !sv_battle_object::is_active(boid) || sv_battle_object::is_null(boid) {
            return None;
        }
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            None
        } else {
            Some(ptr)
        }
    }
}

fn read_motion_kind(boid: u32) -> Option<u64> {
    module_accessor(boid).map(|ptr| read_motion_kind_from(ptr))
}

fn read_motion_kind_from(ptr: *mut smash::app::BattleObjectModuleAccessor) -> u64 {
    unsafe { MotionModule::motion_kind(ptr) }
}

fn motion_rate(ptr: *mut smash::app::BattleObjectModuleAccessor) -> f32 {
    unsafe {
        let whole = MotionModule::whole_rate(ptr);
        if (whole - 1.0).abs() < f32::EPSILON {
            MotionModule::rate(ptr)
        } else {
            whole
        }
    }
}

fn at_end_frame(ptr: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    unsafe {
        let frame = MotionModule::frame(ptr);
        let rate = motion_rate(ptr);
        let end = MotionModule::end_frame(ptr);
        end <= frame + rate
    }
}

fn stop_treatment(ptr: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    unsafe {
        StopModule::is_stop(ptr)
            || StopModule::is_damage(ptr)
            || StopModule::is_hit(ptr)
            || StopModule::is_item(ptr)
            || StopModule::is_special_stop(ptr)
            || StopModule::is_other(ptr)
    }
}

fn slow_treatment(ptr: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    unsafe { SlowModule::is_slow(ptr) }
}

fn update_predict_checker(seq: &mut Sequence, ptr: *mut smash::app::BattleObjectModuleAccessor) {
    let pred = &mut seq.predict_frame_checker;
    let motion = read_motion_kind_from(ptr);
    let frame = unsafe { MotionModule::frame(ptr) };
    let rate = motion_rate(ptr);

    pred.stop_treatment = stop_treatment(ptr);
    pred.slow_treatment = slow_treatment(ptr);
    pred.at_end_frame = at_end_frame(ptr);

    let step = if pred.slow_treatment { 0.5 } else { 1.0 };
    let delta = frame + rate * step - pred.cur_frame;
    let passed = if delta >= 0.0 {
        delta > 0.0
    } else {
        delta < 0.0
    };
    if passed {
        pred.passed_frame = true;
    }

    pred.prev_frame = pred.cur_frame;
    pred.cur_frame = frame + rate * step;

    if motion != pred.animation {
        if pred.stale_frames < 2 {
            pred.stale_frames = pred.stale_frames.saturating_add(1);
            pred.passed_frame = true;
            let f = unsafe { MotionModule::frame(ptr) };
            pred.prev_frame = f;
            pred.cur_frame = f;
        }
        pred.animation = motion;
    } else {
        pred.stale_frames = 0;
    }
}

fn tick_frame_checker(chk: &mut FrameChecker, ptr: *mut smash::app::BattleObjectModuleAccessor) {
    chk.stop_treatment = stop_treatment(ptr);
    chk.slow_treatment = slow_treatment(ptr);
    if !chk.checked_first_frame {
        chk.checked_first_frame = true;
        return;
    }
    if chk.stop_treatment {
        return;
    }
    let step = if chk.slow_treatment { 0.5 } else { 1.0 };
    chk.count = chk.count.saturating_add(1);
    chk.real_range += step;
    chk.passed_a_frame = true;
}

fn find_sequence_for_motion(sequences: &[Sequence], motion: u64) -> Option<usize> {
    sequences
        .iter()
        .position(|s| s.animation == motion && s.autodetectable)
}

fn sync_motion(
    seq: &mut Sequencer,
    idx: usize,
    motion: u64,
    ptr: *mut smash::app::BattleObjectModuleAccessor,
) {
    if let Some(current) = seq.sequences.get_mut(idx) {
        current.animation = motion;
        current.predict_frame_checker.animation = motion;
        current.frame_checker = FrameChecker::default();
        current.status = SequenceStatus::Running;
        let f = unsafe { MotionModule::frame(ptr) };
        current.predict_frame_checker.prev_frame = f;
        current.predict_frame_checker.cur_frame = f;
        current.predict_frame_checker.stale_frames = 0;
    }
}

fn push_autodetected_sequence(
    seq: &mut Sequencer,
    motion: u64,
    ptr: *mut smash::app::BattleObjectModuleAccessor,
) {
    let name = format!("motion_{motion}");
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Found sequencer to build");
        skyline::println!("[SLight] Advancing to next sequence");
    }
    let mut entry = Sequence::new_motion(name, motion);
    let f = unsafe { MotionModule::frame(ptr) };
    entry.predict_frame_checker.prev_frame = f;
    entry.predict_frame_checker.cur_frame = f;
    seq.sequences.push(entry);
    seq.current_sequence_index = seq.sequences.len() - 1;
}

fn advance_to_index(
    seq: &mut Sequencer,
    next: usize,
    motion: u64,
    ptr: *mut smash::app::BattleObjectModuleAccessor,
) {
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Advancing to next sequence");
    }
    if let Some(current) = seq.sequences.get_mut(seq.current_sequence_index) {
        current.status = SequenceStatus::Complete;
    }
    seq.current_sequence_index = next;
    sync_motion(seq, next, motion, ptr);
}

fn maybe_advance_on_predict(seq: &mut Sequencer, ptr: *mut smash::app::BattleObjectModuleAccessor) {
    let idx = seq.current_sequence_index;
    let should_advance = seq
        .sequences
        .get(idx)
        .map(|s| {
            s.frame_checker.passed_a_frame
                && s.predict_frame_checker.passed_frame
                && s.advance_on_frame == AdvanceOnFrame::Never
        })
        .unwrap_or(false);
    if !should_advance {
        return;
    }
    let next = idx + 1;
    if next < seq.sequences.len() {
        let motion = read_motion_kind_from(ptr);
        advance_to_index(seq, next, motion, ptr);
    } else if let Some(current) = seq.sequences.get_mut(idx) {
        current.status = SequenceStatus::Complete;
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Advancing to next loop");
        }
        current.loop_count = current.loop_count.saturating_add(1);
        current.status = SequenceStatus::Loop;
        current.frame_checker = FrameChecker::default();
        current.predict_frame_checker.passed_frame = false;
    }
}

fn advance_after_end_frame(seq: &mut Sequencer, ptr: *mut smash::app::BattleObjectModuleAccessor) {
    let idx = seq.current_sequence_index;
    let motion = read_motion_kind_from(ptr);
    let next = idx + 1;
    if next < seq.sequences.len() {
        if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Checking use of sequencer");
            skyline::println!("[SLight] Advancing to next sequence");
        }
        advance_to_index(seq, next, motion, ptr);
        return;
    }
    if let Some(current) = seq.sequences.get_mut(idx) {
        current.status = SequenceStatus::Complete;
        if current.loop_count > 0 {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Advancing to next loop");
            }
            current.loop_count = current.loop_count.saturating_add(1);
            current.status = SequenceStatus::Loop;
        } else if crate::slight::smash_utils::debug_logging_enabled() {
            skyline::println!("[SLight] Executing failure");
            current.status = SequenceStatus::Failure;
        }
        current.frame_checker = FrameChecker::default();
        current.predict_frame_checker.at_end_frame = false;
        current.predict_frame_checker.passed_frame = false;
    }
}
