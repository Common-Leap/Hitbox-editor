// Integrates BNSH shader decoding with particle_renderer
// Provides functions to load BNSH shaders from effect files into wgpu pipelines
//
// The C++ BNSH decoder (https://github.com/maierfelix/bnsh-decoder) outputs SPIR-V,
// which we convert to WGSL using spirv-cross for immediate wgpu use without overhead.
//
// For bindless texture resolution, we also extract shader reflection data that contains:
// - Driver jump tables for sampler -> GPU binding slot mapping
// - Material texture bindings for desktop hardware compatibility

use anyhow::Result;
use crate::bnsh_ffi::BnshDecoder;
use crate::bnsh_reflection;
use crate::effects::PtclFile;
use crate::shader_registry::ShaderKey;
use crate::spirv_to_wgsl::{BindingClass, DescriptorInfo};
use std::collections::HashMap;

/// A decoded shader ready for wgpu pipeline creation
/// 
/// Contains both SPIR-V bytes and WGSL source code, plus reflection data
/// for bindless texture resolution on desktop hardware.
#[derive(Debug, Clone)]
pub struct DecodedShader {
    pub spirv: Vec<u8>,        // SPIR-V bytes for direct wgpu use
    pub wgsl_source: String,   // WGSL shader code (fallback)
    pub entry_point: String,   // e.g., "main" or "vs_main"
    pub sampler_count: u32,
    pub uniform_buffer_count: u32,
    /// Shader reflection data: contains sampler names, GPU binding slots, etc.
    pub reflection: Option<bnsh_reflection::ShaderStageReflection>,
}

impl DecodedShader {
    /// Get shader source as bytes for validation or debugging
    #[allow(dead_code)]
    pub fn source_bytes(&self) -> Vec<u8> {
        self.wgsl_source.as_bytes().to_vec()
    }

    /// Get a summary of the shader (SPIR-V word count, entry point)
    pub fn summary(&self) -> String {
        let reflection_info = if let Some(ref refl) = self.reflection {
            format!(
                ", {} samplers, {} cbuffers",
                refl.sampler_names.len(),
                refl.constant_buffer_names.len()
            )
        } else {
            "".to_string()
        };

        let spv_words = self.spirv.len() / 4;
        format!(
            "SPIR-V: {} words, entry_point={}{}", 
            spv_words,
            self.entry_point,
            reflection_info
        )
    }

    /// Extract bindless texture bindings for a specific material
    pub fn resolve_material_bindings(
        &self,
        material_textures: &[(String, u32)], // (texture_name, bntx_index)
    ) -> HashMap<String, u32> {
        if let Some(ref reflection) = self.reflection {
            bnsh_reflection::resolve_material_sampler_bindings(reflection, material_textures)
        } else {
            HashMap::new()
        }
    }
}

/// A pair of vertex/fragment shaders extracted from an effect file
#[derive(Debug, Clone, Default)]
pub struct EffectShaderPair {
    pub vertex: Option<DecodedShader>,
    pub fragment: Option<DecodedShader>,
    pub compute: Option<DecodedShader>,
}

/// Locate raw program sections (magic 0x12345678) inside a BNSH container.
pub fn find_bnsh_bytecode_offsets(bnsh: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    for ii in 0..(bnsh.len() / 4) {
        let off = ii * 4;
        if off + 4 > bnsh.len() {
            break;
        }
        let magic = u32::from_le_bytes([
            bnsh[off],
            bnsh[off + 1],
            bnsh[off + 2],
            bnsh[off + 3],
        ]);
        if magic == 0x1234_5678 {
            offsets.push(off);
        }
    }
    offsets
}

fn decode_single_bnsh_blob(bnsh_data: &[u8], index: u32) -> Result<Option<(DecodedShader, bool)>> {
    match BnshDecoder::decode_wgsl_with_index(bnsh_data, index) {
        Ok(wgsl_result) => {
            let is_fragment = wgsl_result.is_fragment;
            let reflection = extract_shader_reflection(bnsh_data, is_fragment).ok().flatten();
            Ok(Some((
                DecodedShader {
                    spirv: wgsl_result.spirv,
                    wgsl_source: wgsl_result.wgsl,
                    entry_point: wgsl_result.entry_point.clone(),
                    sampler_count: wgsl_result.sampler_count,
                    uniform_buffer_count: wgsl_result.uniform_buffer_count,
                    reflection,
                },
                is_fragment,
            )))
        }
        Err(e) => {
            eprintln!("[BNSH] decode failed (index={index}): {e}");
            Ok(None)
        }
    }
}

