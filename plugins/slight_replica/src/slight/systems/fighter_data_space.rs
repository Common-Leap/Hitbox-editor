//! Per-agent RealDataMap / DataMap storage — Jorge fighter_data_space facade.

use parking_lot::Mutex;
use smash::app::lua_bind::{DamageModule, MotionModule};
use smash::app::sv_battle_object;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::slight::agents::AgentRecord;
use crate::slight::effect_viewer::effect_data::EffectData;
use crate::slight::systems::multipliers::EFFECT_DATA_KEY;
use crate::slight::systems::time_counting::FrameChecker;

/// Global Effect-data slot registered at install (Jorge sentinel key `u32::MAX`).
pub const INSTALL_INDEX: u32 = u32::MAX;

static COMMON_SPACES: LazyLock<Mutex<HashMap<u32, RealDataMap>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PERSONAL_SPACES: LazyLock<Mutex<HashMap<u64, RealDataMap>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataPurpose {
    Common,
    Personal,
    Slight,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LuaStateAgent {
    pub boid: u32,
    pub lua_state: u64,
    pub object_id: u32,
}

/// Jorge `BasicDebug` — 13 serde fields @ 0x1752f8.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasicDebug {
    pub is_active: bool,
    pub lua_state: u64,
    pub object_id: u32,
    pub index: u32,
    pub team: i32,
    pub category: i32,
    pub kind: i32,
    pub status: i32,
    pub animation: u64,
    pub frame: u32,
    pub damage: f32,
    pub situation: i32,
    pub lock_stick: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DebugPair {
    pub previous: BasicDebug,
    pub current: BasicDebug,
}

/// Jorge `DataMap` — Effect data slot + debug pair + invalid flags.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataMap {
    pub invalid: bool,
    pub invalid_drop: bool,
    pub effect_data: EffectData,
    pub debug: DebugPair,
}

/// Jorge `RealDataMap` — per lua-state agent with indexed DataMap vector.
#[derive(Clone, Debug)]
pub struct RealDataMap {
    pub lua_state_agent: LuaStateAgent,
    pub data_map_index: usize,
    pub purpose: DataPurpose,
    pub time_counter: FrameChecker,
    pub maps: Vec<DataMap>,
}

impl Default for RealDataMap {
    fn default() -> Self {
        Self {
            lua_state_agent: LuaStateAgent::default(),
            data_map_index: 0,
            purpose: DataPurpose::Common,
            time_counter: FrameChecker::default(),
            maps: vec![DataMap::default()],
        }
    }
}

impl RealDataMap {
    pub fn active_map(&self) -> Option<&DataMap> {
        self.maps.get(self.data_map_index)
    }

    pub fn active_map_mut(&mut self) -> Option<&mut DataMap> {
        let idx = self.data_map_index;
        self.maps.get_mut(idx)
    }

    pub fn union_maps(&mut self, other: RealDataMap) {
        self.maps.extend(other.maps);
        if self.data_map_index >= self.maps.len() {
            self.data_map_index = self.maps.len().saturating_sub(1);
        }
    }
}

pub fn install() {
    let mut map = RealDataMap::default();
    map.purpose = DataPurpose::Slight;
    map.maps[0].effect_data = EffectData::default();
    COMMON_SPACES.lock().insert(INSTALL_INDEX, map);
    skyline::println!("[SLight] Fighter Data Space — Effect data slot registered");
}

fn new_common_space(boid: u32) -> RealDataMap {
    let mut map = RealDataMap::default();
    map.purpose = DataPurpose::Common;
    map.lua_state_agent = LuaStateAgent {
        boid,
        lua_state: agent_lua_state(boid),
        object_id: boid,
    };
    map.maps[0].effect_data.index = boid;
    map
}

fn new_personal_space(agent: &LuaStateAgent) -> RealDataMap {
    let mut map = RealDataMap::default();
    map.purpose = DataPurpose::Personal;
    map.lua_state_agent = agent.clone();
    map.maps[0].effect_data.index = agent.boid;
    map
}

fn agent_lua_state(boid: u32) -> u64 {
    unsafe {
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            0
        } else {
            ptr as u64
        }
    }
}

