// NVN register chain emulation: analyzes BNSH shaders for cbuf slot usage,
// maps each slot to its game data source, and generates NVN constant buffer
// data from actual PTCL emitter parameters.
//
// The NVN position/color/UV chains are fixed-function GPU stages that run
// before the shader. On Vulkan we emulate them by (1) computing the chain
// outputs on CPU and feeding them as vertex attributes, and (2) filling the
// NVN constant buffers (cbuf_8/9/10/16) from game data instead of identity.

use crate::effects::{ColorKey, EmitterAnimDef, EmitterDef, TextureRes};
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};

/// ── Phase 0: SPIR-V / WGSL cbuf slot analyzer ──────────────────────────────

/// Extracts all cbuf slot accesses from WGSL source.
/// Returns a map: buffer_name → set of slot indices accessed.
///
/// WGSL storage buffers for NVN constant buffers are named like:
///   cbuf_8_1 (vertex color chain)
///   cbuf_9_1 (vertex position chain)
///   cbuf_10_1 (particle attribute buffer)
///   cbuf_16_1 (fragment color chain)
///
/// They are accessed as: `buf_name._m0_[index]` where index is the slot.
pub fn extract_cbuf_slots_from_wgsl(wgsl: &str) -> HashMap<String, HashSet<u32>> {
    let mut usage: HashMap<String, HashSet<u32>> = HashMap::new();

    for (line_idx, line) in wgsl.lines().enumerate() {
        let trimmed = line.trim();
        // Find patterns like: cbuf_9_1_._m0_[5] or _m0_[17]
        for (start, _) in trimmed.match_indices("_m0_") {
            let after = &trimmed[start + 4..]; // skip "_m0_"
            // Look for [digits] pattern
            if let Some(bracket_open) = after.find('[') {
                let from_bracket = &after[bracket_open + 1..];
                if let Some(bracket_close) = from_bracket.find(']') {
                    let num_str = &from_bracket[..bracket_close];
                    if let Ok(idx) = num_str.parse::<u32>() {
                        // Find which buffer this belongs to by looking backward
                        let before = &trimmed[..start];
                        let buf_name = extract_buffer_name(before);
                        usage.entry(buf_name).or_default().insert(idx);
                    }
                }
            }
        }
    }

    usage
}

