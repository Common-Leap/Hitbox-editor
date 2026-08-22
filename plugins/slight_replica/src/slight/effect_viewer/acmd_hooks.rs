//! ACMD effect capture — hooks the `sv_animcmd::EFFECT*` script primitives.
//!
//! Character move vfx are spawned by ACMD scripts through these lua functions, which do NOT
//! route through the `lua_bind::EffectModule__req*_impl` shims (our module hooks) — which is
//! why character effects never showed while sys_* ones did. The original plugin parsed the
//! same lua args (decomp FUN_71000baa90 calls `lib::L2CAgent::pop_lua_stack` repeatedly).
//!
//! Shared arg layout of the hooked variants (acmd EFFECT macro family):
//!   1 = graphic (hash40), 2 = joint (hash40), 3..5 = pos xyz, 6..8 = rot zr,yr,xr, 9 = size

use smash::lib::{L2CValue, L2CValueType};
use smash::phx::Vector3f;

// `L2CFighterAnimcmdEffectCommon` owns the game's shared status colouring (damage burn,
// invincibility flash, etc.). It is not the fighter's selected move effect script. Remember its
// persistent Lua state without taking a lock on a game worker; colour hooks use this boundary to
// keep common status noise out of the live ACMD capture while leaving fighter-authored commands
// alone.
const COMMON_EFFECT_LUA_SLOTS: usize = 64;
static COMMON_EFFECT_LUA_STATES: [std::sync::atomic::AtomicU64; COMMON_EFFECT_LUA_SLOTS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; COMMON_EFFECT_LUA_SLOTS];

fn remember_common_effect_lua_state(lua_state: u64) {
    if lua_state == 0 {
        return;
    }

    // This is a small open-addressed set rather than one hash slot per state. A collision must
    // fail open (some common feedback may appear) rather than replace a known state and risk
    // treating a move's state as common.
    let start = lua_state as usize % COMMON_EFFECT_LUA_SLOTS;
    for offset in 0..COMMON_EFFECT_LUA_SLOTS {
        let slot = &COMMON_EFFECT_LUA_STATES[(start + offset) % COMMON_EFFECT_LUA_SLOTS];
        match slot.compare_exchange(
            0,
            lua_state,
            std::sync::atomic::Ordering::Release,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(current) if current == lua_state => return,
            Err(_) => {}
        }
    }
}

fn is_common_effect_lua_state(lua_state: u64) -> bool {
    if lua_state == 0 {
        return false;
    }

    let start = lua_state as usize % COMMON_EFFECT_LUA_SLOTS;
    for offset in 0..COMMON_EFFECT_LUA_SLOTS {
        let current = COMMON_EFFECT_LUA_STATES[(start + offset) % COMMON_EFFECT_LUA_SLOTS]
            .load(std::sync::atomic::Ordering::Acquire);
        if current == lua_state {
            return true;
        }
        if current == 0 {
            return false;
        }
    }
    false
}

/// Reserved prefix the editor gives an AUTHORED EDIT's carrier clone.
///
/// Must stay in step with `mod_project::EDIT_CLONE_PREFIX` on the editor side. It is the one
/// signal that tells a redirect apart at spawn time: a target under this prefix means the
/// REQUESTED kind is a real vanilla one (so leaving the request alone still renders something),
/// while any other target means the request is a user-invented transplant name that nothing in
/// the game can serve.
const EDIT_CLONE_PREFIX: &str = "vsnedit_";

/// Distinct effect kinds an ACMD has requested this session (hash → request count). Written
/// to `sd:/effect_viewer_spawn.txt` only when a NEW hash first appears (cheap), so after a
/// move we can read which kinds it spawned by NAME — the direct check for "did the transplant
/// redirect fire?" (e.g. is `alucard_backdash_os` in the list after kirby's backdash?).
/// eff_hash → (request count, last resolved handle). A NON-zero handle means the game found
/// & spawned the kind; `h=0` means the kind wasn't registered (nothing to spawn) — the exact
/// split we need for a transplant: is `alucard_backdash_os` registered after the re-read?
struct SpawnStat {
    count: u32,
    created: bool,
    last_handle: u32,
}
static SPAWN_SEEN: parking_lot::Mutex<Option<std::collections::HashMap<u64, SpawnStat>>> =
    parking_lot::Mutex::new(None);

/// Called across the spawn: `h_before`/`h_after` are get_last_handle before & after original.
/// They differ ⇒ this kind really created an effect (registered). Equal ⇒ NOT-FOUND (a bare
/// non-zero handle is unreliable — it can be a stale handle from a prior effect).
fn record_spawn(eff_hash: u64, h_before: u32, h_after: u32) {
    // Every newly seen kind rewrites the whole file, so a moveset that spawns hundreds of
    // custom kinds pays for it on the game thread. The file answers a research question, not
    // a user-facing one — behind the trace opt-in.
    if !crate::slight::smash_utils::trace_enabled() {
        return;
    }
    let created = h_after != h_before && h_after != 0;
    let mut g = match SPAWN_SEEN.try_lock() {
        Some(g) => g,
        None => return,
    };
    let map = g.get_or_insert_with(std::collections::HashMap::new);
    let e = map.entry(eff_hash).or_insert(SpawnStat {
        count: 0,
        created: false,
        last_handle: 0,
    });
    let is_new = e.count == 0;
    e.count += 1;
    let newly_created = created && !e.created;
    if created {
        e.created = true;
        e.last_handle = h_after;
    }
    if !is_new && !newly_created {
        return;
    }
    let mut lines: Vec<(u64, u32, bool, u32, String)> = map
        .iter()
        .map(|(h, s)| {
            (
                *h,
                s.count,
                s.created,
                s.last_handle,
                crate::slight::effect_viewer::effect_names::label(*h),
            )
        })
        .collect();
    lines.sort_by(|a, b| a.4.cmp(&b.4));
    let mut out = format!("distinct_kinds={}\n", lines.len());
    for (h, c, cr, lh, name) in &lines {
        out.push_str(&format!(
            "{name}  ({h:#x})  x{c}  {}  handle={lh:#x}\n",
            if *cr { "CREATED" } else { "NOT-FOUND" }
        ));
    }
    let _ = std::fs::write("sd:/effect_viewer_spawn.txt", out);
}

/// Read a hash40 arg (Hash or Int lua value); None if the slot isn't hash-like.
unsafe fn arg_hash(agent: &mut smash::lib::L2CAgent, index: i32) -> Option<u64> {
    let v: L2CValue = agent.pop_lua_stack(index);
    match v.val_type {
        L2CValueType::Hash | L2CValueType::Int => Some(v.inner.raw & 0xff_ffff_ffff),
        _ => None,
    }
}

/// Read a numeric arg (Num or Int); defaults when the script omitted it.
unsafe fn arg_num(agent: &mut smash::lib::L2CAgent, index: i32, default: f32) -> f32 {
    let v: L2CValue = agent.pop_lua_stack(index);
    match v.val_type {
        L2CValueType::Num => v.inner.raw_float,
        L2CValueType::Int => (v.inner.raw as i64) as f32,
        _ => default,
    }
}

unsafe fn arg_bool(agent: &mut smash::lib::L2CAgent, index: i32, default: bool) -> bool {
    let v: L2CValue = agent.pop_lua_stack(index);
    match v.val_type {
        L2CValueType::Bool | L2CValueType::Int => v.inner.raw & 1 != 0,
        _ => default,
    }
}

#[derive(Clone, Copy)]
struct ParsedEffectArgs {
    eff_hash: u64,
    bone_hash: u64,
    pos: Vector3f,
    rot: Vector3f,
    size: f32,
    /// Trailing `EffectModule::req` arguments as the ACMD supplied them.
    ///
    /// These used to be hardcoded to `0, 0, false, 0` on the carrier proxy path, so a
    /// proxied effect rendered with different parameters from the same effect spawned
    /// natively — "right effect, right edits, looks wrong". Carrying the script's own values
    /// keeps the proxy faithful.
    arg6: u32,
    arg7: i32,
    arg8: bool,
    arg9: i32,
}

struct PendingCarrierSpawn {
    source_id: u32,
    logical_hash: u64,
    args: ParsedEffectArgs,
    /// Script/scoped transform before global kind pins are overlaid.
    base_args: ParsedEffectArgs,
    is_follow: bool,
    ttl: u16,
}

static PENDING_CARRIER_SPAWNS: std::sync::LazyLock<parking_lot::Mutex<Vec<PendingCarrierSpawn>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));

/// A follow effect cannot be attached directly to Kirby without making Kirby its resource
/// owner. Spawn it as a world-space carrier effect, then mirror Kirby's authored joint transform
/// onto the carrier-owned handle every frame.
struct CarrierFollow {
    source_id: u32,
    owner_id: u32,
    handle: u32,
    logical_hash: u64,
    bone_hash: u64,
    /// Script/scoped values, used again when a global pin is cleared.
    base_local_pos: Vector3f,
    base_local_rot: Vector3f,
}

static CARRIER_FOLLOWS: std::sync::LazyLock<parking_lot::Mutex<Vec<CarrierFollow>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));