pub fn ensure(boid: u32) -> RealDataMap {
    common_space(boid).clone()
}

pub fn common_space(boid: u32) -> RealDataMap {
    COMMON_SPACES
        .lock()
        .entry(boid)
        .or_insert_with(|| new_common_space(boid))
        .clone()
}

pub fn personal_space(lua_state: u64, boid: u32) -> RealDataMap {
    let agent = LuaStateAgent {
        boid,
        lua_state,
        object_id: boid,
    };
    PERSONAL_SPACES
        .lock()
        .entry(lua_state)
        .or_insert_with(|| new_personal_space(&agent))
        .clone()
}

pub fn set_effect_data(boid: u32, data: EffectData) {
    let mut spaces = COMMON_SPACES.lock();
    let space = spaces.entry(boid).or_insert_with(|| new_common_space(boid));
    if let Some(map) = space.active_map_mut() {
        map.invalid = false;
        map.effect_data = data;
    }
}

pub fn get_effect_data(boid: u32) -> Option<EffectData> {
    unwrap_effect_data(boid).ok()
}

/// Jorge FUN_71000d7c88 — keyed unwrap with invalid-map guards.
pub fn unwrap_effect_data(boid: u32) -> Result<EffectData, &'static str> {
    let spaces = COMMON_SPACES.lock();
    let Some(space) = spaces.get(&boid) else {
        return Err("Data space doesn't exists");
    };
    if space.data_map_index >= space.maps.len() {
        return Err(
            "Data space exists, but index doesn't point to a map, did you perform the 'union' operation?",
        );
    }
    let map = &space.maps[space.data_map_index];
    if map.invalid {
        return Err("Tried to get and unwrap data with an invalid map");
    }
    Ok(map.effect_data.clone())
}

pub fn effect_data_for(boid: u32) -> Option<EffectData> {
    get_effect_data(boid).or_else(|| {
        COMMON_SPACES
            .lock()
            .get(&INSTALL_INDEX)
            .and_then(|s| s.active_map().map(|m| m.effect_data.clone()))
    })
}

pub fn set_damage(boid: u32, damage: f32) {
    let mut spaces = COMMON_SPACES.lock();
    let space = spaces.entry(boid).or_insert_with(|| new_common_space(boid));
    if let Some(map) = space.active_map_mut() {
        map.debug.current.damage = damage;
    }
}

pub fn set_data_map_index(boid: u32, index: usize) {
    let mut spaces = COMMON_SPACES.lock();
    if let Some(space) = spaces.get_mut(&boid) {
        space.data_map_index = index;
    }
}

pub fn mark_invalid(boid: u32, invalid_drop: bool) {
    let mut spaces = COMMON_SPACES.lock();
    if let Some(space) = spaces.get_mut(&boid) {
        if let Some(map) = space.active_map_mut() {
            map.invalid = true;
            map.invalid_drop = invalid_drop;
        }
    }
}

/// Jorge invalid_drop — refuse drop when flagged, else remove map entry.
pub fn drop_map(boid: u32) -> bool {
    let mut spaces = COMMON_SPACES.lock();
    let Some(space) = spaces.get_mut(&boid) else {
        return false;
    };
    let invalid_drop = space.active_map().map(|m| m.invalid_drop).unwrap_or(false);
    if invalid_drop {
        skyline::println!("[SLight] Invalid drop");
        return false;
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!(
            "[SLight] Trying to drop map in {boid:?} - Map removed (Purpose: {:?})",
            space.purpose
        );
    }
    spaces.remove(&boid);
    true
}

