//! Agent snapshot for RPM — Jorge FUN_71001078f8 (`AgentInfo` header on init when after-win).

use serde::Serialize;
use smash::app::lua_bind::{
    DamageModule, MotionModule, PostureModule, StatusModule, TeamModule, WorkModule,
};
use smash::app::sv_battle_object;
use smash::app::utility;

use crate::slight::frame_context;

const WORK_SLOT_VAR: i32 = 0x10000042;

#[derive(Clone, Debug, Serialize)]
pub struct AgentInfo {
    pub active: bool,
    /// Jorge rodata typo — keep for RPM client compatibility.
    #[serde(rename = "figther_index")]
    pub fighter_index: i32,
    pub founder: i32,
    pub boid: u32,
    pub team: i32,
    pub slot: i32,
    pub category: i32,
    pub fighter_kind: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fighter_name: Option<String>,
    pub pos: Pos3,
    pub status_kind: i32,
    pub motion_kind: u64,
    pub frame: f32,
    pub damage: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Pos3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub fn snapshot(boid: u32) -> Option<AgentInfo> {
    unsafe {
        if !sv_battle_object::is_active(boid) || sv_battle_object::is_null(boid) {
            return None;
        }
        let ptr = sv_battle_object::module_accessor(boid);
        if ptr.is_null() {
            return None;
        }
        let active = sv_battle_object::is_active(boid) && !sv_battle_object::is_null(boid);
        let category = sv_battle_object::category(boid);
        let kind = utility::get_kind(&mut *ptr);
        let pos_ptr = PostureModule::pos(ptr);
        let pos = Pos3 {
            x: (*pos_ptr).x,
            y: (*pos_ptr).y,
            z: (*pos_ptr).z,
        };
        let fighter_name = if category == 1 {
            Some(kind.to_string())
        } else {
            None
        };
        Some(AgentInfo {
            active,
            fighter_index: frame_context::resolve_work_boid(boid) as i32,
            founder: sv_battle_object::get_founder_id(boid),
            boid,
            team: TeamModule::team_no(ptr) as i32,
            slot: WorkModule::get_int(ptr, WORK_SLOT_VAR),
            category,
            fighter_kind: kind,
            fighter_name,
            pos,
            status_kind: StatusModule::status_kind(ptr),
            motion_kind: MotionModule::motion_kind(ptr),
            frame: MotionModule::frame(ptr),
            damage: DamageModule::damage(ptr, 0),
        })
    }
}

pub fn notify_if_after_win(boid: u32) {
    if !crate::slight::frame_context::is_after_win() {
        return;
    }
    let Some(info) = snapshot(boid) else {
        return;
    };
    crate::rust_extender::debuggable_server::notify_agent_info(&info);
}

pub fn clear() {}
