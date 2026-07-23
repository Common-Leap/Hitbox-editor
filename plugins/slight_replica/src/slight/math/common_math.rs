//! Jorge `common_math` helpers — lerp / clamp for extras animations.

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[inline]
pub fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}
