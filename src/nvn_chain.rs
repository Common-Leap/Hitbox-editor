// NVN register chain emulation: analyzes BNSH shaders for cbuf slot usage,
// maps each slot to its game data source, and generates NVN constant buffer
// data from actual PTCL emitter parameters.
//
// The NVN position/color/UV chains are fixed-function GPU stages that run
// before the shader. On Vulkan we emulate them by (1) computing the chain
// outputs on CPU and feeding them as vertex attributes, and (2) filling the
// NVN constant buffers (cbuf_8/9/10/16) from game data instead of identity.

use crate::effects::{ColorKey, DisplaySide, EmitterAnimDef, EmitterDef, TextureAnimFlags, TextureRes};
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};

/// ── Phase 0: SPIR-V / WGSL cbuf slot analyzer ──────────────────────────────

/// SPIR-V opcodes used for cbuf access reflection.
const OP_NAME: u32 = 5;
const OP_TYPE_INT: u32 = 21;
const OP_CONSTANT: u32 = 43;
const OP_ACCESS_CHAIN: u32 = 65;
const OP_IN_BOUNDS_ACCESS_CHAIN: u32 = 66;

/// True when an OpName names an NVN constant-buffer SSBO/UBO.
fn is_cbuf_spirv_name(name: &str) -> bool {
    name.starts_with("cbuf_1")
        || name.starts_with("cbuf_8")
        || name.starts_with("cbuf_9")
        || name.starts_with("cbuf_10")
        || name.starts_with("cbuf_16")
}

/// Map SPIR-V access-chain indices to the `_m0_[N]` slot index.
///
/// NVN cbufs are a struct whose member 0 (`_m0_`) is a runtime array of vec4 slots.
fn cbuf_slot_from_access_indices(indices: &[u32]) -> Option<u32> {
    if indices.is_empty() {
        return None;
    }
    if indices[0] == 0 && indices.len() >= 2 {
        Some(indices[1])
    } else {
        Some(indices[0])
    }
}

/// Resolve an access-chain index operand (constant id or literal).
fn resolve_spirv_index(
    operand: u32,
    constants: &std::collections::HashMap<u32, u32>,
) -> Option<u32> {
    constants.get(&operand).copied().or(Some(operand))
}

/// Decode a null-terminated string embedded in SPIR-V instruction literal words.
fn decode_spirv_name_words(name_words: &[u32]) -> String {
    let mut bytes = Vec::new();
    for &w in name_words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn record_cbuf_spirv_access(
    names: &HashMap<u32, String>,
    usage: &mut HashMap<String, HashSet<u32>>,
    root_var: u32,
    indices: &[u32],
) {
    let Some(name) = names.get(&root_var) else {
        return;
    };
    if !is_cbuf_spirv_name(name) {
        return;
    }
    let Some(slot) = cbuf_slot_from_access_indices(indices) else {
        return;
    };
    usage.entry(name.clone()).or_default().insert(slot);
}

/// Extract cbuf `_m0_[N]` slot accesses by walking SPIR-V `OpAccessChain` instructions.
///
/// Pure Rust reflection over SPIR-V bytecode (same information SPIRV-Reflect exposes for
/// SSBO member access). Returns an empty map when parsing fails or no cbuf accesses exist.
pub fn extract_cbuf_slots_from_spirv(spirv: &[u8]) -> HashMap<String, HashSet<u32>> {
    let Ok(words) = crate::spirv_to_wgsl::bytes_to_words(spirv) else {
        return HashMap::new();
    };
    if words.first() != Some(&0x0723_0203) {
        return HashMap::new();
    }

    let mut names: HashMap<u32, String> = HashMap::new();
    let mut constants: HashMap<u32, u32> = HashMap::new();
    let mut access_roots: HashMap<u32, (u32, Vec<u32>)> = HashMap::new();
    let mut usage: HashMap<String, HashSet<u32>> = HashMap::new();

    let mut i = 5usize;
    while i < words.len() {
        let word = words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xffff;
        if word_count == 0 || i + word_count > words.len() {
            break;
        }

        match opcode {
            OP_NAME if word_count >= 3 => {
                let target = words[i + 1];
                let name = decode_spirv_name_words(&words[i + 2..i + word_count]);
                if !name.is_empty() {
                    names.insert(target, name);
                }
            }
            OP_CONSTANT if word_count >= 4 => {
                constants.insert(words[i + 2], words[i + word_count - 1]);
            }
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN if word_count >= 5 => {
                let base = words[i + 3];
                let index_ops = &words[i + 4..i + word_count];
                let mut indices = Vec::with_capacity(index_ops.len());
                for &op in index_ops {
                    if let Some(idx) = resolve_spirv_index(op, &constants) {
                        indices.push(idx);
                    }
                }

                let (root_var, mut chain) = if let Some((root, prefix)) = access_roots.get(&base) {
                    (*root, prefix.clone())
                } else {
                    (base, Vec::new())
                };
                chain.extend(indices);
                access_roots.insert(words[i + 2], (root_var, chain.clone()));
                record_cbuf_spirv_access(&names, &mut usage, root_var, &chain);
            }
            _ => {}
        }

        i += word_count;
    }

    usage
}

/// Merge SPIR-V reflection (preferred) with WGSL regex fallback.
pub fn cbuf_slot_usage_from_shaders(
    vs_spirv: Option<&[u8]>,
    fs_spirv: Option<&[u8]>,
    vs_wgsl: &str,
    fs_wgsl: &str,
) -> HashMap<String, HashSet<u32>> {
    let spirv_usage = merge_cbuf_slot_usage([
        vs_spirv
            .map(extract_cbuf_slots_from_spirv)
            .unwrap_or_default(),
        fs_spirv
            .map(extract_cbuf_slots_from_spirv)
            .unwrap_or_default(),
    ]);
    if spirv_usage.values().any(|slots| !slots.is_empty()) {
        let mut usage = spirv_usage;
        supplement_hybrid_finalize_slots(&mut usage, vs_wgsl);
        supplement_family_a_cbuf8_position_chain_slots(&mut usage, vs_wgsl);
        supplement_cbuf9_dynamic_subdiv_slots(&mut usage, vs_wgsl, fs_wgsl);
        // Patched hybrid billboard code reads VP/basis slots that SPIR-V alone may not list.
        if vs_wgsl.contains("_vp0 * _world") || vs_wgsl.contains("gl_Position = _vp0 *") {
            for (name, slots) in extract_cbuf_slots_from_wgsl(vs_wgsl) {
                usage.entry(name).or_default().extend(slots);
            }
        }
        return usage;
    }
    cbuf_slot_usage_from_wgsl(vs_wgsl, fs_wgsl)
}

/// Map naga reflection names (`cbuf_8`) and WGSL globals (`cbuf_8_1_`) to a cbuffer family.
pub(crate) fn cbuf_descriptor_family(buf_name: &str) -> Option<u8> {
    match buf_name.trim_end_matches('_') {
        "cbuf_8" | "cbuf_8_1" => Some(8),
        "cbuf_9" | "cbuf_9_1" => Some(9),
        "cbuf_10" | "cbuf_10_1" => Some(10),
        "cbuf_16" | "cbuf_16_1" => Some(16),
        "cbuf_1" | "cbuf_1_1" => Some(1),
        _ => None,
    }
}

/// Optional flipbook atlas scale for [`force_hybrid_billboard_cbuf_defaults`].
pub struct FlipbookAtlasCbuf<'a> {
    pub emitter: &'a EmitterDef,
    pub life_t: f32,
    /// Batch-averaged per-particle tile scale when IsScale animates UV scale.
    pub batch_tex_scale: Option<[f32; 2]>,
}

