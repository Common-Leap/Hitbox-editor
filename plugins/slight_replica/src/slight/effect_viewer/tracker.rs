//! Per-agent effect tracker — maps to FUN_71000a256c / FUN_710009d26c reconcile.

use parking_lot::Mutex;
use smash::app::lua_bind::EffectModule;
use std::collections::HashMap;
use std::sync::LazyLock;

use super::effect_data::{hash_label, EffectData, Point3D};

pub const MAX_EFFECT_INDEX: u32 = 624;

/// Lifetime (frames) granted to a synthetic (handle-less) effect; refreshed when the same
/// (owner, effect) spawns again. Fire-and-forget vfx are short — 2s keeps the entry visible
/// in RPM while it plausibly lives, then auto-expires (there is no handle to is_exist on).
pub const SYNTHETIC_TTL: u16 = 120;

/// Frames an effect's owning accessor may be absent from the live-agent map before the
/// effect is dropped. Absorbs one-frame registry flaps around agent init/reinit.
const DEAD_ACCESSOR_GRACE: u8 = 10;

pub struct TrackedEffect {
    pub id: u64,
    pub handle: u32,
    pub effect_hash: u64,
    pub bone_hash: u64,
    pub module_accessor_addr: u64,
    pub boid: u32,
    pub fighter_kind: i32,
    pub status_kind: i32,
    pub category: i32,
    pub entry_id: i32,
    pub founder_entry_id: Option<i32>,
    pub stale: bool,
    pub rpm_notified: bool,
    /// Spawn returned no handle (fire-and-forget) — tracked under a pseudo-handle for
    /// display; cannot be targeted by EffectModule setters (read-only in RPM).
    pub synthetic: bool,
    /// Frames until auto-expiry (synthetic effects only).
    pub ttl: u16,
    /// Consecutive reconcile passes with the owning accessor missing from the live map.
    pub missing_frames: u8,
    pub data: EffectData,
}

pub struct EffectTracker {
    next_id: u64,
    effects: Vec<TrackedEffect>,
}

impl Default for EffectTracker {
    fn default() -> Self {
        Self {
            next_id: 1,
            effects: Vec::with_capacity(256),
        }
    }
}

impl EffectTracker {
    pub fn upsert_spawn(
        &mut self,
        module_accessor_addr: u64,
        handle: u32,
        effect_hash: u64,
        bone_hash: u64,
        is_follow: bool,
        pos: Point3D,
        rot: Point3D,
        scale: f32,
        boid: u32,
        fighter_kind: i32,
        status_kind: i32,
        category: i32,
        entry_id: i32,
        founder_entry_id: Option<i32>,
        synthetic: bool,
    ) -> (u64, bool, bool) {
        // Synthetic (handle-less) effects dedup on (owner, effect kind): each re-spawn of the
        // same fire-and-forget vfx refreshes the one RPM entry instead of flooding the tracker.
        // Real effects dedup on (owner, handle) — the game's identity for a live effect.
        let existing = if synthetic {
            self.effects.iter_mut().find(|e| {
                e.synthetic
                    && e.module_accessor_addr == module_accessor_addr
                    && e.effect_hash == effect_hash
            })
        } else {
            self.effects.iter_mut().find(|e| {
                !e.synthetic && e.module_accessor_addr == module_accessor_addr && e.handle == handle
            })
        };

        if let Some(existing) = existing {
            // A reused (owner, handle) slot now holding a DIFFERENT effect is a new spawn in
            // the game's eyes — re-notify RPM (the old content was hidden or replaced).
            let reshow = !synthetic && existing.effect_hash != effect_hash;
            existing.effect_hash = effect_hash;
            existing.bone_hash = bone_hash;
            existing.data.effect_name = hash_label(effect_hash);
            existing.data.bone_name = hash_label(bone_hash);
            existing.data.is_follow = is_follow;
            existing.data.scale = scale;
            existing.data.pos = pos;
            existing.data.rot = rot;
            existing.stale = false;
            existing.ttl = SYNTHETIC_TTL;
            existing.missing_frames = 0;
            if reshow {
                existing.rpm_notified = false;
            }
            return (existing.id, reshow, reshow);
        }

        let id = self.next_id;
        self.next_id += 1;
        let mut data = EffectData::default();
        data.index = handle;
        data.effect_name = hash_label(effect_hash);
        data.bone_name = hash_label(bone_hash);
        data.is_follow = is_follow;
        data.scale = scale;
        data.pos = pos;
        data.rot = rot;

        self.effects.push(TrackedEffect {
            id,
            handle,
            effect_hash,
            bone_hash,
            module_accessor_addr,
            boid,
            fighter_kind,
            status_kind,
            category,
            entry_id,
            founder_entry_id,
            stale: false,
            rpm_notified: false,
            synthetic,
            ttl: SYNTHETIC_TTL,
            missing_frames: 0,
            data,
        });
        (id, true, false)
    }