/// Extract the buffer variable name from text before `_m0_[`
fn extract_buffer_name(before: &str) -> String {
    // Pattern: `variable_name._m0_[` or `variable_name ._m0_[`
    let trimmed = before.trim_end();
    // Walk backwards to find the start of the variable name
    let mut end = trimmed.len();
    // Skip the dot
    if trimmed.ends_with('.') {
        end -= 1;
    } else if trimmed.ends_with(" .") {
        end -= 2;
    }
    let name_end = end;
    // Walk backwards past alphanumeric/underscore characters
    let mut start = name_end;
    while start > 0 {
        let c = trimmed.as_bytes()[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    if start < name_end {
        trimmed[start..name_end].to_string()
    } else {
        "unknown".to_string()
    }
}

/// Print a summary of cbuf slot usage extracted from WGSL
pub fn print_cbuf_usage(wgsl: &str, label: &str) {
    let usage = extract_cbuf_slots_from_wgsl(wgsl);
    println!("[NVN-ANALYZE] === Cbuf usage for {label} ===");
    for (buf_name, slots) in &usage {
        let mut sorted: Vec<u32> = slots.iter().copied().collect();
        sorted.sort();
        println!("[NVN-ANALYZE]   {buf_name}: slots {sorted:?}");
    }
    if usage.is_empty() {
        println!("[NVN-ANALYZE]   (no _m0_[N] accesses found)");
    }
}

/// ── NVN Slot → Game Data Mapping ────────────────────────────────────────────
///
/// Complete mapping verified via SPIR-V/WGSL analysis of ALL cbuf slot accesses
/// across multiple BNSH shaders (particle_bnsh, Mario, Window effects).
///
/// cbuf_1 (Render flags / texture tiling):
///   Verified accesses: 0, 1
///   [0]      = Render flags bitmask (2 × u32 packed as f32, all-1 = pass all)
///   [1]      = Texture tiling density/offset (.x = scale, .y = additive bias)
///
/// cbuf_8 (Vertex Color Chain — SHARED with position chain data):
///   Verified accesses: 0,1,2,3, 6,7, 8,9,10,11, 16, 17,18
///   [0..3]   = Color table 0 first 4 entries (RGBA keyframes from EmitterDef.color0)
///              (particle FS only)
///   [6..7]   = Color chain coefficients / blend factors (interpolation weights)
///              (particle FS only)
///   [8..11]  = View-projection matrix (row-major 4×4, all shaders read this)
///   [16]     = Color table 1 first entry (first keyframe of EmitterDef.color1)
///              (particle shaders only)
///   [17..18] = Position chain parameters (model shaders read both; particle VS only reads 18)
///
/// cbuf_9 (Vertex Position Chain — most heavily accessed buffer):
///   Verified accesses: 0,1,2,3, 5, 8,9,10, 13,14,15, 17, 44,45,46,47,
///                      48,49,50,51, 53, 59,60,61,62, 68,69,70,71, 76,77,
///                      84, 92, 96,97,98,99
///   [0..3]   = NOT READ by the native VS (dead data). VP now lives in cbuf_8[8-11].
///   [5]      = Render flags bitmask (2 × u32 packed as f32, all-1 = pass all)
///   [8]      = Sprite sheet columns count (.x) + division data (.z used in frag as divisor)
///   [9]      = Animation blend weight / sprite-sheet column mixing
///   [10]     = Sprite sheet columns (redundant, for alternative animation path)
///   [13..15] = UV animation coefficients (scroll/scale combiner)
///   [17]     = Texture dimensions (.xy = width, height as f32). ONLY accessed by
///              model/mesh shaders; particle BNSH variant does NOT read this.
///   [44..47] = UV corner expansion / rotation coefficients (Phase 2 out_attr2_ chain).
///              [44].xy = alpha-modulation coeffs (0 = no alpha→UV bleeding)
///              [44].z  = rotation basis Z / gpr_13_ initial value
///              [45].x  = scale multiplier for gpr_15_
///              [45].y  = multiplier for gpr_24_/gpr_25_
///              [45].z  = alpha-accumulation coeff for gpr_0_ update
///              [46]    = camera-blending basis (.x/.y = offset accum, .z = init, .w = fma coeff)
///              [47].x  = alpha-modulation coeff for gpr_14_
///              [47].y  = center offset Y (accumulated into gpr_23_)
///              [47].z  = gpr_23_ initial value (UV center offset)
///   [48]     = Subdivision count (.z = 1.0/grid_size, .w = divisor for frag shader)
///   [49..51] = Additional subdivision / sprite layout data
///   [53]     = Secondary subdivision count (.z)
///   [59..62] = Frame interpolation pair 1: blend=(59), lo=(60), hi=(61), coeff=(62)
///              Chain: t = (life_t - lo.w) / (hi.w - lo.w),
///                     coeff.xyz = lo.xyz + (hi.xyz - lo.xyz) * t,
///                     result = blend * table[index] * coeff.xyz
///   [68..71] = Frame interpolation pair 2: lo=(68), hi=(69), coeff=(70), blend2=(71)
///              Same computation as pair 1, for color table 1.
///   [76..77] = Frame interpolation pair 3: lo=(76), hi=(77) (texture frame selector)
///   [84]     = Additional animation timing data
///   [92]     = UV tile/scroll data
///   [96..99] = Sprite sheet UV layout matrix (4 rows):
///              [96] = (0, 0, 0, tex_scale_u)    — column stride
///              [97] = (tex_scale_u, tex_scale_v, 0, 1.0) — cell size + identity
///              [98] = (0, 0, 0, tex_scale_v)    — row stride
///              [99] = (tex_scale_u, tex_scale_v, 0, 1.0) — cell size + identity
///
/// cbuf_10 (Particle Attribute Buffer):
///   Verified accesses: 0,1,2,3, 4,5,6, 8,9,10
///   [0]      = Identity quaternion .w (= 1.0, used as multiplier in frag chain)
///   [1..3]   = Identity quaternion (xyz = 0, neutral rotation basis)
///   [2.x]    = Particle life gate (VS/FS compare in_attr5_.w > cbuf_10[2].x to
///              early-cull expired particles; must be >= 1.0 for normalized life_t)
///   [4]      = UV transform matrix row U (tex_scale.x, 0, 0, tex_offset.x)
///   [5]      = UV transform matrix row V (0, tex_scale.y, 0, tex_offset.y)
///   [6]      = UV transform matrix row W (0, 0, 1, 0)
///   [8..10]  = Extra particle attribute defaults (safe identity/zero)
///
/// cbuf_16 (Fragment Color Chain):
///   Verified accesses: 1, 2, 3
///   NOTE: cbuf_16 is NOT a full copy of cbuf_8 — only 3 specific slots are read.
///   [1]      = Fragment color chain coefficient (particle FS only)
///   [2..3]   = Fragment color chain coefficients (all shaders)
///
/// ── Rendering Architecture Notes ─────────────────────────────────────────────
///
/// The NVN vertex shader natively computes gl_Position = cbuf_8[8-11] ×
/// vec4(gpr_10, gpr_13, gpr_9, 1.0), where the three gpr registers carry
/// the transformed vertex position (rotation by cbuf_10[4-6] plus a small
/// ±0.5 corner offset from gl_VertexIndex bits 0-1).
///
/// Phase 3: the vertex buffer provides particle CENTERS (in_attr0_) for every
/// corner vertex.  The native chain applies ±0.5 corner offsets from
/// gl_VertexIndex bits 0-1, scaled by per-vertex size (in_attr4_.y) and the
/// camera basis in cbuf_9[46-47], then transforms via cbuf_8[8-11] VP matrix.

/// Merge cbuf slot usage maps from multiple shader stages (VS + FS).
pub fn merge_cbuf_slot_usage(
    maps: impl IntoIterator<Item = HashMap<String, HashSet<u32>>>,
) -> HashMap<String, HashSet<u32>> {
    let mut merged: HashMap<String, HashSet<u32>> = HashMap::new();
    for map in maps {
        for (name, slots) in map {
            merged.entry(name).or_default().extend(slots);
        }
    }
    merged
}

/// Extract merged cbuf slot usage from vertex + fragment WGSL sources.
pub fn cbuf_slot_usage_from_wgsl(vs_wgsl: &str, fs_wgsl: &str) -> HashMap<String, HashSet<u32>> {
    merge_cbuf_slot_usage([
        extract_cbuf_slots_from_wgsl(vs_wgsl),
        extract_cbuf_slots_from_wgsl(fs_wgsl),
    ])
}

/// ── Phase 4a: Data-driven NVN chain evaluator ─────────────────────────────

/// Input for [`NvnChainEvaluator::evaluate`]: either WGSL sources or a precomputed slot map.
pub enum NvnSlotInput<'a> {
    Wgsl { vs: &'a str, fs: &'a str },
    Usage(&'a HashMap<String, HashSet<u32>>),
}

/// Evaluates NVN constant buffer contents from emitter + particle state,
/// writing only slots the decoded shader actually reads.
pub struct NvnChainEvaluator;

/// Parameters for one NVN chain evaluation pass.
pub struct NvnChainParams<'a> {
    pub emitter: &'a EmitterDef,
    pub life_t: f32,
    pub view_proj: &'a Mat4,
    pub tex_res: Option<&'a TextureRes>,
    pub cam_right: Vec3,
    pub cam_up: Vec3,
    pub aspect: f32,
}

impl<'a> NvnChainParams<'a> {
    pub fn new(
        emitter: &'a EmitterDef,
        life_t: f32,
        view_proj: &'a Mat4,
        tex_res: Option<&'a TextureRes>,
    ) -> Self {
        Self {
            emitter,
            life_t,
            view_proj,
            tex_res,
            cam_right: Vec3::X,
            cam_up: Vec3::Y,
            aspect: 1.0,
        }
    }

    pub fn with_camera(mut self, cam_right: Vec3, cam_up: Vec3, aspect: f32) -> Self {
        self.cam_right = cam_right;
        self.cam_up = cam_up;
        self.aspect = aspect;
        self
    }

    fn eval_context(&self) -> NvnEvalContext<'a> {
        NvnEvalContext {
            emitter: self.emitter,
            life_t: self.life_t,
            view_proj: self.view_proj,
            tex_res: self.tex_res,
            cam_right: self.cam_right,
            cam_up: self.cam_up,
            aspect: self.aspect,
        }
    }
}

