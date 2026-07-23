//! Fighter magic / version tracking — Jorge gauge_system facade (MagicChangeData).

use parking_lot::Mutex;
use smash::app::sv_battle_object;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MagicChangeData {
    pub previous_magic: u32,
    pub current_magic: u32,
    pub difference: i32,
}

static MAGIC: LazyLock<Mutex<HashMap<u32, MagicChangeData>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn install() {
    skyline::println!("[SLight] Gauge system ready");
}

pub fn on_frame() {
    // Registry records carry FULL battle object ids (0..512 enumeration can't reach them).
    let recs = crate::slight::agents::all_records();
    let live: std::collections::HashSet<u32> = recs.iter().map(|r| r.boid).collect();
    MAGIC.lock().retain(|boid, _| live.contains(boid));
    for rec in recs {
        let boid = rec.boid;
        if !unsafe { sv_battle_object::is_active(boid) } {
            MAGIC.lock().remove(&boid);
            continue;
        }
        let magic = read_magic(boid);
        if magic == 0 {
            continue;
        }
        let mut map = MAGIC.lock();
        let entry = map.entry(boid).or_default();
        if entry.current_magic == 0 {
            entry.previous_magic = magic;
            entry.current_magic = magic;
            entry.difference = 0;
            continue;
        }
        if entry.current_magic != magic {
            entry.previous_magic = entry.current_magic;
            entry.difference = magic as i32 - entry.current_magic as i32;
            entry.current_magic = magic;
            drop(map);
            on_magic_change(boid, magic);
        }
    }
}

/// The "magic" is an IDENTITY/cache key for the object — it changes when a fighter transforms or
/// the object is replaced (boid recycled), which is when the original reinits and invalidates its
/// effects. It must NOT depend on status (which changes every action — using it caused a reinit +
/// effect-invalidation on every action, breaking effect tracking).
fn read_magic(boid: u32) -> u32 {
    unsafe {
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return 0;
        }
        let kind = sv_battle_object::kind(boid) as u32;
        let entry = sv_battle_object::entry_id(boid) as u32;
        let addr = ptr as usize as u32; // distinguishes object replacements at the same boid
        kind.wrapping_mul(0x9e3779b9) ^ entry.wrapping_mul(0x85ebca6b) ^ addr
    }
}

fn on_magic_change(boid: u32, magic: u32) {
    if let Some(prev) = MAGIC.lock().get(&boid) {
        skyline::println!(
            "[SLight] Magic change: boid {boid} res magic {magic} difference type: {}",
            prev.difference
        );
    }
    crate::slight::agents::refresh_all();
    crate::slight::systems::main_module::on_reinit_fighter(boid);
    crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .invalidate_boid(boid);
}

pub fn snapshot(boid: u32) -> Option<MagicChangeData> {
    MAGIC.lock().get(&boid).cloned()
}

pub fn clear() {
    MAGIC.lock().clear();
}