unsafe fn source_world_transform(
    source: *mut smash::app::BattleObjectModuleAccessor,
    bone_hash: u64,
    local_pos: &Vector3f,
    local_rot: &Vector3f,
) -> Option<(Vector3f, Vector3f)> {
    if source.is_null() {
        return None;
    }

    // FACING. A fighter-owned effect is mirrored by the fighter's `lr` (-1 when facing left);
    // the carrier is a separate object with its own facing, so a proxied spawn lost the
    // mirroring and sat on the wrong side / faced the wrong way. Apply it to the offset here.
    let lr = smash::app::lua_bind::PostureModule::lr(source);
    let local_pos = &Vector3f {
        x: local_pos.x * lr,
        y: local_pos.y,
        z: local_pos.z,
    };

    if bone_hash == 0 {
        let posture_pos = smash::app::lua_bind::PostureModule::pos(source);
        if posture_pos.is_null() {
            return None;
        }
        return Some((
            Vector3f {
                x: (*posture_pos).x + local_pos.x,
                y: (*posture_pos).y + local_pos.y,
                z: (*posture_pos).z + local_pos.z,
            },
            Vector3f {
                x: local_rot.x,
                // Facing-left flips the effect's yaw, same as a fighter-owned spawn.
                y: if lr < 0.0 {
                    180.0 - local_rot.y
                } else {
                    local_rot.y
                },
                z: local_rot.z,
            },
        ));
    }

    let bone = smash::phx::Hash40 { hash: bone_hash };
    let mut world_pos = Vector3f {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    smash::app::lua_bind::ModelModule::joint_global_position_with_offset(
        source,
        bone,
        local_pos,
        &mut world_pos,
        true,
    );
    let mut joint_rot = Vector3f {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    smash::app::lua_bind::ModelModule::joint_global_rotation(source, bone, &mut joint_rot, true);
    // COMPOSE the rotations, do not add them.
    //
    // This used to be `joint_rot.x + local_rot.x` (and y, z). Euler angles only add when the
    // rotations share an axis; for a general bone orientation the sum is a different rotation
    // entirely, which is why carrier-proxied effects came out at the wrong angle while the
    // effect and its edits were otherwise correct. Compose as matrices and re-extract.
    Some((world_pos, compose_euler_zyx(&joint_rot, local_rot)))
}

/// Compose two ZYX-Euler rotations (degrees) as `joint * local`, returning ZYX Euler degrees.
///
/// The game's `joint_global_rotation` and the ACMD `rot` argument are both ZYX Euler in
/// degrees, so this stays in that convention rather than exposing quaternions to callers.
fn compose_euler_zyx(a: &Vector3f, b: &Vector3f) -> Vector3f {
    fn to_mat(e: &Vector3f) -> [[f32; 3]; 3] {
        let (rx, ry, rz) = (e.x.to_radians(), e.y.to_radians(), e.z.to_radians());
        let (sx, cx) = rx.sin_cos();
        let (sy, cy) = ry.sin_cos();
        let (sz, cz) = rz.sin_cos();
        // R = Rz * Ry * Rx
        [
            [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
            [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
            [-sy, cy * sx, cy * cx],
        ]
    }
    let (m, n) = (to_mat(a), to_mat(b));
    let mut r = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| m[i][k] * n[k][j]).sum();
        }
    }
    // Extract ZYX Euler. Guard gimbal lock (|sin(y)| ~ 1), where x and z are degenerate.
    let sy = -r[2][0];
    let (x, y, z) = if sy.abs() > 0.999_9 {
        (r[1][2].atan2(r[1][1]), sy.clamp(-1.0, 1.0).asin(), 0.0)
    } else {
        (
            r[2][1].atan2(r[2][2]),
            sy.clamp(-1.0, 1.0).asin(),
            r[1][0].atan2(r[0][0]),
        )
    };
    Vector3f {
        x: x.to_degrees(),
        y: y.to_degrees(),
        z: z.to_degrees(),
    }
}

/// Keep carrier-owned EFFECT_FOLLOW instances attached to the source fighter's requested joint.
/// Called from that fighter's game-thread line callback.
pub unsafe fn pump_carrier_follows(source: *mut smash::app::BattleObjectModuleAccessor) {
    if source.is_null() {
        return;
    }
    let source_id = (*source).battle_object_id;

    // A changed carrier needs a short unload/reload window. Preserve requests made during that
    // window and dispatch them on the first frame their stored real kind becomes available.
    let mut ready = Vec::new();
    {
        let mut pending = PENDING_CARRIER_SPAWNS.lock();
        let old = std::mem::take(&mut *pending);
        for mut spawn in old {
            if spawn.source_id != source_id {
                pending.push(spawn);
                continue;
            }
            if crate::slight::effect_viewer::effect_reload::auto_carrier_boma_for_kind(
                spawn.args.eff_hash,
            )
            .is_some()
            {
                ready.push(spawn);
            } else if spawn.ttl > 0
                && crate::slight::effect_viewer::effect_reload::is_staged_carrier_kind(
                    spawn.args.eff_hash,
                )
            {
                spawn.ttl -= 1;
                pending.push(spawn);
            }
        }
    }
    for spawn in ready {
        if spawn_via_carrier(
            source,
            &spawn.args,
            &spawn.base_args,
            spawn.is_follow,
            spawn.logical_hash,
            false,
        ) {
            crate::slight::effect_viewer::kinds::mark_acmd(spawn.logical_hash);
        }
    }

    let current_owner = crate::slight::effect_viewer::effect_reload::auto_carrier_boma();
    let mut follows = CARRIER_FOLLOWS.lock();
    follows.retain(|follow| {
        let Some(owner) = current_owner else {
            return false;
        };
        if (*owner).battle_object_id != follow.owner_id {
            return false;
        }
        if follow.source_id != source_id {
            return true;
        }
        if !smash::app::lua_bind::EffectModule::is_exist_effect(owner, follow.handle) {
            return false;
        }
        // Carrier handles are physically world-space, but the editor exposes the same
        // joint-local offset/rotation values as a normal ACMD follow effect. Resolve the
        // current pin into world space here instead of letting generic live editing feed a
        // local offset directly to EffectModule::set_pos.
        let pins = crate::slight::effect_viewer::kinds::pinned_of(follow.logical_hash);
        let local_pos = pins
            .as_ref()
            .and_then(|p| p.pos.as_ref())
            .map(|p| Vector3f {
                x: p.x,
                y: p.y,
                z: p.z,
            })
            .unwrap_or(follow.base_local_pos);
        let local_rot = pins
            .as_ref()
            .and_then(|p| p.rot.as_ref())
            .map(|r| Vector3f {
                x: r.x,
                y: r.y,
                z: r.z,
            })
            .unwrap_or(follow.base_local_rot);
        let Some((world_pos, world_rot)) =
            source_world_transform(source, follow.bone_hash, &local_pos, &local_rot)
        else {
            return false;
        };
        smash::app::lua_bind::EffectModule::set_pos(owner, follow.handle, &world_pos);
        smash::app::lua_bind::EffectModule::set_rot(owner, follow.handle, &world_rot);
        true
    });
}

/// Stop every carrier-owned follow handle that originated from this fighter and logical effect
/// kind. The game's EFFECT_OFF_KIND runs against the source fighter's EffectModule, but
/// transplanted follow effects physically belong to the hidden carrier. Killing the concrete
/// handles here preserves the script's stop semantics across that ownership boundary.
unsafe fn kill_carrier_follow_kind(
    source: *mut smash::app::BattleObjectModuleAccessor,
    logical_hash: u64,
    fade: bool,
    detach: bool,
) {
    if source.is_null() {
        return;
    }
    let source_id = (*source).battle_object_id;
    let current_owner = crate::slight::effect_viewer::effect_reload::auto_carrier_boma();
    let owner_id = current_owner.map(|owner| (*owner).battle_object_id);

    let mut handles = Vec::new();
    {
        let mut follows = CARRIER_FOLLOWS.lock();
        let old = std::mem::take(&mut *follows);
        for follow in old {
            if follow.source_id == source_id
                && follow.logical_hash == logical_hash
                && Some(follow.owner_id) == owner_id
            {
                handles.push(follow.handle);
            } else {
                follows.push(follow);
            }
        }
    }

    // A stop can arrive while a carrier replacement is still becoming ready. Do not let that
    // delayed request appear after the script has already turned the effect off.
    PENDING_CARRIER_SPAWNS
        .lock()
        .retain(|spawn| !(spawn.source_id == source_id && spawn.logical_hash == logical_hash));

    if let Some(owner) = current_owner {
        for handle in handles {
            smash::app::lua_bind::EffectModule::kill(owner, handle, fade, detach);
        }
    }
}

/// Spawn a carrier-owned kind through the carrier's own EffectModule. ACMD's EFFECT family
/// bypasses the public EffectModule req shims, so rewriting Kirby's lua hash still leaves Kirby
/// as the resource owner. That produces a real handle against the carrier's table but no pixels.
/// Returning true tells the ACMD hook to skip its Kirby-owned original call.
unsafe fn spawn_via_carrier(
    source_boma: *mut smash::app::BattleObjectModuleAccessor,
    args: &ParsedEffectArgs,
    base_args: &ParsedEffectArgs,
    is_follow: bool,
    logical_hash: u64,
    queue_if_unready: bool,
) -> bool {
    let Some((world_pos, world_rot)) =
        source_world_transform(source_boma, args.bone_hash, &args.pos, &args.rot)
    else {
        return false;
    };
    let Some(carrier) =
        crate::slight::effect_viewer::effect_reload::auto_carrier_boma_for_kind(args.eff_hash)
    else {
        if queue_if_unready
            && !source_boma.is_null()
            && crate::slight::effect_viewer::effect_reload::is_staged_carrier_kind(args.eff_hash)
        {
            let mut pending = PENDING_CARRIER_SPAWNS.lock();
            if pending.len() >= 64 {
                pending.remove(0);
            }
            pending.push(PendingCarrierSpawn {
                source_id: (*source_boma).battle_object_id,
                logical_hash,
                args: *args,
                base_args: *base_args,
                is_follow,
                ttl: 300,
            });
            crate::slight::effect_viewer::effect_reload::mark(&format!(
                "carrier_spawn_buffered logical={logical_hash:#x} real={:#x}",
                args.eff_hash
            ));
            // A queued carrier request is accepted by the carrier state machine. There is no
            // EffectModule handle yet, so this is the carrier path's explicit confirmation and
            // prevents a second coroutine-boundary attempt from enqueueing the same request again.
            note_injected_success();
            return true;
        }
        return false;
    };

    let before = smash::app::lua_bind::EffectModule::get_last_handle(carrier) as u32;
    let effect = smash::phx::Hash40 {
        hash: args.eff_hash,
    };
    let _guard = super::CarrierProxyGuard::new(logical_hash);
    // The carrier does not share Kirby's skeleton, so attaching the effect to the same joint name
    // puts it on the carrier's `top` (or fails for fighter-only joints). A non-follow request with
    // an absolute transform preserves carrier ownership; follow behavior is mirrored below.
    let result = smash::app::lua_bind::EffectModule::req(
        carrier, effect, &world_pos, &world_rot, args.size, args.arg6, args.arg7, args.arg8,
        args.arg9,
    );
    let after = smash::app::lua_bind::EffectModule::get_last_handle(carrier) as u32;
    record_spawn(logical_hash, before, after);
    let handle = if result != 0 {
        result as u32
    } else if after != before {
        after
    } else {
        0
    };
    if handle != 0 {
        note_injected_success();
        // Force the absolute transform after creation as well: set_pos/set_rot are explicitly
        // world-space for non-follow handles.
        smash::app::lua_bind::EffectModule::set_pos(carrier, handle, &world_pos);
        smash::app::lua_bind::EffectModule::set_rot(carrier, handle, &world_rot);
        super::remember_proxy_handle(source_boma, carrier, handle as u64, logical_hash);
        // The underlying carrier request is deliberately non-follow/world-space, so its
        // EffectModule hook initially observes world coordinates. Replace that observation
        // with the user-facing ACMD values: joint-local offset/rotation and original scale.
        // This also marks the tracked instance as follow, preventing generic live editing
        // from applying local offsets as absolute world positions.
        super::track_spawn(
            carrier,
            handle as u64,
            logical_hash,
            base_args.bone_hash,
            is_follow,
            &base_args.pos,
            &base_args.rot,
            base_args.size,
        );
        if is_follow && !source_boma.is_null() {
            let source_id = (*source_boma).battle_object_id;
            let owner_id = (*carrier).battle_object_id;
            let mut follows = CARRIER_FOLLOWS.lock();
            if follows.len() >= 512 {
                follows.retain(|follow| {
                    follow.owner_id == owner_id
                        && smash::app::lua_bind::EffectModule::is_exist_effect(
                            carrier,
                            follow.handle,
                        )
                });
            }
            follows.push(CarrierFollow {
                source_id,
                owner_id,
                handle,
                logical_hash,
                bone_hash: base_args.bone_hash,
                base_local_pos: Vector3f {
                    x: base_args.pos.x,
                    y: base_args.pos.y,
                    z: base_args.pos.z,
                },
                base_local_rot: Vector3f {
                    x: base_args.rot.x,
                    y: base_args.rot.y,
                    z: base_args.rot.z,
                },
            });
        }
    }

    static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if crate::slight::smash_utils::trace_enabled()
        && LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 64
    {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sd:/effect_viewer_carrier_spawn.txt")
        {
            let source_id = if source_boma.is_null() {
                0
            } else {
                (*source_boma).battle_object_id
            };
            let owner_id = (*carrier).battle_object_id;
            let _ = writeln!(
                file,
                "ACMD_PROXY logical={logical_hash:#x} real={:#x} source={source_id:#x} owner={owner_id:#x} result={result:#x} before={before:#x} after={after:#x} handle={handle:#x} follow={is_follow} bone={:#x} local=({:.2},{:.2},{:.2}) base=({:.2},{:.2},{:.2}) scale={:.3} base_scale={:.3} world=({:.2},{:.2},{:.2})",
                args.eff_hash,
                args.bone_hash,
                args.pos.x,
                args.pos.y,
                args.pos.z,
                base_args.pos.x,
                base_args.pos.y,
                base_args.pos.z,
                args.size,
                base_args.size,
                world_pos.x,
                world_pos.y,
                world_pos.z,
            );
        }
    }
    true
}

/// Parse EFFECT-family args. MUST run BEFORE original — the sv_animcmd implementation
/// consumes the lua stack, so reading afterwards yields garbage (the first ACMD build did
/// exactly that: hooks fired but every parse bailed and nothing was captured).
///
/// `flip`: EFFECT_FLIP-family layouts carry TWO graphics (left/right) so every later arg
/// shifts by one: (gfxL, gfxR, joint, pos3, rot3, size).
unsafe fn parse_args(lua_state: u64, flip: bool) -> Option<ParsedEffectArgs> {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let eff_hash = arg_hash(&mut agent, 1)?;
    let off: i32 = if flip { 1 } else { 0 };
    let bone_hash = arg_hash(&mut agent, 2 + off).unwrap_or(0);
    let pos = Vector3f {
        x: arg_num(&mut agent, 3 + off, 0.0),
        y: arg_num(&mut agent, 4 + off, 0.0),
        z: arg_num(&mut agent, 5 + off, 0.0),
    };
    let rot = Vector3f {
        x: arg_num(&mut agent, 8 + off, 0.0), // xr
        y: arg_num(&mut agent, 7 + off, 0.0), // yr
        z: arg_num(&mut agent, 6 + off, 0.0), // zr
    };
    let size = arg_num(&mut agent, 9 + off, 1.0);
    // Trailing args, defaulted to what the old hardcoded call used so a macro variant that
    // does not supply them behaves exactly as before.
    Some(ParsedEffectArgs {
        eff_hash,
        bone_hash,
        pos,
        rot,
        size,
        arg6: arg_num(&mut agent, 10 + off, 0.0).max(0.0) as u32,
        arg7: arg_num(&mut agent, 11 + off, 0.0) as i32,
        arg8: arg_bool(&mut agent, 12 + off, false),
        arg9: arg_num(&mut agent, 13 + off, 0.0) as i32,
    })
}

/// Read every lua arg (stops at the first Void — beyond the pushed args). Preserving the
/// exact arg vector lets us rewrite values without knowing each variant's arity.
unsafe fn read_all_args(agent: &mut smash::lib::L2CAgent, max: i32) -> Vec<L2CValue> {
    let mut vals = Vec::new();
    for i in 1..=max {
        let v: L2CValue = agent.pop_lua_stack(i);
        if matches!(v.val_type, L2CValueType::Void) {
            break;
        }
        vals.push(v);
    }
    vals
}

/// Rewrite the spawn args in-place for pinned pos/rot/scale — the effect then SPAWNS with
/// the edited values, in the script's own coordinate space (bone-relative offsets for
/// follow effects — "position offsets instead of flat positions"). Only the pos/rot/size
/// slots change; all other args (including unknown trailing ones) are pushed back verbatim.
unsafe fn rewrite_args(
    lua_state: u64,
    pins: &crate::slight::effect_viewer::kinds::Pinned,
    flip: bool,
) {
    if pins.pos.is_none() && pins.rot.is_none() && pins.scale.is_none() {
        return;
    }
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let mut vals = read_all_args(&mut agent, 20);
    let off = if flip { 1 } else { 0 };
    if vals.len() < 9 + off {
        return; // unexpected arity — leave the script untouched
    }
    // 0-based slots: [2+off..=4+off] = pos xyz, [5+off..=7+off] = rot zr,yr,xr, [8+off] = size
    if let Some(p) = &pins.pos {
        vals[2 + off] = L2CValue::new_num(p.x);
        vals[3 + off] = L2CValue::new_num(p.y);
        vals[4 + off] = L2CValue::new_num(p.z);
    }
    if let Some(r) = &pins.rot {
        vals[5 + off] = L2CValue::new_num(r.z); // zr
        vals[6 + off] = L2CValue::new_num(r.y); // yr
        vals[7 + off] = L2CValue::new_num(r.x); // xr
    }
    if let Some(s) = pins.scale {
        vals[8 + off] = L2CValue::new_num(s);
    }
    agent.clear_lua_stack();
    for v in vals.iter_mut() {
        agent.push_lua_stack(v);
    }
}

/// Rewrite the requested effect kind in-place (live transplant alias): the graphic slot(s)
/// equal to `from` become `to`. FLIP variants carry two graphics (left/right) — both are
/// checked. All other args are pushed back verbatim.
unsafe fn rewrite_kind(lua_state: u64, from: u64, to: u64, flip: bool) {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let mut vals = read_all_args(&mut agent, 20);
    let graphics = if flip { 2 } else { 1 };
    let mut changed = false;
    for v in vals.iter_mut().take(graphics) {
        if matches!(v.val_type, L2CValueType::Hash | L2CValueType::Int)
            && (v.inner.raw & 0xff_ffff_ffff) == from
        {
            *v = L2CValue::new_hash(to);
            changed = true;
        }
    }
    if !changed {
        return;
    }
    agent.clear_lua_stack();
    for v in vals.iter_mut() {
        agent.push_lua_stack(v);
    }
}

/// The owner fighter's costume slot (color index), or -1 when unresolvable.
unsafe fn costume_of(lua_state: u64) -> i32 {
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return -1;
    }
    smash::app::lua_bind::WorkModule::get_int(
        boma,
        *smash::lib::lua_const::FIGHTER_INSTANCE_WORK_ID_INT_COLOR,
    )
}

/// One-time: log the CONCRETE EffectModule vtable targets (req = vtable+0x68,
/// req_common = +0x228) as main-text offsets, so the real kind-resolution path can be
/// decompiled offline. The lua-binding stubs only show the virtual dispatch.
static REQ_VT_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe fn log_req_vtable_once(boma: *mut smash::app::BattleObjectModuleAccessor) {
    use std::sync::atomic::Ordering;
    if !crate::slight::smash_utils::trace_enabled() || REQ_VT_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let obj = *((boma as usize + 0x140) as *const usize);
    if obj == 0 {
        REQ_VT_LOGGED.store(false, Ordering::Relaxed);
        return;
    }
    let vt = *(obj as *const usize);
    let text = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize;
    let f_req = *((vt + 0x68) as *const usize);
    let f_req_common = *((vt + 0x228) as *const usize);
    // The +0x68 target (FUN_0044de70) is another dispatch shell: inner = *(obj+0x10),
    // calls vt2+0x190 then vt2+0x50. Log the CONCRETE targets so the real resolver can
    // be decompiled, plus raw pointers for live TCP `peek` chasing.
    let inner = *((obj + 0x10) as *const usize);
    let (vt2, f190, f50) = if inner != 0 {
        let vt2 = *(inner as *const usize);
        (
            vt2,
            *((vt2 + 0x190) as *const usize),
            *((vt2 + 0x50) as *const usize),
        )
    } else {
        (0, 0, 0)
    };
    // The linked lua_bind shims give the REAL in-game shim addresses (linkage resolved by
    // rtld — hook patches don't change the symbol address). Decompiling the req_follow shim
    // offline yields the exact vtable slot it dispatches through; the full vtable dump below
    // then hands us the concrete impl address without another device round.
    let sh_req_follow = smash::app::lua_bind::EffectModule::req_follow as *const () as usize;
    let sh_req = smash::app::lua_bind::EffectModule::req as *const () as usize;
    let sh_req_on_joint = smash::app::lua_bind::EffectModule::req_on_joint as *const () as usize;
    let mut vt_dump = String::new();
    for slot in 0..96usize {
        let f = *((vt + slot * 8) as *const usize);
        vt_dump.push_str(&format!(
            "vt[{:#x}]=+{:#x}\n",
            slot * 8,
            f.wrapping_sub(text)
        ));
    }
    let _ = std::fs::write(
        "sd:/effect_viewer_reqvt.txt",
        format!(
            "text={text:#x}\nboma_obj={obj:#x}\nvtable=+{:#x}\nshim_req=+{:#x}\nshim_req_follow=+{:#x}\nshim_req_on_joint=+{:#x}\nreq(vt+0x68)=+{:#x}\nreq_common(vt+0x228)=+{:#x}\ninner={inner:#x}\nvt2=+{:#x}\ninner_f190=+{:#x}\ninner_f50=+{:#x}\n--- vtable dump ---\n{vt_dump}",
            vt.wrapping_sub(text),
            sh_req.wrapping_sub(text),
            sh_req_follow.wrapping_sub(text),
            sh_req_on_joint.wrapping_sub(text),
            f_req.wrapping_sub(text),
            f_req_common.wrapping_sub(text),
            vt2.wrapping_sub(text),
            f190.wrapping_sub(text),
            f50.wrapping_sub(text),
        ),
    );
}

// ── Per-spawn LAST_EFFECT_SET_* modifiers ─────────────────────────────────────
//
// None of these last-target modifiers takes an effect kind: they modify whatever spawned last. So
// a rule that
// retunes one spawn cannot be matched at the modifier line itself — there is nothing there to
// match on. The spawn hook, which does know the kind, leaves the wanted values here for the two
// places that need them: the handle it applies to right after the spawn, and the script's own
// modifier line, which runs afterwards and would otherwise overwrite them.
//
// `f32::to_bits` never produces `NO_VALUE`, which is a quiet NaN payload no real value can be.
const NO_VALUE: u32 = u32::MAX;

/// One `Option<f32>` a hook can hand to a later hook on the same thread of execution.
struct PendingValue(std::sync::atomic::AtomicU32);

impl PendingValue {
    const fn new() -> Self {
        Self(std::sync::atomic::AtomicU32::new(NO_VALUE))
    }

