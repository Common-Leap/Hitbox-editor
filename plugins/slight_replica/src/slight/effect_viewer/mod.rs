pub mod acmd_hooks;
pub mod apply;
pub mod arcrop;
pub mod effect_data;
pub mod effect_names;
pub mod effect_reload;
pub mod frame_tick;
pub mod kinds;
pub mod live_eff;
pub mod resource_reload;
pub mod show;
pub mod spawn_rules;
pub mod tracker;

use smash::app::lua_bind::{EffectModule, StatusModule};
use smash::app::utility;
use smash::phx::{self, Vector3f};

use effect_data::Point3D;

/// Pseudo-handle range for synthetic (handle-less) effects — top bit set so they can never
/// collide with real EffectModule handles.
const SYNTH_HANDLE_BASE: u32 = 0x8000_0000;
static SYNTH_HANDLE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// A carrier-owned handle returned to a different source object's ACMD call. Cleanup calls arrive
/// on the source module, so retain the true owner for handle-based kill/remove operations.
static PROXY_HANDLES: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(usize, u32), usize>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
/// Allows an intentional ACMD proxy request through the carrier-effect suppression hooks.
static CARRIER_PROXY_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Editor-facing transplant hash for the current carrier request. The carrier instantiates the
/// donor's real kind, but live tracking and pins must remain keyed to `<copy>_os`.
static CARRIER_PROXY_LOGICAL_HASH: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Set while an ACMD spawn is being attempted on the FIGHTER itself, so [`spawn_owner`] leaves
/// the owner alone instead of redirecting it to the carrier.
static DIRECT_SPAWN_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lets the fighter own a carrier-loaded effect for the duration of one request.
///
/// The carrier exists to LOAD resources; it does not have to own the instances. Owning them was
/// costing correctness: the carrier is a hidden item with no fighter skeleton, so an effect
/// spawned on it has no bone to attach to and gets an absolute world transform instead of the
/// joint's full matrix. Billboard particles survive that (the follow pump keeps them roughly in
/// place), but a MESH emitter takes its orientation and scale from the owner, so the model drew
/// at the wrong basis entirely — a shapeless blob where a cone should be — and lingered in mid
/// air when the follow record was not there to move it.
///
/// Effect kinds live in the effect manager's GLOBAL kind map once the carrier's load registers
/// them, so any object may request one. The fighter is tried first and the carrier remains the
/// fallback for kinds it genuinely cannot serve.
pub struct DirectSpawnGuard;

