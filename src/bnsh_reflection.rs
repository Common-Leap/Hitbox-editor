// BNSH Shader Reflection: Extract driver jump tables for bindless texture resolution
// Based on BNSH.ksy specification: https://github.com/maierfelix/bnsh-decoder/blob/master/BNSH.ksy
//
// For games like LGPE that use bindless textures in most shaders, we need to resolve
// the driver jump table which associates material textures with their GPU binding slots.

use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Driver jump-table indirection buffer size (2^5) per bnsh-decoder README.
pub const STR_INDEX_BUFFER_SIZE: usize = 32;

/// README `getConstantBufferBindingIndices`: map GPU slot → cbuffer dictionary index.
pub fn get_constant_buffer_binding_indices(slt: &[u32]) -> Vec<u32> {
    if slt.is_empty() {
        return Vec::new();
    }
    let size = slt
        .iter()
        .copied()
        .max()
        .map(|m| (m as usize + 1).max(slt.len()))
        .unwrap_or(slt.len())
        .min(STR_INDEX_BUFFER_SIZE);
    let mut out = vec![u32::MAX; size];
    for (ii, &slot) in slt.iter().enumerate() {
        let idx = slot as usize;
        if idx < size {
            out[idx] = ii as u32;
        }
    }
    out
}

/// README `getSamplerBindingIndices`: resolve texture binding per sampler via strIndexBuffer.
pub fn get_sampler_binding_indices(str: &[u32], slt: &[u32], smp: &[u32]) -> Vec<u32> {
    let size = str.len();
    if size == 0 {
        return Vec::new();
    }
    debug_assert_eq!(slt.len(), size);
    debug_assert_eq!(smp.len(), size);

    let mut str_index_buffer = vec![0u32; STR_INDEX_BUFFER_SIZE];
    for ii in 0..size {
        let idx = slt[ii] as usize;
        if idx < STR_INDEX_BUFFER_SIZE {
            str_index_buffer[idx] = str[ii];
        }
    }
    let mut out = vec![0u32; size];
    for ii in 0..size {
        let idx = smp[ii] as usize;
        if idx < STR_INDEX_BUFFER_SIZE {
            out[ii] = str_index_buffer[idx];
        }
    }
    out
}

/// Match material slot keys (`_col`, `_emi`, …) to shader sampler dictionary names (`tex_col`, …).
pub fn match_material_sampler_name<'a>(
    material_key: &str,
    sampler_names: &'a [String],
) -> Option<&'a String> {
    for name in sampler_names {
        if name.eq_ignore_ascii_case(material_key) {
            return Some(name);
        }
    }
    let key = material_key.trim_start_matches('_').to_ascii_lowercase();
    for name in sampler_names {
        let lower = name.to_ascii_lowercase();
        if lower == key
            || lower == format!("tex_{key}")
            || lower.ends_with(&format!("_{key}"))
        {
            return Some(name);
        }
    }
    None
}

/// Parsed reflection data from a single shader stage (vertex, fragment, compute, etc.)
#[derive(Debug, Clone, Default)]
pub struct ShaderStageReflection {
    /// Vertex / stage input names from the shader input dictionary.
    pub input_names: Vec<String>,
    /// Sampler names extracted from the sampler dictionary
    pub sampler_names: Vec<String>,
    /// Constant buffer names from the constant_buffer dictionary
    pub constant_buffer_names: Vec<String>,
    /// Texture/image names (if present)
    pub texture_names: Vec<String>,
    /// Index into shader_slots for first image/texture
    pub index_image: u32,
    /// Shader slot array: GPU binding slots for each resource
    pub shader_slots: Vec<u32>,
    /// Index into shader_slots for first sampler
    pub index_sampler: u32,
    /// Index into shader_slots for first constant buffer
    pub index_constant_buffer: u32,
    /// Index into shader_slots for first shader output
    pub index_shader_output: u32,
    /// Total number of shader slot entries (= index_unordered_access_buffer)
    pub index_unordered_access_buffer: u32,
}

impl ShaderStageReflection {
    /// Build the driver jump table for samplers: maps sampler name -> GPU binding slot
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

    /// Build the driver jump table for constant buffers (README `getConstantBufferBindingIndices`).
    pub fn build_cbuffer_jump_table(&self) -> HashMap<String, u32> {
        self.build_cbuffer_binding_pairs()
            .into_iter()
            .collect()
    }