/// Fill VP / life-gate slots required by the hybrid billboard override when evaluate missed them.
pub fn force_hybrid_billboard_cbuf_defaults(
    data: &mut NvnBufferData,
    buf_name: &str,
    view_proj: &Mat4,
    cam_right: Vec3,
    cam_up: Vec3,
    flipbook: Option<FlipbookAtlasCbuf<'_>>,
) {
    match cbuf_descriptor_family(buf_name) {
        Some(8) => {
            let vp = view_proj.to_cols_array_2d();
            for slot in 8..=11u64 {
                data.set(slot, vp[(slot - 8) as usize]);
            }
            // Family-A billboards: CPU attr0 is world-space; pre-transform [0..3]/[12..14] stay identity.
            for slot in 0..=3u64 {
                if !data.slot_data.contains_key(&slot) {
                    data.set(slot, cbuf_8_identity_pretransform_column(slot as u32));
                }
            }
            for slot in 12..=14u64 {
                if !data.slot_data.contains_key(&slot) {
                    data.set(slot, cbuf_8_identity_pretransform_column((slot - 12) as u32));
                }
            }
        }
        Some(9) => {
            let vp = view_proj.to_cols_array_2d();
            for slot in 0..=3u64 {
                if !data.slot_data.contains_key(&slot) {
                    data.set(slot, vp[slot as usize]);
                }
            }
            if !data.slot_data.contains_key(&120) {
                data.set(120, [cam_right.x, cam_right.y, cam_right.z, 1.0]);
            }
            if !data.slot_data.contains_key(&121) {
                data.set(121, [0.0, cam_up.x, cam_up.y, cam_up.z]);
            }
            // FS alpha gate: discard when gpr_5 <= cbuf_9[94].z — unset GPU memory reads as 0 and
            // culls almost all fragments when the NVN chain briefly hits zero.
            if !data.slot_data.contains_key(&94) {
                data.set(94, [0.0, 0.0, -1.0e6, 0.0]);
            }
            if let Some(fb) = flipbook {
                let needs_atlas = crate::effects::emitter_uses_tex_pattern(fb.emitter)
                    || fb.emitter.tex_pat_frame_count > 1
                    || fb.batch_tex_scale.is_some()
                    || (fb.emitter.tex_scale_uv[0] - 1.0).abs() > 0.001
                    || (fb.emitter.tex_scale_uv[1] - 1.0).abs() > 0.001;
                let ts = if needs_atlas {
                    fb.batch_tex_scale
                        .map(|b| [b[0].abs().max(0.001), b[1].abs().max(0.001)])
                        .unwrap_or_else(|| atlas_tile_scale_uv(fb.emitter, fb.life_t))
                } else {
                    [1.0, 1.0]
                };
                if !data.slot_data.contains_key(&127) {
                    data.set(127, [ts[0], ts[1], 0.0, 1.0]);
                }
            }
        }
        Some(10) => {
            if !data.slot_data.contains_key(&0) {
                data.set(0, [1.0, 1.0, 1.0, 1.0]);
            }
            if !data.slot_data.contains_key(&1) {
                // Whole row 1 like the game — .w=0 zeroed out_attr1.w (FS discard gate).
                data.set(1, [1.0, 1.0, 1.0, 1.0]);
            }
            if !data.slot_data.contains_key(&2) {
                // cbuf_10[2].x = emitter clock (frames). The native VS/FS age chain reads
                // age = clock - attr<birth>.w; the vertex builder feeds birth = clock - age
                // with the same CLOCK when the frame-clock feed is on (task #22). With the
                // legacy normalized feed leave x = 1.0. Capture: .yzw carry 1s.
                let clock = if crate::fx_env::fx_frame_clock_enabled() {
                    EMITTER_CLOCK_FRAMES
                } else {
                    1.0
                };
                data.set(2, [clock, 1.0, 1.0, 1.0]);
            }
            if !data.slot_data.contains_key(&3) {
                data.set(3, [1.0, 1.0, 1.0, 1.0]);
            }
            // Slots [8] and [10] are neutral-multiply constants the native VS folds into the
            // billboard geometry (like [1]/[3]); a stale 0 collapses every billboard to a point.
            // These are VS-only reads, so a renderer whose cbuf_10 usage scan omits the VS slots
            // (e.g. the live viewport's lazily-built pipeline) never fills them via
            // build_cbuf_10_slots — force them here so both paths agree. [9] is the emitter's
            // indirect/tex2 UV offset (neutral 0 when unset).
            if !data.slot_data.contains_key(&8) {
                data.set(8, [1.0, 1.0, 1.0, 1.0]);
            }
            if !data.slot_data.contains_key(&9) {
                let uv = flipbook
                    .as_ref()
                    .map(|fb| {
                        [
                            fb.emitter.indirect_tex_offset_uv[0],
                            fb.emitter.indirect_tex_offset_uv[1],
                            fb.emitter.tex2_offset_uv[0],
                            fb.emitter.tex2_offset_uv[1],
                        ]
                    })
                    .unwrap_or([0.0, 0.0, 0.0, 0.0]);
                data.set(9, uv);
            }
            if !data.slot_data.contains_key(&10) {
                data.set(10, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        Some(16) => {
            if !data.slot_data.contains_key(&4) {
                data.set(4, crate::combiner::fs_cbuf_16_slot_4());
            }
        }
        _ => {}
    }
}

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
///   Verified accesses: 0,1,2,3, 6,7, 8,9,10,11, 12,13,14, 16, 17,18
///   [0..3]   = Color table 0 (used by some FS variants; particle BNSH Family A often
///              reads cbuf_9 colour splines instead — do NOT conflate with [12..14])
///   [6..7]   = Color chain coefficients / blend factors (interpolation weights)
///   [8..11]  = View-projection matrix (row-major 4×4, all particle VS read this)
///   [12..14] = 3×4 world-position transform rows (NOT colour keyframes — must be
///              identity/pass-through rows before the VP multiply at [8..11])
///   [16]     = Color table 1 first entry (first keyframe of EmitterDef.color1)
///   [17..18] = Position chain parameters (model shaders read both; particle VS only reads 18)
///
/// cbuf_9 (Vertex Position Chain — most heavily accessed buffer):
///   Verified accesses: 0,1,2,3, 5, 8,9,10, 13,14,15, 17, 44,45,46,47,
///                      48,49,50,51, 53, 59,60,61,62, 68,69,70,71, 76,77,
///                      84, 92, 96,97,98,99
///   [0..3]   = View-projection matrix in Family-B particle VS (legacy); Family-A uses cbuf_8[8..11].
///   [5]      = Render flags bitmask in late VS (bitcast<i32> AND). Family-A VS also reads
///              `.x` as a float scale early in the position chain (~L589) — must be 1.0, not
///              bitcast(!0u32) which is NaN and poisons gpr_17_ before the VP multiply.
///   [8]      = Sprite sheet columns count (.x) + division data (.z used in frag as divisor)
///   [9]      = Animation blend weight / sprite-sheet column mixing
///   [10]     = Sprite sheet columns (redundant, for alternative animation path)
///   [13..15] = UV animation coefficients (scroll/scale combiner)
///   [17]     = Texture dimensions (.xy = width, height as f32). ONLY accessed by
///              model/mesh shaders; particle BNSH variant does NOT read this.
///   [44..47] = TexScrollAnim0 UV transform (capture-verified against authored data):
///              [44].xy = scroll speeds (ScrollAddX/Y-ish; small per-frame values)
///              [45]    = (1,1,0,0)-style enable/multiplier pair
///              [46].xy = ScaleX/ScaleY — the authored UV tiling scale, VERBATIM
///                        (ring1_sub (3.0,1.2), flare2 (7.0,1.0) match exactly)
///              [47].y  = Rotation in radians (π/2 observed)
///              CONTESTED: our hybrid billboard override repurposes [46]/[47] as camera
///              right/up (`force_hybrid_billboard_cbuf_defaults`) — the game semantics
///              cannot be filled until the override's basis moves elsewhere (task #15).
///   [48]     = Subdivision count (.z = 1.0/grid_size, .w = divisor for frag shader)
///   [49..51] = Additional subdivision / sprite layout data
///   [53]     = Secondary subdivision count (.z)
///   [59]     = Combiner blend coefficient
///   [60..67] = Colour0 keyframe table: raw authored keys [r,g,b,t], forward life axis,
///              pads = last value @ idx+last_t. The microcode is unrolled for the key
///              count and lerps between adjacent entries (see `nvn_color_table0_entries`).
///   [68..75] = Alpha keyframe table, same layout with [a,a,a,w] entries
///              (see `nvn_alpha_table1_entries`).
///   [76..77] = Frame interpolation pair 3: lo=(76), hi=(77) (texture frame selector)
///   [84]     = Additional animation timing data
///   [92]     = UV tile/scroll data (slot1 indirect .xy, slot2 tex2 .zw)
///   [96..103]= GAME: particle scale keyframe table [s,s,s,t] (raw scale_keys, forward
///              axis, pads idx+last_t — ring1_sub (0.95@0, 0.95@0.2, 0.58@1.0) matches
///              its 3 authored scale keys; smoke1_normal grows 0.92→1.38). OUR fills
///              still use the legacy sprite-layout convention consumed by the patched
///              flipbook paths ([96]=(0,0,0,su), [97]=(su,sv,0,1), [98]=(0,0,0,sv),
///              [99]=(su,sv,0,1)) — migrating requires the same override rework as
///              [46]/[47] (task #15).
///   [100]    = TextureAnim3–4 scroll UV (.xy = slot3, .zw = slot4) — mirrors [92] pattern
///   [101]    = TextureAnim5 scroll UV (.xy = slot5)
///
/// cbuf_10 (Particle Attribute Buffer):
///   Verified accesses: 0,1,2,3, 4,5,6, 8,9,10
///   [0]      = Per-channel scale (.xyz) and alpha scale (.w) — must be all-ones so
///              colour and position chain multiplies pass through unchanged
///   [1]      = Neutral multiply (.xyz = 1) — VS scales gpr registers by these components
///   [2]      = Life gate threshold in .x (in_attr5_.w > cbuf_10[2].x early-culls)
///   [3]      = Rotation-chain coefficients (.x→out_attr3, .y→gpr_20 scale, .z/.w multiplied
///              into corner rotation — all components must be 1.0 for neutral pass-through)
///   [4]      = UV transform matrix row U (tex_scale.x, 0, 0, tex_offset.x)
///   [5]      = UV transform matrix row V (0, tex_scale.y, 0, tex_offset.y)
///   [6]      = UV transform matrix row W (0, 0, 1, 0)
///   [8..10]  = Extra particle attribute defaults (safe identity/zero)
///   [11]     = TextureAnim3–4 per-draw UV offset (.xy = slot3, .zw = slot4) — mirrors [9]
///   [12]     = TextureAnim5 offset (.xy) + slot3 scale (.zw) when combiner reads extra slots
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
/// the transformed vertex position (rotation by cbuf_10[4-6] plus corner offsets
/// from in_attr6_/in_attr7_ scaled by in_attr4_.y).
///
/// Phase 3: the vertex buffer provides particle CENTERS (in_attr0_) for every
/// corner vertex.  Family-A BNSH shaders read ±0.5 corner seeds from in_attr6_.xy
/// / in_attr7_.xyz (not gl_VertexIndex bits), apply sin/cos rotation via
/// cbuf_10[3-6], then transform via cbuf_8[12-14] and cbuf_8[8-11] VP matrix.

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
    let mut usage = merge_cbuf_slot_usage([
        extract_cbuf_slots_from_wgsl(vs_wgsl),
        extract_cbuf_slots_from_wgsl(fs_wgsl),
    ]);
    supplement_hybrid_finalize_slots(&mut usage, vs_wgsl);
    supplement_family_a_cbuf8_position_chain_slots(&mut usage, vs_wgsl);
    supplement_cbuf9_dynamic_subdiv_slots(&mut usage, vs_wgsl, fs_wgsl);
    usage
}

/// Ensure VP and camera-basis cbuf slots are filled whenever the VS uses the hybrid
/// billboard finalize/override path (including when the decoded chain looks "trusted"
/// but we still replace gl_Position after main_1()).
/// True when WGSL indexes cbuf_9 with a non-constant slot (SPIR-V reflection misses these).
pub fn wgsl_has_dynamic_cbuf9_slot_index(wgsl: &str) -> bool {
    for line in wgsl.lines() {
        if !line.contains("cbuf_9") {
            continue;
        }
        let mut rest = line;
        while let Some(idx) = rest.find("_m0_[") {
            let after = &rest[idx + 5..];
            if let Some(close) = after.find(']') {
                let index_expr = after[..close].trim();
                if !index_expr.chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
            rest = &rest[idx + 5..];
        }
    }
    false
}

/// Ensure subdivision layout slots are evaluated when the VS dynamically indexes `_m0_[48+N]`.
pub fn supplement_cbuf9_dynamic_subdiv_slots(
    usage: &mut HashMap<String, HashSet<u32>>,
    vs_wgsl: &str,
    fs_wgsl: &str,
) {
    let dynamic = wgsl_has_dynamic_cbuf9_slot_index(vs_wgsl)
        || wgsl_has_dynamic_cbuf9_slot_index(fs_wgsl);
    let corner_path = vs_wgsl.contains("in_attr6_1") && vs_wgsl.contains("in_attr7_1");
    let subdiv_static = usage
        .get("cbuf_9_1_")
        .is_some_and(|s| s.contains(&48) || s.contains(&53));
    if dynamic || (corner_path && subdiv_static) {
        usage
            .entry("cbuf_9_1_".to_string())
            .or_default()
            .extend(48..=51);
    }
}

pub fn supplement_hybrid_finalize_slots(
    usage: &mut HashMap<String, HashSet<u32>>,
    vs_wgsl: &str,
) {
    let billboard = crate::spirv_to_wgsl::billboard_particle_vs(vs_wgsl);
    let hybrid_finalize = vs_wgsl.contains("gl_Position = _vp0 *")
        || vs_wgsl.contains("_vp0 * _world.x");
    if crate::spirv_to_wgsl::is_partial_family_b_billboard_vs(vs_wgsl) || billboard {
        usage
            .entry("cbuf_9_1_".to_string())
            .or_default()
            .extend([46u32, 47]);
    }
    if hybrid_finalize {
        // Hybrid override may still read camera basis from cbuf_9[46]/[47] when the decoded
        // shader references slot 46 (common on Family-A Samus-style VS that also touch cbuf_8[17]).
        if vs_wgsl.contains("cbuf_9_1_._m0_[46]")
            || vs_wgsl.contains("in_attr7_1")
        {
            usage
                .entry("cbuf_9_1_".to_string())
                .or_default()
                .extend([46u32, 47]);
        }
        if crate::spirv_to_wgsl::uses_cbuf8_vp(vs_wgsl) {
            supplement_family_a_cbuf8_position_chain_slots(usage, vs_wgsl);
        } else if crate::spirv_to_wgsl::uses_cbuf9_vp(vs_wgsl) {
            usage
                .entry("cbuf_9_1_".to_string())
                .or_default()
                .extend(0..=3);
        }
    }
}

/// Ensure Family-A particle billboards evaluate the full cbuf_8 pre-transform + VP block.
///
/// SPIR-V reflection usually lists these slots for Samus bomb-style VS, but supplement when
/// hybrid finalize or partial scans miss [0..3]/[12..14]. Mesh/model VS (no corner attrs) keep
/// [`NvnChainParams::world_trs`] at [12..14] instead of identity.
pub fn supplement_family_a_cbuf8_position_chain_slots(
    usage: &mut HashMap<String, HashSet<u32>>,
    vs_wgsl: &str,
) {
    if !crate::spirv_to_wgsl::uses_cbuf8_vp(vs_wgsl) || !vs_wgsl.contains("in_attr0_1") {
        return;
    }
    if !vs_wgsl.contains("in_attr6_1") && !vs_wgsl.contains("in_attr2_1") {
        return;
    }
    usage
        .entry("cbuf_8_1_".to_string())
        .or_default()
        .extend(0..=3);
    usage
        .entry("cbuf_8_1_".to_string())
        .or_default()
        .extend(8..=11);
    usage
        .entry("cbuf_8_1_".to_string())
        .or_default()
        .extend(12..=14);
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
    /// 3×4 world transform rows for cbuf_8[12..14] on mesh/model VS. Ignored for Family-A
    /// particle billboards (cbuf_8 VP at [8..11]): CPU attr0 is already world-space so [12..14]
    /// must stay identity — see [`cbuf_8_position_transform`].
    pub world_trs: Mat4,
    /// Average flipbook crossfade blend for the current draw batch (0..1).
    pub pat_blend: f32,
    /// Batch-averaged per-particle UV offsets for TextureAnim3–5 (attr11 carries slots 3–4).
    pub tex_extra_avg: [[f32; 2]; 3],
    /// Batch-averaged particle velocity (Stripe / ComplexStripe basis).
    pub batch_velocity: Vec3,
    /// Batch-averaged flipbook tile scale from per-particle `tex_scale_live`.
    pub batch_tex_scale: Option<[f32; 2]>,
    /// Min/max normalized life in the current draw batch (for scroll/atlas envelope).
    pub batch_life_min: f32,
    pub batch_life_max: f32,
    /// PRMA meshes for primitive billboard mode.
    pub primitives: &'a [crate::effects::PrimitiveData],
    /// BFRES models for primitive draw mesh lookup.
    pub bfres_models: &'a [crate::effects::BfresModel],
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
            world_trs: Mat4::IDENTITY,
            pat_blend: 0.0,
            tex_extra_avg: [[0.0, 0.0]; 3],
            batch_velocity: Vec3::ZERO,
            batch_tex_scale: None,
            batch_life_min: life_t,
            batch_life_max: life_t,
            primitives: &[],
            bfres_models: &[],
        }
    }

    pub fn with_batch_life_range(mut self, min: f32, max: f32) -> Self {
        self.batch_life_min = min.clamp(0.0, 1.0);
        self.batch_life_max = max.clamp(0.0, 1.0);
        self
    }

    pub fn with_camera(mut self, cam_right: Vec3, cam_up: Vec3, aspect: f32) -> Self {
        self.cam_right = cam_right;
        self.cam_up = cam_up;
        self.aspect = aspect;
        self
    }

    pub fn with_world_trs(mut self, world_trs: Mat4) -> Self {
        self.world_trs = world_trs;
        self
    }

    pub fn with_pat_blend(mut self, pat_blend: f32) -> Self {
        self.pat_blend = pat_blend;
        self
    }

    pub fn with_tex_extra_avg(mut self, tex_extra_avg: [[f32; 2]; 3]) -> Self {
        self.tex_extra_avg = tex_extra_avg;
        self
    }

    pub fn with_batch_velocity(mut self, batch_velocity: Vec3) -> Self {
        self.batch_velocity = batch_velocity;
        self
    }

    pub fn with_batch_tex_scale(mut self, batch_tex_scale: Option<[f32; 2]>) -> Self {
        self.batch_tex_scale = batch_tex_scale;
        self
    }

    pub fn with_primitives(mut self, primitives: &'a [crate::effects::PrimitiveData]) -> Self {
        self.primitives = primitives;
        self
    }

    pub fn with_bfres_models(mut self, bfres_models: &'a [crate::effects::BfresModel]) -> Self {
        self.bfres_models = bfres_models;
        self
    }

    fn eval_context(&self) -> NvnEvalContext<'a> {
        let batch_life_t = cbuf_batch_life_t(
            self.emitter,
            self.life_t,
            self.batch_life_min,
            self.batch_life_max,
        );
        NvnEvalContext {
            emitter: self.emitter,
            life_t: self.life_t,
            batch_life_t,
            view_proj: self.view_proj,
            tex_res: self.tex_res,
            cam_right: self.cam_right,
            cam_up: self.cam_up,
            aspect: self.aspect,
            world_trs: self.world_trs,
            pat_blend: self.pat_blend,
            tex_extra_avg: self.tex_extra_avg,
            batch_velocity: self.batch_velocity,
            batch_tex_scale: self.batch_tex_scale,
            batch_life_min: self.batch_life_min,
            batch_life_max: self.batch_life_max,
            primitives: self.primitives,
            bfres_models: self.bfres_models,
        }
    }
}

/// Per-evaluation context shared by all cbuf slot fillers.
struct NvnEvalContext<'a> {
    emitter: &'a EmitterDef,
    /// Batch-average normalized life (emitter TRS / pat_blend averaging).
    life_t: f32,
    /// Life used for scroll/atlas cbuf rows (may differ when anim varies per-particle).
    batch_life_t: f32,
    view_proj: &'a Mat4,
    tex_res: Option<&'a TextureRes>,
    cam_right: Vec3,
    cam_up: Vec3,
    aspect: f32,
    world_trs: Mat4,
    pat_blend: f32,
    tex_extra_avg: [[f32; 2]; 3],
    batch_velocity: Vec3,
    batch_tex_scale: Option<[f32; 2]>,
    batch_life_min: f32,
    batch_life_max: f32,
    primitives: &'a [crate::effects::PrimitiveData],
    bfres_models: &'a [crate::effects::BfresModel],
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
    use crate::effects::{effective_tex_scale_uv as eff, TextureAnimFlags};
    let anim = TextureAnimFlags {
        pattern_anim_type: emitter.tex_pattern_anim_type,
        is_scroll: emitter.tex_is_scroll,
        is_rotate: emitter.tex_is_rotate,
        is_scale: emitter.tex_is_scale,
        inv_rand_u: emitter.tex_inv_rand_u,
        inv_rand_v: emitter.tex_inv_rand_v,
        pat_loop_random: emitter.tex_pat_loop_random,
        crossfade: emitter.tex_crossfade,
        scroll_rotation: emitter.tex_scroll_rotation,
        scroll_rotation_add: emitter.tex_scroll_rotation_add,
    };
    eff(
        emitter.tex_scale_uv,
        &anim,
        emitter.anim_tex_scale.as_ref(),
        life_t,
    )
}

/// Positive atlas cell size for cbuf slot 99 / flipbook grid metadata.
/// Per-particle InvRandU/V sign flips live in vertex attr5 offsets — not here.
fn atlas_tile_scale_uv(emitter: &EmitterDef, life_t: f32) -> [f32; 2] {
    let ts = effective_tex_scale_uv(emitter, life_t);
    [ts[0].abs().max(0.001), ts[1].abs().max(0.001)]
}

fn scroll_uv_angle(emitter: &EmitterDef, life_t: f32) -> f32 {
    use crate::effects::{scroll_uv_angle_at_life, TextureAnimFlags};
    let anim = TextureAnimFlags {
        pattern_anim_type: emitter.tex_pattern_anim_type,
        is_scroll: emitter.tex_is_scroll,
        is_rotate: emitter.tex_is_rotate,
        is_scale: emitter.tex_is_scale,
        inv_rand_u: emitter.tex_inv_rand_u,
        inv_rand_v: emitter.tex_inv_rand_v,
        pat_loop_random: emitter.tex_pat_loop_random,
        crossfade: emitter.tex_crossfade,
        scroll_rotation: emitter.tex_scroll_rotation,
        scroll_rotation_add: emitter.tex_scroll_rotation_add,
    };
    scroll_uv_angle_at_life(&anim, life_t, emitter.lifetime)
}

