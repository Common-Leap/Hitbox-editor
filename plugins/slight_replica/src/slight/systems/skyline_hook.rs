//! Collision / attack hit log — Jorge skyline_hook facade (FUN_71000d1fb4).

use parking_lot::Mutex;
use skyline::hooks::{getRegionAddress, A64HookFunction, Region};
use skyline::libc;
use std::collections::VecDeque;
use std::sync::LazyLock;

static INSTALLED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static HITS: LazyLock<Mutex<VecDeque<HitRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(32)));

/// Whether to install the manual inline collision hook in every boot. Off by default: use the
/// ABI-verified binding wrapper fallback, which is partial because direct body callers bypass the
/// exported wrapper. A one-shot `sd:/slight/debug/inline_collision_hook.txt` trigger can request
/// the inline path for a live test without changing the shipped default.
const ENABLE_INLINE_COLLISION_HOOK: bool = false;

/// 40-byte ARM64 pattern scanned in game `.text` @ FUN_71000d1fb4.
const COLLISION_PATTERN: [u8; 40] = [
    0xff, 0x03, 0x03, 0xd1, 0xe8, 0x2b, 0x00, 0xfd, 0xfc, 0x6f, 0x06, 0xa9, 0xfa, 0x67, 0x07, 0xa9,
    0xf8, 0x5f, 0x08, 0xa9, 0xf6, 0x57, 0x09, 0xa9, 0xf4, 0x4f, 0x0a, 0xa9, 0xfd, 0x7b, 0x0b, 0xa9,
    0xfd, 0xc3, 0x02, 0x91, 0xfb, 0x03, 0x00, 0xaa,
];

/// The 13.0.4 `FighterManager::notify_log_event_collision_hit` ABI reached by the scanned
/// wrapper. The first two integers are the attacker/defender battle-object IDs; the native
/// function also carries the hit value, collision id, and a boolean flag in their ABI-native
/// registers. Keep the float and bool in their real positions — treating this as six integer
/// arguments corrupts both the callback fields and the return path on AArch64.
type CollisionHitTrampoline = unsafe extern "C" fn(
    *mut smash::app::FighterManager,
    u32,
    u32,
    f32,
    i32,
    bool,
) -> u64;

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

/// Context passed to registered callbacks — the fields that survive the 13.0.4 native notify
/// boundary, plus the opaque manager receiver retained for diagnostics.
#[derive(Clone, Debug)]
pub struct CollisionContext {
    pub manager: u64,
    pub attacker_boid: u32,
    pub defender_boid: u32,
    pub damage: f32,
    pub collision_id: u32,
    pub flags: u32,
    pub tick: u64,
}

pub fn install() {
    if *INSTALLED.lock() {
        return;
    }

    register_callback(collision_queue_callback);

    let inline_requested_by_file = crate::slight::smash_utils::consume_sd_trigger(
        crate::slight::smash_utils::DEBUG_INLINE_COLLISION,
    );
    if inline_requested_by_file {
        skyline::println!(
            "[SLight] Skyline Hook: one-shot inline collision hook requested by SD trigger"
        );
        crate::slight::diag::note("COLLISION_HOOK request=inline-one-shot");
    }
    let inline_requested = ENABLE_INLINE_COLLISION_HOOK || inline_requested_by_file;

    // The pinned bindings expose the exported FighterManager wrapper, but static 13.0.4 analysis
    // shows that the wrapper branches to this body and that other game sites call the body
    // directly. Hook the body so those sites are not missed; a wrapper-only
    // `skyline::hook(replace=…)` would provide incomplete collision coverage. The trampoline's
    // native ABI is corrected above, but an earlier Eden run still produced a bad trampoline that
    // null-jumped when a hit fired. Keep this disabled until the corrected path is proven on
    // hardware or with a compatible hook implementation.
    if !inline_requested {
        skyline::println!(
            "[SLight] Skyline Hook: inline collision hook disabled (Eden-unsafe); installing wrapper fallback (partial coverage)"
        );
        crate::slight::diag::note("COLLISION_HOOK mode=wrapper-fallback coverage=partial");
        skyline::install_hook!(fallback_collision_hit_hook);
        *INSTALLED.lock() = true;
        return;
    }

    let offset = match scan_collision_pattern() {
        Some(off) => off,
        None => {
            skyline::println!(
                "[SLight] Skyline Hook: body pattern missing or ambiguous; installing wrapper fallback (partial coverage)"
            );
            crate::slight::diag::note(
                "COLLISION_HOOK mode=wrapper-fallback reason=pattern-missing-or-ambiguous",
            );
            skyline::install_hook!(fallback_collision_hit_hook);
            *INSTALLED.lock() = true;
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
            crate::slight::diag::note("COLLISION_HOOK mode=inline-body failure=null-trampoline");
            return;
        }
        TRAMPOLINE = Some(std::mem::transmute(trampoline_ptr));
    }

    *INSTALLED.lock() = true;
    crate::slight::diag::note(format!(
        "COLLISION_HOOK mode=inline-body offset=0x{offset:x}"
    ));
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
        let mut match_offset = None;
        for off in 0..=slide {
            if (0..COLLISION_PATTERN.len())
                .all(|i| *text_start.add(off + i) == COLLISION_PATTERN[i])
            {
                // A repeated prologue is not evidence that the first match is the collision
                // body. Refuse to hook an ambiguous target and let the binding fallback report
                // its known partial coverage instead of risking an unrelated trampoline.
                if match_offset.is_some() {
                    return None;
                }
                match_offset = Some(off);
            }
        }
        match_offset
    }
}

