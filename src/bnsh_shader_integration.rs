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

    /// Get a summary of the shader (line count, byte count)
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

        format!(
            "WGSL shader: {} lines, {} bytes, entry_point={}{}", 
            self.wgsl_source.lines().count(),
            self.wgsl_source.len(),
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
#[derive(Debug, Clone)]
pub struct EffectShaderPair {
    pub vertex: Option<DecodedShader>,
    pub fragment: Option<DecodedShader>,
    pub compute: Option<DecodedShader>,
}

/// Extract and decode shaders from a PTCL effect file
/// 
/// # Arguments
/// * `ptcl` - Parsed effect file containing shader binaries
/// 
/// # Returns
/// Decoded shaders ready for GPU pipeline creation
/// 
/// # Notes
/// - Uses the real bnsh-decoder from https://github.com/maierfelix/bnsh-decoder
/// - Converts BNSH → SPIR-V → WGSL automatically via spirv-cross
/// - Extracts shader reflection data for bindless texture resolution
/// - shader_binary_1 (GRSN section) is typically vertex/geometry
/// - shader_binary_2 (GRSC section) is typically fragment/compute
pub fn decode_effect_shaders(ptcl: &PtclFile) -> Result<EffectShaderPair> {
    let mut pair = EffectShaderPair {
        vertex: None,
        fragment: None,
        compute: None,
    };

    eprintln!("[BNSH] === decode_effect_shaders START ===");
    eprintln!("[BNSH] shader_binary_1 size: {} bytes", ptcl.shader_binary_1.len());
    eprintln!("[BNSH] shader_binary_2 size: {} bytes", ptcl.shader_binary_2.len());

    // Decode first shader binary (typically vertex)
    if !ptcl.shader_binary_1.is_empty() {
        eprintln!("[BNSH] Attempting to decode shader_binary_1...");
        match BnshDecoder::decode_wgsl_with_index(&ptcl.shader_binary_1, 1) {
            Ok(wgsl_result) => {
                // Extract reflection data from the BNSH binary
                let reflection = extract_shader_reflection(&ptcl.shader_binary_1)
                    .ok()
                    .flatten();

                let shader = DecodedShader {
                    spirv: wgsl_result.spirv,
                    wgsl_source: wgsl_result.wgsl,
                    entry_point: wgsl_result.entry_point.clone(),
                    sampler_count: wgsl_result.sampler_count,
                    uniform_buffer_count: wgsl_result.uniform_buffer_count,
                    reflection,
                };
                
                eprintln!("[BNSH] ✓ Decoded shader 1 via bnsh-decoder: {}", shader.summary());
                
                // Assign based on stage information from decode result
                if wgsl_result.is_fragment {
                    pair.fragment = Some(shader);
                } else {
                    pair.vertex = Some(shader);
                }
            }
            Err(e) => {
                eprintln!("[BNSH] ✗ Failed to decode shader binary 1: {}", e);
                // Continue with other shaders
            }
        }
    } else {
        eprintln!("[BNSH] shader_binary_1 is empty, skipping");
    }

    // Decode second shader binary (typically fragment)
    if !ptcl.shader_binary_2.is_empty() {
        eprintln!("[BNSH] Attempting to decode shader_binary_2...");
        match BnshDecoder::decode_wgsl_with_index(&ptcl.shader_binary_2, 2) {
            Ok(wgsl_result) => {
                // Extract reflection data from the BNSH binary
                let reflection = extract_shader_reflection(&ptcl.shader_binary_2)
                    .ok()
                    .flatten();

                let shader = DecodedShader {
                    spirv: wgsl_result.spirv,
                    wgsl_source: wgsl_result.wgsl,
                    entry_point: wgsl_result.entry_point.clone(),
                    sampler_count: wgsl_result.sampler_count,
                    uniform_buffer_count: wgsl_result.uniform_buffer_count,
                    reflection,
                };
                
                eprintln!("[BNSH] ✓ Decoded shader 2 via bnsh-decoder: {}", shader.summary());
                
                // Assign based on stage information from decode result
                if wgsl_result.is_fragment {
                    pair.fragment = Some(shader);
                } else {
                    pair.vertex = Some(shader);
                }
            }
            Err(e) => {
                eprintln!("[BNSH] ✗ Failed to decode shader binary 2: {}", e);
                // Continue with what we have
            }
        }
    } else {
        eprintln!("[BNSH] shader_binary_2 is empty, skipping");
    }

    // Validate we have at least vertex and fragment
    if pair.vertex.is_none() || pair.fragment.is_none() {
        eprintln!("[BNSH] Warning: Effect file missing vertex or fragment shader");
        if pair.vertex.is_none() {
            eprintln!("  - No vertex shader decoded");
        }
        if pair.fragment.is_none() {
            eprintln!("  - No fragment shader decoded");
        }
    }

    Ok(pair)
}

/// Extract shader reflection data from a BNSH binary
/// 
/// Parses the BNSH structure to find reflection data containing:
/// - Sampler names and their GPU binding slots
/// - Constant buffer information
/// - Driver jump tables for bindless texture resolution
fn extract_shader_reflection(bnsh_binary: &[u8]) -> Result<Option<bnsh_reflection::ShaderStageReflection>> {
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
    let ofs_fragment_reflection = u64::from_le_bytes([
        bnsh_binary[ofs_shader_reflection + 0x20],
        bnsh_binary[ofs_shader_reflection + 0x21],
        bnsh_binary[ofs_shader_reflection + 0x22],
        bnsh_binary[ofs_shader_reflection + 0x23],
        bnsh_binary[ofs_shader_reflection + 0x24],
        bnsh_binary[ofs_shader_reflection + 0x25],
        bnsh_binary[ofs_shader_reflection + 0x26],
        bnsh_binary[ofs_shader_reflection + 0x27],
    ]) as usize;

    eprintln!("[BNSH_REFL] Fragment reflection at {:#x}", ofs_fragment_reflection);

    if ofs_fragment_reflection == 0 {
        eprintln!("[BNSH_REFL] No fragment reflection data");
        return Ok(None);
    }

    // Parse fragment stage reflection using the existing parser
    match bnsh_reflection::parse_shader_stage_reflection(bnsh_binary, ofs_fragment_reflection) {
        Ok(reflection) => {
            eprintln!("[BNSH_REFL] ✓ Successfully extracted fragment reflection");
            Ok(Some(reflection))
        }
        Err(e) => {
            eprintln!("[BNSH_REFL] ✗ Failed to parse fragment reflection: {}", e);
            Ok(None)
        }
    }
}

/// Get summary stats about decoded shaders
pub fn get_shader_stats(pair: &EffectShaderPair) -> ShaderStats {
    let mut stats = ShaderStats::default();
    
    if let Some(shader) = &pair.vertex {
        stats.has_vertex = true;
        stats.vertex_lines = shader.wgsl_source.lines().count();
        stats.vertex_bytes = shader.wgsl_source.len();
        stats.vertex_samplers = shader.sampler_count;
        stats.vertex_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.fragment {
        stats.has_fragment = true;
        stats.fragment_lines = shader.wgsl_source.lines().count();
        stats.fragment_bytes = shader.wgsl_source.len();
        stats.fragment_samplers = shader.sampler_count;
        stats.fragment_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.compute {
        stats.has_compute = true;
        stats.compute_lines = shader.wgsl_source.lines().count();
        stats.compute_bytes = shader.wgsl_source.len();
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
    pub vertex_lines: usize,
    pub fragment_lines: usize,
    pub compute_lines: usize,
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
    pub fn total_lines(&self) -> usize {
        self.vertex_lines + self.fragment_lines + self.compute_lines
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
        assert_eq!(stats.total_lines(), 0);
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
        assert!(summary.contains("WGSL shader"));
        assert!(summary.contains("lines"));
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
    fn test_material_texture_bindings_resolve_empty() {
        let bindings = MaterialTextureBindings::default();
        let reflection = bnsh_reflection::ShaderStageReflection {
            sampler_names: vec!["_col".to_string()],
            constant_buffer_names: vec![],
            texture_names: vec![],
            shader_slots: vec![5],
            index_sampler: 0,
            index_constant_buffer: 1,
        };
        
        let resolved = bindings.resolve_with_reflection(&reflection);
        assert!(resolved.is_empty());
    }
}