    fn set(&self, value: Option<f32>) {
        self.0.store(
            value.map(f32::to_bits).unwrap_or(NO_VALUE),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn get(&self) -> Option<f32> {
        match self.0.load(std::sync::atomic::Ordering::Relaxed) {
            NO_VALUE => None,
            bits => Some(f32::from_bits(bits)),
        }
    }
}

static PENDING_RATE: PendingValue = PendingValue::new();
static PENDING_CAMERA_OFFSET: PendingValue = PendingValue::new();
static PENDING_ALPHA: PendingValue = PendingValue::new();
// Three slots rather than one, so a partially-stored tint can never be read: each component is
// set and read on its own, and `pending_tint` only reports a colour when all three are present.
static PENDING_TINT: [PendingValue; 3] = [
    PendingValue::new(),
    PendingValue::new(),
    PendingValue::new(),
];
static PENDING_PARTICLE_TINT: [PendingValue; 3] = [
    PendingValue::new(),
    PendingValue::new(),
    PendingValue::new(),
];
static PENDING_SCALE_W: [PendingValue; 3] = [
    PendingValue::new(),
    PendingValue::new(),
    PendingValue::new(),
];
static PENDING_SCALE_W_COUNT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn set_pending_rate(rate: Option<f32>) {
    PENDING_RATE.set(rate);
}

fn pending_rate() -> Option<f32> {
    PENDING_RATE.get()
}

fn set_pending_camera_offset(offset: Option<f32>) {
    PENDING_CAMERA_OFFSET.set(offset);
}

fn pending_camera_offset() -> Option<f32> {
    PENDING_CAMERA_OFFSET.get()
}

fn set_pending_tint(tint: Option<[f32; 3]>, alpha: Option<f32>) {
    for (slot, component) in PENDING_TINT.iter().enumerate() {
        component.set(tint.map(|rgb| rgb[slot]));
    }
    PENDING_ALPHA.set(alpha);
}

fn pending_tint() -> Option<[f32; 3]> {
    Some([
        PENDING_TINT[0].get()?,
        PENDING_TINT[1].get()?,
        PENDING_TINT[2].get()?,
    ])
}

fn set_pending_particle_tint(tint: Option<[f32; 3]>) {
    for (slot, component) in PENDING_PARTICLE_TINT.iter().enumerate() {
        component.set(tint.map(|rgb| rgb[slot]));
    }
}

fn pending_particle_tint() -> Option<[f32; 3]> {
    Some([
        PENDING_PARTICLE_TINT[0].get()?,
        PENDING_PARTICLE_TINT[1].get()?,
        PENDING_PARTICLE_TINT[2].get()?,
    ])
}

fn set_pending_scale_w(values: Option<Vec<f32>>) {
    let values = values.filter(|values| {
        (1..=3).contains(&values.len()) && values.iter().all(|value| value.is_finite())
    });
    for (slot, pending) in PENDING_SCALE_W.iter().enumerate() {
        pending.set(values.as_ref().and_then(|values| values.get(slot)).copied());
    }
    PENDING_SCALE_W_COUNT.store(
        values.map_or(0, |values| values.len() as u8),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn pending_scale_w() -> Option<Vec<f32>> {
    let count = PENDING_SCALE_W_COUNT.load(std::sync::atomic::Ordering::Relaxed) as usize;
    if !(1..=3).contains(&count) {
        return None;
    }
    (0..count)
        .map(|slot| PENDING_SCALE_W[slot].get())
        .collect::<Option<Vec<_>>>()
}

/// The editor's per-spawn modifiers, applied to the handle the spawn just produced.
///
/// Called with the handle from before the spawn so a kind that created nothing is not credited
/// with someone else's effect — `get_last_handle` keeps returning the previous one.
unsafe fn apply_pending_modifiers(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    h_before: u32,
    h_after: u32,
) {
    if boma.is_null() || h_after == 0 || h_after == h_before {
        return;
    }
    if let Some(rate) = pending_rate() {
        smash::app::lua_bind::EffectModule::set_rate(boma, h_after, rate);
    }
    if let Some([r, g, b]) = pending_tint() {
        smash::app::lua_bind::EffectModule::set_rgb(boma, h_after, r, g, b);
    }
    if let Some(alpha) = PENDING_ALPHA.get() {
        smash::app::lua_bind::EffectModule::set_alpha(boma, h_after, alpha);
    }
}

/// Apply the camera-flat modifier to a spawn even when the live rule is editing a source that
/// has no `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` line (or when a retime injection replays only
/// the captured spawn call). The same hooked sv_animcmd primitive is used, with a synthetic
/// one-argument lua stack; capture is suppressed because this is editor plumbing, not authored
/// ACMD.
unsafe fn apply_pending_camera_offset(lua_state: u64) {
    let Some(offset) = pending_camera_offset() else {
        return;
    };
    let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    let mut value = L2CValue::new_num(offset);
    agent.push_lua_stack(&mut value);
    hook_last_effect_set_offset_to_camera_flat(lua_state);
    agent.clear_lua_stack();
}

/// Apply a particle tint even when the live rule is editing a source that has no authored
/// `LAST_PARTICLE_SET_COLOR` line, or when a retime injection replays only the captured spawn.
/// The hooked primitive is used instead of guessing an `EffectModule` setter for a particle's
/// distinct target; capture is suppressed because this is editor plumbing, not authored ACMD.
unsafe fn apply_pending_particle_tint(lua_state: u64) {
    let Some([r, g, b]) = pending_particle_tint() else {
        return;
    };
    let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for value in [r, g, b] {
        let mut value = L2CValue::new_num(value);
        agent.push_lua_stack(&mut value);
    }
    hook_last_particle_set_color(lua_state);
    agent.clear_lua_stack();
}

/// Apply the native dynamic-arity scale-W modifier even when the edited source has no authored
/// `LAST_EFFECT_SET_SCALE_W` line, or when a retime injection replays only the captured spawn.
/// The same hooked primitive is used so the live path and exported source share one ABI.
unsafe fn apply_pending_scale_w(lua_state: u64) {
    let Some(values) = pending_scale_w() else {
        return;
    };
    let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for value in values {
        let mut value = L2CValue::new_num(value);
        agent.push_lua_stack(&mut value);
    }
    hook_last_effect_set_scale_w(lua_state);
    agent.clear_lua_stack();
}

/// Overwrite the arguments of a `LAST_EFFECT_SET_*` call with the editor's own values.
///
/// Rewriting the arguments beats calling the `EffectModule` setter after the original: the
/// original runs last either way, so anything applied before it would simply be overwritten by
/// the script's own value. The pending values are left in place rather than cleared, because a
/// second line of the same kind still names the same spawn and must be overridden too.
unsafe fn override_modifier_args(lua_state: u64, values: &[f32]) {
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let mut vals = read_all_args(&mut agent, 4);
    // A call with fewer arguments than the override wants is not this signature, so it is left
    // exactly as the script wrote it rather than half-rewritten.
    if vals.len() < values.len() {
        return;
    }
    for (slot, value) in values.iter().enumerate() {
        vals[slot] = L2CValue::new_num(*value);
    }
    agent.clear_lua_stack();
    for v in vals.iter_mut() {
        agent.push_lua_stack(v);
    }
}

/// Replace the complete dynamic scale-W stack. Its native contract is exactly the one-to-three
/// values represented by the editor, so changing arity is intentional and safe here.
unsafe fn override_scale_w_args(lua_state: u64, values: &[f32]) {
    if !(1..=3).contains(&values.len()) || values.iter().any(|value| !value.is_finite()) {
        return;
    }
    let mut agent = smash::lib::L2CAgent::new(lua_state);
    agent.clear_lua_stack();
    for value in values {
        let mut value = L2CValue::new_num(*value);
        agent.push_lua_stack(&mut value);
    }
}

/// Match a point-control rule against the primitive's authored arguments before the game
/// consumes them. The capture happens first, so a suppressed authored call still teaches the
/// editor what the pristine script did.
unsafe fn control_suppressed(
    lua_state: u64,
    func: &str,
    args: &[crate::slight::hitbox_viewer::LuaArg],
) -> bool {
    if crate::slight::hitbox_viewer::is_injecting() {
        return false;
    }
    if !crate::slight::effect_viewer::control_rules::any_for(func) {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let motion = resolve_injection_motion(boma, CoroutineBoundary::Control, frame).0;
    crate::slight::effect_viewer::control_rules::suppressed(func, args, motion, frame)
}

// `EFFECT_DETACH_KIND` is deliberately NOT hooked at its shared game entry point.
//
// One Slot Effects installs an `A64InlineHook` named `effect_detach_hook` on this exact
// sv_animcmd entry point. Combining that one-instruction inline patch with Skyline's replacement
// hook leaves the inline trampoline returning to the replacement stub's second instruction. That
// instruction branches through x17 after the trampoline has loaded x17 with its own address, so
// the game loops there forever when a script detaches an effect (Ganon's forward smash does this
// after the hit).
//
// Fighter lua2cpp NROs call the shared function through their own JUMP_SLOT relocation. Patch that
// data pointer as each fighter module loads instead: Visionary receives the pristine Lua stack,
// then forwards through its own untouched import to One Slot Effects and the game. This preserves
// capture, suppression, and exact-frame injection without either plugin rewriting the other's
// instructions. `EFFECT_DETACH_KIND_WORK` is a distinct entry point and remains a normal Skyline
// hook below.

const EFFECT_DETACH_KIND_SYMBOL: &str = "_ZN3app10sv_animcmd18EFFECT_DETACH_KINDEP9lua_State";
const R_AARCH64_JUMP_SLOT: u32 = 1026;

/// Per-fighter import wrapper for the shared command One Slot Effects owns.
///
/// This must call the generated binding, not a saved fighter GOT value. Visionary's own import is
/// never rewritten by the NRO-load callback, so this forwards into One Slot Effects' inline hook
/// exactly once and preserves its slot remapping.
unsafe extern "C" fn imported_effect_detach_kind(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 2);
    crate::slight::hitbox_viewer::record(lua_state, "EFFECT_DETACH_KIND", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Control);
    if control_suppressed(lua_state, "EFFECT_DETACH_KIND", &typed) {
        return;
    }
    smash::app::sv_animcmd::EFFECT_DETACH_KIND(lua_state);
}

unsafe fn patch_jump_slot(
    module: *mut nnsdk::root::rtld::ModuleObject,
    symbol: &str,
    replacement: *const (),
) -> usize {
    if module.is_null() {
        return 0;
    }
    let module = &mut *module;
    if module.module_base == 0
        || module.dynsym.is_null()
        || module.dynstr.is_null()
        || module.rela_or_rel_plt_size == 0
    {
        return 0;
    }

    unsafe fn patch_entry(
        module: &mut nnsdk::root::rtld::ModuleObject,
        offset: u64,
        info: u64,
        wanted: &[u8],
        replacement: *const (),
    ) -> bool {
        if info as u32 != R_AARCH64_JUMP_SLOT {
            return false;
        }
        let symbol_index = (info >> 32) as usize;
        if symbol_index >= module.hash_nchain_value as usize {
            return false;
        }
        let symbol = &*module.dynsym.add(symbol_index);
        let name_offset = symbol.st_name as usize;
        if name_offset >= module.dynstr_size as usize {
            return false;
        }
        let remaining = module.dynstr_size as usize - name_offset;
        let name = std::slice::from_raw_parts(module.dynstr.add(name_offset), remaining);
        if name.len() <= wanted.len() || &name[..wanted.len()] != wanted || name[wanted.len()] != 0
        {
            return false;
        }

        let slot = (module.module_base as usize).wrapping_add(offset as usize) as *mut *const ();
        if slot.is_null() {
            return false;
        }
        std::ptr::write_volatile(slot, replacement);
        true
    }

    let wanted = symbol.as_bytes();
    let mut patched = 0;
    if module.is_rela {
        let entries =
            module.rela_or_rel_plt_size as usize / std::mem::size_of::<nnsdk::root::Elf64_Rela>();
        let table = module.rela_or_rel_plt.rela;
        if table.is_null() {
            return 0;
        }
        for index in 0..entries {
            let entry = &*table.add(index);
            patched +=
                patch_entry(module, entry.r_offset, entry.r_info, wanted, replacement) as usize;
        }
    } else {
        let entries =
            module.rela_or_rel_plt_size as usize / std::mem::size_of::<nnsdk::root::Elf64_Rel>();
        let table = module.rela_or_rel_plt.rel;
        if table.is_null() {
            return 0;
        }
        for index in 0..entries {
            let entry = &*table.add(index);
            patched +=
                patch_entry(module, entry.r_offset, entry.r_info, wanted, replacement) as usize;
        }
    }
    patched
}

extern "Rust" fn hook_effect_detach_kind_import(info: &skyline::nro::NroInfo) {
    // Game lua2cpp modules are the authored ACMD callers. Do not redirect arbitrary plugin
    // imports: a plugin may call the command outside an ACMD coroutine and should not be captured
    // as fighter source.
    if !info.name.contains("lua2cpp_") {
        return;
    }

    let module_object = info.module.ModuleObject;
    if module_object.is_null() {
        return;
    }

    let patched = unsafe {
        patch_jump_slot(
            module_object,
            EFFECT_DETACH_KIND_SYMBOL,
            imported_effect_detach_kind as *const (),
        )
    };
    if patched != 0 {
        skyline::println!(
            "[SLight] EFFECT_DETACH_KIND import wrapped for {}",
            info.name
        );
        crate::slight::diag::note(format!(
            "EFFECT_DETACH_KIND import=wrapped module={}",
            info.name
        ));
    }
}

fn install_effect_detach_kind_import_hook() {
    // The standard mod stack already includes libnro_hook. Keeping registration beside the ACMD
    // hooks makes the dependency and the one exceptional command explicit.
    if skyline::nro::add_hook(hook_effect_detach_kind_import).is_ok() {
        skyline::println!("[SLight] EFFECT_DETACH_KIND fighter-import hook registered");
    } else {
        skyline::println!("[SLight] EFFECT_DETACH_KIND import hook unavailable");
        crate::slight::diag::note("EFFECT_DETACH_KIND import=unavailable");
    }
}

/// The wrapper resolves `EFFECT_DETACH_KIND_WORK`'s WorkModule slot before reaching this
/// primitive, so captures contain the runtime handle, not the authored Work ID. That runtime
/// value is still exact for suppression/retime of the captured call; source parsing retains the
/// authored token separately.
#[skyline::hook(replace = smash::app::sv_animcmd::EFFECT_DETACH_KIND_WORK)]
unsafe fn hook_effect_detach_kind_work(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 2);
    crate::slight::hitbox_viewer::record(lua_state, "EFFECT_DETACH_KIND_WORK", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Control);
    if control_suppressed(lua_state, "EFFECT_DETACH_KIND_WORK", &typed) {
        return;
    }
    original!()(lua_state);
}

#[skyline::hook(replace = smash::app::sv_animcmd::ENABLE_AREA)]
unsafe fn hook_enable_area(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "ENABLE_AREA", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Control);
    if control_suppressed(lua_state, "ENABLE_AREA", &typed) {
        return;
    }
    original!()(lua_state);
}

#[skyline::hook(replace = smash::app::sv_animcmd::UNABLE_AREA)]
unsafe fn hook_unable_area(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "UNABLE_AREA", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Control);
    if control_suppressed(lua_state, "UNABLE_AREA", &typed) {
        return;
    }
    original!()(lua_state);
}

/// `LAST_EFFECT_SET_RATE` — timeline data and, when the editor has retuned this spawn, a value
/// to override.
///
/// Recorded so a live capture reconstructs the move's rates: the spawn call itself carries no
/// rate, so without this line the editor would read every captured effect as having none.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_RATE)]
unsafe fn hook_last_effect_set_rate(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_EFFECT_SET_RATE", &typed);

    if let Some(rate) = pending_rate() {
        override_modifier_args(lua_state, &[rate]);
    }

    original!()(lua_state);
}

/// `LAST_EFFECT_SET_WORK_INT` stores the last effect handle in an authored WorkModule slot.
/// Capture the resolved runtime integer for reconstruction, but do not rewrite it: the source
/// token and the runtime handle are different ID spaces and no portable mapping is known here.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_WORK_INT)]
unsafe fn hook_last_effect_set_work_int(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_EFFECT_SET_WORK_INT", &typed);
    original!()(lua_state);
}

/// `LAST_EFFECT_SET_COLOR` — recorded and overridden on the same terms as the rate above.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_COLOR)]
unsafe fn hook_last_effect_set_color(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 3);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_EFFECT_SET_COLOR", &typed);

    if let Some(rgb) = pending_tint() {
        override_modifier_args(lua_state, &rgb);
    }

    original!()(lua_state);
}

/// `LAST_PARTICLE_SET_COLOR` — recorded and overridden separately from the effect tint because
/// its target is the last particle, not the last spawned effect.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_PARTICLE_SET_COLOR)]
unsafe fn hook_last_particle_set_color(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 3);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_PARTICLE_SET_COLOR", &typed);

    if let Some(rgb) = pending_particle_tint() {
        override_modifier_args(lua_state, &rgb);
    }

    original!()(lua_state);
}

/// `LAST_EFFECT_SET_ALPHA` — recorded and overridden on the same terms as the rate above.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_ALPHA)]
unsafe fn hook_last_effect_set_alpha(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_EFFECT_SET_ALPHA", &typed);

    if let Some(alpha) = PENDING_ALPHA.get() {
        override_modifier_args(lua_state, &[alpha]);
    }

    original!()(lua_state);
}

/// `LAST_EFFECT_SET_SCALE_W` reads one through three values from the Lua stack on the pinned
/// game build. Capture the exact arity and replace the complete stack when a live rule supplies
/// edited values, including rules for a source that had no authored line.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_SCALE_W)]
unsafe fn hook_last_effect_set_scale_w(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 3);
    crate::slight::hitbox_viewer::record(lua_state, "LAST_EFFECT_SET_SCALE_W", &typed);

    if let Some(values) = pending_scale_w() {
        override_scale_w_args(lua_state, &values);
    }

    original!()(lua_state);
}

/// `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` — recorded and overridden on the same terms as the
/// other per-spawn modifier lines. The original applies the value to the last effect handle, so
/// rewriting its one numeric argument is enough; no guessed EffectModule setter is needed here.
#[skyline::hook(replace = smash::app::sv_animcmd::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT)]
unsafe fn hook_last_effect_set_offset_to_camera_flat(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(
        lua_state,
        "LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT",
        &typed,
    );

    if let Some(offset) = pending_camera_offset() {
        override_modifier_args(lua_state, &[offset]);
    }

    original!()(lua_state);
}

/// After original ran (effect spawned): track via the spawn path — result_h = 0 makes
/// track_spawn resolve the real handle via EffectModule::get_last_handle.
unsafe fn track(lua_state: u64, args: ParsedEffectArgs, is_follow: bool, h_before: u32) {
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    log_req_vtable_once(boma);
    // Spawn observability: did THIS kind actually create an effect? get_last_handle changing
    // across original!() is the reliable signal (a bare non-zero handle can be stale).
    let h_after = smash::app::lua_bind::EffectModule::get_last_handle(boma) as u32;
    note_injected_handle(h_before, h_after);
    record_spawn(args.eff_hash, h_before, h_after);
    // Before the script's own modifier lines run, so a spawn the script never retunes still
    // gets the editor's values. When there IS such a line, it runs after this and would win —
    // which is why each `hook_last_effect_set_*` rewrites its arguments as well.
    apply_pending_modifiers(boma, h_before, h_after);
    apply_pending_camera_offset(lua_state);
    apply_pending_scale_w(lua_state);
    if h_after != 0 && h_after != h_before {
        apply_pending_particle_tint(lua_state);
    }
    super::track_spawn(
        boma,
        0,
        args.eff_hash,
        args.bone_hash,
        is_follow,
        &args.pos,
        &args.rot,
        args.size,
    );
    crate::slight::effect_viewer::kinds::mark_acmd(args.eff_hash);
}

