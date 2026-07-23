//! Re-parse `.eff` resources mid-match via the game's effect manager (SSBU 13.0.4).
//!
//! Ported from the original effect viewer's `effect_reload.rs` (working LiveEdit build).
//! This is the piece the resident-byte overwrite was missing: the game keeps each eff's
//! PARSED emitter data, so to apply an edited/merged eff live we make the effect manager
//! UNLOAD then LOAD the slot — forcing a real re-parse of the (arcrop-redirected) file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::LazyLock;

use parking_lot::Mutex;

// Offsets from HDR-Development/smashline 13.0.4 support.
const EFFECT_MANAGER_OFFSET: usize = 0x5333920;
const LOAD_EFFECTS_OFFSET: usize = 0x355f8f0;
const UNLOAD_EFFECTS_OFFSET: usize = 0x3563720;

static ACTIVE_SLOTS: LazyLock<Mutex<HashMap<u32, EffectSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Handles seen for a path hash — kept after unload so reload can still target them.
static KNOWN_HANDLES: LazyLock<Mutex<HashMap<u64, Vec<u32>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LAST_REPARSED: AtomicU64 = AtomicU64::new(0);
static LOAD_HOOK_CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct EffectSlot {
    search_index: u32,
    path_hash: u64,
    /// What load_effects returned for this slot — lets us compare our donor loads
    /// against the values the GAME's own (working) loads produce.
    result: u32,
}

#[skyline::from_offset(LOAD_EFFECTS_OFFSET)]
fn load_effects(manager: *mut u64, handle: u32, search_index: &u32) -> u32;

#[skyline::from_offset(UNLOAD_EFFECTS_OFFSET)]
fn unload_effects(manager: *mut u64, handle: u32);

/// The RESIDENCY WRAPPER (game fn @ ~0x3563470) — same signature as load_effects, but it first
/// schedules the search_index's dependency dirs into the resource WORKER (fn_3540450 @0x3540450 →
/// schedule-load 0x353ff00) so they become resident, THEN calls the inner load_effects. The
/// assist-summon path uses THIS; our co-load called the inner load_effects directly, skipping the
/// residency → the worker never loaded the donor's dirs → +0x540 stayed zero → invisible. Entry
/// resolved at runtime by scanning back from a known interior insn (0x3563488 = `add x29,sp,#0x60`)
/// for the `sub sp,sp,#imm` prologue, so a wrong offset cannot crash us.
fn load_effects_resident_entry() -> usize {
    use std::sync::atomic::AtomicUsize;
    static ENTRY: AtomicUsize = AtomicUsize::new(usize::MAX);
    let cached = ENTRY.load(Ordering::Relaxed);
    if cached != usize::MAX {
        return cached;
    }
    let text = unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize };
    let interior = text + 0x3563488;
    let mut found = 0usize;
    for i in 1..=10 {
        let a = interior - i * 4;
        let w = unsafe { *(a as *const u32) };
        if (w & 0xFF80_03FF) == 0xD100_03FF {
            // sub sp,sp,#imm
            found = a;
            break;
        }
    }
    ENTRY.store(found, Ordering::Relaxed);
    found
}

unsafe fn load_effects_resident(manager: *mut u64, handle: u32, idx: &u32) -> u32 {
    let entry = load_effects_resident_entry();
    if entry == 0 {
        return u32::MAX; // not resolved — caller falls back to raw load_effects
    }
    let f: extern "C" fn(*mut u64, u32, *const u32) -> u32 = std::mem::transmute(entry);
    f(manager, handle, idx as *const u32)
}

/// The deferred-load QUEUE DRAIN (game fn @ 0x354a120). Processes each queued resource-load
/// callback (ring buffer reachable from x0: base=*(x0+8), index@+0x30, count@+0x38), calling
/// each item's vtable[0]. In the assist-summon path the traced stack shows load_effects running
/// AS one of these callbacks — i.e. the drain executes with the donor's dir resources resident,
/// which is exactly what fills the set's +0x540 render block. Our detached mid-match co-load
/// schedules into this queue (via the residency wrapper) but nothing drains it in our context,
/// so the reads never complete → +0x540 stays zero → invisible. Driving the drain ourselves,
/// on the object the game passes it, completes the load in-context. THIS is the render push.
const DRAIN_QUEUE_OFFSET: usize = 0x354a120;
#[skyline::from_offset(DRAIN_QUEUE_OFFSET)]
fn drain_load_queue(obj: *mut u64);

/// The queue object the game passes to the drain — captured LIVE from the hook (it's a
/// resource-scheduler singleton we can't derive statically). 0 until the game drains once.
static DRAIN_QUEUE_OBJ: AtomicUsize = AtomicUsize::new(0);
static DRAIN_FIRES: AtomicU64 = AtomicU64::new(0);
/// Set while the GAME's own alucard (idx=69) load_effects runs, so hook_build_effect_set can log
/// the a4d90 call it makes as the working reference to diff our co-load against.
static GAME_ALUCARD_LOADING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[skyline::hook(offset = DRAIN_QUEUE_OFFSET)]
fn hook_drain_queue(obj: *mut u64) {
    // Capture the queue object the game drains so our pump can drive the same one.
    DRAIN_QUEUE_OBJ.store(obj as usize, Ordering::Relaxed);
    let n = DRAIN_FIRES.fetch_add(1, Ordering::Relaxed);
    if n == 0 || n == 600 || n == 6000 {
        // Prove the drain fires mid-match + show current queue depth (items waiting).
        let (idx, cnt) = unsafe {
            let base = *((obj as usize + 8) as *const usize);
            if base > 0x1000 {
                (
                    *((base + 0x30) as *const u32),
                    *((base + 0x38) as *const u32),
                )
            } else {
                (u32::MAX, u32::MAX)
            }
        };
        dlog(&format!(
            "DRAIN fired total={} obj={:#x} q_index={idx} q_count={cnt}",
            n + 1,
            obj as usize
        ));
    }
    original!()(obj);
}

// Effect-manager mutex (mgr+0x19498). load_effects/unload_effects lock this at entry; a hang
// there = the mutex is already held (deadlock). Probe = can WE acquire it right now?
#[skyline::from_offset(0x39c1410)]
fn mgr_mutex_lock(mutex: *mut u8);
#[skyline::from_offset(0x39c1420)]
fn mgr_mutex_unlock(mutex: *mut u8);

// The effect-set BUILDER (`func_0xa4d90`): destroys the old set object at mgr[+0x98][slot] and
// rebuilds it from the effect-data section of the buffer. This is the GPU/resource-heavy part
// of load_effects, WITHOUT its folder/res/name-table machinery — so calling it directly
// isolates which half freezes mid-match.
#[skyline::from_offset(0xa4d90)]
fn build_effect_set(
    p1: u64,
    p2: *mut u8,
    buffer_section: *const u8,
    slot: u32,
    flag: u8,
    zero: u64,
) -> u64;

/// OBSERVE the emitter builder (a4d90). The set diff proved our co-loaded set is built only
/// PARTIALLY — its effect-parameter region (inside the 0x540-byte object) stays stale, while the
/// assist's identical load fills it. a4d90 builds the set from `effect_data` (param_3) using the
/// mode `flag` (param_5 = *(byte*)mgr+0x194b8). Log both loads' args + the first bytes of the
/// effect-data section: if `effect_data` content or `flag` differ between the assist and our
/// co-load, that's the root cause of the params-less build. Read-only — always calls original.
#[skyline::hook(offset = 0xa4d90)]
fn hook_build_effect_set(
    p1: u64,
    p2: *mut u8,
    effect_data: *const u8,
    slot: u32,
    flag: u8,
    zero: u64,
) -> u64 {
    let is_donor = IN_DONOR_LOAD.load(Ordering::Relaxed);
    let game_alucard = GAME_ALUCARD_LOADING.load(Ordering::Relaxed);
    // Log the alucard-relevant builds only: our donor co-load, OR the game's own alucard (idx=69)
    // load (flagged in hook_load_effects). That gives the two a4d90 arg-sets to diff directly.
    let want = is_donor || game_alucard;
    if want {
        let head: [u8; 16] = unsafe {
            let mut b = [0u8; 16];
            if !effect_data.is_null() {
                std::ptr::copy_nonoverlapping(effect_data, b.as_mut_ptr(), 16);
            }
            b
        };
        static N: AtomicU64 = AtomicU64::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 60 {
            dlog(&format!(
                "a4d90 donor={is_donor} game_alucard={game_alucard} slot={slot} flag={flag:#x} p1={p1:#x} data={:#x} head={head:02x?}",
                effect_data as usize
            ));
        }
    }
    original!()(p1, p2, effect_data, slot, flag, zero)
}

/// Allocate from the GAME's effect allocator (the object at `mgr+0x194d8` whose vtable+0x10
/// `load_effects`/`a4d90` themselves call: `(*vtable[2])(obj, size, align)`). Build n proved
/// eff buffers must live in game-allocated memory: the SAME bytes that build fine from the
/// game's own resident buffer hard-freeze from a plugin-heap Vec (the effect system maps the
/// buffer for GPU access in place — plugin heap isn't GPU-visible).
/// The game's general allocator used by load_effects for kind-map nodes
/// (`func_0x0392dce0(align, size)` — seen allocating the 0x20-byte name-map nodes).
#[skyline::from_offset(0x392dce0)]
fn game_malloc(align: u64, size: u64) -> *mut u8;

/// The REAL kind resolver `req()` calls (FUN_02601110). Takes (effect_manager, kind_hash40),
/// returns a pointer to the resolved entry (`slot_entry_table + entry_index*0x10`) or 0.
/// Calling it directly is the ground truth for "is this kind registered + reachable" — unlike
/// the get_last_handle heuristic, which only says whether an effect instance got created.
#[skyline::from_offset(0x2601110)]
fn resolve_kind(manager: *mut u64, kind_hash: u64) -> *const u8;

/// HOOK on the resolver: we can't safely call it ourselves (build t froze), but the game
/// calls it constantly — so observe it on the game's own path. Logs (a) the manager pointer
/// the GAME passes vs the one we derived (a mismatch would explain both the refusal and the
/// build-t freeze — two manager instances), and (b) every `_os` lookup with its result.
/// Hook the CONFIRMED req impl (obj vtable+0x68 = FUN_0044de70). It reads inner=*(obj+0x10)
/// and, if nonzero, calls inner_vt[0x190] then tail-calls inner_vt[0x50] — the real
/// resolve+instantiate. Capturing param_1 (the true obj) and inner HERE (at the real call,
/// not a track_spawn deref that read inner=0x3) yields the actual impl addresses to
/// decompile. One-shot to avoid log spam / perf.
/// The texture-handler guard `FUN_00093f10` = `*(char*)(obj+0x20) == 3`. Inside the effect
/// set builder (99560 → 9a100) this gates whether textures get set up at all. If it returns
/// FALSE during our mid-match co-load, textures are skipped → effect spawns but draws nothing.
/// Hook it (only while IN_DONOR_LOAD) to see the state byte + result, and FORCE it: if the
/// byte isn't 3 during co-load, set it to 3 so the texture path runs. (state 3 = the loading
/// phase where fighter-load does the same.)
/// Countdown of texture-guard calls to force to `true` after a co-load. The texture setup is
/// DEFERRED past the load_effects call (IN_DONOR_LOAD is already cleared when 9a100 runs), so
/// we open a window: after a donor co-loads, force the next N guard calls so the deferred
/// texture processing for the freshly co-loaded eff actually runs.
pub static TEX_FORCE_BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[skyline::hook(offset = 0x93f10)]
fn hook_tex_guard(param_1: *mut u8) -> bool {
    // Force DISABLED: overriding a running effect's state byte froze the game. Pass through.
    let _ = &TEX_FORCE_BUDGET;
    original!()(param_1)
}

#[skyline::hook(offset = 0x44de70)]
fn hook_req_impl(param_1: *mut u64) {
    static CAP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if CAP.fetch_add(1, Ordering::Relaxed) < 3 {
        use std::io::Write;
        unsafe {
            let text = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize;
            let obj = param_1 as usize;
            // Dump the object's first 0x80 bytes as u64 words. Whichever word is a heap
            // pointer (0x10_0000_0000..0x20_0000_0000) whose *[0] is a text-range vtable is
            // the real `inner`; +0x10 = 0x3 is a flag, not the pointer. For each plausible
            // inner, resolve its vtable's 0x190/0x50 slots.
            let mut words = String::new();
            for i in 0..16usize {
                let w = *((obj + i * 8) as *const usize);
                words.push_str(&format!("+{:#04x}={w:#x} ", i * 8));
            }
            let mut cand = String::new();
            for i in 0..16usize {
                let p = *((obj + i * 8) as *const usize);
                if (0x10_0000_0000..0x20_0000_0000).contains(&p) {
                    let vt = *(p as *const usize);
                    if (text..text + 0x4000_0000).contains(&vt) {
                        let f190 = *((vt + 0x190) as *const usize);
                        let f50 = *((vt + 0x50) as *const usize);
                        cand.push_str(&format!(
                            "[inner@+{:#x}={p:#x} vt=+{:#x} f190=+{:#x} f50=+{:#x}] ",
                            i * 8,
                            vt.wrapping_sub(text),
                            f190.wrapping_sub(text),
                            f50.wrapping_sub(text),
                        ));
                    }
                }
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("sd:/effect_viewer_os_req.txt")
            {
                let _ = writeln!(
                    f,
                    "REQ_IMPL obj={obj:#x} words: {words}\n  candidates: {cand}"
                );
            }
        }
    }
    original!()(param_1)
}

#[skyline::hook(offset = 0x2601110)]
fn hook_resolve_kind(mgr: *mut u64, kind_hash: u64) -> *const u8 {
    let r = original!()(mgr, kind_hash);
    use std::sync::atomic::AtomicBool;
    static FIRST: AtomicBool = AtomicBool::new(false);
    let first = !FIRST.swap(true, Ordering::Relaxed);
    let h40 = kind_hash & 0xff_ffff_ffff;
    let label = crate::slight::effect_viewer::effect_names::label(h40);
    let interesting = first || label.ends_with("_os");
    if interesting {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_os_req.txt")
        {
            let ours = effect_manager();
            let entry = if r.is_null() {
                "NULL".to_string()
            } else {
                format!("{:02x?}", unsafe { std::slice::from_raw_parts(r, 8) })
            };
            let _ = writeln!(
                f,
                "RESOLVER {label} h40={h40:#x} mgr={mgr:?} ours={ours:?} same={} -> {entry}",
                mgr == ours
            );
        }
    }
    r
}

/// The manager-GLOBAL kind-name map `req(hash40(name))` resolves through. Decompiled from
/// load_effects' registration loop @0x3561xxx: buckets ptr @mgr+0x193d8, bucket count
/// @+0x193e0, global-list sentinel @+0x193e8, size @+0x193f0 (libc++ unordered_map). Node:
/// {+0 next, +8 hash40, +0x10 hash40 (key), +0x18 handle i32, +0x1c entry_index i32}.
/// Key = hash40 of the LOWERCASED entry name (the game tolowers before CRC).
unsafe fn kind_bucket(h: u64, count: usize) -> usize {
    if count & (count - 1) == 0 {
        (h & (count as u64 - 1)) as usize
    } else {
        (h % count as u64) as usize
    }
}

/// Look up a kind hash40 in the manager's name map → (handle, entry_index).
unsafe fn kind_lookup(mgr: *mut u64, h40: u64) -> Option<(u32, u32)> {
    if mgr.is_null() {
        return None;
    }
    let base = mgr as usize;
    let buckets = *((base + 0x193d8) as *const usize);
    let count = *((base + 0x193e0) as *const u64) as usize;
    if buckets == 0 || count == 0 {
        return None;
    }
    let bucket = kind_bucket(h40, count);
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        return None;
    }
    let mut node = *(before as *const usize);
    let mut guard = 0;
    while node != 0 && guard < 100_000 {
        guard += 1;
        let nh = *((node + 8) as *const u64);
        if kind_bucket(nh, count) != bucket {
            break; // walked past this bucket's chain segment
        }
        if *((node + 0x10) as *const u64) == h40 {
            return Some((
                *((node + 0x18) as *const i32) as u32,
                *((node + 0x1c) as *const i32) as u32,
            ));
        }
        node = *(node as *const usize);
    }
    None
}