impl DirectSpawnGuard {
    pub fn new() -> Self {
        DIRECT_SPAWN_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for DirectSpawnGuard {
    fn drop(&mut self) {
        DIRECT_SPAWN_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub struct CarrierProxyGuard;

impl CarrierProxyGuard {
    pub fn new(logical_hash: u64) -> Self {
        CARRIER_PROXY_LOGICAL_HASH.store(
            logical_hash & 0xff_ffff_ffff,
            std::sync::atomic::Ordering::Release,
        );
        CARRIER_PROXY_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for CarrierProxyGuard {
    fn drop(&mut self) {
        CARRIER_PROXY_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        CARRIER_PROXY_LOGICAL_HASH.store(0, std::sync::atomic::Ordering::Release);
    }
}

fn carrier_proxy_logical_hash(actual_hash: u64) -> u64 {
    if !CARRIER_PROXY_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return actual_hash;
    }
    let logical = CARRIER_PROXY_LOGICAL_HASH.load(std::sync::atomic::Ordering::Acquire);
    if logical == 0 {
        actual_hash
    } else {
        logical
    }
}

fn suppress_carrier_request(module_accessor: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    (unsafe { crate::slight::effect_viewer::effect_reload::is_auto_carrier_boma(module_accessor) })
        && !CARRIER_PROXY_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

unsafe fn spawn_owner(
    source: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: u64,
) -> *mut smash::app::BattleObjectModuleAccessor {
    // The ACMD path asks for the fighter to own the instance (see [`DirectSpawnGuard`]).
    // Redirecting it here would put the effect back on the skeleton-less carrier, which is the
    // thing that broke mesh emitters.
    if DIRECT_SPAWN_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return source;
    }
    crate::slight::effect_viewer::effect_reload::auto_carrier_boma_for_kind(eff_hash)
        .unwrap_or(source)
}

unsafe fn remember_proxy_handle(
    source: *mut smash::app::BattleObjectModuleAccessor,
    owner: *mut smash::app::BattleObjectModuleAccessor,
    result: u64,
    eff_hash: u64,
) {
    if source.is_null() || owner.is_null() || source == owner {
        return;
    }
    let handle = if result != 0 {
        result as u32
    } else {
        EffectModule::get_last_handle(owner) as u32
    };
    if handle == 0 {
        return;
    }
    PROXY_HANDLES
        .lock()
        .insert((source as usize, handle), owner as usize);

    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 32 {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_carrier_spawn.txt")
        {
            let source_id = (*source).battle_object_id;
            let owner_id = (*owner).battle_object_id;
            let _ = writeln!(
                file,
                "kind={eff_hash:#x} source={source_id:#x} owner={owner_id:#x} handle={handle:#x}"
            );
        }
    }
}

fn proxy_owner_for_handle(
    source: *mut smash::app::BattleObjectModuleAccessor,
    handle: u32,
) -> Option<*mut smash::app::BattleObjectModuleAccessor> {
    let owner = PROXY_HANDLES
        .lock()
        .get(&(source as usize, handle))
        .copied()
        .map(|owner| owner as *mut smash::app::BattleObjectModuleAccessor)?;
    let current = unsafe { crate::slight::effect_viewer::effect_reload::auto_carrier_boma() };
    (current == Some(owner)).then_some(owner)
}

pub fn init_fighter(boid: u32) {
    crate::slight::systems::main_module::on_init_fighter(boid);
}

pub fn each_frame(active_effect_ids: &[u64]) {
    for id in active_effect_ids {
        frame_tick::tick_effect(*id, true);
    }
    crate::slight::extras::tick_tracked_effects();
}

pub fn track_spawn(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    result_h: u64,
    eff_hash: u64,
    bone_hash: u64,
    is_follow: bool,
    pos: *const Vector3f,
    rot: *const Vector3f,
    scale: f32,
) {
    let handle = if result_h != 0 {
        result_h as u32
    } else {
        unsafe { EffectModule::get_last_handle(module_accessor) as u32 }
    };

    // GROUND-TRUTH _os PROBE + TRIANGULATION. Every EffectModule req variant funnels here,
    // so this fires regardless of which one ACMD actually used (req_follow/on_joint/
    // continual/time_follow guessing kept missing). On a refused `_os` request, re-fire
    // controlled variants from this same game-thread context to localize the ADD gate.
    if effect_names::is_transplant_label(&effect_names::label(eff_hash))
        || crate::slight::effect_viewer::effect_reload::is_coloaded_kind(eff_hash)
    {
        use std::io::Write;
        let gl = unsafe { EffectModule::get_last_handle(module_accessor) as u32 };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_os_req.txt")
        {
            let _ = writeln!(
                f,
                "{} follow={is_follow} result_h={result_h:#x} get_last_handle={gl:#x} bone={bone_hash:#x} scale={scale}",
                effect_names::label(eff_hash),
            );
        }
    }
    // ONE-SHOT REAL-IMPL CAPTURE. Re-firing req from a hook returns 0 even for a known-good
    // kind (re-entrancy guard), so triangulation is dead. Instead read the fighter's live
    // EffectModule vtable during a REAL spawn to get the concrete req-impl addresses:
    // shim → obj=*(boma+0x140), impl FUN_0044de70 calls inner=*(obj+0x10) then
    // inner_vt[0x190] and inner_vt[0x50]. Those two are the actual resolve/instantiate the
    // gate lives in — decompile them offline, no more device rounds to find the gate.
    static IMPL_CAP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !IMPL_CAP.swap(true, std::sync::atomic::Ordering::Relaxed) {
        use std::io::Write;
        unsafe {
            let text = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize;
            let obj = *((module_accessor as usize + 0x140) as *const usize);
            let obj_vt = if obj != 0 { *(obj as *const usize) } else { 0 };
            let inner = if obj != 0 {
                *((obj + 0x10) as *const usize)
            } else {
                0
            };
            let inner_vt = if inner != 0 {
                *(inner as *const usize)
            } else {
                0
            };
            let f190 = if inner_vt != 0 {
                *((inner_vt + 0x190) as *const usize)
            } else {
                0
            };
            let f50 = if inner_vt != 0 {
                *((inner_vt + 0x50) as *const usize)
            } else {
                0
            };
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("sd:/effect_viewer_os_req.txt")
            {
                let _ = writeln!(
                    f,
                    "IMPL_CAP obj={obj:#x} obj_vt=+{:#x} inner={inner:#x} inner_vt=+{:#x} f190=+{:#x} f50=+{:#x}",
                    obj_vt.wrapping_sub(text),
                    inner_vt.wrapping_sub(text),
                    f190.wrapping_sub(text),
                    f50.wrapping_sub(text),
                );
            }
        }
    }

    let mod_addr = module_accessor as u64;
    let (category, fighter_kind, status_kind) = unsafe {
        let cat = utility::get_category(&mut *module_accessor);
        let kind = utility::get_kind(&mut *module_accessor);
        let status = if cat == 0 {
            StatusModule::status_kind(module_accessor)
        } else {
            0
        };
        (cat, kind, status)
    };

    // DIAG: capture EVERY requested effect (before dedup) to sd:/slight/diag.txt.
    crate::slight::diag::note_spawn(
        &effect_names::label(eff_hash),
        is_follow,
        handle,
        category,
        status_kind,
    );

    // Fire-and-forget spawns return no handle (and get_last_handle can also yield 0). Those
    // are many of the character-specific vfx — track them under a synthetic pseudo-handle so
    // they still appear in RPM (display-only: no handle means the EffectModule setters can't
    // target them; tracker::reconcile expires them by TTL instead of is_exist_effect).
    let synthetic = handle == 0;
    let handle = if synthetic {
        SYNTH_HANDLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) | SYNTH_HANDLE_BASE
    } else {
        handle
    };

    let boid = crate::slight::agents::boid_from_module(module_accessor).unwrap_or(0);
    let entry_id = if boid > 0 {
        unsafe { smash::app::sv_battle_object::entry_id(boid) }
    } else {
        -1
    };
    let founder_entry_id = crate::slight::agents::lookup(boid).and_then(|a| a.founder_entry_id);

    let pos3 = vector3(pos);
    let rot3 = vector3(rot);

    let (id, is_new, reshow) = tracker::EFFECT_TRACKER.lock().upsert_spawn(
        mod_addr,
        handle,
        eff_hash,
        bone_hash,
        is_follow,
        pos3,
        rot3,
        scale,
        boid,
        fighter_kind,
        status_kind,
        category,
        entry_id,
        founder_entry_id,
        synthetic,
    );
    crate::slight::diag::note_result(is_new, reshow);

    if category != 0 {
        let _ = crate::slight::agents::upsert_module(module_accessor);
    }

    if is_new {
        crate::slight::systems::dynamic_memory::bind_effect(boid, handle, id);
    }

    // Kind-level RPM view: one tab per effect kind (eff_hash). The newest spawn's params
    // become the live view (pins win); RPM notifies via the pending queue keyed by eff_hash.
    let spawn_data = tracker::EFFECT_TRACKER
        .lock()
        .get(id)
        .map(|e| e.data.clone());
    if let Some(spawn_data) = spawn_data {
        let label = effect_names::label(eff_hash);
        kinds::observe_spawn(eff_hash, &label, &spawn_data);
        show::queue_show(eff_hash);
        crate::slight::pending::process();

        // Enforce existing pins on the new instance right away (per-frame enforcement will
        // keep it that way) — "force them to be that value until they are edited next".
        // Only PINNED fields are applied; the effect keeps its native look otherwise.
        if !synthetic {
            if let Some(pins) = kinds::pinned_of(eff_hash) {
                apply::apply_pinned(module_accessor, handle, &pins, is_follow);
            }
        }
    }
}

/// Instance removal — game-side only. Kind tabs persist in RPM (so pinned edits survive
/// across spawns and the list doesn't churn); RemoveAll clears them at match end.
pub fn on_removed(_id: u64, _notified: bool) {}

fn vector3(ptr: *const Vector3f) -> Point3D {
    if ptr.is_null() {
        return Point3D::default();
    }
    unsafe {
        Point3D {
            x: (*ptr).x,
            y: (*ptr).y,
            z: (*ptr).z,
        }
    }
}

pub fn handle_kill(module_accessor: *mut smash::app::BattleObjectModuleAccessor, handle: u32) {
    let mod_addr = module_accessor as u64;
    if let Some((id, notified)) = tracker::EFFECT_TRACKER
        .lock()
        .remove_by_handle(mod_addr, handle)
    {
        on_removed(id, notified);
    }
}

pub fn handle_kill_hash(module_accessor: *mut smash::app::BattleObjectModuleAccessor, hash: u64) {
    let mod_addr = module_accessor as u64;
    for (id, notified) in tracker::EFFECT_TRACKER
        .lock()
        .remove_by_hash(mod_addr, hash)
    {
        on_removed(id, notified);
    }
}

pub fn handle_kill_all(module_accessor: *mut smash::app::BattleObjectModuleAccessor) {
    let mod_addr = module_accessor as u64;
    for (id, notified) in tracker::EFFECT_TRACKER.lock().remove_all_module(mod_addr) {
        on_removed(id, notified);
    }
}

#[skyline::hook(replace = EffectModule::req)]
fn hook_req(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a6: u32,
    a7: i32,
    a8: bool,
    a9: i32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let logical_hash = carrier_proxy_logical_hash(eff_hash.hash);
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, pos, rot, size, a6, a7, a8, a9);
    unsafe { remember_proxy_handle(module_accessor, owner, r, logical_hash) };
    track_spawn(owner, r, logical_hash, 0, false, pos, rot, size);
    r
}

/// The kind a request should ACTUALLY use: transplant/edit alias first, then the co-loaded
/// donor retarget.
///
/// Same order the ACMD path applies them, and that agreement is the point. The alias used to
/// live only in the ACMD hook, so every EffectModule-level path disagreed with it about which
/// kind an effect was — with two visible consequences:
///
///   * `kill_kind(kirby_dash)` did not match an instance spawned as `vsnedit_kirby_dash`, so an
///     edited effect ignored its own stop command and lingered to the end of the animation, and
///   * a re-request that did not come through ACMD spawned the VANILLA kind, so the effect
///     flipped back to unedited for a while (notably on hit) before an ACMD spawn restored it.
///
/// `costume` is the fighter colour index, or -1 when it cannot be resolved — slot-gated aliases
/// then do not match, which is the documented conservative behaviour.
fn alias_and_remap(eff_hash: phx::Hash40, costume: i32) -> phx::Hash40 {
    let aliased = if spawn_rules::any_alias() {
        spawn_rules::alias_for(eff_hash.hash, costume).unwrap_or(eff_hash.hash)
    } else {
        eff_hash.hash
    };
    match crate::slight::effect_viewer::effect_reload::coload_remap(aliased) {
        Some(real) => phx::Hash40 { hash: real },
        None => phx::Hash40 { hash: aliased },
    }
}

/// Fighter colour index for an object, or -1 when it cannot be resolved.
unsafe fn costume_of_boma(boma: *mut smash::app::BattleObjectModuleAccessor) -> i32 {
    if boma.is_null() {
        return -1;
    }
    smash::app::lua_bind::WorkModule::get_int(
        boma,
        *smash::lib::lua_const::FIGHTER_INSTANCE_WORK_ID_INT_COLOR,
    )
}

/// Resolve a requested kind at the req boundary — the robust interception point, since it
/// catches every request path rather than just the lua ACMD argument.
///
/// This applies the transplant/edit ALIAS as well as the co-loaded donor retarget, so that
/// every EffectModule path agrees with the ACMD path about which kind an effect is. It used to
/// apply only the retarget, and that divergence is what let a non-ACMD re-request spawn the
/// vanilla kind over an edited one.
fn remap_eff(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
) -> phx::Hash40 {
    let out = alias_and_remap(eff_hash, unsafe { costume_of_boma(module_accessor) });
    // Log only when the input is a known merged `_os` kind, so we can see whether remap_eff
    // is even reached for the refused spawn and what it decided.
    if effect_names::is_transplant_label(&effect_names::label(eff_hash.hash)) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_os_req.txt")
        {
            let _ = writeln!(
                f,
                "remap_eff in={:#x} out={:#x} mapsize={}",
                eff_hash.hash,
                out.hash,
                crate::slight::effect_viewer::effect_reload::coload_map_size(),
            );
        }
    }
    out
}

#[skyline::hook(replace = EffectModule::req_follow)]
fn hook_req_follow(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    bone_hash: phx::Hash40,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a7: bool,
    a8: u32,
    a9: i32,
    a10: i32,
    a11: i32,
    a12: i32,
    a13: bool,
    a14: bool,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(
        owner, eff_hash, bone_hash, pos, rot, size, a7, a8, a9, a10, a11, a12, a13, a14,
    );
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        bone_hash.hash,
        true,
        pos,
        rot,
        size,
    );
    r
}

#[skyline::hook(replace = EffectModule::req_on_joint)]
fn hook_req_on_joint(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    bone_hash: phx::Hash40,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a7: *const Vector3f,
    a8: *const Vector3f,
    a9: bool,
    a10: u32,
    a11: i32,
    a12: i32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(
        owner, eff_hash, bone_hash, pos, rot, size, a7, a8, a9, a10, a11, a12,
    );
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        bone_hash.hash,
        true,
        pos,
        rot,
        size,
    );
    r
}

