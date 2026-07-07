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

// Sub offscreen is cleared to white; discard untouched backdrop before reverse-subtract blit.
@fragment
fn fs_sub_main(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_particle, s_particle, in.uv);
    if (c.r > 0.999 && c.g > 0.999 && c.b > 0.999) {
        discard;
    }
    return c;
}
