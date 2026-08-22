//! Agent registry — global fighters / founder linkage (DAT_71001e1b80 layout).

use std::collections::HashMap;

use parking_lot::Mutex;
use smash::app::lua_bind::StatusModule;
use smash::app::sv_battle_object;
use std::sync::LazyLock;

use super::effect_viewer::effect_data::EffectData;

pub const MAX_BOID: u32 = 512;
pub const BOID_MASK: u32 = 0x1FF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRecord {
    pub boid: u32,
    pub module_accessor_addr: u64,
    pub category: i32,
    pub kind: i32,
    pub status_kind: i32,
    pub entry_id: i32,
    pub founder_entry_id: Option<i32>,
}

struct AgentRegistry {
    by_boid: HashMap<u32, AgentRecord>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self {
            by_boid: HashMap::with_capacity(64),
        }
    }
}

impl AgentRegistry {
    /// Battle object ids are FULL ids (e.g. 0x2000000x for fighters) — they cannot be
    /// enumerated by scanning 0..512, so refresh only PRUNES dead entries and re-probes the
    /// live ones (status changes each frame). New agents enter via `upsert_module` (per-agent
    /// smashline callbacks + effect-spawn hooks).
    fn refresh(&mut self) {
        let ids: Vec<u32> = self.by_boid.keys().copied().collect();
        for boid in ids {
            match probe(boid) {
                Some(rec) => {
                    self.by_boid.insert(boid, rec);
                }
                None => {
                    self.by_boid.remove(&boid);
                }
            }
        }
    }

    fn upsert_module(
        &mut self,
        module_accessor: *mut smash::app::BattleObjectModuleAccessor,
    ) -> Option<AgentRecord> {
        let boid = boid_from_module(module_accessor)?;
        let rec = probe(boid)?;
        self.by_boid.insert(boid, rec.clone());
        Some(rec)
    }

    fn get(&self, boid: u32) -> Option<&AgentRecord> {
        self.by_boid.get(&boid)
    }

    fn clear(&mut self) {
        self.by_boid.clear();
    }

    fn live_accessors(&self) -> Vec<(u64, *mut smash::app::BattleObjectModuleAccessor)> {
        let mut out = Vec::new();
        for boid in self.by_boid.keys() {
            if !is_live(*boid) {
                continue;
            }
            let ptr = unsafe { sv_battle_object::module_accessor(*boid) };
            if !ptr.is_null() {
                out.push((ptr as u64, ptr));
            }
        }
        out
    }
}

static AGENTS: LazyLock<Mutex<AgentRegistry>> =
    LazyLock::new(|| Mutex::new(AgentRegistry::default()));

pub fn refresh_all() {
    AGENTS.lock().refresh();
}

pub fn upsert_module(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
) -> Option<AgentRecord> {
    AGENTS.lock().upsert_module(module_accessor)
}

pub fn lookup(boid: u32) -> Option<AgentRecord> {
    AGENTS.lock().get(boid).cloned()
}

/// Return whether `boid` is the one fighter callback that drives the global frame pass.
///
/// `StatusLine::Main` is per agent, not a frame clock.  Treating the first repeated battle
/// object id as a frame edge was fragile: an agent may re-enter its Main line before every
/// weapon has run, especially in busy CPU matches, and each false edge re-ran the complete
/// global fighter and weapon dispatch.  Use the lowest live fighter entry instead.  Smash
/// assigns stable non-negative entry ids for the entire match, so exactly that fighter's Main
/// callback is a reliable once-per-game-frame driver.  The BOID tie-breaker keeps malformed or
/// transient duplicate entries deterministic.
pub fn is_frame_driver(boid: u32) -> bool {
    let registry = AGENTS.lock();
    registry
        .by_boid
        .values()
        .filter(|rec| rec.category == 0 && rec.entry_id >= 0)
        .min_by_key(|rec| (rec.entry_id, rec.boid))
        .is_some_and(|rec| rec.boid == boid)
}