#[skyline::hook(replace = EffectModule::req_emit)]
fn hook_req_emit(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    a3: u32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, a3);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        0,
        false,
        std::ptr::null(),
        std::ptr::null(),
        1.0,
    );
    r
}

#[skyline::hook(replace = EffectModule::req_2d)]
fn hook_req_2d(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a6: u32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, pos, rot, size, a6);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(owner, r, eff_hash.hash, 0, false, pos, rot, size);
    r
}

#[skyline::hook(replace = EffectModule::req_common)]
fn hook_req_common(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    size: f32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, size);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        0,
        false,
        std::ptr::null(),
        std::ptr::null(),
        size,
    );
    r
}

#[skyline::hook(replace = EffectModule::req_continual)]
fn hook_req_continual(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    bone_hash: phx::Hash40,
    size: f32,
    a5: u32,
    a6: i32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, bone_hash, size, a5, a6);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        bone_hash.hash,
        true,
        std::ptr::null(),
        std::ptr::null(),
        size,
    );
    r
}

#[skyline::hook(replace = EffectModule::req_time)]
fn hook_req_time(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    a3: i32,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a7: u32,
    a8: bool,
    a9: bool,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, a3, pos, rot, size, a7, a8, a9);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(owner, r, eff_hash.hash, 0, false, pos, rot, size);
    r
}

