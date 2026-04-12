// Primitive mesh particle shader.
// Renders VFXB primitive mesh geometry with per-instance Y-axis rotation, scale, and color.

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
    rotation_y: f32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<storage, read> instances: array<MeshInstance>;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;

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

@vertex
fn vs_main(
    vert: VertexInput,
    @builtin(instance_index) instance_idx: u32,
) -> VertexOutput {
    let inst = instances[instance_idx];

    // Y-axis rotation matrix
    let s = sin(inst.rotation_y);
    let c = cos(inst.rotation_y);
    let rotated = vec3<f32>(
        vert.position.x * c + vert.position.z * s,
        vert.position.y,
        -vert.position.x * s + vert.position.z * c,
    );

    // Scale then translate to world position
    let world_pos = rotated * inst.scale + inst.world_pos;

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = vert.uv;
    out.color = inst.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    return tex_color * in.color;
}
