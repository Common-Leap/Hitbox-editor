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

/// Distinct effect kinds an ACMD has requested this session (hash → request count). Written
/// to `sd:/effect_viewer_spawn.txt` only when a NEW hash first appears (cheap), so after a
/// move we can read which kinds it spawned by NAME — the direct check for "did the one-slot
/// redirect fire?" (e.g. is `alucard_backdash_os` in the list after kirby's backdash?).
/// eff_hash → (request count, last resolved handle). A NON-zero handle means the game found
/// & spawned the kind; `h=0` means the kind wasn't registered (nothing to spawn) — the exact
/// split we need for a one-slot: is `alucard_backdash_os` registered after the re-read?
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

struct ParsedEffectArgs {
    eff_hash: u64,
    bone_hash: u64,
    pos: Vector3f,
    rot: Vector3f,
    size: f32,
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
    Some(ParsedEffectArgs {
        eff_hash,
        bone_hash,
        pos,
        rot,
        size,
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

/// Rewrite the requested effect kind in-place (live one-slot alias): the graphic slot(s)
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
    if REQ_VT_LOGGED.swap(true, Ordering::Relaxed) {
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
            // Read args BEFORE original consumes the lua stack. `parsed` keeps the SCRIPT'S
            // authored values (pre-rewrite) — the tracked/observed data must never be
            // contaminated by the user's pins.
            let parsed = parse_args(lua_state, $flip);
            if let Some(args) = parsed.as_ref() {
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
                        scoped_transform = true;
                    }
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
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("sd:/effect_viewer_os_req.txt")
                    {
                        let _ = writeln!(
                            f,
                            "acmd_rewrite from={:#x} to={real:#x} arg0_after={after:#x}",
                            args.eff_hash
                        );
                    }
                }
                // Live one-slot alias LAST (after rules/pins keyed on the requested
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
                        rewrite_kind(lua_state, args.eff_hash, real, $flip);
                    }
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
effect_hook!(hook_eff_follow, smash::app::sv_animcmd::EFFECT_FOLLOW, true);
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
    hook_eff_flw_pos,
    smash::app::sv_animcmd::EFFECT_FLW_POS,
    true
);
effect_hook!(
    hook_eff_flip,
    smash::app::sv_animcmd::EFFECT_FLIP,
    false,
    true
);
effect_hook!(
    hook_eff_follow_flip,
    smash::app::sv_animcmd::EFFECT_FOLLOW_FLIP,
    true,
    true
);

// ── Live retime injection (per-frame, from the agent line callback) ──────────

/// (boid, rule idx) → (motion, frame) it last fired at. Refires when the motion loops
/// (frame goes backwards) or the motion changes — mirrors the hitbox inject latch.
static EFF_FIRED: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u32, usize), (u64, f32)>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Dispatch a captured EFFECT spawn to the matching sv_animcmd function by its short name.
unsafe fn dispatch_effect(func: &str, lua_state_agent: u64) {
    use smash::app::sv_animcmd as sv;
    match func {
        "EFFECT_FOLLOW" => sv::EFFECT_FOLLOW(lua_state_agent),
        "EFFECT_FOLLOW_NO_SCALE" => sv::EFFECT_FOLLOW_NO_SCALE(lua_state_agent),
        "EFFECT_FOLLOW_NO_STOP" => sv::EFFECT_FOLLOW_NO_STOP(lua_state_agent),
        "EFFECT_FLW_POS" => sv::EFFECT_FLW_POS(lua_state_agent),
        "EFFECT_FLIP" => sv::EFFECT_FLIP(lua_state_agent),
        "EFFECT_FOLLOW_FLIP" => sv::EFFECT_FOLLOW_FLIP(lua_state_agent),
        _ => sv::EFFECT(lua_state_agent),
    }
}

/// Fire due effect-retime injections for this agent's current motion (once per playback).
pub unsafe fn inject_tick(lua_state: u64) {
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
        hook_eff_follow,
        hook_eff_follow_no_scale,
        hook_eff_follow_no_stop,
        hook_eff_flw_pos,
        hook_eff_flip,
        hook_eff_follow_flip,
    );
    skyline::println!("[SLight] ACMD EFFECT hooks installed (7 variants)");
    crate::slight::diag::note("ACMD hooks installed");
}
