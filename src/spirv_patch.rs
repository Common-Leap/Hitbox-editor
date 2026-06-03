// SPIR-V reflection and patching utilities for BNSH shader integration.
//
// When BNSH shaders are decoded from game effect files, their SPIR-V uses
// binding numbers and descriptor sets from the Nintendo Switch NVN driver.
// This module provides:
// 1. Reflection: parse SPIR-V to find descriptor bindings and their types
// 2. Patching: remap SPIR-V binding/decorations to match our layouts

use std::collections::HashMap;
use anyhow::{Result, anyhow};

/// A parsed descriptor binding from SPIR-V
#[derive(Debug, Clone, PartialEq)]
pub struct SpirvBinding {
    pub set: u32,
    pub binding: u32,
    pub ty: SpirvBindingType,
}

/// The type of a SPIR-V descriptor binding
#[derive(Debug, Clone, PartialEq)]
pub enum SpirvBindingType {
    SampledImage,
    Sampler,
    UniformBuffer,
    StorageBuffer,
    StorageImage,
    Unknown(String),
}

/// Parse descriptor bindings from SPIR-V binary words.
///
/// Walks the instruction stream looking for:
/// - OpDecorate with Binding/DescriptorSet decorations
/// - OpTypeImage, OpTypeSampler, OpTypeSampledImage for type info
/// - OpVariable with UniformConstant/Uniform/StorageBuffer storage class
pub fn parse_spirv_bindings(spirv_words: &[u32]) -> Result<Vec<SpirvBinding>> {
    if spirv_words.len() < 5 {
        return Err(anyhow!("SPIR-V too short: {} words", spirv_words.len()));
    }
    if spirv_words[0] != 0x07230203 {
        return Err(anyhow!("Invalid SPIR-V magic: {:#x}", spirv_words[0]));
    }

    // Collect decorations (Binding, DescriptorSet) per target ID
    let mut decorations: std::collections::HashMap<u32, Vec<(u32, u32)>> = std::collections::HashMap::new();
    let mut type_info: std::collections::HashMap<u32, &str> = std::collections::HashMap::new();
    let mut result_types: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut var_storage: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut pointer_types: std::collections::HashMap<u32, (u32, u32)> = std::collections::HashMap::new();

    let mut i = 5;
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() {
            break;
        }

        match opcode {
            71 => { // OpDecorate
                if word_count >= 3 {
                    let target = spirv_words[i + 1];
                    let decoration = spirv_words[i + 2];
                    let value = if word_count >= 4 { spirv_words[i + 3] } else { 0 };
                    decorations.entry(target).or_default().push((decoration, value));
                }
            }
            11 => if word_count >= 2 { type_info.insert(spirv_words[i + 1], "image"); }, // OpTypeImage
            14 => if word_count >= 2 { type_info.insert(spirv_words[i + 1], "sampler"); }, // OpTypeSampler
            27 => if word_count >= 3 { type_info.insert(spirv_words[i + 1], "sampled_image"); }, // OpTypeSampledImage
            32 => if word_count >= 4 { // OpTypePointer
                pointer_types.insert(spirv_words[i + 1], (spirv_words[i + 2], spirv_words[i + 3]));
            }
            59 => if word_count >= 3 { // OpVariable
                result_types.insert(spirv_words[i + 2], spirv_words[i + 1]);
                if word_count >= 4 { var_storage.insert(spirv_words[i + 2], spirv_words[i + 3]); }
            }
            _ => {}
        }
        i += word_count;
    }

    let mut bindings: Vec<SpirvBinding> = Vec::new();

    for (&var_id, &type_id) in &result_types {
        let set = decorations.get(&var_id)
            .and_then(|d| d.iter().find(|(dec, _)| *dec == 3))
            .map(|(_, v)| *v)
            .unwrap_or(0);
        let binding = decorations.get(&var_id)
            .and_then(|d| d.iter().find(|(dec, _)| *dec == 33))
            .map(|(_, v)| *v);

        let Some(binding) = binding else { continue; };

        let storage_class = var_storage.get(&var_id).copied();
        let pointed_type = pointer_types.get(&type_id).map(|&(_, inner)| inner);
        let base_type = pointed_type
            .and_then(|pt| pointer_types.get(&pt).map(|&(_, inner)| inner))
            .or(pointed_type)
            .unwrap_or(type_id);

        let spirv_type = match type_info.get(&base_type).copied() {
            Some("sampled_image") => SpirvBindingType::SampledImage,
            Some("image") => SpirvBindingType::SampledImage,
            Some("sampler") => SpirvBindingType::Sampler,
            None => match storage_class {
                Some(1) | Some(2) => SpirvBindingType::UniformBuffer,
                Some(12) => SpirvBindingType::StorageBuffer,
                _ => SpirvBindingType::Unknown(format!("sc={}", storage_class.unwrap_or(99))),
            }
            Some(other) => SpirvBindingType::Unknown(other.to_string()),
        };

        bindings.push(SpirvBinding { set, binding, ty: spirv_type });
    }

    bindings.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));
    Ok(bindings)
}