/// Per-evaluation context shared by all cbuf slot fillers.
struct NvnEvalContext<'a> {
    emitter: &'a EmitterDef,
    life_t: f32,
    view_proj: &'a Mat4,
    tex_res: Option<&'a TextureRes>,
    cam_right: Vec3,
    cam_up: Vec3,
    aspect: f32,
}

impl NvnChainEvaluator {
    /// Build `NvnBufferData` for every cbuf buffer referenced in `input`.
    pub fn evaluate(
        input: NvnSlotInput<'_>,
        params: &NvnChainParams<'_>,
    ) -> HashMap<String, NvnBufferData> {
        let usage = match input {
            NvnSlotInput::Wgsl { vs, fs } => cbuf_slot_usage_from_wgsl(vs, fs),
            NvnSlotInput::Usage(map) => map.clone(),
        };
        Self::evaluate_usage(&usage, params)
    }

    /// Build `NvnBufferData` using a precomputed slot-usage map.
    pub fn evaluate_usage(
        usage: &HashMap<String, HashSet<u32>>,
        params: &NvnChainParams<'_>,
    ) -> HashMap<String, NvnBufferData> {
        let ctx = params.eval_context();
        let mut result = HashMap::new();
        for (buf_name, slots) in usage {
            if slots.is_empty() {
                continue;
            }
            let data = build_cbuf_by_name(buf_name, slots, &ctx);
            result.insert(buf_name.clone(), data);
        }
        result
    }
}

/// Sample an EA* emitter animation track at normalized time `t` (0..1).
pub fn sample_emitter_anim(anim: &EmitterAnimDef, t: f32) -> [f32; 3] {
    if !anim.enable || anim.key_frames.is_empty() {
        return [1.0, 1.0, 1.0];
    }
    let keys = &anim.key_frames;
    let t = t.clamp(0.0, 1.0);
    if t <= keys[0].time {
        return [keys[0].x, keys[0].y, keys[0].z];
    }
    let last = &keys[keys.len() - 1];
    if t >= last.time {
        return [last.x, last.y, last.z];
    }
    for i in 0..keys.len() - 1 {
        let a = &keys[i];
        let b = &keys[i + 1];
        if t >= a.time && t <= b.time {
            let range = (b.time - a.time).max(0.0001);
            let s = (t - a.time) / range;
            return [
                a.x + (b.x - a.x) * s,
                a.y + (b.y - a.y) * s,
                a.z + (b.z - a.z) * s,
            ];
        }
    }
    [1.0, 1.0, 1.0]
}

fn effective_tex_scale_uv(emitter: &EmitterDef, life_t: f32) -> [f32; 2] {
    let mut scale = emitter.tex_scale_uv;
    if let Some(anim) = &emitter.anim_tex_scale {
        if anim.enable {
            let v = sample_emitter_anim(anim, life_t);
            scale[0] *= v[0].max(0.001);
            scale[1] *= v[1].max(0.001);
        }
    }
    scale
}

fn color0_entry(emitter: &EmitterDef, index: usize, life_t: f32) -> [f32; 4] {
    let c0_len = emitter.color0.len();
    let base = if index < c0_len {
        let k = &emitter.color0[index];
        [k.r, k.g, k.b, k.a]
    } else {
        let last = if c0_len > 0 {
            &emitter.color0[c0_len - 1]
        } else {
            &ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 0.0 }
        };
        [last.r, last.g, last.b, last.a]
    };
    if let Some(anim) = &emitter.anim_color0 {
        if anim.enable {
            let ea = sample_emitter_anim(anim, life_t);
            return [base[0] * ea[0], base[1] * ea[1], base[2] * ea[2], base[3]];
        }
    }
    base
}

fn color1_entry(emitter: &EmitterDef, index: usize, life_t: f32) -> [f32; 4] {
    let c1_len = emitter.color1.len();
    let base = if index < c1_len {
        let k = &emitter.color1[index];
        [k.r, k.g, k.b, k.a]
    } else {
        let last = if c1_len > 0 {
            &emitter.color1[c1_len - 1]
        } else {
            &ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 0.0 }
        };
        [last.r, last.g, last.b, last.a]
    };
    if let Some(anim) = &emitter.anim_color1 {
        if anim.enable {
            let ea = sample_emitter_anim(anim, life_t);
            return [base[0] * ea[0], base[1] * ea[1], base[2] * ea[2], base[3]];
        }
    }
    base
}

fn alpha_samples(emitter: &EmitterDef, life_t: f32) -> (f32, f32) {
    let mut a0 = emitter.alpha0.sample(life_t);
    let mut a1 = emitter.alpha1.sample(life_t);
    if let Some(anim) = &emitter.anim_alpha {
        if anim.enable {
            let ea = sample_emitter_anim(anim, life_t);
            a0 = ea[0];
            a1 = ea[1];
        }
    }
    (a0, a1)
}

