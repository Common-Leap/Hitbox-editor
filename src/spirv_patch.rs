// SPIR-V reflection and patching utilities for BNSH shader integration.
//
// When BNSH shaders are decoded from game effect files, their SPIR-V uses
// binding numbers and descriptor sets from the Nintendo Switch NVN driver.
// This module provides:
// 1. Reflection: parse SPIR-V to find descriptor bindings and their types
// 2. NVN→Vulkan patches: execution modes, vertex builtins, input locations

use anyhow::{Result, anyhow};

/// A parsed descriptor binding from SPIR-V
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct SpirvBinding {
    pub set: u32,
    pub binding: u32,
    pub ty: SpirvBindingType,
}

/// The type of a SPIR-V descriptor binding
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Create a human-readable summary of the SPIR-V bindings for debugging.
#[allow(dead_code)]
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

/// Patch vertex built-in decorations: VertexIndex(32) → VertexId(43),
/// InstanceIndex(33) → InstanceId(42).
///
/// The BNSH decoder converts NVN's `VertexId`/`InstanceId` to Vulkan's
/// `VertexIndex`/`InstanceIndex` in the SPIR-V output.  When spirv-cross
/// sees `VertexIndex`/`InstanceIndex` with `--vulkan-semantics` it emits
/// `gl_VertexID + gl_BaseVertexARB` / `gl_InstanceID + gl_BaseInstanceARB`
/// in GLSL, which naga cannot parse.
///
/// By reverting to `VertexId`/`InstanceId` spirv-cross outputs plain
/// `gl_VertexID`/`gl_InstanceID` with no ARB extension references.
/// This is correct because our draw calls always use base_vertex=0 and
/// base_instance=0, so `VertexIndex == VertexId` and `InstanceIndex == InstanceId`.
fn nvn_patch_vertex_builtins(spirv_words: &mut [u32]) -> (usize, usize) {
    let mut vi_patched = 0usize;
    let mut ii_patched = 0usize;

    let mut i = 5;
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() {
            break;
        }

        // OpDecorate = 71, 4 words: [opcode|count=4, target, decoration, value]
        if opcode == 71 && word_count >= 4 {
            let decoration = spirv_words[i + 2];
            let value = spirv_words[i + 3];
            if decoration == 11 {
                match value {
                    32 => { // VertexIndex → VertexId (43)
                        spirv_words[i + 3] = 43;
                        vi_patched += 1;
                    }
                    33 => { // InstanceIndex → InstanceId (42)
                        spirv_words[i + 3] = 42;
                        ii_patched += 1;
                    }
                    _ => {}
                }
            }
        }

        i += word_count;
    }

    (vi_patched, ii_patched)
}

/// Strip decorations that cause naga GLSL frontend to fail:
/// - `Precise` (decoration 12): GLSL-only qualifier, no SPIR-V equivalent →
///   change to `RelaxedPrecision` (0), which spirv-cross ignores in GLSL output.
/// - `BuiltIn PointSize` (decoration 11, value 1): naga cannot parse `gl_PointSize`
///   in GLSL → change the decoration to `NoPerspective` (0), making spirv-cross
///   emit the variable as a plain output with its original name.
fn nvn_strip_problematic_decorations(spirv_words: &mut [u32]) -> (usize, usize) {
    let mut precise_stripped = 0usize;
    let mut pointsize_stripped = 0usize;
    let mut deco71_12 = 0usize;
    let mut deco71_11pt = 0usize;
    let mut deco72_12 = 0usize;

    let mut i = 5;
    while i < spirv_words.len() {
        let word = spirv_words[i];
        let word_count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if word_count == 0 || i + word_count > spirv_words.len() {
            break;
        }

        // OpDecorate = 71, word layout: [target, decoration, value...]
        if opcode == 71 && word_count >= 3 {
            let decoration = spirv_words[i + 2];
            if decoration == 12 {
                deco71_12 += 1;
                spirv_words[i + 2] = 0;
                precise_stripped += 1;
            } else if decoration == 11 && word_count >= 4 && spirv_words[i + 3] == 1 {
                deco71_11pt += 1;
                spirv_words[i + 2] = 4;
                pointsize_stripped += 1;
            }
        }

        // OpMemberDecorate = 72, word layout: [struct_id, member, decoration, value...]
        if opcode == 72 && word_count >= 4 {
            let decoration = spirv_words[i + 3];
            if decoration == 12 {
                deco72_12 += 1;
                spirv_words[i + 3] = 0;
                precise_stripped += 1;
            }
        }

        i += word_count;
    }

    if deco71_12 > 0 || deco72_12 > 0 || deco71_11pt > 0 {
        eprintln!("[SPIRV-Patch] problematic decorations: OpDecorate(71) Precise={} PointSize={}, OpMemberDecorate(72) Precise={}",
            deco71_12, deco71_11pt, deco72_12);
    }

    (precise_stripped, pointsize_stripped)
}