macro_rules! effect_hook {
    ($hook_name:ident, $target:path, $follow:expr) => {
        effect_hook!($hook_name, $target, $follow, false);
    };
    ($hook_name:ident, $target:path, $follow:expr, $flip:expr) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let source_boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                as *mut smash::app::BattleObjectModuleAccessor;
            if crate::slight::effect_viewer::effect_reload::is_auto_carrier_boma(source_boma) {
                return;
            }
            // Read args BEFORE original consumes the lua stack. `parsed` keeps the SCRIPT'S
            // authored values (pre-rewrite) — the tracked/observed data must never be
            // contaminated by the user's pins.
            let parsed = parse_args(lua_state, $flip);
            if let Some(args) = parsed.as_ref() {
                // A direct effect command can be the first executable statement of a script;
                // give the replacement engine this real command context even when the script
                // has no preceding `frame`, `wait`, or `is_excute` boundary.
                inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);
                // Mark this as ACMD before consulting saved pins. This also migrates away
                // legacy kind-global pos/rot pins before a carrier-owned transplant can
                // mistake those script offsets for absolute world coordinates.
                crate::slight::effect_viewer::kinds::mark_acmd(args.eff_hash);
                // Preserve the script-space transform separately from the final rewritten
                // request. Carrier follow handles need this baseline so clearing a global pin
                // restores the move's own offset/rotation instead of retaining the old pin.
                let mut carrier_base_args = *args;
                // Live-ACMD capture: record the pristine script line (typed args) so the
                // editor can reconstruct the move's effect script from the game itself.
                {
                    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 20);
                    crate::slight::hitbox_viewer::record(
                        lua_state,
                        crate::slight::hitbox_viewer::short_func(stringify!($target)),
                        &typed,
                    );
                }
                // Cleared for every spawn, before any rule is consulted. A modifier left over
                // from the previous spawn would be applied to this one and to whatever modifier
                // line follows it, which is the exact misattribution the `LAST_EFFECT_SET_*`
                // family invites.
                set_pending_rate(None);
                set_pending_camera_offset(None);
                set_pending_tint(None, None);
                set_pending_particle_tint(None);
                set_pending_scale_w(None);
                // Eff-editor spawn rules: suppression + PER-SPAWN transform, both scoped to
                // (motion, frame window) so editing one spawn doesn't affect the others.
                let mut scoped_transform = false;
                if crate::slight::effect_viewer::spawn_rules::any_for(args.eff_hash) {
                    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                        as *mut smash::app::BattleObjectModuleAccessor;
                    let (motion, frame) = if boma.is_null() {
                        (0u64, -1.0f32)
                    } else {
                        let frame = smash::app::lua_bind::MotionModule::frame(boma);
                        (
                            resolve_injection_motion(boma, CoroutineBoundary::Effect, frame).0,
                            frame,
                        )
                    };
                    // A suppressed spawn skips original entirely — the effect never exists
                    // (unlike a visible=false pin, which spawns it).
                    let preserve_authored = !crate::slight::hitbox_viewer::is_injecting()
                        && preserve_authored_effect(source_boma, args.eff_hash, motion, frame);
                    if !crate::slight::hitbox_viewer::is_injecting()
                        && crate::slight::effect_viewer::spawn_rules::suppressed(
                            args.eff_hash, motion, frame,
                        )
                        && !preserve_authored
                    {
                        return;
                    }
                    // Scoped transform wins over the global pin for THIS spawn only.
                    if let Some((pos, rot, scale)) =
                        crate::slight::effect_viewer::spawn_rules::transform_for(
                            args.eff_hash,
                            motion,
                            frame,
                        )
                    {
                        let pins = crate::slight::effect_viewer::kinds::Pinned {
                            pos: pos.map(|p| super::effect_data::Point3D {
                                x: p[0],
                                y: p[1],
                                z: p[2],
                            }),
                            rot: rot.map(|r| super::effect_data::Point3D {
                                x: r[0],
                                y: r[1],
                                z: r[2],
                            }),
                            scale,
                            ..Default::default()
                        };
                        rewrite_args(lua_state, &pins, $flip);
                        if let Some(scoped_args) = parse_args(lua_state, $flip) {
                            carrier_base_args = scoped_args;
                        }
                        scoped_transform = true;
                    }
                    // Looked up separately from the transform: a spawn can be retuned without
                    // being moved, so these must not sit inside the branch above.
                    set_pending_rate(crate::slight::effect_viewer::spawn_rules::rate_for(
                        args.eff_hash,
                        motion,
                        frame,
                    ));
                    set_pending_camera_offset(
                        crate::slight::effect_viewer::spawn_rules::camera_offset_for(
                            args.eff_hash,
                            motion,
                            frame,
                        ),
                    );
                    let (tint, alpha) = crate::slight::effect_viewer::spawn_rules::tint_for(
                        args.eff_hash,
                        motion,
                        frame,
                    )
                    .unwrap_or((None, None));
                    set_pending_tint(tint, alpha);
                    set_pending_particle_tint(
                        crate::slight::effect_viewer::spawn_rules::particle_tint_for(
                            args.eff_hash,
                            motion,
                            frame,
                        ),
                    );
                    set_pending_scale_w(
                        crate::slight::effect_viewer::spawn_rules::scale_w_for(
                            args.eff_hash,
                            motion,
                            frame,
                        ),
                    );
                }
                // Global kind pin (color/speed multipliers, or legacy global pos/rot) —
                // only when no per-spawn transform already rewrote the args.
                if !scoped_transform {
                    if let Some(pins) =
                        crate::slight::effect_viewer::kinds::pinned_of(args.eff_hash)
                    {
                        rewrite_args(lua_state, &pins, $flip);
                    }
                }
                // Co-load retarget: if the move requests a merged `_os` kind DIRECTLY (the
                // baked redirect), and its real donor kind is co-loaded (GPU-valid), rewrite
                // straight to the real kind.
                if let Some(real) =
                    crate::slight::effect_viewer::effect_reload::coload_remap(args.eff_hash)
                {
                    rewrite_kind(lua_state, args.eff_hash, real, $flip);
                    // Verify the rewrite actually took: re-read arg 0's hash from the lua stack.
                    let mut ag = smash::lib::L2CAgent::new(lua_state);
                    let vals = read_all_args(&mut ag, 4);
                    let after = vals
                        .first()
                        .map(|v| v.inner.raw & 0xff_ffff_ffff)
                        .unwrap_or(0);
                    // Unbounded — one line per remapped spawn, so it stays behind the trace
                    // opt-in rather than writing to SD from inside every ACMD frame.
                    use std::io::Write;
                    if let Some(mut f) = crate::slight::smash_utils::trace_enabled()
                        .then(|| {
                            std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open("sd:/effect_viewer_os_req.txt")
                                .ok()
                        })
                        .flatten()
                    {
                        let _ = writeln!(
                            f,
                            "acmd_rewrite from={:#x} to={real:#x} arg0_after={after:#x}",
                            args.eff_hash
                        );
                    }
                }
                // Live transplant alias LAST (after rules/pins keyed on the requested
                // hash): swap the graphic to the donor kind that actually exists in
                // the loaded eff resources, optionally gated to costume slots.
                if crate::slight::effect_viewer::spawn_rules::any_alias() {
                    if let Some(to) = crate::slight::effect_viewer::spawn_rules::alias_for(
                        args.eff_hash,
                        costume_of(lua_state),
                    ) {
                        // If the alias target is a merged `_os` kind but the donor's REAL
                        // kind is now co-loaded (GPU-valid), retarget to the real kind so the
                        // spawn renders. Falls back to the `_os` target when no co-load.
                        let real = crate::slight::effect_viewer::effect_reload::coload_remap(to)
                            .unwrap_or(to);
                        // Rewrite when the target can be served RIGHT NOW — and, for a
                        // transplant, even when it cannot.
                        //
                        // A carrier-owned kind exists only in the carrier's resources, so while
                        // the carrier is still coming up `spawn_via_carrier` cannot serve it.
                        // What to do then depends on what the REQUESTED kind is:
                        //
                        //  - Authored EDIT: the request names a real vanilla kind (kirby_dash)
                        //    and the target is its `vsnedit_` clone. Leaving the request alone
                        //    renders the effect unedited — strictly better than invisible.
                        //  - TRANSPLANT: the request names something the user invented
                        //    (bomberman_bomb_tp). NOTHING in the game has that kind, so leaving
                        //    it alone renders nothing at all. Rewriting costs nothing and lets
                        //    `spawn_via_carrier` BUFFER the spawn (queue_if_unready) so it plays
                        //    as soon as the carrier is up.
                        let carrier_owned =
                            crate::slight::effect_viewer::effect_reload::is_staged_carrier_kind(
                                real,
                            );
                        let has_vanilla_fallback =
                            crate::slight::effect_viewer::effect_names::label(real)
                                .starts_with(EDIT_CLONE_PREFIX);
                        let servable = !carrier_owned
                            || !has_vanilla_fallback
                            || crate::slight::effect_viewer::effect_reload::auto_carrier_boma_for_kind(
                                real,
                            )
                            .is_some();
                        if servable {
                            rewrite_kind(lua_state, args.eff_hash, real, $flip);
                        } else {
                            // Bounded, one line per distinct kind: this is the difference
                            // between "the edit did not apply" and "the effect vanished",
                            // and guessing between those has cost a lot of test runs.
                            static SKIPPED: parking_lot::Mutex<Option<Vec<u64>>> =
                                parking_lot::Mutex::new(None);
                            let mut g = SKIPPED.lock();
                            let seen = g.get_or_insert_with(Vec::new);
                            if !seen.contains(&real) && seen.len() < 32 {
                                seen.push(real);
                                use std::io::Write;
                                if let Ok(mut f) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open("sd:/effect_viewer_alias_skip.txt")
                                {
                                    let _ = writeln!(
                                        f,
                                        "SKIP rewrite {} -> {} (carrier_owned={carrier_owned} \
                                         carrier_state={} kinds={}) — left vanilla so the \
                                         effect still renders",
                                        crate::slight::effect_viewer::effect_names::label(
                                            args.eff_hash
                                        ),
                                        crate::slight::effect_viewer::effect_names::label(real),
                                        crate::slight::effect_viewer::effect_reload::carrier_state(),
                                        crate::slight::effect_viewer::effect_reload::carrier_kind_count(),
                                    );
                                }
                            }
                        }
                    }
                }
                // The FIGHTER owns the spawn whenever it can.
                //
                // The carrier's job is to LOAD the resources; owning the instances too was a
                // separate decision, and a costly one. It is a hidden item with no fighter
                // skeleton, so its effects get an absolute world transform rather than the
                // joint's matrix. Particles tolerate that; a mesh emitter takes its orientation
                // and scale from the owner and drew as a shapeless blob instead of the dash cone.
                //
                // Kinds are registered in the effect manager's global map, so the fighter can
                // request one the carrier loaded. Let the ORIGINAL ACMD run — it already passes
                // the real bone, joint-local offsets and scale — and only fall back to carrier
                // ownership if that produced no effect at all.
                //
                // Scoped strictly to CARRIER-OWNED kinds. Every other effect in the game keeps
                // the untouched path below — this hook runs for every ACMD effect there is, and
                // rerouting all of them to prove a point about mesh emitters would be a very
                // large blast radius for a very small hypothesis.
                let final_args = parse_args(lua_state, $flip);
                let carrier_kind = final_args.as_ref().is_some_and(|a| {
                    crate::slight::effect_viewer::effect_reload::is_staged_carrier_kind(a.eff_hash)
                });
                if carrier_kind {
                    let own_handle = |boma: *mut smash::app::BattleObjectModuleAccessor| -> u32 {
                        if boma.is_null() {
                            0
                        } else {
                            smash::app::lua_bind::EffectModule::get_last_handle(boma) as u32
                        }
                    };
                    let direct_before = own_handle(source_boma);
                    {
                        let _direct = crate::slight::effect_viewer::DirectSpawnGuard::new();
                        original!()(lua_state);
                    }
                    let direct_after = own_handle(source_boma);
                    if direct_after != direct_before && direct_after != 0 {
                        crate::slight::effect_viewer::kinds::mark_acmd(args.eff_hash);
                        track(lua_state, *args, $follow, direct_before);
                        return;
                    }
                    // The fighter could not serve it. Fall back to carrier ownership with an
                    // absolute transform, which is where transplants lived before.
                    if let Some(final_args) = final_args {
                        if spawn_via_carrier(
                            source_boma,
                            &final_args,
                            &carrier_base_args,
                            $follow,
                            args.eff_hash,
                            true,
                        ) {
                            crate::slight::effect_viewer::kinds::mark_acmd(args.eff_hash);
                            return;
                        }
                    }
                    // Neither path produced anything. The original has already run, so returning
                    // here is what keeps it from running twice.
                    return;
                }
            }
            // Handle BEFORE the spawn — if get_last_handle is unchanged after original ran,
            // this kind created NO effect (NOT-FOUND); if it changed, it really spawned.
            let h_before: u32 = if parsed.is_some() {
                let b = smash::app::sv_system::battle_object_module_accessor(lua_state)
                    as *mut smash::app::BattleObjectModuleAccessor;
                if b.is_null() {
                    0
                } else {
                    smash::app::lua_bind::EffectModule::get_last_handle(b) as u32
                }
            } else {
                0
            };
            original!()(lua_state);
            // One-shot probes: fired at all / parse outcome.
            static FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !FIRED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                crate::slight::diag::note(concat!("ACMD first fire: ", stringify!($hook_name)));
            }
            match parsed {
                Some(args) => track(lua_state, args, $follow, h_before),
                None => {
                    static MISS: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !MISS.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        crate::slight::diag::note(concat!(
                            "ACMD parse miss: ",
                            stringify!($hook_name)
                        ));
                    }
                }
            }
        }
    };
}

effect_hook!(hook_eff, smash::app::sv_animcmd::EFFECT, false);
effect_hook!(hook_eff_alpha, smash::app::sv_animcmd::EFFECT_ALPHA, false);
effect_hook!(hook_eff_attr, smash::app::sv_animcmd::EFFECT_ATTR, false);
effect_hook!(hook_eff_follow, smash::app::sv_animcmd::EFFECT_FOLLOW, true);
effect_hook!(
    hook_eff_follow_alpha,
    smash::app::sv_animcmd::EFFECT_FOLLOW_ALPHA,
    true
);
effect_hook!(
    hook_eff_follow_color,
    smash::app::sv_animcmd::EFFECT_FOLLOW_COLOR,
    true
);
effect_hook!(
    hook_eff_follow_no_scale,
    smash::app::sv_animcmd::EFFECT_FOLLOW_NO_SCALE,
    true
);
effect_hook!(
    hook_eff_follow_no_stop,
    smash::app::sv_animcmd::EFFECT_FOLLOW_NO_STOP,
    true
);
effect_hook!(
    hook_eff_follow_no_stop_flip,
    smash::app::sv_animcmd::EFFECT_FOLLOW_NO_STOP_FLIP,
    true,
    true
);
effect_hook!(
    hook_eff_flw_pos,
    smash::app::sv_animcmd::EFFECT_FLW_POS,
    true
);
effect_hook!(
    hook_eff_flw_pos_no_stop,
    smash::app::sv_animcmd::EFFECT_FLW_POS_NO_STOP,
    true
);
effect_hook!(
    hook_eff_flw_pos_unsync_vis,
    smash::app::sv_animcmd::EFFECT_FLW_POS_UNSYNC_VIS,
    true
);
effect_hook!(
    hook_eff_flw_unsync_vis,
    smash::app::sv_animcmd::EFFECT_FLW_UNSYNC_VIS,
    true
);
effect_hook!(
    hook_eff_flip,
    smash::app::sv_animcmd::EFFECT_FLIP,
    false,
    true
);
effect_hook!(
    hook_eff_flip_alpha,
    smash::app::sv_animcmd::EFFECT_FLIP_ALPHA,
    false,
    true
);
effect_hook!(
    hook_eff_follow_flip,
    smash::app::sv_animcmd::EFFECT_FOLLOW_FLIP,
    true,
    true
);
effect_hook!(
    hook_eff_follow_flip_alpha,
    smash::app::sv_animcmd::EFFECT_FOLLOW_FLIP_ALPHA,
    true,
    true
);
effect_hook!(
    hook_eff_follow_flip_color,
    smash::app::sv_animcmd::EFFECT_FOLLOW_FLIP_COLOR,
    true,
    true
);
effect_hook!(
    hook_eff_follow_flip_rnd,
    smash::app::sv_animcmd::EFFECT_FOLLOW_FLIP_RND,
    true,
    true
);
effect_hook!(hook_foot_eff, smash::app::sv_animcmd::FOOT_EFFECT, false);
effect_hook!(
    hook_foot_eff_flip,
    smash::app::sv_animcmd::FOOT_EFFECT_FLIP,
    false,
    true
);
effect_hook!(
    hook_landing_eff,
    smash::app::sv_animcmd::LANDING_EFFECT,
    false
);
effect_hook!(
    hook_landing_eff_flip,
    smash::app::sv_animcmd::LANDING_EFFECT_FLIP,
    false,
    true
);
effect_hook!(hook_down_eff, smash::app::sv_animcmd::DOWN_EFFECT, false);

