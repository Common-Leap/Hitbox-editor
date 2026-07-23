//! Jorge `extras.rs` — Color / RainbowMovement effect tint animations.

use crate::slight::effect_viewer::effect_data::{Color, EffectData};
use crate::slight::math::common_math;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainbowMovement {
    BlueToRed,
    GreenToBlue,
    RedToGreen,
}

impl RainbowMovement {
    pub fn from_state(state: f32) -> Option<Self> {
        match state as i32 {
            0 => Some(Self::BlueToRed),
            1 => Some(Self::GreenToBlue),
            2 => Some(Self::RedToGreen),
            _ => None,
        }
    }

    fn endpoints(self) -> (Color, Color) {
        match self {
            Self::BlueToRed => (
                Color {
                    red: 0.0,
                    green: 0.0,
                    blue: 1.0,
                    alpha: 1.0,
                },
                Color {
                    red: 1.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 1.0,
                },
            ),
            Self::GreenToBlue => (
                Color {
                    red: 0.0,
                    green: 1.0,
                    blue: 0.0,
                    alpha: 1.0,
                },
                Color {
                    red: 0.0,
                    green: 0.0,
                    blue: 1.0,
                    alpha: 1.0,
                },
            ),
            Self::RedToGreen => (
                Color {
                    red: 1.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 1.0,
                },
                Color {
                    red: 0.0,
                    green: 1.0,
                    blue: 0.0,
                    alpha: 1.0,
                },
            ),
        }
    }
}

fn lerp_color(a: &Color, b: &Color, t: f32) -> Color {
    Color {
        red: common_math::lerp(a.red, b.red, t),
        green: common_math::lerp(a.green, b.green, t),
        blue: common_math::lerp(a.blue, b.blue, t),
        alpha: common_math::lerp(a.alpha, b.alpha, t),
    }
}

/// Advance rainbow tint on EffectData — Jorge extras tick @ frame rate.
pub fn tick_rainbow(data: &mut EffectData, dt: f32) {
    let movement = RainbowMovement::from_state(data.rainbow.movement_state);
    let Some(movement) = movement else {
        return;
    };
    let (from, to) = movement.endpoints();
    let t = common_math::clamp01(data.rainbow.movement_state.fract() + dt * 0.02);
    data.rainbow.color = lerp_color(&from, &to, t);
    data.rainbow.movement_state = movement as i32 as f32 + t;
    if t >= 0.999 {
        data.rainbow.movement_state = ((movement as i32 + 1) % 3) as f32;
    }
}

pub fn set_movement(data: &mut EffectData, movement: RainbowMovement) {
    data.rainbow.movement_state = movement as i32 as f32;
    let (from, _) = movement.endpoints();
    data.rainbow.color = from;
}

pub fn tick_tracked_effects() {
    let ids: Vec<u64> = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .iter()
        .map(|e| e.id)
        .collect();
    let mut tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
    for id in ids {
        if let Some(effect) = tracker.get_mut(id) {
            tick_rainbow(&mut effect.data, 1.0);
        }
    }
}