/// Guarded diagnostic walk of a kind-map bucket chain: reports hit/miss, chain length, and
/// CYCLES (node revisited) — the game's resolver has no guard, so a cycle in a bucket chain
/// hangs it forever. This walker cannot hang and names the corruption if present.
unsafe fn kind_chain_debug(mgr: *mut u64, h40: u64) -> String {
    if mgr.is_null() {
        return "mgr=null".into();
    }
    let base = mgr as usize;
    let buckets = *((base + 0x193d8) as *const usize);
    let count = *((base + 0x193e0) as *const u64) as usize;
    let size = *((base + 0x193f0) as *const u64);
    if buckets == 0 || count == 0 {
        return format!("empty map (buckets={buckets:#x} count={count})");
    }
    let bucket = kind_bucket(h40, count);
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        return format!("bucket {bucket}/{count} empty (map size={size})");
    }
    let mut node = *(before as *const usize);
    let mut seen = std::collections::HashSet::new();
    let mut steps = 0usize;
    while node != 0 {
        if !seen.insert(node) {
            return format!("CYCLE after {steps} steps at node {node:#x} (bucket {bucket})");
        }
        if steps > 4096 {
            return format!("chain >4096 (bucket {bucket}) — degenerate");
        }
        let nh = *((node + 8) as *const u64);
        if kind_bucket(nh, count) != bucket {
            return format!("miss after {steps} steps (left bucket {bucket}, size={size})");
        }
        if *((node + 0x10) as *const u64) == h40 {
            let handle = *((node + 0x18) as *const i32);
            let idx = *((node + 0x1c) as *const i32);
            return format!("HIT step {steps}: node={node:#x} handle={handle} idx={idx}");
        }
        node = *(node as *const usize);
        steps += 1;
    }
    format!("miss at chain end after {steps} steps (bucket {bucket}, size={size})")
}

/// Guarded replica of the game's kind resolver FUN_02601110 — identical three-step walk
/// (kind map @+0x193d8 → handle map @+0x193b0 → slot entry table @slot*0xa8+0x18018) but
/// with cycle guards on both bucket chains, so it can NEVER hang and it narrates each step.
unsafe fn resolve_kind_replica(mgr: *mut u64, h40: u64) -> (String, Option<*const u8>) {
    if mgr.is_null() {
        return ("mgr=null".into(), None);
    }
    let base = mgr as usize;
    // Step 1: kind map → (handle, entry_idx)
    let Some((handle, entry_idx)) = kind_lookup(mgr, h40) else {
        return (
            format!("kind-map miss ({})", kind_chain_debug(mgr, h40)),
            None,
        );
    };
    // Step 2: handle map → slot (guarded walk; the game's version has no guard)
    let buckets = *((base + 0x193b0) as *const usize);
    let count = *((base + 0x193b8) as *const u64) as usize;
    if buckets == 0 || count == 0 {
        return (
            format!("handle-map empty (handle={handle} idx={entry_idx})"),
            None,
        );
    }
    let hh = handle as u64;
    let bucket = kind_bucket(hh, count);
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        return (
            format!("handle-map bucket empty (handle={handle} idx={entry_idx})"),
            None,
        );
    }
    let mut node = *(before as *const usize);
    let mut seen = std::collections::HashSet::new();
    let mut slot: Option<i32> = None;
    let mut steps = 0usize;
    while node != 0 {
        if !seen.insert(node) {
            return (
                format!("handle-map CYCLE after {steps} steps at {node:#x} (handle={handle})"),
                None,
            );
        }
        let nh = *((node + 8) as *const u64);
        if nh == hh {
            if *((node + 0x10) as *const i32) == handle as i32 {
                slot = Some(*((node + 0x14) as *const i32));
                break;
            }
        } else if kind_bucket(nh, count) != bucket {
            break;
        }
        node = *(node as *const usize);
        steps += 1;
    }
    let Some(slot) = slot else {
        return (
            format!("handle-map miss after {steps} steps (handle={handle} idx={entry_idx})"),
            None,
        );
    };
    // Step 3: slot's entry table
    let table = *((base + slot as usize * 0xa8 + 0x18018) as *const usize);
    if table == 0 {
        return (
            format!("slot {slot} entry table NULL (handle={handle} idx={entry_idx})"),
            None,
        );
    }
    (
        format!("handle={handle} idx={entry_idx} slot={slot} table={table:#x}"),
        Some((table + entry_idx as usize * 0x10) as *const u8),
    )
}

/// Inspect the live effect-SET object for a manager slot (built by FUN_00099560):
/// set_obj = mgr[+0x194d0]→[+0x98][slot]; +0x4c = set count; +0x58 = per-set array,
/// stride 0x38: {+4 emitter_count i32, +0x18 emitters ptr (stride 0x3e0), +0x30 name ptr}.
/// Dumping the donor set beside a vanilla set shows whether the set BUILD (not the
/// name/entry resolution) is where instantiation dies.
unsafe fn set_object_debug(mgr: *mut u64, slot: u32, set_idx0: usize) -> String {
    let p1 = *((mgr as usize + 0x194d0) as *const usize);
    if p1 == 0 {
        return "p1=null".into();
    }
    let arr = *((p1 + 0x98) as *const usize);
    if arr == 0 {
        return "set-obj array null".into();
    }
    let set_obj = *((arr + slot as usize * 8) as *const usize);
    if set_obj == 0 {
        return format!("slot {slot}: set object NULL");
    }
    let count = *((set_obj + 0x4c) as *const i32);
    let sets = *((set_obj + 0x58) as *const usize);
    if sets == 0 {
        return format!("slot {slot}: count={count} but per-set array NULL");
    }
    if set_idx0 >= count.max(0) as usize {
        return format!("slot {slot}: count={count} — set idx {set_idx0} OUT OF RANGE");
    }
    let e = sets + set_idx0 * 0x38;
    let f0 = *(e as *const u32);
    let emitter_count = *((e + 4) as *const i32);
    let emitters = *((e + 0x18) as *const usize);
    let name_ptr = *((e + 0x30) as *const usize);
    let name = if name_ptr != 0 {
        let mut len = 0usize;
        while len < 48 && *((name_ptr + len) as *const u8) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(std::slice::from_raw_parts(name_ptr as *const u8, len)).into_owned()
    } else {
        "<null>".into()
    };
    format!(
        "slot {slot} count={count} set[{set_idx0}]: f0={f0:#x} emitters={emitter_count} ptr={emitters:#x} name='{name}'"
    )
}

/// Insert a kind into the manager's name map — an exact replica of load_effects' own
/// insertion code (node alloc via the same game allocator, same libc++ bucket-head insert
/// incl. rehoming the displaced head's bucket pointer). This IS live kind registration:
/// after this, `req(hash40(name))` resolves to (handle, entry_index) like any vanilla kind.
unsafe fn kind_register(mgr: *mut u64, h40: u64, handle: u32, entry_idx: u32) -> bool {
    let base = mgr as usize;
    let buckets = *((base + 0x193d8) as *const usize);
    let count = *((base + 0x193e0) as *const u64) as usize;
    let sentinel = (base + 0x193e8) as *mut usize;
    let size_p = (base + 0x193f0) as *mut u64;
    if buckets == 0 || count == 0 {
        return false;
    }
    let node = game_malloc(0x10, 0x20) as usize;
    if node == 0 {
        return false;
    }
    *((node + 8) as *mut u64) = h40;
    *((node + 0x10) as *mut u64) = h40;
    *((node + 0x18) as *mut i32) = handle as i32;
    *((node + 0x1c) as *mut i32) = entry_idx as i32;
    *(node as *mut usize) = 0;
    let bucket = kind_bucket(h40, count);
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        // Empty bucket: splice at the global list head; this bucket's "before" becomes the
        // sentinel, and the displaced old head's bucket must now point at OUR node.
        *(node as *mut usize) = *sentinel;
        *sentinel = node;
        *((buckets + bucket * 8) as *mut usize) = sentinel as usize;
        let displaced = *(node as *const usize);
        if displaced != 0 {
            let dh = *((displaced + 8) as *const u64);
            *((buckets + kind_bucket(dh, count) * 8) as *mut usize) = node;
        }
    } else {
        *(node as *mut usize) = *(before as *const usize);
        *(before as *mut usize) = node;
    }
    *size_p += 1;
    true
}

/// Walk an EFFN buffer's entry-name table (same layout load_effects reads: names start at
/// 0x10 + num_effects*0x10 + multi_part*4 + num_external, consecutive NUL-terminated strings
/// in entry order). Returns (name, entry_index).
unsafe fn eff_entry_names(buf: *const u8) -> Vec<(String, u32)> {
    let base = buf as usize;
    let num = *((base + 8) as *const u16) as usize;
    let ext = *((base + 0xa) as *const u16) as usize;
    let multi = *((base + 0xc) as *const u16) as usize;
    let mut p = base + 0x10 + num * 0x10 + multi * 4 + ext;
    let mut out = Vec::with_capacity(num);
    for i in 0..num {
        let start = p;
        while *(p as *const u8) != 0 {
            p += 1;
        }
        let bytes = std::slice::from_raw_parts(start as *const u8, p - start);
        out.push((String::from_utf8_lossy(bytes).into_owned(), i as u32));
        p += 1;
    }
    out
}

unsafe fn game_effect_alloc(mgr: *mut u64, size: usize, align: usize) -> *mut u8 {
    if mgr.is_null() {
        return std::ptr::null_mut();
    }
    let obj = (mgr as usize + 0x194d8) as *mut u64;
    let vtable = *obj as usize;
    if vtable == 0 {
        return std::ptr::null_mut();
    }
    let alloc_fn: extern "C" fn(*mut u64, usize, usize) -> *mut u8 =
        std::mem::transmute(*((vtable + 0x10) as *const usize));
    alloc_fn(obj, size, align)
}

/// The manager slot index the folder handle currently occupies (map node `+0x14`), for a
/// direct `build_effect_set` call on the fighter's own live slot.
unsafe fn manager_slot_for_handle(mgr: *mut u64, handle: u32) -> Option<u32> {
    if mgr.is_null() {
        return None;
    }
    let base = mgr as usize;
    let buckets = *((base + 0x193b0) as *const usize);
    let count = *((base + 0x193b8) as *const u64) as usize;
    if buckets == 0 || count == 0 {
        return None;
    }
    let h = handle as u64;
    let bucket = if count & (count - 1) == 0 {
        (h & (count as u64 - 1)) as usize
    } else {
        (h % count as u64) as usize
    };
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        return None;
    }
    let mut node = *(before as *const usize);
    let mut guard = 0;
    while node != 0 && guard < 4096 {
        guard += 1;
        if *((node + 8) as *const u64) == h && *((node + 0x10) as *const i32) == handle as i32 {
            return Some(*((node + 0x14) as *const i32) as u32);
        }
        node = *(node as *const usize);
    }
    None
}

// Resource-service loaders (smashline `utils::load_file`, verified as 13.0.4 prologues).
// A donor effect FOLDER whose fighter/assist isn't in the match has no resident files, so
// `load_effects` returns 0. Adding the folder's `ef_*.eff` FILE to the res service makes
// the game decompress it; the folder load then succeeds a frame or two later.
const ENSURE_DIR_LOADED_OFFSET: usize = 0x35407a0;
const FILESYSTEM_INFO_OFFSET: usize = 0x5331f20;

// The real directory get-or-load (decompiled from 13.0.4): if the arc DirInfo group at
// `dir_index` is already loaded it just refcounts it; otherwise it kicks off the actual
// load (marks the entry loading + enqueues its files to the res loading thread). This is
// what makes an ABSENT donor character's effect folder resident — `add_to_res_service`
// (0x3540450) only refcounts an already-loaded file, so it never triggered a read.
#[skyline::from_offset(ENSURE_DIR_LOADED_OFFSET)]
fn ensure_dir_loaded(filesystem: *mut u64, dir_index: u32) -> u64;