pub fn on_frame() {
    let frame = crate::slight::frame_context::match_frame();
    let Some(rec) = crate::slight::frame_context::current_agent() else {
        return;
    };
    if rec.category != 0 {
        return;
    }

    let boid = rec.boid;
    // The BasicDebug needs the fighter's CURRENT damage. `damage_manager::snapshot` is the init-time
    // value (set once, never updated), so it would always read as the starting damage.
    let damage = unsafe {
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            0.0
        } else {
            DamageModule::damage(ptr, 0)
        }
    };
    let debug = probe_debug(&rec, frame, damage);

    {
        let mut spaces = COMMON_SPACES.lock();
        let space = spaces.entry(boid).or_insert_with(|| new_common_space(boid));
        space.lua_state_agent = LuaStateAgent {
            boid,
            lua_state: rec.module_accessor_addr,
            object_id: boid,
        };

        if let Some(map) = space.active_map_mut() {
            map.debug.previous = map.debug.current.clone();
            map.debug.current = debug.clone();
            map.effect_data.index = boid;
            sync_effect_from_tracker(boid, &mut map.effect_data);
        }

        tick_time_counter(space);
    }

    let lua_key = rec.module_accessor_addr;
    {
        let mut personal = PERSONAL_SPACES.lock();
        let space = personal.entry(lua_key).or_insert_with(|| {
            new_personal_space(&LuaStateAgent {
                boid,
                lua_state: lua_key,
                object_id: boid,
            })
        });
        if let Some(map) = space.active_map_mut() {
            map.debug.previous = map.debug.current.clone();
            map.debug.current = debug.clone();
            map.effect_data.index = boid;
        }
        tick_time_counter(space);
    }
}

fn tick_time_counter(space: &mut RealDataMap) {
    // The Time counting facade owns the per-agent FrameChecker and advances it in its post_frame
    // (which runs before the Fighter Data Space frame). Here we only MIRROR it into the data space
    // — advancing again here double-counted real_range/count.
    space.time_counter =
        crate::slight::systems::time_counting::checker_for(space.lua_state_agent.boid);
}

fn probe_debug(rec: &AgentRecord, frame: u32, damage: f32) -> BasicDebug {
    let animation = unsafe {
        let ptr = sv_battle_object::module_accessor(rec.boid);
        if ptr.is_null() {
            0
        } else {
            MotionModule::motion_kind(ptr)
        }
    };
    BasicDebug {
        is_active: true,
        lua_state: rec.module_accessor_addr,
        object_id: rec.boid,
        index: rec.entry_id.max(0) as u32,
        team: 0,
        category: rec.category,
        kind: rec.kind,
        status: rec.status_kind,
        animation,
        frame,
        damage,
        situation: 0,
        lock_stick: false,
    }
}

fn sync_effect_from_tracker(boid: u32, data: &mut EffectData) {
    let tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
    let Some(effect) = tracker.iter().find(|e| e.boid == boid) else {
        return;
    };
    if data.effect_name.is_empty() || data.effect_name == "0x0" {
        data.effect_name = effect.data.effect_name.clone();
    }
    if data.bone_name.is_empty() || data.bone_name == "0x0" {
        data.bone_name = effect.data.bone_name.clone();
    }
    data.scale = effect.data.scale;
    data.rate = effect.data.rate;
    data.frame = effect.data.frame;
    data.visible = effect.data.visible;
    data.is_follow = effect.data.is_follow;
    data.pos = effect.data.pos.clone();
    data.rot = effect.data.rot.clone();
    data.rainbow = effect.data.rainbow.clone();
}

pub fn init_fighter(boid: u32) {
    let space = common_space(boid);
    let _ = personal_space(space.lua_state_agent.lua_state, boid);
    skyline::println!("[SLight] Common data space for an index — fighter {boid}");
}

pub fn init_personal(lua_state: u64, boid: u32) {
    let _ = personal_space(lua_state, boid);
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Personal data space — lua {lua_state:#x} fighter {boid}");
    }
}

pub fn debug_pair(boid: u32) -> Option<DebugPair> {
    COMMON_SPACES
        .lock()
        .get(&boid)
        .and_then(|s| s.active_map().map(|m| m.debug.clone()))
}

pub fn keyed_get(boid: u32, key: &str) -> Result<EffectData, &'static str> {
    if key != EFFECT_DATA_KEY {
        return Err("Error. Tried unwrapping key , but it doesn't exists");
    }
    unwrap_effect_data(boid)
}

pub fn clear() {
    COMMON_SPACES.lock().clear();
    PERSONAL_SPACES.lock().clear();
}