fn fill_cbuf_9_uv_rotation_slots(
    data: &mut NvnBufferData,
    slots: &HashSet<u32>,
    angle: f32,
    aspect: f32,
    cam_right: Vec3,
    cam_up: Vec3,
    emitter: &EmitterDef,
    view_dir: Vec3,
    batch_velocity: Vec3,
    mesh_ctx: Option<&crate::effects::SpawnMeshContext<'_>>,
    primitives: &[crate::effects::PrimitiveData],
) {
    let (basis_right, basis_up) = crate::effects::billboard_basis_for_emitter(
        emitter,
        cam_right,
        cam_up,
        view_dir,
        batch_velocity,
        mesh_ctx,
        primitives,
    );
    let primitive_mesh_loaded = emitter.billboard_type == crate::effects::BillboardType::Primitive
        && (mesh_ctx
            .and_then(|ctx| crate::effects::emitter_draw_mesh(ctx, emitter))
            .is_some()
            || crate::effects::emitter_primitive(emitter, primitives).is_some());
    let pivot47 = crate::effects::billboard_pivot_cbuf47(emitter.offset_type);
    let (c, s) = (angle.cos(), angle.sin());
    let aspect_scale = if aspect > 0.0 { 1.0 / aspect } else { 1.0 };
    for &slot in slots {
        match slot {
            // UV 2×2 rotation matrix coefficients (scroll TexScrollAnim rotation path).
            44 if angle.abs() > 1e-6 => data.set(44, [c, -s, 1.0, 1.0]),
            45 if angle.abs() > 1e-6 => data.set(45, [aspect_scale, s, c, 1.0]),
            44 => data.set(44, [0.0, 0.0, 1.0, 1.0]),
            45 => data.set(45, [aspect_scale, 1.0, 0.0, 1.0]),
            46 => data.set(46, [basis_right.x, basis_right.y, basis_right.z, 1.0]),
            47 if primitive_mesh_loaded && crate::fx_env::fx_native_vs_pos_enabled() => {
                // Native VS: .y/.z carry pivot offsets; .w carries mesh-up Z for gpr coupling.
                data.set(
                    47,
                    [pivot47[0], pivot47[1], pivot47[2], basis_up.z],
                )
            }
            47 if primitive_mesh_loaded => {
                // Patched VS mode 7 reads mesh up from .yzw.
                data.set(47, [0.0, basis_up.x, basis_up.y, basis_up.z])
            }
            47 if crate::fx_env::fx_native_vs_pos_enabled() => data.set(
                47,
                [
                    pivot47[0],
                    pivot47[1],
                    pivot47[2],
                    basis_up.z,
                ],
            ),
            47 => data.set(47, [0.0, basis_up.x, basis_up.y, basis_up.z]),
            _ => {}
        }
    }
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
    nvn_emitter_color_endpoint(emitter, life_t, NvnColorEndpointMode::Display)
}

/// How [`nvn_emitter_color_endpoint`] applies the combiner for native FS table fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NvnColorEndpointMode {
    /// Full combiner (CPU display / process 2+ GPU tables with one Hermite RGB spline).
    Display,
    /// Hermite RGB/A endpoints for process 0/1: colour0 + alpha combiner only; texture×colour
    /// and colour1 are handled by the NVN FS chain + `enhance_native_fragment_wgsl`.
    NativeTableEndpoint,
}

/// Sample emitter colour at normalized life for native FS cbuf_9 table keyframes or CPU display.
fn nvn_emitter_color_endpoint(
    emitter: &EmitterDef,
    life_t: f32,
    mode: NvnColorEndpointMode,
) -> [f32; 4] {
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

    let combiner = &emitter.combiner;
    if mode == NvnColorEndpointMode::NativeTableEndpoint
        && crate::combiner::combiner_is_configured(combiner)
        && combiner.color_combiner_process <= 1
    {
        let alpha = crate::combiner::combine_particle_rgba(
            [c0.x, c0.y, c0.z, c0.w],
            [c1.x, c1.y, c1.z, c1.w],
            a0,
            a1,
            combiner,
        )[3];
        return [c0.x, c0.y, c0.z, alpha];
    }

    crate::combiner::combine_particle_rgba(
        [c0.x, c0.y, c0.z, c0.w],
        [c1.x, c1.y, c1.z, c1.w],
        a0,
        a1,
        combiner,
    )
}

/// Per-pixel time the native FS feeds to cbuf_9 colour/alpha Hermite splines (lifetime path).
///
/// Early FS sets `gpr_0 = cbuf_10[2].x - in_attr5.w` (remaining life), then on the lifetime
/// branch (~L1711): `gpr_10 = gpr_0 / float(int(trunc(in_attr4.w)))`.
pub fn nvn_fs_spline_time(life_t: f32, life_gate: f32, attr4w: f32) -> f32 {
    let remaining = (life_gate - life_t).clamp(0.0, life_gate.max(0.0));
    let frame_scale = attr4w.trunc().max(1.0);
    remaining / frame_scale
}

/// Flipbook-branch spline time when `cbuf_9[8].x > 0` (~L1678): fractional frame index.
pub fn nvn_fs_spline_time_flipbook(
    life_t: f32,
    life_gate: f32,
    attr7x: f32,
    cols: f32,
    cbuf9_9y: f32,
) -> f32 {
    if cols <= 0.0 {
        return nvn_fs_spline_time(life_t, life_gate, 1.0);
    }
    let remaining = (life_gate - life_t).clamp(0.0, life_gate.max(0.0));
    let v = attr7x * cbuf9_9y * cols + remaining;
    (v / cols).fract()
}

/// Max keyframes per cbuf_9 table: colour0 occupies [60..67], alpha [68..75] (pattern
/// frames start at [76]).
const NVN_TABLE_MAX_KEYS: usize = 8;

/// Fixed emitter-clock origin fed in `cbuf_10[2].x` (frames). The game uploads the
/// emitter's real elapsed frame count; the shader only ever uses `clock - birth`, so any
/// shared origin works — particle births are fed as `EMITTER_CLOCK_FRAMES - age`.
/// Large enough that births never go negative, small enough for exact f32 arithmetic.
pub const EMITTER_CLOCK_FRAMES: f32 = 300.0;




/// Build NVN colour-table-0 keyframes (cbuf_9 slots 60..67) for the native colour spline.
///
/// The decoded microcode (bomb_base1 / AttackBombFlash1B VS) evaluates the table
/// piecewise-LINEARLY per keyframe pair — `v = v0 + (t - t0) * (v1 - v0) / (t1 - t0)`
/// gated by `step(t0, t) * (1 - step(t1, t))` masks — and is unrolled for the emitter's
/// exact key count (a 4-key emitter reads slots 60..63).
///
/// Ryujinx captures (bomb explosion, 19 exact matches at zero error) pin the layout:
/// the entries are the RAW authored colour0 keys on a FORWARD life axis (`.w` = key
/// time, birth at 0, death at 1) — no combiner or colour-scale pre-baking (the FS chain
/// applies cbuf_10[0]/cbuf_16 itself; baking them here double-applies).
fn nvn_color_table0_entries(emitter: &EmitterDef) -> Vec<[f32; 4]> {
    if !emitter.color0.is_empty() {
        return emitter
            .color0
            .iter()
            .map(|k| [k.r, k.g, k.b, k.frame.clamp(0.0, 1.0)])
            .collect();
    }
    // Keyless emitters: single constant entry (captures show 1-key tables for these).
    let c = crate::effects::sample_color_or_white(&emitter.color0, 0.0);
    vec![[c.x, c.y, c.z, 0.0]]
}

/// Build NVN alpha-table keyframes (cbuf_9 slots 68..75) for the native alpha spline.
///
/// Same capture-pinned layout as the colour table: raw alpha0 curve, forward axis,
/// values may exceed 1.0 (intensity scale — 1.5 observed in bomb captures). alpha1 is
/// combined by the FS chain via cbuf constants, not here.
fn nvn_alpha_table1_entries(emitter: &EmitterDef) -> Vec<[f32; 4]> {
    if !emitter.alpha0_keys.is_empty() {
        let mut entries: Vec<[f32; 4]> = emitter
            .alpha0_keys
            .iter()
            .map(|k| [k.a, k.a, k.a, k.frame.clamp(0.0, 1.0)])
            .collect();
        // Capture-verified: single-key tables get a synthesized t=0 clamp key prepended
        // (ring1_sub: authored 1.0@0.08 dumps as (1@0),(1@0.08),(pads idx+0.08));
        // multi-key tables starting past t=0 do NOT (smokeBase starts at 0.08 verbatim).
        if entries.len() == 1 && entries[0][3] > 0.0 {
            // The game's synthesized lead key carries yz = 0 (unread by the shader,
            // but matched for golden exactness).
            entries.insert(0, [entries[0][0], 0.0, 0.0, 0.0]);
        }
        return entries;
    }
    let a = &emitter.alpha0;
    if a.time2 <= 0.0 && a.time3 <= 0.0 {
        return vec![[a.start_value, a.start_value, a.start_value, 0.0]];
    }
    // 3v4k envelope: start / plateau start / plateau end / end.
    let v1 = a.start_value;
    let v2 = v1 + a.start_diff;
    let v3 = v2 + a.end_diff;
    vec![
        [v1, v1, v1, 0.0],
        [v2, v2, v2, a.time2.clamp(0.0, 1.0)],
        [v2, v2, v2, a.time3.clamp(0.0, 1.0)],
        [v3, v3, v3, 1.0],
    ]
}

/// Build the particle scale keyframe table (cbuf_9 slots 96..103) — same capture-pinned
/// layout as colour/alpha: raw authored scale keys [x,y,z,t], forward axis. Per-axis
/// stretch is real: flashLine1_b's captured rows carry (1.61, 1.07, 0.688)@0.42;
/// uniform emitters (ring1_sub (0.95@0, 0.95@0.2, 0.58@1.0)) match as before.
fn nvn_scale_table_entries(emitter: &EmitterDef) -> Vec<[f32; 4]> {
    if !emitter.scale_keys.is_empty() {
        let mut entries: Vec<[f32; 4]> = emitter
            .scale_keys
            .iter()
            .map(|k| [k.r, k.g, k.b, k.frame.clamp(0.0, 1.0)])
            .collect();
        if entries.len() == 1 && entries[0][3] > 0.0 {
            entries.insert(0, [entries[0][0], 0.0, 0.0, 0.0]);
        }
        return entries;
    }
    let a = &emitter.scale_anim;
    if a.time2 <= 0.0 && a.time3 <= 0.0 {
        return vec![[a.start_value, a.start_value, a.start_value, 0.0]];
    }
    let v1 = a.start_value;
    let v2 = v1 + a.start_diff;
    let v3 = v2 + a.end_diff;
    vec![
        [v1, v1, v1, 0.0],
        [v2, v2, v2, a.time2.clamp(0.0, 1.0)],
        [v2, v2, v2, a.time3.clamp(0.0, 1.0)],
        [v3, v3, v3, 1.0],
    ]
}

/// Table entry for slot offset `i`, padding past the end with the last value at strictly
/// increasing times — duplicated key times make the shader's `1/(t1-t0)` go inf/NaN.
///
/// Pad times use the game's convention `w = slot_index + last_real_key_time`, verified
/// against Ryujinx captures across 1/2/3/4-key tables (e.g. a 4-key table ending at
/// t=1.0 pads slots 4.. as w = 5, 6, 7, 8; a 2-key table ending at t=0.46 pads as
/// w = 2.46, 3.46, …).
fn nvn_table_entry(entries: &[[f32; 4]], i: usize) -> [f32; 4] {
    match entries.get(i) {
        Some(e) => *e,
        None => {
            let last = entries.last().copied().unwrap_or([1.0, 1.0, 1.0, 0.0]);
            [last[0], last[1], last[2], i as f32 + last[3]]
        }
    }
}

/// Colour1 keyframe table (cbuf_9 slots 76..83): the second multiplier colour chain.
/// Capture-verified layout (game samus/common fireBase draw frame_004272_draw_0020:
/// [76..79] = its authored colour1 ramp (1,.734,.603)@0 → (.095,0,0)@.32). The old
/// heuristic wrote flipbook pattern-pair data here, zeroing the chain for most emitters.
fn nvn_color_table1_entries(emitter: &EmitterDef) -> Vec<[f32; 4]> {
    if !emitter.color1.is_empty() {
        return emitter
            .color1
            .iter()
            .map(|k| [k.r, k.g, k.b, k.frame.clamp(0.0, 1.0)])
            .collect();
    }
    let c = crate::effects::sample_color_or_white(&emitter.color1, 0.0);
    vec![[c.x, c.y, c.z, 0.0]]
}

/// Alpha1 keyframe table (cbuf_9 slots 84..91) — same (a,a,a,t) encoding as alpha0.
/// Constant-1 single key when unauthored, padding to the game's (1,1,1,i) rows
/// (capture: fire_g / fireBase [84..87] = (1,1,1,0..3)).
fn nvn_alpha_table2_entries(emitter: &EmitterDef) -> Vec<[f32; 4]> {
    if !emitter.alpha1_keys.is_empty() {
        let mut entries: Vec<[f32; 4]> = emitter
            .alpha1_keys
            .iter()
            .map(|k| [k.a, k.a, k.a, k.frame.clamp(0.0, 1.0)])
            .collect();
        if entries.len() == 1 && entries[0][3] > 0.0 {
            entries.insert(0, [entries[0][0], 0.0, 0.0, 0.0]);
        }
        return entries;
    }
    let a = &emitter.alpha1;
    if a.time2 <= 0.0 && a.time3 <= 0.0 {
        return vec![[a.start_value, a.start_value, a.start_value, 0.0]];
    }
    let v1 = a.start_value;
    let v2 = v1 + a.start_diff;
    let v3 = v2 + a.end_diff;
    vec![
        [v1, v1, v1, 0.0],
        [v2, v2, v2, a.time2.clamp(0.0, 1.0)],
        [v2, v2, v2, a.time3.clamp(0.0, 1.0)],
        [v3, v3, v3, 1.0],
    ]
}

fn cbuf_base_kind(buf_name: &str) -> Option<&'static str> {
    // Longer prefixes first — `cbuf_1` is a prefix of `cbuf_10` / `cbuf_16`.
    if buf_name.starts_with("cbuf_16") {
        Some("cbuf_16")
    } else if buf_name.starts_with("cbuf_10") {
        Some("cbuf_10")
    } else if buf_name.starts_with("cbuf_9") {
        Some("cbuf_9")
    } else if buf_name.starts_with("cbuf_8") {
        Some("cbuf_8")
    } else if buf_name.starts_with("cbuf_1") {
        Some("cbuf_1")
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
        Some("cbuf_1") => build_cbuf_1_slots(slots, ctx),
        Some("cbuf_8") => build_cbuf_8_slots(slots, ctx),
        Some("cbuf_9") => build_cbuf_9_slots(slots, ctx),
        Some("cbuf_10") => build_cbuf_10_slots(slots, ctx),
        Some("cbuf_16") => build_cbuf_16_slots(slots, ctx),
        _ => NvnBufferData::default(),
    };
    fill_unmapped_cbuf_slots(&mut data, slots, cbuf_unmapped_default(buf_name));
    data
}

