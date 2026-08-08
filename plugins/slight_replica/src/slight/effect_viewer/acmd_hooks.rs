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
static PENDING_TINT: [PendingValue; 3] =
    [PendingValue::new(), PendingValue::new(), PendingValue::new()];
static PENDING_PARTICLE_TINT: [PendingValue; 3] =
    [PendingValue::new(), PendingValue::new(), PendingValue::new()];

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

/// Match a point-control rule against the primitive's authored arguments before the game
/// consumes them. The capture happens first, so a suppressed authored call still teaches the
/// editor what the pristine script did.
unsafe fn control_suppressed(
    lua_state: u64,
    func: &str,
    args: &[crate::slight::hitbox_viewer::LuaArg],
) -> bool {
    if !crate::slight::effect_viewer::control_rules::any_for(func) {
        return false;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return false;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    crate::slight::effect_viewer::control_rules::suppressed(func, args, motion, frame)
}

/// `EFFECT_DETACH_KIND` is a point action, not the kill-kind path used by `EFFECT_OFF_KIND`.
#[skyline::hook(replace = smash::app::sv_animcmd::EFFECT_DETACH_KIND)]
unsafe fn hook_effect_detach_kind(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 2);
    crate::slight::hitbox_viewer::record(lua_state, "EFFECT_DETACH_KIND", &typed);
    if control_suppressed(lua_state, "EFFECT_DETACH_KIND", &typed) {
        return;
    }
    original!()(lua_state);
}

/// The wrapper resolves `EFFECT_DETACH_KIND_WORK`'s WorkModule slot before reaching this
/// primitive, so captures contain the runtime handle, not the authored Work ID. That runtime
/// value is still exact for suppression/retime of the captured call; source parsing retains the
/// authored token separately.
#[skyline::hook(replace = smash::app::sv_animcmd::EFFECT_DETACH_KIND_WORK)]
unsafe fn hook_effect_detach_kind_work(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 2);
    crate::slight::hitbox_viewer::record(lua_state, "EFFECT_DETACH_KIND_WORK", &typed);
    if control_suppressed(lua_state, "EFFECT_DETACH_KIND_WORK", &typed) {
        return;
    }
    original!()(lua_state);
}

#[skyline::hook(replace = smash::app::sv_animcmd::ENABLE_AREA)]
unsafe fn hook_enable_area(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "ENABLE_AREA", &typed);
    if control_suppressed(lua_state, "ENABLE_AREA", &typed) {
        return;
    }
    original!()(lua_state);
}

