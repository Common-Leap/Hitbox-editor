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
use crate::effects::{BlendType, EmitterDef, EmitterSet, PtclFile, TextureRes};
use crate::shader_registry::{ShaderKey, ShaderRegistry, ShaderVsProfile};
use crate::spirv_to_wgsl::{BindingClass, DescriptorInfo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    let (mut b1, mut b2) = ptcl.shader_registry.legacy_pair();
    if b1.is_empty() && !ptcl.shader_binary_1.is_empty() {
        b1 = ptcl.shader_binary_1.clone();
    }
    if b2.is_empty() && !ptcl.shader_binary_2.is_empty() {
        b2 = ptcl.shader_binary_2.clone();
    }

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
    if out.is_empty() {
        if legacy.vertex.is_some() || legacy.fragment.is_some() {
            let key = legacy_shader_fallback_key(ptcl, &legacy);
            if key != 0 {
                out.insert(key, legacy);
            }
        }
    }
    Ok(out)
}

/// Synthetic registry key when the PTCL has legacy VS/FS blobs but no embedded Shader.bnsh entries.
fn legacy_shader_fallback_key(ptcl: &PtclFile, legacy: &EffectShaderPair) -> crate::shader_registry::ShaderKey {
    if !ptcl.shader_binary_2.is_empty() {
        return crate::shader_registry::hash_bnsh_key(&ptcl.shader_binary_2);
    }
    if !ptcl.shader_binary_1.is_empty() {
        return crate::shader_registry::hash_bnsh_key(&ptcl.shader_binary_1);
    }
    if let Some(fs) = &legacy.fragment {
        return crate::shader_registry::hash_bnsh_key(&fs.spirv);
    }
    if let Some(vs) = &legacy.vertex {
        return crate::shader_registry::hash_bnsh_key(&vs.spirv);
    }
    0
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
pub fn extract_shader_reflection(
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

/// Best-effort VS profile from BNSH reflection without SPIR-V decode.
pub fn vs_profile_from_bnsh_bytes(bnsh: &[u8]) -> crate::shader_registry::ShaderVsProfile {
    match extract_shader_reflection(bnsh, false) {
        Ok(Some(refl)) => crate::shader_registry::vs_profile_from_reflection(&refl),
        _ => crate::shader_registry::ShaderVsProfile::Unknown,
    }
}

/// Samus bomb flare shader (`P_SamusAttackBomb/flare1`).
pub const BOMB_SHADER_KEY: ShaderKey = 0x5740_678a_2aa5_959f;

/// Gitignored local mirror: `tests/fixtures/shaders/` (auto-populated from export/cache).
pub fn shader_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shaders")
}

fn sanitize_fixture_stem(stem: &str) -> String {
    let mut out: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out = "shader".to_string();
    }
    out
}

fn shader_fixture_path(stem: &str, key: ShaderKey) -> PathBuf {
    shader_fixtures_dir().join(format!(
        "{}_{:016x}.bnsh",
        sanitize_fixture_stem(stem),
        key
    ))
}

fn shader_key_from_fixture_filename(name: &str) -> Option<ShaderKey> {
    let stem = name.strip_suffix(".bnsh")?;
    let hex = stem.rsplit('_').next()?;
    u64::from_str_radix(hex, 16).ok()
}

fn write_shader_fixture(stem: &str, key: ShaderKey, bytes: &[u8]) -> std::io::Result<bool> {
    let dest = shader_fixture_path(stem, key);
    if dest.exists() {
        if let Ok(existing) = std::fs::read(&dest) {
            if crate::shader_registry::hash_bnsh_key(&existing) == key {
                return Ok(false);
            }
        }
    }
    std::fs::create_dir_all(shader_fixtures_dir())?;
    std::fs::write(dest, bytes)?;
    Ok(true)
}

/// Read a synced `.bnsh` from `tests/fixtures/shaders/` by registry key.
pub fn read_shader_fixture_bytes(key: ShaderKey) -> Option<Vec<u8>> {
    let dir = shader_fixtures_dir();
    if !dir.is_dir() {
        return None;
    }
    let suffix = format!("_{key:016x}.bnsh");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return None;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bnsh") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str())?;
        if name.ends_with(&suffix) || shader_key_from_fixture_filename(name) == Some(key) {
            return std::fs::read(path).ok();
        }
    }
    None
}

/// Copy missing shaders into `tests/fixtures/shaders/` from export + PTCL dump cache.
pub fn ensure_shader_fixtures(fighter: &str) -> usize {
    match try_ensure_shader_fixtures(fighter) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[FIXTURE] sync skipped: {e}");
            0
        }
    }
}