fn cbuf_unmapped_default(buf_name: &str) -> [f32; 4] {
    match cbuf_base_kind(buf_name) {
        // Coefficient slots default to neutral/identity, not white (which breaks splines/branches).
        Some("cbuf_9") | Some("cbuf_10") => [0.0, 0.0, 0.0, 1.0],
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}

/// True when the shader reads cbuf_8 VP columns [8..11] (particle position chain).
fn cbuf_8_vp_block_active(slots: &HashSet<u32>) -> bool {
    slots.iter().any(|&s| (8..=11).contains(&s))
}

/// Identity column for cbuf_8[0..3] or row for [12..14] (slot index 0..3 within each block).
fn cbuf_8_identity_pretransform_column(index: u32) -> [f32; 4] {
    let mut v = [0.0f32; 4];
    v[index as usize] = 1.0;
    v
}

/// World matrix applied at cbuf_8[12..14] before the Family-A VP multiply at [8..11].
///
/// Particle draws upload world-space centers in attr0, so Family-A billboards must not also
/// apply emitter/bone TRS here (that double-transforms and pushes clip off-screen). Mesh VS
/// without the cbuf_8 VP block still use [`NvnEvalContext::world_trs`].
///
/// [`finalize_native_vs_clip_position`] remains required for Family-A clip position because
/// native `main_1()` reads cbuf_9[46]/[47] as scalar fma state — not camera basis vectors —
/// and our PTCL-backed evaluator does not reproduce that chain exactly (Samus bomb 0x5740678a…).
fn cbuf_8_position_transform(ctx: &NvnEvalContext<'_>, vp_block: bool) -> Mat4 {
    if vp_block {
        Mat4::IDENTITY
    } else {
        ctx.world_trs
    }
}

/// Pack a `u32` into an f32 cbuf component preserving bit pattern (for `floatBitsToInt` compares).
pub(crate) fn nvn_pack_u32_slot(v: u32) -> f32 {
    f32::from_bits(v)
}

/// NVN draw-path render-flag bit written into cbuf_1[0].yzw int-equality refs.
fn nvn_draw_path_flag_bit(draw_path: u32) -> u32 {
    if draw_path == 0 {
        1
    } else {
        1u32.wrapping_shl(draw_path.min(31))
    }
}

/// Composite NVN render-flag bitmask from draw path, EmitterStatic flags, and display side.
pub(crate) fn nvn_emitter_render_flag_mask(emitter: &EmitterDef) -> u32 {
    let path_bit = nvn_draw_path_flag_bit(emitter.draw_path);
    let static_mask = emitter.flags1 | emitter.flags2 | emitter.flags3 | emitter.flags4;
    let side_bits = match emitter.display_side {
        DisplaySide::Both => 0,
        DisplaySide::Front => 1 << 8,
        DisplaySide::Back => 1 << 9,
        DisplaySide::Unknown(v) => v,
    };
    path_bit | static_mask | side_bits
}

/// cbuf_1[0]: `.x` = VS float scale (must stay 1.0); `.y`/`.z`/`.w` = render-flag int refs.
pub(crate) fn nvn_cbuf_1_render_flags_slot0(emitter: &EmitterDef) -> [f32; 4] {
    let flag = nvn_pack_u32_slot(nvn_emitter_render_flag_mask(emitter));
    [1.0, flag, flag, flag]
}

/// cbuf_9[5]: `.x` = VS float scale; `.y`/`.z`/`.w` = render-flag AND mask (int bits).
pub(crate) fn nvn_cbuf_9_render_flags_slot5(emitter: &EmitterDef) -> [f32; 4] {
    let flag = nvn_pack_u32_slot(nvn_emitter_render_flag_mask(emitter));
    [1.0, flag, flag, flag]
}

/// cbuf_9[49..51]: sprite subdivision layout rows used when the VS dynamically indexes
/// `_m0_[48 + corner]` (bomb flare VS). Mirrors [48]/[53] grid metadata.
fn nvn_cbuf_9_subdivision_layout_slots(subdiv: f32, cols: f32) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let inv = if subdiv > 0.0 { 1.0 / subdiv } else { 1.0 };
    (
        [cols, 0.0, subdiv, inv],
        [0.0, cols, subdiv, inv],
        [cols, cols, subdiv, inv],
    )
}

/// Life parameter for batch cbuf slots whose native FS path is per-pixel via attr5.w.
///
/// Colour/alpha Hermite tables (cbuf_9[60..71]) use life endpoints 0/1 — the fragment
/// shader evaluates splines with `gpr_10 = cbuf_10[2].x - in_attr5.w` (remaining life).
/// Scroll rotation and life-varying UV scale in cbuf_9/cbuf_10 are also per-particle when
/// IsRotate/IsScale animate over life; batch cbufs use a neutral envelope life instead of
/// the draw average so multi-particle batches are not pinned to one particle's age.
fn cbuf_batch_life_t(emitter: &EmitterDef, batch_life_t: f32, life_min: f32, life_max: f32) -> f32 {
    let life_varying = emitter.tex_is_rotate
        || emitter.tex_is_scale
        || emitter.anim_tex_scale.as_ref().is_some_and(|a| a.enable);
    if !life_varying {
        return batch_life_t;
    }
    if (life_max - life_min).abs() > 1.0e-4 {
        // Span the batch envelope so scroll/scale cbuf rows cover all particles.
        (life_min + life_max) * 0.5
    } else {
        // Uniform-age batch: batch average matches the sole per-particle attr5.w.
        batch_life_t
    }
}

/// cbuf_9[76..78]: flipbook frame-interpolation pair 3 (lo/hi/extension).
fn nvn_pattern_frame_pair3_slots(
    emitter: &EmitterDef,
    pat_blend: f32,
) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let anim = TextureAnimFlags {
        pattern_anim_type: emitter.tex_pattern_anim_type,
        is_scroll: emitter.tex_is_scroll,
        is_rotate: emitter.tex_is_rotate,
        is_scale: emitter.tex_is_scale,
        inv_rand_u: emitter.tex_inv_rand_u,
        inv_rand_v: emitter.tex_inv_rand_v,
        pat_loop_random: emitter.tex_pat_loop_random,
        crossfade: emitter.tex_crossfade,
        scroll_rotation: emitter.tex_scroll_rotation,
        scroll_rotation_add: emitter.tex_scroll_rotation_add,
    };
    let frame_count = emitter.tex_pat_frame_count.max(1) as f32;
    let (lo_frame, _, _) = crate::effects::pattern_frame_with_crossfade(
        &anim,
        emitter.tex_pat_frame_count,
        &emitter.tex_pat_frame_table,
        emitter.tex_pat_frequency,
        0.0,
        0.0,
        None,
    );
    let (hi_frame, _, _) = crate::effects::pattern_frame_with_crossfade(
        &anim,
        emitter.tex_pat_frame_count,
        &emitter.tex_pat_frame_table,
        emitter.tex_pat_frequency,
        1.0,
        0.0,
        None,
    );
    let blend = if emitter.tex_crossfade {
        pat_blend.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let ext_z = if blend > 0.0 { blend } else { 1.0 };
    (
        [lo_frame as f32, 0.0, 0.0, 0.0],
        [hi_frame as f32, 0.0, 0.0, frame_count],
        [0.0, 0.0, ext_z, frame_count],
    )
}

/// cbuf_1[1]: texture tiling — `.x` = UV scale, `.y` = additive bias (see slot docs).
fn nvn_cbuf_1_tex_tiling_slot1(emitter: &EmitterDef, life_t: f32) -> [f32; 4] {
    let ts = effective_tex_scale_uv(emitter, life_t);
    [ts[0].max(0.001), 0.0, 0.0, 0.0]
}

fn build_cbuf_1_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let mut data = NvnBufferData::default();
    for &slot in slots {
        match slot {
            0 => data.set(0, nvn_cbuf_1_render_flags_slot0(ctx.emitter)),
            1 => data.set(1, nvn_cbuf_1_tex_tiling_slot1(ctx.emitter, ctx.batch_life_t)),
            _ => {}
        }
    }
    data
}