/// Final displayed particle colour (RGBA) for the native FS colour chain at normalized life.
///
/// Mirrors the CPU simulation (`ParticleSystem::step`): sample colour0/colour1 + alpha0/alpha1
/// over life, then run the emitter's colour combiner. This is the value the native fragment
/// shader reads back through `cbuf_9[60]` (RGB) / `cbuf_9[68].x` (A).
fn emitter_display_color(emitter: &EmitterDef, life_t: f32) -> [f32; 4] {
    use crate::effects::{sample_alpha, sample_color_or_white};
    use glam::Vec4;
    let t = life_t.clamp(0.0, 1.0);
    let c0 = sample_color_or_white(&emitter.color0, t);
    let c1 = if emitter.color1.is_empty() {
        Vec4::ONE
    } else {
        sample_color_or_white(&emitter.color1, t)
    };
    let a0 = if emitter.alpha0_keys.is_empty() {
        emitter.alpha0.sample(t)
    } else {
        sample_alpha(&emitter.alpha0_keys, t)
    };
    let a1 = if emitter.alpha1_keys.is_empty() {
        emitter.alpha1.sample(t)
    } else {
        sample_alpha(&emitter.alpha1_keys, t)
    };
    crate::combiner::combine_particle_rgba(
        [c0.x, c0.y, c0.z, c0.w],
        [c1.x, c1.y, c1.z, c1.w],
        a0,
        a1,
        &emitter.combiner,
    )
}

fn cbuf_base_kind(buf_name: &str) -> Option<&'static str> {
    if buf_name.starts_with("cbuf_8") {
        Some("cbuf_8")
    } else if buf_name.starts_with("cbuf_9") {
        Some("cbuf_9")
    } else if buf_name.starts_with("cbuf_10") {
        Some("cbuf_10")
    } else if buf_name.starts_with("cbuf_16") {
        Some("cbuf_16")
    } else {
        None
    }
}

/// Fill shader-requested slots that have no explicit mapping with a safe default.
fn fill_unmapped_cbuf_slots(data: &mut NvnBufferData, slots: &HashSet<u32>, default: [f32; 4]) {
    for &slot in slots {
        if !data.slot_data.contains_key(&(slot as u64)) {
            data.set(slot as u64, default);
        }
    }
}

fn build_cbuf_by_name(
    buf_name: &str,
    slots: &HashSet<u32>,
    ctx: &NvnEvalContext<'_>,
) -> NvnBufferData {
    let mut data = match cbuf_base_kind(buf_name) {
        Some("cbuf_8") => build_cbuf_8_slots(slots, ctx),
        Some("cbuf_9") => build_cbuf_9_slots(slots, ctx),
        Some("cbuf_10") => build_cbuf_10_slots(slots, ctx),
        Some("cbuf_16") => build_cbuf_16_slots(slots, ctx),
        _ => NvnBufferData::default(),
    };
    fill_unmapped_cbuf_slots(&mut data, slots, [1.0, 1.0, 1.0, 1.0]);
    data
}

fn build_cbuf_8_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let combiner_coeffs = crate::combiner::eval_combiner_gpu_coeffs(&ctx.emitter.combiner);
    let mut data = NvnBufferData::default();
    for &slot in slots {
        // NOTE: specific slots are matched BEFORE the color-table ranges (0..=15 / 16..=31),
        // otherwise the range arms shadow them (slots 6,7,8..11 fall inside 0..=15) and the
        // combiner / view-projection data is silently replaced by colour data.
        match slot {
            6 => data.set(6, combiner_coeffs.cbuf_8_slot_6),
            7 => data.set(7, combiner_coeffs.cbuf_8_slot_7),
            8..=11 => {
                let vp = ctx.view_proj.to_cols_array_2d();
                data.set(slot as u64, vp[(slot - 8) as usize]);
            }
            17 | 18 => data.set(slot as u64, [1.0, 1.0, 1.0, 1.0]),
            0..=15 => data.set(slot as u64, color0_entry(ctx.emitter, slot as usize, ctx.life_t)),
            32 => {
                let (a0, a1) = alpha_samples(ctx.emitter, ctx.life_t);
                let scale = ctx.emitter.scale_anim.sample(ctx.life_t);
                data.set(32, [a0, a1, scale, ctx.life_t]);
            }
            16..=31 => {
                let idx = (slot - 16) as usize;
                data.set(slot as u64, color1_entry(ctx.emitter, idx, ctx.life_t));
            }
            _ => {}
        }
    }
    data
}

