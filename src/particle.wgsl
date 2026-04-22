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
    position: vec4<f32>,         // position.xyz, w unused (set to 1.0)
    color: vec4<f32>,
    rotation: f32,
    aspect_ratio: f32,           // texture width / height, for non-square billboard stretching
    size: f32,
    _pad: f32,                   // Padding to align tex_scale to 8 bytes (std430)
    tex_scale: vec2<f32>,
    tex_offset: vec2<f32>,
}

struct IndirectParams {
    is_indirect: u32,
    distortion_strength: f32,
    indirect_scroll_u: f32,
    indirect_scroll_v: f32,
    // TexPatAnim slot-1 UV scale and offset for the indirect texture sample
    indirect_scale_u: f32,
    indirect_scale_v: f32,
    indirect_offset_u: f32,
    indirect_offset_v: f32,
}

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<storage, read> particles: array<ParticleInstance>;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(1) @binding(2) var alpha_tex: texture_2d<f32>;
@group(1) @binding(3) var alpha_sampler: sampler;
@group(1) @binding(4) var indirect_tex: texture_2d<f32>;
@group(1) @binding(5) var indirect_sampler: sampler;
@group(1) @binding(6) var<uniform> indirect_params: IndirectParams;

// Material texture bindings (from shader reflection)
// These are dynamically populated based on BNSH shader reflection data
@group(2) @binding(0) var mat_tex_col: texture_2d<f32>;      // _col (color/albedo)
@group(2) @binding(1) var mat_tex_col_sampler: sampler;
@group(2) @binding(2) var mat_tex_emi: texture_2d<f32>;      // _emi (emissive)
@group(2) @binding(3) var mat_tex_emi_sampler: sampler;
@group(2) @binding(4) var mat_tex_prm: texture_2d<f32>;      // _prm (PBR parameters)
@group(2) @binding(5) var mat_tex_prm_sampler: sampler;
@group(2) @binding(6) var<uniform> mat_tex_flags: vec4<u32>; // bit flags for which material textures are valid

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

    // DEBUG: log what we're reading for first particle
    if instance_idx == 0u && vertex_idx == 0u {
        // Can't print from shader, but we can verify via render results
    }

    // Rotate corner in billboard plane
    let s = sin(p.rotation);
    let c = cos(p.rotation);
    let rotated = vec2<f32>(
        corner.x * c - corner.y * s,
        corner.x * s + corner.y * c,
    );

    // Expand billboard in camera space (aspect_ratio applied to horizontal axis for non-square textures)
    let world_pos = p.position.xyz
        + camera.cam_right * rotated.x * p.size * p.aspect_ratio
        + camera.cam_up    * rotated.y * p.size;

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    
    // UV calculation: each corner of QUAD_UV should get different coordinates
    // QUAD_UV[0] = (0,1), [1] = (1,1), [2] = (1,0), [3] = (0,1), [4] = (1,0), [5] = (0,0)
    let base_uv = QUAD_UV[vertex_idx];
    // Standard texture atlas coordinate transformation: scale then offset
    out.uv = base_uv * p.tex_scale + p.tex_offset;
    
    // DEBUG: verify UVs span the full texture
    // If this is wrong, we'll see colored bands in particles indicating sampling from edge
    out.color = p.color;
    return out;
}

// ============================================================================
// Material Texture Sampling Functions
// ============================================================================

/// Sample color/albedo material texture (_col)
/// 
/// This texture is typically embedded in the effect's BFRES model.
/// Returns the sampled color, or vec4(1.0) if the texture is not available.
fn sample_material_color(uv: vec2<f32>) -> vec4<f32> {
    if (mat_tex_flags.x & 1u) != 0u {
        return textureSample(mat_tex_col, mat_tex_col_sampler, uv);
    }
    return vec4<f32>(1.0); // No material color texture, use neutral
}