/// The REAL dir-load ENQUEUE (FUN_03540860). `ensure_dir_loaded` only calls this when the dir
/// descriptor's `+8 & 1 == 0` (not loaded); our alucard dir has `+8 = 0x01` (bit0 set) so it
/// short-circuits to a refcount and NEVER enqueues — which is exactly why PURE_GAME_LOADER never
/// completed. Calling 3540860 DIRECTLY forces the enqueue: it marks the descriptor loading and
/// queues the dir's files onto the async res worker, which loads them through the proper pipeline
/// (real resource handles → set readiness → state advance → texture upload). Must be called under
/// the service lock (ensure_dir_loaded wraps it in 39c1490/39c14a0 on `*service`).
#[skyline::from_offset(0x3540860)]
fn enqueue_dir_load(filesystem: *mut u64, dir_index: u32) -> u64;
#[skyline::from_offset(0x39c1490)]
fn res_svc_lock(mutex: u64);
#[skyline::from_offset(0x39c14a0)]
fn res_svc_unlock(mutex: u64);

/// Force the REAL async load of a dir (bypassing ensure_dir_loaded's "already loaded" refcount
/// short-circuit) by calling the enqueue directly under the service lock. Returns the descriptor.
unsafe fn force_enqueue_dir(dir_index: u32) -> u64 {
    let svc = filesystem();
    if svc.is_null() {
        return 0;
    }
    let mutex = *(svc as *const u64); // *service = the lock object ensure_dir_loaded locks
    res_svc_lock(mutex);
    let r = enqueue_dir_load(svc, dir_index);
    res_svc_unlock(mutex);
    r
}

/// OBSERVE the game's OWN dir-load requests. When something loads mid-match successfully (an
/// Assist Trophy / Poké Ball summon), the game calls this — capturing the dir_index + the
/// returned group's state byte reveals the working load path to replicate. Logs to a
/// dedicated file so it survives. Suppress our own calls (IN_DONOR_LOAD) to isolate the game's.
/// Global monotonic sequence across the load + dir hooks, so the assist-summon load ORDER can
/// be reconstructed (which dirs go resident right before load_effects(idx=69)).
pub static TRACE_SEQ: AtomicU64 = AtomicU64::new(0);
/// Append a line to the assist-summon trace with a shared sequence number.
fn tlog(s: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_assisttrace.txt")
    {
        let _ = writeln!(f, "{s}");
    }
}

#[skyline::hook(offset = ENSURE_DIR_LOADED_OFFSET)]
fn hook_ensure_dir_loaded(filesystem: *mut u64, dir_index: u32) -> u64 {
    // SPAWN-CHAIN CAPTURE (build bc): when the alucard assist is summoned it requests its effect
    // dirs (4801 + deps) from the SPAWN context — a backtrace here reveals the object-spawn call
    // chain we need to replicate to spawn a carrier programmatically. Capture BEFORE original so
    // the spawn frames are on the stack. One-shot per dir, only mid-match, only for alucard's dirs.
    if !IN_DONOR_LOAD.load(Ordering::Relaxed)
        && crate::slight::agent_extender::driver_has_ticked()
        && matches!(dir_index, 4801 | 17736 | 13526 | 14439)
    {
        static SEEN: AtomicU64 = AtomicU64::new(0);
        let bit = 1u64 << (dir_index % 60);
        if SEEN.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            let bt = unsafe { capture_backtrace() };
            let chain: Vec<String> = bt.iter().map(|o| format!("{o:#x}")).collect();
            tlog(&format!(
                "SPAWN_CHAIN dir={dir_index} stack=[{}]",
                chain.join(", ")
            ));
        }
    }
    let r = original!()(filesystem, dir_index);
    if !IN_DONOR_LOAD.load(Ordering::Relaxed) {
        let state = if r != 0 {
            unsafe { *((r as usize + 0x08) as *const u64) }
        } else {
            0
        };
        // ASSIST TRACE: log EVERY mid-match dir load with a shared sequence number so the
        // summon's residency burst can be correlated to its load_effects(idx=69) call.
        if crate::slight::agent_extender::driver_has_ticked() {
            static TN: AtomicU64 = AtomicU64::new(0);
            if TN.fetch_add(1, Ordering::Relaxed) < 20000 {
                let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed);
                tlog(&format!(
                    "seq={seq} DIR dir_index={dir_index} ret={r:#x} state8={state:#x}"
                ));
            }
        }
        use std::io::Write;
        static N: AtomicU64 = AtomicU64::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 4000 {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("sd:/effect_viewer_dirload.txt")
            {
                let _ = writeln!(
                    f,
                    "GAME ensure_dir dir_index={dir_index} ret={r:#x} state8={state:#x}"
                );
            }
        }
    }
    r
}

/// The effect-set resource-readiness check `FUN_000987f0(out, set, .., handle_a, handle_b, ..)`
/// — the 0→1 state transition (@0x93228) calls it; if it writes 0x10c08 (not-ready) into `*out`
/// the set stays at state 0 (never renders). Not-ready needs: `896a0(set+0x608)` busy, OR either
/// resource handle's `+0x18` data == 0 (async loader hasn't completed). We OBSERVE it for ONLY
/// our co-loaded set (COLOAD_TICK_SET): if this hook FIRES for our set, the set IS being ticked
/// (fix = satisfy readiness); if it NEVER fires, the set is never ticked (fix = activate the slot
/// in the manager's per-frame list). Either way the handle `+0x18` values pinpoint the stuck
/// resource. Observe-only — we call original and never alter the result.
#[skyline::hook(offset = 0x987f0)]
fn hook_readiness(
    out: *mut i32,
    set: *mut u64,
    p3: u64,
    p4: u64,
    p5: u64,
    handle_a: usize,
    handle_b: usize,
    p8: u64,
) {
    original!()(out, set, p3, p4, p5, handle_a, handle_b, p8);
    // Global liveness counter — proves the hook fires at all (so a zero co-loaded-set count is a
    // real "never ticked", not a dead hook). Logged sparsely.
    {
        static G: AtomicU64 = AtomicU64::new(0);
        let g = G.fetch_add(1, Ordering::Relaxed);
        if g == 1 || g == 2000 || g == 20000 {
            dlog(&format!("READY_CHK_GLOBAL fired total={}", g + 1));
        }
    }
    let target = COLOAD_TICK_SET.load(Ordering::Relaxed);
    if target != 0 && set as usize == target {
        static N: AtomicU64 = AtomicU64::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 80 {
            unsafe {
                let result = *out;
                let busy610 = *((set as usize + 0x610) as *const u32);
                let a18 = if handle_a != 0 {
                    *((handle_a + 0x18) as *const u64)
                } else {
                    0
                };
                let b18 = if handle_b != 0 {
                    *((handle_b + 0x18) as *const u64)
                } else {
                    0
                };
                let state20 = *((set as usize + 0x20) as *const u8);
                let setu = set as usize;
                dlog(&format!(
                    "READY_CHK set={setu:#x} state20=0x{state20:02x} result={result:#x} busy610={busy610:#x} ha={handle_a:#x} ha+18={a18:#x} hb={handle_b:#x} hb+18={b18:#x}"
                ));
            }
        }
    }
}

fn filesystem() -> *mut u64 {
    let text = unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as *mut u8 };
    unsafe { *text.add(FILESYSTEM_INFO_OFFSET).cast::<*mut u64>() }
}

/// Request an effect FOLDER (by path hash) be made resident via the game's own directory
/// loader. Async: the files decompress over the next frames; the caller retries
/// `load_effects` until it returns 1. Returns the DirInfo index used, or None if the arc
/// has no such directory group.
fn request_dir_load(folder_hash: u64) -> Option<u32> {
    use crate::slight::effect_viewer::resource_reload as rr;
    let fs = filesystem();
    if fs.is_null() {
        mark("dirload_no_fs");
        return None;
    }
    let dir_index = rr::dir_info_index_for_path_hash(folder_hash);
    mark(&format!(
        "dirload folder={folder_hash:#x} dir_index={dir_index:?}"
    ));
    let dir_index = dir_index?;
    IN_DONOR_LOAD.store(true, Ordering::Relaxed); // so the observe-hook skips our own call
    let ret = unsafe { ensure_dir_loaded(fs, dir_index) };
    IN_DONOR_LOAD.store(false, Ordering::Relaxed);
    // Dump the returned dir-group object's state so we can tell enqueued-load vs no-op
    // refcount. Per the loader RE: a DirInfo/group has a state byte (5 = loading); the
    // enqueue also sets a dirty byte the res thread polls. If the state shows already-
    // loaded/idle, ensure_dir_loaded refcounted WITHOUT queuing our load → we must force
    // the enqueue (FUN_03540860) rather than drain the queue.
    if ret != 0 {
        let words: String = (0..12)
            .map(|i| {
                let w = unsafe { *((ret as usize + i * 8) as *const u64) };
                format!("+{:#04x}={w:#x} ", i * 8)
            })
            .collect();
        mark(&format!(
            "dirload_ensure ret={ret:#x} dir_index={dir_index}\n  group: {words}"
        ));
    } else {
        mark(&format!("dirload_ensure ret=0 dir_index={dir_index}"));
    }
    Some(dir_index)
}

/// donor ef path hashes we've registered an Arcropolis serve for (so we register once).
static DONOR_SERVED: LazyLock<Mutex<std::collections::HashSet<u64>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Arcropolis generic (disk) callback: when the game's loader reads a donor eff, hand it
/// our editor-supplied STRIPPED bytes. This is the SAFE mechanism (no raw memory writes —
/// which froze the game) and it's the same path the live-eff serving proved works.
extern "C" fn donor_serve_cb(
    hash: u64,
    out: *mut u8,
    capacity: usize,
    out_size: &mut usize,
) -> bool {
    let bytes = match DONOR_BYTES.lock().get(&hash) {
        Some(b) if b.len() <= capacity => b.clone(),
        _ => return false,
    };
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    *out_size = bytes.len();
    crate::slight::diag::note(format!("donor served {} B for {hash:#x}", bytes.len()));
    mark(&format!(
        "donor_served hash={hash:#x} bytes={}",
        bytes.len()
    ));
    true
}

/// Register the Arcropolis serve for a donor eff (once), so the loader reads OUR stripped
/// bytes instead of failing on absent/DLC data. Returns true once registered (bytes present).
fn register_donor_serve(ef_file: &str) -> bool {
    let hash = smash::hash40(&ef_file.to_lowercase());
    if DONOR_SERVED.lock().contains(&hash) {
        return true;
    }
    let size = match DONOR_BYTES.lock().get(&hash) {
        Some(b) if b.len() >= 4 && &b[..4] == b"EFFN" => b.len(),
        Some(_) => {
            DONOR_BYTES.lock().remove(&hash);
            return false;
        }
        None => return false, // bytes not here yet
    };
    if crate::slight::effect_viewer::arcrop::register_disk(hash, size, donor_serve_cb) {
        DONOR_SERVED.lock().insert(hash);
        crate::slight::diag::note(format!("donor serve registered: {ef_file} ({size} B)"));
        mark(&format!("donor_serve_registered {ef_file} {size}"));
        true
    } else {
        false
    }
}

/// file-path hash → leaked buffer pointer (reused across retries).
static LEAKED_DONORS: LazyLock<Mutex<HashMap<u64, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// INSTRUMENTED direct injection (experimental live path). Leaks the donor bytes once and
/// tries to fill the game's data slot. Every memory step writes an immediate-flush marker
/// to sd:/effect_viewer_inject.txt so a freeze pinpoints the exact faulting operation.
fn inject_donor(
    ef_file: &str,
) -> Option<crate::slight::effect_viewer::resource_reload::FillResult> {
    use crate::slight::effect_viewer::resource_reload as rr;
    let hash = smash::hash40(&ef_file.to_lowercase());
    let ptr: *const u8 = {
        if let Some(&p) = LEAKED_DONORS.lock().get(&hash) {
            p as *const u8
        } else {
            let bytes: Vec<u8> = DONOR_BYTES.lock().get(&hash).cloned()?;
            if bytes.len() < 4 || &bytes[..4] != b"EFFN" {
                return None;
            }
            let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            LEAKED_DONORS.lock().insert(hash, leaked.as_ptr() as usize);
            leaked.as_ptr()
        }
    };
    mark(&format!("inject_call {ef_file} hash={hash:#x}"));
    Some(rr::inject_resident_buffer(hash, ptr))
}

fn effect_manager() -> *mut u64 {
    let text = unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as *mut u8 };
    // Two-level singleton: the static slot holds a pointer that is NULL until the game
    // constructs the manager. Deref in two guarded steps — a combined `**` here crashed
    // the game at BOOT when reload ran with a manifest present.
    unsafe {
        let holder = *text.add(EFFECT_MANAGER_OFFSET).cast::<*mut *mut u64>();
        if holder.is_null() {
            return std::ptr::null_mut();
        }
        *holder
    }
}

fn path_hash_from_search_index(search_index: u32) -> Option<u64> {
    crate::slight::effect_viewer::resource_reload::path_hash_for_search_index(search_index)
}

fn remember_handle(path_hash: u64, handle: u32) {
    if path_hash == 0 {
        return;
    }
    let mut known = KNOWN_HANDLES.lock();
    let handles = known.entry(path_hash).or_default();
    if !handles.contains(&handle) {
        handles.push(handle);
    }
}

fn track_slot(handle: u32, search_index: u32, result: u32) {
    let path_hash = path_hash_from_search_index(search_index).unwrap_or(0);
    ACTIVE_SLOTS.lock().insert(
        handle,
        EffectSlot {
            search_index,
            path_hash,
            result,
        },
    );
    remember_handle(path_hash, handle);
}

fn untrack_slot(handle: u32) {
    ACTIVE_SLOTS.lock().remove(&handle);
}

/// Scan the raw stack for text-range return addresses whose preceding instruction is a BL —
/// i.e. genuine call-return sites. Works without a frame-pointer chain (which SSBU omits here).
/// Reveals the GAME callers (e.g. the assist-summon orchestrator that calls load_effects).
#[inline(never)]
unsafe fn capture_backtrace() -> Vec<usize> {
    let text = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize;
    let rodata = skyline::hooks::getRegionAddress(skyline::hooks::Region::Rodata) as usize;
    let mut sp: usize;
    std::arch::asm!("mov {}, sp", out(reg) sp);
    let mut out = Vec::new();
    let mut last = 0usize;
    for i in 0..768 {
        let v = *((sp + i * 8) as *const usize);
        if v >= text + 4 && v < rodata {
            // A return address points just AFTER a BL/BLR. Check the prior instruction.
            let prev = *((v - 4) as *const u32);
            let is_bl = (prev & 0xFC00_0000) == 0x9400_0000; // BL imm26
            let is_blr = (prev & 0xFFFF_FC1F) == 0xD63F_0000; // BLR reg
            if (is_bl || is_blr) && v != last {
                out.push(v - text);
                last = v;
                if out.len() >= 48 {
                    break;
                }
            }
        }
    }
    out
}