fn assign_stage(pair: &mut EffectShaderPair, shader: DecodedShader, is_fragment: bool) {
    if is_fragment {
        if pair.fragment.is_none() {
            pair.fragment = Some(shader);
        }
    } else if pair.vertex.is_none() {
        pair.vertex = Some(shader);
    } else if pair.fragment.is_none() {
        pair.fragment = Some(shader);
    }
}

/// Decode all shader stages from one BNSH binary.
pub fn decode_bnsh_bytes(bnsh_data: &[u8]) -> Result<EffectShaderPair> {
    let mut pair = EffectShaderPair {
        vertex: None,
        fragment: None,
        compute: None,
    };

    if bnsh_data.is_empty() {
        return Ok(pair);
    }

    let section_offsets = find_bnsh_bytecode_offsets(bnsh_data);
    if section_offsets.is_empty() {
        if let Some((shader, is_frag)) = decode_single_bnsh_blob(bnsh_data, 0)? {
            assign_stage(&mut pair, shader, is_frag);
        }
    } else {
        for (i, &off) in section_offsets.iter().enumerate() {
            if pair.vertex.is_some() && pair.fragment.is_some() {
                break;
            }
            let slice = &bnsh_data[off..];
            if let Some((shader, is_frag)) = decode_single_bnsh_blob(slice, i as u32)? {
                assign_stage(&mut pair, shader, is_frag);
            }
        }
    }

    // Fallback: full BNSH container (bnsh-decoder scans for first bytecode section).
    if pair.vertex.is_none() || pair.fragment.is_none() {
        if let Some((shader, is_frag)) = decode_single_bnsh_blob(bnsh_data, 0)? {
            assign_stage(&mut pair, shader, is_frag);
        }
    }

    // NintendoWare convention on a single container: index 1 = VS, 0/2 = FS.
    if pair.vertex.is_none() {
        if let Some((shader, is_frag)) = decode_single_bnsh_blob(bnsh_data, 1)? {
            if !is_frag {
                pair.vertex = Some(shader);
            }
        }
    }
    if pair.fragment.is_none() {
        for idx in [0u32, 2] {
            if let Some((shader, is_frag)) = decode_single_bnsh_blob(bnsh_data, idx)? {
                if is_frag {
                    pair.fragment = Some(shader);
                    break;
                }
            }
        }
    }

    if pair.vertex.is_none() || pair.fragment.is_none() {
        eprintln!(
            "[BNSH] Warning: incomplete pair (vs={}, fs={})",
            pair.vertex.is_some(),
            pair.fragment.is_some()
        );
    }

    Ok(pair)
}

/// Fill missing stages from the effect-wide legacy pair (keeps VS+FS matched).
#[allow(dead_code)]
fn fill_pairs_from_legacy(pairs: &mut HashMap<ShaderKey, EffectShaderPair>, legacy: &EffectShaderPair) {
    if legacy.vertex.is_none() && legacy.fragment.is_none() {
        return;
    }
    for pair in pairs.values_mut() {
        if pair.vertex.is_none() || pair.fragment.is_none() {
            if legacy.vertex.is_some() {
                pair.vertex = legacy.vertex.clone();
            }
            if legacy.fragment.is_some() {
                pair.fragment = legacy.fragment.clone();
            }
        }
    }
}

/// Attach the effect-wide particle vertex shader to FS-only registry entries.
///
/// Each embedded `Shader.bnsh` is typically a fragment variant; the shared
/// effect vertex shader (binary_1) is paired with it — not a fallback, this is
/// how NintendoWare stores per-emitter FS variants.
pub fn pair_registry_shaders(
    pairs: &mut HashMap<ShaderKey, EffectShaderPair>,
    effect_stages: &EffectShaderPair,
) {
    let effect_vs = effect_stages.vertex.as_ref();
    for pair in pairs.values_mut() {
        if pair.vertex.is_none() {
            pair.vertex = effect_vs.cloned();
        }
    }
}

#[allow(dead_code)]
pub fn finalize_shader_pairs(
    pairs: &mut HashMap<ShaderKey, EffectShaderPair>,
    effect_stages: &EffectShaderPair,
) {
    pair_registry_shaders(pairs, effect_stages);
}

