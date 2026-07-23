//! Per-agent dynamic allocation slots — Jorge dynamic_memory facade.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

static SLOTS: LazyLock<Mutex<HashMap<u32, AgentSlots>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, Default)]
struct AgentSlots {
    effect_slots: Vec<Option<u64>>,
    initialized: bool,
}

const DEFAULT_SLOTS: usize = 128;

pub fn install() {
    let mut map = SLOTS.lock();
    for boid in 0..8u32 {
        map.entry(boid).or_default().effect_slots = vec![None; DEFAULT_SLOTS];
    }
}

pub fn ensure_agent(boid: u32) {
    let mut map = SLOTS.lock();
    let slots = map.entry(boid).or_default();
    if slots.effect_slots.is_empty() {
        slots.effect_slots = vec![None; DEFAULT_SLOTS];
    }
    // Must set `initialized` even when the entry already exists: install() pre-creates boids 0..8
    // with `initialized=false`, so `or_insert_with` would never flip it for the player fighters.
    slots.initialized = true;
}

pub fn has_agent(boid: u32) -> bool {
    SLOTS
        .lock()
        .get(&boid)
        .map(|s| s.initialized)
        .unwrap_or(false)
}

pub fn bind_effect(boid: u32, handle: u32, effect_id: u64) {
    ensure_agent(boid);
    let idx = (handle as usize).min(DEFAULT_SLOTS.saturating_sub(1));
    if let Some(slots) = SLOTS.lock().get_mut(&boid) {
        if idx < slots.effect_slots.len() {
            slots.effect_slots[idx] = Some(effect_id);
        }
    }
}

pub fn lookup_effect(boid: u32, handle: u32) -> Option<u64> {
    let idx = handle as usize;
    SLOTS
        .lock()
        .get(&boid)?
        .effect_slots
        .get(idx)
        .copied()
        .flatten()
}

pub fn clear() {
    SLOTS.lock().clear();
}