#[skyline::hook(replace = smash::app::sv_animcmd::UNABLE_AREA)]
unsafe fn hook_unable_area(lua_state: u64) {
    let typed = crate::slight::hitbox_viewer::read_args_typed(lua_state, 1);
    crate::slight::hitbox_viewer::record(lua_state, "UNABLE_AREA", &typed);
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
    record_spawn(args.eff_hash, h_before, h_after);
    // Before the script's own modifier lines run, so a spawn the script never retunes still
    // gets the editor's values. When there IS such a line, it runs after this and would win —
    // which is why each `hook_last_effect_set_*` rewrites its arguments as well.
    apply_pending_modifiers(boma, h_before, h_after);
    apply_pending_camera_offset(lua_state);
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
                // Eff-editor spawn rules: suppression + PER-SPAWN transform, both scoped to
                // (motion, frame window) so editing one spawn doesn't affect the others.
                let mut scoped_transform = false;
                if crate::slight::effect_viewer::spawn_rules::any_for(args.eff_hash) {
                    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                        as *mut smash::app::BattleObjectModuleAccessor;
                    let (motion, frame) = if boma.is_null() {
                        (0u64, -1.0f32)
                    } else {
                        (
                            smash::app::lua_bind::MotionModule::motion_kind(boma),
                            smash::app::lua_bind::MotionModule::frame(boma),
                        )
                    };
                    // A suppressed spawn skips original entirely — the effect never exists
                    // (unlike a visible=false pin, which spawns it).
                    if crate::slight::effect_viewer::spawn_rules::suppressed(
                        args.eff_hash,
                        motion,
                        frame,
                    ) {
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
            crate::slight::hitbox_viewer::record(lua_state, $command, &typed);

            let cmd_hash = smash::hash40($lower);
            if crate::slight::effect_viewer::spawn_rules::any_for(cmd_hash) {
                let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
                    as *mut smash::app::BattleObjectModuleAccessor;
                let (motion, frame) = if boma.is_null() {
                    (0u64, -1.0f32)
                } else {
                    (
                        smash::app::lua_bind::MotionModule::motion_kind(boma),
                        smash::app::lua_bind::MotionModule::frame(boma),
                    )
                };
                // A disabled colour command skips original entirely, so the tint never
                // happens — the same meaning suppression has for a spawn.
                if crate::slight::effect_viewer::spawn_rules::suppressed(cmd_hash, motion, frame) {
                    return;
                }
                if let Some((color, transition)) =
                    crate::slight::effect_viewer::spawn_rules::color_for(cmd_hash, motion, frame)
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

            original!()(lua_state);
        }
    };
}

color_hook!(hook_flash, smash::app::sv_animcmd::FLASH, "FLASH", "flash", 0, 4);
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

    let mut agent = smash::lib::L2CAgent::new(lua_state);
    let logical_hash = arg_hash(&mut agent, 1);
    let fade = arg_bool(&mut agent, 2, false);
    let detach = arg_bool(&mut agent, 3, true);

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

    if let Some(logical_hash) = logical_hash {
        let source = smash::app::sv_system::battle_object_module_accessor(lua_state)
            as *mut smash::app::BattleObjectModuleAccessor;
        kill_carrier_follow_kind(source, logical_hash, fade, detach);
    }
}

// ── Live retime injection (per-frame, from the agent line callback) ──────────

/// (boid, rule idx) → (motion, frame) it last fired at. Refires when the motion loops
/// (frame goes backwards) or the motion changes — mirrors the hitbox inject latch.
static EFF_FIRED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u32, usize), (u64, f32)>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