/// Fill missing stages by borrowing from other decoded variants in the same effect.
///
/// Deprecated for cross-key FS pooling — kept for tests that decode multi-section blobs.
#[deprecated(note = "use finalize_shader_pairs with canonical legacy pair instead")]
pub fn complete_shader_pairs(pairs: &mut HashMap<ShaderKey, EffectShaderPair>) {
    let mut pool_vs: Vec<DecodedShader> = Vec::new();
    let mut pool_fs: Vec<DecodedShader> = Vec::new();

    for pair in pairs.values() {
        if let Some(vs) = &pair.vertex {
            if !pool_vs.iter().any(|v| v.spirv == vs.spirv) {
                pool_vs.push(vs.clone());
            }
        }
        if let Some(fs) = &pair.fragment {
            if !pool_fs.iter().any(|f| f.spirv == fs.spirv) {
                pool_fs.push(fs.clone());
            }
        }
    }

    if pool_vs.is_empty() && pool_fs.is_empty() {
        return;
    }

    // Prefer the largest stage blob as fallback (usually the full particle shader).
    pool_vs.sort_by_key(|s| std::cmp::Reverse(s.spirv.len()));
    pool_fs.sort_by_key(|s| std::cmp::Reverse(s.spirv.len()));
    let fallback_vs = pool_vs.first().cloned();
    let fallback_fs = pool_fs.first().cloned();

    for pair in pairs.values_mut() {
        if pair.vertex.is_none() {
            pair.vertex = fallback_vs.clone();
        }
        if pair.fragment.is_none() {
            pair.fragment = fallback_fs.clone();
        }
    }
}

/// Decode shaders for a registry key.
pub fn decode_shader_for_key(
    registry: &crate::shader_registry::ShaderRegistry,
    key: ShaderKey,
) -> Result<EffectShaderPair> {
    let resolved = registry.resolve(key, -1);
    let bytes = registry
        .get(resolved)
        .ok_or_else(|| anyhow::anyhow!("No BNSH registered for key {resolved:#x}"))?;
    decode_bnsh_bytes(bytes)
}

/// Decode using legacy binary_1=vertex / binary_2=fragment convention.
pub fn decode_legacy_stage_pair(ptcl: &PtclFile) -> EffectShaderPair {
    let mut pair = EffectShaderPair::default();
    let (b1, b2) = ptcl.shader_registry.legacy_pair();

    if !b1.is_empty() {
        if let Ok(wgsl) = BnshDecoder::decode_wgsl_with_index(&b1, 1) {
            let is_vertex = !wgsl.is_fragment;
            pair.vertex = Some(DecodedShader {
                spirv: wgsl.spirv,
                wgsl_source: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                sampler_count: wgsl.sampler_count,
                uniform_buffer_count: wgsl.uniform_buffer_count,
                reflection: extract_shader_reflection(&b1, !is_vertex).ok().flatten(),
            });
        }
    }

    if !b2.is_empty() && b2 != b1 {
        if let Ok(wgsl) = BnshDecoder::decode_wgsl_with_index(&b2, 2) {
            pair.fragment = Some(DecodedShader {
                spirv: wgsl.spirv,
                wgsl_source: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                sampler_count: wgsl.sampler_count,
                uniform_buffer_count: wgsl.uniform_buffer_count,
                reflection: extract_shader_reflection(&b2, true).ok().flatten(),
            });
        }
    } else if pair.fragment.is_none() {
        if let Ok(wgsl) = BnshDecoder::decode_wgsl_with_index(&b1, 2) {
            pair.fragment = Some(DecodedShader {
                spirv: wgsl.spirv,
                wgsl_source: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                sampler_count: wgsl.sampler_count,
                uniform_buffer_count: wgsl.uniform_buffer_count,
                reflection: extract_shader_reflection(&b1, true).ok().flatten(),
            });
        }
    }

    pair
}

/// Decode all unique shaders in the PTCL registry.
pub fn decode_all_effect_shaders(ptcl: &PtclFile) -> Result<HashMap<ShaderKey, EffectShaderPair>> {
    let legacy = decode_legacy_stage_pair(ptcl);
    let mut out = HashMap::new();
    for (key, bytes) in ptcl.shader_registry.iter() {
        match decode_bnsh_bytes(bytes) {
            Ok(pair) => {
                out.insert(key, pair);
            }
            Err(e) => {
                eprintln!("[BNSH] Failed to decode shader {key:#x}: {e}");
            }
        }
    }
    pair_registry_shaders(&mut out, &legacy);
    Ok(out)
}

/// Extract and decode the default shader pair from a PTCL file.
///
/// Uses the shader registry (all embedded Shader.bnsh) rather than the legacy
/// global binary_1/binary_2 pair from unrelated emitters.
pub fn decode_effect_shaders(ptcl: &PtclFile) -> Result<EffectShaderPair> {
    let default_key = ptcl.shader_registry.default_key();
    if default_key != 0 {
        return decode_shader_for_key(&ptcl.shader_registry, default_key);
    }

    // Fallback: legacy fields
    eprintln!("[BNSH] Registry empty, falling back to shader_binary_1/2");
    let mut pair = EffectShaderPair {
        vertex: None,
        fragment: None,
        compute: None,
    };

    if !ptcl.shader_binary_1.is_empty() {
        let p = decode_bnsh_bytes(&ptcl.shader_binary_1)?;
        if let Some(vs) = p.vertex.or(p.fragment) {
            pair.vertex = Some(vs);
        }
    }
    if !ptcl.shader_binary_2.is_empty() {
        let p = decode_bnsh_bytes(&ptcl.shader_binary_2)?;
        if let Some(fs) = p.fragment.or(p.vertex) {
            pair.fragment = Some(fs);
        }
    }

    Ok(pair)
}

