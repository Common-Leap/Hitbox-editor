// BNSH Shader Reflection: Extract driver jump tables for bindless texture resolution
// Based on BNSH.ksy specification: https://github.com/maierfelix/bnsh-decoder/blob/master/BNSH.ksy
//
// For games like LGPE that use bindless textures in most shaders, we need to resolve
// the driver jump table which associates material textures with their GPU binding slots.

use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Parsed reflection data from a single shader stage (vertex, fragment, compute, etc.)
#[derive(Debug, Clone, Default)]
pub struct ShaderStageReflection {
    /// Sampler names extracted from the sampler dictionary
    pub sampler_names: Vec<String>,
    /// Constant buffer names from the constant_buffer dictionary
    pub constant_buffer_names: Vec<String>,
    /// Texture names (if present)
    #[allow(dead_code)]
    pub texture_names: Vec<String>,
    /// Shader slot array: GPU binding slots for each resource
    pub shader_slots: Vec<u32>,
    /// Index into shader_slots for first sampler
    pub index_sampler: u32,
    /// Index into shader_slots for first constant buffer
    pub index_constant_buffer: u32,
}

impl ShaderStageReflection {
    /// Build the driver jump table for samplers: maps sampler name -> GPU binding slot
    #[allow(dead_code)]
    pub fn build_sampler_jump_table(&self) -> HashMap<String, u32> {
        let mut table = HashMap::new();
        for (sampler_idx, sampler_name) in self.sampler_names.iter().enumerate() {
            let slot_idx = self.index_sampler as usize + sampler_idx;
            if slot_idx < self.shader_slots.len() {
                let gpu_slot = self.shader_slots[slot_idx];
                table.insert(sampler_name.clone(), gpu_slot);
            }
        }
        table
    }

    /// Build the driver jump table for constant buffers
    #[allow(dead_code)]
    pub fn build_cbuffer_jump_table(&self) -> HashMap<String, u32> {
        let mut table = HashMap::new();
        for (cbuf_idx, cbuf_name) in self.constant_buffer_names.iter().enumerate() {
            let slot_idx = self.index_constant_buffer as usize + cbuf_idx;
            if slot_idx < self.shader_slots.len() {
                let gpu_slot = self.shader_slots[slot_idx];
                table.insert(cbuf_name.clone(), gpu_slot);
            }
        }
        table
    }
}

/// Parse a dictionary from BNSH reflection data
/// Dictionary format: magic "_DIC" + str_count + padding + strings
#[allow(dead_code)]
fn parse_dictionary(data: &[u8], ofs_entry: usize) -> Result<Vec<String>> {
    if ofs_entry == 0 {
        return Ok(Vec::new());
    }
    if ofs_entry + 0x20 > data.len() {
        return Err(anyhow!("Dictionary offset {:#x} out of bounds", ofs_entry));
    }

    // Check magic "_DIC"
    if &data[ofs_entry..ofs_entry + 4] != b"_DIC" {
        return Err(anyhow!("Dictionary magic not found at {:#x}", ofs_entry));
    }

    let str_count = u32::from_le_bytes([
        data[ofs_entry + 4],
        data[ofs_entry + 5],
        data[ofs_entry + 6],
        data[ofs_entry + 7],
    ]) as usize;

    let mut strings = Vec::new();
    let mut str_offset = ofs_entry + 0x20; // Dictionary entries start at ofs_entry + 0x20

    for _ in 0..str_count.min(512) {
        // str_entry: ofs_str (u4) + padding (u4) + unk1 (u4) + unk2 (u4)
        if str_offset + 16 > data.len() {
            break;
        }

        let ofs_str = u32::from_le_bytes([
            data[str_offset],
            data[str_offset + 1],
            data[str_offset + 2],
            data[str_offset + 3],
        ]) as usize;

        str_offset += 16;

        // Read the string at ofs_str
        if ofs_str + 2 > data.len() {
            continue;
        }

        let str_len = u16::from_le_bytes([data[ofs_str], data[ofs_str + 1]]) as usize;
        if ofs_str + 2 + str_len > data.len() {
            continue;
        }

        match String::from_utf8(data[ofs_str + 2..ofs_str + 2 + str_len].to_vec()) {
            Ok(s) => strings.push(s),
            Err(_) => strings.push(format!("?invalid_string_{}", strings.len())),
        }
    }

    Ok(strings)
}