/// Remap SPIR-V bindings in-place. `remap` maps (old_set, old_binding) → (new_set, new_binding).
/// Returns the number of decorations patched.
pub fn remap_spirv_bindings(
    spirv_words: &mut [u32],
    remap: &std::collections::HashMap<(u32, u32), (u32, u32)>,
) -> usize {
    if remap.is_empty() { return 0; }

    let mut target_bindings: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut target_sets: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    let mut i = 5;
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() { break; }
        if opcode == 71 && word_count >= 4 {
            let target = spirv_words[i + 1];
            let decoration = spirv_words[i + 2];
            if decoration == 33 { target_bindings.insert(target, spirv_words[i + 3]); }
            if decoration == 3 { target_sets.insert(target, spirv_words[i + 3]); }
        }
        i += word_count;
    }

    let mut changed = 0;
    let mut i = 5;
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() { break; }
        if opcode == 71 && word_count >= 4 {
            let target = spirv_words[i + 1];
            let decoration = spirv_words[i + 2];
            if decoration == 33 {
                let set = target_sets.get(&target).copied().unwrap_or(0);
                let old = spirv_words[i + 3];
                let key = (set, old);
                if let Some(&(_, new_binding)) = remap.get(&key) {
                    spirv_words[i + 3] = new_binding;
                    changed += 1;
                }
            }
            if decoration == 3 {
                let binding = target_bindings.get(&target).copied().unwrap_or(0);
                let old = spirv_words[i + 3];
                let key = (old, binding);
                if let Some(&(new_set, _)) = remap.get(&key) {
                    spirv_words[i + 3] = new_set;
                    changed += 1;
                }
            }
        }
        i += word_count;
    }
    changed
}

/// Create a human-readable summary of the SPIR-V bindings for debugging.
pub fn format_bindings_summary(bindings: &[SpirvBinding]) -> String {
    if bindings.is_empty() {
        return "no bindings".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for b in bindings {
        let ty_str = match &b.ty {
            SpirvBindingType::SampledImage => "tex",
            SpirvBindingType::Sampler => "smp",
            SpirvBindingType::UniformBuffer => "ubuf",
            SpirvBindingType::StorageBuffer => "sbuf",
            SpirvBindingType::StorageImage => "simg",
            SpirvBindingType::Unknown(s) => s,
        };
        parts.push(format!("s{}b{}({})", b.set, b.binding, ty_str));
    }
    parts.join(" ")
}

/// Patch NVN-specific SPIR-V execution modes to be Vulkan-compatible.
///
/// NVN (Nintendo Switch) uses SPIR-V conventions that differ from Vulkan:
/// - `OriginLowerLeft` (8) → `OriginUpperLeft` (7): NVN uses lower-left origin
///   for fragment coordinates, but Vulkan/WGPU requires upper-left.
/// - `PixelCenterInteger` (6) → removed: NVN uses integer pixel centers,
///   Vulkan uses half-integer centers.
///
/// This runs in-place on the SPIR-V word array and reports what was patched.
pub fn nvn_patch_execution_modes(spirv_words: &mut [u32]) -> (usize, usize) {
    let mut lower_left_patched = 0usize;
    let mut pixel_center_patched = 0usize;

    let mut i = 5; // skip header (5 words)
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() {
            break;
        }

        // OpExecutionMode = 17, OpExecutionModeId = 331
        if opcode == 17 && word_count >= 3 {
            let mode = spirv_words[i + 2];
            match mode {
                8 => { // OriginLowerLeft → OriginUpperLeft
                    spirv_words[i + 2] = 7;
                    lower_left_patched += 1;
                }
                6 => { // PixelCenterInteger → OriginUpperLeft (harmless no-op)
                    spirv_words[i + 2] = 7;
                    pixel_center_patched += 1;
                }
                _ => {}
            }
        }

        i += word_count;
    }

    (lower_left_patched, pixel_center_patched)
}