// ── AFTER_IMAGE trails ───────────────────────────────────────────────────────
//
// The named arg29 wrappers are ordinary sv_animcmd functions. Vanilla also uses the raw
// `sv_module_access::effect` path for `MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON`; there is no
// `AFTER_IMAGE3_ON` Skyline macro to call, so that path stays explicitly bound to the pinned
// native dispatcher and command id.

fn trail_arg_hash(arg: &crate::slight::hitbox_viewer::LuaArg) -> Option<u64> {
    match arg {
        crate::slight::hitbox_viewer::LuaArg::Hash(hash) => Some(*hash),
        crate::slight::hitbox_viewer::LuaArg::Int(value) => Some(*value as u64),
        _ => None,
    }
}

unsafe fn is_fighter_boma(boma: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    !boma.is_null()
        && smash::app::utility::get_category(&mut *boma)
            == *smash::lib::lua_const::BATTLE_OBJECT_CATEGORY_FIGHTER
}

unsafe fn trail_suppressed(lua_state: u64, hash: Option<u64>) -> bool {
    if crate::slight::hitbox_viewer::is_injecting() {
        return false;
    }
    let Some(hash) = hash else {
        return false;
    };
    if !crate::slight::effect_viewer::spawn_rules::any_for(hash) {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    if !is_fighter_boma(boma) {
        return false;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let motion = resolve_injection_motion(boma, CoroutineBoundary::Effect, frame).0;
    crate::slight::effect_viewer::spawn_rules::suppressed(hash, motion, frame)
        && !preserve_authored_effect(boma, hash, motion, frame)
}

unsafe fn lifetime_stop_suppressed(lua_state: u64, func: &str, eff_hash: u64) -> bool {
    if crate::slight::hitbox_viewer::is_injecting() {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    if !is_fighter_boma(boma) {
        return false;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let motion = resolve_injection_motion(boma, CoroutineBoundary::Effect, frame).0;
    crate::slight::effect_viewer::spawn_rules::stop_suppressed(func, eff_hash, motion, frame)
        && !preserve_authored_effect(boma, eff_hash, motion, frame)
}

macro_rules! named_trail_hook {
    ($hook_name:ident, $target:path, $func:literal) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let typed = crate::slight::hitbox_viewer::read_args_exact(lua_state, 29);
            let hash = typed.first().and_then(trail_arg_hash);
            crate::slight::hitbox_viewer::record(lua_state, $func, &typed);
            inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);
            if trail_suppressed(lua_state, hash) {
                return;
            }
            original!()(lua_state);
            if crate::slight::hitbox_viewer::is_injecting() {
                note_injected_success();
            }
        }
    };
}

named_trail_hook!(
    hook_after_image4_on_arg29,
    smash::app::sv_animcmd::AFTER_IMAGE4_ON_arg29,
    "AFTER_IMAGE4_ON_arg29"
);
named_trail_hook!(
    hook_after_image4_on_work_arg29,
    smash::app::sv_animcmd::AFTER_IMAGE4_ON_WORK_arg29,
    "AFTER_IMAGE4_ON_WORK_arg29"
);

/// Capture and optionally suppress the raw AFTER_IMAGE3 command. The command id is argument 0;
/// the editor receives the remaining typed payload so it can retain and replay the exact raw
/// effect call without pretending an unavailable Skyline macro exists.
#[skyline::hook(replace = smash::app::sv_module_access::effect)]
unsafe fn hook_raw_effect(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_exact(lua_state, 27);
    let command = typed.first().and_then(|arg| match arg {
        crate::slight::hitbox_viewer::LuaArg::Int(value) => Some(*value as u64),
        crate::slight::hitbox_viewer::LuaArg::Num(value) => Some(*value as u64),
        crate::slight::hitbox_viewer::LuaArg::Hash(value) => Some(*value),
        _ => None,
    });
    if command == Some(*smash::lib::lua_const::MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON as u64) {
        let payload = &typed[1..];
        let hash = payload.first().and_then(trail_arg_hash);
        crate::slight::hitbox_viewer::record(lua_state, "AFTER_IMAGE3_ON", payload);
        inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);
        if trail_suppressed(lua_state, hash) {
            return;
        }
    }
    original!()(lua_state);
    if command == Some(*smash::lib::lua_const::MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON as u64)
        && crate::slight::hitbox_viewer::is_injecting()
    {
        note_injected_success();
    }
}

/// The trail stop is part of the captured effect timeline. It is also the exact native
/// termination primitive used by retime injection, so the hook records authored calls and lets
/// injected calls pass under `InjectGuard`.
#[skyline::hook(replace = smash::app::sv_animcmd::AFTER_IMAGE_OFF)]
unsafe fn hook_after_image_off(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "AFTER_IMAGE_OFF", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);
    if lifetime_stop_suppressed(
        lua_state,
        "AFTER_IMAGE_OFF",
        smash::hash40("after_image_off"),
    ) {
        return;
    }
    original!()(lua_state);
    if crate::slight::hitbox_viewer::is_injecting() {
        note_injected_success();
    }
}

// ── Model and screen colour ──────────────────────────────────────────────────
//
// `FLASH` and the `BURN_COLOR` family tint the fighter's model or the screen flash. They are
// not spawns — there is no graphic, joint, or handle — but they belong to the move's effect
// timeline, and before this the editor could neither see them in a live capture nor preview a
// change to one.
//
// They name no effect kind either, so a rule for one is keyed on hash40 of the lowercased
// command name; see the note on `SpawnRule::color`. Everything else is the same shape as the
// spawn hooks: record the script's own arguments first, then rewrite the Lua stack before
// `original!()` runs, so the game does the work with the editor's values rather than having
// them applied on top afterwards.

/// One colour command's hook.
///
/// `$slot_base` is where the four colour components start on the Lua stack, which is 1 for the
/// `_FRM` / `_FRAME` forms — the interpolation length comes first — and 0 for the rest. Getting
/// it wrong writes the length into the red channel, which is why it is stated per command here
/// rather than derived from the argument count.
macro_rules! color_hook {
    ($hook_name:ident, $target:path, $command:literal, $lower:literal, $slot_base:expr, $argc:expr) => {
        #[skyline::hook(replace = $target)]
        unsafe fn $hook_name(lua_state: u64) {
            let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, $argc);
            // The common effect agent drives status feedback such as invincibility flashing and
            // damage burn while the selected move is playing. It shares fighter/motion/frame
            // with the move but is not that move's `effect_` script, so recording it makes live
            // fetch invent editable rows. The native command must still run unchanged.
            if !is_common_effect_lua_state(lua_state) {
                crate::slight::hitbox_viewer::record(lua_state, $command, &typed);
                inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);

                let cmd_hash = smash::hash40($lower);
                if crate::slight::effect_viewer::spawn_rules::any_for(cmd_hash) {
                    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                        as *mut smash::app::BattleObjectModuleAccessor;
                    let (motion, frame) = if boma.is_null() {
                        (0u64, -1.0f32)
                    } else {
                        let frame = smash::app::lua_bind::MotionModule::frame(boma);
                        (
                            resolve_injection_motion(boma, CoroutineBoundary::Effect, frame).0,
                            frame,
                        )
                    };
                    // A disabled colour command skips original entirely, so the tint never
                    // happens — the same meaning suppression has for a spawn.
                    let preserve_authored = !crate::slight::hitbox_viewer::is_injecting()
                        && preserve_authored_effect(boma, cmd_hash, motion, frame);
                    if !crate::slight::hitbox_viewer::is_injecting()
                        && crate::slight::effect_viewer::spawn_rules::suppressed(
                            cmd_hash, motion, frame,
                        )
                        && !preserve_authored
                    {
                        return;
                    }
                    if let Some((color, transition)) =
                        crate::slight::effect_viewer::spawn_rules::color_for(
                            cmd_hash, motion, frame,
                        )
                    {
                        let mut agent = smash::lib::L2CAgent::new(lua_state);
                        let mut vals = read_all_args(&mut agent, $argc);
                        let mut wrote = false;
                        if let Some(frames) = transition.filter(|_| $slot_base == 1) {
                            if let Some(slot) = vals.get_mut(0) {
                                *slot = L2CValue::new_num(frames);
                                wrote = true;
                            }
                        }
                        if let Some(rgba) = color {
                            for (offset, component) in rgba.iter().enumerate() {
                                if let Some(slot) = vals.get_mut($slot_base + offset) {
                                    *slot = L2CValue::new_num(*component);
                                    wrote = true;
                                }
                            }
                        }
                        // Only touch the stack if something was actually replaced: clearing and
                        // repushing an unchanged argument list is work the game does not need, and
                        // a short read would otherwise drop arguments the script did pass.
                        if wrote {
                            agent.clear_lua_stack();
                            for v in vals.iter_mut() {
                                agent.push_lua_stack(v);
                            }
                        }
                    }
                }
            }

            original!()(lua_state);
            if crate::slight::hitbox_viewer::is_injecting() {
                note_injected_success();
            }
        }
    };
}

color_hook!(
    hook_flash,
    smash::app::sv_animcmd::FLASH,
    "FLASH",
    "flash",
    0,
    4
);
color_hook!(
    hook_flash_frm,
    smash::app::sv_animcmd::FLASH_FRM,
    "FLASH_FRM",
    "flash_frm",
    1,
    5
);
color_hook!(
    hook_burn_color,
    smash::app::sv_animcmd::BURN_COLOR,
    "BURN_COLOR",
    "burn_color",
    0,
    4
);
color_hook!(
    hook_burn_color_frame,
    smash::app::sv_animcmd::BURN_COLOR_FRAME,
    "BURN_COLOR_FRAME",
    "burn_color_frame",
    1,
    5
);
color_hook!(
    hook_burn_color_normal,
    smash::app::sv_animcmd::BURN_COLOR_NORMAL,
    "BURN_COLOR_NORMAL",
    "burn_color_normal",
    0,
    0
);
color_hook!(
    hook_start_info_flash_eye,
    smash::app::sv_animcmd::START_INFO_FLASH_EYE,
    "START_INFO_FLASH_EYE",
    "start_info_flash_eye",
    0,
    0
);
// `COL_NORMAL` is the colour-blend family's reset — `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL`,
// alongside FLASH above — not the body-collision command the editor's hurtbox panel calls it.
// It belongs on this hook list rather than beside `hook_col_pri` in the hitbox viewer. Taking no
// arguments, the only live edit it has is suppression, which is exactly what an argument-free
// reset should offer: disable it and the tint before it keeps running.
color_hook!(
    hook_col_normal,
    smash::app::sv_animcmd::COL_NORMAL,
    "COL_NORMAL",
    "col_normal",
    0,
    0
);

/// EFFECT_OFF_KIND is both timeline data and a lifetime command. Capture its pristine typed
/// arguments, preserve the game's original call, then bridge the stop to concrete carrier-owned
/// follow handles.
#[skyline::hook(replace = smash::app::sv_animcmd::EFFECT_OFF_KIND)]
unsafe fn hook_effect_off_kind(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 3);
    crate::slight::hitbox_viewer::record(lua_state, "EFFECT_OFF_KIND", &typed);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Effect);

    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let logical_hash = arg_hash(&mut agent, 1);
    let fade = arg_bool(&mut agent, 2, false);
    let detach = arg_bool(&mut agent, 3, true);

    if let Some(logical_hash) = logical_hash {
        if lifetime_stop_suppressed(lua_state, "EFFECT_OFF_KIND", logical_hash) {
            return;
        }
    }

    // Rewrite the KIND ARGUMENT, exactly as the spawn hooks do.
    //
    // Hooking `EffectModule::{kill,end,detach}_kind` does not work for this: those are the
    // lua_bind wrappers, and the game's own call from inside EFFECT_OFF_KIND goes straight to
    // the real method, so the hooks never fire. Measured — after adding all three, the trace
    // file still held exactly one line, for an unrelated kind.
    //
    // The spawn side never had this problem because it rewrites the Lua argument BEFORE calling
    // the original, and the original then does the right thing with it. The stop needs the same
    // shape: hand `original!()` the kind the effect was actually spawned as. Without it the stop
    // names `kirby_dash`, the effect exists as `vsnedit_kirby_dash`, nothing matches, and it
    // survives until the animation ends and everything is flushed.
    if let Some(from) = logical_hash {
        let to = crate::slight::effect_viewer::spawn_rules::alias_for(from, costume_of(lua_state))
            .unwrap_or(from);
        let to = crate::slight::effect_viewer::effect_reload::coload_remap(to).unwrap_or(to);
        if to != from {
            rewrite_kind(lua_state, from, to, false);
        }
        // One line per distinct kind: whether this fires at all, and what it resolved to, is
        // the question two builds of hook-guessing failed to answer.
        static SEEN: parking_lot::Mutex<Option<Vec<u64>>> = parking_lot::Mutex::new(None);
        if let Some(mut g) = SEEN.try_lock() {
            let seen = g.get_or_insert_with(Vec::new);
            if !seen.contains(&from) && seen.len() < 32 {
                seen.push(from);
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("sd:/effect_viewer_kill.txt")
                {
                    let _ = writeln!(
                        f,
                        "EFFECT_OFF_KIND from={} to={} fade={fade} detach={detach}",
                        crate::slight::effect_viewer::effect_names::label(from),
                        crate::slight::effect_viewer::effect_names::label(to),
                    );
                }
            }
        }
    }

    original!()(lua_state);

    if crate::slight::hitbox_viewer::is_injecting() {
        note_injected_success();
    }

    if let Some(logical_hash) = logical_hash {
        let source = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        kill_carrier_follow_kind(source, logical_hash, fade, detach);
    }
}

// ── Live retime injection (from the ACMD coroutine boundary) ─────────────────

/// Native ACMD can resume more than one coroutine for the same object and motion frame. Permit
/// one bounded retry across those resumes, but never carry an unconfirmed request into a later
/// motion frame where it would visibly become a late/original-timing spawn.
const MAX_ATTEMPTS_PER_FRAME: u8 = 2;
const MOTION_FRAME_WIDTH: f32 = 0.5;
const STARTUP_FRAME_MAX: f32 = 1.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoroutineBoundary {
    Start,
    Call,
    Execute,
    Effect,
    Control,
    Frame,
    Wait,
    Resume,
}

impl CoroutineBoundary {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Call => "call",
            Self::Execute => "execute",
            Self::Effect => "effect",
            Self::Control => "control",
            Self::Frame => "frame",
            Self::Wait => "wait",
            Self::Resume => "resume",
        }
    }
}

/// A motion transition publishes its new ACMD coroutines before `MotionModule::motion_kind`
/// becomes observable from every native callback.  Keep the requested hash per battle object so
/// the first command boundary cannot accidentally select the previous move's replacement list.
/// The ACMD scheduler is reached concurrently from the game, effect, sound, and expression
/// coroutines. Horizon's parking path is not safe for those workers: a contended
/// `parking_lot::Mutex` can leave the waiter spinning forever. These small tables therefore use
/// atomics and tolerate a lost hint rather than ever parking a game worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MotionHint {
    motion: u64,
}

const ACMD_STATE_SLOTS: usize = 128;
const ACMD_STATE_RESERVED: u64 = 1;

struct MotionHintSlot {
    /// 0 = empty, 1 = being published, otherwise battle-object id + 2.
    owner: std::sync::atomic::AtomicU64,
    motion: std::sync::atomic::AtomicU64,
}

