//! Collision / attack hit log — Jorge skyline_hook facade (FUN_71000d1fb4).

use parking_lot::Mutex;
use skyline::hooks::{getRegionAddress, A64HookFunction, Region};
use skyline::libc;
use std::collections::VecDeque;
use std::sync::LazyLock;

static INSTALLED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static HITS: LazyLock<Mutex<VecDeque<HitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));

/// Whether to install the manual inline collision hook. Off: it null-jumps under Eden's JIT
/// (the trampoline is bad) and only feeds the damage log. Re-enable for real hardware.
const ENABLE_INLINE_COLLISION_HOOK: bool = false;

/// 40-byte ARM64 pattern scanned in game `.text` @ FUN_71000d1fb4.
const COLLISION_PATTERN: [u8; 40] = [
    0xff, 0x03, 0x03, 0xd1, 0xe8, 0x2b, 0x00, 0xfd, 0xfc, 0x6f, 0x06, 0xa9, 0xfa, 0x67, 0x07, 0xa9,
    0xf8, 0x5f, 0x08, 0xa9, 0xf6, 0x57, 0x09, 0xa9, 0xf4, 0x4f, 0x0a, 0xa9, 0xfd, 0x7b, 0x0b, 0xa9,
    0xfd, 0xc3, 0x02, 0x91, 0xfb, 0x03, 0x00, 0xaa,
];

/// Hook replacement — Jorge `notify_log_event_collision_hit` (6 args).
type CollisionHitHook = unsafe extern "C" fn(u32, u64, u32, u32, u32, u32) -> u64;

/// Original game function — Jorge calls trampoline with **5** args (no param_1).
type CollisionHitTrampoline = unsafe extern "C" fn(u64, u32, u32, u32, u32) -> u64;

type CollisionCallback = fn(&CollisionContext);

static mut TRAMPOLINE: Option<CollisionHitTrampoline> = None;
static CALLBACKS: LazyLock<Mutex<Vec<CollisionCallback>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(8)));

#[derive(Clone, Debug)]
pub struct HitRecord {
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub tick: u64,
}

/// Context passed to registered callbacks — mirrors Jorge `local_158` bundle.
#[derive(Clone, Debug)]
pub struct CollisionContext {
    pub log_kind: u32,
    pub param_2: u64,
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub collision_id: u32,
    pub flags: u32,
    pub tick: u64,
}

pub fn install() {
    if *INSTALLED.lock() {
        return;
    }

    register_callback(collision_queue_callback);

    // The collision hit notify is hooked with a manual `A64HookFunction` inline hook (there's no
    // clean L2C symbol to `skyline::hook(replace=…)`). That inline hook + Eden's dynarmic JIT
    // produces a bad trampoline that null-jumps when a hit fires (crash in `collision_hit_hook`).
    // It only feeds the damage *log* (not multiplier application), so disable it under the emulator
    // to keep the core effect viewer usable. TODO: robust 13.0.4 hook (or run on real hardware).
    if !ENABLE_INLINE_COLLISION_HOOK {
        skyline::println!("[SLight] Skyline Hook: inline collision hook disabled (Eden-unsafe)");
        return;
    }

    let offset = match scan_collision_pattern() {
        Some(off) => off,
        None => {
            skyline::println!(
                "[SLight] Skyline Hook: collision pattern not found in .text (version mismatch?)"
            );
            return;
        }
    };

    unsafe {
        let text = getRegionAddress(Region::Text);
        let target = (text as *const u8).add(offset) as *const libc::c_void;
        let mut trampoline_ptr: *mut libc::c_void = std::ptr::null_mut();
        A64HookFunction(
            target,
            collision_hit_hook as *const libc::c_void,
            &mut trampoline_ptr,
        );
        if trampoline_ptr.is_null() {
            skyline::println!("[SLight] Skyline Hook: A64HookFunction returned null trampoline");
            return;
        }
        TRAMPOLINE = Some(std::mem::transmute(trampoline_ptr));
    }

    *INSTALLED.lock() = true;
    skyline::println!("[SLight] Skyline Hook installed @ text+0x{offset:x}");
}

fn register_callback(cb: CollisionCallback) {
    let mut callbacks = CALLBACKS.lock();
    if callbacks.len() >= 8 {
        skyline::println!("[SLight] Skyline Hook: callback list full");
        return;
    }
    callbacks.push(cb);
}

fn scan_collision_pattern() -> Option<usize> {
    unsafe {
        let text_start = getRegionAddress(Region::Text) as *const u8;
        let text_end = getRegionAddress(Region::Rodata) as *const u8;
        if text_start.is_null() || text_end <= text_start {
            return None;
        }
        let len = text_end.offset_from(text_start) as usize;
        if len < COLLISION_PATTERN.len() {
            return None;
        }
        let slide = len - COLLISION_PATTERN.len();
        for off in 0..=slide {
            if (0..COLLISION_PATTERN.len())
                .all(|i| *text_start.add(off + i) == COLLISION_PATTERN[i])
            {
                return Some(off);
            }
        }
        None
    }
}

unsafe extern "C" fn collision_hit_hook(
    param_1: u32,
    param_2: u64,
    attacker: u32,
    defender: u32,
    collision_id: u32,
    flags: u32,
) -> u64 {
    let tick = crate::slight::frame_context::match_ticks();
    let ctx = CollisionContext {
        log_kind: param_1,
        param_2,
        attacker_boid: attacker,
        defender_boid: defender,
        collision_id,
        flags,
        tick,
    };

    run_callbacks(&ctx);

    if let Some(orig) = TRAMPOLINE {
        orig(param_2, attacker, defender, collision_id, flags)
    } else {
        0
    }
}

fn run_callbacks(ctx: &CollisionContext) {
    for callback in CALLBACKS.lock().iter() {
        callback(ctx);
    }
}

fn collision_queue_callback(ctx: &CollisionContext) {
    notify_log_event_collision_hit(ctx);
}

/// Jorge @ 71000eb4b0 — collision hit notify + damage/overload queues.
pub fn notify_log_event_collision_hit(ctx: &CollisionContext) {
    record_hit(ctx.attacker_boid, ctx.defender_boid, ctx.tick);
    crate::slight::systems::damage_manager::on_collision_hit(ctx);
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!(
            "[SLight] notify_log_event_collision_hit {} -> {} id={} flags={}",
            ctx.attacker_boid,
            ctx.defender_boid,
            ctx.collision_id,
            ctx.flags
        );
    }
}

/// Jorge @ 71000dfda8 — after-win attack log hook entry (SD debug path).
pub fn handle_attack(param: u64) {
    if crate::slight::frame_context::is_after_win()
        && crate::slight::smash_utils::debug_logging_enabled()
    {
        skyline::println!("[SLight] handle_attack param={param:#x}");
    }
}

pub fn is_installed() -> bool {
    *INSTALLED.lock()
}

pub fn record_hit(attacker: u32, defender: u32, tick: u64) {
    let mut q = HITS.lock();
    q.push_back(HitRecord {
        attacker_boid: attacker,
        defender_boid: defender,
        tick,
    });
    while q.len() > 64 {
        q.pop_front();
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Skyline Hook hit: {attacker} -> {defender} @ tick {tick}");
    }
}

pub fn drain_hits() -> Vec<HitRecord> {
    HITS.lock().drain(..).collect()
}

pub fn clear() {
    HITS.lock().clear();
    unsafe {
        TRAMPOLINE = None;
    }
    CALLBACKS.lock().clear();
    *INSTALLED.lock() = false;
}