/// Apply all NVN→Vulkan patches to SPIR-V binary words.
/// Returns a summary of what was patched.
pub fn nvn_to_vulkan_patch(spirv_words: &mut [u32]) -> Vec<String> {
    let mut patches = Vec::new();

    let (ll, pc) = nvn_patch_execution_modes(spirv_words);
    if ll > 0 {
        patches.push(format!("OriginLowerLeft→OriginUpperLeft x{}", ll));
    }
    if pc > 0 {
        patches.push(format!("PixelCenterInteger→OriginUpperLeft x{}", pc));
    }

    patches
}

/// Our pipeline layout's Group 1 texture-binding slots (in order of use).
const TEX_BINDINGS: [u32; 4] = [0, 2, 4, 7];
/// Our pipeline layout's Group 1 sampler-binding slots (in order of use).
const SMP_BINDINGS: [u32; 4] = [1, 3, 5, 8];

/// Build a binding remap from NVN convention to our hardcoded pipeline layout.
///
/// NVN (Nintendo Switch) convention:
///   set 0 = textures + samplers (CombinedImageSampler)
///   set 1 = uniform buffers
///   set 2 = storage buffers
///
/// Our layout (`particle.wgsl`):
///   group 0: Uniform(0), Storage(1)       ← camera + particle storage
///   group 1: Texture(0,2,4,7), Sampler(1,3,5,8), Uniform(6)  ← texture slots
///
/// The remap assigns bindings from both VS and FS into a single unified map.
/// Returns `None` if any binding cannot be remapped (overflow / unsupported type).
/// Check if all SPIR-V bindings are storage buffers in set 0 (bindless pattern).
/// Returns the count of bindings if bindless, or None if not.
fn is_bindless_all_storage(bindings: &[SpirvBinding]) -> Option<usize> {
    if bindings.is_empty() {
        return None;
    }
    if bindings.iter().all(|b| b.set == 0 && matches!(b.ty, SpirvBindingType::StorageBuffer)) {
        Some(bindings.len())
    } else {
        None
    }
}