#[skyline::hook(offset = LOAD_EFFECTS_OFFSET)]
fn hook_load_effects(manager: *mut u64, handle: u32, search_index: &u32) -> u32 {
    LOAD_HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
    // Capture the GAME's call stack when it loads the alucard assist (idx=69) mid-match — the
    // return addresses ARE the summon orchestrator we need to replicate for our donor.
    if !IN_DONOR_LOAD.load(Ordering::Relaxed)
        && *search_index == 69
        && crate::slight::agent_extender::driver_has_ticked()
    {
        static BT_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !BT_DONE.swap(true, Ordering::Relaxed) {
            let bt = unsafe { capture_backtrace() };
            let chain: Vec<String> = bt.iter().map(|o| format!("{o:#x}")).collect();
            tlog(&format!(
                "ASSIST_LOAD_CALLERS handle={handle} idx=69 stack=[{}]",
                chain.join(", ")
            ));
        }
    }
    // Mark the GAME's own alucard (idx=69) load so hook_build_effect_set logs the a4d90 call it
    // makes INSIDE original — that's the working reference to diff our co-load's a4d90 args against.
    let mark_game_alucard = !IN_DONOR_LOAD.load(Ordering::Relaxed) && *search_index == 69;
    if mark_game_alucard {
        GAME_ALUCARD_LOADING.store(true, Ordering::Relaxed);
    }
    let result = original!()(manager, handle, search_index);
    if mark_game_alucard {
        GAME_ALUCARD_LOADING.store(false, Ordering::Relaxed);
    }
    track_slot(handle, *search_index, result);
    // RELOAD PROBE (build bb): log every GAME fighter/effect load with its path name, so we can
    // SEE whether a training-mode RESET re-runs the blessed fighter eff-load (our candidate for an
    // automatic, menu-free reload trigger). If kirby's ef_kirby.eff reloads on reset, that's it.
    if !IN_DONOR_LOAD.load(Ordering::Relaxed) && crate::slight::agent_extender::driver_has_ticked()
    {
        let ph = path_hash_from_search_index(*search_index).unwrap_or(0);
        // hash40("effect/fighter/kirby/ef_kirby.eff") lets us spot kirby's eff reload specifically.
        let kirby = smash::hash40("effect/fighter/kirby/ef_kirby.eff");
        let tag = if ph == kirby { " <== KIRBY EFF" } else { "" };
        dlog(&format!(
            "GAME_EFF_LOAD idx={} result={result} path_hash={ph:#x}{tag}",
            *search_index
        ));
    }
    // ASSIST TRACE: log EVERY mid-match game load_effects with the shared sequence number, so we
    // can see exactly which dir-loads (residency) precede the assist's load_effects(idx=69).
    if !IN_DONOR_LOAD.load(Ordering::Relaxed) && crate::slight::agent_extender::driver_has_ticked()
    {
        let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed);
        tlog(&format!(
            "seq={seq} LOAD handle={handle} idx={} result={result}",
            *search_index
        ));
    }
    // Capture the GAME's alucard load (idx==69) MID-MATCH as the working reference — dump its set
    // right after load_effects returns: if +0x540 is already filled here, the game's load fills it
    // synchronously and the difference vs ours is purely the resident-buffer/sub-resource state.
    if !IN_DONOR_LOAD.load(Ordering::Relaxed)
        && result == 1
        && *search_index == 69
        && crate::slight::agent_extender::driver_has_ticked()
        && GAME_WATCH_SET.load(Ordering::Relaxed) == 0
    {
        unsafe {
            let set = set_object_for_handle(manager, handle);
            if set != 0 {
                GAME_WATCH_SET.store(set, Ordering::Relaxed);
                let st = *((set + 0x20) as *const u8);
                let f540 = *((set + 0x540) as *const u64);
                dlog(&format!("GAME_WATCH ALUCARD handle={handle} idx=69 set={set:#x} state=0x{st:02x} at+0x540={f540:#x}"));
                dump_set("GAME_at_load", set, 0x700);
                dump_emitter_chain("GAME_at_load", set);
            }
        }
    }
    // Piggy-back donor eff loads onto the target fighter's own effect load, so a
    // cross-fighter one-slot has its donor content resident from match start. Enqueue
    // only (the per-frame pump does the res-service load + retry) to avoid re-entrancy.
    if !IN_DONOR_LOAD.load(Ordering::Relaxed) {
        if let Some(path_hash) = path_hash_from_search_index(*search_index) {
            enqueue_donors_for(handle, path_hash);
        }
    }
    result
}

#[skyline::hook(offset = UNLOAD_EFFECTS_OFFSET)]
fn hook_unload_effects(manager: *mut u64, handle: u32) {
    // Donor resources ride on the target's handle — release them when it goes.
    let derived: Vec<u32> = DONORS_LOADED
        .lock()
        .remove(&handle)
        .map(|m| m.into_values().collect())
        .unwrap_or_default();
    for d in derived {
        unsafe { unload_effects(manager, d) };
    }
    original!()(manager, handle);
    untrack_slot(handle);
}

// ── Cross-fighter donor eff loading (smashline's "effect transplant" mechanism) ──
//
// `load_effects` takes CALLER-CHOSEN handles: smashline loads extra eff files for a
// fighter with `fighter_handle + k * 2000` (see HDR smashline effects.rs). We do the
// same for one-slot donors — the donor fighter's VANILLA eff becomes resident, and the
// kind alias renders the copy through it. No file serving involved (which the game's
// loader bypasses entirely on this Eden setup).

/// One target eff (hash40 of its arc path) → donor eff arc paths to co-load.
#[derive(serde::Deserialize)]
pub struct DonorSpec {
    pub target: String,
    pub donors: Vec<String>,
}

/// target path hash → donor paths.
/// target folder hash → donor eff FILE paths (e.g. "effect/fighter/pickel/ef_pickel.eff").
static DONOR_SPECS: LazyLock<Mutex<HashMap<u64, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// target handle → (donor folder hash → derived handle) fully loaded (result==1).
static DONORS_LOADED: LazyLock<Mutex<HashMap<u32, HashMap<u64, u32>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// donor eff FILE path hash → stripped eff bytes supplied by the editor (arcrop_load_file
/// can't read vanilla donor files, so the editor sends the bytes to inject as resident data).
static DONOR_BYTES: LazyLock<Mutex<HashMap<u64, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Overwrite the immediate-flush freeze marker: the LAST value written names the last step
/// reached before a hang (diag notes are ring-buffered and lost on freeze; this isn't).
pub fn mark(step: &str) {
    let _ = std::fs::write("sd:/effect_viewer_inject.txt", format!("step={step}\n"));
    // Full trail (append): inject.txt only keeps the LAST step (freeze pinpoint); the trail
    // preserves the whole chain for post-run analysis. Truncated at each reread start.
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_marks.txt")
    {
        let _ = writeln!(f, "{step}");
    }
}

/// Store editor-supplied stripped donor buffers (keyed by donor eff FILE path).
pub fn set_donor_bytes(list: Vec<(String, Vec<u8>)>) {
    let sizes: Vec<usize> = list.iter().map(|(_, b)| b.len()).collect();
    mark(&format!(
        "recv_donor_bytes count={} sizes={sizes:?}",
        list.len()
    ));
    let mut map = DONOR_BYTES.lock();
    for (path, bytes) in list {
        map.insert(smash::hash40(&path.to_lowercase()), bytes);
    }
    crate::slight::diag::note(format!("donor bytes: {} buffer(s) staged", map.len()));
    mark(&format!("staged_donor_bytes count={}", map.len()));
}

/// Minimal standard-alphabet base64 decoder (no crate dep in the plugin build).
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}
/// Re-entrancy guard: our own load/unload calls re-enter the hooks above.
static IN_DONOR_LOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A donor whose files we've asked the res service to load; we retry `load_effects` each
/// frame until it returns 1 (files resident) or the retry budget runs out.
#[derive(Clone)]
struct PendingDonor {
    target_handle: u32,
    folder_hash: u64,
    folder_path: String,
    ef_file: String,
    derived: u32,
    /// Kicked the directory loader (allocates the empty data slot we fill).
    dir_requested: bool,
    /// Our bytes are in the slot — safe to call load_effects now.
    filled: bool,
    /// One-time drain-drive done (schedules dep dirs + drains the queue in-context).
    drain_done: bool,
    /// One-time sub-resource fill done (arcrop-fills the dir's textures/models).
    subres_done: bool,
    /// Consecutive arcrop read-misses on the .eff (bytes not served) → give up (stale donor).
    read_misses: u32,
    /// Forced the REAL async dir-load enqueue (FUN_03540860) — one-shot.
    enqueue_forced: bool,
    tries: u32,
}

/// Donors awaiting residency (retried on the game thread by [`pump_donor_queue`]).
static PENDING_DONORS: LazyLock<Mutex<Vec<PendingDonor>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// After a donor co-load succeeds: hash40(`name_os`) → hash40(`name`) for its entries, so the
/// effect hook can rewrite a merged-`_os` spawn onto the REAL co-loaded kind (GPU-valid).
pub static CO_LOADED_REMAP: LazyLock<Mutex<HashMap<u64, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Co-loaded set object to drive per-frame (its update = vtable+0x70 runs the state machine
/// that does GPU texture setup), and how many frames left to tick it.
pub static COLOAD_TICK_SET: AtomicUsize = AtomicUsize::new(0);
pub static COLOAD_TICK_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Called from the game-thread frame pump: tick the co-loaded set's update so its readiness
/// state machine advances (0→1→2→3) and processes textures. Logs the state each frame.
/// Drive our co-loaded set's update ourselves. CONFIRMED to FREEZE (build au): forcing state=3
/// then calling the update hangs in 9a100's texture-stream loop (the intermediate states 1→2 that
/// set up the stream were skipped); driving at state 0 hangs waiting on the not-ready resource
/// (build x). The set must advance NATURALLY via the manager tick once its resource is ready — not
/// driven by us. Kept OFF; the readiness path is the live investigation.
const DRIVE_SET_UPDATE: bool = false;

pub fn pump_coload_tick() {
    // One-shot: dump the assist-orchestrator text windows from live memory for offline disasm.
    dump_text_windows();
    let set = COLOAD_TICK_SET.load(Ordering::Relaxed);
    let frames = COLOAD_TICK_FRAMES.load(Ordering::Relaxed);
    if set == 0 || frames == 0 {
        return;
    }
    if !DRIVE_SET_UPDATE {
        // Safe monitor only: read the state byte occasionally (never call the update — it hangs).
        static F: AtomicU64 = AtomicU64::new(0);
        let n = F.fetch_add(1, Ordering::Relaxed);
        if n % 60 == 0 && n < 60 * 60 {
            unsafe {
                let st = *((set + 0x20) as *const u8);
                let gw = GAME_WATCH_SET.load(Ordering::Relaxed);
                let gst = if gw != 0 {
                    *((gw + 0x20) as *const u8)
                } else {
                    0xff
                };
                dlog(&format!(
                    "coload_monitor f={n} ours_state=0x{st:02x} game_state=0x{gst:02x}"
                ));
            }
        }
        return;
    }
    unsafe {
        let vt = *(set as *const usize);
        if vt < 0x1000 {
            COLOAD_TICK_SET.store(0, Ordering::Relaxed);
            COLOAD_TICK_FRAMES.store(0, Ordering::Relaxed);
            return;
        }
        // THE FIX (build at): the GRTF texture upload FUN_0009a100 is gated by
        // FUN_00093f10(set) = `*(set+0x20) == 3`. Our co-loaded set is created under a synthetic
        // handle that the manager never enrolls in its per-frame tick list, so its state machine
        // never advances past 0 → 9a100 always skips → the effect's textures (ef_cmn_* on the 2nd
        // emitter) stay empty → invisible. FORCE the state byte to 3, THEN drive the set update
        // (vtable+0x70) once: with the gate satisfied it runs the texture-upload path directly.
        // (Driving the update at state 0 is what froze build x — it loops waiting on the async
        // loader; jumping straight to 3 skips that wait.) One drive per frame for a few frames in
        // case the upload needs multiple passes, then stop.
        let st_before = *((set + 0x20) as *const u8);
        *((set + 0x20) as *mut u8) = 3;
        let update: extern "C" fn(usize) = std::mem::transmute(*((vt + 0x70) as *const usize));
        update(set);
        // Did the previously-empty 2nd-emitter texture (+0x4e0) get real data now?
        let t4e0 = *((set + 0x4e0) as *const usize);
        let t4e0_w0 = if t4e0 > 0x1000_0000 && t4e0 < 0x20_0000_0000 {
            *(t4e0 as *const u64)
        } else {
            0
        };
        let st_after = *((set + 0x20) as *const u8);
        let n = 6 - frames.min(6);
        if n < 6 {
            dlog(&format!(
                "coload_tick_force f={n} st 0x{st_before:02x}->0x{st_after:02x} tex4e0={t4e0:#x} w0={t4e0_w0:#x} {}",
                if t4e0_w0 != 0 { "*** TEX UPLOADED ***" } else { "(tex still empty)" }
            ));
        }
        // Drive only a handful of frames (not 240) to limit risk, then stop.
        let left = frames.saturating_sub(1).min(5);
        COLOAD_TICK_FRAMES.store(left, Ordering::Relaxed);
    }
}

/// Resolve a requested effect kind to the real co-loaded donor kind, if one exists.
pub fn coload_remap(hash: u64) -> Option<u64> {
    let r = CO_LOADED_REMAP
        .lock()
        .get(&(hash & 0xff_ffff_ffff))
        .copied();
    if let Some(to) = r {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_donorfill.txt")
        {
            let _ = writeln!(f, "coload_remap_fire {hash:#x} -> {to:#x}");
        }
    }
    r
}

/// True if a hash is a co-loaded REAL donor kind (a remap value) — so the spawn probe can
/// log its req result and confirm the real kind renders.
pub fn is_coloaded_kind(hash: u64) -> bool {
    let h = hash & 0xff_ffff_ffff;
    CO_LOADED_REMAP.lock().values().any(|v| *v == h)
}

/// Current size of the co-load remap (diagnostic: is it populated when a req is remapped?).
pub fn coload_map_size() -> usize {
    CO_LOADED_REMAP.lock().len()
}

/// One-shot: dump raw instruction bytes around the assist-orchestrator return addresses from
/// LIVE text memory (Ghidra/capstone-on-file both fail on this segment), for offline capstone.
pub fn dump_text_windows() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let text = unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize };
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("sd:/effect_viewer_textdump.txt")
    else {
        return;
    };
    let _ = writeln!(f, "text_base={text:#x}");
    // (off, bytes_before, bytes_after): the full direct-caller function + the res-service
    // increment/decrement funcs, to see the service object (x22) + the residency call recipe.
    for &(off, before, after) in &[
        (0x35636e8usize, 0x260usize, 0x40usize), // full direct load_effects caller function
        (0x3540450, 0x10, 0x140),                // fn_3540450 res-service refcount INCREMENT
        (0x3540560, 0x10, 0x120),                // res_dec DECREMENT
    ] {
        let start = off.saturating_sub(before);
        let end = off + after;
        let _ = writeln!(f, "\n=== WINDOW off={off:#x} [{start:#x}..{end:#x}] ===");
        let mut a = start;
        while a < end {
            let w = unsafe { *((text + a) as *const u32) };
            let _ = writeln!(f, "{a:#09x}: {w:08x}");
            a += 4;
        }
    }
    let _ = f.flush();
}
/// Long window (~60s) to test whether the game's OWN async resource worker ever completes a
/// mid-match folder load (vs our manual byte-fill, which bypasses the resource-handle setup).
const DONOR_MAX_TRIES: u32 = 600;
/// When true, do NOT manually fill the resident slot — let the game's real loader do it, so
/// resource handles get set up properly (the readiness check the render gate needs). Tests
/// "reuse the game's loader" end-to-end. Build aw2: now paired with force_enqueue_dir (the REAL
/// FUN_03540860 enqueue) which the old ensure_dir_loaded refcount-short-circuit was skipping — so
/// this finally exercises the true async pipeline. Poll whether the worker makes the eff resident.
const PURE_GAME_LOADER: bool = false;
/// ASSIST-STYLE co-load (build 2026-07-21ac): replicate the exact assist-summon recipe traced
/// on-device — call `ensure_dir_loaded(dir)` each try then `load_effects(handle, idx)` IMMEDIATELY,
/// with NO arcrop pre-fill and NO wait-for-residency. The assist's own load_effects fills the
/// +0x540 parametric block synchronously (our arcrop pre-fill was PREVENTING load_effects from
/// doing its own sub-resource load). If load_effects still returns 0 after ASSIST_STYLE_TRIES,
/// fall back to the arcrop pre-fill path (so we never regress to no-registration).
const ASSIST_STYLE: bool = false;
const ASSIST_STYLE_TRIES: u32 = 90;
/// Combined crash cap for co-loaded donor buffers. The per-donor 4 MB cap stopped ONE huge
/// donor overflowing the effect heap, but several simultaneous donors (a stale ridley fighter-eff
/// + bomberman + the real one) still summed past the heap and crashed. Once total game_effect_alloc
/// for donor fills crosses this, skip further fills rather than crash. Tracks bytes actually taken.
const COLOAD_COMBINED_CAP: usize = 16 * 1024 * 1024;
static COLOAD_TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Actively DRIVE the queue-drain from our pump. Build ak proved this HANGS the game: the
/// residency wrapper spins waiting for the async worker while we block the game thread it needs,
/// and 0x354a120 itself fired only once/session (not a per-frame pump). Kept off — the observe
/// hook still captures the object + fire rate. Re-enable only with a non-blocking scheme.
const DRAIN_DRIVE: bool = false;
/// RENDER PUSH v2 (build am): after the donor `.eff` is resident, arcrop-fill EACH of its dir's
/// SUB-resource files (textures/models) the same proven, synchronous, non-blocking way — so
/// load_effects builds the set with everything resident and can do its own GPU upload. Bounded by
/// the combined heap cap. No worker, no drain, no blocking (unlike build ak, which hung).
const SUBRES_FILL: bool = true;