impl MotionHintSlot {
    const fn new() -> Self {
        Self {
            owner: std::sync::atomic::AtomicU64::new(0),
            motion: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

static MOTION_HINTS: [MotionHintSlot; ACMD_STATE_SLOTS] =
    [const { MotionHintSlot::new() }; ACMD_STATE_SLOTS];

/// `start_coroutine` enters the authored function before its hook returns.  Keep a bounded marker
/// for that first execution so a frame-zero request can be retried from the first real command
/// boundary if the pre-start native dispatch did not create a handle.
static ACMD_STARTUP_PENDING: [std::sync::atomic::AtomicU64; ACMD_STATE_SLOTS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; ACMD_STATE_SLOTS];

/// A motion transition normally resets the latch, but some native replay paths restart an ACMD
/// coroutine without going through a visible MotionModule change. Collapse the several category
/// starts at frame zero into one playback edge and reset once there as well.
static ACMD_PLAYBACK_STARTED: [std::sync::atomic::AtomicU64; ACMD_STATE_SLOTS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; ACMD_STATE_SLOTS];

fn acmd_owner_key(boid: u32) -> u64 {
    boid as u64 + 2
}

fn startup_key(boid: u32, lua_state: u64) -> u64 {
    let mut key = 0xcbf29ce484222325_u64;
    for value in [boid as u64, lua_state] {
        for byte in value.to_le_bytes() {
            key ^= byte as u64;
            key = key.wrapping_mul(0x100000001b3);
        }
    }
    if key <= ACMD_STATE_RESERVED {
        key + 2
    } else {
        key
    }
}

fn remember_motion_hint_atomic(boid: u32, motion: u64) {
    use std::sync::atomic::Ordering;

    let owner = acmd_owner_key(boid);
    let home = owner as usize % ACMD_STATE_SLOTS;
    for offset in 0..ACMD_STATE_SLOTS {
        let slot = &MOTION_HINTS[(home + offset) % ACMD_STATE_SLOTS];
        let current = slot.owner.load(Ordering::Acquire);
        if current == owner {
            slot.motion.store(motion, Ordering::Release);
            return;
        }
        if current == 0
            && slot
                .owner
                .compare_exchange(0, ACMD_STATE_RESERVED, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            slot.motion.store(motion, Ordering::Relaxed);
            slot.owner.store(owner, Ordering::Release);
            return;
        }
    }

    // Stale hints must not make the table permanently full. Replacing the home slot can only
    // lose another object's startup optimization; it cannot alter the native motion call.
    let slot = &MOTION_HINTS[home];
    slot.owner.store(ACMD_STATE_RESERVED, Ordering::Release);
    slot.motion.store(motion, Ordering::Relaxed);
    slot.owner.store(owner, Ordering::Release);
}

fn motion_hint_atomic(boid: u32) -> Option<u64> {
    use std::sync::atomic::Ordering;

    let owner = acmd_owner_key(boid);
    let home = owner as usize % ACMD_STATE_SLOTS;
    for offset in 0..ACMD_STATE_SLOTS {
        let slot = &MOTION_HINTS[(home + offset) % ACMD_STATE_SLOTS];
        if slot.owner.load(Ordering::Acquire) == owner {
            return Some(slot.motion.load(Ordering::Acquire));
        }
    }
    None
}

fn clear_motion_hint_atomic(boid: u32) {
    use std::sync::atomic::Ordering;

    let owner = acmd_owner_key(boid);
    let home = owner as usize % ACMD_STATE_SLOTS;
    for offset in 0..ACMD_STATE_SLOTS {
        let slot = &MOTION_HINTS[(home + offset) % ACMD_STATE_SLOTS];
        if slot
            .owner
            .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
    }
}

fn insert_atomic_set(slots: &[std::sync::atomic::AtomicU64], key: u64) -> bool {
    use std::sync::atomic::Ordering;

    let home = key as usize % slots.len();
    for offset in 0..slots.len() {
        let slot = &slots[(home + offset) % slots.len()];
        let current = slot.load(Ordering::Acquire);
        if current == key {
            return false;
        }
        if current == 0
            && slot
                .compare_exchange(0, key, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            return true;
        }
    }
    // Bounded stale-state recovery. As above, losing one marker means a retry is skipped; it
    // never changes or suppresses the game's authored call.
    slots[home].store(key, Ordering::Release);
    true
}

fn remove_atomic_set(slots: &[std::sync::atomic::AtomicU64], key: u64) -> bool {
    use std::sync::atomic::Ordering;

    let home = key as usize % slots.len();
    for offset in 0..slots.len() {
        let slot = &slots[(home + offset) % slots.len()];
        if slot
            .compare_exchange(key, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
    false
}

unsafe fn remember_motion_hint(boma: *mut smash::app::BattleObjectModuleAccessor, motion: u64) {
    if boma.is_null() {
        return;
    }
    // A new MotionModule transition is also the boundary between two playbacks of the same
    // motion. A frame-zero latch may otherwise still contain a confirmed result from the prior
    // playback when the native frame counter has not yet moved backward far enough to expose the
    // loop. Reset both rule families before publishing the new startup hint.
    reset_effect_injection_latches();
    reset_control_injection_latches();
    remember_motion_hint_atomic((*boma).battle_object_id, motion);
}

unsafe fn remember_acmd_startup(agent: *mut smash::lua2cpp::L2CAgentBase) {
    if agent.is_null() || (*agent).agent.lua_state_agent == 0 {
        return;
    }
    let boma = (*agent).agent.module_accessor;
    if boma.is_null() {
        return;
    }
    let key = startup_key((*boma).battle_object_id, (*agent).agent.lua_state_agent);
    insert_atomic_set(&ACMD_STARTUP_PENDING, key);
}

unsafe fn begin_acmd_playback(boma: *mut smash::app::BattleObjectModuleAccessor) {
    if boma.is_null() {
        return;
    }
    let boid = (*boma).battle_object_id;
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    if frame.is_finite() && frame <= STARTUP_FRAME_MAX {
        let first_category = insert_atomic_set(&ACMD_PLAYBACK_STARTED, acmd_owner_key(boid));
        if first_category {
            reset_effect_injection_latches();
            reset_control_injection_latches();
        }
    } else if frame.is_finite() {
        remove_atomic_set(&ACMD_PLAYBACK_STARTED, acmd_owner_key(boid));
    }
}

unsafe fn note_acmd_progress(boma: *mut smash::app::BattleObjectModuleAccessor, frame: f32) {
    if !boma.is_null() && frame.is_finite() && frame > STARTUP_FRAME_MAX {
        remove_atomic_set(
            &ACMD_PLAYBACK_STARTED,
            acmd_owner_key((*boma).battle_object_id),
        );
    }
}

unsafe fn take_acmd_startup(
    lua_state: u64,
    boma: *mut smash::app::BattleObjectModuleAccessor,
) -> bool {
    if lua_state == 0 || boma.is_null() {
        return false;
    }
    remove_atomic_set(
        &ACMD_STARTUP_PENDING,
        startup_key((*boma).battle_object_id, lua_state),
    )
}

fn choose_motion_hint(
    observed: u64,
    hint: Option<MotionHint>,
    boundary: CoroutineBoundary,
    observed_frame: f32,
    hint_has_rules: bool,
) -> u64 {
    let Some(hint) = hint else {
        return observed;
    };
    if hint.motion == observed || !hint_has_rules {
        return observed;
    }

    // The explicit pre-start frame-zero path and the following command boundaries are inside the
    // transition that requested the hinted motion. A post-call notification is still not a
    // dispatch point. Resume is restricted to the low startup window so an old hint cannot affect
    // a later, unrelated motion.
    let startup = matches!(
        boundary,
        CoroutineBoundary::Start
            | CoroutineBoundary::Execute
            | CoroutineBoundary::Effect
            | CoroutineBoundary::Control
            | CoroutineBoundary::Frame
            | CoroutineBoundary::Wait
    );
    let low_resume = boundary == CoroutineBoundary::Resume
        && observed_frame.is_finite()
        && observed_frame <= 1.0;
    if startup || low_resume {
        hint.motion
    } else {
        observed
    }
}

unsafe fn resolve_injection_motion(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    boundary: CoroutineBoundary,
    observed_frame: f32,
) -> (u64, u64) {
    let observed = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let boid = (*boma).battle_object_id;
    let hint = motion_hint_atomic(boid);
    let hint_has_rules = hint.is_some_and(|hint| {
        crate::slight::effect_viewer::spawn_rules::has_inject_for(hint)
            || crate::slight::effect_viewer::control_rules::has_inject_for(hint)
    });
    let resolved = choose_motion_hint(
        observed,
        hint.map(|motion| MotionHint { motion }),
        boundary,
        observed_frame,
        hint_has_rules,
    );
    if hint.is_some_and(|hint| hint == observed) {
        // Keep the hint alive while the native accessor still publishes the old motion. This
        // covers the entire startup window, including authored suppression and paired stops;
        // discard it as soon as the accessor agrees with the requested hash.
        clear_motion_hint_atomic(boid);
    }
    (resolved, observed)
}

/// The native startup path can briefly report an uninitialized negative frame even though an
/// ACMD coroutine is executing at motion frame zero. The command-boundary hooks are part of that
/// same startup path, so normalize them too; resume boundaries and all positive targets retain
/// the exact native frame.
fn injection_frame(boundary: CoroutineBoundary, observed_frame: f32) -> f32 {
    if matches!(
        boundary,
        CoroutineBoundary::Start
            | CoroutineBoundary::Call
            | CoroutineBoundary::Execute
            | CoroutineBoundary::Effect
            | CoroutineBoundary::Control
            | CoroutineBoundary::Frame
            | CoroutineBoundary::Wait
    ) && observed_frame.is_finite()
        && observed_frame < 0.0
    {
        0.0
    } else {
        observed_frame
    }
}

fn script_frame_to_motion_frame(script_frame: f32) -> Option<f32> {
    script_frame
        .is_finite()
        .then(|| script_frame.max(1.0) - 1.0)
}

/// A bounded, confirmation-aware live injection latch.
///
/// The rule index is the identity of the current desktop rule slot. The motion, target, and
/// content fingerprint ensure a retime, motion change, motion loop, or changed payload cannot
/// inherit an old confirmation. `missed` is deliberately distinct from `confirmed`: a native
/// call that did not produce a confirmation is never reported as fired.
#[derive(Clone, Copy, Debug)]
struct InjectionLatch {
    motion: u64,
    target_motion_frame: f32,
    rule_identity: usize,
    rule_fingerprint: u64,
    last_attempt_frame: f32,
    last_observed_frame: f32,
    attempts: u8,
    confirmed: bool,
    missed: bool,
}

impl InjectionLatch {
    fn same_identity(
        &self,
        motion: u64,
        target_motion_frame: f32,
        rule_identity: usize,
        rule_fingerprint: u64,
    ) -> bool {
        self.motion == motion
            && self.target_motion_frame.to_bits() == target_motion_frame.to_bits()
            && self.rule_identity == rule_identity
            && self.rule_fingerprint == rule_fingerprint
    }
}

fn in_requested_motion_frame(frame: f32, target: f32) -> bool {
    frame >= target && frame < target + MOTION_FRAME_WIDTH
}

fn same_motion_frame(a: f32, b: f32) -> bool {
    (a - b).abs() < MOTION_FRAME_WIDTH
}

/// Pure latch policy used by both the effect and point-control injectors. Calls are only accepted
/// during the requested motion frame, and an unconfirmed request gets one same-frame retry.
fn latch_can_attempt(
    latch: Option<&InjectionLatch>,
    motion: u64,
    target_motion_frame: f32,
    rule_identity: usize,
    rule_fingerprint: u64,
    frame: f32,
) -> bool {
    let Some(latch) = latch else {
        return in_requested_motion_frame(frame, target_motion_frame);
    };
    if !latch.same_identity(motion, target_motion_frame, rule_identity, rule_fingerprint)
        || frame < latch.last_attempt_frame
    {
        return in_requested_motion_frame(frame, target_motion_frame);
    }
    if latch.confirmed || latch.missed || latch.attempts >= MAX_ATTEMPTS_PER_FRAME {
        return false;
    }
    in_requested_motion_frame(frame, target_motion_frame)
}

fn latch_attempt_number(
    latch: Option<&InjectionLatch>,
    motion: u64,
    target_motion_frame: f32,
    rule_identity: usize,
    rule_fingerprint: u64,
    frame: f32,
) -> u8 {
    latch
        .filter(|latch| {
            latch.same_identity(motion, target_motion_frame, rule_identity, rule_fingerprint)
                && frame >= latch.last_attempt_frame
        })
        .filter(|latch| same_motion_frame(latch.last_attempt_frame, frame))
        .map_or(1, |latch| latch.attempts.saturating_add(1))
}

fn store_latch(
    latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_identity: usize,
    rule_fingerprint: u64,
    frame: f32,
    confirmed: bool,
    missed: bool,
) -> u8 {
    let attempts = latch_attempt_number(
        latches.get(&key),
        motion,
        target_motion_frame,
        rule_identity,
        rule_fingerprint,
        frame,
    );
    latches.insert(
        key,
        InjectionLatch {
            motion,
            target_motion_frame,
            rule_identity,
            rule_fingerprint,
            last_attempt_frame: frame,
            last_observed_frame: frame,
            attempts,
            confirmed,
            missed,
        },
    );
    attempts
}

fn mark_latch_missed(
    latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_fingerprint: u64,
    frame: f32,
) -> bool {
    if latches.get(&key).is_some_and(|latch| {
        latch.same_identity(motion, target_motion_frame, key.1, rule_fingerprint)
            && (latch.confirmed || latch.missed)
    }) {
        return false;
    }
    // Marking a request missed is bookkeeping, not another dispatch attempt. Preserve the
    // number of attempts already made during the requested frame for diagnostics.
    let attempts = latches
        .get(&key)
        .filter(|latch| latch.same_identity(motion, target_motion_frame, key.1, rule_fingerprint))
        .map_or(0, |latch| latch.attempts);
    latches.insert(
        key,
        InjectionLatch {
            motion,
            target_motion_frame,
            rule_identity: key.1,
            rule_fingerprint,
            last_attempt_frame: frame,
            last_observed_frame: frame,
            attempts,
            confirmed: false,
            missed: true,
        },
    );
    true
}

#[derive(Clone, Copy)]
struct InjectionAttemptContext {
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_identity: usize,
    rule_fingerprint: u64,
    confirmed: bool,
}

static ACTIVE_INJECTION: std::sync::LazyLock<parking_lot::Mutex<Option<InjectionAttemptContext>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(None));

/// Context for one native replacement dispatch. This is separate from `InjectGuard`: the latter
/// prevents the replacement from entering pristine capture, while this context lets effect hooks
/// report whether their native call actually created the requested runtime object.
struct InjectionAttemptGuard;

impl InjectionAttemptGuard {
    fn new(
        key: (u32, usize),
        motion: u64,
        target_motion_frame: f32,
        rule_fingerprint: u64,
    ) -> Self {
        *ACTIVE_INJECTION.lock() = Some(InjectionAttemptContext {
            key,
            motion,
            target_motion_frame,
            rule_identity: key.1,
            rule_fingerprint,
            confirmed: false,
        });
        Self
    }

    fn confirmed() -> bool {
        ACTIVE_INJECTION
            .lock()
            .as_ref()
            .is_some_and(|context| context.confirmed)
    }
}

impl Drop for InjectionAttemptGuard {
    fn drop(&mut self) {
        *ACTIVE_INJECTION.lock() = None;
    }
}

fn note_injected_success() {
    if let Some(context) = ACTIVE_INJECTION.lock().as_mut() {
        // Touch the identity fields while the context is live. Besides documenting the latch
        // contract, this makes it impossible for a future nested dispatcher to confirm a
        // different rule without replacing the active context first.
        let _ = (
            context.key,
            context.motion,
            context.target_motion_frame,
            context.rule_identity,
            context.rule_fingerprint,
        );
        context.confirmed = true;
    }
}

fn note_injected_handle(h_before: u32, h_after: u32) {
    if h_after != 0 && h_after != h_before {
        note_injected_success();
    }
}

/// An effect command family whose native return is the only confirmation available. Color,
/// stop, and trail primitives do not expose a new ordinary effect handle, so their hook reaching
/// the native dispatcher is their success signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchConfirmation {
    Handle,
    NativeReturn,
}

static EFF_FIRED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u32, usize), InjectionLatch>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
static EFFECT_LATCH_RESET_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static CONTROL_LATCH_RESET_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static LATE_FALLBACK_NOTED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashSet<(u32, u64, u64)>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

/// A desktop rule push replaces the entire sparse rule list. Rule indices are therefore not
/// stable identities: retaining a latch from the previous list can make a newly retimed call
/// look as if it already fired during the current playback.
pub fn reset_effect_injection_latches() {
    if let Some(mut latches) = EFF_FIRED.try_lock() {
        latches.clear();
        EFFECT_LATCH_RESET_PENDING.store(false, std::sync::atomic::Ordering::Release);
    } else {
        // The network thread can install a rule while the game thread is dispatching a
        // coroutine. Never park that thread behind the game thread; the next ACMD boundary
        // services the reset before it reads a latch.
        EFFECT_LATCH_RESET_PENDING.store(true, std::sync::atomic::Ordering::Release);
    }
    if let Some(mut noted) = LATE_FALLBACK_NOTED.try_lock() {
        noted.clear();
    }
}

pub fn reset_control_injection_latches() {
    if let Some(mut latches) = CONTROL_FIRED.try_lock() {
        latches.clear();
        CONTROL_LATCH_RESET_PENDING.store(false, std::sync::atomic::Ordering::Release);
    } else {
        CONTROL_LATCH_RESET_PENDING.store(true, std::sync::atomic::Ordering::Release);
    }
}

fn service_pending_latch_resets() {
    if EFFECT_LATCH_RESET_PENDING.swap(false, std::sync::atomic::Ordering::AcqRel) {
        if let Some(mut latches) = EFF_FIRED.try_lock() {
            latches.clear();
        } else {
            EFFECT_LATCH_RESET_PENDING.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    if CONTROL_LATCH_RESET_PENDING.swap(false, std::sync::atomic::Ordering::AcqRel) {
        if let Some(mut latches) = CONTROL_FIRED.try_lock() {
            latches.clear();
        } else {
            CONTROL_LATCH_RESET_PENDING.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// Keep the authored call visible when an earlier replacement was not confirmed in its exact
/// frame. This is the important late-edit safety boundary: a retime received after frame 1 is
/// reported/missed and waits for the next playback, but it must not hide the original call at
/// its old frame and leave an invisible move in the current playback.
pub unsafe fn preserve_authored_effect(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    authored_hash: u64,
    motion: u64,
    frame: f32,
) -> bool {
    service_pending_latch_resets();
    if boma.is_null() {
        return false;
    }
    let boid = (*boma).battle_object_id;
    let candidates =
        crate::slight::effect_viewer::spawn_rules::replacement_injections_for_suppression(
            authored_hash,
            motion,
            frame,
        );
    let mut late = false;
    {
        let latches = EFF_FIRED.lock();
        for (index, injection, fingerprint) in candidates {
            if frame < injection.frame + MOTION_FRAME_WIDTH {
                continue;
            }
            let confirmed = latches.get(&(boid, index)).is_some_and(|latch| {
                latch.same_identity(motion, injection.frame, index, fingerprint) && latch.confirmed
            });
            if !confirmed {
                late = true;
                break;
            }
        }
    }
    if late {
        let key = (boid, motion, authored_hash);
        let first = {
            let mut noted = LATE_FALLBACK_NOTED.lock();
            if noted.len() < 512 {
                noted.insert(key)
            } else {
                false
            }
        };
        if first {
            crate::slight::diag::note(format!(
                "preserved authored effect after unconfirmed retime: motion {motion:#x} frame {frame:.1} hash {authored_hash:#x} — replacement applies on next playback"
            ));
        }
    }
    late
}

static CONTROL_FIRED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u32, usize), InjectionLatch>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Dispatch a captured EFFECT spawn to the matching sv_animcmd function by its short name.
unsafe fn dispatch_effect(func: &str, lua_state_agent: u64) -> DispatchConfirmation {
    use smash::app::sv_animcmd as sv;
    let confirmation = match func {
        "EFFECT_OFF_KIND"
        | "AFTER_IMAGE_OFF"
        | "AFTER_IMAGE3_ON"
        | "AFTER_IMAGE4_ON_arg29"
        | "AFTER_IMAGE4_ON_WORK_arg29"
        | "FLASH"
        | "FLASH_FRM"
        | "BURN_COLOR"
        | "BURN_COLOR_FRAME"
        | "BURN_COLOR_NORMAL"
        | "START_INFO_FLASH_EYE"
        | "COL_NORMAL" => DispatchConfirmation::NativeReturn,
        _ => DispatchConfirmation::Handle,
    };
    match func {
        "EFFECT_OFF_KIND" => sv::EFFECT_OFF_KIND(lua_state_agent),
        "AFTER_IMAGE_OFF" => sv::AFTER_IMAGE_OFF(lua_state_agent),
        // This is the measured native raw-effect route for AFTER_IMAGE3_ON. No Skyline
        // AFTER_IMAGE3_ON macro exists in the pinned bindings, so do not substitute an arg29
        // wrapper with a different ABI.
        "AFTER_IMAGE3_ON" => smash::app::sv_module_access::effect(lua_state_agent),
        "AFTER_IMAGE4_ON_arg29" => sv::AFTER_IMAGE4_ON_arg29(lua_state_agent),
        "AFTER_IMAGE4_ON_WORK_arg29" => sv::AFTER_IMAGE4_ON_WORK_arg29(lua_state_agent),
        "FLASH" => sv::FLASH(lua_state_agent),
        "FLASH_FRM" => sv::FLASH_FRM(lua_state_agent),
        "BURN_COLOR" => sv::BURN_COLOR(lua_state_agent),
        "BURN_COLOR_FRAME" => sv::BURN_COLOR_FRAME(lua_state_agent),
        "BURN_COLOR_NORMAL" => sv::BURN_COLOR_NORMAL(lua_state_agent),
        "START_INFO_FLASH_EYE" => sv::START_INFO_FLASH_EYE(lua_state_agent),
        "COL_NORMAL" => sv::COL_NORMAL(lua_state_agent),
        "DOWN_EFFECT" => sv::DOWN_EFFECT(lua_state_agent),
        "FOOT_EFFECT" => sv::FOOT_EFFECT(lua_state_agent),
        "FOOT_EFFECT_FLIP" => sv::FOOT_EFFECT_FLIP(lua_state_agent),
        "LANDING_EFFECT" => sv::LANDING_EFFECT(lua_state_agent),
        "LANDING_EFFECT_FLIP" => sv::LANDING_EFFECT_FLIP(lua_state_agent),
        "EFFECT_ALPHA" => sv::EFFECT_ALPHA(lua_state_agent),
        "EFFECT_ATTR" => sv::EFFECT_ATTR(lua_state_agent),
        "EFFECT_FOLLOW" => sv::EFFECT_FOLLOW(lua_state_agent),
        "EFFECT_FOLLOW_ALPHA" => sv::EFFECT_FOLLOW_ALPHA(lua_state_agent),
        "EFFECT_FOLLOW_COLOR" => sv::EFFECT_FOLLOW_COLOR(lua_state_agent),
        "EFFECT_FOLLOW_NO_SCALE" => sv::EFFECT_FOLLOW_NO_SCALE(lua_state_agent),
        "EFFECT_FOLLOW_NO_STOP" => sv::EFFECT_FOLLOW_NO_STOP(lua_state_agent),
        "EFFECT_FOLLOW_NO_STOP_FLIP" => sv::EFFECT_FOLLOW_NO_STOP_FLIP(lua_state_agent),
        "EFFECT_FLW_POS" => sv::EFFECT_FLW_POS(lua_state_agent),
        "EFFECT_FLW_POS_NO_STOP" => sv::EFFECT_FLW_POS_NO_STOP(lua_state_agent),
        "EFFECT_FLW_POS_UNSYNC_VIS" => sv::EFFECT_FLW_POS_UNSYNC_VIS(lua_state_agent),
        "EFFECT_FLW_UNSYNC_VIS" => sv::EFFECT_FLW_UNSYNC_VIS(lua_state_agent),
        "EFFECT_FLIP" => sv::EFFECT_FLIP(lua_state_agent),
        "EFFECT_FLIP_ALPHA" => sv::EFFECT_FLIP_ALPHA(lua_state_agent),
        "EFFECT_FOLLOW_FLIP" => sv::EFFECT_FOLLOW_FLIP(lua_state_agent),
        "EFFECT_FOLLOW_FLIP_ALPHA" => sv::EFFECT_FOLLOW_FLIP_ALPHA(lua_state_agent),
        "EFFECT_FOLLOW_FLIP_COLOR" => sv::EFFECT_FOLLOW_FLIP_COLOR(lua_state_agent),
        "EFFECT_FOLLOW_FLIP_RND" => sv::EFFECT_FOLLOW_FLIP_RND(lua_state_agent),
        _ => sv::EFFECT(lua_state_agent),
    }
    confirmation
}

unsafe fn dispatch_control(func: &str, lua_state_agent: u64) -> bool {
    use smash::app::sv_animcmd as sv;
    match func {
        "EFFECT_DETACH_KIND" => {
            sv::EFFECT_DETACH_KIND(lua_state_agent);
            true
        }
        "EFFECT_DETACH_KIND_WORK" => {
            sv::EFFECT_DETACH_KIND_WORK(lua_state_agent);
            true
        }
        "ENABLE_AREA" => {
            sv::ENABLE_AREA(lua_state_agent);
            true
        }
        "UNABLE_AREA" => {
            sv::UNABLE_AREA(lua_state_agent);
            true
        }
        _ => false,
    }
}

fn clear_stale_latch(
    latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_fingerprint: u64,
    frame: f32,
) {
    if latches.get(&key).is_some_and(|latch| {
        !latch.same_identity(motion, target_motion_frame, key.1, rule_fingerprint)
            || frame < latch.last_observed_frame
    }) {
        latches.remove(&key);
    }
}

fn observed_latch(
    latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_fingerprint: u64,
    frame: f32,
) -> Option<InjectionLatch> {
    clear_stale_latch(
        latches,
        key,
        motion,
        target_motion_frame,
        rule_fingerprint,
        frame,
    );
    if let Some(latch) = latches.get_mut(&key) {
        latch.last_observed_frame = frame;
    }
    latches.get(&key).copied()
}

fn report_missed_frame(
    latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
    key: (u32, usize),
    motion: u64,
    target_motion_frame: f32,
    rule_fingerprint: u64,
    frame: f32,
) -> bool {
    mark_latch_missed(
        latches,
        key,
        motion,
        target_motion_frame,
        rule_fingerprint,
        frame,
    )
}

unsafe fn load_args(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    args: &[crate::slight::hitbox_viewer::LuaArg],
) {
    agent.agent.clear_lua_stack();
    for arg in args {
        let mut value = arg.to_l2c();
        agent.agent.push_lua_stack(&mut value);
    }
}

/// Fire due C4 point-control injections from the live ACMD agent. Work-slot controls replay the
/// captured runtime handle, which is exact for the live call even though the source editor keeps
/// the authored Work ID token separately.
unsafe fn inject_control(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: u64,
    boundary: CoroutineBoundary,
    frame: f32,
    observed_frame: f32,
) {
    use crate::slight::effect_viewer::control_rules;
    if !control_rules::any_inject() {
        return;
    }
    let boid = (*boma).battle_object_id;
    for (idx, injection, work_slot, rule_fingerprint) in control_rules::injections_for(motion) {
        let key = (boid, idx);
        let latch = {
            let mut fired = CONTROL_FIRED.lock();
            observed_latch(
                &mut fired,
                key,
                motion,
                injection.frame,
                rule_fingerprint,
                frame,
            )
        };
        if frame >= injection.frame + MOTION_FRAME_WIDTH {
            let report = report_missed_frame(
                &mut CONTROL_FIRED.lock(),
                key,
                motion,
                injection.frame,
                rule_fingerprint,
                frame,
            );
            if report {
                crate::slight::diag::note(format!(
                    "effect control injection missed exact frame: boundary={boundary} motion {motion:#x} target {target:.1} current {frame:.1} observed {observed_frame:.1} command '{command}' attempts={attempts} confirmation=unconfirmed",
                    boundary = boundary.label(),
                    command = injection.func,
                    target = injection.frame,
                    attempts = CONTROL_FIRED
                        .lock()
                        .get(&key)
                        .map(|latch| latch.attempts)
                        .unwrap_or(0),
                ));
            }
            continue;
        }
        if !latch_can_attempt(
            latch.as_ref(),
            motion,
            injection.frame,
            idx,
            rule_fingerprint,
            frame,
        ) {
            continue;
        }

        let mut args = injection.args.clone();
        if let Some(slot) = work_slot {
            if injection.func != "EFFECT_DETACH_KIND_WORK" || args.len() != 2 {
                crate::slight::diag::note(
                    "effect control work-slot injection rejected: unexpected command shape",
                );
                continue;
            }
            let handle = smash::app::lua_bind::WorkModule::get_int64(boma, slot);
            args[0] = crate::slight::hitbox_viewer::LuaArg::Int(handle as i64);
        }
        load_args(agent, &args);
        let confirmed = {
            let _attempt =
                InjectionAttemptGuard::new(key, motion, injection.frame, rule_fingerprint);
            let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
            let dispatched = dispatch_control(&injection.func, agent.agent.lua_state_agent);
            if dispatched {
                note_injected_success();
            }
            InjectionAttemptGuard::confirmed()
        };
        agent.agent.clear_lua_stack();
        let attempts = store_latch(
            &mut CONTROL_FIRED.lock(),
            key,
            motion,
            injection.frame,
            idx,
            rule_fingerprint,
            frame,
            confirmed,
            false,
        );
        crate::slight::diag::note(format!(
            "effect control injection '{}' (boundary={boundary} motion {motion:#x} target {target:.1} current {frame:.1} observed {observed_frame:.1} attempt {attempts}/{MAX_ATTEMPTS_PER_FRAME} confirmation={status})",
            injection.func,
            boundary = boundary.label(),
            target = injection.frame,
            status = if confirmed { "confirmed" } else { "unconfirmed" },
        ));
    }
}

unsafe fn inject_effect(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: u64,
    boundary: CoroutineBoundary,
    frame: f32,
    observed_frame: f32,
) {
    use crate::slight::effect_viewer::spawn_rules;
    if !spawn_rules::any_inject() {
        return;
    }
    let injections = spawn_rules::injections_for(motion);
    if injections.is_empty() {
        return;
    }
    let boid = (*boma).battle_object_id;

    for (idx, inj, rule_fingerprint) in injections {
        let key = (boid, idx);
        let latch = {
            let mut fired = EFF_FIRED.lock();
            observed_latch(&mut fired, key, motion, inj.frame, rule_fingerprint, frame)
        };
        if frame >= inj.frame + MOTION_FRAME_WIDTH {
            let report = report_missed_frame(
                &mut EFF_FIRED.lock(),
                key,
                motion,
                inj.frame,
                rule_fingerprint,
                frame,
            );
            if report {
                crate::slight::diag::note(format!(
                    "effect injection missed exact frame: boundary={boundary} motion {motion:#x} target {target:.1} current {frame:.1} observed {observed_frame:.1} command '{command}' attempts={attempts} confirmation=unconfirmed",
                    boundary = boundary.label(),
                    command = inj.func,
                    target = inj.frame,
                    attempts = EFF_FIRED
                        .lock()
                        .get(&key)
                        .map(|latch| latch.attempts)
                        .unwrap_or(0),
                ));
            }
            continue;
        }
        if !latch_can_attempt(
            latch.as_ref(),
            motion,
            inj.frame,
            idx,
            rule_fingerprint,
            frame,
        ) {
            continue;
        }

        load_args(agent, &inj.args);
        let confirmed = {
            let _attempt = InjectionAttemptGuard::new(key, motion, inj.frame, rule_fingerprint);
            // Replays re-enter our own EFFECT hooks — keep them out of pristine capture and
            // authored suppression, or the editor will observe its own retime as source data.
            let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
            let confirmation = dispatch_effect(&inj.func, agent.agent.lua_state_agent);
            if confirmation == DispatchConfirmation::NativeReturn {
                note_injected_success();
            }
            InjectionAttemptGuard::confirmed()
        };
        agent.agent.clear_lua_stack();
        let attempts = store_latch(
            &mut EFF_FIRED.lock(),
            key,
            motion,
            inj.frame,
            idx,
            rule_fingerprint,
            frame,
            confirmed,
            false,
        );
        crate::slight::diag::note(format!(
            "effect injection '{}' (boundary={boundary} motion {motion:#x} target {target:.1} current {frame:.1} observed {observed_frame:.1} attempt {attempts}/{MAX_ATTEMPTS_PER_FRAME} confirmation={status})",
            inj.func,
            boundary = boundary.label(),
            target = inj.frame,
            status = if confirmed { "confirmed" } else { "unconfirmed" },
        ));
    }
}

unsafe fn inject_for_lua_state_at_frame(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    boma: *mut smash::app::BattleObjectModuleAccessor,
    boundary: CoroutineBoundary,
    frame_override: Option<f32>,
) {
    service_pending_latch_resets();
    crate::slight::effect_viewer::spawn_rules::service_pending();
    crate::slight::effect_viewer::control_rules::service_pending();
    // Without a structural retime there is nothing for the boundary scheduler to do. In
    // particular, ordinary capture must not enter native motion queries or scheduler state just
    // because an effect command happened to run. Suppression/value rules are handled locally by
    // their command hooks and do not need this path.
    if !crate::slight::effect_viewer::spawn_rules::any_inject()
        && !crate::slight::effect_viewer::control_rules::any_inject()
    {
        return;
    }
    if boma.is_null()
        || agent.agent.lua_state_agent == 0
        || crate::slight::hitbox_viewer::is_injecting()
    {
        return;
    }
    let observed_frame = smash::app::lua_bind::MotionModule::frame(boma);
    note_acmd_progress(boma, observed_frame);
    let frame = frame_override.unwrap_or_else(|| injection_frame(boundary, observed_frame));
    // A post-start/call notification is not a reliable native command context. The explicit
    // start path passes a frame-zero override before the native body runs; a plain lifecycle
    // notification must not consume an attempt by itself.
    if matches!(boundary, CoroutineBoundary::Start | CoroutineBoundary::Call)
        && frame_override.is_none()
    {
        return;
    }
    let (motion, observed_motion) = resolve_injection_motion(boma, boundary, observed_frame);
    if motion != observed_motion {
        crate::slight::diag::note(format!(
            "ACMD injection resolved stale motion hash: observed {observed_motion:#x} requested {motion:#x} boundary={}",
            boundary.label(),
        ));
    }
    inject_control(agent, boma, motion, boundary, frame, observed_frame);
    inject_effect(agent, boma, motion, boundary, frame, observed_frame);
}

unsafe fn inject_for_lua_state(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    boma: *mut smash::app::BattleObjectModuleAccessor,
    boundary: CoroutineBoundary,
) {
    inject_for_lua_state_at_frame(agent, boma, boundary, None);
}

/// Replay live rules from an ACMD agent rather than a status-line callback. All ACMD agents for a
/// battle object expose the same module accessor; the replacement only needs the typed Lua stack
/// and that accessor, so it must not depend on having observed an effect command from this exact
/// coroutine first. A newly created effect coroutine can otherwise miss script frame one simply
/// because its first authored effect is later in the function. The latch makes a confirmed
/// replacement idempotent across the shared ACMD boundaries. Lifecycle-only Start/Call
/// notifications are deliberately ignored by `inject_for_lua_state`.
unsafe fn inject_for_acmd(agent: &mut smash::lua2cpp::L2CAgentBase, boundary: CoroutineBoundary) {
    let boma = agent.agent.module_accessor;
    inject_for_lua_state(agent, boma, boundary);
}

unsafe fn inject_for_acmd_at_frame(
    agent: &mut smash::lua2cpp::L2CAgentBase,
    boundary: CoroutineBoundary,
    frame: f32,
) {
    let boma = agent.agent.module_accessor;
    inject_for_lua_state_at_frame(agent, boma, boundary, Some(frame));
}

/// The `frame` and `wait` primitives are reliable points inside an ACMD coroutine. The native
/// primitive must return first: only then has the coroutine reached the requested motion frame
/// and the game has established the command's valid execution context. A temporary L2C agent is
/// sufficient here: replacement dispatch only uses its Lua stack, while the module accessor comes
/// from the captured state.
unsafe fn inject_before_acmd_wait(lua_state: u64, boundary: CoroutineBoundary) {
    if lua_state == 0 || crate::slight::hitbox_viewer::is_injecting() {
        return;
    }
    if !crate::slight::effect_viewer::spawn_rules::any_inject()
        && !crate::slight::effect_viewer::control_rules::any_inject()
    {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    let observed_frame = smash::app::lua_bind::MotionModule::frame(boma);
    let startup = take_acmd_startup(lua_state, boma)
        && (!observed_frame.is_finite() || observed_frame <= 1.0);
    let mut agent = smash::lua2cpp::L2CAgentBase {
        agent: smash::lib::L2CAgent::new(lua_state),
        unk48: [0; 0x10],
    };
    if startup {
        inject_for_lua_state_at_frame(&mut agent, boma, boundary, Some(0.0));
    } else {
        inject_for_lua_state(&mut agent, boma, boundary);
    }
}

/// `frame()` carries the script's one-based target even while the native motion accessor is
/// crossing the first frame boundary. Use that target after the wait returns so script frames
/// one, two, and three become motion frames zero, one, and two without relying on a transient
/// startup value from `MotionModule::frame`.
unsafe fn inject_after_acmd_frame(lua_state: u64, target: f32) {
    if lua_state == 0 || crate::slight::hitbox_viewer::is_injecting() {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    // A startup marker that survived until an absolute wait means the script did not expose a
    // frame-zero command boundary. Do not reuse it to pretend that the later wait is frame zero;
    // the pre-start attempt has already covered that exact target.
    let _ = take_acmd_startup(lua_state, boma);
    let mut agent = smash::lua2cpp::L2CAgentBase {
        agent: smash::lib::L2CAgent::new(lua_state),
        unk48: [0; 0x10],
    };
    inject_for_lua_state_at_frame(
        &mut agent,
        boma,
        CoroutineBoundary::Frame,
        script_frame_to_motion_frame(target),
    );
}

/// Record the requested motion before the native transition runs.  On the first ACMD boundary
/// the module's published `motion_kind` can still be the previous move, so observing it after
/// `original!()` is too late for a frame-zero replacement.
#[skyline::hook(replace = smash::app::lua_bind::MotionModule::change_motion)]
unsafe fn hook_motion_change(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: smash::phx::Hash40,
    arg3: f32,
    arg4: f32,
    arg5: bool,
    arg6: f32,
    arg7: bool,
    arg8: bool,
) -> u64 {
    remember_motion_hint(boma, motion.hash);
    original!()(boma, motion, arg3, arg4, arg5, arg6, arg7, arg8)
}

#[skyline::hook(replace = smash::app::lua_bind::MotionModule::change_motion_inherit_frame)]
unsafe fn hook_motion_change_inherit_frame(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: smash::phx::Hash40,
    frame: f32,
    rate: f32,
    arg5: f32,
    arg6: bool,
    arg7: bool,
) -> u64 {
    remember_motion_hint(boma, motion.hash);
    original!()(boma, motion, frame, rate, arg5, arg6, arg7)
}

#[skyline::hook(
    replace = smash::app::lua_bind::MotionModule::change_motion_inherit_frame_keep_rate
)]
unsafe fn hook_motion_change_inherit_frame_keep_rate(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: smash::phx::Hash40,
    arg3: f32,
    arg4: f32,
    arg5: f32,
) -> u64 {
    remember_motion_hint(boma, motion.hash);
    original!()(boma, motion, arg3, arg4, arg5)
}

#[skyline::hook(
    replace = smash::app::lua_bind::MotionModule::change_motion_force_inherit_frame
)]
unsafe fn hook_motion_change_force_inherit_frame(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: smash::phx::Hash40,
    arg3: f32,
    arg4: f32,
    arg5: f32,
) -> u64 {
    remember_motion_hint(boma, motion.hash);
    original!()(boma, motion, arg3, arg4, arg5)
}

#[skyline::hook(replace = smash::app::lua_bind::MotionModule::change_motion_kind)]
unsafe fn hook_motion_change_kind(
    boma: *mut smash::app::BattleObjectModuleAccessor,
    motion: smash::phx::Hash40,
) -> u64 {
    remember_motion_hint(boma, motion.hash);
    original!()(boma, motion)
}

/// `is_excute` is the boundary immediately before an ACMD block's commands. Some scripts begin
/// with an effect at frame one and do not call `frame` or `wait` before it, so the startup and
/// pre-yield hooks alone would observe the request only after the authored call had been
/// suppressed. Run the same exact-frame injector after the native predicate returns and before
/// the script executes its command body.
#[skyline::hook(replace = smash::app::sv_animcmd::is_excute)]
unsafe fn hook_acmd_is_excute(lua_state: u64) -> bool {
    let execute = original!()(lua_state);
    if execute && !crate::slight::hitbox_viewer::is_injecting() {
        inject_before_acmd_wait(lua_state, CoroutineBoundary::Execute);
    }
    execute
}

/// Inject after an absolute frame wait so a replacement at script frame one (motion frame zero)
/// is dispatched from the ACMD coroutine itself after the scheduler reaches that exact boundary.
#[skyline::hook(replace = smash::app::sv_animcmd::frame)]
unsafe fn hook_acmd_frame(lua_state: u64, target: f32) {
    original!()(lua_state, target);
    inject_after_acmd_frame(lua_state, target);
}

/// Relative waits use the same post-yield boundary as absolute frame calls.
#[skyline::hook(replace = smash::app::sv_animcmd::wait)]
unsafe fn hook_acmd_wait(lua_state: u64, target: f32) {
    original!()(lua_state, target);
    inject_before_acmd_wait(lua_state, CoroutineBoundary::Wait);
}

/// `call_coroutine` is retained as a lifecycle hook for the pinned runtime, but it is not a live
/// dispatch point: its callback runs before the ACMD has necessarily reached a valid command
/// context, and `inject_for_lua_state` intentionally returns for this boundary.
#[skyline::hook(replace = smash::lua2cpp::L2CAgentBase_call_coroutine)]
unsafe fn hook_call_coroutine(
    agent: *mut smash::lua2cpp::L2CAgentBase,
    coroutine_index: i32,
    name: smash::phx::Hash40,
) -> smash::lib::L2CValue {
    if !agent.is_null() {
        begin_acmd_playback((*agent).agent.module_accessor);
        // Some runtime paths enter the first ACMD slice from `call_coroutine` rather than
        // `start_coroutine`. The marker is idempotent and lets the first `is_excute`/effect
        // command use the same frame-zero startup normalization in either ordering.
        remember_acmd_startup(agent);
    }
    let result = original!()(agent, coroutine_index, name);
    if !agent.is_null() {
        inject_for_acmd(&mut *agent, CoroutineBoundary::Call);
    }
    result
}

#[skyline::hook(replace = smash::lua2cpp::L2CAgentBase_start_coroutine)]
unsafe fn hook_start_coroutine(
    agent: *mut smash::lua2cpp::L2CAgentBase,
    coroutine_index: i32,
    name: smash::phx::Hash40,
    state: u64,
) -> smash::lib::L2CValue {
    if !agent.is_null() {
        begin_acmd_playback((*agent).agent.module_accessor);
        remember_acmd_startup(agent);
        // The native start call immediately executes the first ACMD slice. A post-start hook is
        // therefore already too late for script frame one; use the same initialized agent before
        // entering the body, then let the startup marker provide one exact-frame retry at the
        // first `is_excute`/effect/control boundary if the native call did not confirm.
        inject_for_acmd_at_frame(&mut *agent, CoroutineBoundary::Start, 0.0);
    }
    let result = original!()(agent, coroutine_index, name, state);
    result
}

#[skyline::hook(replace = smash::lua2cpp::L2CAgentBase_resume_coroutine)]
unsafe fn hook_resume_coroutine(
    agent: *mut smash::lua2cpp::L2CAgentBase,
    coroutine_index: i32,
    state: u64,
) -> smash::lib::L2CValue {
    let result = original!()(agent, coroutine_index, state);
    if !agent.is_null() {
        inject_for_acmd(&mut *agent, CoroutineBoundary::Resume);
    }
    result
}

/// Mark the persistent Lua state used by the game's common effect script bank. Fighter-specific
/// effect agents are distinct objects and intentionally do not enter this set.
#[skyline::hook(replace = smash::lua2cpp::L2CFighterAnimcmdEffectCommon_L2CFighterAnimcmdEffectCommon)]
unsafe fn hook_common_effect_agent_ctor(
    this: *mut smash::lua2cpp::L2CFighterAnimcmdEffectCommon,
    battle_object: *mut smash::app::BattleObject,
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    lua_state: *mut smash::lua_State,
) {
    original!()(this, battle_object, module_accessor, lua_state);
    if !this.is_null() {
        remember_common_effect_lua_state((*this).agent.lua_state_agent);
    }
}

pub fn install() {
    install_effect_detach_kind_import_hook();
    skyline::install_hooks!(
        hook_motion_change,
        hook_motion_change_inherit_frame,
        hook_motion_change_inherit_frame_keep_rate,
        hook_motion_change_force_inherit_frame,
        hook_motion_change_kind,
        hook_acmd_is_excute,
        hook_acmd_frame,
        hook_acmd_wait,
        hook_call_coroutine,
        hook_start_coroutine,
        hook_resume_coroutine,
        hook_common_effect_agent_ctor,
        hook_eff,
        hook_eff_alpha,
        hook_eff_attr,
        hook_eff_follow,
        hook_eff_follow_alpha,
        hook_eff_follow_color,
        hook_eff_follow_no_scale,
        hook_eff_follow_no_stop,
        hook_eff_follow_no_stop_flip,
        hook_eff_flw_pos,
        hook_eff_flw_pos_no_stop,
        hook_eff_flw_pos_unsync_vis,
        hook_eff_flw_unsync_vis,
        hook_eff_flip,
        hook_eff_flip_alpha,
        hook_eff_follow_flip,
        hook_eff_follow_flip_alpha,
        hook_eff_follow_flip_color,
        hook_eff_follow_flip_rnd,
        hook_foot_eff,
        hook_foot_eff_flip,
        hook_landing_eff,
        hook_landing_eff_flip,
        hook_down_eff,
        hook_after_image4_on_arg29,
        hook_after_image4_on_work_arg29,
        hook_raw_effect,
        hook_after_image_off,
        hook_effect_off_kind,
        hook_effect_detach_kind_work,
        hook_enable_area,
        hook_unable_area,
        hook_last_effect_set_rate,
        hook_last_effect_set_work_int,
        hook_last_effect_set_offset_to_camera_flat,
        hook_last_effect_set_color,
        hook_last_particle_set_color,
        hook_last_effect_set_alpha,
        hook_last_effect_set_scale_w,
        hook_flash,
        hook_flash_frm,
        hook_burn_color,
        hook_burn_color_frame,
        hook_burn_color_normal,
        hook_start_info_flash_eye,
        hook_col_normal,
    );
    skyline::println!(
        "[SLight] ACMD effect hooks installed (40 spawn/stop/colour/control variants)"
    );
    crate::slight::diag::note("ACMD hooks installed");
}

#[cfg(test)]
mod injection_latch_tests {
    use super::*;

    const RULE_FINGERPRINT: u64 = 0xfeed_beef;

    fn attempt(
        latches: &mut std::collections::HashMap<(u32, usize), InjectionLatch>,
        key: (u32, usize),
        motion: u64,
        target: f32,
        frame: f32,
        confirmed: bool,
    ) -> u8 {
        store_latch(
            latches,
            key,
            motion,
            target,
            key.1,
            RULE_FINGERPRINT,
            frame,
            confirmed,
            false,
        )
    }

    fn can_attempt(
        latch: Option<&InjectionLatch>,
        motion: u64,
        target: f32,
        rule_identity: usize,
        frame: f32,
    ) -> bool {
        latch_can_attempt(
            latch,
            motion,
            target,
            rule_identity,
            RULE_FINGERPRINT,
            frame,
        )
    }

    #[test]
    fn second_coroutine_resume_can_confirm_and_then_latch_prevents_duplicates() {
        let key = (7, 3);
        let motion = 0x1234;
        let target = 0.0;
        let mut latches = std::collections::HashMap::new();

        assert!(can_attempt(None, motion, target, key.1, target));
        assert_eq!(attempt(&mut latches, key, motion, target, target, false), 1);
        assert!(can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            target
        ));
        assert_eq!(attempt(&mut latches, key, motion, target, target, true), 2);
        assert!(latches.get(&key).is_some_and(|latch| latch.confirmed));
        assert!(!can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            target
        ));
    }

    #[test]
    fn unconfirmed_frame_zero_is_missed_without_a_late_retry() {
        let key = (8, 4);
        let motion = 0x5678;
        let target = 0.0;
        let mut latches = std::collections::HashMap::new();

        assert_eq!(attempt(&mut latches, key, motion, target, target, false), 1);
        assert!(can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            target
        ));
        assert_eq!(attempt(&mut latches, key, motion, target, target, false), 2);
        assert!(!can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            target
        ));

        assert!(mark_latch_missed(
            &mut latches,
            key,
            motion,
            target,
            RULE_FINGERPRINT,
            target + MOTION_FRAME_WIDTH,
        ));
        let latch = latches
            .get(&key)
            .expect("missed request remains observable");
        assert!(latch.missed && !latch.confirmed);
        assert_eq!(latch.attempts, 2);
        assert!(!can_attempt(latches.get(&key), motion, target, key.1, 3.0));
    }