/// Extract shader reflection for a specific stage from a BNSH binary.
fn extract_shader_reflection(
    bnsh_binary: &[u8],
    is_fragment: bool,
) -> Result<Option<bnsh_reflection::ShaderStageReflection>> {
    if bnsh_binary.len() < 0x30 {
        return Ok(None);
    }

    // Check for BNSH magic
    if &bnsh_binary[0..8] != b"BNSH\x00\x00\x00\x00" && &bnsh_binary[0..4] != b"BNSH" {
        eprintln!("[BNSH_REFL] Invalid BNSH magic");
        return Ok(None);
    }

    // Read ofs_first_block from header offset 0x10 (u2)
    if bnsh_binary.len() < 0x12 {
        return Ok(None);
    }
    let ofs_first_block = u16::from_le_bytes([
        bnsh_binary[0x10],
        bnsh_binary[0x11],
    ]) as usize;

    eprintln!("[BNSH_REFL] BNSH file detected, ofs_first_block = {:#x}", ofs_first_block);

    // Find GRSC block by scanning blocks
    let mut block_pos = ofs_first_block;
    let mut grsc_found = false;
    let mut grsc_pos = 0usize;

    while block_pos + 16 < bnsh_binary.len() {
        // Block header: magic (4), ofs_next (u4), block_size (u4), reserved (u4)
        let magic = &bnsh_binary[block_pos..block_pos + 4];
        let ofs_next = u32::from_le_bytes([
            bnsh_binary[block_pos + 4],
            bnsh_binary[block_pos + 5],
            bnsh_binary[block_pos + 6],
            bnsh_binary[block_pos + 7],
        ]) as usize;

        eprintln!("[BNSH_REFL] Found block at {:#x}: {:?}", block_pos, std::str::from_utf8(magic).unwrap_or("?????"));

        if magic == b"grsc" || magic == b"GRSC" {
            grsc_found = true;
            grsc_pos = block_pos + 16; // Block data starts after header
            break;
        }

        if ofs_next == 0 || ofs_next < block_pos {
            break;
        }
        block_pos = ofs_next + 0x60; // Next block header is at ofs_next + 0x60 per BNSH.ksy
    }

    if !grsc_found {
        eprintln!("[BNSH_REFL] GRSC block not found");
        return Ok(None);
    }

    eprintln!("[BNSH_REFL] Found GRSC block at {:#x}", grsc_pos);

    // Parse GRSC block:
    // +0x00: target_api_type (u2)
    // +0x08: shader_variation_count (u4)
    // +0x0C: ofs_shader_variation_array (u8)
    if grsc_pos + 0x14 > bnsh_binary.len() {
        return Ok(None);
    }

    let shader_variation_count = u32::from_le_bytes([
        bnsh_binary[grsc_pos + 0x08],
        bnsh_binary[grsc_pos + 0x09],
        bnsh_binary[grsc_pos + 0x0A],
        bnsh_binary[grsc_pos + 0x0B],
    ]) as usize;

    let ofs_shader_variation_array = u64::from_le_bytes([
        bnsh_binary[grsc_pos + 0x0C],
        bnsh_binary[grsc_pos + 0x0D],
        bnsh_binary[grsc_pos + 0x0E],
        bnsh_binary[grsc_pos + 0x0F],
        bnsh_binary[grsc_pos + 0x10],
        bnsh_binary[grsc_pos + 0x11],
        bnsh_binary[grsc_pos + 0x12],
        bnsh_binary[grsc_pos + 0x13],
    ]) as usize;

    eprintln!("[BNSH_REFL] GRSC: {} shader variations at {:#x}", shader_variation_count, ofs_shader_variation_array);

    if shader_variation_count == 0 || ofs_shader_variation_array + 64 > bnsh_binary.len() {
        return Ok(None);
    }

    // Parse first shader_variation:
    // +0x00: ofs_source_program (u8)
    // +0x08: ofs_intermediate_program (u8)
    // +0x10: ofs_binary_program (u8)
    // +0x18: ofs_parent (u8)
    // +0x20: reserved[0x20]
    let shader_var_pos = ofs_shader_variation_array;

    let ofs_binary_program = u64::from_le_bytes([
        bnsh_binary[shader_var_pos + 0x10],
        bnsh_binary[shader_var_pos + 0x11],
        bnsh_binary[shader_var_pos + 0x12],
        bnsh_binary[shader_var_pos + 0x13],
        bnsh_binary[shader_var_pos + 0x14],
        bnsh_binary[shader_var_pos + 0x15],
        bnsh_binary[shader_var_pos + 0x16],
        bnsh_binary[shader_var_pos + 0x17],
    ]) as usize;

    eprintln!("[BNSH_REFL] Binary program at {:#x}", ofs_binary_program);

    if ofs_binary_program + 0x48 > bnsh_binary.len() {
        return Ok(None);
    }

    // Parse shader_program_data:
    // +0x00: shader_info_data (0x60 bytes)
    // +0x60: object_size (u4)
    // +0x68: ofs_shader_reflection (u8)
    let ofs_shader_reflection = u64::from_le_bytes([
        bnsh_binary[ofs_binary_program + 0x68],
        bnsh_binary[ofs_binary_program + 0x69],
        bnsh_binary[ofs_binary_program + 0x6A],
        bnsh_binary[ofs_binary_program + 0x6B],
        bnsh_binary[ofs_binary_program + 0x6C],
        bnsh_binary[ofs_binary_program + 0x6D],
        bnsh_binary[ofs_binary_program + 0x6E],
        bnsh_binary[ofs_binary_program + 0x6F],
    ]) as usize;

    eprintln!("[BNSH_REFL] Shader reflection data at {:#x}", ofs_shader_reflection);

    if ofs_shader_reflection == 0 || ofs_shader_reflection + 0x48 > bnsh_binary.len() {
        return Ok(None);
    }

    // Parse shader_reflection_data:
    // +0x00: ofs_vertex_reflection (u8)
    // +0x08: ofs_hull_reflection (u8)
    // +0x10: ofs_domain_reflection (u8)
    // +0x18: ofs_geometry_reflection (u8)
    // +0x20: ofs_fragment_reflection (u8)
    // +0x28: ofs_compute_reflection (u8)
    let stage_offset = if is_fragment { 0x20 } else { 0x00 };
    let stage_label = if is_fragment { "fragment" } else { "vertex" };
    let ofs_stage_reflection = u64::from_le_bytes([
        bnsh_binary[ofs_shader_reflection + stage_offset],
        bnsh_binary[ofs_shader_reflection + stage_offset + 1],
        bnsh_binary[ofs_shader_reflection + stage_offset + 2],
        bnsh_binary[ofs_shader_reflection + stage_offset + 3],
        bnsh_binary[ofs_shader_reflection + stage_offset + 4],
        bnsh_binary[ofs_shader_reflection + stage_offset + 5],
        bnsh_binary[ofs_shader_reflection + stage_offset + 6],
        bnsh_binary[ofs_shader_reflection + stage_offset + 7],
    ]) as usize;

    eprintln!(
        "[BNSH_REFL] {} reflection at {:#x}",
        stage_label, ofs_stage_reflection
    );

    if ofs_stage_reflection == 0 {
        eprintln!("[BNSH_REFL] No {} reflection data", stage_label);
        return Ok(None);
    }

    match bnsh_reflection::parse_shader_stage_reflection(bnsh_binary, ofs_stage_reflection) {
        Ok(reflection) => {
            eprintln!(
                "[BNSH_REFL] ✓ Successfully extracted {} reflection",
                stage_label
            );
            Ok(Some(reflection))
        }
        Err(e) => {
            eprintln!(
                "[BNSH_REFL] ✗ Failed to parse {} reflection: {}",
                stage_label, e
            );
            Ok(None)
        }
    }
}