fn try_ensure_shader_fixtures(fighter: &str) -> std::io::Result<usize> {
    let mut written = 0;

    if let Some(_eff_path) = crate::scratch_dirs::resolve_fighter_eff(fighter) {
        if let Ok(eff) = crate::effects::EffIndex::from_file(&_eff_path) {
            if let Ok(ptcl) = crate::effects::PtclFile::parse(&eff.ptcl_data) {
                let mut stems: HashMap<ShaderKey, String> = HashMap::new();
                for set in &ptcl.emitter_sets {
                    for em in &set.emitters {
                        if em.shader_key != 0 {
                            stems
                                .entry(em.shader_key)
                                .or_insert_with(|| sanitize_fixture_stem(&em.name));
                        }
                    }
                }
                for (key, bytes) in ptcl.shader_registry.iter() {
                    let stem = stems
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| format!("shader_{key:#x}"));
                    if write_shader_fixture(&stem, key, bytes)? {
                        written += 1;
                        eprintln!("[FIXTURE] wrote {stem}_{key:016x}.bnsh from export");
                    }
                }
            }
        }
    }

    let root = crate::scratch_dirs::effect_dump_cache_root();
    let mut on_shader = |path: &Path| {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        let key = crate::shader_registry::hash_bnsh_key(&bytes);
        let stem = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(sanitize_fixture_stem)
            .unwrap_or_else(|| format!("shader_{key:#x}"));
        if let Ok(true) = write_shader_fixture(&stem, key, &bytes) {
            written += 1;
            eprintln!("[FIXTURE] wrote {stem}_{key:016x}.bnsh from cache");
        }
    };
    crate::scratch_dirs::walk_files_named(&root, "Shader.bnsh", &mut on_shader);

    Ok(written)
}

/// Number of `.bnsh` files under [`shader_fixtures_dir`] (no decode).
pub fn count_shader_fixtures_on_disk() -> usize {
    let dir = shader_fixtures_dir();
    if !dir.is_dir() {
        return 0;
    }
    std::fs::read_dir(&dir)
        .ok()
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        == Some("bnsh")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Shader registry entry count from the fighter `.eff` export (no BNSH decode).
pub fn shader_registry_entry_count(fighter: &str) -> Option<usize> {
    let eff_path = crate::scratch_dirs::resolve_fighter_eff(fighter)?;
    let eff = crate::effects::EffIndex::from_file(&eff_path).ok()?;
    let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data).ok()?;
    Some(ptcl.shader_registry.len())
}

/// `(registry_entries, fixtures_on_disk_for_registry_keys)` without decoding BNSH.
pub fn registry_fixture_coverage(fighter: &str) -> Option<(usize, usize)> {
    let eff_path = crate::scratch_dirs::resolve_fighter_eff(fighter)?;
    let eff = crate::effects::EffIndex::from_file(&eff_path).ok()?;
    let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data).ok()?;
    let total = ptcl.shader_registry.len();
    let present = ptcl
        .shader_registry
        .iter()
        .filter(|(k, _)| read_shader_fixture_bytes(*k).is_some())
        .count();
    Some((total, present))
}

/// Decode every synced `.bnsh` under `tests/fixtures/shaders/`.
pub fn decode_shader_fixtures(fighter: &str) -> (HashMap<ShaderKey, EffectShaderPair>, HashMap<ShaderKey, String>) {
    ensure_shader_fixtures(fighter);
    let mut pairs = HashMap::new();
    let mut labels = HashMap::new();
    let dir = shader_fixtures_dir();
    if !dir.is_dir() {
        return (pairs, labels);
    }
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return (pairs, labels);
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bnsh") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let key = crate::shader_registry::hash_bnsh_key(&bytes);
        match decode_bnsh_bytes(&bytes) {
            Ok(pair) => {
                labels.insert(key, path.display().to_string());
                pairs.insert(key, pair);
            }
            Err(e) => {
                eprintln!(
                    "[FIXTURE] decode failed {:?}: {e}",
                    path.file_name().unwrap_or_default()
                );
            }
        }
    }
    if !pairs.is_empty() {
        #[allow(deprecated)]
        {
            complete_shader_pairs(&mut pairs);
        }
    }
    (pairs, labels)
}

/// Decode shader pairs embedded in a fighter `.eff` from the configured effect export.
pub fn decode_shaders_from_fighter_eff(fighter: &str) -> Option<HashMap<ShaderKey, EffectShaderPair>> {
    let eff_path = crate::scratch_dirs::resolve_fighter_eff(fighter)?;
    let eff = crate::effects::EffIndex::from_file(&eff_path).ok()?;
    let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data).ok()?;
    decode_all_effect_shaders(&ptcl).ok()
}

