// Primitive mesh particle shader.
// Renders VFXB primitive mesh geometry with per-instance full 3-axis rotation, scale, and color.
// Supports the same indirect texture distortion path as particle.wgsl.

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    cam_right: vec3<f32>,
    _pad0: f32,
    cam_up: vec3<f32>,
    _pad1: f32,
}

struct MeshInstance {
    world_pos: vec3<f32>,
    scale: f32,
    color: vec4<f32>,
    rotation_x: f32,
    rotation_y: f32,
    rotation_z: f32,
    _pad: f32,
    tex_scale: vec2<f32>,
    tex_offset: vec2<f32>,
}

struct IndirectParams {
    is_indirect: u32,
    distortion_strength: f32,
    indirect_scroll_u: f32,
    indirect_scroll_v: f32,
    indirect_scale_u: f32,
    indirect_scale_v: f32,
    indirect_offset_u: f32,
    indirect_offset_v: f32,
}

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<storage, read> instances: array<MeshInstance>;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(1) @binding(2) var alpha_tex: texture_2d<f32>;
@group(1) @binding(3) var alpha_sampler: sampler;
@group(1) @binding(4) var indirect_tex: texture_2d<f32>;
@group(1) @binding(5) var indirect_sampler: sampler;
@group(1) @binding(6) var<uniform> indirect_params: IndirectParams;
@group(2) @binding(0) var emissive_tex: texture_2d<f32>;
@group(2) @binding(1) var emissive_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) normal: vec3<f32>,
}

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

// Build a ZYX Euler rotation matrix (matches glam::EulerRot::ZYX used on the CPU side).
fn euler_zyx(rx: f32, ry: f32, rz: f32) -> mat3x3<f32> {
    let sx = sin(rx); let cx = cos(rx);
    let sy = sin(ry); let cy = cos(ry);
    let sz = sin(rz); let cz = cos(rz);
    // R = Rz * Ry * Rx
    return mat3x3<f32>(
        vec3<f32>( cy*cz,  cy*sz, -sy),
        vec3<f32>( cz*sx*sy - cx*sz,  cx*cz + sx*sy*sz,  cy*sx),
        vec3<f32>( cx*cz*sy + sx*sz, -cz*sx + cx*sy*sz,  cx*cy),
    );
}

@vertex
fn vs_main(
    vert: VertexInput,
    @builtin(instance_index) instance_idx: u32,
) -> VertexOutput {
    let inst = instances[instance_idx];

    // Full 3-axis rotation (ZYX Euler, matching emitter_rotation convention)
    let rot = euler_zyx(inst.rotation_x, inst.rotation_y, inst.rotation_z);
    let rotated = rot * vert.position;

    // Scale then translate to world position
    let world_pos = rotated * inst.scale + inst.world_pos;

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = vert.uv * inst.tex_scale + inst.tex_offset;
    out.color = inst.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base_uv = in.uv;

    var final_uv: vec2<f32>;
    var alpha_mask: f32;

    if indirect_params.is_indirect == 1u {
        let indirect_scale  = vec2<f32>(indirect_params.indirect_scale_u,  indirect_params.indirect_scale_v);
        let indirect_offset = vec2<f32>(indirect_params.indirect_offset_u, indirect_params.indirect_offset_v);
        let indirect_scroll = vec2<f32>(indirect_params.indirect_scroll_u, indirect_params.indirect_scroll_v);
        let indirect_uv = base_uv * indirect_scale + indirect_offset + indirect_scroll;
        let offset = textureSample(indirect_tex, indirect_sampler, indirect_uv).rg;
        final_uv   = base_uv + (offset * 2.0 - 1.0) * indirect_params.distortion_strength;
        alpha_mask = 1.0;
    } else {
        final_uv   = base_uv;
        alpha_mask = textureSample(alpha_tex, alpha_sampler, base_uv).a;
    }

    let tex_color   = textureSample(tex, tex_sampler, final_uv);
    let final_alpha = tex_color.a * alpha_mask;
    let result = vec4<f32>(tex_color.rgb, final_alpha) * in.color;
    if result.a < 0.001 { discard; }
    // Add emissive contribution (additive, clamped to [0,1])
    let emissive = textureSample(emissive_tex, emissive_sampler, final_uv).rgb;
    let lit_rgb = clamp(result.rgb + emissive * result.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(lit_rgb, result.a);
}