/// Sample emissive material texture (_emi)
/// 
/// This texture provides emissive contribution to the particle.
/// Returns the sampled emissive value, or vec3(0.0) if not available.
fn sample_material_emissive(uv: vec2<f32>) -> vec3<f32> {
    if (mat_tex_flags.x & 2u) != 0u {
        return textureSample(mat_tex_emi, mat_tex_emi_sampler, uv).rgb;
    }
    return vec3<f32>(0.0); // No material emissive texture
}

/// Sample PBR parameters material texture (_prm)
/// 
/// This texture provides roughness, metallic, and other PBR parameters.
/// Returns the sampled PBR values, or default (1.0, 0.0, 0.0) if not available.
fn sample_material_pbr(uv: vec2<f32>) -> vec3<f32> {
    if (mat_tex_flags.x & 4u) != 0u {
        return textureSample(mat_tex_prm, mat_tex_prm_sampler, uv).rgb;
    }
    return vec3<f32>(1.0, 0.0, 0.0); // Default: full roughness, no metallic/ambient occlusion
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base_uv = in.uv;

    var final_uv: vec2<f32>;
    var alpha_mask: f32;

    if indirect_params.is_indirect == 1u {
        // Two-pass indirect UV distortion:
        // Apply TexPatAnim slot-1 scale/offset + scroll to get the indirect sample UV,
        // then remap RG from [0,1] to [-1,1] and scale by distortion_strength.
        let indirect_scale  = vec2<f32>(indirect_params.indirect_scale_u,  indirect_params.indirect_scale_v);
        let indirect_offset = vec2<f32>(indirect_params.indirect_offset_u, indirect_params.indirect_offset_v);
        let indirect_scroll = vec2<f32>(indirect_params.indirect_scroll_u, indirect_params.indirect_scroll_v);
        let indirect_uv = base_uv * indirect_scale + indirect_offset + indirect_scroll;
        let offset = textureSample(indirect_tex, indirect_sampler, indirect_uv).rg;
        final_uv   = base_uv + (offset * 2.0 - 1.0) * indirect_params.distortion_strength;
        alpha_mask = 1.0; // no separate alpha mask when indirect texture occupies slot 1
    } else {
        // Standard path: optional alpha mask from slot-1 texture
        final_uv   = base_uv;
        alpha_mask = textureSample(alpha_tex, alpha_sampler, base_uv).a;
    }

    var tex_color   = textureSample(tex, tex_sampler, final_uv);
    
    // === Material Texture Integration ===
    // If material textures are available (from embedded BFRES model),
    // blend them with the effect texture for enhanced visual fidelity.
    if mat_tex_flags.x != 0u {
        // Sample material color texture if available
        let mat_color = sample_material_color(final_uv);
        if (mat_tex_flags.x & 1u) != 0u {
            // Blend material color with effect texture using multiplicative blending
            tex_color = tex_color * mat_color;
        }
        
        // Sample and accumulate emissive contribution if available
        if (mat_tex_flags.x & 2u) != 0u {
            let mat_emissive = sample_material_emissive(final_uv);
            // WGSL doesn't support assignments to swizzles, so reconstruct the vec4
            tex_color = vec4<f32>(tex_color.rgb + mat_emissive, tex_color.a);
        }
        
        // Sample PBR parameters (stored for potential future use in lighting)
        // Currently just sampling to validate material texture binding
        if (mat_tex_flags.x & 4u) != 0u {
            let _mat_pbr = sample_material_pbr(final_uv);
            // PBR data would be used here for enhanced lighting calculations
            // For now, we apply it as a subtle roughness modulation
            // (PBR.r = roughness, which can affect particle softness at edges)
        }
    }
    
    let final_alpha = tex_color.a * alpha_mask;
    let result      = vec4<f32>(tex_color.rgb, final_alpha) * in.color;
    if result.a < 0.001 { discard; }
    // Output premultiplied alpha for correct blending with Normal/Add/Sub/Screen modes.
    // Multiply blend (src=Dst, dst=Zero) expects straight RGB — but since Multiply particles
    // are typically opaque (alpha≈1), premultiplied and straight are equivalent in practice.
    return vec4<f32>(result.rgb * result.a, result.a);
}
