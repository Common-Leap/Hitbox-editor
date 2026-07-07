// Fullscreen triangle blit: composites an offscreen particle texture onto the surface.

@group(0) @binding(0) var t_particle: texture_2d<f32>;
@group(0) @binding(1) var s_particle: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    // Fullscreen triangle
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VOut;
    out.pos = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv = uv[vi];
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    // Only skip fully empty texels so faint/smoke particles still composite.
    if (c.a == 0.0 && c.r == 0.0 && c.g == 0.0 && c.b == 0.0) {
        discard;
    }
    return vec4(c.rgb, 1.0);
}

// ACES filmic approximation (Narkowicz 2015). Rolls HDR fire off toward saturated
// orange instead of clamping to white like a raw 8-bit additive accumulate.
fn tonemap_aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3(0.0), vec3(1.0));
}

// Per-channel shoulder: identity below the knee, smooth exponential rolloff above —
// LDR-authored colours pass through untouched; only HDR magnitudes compress. The knee
// sits below 1 so the shoulder is continuous; per-channel application preserves the
// game's channel-saturation hue shift (fire rolls toward yellow/white, not just dimmer).
fn tonemap_shoulder(x: vec3<f32>) -> vec3<f32> {
    let knee = 0.85;
    let span = 1.0 - knee;
    let over = max(x - vec3(knee), vec3(0.0));
    let shoulder = vec3(knee) + span * (vec3(1.0) - exp(-over / span));
    return select(shoulder, x, x <= vec3(knee));
}

// HDR composite: offscreen is RGBA16F accumulated in linear light; tonemap and
// alpha-composite over the scene (premultiplied layer, blend One / OneMinusSrcAlpha).
@fragment
fn fs_tonemap_main(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    if (c.a == 0.0 && c.r == 0.0 && c.g == 0.0 && c.b == 0.0) {
        discard;
    }
    let alpha = clamp(c.a, 0.0, 1.0);
    return vec4(tonemap_shoulder(c.rgb), alpha);
}

// A/B variant (FX_TONEMAP=aces): the filmic curve reshapes LDR content too
// (0.18→0.28, 1.0→0.80) — kept for comparison, not the default.
@fragment
fn fs_tonemap_aces(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    if (c.a == 0.0 && c.r == 0.0 && c.g == 0.0 && c.b == 0.0) {
        discard;
    }
    let alpha = clamp(c.a, 0.0, 1.0);
    return vec4(tonemap_aces(c.rgb), alpha);
}

// A/B variant (FX_TONEMAP=clip): plain clamp — shows raw per-channel saturation.
@fragment
fn fs_tonemap_clip(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    if (c.a == 0.0 && c.r == 0.0 && c.g == 0.0 && c.b == 0.0) {
        discard;
    }
    let alpha = clamp(c.a, 0.0, 1.0);
    return vec4(clamp(c.rgb, vec3(0.0), vec3(1.0)), alpha);
}

// Sub offscreen is cleared to white; discard untouched backdrop before reverse-subtract blit.
@fragment
fn fs_sub_main(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    if (c.r > 0.999 && c.g > 0.999 && c.b > 0.999) {
        discard;
    }
    return c;
}