/// Remap vertex input location decorations to 0-based sequential values.
///
/// NVN vertex attribute indices (e.g. 8–15) are used directly as SPIR-V
/// `@location` values.  Our vertex buffer layout uses `@location(0..7)`.
/// This function finds the minimum input location and subtracts it from
/// all input location decorations so the shader reads from 0-based slots.
///
/// Only meaningful for vertex shaders — applies a no-op for other stages
/// since they typically have no input location decorations.
pub fn nvn_remap_vertex_input_locations(spirv_words: &mut [u32]) -> usize {
    // Step 1: collect result IDs of all OpVariable(Input) instructions
    let mut input_ids = Vec::new();
    let mut i = 5;
    while i < spirv_words.len() {
        let w = spirv_words[i];
        let wc = (w >> 16) as usize;
        let op = w & 0xFFFF;
        if wc == 0 || i + wc > spirv_words.len() { break; }
        if op == 59 && wc >= 4 && spirv_words[i + 3] == 1 {
            input_ids.push(spirv_words[i + 2]);
        }
        i += wc;
    }
    if input_ids.is_empty() {
        return 0;
    }

    // Step 2: find Location(30) decorations on input variables
    let mut locs: Vec<(usize, u32)> = Vec::new();
    let mut i = 5;
    while i < spirv_words.len() {
        let w = spirv_words[i];
        let wc = (w >> 16) as usize;
        let op = w & 0xFFFF;
        if wc == 0 || i + wc > spirv_words.len() { break; }
        if op == 71 && wc >= 4 {
            let target = spirv_words[i + 1];
            let decoration = spirv_words[i + 2];
            if decoration == 30 && input_ids.contains(&target) {
                locs.push((i + 3, spirv_words[i + 3]));
            }
        }
        i += wc;
    }
    if locs.is_empty() { return 0; }

    let min_loc = locs.iter().map(|(_, v)| *v).min().unwrap();
    if min_loc == 0 {
        // The vertex buffer covers locations 0-11 (12 attributes, stride 192)
        // so NVN inputs at locations 8+ are already within range — no remap needed.
        return 0;
    }

    let count = locs.len();
    for &(word_idx, _) in &locs {
        spirv_words[word_idx] -= min_loc;
    }
    count
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

    let (vi, ii) = nvn_patch_vertex_builtins(spirv_words);
    if vi > 0 {
        patches.push(format!("VertexIndex→VertexId x{}", vi));
    }
    if ii > 0 {
        patches.push(format!("InstanceIndex→InstanceId x{}", ii));
    }

    let (ps, pt) = nvn_strip_problematic_decorations(spirv_words);
    if ps > 0 {
        patches.push(format!("Precise→RelaxedPrecision x{}", ps));
    }
    if pt > 0 {
        patches.push(format!("PointSizeBuiltIn→NoPerspective x{}", pt));
    }

    patches
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