static CONTROL_FIRED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u32, usize), (u64, f32)>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Dispatch a captured EFFECT spawn to the matching sv_animcmd function by its short name.
unsafe fn dispatch_effect(func: &str, lua_state_agent: u64) {
    use smash::app::sv_animcmd as sv;
    match func {
        "EFFECT_OFF_KIND" => sv::EFFECT_OFF_KIND(lua_state_agent),
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
}

unsafe fn dispatch_control(func: &str, lua_state_agent: u64) {
    use smash::app::sv_animcmd as sv;
    match func {
        "EFFECT_DETACH_KIND" => sv::EFFECT_DETACH_KIND(lua_state_agent),
        "EFFECT_DETACH_KIND_WORK" => sv::EFFECT_DETACH_KIND_WORK(lua_state_agent),
        "ENABLE_AREA" => sv::ENABLE_AREA(lua_state_agent),
        "UNABLE_AREA" => sv::UNABLE_AREA(lua_state_agent),
        _ => {}
    }
}

/// Fire due C4 point-control injections once per motion playback. Work-slot controls replay the
/// captured runtime handle, which is exact for the live call even though the source editor keeps
/// the authored Work ID token separately.
unsafe fn inject_control_tick(lua_state: u64) {
    use crate::slight::effect_viewer::control_rules;
    if !control_rules::any_inject() {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let boid = (*boma).battle_object_id;
    for (idx, injection) in control_rules::injections_for(motion) {
        let key = (boid, idx);
        let due = frame >= injection.frame;
        let already = CONTROL_FIRED
            .lock()
            .get(&key)
            .map(|(m, f)| *m == motion && frame >= *f)
            .unwrap_or(false);
        if due && !already {
            let mut agent = smash::lib::L2CAgent::new(lua_state);
            agent.clear_lua_stack();
            for arg in &injection.args {
                let mut value = arg.to_l2c();
                agent.push_lua_stack(&mut value);
            }
            {
                let _guard = crate::slight::hitbox_viewer::InjectGuard::new();
                dispatch_control(&injection.func, agent.lua_state_agent);
            }
            agent.clear_lua_stack();
            CONTROL_FIRED.lock().insert(key, (motion, frame));
            crate::slight::diag::note(format!(
                "injected effect control '{}' (motion {motion:#x} frame {frame:.1})",
                injection.func
            ));
        }
        if !due {
            let mut fired = CONTROL_FIRED.lock();
            if let Some((m, f)) = fired.get(&key).copied() {
                if m != motion || frame < f {
                    fired.remove(&key);
                }
            }
        }
    }
}

/// Fire due effect-retime injections for this agent's current motion (once per playback).
pub unsafe fn inject_tick(lua_state: u64) {
    inject_control_tick(lua_state);
    use crate::slight::effect_viewer::spawn_rules;
    if !spawn_rules::any_inject() {
        return;
    }
    let boma = smash::app::sv_system::battle_object_module_accessor(lua_state)
        as *mut smash::app::BattleObjectModuleAccessor;
    if boma.is_null() {
        return;
    }
    let motion = smash::app::lua_bind::MotionModule::motion_kind(boma);
    let injections = spawn_rules::injections_for(motion);
    if injections.is_empty() {
        return;
    }
    let frame = smash::app::lua_bind::MotionModule::frame(boma);
    let boid = (*boma).battle_object_id;

    for (idx, inj) in injections {
        let key = (boid, idx);
        let due = frame >= inj.frame;
        let already = {
            let fired = EFF_FIRED.lock();
            fired
                .get(&key)
                .map(|(m, f)| *m == motion && frame >= *f)
                .unwrap_or(false)
        };
        if due && !already {
            let mut agent = smash::lib::L2CAgent::new(lua_state);
            agent.clear_lua_stack();
            for a in &inj.args {
                let mut v = a.to_l2c();
                agent.push_lua_stack(&mut v);
            }
            {
                // Replays re-enter our own EFFECT hooks — keep them out of the pristine
                // capture, or the editor sees the user's retime as an original spawn.
                let _g = crate::slight::hitbox_viewer::InjectGuard::new();
                dispatch_effect(&inj.func, agent.lua_state_agent);
            }
            agent.clear_lua_stack();
            EFF_FIRED.lock().insert(key, (motion, frame));
            crate::slight::diag::note(format!(
                "injected effect '{}' (motion {motion:#x} frame {frame:.1})",
                inj.func
            ));
        }
        if !due {
            let mut fired = EFF_FIRED.lock();
            if let Some((m, f)) = fired.get(&key).copied() {
                if m != motion || frame < f {
                    fired.remove(&key);
                }
            }
        }
    }
}

pub fn install() {
    skyline::install_hooks!(
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
        hook_effect_off_kind,
        hook_effect_detach_kind,
        hook_effect_detach_kind_work,
        hook_enable_area,
        hook_unable_area,
        hook_last_effect_set_rate,
        hook_last_effect_set_work_int,
        hook_last_effect_set_offset_to_camera_flat,
        hook_last_effect_set_color,
        hook_last_particle_set_color,
        hook_last_effect_set_alpha,
        hook_flash,
        hook_flash_frm,
        hook_burn_color,
        hook_burn_color_frame,
        hook_burn_color_normal,
        hook_start_info_flash_eye,
        hook_col_normal,
    );
    skyline::println!(
        "[SLight] ACMD effect hooks installed (39 spawn/stop/colour/control variants)"
    );
    crate::slight::diag::note("ACMD hooks installed");
}