    /// slt slice used by README cbuffer/sampler jump-table resolution.
    pub fn cbuffer_slt_slice(&self) -> Vec<u32> {
        self.shader_slot_slice(self.index_shader_output, self.index_image)
    }

    /// Per-cbuffer (name, gpu_binding_slot) via driver jump table, with direct-slot fallback.
    pub fn build_cbuffer_binding_pairs(&self) -> Vec<(String, u32)> {
        let cbuf_count = self.constant_buffer_names.len();
        if cbuf_count == 0 {
            return Vec::new();
        }

        let slt = self.cbuffer_slt_slice();
        if !slt.is_empty() {
            let indices = get_constant_buffer_binding_indices(&slt);
            let mut pairs = Vec::new();
            for (gpu_slot, &dict_idx) in indices.iter().enumerate() {
                if dict_idx == u32::MAX {
                    continue;
                }
                let dict_idx = dict_idx as usize;
                if dict_idx >= cbuf_count {
                    continue;
                }
                pairs.push((
                    self.constant_buffer_names[dict_idx].clone(),
                    gpu_slot as u32,
                ));
            }
            if !pairs.is_empty() {
                return pairs;
            }
        }

        let start = self.index_constant_buffer as usize;
        self.constant_buffer_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let gpu_slot = self
                    .shader_slots
                    .get(start + i)
                    .copied()
                    .unwrap_or(i as u32);
                (name.clone(), gpu_slot)
            })
            .collect()
    }

    /// Build the driver jump table for texture/image resources.
    pub fn build_texture_jump_table(&self) -> HashMap<String, u32> {
        let mut table = HashMap::new();
        for (tex_idx, tex_name) in self.texture_names.iter().enumerate() {
            let slot_idx = self.index_image as usize + tex_idx;
            if slot_idx < self.shader_slots.len() {
                let gpu_slot = self.shader_slots[slot_idx];
                table.insert(tex_name.clone(), gpu_slot);
            }
        }
        table
    }

    fn shader_slot_slice(&self, start: u32, end: u32) -> Vec<u32> {
        let s = start as usize;
        let e = end as usize;
        if s <= e && e <= self.shader_slots.len() {
            self.shader_slots[s..e].to_vec()
        } else {
            Vec::new()
        }
    }

    /// str / slt / smp slices for README bindless resolution.
    pub fn sampler_texture_binding_arrays(&self) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let str_ = self.shader_slot_slice(self.index_image, self.index_sampler);
        let mut slt = self.shader_slot_slice(self.index_shader_output, self.index_image);
        let smp = self.shader_slot_slice(self.index_sampler, self.index_constant_buffer);
        let size = str_.len();
        if slt.len() != size {
            if slt.is_empty() && size > 0 {
                slt = (0..size as u32).collect();
            } else if slt.len() > size {
                slt.truncate(size);
            }
        }
        let mut smp_aligned = smp;
        if smp_aligned.len() > size {
            smp_aligned.truncate(size);
        }
        while smp_aligned.len() < size {
            smp_aligned.push(0);
        }
        (str_, slt, smp_aligned)
    }

    /// Per-sampler (texture_binding, sampler_binding) using README jump-table resolution.
    pub fn build_sampler_texture_pairs(&self) -> Vec<(String, u32, u32)> {
        let (str_, slt, smp) = self.sampler_texture_binding_arrays();
        if str_.is_empty() || smp.is_empty() || self.sampler_names.is_empty() {
            return self.build_sampler_texture_pairs_fallback();
        }
        let tex_bindings = get_sampler_binding_indices(&str_, &slt, &smp);
        self.sampler_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let sampler_binding = self.shader_slots
                    .get(self.index_sampler as usize + i)
                    .copied()
                    .unwrap_or(0);
                let tex_binding = tex_bindings
                    .get(i)
                    .copied()
                    .unwrap_or(sampler_binding);
                (name.clone(), tex_binding, sampler_binding)
            })
            .collect()
    }

    fn build_sampler_texture_pairs_fallback(&self) -> Vec<(String, u32, u32)> {
        self.sampler_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let sampler_binding = self.shader_slots
                    .get(self.index_sampler as usize + i)
                    .copied()
                    .unwrap_or(0);
                (name.clone(), sampler_binding, sampler_binding.saturating_add(1))
            })
            .collect()
    }

    /// Ordered (texture_binding, sampler_binding) pairs for emitter slots 0/1/2.
    pub fn build_ordered_texture_pairs(&self) -> Vec<(u32, u32)> {
        let mut entries: Vec<(u32, u32)> = self
            .build_sampler_texture_pairs()
            .into_iter()
            .map(|(_, tex, smp)| (tex, smp))
            .collect();
        entries.sort_by_key(|(tex, _)| *tex);
        entries.into_iter().take(3).collect()
    }

    /// Material texture WGSL binding slots for _col / _emi / _prm.
    pub fn material_texture_slots(&self) -> (u32, u32, u32) {
        let tex_by_sampler: HashMap<String, u32> = self
            .build_sampler_texture_pairs()
            .into_iter()
            .map(|(name, tex, _)| (name, tex))
            .collect();
        let slot = |key: &str, default: u32| -> u32 {
            match_material_sampler_name(key, &self.sampler_names)
                .and_then(|n| tex_by_sampler.get(n))
                .copied()
                .unwrap_or(default)
        };
        (slot("_col", 0), slot("_emi", 2), slot("_prm", 4))
    }
}