/// Work the TCP thread may NOT do itself: `load_effects` must run on the game's thread
/// (the game only ever calls it from its loading/main threads — a foreign-thread call
/// "succeeds" but its async resource completion never happens). Drained per frame.
static DONOR_QUEUE: LazyLock<Mutex<Vec<(u32, u64)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Effect resources are loaded per FOLDER: decompiled 13.0.4 `load_effects` validates a
/// directory flag on the search entry (and resolves the folder's children + its `trail`
/// sibling); an eff FILE index fails with result 0. The game's own loads confirm it —
/// e.g. kirby loads under handle 774 = search index of "effect/fighter/kirby".
fn eff_dir(path: &str) -> &str {
    if path.ends_with(".eff") {
        path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(path)
    } else {
        path
    }
}

/// Full-list replace over TCP; queues donor co-loads for targets already in match
/// (executed on the game thread by [`pump_donor_queue`]). Keys + donors are normalized
/// to effect FOLDERS (the unit load_effects actually works in).
pub fn set_donor_specs(specs: Vec<DonorSpec>) {
    {
        let mut map = DONOR_SPECS.lock();
        map.clear();
        for s in specs {
            let target_dir = eff_dir(&s.target.to_lowercase()).to_string();
            // Keep the donor as its ef FILE path: the res-service preload needs the file,
            // load_effects needs the folder (derived at apply time).
            let donors = s.donors.iter().map(|d| d.to_lowercase()).collect();
            map.insert(smash::hash40(&target_dir), donors);
        }
        crate::slight::diag::note(format!("donor eff specs: {} target(s)", map.len()));
    }
    let active: Vec<(u32, u64)> = ACTIVE_SLOTS
        .lock()
        .iter()
        .map(|(h, s)| (*h, s.path_hash))
        .collect();
    if !active.is_empty() {
        *DONOR_QUEUE.lock() = active;
    }
}

/// Game-thread pump (called from the per-frame agent line callback): apply queued donor
/// co-loads AND retry pending donors whose res-service files are still decompressing.
pub fn pump_donor_queue() {
    let manager = effect_manager();
    if manager.is_null() {
        return;
    }
    // 1. New targets: enqueue their donors as pending (res-service load kicks off).
    let fresh: Vec<(u32, u64)> = {
        match DONOR_QUEUE.try_lock() {
            Some(mut q) if !q.is_empty() => std::mem::take(&mut *q),
            _ => Vec::new(),
        }
    };
    for (handle, path_hash) in fresh {
        enqueue_donors_for(handle, path_hash);
    }
    // 2. Retry residency-pending donors (bounded work per frame is fine — the list is tiny).
    retry_pending_donors(manager);
}

/// Turn a target's donor specs into PendingDonor entries (skipping already-loaded ones).
/// CARRIER MODE (build ay): stop doing our OWN co-load (it registers a competing set but can never
/// render — its resource handles never become ready, texture upload gated on state 3 never runs).
/// Instead: (1) register the Arcropolis serve so when a real game OBJECT (the carrier, e.g. the
/// alucard assist) loads the donor's effect dir it reads OUR bytes through the BLESSED pipeline
/// (proper handles + textures), and (2) build the `_os`→real remap from the donor names so kirby's
/// merged spawn redirects onto the carrier's properly-loaded kind. The carrier's lifecycle (game-
/// managed) handles load AND unload — no manual refcount fiddling. This is the only mechanism
/// proven to load a foreign effect's resources mid-match.
const CARRIER_MODE: bool = false;
/// BAKE-ONLY (build ba): do NO donor co-load and NO carrier redirect at all. Rely purely on the
/// editor's MERGED eff (donor entries + textures baked into the fighter's own eff, served via
/// live_eff) which the fighter's OWN blessed load picks up at fighter-load — the proven path that
/// renders correctly. This restores normal assist spawning (we no longer hijack any dir) and gives
/// a clean baseline to confirm cross-fighter one-slots render on a fresh match entry, before we
/// tackle triggering that reload automatically.
const BAKE_ONLY: bool = true;
/// The carrier object's effect dir path we hijack: the user summons the Alucard assist, and we
/// serve the DONOR's bytes under alucard's eff path — so the carrier's blessed load pulls the
/// DONOR's effect (proper handles/textures) even though the donor is a different character. This
/// proves the redirect (carrier ≠ effect) before building a dedicated/custom carrier.
const CARRIER_EFF_PATH: &str = "effect/assist/alucard/ef_alucard.eff";

/// Build the `hash(name_os) → hash(name)` redirect remap from a served donor eff's entry names,
/// without co-loading. Lets the carrier's own load provide the real (renderable) kinds.
fn build_remap_from_served(ef_file: &str) {
    let hash = smash::hash40(&ef_file.to_lowercase());
    let bytes = match DONOR_BYTES.lock().get(&hash) {
        Some(b) if b.len() >= 0x10 && &b[..4] == b"EFFN" => b.clone(),
        _ => return,
    };
    let names = unsafe { eff_entry_names(bytes.as_ptr()) };
    let mut remap = CO_LOADED_REMAP.lock();
    let mut n = 0;
    for (name, _) in &names {
        let lo = name.to_lowercase();
        remap.insert(smash::hash40(&format!("{lo}_os")), smash::hash40(&lo));
        n += 1;
    }
    dlog(&format!("carrier_remap built {n} entries from {ef_file}"));
}

fn enqueue_donors_for(target_handle: u32, target_path_hash: u64) {
    use crate::slight::effect_viewer::resource_reload as rr;
    if BAKE_ONLY {
        // No co-load, no carrier hijack — the merged eff (live_eff) renders at fighter load.
        return;
    }
    let donors = match DONOR_SPECS.lock().get(&target_path_hash) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return,
    };
    for ef_file in donors {
        let folder = eff_dir(&ef_file).to_string();
        let folder_hash = smash::hash40(&folder);
        if CARRIER_MODE {
            // Serve the DONOR's bytes under the CARRIER's eff path, so when the user summons the
            // carrier (alucard assist) its blessed load pulls the donor's effect. Then build the
            // donor's `_os`→real remap so kirby's spawn lands on the carrier-loaded kind. Do NOT
            // co-load ourselves. (carrier ≠ effect test; also the basis for a custom carrier.)
            let donor_hash = smash::hash40(&ef_file.to_lowercase());
            let carrier_hash = smash::hash40(CARRIER_EFF_PATH);
            let bytes = DONOR_BYTES.lock().get(&donor_hash).cloned();
            if let Some(bytes) = bytes {
                let n = bytes.len();
                DONOR_BYTES.lock().insert(carrier_hash, bytes);
                DONOR_SERVED.lock().remove(&carrier_hash); // allow (re)register at donor size
                let ok = register_donor_serve(CARRIER_EFF_PATH);
                build_remap_from_served(&ef_file);
                dlog(&format!("carrier_redirect serve {ef_file} ({n} B) UNDER {CARRIER_EFF_PATH} registered={ok}"));
            } else {
                dlog(&format!("carrier_redirect NO bytes for {ef_file} yet"));
            }
            continue;
        }
        if DONORS_LOADED
            .lock()
            .get(&target_handle)
            .is_some_and(|m| m.contains_key(&folder_hash))
        {
            continue;
        }
        if rr::search_index_for_path_hash(folder_hash).is_none() {
            crate::slight::diag::note(format!("donor eff folder not in arc: {folder}"));
            continue;
        }
        let mut pend = PENDING_DONORS.lock();
        if pend
            .iter()
            .any(|p| p.target_handle == target_handle && p.folder_hash == folder_hash)
        {
            continue;
        }
        let count = DONORS_LOADED
            .lock()
            .get(&target_handle)
            .map(|m| m.len() as u32)
            .unwrap_or(0)
            + pend
                .iter()
                .filter(|p| p.target_handle == target_handle)
                .count() as u32;
        // smashline derivation: fighter handle + k*2000.
        let derived = target_handle.wrapping_add((count + 1).wrapping_mul(2000));
        pend.push(PendingDonor {
            target_handle,
            folder_hash,
            folder_path: folder,
            ef_file,
            derived,
            dir_requested: false,
            filled: false,
            drain_done: false,
            subres_done: false,
            read_misses: 0,
            enqueue_forced: false,
            tries: 0,
        });
    }
}

/// Append-only donor-coload log (marks.txt gets wiped by each force_reread, hiding this path).
fn dlog(s: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_donorfill.txt")
    {
        let _ = writeln!(f, "{s}");
    }
}

/// A GAME-loaded set captured mid-match (e.g. an assist summon) — a WORKING reference whose
/// resource-readiness DOES advance. Diffing it against our stuck co-loaded set reveals the
/// missing field (resource handles / flags).
pub static GAME_WATCH_SET: AtomicUsize = AtomicUsize::new(0);