    #[test]
    fn motion_target_and_rule_changes_clear_the_old_identity() {
        let key = (9, 5);
        let mut latches = std::collections::HashMap::new();
        assert_eq!(attempt(&mut latches, key, 0x1111, 2.0, 2.0, true), 1);

        let changed_target = observed_latch(&mut latches, key, 0x1111, 3.0, RULE_FINGERPRINT, 3.0);
        assert!(changed_target.is_none());
        assert!(can_attempt(latches.get(&key), 0x1111, 3.0, key.1, 3.0));

        let changed_motion = observed_latch(&mut latches, key, 0x2222, 3.0, RULE_FINGERPRINT, 3.0);
        assert!(changed_motion.is_none());
        assert!(can_attempt(latches.get(&key), 0x2222, 3.0, key.1, 3.0));

        let changed_rule = observed_latch(
            &mut latches,
            key,
            0x2222,
            3.0,
            RULE_FINGERPRINT.wrapping_add(1),
            3.0,
        );
        assert!(changed_rule.is_none());

        // A backwards frame is a loop/new playback, even when the motion hash and rule payload
        // are unchanged.
        assert_eq!(attempt(&mut latches, key, 0x2222, 3.0, 3.0, true), 1);
        assert!(observed_latch(&mut latches, key, 0x2222, 3.0, RULE_FINGERPRINT, 0.0,).is_none());
        assert!(can_attempt(latches.get(&key), 0x2222, 3.0, key.1, 0.0));
    }