/// Parse a dictionary from BNSH reflection data
/// Dictionary format: magic "_DIC" + str_count + padding + strings
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
    // +0x48: index_image (u4)
    // +0x4C: ofs_image_dictionary (u4)

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

    let ofs_input_dict = read_u8(ofs_reflection + 0x00) as usize;
    let ofs_sampler_dict = read_u8(ofs_reflection + 0x10) as usize;
    let ofs_cbuffer_dict = read_u8(ofs_reflection + 0x18) as usize;
    let ofs_image_dict = read_u4(ofs_reflection + 0x4C) as usize;
    let index_shader_output = read_u4(ofs_reflection + 0x28);
    let index_sampler = read_u4(ofs_reflection + 0x2C);
    let index_constant_buffer = read_u4(ofs_reflection + 0x30);
    let index_unordered_access_buffer = read_u4(ofs_reflection + 0x34);
    let ofs_shader_slot_array = read_u4(ofs_reflection + 0x38) as usize;
    let index_image = read_u4(ofs_reflection + 0x48);

    // Parse dictionaries
    let input_names = parse_dictionary(data, ofs_input_dict).unwrap_or_default();
    let sampler_names = parse_dictionary(data, ofs_sampler_dict).unwrap_or_default();
    let constant_buffer_names = parse_dictionary(data, ofs_cbuffer_dict).unwrap_or_default();
    let texture_names = parse_dictionary(data, ofs_image_dict).unwrap_or_default();

    // Parse shader slot array (length = index_unordered_access_buffer per BNSH.ksy)
    let slot_count = index_unordered_access_buffer as usize;
    let mut shader_slots = Vec::with_capacity(slot_count);
    let mut slot_offset = ofs_shader_slot_array;
    for _ in 0..slot_count {
        if slot_offset + 4 > data.len() {
            break;
        }
        shader_slots.push(read_u4(slot_offset));
        slot_offset += 4;
    }

    if crate::fx_debug_enabled() {
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
    }

    Ok(finalize_stage_reflection(ShaderStageReflection {
        input_names,
        sampler_names,
        constant_buffer_names,
        texture_names,
        shader_slots,
        index_sampler,
        index_constant_buffer,
        index_shader_output,
        index_unordered_access_buffer,
        index_image,
    }))
}

/// Post-process parsed stage reflection: fill missing sampler names from image dict when needed.
fn finalize_stage_reflection(mut reflection: ShaderStageReflection) -> ShaderStageReflection {
    if reflection.sampler_names.is_empty() && !reflection.texture_names.is_empty() {
        reflection.sampler_names = reflection.texture_names.clone();
        if crate::fx_debug_enabled() {
            eprintln!(
                "[BNSH_REFL] Sampler dict empty — using {} image dict name(s) as samplers",
                reflection.sampler_names.len()
            );
        }
    }
    reflection
}