#[skyline::hook(replace = EffectModule::req_time_follow)]
fn hook_req_time_follow(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    bone_hash: phx::Hash40,
    a4: i32,
    pos: *const Vector3f,
    rot: *const Vector3f,
    size: f32,
    a8: bool,
    a9: u32,
) -> u64 {
    if suppress_carrier_request(module_accessor) {
        return 0;
    }
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash, bone_hash, a4, pos, rot, size, a8, a9);
    unsafe { remember_proxy_handle(module_accessor, owner, r, eff_hash.hash) };
    track_spawn(
        owner,
        r,
        eff_hash.hash,
        bone_hash.hash,
        true,
        pos,
        rot,
        size,
    );
    r
}

#[skyline::hook(replace = EffectModule::kill)]
fn hook_kill(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    handle: u32,
    a3: bool,
    a4: bool,
) {
    let owner = proxy_owner_for_handle(module_accessor, handle).unwrap_or(module_accessor);
    original!()(owner, handle, a3, a4);
    handle_kill(owner, handle);
    PROXY_HANDLES
        .lock()
        .remove(&(module_accessor as usize, handle));
}

/// Bounded trace of every stop we see, one line per (function, kind). Which function actually
/// stops a given effect is not obvious — `kill_kind` turned out to be called about once per
/// session — so this records what really happens rather than what seems likely.
fn trace_stop(func: &str, requested: phx::Hash40, aliased: phx::Hash40, arg: i32) {
    static SEEN: parking_lot::Mutex<Option<Vec<(&'static str, u64)>>> =
        parking_lot::Mutex::new(None);
    let Some(mut g) = SEEN.try_lock() else { return };
    let seen = g.get_or_insert_with(Vec::new);
    // `func` is always a literal from the call sites below.
    let key: &'static str = match func {
        "kill_kind" => "kill_kind",
        "end_kind" => "end_kind",
        "detach_kind" => "detach_kind",
        _ => "other",
    };
    if seen.contains(&(key, requested.hash)) || seen.len() >= 48 {
        return;
    }
    seen.push((key, requested.hash));
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sd:/effect_viewer_kill.txt")
    {
        let _ = writeln!(
            f,
            "{func} requested={} aliased={} arg={arg}",
            effect_names::label(requested.hash),
            effect_names::label(aliased.hash),
        );
    }
}

