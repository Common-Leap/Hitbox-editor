//! Per-agent last-frame effect snapshots — Jorge last_frame_data facade.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::slight::effect_viewer::effect_data::EffectData;

static SNAPSHOTS: LazyLock<Mutex<HashMap<u32, HashMap<u64, EffectData>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn install() {
    skyline::println!("[SLight] Last frame data ready");
}

pub fn on_post_frame() {
    let tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
    let mut snap = SNAPSHOTS.lock();
    snap.clear();
    for effect in tracker.iter() {
        snap.entry(effect.boid)
            .or_default()
            .insert(effect.id, effect.data.clone());
    }
}

pub fn get(boid: u32, effect_id: u64) -> Option<EffectData> {
    SNAPSHOTS
        .lock()
        .get(&boid)
        .and_then(|m| m.get(&effect_id))
        .cloned()
}

pub fn for_boid(boid: u32) -> Vec<(u64, EffectData)> {
    SNAPSHOTS
        .lock()
        .get(&boid)
        .map(|m| m.iter().map(|(k, v)| (*k, v.clone())).collect())
        .unwrap_or_default()
}

pub fn clear() {
    SNAPSHOTS.lock().clear();
}