fn build_cbuf_8_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let combiner_coeffs = crate::combiner::eval_combiner_gpu_coeffs(&ctx.emitter.combiner);
    let vp_block = cbuf_8_vp_block_active(slots);
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
            // Particle VS/FS treat [12..14] as a 3×4 transform applied before the VP at [8..11].
            12..=14 => {
                let rows =
                    crate::effects::mat4_to_cbuf_rows_3x4(cbuf_8_position_transform(ctx, vp_block));
                let row = (slot - 12) as usize;
                data.set(slot as u64, rows[row]);
            }
            // Some VS variants read [0..3] as a 4×4 pre-transform (fma chains), not colour table 0.
            // When the VP block [8..11] is also present, treat [0..3] as identity columns.
            0..=3 if vp_block => {
                let mut v = [0.0f32; 4];
                v[slot as usize] = 1.0;
                data.set(slot as u64, v);
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
    let ts = atlas_tile_scale_uv(ctx.emitter, ctx.batch_life_t);
    let su = ts[0];
    let sv = ts[1];
    let cols = (1.0 / su).round();
    let subdiv = cols.max(1.0);
    let frame_count = ctx.emitter.tex_pat_frame_count.max(1) as f32;
    let flipbook = crate::effects::emitter_uses_tex_pattern(ctx.emitter);
    let vp = ctx.view_proj.to_cols_array_2d();
    let uv_angle = scroll_uv_angle(ctx.emitter, ctx.batch_life_t);
    let (sub49, sub50, sub51) = nvn_cbuf_9_subdivision_layout_slots(subdiv, cols);

    for &slot in slots {
        match slot {
            0..=3 => data.set(slot as u64, vp[slot as usize]),
            5 => data.set(5, nvn_cbuf_9_render_flags_slot5(ctx.emitter)),
            // .x/.y gate flipbook vs lifetime spline time; UV columns stay in [10].y.
            8 => {
                if flipbook {
                    data.set(8, [cols, 0.0, subdiv, 0.0]);
                } else {
                    data.set(8, [0.0, 0.0, 0.0, 0.0]);
                }
            }
            9 => {
                let blend = if ctx.emitter.tex_crossfade {
                    ctx.pat_blend.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                data.set(9, [blend, 1.0 - blend, 1.0, 1.0]);
            }
            10 => data.set(10, [0.0, cols, 0.0, 0.0]),
            13 => data.set(13, [ctx.emitter.tex_scroll_uv[0], ctx.emitter.tex_scroll_uv[1], 0.0, 0.0]),
            14 => data.set(14, [1.0, 1.0, 0.0, 0.0]),
            15 => {
                let blend = if ctx.emitter.tex_crossfade {
                    ctx.pat_blend.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                data.set(15, [blend, 1.0 - blend, 0.0, 0.0]);
            }
            17 => {
                if let Some(tex) = ctx.tex_res {
                    data.set(17, [tex.width as f32, tex.height as f32, 0.0, 0.0]);
                } else {
                    data.set(17, [256.0, 256.0, 0.0, 0.0]);
                }
            }
            // [44..47]: GAME semantics = TexScrollAnim0 UV transform ([46].xy = authored
            // ScaleXY — capture-verified), but our fills come from
            // `fill_cbuf_9_uv_rotation_slots` (camera basis + native-VS pivots) which the
            // current native/override web depends on. Game-parity fills are gated on
            // resolving that consumption (task #15); the editor-injected billboard basis
            // has already moved to [120]/[121].
            44..=47 => {}
            // Editor-injected billboard camera basis (moved off the game's [46]/[47]).
            120 => data.set(120, [ctx.cam_right.x, ctx.cam_right.y, ctx.cam_right.z, 1.0]),
            121 => data.set(121, [0.0, ctx.cam_up.x, ctx.cam_up.y, ctx.cam_up.z]),
            48 => data.set(48, [0.0, 0.0, subdiv, subdiv]),
            49 => data.set(49, sub49),
            50 => data.set(50, sub50),
            51 => data.set(51, sub51),
            53 => data.set(53, [0.0, 0.0, subdiv, 0.0]),
            // [59].x = emitter ColorScale (capture-verified across 6 emitters: 1.2/1.4/
            // 1.7/2.0 exactly matching the authored scale, yzw = 0; the game keeps
            // cbuf_10[0] at 1). Replaces the old heuristic combiner-coefficient fill.
            59 => data.set(59, [ctx.emitter.color_scale.max(0.0), 0.0, 0.0, 0.0]),
            // Colour0/alpha0 [60..75], colour1 [76..83] and alpha1 [84..91] keyframe
            // tables are filled whole by `fill_cbuf_9_color_alpha_tables` after this
            // loop. ([76..78] previously held a heuristic flipbook pattern-pair fill —
            // capture frame_004272_draw_0020 pins [76..83] as the colour1 ramp.)
            76..=91 => {}
            // World-position chain coefficients (zero .xyz = no extra offset; .w = compare sentinel)
            113 => data.set(113, [1.0, 1.0, 1.0, 1.0]),
            114 => data.set(114, [0.0, 0.0, 0.0, 1.0]),
            // Axis scales for in_attr0_.xyz — must be 1.0 so particle center reaches gl_Position
            115 => data.set(115, [1.0, 1.0, 1.0, 1.0]),
            92 => data.set(
                92,
                [
                    ctx.emitter.indirect_scroll_uv[0],
                    ctx.emitter.indirect_scroll_uv[1],
                    ctx.emitter.tex2_scroll_uv[0],
                    ctx.emitter.tex2_scroll_uv[1],
                ],
            ),
            // Editor flipbook layout (moved off the game's scale-key table at [96..103];
            // consumed by the injected crossfade fallback + corner scale).
            124 => data.set(124, [0.0, 0.0, 0.0, su]),
            125 => data.set(125, [su, sv, 0.0, 1.0]),
            126 => data.set(126, [0.0, 0.0, 0.0, sv]),
            127 => data.set(127, [su, sv, 0.0, 1.0]),
            // [96..103] = particle scale keyframe table, filled whole by
            // `fill_cbuf_9_color_alpha_tables` alongside colour/alpha.
            100 if crate::effects::extra_tex_slot_active(ctx.emitter, 0)
                || crate::effects::extra_tex_slot_active(ctx.emitter, 1) =>
            {
                data.set(
                    100,
                    [
                        ctx.emitter.tex_extra_slots[0].scroll_uv[0],
                        ctx.emitter.tex_extra_slots[0].scroll_uv[1],
                        ctx.emitter.tex_extra_slots[1].scroll_uv[0],
                        ctx.emitter.tex_extra_slots[1].scroll_uv[1],
                    ],
                )
            }
            101 if crate::effects::extra_tex_slot_active(ctx.emitter, 2) => data.set(
                101,
                [
                    ctx.emitter.tex_extra_slots[2].scroll_uv[0],
                    ctx.emitter.tex_extra_slots[2].scroll_uv[1],
                    0.0,
                    0.0,
                ],
            ),
            // FS life gate: discard when alpha GPR <= .z. Large negative .z disables the cull.
            94 => data.set(94, [0.0, 0.0, -1.0e6, 0.0]),
            _ => {}
        }
    }
    fill_cbuf_9_uv_rotation_slots(
        &mut data,
        slots,
        uv_angle,
        ctx.aspect,
        ctx.cam_right,
        ctx.cam_up,
        ctx.emitter,
        ctx.cam_right.cross(ctx.cam_up).normalize_or_zero(),
        ctx.batch_velocity,
        Some(&crate::effects::SpawnMeshContext {
            primitives: ctx.primitives,
            bfres_models: ctx.bfres_models,
        }),
        ctx.primitives,
    );
    fill_cbuf_9_color_alpha_tables(&mut data, slots, ctx.emitter);
    // Editor-injected lanes (billboard basis 120/121, flipbook tile layout 124..127) are
    // read by PATCHED WGSL, which slot-usage scanning may not cover on every path — fill
    // them unconditionally (cheap constants; unread slots are harmless).
    data.set(120, [ctx.cam_right.x, ctx.cam_right.y, ctx.cam_right.z, 1.0]);
    data.set(121, [0.0, ctx.cam_up.x, ctx.cam_up.y, ctx.cam_up.z]);
    data.set(124, [0.0, 0.0, 0.0, su]);
    data.set(125, [su, sv, 0.0, 1.0]);
    data.set(126, [0.0, 0.0, 0.0, sv]);
    data.set(127, [su, sv, 0.0, 1.0]);
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

/// Fill the colour/alpha keyframe slots the shader actually reads (the microcode is
/// unrolled per key count, so the requested set covers exactly its table length). Unread
/// slots stay unset — matching the game, which leaves them at zero in captures.
fn fill_cbuf_9_color_alpha_tables(
    data: &mut NvnBufferData,
    slots: &HashSet<u32>,
    emitter: &EmitterDef,
) {
    if slots.iter().any(|&s| (60..=67).contains(&s)) {
        let entries = nvn_color_table0_entries(emitter);
        for i in 0..NVN_TABLE_MAX_KEYS {
            if slots.contains(&(60 + i as u32)) {
                data.set(60 + i as u64, nvn_table_entry(&entries, i));
            }
        }
    }
    if slots.iter().any(|&s| (68..=75).contains(&s)) {
        let entries = nvn_alpha_table1_entries(emitter);
        for i in 0..NVN_TABLE_MAX_KEYS {
            if slots.contains(&(68 + i as u32)) {
                data.set(68 + i as u64, nvn_table_entry(&entries, i));
            }
        }
    }
    if slots.iter().any(|&s| (76..=83).contains(&s)) {
        let entries = nvn_color_table1_entries(emitter);
        for i in 0..NVN_TABLE_MAX_KEYS {
            if slots.contains(&(76 + i as u32)) {
                data.set(76 + i as u64, nvn_table_entry(&entries, i));
            }
        }
    }
    if slots.iter().any(|&s| (84..=91).contains(&s)) {
        let entries = nvn_alpha_table2_entries(emitter);
        for i in 0..NVN_TABLE_MAX_KEYS {
            if slots.contains(&(84 + i as u32)) {
                data.set(84 + i as u64, nvn_table_entry(&entries, i));
            }
        }
    }
    // Scale keyframe table: the game uses [96..103], but [100]/[101] are still the
    // editor's extra-tex scroll lane (injected FS reads) — cap at 4 entries until that
    // lane moves to spare slots too (task #15).
    if slots.iter().any(|&s| (96..=99).contains(&s)) {
        let entries = nvn_scale_table_entries(emitter);
        for i in 0..4 {
            if slots.contains(&(96 + i as u32)) {
                data.set(96 + i as u64, nvn_table_entry(&entries, i));
            }
        }
    }
}

fn build_cbuf_10_slots(slots: &HashSet<u32>, ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    let mut data = NvnBufferData::default();
    let ts = effective_tex_scale_uv(ctx.emitter, ctx.batch_life_t);
    let su = ts[0].max(0.001);
    let sv = ts[1].max(0.001);
    let angle = scroll_uv_angle(ctx.emitter, ctx.batch_life_t);
    let (c, s) = (angle.cos(), angle.sin());
    let rotate = angle.abs() > 1e-6;
    // Per-particle UV offset is supplied via vertex attr5.xy; keep cbuf offset neutral.
    let to = [0.0f32, 0.0];

    for &slot in slots {
        match slot {
            // Per-channel multiply in the native chains. Capture-verified: the game keeps
            // this at 1 and feeds ColorScale via cbuf_9[59].x instead (the VS multiplies
            // both into the colour, so the product is unchanged).
            0 => data.set(0, [1.0, 1.0, 1.0, 1.0]),
            // VS multiplies gpr components by .xyzw (see bomb VS cbuf_10[1] at ~line 2617).
            // Capture: the game keeps the whole row at 1 — [1,1,1,0] zeroed out_attr1.w
            // (the FS discard gate multiplies by it → every fragment discarded).
            1 => data.set(1, [1.0, 1.0, 1.0, 1.0]),
            // [2].x: the GAME uploads the emitter clock in frames here (age = clock -
            // attr_birth.w, lifetime = trunc(attr_life.w) — see
            // docs/game-particle-vertex-layout.md). The frame-clock feed (task #22)
            // uploads birth = EMITTER_CLOCK_FRAMES - p.age, so the clock MUST be the
            // same constant: the old legacy 1.0 here made the native chain see
            // birth(≈300) > clock(1) → early-return cull → colour varyings stayed
            // zero-init (all effects rendered gray) while the billboard override
            // resurrected the quads. force_hybrid_billboard_cbuf_defaults only fills
            // MISSING slots, so shaders whose usage includes [2] took this arm.
            2 => data.set(
                2,
                [
                    if crate::fx_env::fx_frame_clock_enabled() {
                        EMITTER_CLOCK_FRAMES
                    } else {
                        1.0
                    },
                    // Capture: game rows carry 1s alongside the clock, not zeros.
                    1.0,
                    1.0,
                    1.0,
                ],
            ),
            // Native VS (Family A) assigns .y to gpr_20 then multiplies it into the sin/cos
            // rotation chain; .z/.w are pure multiplies (see bomb VS ~L1213/L1317/L1325).
            // [0,0,0,1] zeroed .y/.z and collapsed world position before the VP multiply.
            3 => data.set(3, [1.0, 1.0, 1.0, 1.0]),
            // Rows 4-6: UV transform matrix. The native VS also multiplies position GPRs by .x on
            // these rows, so .x must stay 1.0. Scroll rotation lives in cbuf_9[44/45].
            4 => data.set(4, [1.0, if rotate { su * c } else { 1.0 }, if rotate { -su * s } else { 1.0 }, to[0]]),
            5 => data.set(5, [1.0, if rotate { sv * s } else { ts[1] }, if rotate { sv * c } else { ts[1] }, to[1]]),
            6 => data.set(6, [1.0, 1.0, 1.0, 0.0]),
            8 => data.set(8, [1.0, 1.0, 1.0, 1.0]),
            9 => data.set(
                9,
                [
                    ctx.emitter.indirect_tex_offset_uv[0],
                    ctx.emitter.indirect_tex_offset_uv[1],
                    ctx.emitter.tex2_offset_uv[0],
                    ctx.emitter.tex2_offset_uv[1],
                ],
            ),
            10 => data.set(10, [1.0, 1.0, 1.0, 1.0]),
            11 if crate::effects::extra_tex_slot_active(ctx.emitter, 0)
                || crate::effects::extra_tex_slot_active(ctx.emitter, 1) =>
            {
                data.set(
                    11,
                    [
                        ctx.tex_extra_avg[0][0],
                        ctx.tex_extra_avg[0][1],
                        ctx.tex_extra_avg[1][0],
                        ctx.tex_extra_avg[1][1],
                    ],
                )
            }
            12 if crate::effects::extra_tex_slot_active(ctx.emitter, 2)
                || crate::effects::extra_tex_slot_active(ctx.emitter, 0) =>
            {
                data.set(
                    12,
                    [
                        ctx.tex_extra_avg[2][0],
                        ctx.tex_extra_avg[2][1],
                        ctx.emitter.tex_extra_slots[0].scale_uv[0],
                        ctx.emitter.tex_extra_slots[0].scale_uv[1],
                    ],
                )
            }
            _ => {}
        }
    }
    data
}

fn build_cbuf_16_slots(slots: &HashSet<u32>, _ctx: &NvnEvalContext<'_>) -> NvnBufferData {
    // Capture-verified constants (Ryujinx sessions, 92 shader pairs): the dominant FS
    // family uploads near-fixed values here — NOT per-emitter combiner coefficients and
    // NOT ColorScale (which lives in cbuf_9[59].x). The old heuristic fills mismatched
    // the game on every slot for every matched emitter and skewed native-FS colours.
    //   [0] = (0.5, 0, 0, 0)
    //   [1] = (1, 1, 1, 1)
    //   [2] = (1, 0, X, Y) with small per-family variants (X∈{0,1}, Y∈{-1,0}) — the
    //         bomb/impact families use (1, 0, 1, -1)
    //   [3] = 0 (some families carry angle-like values, e.g. (0, 180, 0, 0))
    //   [4] = (1, -99999, 1, 0) — alpha-test gate, -99999 disables
    //   [5] = 0 (rare tiny scroll-like values in some families)
    // Per-family variants are follow-up RE (task #15); these constants match the
    // majority of captured draws exactly.
    let mut data = NvnBufferData::default();
    for &slot in slots {
        match slot {
            0 => data.set(0, [0.5, 0.0, 0.0, 0.0]),
            1 => data.set(1, [1.0, 1.0, 1.0, 1.0]),
            2 => data.set(2, [1.0, 0.0, 1.0, -1.0]),
            3 => data.set(3, [0.0, 0.0, 0.0, 0.0]),
            4 => data.set(4, [1.0, -99999.0, 1.0, 0.0]),
            5 => data.set(5, [0.0, 0.0, 0.0, 0.0]),
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
    s.extend(12..=14);
    s
}

fn documented_cbuf_9_slots() -> HashSet<u32> {
    [
        0, 1, 2, 3, 5, 8, 9, 10, 13, 14, 15, 17, 44, 45, 46, 47, 48, 49, 50, 51, 53, 59, 60, 61,
        62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83,
        84, 85, 86, 87, 88, 89, 90, 91, 92, 94, 96, 97, 98, 99, 100, 101, 113, 114, 115,
    ]
    .into_iter()
    .collect()
}

fn documented_cbuf_10_slots() -> HashSet<u32> {
    [0, 1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12].into_iter().collect()
}

fn documented_cbuf_16_slots() -> HashSet<u32> {
    [0, 1, 2, 3, 4].into_iter().collect()
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
    fn test_extract_cbuf_slots_from_spirv_invalid_input() {
        assert!(extract_cbuf_slots_from_spirv(&[]).is_empty());
        assert!(extract_cbuf_slots_from_spirv(b"not spirv").is_empty());
    }

    #[test]
    fn test_extract_cbuf_slots_from_spirv_access_chains() {
        fn push_op(words: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
            let wc = (operands.len() + 1) as u32;
            words.push((wc << 16) | u32::from(opcode));
            words.extend_from_slice(operands);
        }
        fn spirv_name_literals(name: &str) -> Vec<u32> {
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(0);
            while !bytes.len().is_multiple_of(4) {
                bytes.push(0);
            }
            bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }

        let mut words = vec![0x0723_0203, 0x0001_0500, 0x0008_0001, 32, 0];
        push_op(&mut words, 11, &[1]); // OpCapability Shader
        push_op(&mut words, 14, &[0, 1]); // OpMemoryModel Logical GLSL450

        let ty_i32 = 2;
        push_op(&mut words, OP_TYPE_INT as u16, &[ty_i32, 32, 1]); // signed i32

        let var_cbuf = 7;
        let c0 = 8;
        let c5 = 9;
        let c17 = 10;
        let ac5 = 11;
        let ac17 = 12;
        {
            let mut name_ops = vec![var_cbuf];
            name_ops.extend(spirv_name_literals("cbuf_9_1_"));
            push_op(&mut words, OP_NAME as u16, &name_ops);
        }

        push_op(&mut words, OP_CONSTANT as u16, &[ty_i32, c0, 0]);
        push_op(&mut words, OP_CONSTANT as u16, &[ty_i32, c5, 5]);
        push_op(&mut words, OP_CONSTANT as u16, &[ty_i32, c17, 17]);
        push_op(&mut words, OP_ACCESS_CHAIN as u16, &[ty_i32, ac5, var_cbuf, c0, c5]);
        push_op(&mut words, OP_ACCESS_CHAIN as u16, &[ty_i32, ac17, var_cbuf, c0, c17]);

        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let usage = extract_cbuf_slots_from_spirv(&bytes);
        let slots = usage.get("cbuf_9_1_").expect("cbuf variable");
        assert!(slots.contains(&5));
        assert!(slots.contains(&17));
    }

    #[test]
    fn test_cbuf_slot_usage_from_shaders_prefers_spirv() {
        fn push_op(words: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
            let wc = (operands.len() + 1) as u32;
            words.push((wc << 16) | u32::from(opcode));
            words.extend_from_slice(operands);
        }
        fn spirv_name_literals(name: &str) -> Vec<u32> {
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(0);
            while !bytes.len().is_multiple_of(4) {
                bytes.push(0);
            }
            bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }

        let mut words = vec![0x0723_0203, 0x0001_0500, 0x0008_0001, 32, 0];
        push_op(&mut words, 11, &[1]);
        push_op(&mut words, 14, &[0, 1]);
        let ty_i32 = 2;
        push_op(&mut words, OP_TYPE_INT as u16, &[ty_i32, 32, 1]);
        let var_cbuf = 7;
        let c0 = 8;
        let c8 = 9;
        {
            let mut name_ops = vec![var_cbuf];
            name_ops.extend(spirv_name_literals("cbuf_8_1_"));
            push_op(&mut words, OP_NAME as u16, &name_ops);
        }
        push_op(&mut words, OP_CONSTANT as u16, &[ty_i32, c0, 0]);
        push_op(&mut words, OP_CONSTANT as u16, &[ty_i32, c8, 8]);
        push_op(&mut words, OP_ACCESS_CHAIN as u16, &[ty_i32, 11, var_cbuf, c0, c8]);

        let spirv: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let vs_wgsl = "let a = cbuf_9_1_._m0_[99];";
        let fs_wgsl = "";
        let usage = cbuf_slot_usage_from_shaders(Some(&spirv), None, vs_wgsl, fs_wgsl);
        assert!(usage.get("cbuf_8_1_").unwrap().contains(&8));
        assert!(!usage.get("cbuf_8_1_").unwrap().contains(&99));
    }

    #[test]
    fn test_cbuf_slot_usage_from_shaders_falls_back_to_wgsl() {
        let vs = "let a = cbuf_9_1_._m0_[5];";
        let fs = "let b = cbuf_8_1_._m0_[8];";
        let usage = cbuf_slot_usage_from_shaders(None, None, vs, fs);
        assert!(usage.get("cbuf_9_1_").unwrap().contains(&5));
        assert!(usage.get("cbuf_8_1_").unwrap().contains(&8));
    }

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

        // Bank 16 carries capture-verified game constants (not combiner coefficients —
        // those remain in cbuf_8[6]/[7] and the FxTexBlendCoeffs uniform).
        let c16 = result.get("cbuf_16_1_").unwrap();
        assert_eq!(c16.slot_data.get(&1).copied(), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(c16.slot_data.get(&2).copied(), Some([1.0, 0.0, 1.0, -1.0]));
    }

    #[test]
    fn test_cbuf_16_modulate_neutral_fills_match_native_fs() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_16_1_".to_string(), [1u32, 2, 3].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let c16 = NvnChainEvaluator::evaluate_usage(&usage, &params)
            .get("cbuf_16_1_")
            .unwrap()
            .slot_data
            .clone();
        // Game constants for the dominant FS family (capture-verified).
        assert_eq!(c16.get(&1).copied(), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(c16.get(&2).copied(), Some([1.0, 0.0, 1.0, -1.0]));
        assert_eq!(c16.get(&3).copied(), Some([0.0, 0.0, 0.0, 0.0]));
    }

    #[test]
    fn test_native_table_endpoints_use_color0_for_process1() {
        let mut emitter = EmitterDef::default();
        emitter.combiner.color_combiner_process = 1;
        emitter.color0 = vec![
            ColorKey {
                frame: 0.0,
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            ColorKey {
                frame: 1.0,
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
        ];
        emitter.color1 = vec![ColorKey {
            frame: 0.0,
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        }];
        let entries = nvn_color_table0_entries(&emitter);
        let kf60 = entries[0];
        let kf_last = *entries.last().unwrap();
        // Forward axis (capture-pinned): first entry = birth key, last entry = death key.
        assert!((kf60[0] - 1.0).abs() < 0.01, "first entry is the birth colour0 key");
        assert!((kf_last[1] - 1.0).abs() < 0.01, "last entry is the death colour0 key");
        assert!((kf60[2] - 1.0).abs() > 0.5, "colour1 must not leak into RGB table");
    }

    #[test]
    fn test_cbuf_10_slot11_carries_extra_tex_offsets() {
        let mut emitter = EmitterDef::default();
        emitter.textures = std::iter::repeat_with(TextureRes::default).take(4).collect();
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [11u32, 12].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None)
            .with_tex_extra_avg([[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot11 = result.get("cbuf_10_1_").unwrap().slot_data.get(&11).unwrap();
        assert!((slot11[0] - 0.1).abs() < 0.001);
        assert!((slot11[2] - 0.3).abs() < 0.001);
        let slot12 = result.get("cbuf_10_1_").unwrap().slot_data.get(&12).unwrap();
        assert!((slot12[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cbuf_9_slot100_carries_extra_tex_scroll() {
        let mut emitter = EmitterDef::default();
        emitter.textures = std::iter::repeat_with(TextureRes::default).take(4).collect();
        emitter.tex_extra_slots[0].scroll_uv = [0.05, 0.06];
        emitter.tex_extra_slots[1].scroll_uv = [0.07, 0.08];
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [100u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot100 = result.get("cbuf_9_1_").unwrap().slot_data.get(&100).unwrap();
        assert!((slot100[0] - 0.05).abs() < 0.001);
        assert!((slot100[3] - 0.08).abs() < 0.001);
    }

    #[test]
    fn test_cbuf_9_slot9_carries_crossfade_blend() {
        let mut emitter = EmitterDef::default();
        emitter.tex_crossfade = true;
        emitter.tex_pat_frame_count = 4;
        let usage = documented_cbuf_9_slots();
        let mut map = HashMap::new();
        map.insert("cbuf_9_1_".to_string(), usage);
        let params = NvnChainParams::new(&emitter, 0.25, &Mat4::IDENTITY, None)
            .with_pat_blend(0.4);
        let result = NvnChainEvaluator::evaluate_usage(&map, &params);
        let slot9 = result.get("cbuf_9_1_").unwrap().slot_data.get(&9).unwrap();
        assert!((slot9[0] - 0.4).abs() < 0.001);
        assert!((slot9[1] - 0.6).abs() < 0.001);
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
        // With the frame-clock feed (FX_FRAME_CLOCK, default on except lib tests where
        // env decides) the builder must upload the SAME clock the vertex feed uses —
        // the legacy 1.0 made the native age chain cull every frame-clock-fed particle
        // (birth ≈ 300 > clock 1) and all effects rendered gray. .yzw carry 1s (capture).
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [2u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot2 = *result.get("cbuf_10_1_").unwrap().slot_data.get(&2).unwrap();
        let expected_clock = if crate::fx_env::fx_frame_clock_enabled() {
            EMITTER_CLOCK_FRAMES
        } else {
            1.0
        };
        assert_eq!(slot2, [expected_clock, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn test_cbuf_8_slots_0_3_are_transform_columns_when_position_chain() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_8_1_".to_string(),
            [0u32, 1, 2, 3, 8, 9].into_iter().collect(),
        );
        let mut emitter = EmitterDef::default();
        emitter.color0 = vec![ColorKey {
            frame: 0.0,
            r: 0.9,
            g: 0.1,
            b: 0.2,
            a: 0.3,
        }];
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c8 = result.get("cbuf_8_1_").unwrap();
        assert_eq!(c8.slot_data.get(&0).copied(), Some([1.0, 0.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&1).copied(), Some([0.0, 1.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&2).copied(), Some([0.0, 0.0, 1.0, 0.0]));
        assert_eq!(c8.slot_data.get(&3).copied(), Some([0.0, 0.0, 0.0, 1.0]));
        // VP column 0 still written at slot 8.
        assert!(c8.slot_data.get(&8).is_some());
    }

    #[test]
    fn test_cbuf_8_slots_12_14_emit_world_trs_without_vp_block() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_8_1_".to_string(),
            [12u32, 13, 14].into_iter().collect(),
        );
        let emitter = EmitterDef::default();
        let trs = Mat4::from_translation(Vec3::new(3.0, 4.0, 5.0));
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None)
            .with_world_trs(trs);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c8 = result.get("cbuf_8_1_").unwrap();
        assert_eq!(c8.slot_data.get(&12).copied(), Some([1.0, 0.0, 0.0, 3.0]));
        assert_eq!(c8.slot_data.get(&13).copied(), Some([0.0, 1.0, 0.0, 4.0]));
        assert_eq!(c8.slot_data.get(&14).copied(), Some([0.0, 0.0, 1.0, 5.0]));
    }

    #[test]
    fn test_cbuf_8_slots_12_14_identity_when_family_a_vp_block() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_8_1_".to_string(),
            [0u32, 1, 2, 3, 8, 9, 10, 11, 12, 13, 14].into_iter().collect(),
        );
        let emitter = EmitterDef::default();
        let trs = Mat4::from_translation(Vec3::new(3.0, 4.0, 5.0));
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None)
            .with_world_trs(trs);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c8 = result.get("cbuf_8_1_").unwrap();
        assert_eq!(c8.slot_data.get(&12).copied(), Some([1.0, 0.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&13).copied(), Some([0.0, 1.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&14).copied(), Some([0.0, 0.0, 1.0, 0.0]));
        assert_eq!(c8.slot_data.get(&0).copied(), Some([1.0, 0.0, 0.0, 0.0]));
    }

    #[test]
    fn test_supplement_family_a_cbuf8_position_chain_slots() {
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] gl_Position";
        let mut usage = extract_cbuf_slots_from_wgsl(vs);
        supplement_family_a_cbuf8_position_chain_slots(&mut usage, vs);
        let slots = usage.get("cbuf_8_1_").unwrap();
        for slot in 0..=3 {
            assert!(slots.contains(&slot), "missing pre-transform slot {slot}");
        }
        for slot in 8..=11 {
            assert!(slots.contains(&slot), "missing VP slot {slot}");
        }
        for slot in 12..=14 {
            assert!(slots.contains(&slot), "missing world-row slot {slot}");
        }
    }

    #[test]
    fn test_force_hybrid_fills_cbuf8_pretransform_identity() {
        let mut data = NvnBufferData::default();
        force_hybrid_billboard_cbuf_defaults(
            &mut data,
            "cbuf_8_1_",
            &Mat4::IDENTITY,
            Vec3::X,
            Vec3::Y,
            None,
        );
        assert_eq!(data.slot_data.get(&0).copied(), Some([1.0, 0.0, 0.0, 0.0]));
        assert_eq!(data.slot_data.get(&12).copied(), Some([1.0, 0.0, 0.0, 0.0]));
        assert_eq!(data.slot_data.get(&14).copied(), Some([0.0, 0.0, 1.0, 0.0]));
        assert!(data.slot_data.contains_key(&8));
    }

    #[test]
    fn test_bomb_family_a_cbuf8_pretransform_from_fixture_usage() {
        let Some((pair, _)) = bomb_shader_pair_for_phase0() else {
            eprintln!("SKIP bomb cbuf8 — no export/fixture");
            return;
        };
        let vs_shader = pair.vertex.as_ref().expect("bomb VS");
        let (vs_wgsl, _) = crate::spirv_to_wgsl::spirv_to_wgsl(
            vs_shader.spirv.as_slice(),
            naga::ShaderStage::Vertex,
            "bomb_cbuf8_test_vs",
        )
        .expect("vs wgsl");
        let fs_wgsl = pair
            .fragment
            .as_ref()
            .map(|fs| {
                crate::spirv_to_wgsl::spirv_to_wgsl(
                    fs.spirv.as_slice(),
                    naga::ShaderStage::Fragment,
                    "bomb_cbuf8_test_fs",
                )
                .expect("fs wgsl")
                .0
            })
            .unwrap_or_default();
        let vs_prefixed = crate::spirv_to_wgsl::wire_vertex_simulation_varyings(&vs_wgsl);
        let usage = cbuf_slot_usage_from_shaders(
            Some(vs_shader.spirv.as_slice()),
            pair.fragment.as_ref().map(|s| s.spirv.as_slice()),
            &vs_prefixed,
            &fs_wgsl,
        );
        let trs = Mat4::from_translation(Vec3::new(9.0, 8.0, 7.0));
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None).with_world_trs(trs);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c8 = result.get("cbuf_8_1_").expect("cbuf_8 usage");
        if c8.slot_data.contains_key(&8) {
            assert_eq!(
                c8.slot_data.get(&12).copied(),
                Some([1.0, 0.0, 0.0, 0.0]),
                "Family-A bomb must not double-apply world_trs at cbuf_8[12]"
            );
            assert_eq!(
                c8.slot_data.get(&0).copied(),
                Some([1.0, 0.0, 0.0, 0.0]),
                "Family-A bomb cbuf_8[0] must be identity column, not colour"
            );
        }
    }

    #[test]
    fn test_cbuf_8_slots_12_14_are_transform_rows_not_color() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_8_1_".to_string(),
            [12u32, 13, 14, 0].into_iter().collect(),
        );
        let mut emitter = EmitterDef::default();
        emitter.color0 = vec![ColorKey {
            frame: 0.0,
            r: 0.9,
            g: 0.1,
            b: 0.2,
            a: 0.3,
        }];
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c8 = result.get("cbuf_8_1_").unwrap();
        assert_eq!(c8.slot_data.get(&12).copied(), Some([1.0, 0.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&13).copied(), Some([0.0, 1.0, 0.0, 0.0]));
        assert_eq!(c8.slot_data.get(&14).copied(), Some([0.0, 0.0, 1.0, 0.0]));
        // Slot 0 still receives colour data when explicitly requested.
        let slot0 = c8.slot_data.get(&0).unwrap();
        assert!((slot0[0] - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_nvn_color_table0_keyframes_span_life_endpoints() {
        let mut emitter = EmitterDef::default();
        emitter.color0 = vec![
            ColorKey {
                frame: 0.0,
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            ColorKey {
                frame: 1.0,
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
        ];
        emitter.alpha0_keys = vec![
            ColorKey {
                frame: 0.0,
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            ColorKey {
                frame: 1.0,
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        ];
        let entries = nvn_color_table0_entries(&emitter);
        let kf60 = entries[0];
        let kf_last = *entries.last().unwrap();
        assert_eq!(kf60[3], 0.0);
        assert_eq!(kf_last[3], 1.0);
        // Forward axis (capture-pinned): entry 0 = birth key, last entry = death key,
        // values RAW from the PTCL keys (no combiner/alpha1 baking).
        assert!((kf60[0] - 1.0).abs() < 0.01, "entry 0 carries the raw birth key");
        assert!((kf_last[1] - 1.0).abs() < 0.01, "last entry carries the raw death key");
        let alpha = nvn_alpha_table1_entries(&emitter);
        let a68 = alpha[0];
        let a_last = *alpha.last().unwrap();
        assert_eq!(a68[3], 0.0);
        assert_eq!(a_last[3], 1.0);
        assert!((a68[0] - 1.0).abs() < 0.01, "raw alpha0 key value, no alpha1 baking");
        assert!((a_last[0] - 1.0).abs() < 0.01);
        assert!(
            alpha.windows(2).all(|w| w[0][3] < w[1][3]),
            "alpha keyframe times must be strictly increasing"
        );
    }

    #[test]
    fn test_nvn_fs_spline_time_is_remaining_life() {
        assert!((nvn_fs_spline_time(0.0, 1.0, 1.0) - 1.0).abs() < 1e-5);
        assert!((nvn_fs_spline_time(1.0, 1.0, 1.0) - 0.0).abs() < 1e-5);
        assert!((nvn_fs_spline_time(0.25, 1.0, 1.0) - 0.75).abs() < 1e-5);
        assert!((nvn_fs_spline_time(0.5, 1.0, 2.0) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn test_cbuf_9_slot8_x_nonzero_for_flipbook_emitter() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [8u32, 10].into_iter().collect());
        let mut emitter = EmitterDef::default();
        emitter.tex_scale_uv = [0.25, 1.0];
        emitter.tex_pat_frame_count = 4;
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        assert_eq!(c9.slot_data.get(&8).copied(), Some([4.0, 0.0, 4.0, 0.0]));
        assert_eq!(c9.slot_data.get(&10).copied(), Some([0.0, 4.0, 0.0, 0.0]));
    }

    #[test]
    fn test_cbuf_10_offset_neutral_for_attr5_path() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_10_1_".to_string(),
            [4u32, 5, 9].into_iter().collect(),
        );
        let mut emitter = EmitterDef::default();
        emitter.tex_offset_uv = [0.25, 0.5];
        emitter.tex_scale_uv = [0.125, 0.25];
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c10 = result.get("cbuf_10_1_").unwrap();
        assert_eq!(c10.slot_data.get(&4).map(|v| v[3]), Some(0.0));
        assert_eq!(c10.slot_data.get(&5).map(|v| v[3]), Some(0.0));
        assert_eq!(c10.slot_data.get(&9).map(|v| v[0]), Some(0.0));
        assert_eq!(c10.slot_data.get(&9).map(|v| v[1]), Some(0.0));
    }

    #[test]
    fn test_cbuf_9_slot8_x_zero_forces_lifetime_spline_path() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [8u32, 10].into_iter().collect());
        let mut emitter = EmitterDef::default();
        emitter.tex_scale_uv = [0.25, 1.0];
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        assert_eq!(c9.slot_data.get(&8).copied(), Some([0.0, 0.0, 0.0, 0.0]));
        assert_eq!(c9.slot_data.get(&10).copied(), Some([0.0, 4.0, 0.0, 0.0]));
    }

    #[test]
    fn test_cbuf_9_color_keyframes_use_life_endpoints_not_draw_life_t() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_9_1_".to_string(),
            [60u32, 61, 68, 69].into_iter().collect(),
        );
        let mut emitter = EmitterDef::default();
        emitter.color0 = vec![
            ColorKey {
                frame: 0.0,
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            ColorKey {
                frame: 1.0,
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 0.5,
            },
        ];
        emitter.alpha0_keys = vec![
            ColorKey {
                frame: 0.0,
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            ColorKey {
                frame: 1.0,
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.5,
            },
        ];
        // Draw-time life_t=0.5 would yield purple-ish if flat-sampled; endpoints must differ.
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        let kf60 = c9.slot_data.get(&60).unwrap();
        let kf61 = c9.slot_data.get(&61).unwrap();
        assert!((kf60[0] - 1.0).abs() < 0.01, "slot 60 carries the birth key (forward w=0)");
        assert!((kf61[2] - 1.0).abs() < 0.01, "slot 61 carries the death key (forward w=1)");
        assert!(kf60[3] < kf61[3], "keyframe times must ascend over forward life 0..1");
        let kf68 = c9.slot_data.get(&68).unwrap();
        let kf69 = c9.slot_data.get(&69).unwrap();
        assert!(kf68[3] < kf69[3], "alpha keyframe times must ascend over forward life 0..1");
        assert_ne!(kf68[0], kf69[0], "alpha endpoints should differ for fade emitter");
        let alpha = nvn_alpha_table1_entries(&emitter);
        assert_eq!(*kf68, nvn_table_entry(&alpha, 0));
        assert_eq!(*kf69, nvn_table_entry(&alpha, 1));
    }

    #[test]
    fn test_cbuf_9_slot5_x_is_finite_float_not_nan_mask() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [5u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot5 = result.get("cbuf_9_1_").unwrap().slot_data.get(&5).unwrap();
        assert_eq!(slot5[0], 1.0);
        assert!(slot5[0].is_finite(), "native VS assigns cbuf_9[5].x to gpr_17 as float");
    }

    #[test]
    fn test_cbuf_9_slot47_pivot_when_native_vs() {
        crate::spirv_to_wgsl::with_test_env("FX_NATIVE_VS_POS", "1", || {
            let mut usage = HashMap::new();
            usage.insert("cbuf_9_1_".to_string(), [47u32].into_iter().collect());
            let mut emitter = EmitterDef::default();
            emitter.offset_type = 1;
            let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
            let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
            let slot47 = result.get("cbuf_9_1_").unwrap().slot_data.get(&47).unwrap();
            assert_eq!(slot47[1], -0.5, ".y carries pivot Y offset");
        });
    }

    #[test]
    fn test_cbuf_9_slot46_uses_billboard_basis() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [46u32].into_iter().collect());
        let mut emitter = EmitterDef::default();
        emitter.billboard_type = crate::effects::BillboardType::PlateXy;
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot46 = result.get("cbuf_9_1_").unwrap().slot_data.get(&46).unwrap();
        assert_eq!(slot46[0], 1.0);
        assert_eq!(slot46[1], 0.0);
        assert_eq!(slot46[2], 0.0);
    }

    #[test]
    fn test_cbuf_9_primitive_mesh_basis_slots() {
        crate::spirv_to_wgsl::with_test_env("FX_NATIVE_VS_POS", "0", || {
            let mut usage = HashMap::new();
            usage.insert(
                "cbuf_9_1_".to_string(),
                [46u32, 47].into_iter().collect(),
            );
            let prim = crate::effects::PrimitiveData {
                id: 1,
                vertices: vec![
                    crate::effects::MeshVertex {
                        position: [0.0, 0.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                    crate::effects::MeshVertex {
                        position: [1.0, 0.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                    crate::effects::MeshVertex {
                        position: [0.0, 1.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                ],
                indices: vec![0, 1, 2],
            };
            let mut emitter = EmitterDef::default();
            emitter.billboard_type = crate::effects::BillboardType::Primitive;
            emitter.particle_primitive_id = 1;
            let primitives = [prim];
            let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None)
                .with_primitives(&primitives);
            let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
            let c9 = result.get("cbuf_9_1_").unwrap();
            let slot46 = c9.slot_data.get(&46).unwrap();
            let slot47 = c9.slot_data.get(&47).unwrap();
            assert!(slot46[0].abs() > 0.9, "mesh right.x from triangle");
            assert!(slot47[2].abs() > 0.9, "patched VS mode 7 reads mesh up.y in .z");
            assert_eq!(slot47[0], 0.0);
            assert_eq!(slot47[1], 0.0);
        });
    }

    #[test]
    fn test_cbuf_9_slot47_primitive_pivot_native_vs() {
        crate::spirv_to_wgsl::with_test_env("FX_NATIVE_VS_POS", "1", || {
            let mut usage = HashMap::new();
            usage.insert(
                "cbuf_9_1_".to_string(),
                [47u32].into_iter().collect(),
            );
            let prim = crate::effects::PrimitiveData {
                id: 1,
                vertices: vec![
                    crate::effects::MeshVertex {
                        position: [0.0, 0.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                    crate::effects::MeshVertex {
                        position: [1.0, 0.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                    crate::effects::MeshVertex {
                        position: [0.0, 1.0, 0.0],
                        uv: [0.0, 0.0],
                        normal: [0.0, 0.0, 1.0],
                    },
                ],
                indices: vec![0, 1, 2],
            };
            let mut emitter = EmitterDef::default();
            emitter.billboard_type = crate::effects::BillboardType::Primitive;
            emitter.particle_primitive_id = 1;
            emitter.offset_type = 1;
            let primitives = [prim];
            let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None)
                .with_primitives(&primitives);
            let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
            let slot47 = result.get("cbuf_9_1_").unwrap().slot_data.get(&47).unwrap();
            assert_eq!(slot47[1], -0.5, ".y carries pivot Y for native VS");
            assert_eq!(slot47[2], 0.0, ".z carries pivot X for native VS");
        });
    }

    #[test]
    fn test_cbuf_10_slot3_is_neutral_rotation_coeff() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [3u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot3 = result.get("cbuf_10_1_").unwrap().slot_data.get(&3).unwrap();
        assert_eq!(slot3[0], 1.0, ".x feeds out_attr3 chain");
        assert_eq!(slot3[1], 1.0, ".y must not zero gpr_20 scale");
        assert_eq!(slot3[2], 1.0, ".z is a pure multiply in rotation chain");
        assert_eq!(slot3[3], 1.0, ".w is a pure multiply in rotation chain");
    }

    #[test]
    fn test_cbuf_10_rows_4_6_x_components_pass_through_native_vs() {
        let mut usage = HashMap::new();
        usage.insert(
            "cbuf_10_1_".to_string(),
            [4u32, 5, 6].into_iter().collect(),
        );
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c10 = result.get("cbuf_10_1_").unwrap();
        for slot in [4u64, 5, 6] {
            let row = c10.slot_data.get(&slot).unwrap();
            assert_eq!(
                row[0], 1.0,
                "slot {slot} .x is multiplied into gpr_28/29/31 — must not be zero"
            );
        }
    }

    #[test]
    fn test_cbuf_10_slot1_is_neutral_multiply() {
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [1u32].into_iter().collect());
        let emitter = EmitterDef::default();
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot1 = result.get("cbuf_10_1_").unwrap().slot_data.get(&1).unwrap();
        assert_eq!(slot1[0], 1.0, ".x must not zero gpr_9 in VS chain");
        assert_eq!(slot1[1], 1.0);
        assert_eq!(slot1[2], 1.0, ".z must not zero gpr_2 in VS chain");
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
    fn test_cbuf_9_uv_rotation_slots() {
        let mut emitter = EmitterDef::default();
        emitter.tex_is_rotate = true;
        emitter.tex_scroll_rotation = 0.0;
        emitter.tex_scroll_rotation_add = std::f32::consts::FRAC_PI_2;
        emitter.lifetime = 1.0;

        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [44u32, 45].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 1.0, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        let s44 = c9.slot_data.get(&44).unwrap();
        assert!(s44[0].abs() < 0.01, "cos(π/2)≈0 got {}", s44[0]);
        assert!((s44[1] - (-1.0)).abs() < 0.01, "-sin(π/2)≈-1 got {}", s44[1]);
    }

    #[test]
    fn test_ea_anim_tex_scale_modulates_cbuf_9() {
        let mut emitter = EmitterDef::default();
        emitter.tex_scale_uv = [0.5, 0.25];
        emitter.tex_is_scale = true;
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
    fn test_supplement_hybrid_finalize_slots_for_partial_family_b() {
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] gl_Position";
        assert!(crate::spirv_to_wgsl::is_partial_family_b_billboard_vs(vs));
        let mut usage = extract_cbuf_slots_from_wgsl(vs);
        supplement_hybrid_finalize_slots(&mut usage, vs);
        let slots = usage.get("cbuf_9_1_").unwrap();
        assert!(slots.contains(&46));
        assert!(slots.contains(&47));
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

    /// Prefer Samus export (VS+FS) for bomb slot analysis; fall back to gitignored fixture.
    fn bomb_shader_pair_for_phase0() -> Option<(
        crate::bnsh_shader_integration::EffectShaderPair,
        &'static str,
    )> {
        let (export_pairs, _) =
            crate::bnsh_shader_integration::decode_effect_export_shaders("samus");
        if let Some(pair) = export_pairs.get(&crate::bnsh_shader_integration::BOMB_SHADER_KEY) {
            if pair.vertex.is_some() || pair.fragment.is_some() {
                return Some((pair.clone(), "export"));
            }
        }
        let bytes = crate::bnsh_shader_integration::read_shader_fixture_bytes(
            crate::bnsh_shader_integration::BOMB_SHADER_KEY,
        )?;
        let pair = crate::bnsh_shader_integration::decode_bnsh_bytes(&bytes).ok()?;
        if pair.vertex.is_some() || pair.fragment.is_some() {
            Some((pair, "fixture"))
        } else {
            None
        }
    }

    /// Phase 0: Analyze bomb BNSH fixture WGSL and cross-reference cbuf slot coverage.
    #[test]
    fn test_phase0_analyze_all_shaders() {
        use std::collections::HashMap;

        crate::spirv_to_wgsl::with_test_env("FX_NATIVE_VS_POS", "1", || {
            crate::spirv_to_wgsl::with_test_env("FX_NATIVE_FS", "1", || {
                run_phase0_analyze_all_shaders();
            });
        });
    }

    fn run_phase0_analyze_all_shaders() {
        use std::collections::HashMap;

        let known: HashMap<&str, HashSet<u32>> = [
            ("cbuf_8_1_", documented_cbuf_8_slots()),
            ("cbuf_9_1_", documented_cbuf_9_slots()),
            ("cbuf_10_1_", documented_cbuf_10_slots()),
            ("cbuf_16_1_", documented_cbuf_16_slots()),
            ("cbuf_1_1_", [0u32, 1].into_iter().collect()),
        ]
        .into_iter()
        .collect();

        let Some((pair, source)) = bomb_shader_pair_for_phase0() else {
            eprintln!("[PHASE0] SKIP bomb — set data_root or sync tests/fixtures/shaders");
            return;
        };
        let vs_shader = pair
            .vertex
            .as_ref()
            .or(pair.fragment.as_ref())
            .expect("bomb pair has a stage");
        let (vs_wgsl, _) = crate::spirv_to_wgsl::spirv_to_wgsl(
            vs_shader.spirv.as_slice(),
            if pair.vertex.is_some() {
                naga::ShaderStage::Vertex
            } else {
                naga::ShaderStage::Fragment
            },
            "phase0_bomb_vs",
        )
        .expect("stage wgsl");
        let fs_wgsl = if let Some(fs) = pair.fragment.as_ref() {
            crate::spirv_to_wgsl::spirv_to_wgsl(
                fs.spirv.as_slice(),
                naga::ShaderStage::Fragment,
                "phase0_bomb_fs",
            )
            .expect("fs wgsl")
            .0
        } else {
            String::new()
        };
        let vs_prefixed = crate::spirv_to_wgsl::wire_vertex_simulation_varyings(&vs_wgsl);
        let fs_prefixed = crate::spirv_to_wgsl::wire_extra_tex_fragment_input(
            &crate::spirv_to_wgsl::wire_crossfade_fragment_input(&fs_wgsl, &vs_prefixed),
            &vs_prefixed,
        );
        let patched = crate::spirv_to_wgsl::patch_vertex_wgsl_with_hint(
            &vs_prefixed,
            &fs_prefixed,
            None,
        );
        let usage = cbuf_slot_usage_from_shaders(
            pair.vertex.as_ref().map(|s| s.spirv.as_slice()),
            pair.fragment.as_ref().map(|s| s.spirv.as_slice()),
            &patched,
            &fs_prefixed,
        );

        let dump_path =
            crate::scratch_dirs::workshop_tmp_path("phase0_bomb_vs.wgsl");
        let _ = std::fs::write(&dump_path, &patched);

        let mut all_ok = true;
        eprintln!(
            "\n[PHASE0] === bomb flare ({:#x}, {source}) ===",
            crate::bnsh_shader_integration::BOMB_SHADER_KEY
        );
        for buf in ["cbuf_1_1_", "cbuf_8_1_", "cbuf_9_1_", "cbuf_10_1_", "cbuf_16_1_"] {
            let actual: Vec<u32> = usage
                .get(buf)
                .map(|s| {
                    let mut v: Vec<u32> = s.iter().copied().collect();
                    v.sort();
                    v
                })
                .unwrap_or_default();
            let known_slots: Vec<u32> = known
                .get(buf)
                .map(|s| {
                    let mut v: Vec<u32> = s.iter().copied().collect();
                    v.sort();
                    v
                })
                .unwrap_or_default();
            if actual.is_empty() {
                continue;
            }
            let unknown: Vec<u32> = actual
                .iter()
                .filter(|s| !known_slots.contains(s))
                .copied()
                .collect();
            eprintln!("  {buf}: slots {actual:?}");
            if !unknown.is_empty() {
                eprintln!("    ⚠ UNKNOWN slots: {unknown:?}");
                all_ok = false;
            }
        }

        // Dynamic subdiv slots must be filled when usage includes them.
        if let Some(c9) = usage.get("cbuf_9_1_") {
            let emitter = EmitterDef {
                tex_scale_uv: [0.25, 0.25],
                tex_pat_frame_count: 4,
                ..Default::default()
            };
            let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
            let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
            let data = result.get("cbuf_9_1_").unwrap();
            for slot in [49u32, 50, 51] {
                if c9.contains(&slot) {
                    assert!(
                        data.slot_data.contains_key(&(slot as u64)),
                        "cbuf_9 slot {slot} requested but not filled"
                    );
                }
            }
        }

        assert!(all_ok, "Unknown cbuf slots found — see stderr for details");
    }

    #[test]
    fn test_cbuf9_slots49_51_subdivision_layout() {
        let emitter = EmitterDef {
            tex_scale_uv: [0.25, 0.25],
            ..Default::default()
        };
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [49u32, 50, 51].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        let s49 = c9.slot_data.get(&49).unwrap();
        assert!((s49[0] - 4.0).abs() < 0.01, "cols in .x");
        assert!((s49[2] - 4.0).abs() < 0.01, "subdiv in .z");
    }

    #[test]
    fn test_supplement_cbuf9_dynamic_subdiv_slots() {
        let vs = "main_1(); in_attr6_1 in_attr7_1 cbuf_9_1_._m0_[i32(gpr)]";
        let fs = "";
        let mut usage = extract_cbuf_slots_from_wgsl(vs);
        usage
            .entry("cbuf_9_1_".to_string())
            .or_default()
            .insert(48);
        supplement_cbuf9_dynamic_subdiv_slots(&mut usage, vs, fs);
        let slots = usage.get("cbuf_9_1_").unwrap();
        assert!(slots.contains(&49));
        assert!(slots.contains(&50));
        assert!(slots.contains(&51));
    }

    #[test]
    fn test_cbuf_batch_life_t_neutral_for_life_varying_scroll() {
        let mut emitter = EmitterDef::default();
        emitter.tex_is_rotate = true;
        emitter.tex_scroll_rotation_add = 1.0;
        emitter.lifetime = 2.0;
        // Uniform-age batch keeps the shared normalized life (attr5.w matches batch average).
        assert!((cbuf_batch_life_t(&emitter, 0.75, 0.75, 0.75) - 0.75).abs() < 0.001);
        // Multi-age batch → envelope midpoint (not pinned to draw average alone).
        assert!((cbuf_batch_life_t(&emitter, 0.5, 0.2, 0.8) - 0.5).abs() < 0.001);
        assert!((cbuf_batch_life_t(&emitter, 0.9, 0.2, 0.8) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cbuf10_scroll_uses_batch_life_envelope_not_draw_average() {
        let mut emitter = EmitterDef::default();
        emitter.tex_is_rotate = true;
        emitter.tex_scroll_rotation = 0.0;
        emitter.tex_scroll_rotation_add = std::f32::consts::PI;
        emitter.lifetime = 1.0;
        let mut usage = HashMap::new();
        usage.insert("cbuf_10_1_".to_string(), [4u32, 5].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.75, &Mat4::IDENTITY, None)
            .with_batch_life_range(0.2, 0.8);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c10 = result.get("cbuf_10_1_").unwrap();
        let slot4 = c10.slot_data.get(&4).unwrap();
        // Midpoint life 0.5 → rotation = PI * 0.5; cos embedded in row 4.
        let expected = (std::f32::consts::PI * 0.5).cos();
        assert!(
            (slot4[1] - expected).abs() < 0.05,
            "slot4.y={} expected ~{expected}",
            slot4[1]
        );
    }

    #[test]
    fn test_bomb_fixture_cbuf9_dynamic_subdiv_filled() {
        let Some((pair, source)) = bomb_shader_pair_for_phase0() else {
            eprintln!("SKIP: bomb shader unavailable");
            return;
        };
        let primary = pair
            .vertex
            .as_ref()
            .or(pair.fragment.as_ref())
            .expect("bomb stage");
        let (vs_w, _) = crate::spirv_to_wgsl::spirv_to_wgsl(
            primary.spirv.as_slice(),
            if pair.vertex.is_some() {
                naga::ShaderStage::Vertex
            } else {
                naga::ShaderStage::Fragment
            },
            "bomb_vs",
        )
        .unwrap();
        let fs_w = if let Some(fs) = pair.fragment.as_ref() {
            if pair.vertex.is_some() {
                crate::spirv_to_wgsl::spirv_to_wgsl(
                    fs.spirv.as_slice(),
                    naga::ShaderStage::Fragment,
                    "bomb_fs",
                )
                .unwrap()
                .0
            } else {
                vs_w.clone()
            }
        } else {
            String::new()
        };
        let vs_p = crate::spirv_to_wgsl::wire_vertex_simulation_varyings(&vs_w);
        let usage = cbuf_slot_usage_from_shaders(
            pair.vertex.as_ref().map(|s| s.spirv.as_slice()),
            pair.fragment.as_ref().map(|s| s.spirv.as_slice()),
            &vs_p,
            &fs_w,
        );
        let c9 = usage.get("cbuf_9_1_").cloned().unwrap_or_default();
        eprintln!(
            "[BOMB-CBUF9] source={source} slots={c9:?} dynamic={}",
            wgsl_has_dynamic_cbuf9_slot_index(&vs_p)
        );
        if c9.is_empty() {
            eprintln!("SKIP: no cbuf_9 slots reported for this bomb blob");
            return;
        }
        let emitter = EmitterDef {
            tex_scale_uv: [0.5, 0.5],
            ..Default::default()
        };
        let result = NvnChainEvaluator::evaluate_usage(
            &usage,
            &NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None),
        );
        let Some(data) = result.get("cbuf_9_1_") else {
            panic!("evaluator produced no cbuf_9 data");
        };
        for slot in c9 {
            assert!(
                data.slot_data.contains_key(&(slot as u64)),
                "missing fill for cbuf_9[{slot}]"
            );
        }
    }

    #[test]
    fn force_hybrid_sets_cbuf9_slot127_for_flipbook() {
        let emitter = EmitterDef {
            tex_scale_uv: [0.25, 0.25],
            tex_pat_frame_count: 8,
            ..Default::default()
        };
        let mut data = NvnBufferData::default();
        force_hybrid_billboard_cbuf_defaults(
            &mut data,
            "cbuf_9_1_",
            &Mat4::IDENTITY,
            Vec3::X,
            Vec3::Y,
            Some(FlipbookAtlasCbuf {
                emitter: &emitter,
                life_t: 0.5,
                batch_tex_scale: None,
            }),
        );
        let slot = data.slot_data.get(&127).expect("slot 127 must be filled");
        assert!((slot[0] - 0.25).abs() < 0.001, "slot127.x={}", slot[0]);
        assert!((slot[1] - 0.25).abs() < 0.001, "slot127.y={}", slot[1]);
    }

    #[test]
    fn build_cbuf9_slot127_uses_emitter_atlas_tile_scale() {
        let emitter = EmitterDef {
            tex_scale_uv: [0.5, 0.5],
            tex_pat_frame_count: 4,
            ..Default::default()
        };
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [127u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_9_1_")
            .unwrap()
            .slot_data
            .get(&127)
            .expect("slot 127");
        assert!((slot[0] - 0.5).abs() < 0.001);
        assert!((slot[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn build_cbuf16_slot4_enables_native_fs_colour_branch() {
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_16_1_".to_string(), [4u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_16_1_")
            .unwrap()
            .slot_data
            .get(&4)
            .expect("slot 4");
        // Capture constant: (1, -99999, 1, 0) — .y disables the alpha-test gate.
        assert_eq!(*slot, [1.0, -99999.0, 1.0, 0.0]);
    }

    #[test]
    fn build_cbuf16_slot0_is_game_constant_not_color_scale() {
        // Capture-verified: ColorScale lives in cbuf_9[59].x; bank 16 slot 0 is the
        // fixed (0.5, 0, 0, 0) the game uploads for the dominant FS family.
        let emitter = EmitterDef {
            color_scale: 2.5,
            ..Default::default()
        };
        let mut usage = HashMap::new();
        usage.insert("cbuf_16_1_".to_string(), [0u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_16_1_")
            .unwrap()
            .slot_data
            .get(&0)
            .expect("slot 0");
        assert_eq!(*slot, [0.5, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn build_cbuf9_slot94_disables_fs_life_discard() {
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [94u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_9_1_")
            .unwrap()
            .slot_data
            .get(&94)
            .expect("slot 94");
        assert!(slot[2] < -1.0e5, "life gate .z must disable discard: {}", slot[2]);
    }

    #[test]
    fn build_cbuf1_slot0_float_scale_and_int_flag_refs() {
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_1_1_".to_string(), [0u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_1_1_")
            .unwrap()
            .slot_data
            .get(&0)
            .expect("slot 0");
        assert_eq!(slot[0], 1.0, ".x must be float 1.0 for VS scale chains");
        assert_eq!(
            slot[1].to_bits(),
            1,
            ".y must preserve int 1 bits for render-flag equality"
        );
        assert_eq!(slot[2].to_bits(), 1);
        assert_eq!(slot[3].to_bits(), 1);
    }

    #[test]
    fn build_cbuf1_slot0_draw_path_encodes_flag_bit() {
        let emitter = EmitterDef {
            draw_path: 2,
            ..Default::default()
        };
        let slot0 = nvn_cbuf_1_render_flags_slot0(&emitter);
        assert_eq!(slot0[0], 1.0);
        assert_eq!(slot0[1].to_bits(), 1u32 << 2);
        assert_eq!(slot0[2].to_bits(), 1u32 << 2);
        assert_eq!(slot0[3].to_bits(), 1u32 << 2);
    }

    #[test]
    fn build_cbuf1_slot0_includes_static_flags_and_display_side() {
        let emitter = EmitterDef {
            draw_path: 1,
            flags1: 0x100,
            display_side: DisplaySide::Front,
            ..Default::default()
        };
        let mask = nvn_emitter_render_flag_mask(&emitter);
        assert_eq!(mask, (1u32 << 1) | 0x100 | (1 << 8));
        let slot0 = nvn_cbuf_1_render_flags_slot0(&emitter);
        assert_eq!(slot0[1].to_bits(), mask);
    }

    #[test]
    fn build_cbuf9_slot5_matches_render_flag_mask() {
        let emitter = EmitterDef {
            draw_path: 5,
            ..Default::default()
        };
        let slot5 = nvn_cbuf_9_render_flags_slot5(&emitter);
        assert_eq!(slot5[0], 1.0);
        assert_eq!(slot5[1].to_bits(), 1u32 << 5);
        assert_eq!(slot5[2].to_bits(), 1u32 << 5);
        assert_eq!(slot5[3].to_bits(), 1u32 << 5);
    }

    #[test]
    fn build_cbuf9_slot59_carries_color_scale() {
        // Capture-verified (6 emitters): [59] = [ColorScale, 0, 0, 0]; cbuf_10[0] stays 1.
        let mut emitter = EmitterDef::default();
        emitter.color_scale = 1.4;
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [59u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot59 = result.get("cbuf_9_1_").unwrap().slot_data.get(&59).unwrap();
        assert_eq!(*slot59, [1.4, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn scale_table_carries_per_axis_stretch() {
        // Capture-pinned: flashLine1_b's cbuf_9[96..] rows are (x,y,z,t) with x≠y —
        // the authored per-axis stretch must survive into the table verbatim.
        let emitter = EmitterDef {
            scale_keys: vec![
                ColorKey { frame: 0.0, r: 0.62, g: 0.62, b: 0.62, a: 0.62 },
                ColorKey { frame: 0.42, r: 1.61, g: 1.07, b: 0.688, a: 1.61 },
            ],
            ..Default::default()
        };
        let entries = nvn_scale_table_entries(&emitter);
        assert_eq!(entries[0], [0.62, 0.62, 0.62, 0.0]);
        assert_eq!(entries[1], [1.61, 1.07, 0.688, 0.42]);
    }

    #[test]
    fn build_cbuf9_slots76_78_fill_color1_table() {
        // Capture-pinned (frame_004272_draw_0020): [76..83] is the colour1 keyframe
        // table (r,g,b,t) — NOT flipbook pattern-pair data as previously guessed.
        let emitter = EmitterDef {
            color1: vec![
                ColorKey { frame: 0.0, r: 1.0, g: 0.734, b: 0.603, a: 1.0 },
                ColorKey { frame: 0.14, r: 1.0, g: 0.652, b: 0.571, a: 1.0 },
                ColorKey { frame: 0.28, r: 1.0, g: 0.331, b: 0.0, a: 1.0 },
            ],
            ..Default::default()
        };
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [76u32, 77, 78, 79].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.25, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c9 = result.get("cbuf_9_1_").unwrap();
        assert_eq!(*c9.slot_data.get(&76).unwrap(), [1.0, 0.734, 0.603, 0.0]);
        assert_eq!(*c9.slot_data.get(&77).unwrap(), [1.0, 0.652, 0.571, 0.14]);
        assert_eq!(*c9.slot_data.get(&78).unwrap(), [1.0, 0.331, 0.0, 0.28]);
        // Past-end rows pad (last rgb, i + last time) like the other tables.
        assert_eq!(*c9.slot_data.get(&79).unwrap(), [1.0, 0.331, 0.0, 3.28]);
    }

    #[test]
    fn build_cbuf1_slot1_carries_tex_scale() {
        let emitter = EmitterDef {
            tex_scale_uv: [0.25, 0.5],
            ..Default::default()
        };
        let mut usage = HashMap::new();
        usage.insert("cbuf_1_1_".to_string(), [1u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_1_1_")
            .unwrap()
            .slot_data
            .get(&1)
            .expect("slot 1");
        assert!((slot[0] - 0.25).abs() < 0.001, "tiling .x = tex scale U");
        assert_eq!(slot[1], 0.0, "tiling .y = neutral additive bias");
    }

    #[test]
    fn build_cbuf_by_name_routes_cbuf_1_family() {
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_1_1".to_string(), [0u32, 1].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.0, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let c1 = result.get("cbuf_1_1").expect("evaluator must fill cbuf_1");
        assert!(c1.slot_data.contains_key(&0));
        assert!(c1.slot_data.contains_key(&1));
    }

    #[test]
    fn cbuf_base_kind_does_not_confuse_cbuf_1_with_cbuf_16() {
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_16_1_".to_string(), [4u32].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let slot = result
            .get("cbuf_16_1_")
            .unwrap()
            .slot_data
            .get(&4)
            .expect("slot 4");
        assert!(
            slot[1] < -1.0e4,
            "cbuf_16 must not route through cbuf_1 builder (game gate = -99999)"
        );
    }

    #[test]
    fn build_cbuf9_color_table_fills_only_requested_slots() {
        // Game convention (Ryujinx captures): only the slots the unrolled shader reads are
        // written; unread table slots stay zero. A 3-key shader requests 60..62.
        let emitter = EmitterDef::default();
        let mut usage = HashMap::new();
        usage.insert("cbuf_9_1_".to_string(), [60u32, 61, 62].into_iter().collect());
        let params = NvnChainParams::new(&emitter, 0.5, &Mat4::IDENTITY, None);
        let result = NvnChainEvaluator::evaluate_usage(&usage, &params);
        let data = result.get("cbuf_9_1_").unwrap();
        assert!(data.slot_data.contains_key(&60));
        assert!(data.slot_data.contains_key(&61));
        assert!(data.slot_data.contains_key(&62));
        assert!(!data.slot_data.contains_key(&63), "unrequested table slots stay unset");
        // Default emitter = 1-key table (key at t=0); pads use the game convention
        // w = slot_index + last_real_key_time.
        let kf62 = data.slot_data.get(&62).unwrap();
        assert_eq!(kf62[3], 2.0, "pad entry time must be slot_index + last key time");
    }

    #[test]
    fn force_hybrid_does_not_clobber_evaluator_basis_slots() {
        let mut data = NvnBufferData::default();
        data.set(46, [0.1, 0.2, 0.3, 1.0]);
        data.set(47, [0.0, 0.4, 0.5, 0.6]);
        force_hybrid_billboard_cbuf_defaults(
            &mut data,
            "cbuf_9_1_",
            &Mat4::IDENTITY,
            Vec3::X,
            Vec3::Y,
            None,
        );
        assert_eq!(data.slot_data.get(&46).copied(), Some([0.1, 0.2, 0.3, 1.0]));
        assert_eq!(data.slot_data.get(&47).copied(), Some([0.0, 0.4, 0.5, 0.6]));
    }
}
