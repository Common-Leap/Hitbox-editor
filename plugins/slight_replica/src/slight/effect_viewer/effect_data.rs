//! Jorge `EffectData` — 11 serde fields from NRO reflection @ 0x175376.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Point3D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Color {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rainbow {
    pub color: Color,
    pub movement_state: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EffectData {
    pub index: u32,
    #[serde(rename = "effect_name")]
    pub effect_name: String,
    #[serde(rename = "bone_name")]
    pub bone_name: String,
    pub is_follow: bool,
    pub visible: bool,
    pub scale: f32,
    pub rate: f32,
    pub frame: f32,
    pub pos: Point3D,
    pub rot: Point3D,
    pub rainbow: Rainbow,
}

impl Default for EffectData {
    fn default() -> Self {
        Self {
            index: 0,
            effect_name: "0x0".into(),
            bone_name: "0x0".into(),
            is_follow: false,
            visible: true,
            scale: 1.0,
            rate: 1.0,
            frame: 0.0,
            pos: Point3D::default(),
            rot: Point3D::default(),
            rainbow: Rainbow {
                color: Color {
                    red: 1.0,
                    green: 1.0,
                    blue: 1.0,
                    alpha: 1.0,
                },
                movement_state: 0.0,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpmEffectData {
    pub index: u32,
    pub effect_name: String,
    pub bone_name: String,
    pub is_follow: bool,
    pub visible: bool,
    pub scale: f32,
    pub frame: f32,
    pub pos: Point3D,
    pub rot: Point3D,
    pub speed: f32,
    pub rainbow: Rainbow,
}

impl RpmEffectData {
    pub fn from_effect_data(d: &EffectData) -> Self {
        Self {
            index: d.index,
            effect_name: d.effect_name.clone(),
            bone_name: d.bone_name.clone(),
            is_follow: d.is_follow,
            visible: d.visible,
            scale: d.scale,
            frame: d.frame,
            pos: d.pos.clone(),
            rot: d.rot.clone(),
            speed: d.rate,
            rainbow: d.rainbow.clone(),
        }
    }
}

pub fn hash_label(hash: u64) -> String {
    super::effect_names::label(hash)
}

pub fn effect_index(handle: u32) -> u32 {
    handle >> 4
}

pub fn is_valid_effect_index(handle: u32) -> bool {
    effect_index(handle) < 625
}