/// `EFFECT_OFF_KIND(kind, fade, detach)` stops an effect through `end_kind` / `detach_kind`,
/// NOT through `kill_kind` — which is why aliasing only `kill_kind` changed nothing and an
/// edited effect kept ignoring its own stop, lingering until the animation ended.
///
/// Both take the same shape, and both need the same two corrections as `kill_kind`: the kind
/// may have been ALIASED at spawn (`kirby_dash` → `vsnedit_kirby_dash`), and the owner may be
/// the fighter or the carrier. Issue against every combination; stopping a kind an object does
/// not have is a no-op.
macro_rules! stop_kind_hook {
    ($hook:ident, $target:path, $label:literal) => {
        #[skyline::hook(replace = $target)]
        fn $hook(
            module_accessor: *mut smash::app::BattleObjectModuleAccessor,
            eff_hash: phx::Hash40,
            arg3: i32,
        ) -> u64 {
            let costume = unsafe { costume_of_boma(module_accessor) };
            let requested =
                match crate::slight::effect_viewer::effect_reload::coload_remap(eff_hash.hash) {
                    Some(real) => phx::Hash40 { hash: real },
                    None => eff_hash,
                };
            let aliased = alias_and_remap(eff_hash, costume);
            trace_stop($label, requested, aliased, arg3);

            let r = original!()(module_accessor, requested, arg3);
            handle_kill_hash(module_accessor, requested.hash);
            if aliased.hash != requested.hash {
                original!()(module_accessor, aliased, arg3);
                handle_kill_hash(module_accessor, aliased.hash);
            }
            if let Some(carrier) =
                unsafe { crate::slight::effect_viewer::effect_reload::auto_carrier_boma() }
                    .filter(|c| *c != module_accessor)
            {
                original!()(carrier, requested, arg3);
                handle_kill_hash(carrier, requested.hash);
                if aliased.hash != requested.hash {
                    original!()(carrier, aliased, arg3);
                    handle_kill_hash(carrier, aliased.hash);
                }
            }
            r
        }
    };
}

