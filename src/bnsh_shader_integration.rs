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
use crate::effects::{BlendType, ColorKey, EmitterDef, EmitterSet, PtclFile, TextureRes};
use crate::shader_registry::{CombinerState, ShaderKey, ShaderRegistry, ShaderVsProfile};
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

/// Build a BNSH container where `section_index` is the sole bytecode section.
///
/// bnsh-decoder only reads the first `0x12345678` section; dual-section registry blobs
/// store the shared VS in section 0 and the per-key FS variant in section 1.
fn synthetic_bnsh_for_bytecode_section(full: &[u8], section_index: usize) -> Option<Vec<u8>> {
    let offsets = find_bnsh_bytecode_offsets(full);
    let (&first, section_start) = offsets.first().zip(offsets.get(section_index))?;
    let section_end = offsets
        .get(section_index + 1)
        .copied()
        .unwrap_or(full.len());
    Some(
        [full.get(..first)?, full.get(*section_start..section_end)?]
            .concat(),
    )
}

fn decode_bnsh_bytecode_section(
    full: &[u8],
    section_index: usize,
    quiet_failures: bool,
) -> Result<Option<(DecodedShader, bool)>> {
    let Some(synthetic) = synthetic_bnsh_for_bytecode_section(full, section_index) else {
        return Ok(None);
    };
    decode_single_bnsh_blob(&synthetic, full, 0, quiet_failures)
}

fn decode_single_bnsh_blob(
    bytecode_slice: &[u8],
    reflection_container: &[u8],
    index: u32,
    quiet_failures: bool,
) -> Result<Option<(DecodedShader, bool)>> {
    match BnshDecoder::decode_wgsl_with_index(bytecode_slice, index) {
        Ok(wgsl_result) => {
            // SPIR-V OpEntryPoint is authoritative; BNSH JSON often mislabels FS-only registry blobs as VS.
            let is_fragment = crate::spirv_to_wgsl::spirv_is_fragment(&wgsl_result.spirv)
                .unwrap_or(wgsl_result.is_fragment || wgsl_result.sampler_count > 0);
            let reflection = extract_shader_reflection(reflection_container, is_fragment)
                .ok()
                .flatten();
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
            if !quiet_failures {
                eprintln!("[BNSH] decode failed (index={index}): {e}");
            } else if crate::fx_debug_enabled() {
                eprintln!("[BNSH] decode skipped (index={index}): {e}");
            }
            Ok(None)
        }
    }
}

