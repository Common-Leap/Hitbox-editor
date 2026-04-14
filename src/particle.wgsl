// Particle billboard shader.
// Each particle is a screen-aligned quad expanded from a single instance.
// Vertex index 0-5 forms two triangles (a quad) via the vertex_index trick.

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    cam_right: vec3<f32>,
    _pad0: f32,
    cam_up: vec3<f32>,
    _pad1: f32,
}

struct ParticleInstance {
    position: vec3<f32>,
    size: f32,
    color: vec4<f32>,
    rotation: f32,
    _pad_rot: f32,
    tex_scale: vec2<f32>,
    tex_offset: vec2<f32>,
    _pad: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<storage, read> particles: array<ParticleInstance>;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

// Quad corners in local space (two triangles, CCW)
const QUAD_POS = array<vec2<f32>, 6>(
    vec2<f32>(-0.5, -0.5),
    vec2<f32>( 0.5, -0.5),
    vec2<f32>( 0.5,  0.5),
    vec2<f32>(-0.5, -0.5),
    vec2<f32>( 0.5,  0.5),
    vec2<f32>(-0.5,  0.5),
);
const QUAD_UV = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.0, 0.0),
);

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_idx: u32,
    @builtin(instance_index) instance_idx: u32,
) -> VertexOutput {
    let p = particles[instance_idx];
    let corner = QUAD_POS[vertex_idx];

    // Rotate corner in billboard plane
    let s = sin(p.rotation);
    let c = cos(p.rotation);
    let rotated = vec2<f32>(
        corner.x * c - corner.y * s,
        corner.x * s + corner.y * c,
    );

    // Expand billboard in camera space
    let world_pos = p.position
        + camera.cam_right * rotated.x * p.size
        + camera.cam_up    * rotated.y * p.size;

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = QUAD_UV[vertex_idx] * p.tex_scale + p.tex_offset;
    out.color = p.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    // Use texture as alpha mask (R channel) multiplied by particle color.
    // BC5 textures store intensity in R; BC7/other textures use full RGBA.
    // The tex_color.a already encodes the mask (set by upload_textures swizzle).
    let result = vec4<f32>(in.color.rgb, tex_color.a * in.color.a);
    if result.a < 0.01 {
        discard;
    }
    return result;
}