/// Hexdump `len` bytes of a set object + a few candidate resource-handle pointer fields, to
/// `sd:/effect_viewer_setdump.txt` (append). Used to diff a working game set vs our stuck one.
fn dump_set(label: &str, set: usize, len: usize) {
    use std::io::Write;
    if set == 0 {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_setdump.txt")
    else {
        return;
    };
    let _ = writeln!(f, "=== SET {label} @ {set:#x} (len {len}) ===");
    unsafe {
        // Candidate pointer fields the readiness path touched: +8 (handle), +0x20 (state byte),
        // +0x150 (tex register), +0x608/+0x610 (busy state), +0x6b8/+0x6d8 (handle_a/b slots per
        // 987f0 param_2+0xd7/+0xdb long-units = +0x6b8/+0x6d8).
        for off in [0x8usize, 0x150, 0x608, 0x610, 0x6b8, 0x6d8] {
            let v = *((set + off) as *const u64);
            let inner18 = if v > 0x1000_0000 && v < 0x2_0000_0000 {
                *((v as usize + 0x18) as *const u64)
            } else {
                0
            };
            let _ = writeln!(f, "  +{off:#05x} = {v:#018x}  (->+0x18={inner18:#x})");
        }
        for row in 0..(len / 16) {
            let base = set + row * 16;
            let mut line = format!("  {:04x}:", row * 16);
            for b in 0..16 {
                line.push_str(&format!(" {:02x}", *((base + b) as *const u8)));
            }
            let _ = writeln!(f, "{line}");
        }
        // Scan for libc++ std::string records {size, cap, data_ptr} in the resource region and
        // print their contents — these 39-char strings are populated in our co-load but zero in
        // a working assist set; likely UNRESOLVED resource file paths (models/textures) the
        // co-load never loaded → mesh emitters render nothing.
        let _ = writeln!(f, "  --- embedded strings (resource region) ---");
        let mut o = 0x100usize;
        while o + 0x18 <= len {
            let size = *((set + o) as *const u64);
            let cap = *((set + o + 8) as *const u64);
            let ptr = *((set + o + 0x10) as *const u64) as usize;
            if size >= 4
                && size < 0x100
                && cap >= size
                && cap < 0x200
                && ptr > 0x1000_0000
                && ptr < 0x20_0000_0000
            {
                // Read up to `size` bytes as text.
                let n = size.min(120) as usize;
                let mut s = String::new();
                let mut printable = true;
                for i in 0..n {
                    let ch = *((ptr + i) as *const u8);
                    if ch == 0 {
                        break;
                    }
                    if ch < 0x20 || ch > 0x7e {
                        printable = false;
                        break;
                    }
                    s.push(ch as char);
                }
                if printable && s.len() >= 4 {
                    let _ = writeln!(f, "  +{o:#05x} str[{size}] = {s:?}");
                }
            }
            o += 8;
        }
    }
    let _ = writeln!(f, "");
}

/// Follow the set's EMITTER sub-objects one pointer-level deep and dump their targets, so the
/// working assist set and our co-loaded set can be compared for TEXTURE/RESOURCE-HANDLE binding —
/// the last unruled-out difference. The set's emitter objects sit at +0x450/+0x600/+0x690
/// (vtables 0x854b1d08 / 0x854b1e08); each holds pointers to sub-allocations that carry the
/// emitter's GPU texture handle. If ours are null where the working set's are real GPU objects,
/// that's the invisibility cause. Read-only.
fn dump_emitter_chain(label: &str, set: usize) {
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_emitters.txt")
    else {
        return;
    };
    let _ = writeln!(f, "=== EMITTERS {label} @ {set:#x} ===");
    unsafe {
        // Real game heap pointers live at ~0x11xx_xxxxxx..0x12xx_xxxxxx. The loose old bound
        // (>0x1000_0000) let GARBAGE values like 0x0009_54eb_7800 (~40GB, UNMAPPED) through, and
        // reading 0x120 bytes from those faulted/hung the game. Tighten to the real heap window.
        let is_ptr = |v: u64| v >= 0x10_0000_0000 && v < 0x14_0000_0000 && v & 7 == 0;
        // Scan the whole set object (0x540 bytes) for pointer fields; for each, note the target's
        // vtable/first words. A GPU texture/resource handle target has a vtable in the 0x85.. code
        // region or holds a further pointer chain; a null/leftover field points nowhere.
        for off in (0x40..0x540usize).step_by(8) {
            let v = *((set + off) as *const u64);
            if !is_ptr(v) {
                continue;
            }
            let t = v as usize;
            // Dump the first 4 words of the target + its (vtable[0]) if it looks like an object.
            let w0 = *((t) as *const u64);
            let w1 = *((t + 8) as *const u64);
            let w2 = *((t + 16) as *const u64);
            let w3 = *((t + 24) as *const u64);
            // Extract ASCII name runs (>=3 chars) from the target's first 0x120 bytes — texture /
            // shader / resource NAMES (BNTX/BNSH embed the name; the effect resource table names
            // its textures). This tells us WHICH file the empty +0x4e0 texture wants so we can load
            // it. Also note a 4-char magic (BNTX/BNSH/FRES/VFXB) if present at the head.
            let magic = {
                let m = [
                    (w0 & 0xff) as u8,
                    ((w0 >> 8) & 0xff) as u8,
                    ((w0 >> 16) & 0xff) as u8,
                    ((w0 >> 24) & 0xff) as u8,
                ];
                if m.iter().all(|&c| c.is_ascii_uppercase()) {
                    String::from_utf8_lossy(&m).to_string()
                } else {
                    String::new()
                }
            };
            let mut names: Vec<String> = Vec::new();
            let mut cur = String::new();
            for i in 0..0x120usize {
                let ch = *((t + i) as *const u8);
                if (0x20..=0x7e).contains(&ch) {
                    cur.push(ch as char);
                } else {
                    if cur.len() >= 3 {
                        names.push(cur.clone());
                    }
                    cur.clear();
                }
            }
            if cur.len() >= 3 {
                names.push(cur);
            }
            let names_s = if names.is_empty() {
                String::new()
            } else {
                format!(" names={names:?}")
            };
            let magic_s = if magic.is_empty() {
                String::new()
            } else {
                format!(" magic={magic}")
            };
            let _ = writeln!(
                f,
                "  +{off:#05x} -> {v:#014x} [{w0:#018x} {w1:#018x} {w2:#018x} {w3:#018x}]{magic_s}{names_s}"
            );
        }
    }
    let _ = writeln!(f, "");
}

/// The set object for a manager handle, or 0. set = *(*(*(mgr+0x194d0)+0x98)+slot*8).
unsafe fn set_object_for_handle(mgr: *mut u64, handle: u32) -> usize {
    let Some(slot) = manager_slot_for_handle(mgr, handle) else {
        return 0;
    };
    let p1 = *((mgr as usize + 0x194d0) as *const usize);
    if p1 == 0 {
        return 0;
    }
    let arr = *((p1 + 0x98) as *const usize);
    if arr == 0 {
        return 0;
    }
    *((arr + slot as usize * 8) as *const usize)
}

fn retry_pending_donors(manager: *mut u64) {
    use crate::slight::effect_viewer::resource_reload as rr;
    let mut pend = match PENDING_DONORS.try_lock() {
        Some(p) if !p.is_empty() => p,
        _ => {
            // One-shot visibility into WHY the co-load isn't running: is the pending list empty?
            static NOTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NOTED.fetch_add(1, Ordering::Relaxed);
            if n % 240 == 0 {
                dlog(&format!(
                    "retry: pending EMPTY (specs={} loaded={})",
                    DONOR_SPECS.lock().len(),
                    DONORS_LOADED
                        .lock()
                        .values()
                        .map(|m| m.len())
                        .sum::<usize>(),
                ));
            }
            return;
        }
    };
    dlog(&format!("retry: {} pending", pend.len()));
    let mut done: Vec<usize> = Vec::new();
    for (i, p) in pend.iter_mut().enumerate() {
        p.tries += 1;
        // Give up a donor whose bytes are never served (stale/foreign one-slot the editor listed
        // but has no buffer for, e.g. a leftover ridley/bomberman). Retrying it forever wasted the
        // combined heap cap every frame and starved the real donor — the alucard-invisible bug.
        if !p.filled && p.read_misses >= 5 {
            dlog(&format!(
                "donor GIVE UP (unserved bytes): {} after {} read-misses",
                p.ef_file, p.read_misses
            ));
            done.push(i);
            continue;
        }
        // Serve the editor-supplied donor bytes so arcrop can read them (arcrop can't read a
        // non-resident base-arc file without a callback). Build z-l: the editor now sends the
        // FULL donor eff (not stripped) → the co-loaded kind has all its textures/models, so a
        // mesh effect like alucard_backdash renders.
        register_donor_serve(&p.ef_file);
        // Step 1: kick the directory loader once so the game ALLOCATES an (empty) data slot
        // for the donor's files. Marked so a freeze here is distinguishable.
        // Re-request the dir load EACH retry (not just once): the async loader may drop an
        // early request before the fighter's own load finished, and a real game folder like
        // effect/assist/alucard needs the request to stick. Also probe residency of the eff
        // file so we can see IF/WHEN it lands, isolating "loader ignores us" from "wrong idx".
        // ASSIST-STYLE: kick the dir load EVERY try (the assist ensured its dir right before its
        // load_effects). Otherwise keep the old cadence.
        let assist_phase = ASSIST_STYLE && !p.filled && p.tries <= ASSIST_STYLE_TRIES;
        if assist_phase || p.tries <= 8 || p.tries % 30 == 0 {
            request_dir_load(p.folder_hash);
            let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
            let resident = rr::resident_buffer(ef_hash).is_some();
            mark(&format!(
                "donor_resident_probe {} resident={resident} tries={}",
                p.ef_file, p.tries
            ));
            p.dir_requested = true;
        }
        // ── PIPELINE LOAD (build aw): FORCE the real async dir-load enqueue ──────────────────
        // ensure_dir_loaded short-circuits to a refcount because our dir's descriptor +8 has bit0
        // set ("already loaded"), so it never enqueues — that's why the worker never loaded our
        // resources and the set's handles stay not-ready. Call FUN_03540860 DIRECTLY (once) to
        // force the enqueue, then poll the descriptor state each frame so we can SEE the worker
        // pick it up and advance it (bit0=loading → real data), setting up proper resource handles.
        // Re-force the real enqueue every 60 frames until the eff is resident (one enqueue may be
        // dropped; the worker may need re-kicking). Poll the descriptor + residency each time.
        if !p.filled && (!p.enqueue_forced || p.tries % 60 == 0) {
            if let Some(dir_index) = rr::dir_info_index_for_path_hash(p.folder_hash) {
                let desc = unsafe { force_enqueue_dir(dir_index) };
                let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
                let resident = rr::resident_buffer(ef_hash).is_some();
                let d8 = if desc != 0 {
                    unsafe { *((desc as usize + 8) as *const u64) }
                } else {
                    0
                };
                p.enqueue_forced = true;
                dlog(&format!("force_enqueue {} dir_index={dir_index} tries={} desc={desc:#x} +8={d8:#x} eff_resident={resident}", p.ef_file, p.tries));
            }
        }
        // ── RENDER PUSH (build ak): drive the game's deferred-load QUEUE DRAIN ourselves ─────
        // The set's +0x540 render block only fills when load_effects builds the set with the
        // donor's dir SUB-resources (models/textures) resident. Mid-match our schedule enqueues
        // those reads but nothing drains the queue in our context. So, ONCE per donor: schedule
        // the search_index's dependency dirs via the residency wrapper (0x353d5e0 dep-expand →
        // fn_3540450 schedule), then drive the game's own drain (0x354a120) on the object it uses
        // — captured live from the hook — to process those reads in-context. Gated to a single
        // pass: repeated wrapper calls churn res-service refcounts (that was the build-ah crash).
        let drain_obj = DRAIN_QUEUE_OBJ.load(Ordering::Relaxed);
        if DRAIN_DRIVE && !p.drain_done && drain_obj != 0 && p.dir_requested {
            if let Some(idx) = rr::search_index_for_path_hash(p.folder_hash) {
                IN_DONOR_LOAD.store(true, Ordering::Relaxed);
                let sched = unsafe { load_effects_resident(manager, p.derived, &idx) };
                for _ in 0..8 {
                    unsafe { drain_load_queue(drain_obj as *mut u64) };
                }
                IN_DONOR_LOAD.store(false, Ordering::Relaxed);
                p.drain_done = true;
                let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
                let resident = rr::resident_buffer(ef_hash).is_some();
                dlog(&format!(
                    "drain_drive {} sched={sched} obj={drain_obj:#x} eff_resident={resident} — deps scheduled + queue drained x8",
                    p.ef_file
                ));
            }
        }
        // Step 2: COMPLETE the queued read ourselves (build z-c). The dir-group dump proved
        // ensure_dir_loaded queues the donor's 1 eff file + allocates its LoadedData slot
        // (state=5 loading), but the async res thread never drains it mid-match. So finish the
        // read: pull the REAL donor eff from the arc via arcrop (a direct read, independent of
        // the stalled thread), copy into a GAME-heap buffer (GPU-visible), and point the
        // resident slot at it + mark loaded. load_effects (step 3) then runs the game's OWN
        // full load — including the texture/model GPU upload path (9a100) that bare a4d90
        // skipped — registering the donor kind with valid GPU bindings, under its OWN handle
        // (kirby's slot untouched).
        // PURE-GAME-LOADER mode: skip our manual fill entirely; poll whether the game's own
        // async worker made the folder resident (real resource handles). Log every 30 frames.
        if PURE_GAME_LOADER {
            let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
            let resident = rr::resident_buffer(ef_hash).is_some();
            if resident {
                p.filled = true; // real residency — fall through to load_effects
                dlog(&format!(
                    "PURE resident=TRUE at try {} — game loader completed!",
                    p.tries
                ));
            } else if p.tries % 30 == 0 {
                dlog(&format!(
                    "PURE waiting real residency… try {} (state stuck?)",
                    p.tries
                ));
            }
        } else if assist_phase {
            // NO arcrop pre-fill: let load_effects (below) do the game's own sub-resource load,
            // exactly like the assist summon. p.filled stays false through this phase.
        } else if !p.filled {
            let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
            if let Some(size) = rr::resident_len(ef_hash) {
                // Cap at 4 MB: game_effect_alloc of a 6.4 MB donor (pickel) overflowed the
                // effect heap and crashed. Bigger donors need a different (bulk) heap — skip
                // for now rather than crash.
                let prior = COLOAD_TOTAL_BYTES.load(Ordering::Relaxed);
                if size >= 8 && size < 4 * 1024 * 1024 && prior + size <= COLOAD_COMBINED_CAP {
                    // Reserve the bytes up front so concurrent/subsequent donors see the running
                    // total and stop before the combined cap — prevents multi-donor heap overflow.
                    COLOAD_TOTAL_BYTES.fetch_add(size, Ordering::Relaxed);
                    let mut kept = false;
                    let buf = unsafe { game_effect_alloc(manager, size, 0x1000) };
                    if !buf.is_null() {
                        let slice = unsafe { std::slice::from_raw_parts_mut(buf, size) };
                        mark(&format!("donor_fill_read {} size={size}", p.ef_file));
                        let read = crate::slight::effect_viewer::arcrop::load_file(ef_hash, slice);
                        dlog(&format!(
                            "donor_fill_read {} size={size} got={read:?} magic={:02x?}",
                            p.ef_file,
                            &slice[..4.min(size)]
                        ));
                        if read.is_some() && &slice[..4] == b"EFFN" {
                            let rp = rr::repoint_resident_buffer(ef_hash, buf);
                            dlog(&format!("donor_filled {} rp={rp:?}", p.ef_file));
                            p.filled = rp.is_some();
                            kept = p.filled;
                        } else if read.is_none() {
                            // Bytes aren't served for this donor (stale/foreign one-slot the editor
                            // sent but has no buffer for) — give up so it stops re-reserving the
                            // cap every frame and starving real donors. Was the alucard-starve bug.
                            p.read_misses += 1;
                        }
                    }
                    // Release the reservation on EVERY non-kept path (alloc fail, read fail, magic
                    // mismatch, repoint fail). Leaving it reserved on failure leaked the cap each
                    // retry until it wedged, starving the real donor (build am bug).
                    if !kept {
                        COLOAD_TOTAL_BYTES.fetch_sub(size, Ordering::Relaxed);
                    }
                } else if prior + size > COLOAD_COMBINED_CAP {
                    dlog(&format!(
                        "donor_fill_SKIP {} size={size}: combined cap ({} + {size} > {COLOAD_COMBINED_CAP}) — skipping to avoid heap overflow",
                        p.ef_file, prior
                    ));
                }
            }
        }
        // ── RENDER PUSH v2 (build am): fill the donor dir's SUB-resources directly ──────────
        // The set's +0x540 render block needs the eff's referenced textures/models resident too,
        // not just the .eff. The async worker won't service them mid-match and driving it hangs
        // (build ak), so enumerate the donor DirInfo's child files and arcrop-fill EACH the same
        // proven synchronous way — no worker, no drain, no blocking. Once per donor, after the
        // .eff is filled (the dir load allocated the child slots we repoint). load_effects (below)
        // then builds the set with everything resident + runs its own GPU upload path.
        if SUBRES_FILL && p.filled && !p.subres_done {
            p.subres_done = true;
            if let Some(dir_index) = rr::dir_info_index_for_path_hash(p.folder_hash) {
                let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
                // The effect's OWN dir holds only the .eff (children=1). The assist-summon trace
                // proved the WORKING alucard load also touched dep dirs 17736/13526/14439 — the
                // referenced textures/models the render needs. Those aren't tree children of the
                // eff dir, so fill them explicitly. (alucard-specific dep list for now — this run
                // both PROVES where the render data lives and, if their slots exist, FILLS it. A
                // general dep-expansion via the game's 0x353d5e0 comes next once this is confirmed.)
                let mut dirs_to_fill: Vec<u32> = vec![dir_index];
                if p.folder_path.contains("alucard") {
                    dirs_to_fill.extend_from_slice(&[17736, 13526, 14439]);
                }
                for d in dirs_to_fill {
                    let children = rr::dir_child_file_hashes(d);
                    let total = children.len();
                    let (mut filled, mut no_slot, mut capped, mut failed) =
                        (0usize, 0usize, 0usize, 0usize);
                    for ch in children {
                        if ch == ef_hash {
                            continue; // already filled above
                        }
                        let Some(size) = rr::resident_len(ch) else {
                            no_slot += 1; // no allocated loaded_data slot to point at
                            continue;
                        };
                        let prior = COLOAD_TOTAL_BYTES.load(Ordering::Relaxed);
                        if size < 8 || size >= 4 * 1024 * 1024 || prior + size > COLOAD_COMBINED_CAP
                        {
                            capped += 1;
                            continue;
                        }
                        COLOAD_TOTAL_BYTES.fetch_add(size, Ordering::Relaxed);
                        let buf = unsafe { game_effect_alloc(manager, size, 0x1000) };
                        if buf.is_null() {
                            COLOAD_TOTAL_BYTES.fetch_sub(size, Ordering::Relaxed);
                            failed += 1;
                            continue;
                        }
                        let slice = unsafe { std::slice::from_raw_parts_mut(buf, size) };
                        let mut kept = false;
                        if crate::slight::effect_viewer::arcrop::load_file(ch, slice).is_some()
                            && rr::repoint_resident_buffer(ch, buf).is_some()
                        {
                            filled += 1;
                            kept = true;
                        } else {
                            failed += 1;
                        }
                        // Release the reservation for any sub-resource we didn't keep.
                        if !kept {
                            COLOAD_TOTAL_BYTES.fetch_sub(size, Ordering::Relaxed);
                        }
                    }
                    dlog(&format!(
                        "subres_fill {} dir={d} children={total} filled={filled} no_slot={no_slot} capped={capped} failed={failed}",
                        p.ef_file
                    ));
                }
            }
        }
        // Step 3: register via load_effects (harmless on an unresident slot — returns 0).
        let Some(idx) = rr::search_index_for_path_hash(p.folder_hash) else {
            done.push(i);
            continue;
        };
        IN_DONOR_LOAD.store(true, Ordering::Relaxed);
        // In assist-phase, go through the RESIDENCY WRAPPER (schedules dep dirs into the worker
        // then load_effects) — the assist's own path. Falls back to raw load_effects if the
        // wrapper entry couldn't be resolved (u32::MAX) or outside assist-phase.
        let mut result = if assist_phase {
            unsafe { load_effects_resident(manager, p.derived, &idx) }
        } else {
            unsafe { load_effects(manager, p.derived, &idx) }
        };
        let via_wrapper = assist_phase && result != u32::MAX;
        if result == u32::MAX {
            result = unsafe { load_effects(manager, p.derived, &idx) };
        }
        IN_DONOR_LOAD.store(false, Ordering::Relaxed);
        if p.tries <= 3 {
            dlog(&format!(
                "wrapper_entry={:#x} via_wrapper={via_wrapper}",
                load_effects_resident_entry()
            ));
        }
        dlog(&format!("load_effects {} derived={} idx={idx} filled={} assist_phase={assist_phase} via_wrapper={via_wrapper} result={result}", p.ef_file, p.derived, p.filled));
        if result == 1 {
            DONORS_LOADED
                .lock()
                .entry(p.target_handle)
                .or_default()
                .insert(p.folder_hash, p.derived);
            // The co-load registered the donor's REAL kinds (e.g. `alucard_backdash`) with
            // valid GPU bindings. But the one-slot redirect points the move at the MERGED
            // `_os` name (`alucard_backdash_os`, which lives in kirby's eff → GPU-broken). So
            // build a remap `hash(X_os) → hash(X)` from the donor's entry names; the effect
            // hook applies it as a final rewrite so the spawn lands on the real co-loaded kind.
            let ef_hash = smash::hash40(&p.ef_file.to_lowercase());
            if let Some(buf) = rr::resident_buffer(ef_hash) {
                let names = unsafe { eff_entry_names(buf) };
                let mut remap = CO_LOADED_REMAP.lock();
                for (name, _) in &names {
                    let real = smash::hash40(&name.to_lowercase());
                    let os = smash::hash40(&format!("{}_os", name.to_lowercase()));
                    remap.insert(os, real);
                }
                dlog(&format!(
                    "co_load_remap {} entries from {}",
                    names.len(),
                    p.ef_file
                ));
                // ── DIAGNOSTIC (build 2026-07-21v): does req() actually resolve the donor
                // kinds after load_effects? resolve_kind_replica is cycle-guarded (cannot hang),
                // so this is zero-risk. If every kind MISSES the manager kind-name map, the load
                // never registered them → the fix is live kind_register. If it HITS but req still
                // returns 0, the block is downstream (handle-map/slot/state), not registration.
                drop(remap);
                for (name, entry_idx) in &names {
                    let real = smash::hash40(&name.to_lowercase());
                    let (desc, ptr) = unsafe { resolve_kind_replica(manager, real) };
                    dlog(&format!(
                        "resolve[{name}] hash={real:#013x} donor_entry_idx={entry_idx} => {}{}",
                        if ptr.is_some() { "HIT " } else { "MISS " },
                        desc
                    ));
                }
            }
            // TARGETED state-3 force (build z-p). The effect-set's resource-readiness state
            // machine (byte @set+0x20) only reaches 3 when the async loader signals completion,
            // which our manual fill bypasses → textures never process. Force state 3 on ONLY
            // the freshly co-loaded set (not all effects — the broad force froze on running
            // ones). set = *(*(*(mgr+0x194d0)+0x98) + slot*8) for this donor's slot.
            unsafe {
                if let Some(slot) = manager_slot_for_handle(manager, p.derived) {
                    let p1 = *((manager as usize + 0x194d0) as *const usize);
                    if p1 != 0 {
                        let arr = *((p1 + 0x98) as *const usize);
                        if arr != 0 {
                            let set = *((arr + slot as usize * 8) as *const usize);
                            if set != 0 {
                                // Register the co-loaded set to be TICKED each frame — its
                                // resource-readiness state machine (vtable+0x70) only runs when
                                // ticked, and that machine does the real GPU texture setup as it
                                // advances 0→1→2→3. An inactive synthetic-handle set never ticks,
                                // so drive its update ourselves from the game-thread pump.
                                let st = *((set + 0x20) as *const u8);
                                COLOAD_TICK_SET.store(set, Ordering::Relaxed);
                                COLOAD_TICK_FRAMES.store(240, Ordering::Relaxed);
                                let f540 = *((set + 0x540) as *const u64);
                                dlog(&format!("coload_set_tick_start slot={slot} set={set:#x} state=0x{st:02x} at+0x540={f540:#x} {}", if f540 != 0 { "*** +0x540 FILLED ***" } else { "(+0x540 zero)" }));
                                dump_set("COLOAD_at_load", set, 0x700);
                                dump_emitter_chain("COLOAD_at_load", set);
                            } else {
                                dlog(&format!("coload_set_force slot={slot} set=NULL"));
                            }
                        }
                    }
                }
            }
            crate::slight::diag::note(format!(
                "donor eff LOADED: {} (handle {} target {}, idx {idx}, after {} tries)",
                p.folder_path, p.derived, p.target_handle, p.tries
            ));
            skyline::println!(
                "[SLight] donor eff LOADED: {} handle={}",
                p.folder_path,
                p.derived
            );
            done.push(i);
        } else if p.tries >= DONOR_MAX_TRIES {
            crate::slight::diag::note(format!(
                "donor eff GAVE UP: {} (idx {idx}, result {result} after {} tries — files never became resident)",
                p.folder_path, p.tries
            ));
            done.push(i);
        }
    }
    for i in done.into_iter().rev() {
        pend.remove(i);
    }
}

// ── Synchronous LIVE RE-READ (the live effect memory-management path) ─────────────
//
// The decompiled truth: `unload_effects` only drops the effect-MANAGER refcount; it never
// frees the res-service file buffer, and `load_effects` re-parses `LoadedData[di].buffer`
// in place. So the ONLY way to make merged bytes appear mid-match without re-entry is to
// swap the resident buffer ourselves, then reparse. We do it SYNCHRONOUSLY on the game
// thread in one frame — no dependency on the async res loading thread (whose mid-match
// servicing is unproven) and no arc read: the merged bytes come straight off SD.
//
// Safety: operate ONLY on the fighter's OWN, currently-loaded eff (a valid fp/di/handle the
// game itself uses). Order: kill on-screen effects → unload_effects (manager stops holding
// the old buffer) → repoint the slot at a leaked merged buffer → load_effects (re-parse
// merged) → respawn. The old buffer is never freed (no UAF). Every step writes an immediate-
// flush marker so a freeze pinpoints the exact instruction.

/// Fighter eff arc paths queued (over TCP) for a synchronous live re-read; drained on the
/// game thread by [`pump_force_reread`].
static REREAD_QUEUE: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// TCP entry point: request a live re-read of `ef_path` (e.g. "effect/fighter/kirby/ef_kirby.eff").
pub fn queue_force_reread(ef_path: String) {
    mark(&format!("reread_queued {ef_path}"));
    REREAD_QUEUE.lock().push(ef_path);
}

/// Game-thread pump — drains [`REREAD_QUEUE`]. Called each frame beside [`pump_donor_queue`].
pub fn pump_force_reread() {
    let jobs: Vec<String> = match REREAD_QUEUE.try_lock() {
        Some(mut q) if !q.is_empty() => std::mem::take(&mut *q),
        _ => return,
    };
    for ef_path in jobs {
        do_force_reread(&ef_path);
    }
}

/// Force `load_effects` to FULLY re-parse instead of early-outing. The effect manager keeps
/// a handle→slot hash map at `mgr+0x193b0` (libc++ unordered_map: `+0x193b8` = bucket count,
/// node `+0` = next, `+8` = hash, `+0x10` = key(handle:i32)). `load_effects` refcount-bumps
/// + returns 1 when the handle is present, and `unload_effects` never removes it — so a
/// reparse is a no-op and NEW entry names are never registered (`req` NOT-FOUND). We make the
/// early-out MISS by mangling the matched node's KEY so the lookup fails; `load_effects` then
/// takes the full path, re-hashes every entry name (registering the new one-slot kinds) and
/// rebuilds the sets. No unlink/free — the stale node is inert (its key never matches again),
/// and the old slot (free byte already set by unload_effects) is reused/GC'd. Returns true if
/// a node was found + mangled.
unsafe fn evict_handle_from_manager_map(mgr: *mut u64, handle: u32) -> bool {
    if mgr.is_null() {
        return false;
    }
    let base = mgr as usize;
    let buckets = *((base + 0x193b0) as *const usize);
    let count = *((base + 0x193b8) as *const u64) as usize;
    if buckets == 0 || count == 0 {
        return false;
    }
    let h = handle as u64;
    let bucket = if count & (count - 1) == 0 {
        (h & (count as u64 - 1)) as usize
    } else {
        (h % count as u64) as usize
    };
    // buckets[bucket] points to the node BEFORE the bucket's first (libc++ hash_table).
    let before = *((buckets + bucket * 8) as *const usize);
    if before == 0 {
        return false;
    }
    let mut node = *(before as *const usize); // first actual node
    let mut guard = 0;
    while node != 0 && guard < 4096 {
        guard += 1;
        let node_hash = *((node + 8) as *const u64);
        let node_key = *((node + 0x10) as *const i32);
        if node_hash == h && node_key == handle as i32 {
            *((node + 0x10) as *mut i32) = 0x00ff_ffff; // impossible handle → early-out misses
            return true;
        }
        node = *(node as *const usize); // next
    }
    false
}

fn do_force_reread(ef_path: &str) {
    // DISABLED (build z-f): the merged-eff reread is superseded by the CO-LOAD path
    // (retry_pending_donors loads the donor's real folder via the game's own loader →
    // GPU-valid). Every reread variant either broke the fighter's effects (a4d90 GPU
    // wall) or interfered with the co-loaded/boot-registered kinds (pickel crash). Leave
    // it fully inert so nothing touches the fighter's resident buffer or sets.
    let _ = std::fs::write(
        "sd:/effect_viewer_marks.txt",
        format!("reread DISABLED (co-load path handles cross-fighter) ef={ef_path}\n"),
    );
    return;
    #[allow(unreachable_code)]
    {
        use crate::slight::effect_viewer::resource_reload as rr;
        mark(&format!("reread_start {ef_path}"));
        let manager = effect_manager();
        if manager.is_null() {
            mark("reread_abort no_manager");
            return;
        }
        let folder = eff_dir(ef_path).to_string();
        let folder_hash = smash::hash40(&folder);
        let ef_hash = smash::hash40(&ef_path.to_lowercase());

        // The REAL game slots for this fighter's effect folder (result==1 only — never our own
        // failed handle-0/donor slots). No slot ⇒ the fighter isn't loaded; bail (safe).
        let slots: Vec<(u32, u32)> = ACTIVE_SLOTS
            .lock()
            .iter()
            .filter(|(_, s)| s.path_hash == folder_hash && s.result == 1)
            .map(|(h, s)| (*h, s.search_index))
            .collect();
        if slots.is_empty() {
            mark(&format!("reread_abort not_loaded folder={folder}"));
            let _ = std::fs::write(
            "sd:/effect_viewer_reread.txt",
            format!("build reread\nef={ef_path}\nresult=NOT_LOADED (fighter's eff folder has no live slot)\n"),
        );
            return;
        }

        // Merged bytes straight off SD (the editor wrote them; the disk_cb serves the same file).
        let Some(sd_path) = crate::slight::effect_viewer::live_eff::served_path(ef_hash) else {
            mark("reread_abort no_served");
            let _ = std::fs::write(
                "sd:/effect_viewer_reread.txt",
                format!(
                    "build reread\nef={ef_path}\nresult=NO_SERVED (deploy the merged eff first)\n"
                ),
            );
            return;
        };
        let merged = match std::fs::read(&sd_path) {
            Ok(b) => b,
            Err(e) => {
                mark(&format!("reread_abort read_err {e}"));
                return;
            }
        };
        if merged.len() < 8 || &merged[..4] != b"EFFN" {
            mark("reread_abort bad_merged");
            return;
        }
        let merged_len = merged.len();

        // FULL LIVE-ADD CHAIN (build o). Build n settled the freeze cause: eff buffers must live
        // in GAME-allocated memory (the same bytes that build fine from the game's resident
        // buffer hard-freeze from a plugin-heap Vec — the effect system maps the buffer for GPU
        // access in place). So: allocate from the game's own effect allocator, copy the merged
        // eff in, then evict the handle from the manager map and run load_effects' FULL path —
        // which re-hashes every entry name (registering the new `_os` kinds) and rebuilds the
        // effect sets from the merged buffer. Never freed: the parsed sets reference it forever.
        mark(&format!(
            "reread_pre_galloc slots={} len={merged_len}",
            slots.len()
        ));
        let galloc = unsafe { game_effect_alloc(manager, merged_len, 0x1000) };
        mark(&format!("reread_galloc {galloc:?}"));
        if galloc.is_null() {
            mark("reread_abort galloc_failed");
            let _ = std::fs::write(
                "sd:/effect_viewer_reread.txt",
                format!("build reread\nef={ef_path}\nresult=GALLOC_FAILED len={merged_len}\n"),
            );
            return;
        }
        unsafe { std::ptr::copy_nonoverlapping(merged.as_ptr(), galloc, merged_len) };
        drop(merged);
        mark("reread_copied_to_game_heap");

        // NON-DESTRUCTIVE IN-PLACE ADD (build z7). The unload→evict→load_effects chain MOVED the
        // fighter's slot (4→7) and destroyed the effect-set object the fighter was actively
        // referencing → ALL of kirby's effects went invisible (user-reported regression). Fix:
        // touch NOTHING the fighter points at. Keep the handle, keep the slot, keep every existing
        // set. Just: (1) repoint the resident buffer to the merged bytes, (2) rebuild the set
        // object IN the fighter's CURRENT slot from the merged buffer (a4d90 destroys+rebuilds at
        // the same index — proven live-safe, and the fighter re-reads set ptrs each frame), (3)
        // update that slot's entry-table pointer @+0x18018 so the resolver can reach the appended
        // entry, (4) register the new `_os` kinds directly. No slot move, no teardown of live sets.
        IN_DONOR_LOAD.store(true, Ordering::Relaxed);
        let rp = rr::repoint_resident_buffer(ef_hash, galloc);
        mark(&format!("reread_repointed {rp:?}"));
        let p1 = unsafe { *((manager as usize + 0x194d0) as *const u64) };
        let p2 = (manager as usize + 0x194d8) as *mut u8;
        let flag = unsafe { *((manager as usize + 0x194b8) as *const u8) };
        let sv = unsafe { *((galloc as usize + 0xe) as *const i16) } as usize;
        let section = (galloc as usize + sv * 0x1000) as *const u8;
        let num_entries = unsafe { *((galloc as usize + 8) as *const u16) } as usize;
        let _ = (p1, p2, flag, section, num_entries); // (set-rebuild path disabled — see below)
        let mut results: Vec<u32> = Vec::new();
        // GPU-BINDING WALL (build z8). Even a same-slot in-place a4d90 rebuild broke ALL of the
        // fighter's effects — same as build m rebuilding kirby's OWN vanilla buffer. a4d90 rebuilds
        // the CPU-side effect-set but the eff's textures/models are uploaded to the GPU only ONCE,
        // at fighter-load, by a separate path; a live rebuild leaves every set referencing
        // un-uploaded resources → all invisible. So DO NOT rebuild the set here (it's destructive
        // with no live GPU re-upload). Only register the new kind's NAME so we can measure whether
        // resolution alone is enough while the GPU-upload path is found. Existing effects keep
        // their fighter-load GPU bindings and stay visible.
        for (handle, _search_index) in &slots {
            let slot = unsafe { manager_slot_for_handle(manager, *handle) };
            results.push(1);
            mark(&format!("reread_norebuild handle={handle} slot={slot:?} (GPU-binding wall — set rebuild disabled)"));
        }

        // Build s froze somewhere between load_effects completing and the probe mark. Bisect:
        // NO map insertion this build (kind_register suspended), a GUARDED diagnostic bucket
        // walk first (cycle-detecting, cannot hang), and a mark around EVERY resolver call so
        // the freezing step is named in the trail.
        let names = unsafe { eff_entry_names(galloc) };
        mark(&format!("reread_names n={}", names.len()));
        let missing: Vec<(String, u32, u64)> = names
            .iter()
            .filter_map(|(n, i)| {
                let h40 = smash::hash40(&n.to_lowercase());
                match unsafe { kind_lookup(manager, h40) } {
                    Some(_) => None,
                    None => Some((n.clone(), *i, h40)),
                }
            })
            .collect();
        mark(&format!(
            "reread_missing {:?}",
            missing
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect::<Vec<_>>()
        ));
        // Kind registration disabled too: without the rebuilt set + entry-table pointer, a
        // registered node would make the resolver read past the vanilla 40-entry table (OOB).
        // Re-enable once the GPU-upload path lets the set be rebuilt safely.
        let _ = kind_register as *const () as usize; // keep referenced
        let registered = 0usize;

        // Build t froze INSIDE the game resolver even though the kind chain was clean ("HIT
        // step 0") — a leaf function that provably terminates on well-formed maps. Something
        // about calling it is unsound in a way we can't see, so DON'T: replicate its complete
        // walk (kind map → handle map → slot entry table) in guarded Rust instead. Probing a
        // VANILLA kind alongside the `_os` ones also answers the entry-FLAGS question: the
        // spawn callsites require bit0/bit1 of entry byte0, which is 0x00 on disk — if the
        // vanilla kind's LIVE entry shows nonzero flags, the game mutates entries at load and
        // our appended entry is missing that mutation.
        let mut probe = String::new();
        let mut probe_names: Vec<&str> = vec!["kirby_attack_arc", "kirby_dash"];
        probe_names.extend(
            names
                .iter()
                .filter(|(n, _)| n.ends_with("_os"))
                .map(|(n, _)| n.as_str()),
        );
        for name in probe_names {
            let h40 = smash::hash40(&name.to_lowercase());
            let (diag, entry_ptr) = unsafe { resolve_kind_replica(manager, h40) };
            let line = if let Some(ptr) = entry_ptr {
                let entry = unsafe { std::slice::from_raw_parts(ptr, 0x10) };
                let set_id = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
                // set_id is a 1-based handle; follow it into the LIVE set object at this slot.
                let set_diag = if set_id > 0 {
                    let slot = diag
                        .split("slot=")
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse::<u32>().ok());
                    match slot {
                        Some(slot) => unsafe {
                            set_object_debug(manager, slot, set_id as usize - 1)
                        },
                        None => "slot unparsed".into(),
                    }
                } else {
                    "set_id=0".into()
                };
                format!(
                "{name}: {diag} -> {ptr:?} flags={:#04x} type={:#04x} set_id={set_id} in_galloc={} | SET: {set_diag}\n",
                entry[0],
                entry[1],
                (ptr as usize) >= galloc as usize && (ptr as usize) < galloc as usize + merged_len,
            )
            } else {
                format!("{name}: {diag} -> UNRESOLVED\n")
            };
            mark(&format!("reread_replica {}", line.trim_end()));
            probe.push_str(&line);
        }
        mark("reread_probe_complete");

        IN_DONOR_LOAD.store(false, Ordering::Relaxed);
        // 4. Respawn on-screen effects from the freshly parsed data.
        let respawned = crate::slight::effect_viewer::live_eff::respawn_live_effects();
        mark(&format!("reread_done respawned={respawned}"));

        let ok = results.iter().all(|r| *r == 1);
        let _ = std::fs::write(
        "sd:/effect_viewer_reread.txt",
        format!(
            "build reread\nef={ef_path}\nfolder={folder}\nmerged_len={merged_len}\nslots={:?}\nload_results={results:?}\nnames={} missing_after_load={} manually_registered={registered}\n--- resolver probe ---\n{probe}respawned={respawned}\nresult={}\n",
            slots,
            names.len(),
            missing.len(),
            if ok { "OK" } else { "PARTIAL/FAIL (see load_results)" }
        ),
    );
        crate::slight::diag::note(format!(
            "live re-read {ef_path}: results={results:?} respawned={respawned}"
        ));
    }
}