    #[test]
    fn changed_rule_fingerprint_can_retry_after_confirmation() {
        let key = (10, 6);
        let motion = 0x3333;
        let target = 0.0;
        let mut latches = std::collections::HashMap::new();

        assert_eq!(attempt(&mut latches, key, motion, target, target, true), 1);
        assert!(!latch_can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            RULE_FINGERPRINT,
            target
        ));
        assert!(latch_can_attempt(
            latches.get(&key),
            motion,
            target,
            key.1,
            RULE_FINGERPRINT.wrapping_add(1),
            target
        ));
    }

    #[test]
    fn exact_motion_frame_window_is_not_shifted_for_low_targets() {
        assert!(in_requested_motion_frame(0.0, 0.0));
        assert!(in_requested_motion_frame(1.0, 1.0));
        assert!(in_requested_motion_frame(2.0, 2.0));
        assert!(!in_requested_motion_frame(0.5, 0.0));
        assert!(!in_requested_motion_frame(2.5, 2.0));
    }

    #[test]
    fn startup_boundary_exposes_frame_zero_without_shifting_resume_frames() {
        assert_eq!(injection_frame(CoroutineBoundary::Start, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Call, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Execute, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Effect, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Control, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Frame, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Wait, -1.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Start, 0.0), 0.0);
        assert_eq!(injection_frame(CoroutineBoundary::Start, 1.0), 1.0);
        assert_eq!(injection_frame(CoroutineBoundary::Resume, -1.0), -1.0);
        assert_eq!(injection_frame(CoroutineBoundary::Resume, 0.0), 0.0);
    }

    #[test]
    fn script_frame_targets_keep_the_zero_based_motion_conversion() {
        assert_eq!(script_frame_to_motion_frame(1.0), Some(0.0));
        assert_eq!(script_frame_to_motion_frame(2.0), Some(1.0));
        assert_eq!(script_frame_to_motion_frame(3.0), Some(2.0));
        assert_eq!(script_frame_to_motion_frame(0.0), Some(0.0));
        assert_eq!(script_frame_to_motion_frame(f32::NAN), None);
    }

    #[test]
    fn startup_motion_hint_wins_only_for_a_rule_bearing_new_motion() {
        let hint = Some(MotionHint { motion: 0x2222 });
        assert_eq!(
            choose_motion_hint(0x1111, hint, CoroutineBoundary::Execute, 0.0, true,),
            0x2222
        );
        assert_eq!(
            choose_motion_hint(0x1111, hint, CoroutineBoundary::Effect, 0.0, true,),
            0x2222
        );
        assert_eq!(
            choose_motion_hint(0x1111, hint, CoroutineBoundary::Execute, 0.0, false,),
            0x1111
        );
    }

    #[test]
    fn stale_motion_hint_is_not_used_after_the_low_startup_window() {
        assert_eq!(
            choose_motion_hint(
                0x1111,
                Some(MotionHint { motion: 0x2222 }),
                CoroutineBoundary::Resume,
                2.0,
                true,
            ),
            0x1111
        );
        assert_eq!(
            choose_motion_hint(
                0x1111,
                Some(MotionHint { motion: 0x2222 }),
                CoroutineBoundary::Resume,
                1.0,
                true,
            ),
            0x2222
        );
    }
}