/// Parse shader reflection data from a single stage (vertex, fragment, etc.)
#[allow(dead_code)]
pub fn parse_shader_stage_reflection(data: &[u8], ofs_reflection: usize) -> Result<ShaderStageReflection> {
    if ofs_reflection == 0 {
        return Ok(ShaderStageReflection::default());
    }
    if ofs_reflection + 0x50 > data.len() {
        return Err(anyhow!(
            "Shader reflection offset {:#x} too close to end of data",
            ofs_reflection
        ));
    }

    // shader_reflection_stage_data layout:
    // +0x00: ofs_shader_input_dictionary (u8)
    // +0x08: ofs_shader_output_dictionary (u8)
    // +0x10: ofs_sampler_dictionary (u8)
    // +0x18: ofs_constant_buffer_dictionary (u8)
    // +0x20: ofs_unordered_access_buffer_dictionary (u8)
    // +0x28: index_shader_output (u4)
    // +0x2C: index_sampler (u4)
    // +0x30: index_constant_buffer (u4)
    // +0x34: index_unordered_access_buffer (u4)
    // +0x38: ofs_shader_slot_array (u4)
    // +0x3C: compute_workgroup_size_x/y/z (u4 x3)
    // +0x48: index_image (u4) - for texture bindless resolution (unused for now)

    let read_u8 = |off: usize| -> u64 {
        if off + 8 > data.len() {
            return 0;
        }
        u64::from_le_bytes(data[off..off + 8].try_into().unwrap_or([0; 8]))
    };

    let read_u4 = |off: usize| -> u32 {
        if off + 4 > data.len() {
            return 0;
        }
        u32::from_le_bytes(data[off..off + 4].try_into().unwrap_or([0; 4]))
    };

    let ofs_sampler_dict = read_u8(ofs_reflection + 0x10) as usize;
    let ofs_cbuffer_dict = read_u8(ofs_reflection + 0x18) as usize;
    let ofs_image_dict = read_u8(ofs_reflection + 0x20) as usize; // might be at different offset
    let index_sampler = read_u4(ofs_reflection + 0x2C);
    let index_constant_buffer = read_u4(ofs_reflection + 0x30);
    let ofs_shader_slot_array = read_u4(ofs_reflection + 0x38) as usize;
    let _index_image = read_u4(ofs_reflection + 0x48);  // not used yet, may be needed for texture bindless resolution

    // Parse dictionaries
    let sampler_names = parse_dictionary(data, ofs_sampler_dict).unwrap_or_default();
    let constant_buffer_names = parse_dictionary(data, ofs_cbuffer_dict).unwrap_or_default();
    let texture_names = parse_dictionary(data, ofs_image_dict).unwrap_or_default();

    // Parse shader slot array (array of u32 indices)
    let mut shader_slots = Vec::new();
    let mut slot_offset = ofs_shader_slot_array;
    for _ in 0..256 {
        // Reasonable upper limit
        if slot_offset + 4 > data.len() {
            break;
        }
        shader_slots.push(read_u4(slot_offset));
        slot_offset += 4;
    }

    eprintln!(
        "[BNSH_REFL] Stage reflection: {} samplers, {} cbuffers, {} textures, {} slots",
        sampler_names.len(),
        constant_buffer_names.len(),
        texture_names.len(),
        shader_slots.len()
    );
    if !sampler_names.is_empty() {
        eprintln!(
            "[BNSH_REFL]   Samplers: {:?}",
            &sampler_names[..sampler_names.len().min(3)]
        );
    }

    Ok(ShaderStageReflection {
        sampler_names,
        constant_buffer_names,
        texture_names,
        shader_slots,
        index_sampler,
        index_constant_buffer,
    })
}

/// Resolve bindless texture samplers using the driver jump table
/// Maps material texture names (from FMAT) to their GPU binding slots
#[allow(dead_code)]
pub fn resolve_material_sampler_bindings(
    stage_reflection: &ShaderStageReflection,
    material_textures: &[(String, u32)], // (texture_name, bntx_index)
) -> HashMap<String, u32> {
    let mut bindings = HashMap::new();

    // Build the jump table: sampler_name -> gpu_slot
    let sampler_table = stage_reflection.build_sampler_jump_table();

    for (material_tex_name, bntx_index) in material_textures {
        // Try to find a matching sampler for this texture
        // Common patterns: _col, _emi, _prm, etc.
        for (sampler_name, gpu_slot) in sampler_table.iter() {
            // Check if sampler name matches texture name (case-insensitive suffix match)
            if sampler_name.to_lowercase().contains(&material_tex_name.to_lowercase())
                || material_tex_name
                    .to_lowercase()
                    .contains(&sampler_name.to_lowercase())
            {
                bindings.insert(material_tex_name.clone(), *gpu_slot);
                eprintln!(
                    "[BNSH_BINDLESS] Material texture '{}' (bntx {}) -> GPU slot {}",
                    material_tex_name, bntx_index, gpu_slot
                );
                break;
            }
        }
    }

    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sampler_jump_table() {
        let mut reflection = ShaderStageReflection {
            sampler_names: vec!["tex_diffuse".to_string(), "tex_normal".to_string()],
            shader_slots: vec![0, 2, 5], // GPU slots
            index_sampler: 0,
            ..Default::default()
        };

        let table = reflection.build_sampler_jump_table();
        assert_eq!(table.get("tex_diffuse"), Some(&0));
        assert_eq!(table.get("tex_normal"), Some(&2));
    }

    #[test]
    fn test_resolve_material_bindings() {
        let reflection = ShaderStageReflection {
            sampler_names: vec!["tex_col".to_string(), "tex_emi".to_string()],
            shader_slots: vec![0, 1, 2],
            index_sampler: 0,
            ..Default::default()
        };

        let materials = vec![("col".to_string(), 10), ("emi".to_string(), 11)];
        let bindings = resolve_material_sampler_bindings(&reflection, &materials);

        assert!(bindings.contains_key("col"));
        assert!(bindings.contains_key("emi"));
    }
}