/// Map WGSL descriptor (set, binding) → emitter texture slot (0/1/2).
///
/// Uses BNSH driver jump tables from fragment (preferred) or vertex reflection,
/// cross-referenced with decoded WGSL descriptor bindings. Emitter slot 0 is the
/// primary color texture, 1 is alpha/indirect, 2 is tertiary.
pub fn map_emitter_slots_to_descriptors(
    fs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    vs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    descriptors: &[DescriptorInfo],
) -> HashMap<(u32, u32), u32> {
    let refl = fs_refl.or(vs_refl);
    if let Some(refl) = refl {
        let pairs = refl.build_ordered_texture_pairs();
        if !pairs.is_empty() {
            return map_pairs_to_descriptors(&pairs, descriptors);
        }
    }
    map_descriptors_by_binding_order(descriptors)
}

fn descriptor_set_for_binding(
    descriptors: &[DescriptorInfo],
    binding: u32,
    class: BindingClass,
) -> Option<u32> {
    descriptors
        .iter()
        .find(|d| d.binding == binding && d.class == class)
        .map(|d| d.set)
}

fn map_pairs_to_descriptors(
    pairs: &[(u32, u32)],
    descriptors: &[DescriptorInfo],
) -> HashMap<(u32, u32), u32> {
    let mut out = HashMap::new();
    for (emitter_slot, &(tex_binding, sampler_binding)) in pairs.iter().enumerate() {
        let emitter_slot = emitter_slot as u32;
        let set = descriptor_set_for_binding(descriptors, tex_binding, BindingClass::Texture)
            .or_else(|| descriptor_set_for_binding(descriptors, sampler_binding, BindingClass::Sampler))
            .unwrap_or(0);
        out.insert((set, tex_binding), emitter_slot);
        out.insert((set, sampler_binding), emitter_slot);
    }
    out
}