stop_kind_hook!(hook_end_kind, EffectModule::end_kind, "end_kind");
stop_kind_hook!(hook_detach_kind, EffectModule::detach_kind, "detach_kind");

/// Stopping an effect has to reach it wherever it actually is.
///
/// Two things move under our feet here. The KIND may have been aliased at spawn
/// (`kirby_dash` → `vsnedit_kirby_dash`), and the OWNER may be either the fighter (the normal
/// case now) or the carrier (the fallback for kinds the fighter cannot serve). A stop issued
/// against one combination silently misses the other.
///
/// So issue it against every combination. Killing a kind an object does not have is a no-op, so
/// the extra calls cost a lookup and cannot remove anything they should not.
#[skyline::hook(replace = EffectModule::kill_kind)]
fn hook_kill_kind(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
    a3: bool,
    a4: bool,
) -> u64 {
    let costume = unsafe { costume_of_boma(module_accessor) };
    // Deliberately NOT `remap_eff`: that now aliases, and the whole point here is to stop both
    // the kind as requested AND the kind it may have been aliased to at spawn.
    let requested = match crate::slight::effect_viewer::effect_reload::coload_remap(eff_hash.hash) {
        Some(real) => phx::Hash40 { hash: real },
        None => eff_hash,
    };
    let aliased = alias_and_remap(eff_hash, costume);
    let carrier = unsafe { crate::slight::effect_viewer::effect_reload::auto_carrier_boma() };

    // The requested kind on the requesting object is the vanilla behaviour, and its result is
    // what the caller gets back.
    let r = original!()(module_accessor, requested, a3, a4);
    handle_kill_hash(module_accessor, requested.hash);

    let also = |boma: *mut smash::app::BattleObjectModuleAccessor, kind: phx::Hash40| {
        original!()(boma, kind, a3, a4);
        handle_kill_hash(boma, kind.hash);
    };
    if aliased.hash != requested.hash {
        also(module_accessor, aliased);
    }
    if let Some(carrier) = carrier.filter(|c| *c != module_accessor) {
        also(carrier, requested);
        if aliased.hash != requested.hash {
            also(carrier, aliased);
        }
    }
    trace_stop("kill_kind", requested, aliased, a3 as i32);
    r
}