/// Resolve bindless texture samplers using the driver jump table
/// Maps material texture names (from FMAT) to their GPU binding slots
pub fn resolve_material_sampler_bindings(
    stage_reflection: &ShaderStageReflection,
    material_textures: &[(String, u32)], // (texture_name, bntx_index)
) -> HashMap<String, u32> {
    let mut bindings = HashMap::new();

    let tex_by_sampler: HashMap<String, u32> = stage_reflection
        .build_sampler_texture_pairs()
        .into_iter()
        .map(|(name, tex, _)| (name, tex))
        .collect();

    for (material_tex_name, bntx_index) in material_textures {
        let gpu_slot = match_material_sampler_name(material_tex_name, &stage_reflection.sampler_names)
            .and_then(|n| tex_by_sampler.get(n))
            .copied();
        if let Some(gpu_slot) = gpu_slot {
            bindings.insert(material_tex_name.clone(), gpu_slot);
            eprintln!(
                "[BNSH_BINDLESS] Material texture '{}' (bntx {}) -> GPU slot {}",
                material_tex_name, bntx_index, gpu_slot
            );
        }
    }

    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sampler_jump_table() {
        let reflection = ShaderStageReflection {
            sampler_names: vec!["tex_diffuse".to_string(), "tex_normal".to_string()],
            shader_slots: vec![0, 2, 5],
            index_sampler: 0,
            ..Default::default()
        };

        let table = reflection.build_sampler_jump_table();
        assert_eq!(table.get("tex_diffuse"), Some(&0));
        assert_eq!(table.get("tex_normal"), Some(&2));
    }

    #[test]
    fn test_ordered_texture_pairs_fallback() {
        let reflection = ShaderStageReflection {
            sampler_names: vec!["a".into(), "b".into(), "c".into()],
            shader_slots: vec![4, 8, 12],
            index_sampler: 0,
            ..Default::default()
        };
        let pairs = reflection.build_ordered_texture_pairs();
        assert_eq!(pairs, vec![(4, 5), (8, 9), (12, 13)]);
    }

    #[test]
    fn test_get_constant_buffer_binding_indices() {
        let slt = vec![5u32, 2, 7];
        let out = get_constant_buffer_binding_indices(&slt);
        assert_eq!(out[5], 0);
        assert_eq!(out[2], 1);
        assert_eq!(out[7], 2);
    }

    #[test]
    fn test_get_sampler_binding_indices_readme() {
        let str_ = vec![10u32, 14, 18];
        let slt = vec![0u32, 1, 2];
        let smp = vec![1u32, 0, 2];
        let out = get_sampler_binding_indices(&str_, &slt, &smp);
        assert_eq!(out, vec![14, 10, 18]);
    }

    #[test]
    fn test_build_sampler_texture_pairs_with_jump_table() {
        // index_shader_output=0, index_image=0, index_sampler=3, index_constant_buffer=6
        // slots[0..3] = texture bindings, slots[3..6] = sampler indirection indices
        let reflection = ShaderStageReflection {
            sampler_names: vec!["tex_col".into(), "tex_emi".into(), "tex_prm".into()],
            index_shader_output: 0,
            index_image: 0,
            index_sampler: 3,
            index_constant_buffer: 6,
            index_unordered_access_buffer: 6,
            shader_slots: vec![0, 2, 4, 1, 0, 2],
            ..Default::default()
        };
        let pairs = reflection.build_sampler_texture_pairs();
        assert_eq!(pairs[0], ("tex_col".into(), 2, 1));
        assert_eq!(pairs[1], ("tex_emi".into(), 0, 0));
        assert_eq!(pairs[2], ("tex_prm".into(), 4, 2));
    }

    #[test]
    fn test_match_material_sampler_name() {
        let names = vec!["tex_col".into(), "tex_emi".into()];
        assert_eq!(
            match_material_sampler_name("_col", &names).map(String::as_str),
            Some("tex_col")
        );
        assert_eq!(
            match_material_sampler_name("emi", &names).map(String::as_str),
            Some("tex_emi")
        );
    }

    #[test]
    fn test_material_texture_slots() {
        let reflection = ShaderStageReflection {
            sampler_names: vec!["tex_col".into(), "tex_emi".into(), "tex_prm".into()],
            index_shader_output: 0,
            index_image: 0,
            index_sampler: 3,
            index_constant_buffer: 6,
            index_unordered_access_buffer: 6,
            shader_slots: vec![0, 2, 4, 1, 0, 2],
            ..Default::default()
        };
        assert_eq!(reflection.material_texture_slots(), (2, 0, 4));
    }

    #[test]
    fn test_build_cbuffer_binding_pairs_jump_table() {
        // slt maps gpu slots 5,2,7 -> cbuffer dict indices 0,1,2
        let reflection = ShaderStageReflection {
            constant_buffer_names: vec!["cb0".into(), "cb1".into(), "cb2".into()],
            index_shader_output: 0,
            index_image: 3,
            index_constant_buffer: 3,
            index_unordered_access_buffer: 3,
            shader_slots: vec![5, 2, 7],
            ..Default::default()
        };
        let pairs = reflection.build_cbuffer_binding_pairs();
        assert_eq!(pairs.len(), 3);
        let map: HashMap<&str, u32> = pairs.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        assert_eq!(map.get("cb0"), Some(&5));
        assert_eq!(map.get("cb1"), Some(&2));
        assert_eq!(map.get("cb2"), Some(&7));
    }

    #[test]
    fn test_build_cbuffer_binding_pairs_direct_fallback() {
        let reflection = ShaderStageReflection {
            constant_buffer_names: vec!["Global".into(), "Material".into()],
            index_constant_buffer: 2,
            index_unordered_access_buffer: 4,
            shader_slots: vec![0, 0, 13, 6],
            ..Default::default()
        };
        let pairs = reflection.build_cbuffer_binding_pairs();
        assert_eq!(pairs, vec![("Global".into(), 13), ("Material".into(), 6)]);
    }

    #[test]
    fn test_finalize_stage_reflection_image_fallback() {
        let reflection = finalize_stage_reflection(ShaderStageReflection {
            texture_names: vec!["sysTexture0".into()],
            ..Default::default()
        });
        assert_eq!(reflection.sampler_names, vec!["sysTexture0"]);
    }

    #[test]
    fn test_parse_stage_reflection_synthetic() {
        // Minimal in-memory layout for shader_reflection_stage_data + _DIC sampler dict.
        let mut data = vec![0u8; 0x600];
        let stage = 0x200usize;
        let sampler_dict = 0x300usize;
        let slot_array = 0x400usize;

        // sampler dictionary entry (dictionary_entry.ofs_entry u8)
        data[stage + 0x10..stage + 0x18]
            .copy_from_slice(&(sampler_dict as u64).to_le_bytes());
        // index_sampler = 0, index_unordered_access_buffer = 2
        data[stage + 0x2C..stage + 0x30].copy_from_slice(&0u32.to_le_bytes());
        data[stage + 0x34..stage + 0x38].copy_from_slice(&2u32.to_le_bytes());
        data[stage + 0x38..stage + 0x3C].copy_from_slice(&(slot_array as u32).to_le_bytes());
        // ofs_image_dictionary unused (0)
        data[stage + 0x4C..stage + 0x50].copy_from_slice(&0u32.to_le_bytes());

        data[slot_array..slot_array + 4].copy_from_slice(&7u32.to_le_bytes());
        data[slot_array + 4..slot_array + 8].copy_from_slice(&9u32.to_le_bytes());

        data[sampler_dict..sampler_dict + 4].copy_from_slice(b"_DIC");
        data[sampler_dict + 4..sampler_dict + 8].copy_from_slice(&1u32.to_le_bytes());
        let str_entry = sampler_dict + 0x20;
        let str_body = sampler_dict + 0x40;
        data[str_entry..str_entry + 4].copy_from_slice(&(str_body as u32).to_le_bytes());
        let name = b"tex0";
        data[str_body..str_body + 2].copy_from_slice(&(name.len() as u16).to_le_bytes());
        data[str_body + 2..str_body + 2 + name.len()].copy_from_slice(name);

        let reflection = parse_shader_stage_reflection(&data, stage).expect("parse synthetic stage");
        assert_eq!(reflection.sampler_names, vec!["tex0".to_string()]);
        assert_eq!(reflection.shader_slots, vec![7, 9]);
        assert_eq!(reflection.index_unordered_access_buffer, 2);
        assert_eq!(reflection.build_sampler_jump_table().get("tex0"), Some(&7));
    }
}