/// Attach stage reflection from the full BNSH container when a stage was decoded without it.
pub fn enrich_pair_reflection_from_container(pair: &mut EffectShaderPair, container: &[u8]) {
    if container.len() < 0x30 {
        return;
    }
    if pair
        .vertex
        .as_ref()
        .is_some_and(|s| s.reflection.is_none())
    {
        if let Ok(Some(r)) = extract_shader_reflection(container, false) {
            if let Some(vs) = &mut pair.vertex {
                vs.reflection = Some(r);
            }
        }
    }
    if pair
        .fragment
        .as_ref()
        .is_some_and(|s| s.reflection.is_none())
    {
        if let Ok(Some(r)) = extract_shader_reflection(container, true) {
            if let Some(fs) = &mut pair.fragment {
                fs.reflection = Some(r);
            }
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
    }
}

/// Move a fragment SPIR-V blob out of the vertex slot when JSON stage inference was wrong.
fn reconcile_misassigned_stages(pair: &mut EffectShaderPair) {
    if pair.fragment.is_some() {
        return;
    }
    let Some(vs) = pair.vertex.take() else {
        return;
    };
    let is_fragment = crate::spirv_to_wgsl::spirv_is_fragment(&vs.spirv)
        .unwrap_or(vs.sampler_count > 0);
    if is_fragment {
        pair.fragment = Some(vs);
    } else {
        pair.vertex = Some(vs);
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

    let section_count = find_bnsh_bytecode_offsets(bnsh_data).len();
    match section_count {
        0 => {
            if let Some((shader, is_frag)) = decode_single_bnsh_blob(bnsh_data, bnsh_data, 0, false)? {
                assign_stage(&mut pair, shader, is_frag);
            }
        }
        1 => {
            if let Some((shader, is_frag)) = decode_bnsh_bytecode_section(bnsh_data, 0, false)? {
                assign_stage(&mut pair, shader, is_frag);
            }
        }
        _ => {
            // Dual-section registry blobs: section 1 is the per-key FS variant.
            if let Some((shader, is_frag)) = decode_bnsh_bytecode_section(bnsh_data, 1, false)? {
                assign_stage(&mut pair, shader, is_frag);
            }
        }
    }

    reconcile_misassigned_stages(&mut pair);
    enrich_pair_reflection_from_container(&mut pair, bnsh_data);

    if pair.vertex.is_none() || pair.fragment.is_none() {
        if crate::fx_debug_enabled() {
            eprintln!(
                "[BNSH] Warning: incomplete pair after decode (vs={}, fs={})",
                pair.vertex.is_some(),
                pair.fragment.is_some()
            );
        }
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

/// Attach the shared effect vertex shader to FS-only registry entries.
pub fn pair_registry_shaders(
    pairs: &mut HashMap<ShaderKey, EffectShaderPair>,
    effect_stages: &EffectShaderPair,
) {
    finalize_shader_pairs(pairs, effect_stages);
}

/// Fill missing vertex stages from the effect-wide legacy pair (`shader_binary_1`).
///
/// Registry entries are FS-only variants; each key keeps its own decoded fragment shader.
/// Missing fragment stages are not pooled — a failed FS decode stays incomplete.
pub fn finalize_shader_pairs(
    pairs: &mut HashMap<ShaderKey, EffectShaderPair>,
    effect_stages: &EffectShaderPair,
) {
    let Some(legacy_vs) = effect_stages.vertex.clone() else {
        return;
    };

    let mut filled_vs = 0usize;
    for pair in pairs.values_mut() {
        if pair.vertex.is_none() {
            pair.vertex = Some(legacy_vs.clone());
            filled_vs += 1;
        }
    }

    if filled_vs > 0 {
        eprintln!(
            "[BNSH] finalize_shader_pairs: filled {} missing VS from effect legacy pair",
            filled_vs
        );
    }
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
            pair.vertex = Some(DecodedShader {
                spirv: wgsl.spirv,
                wgsl_source: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                sampler_count: wgsl.sampler_count,
                uniform_buffer_count: wgsl.uniform_buffer_count,
                reflection: extract_shader_reflection(&b1, false).ok().flatten(),
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
    } else if pair.fragment.is_none() && !b1.is_empty() {
        for idx in [2u32, 0] {
            if let Ok(wgsl) = BnshDecoder::decode_wgsl_with_index(&b1, idx) {
                if wgsl.is_fragment || wgsl.sampler_count > 0 {
                    pair.fragment = Some(DecodedShader {
                        spirv: wgsl.spirv,
                        wgsl_source: wgsl.wgsl,
                        entry_point: wgsl.entry_point,
                        sampler_count: wgsl.sampler_count,
                        uniform_buffer_count: wgsl.uniform_buffer_count,
                        reflection: extract_shader_reflection(&b1, true).ok().flatten(),
                    });
                    break;
                }
            }
        }
    }

    enrich_pair_reflection_from_container(&mut pair, &b1);
    if !b2.is_empty() && b2 != b1 {
        enrich_pair_reflection_from_container(&mut pair, &b2);
    }

    if pair.fragment.is_none() {
        for (_key, bytes) in ptcl.shader_registry.iter() {
            if bytes.is_empty() {
                continue;
            }
            if let Ok(wgsl) = BnshDecoder::decode_wgsl_with_index(bytes, 0) {
                if wgsl.is_fragment || wgsl.sampler_count > 0 {
                    pair.fragment = Some(DecodedShader {
                        spirv: wgsl.spirv,
                        wgsl_source: wgsl.wgsl,
                        entry_point: wgsl.entry_point,
                        sampler_count: wgsl.sampler_count,
                        uniform_buffer_count: wgsl.uniform_buffer_count,
                        reflection: extract_shader_reflection(bytes, true).ok().flatten(),
                    });
                    eprintln!(
                        "[BNSH] legacy pair: found fragment stage in registry ({} samplers)",
                        pair.fragment.as_ref().map(|s| s.sampler_count).unwrap_or(0)
                    );
                    break;
                }
            }
        }
    }

    pair
}

/// Decode all unique shaders in the PTCL registry.
pub fn decode_all_effect_shaders(ptcl: &PtclFile) -> Result<HashMap<ShaderKey, EffectShaderPair>> {
    let legacy = decode_legacy_stage_pair(ptcl);
    let mut out = HashMap::new();
    let mut decode_failures = 0usize;
    for (key, bytes) in ptcl.shader_registry.iter() {
        match decode_bnsh_bytes(bytes) {
            Ok(pair) => {
                out.insert(key, pair);
            }
            Err(e) => {
                decode_failures += 1;
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
    let unique_pipelines: std::collections::HashSet<ShaderKey> =
        out.values().map(spirv_pipeline_key).collect();
    if out.len() > unique_pipelines.len() {
        eprintln!(
            "[BNSH] deduped {} SPIR-V pipeline(s) from {} registry keys ({} unique)",
            out.len() - unique_pipelines.len(),
            out.len(),
            unique_pipelines.len()
        );
    }
    if decode_failures > 0 {
        eprintln!(
            "[BNSH] decode_all_effect_shaders: {} registry key(s) failed to decode",
            decode_failures
        );
    }
    Ok(out)
}

/// Content hash of the decoded VS+FS SPIR-V pair (diagnostics only — GPU cache uses registry keys).
pub fn spirv_pipeline_key(pair: &EffectShaderPair) -> ShaderKey {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if let Some(vs) = &pair.vertex {
        h.update(&vs.spirv);
    }
    if let Some(fs) = &pair.fragment {
        h.update(&fs.spirv);
    }
    u64::from_le_bytes(h.finalize()[0..8].try_into().unwrap())
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

    // Read ofs_first_block from header offset 0x16 (u2); 0x10 is ofs_file_name (u4).
    if bnsh_binary.len() < 0x18 {
        return Ok(None);
    }
    let ofs_first_block = u16::from_le_bytes([
        bnsh_binary[0x16],
        bnsh_binary[0x17],
    ]) as usize;

    if crate::fx_debug_enabled() {
        eprintln!("[BNSH_REFL] BNSH file detected, ofs_first_block = {:#x}", ofs_first_block);
    }

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

        if crate::fx_debug_enabled() {
            eprintln!(
                "[BNSH_REFL] Found block at {:#x}: {:?}",
                block_pos,
                std::str::from_utf8(magic).unwrap_or("?????")
            );
        }

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
        if crate::fx_debug_enabled() {
            eprintln!("[BNSH_REFL] GRSC block not found");
        }
        return Ok(None);
    }

    if crate::fx_debug_enabled() {
        eprintln!("[BNSH_REFL] Found GRSC block at {:#x}", grsc_pos);
    }

    // Parse GRSC block:
    // +0x00: target_api_type (u2)
    // +0x08: compiler_version (4)
    // +0x0C: shader_variation_count (u4)
    // +0x10: ofs_shader_variation_array (u8)
    if grsc_pos + 0x18 > bnsh_binary.len() {
        return Ok(None);
    }

    let shader_variation_count = u32::from_le_bytes([
        bnsh_binary[grsc_pos + 0x0C],
        bnsh_binary[grsc_pos + 0x0D],
        bnsh_binary[grsc_pos + 0x0E],
        bnsh_binary[grsc_pos + 0x0F],
    ]) as usize;

    let ofs_shader_variation_array = u64::from_le_bytes([
        bnsh_binary[grsc_pos + 0x10],
        bnsh_binary[grsc_pos + 0x11],
        bnsh_binary[grsc_pos + 0x12],
        bnsh_binary[grsc_pos + 0x13],
        bnsh_binary[grsc_pos + 0x14],
        bnsh_binary[grsc_pos + 0x15],
        bnsh_binary[grsc_pos + 0x16],
        bnsh_binary[grsc_pos + 0x17],
    ]) as usize;

    if crate::fx_debug_enabled() {
        eprintln!(
            "[BNSH_REFL] GRSC: {} shader variations at {:#x}",
            shader_variation_count,
            ofs_shader_variation_array
        );
    }

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

    if crate::fx_debug_enabled() {
        eprintln!("[BNSH_REFL] Binary program at {:#x}", ofs_binary_program);
    }

    if ofs_binary_program + 0x80 > bnsh_binary.len() {
        return Ok(None);
    }

    // Parse shader_program_data:
    // +0x00: shader_info_data (0x60 bytes)
    // +0x60: object_size (u4)
    // +0x68: ofs_object (u8)
    // +0x70: ofs_parent (u8)
    // +0x78: ofs_shader_reflection (u8)
    let ofs_shader_reflection = u64::from_le_bytes([
        bnsh_binary[ofs_binary_program + 0x78],
        bnsh_binary[ofs_binary_program + 0x79],
        bnsh_binary[ofs_binary_program + 0x7A],
        bnsh_binary[ofs_binary_program + 0x7B],
        bnsh_binary[ofs_binary_program + 0x7C],
        bnsh_binary[ofs_binary_program + 0x7D],
        bnsh_binary[ofs_binary_program + 0x7E],
        bnsh_binary[ofs_binary_program + 0x7F],
    ]) as usize;

    if crate::fx_debug_enabled() {
        eprintln!("[BNSH_REFL] Shader reflection data at {:#x}", ofs_shader_reflection);
    }

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

    if crate::fx_debug_enabled() {
        eprintln!(
            "[BNSH_REFL] {} reflection at {:#x}",
            stage_label,
            ofs_stage_reflection
        );
    }

    if ofs_stage_reflection == 0 {
        if crate::fx_debug_enabled() {
            eprintln!("[BNSH_REFL] No {} reflection data", stage_label);
        }
        return Ok(None);
    }

    match bnsh_reflection::parse_shader_stage_reflection(bnsh_binary, ofs_stage_reflection) {
        Ok(reflection) => {
            if crate::fx_debug_enabled() {
                eprintln!(
                    "[BNSH_REFL] ✓ Successfully extracted {} reflection",
                    stage_label
                );
            }
            Ok(Some(reflection))
        }
        Err(e) => {
            if crate::fx_debug_enabled() {
                eprintln!(
                    "[BNSH_REFL] ✗ Failed to parse {} reflection: {}",
                    stage_label, e
                );
            }
            Ok(None)
        }
    }
}

/// Combined reflection-driven binding map for BNSH draw setup.
#[derive(Debug, Clone, Default)]
pub struct ReflectionBindingMap {
    /// WGSL (set, binding) → emitter texture slot (0/1/2).
    pub emitter_textures: HashMap<(u32, u32), u32>,
    /// WGSL storage binding → BNSH cbuffer dictionary name (via jump table).
    pub storage_cbuf_by_binding: HashMap<u32, String>,
}

/// Build emitter texture + storage cbuffer maps from fragment reflection and WGSL descriptors.
///
/// Particle billboards bind emitter BNTX via FS jump tables only. Missing maps indicate a
/// decode/reflection bug upstream — not something to paper over with VS or binding-order guesses.
pub fn build_reflection_binding_map(
    fs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    descriptors: &[DescriptorInfo],
) -> ReflectionBindingMap {
    let storage_cbuf_by_binding = map_storage_cbuf_bindings(fs_refl, descriptors);
    log_unresolved_cbuffer_bindings(fs_refl, descriptors, &storage_cbuf_by_binding);
    ReflectionBindingMap {
        emitter_textures: map_emitter_slots_to_descriptors(fs_refl, descriptors),
        storage_cbuf_by_binding,
    }
}

/// Log WGSL storage cbuf bindings that the jump table could not resolve (validation only).
fn log_unresolved_cbuffer_bindings(
    fs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    descriptors: &[DescriptorInfo],
    resolved: &HashMap<u32, String>,
) {
    if !crate::fx_debug_enabled() {
        return;
    }
    for d in descriptors {
        if d.class != BindingClass::Storage || !d.name.starts_with("cbuf_") {
            continue;
        }
        if !resolved.contains_key(&d.binding) {
            let dict_hint = fs_refl
                .map(|r| r.constant_buffer_names.len())
                .unwrap_or(0);
            eprintln!(
                "[BNSH-BIND] unresolved cbuf storage binding {} ({}) — jump table has {} dict name(s)",
                d.binding,
                d.name,
                dict_hint
            );
        }
    }
}

/// Map WGSL storage-buffer bindings → cbuffer dictionary names using README jump tables.
pub fn map_storage_cbuf_bindings(
    fs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    descriptors: &[DescriptorInfo],
) -> HashMap<u32, String> {
    fs_refl
        .map(|refl| map_storage_cbuf_bindings_from_reflection(refl, descriptors))
        .unwrap_or_default()
}

fn map_storage_cbuf_bindings_from_reflection(
    refl: &bnsh_reflection::ShaderStageReflection,
    descriptors: &[DescriptorInfo],
) -> HashMap<u32, String> {
    let gpu_to_name: HashMap<u32, String> = refl
        .build_cbuffer_binding_pairs()
        .into_iter()
        .map(|(name, slot)| (slot, name))
        .collect();
    let mut out = HashMap::new();
    for d in descriptors {
        if d.class != BindingClass::Storage {
            continue;
        }
        if let Some(name) = gpu_to_name.get(&d.binding) {
            out.insert(d.binding, name.clone());
        }
    }
    if crate::fx_debug_enabled() && !out.is_empty() {
        eprintln!(
            "[BNSH-BIND] cbuffer jump table: {} storage binding(s) resolved",
            out.len()
        );
    }
    out
}

/// Map WGSL descriptor (set, binding) → emitter texture slot (0/1/2).
///
/// Uses BNSH driver jump tables from fragment reflection cross-referenced with decoded
/// WGSL descriptor bindings. Emitter slot 0 is the primary color texture, 1 is
/// alpha/indirect, 2 is tertiary.
pub fn map_emitter_slots_to_descriptors(
    fs_refl: Option<&bnsh_reflection::ShaderStageReflection>,
    descriptors: &[DescriptorInfo],
) -> HashMap<(u32, u32), u32> {
    fs_refl
        .map(|refl| map_emitter_slots_from_reflection(refl, descriptors))
        .unwrap_or_default()
}

fn map_emitter_slots_from_reflection(
    refl: &bnsh_reflection::ShaderStageReflection,
    descriptors: &[DescriptorInfo],
) -> HashMap<(u32, u32), u32> {
    let pairs = refl.build_sampler_texture_pairs();
    if !pairs.is_empty() {
        return map_named_sampler_pairs_to_descriptors(&pairs, descriptors);
    }
    let ordered = refl.build_ordered_texture_pairs();
    if !ordered.is_empty() {
        return map_pairs_to_descriptors(&ordered, descriptors);
    }
    HashMap::new()
}

fn map_named_sampler_pairs_to_descriptors(
    pairs: &[(String, u32, u32)],
    descriptors: &[DescriptorInfo],
) -> HashMap<(u32, u32), u32> {
    let mut out = HashMap::new();
    for (emitter_slot, (name, tex_binding, sampler_binding)) in pairs.iter().enumerate() {
        let emitter_slot = emitter_slot as u32;
        let set = descriptor_set_for_binding(descriptors, *tex_binding, BindingClass::Texture)
            .or_else(|| {
                descriptor_set_for_binding(descriptors, *sampler_binding, BindingClass::Sampler)
            })
            .unwrap_or(0);
        if crate::fx_debug_enabled() {
            eprintln!(
                "[BNSH-BIND] sampler pair '{}' tex@{} sampler@{} (set {}) -> emitter slot {}",
                name, tex_binding, sampler_binding, set, emitter_slot
            );
        }
        out.insert((set, *tex_binding), emitter_slot);
        out.insert((set, *sampler_binding), emitter_slot);
    }
    out
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
        finalize_shader_pairs(&mut pairs, &EffectShaderPair::default());
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
        finalize_shader_pairs(&mut pairs, &EffectShaderPair::default());
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

fn fixture_texture_res(offset: u32) -> TextureRes {
    TextureRes {
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
    }
}

/// Clone the first emitter in the Samus PTCL whose [`EmitterDef::shader_key`] matches.
fn clone_emitter_for_shader_key(shader_key: ShaderKey) -> Option<EmitterDef> {
    let path = crate::scratch_dirs::resolve_fighter_eff("samus")?;
    let eff = crate::effects::EffIndex::from_file(&path).ok()?;
    let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data).ok()?;
    for set in &ptcl.emitter_sets {
        for em in &set.emitters {
            if em.shader_key == shader_key {
                return Some(em.clone());
            }
        }
    }
    None
}

/// Colour/alpha/combiner defaults for Samus `P_SamusAttackBomb/flare1` (shader `BOMB_SHADER_KEY`).
///
/// The native FS Hermite tables in cbuf_9[60..71] and combiner cbuf_8/cbuf_16 coeffs require
/// real emitter keyframes — an empty [`EmitterDef::default`] yields zero fragment colour.
fn bomb_shader_fixture_emitter(shader_key: ShaderKey, blend: BlendType) -> EmitterDef {
    if let Some(mut em) = clone_emitter_for_shader_key(shader_key) {
        em.blend_type = blend;
        em.textures = vec![fixture_texture_res(0)];
        em.texture_index = 0;
        return em;
    }

    let tex = fixture_texture_res(0);
    EmitterDef {
        name: "flare1".to_string(),
        blend_type: blend,
        texture_index: 0,
        textures: vec![tex],
        shader_key,
        scale: 0.4,
        color_scale: 1.0,
        lifetime: 21.0,
        rot_type: 4,
        offset_type: 2,
        draw_path: 5,
        tex_pat_frame_count: 2,
        tex_pat_frame_table: vec![0, 0],
        tex_pat_frequency: 1.0,
        color0: vec![
            ColorKey { frame: 0.22, r: 1.0, g: 0.7554686, b: 0.2777778, a: 1.0 },
            ColorKey { frame: 0.51, r: 1.0, g: 0.3307085, b: 0.0, a: 1.0 },
            ColorKey { frame: 1.0, r: 0.968254, g: 0.0, b: 0.0, a: 1.0 },
        ],
        color1: vec![
            ColorKey { frame: 0.2, r: 1.0, g: 0.3978494, b: 0.0, a: 1.0 },
            ColorKey { frame: 0.5, r: 0.9365079, g: 0.2654667, b: 0.0, a: 1.0 },
            ColorKey { frame: 0.8, r: 0.7936508, g: 0.0, b: 0.0, a: 1.0 },
        ],
        alpha0_keys: vec![
            ColorKey { frame: 0.0, r: 0.3412699, g: 0.3412699, b: 0.3412699, a: 0.3412699 },
            ColorKey { frame: 0.09, r: 0.3968254, g: 0.3968254, b: 0.3968254, a: 0.3968254 },
            ColorKey { frame: 0.34, r: 0.3730159, g: 0.3730159, b: 0.3730159, a: 0.3730159 },
            ColorKey { frame: 1.0, r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
        ],
        alpha1_keys: vec![ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
        combiner: CombinerState {
            color_combiner_process: 2,
            alpha_combiner_process: 0,
            apply_alpha: 1,
            ..Default::default()
        },
        scale_anim: crate::effects::AnimKey3v4k {
            start_value: 0.45,
            start_diff: 0.85,
            end_diff: -0.7,
            time2: 0.05,
            time3: 0.14,
        },
        ..Default::default()
    }
}

fn synthetic_fixture_emitter(shader_key: ShaderKey, blend: BlendType) -> EmitterDef {
    if shader_key == BOMB_SHADER_KEY {
        bomb_shader_fixture_emitter(shader_key, blend)
    } else {
        EmitterDef {
            name: "fixture_emitter".to_string(),
            blend_type: blend,
            texture_index: 0,
            textures: vec![fixture_texture_res(0)],
            shader_key,
            scale: 40.0,
            color0: vec![ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
            alpha0_keys: vec![ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
            ..Default::default()
        }
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

    let tex = fixture_texture_res(0);
    let emitter = synthetic_fixture_emitter(shader_key, blend);
    shader_registry.note_emitter_native_color(
        shader_key,
        &emitter.combiner,
        &crate::shader_registry::ParticleColorState::default(),
    );
    Ok(PtclFile {
        emitter_sets: vec![EmitterSet {
            name: "fixture_set".to_string(),
            emitters: vec![emitter],
        }],
        texture_section: vec![0xFFu8; 128],
        texture_section_offset: 0,
        bntx_textures: vec![fixture_texture_res(0)],
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
        let fs_wgsl = match crate::spirv_to_wgsl::spirv_to_wgsl(
            &to_bytes(&fs_w),
            naga::ShaderStage::Fragment,
            &format!("link_fs_{label}"),
        ) {
            Ok((w, _)) => w,
            Err(e) => {
                failures.push(format!("{key:#x}: FS SPIR-V→WGSL failed: {e}"));
                continue;
            }
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

/// Sampler count from fragment reflection (authoritative), not bnsh-decoder JSON stats.
pub fn fragment_sampler_count(pair: &EffectShaderPair) -> u32 {
    pair.fragment
        .as_ref()
        .and_then(|s| s.reflection.as_ref())
        .map(|r| r.sampler_names.len() as u32)
        .unwrap_or_else(|| pair.fragment.as_ref().map(|s| s.sampler_count).unwrap_or(0))
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
        let tex_by_sampler: HashMap<String, u32> = reflection
            .build_sampler_texture_pairs()
            .into_iter()
            .map(|(name, tex, _)| (name, tex))
            .collect();

        let mut resolve_map = |bindings: &HashMap<String, (u32, u32)>| {
            for (material_key, &(_slot, bntx_idx)) in bindings {
                if let Some(sampler_name) =
                    bnsh_reflection::match_material_sampler_name(material_key, &reflection.sampler_names)
                {
                    if let Some(&gpu_slot) = tex_by_sampler.get(sampler_name) {
                        resolved.insert(
                            format!("mat_tex_{}_{}", material_key, bntx_idx),
                            gpu_slot,
                        );
                    }
                }
            }
        };

        resolve_map(&self.sampler_bindings);
        resolve_map(&self.emissive_bindings);
        resolve_map(&self.pbr_bindings);
        
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
    fn test_fs_only_registry_blob_assigns_fragment_not_vertex() {
        let Some(bytes) = read_shader_fixture_bytes(BOMB_SHADER_KEY) else {
            eprintln!("Skipping: bomb fixture unavailable");
            return;
        };
        let pair = decode_bnsh_bytes(&bytes).expect("decode should succeed");
        assert!(
            pair.fragment.is_some(),
            "registry blob should decode a fragment stage"
        );
        assert!(
            pair.vertex.is_none(),
            "FS-only registry blob must not populate vertex slot"
        );
        let fs = pair.fragment.as_ref().unwrap();
        assert_eq!(
            crate::spirv_to_wgsl::spirv_is_fragment(&fs.spirv),
            Some(true)
        );
        assert!(
            fs.reflection.is_some(),
            "fragment reflection should be attached via enrich_pair_reflection_from_container"
        );
    }

    #[test]
    fn test_reconcile_misassigned_stages_moves_fragment_out_of_vertex_slot() {
        let fs = DecodedShader {
            spirv: vec![0x01, 0x02, 0x03, 0x04],
            wgsl_source: String::new(),
            entry_point: "main".into(),
            sampler_count: 2,
            uniform_buffer_count: 1,
            reflection: None,
        };
        let mut pair = EffectShaderPair {
            vertex: Some(fs.clone()),
            fragment: None,
            compute: None,
        };
        reconcile_misassigned_stages(&mut pair);
        assert!(pair.vertex.is_none(), "fragment blob must leave vertex slot");
        assert_eq!(pair.fragment.as_ref().unwrap().spirv, fs.spirv);
    }

    #[test]
    fn test_finalize_shader_pairs_does_not_fill_missing_fragment() {
        let fs = DecodedShader {
            spirv: vec![0x01, 0x02, 0x03, 0x04],
            wgsl_source: String::new(),
            entry_point: "main".into(),
            sampler_count: 3,
            uniform_buffer_count: 2,
            reflection: None,
        };
        let vs = DecodedShader {
            spirv: vec![0xAA, 0xBB, 0xCC, 0xDD],
            wgsl_source: String::new(),
            entry_point: "vs".into(),
            sampler_count: 0,
            uniform_buffer_count: 4,
            reflection: None,
        };
        let effect = EffectShaderPair {
            vertex: Some(vs.clone()),
            fragment: Some(fs),
            compute: None,
        };
        let mut pairs = HashMap::from([(
            1u64,
            EffectShaderPair {
                vertex: Some(vs),
                fragment: None,
                compute: None,
            },
        )]);
        finalize_shader_pairs(&mut pairs, &effect);
        assert!(
            pairs.get(&1).unwrap().fragment.is_none(),
            "missing FS must not be mass-filled from legacy pair"
        );
    }

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
    fn test_map_storage_cbuf_bindings_from_reflection() {
        let reflection = bnsh_reflection::ShaderStageReflection {
            constant_buffer_names: vec!["Global".into(), "Material".into()],
            index_constant_buffer: 2,
            index_unordered_access_buffer: 4,
            shader_slots: vec![0, 0, 13, 6],
            ..Default::default()
        };
        let descriptors = vec![
            DescriptorInfo {
                set: 0,
                binding: 13,
                name: "cbuf_8_1_".into(),
                ty_str: "Storage".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 6,
                name: "cbuf_16_1_".into(),
                ty_str: "Storage".into(),
                class: BindingClass::Storage,
            },
        ];
        let map = map_storage_cbuf_bindings(Some(&reflection), &descriptors);
        assert_eq!(map.get(&13), Some(&"Global".to_string()));
        assert_eq!(map.get(&6), Some(&"Material".to_string()));
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
        let map = map_emitter_slots_to_descriptors(Some(&reflection), &descriptors);
        assert_eq!(map.get(&(0, 2)), Some(&0));
        assert_eq!(map.get(&(0, 3)), Some(&0));
        assert_eq!(map.get(&(0, 6)), Some(&1));
        assert_eq!(map.get(&(0, 7)), Some(&1));
    }

    #[test]
    fn test_map_emitter_slots_empty_without_fragment_reflection() {
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
        let map = map_emitter_slots_to_descriptors(None, &descriptors);
        assert!(map.is_empty());
    }

    #[test]
    fn test_map_storage_cbuf_bindings_empty_without_fragment_reflection() {
        let descriptors = vec![DescriptorInfo {
            set: 0,
            binding: 9,
            name: "cbuf_8_1_".into(),
            ty_str: "Storage".into(),
            class: BindingClass::Storage,
        }];
        let map = map_storage_cbuf_bindings(None, &descriptors);
        assert!(map.is_empty());
    }

    #[test]
    fn test_material_texture_bindings_resolve_with_reflection() {
        let mut bindings = MaterialTextureBindings::default();
        bindings
            .sampler_bindings
            .insert("_col".to_string(), (0, 10));
        let reflection = bnsh_reflection::ShaderStageReflection {
            sampler_names: vec!["tex_col".to_string()],
            index_shader_output: 0,
            index_image: 0,
            index_sampler: 1,
            index_constant_buffer: 2,
            index_unordered_access_buffer: 2,
            shader_slots: vec![5, 0],
            ..Default::default()
        };

        let resolved = bindings.resolve_with_reflection(&reflection);
        assert_eq!(resolved.get("mat_tex__col_10"), Some(&5));
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

    #[test]
    fn test_extract_shader_reflection_from_fixture() {
        let dir = shader_fixtures_dir();
        if !dir.is_dir() {
            eprintln!("Skipping: no tests/fixtures/shaders directory");
            return;
        }
        let mut fixture_path = None;
        if let Some(bytes) = read_shader_fixture_bytes(BOMB_SHADER_KEY) {
            fixture_path = dir
                .read_dir()
                .ok()
                .and_then(|rd| {
                    rd.flatten().find_map(|e| {
                        let p = e.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("bnsh")
                            && std::fs::read(&p).ok().as_deref() == Some(bytes.as_slice())
                        {
                            Some(p)
                        } else {
                            None
                        }
                    })
                });
            let fragment = extract_shader_reflection(&bytes, true)
                .expect("reflection parse should not error")
                .expect("fragment reflection should be present");
            assert!(
                !fragment.sampler_names.is_empty() || !fragment.constant_buffer_names.is_empty(),
                "fixture should expose sampler or cbuffer names"
            );
            assert_eq!(
                fragment.shader_slots.len(),
                fragment.index_unordered_access_buffer as usize
            );
            if !fragment.sampler_names.is_empty() {
                let table = fragment.build_sampler_jump_table();
                assert!(
                    table.values().all(|&slot| fragment.shader_slots.contains(&slot)),
                    "sampler GPU slots must come from shader_slots array"
                );
            }
            eprintln!(
                "[TEST] reflection from fixture: {} samplers {:?}, {} slots {:?}",
                fragment.sampler_names.len(),
                fragment.sampler_names,
                fragment.shader_slots.len(),
                fragment.shader_slots
            );
            return;
        }
        // Any local .bnsh fixture is enough when bomb shader is absent.
        let Ok(rd) = std::fs::read_dir(&dir) else {
            eprintln!("Skipping: cannot read shader fixtures");
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("bnsh") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes.len() < 0x20 || &bytes[0..4] != b"BNSH" {
                continue;
            }
            fixture_path = Some(path.clone());
            let fragment = extract_shader_reflection(&bytes, true)
                .expect("reflection parse should not error")
                .expect("fragment reflection should be present");
            assert_eq!(
                fragment.shader_slots.len(),
                fragment.index_unordered_access_buffer as usize
            );
            eprintln!(
                "[TEST] reflection from {}: samplers={:?} slots={:?}",
                path.display(),
                fragment.sampler_names,
                fragment.shader_slots
            );
            return;
        }
        eprintln!("Skipping: no readable .bnsh fixtures under {:?}", dir);
        let _ = fixture_path;
    }

    #[test]
    fn test_extract_reflection_uses_full_container_not_bytecode_slice() {
        let dir = shader_fixtures_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            eprintln!("Skipping: no shader fixtures");
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("bnsh") {
                continue;
            }
            let Ok(full) = std::fs::read(&path) else {
                continue;
            };
            let offsets = find_bnsh_bytecode_offsets(&full);
            if offsets.is_empty() {
                continue;
            }
            let slice = &full[offsets[0]..];
            assert!(
                extract_shader_reflection(slice, true).ok().flatten().is_none(),
                "bytecode slice alone must not yield reflection"
            );
            assert!(
                extract_shader_reflection(&full, true).ok().flatten().is_some(),
                "full BNSH container must yield reflection for {:?}",
                path.file_name()
            );
            return;
        }
        eprintln!("Skipping: no multi-section BNSH fixture for container-vs-slice test");
    }
}