/// Binding-based fallback used only when the complete 13.0.4 body pattern is unavailable. The
/// exported wrapper is version-pinned and has the same ABI, but static analysis shows that direct
/// body callers bypass it, so this is intentionally reported as partial coverage.
#[skyline::hook(
    replace = smash::app::lua_bind::FighterManager::notify_log_event_collision_hit
)]
unsafe fn fallback_collision_hit_hook(
    manager: *mut smash::app::FighterManager,
    attacker_boid: u32,
    defender_boid: u32,
    damage: f32,
    collision_id: i32,
    flags: bool,
) -> u64 {
    let ctx = collision_context(
        manager,
        attacker_boid,
        defender_boid,
        damage,
        collision_id,
        flags,
    );
    run_callbacks(&ctx);
    original!()(manager, attacker_boid, defender_boid, damage, collision_id, flags)
}

unsafe extern "C" fn collision_hit_hook(
    manager: *mut smash::app::FighterManager,
    attacker_boid: u32,
    defender_boid: u32,
    damage: f32,
    collision_id: i32,
    flags: bool,
) -> u64 {
    let ctx = collision_context(
        manager,
        attacker_boid,
        defender_boid,
        damage,
        collision_id,
        flags,
    );

    run_callbacks(&ctx);

    if let Some(orig) = TRAMPOLINE {
        orig(
            manager,
            attacker_boid,
            defender_boid,
            damage,
            collision_id,
            flags,
        )
    } else {
        0
    }
}

fn collision_context(
    manager: *mut smash::app::FighterManager,
    attacker_boid: u32,
    defender_boid: u32,
    damage: f32,
    collision_id: i32,
    flags: bool,
) -> CollisionContext {
    CollisionContext {
        manager: manager as u64,
        attacker_boid,
        defender_boid,
        damage,
        collision_id: collision_id as u32,
        flags: flags as u32,
        tick: crate::slight::frame_context::match_ticks(),
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

/// Collision hit notify + damage/overload queues.
pub fn notify_log_event_collision_hit(ctx: &CollisionContext) {
    record_hit(ctx.attacker_boid, ctx.defender_boid, ctx.tick);
    crate::slight::systems::damage_manager::on_collision_hit(ctx);
    if crate::slight::smash_utils::debug_logging_enabled() {
        crate::slight::diag::note(format!(
            "COLLISION attacker={} defender={} damage={:.2} id={} flags={}",
            ctx.attacker_boid, ctx.defender_boid, ctx.damage, ctx.collision_id, ctx.flags
        ));
        skyline::println!(
            "[SLight] notify_log_event_collision_hit {} -> {} damage={:.2} id={} flags={}",
            ctx.attacker_boid,
            ctx.defender_boid,
            ctx.damage,
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