#[skyline::hook(replace = EffectModule::kill_all)]
fn hook_kill_all(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    a2: u32,
    a3: bool,
    a4: bool,
) -> u64 {
    if let Some(carrier) =
        unsafe { crate::slight::effect_viewer::effect_reload::auto_carrier_boma() }
    {
        if carrier != module_accessor {
            original!()(carrier, a2, a3, a4);
            handle_kill_all(carrier);
        }
    }
    let r = original!()(module_accessor, a2, a3, a4);
    handle_kill_all(module_accessor);
    let module_addr = module_accessor as usize;
    PROXY_HANDLES
        .lock()
        .retain(|(source, _), owner| *source != module_addr && *owner != module_addr);
    r
}

#[skyline::hook(replace = EffectModule::remove)]
fn hook_remove(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    a2: u32,
    a3: u32,
) -> u64 {
    let owner = proxy_owner_for_handle(module_accessor, a2).unwrap_or(module_accessor);
    let r = original!()(owner, a2, a3);
    handle_kill(owner, a2);
    if a3 != a2 {
        let owner3 = proxy_owner_for_handle(module_accessor, a3).unwrap_or(module_accessor);
        handle_kill(owner3, a3);
    }
    PROXY_HANDLES.lock().remove(&(module_accessor as usize, a2));
    PROXY_HANDLES.lock().remove(&(module_accessor as usize, a3));
    r
}

#[skyline::hook(replace = EffectModule::remove_common)]
fn hook_remove_common(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
) -> u64 {
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash);
    handle_kill_hash(owner, eff_hash.hash);
    r
}

#[skyline::hook(replace = EffectModule::remove_time)]
fn hook_remove_time(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    eff_hash: phx::Hash40,
) -> u64 {
    let eff_hash = remap_eff(module_accessor, eff_hash);
    let owner = unsafe { spawn_owner(module_accessor, eff_hash.hash) };
    let r = original!()(owner, eff_hash);
    handle_kill_hash(owner, eff_hash.hash);
    r
}

pub fn install_hooks() {
    let _ = std::fs::write("sd:/effect_viewer_carrier_spawn.txt", "");
    skyline::install_hook!(hook_req);
    skyline::install_hook!(hook_req_2d);
    skyline::install_hook!(hook_req_follow);
    skyline::install_hook!(hook_req_on_joint);
    skyline::install_hook!(hook_req_emit);
    skyline::install_hook!(hook_req_common);
    skyline::install_hook!(hook_req_continual);
    skyline::install_hook!(hook_req_time);
    skyline::install_hook!(hook_req_time_follow);
    skyline::install_hook!(hook_kill);
    skyline::install_hook!(hook_kill_kind);
    skyline::install_hook!(hook_end_kind);
    skyline::install_hook!(hook_detach_kind);
    skyline::install_hook!(hook_kill_all);
    skyline::install_hook!(hook_remove);
    skyline::install_hook!(hook_remove_common);
    skyline::install_hook!(hook_remove_time);
    skyline::println!("[SLight] EffectModule hooks installed (Jorge 13 used + extras for parity)");
    acmd_hooks::install();
    crate::slight::hitbox_viewer::install();
    // Register live-eff providers BEFORE the arc filesystem mounts, so reserved
    // sizes for editor-merged eff files are honored from this boot on.
    live_eff::install();
}