/// Decode standalone `Shader.bnsh` files from the EffectConverter PTCL dump cache.
pub fn decode_cached_dump_shaders() -> (HashMap<ShaderKey, EffectShaderPair>, HashMap<ShaderKey, String>) {
    let root = crate::scratch_dirs::effect_dump_cache_root();
    let mut pairs = HashMap::new();
    let mut labels = HashMap::new();
    let mut on_shader = |path: &Path| {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        let key = crate::shader_registry::hash_bnsh_key(&bytes);
        if pairs.contains_key(&key) {
            return;
        }
        match decode_bnsh_bytes(&bytes) {
            Ok(pair) => {
                labels.insert(key, path.display().to_string());
                pairs.insert(key, pair);
            }
            Err(e) => {
                eprintln!("[BNSH] cache decode failed {}: {e}", path.display());
            }
        }
    };
    crate::scratch_dirs::walk_files_named(&root, "Shader.bnsh", &mut on_shader);
    if !pairs.is_empty() {
        #[allow(deprecated)]
        {
            complete_shader_pairs(&mut pairs);
        }
    }
    (pairs, labels)
}

/// Merge shaders from a fighter `.eff`, PTCL dump cache, and local fixture mirror.
pub fn decode_effect_export_shaders(fighter: &str) -> (HashMap<ShaderKey, EffectShaderPair>, HashMap<ShaderKey, String>) {
    ensure_shader_fixtures(fighter);
    let mut labels = HashMap::new();
    let mut pairs = decode_shaders_from_fighter_eff(fighter).unwrap_or_default();
    if let Some(eff_path) = crate::scratch_dirs::resolve_fighter_eff(fighter) {
        for key in pairs.keys().copied().collect::<Vec<_>>() {
            labels.insert(key, eff_path.display().to_string());
        }
    }
    let (cache_pairs, cache_labels) = decode_cached_dump_shaders();
    for (key, pair) in cache_pairs {
        pairs.entry(key).or_insert(pair);
        labels.entry(key).or_insert_with(|| {
            cache_labels
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("cache:{key:#x}"))
        });
    }
    let (fixture_pairs, fixture_labels) = decode_shader_fixtures(fighter);
    for (key, pair) in fixture_pairs {
        pairs.entry(key).or_insert(pair);
        labels.entry(key).or_insert_with(|| {
            fixture_labels
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fixture:{key:#x}"))
        });
    }
    (pairs, labels)
}

/// Raw BNSH bytes for `shader_key` from fixtures, effect export, or PTCL dump cache.
pub fn shader_bnsh_bytes_from_export(shader_key: ShaderKey) -> Result<Vec<u8>> {
    ensure_shader_fixtures("samus");
    if let Some(bytes) = read_shader_fixture_bytes(shader_key) {
        return Ok(bytes);
    }
    for fighter in ["samus", "mario"] {
        if let Some(path) = crate::scratch_dirs::resolve_fighter_eff(fighter) {
            if let Ok(eff) = crate::effects::EffIndex::from_file(&path) {
                if let Ok(ptcl) = crate::effects::PtclFile::parse(&eff.ptcl_data) {
                    if let Some(bytes) = ptcl.shader_registry.get(shader_key) {
                        return Ok(bytes.to_vec());
                    }
                }
            }
        }
    }
    let (pairs, _) = decode_cached_dump_shaders();
    if pairs.contains_key(&shader_key) {
        let root = crate::scratch_dirs::effect_dump_cache_root();
        let mut found = None;
        let mut on_shader = |path: &Path| {
            if found.is_some() {
                return;
            }
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            if crate::shader_registry::hash_bnsh_key(&bytes) == shader_key {
                found = Some(bytes);
            }
        };
        crate::scratch_dirs::walk_files_named(&root, "Shader.bnsh", &mut on_shader);
        if let Some(bytes) = found {
            return Ok(bytes);
        }
    }
    anyhow::bail!(
        "shader {shader_key:#x} not found — set editor data root or HITBOX_EFFECT_EXPORT"
    )
}

/// Per-shader VS→FS stage-link validation outcome.
#[derive(Debug, Clone)]
pub struct ShaderLinkResult {
    pub key: ShaderKey,
    pub fixture: Option<String>,
    pub source: ShaderLinkSource,
    pub ok: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderLinkSource {
    Export,
    Cache,
}

/// Aggregate report for CI shader-link coverage.
#[derive(Debug, Clone, Default)]
pub struct ShaderLinkCoverageReport {
    pub export_files: usize,
    pub export_pairs: usize,
    pub cache_extension_pairs: usize,
    pub validated: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<ShaderLinkResult>,
}

impl ShaderLinkCoverageReport {
    pub fn summary_line(&self) -> String {
        format!(
            "[SHADER-LINK] export={}/{} validated={} passed={} failed={} cache_ext={}",
            self.export_pairs,
            self.export_files,
            self.validated,
            self.passed,
            self.failed,
            self.cache_extension_pairs
        )
    }

