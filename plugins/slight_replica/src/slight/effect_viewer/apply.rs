//! Apply EffectData to live game handles — FUN_71000ccd40.

use smash::app::lua_bind::EffectModule;
use smash::phx::Vector3f;

use super::effect_data::EffectData;

pub fn apply_to_game(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    handle: u32,
    data: &EffectData,
    boid: u32,
    fighter_label: &str,
) {
    // Jorge FUN_71000ccd40 sets each property DIRECTLY from EffectData (no multiplier — the
    // Multipliers facade is a combat knockback system, not an effect scaler). Order matches
    // the decomp: scale, rate, frame, visible, alpha, rgb, pos, rot.
    //
    // Follow (bone-attached) effects — most character effects — take everything EXCEPT
    // pos/rot: their transform is driven by the bone every frame, so absolute set_pos/set_rot
    // either fights the follow or detaches it. The old blanket `is_follow → return` made
    // every character effect uneditable.
    let _ = (boid, fighter_label);
    unsafe {
        let scale_v = Vector3f {
            x: data.scale,
            y: data.scale,
            z: data.scale,
        };
        EffectModule::set_scale(module_accessor, handle, &scale_v);
        EffectModule::set_rate(module_accessor, handle, data.rate);
        EffectModule::set_frame(module_accessor, handle, data.frame);
        EffectModule::set_visible(module_accessor, handle, data.visible);

        let c = &data.rainbow.color;
        EffectModule::set_alpha(module_accessor, handle, c.alpha);
        EffectModule::set_rgb(module_accessor, handle, c.red, c.green, c.blue);

        if !data.is_follow {
            let pos = Vector3f {
                x: data.pos.x,
                y: data.pos.y,
                z: data.pos.z,
            };
            EffectModule::set_pos(module_accessor, handle, &pos);

            let rot = Vector3f {
                x: data.rot.x,
                y: data.rot.y,
                z: data.rot.z,
            };
            EffectModule::set_rot(module_accessor, handle, &rot);
        }
    }
}

pub struct ParsedEdit {
    pub id: u64,
    pub scale: Option<f32>,
    pub rate: Option<f32>,
    pub pos: Option<super::effect_data::Point3D>,
    pub rot: Option<super::effect_data::Point3D>,
    pub visible: Option<bool>,
    pub is_follow: Option<bool>,
    pub frame: Option<f32>,
    pub color: Option<super::effect_data::Color>,
    pub movement_state: Option<f32>,
}

impl Default for ParsedEdit {
    fn default() -> Self {
        Self {
            id: 0,
            scale: None,
            rate: None,
            pos: None,
            rot: None,
            visible: None,
            is_follow: None,
            frame: None,
            color: None,
            movement_state: None,
        }
    }
}

impl ParsedEdit {
    pub fn apply_to(&self, data: &mut EffectData) {
        if let Some(v) = self.scale {
            data.scale = v;
        }
        if let Some(v) = self.rate {
            data.rate = v;
        }
        if let Some(v) = &self.pos {
            data.pos = v.clone();
        }
        if let Some(v) = &self.rot {
            data.rot = v.clone();
        }
        if let Some(v) = self.visible {
            data.visible = v;
        }
        if let Some(v) = self.is_follow {
            data.is_follow = v;
        }
        if let Some(v) = self.frame {
            data.frame = v;
        }
        if let Some(c) = &self.color {
            data.rainbow.color = c.clone();
        }
        if let Some(v) = self.movement_state {
            data.rainbow.movement_state = v;
        }
    }
}

/// RPM edit against a KIND tab (edit.id == eff_hash). Pins the fields that changed (enforced
/// every frame until edited again), refreshes the tab, and applies immediately to all live
/// instances of the kind.
pub fn apply_kind_edit(edit: &ParsedEdit) -> bool {
    let Some(pins) = super::kinds::apply_edit(edit) else {
        crate::slight::diag::note_edit(edit.id, false, false, false);
        return false;
    };
    let applied = apply_pinned_to_instances(edit.id, &pins);
    crate::slight::diag::note_edit(edit.id, true, applied > 0, true);
    super::show::queue_show(edit.id);
    true
}