pub fn build_nvn_to_our_layout_remap(
    vs_spirv: &[u32],
    fs_spirv: &[u32],
) -> Option<HashMap<(u32, u32), (u32, u32)>> {
    let vs_bindings = parse_spirv_bindings(vs_spirv).unwrap_or_default();
    let fs_bindings = parse_spirv_bindings(fs_spirv).unwrap_or_default();

    // Merge bindings from both stages, deduplicating by (set, binding).
    let mut all: Vec<SpirvBinding> = vs_bindings;
    for fb in fs_bindings {
        if !all.iter().any(|b| b.set == fb.set && b.binding == fb.binding) {
            all.push(fb);
        }
    }
    all.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));

    // Detect bindless pattern: all storage buffers in set 0 → keep original bindings.
    if is_bindless_all_storage(&all).is_some() {
        eprintln!("[NVN→Layout] Bindless storage-buffer shader detected ({} sbuf bindings) — keeping original bindings", all.len());
        return Some(HashMap::new()); // empty remap = keep as-is
    }

    let mut remap: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut tex_idx = 0usize;
    let mut smp_idx = 0usize;
    let mut uniform_count = 0usize;
    let mut storage_count = 0usize;

    for b in &all {
        let target: Option<(u32, u32)> = match (b.set, &b.ty) {
            // NVN set 0: textures → Group 1 texture slots
            (0, SpirvBindingType::SampledImage) => {
                if tex_idx < TEX_BINDINGS.len() {
                    let slot = TEX_BINDINGS[tex_idx];
                    tex_idx += 1;
                    Some((1, slot))
                } else {
                    eprintln!("[NVN→Layout] ✗ too many textures (max {})", TEX_BINDINGS.len());
                    None
                }
            }
            // NVN set 0: samplers → Group 1 sampler slots
            (0, SpirvBindingType::Sampler) => {
                if smp_idx < SMP_BINDINGS.len() {
                    let slot = SMP_BINDINGS[smp_idx];
                    smp_idx += 1;
                    Some((1, slot))
                } else {
                    eprintln!("[NVN→Layout] ✗ too many samplers (max {})", SMP_BINDINGS.len());
                    None
                }
            }
            // NVN set 0: uniform → Group 1, binding 6 (indirect params slot)
            (0, SpirvBindingType::UniformBuffer) => Some((1, 6)),
            // NVN set 0: storage → Group 0, binding 1 (particle storage)
            (0, SpirvBindingType::StorageBuffer) => Some((0, 1)),
            // NVN set 1: first uniform → Group 0, binding 0 (camera)
            (1, SpirvBindingType::UniformBuffer) if uniform_count == 0 => {
                uniform_count += 1;
                Some((0, 0))
            }
            // NVN set 1: subsequent uniforms → Group 1, binding 6 (indirect params)
            (1, SpirvBindingType::UniformBuffer) => {
                uniform_count += 1;
                eprintln!("[NVN→Layout] uniform #{} → group 1 binding 6", uniform_count);
                Some((1, 6))
            }
            // NVN set 2: storage → Group 0, binding 1 (particle storage)
            (2, SpirvBindingType::StorageBuffer) if storage_count == 0 => {
                storage_count += 1;
                Some((0, 1))
            }
            (2, SpirvBindingType::StorageBuffer) => {
                storage_count += 1;
                eprintln!("[NVN→Layout] ✗ too many storage buffers (max 1)");
                None
            }
            // Catch-all for other types / sets
            _ => {
                eprintln!("[NVN→Layout] ✗ unhandled: set={} bind={} type={:?}",
                    b.set, b.binding, b.ty);
                None
            }
        };

        match target {
            Some((ns, nb)) if (b.set, b.binding) != (ns, nb) => {
                remap.insert((b.set, b.binding), (ns, nb));
            }
            None => return None,
            _ => {}
        }
    }

    Some(remap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        assert!(parse_spirv_bindings(&[]).is_err());
    }

    #[test]
    fn test_parse_invalid_magic() {
        assert!(parse_spirv_bindings(&[0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn test_parse_valid_no_bindings() {
        let words = vec![0x07230203, 0x00010000, 0, 5, 0];
        let result = parse_spirv_bindings(&words).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_remap_nop() {
        let mut spirv = vec![0x07230203u32, 0x00010000, 0, 5, 0];
        let remap = std::collections::HashMap::new();
        assert_eq!(remap_spirv_bindings(&mut spirv, &remap), 0);
    }

    #[test]
    fn test_format_empty() {
        assert_eq!(format_bindings_summary(&[]), "no bindings");
    }

    #[test]
    fn test_nvn_patch_lower_left() {
        // A minimal SPIR-V with OpExecutionMode OriginLowerLeft
        // OpExecutionMode = 17, 3 words: [opcode|count=3, entry_point, mode=8]
        let mut spirv = vec![
            0x07230203u32,  // magic
            0x00010000,     // version 1.0
            0,              // generator
            10,             // bound
            0,              // reserved
            0x00030011,     // OpExecutionMode, 3 words, opcode 17
            1,              // entry point
            8,              // OriginLowerLeft (needs patching)
        ];
        let (ll, pc) = nvn_patch_execution_modes(&mut spirv);
        assert_eq!(ll, 1, "should patch one OriginLowerLeft");
        assert_eq!(pc, 0);
        assert_eq!(spirv[7], 7, "should be OriginUpperLeft");
    }

    #[test]
    fn test_nvn_patch_multiple() {
        let mut spirv = vec![
            0x07230203u32,
            0x00010000,
            0,
            10,
            0,
            0x00030011,     // OpExecutionMode, word 5
            1,              // word 6
            8,              // word 7: OriginLowerLeft
            0x00030011,     // OpExecutionMode, word 8
            1,              // word 9
            6,              // word 10: PixelCenterInteger
            0x00030011,     // OpExecutionMode, word 11
            1,              // word 12
            7,              // word 13: OriginUpperLeft (already correct)
        ];
        let (ll, pc) = nvn_patch_execution_modes(&mut spirv);
        assert_eq!(ll, 1);
        assert_eq!(pc, 1);
        assert_eq!(spirv[7], 7, "OriginLowerLeft → OriginUpperLeft");
        assert_eq!(spirv[10], 7, "PixelCenterInteger → OriginUpperLeft");
        assert_eq!(spirv[13], 7, "OriginUpperLeft unchanged");
    }

    #[test]
    fn test_nvn_to_vulkan_patch_summary() {
        let mut spirv = vec![
            0x07230203u32,
            0x00010000,
            0,
            10,
            0,
            0x00030011,
            1,
            8,  // OriginLowerLeft
        ];
        let patches = nvn_to_vulkan_patch(&mut spirv);
        assert_eq!(patches.len(), 1);
        assert!(patches[0].contains("OriginLowerLeft"));
    }
}