    pub fn assert_all_passed(&self) {
        if self.failed == 0 {
            return;
        }
        for r in self.results.iter().filter(|r| !r.ok).take(20) {
            let label = r
                .fixture
                .as_deref()
                .unwrap_or_else(|| match r.source {
                    ShaderLinkSource::Export => "export",
                    ShaderLinkSource::Cache => "cache",
                });
            eprintln!(
                "[SHADER-LINK] FAIL {label} ({:#x}): {}",
                r.key,
                r.detail.as_deref().unwrap_or("link failed")
            );
        }
        panic!(
            "{} shader(s) failed stage linking (showing up to 20); {}",
            self.failed,
            self.summary_line()
        );
    }
}

/// Validate every shader from the effect export and optional cache extensions.
pub fn shader_link_coverage_report(
    export_pairs: &HashMap<ShaderKey, EffectShaderPair>,
    export_labels: &HashMap<ShaderKey, String>,
    cache_pairs: &HashMap<ShaderKey, EffectShaderPair>,
    cache_labels: &HashMap<ShaderKey, String>,
) -> ShaderLinkCoverageReport {
    let export_files = export_labels.len();
    let export_keys: std::collections::HashSet<ShaderKey> = export_pairs.keys().copied().collect();
    let cache_extension_pairs = cache_pairs
        .keys()
        .filter(|k| !export_keys.contains(k))
        .count();

    let mut merged = export_pairs.clone();
    merged.extend(cache_pairs.iter().map(|(&k, v)| (k, v.clone())));
    merged.retain(|_, pair| pair.vertex.is_some() && pair.fragment.is_some());

    let failures = shader_stage_link_failures(&merged);
    let failure_by_key: HashMap<ShaderKey, String> = failures
        .into_iter()
        .filter_map(|msg| {
            let key_str = msg.split(':').next()?.trim();
            let key = u64::from_str_radix(key_str.trim_start_matches("0x"), 16).ok()?;
            let detail = msg.split_once(':').map(|(_, d)| d.trim().to_string());
            Some((key, detail.unwrap_or_else(|| msg.clone())))
        })
        .collect();

    let mut results = Vec::new();
    for (&key, pair) in &merged {
        let source = if export_keys.contains(&key) {
            ShaderLinkSource::Export
        } else {
            ShaderLinkSource::Cache
        };
        let fixture = export_labels
            .get(&key)
            .or_else(|| cache_labels.get(&key))
            .cloned();
        let (ok, detail) = if pair.vertex.is_none() || pair.fragment.is_none() {
            (false, Some("incomplete decode".into()))
        } else if let Some(err) = failure_by_key.get(&key) {
            (false, Some(err.clone()))
        } else {
            (true, None)
        };
        results.push(ShaderLinkResult {
            key,
            fixture,
            source,
            ok,
            detail,
        });
    }
    results.sort_by_key(|r| (r.source as u8, r.fixture.clone().unwrap_or_default(), r.key));

    let passed = results.iter().filter(|r| r.ok).count();
    let failed = results.len() - passed;
    ShaderLinkCoverageReport {
        export_files,
        export_pairs: export_pairs.len(),
        cache_extension_pairs,
        validated: results.len(),
        passed,
        failed,
        results,
    }
}

/// Build a minimal [`PtclFile`] for GPU tests: one billboard emitter bound to a shader
/// from the configured effect export or PTCL dump cache.
pub fn synthetic_ptcl_from_shader_key(shader_key: ShaderKey, blend: BlendType) -> Result<PtclFile> {
    let bnsh = shader_bnsh_bytes_from_export(shader_key)?;
    let mut shader_registry = ShaderRegistry::default();
    let registered = shader_registry.register(bnsh);
    if registered != shader_key {
        anyhow::bail!(
            "export shader hash {registered:#x} != expected {shader_key:#x}"
        );
    }
    shader_registry.set_vs_profile(shader_key, ShaderVsProfile::ParticleBillboard);

    let make_tex = |offset: u32| TextureRes {
        tex_name: "fixture_col".to_string(),
        width: 4,
        height: 4,
        ftx_format: 0x0B06,
        ftx_data_offset: offset,
        ftx_data_size: 64,
        original_format: 0x0B06,
        original_data_offset: offset,
        original_data_size: 64,
        wrap_mode: 1,
        filter_mode: 0,
        mipmap_count: 1,
        channel_swizzle: 0,
    };
    let tex = make_tex(0);
    let emitter = EmitterDef {
        name: "fixture_emitter".to_string(),
        blend_type: blend,
        texture_index: 0,
        textures: vec![tex],
        shader_key,
        scale: 40.0,
        ..Default::default()
    };
    Ok(PtclFile {
        emitter_sets: vec![EmitterSet {
            name: "fixture_set".to_string(),
            emitters: vec![emitter],
        }],
        texture_section: vec![0xFFu8; 128],
        texture_section_offset: 0,
        bntx_textures: vec![make_tex(0)],
        primitives: vec![],
        bfres_models: vec![],
        shader_registry,
        shader_binary_1: vec![],
        shader_binary_2: vec![],
    })
}

/// Return human-readable VS→FS interface link failures for decoded shader pairs.
pub fn shader_stage_link_failures(pairs: &HashMap<ShaderKey, EffectShaderPair>) -> Vec<String> {
    use crate::spirv_to_wgsl::{
        fragment_input_locations, patch_vertex_wgsl, vertex_return_wires_fs_inputs,
    };

    let mut failures = Vec::new();
    for (&key, pair) in pairs {
        if pair.vertex.is_none() || pair.fragment.is_none() {
            failures.push(format!("{key:#x}: incomplete decode"));
            continue;
        }
        let label = format!("{key:#x}");
        let vs = pair.vertex.as_ref().unwrap();
        let fs = pair.fragment.as_ref().unwrap();
        let Ok(mut vs_w) = crate::spirv_to_wgsl::bytes_to_words(&vs.spirv) else {
            failures.push(format!("{key:#x}: invalid VS SPIR-V"));
            continue;
        };
        let Ok(mut fs_w) = crate::spirv_to_wgsl::bytes_to_words(&fs.spirv) else {
            failures.push(format!("{key:#x}: invalid FS SPIR-V"));
            continue;
        };
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
        let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let Ok((vs_wgsl, _)) = crate::spirv_to_wgsl::spirv_to_wgsl(
            &to_bytes(&vs_w),
            naga::ShaderStage::Vertex,
            &format!("link_vs_{label}"),
        ) else {
            failures.push(format!("{key:#x}: VS SPIR-V→WGSL failed"));
            continue;
        };
        let Ok((fs_wgsl, _)) = crate::spirv_to_wgsl::spirv_to_wgsl(
            &to_bytes(&fs_w),
            naga::ShaderStage::Fragment,
            &format!("link_fs_{label}"),
        ) else {
            failures.push(format!("{key:#x}: FS SPIR-V→WGSL failed"));
            continue;
        };
        let patched = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
        for loc in fragment_input_locations(&fs_wgsl) {
            let needle = format!("@location({loc})");
            if !patched.contains(&needle) {
                failures.push(format!("{key:#x}: VS missing output {needle}"));
            }
        }
        if !vertex_return_wires_fs_inputs(&patched, &fs_wgsl) {
            failures.push(format!("{key:#x}: return VertexOutput missing FS varyings"));
        }
    }
    failures
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
    fn test_sync_shader_fixtures_from_export() {
        let written = ensure_shader_fixtures("samus");
        if written > 0 {
            eprintln!("[FIXTURE] synced {written} new shader(s)");
        }
        if read_shader_fixture_bytes(BOMB_SHADER_KEY).is_none() {
            eprintln!("Skipping: bomb fixture unavailable (set data_root or HITBOX_EFFECT_EXPORT)");
            return;
        }
        assert!(shader_fixtures_dir().is_dir());
    }

    #[test]
    fn test_effect_export_shaders_decode_and_link() {
        let (export_pairs, _) = decode_effect_export_shaders("samus");
        let complete: HashMap<_, _> = export_pairs
            .into_iter()
            .filter(|(_, p)| p.vertex.is_some() && p.fragment.is_some())
            .collect();
        if complete.is_empty() {
            eprintln!(
                "Skipping: no decodable Samus shaders (set data_root / HITBOX_EFFECT_EXPORT, or HITBOX_EFFECT_TMP)"
            );
            return;
        }
        assert!(
            complete.contains_key(&BOMB_SHADER_KEY),
            "bomb shader ({:#x}) must decode from Samus export",
            BOMB_SHADER_KEY
        );
        let failures = shader_stage_link_failures(&complete);
        assert!(
            failures.is_empty(),
            "export link failures: {}",
            failures.join("; ")
        );
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