fn map_descriptors_by_binding_order(descriptors: &[DescriptorInfo]) -> HashMap<(u32, u32), u32> {
    let mut out = HashMap::new();
    let mut textures: Vec<&DescriptorInfo> = descriptors
        .iter()
        .filter(|d| d.class == BindingClass::Texture)
        .collect();
    textures.sort_by_key(|d| (d.set, d.binding));
    for (slot, tex) in textures.iter().take(3).enumerate() {
        let slot = slot as u32;
        out.insert((tex.set, tex.binding), slot);
        if let Some(samp) = descriptors.iter().find(|d| {
            d.set == tex.set
                && d.binding == tex.binding.saturating_add(1)
                && d.class == BindingClass::Sampler
        }) {
            out.insert((samp.set, samp.binding), slot);
        }
    }
    out
}

/// Legacy helper kept for tests — extracts fragment reflection only.
#[cfg(test)]
fn extract_fragment_reflection(bnsh_binary: &[u8]) -> Result<Option<bnsh_reflection::ShaderStageReflection>> {
    extract_shader_reflection(bnsh_binary, true)
}

/// Get summary stats about decoded shaders
pub fn get_shader_stats(pair: &EffectShaderPair) -> ShaderStats {
    let mut stats = ShaderStats::default();
    
    if let Some(shader) = &pair.vertex {
        stats.has_vertex = true;
        stats.vertex_words = shader.spirv.len() / 4;
        stats.vertex_bytes = shader.spirv.len();
        stats.vertex_samplers = shader.sampler_count;
        stats.vertex_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.fragment {
        stats.has_fragment = true;
        stats.fragment_words = shader.spirv.len() / 4;
        stats.fragment_bytes = shader.spirv.len();
        stats.fragment_samplers = shader.sampler_count;
        stats.fragment_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.compute {
        stats.has_compute = true;
        stats.compute_words = shader.spirv.len() / 4;
        stats.compute_bytes = shader.spirv.len();
        stats.compute_samplers = shader.sampler_count;
        stats.compute_buffers = shader.uniform_buffer_count;
    }
    
    stats
}

/// Statistics about decoded shaders
#[derive(Debug, Clone, Default)]
pub struct ShaderStats {
    pub has_vertex: bool,
    pub has_fragment: bool,
    pub has_compute: bool,
    pub vertex_words: usize,
    pub fragment_words: usize,
    pub compute_words: usize,
    pub vertex_bytes: usize,
    pub fragment_bytes: usize,
    pub compute_bytes: usize,
    pub vertex_samplers: u32,
    pub fragment_samplers: u32,
    pub compute_samplers: u32,
    pub vertex_buffers: u32,
    pub fragment_buffers: u32,
    pub compute_buffers: u32,
}

impl ShaderStats {
    #[allow(dead_code)]
    pub fn total_words(&self) -> usize {
        self.vertex_words + self.fragment_words + self.compute_words
    }
    
    #[allow(dead_code)]
    pub fn total_bytes(&self) -> usize {
        self.vertex_bytes + self.fragment_bytes + self.compute_bytes
    }
    
    #[allow(dead_code)]
    pub fn total_samplers(&self) -> u32 {
        self.vertex_samplers + self.fragment_samplers + self.compute_samplers
    }
}

/// Material texture binding information extracted from effect file
/// 
/// Maps shader sampler names to GPU texture slots for bindless resolution.
/// Example: {"col": (slot=5, bntx_idx=10), "nor": (slot=6, bntx_idx=11)}
#[derive(Debug, Clone, Default)]
pub struct MaterialTextureBindings {
    /// Maps sampler name → (GPU binding slot, BNTX texture index)
    pub sampler_bindings: HashMap<String, (u32, u32)>,
    /// Maps emissive sampler name → (GPU binding slot, BNTX texture index)
    pub emissive_bindings: HashMap<String, (u32, u32)>,
    /// Maps PBR params sampler name → (GPU binding slot, BNTX texture index)
    pub pbr_bindings: HashMap<String, (u32, u32)>,
}

impl MaterialTextureBindings {
    /// Extract material texture bindings from effect file
    /// 
    /// Returns bindings for all materials in the BFRES models embedded in the effect.
    /// This allows shaders to resolve material textures to GPU binding slots.
    pub fn from_ptcl_file(ptcl: &PtclFile) -> Self {
        let mut bindings = MaterialTextureBindings::default();
        
        // Extract texture indices from any BFRES models in the effect
        for bfres_model in &ptcl.bfres_models {
            for mesh in &bfres_model.meshes {
                // Build material texture mappings for this mesh
                // Standard texture slots used in Switch materials:
                // - _col (color/albedo) → texture_index
                // - _emi (emissive) → emissive_tex_index
                // - _prm (PBR parameters) → prm_tex_index
                
                if mesh.texture_index != u32::MAX {
                    // Color texture slot found
                    bindings.sampler_bindings.insert(
                        "_col".to_string(),
                        (0, mesh.texture_index), // slot 0, BNTX index
                    );
                }
                
                if mesh.emissive_tex_index != u32::MAX {
                    // Emissive texture slot found
                    bindings.emissive_bindings.insert(
                        "_emi".to_string(),
                        (1, mesh.emissive_tex_index), // slot 1, BNTX index
                    );
                }
                
                if mesh.prm_tex_index != u32::MAX {
                    // PBR params texture slot found
                    bindings.pbr_bindings.insert(
                        "_prm".to_string(),
                        (2, mesh.prm_tex_index), // slot 2, BNTX index
                    );
                }
            }
        }
        
        eprintln!("[MATERIAL_BINDING] Extracted {} color, {} emissive, {} PBR samplers",
            bindings.sampler_bindings.len(),
            bindings.emissive_bindings.len(),
            bindings.pbr_bindings.len());
        
        bindings
    }
    
    /// Resolve material texture bindings using shader reflection data
    /// 
    /// Maps shader sampler names to actual GPU binding slots using reflection
    /// data extracted from BNSH shaders. This enables the GPU to locate material
    /// textures at the correct binding slots.
    pub fn resolve_with_reflection(
        &self,
        reflection: &bnsh_reflection::ShaderStageReflection,
    ) -> HashMap<String, u32> {
        let mut resolved = HashMap::new();
        
        // Build jump table from reflection (sampler_name → GPU slot)
        let sampler_table = reflection.build_sampler_jump_table();
        
        // Map each material texture sampler to its GPU slot
        for (sampler_name, &(_slot, bntx_idx)) in &self.sampler_bindings {
            if let Some(&gpu_slot) = sampler_table.get(sampler_name) {
                resolved.insert(
                    format!("mat_tex_{}_{}", sampler_name, bntx_idx),
                    gpu_slot,
                );
            }
        }
        
        // Add emissive and PBR mappings
        for (sampler_name, &(_slot, bntx_idx)) in &self.emissive_bindings {
            if let Some(&gpu_slot) = sampler_table.get(sampler_name) {
                resolved.insert(
                    format!("mat_tex_{}_{}", sampler_name, bntx_idx),
                    gpu_slot,
                );
            }
        }
        
        for (sampler_name, &(_slot, bntx_idx)) in &self.pbr_bindings {
            if let Some(&gpu_slot) = sampler_table.get(sampler_name) {
                resolved.insert(
                    format!("mat_tex_{}_{}", sampler_name, bntx_idx),
                    gpu_slot,
                );
            }
        }
        
        if !resolved.is_empty() {
            eprintln!("[MATERIAL_BINDING] Resolved {} material texture GPU slots", resolved.len());
        }
        
        resolved
    }
    
    /// Convert material texture bindings to a simple GPU slot map
    /// 
    /// Converts all sampler bindings (color, emissive, PBR) into a flat
    /// HashMap<String, u32> mapping sampler names to their GPU binding slots.
    /// This is used for quick lookup of where material textures should be bound.
    pub fn as_gpu_slots(&self) -> std::collections::HashMap<String, u32> {
        let mut slots = std::collections::HashMap::new();
        
        // Add color samplers (slot 0)
        for (sampler_name, &(gpu_slot, _bntx_idx)) in &self.sampler_bindings {
            slots.insert(sampler_name.clone(), gpu_slot);
        }
        
        // Add emissive samplers (slot 1)
        for (sampler_name, &(gpu_slot, _bntx_idx)) in &self.emissive_bindings {
            slots.insert(sampler_name.clone(), gpu_slot);
        }
        
        // Add PBR samplers (slot 2)
        for (sampler_name, &(gpu_slot, _bntx_idx)) in &self.pbr_bindings {
            slots.insert(sampler_name.clone(), gpu_slot);
        }
        
        slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_per_emitter_pairing_uses_canonical_vs() {
        let pair = EffectShaderPair {
            vertex: None,
            fragment: Some(DecodedShader {
                spirv: vec![0x01, 0x02, 0x03, 0x04],
                wgsl_source: String::new(),
                entry_point: "main".into(),
                sampler_count: 0,
                uniform_buffer_count: 0,
                reflection: None,
            }),
            compute: None,
        };
        let canonical = EffectShaderPair {
            vertex: Some(DecodedShader {
                spirv: vec![0xAA, 0xBB, 0xCC, 0xDD],
                wgsl_source: String::new(),
                entry_point: "vs".into(),
                sampler_count: 0,
                uniform_buffer_count: 0,
                reflection: None,
            }),
            fragment: Some(DecodedShader {
                spirv: vec![0x11, 0x22, 0x33, 0x44],
                wgsl_source: String::new(),
                entry_point: "fs".into(),
                sampler_count: 0,
                uniform_buffer_count: 0,
                reflection: None,
            }),
            compute: None,
        };
        let mut pairs = HashMap::from([(1u64, pair)]);
        finalize_shader_pairs(&mut pairs, &canonical);
        let resolved = pairs.get(&1).unwrap();
        assert_eq!(resolved.vertex.as_ref().unwrap().spirv, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(resolved.fragment.as_ref().unwrap().spirv, vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn test_shader_pair_creation() {
        let pair = EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };
        
        assert!(pair.vertex.is_none());
        assert!(pair.fragment.is_none());
        assert!(pair.compute.is_none());
    }

    #[test]
    fn test_shader_stats() {
        let pair = EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };
        
        let stats = get_shader_stats(&pair);
        assert!(!stats.has_vertex);
        assert!(!stats.has_fragment);
        assert!(!stats.has_compute);
        assert_eq!(stats.total_words(), 0);
    }

    #[test]
    fn test_decoded_shader_summary() {
        let shader = DecodedShader {
            spirv: vec![],
            wgsl_source: "fn main() { }\n".to_string(),
            entry_point: "main".to_string(),
            sampler_count: 1,
            uniform_buffer_count: 2,
            reflection: None,
        };
        
        let summary = shader.summary();
        assert!(summary.contains("SPIR-V"));
        assert!(summary.contains("words"));
        assert!(summary.contains("main"));
    }

    #[test]
    fn test_resolve_material_bindings_no_reflection() {
        let shader = DecodedShader {
            spirv: vec![],
            wgsl_source: "".to_string(),
            entry_point: "main".to_string(),
            sampler_count: 0,
            uniform_buffer_count: 0,
            reflection: None,
        };

        let materials = vec![("col".to_string(), 10)];
        let bindings = shader.resolve_material_bindings(&materials);
        assert!(bindings.is_empty());
    }

    #[test]
    fn test_material_texture_bindings_creation() {
        let bindings = MaterialTextureBindings::default();
        assert!(bindings.sampler_bindings.is_empty());
        assert!(bindings.emissive_bindings.is_empty());
        assert!(bindings.pbr_bindings.is_empty());
    }

    #[test]
    fn test_map_emitter_slots_to_descriptors_from_reflection() {
        let reflection = bnsh_reflection::ShaderStageReflection {
            sampler_names: vec!["s0".into(), "s1".into()],
            shader_slots: vec![2, 6],
            index_sampler: 0,
            ..Default::default()
        };
        let descriptors = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "tex0".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "samp0".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
            DescriptorInfo {
                set: 0,
                binding: 6,
                name: "tex1".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 7,
                name: "samp1".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let map = map_emitter_slots_to_descriptors(Some(&reflection), None, &descriptors);
        assert_eq!(map.get(&(0, 2)), Some(&0));
        assert_eq!(map.get(&(0, 3)), Some(&0));
        assert_eq!(map.get(&(0, 6)), Some(&1));
        assert_eq!(map.get(&(0, 7)), Some(&1));
    }

    #[test]
    fn test_map_emitter_slots_fallback_binding_order() {
        let descriptors = vec![
            DescriptorInfo {
                set: 0,
                binding: 10,
                name: "a".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 11,
                name: "b".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let map = map_emitter_slots_to_descriptors(None, None, &descriptors);
        assert_eq!(map.get(&(0, 10)), Some(&0));
        assert_eq!(map.get(&(0, 11)), Some(&0));
    }

    #[test]
    fn test_material_texture_bindings_resolve_empty() {
        let bindings = MaterialTextureBindings::default();
        let reflection = bnsh_reflection::ShaderStageReflection {
            sampler_names: vec!["_col".to_string()],
            shader_slots: vec![5],
            ..Default::default()
        };
        
        let resolved = bindings.resolve_with_reflection(&reflection);
        assert!(resolved.is_empty());
    }
}