fn build_cbuf_9_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let mut data = NvnBufferData::default();
    let ts = effective_tex_scale_uv(ctx.emitter, ctx.life_t);
    let su = ts[0].max(0.001);
    let sv = ts[1].max(0.001);
    let cols = (1.0 / su).round();
    let subdiv = cols.max(1.0);
    let frame_count = ctx.emitter.tex_pat_frame_count.max(1) as f32;
    let vp = ctx.view_proj.to_cols_array_2d();

    for &slot in slots {
        match slot {
            0..=3 => data.set(slot as u64, vp[slot as usize]),
            5 => {
                let mask: [f32; 4] = bytemuck::cast([!0u32, !0u32, 0u32, 0u32]);
                data.set(5, mask);
            }
            8 => data.set(8, [cols, 0.0, 0.0, 0.0]),
            9 => data.set(9, [1.0, 1.0, 1.0, 1.0]),
            10 => data.set(10, [0.0, cols, 0.0, 0.0]),
            13 => data.set(13, [ctx.emitter.tex_scroll_uv[0], ctx.emitter.tex_scroll_uv[1], 0.0, 0.0]),
            14 => data.set(14, [1.0, 1.0, 0.0, 0.0]),
            15 => data.set(15, [0.0, 0.0, 0.0, 0.0]),
            17 => {
                if let Some(tex) = ctx.tex_res {
                    data.set(17, [tex.width as f32, tex.height as f32, 0.0, 0.0]);
                } else {
                    data.set(17, [256.0, 256.0, 0.0, 0.0]);
                }
            }
            44 => data.set(44, [0.0, 0.0, 1.0, 1.0]),
            45 => {
                let aspect_scale = if ctx.aspect > 0.0 { 1.0 / ctx.aspect } else { 1.0 };
                data.set(45, [aspect_scale, 1.0, 0.0, 1.0]);
            }
            46 => data.set(46, [ctx.cam_right.x, ctx.cam_right.y, ctx.cam_right.z, 1.0]),
            47 => data.set(47, [0.0, ctx.cam_up.x, ctx.cam_up.y, ctx.cam_up.z]),
            48 => data.set(48, [0.0, 0.0, subdiv, subdiv]),
            53 => data.set(53, [0.0, 0.0, subdiv, 0.0]),
            // Colour-table 0 is a 3-keyframe (A=60, B=61, C=62) Hermite animation the native FS
            // evaluates by a per-particle normalised lifetime `t` (= (cbuf_10[2].x - life)/frames,
            // range ~[0,1]). Each segment is enabled once `t >= keyframe.w`; when enabled the base
            // keyframe is *subtracted out* (`fma(enable, -base, base)`) and rebuilt from the
            // segment slopes. We pre-sample the display colour on the CPU and want it emitted flat,
            // so we place every keyframe time strictly AFTER any realistic `t` (1000/1001/1002,
            // distinct to avoid 1/(tB-tA) div-by-zero). All predicates are then false, no segment
            // subtracts the base, and the output equals keyframe-A colour = the display colour.
            59 => data.set(59, [1.0, 1.0, 1.0, 1.0]),
            60 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(60, [c[0], c[1], c[2], 1000.0]);
            }
            61 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(61, [c[0], c[1], c[2], 1001.0]);
            }
            62 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(62, [c[0], c[1], c[2], 1002.0]);
            }
            // Colour-table 1 / alpha: same keyframe-spline structure (base + segments at 68/69/70,
            // 71 closing the last segment), .x carries alpha. Same "times after t" trick so the
            // flat pre-sampled alpha (keyframe-A) passes through.
            68 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(68, [c[3], c[3], c[3], 1000.0]);
            }
            69 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(69, [c[3], c[3], c[3], 1001.0]);
            }
            70 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(70, [c[3], c[3], c[3], 1002.0]);
            }
            71 => {
                let c = emitter_display_color(ctx.emitter, ctx.life_t);
                data.set(71, [c[3], c[3], c[3], 1003.0]);
            }
            76 => data.set(76, [0.0, 0.0, 0.0, 0.0]),
            77 => data.set(77, [0.0, 0.0, 0.0, frame_count]),
            // Frame-interpolation pair 3 extension (bomb VS/FS read .z/.w; .w must be non-zero)
            78 => data.set(78, [0.0, 0.0, 1.0, frame_count]),
            // World-position chain coefficients (zero .xyz = no extra offset; .w = compare sentinel)
            113 => data.set(113, [1.0, 1.0, 1.0, 1.0]),
            114 => data.set(114, [0.0, 0.0, 0.0, 1.0]),
            // Axis scales for in_attr0_.xyz — must be 1.0 so particle center reaches gl_Position
            115 => data.set(115, [1.0, 1.0, 1.0, 1.0]),
            84 => data.set(84, [0.0, 0.0, 0.0, 0.0]),
            96 => data.set(96, [0.0, 0.0, 0.0, su]),
            97 => data.set(97, [su, sv, 0.0, 1.0]),
            98 => data.set(98, [0.0, 0.0, 0.0, sv]),
            99 => data.set(99, [su, sv, 0.0, 1.0]),
            _ => {}
        }
    }
    if std::env::var("FX_DEBUG_CBUF").is_ok() {
        let mut present: Vec<u32> = slots.iter().copied().collect();
        present.sort_unstable();
        eprintln!(
            "[CBUF9] life_t={:.3} slots={present:?} col={:?} c60={:?} c61={:?} c68={:?}",
            ctx.life_t,
            emitter_display_color(ctx.emitter, ctx.life_t),
            data.slot_data.get(&60), data.slot_data.get(&61), data.slot_data.get(&68),
        );
    }
    data
}

fn build_cbuf_10_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let mut data = NvnBufferData::default();
    let ts = effective_tex_scale_uv(ctx.emitter, ctx.life_t);
    let to = ctx.emitter.tex_offset_uv;

    for &slot in slots {
        match slot {
            // Per-channel colour multiplier in the native FS (out.rgb *= cbuf_10[0].xyz,
            // out.a *= cbuf_10[0].w). Must be all-ones so the colour passes through; the VS
            // position chain that also read this slot is replaced by override_billboard_position.
            0 => data.set(0, [1.0, 1.0, 1.0, 1.0]),
            1 => data.set(1, [0.0, 1.0, 0.0, 0.0]),
            2 => data.set(2, [1.0, 0.0, 1.0, 0.0]),
            3 => data.set(3, [0.0, 0.0, 0.0, 1.0]),
            4 => data.set(4, [ts[0], 0.0, 0.0, to[0]]),
            5 => data.set(5, [0.0, ts[1], 0.0, to[1]]),
            6 => data.set(6, [0.0, 0.0, 1.0, 0.0]),
            8 => data.set(8, [1.0, 1.0, 1.0, 1.0]),
            9 => data.set(9, [to[0], to[1], 0.0, 0.0]),
            10 => data.set(10, [1.0, 1.0, 1.0, 1.0]),
            _ => {}
        }
    }
    data
}

fn build_cbuf_16_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let coeffs = crate::combiner::eval_combiner_gpu_coeffs(&ctx.emitter.combiner);
    let mut data = NvnBufferData::default();
    for &slot in slots {
        match slot {
            1 => data.set(1, coeffs.cbuf_16_slot_1),
            2 => data.set(2, coeffs.cbuf_16_slot_2),
            3 => data.set(3, coeffs.cbuf_16_slot_3),
            _ => {}
        }
    }
    data
}