/// Apply ONLY the pinned fields to one live effect handle. Never touches unpinned fields —
/// the effect keeps its native color/frame/etc. unless the user actually edited them.
pub fn apply_pinned(
    accessor: *mut smash::app::BattleObjectModuleAccessor,
    handle: u32,
    pins: &super::kinds::Pinned,
    is_follow: bool,
) {
    unsafe {
        if let Some(s) = pins.scale {
            let v = Vector3f { x: s, y: s, z: s };
            EffectModule::set_scale(accessor, handle, &v);
        }
        if let Some(r) = pins.rate {
            EffectModule::set_rate(accessor, handle, r);
        }
        if let Some(f) = pins.frame {
            EffectModule::set_frame(accessor, handle, f);
        }
        if let Some(vis) = pins.visible {
            EffectModule::set_visible(accessor, handle, vis);
        }
        if let Some(c) = &pins.color {
            EffectModule::set_alpha(accessor, handle, c.alpha);
            EffectModule::set_rgb(accessor, handle, c.red, c.green, c.blue);
        }
        if !is_follow {
            if let Some(p) = &pins.pos {
                let v = Vector3f {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                };
                EffectModule::set_pos(accessor, handle, &v);
            }
            if let Some(r) = &pins.rot {
                let v = Vector3f {
                    x: r.x,
                    y: r.y,
                    z: r.z,
                };
                EffectModule::set_rot(accessor, handle, &v);
            }
        }
    }
}

/// Apply pins to every live (non-synthetic) instance of the kind. Returns instances touched.
pub fn apply_pinned_to_instances(eff_hash: u64, pins: &super::kinds::Pinned) -> usize {
    let instances: Vec<(u64, u32, bool)> = {
        let t = super::tracker::EFFECT_TRACKER.lock();
        t.iter()
            .filter(|e| e.effect_hash == eff_hash && !e.synthetic)
            .map(|e| (e.module_accessor_addr, e.handle, e.data.is_follow))
            .collect()
    };
    let mut applied = 0;
    for (mod_addr, handle, is_follow) in instances {
        let accessor = mod_addr as *mut smash::app::BattleObjectModuleAccessor;
        unsafe {
            if !EffectModule::is_exist_effect(accessor, handle) {
                continue;
            }
        }
        apply_pinned(accessor, handle, pins, is_follow);
        applied += 1;
    }
    applied
}

/// Per-frame enforcement: every kind with pinned fields gets those fields re-applied to all
/// live instances — the game can't drift a pinned value ("force until edited next").
pub fn enforce_pinned() {
    let mut kinds_n = 0u64;
    let mut inst_n = 0u64;
    for (eff_hash, pins) in super::kinds::pinned_kinds() {
        kinds_n += 1;
        inst_n += apply_pinned_to_instances(eff_hash, &pins) as u64;
    }
    // Sparse heartbeat so a broken enforcement is visible in diag (every ~5s while pinning).
    if kinds_n > 0 {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 300 == 0 {
            crate::slight::diag::note(format!("ENF kinds={kinds_n} instances={inst_n}"));
        }
    }
}

pub fn apply_rpm_edit(edit: &ParsedEdit) -> bool {
    let (handle, mod_addr, mut data, boid, fighter_label, synthetic);
    {
        let tracker = super::tracker::EFFECT_TRACKER.lock();
        let Some(e) = tracker.get(edit.id) else {
            crate::slight::diag::note_edit(edit.id, false, false, false);
            return false;
        };
        handle = e.handle;
        mod_addr = e.module_accessor_addr;
        data = e.data.clone();
        boid = e.boid;
        fighter_label = e.data.effect_name.clone();
        synthetic = e.synthetic;
    }

    edit.apply_to(&mut data);

    {
        let mut tracker = super::tracker::EFFECT_TRACKER.lock();
        let Some(e) = tracker.get_mut(edit.id) else {
            crate::slight::diag::note_edit(edit.id, false, false, false);
            return false;
        };
        e.data = data.clone();
    }

    // Synthetic effects have no real handle — nothing in game to target. The edit is
    // persisted to the tracker (and echoed to RPM) but can't change the live vfx.
    if synthetic {
        crate::slight::diag::note_edit(edit.id, true, false, false);
        let name = format_rpm_name(edit.id);
        super::show::show_effect(edit.id, &name, &data);
        return false;
    }

    let accessor = mod_addr as *mut smash::app::BattleObjectModuleAccessor;
    unsafe {
        if !EffectModule::is_exist_effect(accessor, handle) {
            let removed = super::tracker::EFFECT_TRACKER
                .lock()
                .remove_by_handle(mod_addr, handle);
            if let Some((_id, notified)) = removed {
                super::show::hide_effect(edit.id, notified);
            }
            crate::slight::diag::note_edit(edit.id, true, false, false);
            return false;
        }
    }

    apply_to_game(accessor, handle, &data, boid, &fighter_label);
    crate::slight::diag::note_edit(edit.id, true, true, true);

    let name = format_rpm_name(edit.id);
    super::show::show_effect(edit.id, &name, &data);
    super::tracker::EFFECT_TRACKER.lock().mark_notified(edit.id);
    true
}

fn format_rpm_name(id: u64) -> String {
    let tracker = super::tracker::EFFECT_TRACKER.lock();
    let Some(e) = tracker.get(id) else {
        return format!("Effect #{id}");
    };
    crate::slight::agents::format_effect_name(
        id,
        e.category,
        e.fighter_kind,
        e.entry_id,
        e.founder_entry_id,
        &e.data,
    )
}