/// Per-slot dump for the probe file — lets the PC side verify which index space the
/// game's own loads use (compare against hashes it knows).
pub fn slots_debug() -> String {
    let mut out = String::new();
    for (handle, slot) in ACTIVE_SLOTS.lock().iter() {
        out.push_str(&format!(
            "slot handle={handle} search_index={} path_hash={:#x} result={}\n",
            slot.search_index, slot.path_hash, slot.result
        ));
    }
    out
}

pub fn donor_debug_line() -> String {
    format!(
        "donor_specs={} donor_loaded={}",
        DONOR_SPECS.lock().len(),
        DONORS_LOADED
            .lock()
            .values()
            .map(|m| m.len())
            .sum::<usize>(),
    )
}

pub fn install_hooks() {
    skyline::install_hook!(hook_load_effects);
    skyline::install_hook!(hook_ensure_dir_loaded);
    skyline::install_hook!(hook_readiness);
    skyline::install_hook!(hook_unload_effects);
    skyline::install_hook!(hook_drain_queue);
    skyline::install_hook!(hook_build_effect_set);
    skyline::println!("[SLight] Effect manager reload hooks installed");
    crate::slight::diag::note("effect-manager reload hooks installed");
}

/// Unload and reload parsed effect data for the fighter whose eff is `game_path`.
///
/// Effects load per FOLDER (decompiled `load_effects` takes a directory search index; the
/// game's real kirby slot is handle 774 = index of "effect/fighter/kirby", NOT the
/// ef_kirby.eff FILE). Reparsing the file hash only ever matched a bogus handle-0 slot we
/// created and did nothing. This targets the FOLDER slot the game actually loaded, so the
/// unload/reload re-reads the folder's ef file — which ARCropolis redirects to our merged
/// eff (donor baked in), making one-slot entries resident.
pub fn reparse_game_path(game_path: &str) -> usize {
    let manager = effect_manager();
    if manager.is_null() {
        crate::slight::diag::note("effect manager unavailable");
        return 0;
    }

    let folder = eff_dir(game_path).to_string();
    let folder_hash = smash::hash40(&folder);

    // The real slot(s): tracked entries whose path_hash is the FOLDER (handle 774 for
    // kirby). Fall back to matching the folder's search index if path_hash wasn't resolved.
    let folder_idx =
        crate::slight::effect_viewer::resource_reload::search_index_for_path_hash(folder_hash);
    let mut slots: Vec<(u32, u32)> = ACTIVE_SLOTS
        .lock()
        .iter()
        .filter(|(_, s)| {
            s.path_hash == folder_hash || folder_idx.is_some_and(|i| s.search_index == i)
        })
        // Only real game slots (result==1) — never our own failed handle-0/donor slots.
        .filter(|(_, s)| s.result == 1)
        .map(|(h, s)| (*h, s.search_index))
        .collect();
    slots.sort_by_key(|(h, _)| *h);
    slots.dedup_by_key(|(h, _)| *h);

    if slots.is_empty() {
        crate::slight::diag::note(format!(
            "cannot reparse folder {folder} ({folder_hash:#x}) — fighter not loaded (idx={folder_idx:?})"
        ));
        return 0;
    }

    let mut reparsed = 0usize;
    for (handle, search_index) in slots {
        unsafe {
            unload_effects(manager, handle);
            load_effects(manager, handle, &search_index);
        }
        reparsed += 1;
        crate::slight::diag::note(format!(
            "reparsed effect FOLDER {folder} handle {handle} search_index {search_index}"
        ));
    }

    LAST_REPARSED.store(reparsed as u64, Ordering::Relaxed);
    reparsed
}

pub fn debug_line() -> String {
    format!(
        "active_effect_slots={} known_effect_paths={} load_hook_calls={} last_reparsed={} {} {}",
        ACTIVE_SLOTS.lock().len(),
        KNOWN_HANDLES.lock().len(),
        LOAD_HOOK_CALLS.load(Ordering::Relaxed),
        LAST_REPARSED.load(Ordering::Relaxed),
        donor_debug_line(),
        crate::slight::effect_viewer::resource_reload::debug_line(),
    )
}