/// Jorge FUN_71000db2b4 — lookup live fighter by founder entry id.
pub fn lookup_by_founder(founder_id: i32) -> Option<AgentRecord> {
    if founder_id < 0 {
        return None;
    }
    AGENTS
        .lock()
        .by_boid
        .values()
        .find(|rec| {
            rec.category == 0
                && (rec.entry_id == founder_id || rec.founder_entry_id == Some(founder_id))
        })
        .cloned()
}

pub fn has_initialized(boid: u32) -> bool {
    crate::slight::systems::dynamic_memory::has_agent(boid)
}

pub fn all_records() -> Vec<AgentRecord> {
    AGENTS.lock().by_boid.values().cloned().collect()
}

pub fn clear_all() {
    AGENTS.lock().clear();
}

pub fn live_accessors() -> Vec<(u64, *mut smash::app::BattleObjectModuleAccessor)> {
    AGENTS.lock().live_accessors()
}

pub fn boid_from_module(
    module_accessor: *mut smash::app::BattleObjectModuleAccessor,
) -> Option<u32> {
    if module_accessor.is_null() {
        return None;
    }
    unsafe {
        // FULL battle object id — sv_battle_object::* take the full id, NOT a 9-bit index.
        // (Masking with BOID_MASK here made the identity check below fail for all but one
        // fighter, so the registry only ever saw 1 agent and everyone else's effects were
        // treated as owned by dead accessors.)
        let boid = (*module_accessor).battle_object_id;
        if !is_live(boid) {
            return None;
        }
        if sv_battle_object::module_accessor(boid) != module_accessor {
            return None;
        }
        Some(boid)
    }
}

fn is_live(boid: u32) -> bool {
    unsafe { !sv_battle_object::is_null(boid) && sv_battle_object::is_active(boid) }
}

fn probe(boid: u32) -> Option<AgentRecord> {
    if !is_live(boid) {
        return None;
    }
    unsafe {
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return None;
        }
        let category = sv_battle_object::category(boid);
        let kind = sv_battle_object::kind(boid);
        let entry_id = sv_battle_object::entry_id(boid);
        let status_kind = if category == 0 {
            StatusModule::status_kind(ptr)
        } else {
            0
        };
        let founder_entry_id = if category == 0 {
            None
        } else {
            let raw = sv_battle_object::get_founder_id(boid);
            if raw >= 0 && is_live(raw as u32) {
                let fe = sv_battle_object::entry_id(raw as u32);
                (fe >= 0).then_some(fe)
            } else {
                None
            }
        };
        Some(AgentRecord {
            boid,
            module_accessor_addr: ptr as u64,
            category,
            kind,
            status_kind,
            entry_id,
            founder_entry_id,
        })
    }
}

pub fn format_effect_name(
    id: u64,
    category: i32,
    object_kind: i32,
    entry_id: i32,
    founder_entry_id: Option<i32>,
    data: &EffectData,
) -> String {
    // The original has no "Effect #..." fallback string in its binary; the effect name is
    // always the resolved hash label (e.g. "0x0" for a zero hash), used as-is.
    let _ = id;
    let label = data.effect_name.clone();
    // The ignored reference binary's rodata contains exactly one effect-name
    // template: "<name> of weapon with kind <kind> (player: <player>)" with the unknown-owner
    // fallback spelled "Unknwon fighter" — Jorge's original typo, reproduced here for parity.
    // There is no "of agent kind" or "(fighter kind ...)" template in the binary, so any
    // non-fighter object uses the weapon template and a plain fighter uses just its name.
    match category {
        0 => label,
        _ => {
            let player = founder_entry_id
                .or((entry_id >= 0).then_some(entry_id))
                .map(|e| format!("entry {e}"))
                .unwrap_or_else(|| "Unknwon fighter".into());
            format!("{label} of weapon with kind {object_kind} (player: {player})")
        }
    }
}