/// Documented cbuf_8 slots used by legacy [`build_cbuf_8`] (full-table fill).
fn documented_cbuf_8_slots() -> HashSet<u32> {
    let mut s: HashSet<u32> = (0..=31).collect();
    s.extend([6, 7, 32, 17, 18]);
    s.extend(8..=11);
    s
}

fn documented_cbuf_9_slots() -> HashSet<u32> {
    [
        0, 1, 2, 3, 5, 8, 9, 10, 13, 14, 15, 17, 44, 45, 46, 47, 48, 53, 59, 60, 61, 62, 68, 69,
        70, 71, 76, 77, 84, 96, 97, 98, 99,
    ]
    .into_iter()
    .collect()
}

fn documented_cbuf_10_slots() -> HashSet<u32> {
    [0, 1, 2, 3, 4, 5, 6, 8, 9, 10].into_iter().collect()
}

fn documented_cbuf_16_slots() -> HashSet<u32> {
    [1, 2, 3].into_iter().collect()
}

/// ── Phase 3: Fill NVN constant buffers from actual game data ───────────────

/// Data to be written into each NVN constant buffer slot.
#[derive(Debug, Clone)]
pub struct NvnBufferData {
    /// Per-slot float4 data: index → [f32; 4]
    pub slot_data: HashMap<u64, [f32; 4]>,
}

impl NvnBufferData {
    /// Write a float4 into a specific slot
    pub fn set(&mut self, slot: u64, val: [f32; 4]) {
        self.slot_data.insert(slot, val);
    }

    /// Get the raw bytes for a contiguous range of slots [start..end)
    pub fn get_range(&self, start: u64, end: u64) -> Vec<u8> {
        let mut data = vec![0u8; ((end - start) * 16) as usize];
        for (slot, val) in &self.slot_data {
            if *slot >= start && *slot < end {
                let offset = ((slot - start) * 16) as usize;
                let bytes: [u8; 16] = bytemuck::cast(*val);
                if offset + 16 <= data.len() {
                    data[offset..offset + 16].copy_from_slice(&bytes);
                }
            }
        }
        data
    }
}

impl Default for NvnBufferData {
    fn default() -> Self {
        Self { slot_data: HashMap::new() }
    }
}

/// Build NVN constant buffer data for cbuf_8 (vertex color chain + position transform) from emitter data.
/// Fills all documented cbuf_8 slots (legacy helper — prefer [`NvnChainEvaluator`] for shader-driven fills).
pub fn build_cbuf_8(emitter: &EmitterDef, particle_life_t: f32, view_proj: &Mat4) -> NvnBufferData {
    let params = NvnChainParams::new(emitter, particle_life_t, view_proj, None);
    build_cbuf_8_slots(&documented_cbuf_8_slots(), &params.eval_context())
}

/// Build NVN constant buffer data for cbuf_9 (vertex position chain) from emitter and camera data.
/// Fills all documented cbuf_9 slots (legacy helper — prefer [`NvnChainEvaluator`] for shader-driven fills).
pub fn build_cbuf_9(
    emitter: &EmitterDef,
    view_proj: &Mat4,
    tex_res: Option<&TextureRes>,
    cam_right: Vec3,
    cam_up: Vec3,
    aspect: f32,
) -> NvnBufferData {
    let params = NvnChainParams::new(emitter, 0.0, view_proj, tex_res)
        .with_camera(cam_right, cam_up, aspect);
    build_cbuf_9_slots(&documented_cbuf_9_slots(), &params.eval_context())
}

/// Build NVN constant buffer data for cbuf_10 (particle attribute buffer) from emitter data.
/// Fills all documented cbuf_10 slots (legacy helper — prefer [`NvnChainEvaluator`] for shader-driven fills).
pub fn build_cbuf_10(emitter: &EmitterDef) -> NvnBufferData {
    let params = NvnChainParams::new(emitter, 0.0, &Mat4::IDENTITY, None);
    build_cbuf_10_slots(&documented_cbuf_10_slots(), &params.eval_context())
}

/// Build NVN constant buffer data for cbuf_16 (fragment color chain) from emitter data.
/// Fills all documented cbuf_16 slots (legacy helper — prefer [`NvnChainEvaluator`] for shader-driven fills).
pub fn build_cbuf_16(_emitter: &EmitterDef, _particle_life_t: f32) -> NvnBufferData {
    let params = NvnChainParams::new(_emitter, _particle_life_t, &Mat4::IDENTITY, None);
    build_cbuf_16_slots(&documented_cbuf_16_slots(), &params.eval_context())
}

/// Write a complete NvnBufferData into a GPU buffer at the appropriate offsets.
/// Replaces the current hardcoded identity/white values with actual game data.
pub fn write_nvn_buffer(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    data: &NvnBufferData,
    buffers_total_size: u64,
) {
    // Write each slot at its 16-byte-aligned offset
    for (slot, val) in &data.slot_data {
        let offset = slot * 16;
        if offset + 16 <= buffers_total_size {
            queue.write_buffer(buffer, offset, bytemuck::bytes_of(val));
        }
    }
}