    /// Reconcile EVERY tracked effect in a single pass (Jorge FUN_710009d26c). For each effect
    /// whose owning agent is still live, `EffectModule::is_exist_effect` decides whether it's
    /// gone; gone effects are dropped and returned so the caller can hide them in RPM.
    ///
    /// Effects whose accessor is ABSENT from the live map are dropped too (after a short grace
    /// for registry flaps) — the owning object died, so the effect cannot be probed or edited.
    /// Previously these were retained forever, which made the tracker grow without bound and
    /// dragged the per-frame cost (this pass + rainbow ticks) down with every spawn.
    ///
    /// Synthetic (handle-less) effects can't be probed via is_exist_effect; they expire on TTL.
    pub fn reconcile_all(
        &mut self,
        live: &HashMap<u64, *mut smash::app::BattleObjectModuleAccessor>,
    ) -> Vec<(u64, bool)> {
        let mut removed = Vec::new();
        let (mut gone, mut dead, mut expired) = (0u64, 0u64, 0u64);
        self.effects.retain_mut(|e| {
            if e.synthetic {
                if e.ttl == 0 {
                    expired += 1;
                    removed.push((e.id, e.rpm_notified));
                    return false;
                }
                e.ttl -= 1;
                return true;
            }
            if let Some(&accessor) = live.get(&e.module_accessor_addr) {
                e.missing_frames = 0;
                let exist = unsafe { EffectModule::is_exist_effect(accessor, e.handle) };
                if !exist {
                    gone += 1;
                    removed.push((e.id, e.rpm_notified));
                    return false;
                }
                true
            } else if e.missing_frames >= DEAD_ACCESSOR_GRACE {
                dead += 1;
                removed.push((e.id, e.rpm_notified));
                false
            } else {
                e.missing_frames += 1;
                true
            }
        });
        crate::slight::diag::note_reconcile(gone, dead, expired);
        removed
    }

    pub fn remove_by_handle(
        &mut self,
        module_accessor_addr: u64,
        handle: u32,
    ) -> Option<(u64, bool)> {
        let idx = self
            .effects
            .iter()
            .position(|e| e.module_accessor_addr == module_accessor_addr && e.handle == handle)?;
        let e = self.effects.remove(idx);
        Some((e.id, e.rpm_notified))
    }

    pub fn remove_by_hash(&mut self, module_accessor_addr: u64, hash: u64) -> Vec<(u64, bool)> {
        let removed: Vec<_> = self
            .effects
            .iter()
            .filter(|e| e.module_accessor_addr == module_accessor_addr && e.effect_hash == hash)
            .map(|e| (e.id, e.rpm_notified))
            .collect();
        self.effects
            .retain(|e| !(e.module_accessor_addr == module_accessor_addr && e.effect_hash == hash));
        removed
    }

    pub fn remove_all_module(&mut self, module_accessor_addr: u64) -> Vec<(u64, bool)> {
        let removed: Vec<_> = self
            .effects
            .iter()
            .filter(|e| e.module_accessor_addr == module_accessor_addr)
            .map(|e| (e.id, e.rpm_notified))
            .collect();
        self.effects
            .retain(|e| e.module_accessor_addr != module_accessor_addr);
        removed
    }

    pub fn get(&self, id: u64) -> Option<&TrackedEffect> {
        self.effects.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut TrackedEffect> {
        self.effects.iter_mut().find(|e| e.id == id)
    }

    pub fn mark_notified(&mut self, id: u64) {
        if let Some(e) = self.get_mut(id) {
            e.rpm_notified = true;
        }
    }

    pub fn count(&self) -> usize {
        self.effects.len()
    }

    pub fn clear(&mut self) {
        self.effects.clear();
    }

    pub fn invalidate_boid(&mut self, boid: u32) {
        for effect in &mut self.effects {
            if effect.boid == boid {
                effect.stale = true;
            }
        }
        self.effects.retain(|e| e.boid != boid || !e.stale);
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrackedEffect> {
        self.effects.iter()
    }
}

pub static EFFECT_TRACKER: LazyLock<Mutex<EffectTracker>> =
    LazyLock::new(|| Mutex::new(EffectTracker::default()));