/// Fill the "general purpose" slots not covered by specific data
/// with [1,1,1,1] white (safe default for color chains).
pub fn fill_identity_slots(data: &mut NvnBufferData, range_start: u64, range_end: u64, skip_slots: &[u64]) {
    let skip_set: HashSet<u64> = skip_slots.iter().copied().collect();
    for slot in range_start..range_end {
        if !data.slot_data.contains_key(&slot) && !skip_set.contains(&slot) {
            data.set(slot, [1.0, 1.0, 1.0, 1.0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_cbuf_slots() {
        let wgsl = r#"
            let temp = cbuf_9_1_._m0_[0];
            let temp2 = cbuf_9_1_._m0_[5];
            let temp3 = cbuf_8_1_._m0_[17];
            let temp4 = cbuf_10_1_._m0_[4];
            let val = cbuf_16_1_._m0_[64];
        "#;
        let usage = extract_cbuf_slots_from_wgsl(wgsl);
        assert_eq!(usage.get("cbuf_9_1_").map(|s| s.len()).unwrap_or(0), 2);
        assert_eq!(usage.get("cbuf_8_1_").map(|s| s.len()).unwrap_or(0), 1);
        assert_eq!(usage.get("cbuf_10_1_").map(|s| s.len()).unwrap_or(0), 1);
        assert_eq!(usage.get("cbuf_16_1_").map(|s| s.len()).unwrap_or(0), 1);
    }

    #[test]
    fn test_extract_buffer_name() {
        assert_eq!(extract_buffer_name("cbuf_9_1_"), "cbuf_9_1_");
        assert_eq!(extract_buffer_name("  cbuf_8_1_ "), "cbuf_8_1_");
        assert_eq!(extract_buffer_name("some_buf"), "some_buf");
    }

    #[test]
    fn test_build_cbuf_8_empty_emitter() {
        let emitter = crate::effects::EmitterDef::default();
        let vp = Mat4::IDENTITY;
        let data = build_cbuf_8(&emitter, 0.5, &vp);
        assert!(data.slot_data.contains_key(&0), "should have slot 0");
        assert!(data.slot_data.contains_key(&15), "should have slot 15");
    }

    #[test]
    fn test_build_cbuf_9_with_tex() {
        let emitter = crate::effects::EmitterDef::default();
        let view_proj = Mat4::IDENTITY;
        let tex_res = TextureRes {
            width: 128, height: 64,
            ..Default::default()
        };
        let data = build_cbuf_9(
            &emitter,
            &view_proj,
            Some(&tex_res),
            Vec3::X,
            Vec3::Y,
            2.0,
        );
        let slot17 = data.slot_data.get(&17);
        assert!(slot17.is_some(), "slot 17 should exist");
        if let Some(val) = slot17 {
            assert_eq!(val[0], 128.0, "width should be 128");
            assert_eq!(val[1], 64.0, "height should be 64");
        }
    }

    #[test]
    fn test_fill_identity() {
        let mut data = NvnBufferData::default();
        data.set(5, [1.0, 2.0, 3.0, 4.0]); // user data at slot 5
        fill_identity_slots(&mut data, 0, 10, &[5]);
        for slot in 0u64..10 {
            assert!(data.slot_data.contains_key(&slot), "slot {slot} should be filled");
        }
    }

    #[test]
    fn test_combiner_coeffs_in_cbuf_8_and_16() {
        let mut emitter = EmitterDef::default();
        emitter.combiner.texture1_color_blend = 1;
        emitter.combiner.color_combiner_process = 1;
        emitter.combiner.shader_type = 1;

        let mut usage = HashMap::new();
        usage.insert("cbuf_8_1_".to_string(), [6u32, 7].into_iter().collect());
        usage.insert("cbuf_16_1_".to_string(), [1u32, 2, 3].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);

        let c8 = result.get("cbuf_8_1_").unwrap();
        assert!(c8.slot_data.contains_key(&6));
        assert!(c8.slot_data.contains_key(&7));

        let c16 = result.get("cbuf_16_1_").unwrap();
        let slot1 = c16.slot_data.get(&1).unwrap();
        assert_eq!(slot1[3], 0.0, "add blend uses w=0");
    }

    #[test]
    fn test_cbuf_9_slot115_scales_particle_position() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [115u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot115 = result.get("cbuf_9_1_").unwrap().slot_data.get(&115).unwrap();
        assert_eq!(slot115[0], 1.0);
        assert_eq!(slot115[1], 1.0);
        assert_eq!(slot115[2], 1.0);
    }

    #[test]
    fn test_cbuf_10_slot2_life_gate_does_not_cull_alive_particles() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [2u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot2 = result.get("cbuf_10_1_").unwrap().slot_data.get(&2).unwrap();
        assert_eq!(
            slot2[0], 1.0,
            "life gate threshold must be 1.0 so normalized life_t in 0..1 is not culled"
        );
    }

    #[test]
    fn test_evaluator_only_fills_requested_slots() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_8_1_".to_string(), [8u32, 9].into_iter().collect());
        usage.insert("cbuf_16_1_".to_string(), [2u32].into_iter().collect());

        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);

        let c8 = result.get("cbuf_8_1_").expect("cbuf_8");
        assert_eq!(c8.slot_data.len(), 2);
        assert!(c8.slot_data.contains_key(&8));
        assert!(c8.slot_data.contains_key(&9));
        assert!(!c8.slot_data.contains_key(&0));

        let c16 = result.get("cbuf_16_1_").expect("cbuf_16");
        assert_eq!(c16.slot_data.len(), 1);
        assert!(c16.slot_data.contains_key(&2));
    }

    #[test]
    fn test_ea_anim_color_modulates_cbuf_8() {
        let mut emitter = EmitterDef::default();
        emitter.color0 = vec![ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }];
        emitter.anim_color0 = Some(EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                crate::effects::AnimKeyframe { x: 0.5, y: 0.25, z: 0.75, time: 0.0 },
                crate::effects::AnimKeyframe { x: 0.5, y: 0.25, z: 0.75, time: 1.0 },
            ],
        });

        let mut usage = HashMap::new();
        usage.insert("cbuf_8_1_".to_string(), [0u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot0 = result.get("cbuf_8_1_").unwrap().slot_data.get(&0).unwrap();
        assert!((slot0[0] - 0.5).abs() < 0.001);
        assert!((slot0[1] - 0.25).abs() < 0.001);
        assert!((slot0[2] - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_ea_anim_tex_scale_modulates_cbuf_9() {
        let mut emitter = EmitterDef::default();
        emitter.tex_scale_uv = [0.5, 0.25];
        emitter.anim_tex_scale = Some(EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                crate::effects::AnimKeyframe { x: 2.0, y: 4.0, z: 0.0, time: 0.0 },
                crate::effects::AnimKeyframe { x: 2.0, y: 4.0, z: 0.0, time: 1.0 },
            ],
        });

        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [97u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.0, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot97 = result.get("cbuf_9_1_").unwrap().slot_data.get(&97).unwrap();
        assert!((slot97[0] - 1.0).abs() < 0.001, "su = 0.5 * 2.0");
        assert!((slot97[1] - 1.0).abs() < 0.001, "sv = 0.25 * 4.0");
    }

    #[test]
    fn test_ea_anim_alpha_in_cbuf_8_slot_32() {
        let mut emitter = EmitterDef::default();
        emitter.anim_alpha = Some(EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                crate::effects::AnimKeyframe { x: 0.3, y: 0.7, z: 0.0, time: 0.0 },
                crate::effects::AnimKeyframe { x: 0.3, y: 0.7, z: 0.0, time: 1.0 },
            ],
        });

        let mut usage = HashMap::new();
        usage.insert("cbuf_8_1_".to_string(), [32u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot32 = result.get("cbuf_8_1_").unwrap().slot_data.get(&32).unwrap();
        assert!((slot32[0] - 0.3).abs() < 0.001);
        assert!((slot32[1] - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_cbuf_slot_usage_from_wgsl_merges_stages() {
        let vs = "let a = cbuf_9_1_._m0_[5];";
        let fs = "let b = cbuf_8_1_._m0_[8]; let c = cbuf_8_1_._m0_[11];";
        let usage = cbuf_slot_usage_from_wgsl(vs, fs);
        assert!(usage.get("cbuf_9_1_").unwrap().contains(&5));
        assert!(usage.get("cbuf_8_1_").unwrap().contains(&8));
        assert!(usage.get("cbuf_8_1_").unwrap().contains(&11));
    }

    #[test]
    fn test_sample_emitter_anim_interpolates() {
        let anim = EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                crate::effects::AnimKeyframe { x: 0.0, y: 0.0, z: 0.0, time: 0.0 },
                crate::effects::AnimKeyframe { x: 1.0, y: 2.0, z: 3.0, time: 1.0 },
            ],
        };
        let mid = sample_emitter_anim(&anim, 0.5);
        assert!((mid[0] - 0.5).abs() < 0.001);
        assert!((mid[1] - 1.0).abs() < 0.001);
        assert!((mid[2] - 1.5).abs() < 0.001);
    }

    /// Phase 0: Analyze ALL BNSH WGSL files and dump every cbuf slot access.
    /// Cross-references against the known mapping and flags unknown slots.
    #[test]
    fn test_phase0_analyze_all_shaders() {
        let wgsl_dir = std::path::Path::new("/tmp");
        let patterns = &[
            "hitbox_particle_bnsh_vs.wgsl",
            "hitbox_particle_bnsh_fs.wgsl",
            "bnsh_vs.wgsl",
            "bnsh_fs.wgsl",
        ];

        // Known slot mapping from the code comments at lines 97-158
        // (cbuf_name → HashSet of known slots)
        use std::collections::HashMap;
        let known: HashMap<&str, std::collections::HashSet<u32>> = [
            // cbuf_8 (vertex color chain + position data)
            ("cbuf_8_1_", vec![0u32,1,2,3,6,7,8,9,10,11,16,17,18]),
            // cbuf_9 (vertex position chain)
            ("cbuf_9_1_", vec![0,1,2,3,5,8,9,10,13,14,15,17,
                               44,45,46,47,48,49,50,51,53,
                               59,60,61,62,68,69,70,71,76,77,
                               84,92,96,97,98,99]),
            // cbuf_10 (particle attribute buffer)
            ("cbuf_10_1_", vec![0,1,2,3,4,5,6,8,9,10]),
            // cbuf_16 (fragment color chain)
            ("cbuf_16_1_", vec![1,2,3,16]),
            // cbuf_1 (render flags)
            ("cbuf_1_1_", vec![0,1]),
        ].into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect();

        let mut all_ok = true;
        for &fname in patterns {
            let path = wgsl_dir.join(fname);
            if !path.exists() {
                eprintln!("[PHASE0] SKIP {fname} — not found");
                continue;
            }
            let wgsl = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("read {fname}"));
            let usage = crate::nvn_chain::extract_cbuf_slots_from_wgsl(&wgsl);

            let line_count = wgsl.lines().count();
            eprintln!("\n[PHASE0] === {fname} ({line_count} lines) ===");
            for buf in ["cbuf_1_1_", "cbuf_8_1_", "cbuf_9_1_", "cbuf_10_1_", "cbuf_16_1_"] {
                let actual: Vec<u32> = usage.get(buf)
                    .map(|s| { let mut v: Vec<u32> = s.iter().copied().collect(); v.sort(); v })
                    .unwrap_or_default();
                let known_slots: Vec<u32> = known.get(buf)
                    .map(|s| { let mut v: Vec<u32> = s.iter().copied().collect(); v.sort(); v })
                    .unwrap_or_default();

                if actual.is_empty() {
                    continue;
                }
                let unknown: Vec<u32> = actual.iter()
                    .filter(|s| !known_slots.contains(s))
                    .copied().collect();

                let missing: Vec<u32> = known_slots.iter()
                    .filter(|s| !actual.contains(s))
                    .copied().collect();

                eprintln!("  {buf}: slots {actual:?}");
                if !unknown.is_empty() {
                    eprintln!("    ⚠ UNKNOWN slots: {unknown:?}");
                    all_ok = false;
                }
                if !missing.is_empty() {
                    eprintln!("    ⓘ Documented but not accessed in this shader: {missing:?}");
                }
            }
        }

        // If any unknown slots were found, fail the test with details
        assert!(all_ok, "Unknown cbuf slots found — see stderr for details");
    }
}
