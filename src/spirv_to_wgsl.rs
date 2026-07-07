// SPIR-V → WGSL shader conversion pipeline.
//
// Strategy: SPIR‑V → spirv‑cross (Vulkan GLSL) → naga GLSL frontend → WGSL.
// The direct naga SPIR‑V frontend is not used because it cannot handle the
// NVN‑generated SPIR‑V from game shaders (unsupported storage classes,
// extensions, etc.).
//
// Before translation the caller should already have applied:
//   - NVN execution‑mode patches  (nvn_to_vulkan_patch)
//   - Pipeline layouts are built from shader descriptor reflection at runtime

use naga::front::glsl;
use naga::back::wgsl;

use std::process::Command;

use anyhow::{Result, anyhow};

/// Binding resource class, matching wgpu binding types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BindingClass {
    Uniform,
    Storage,
    Texture,
    Sampler,
}

/// A descriptor binding extracted from the naga IR module.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DescriptorInfo {
    pub set: u32,
    pub binding: u32,
    pub name: String,
    pub ty_str: String,
    pub class: BindingClass,
}

/// Extract descriptor bindings from a parsed naga module.
fn extract_descriptors_from_module(module: &naga::Module) -> Vec<DescriptorInfo> {
    let mut descriptors = Vec::new();

    for (handle, var) in module.global_variables.iter() {
        if let Some(binding) = &var.binding {
            let ty_str = module.types.get_handle(var.ty).ok()
                .map(|ty| {
                    match &ty.inner {
                        naga::TypeInner::Image { .. } => "Image".to_string(),
                        naga::TypeInner::Sampler { .. } => "Sampler".to_string(),
                        naga::TypeInner::Struct { .. } => {
                            ty.name.clone().unwrap_or_else(|| "Struct".to_string())
                        }
                        _ => format!("{:?}", ty.inner).chars().take(24).collect(),
                    }
                })
                .unwrap_or_else(|| "Unknown".to_string());

            let class = match var.space {
                naga::AddressSpace::Uniform => BindingClass::Uniform,
                naga::AddressSpace::Storage { .. } => BindingClass::Storage,
                _ => {
                    // Handle space: images / samplers
                    if ty_str == "Image" { BindingClass::Texture }
                    else if ty_str == "Sampler" { BindingClass::Sampler }
                    else { BindingClass::Uniform /* fallback */ }
                }
            };

            descriptors.push(DescriptorInfo {
                set: binding.group,
                binding: binding.binding,
                name: var.name.clone().unwrap_or_else(|| format!("var_{}_{}", binding.group, binding.binding)),
                ty_str,
                class,
            });
        }
    }
    descriptors
}

/// Merge VS/FS descriptor lists for a single wgpu pipeline layout.
///
/// NVN particle shaders reuse the same (set, binding) for VS storage buffers and FS
/// textures. Native FS samples via `@group(1) color_tex`; strip conflicting FS set-0
/// texture/sampler declarations from WGSL before compile ([`strip_fs_wgsl_conflicting_with_vs`]).
/// Here we omit FS texture/sampler entries that overlap VS storage slots.
pub fn merge_stage_pipeline_descriptors(
    vs_descs: &[DescriptorInfo],
    fs_descs: &[DescriptorInfo],
) -> Vec<DescriptorInfo> {
    let vs_storage: std::collections::HashSet<(u32, u32)> = vs_descs
        .iter()
        .filter(|d| d.class == BindingClass::Storage)
        .map(|d| (d.set, d.binding))
        .collect();
    let mut map: std::collections::HashMap<(u32, u32), DescriptorInfo> =
        std::collections::HashMap::new();
    for d in vs_descs {
        map.insert((d.set, d.binding), d.clone());
    }
    for d in fs_descs {
        let key = (d.set, d.binding);
        if vs_storage.contains(&key)
            && matches!(
                d.class,
                BindingClass::Texture
                    | BindingClass::Sampler
                    | BindingClass::Storage
                    | BindingClass::Uniform
            )
        {
            continue;
        }
        map.insert(key, d.clone());
    }
    let mut out: Vec<_> = map.into_values().collect();
    out.sort_by(|a, b| a.set.cmp(&b.set).then(a.binding.cmp(&b.binding)));
    out
}

fn is_wgsl_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Replace whole WGSL identifiers only (not substring matches inside other names).
fn replace_wgsl_identifier(src: &str, from: &str, to: &str) -> String {
    if from.is_empty() || from == to {
        return src.to_string();
    }
    let from_bytes = from.as_bytes();
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(from_bytes) {
            let before_ok = i == 0 || !is_wgsl_ident_byte(bytes[i - 1]);
            let after_idx = i + from_bytes.len();
            let after_ok = after_idx >= bytes.len() || !is_wgsl_ident_byte(bytes[after_idx]);
            if before_ok && after_ok {
                out.push_str(to);
                i = after_idx;
                continue;
            }
        }
        let ch = src[i..].chars().next().expect("valid utf8");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_wgsl_global_var_name(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("var") {
        return None;
    }
    let mut rest = line.strip_prefix("var")?.trim_start();
    if rest.starts_with('<') {
        let end = rest.find('>')? + 1;
        rest = rest[end..].trim_start();
    }
    let colon = rest.find(':')?;
    let name = rest[..colon].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn wgsl_global_var_is_texture(line: &str) -> bool {
    line.contains("texture_")
}

fn wgsl_global_var_is_sampler(line: &str) -> bool {
    line.contains(": sampler") || line.contains(": sampler;")
}

/// Remove FS `@group(0)` texture/sampler globals before native `@group(1)` injection.
///
/// Decoded BNSH particle FS often declares set-0 textures; native FS enhancement adds
/// `color_tex` / `color_sampler` at `@group(1)`. Stripping set-0 resource declarations
/// and redirecting usages avoids duplicate globals and pipeline binding clashes.
pub fn strip_fs_wgsl_conflicting_with_vs(
    fs_wgsl: &str,
    _vs_descs: &[DescriptorInfo],
    fs_descs: &[DescriptorInfo],
) -> String {
    let strip_bindings: std::collections::HashSet<(u32, u32)> = fs_descs
        .iter()
        .filter(|d| {
            // Native FS samples via injected `@group(1) color_tex` / `color_sampler`.
            // Drop all FS set-0 texture/sampler globals so they are not redeclared after enhance.
            d.set == 0
                && matches!(d.class, BindingClass::Texture | BindingClass::Sampler)
        })
        .map(|d| (d.set, d.binding))
        .collect();
    if strip_bindings.is_empty() {
        return fs_wgsl.to_string();
    }

    let stripped_textures: Vec<String> = fs_descs
        .iter()
        .filter(|d| d.class == BindingClass::Texture && strip_bindings.contains(&(d.set, d.binding)))
        .map(|d| d.name.clone())
        .collect();
    let stripped_samplers: Vec<String> = fs_descs
        .iter()
        .filter(|d| d.class == BindingClass::Sampler && strip_bindings.contains(&(d.set, d.binding)))
        .map(|d| d.name.clone())
        .collect();

    let mut texture_names: Vec<String> = stripped_textures;
    let mut sampler_names: Vec<String> = stripped_samplers;

    let mut out = String::new();
    let lines: Vec<&str> = fs_wgsl.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some((group, binding)) = parse_wgsl_group_binding(line) {
            if strip_bindings.contains(&(group, binding)) {
                let var_line = if line.contains("var ") {
                    line
                } else if i + 1 < lines.len() && lines[i + 1].trim().starts_with("var") {
                    lines[i + 1].trim()
                } else {
                    ""
                };
                let is_resource = !var_line.is_empty();
                if is_resource {
                    if let Some(name) = parse_wgsl_global_var_name(var_line) {
                        if wgsl_global_var_is_texture(var_line) {
                            texture_names.push(name);
                        } else if wgsl_global_var_is_sampler(var_line) {
                            sampler_names.push(name);
                        }
                    }
                    if line.contains("var ") {
                        i += 1;
                        continue;
                    }
                    i += 2;
                    continue;
                }
            }
        }
        out.push_str(lines[i]);
        out.push('\n');
        i += 1;
    }
    texture_names.sort_unstable();
    texture_names.dedup();
    sampler_names.sort_unstable();
    sampler_names.dedup();
    // Redirect stripped set-0 textures to the @group(1) emitter slots BY SAMPLER SLOT
    // (spirv-cross names them `texture_<N>_` / `sampler_<N>_` in slot order). Collapsing
    // everything onto color_tex bound the colour texture to every slot — dual-texture
    // emitters (smoke1_fireLine: smoke17 + fire02) never sampled their second texture
    // and the CmnBomb explosion rendered white.
    let slot_of = |name: &str| -> usize {
        name.trim_end_matches('_')
            .rsplit('_')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
            .min(2)
    };
    const TEX_TARGETS: [&str; 3] = ["color_tex", "alpha_tex", "slot2_tex"];
    const SAMPLER_TARGETS: [&str; 3] = ["color_sampler", "alpha_sampler", "slot2_sampler"];
    let mut redirected = out;
    for name in &texture_names {
        redirected = replace_wgsl_identifier(&redirected, name, TEX_TARGETS[slot_of(name)]);
    }
    for name in &sampler_names {
        redirected =
            replace_wgsl_identifier(&redirected, name, SAMPLER_TARGETS[slot_of(name)]);
    }
    // spirv-cross particle FS always samples via these identifiers in the body.
    if !strip_bindings.is_empty() {
        redirected = replace_wgsl_identifier(&redirected, "texture_0_", "color_tex");
        redirected = replace_wgsl_identifier(&redirected, "sampler_0_", "color_sampler");
        redirected = replace_wgsl_identifier(&redirected, "texture_1_", "alpha_tex");
        redirected = replace_wgsl_identifier(&redirected, "sampler_1_", "alpha_sampler");
        redirected = replace_wgsl_identifier(&redirected, "texture_2_", "slot2_tex");
        redirected = replace_wgsl_identifier(&redirected, "sampler_2_", "slot2_sampler");
    }
    redirected
}

fn wgsl_global_matches_descriptor(var_name: &str, desc_name: &str) -> bool {
    let var = var_name.trim_end_matches('_');
    let desc = desc_name.trim_end_matches('_');
    var == desc || var.starts_with(&format!("{desc}_")) || desc.starts_with(&format!("{var}_"))
}

fn next_free_binding(set: u32, occupied: &std::collections::HashSet<(u32, u32)>) -> u32 {
    let mut binding = 0u32;
    while occupied.contains(&(set, binding)) {
        binding += 1;
    }
    binding
}

fn rebind_wgsl_storage_global(
    wgsl: &str,
    set: u32,
    old_binding: u32,
    new_binding: u32,
    desc_name: &str,
) -> String {
    let lines: Vec<&str> = wgsl.lines().collect();
    let mut out = String::with_capacity(wgsl.len());
    let mut i = 0usize;
    while i < lines.len() {
        if let Some((g, b)) = parse_wgsl_group_binding(lines[i]) {
            if g == set && b == old_binding {
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j < lines.len() {
                    if let Some(var_name) = parse_wgsl_global_var_name(lines[j]) {
                        if wgsl_global_matches_descriptor(&var_name, desc_name) {
                            out.push_str(
                                &lines[i].replace(
                                    &format!("@binding({old_binding})"),
                                    &format!("@binding({new_binding})"),
                                ),
                            );
                            out.push('\n');
                            i += 1;
                            continue;
                        }
                    }
                }
            }
        }
        out.push_str(lines[i]);
        out.push('\n');
        i += 1;
    }
    if wgsl.ends_with('\n') {
        out
    } else {
        out.trim_end_matches('\n').to_string()
    }
}

/// Move FS storage/uniform globals off bindings already used by the vertex shader.
///
/// NVN pairs often place `cbuf_9`/`cbuf_16` on `@binding(0/1)` in the FS while the VS uses
/// `cbuf_1`/`cbuf_8` on the same slots. [`merge_stage_pipeline_descriptors`] keeps VS storage,
/// so without this remap the FS would read the wrong GPU buffers.
pub fn remap_fs_storage_bindings_for_vs(
    fs_wgsl: &str,
    vs_descs: &[DescriptorInfo],
    fs_descs: &[DescriptorInfo],
) -> (String, Vec<DescriptorInfo>) {
    let mut occupied: std::collections::HashSet<(u32, u32)> = vs_descs
        .iter()
        .filter(|d| matches!(d.class, BindingClass::Storage | BindingClass::Uniform))
        .map(|d| (d.set, d.binding))
        .collect();
    for d in fs_descs {
        if !matches!(d.class, BindingClass::Storage | BindingClass::Uniform) {
            occupied.insert((d.set, d.binding));
        }
    }

    let mut out_wgsl = fs_wgsl.to_string();
    let mut out_descs = fs_descs.to_vec();
    for (idx, d) in fs_descs.iter().enumerate() {
        if !matches!(d.class, BindingClass::Storage | BindingClass::Uniform) {
            continue;
        }
        let key = (d.set, d.binding);
        if !occupied.contains(&key) {
            occupied.insert(key);
            continue;
        }
        let new_binding = next_free_binding(d.set, &occupied);
        out_wgsl = rebind_wgsl_storage_global(
            &out_wgsl,
            d.set,
            d.binding,
            new_binding,
            &d.name,
        );
        out_descs[idx].binding = new_binding;
        occupied.insert((d.set, new_binding));
    }
    (out_wgsl, out_descs)
}

/// Like [`remap_fs_storage_bindings_for_vs`], but applies the same binding plan to two FS variants.
pub fn remap_fs_storage_bindings_for_vs_pair(
    fs_wgsl: &str,
    fs_wgsl_depth: &str,
    vs_descs: &[DescriptorInfo],
    fs_descs: &[DescriptorInfo],
) -> (String, String, Vec<DescriptorInfo>) {
    let mut occupied: std::collections::HashSet<(u32, u32)> = vs_descs
        .iter()
        .filter(|d| matches!(d.class, BindingClass::Storage | BindingClass::Uniform))
        .map(|d| (d.set, d.binding))
        .collect();
    for d in fs_descs {
        if !matches!(d.class, BindingClass::Storage | BindingClass::Uniform) {
            occupied.insert((d.set, d.binding));
        }
    }

    let mut out_wgsl = fs_wgsl.to_string();
    let mut out_depth = fs_wgsl_depth.to_string();
    let mut out_descs = fs_descs.to_vec();
    for (idx, d) in fs_descs.iter().enumerate() {
        if !matches!(d.class, BindingClass::Storage | BindingClass::Uniform) {
            continue;
        }
        let key = (d.set, d.binding);
        if !occupied.contains(&key) {
            occupied.insert(key);
            continue;
        }
        let new_binding = next_free_binding(d.set, &occupied);
        out_wgsl = rebind_wgsl_storage_global(
            &out_wgsl,
            d.set,
            d.binding,
            new_binding,
            &d.name,
        );
        out_depth = rebind_wgsl_storage_global(
            &out_depth,
            d.set,
            d.binding,
            new_binding,
            &d.name,
        );
        out_descs[idx].binding = new_binding;
        occupied.insert((d.set, new_binding));
    }
    (out_wgsl, out_depth, out_descs)
}

/// Parse-check WGSL before handing it to wgpu (catches stripped-binding regressions early).
pub fn validate_wgsl_shader(wgsl: &str, label: &str) -> Result<()> {
    naga::front::wgsl::parse_str(wgsl)
        .map(|_| ())
        .map_err(|e| anyhow!("WGSL validation failed for {label}: {e}"))
}

fn parse_wgsl_group_binding(line: &str) -> Option<(u32, u32)> {
    let line = line.trim();
    let group_start = line.find("@group(")? + 7;
    let group_end = line[group_start..].find(')')? + group_start;
    let group: u32 = line[group_start..group_end].parse().ok()?;
    let bind_marker = "@binding(";
    let bind_start = line.find(bind_marker)? + bind_marker.len();
    let bind_end = line[bind_start..].find(')')? + bind_start;
    let binding: u32 = line[bind_start..bind_end].parse().ok()?;
    Some((group, binding))
}

/// Convert SPIR-V bytes to words (u32 array).
pub fn bytes_to_words(data: &[u8]) -> Result<Vec<u32>> {
    if data.len() % 4 != 0 {
        return Err(anyhow!("SPIR-V data length not multiple of 4"));
    }
    Ok(data
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Detect fragment vs vertex from SPIR-V `OpEntryPoint` (more reliable than BNSH JSON for NVN).
pub fn spirv_is_fragment(spirv: &[u8]) -> Option<bool> {
    let words = bytes_to_words(spirv).ok()?;
    let mut i = 5usize;
    while i < words.len().min(1024) {
        let word_count = (words[i] >> 16) as usize;
        let opcode = words[i] & 0xffff;
        if opcode == 15 && i + 1 < words.len() {
            let model = words[i + 1];
            return Some(model == 4);
        }
        if word_count == 0 {
            break;
        }
        i += word_count;
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IoField {
    location: u32,
    name: String,
    ty: String,
}

fn parse_struct_io_fields(wgsl: &str, struct_name: &str) -> Vec<IoField> {
    let needle = format!("struct {struct_name} {{");
    let Some(start) = wgsl.find(&needle) else {
        return Vec::new();
    };
    let body = &wgsl[start + needle.len()..];
    let Some(closing) = body.find('}') else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    for line in body[..closing].lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("@builtin(position)") {
            fields.push(IoField {
                location: u32::MAX,
                name: "gl_Position".to_string(),
                ty: "vec4<f32>".to_string(),
            });
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("@location(") else {
            continue;
        };
        let Some(paren) = rest.find(')') else {
            continue;
        };
        let Ok(loc) = rest[..paren].parse::<u32>() else {
            continue;
        };
        let after = rest[paren + 1..].trim();
        if let Some((name, ty)) = parse_field_name_type(after) {
            fields.push(IoField {
                location: loc,
                name,
                ty,
            });
        }
    }
    fields
}

fn type_token_end(s: &str) -> usize {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return i,
            _ => {}
        }
    }
    s.len()
}

fn parse_field_name_type(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let colon = line.find(':')?;
    let name = line[..colon].trim().to_string();
    let ty_end = type_token_end(&line[colon + 1..]);
    let ty = line[colon + 1..][..ty_end].trim().to_string();
    if name.is_empty() || ty.is_empty() {
        None
    } else {
        Some((name, ty))
    }
}

fn fragment_input_struct_name(fs_wgsl: &str) -> Option<String> {
    let frag = fs_wgsl.find("@fragment")?;
    let rest = &fs_wgsl[frag..];
    let fn_pos = rest.find("fn ")?;
    let sig = &rest[fn_pos..];
    let open = sig.find('(')?;
    let close = sig[open + 1..].find(')')? + open + 1;
    let params = &sig[open + 1..close];
    let colon = params.find(':')?;
    let name: String = params[colon + 1..]
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn parse_inline_location_params(params: &str) -> Vec<IoField> {
    let mut fields = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = params[search_from..].find("@location(") {
        let loc_start = search_from + rel;
        let rest = &params[loc_start + "@location(".len()..];
        let Some(paren) = rest.find(')') else {
            break;
        };
        let Ok(loc) = rest[..paren].parse::<u32>() else {
            search_from = loc_start + 1;
            continue;
        };
        let after = rest[paren + 1..].trim();
        if let Some((name, ty)) = parse_field_name_type(after) {
            fields.push(IoField {
                location: loc,
                name,
                ty,
            });
        }
        search_from = loc_start + "@location(".len() + paren + 1;
    }
    fields
}

fn parse_entry_point_location_params(wgsl: &str, marker: &str) -> Vec<IoField> {
    let Some(marker_pos) = wgsl.find(marker) else {
        return Vec::new();
    };
    let after = &wgsl[marker_pos..];
    let Some(fn_pos) = after.find("fn ") else {
        return Vec::new();
    };
    let sig = &after[fn_pos..];
    let Some(open) = sig.find('(') else {
        return Vec::new();
    };
    let mut depth = 0usize;
    let mut close_idx = None;
    for (i, ch) in sig[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_idx = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close_idx else {
        return Vec::new();
    };
    parse_inline_location_params(&sig[open + 1..close])
}

/// Fragment-stage varyings only (excludes `@location` on fragment output).
pub fn fragment_io_fields(fs_wgsl: &str) -> Vec<IoField> {
    let inline = parse_entry_point_location_params(fs_wgsl, "@fragment");
    if !inline.is_empty() {
        return inline;
    }
    if let Some(name) = fragment_input_struct_name(fs_wgsl) {
        let fields = parse_struct_io_fields(fs_wgsl, &name);
        if !fields.is_empty() {
            return fields;
        }
    }
    // Last resort: first struct with in_attr* fields before @fragment.
    if let Some(frag) = fs_wgsl.find("@fragment") {
        let head = &fs_wgsl[..frag];
        for line in head.lines().rev() {
            if line.starts_with("struct ") {
                let Some(rest) = line.trim().strip_prefix("struct ") else {
                    continue;
                };
                let Some(raw_name) = rest.split_whitespace().next() else {
                    continue;
                };
                let name = raw_name
                    .trim_end_matches(" {")
                    .trim_end_matches('{')
                    .to_string();
                let fields = parse_struct_io_fields(fs_wgsl, &name);
                if fields.iter().any(|f| f.name.starts_with("in_attr")) {
                    return fields;
                }
            }
        }
    }
    Vec::new()
}

/// Collect `@location(N)` indices referenced by the fragment shader inputs.
pub fn fragment_input_locations(fs_wgsl: &str) -> Vec<u32> {
    let mut locs: Vec<u32> = fragment_io_fields(fs_wgsl)
        .into_iter()
        .map(|f| f.location)
        .collect();
    locs.sort_unstable();
    locs.dedup();
    locs
}

fn vs_output_name_for_fs_input(fs_name: &str) -> String {
    if let Some(rest) = fs_name.strip_prefix("in_") {
        format!("out_{rest}")
    } else {
        format!("out_{fs_name}")
    }
}

fn return_vertex_output_contains(wgsl: &str, name: &str) -> bool {
    let Some(start) = wgsl.rfind("return VertexOutput(") else {
        return false;
    };
    let rest = &wgsl[start + "return VertexOutput(".len()..];
    let Some(end) = rest.find(')') else {
        return false;
    };
    rest[..end].contains(name)
}

/// spirv-cross often wires outputs via `let _e239 = out_attr0_;` rather than naming
/// them directly in `return VertexOutput(...)`.
fn vertex_output_is_wired(wgsl: &str, out_name: &str) -> bool {
    if return_vertex_output_contains(wgsl, out_name) {
        return true;
    }
    let Some(ret) = wgsl.rfind("return VertexOutput(") else {
        return false;
    };
    let before_return = &wgsl[..ret];
    // spirv-cross snapshots varyings as `let _eNNN = out_attr2_;` — that is a read, not a write.
    before_return.contains(&format!("{out_name} = "))
}

/// True when every fragment varying has a matching name in `return VertexOutput(...)`.
pub fn vertex_return_wires_fs_inputs(vs_wgsl: &str, fs_wgsl: &str) -> bool {
    let fs_inputs = fragment_io_fields(fs_wgsl);
    let vs_outputs = parse_struct_io_fields(vs_wgsl, "VertexOutput");
    for fs_in in &fs_inputs {
        let out_name = vs_outputs
            .iter()
            .find(|o| o.location == fs_in.location)
            .map(|o| o.name.clone())
            .unwrap_or_else(|| vs_output_name_for_fs_input(&fs_in.name));
        if !vertex_output_is_wired(vs_wgsl, &out_name) {
            return false;
        }
    }
    true
}

fn rebuild_vertex_output_struct(wgsl: &str, extra_fields: &[(u32, String, String)]) -> String {
    let start_marker = "struct VertexOutput {";
    let Some(start) = wgsl.find(start_marker) else {
        return wgsl.to_string();
    };
    let body = &wgsl[start + start_marker.len()..];
    let Some(close_rel) = body.find('}') else {
        return wgsl.to_string();
    };
    let end = start + start_marker.len() + close_rel;

    let mut fields = parse_struct_io_fields(wgsl, "VertexOutput");
    for (loc, name, ty) in extra_fields {
        if fields.iter().any(|f| f.location == *loc) {
            continue;
        }
        fields.push(IoField {
            location: *loc,
            name: name.clone(),
            ty: ty.clone(),
        });
    }

    let mut new_body = String::from("struct VertexOutput {\n");
    let mut loc_fields: Vec<_> = fields.iter().filter(|f| f.location != u32::MAX).collect();
    loc_fields.sort_by_key(|f| f.location);
    for f in loc_fields {
        new_body.push_str(&format!(
            "    @location({}) {}: {},\n",
            f.location, f.name, f.ty
        ));
    }
    if let Some(pos) = fields.iter().find(|f| f.name == "gl_Position") {
        new_body.push_str(&format!(
            "    @builtin(position) {}: {},\n",
            pos.name, pos.ty
        ));
    }
    new_body.push('}');

    format!("{}{}{}", &wgsl[..start], new_body, &wgsl[end + 1..])
}

fn find_vertex_output_insertion_point(wgsl: &str) -> Option<usize> {
    let needle = "struct VertexOutput {";
    let start = wgsl.find(needle)?;
    let body = &wgsl[start + needle.len()..];
    let closing = start + needle.len() + body.find('}')?;
    let body_str = &wgsl[start..closing];
    if let Some(bp) = body_str.rfind("@builtin(position)") {
        body_str[..bp].rfind('\n').map(|i| start + i + 1)
    } else {
        Some(closing)
    }
}

fn append_private_var(wgsl: &mut String, name: &str, ty: &str) {
    let decl = format!("var<private> {name}: {ty};");
    if wgsl.contains(&decl) {
        return;
    }
    // Keep new declarations in the pre-entry private-var block (before `@vertex` / `@fragment`),
    // never appended after struct fields or entry-point signatures.
    let search_end = wgsl
        .find("@vertex")
        .or_else(|| wgsl.find("@fragment"))
        .unwrap_or(wgsl.len());
    let head = &wgsl[..search_end];
    if let Some(pos) = head.rfind("var<private> out_attr") {
        let line_end = head[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(search_end);
        wgsl.insert_str(line_end, &format!("{decl}\n"));
    } else if let Some(pos) = head.rfind("var<private>") {
        let line_end = head[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(search_end);
        wgsl.insert_str(line_end, &format!("{decl}\n"));
    } else {
        wgsl.insert_str(search_end, &format!("{decl}\n"));
    }
}

fn add_assignment_before_return(wgsl: &mut String, out: &str, inp: &str) {
    let assign = format!("{out} = {inp};");
    if wgsl.contains(&assign) {
        return;
    }
    let Some(pos) = wgsl.rfind("return VertexOutput(") else {
        return;
    };
    wgsl.insert_str(pos, &format!("\n    {assign}\n    "));
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' | '<' => depth += 1,
            ')' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(s[start..].trim().to_string());
    args
}

fn vertex_return_close_index(wgsl: &str, ret_start: usize) -> Option<usize> {
    let after_open = &wgsl[ret_start + "return VertexOutput(".len()..];
    let mut depth = 1usize;
    for (i, ch) in after_open.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(ret_start + "return VertexOutput(".len() + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_vertex_return_args(wgsl: &str) -> Option<(usize, Vec<String>)> {
    let ret_start = wgsl.rfind("return VertexOutput(")?;
    let after_open = &wgsl[ret_start + "return VertexOutput(".len()..];
    let close_rel = vertex_return_close_index(wgsl, ret_start)? - ret_start
        - "return VertexOutput(".len()
        - 1;
    let args_str = after_open[..close_rel].trim();
    if args_str.is_empty() {
        return Some((ret_start, Vec::new()));
    }
    Some((ret_start, split_top_level_commas(args_str)))
}

fn extend_vertex_return(wgsl: &str, new_output_names: &[String]) -> String {
    if new_output_names.is_empty() {
        return wgsl.to_string();
    }
    let Some((ret_start, mut args)) = parse_vertex_return_args(wgsl) else {
        return wgsl.to_string();
    };
    if args.is_empty() {
        return wgsl.to_string();
    }
    let pos_arg = args.pop().unwrap();
    args.extend(new_output_names.iter().cloned());
    args.push(pos_arg);
    let new_return = format!("return VertexOutput({});", args.join(", "));
    let Some(end) = vertex_return_close_index(wgsl, ret_start) else {
        return wgsl.to_string();
    };
    format!("{}{}{}", &wgsl[..ret_start], new_return, &wgsl[end..])
}

fn replace_vertex_return(wgsl: &str, outputs: &[IoField]) -> String {
    let Some(ret_start) = wgsl.rfind("return VertexOutput(") else {
        return wgsl.to_string();
    };
    let after_open = &wgsl[ret_start + "return VertexOutput(".len()..];
    let mut depth = 1usize;
    let mut close_pos = None;
    for (i, ch) in after_open.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_pos = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close_pos) = close_pos else {
        return wgsl.to_string();
    };
    let args: Vec<String> = outputs.iter().map(|f| f.name.clone()).collect();
    let new_return = format!("return VertexOutput({});", args.join(", "));
    let end = ret_start + "return VertexOutput(".len() + close_pos + 1;
    format!(
        "{}{}{}",
        &wgsl[..ret_start],
        new_return,
        &wgsl[end..]
    )
}

/// Patch vertex WGSL so its outputs satisfy the paired fragment shader's varyings.
///
/// spirv-cross often declares `VertexOutput` fields without wiring them into
/// `return VertexOutput(...)`. wgpu rejects the pipeline when any FS `@location`
/// is missing from the previous stage.
pub fn patch_vertex_wgsl(vs_wgsl: &str, fs_wgsl: &str) -> String {
    patch_vertex_wgsl_with_hint(vs_wgsl, fs_wgsl, None)
}

/// Like [`patch_vertex_wgsl`], but accepts an optional registry/reflection VS profile hint.
pub fn patch_vertex_wgsl_with_hint(
    vs_wgsl: &str,
    fs_wgsl: &str,
    vs_hint: Option<crate::shader_registry::ShaderVsProfile>,
) -> String {
    // Wire CPU vertex attrs before billboard override so clip expansion uses attr6 half-extents.
    let vs_wired = wire_billboard_vertex_inputs(&wire_vertex_simulation_varyings(vs_wgsl));
    let is_billboard = billboard_particle_vs_with_hint(&vs_wired, vs_hint);
    let native_env = crate::fx_env::fx_native_vs_pos_enabled();
    let native_trust = trusts_native_position_chain(&vs_wired);
    if std::env::var("FX_VS_BRANCH_DEBUG").is_ok() {
        let branch = if !is_billboard {
            "passthrough"
        } else if native_env && native_trust {
            "native-chain"
        } else if native_env {
            "finalize-clip"
        } else {
            "cpu-override"
        };
        eprintln!(
            "[VS-BRANCH] len={} hint={:?} billboard={} native_env={} native_trust={} -> {}",
            vs_wgsl.len(), vs_hint, is_billboard, native_env, native_trust, branch
        );
    }
    let vs_wgsl_owned;
    let vs_wgsl: &str = if is_billboard {
        if native_env && native_trust {
            // Full Family-B billboards with a trustworthy NVN position chain.
            &vs_wired
        } else if native_env {
            vs_wgsl_owned = finalize_native_vs_clip_position(&vs_wired);
            &vs_wgsl_owned
        } else {
            // Legacy fallback: replace clip position (and colour varyings) after main_1().
            vs_wgsl_owned = override_billboard_position(&vs_wired);
            &vs_wgsl_owned
        }
    } else {
        &vs_wired
    };

    let fs_inputs = fragment_io_fields(fs_wgsl);
    if fs_inputs.is_empty() {
        return ensure_cpu_particle_varying_passthrough(vs_wgsl, vs_wgsl);
    }

    let mut result = vs_wgsl.to_string();
    let mut vs_outputs = parse_struct_io_fields(&result, "VertexOutput");
    if vs_outputs.is_empty() {
        let wired = wire_vertex_simulation_varyings(&result);
        return ensure_cpu_particle_varying_passthrough(&wired, vs_wgsl);
    }
    let mut vs_inputs = parse_struct_io_fields(&result, "VertexInput");
    if vs_inputs.is_empty() {
        vs_inputs = parse_entry_point_location_params(&result, "@vertex");
    }

    let mut new_private_vars: Vec<(String, String)> = Vec::new();
    for fs_in in &fs_inputs {
        let out_name = vs_output_name_for_fs_input(&fs_in.name);
        if vs_outputs.iter().any(|o| o.location == fs_in.location) {
            continue;
        }
        new_private_vars.push((out_name.clone(), fs_in.ty.clone()));
        vs_outputs.push(IoField {
            location: fs_in.location,
            name: out_name,
            ty: fs_in.ty.clone(),
        });
    }

    if new_private_vars.is_empty() {
        return ensure_cpu_particle_varying_passthrough(&result, vs_wgsl);
    }

    let missing: Vec<(u32, String, String)> = vs_outputs
        .iter()
        .filter(|o| new_private_vars.iter().any(|(n, _)| n == &o.name))
        .map(|o| (o.location, o.name.clone(), o.ty.clone()))
        .collect();
    result = rebuild_vertex_output_struct(&result, &missing);
    for (name, ty) in &new_private_vars {
        append_private_var(&mut result, name, ty);
    }

    let mut new_assignments = String::new();
    for (out_name, _) in &new_private_vars {
        if vertex_output_is_wired(&result, out_name) {
            continue;
        }
        let Some(fs_in) = fs_inputs.iter().find(|f| vs_output_name_for_fs_input(&f.name) == *out_name)
        else {
            continue;
        };
        if preserve_native_vs_varyings(vs_wgsl)
            && matches!(fs_in.location, 0 | 1 | 2)
        {
            continue;
        }
        if let Some(vin) = vs_inputs.iter().find(|v| v.location == fs_in.location) {
            new_assignments.push_str(&format!(
                "\n    {out_name} = {};",
                vertex_input_source_expr(&result, &vin.name)
            ));
        } else if let Some(vin) = vs_inputs.iter().find(|v| v.name == fs_in.name) {
            new_assignments.push_str(&format!(
                "\n    {out_name} = {};",
                vertex_input_source_expr(&result, &vin.name)
            ));
        }
    }

    if !new_assignments.is_empty() {
        if let Some(pos) = result.rfind("return VertexOutput(") {
            result.insert_str(pos, &format!("{new_assignments}\n    "));
        }
    }

    let new_names: Vec<String> = new_private_vars.into_iter().map(|(n, _)| n).collect();
    let result = extend_vertex_return(&result, &new_names);
    ensure_cpu_particle_varying_passthrough(&result, vs_wgsl)
}

fn out_attr2_snapshot_line_start(wgsl: &str) -> Option<usize> {
    let mut last = None;
    let mut offset = 0usize;
    for line in wgsl.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("let _e") && trimmed.contains("out_attr2_") {
            last = Some(offset);
        }
        offset += line.len() + 1;
    }
    last
}

fn vs_main1_body(wgsl: &str) -> &str {
    let Some(start) = wgsl.find("fn main_1()") else {
        return "";
    };
    let rest = &wgsl[start..];
    let end = rest
        .find("\n@vertex")
        .or_else(|| rest.find("\nfn main("))
        .unwrap_or(rest.len());
    &rest[..end]
}

fn vs_main1_reads_in_attr2(wgsl: &str) -> bool {
    vs_main1_body(wgsl).contains("in_attr2_1")
}

fn should_preserve_native_uv_varyings(vs: &str) -> bool {
    vs_has_native_uv_chain(vs) && vs_main1_reads_in_attr2(vs)
}

/// Forward CPU-uploaded quad corner UVs (attr2) over the native NVN UV chain.
///
/// spirv-cross snapshots `out_attr2_` into `let _eNNN = out_attr2_` before
/// `return VertexOutput(...)`. Any passthrough assignment must run before that
/// snapshot, not before the return (which still exports the stale snapshot).
fn ensure_cpu_quad_uv_passthrough(vs: &str, native_hint: &str) -> String {
    if should_preserve_native_uv_varyings(native_hint) {
        return vs.to_string();
    }
    if !vs.contains("out_attr2_") {
        return vs.to_string();
    }
    let mut result = vs.to_string();
    if !result.contains("in_attr2_1") {
        append_private_var(&mut result, "in_attr2_1", "vec4<f32>");
        ensure_vertex_entry_locations(&mut result, &[(2, "in_attr2_", "in_attr2_1")]);
    }
    const ASSIGN_LINE: &str = "out_attr2_ = in_attr2_1;";
    const ASSIGN: &str = "        out_attr2_ = in_attr2_1;\n";
    result = result
        .lines()
        .filter(|line| line.trim() != ASSIGN_LINE)
        .collect::<Vec<_>>()
        .join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    let snapshot = out_attr2_snapshot_line_start(&result);
    if let Some(pos) = result.find("out_attr1_ = in_attr1_1;") {
        if snapshot.map(|s| pos < s).unwrap_or(true) {
            let insert = pos + "out_attr1_ = in_attr1_1;".len();
            result.insert_str(insert, &format!("\n{ASSIGN}"));
            return result;
        }
    }
    if let Some(line_start) = snapshot {
        result.insert_str(line_start, ASSIGN);
        return result;
    }
    if let Some(pos) = result.rfind("return VertexOutput(") {
        result.insert_str(pos, ASSIGN);
    }
    result
}

/// Forward CPU attr2 (quad corners) and attr5 (flipbook tile origin) over native NVN varyings
/// when the decoded VS does not already compute them (see [`preserve_native_vs_varyings`]).
fn ensure_cpu_particle_varying_passthrough(vs: &str, native_hint: &str) -> String {
    ensure_cpu_attr5_passthrough(&ensure_cpu_quad_uv_passthrough(vs, native_hint))
}

fn ensure_cpu_varying_assign(result: &mut String, out_name: &str, in_private: &str) {
    let assign_line = format!("{out_name} = {in_private};");
    let assign = format!("        {assign_line}\n");
    *result = result
        .lines()
        .filter(|line| line.trim() != assign_line)
        .collect::<Vec<_>>()
        .join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    let snapshot = out_attr2_snapshot_line_start(result);
    if let Some(pos) = result.find("out_attr2_ = in_attr2_1;") {
        if snapshot.map(|s| pos < s).unwrap_or(true) {
            let insert = pos + "out_attr2_ = in_attr2_1;".len();
            result.insert_str(insert, &format!("\n{assign}"));
            return;
        }
    }
    if let Some(pos) = result.find("out_attr1_ = in_attr1_1;") {
        if snapshot.map(|s| pos < s).unwrap_or(true) {
            let insert = pos + "out_attr1_ = in_attr1_1;".len();
            result.insert_str(insert, &format!("\n{assign}"));
            return;
        }
    }
    if let Some(line_start) = snapshot {
        result.insert_str(line_start, &assign);
        return;
    }
    if let Some(pos) = result.rfind("return VertexOutput(") {
        result.insert_str(pos, &assign);
    }
}

/// Forward CPU-uploaded flipbook tile origin (attr5.xy) over the native NVN UV offset chain.
fn ensure_cpu_attr5_passthrough(vs: &str) -> String {
    if !vs.contains("out_attr5_") {
        return vs.to_string();
    }
    let mut result = vs.to_string();
    if !result.contains("in_attr5_1") {
        append_private_var(&mut result, "in_attr5_1", "vec4<f32>");
        ensure_vertex_entry_locations(&mut result, &[(5, "in_attr5_", "in_attr5_1")]);
    }
    ensure_cpu_varying_assign(&mut result, "out_attr5_", "in_attr5_1");
    result
}

/// Ensure CPU-uploaded particle attrs reach the VS when spirv-cross omits them from `@vertex main`.
pub(crate) fn wire_billboard_vertex_inputs(vs: &str) -> String {
    let mut result = vs.to_string();
    const ATTRS: &[(u32, &str, &str)] = &[
        (2, "in_attr2_", "in_attr2_1"),
        (6, "in_attr6_", "in_attr6_1"),
        (7, "in_attr7_", "in_attr7_1"),
        (8, "in_attr8_", "in_attr8_1"),
        (9, "in_attr9_", "in_attr9_1"),
    ];
    for &(location, param_name, private_name) in ATTRS {
        if result.contains(private_name) {
            ensure_vertex_entry_locations(&mut result, &[(location, param_name, private_name)]);
        }
    }
    // CPU uploads attr5 (flipbook origin) and attr6 (billboard half-extents) for particle VS.
    // Family-A bomb-style VS may omit in_attr4_1 but still need attr6 for FS life/basis varyings.
    let particle_vs = result.contains("in_attr0_1")
        && result.contains("cbuf_9_1_")
        && (result.contains("main_1();") || result.contains("fn main_1()"))
        && (result.contains("in_attr4_1")
            || uses_cbuf8_vp(&result)
            || uses_cbuf9_vp(&result)
            || vs_has_native_color_chain(&result));
    if particle_vs {
        for &(location, param_name, private_name) in &[
            (2, "in_attr2_", "in_attr2_1"),
            (5, "in_attr5_", "in_attr5_1"),
            (6, "in_attr6_", "in_attr6_1"),
        ] {
            if !result.contains(private_name) {
                append_private_var(&mut result, private_name, "vec4<f32>");
            }
            ensure_vertex_entry_locations(&mut result, &[(location, param_name, private_name)]);
        }
    } else if result.contains("out_attr5_") {
        if !result.contains("in_attr5_1") {
            append_private_var(&mut result, "in_attr5_1", "vec4<f32>");
        }
        ensure_vertex_entry_locations(&mut result, &[(5, "in_attr5_", "in_attr5_1")]);
    }
    // attr2/attr5 passthrough is handled by ensure_cpu_particle_varying_passthrough.
    result
}

pub(crate) fn uses_cbuf8_vp(wgsl: &str) -> bool {
    wgsl.contains("cbuf_8_1_._m0_[8]")
        || wgsl.contains("cbuf_8_1_._m0_[9]")
        || wgsl.contains("cbuf_8_1_._m0_[10]")
        || wgsl.contains("cbuf_8_1_._m0_[11]")
}

pub(crate) fn uses_cbuf9_vp(wgsl: &str) -> bool {
    wgsl.contains("cbuf_9_1_._m0_[0]")
        || wgsl.contains("cbuf_9_1_._m0_[1]")
        || wgsl.contains("cbuf_9_1_._m0_[2]")
        || wgsl.contains("cbuf_9_1_._m0_[3]")
}

/// Model/mesh VS read texture dimensions from cbuf_9[17]; particle billboards do not.
/// Mesh shaders also read cbuf_8[17] (model position params); particle VS only reads [18].
fn uses_mesh_tex_dims_slot(wgsl: &str) -> bool {
    if wgsl.contains("cbuf_9_1_._m0_[17]") && wgsl.contains("cbuf_8_1_._m0_[17]") {
        return true;
    }
    wgsl.contains("cbuf_9_1_._m0_[17]")
        && !wgsl.contains("in_attr6_1")
        && !wgsl.contains("in_attr2_1")
}

/// Model shaders read both cbuf_8[17] and [18]; particle VS only reads [18].
fn uses_model_position_param_slot(wgsl: &str) -> bool {
    wgsl.contains("cbuf_8_1_._m0_[17]")
        && !wgsl.contains("in_attr6_1")
        && !wgsl.contains("in_attr2_1")
}

/// Skinned mesh VS reads emitter/bone world rows from cbuf_8[12..14] without particle corners.
fn uses_model_world_rows_without_corners(wgsl: &str) -> bool {
    wgsl.contains("cbuf_8_1_._m0_[12]")
        && wgsl.contains("cbuf_8_1_._m0_[13]")
        && wgsl.contains("cbuf_8_1_._m0_[14]")
        && !wgsl.contains("in_attr6_1")
        && !wgsl.contains("in_attr2_1")
}

/// True when decoded WGSL or registry metadata indicates a mesh/model vertex shader.
pub fn is_mesh_model_vs(wgsl: &str, hint: Option<crate::shader_registry::ShaderVsProfile>) -> bool {
    if hint == Some(crate::shader_registry::ShaderVsProfile::MeshModel) {
        return true;
    }
    if hint == Some(crate::shader_registry::ShaderVsProfile::ParticleBillboard) {
        return false;
    }
    uses_mesh_tex_dims_slot(wgsl)
        || uses_model_position_param_slot(wgsl)
        || uses_model_world_rows_without_corners(wgsl)
}

/// Classify a decoded vertex shader for registry metadata.
pub fn classify_vs_profile(wgsl: &str) -> crate::shader_registry::ShaderVsProfile {
    if is_mesh_model_vs(wgsl, None) {
        crate::shader_registry::ShaderVsProfile::MeshModel
    } else if billboard_particle_vs_with_hint(wgsl, None) {
        crate::shader_registry::ShaderVsProfile::ParticleBillboard
    } else {
        crate::shader_registry::ShaderVsProfile::Unknown
    }
}

/// Family-B particle VS stores VP in `cbuf_9[0..3]` instead of `cbuf_8[8..11]`.
fn family_b_vp(wgsl: &str) -> bool {
    uses_cbuf9_vp(wgsl) && !uses_cbuf8_vp(wgsl)
}

/// Camera basis right vector slot used by both Family-A and full Family-B chains.
fn uses_cbuf9_camera_basis(wgsl: &str) -> bool {
    wgsl.contains("cbuf_9_1_._m0_[46]")
}

fn native_particle_vs_base(wgsl: &str) -> bool {
    wgsl.contains("main_1();")
        && wgsl.contains("in_attr0_1")
        && wgsl.contains("in_attr4_1")
        && (wgsl.contains("in_attr6_1") || wgsl.contains("in_attr2_1"))
        && wgsl.contains("cbuf_9_1_")
        && uses_cbuf9_camera_basis(wgsl)
        && wgsl.contains("gl_Position")
}

/// Partial Family-B billboard VS: VP in `cbuf_9[0..3]` but no `cbuf_9[46]` camera basis.
///
/// These cannot trust `main_1()` clip position. [`patch_vertex_wgsl`] applies
/// [`finalize_native_vs_clip_position`] instead, deriving up from the VP forward column
/// when attr7 billboard type is absent.
pub fn is_partial_family_b_billboard_vs(wgsl: &str) -> bool {
    billboard_particle_vs_with_hint(wgsl, None) && family_b_vp(wgsl) && !uses_cbuf9_camera_basis(wgsl)
}

/// True when the decoded VS has the inputs needed for VP×billboard finalize
/// (`finalize_native_vs_clip_position` / `override_billboard_position`).
///
/// Mesh/model VS may also declare attr0/4/6, but they read model-only cbuf slots or omit the
/// camera-basis slot `[46]` that particle billboards use for corner expansion.
pub fn billboard_particle_vs(wgsl: &str) -> bool {
    billboard_particle_vs_with_hint(wgsl, None)
}

pub fn billboard_particle_vs_with_hint(
    wgsl: &str,
    hint: Option<crate::shader_registry::ShaderVsProfile>,
) -> bool {
    if is_mesh_model_vs(wgsl, hint) {
        return false;
    }
    // Decoded WGSL defines `fn main_1()`; patched @vertex main calls `main_1();`.
    let has_main_1 = wgsl.contains("main_1();") || wgsl.contains("fn main_1()");
    if !has_main_1 || !wgsl.contains("gl_Position") {
        return false;
    }
    if uses_mesh_tex_dims_slot(wgsl) || uses_model_position_param_slot(wgsl) {
        return false;
    }
    let vp_buf = if uses_cbuf8_vp(wgsl) {
        "cbuf_8_1_"
    } else if uses_cbuf9_vp(wgsl) {
        "cbuf_9_1_"
    } else {
        return false;
    };
    if !wgsl.contains("in_attr0_1") || !wgsl.contains("in_attr4_1") || !wgsl.contains(vp_buf) {
        return false;
    }
    if !wgsl.contains("cbuf_9_1_") {
        return false;
    }
    let family_a = uses_cbuf8_vp(wgsl);
    let partial_family_b = family_b_vp(wgsl) && !uses_cbuf9_camera_basis(wgsl);
    // Full Family-B billboards reference camera basis at cbuf_9[46]. Family-A VP lives in
    // cbuf_8[8..11] and often omits an explicit cbuf_9[46] read in decoded WGSL.
    if !family_a && !partial_family_b && !wgsl.contains("cbuf_9_1_._m0_[46]") {
        return false;
    }
    wgsl.contains("in_attr6_1")
        || wgsl.contains("in_attr2_1")
}

/// Full Family-B reads VP from cbuf_9[0..3] with camera basis at cbuf_9[46]. Partial Family-B
/// omits cbuf_9[46] — see [`is_partial_family_b_billboard_vs`].
///
/// Family-A billboards (cbuf_8 VP at [8..11]) still run an NVN fma pre-transform through
/// cbuf_8[0..3]/[12..14] that our PTCL-backed evaluator does not reproduce exactly (Samus bomb
/// and similar effects rasterize off-screen when `main_1()` clip position is trusted). Keep
/// native colour/UV varyings from `main_1()` but apply [`finalize_native_vs_clip_position`].
pub fn trusts_native_position_chain(wgsl: &str) -> bool {
    if !native_particle_vs_base(wgsl) {
        return false;
    }
    if wgsl.contains("cbuf_8_1_") && uses_cbuf8_vp(wgsl) {
        return false;
    }
    family_b_vp(wgsl)
}

/// Ensure CPU-simulated vertex inputs (attr10 crossfade, attr11 extra-tex UV) exist on the VS
/// even when spirv-cross omitted them from the decoded BNSH signature.
pub fn wire_native_simulation_vertex_inputs(vs_wgsl: &str) -> String {
    wire_vertex_simulation_varyings(vs_wgsl)
}

/// Forward CPU-simulated per-particle varyings the native NVN chains do not reliably
/// propagate (flipbook crossfade attr10, TextureAnim3–4 UV offsets in attr11).
pub fn wire_vertex_simulation_varyings(vs_wgsl: &str) -> String {
    let mut result = vs_wgsl.to_string();
    // CPU always uploads attr10 (crossfade), attr11 (tex3/4 UV), attr12 (tex5 UV).
    wire_native_attr_passthrough(&mut result, 10, "in_attr10_", "in_attr10_1", "out_attr10_");
    wire_native_attr_passthrough(&mut result, 11, "in_attr11_", "in_attr11_1", "out_attr11_");
    wire_native_attr_passthrough(&mut result, 12, "in_attr12_", "in_attr12_1", "out_attr12_");
    if crate::fx_env::fx_native_vs_pos_enabled() {
        result = insert_after_main1_varying_forwards(&result);
    }
    result
}

/// Ensure the fragment shader declares a varying the vertex stage forwards for native
/// crossfade (attr10) when absent from decoded BNSH FS signatures. When the VS cannot
/// forward attr10 (some shader families), [`enhance_native_fragment_wgsl`] falls back to
/// cbuf_9[9].x blend — no FS varying is injected in that case.
pub fn wire_crossfade_fragment_input(fs_wgsl: &str, vs_wgsl: &str) -> String {
    if fs_wgsl.contains("in_attr10_1") {
        return fs_wgsl.to_string();
    }
    let vs_forwards_attr10 = vs_wgsl.contains("in_attr10_1")
        || vs_wgsl.contains("out_attr10_")
        || vs_wgsl.contains("@location(10) in_attr10_");
    if !vs_forwards_attr10 {
        return fs_wgsl.to_string();
    }
    inject_fragment_location_input(fs_wgsl, 10, "in_attr10_", "vec4<f32>", "in_attr10_1")
}

/// Ensure the fragment shader receives CPU quad corner UVs (attr2) for atlas sampling.
/// Decoded BNSH FS often omits `@location(2)` and incorrectly relied on attr6 half-extents.
pub fn wire_quad_uv_fragment_input(fs_wgsl: &str, vs_wgsl: &str) -> String {
    if fs_wgsl.contains("in_attr2_1") {
        return fs_wgsl.to_string();
    }
    let particle_fs = fs_wgsl.contains("in_attr5_1") || fs_wgsl.contains("in_attr6_1");
    if !particle_fs {
        return fs_wgsl.to_string();
    }
    let vs_forwards_attr2 = vs_wgsl.contains("in_attr2_1")
        || vs_wgsl.contains("out_attr2_")
        || vs_wgsl.contains("@location(2) in_attr2_");
    if !vs_forwards_attr2 {
        return fs_wgsl.to_string();
    }
    let mut result = inject_fragment_location_input(fs_wgsl, 2, "in_attr2_", "vec4<f32>", "in_attr2_1");
    // CPU always uploads attr5 (flipbook tile origin) alongside attr2 quad corners.
    if result.contains("in_attr2_1") && !result.contains("in_attr5_1") {
        result = inject_fragment_location_input(&result, 5, "in_attr5_", "vec4<f32>", "in_attr5_1");
    }
    result
}

/// True when native FS WGSL references TextureAnim3–5 cbuf / attr inputs.
pub fn native_fs_extra_tex_slots_needed(wgsl: &str) -> [bool; 3] {
    let uses_cbuf9_100 = wgsl.contains("_m0_[100]");
    let uses_cbuf9_101 = wgsl.contains("_m0_[101]");
    let uses_cbuf10_11 = wgsl.contains("cbuf_10_1_") && wgsl.contains("_m0_[11]");
    let uses_cbuf10_12 = wgsl.contains("cbuf_10_1_") && wgsl.contains("_m0_[12]");
    let uses_attr11 = wgsl.contains("in_attr11_1");
    let uses_attr12 = wgsl.contains("in_attr12_1");
    [
        uses_attr11 || uses_cbuf9_100 || uses_cbuf10_11,
        uses_attr11 || uses_cbuf9_100 || uses_cbuf10_11,
        uses_attr11 || uses_attr12 || uses_cbuf9_101 || uses_cbuf10_12,
    ]
}

/// True when per-draw combiner blend coeffs are uploaded to `@group(2)` binding 6.
pub fn native_fs_tex_blend_uniform_needed(wgsl: &str) -> bool {
    wgsl.contains("_fx_tex_blend")
        || wgsl.contains("cbuf_16_1_")
        || native_fs_extra_tex_slots_needed(wgsl).iter().any(|&b| b)
}

/// True when the BNSH pipeline should keep the decoded NVN fragment colour chain instead of
/// the simplified `patch_fragment_wgsl` attr1-only path.
pub fn should_use_native_fs_fragment(
    fs_wgsl: &str,
    native_color: crate::shader_registry::NativeColorInput,
) -> bool {
    use crate::shader_registry::NativeColorInput;
    // Hard override: force the CPU vertex-colour (attr1) path regardless of structural
    // detection. Diagnostic lever for the native-FS life-chain (task #22).
    if std::env::var("FX_FORCE_PATCHED_FS").is_ok() {
        return false;
    }
    if crate::fx_env::fx_native_fs_enabled() {
        return true;
    }
    // Structural detection outranks the env kill-switch: colour tables in the decoded FS
    // mean the game computes colour there, and the patched attr1-only path would be wrong.
    if fs_has_native_color_chain(fs_wgsl) {
        return true;
    }
    #[cfg(test)]
    {
        if matches!(
            std::env::var("FX_NATIVE_FS").as_deref(),
            Ok("0") | Ok("false") | Ok("no")
        ) {
            return false;
        }
    }
    matches!(native_color, NativeColorInput::FsChain)
}

/// Detect which vertex attrs carry the particle BIRTH time and LIFETIME for a decoded
/// NVN VS. The life chain sits at the top of `main_1()` (capture-verified, see
/// docs/game-particle-vertex-layout.md):
///
/// ```text
/// gpr = in_attr<B>.w;  pred = (gpr > cbuf_10[2].x);   // birth vs emitter clock
/// gpr = cbuf_10[2].x - birth;                          // age
/// gpr = trunc(in_attr<L>.w);  pred = (age >= gpr);     // lifetime cull
/// ```
///
/// The attr slots vary per shader family (bomb: B=4/L=3; impactflash: B=5/L=4), so the
/// vertex builder must place birth/lifetime per shader. Returns `(birth_attr, life_attr)`.
pub fn detect_life_attr_roles(vs_wgsl: &str) -> Option<(u32, u32)> {
    let gate = vs_wgsl.find("cbuf_10_1_._m0_[2]")?;
    let birth = last_attr_w_read(&vs_wgsl[..gate])?;
    let after = &vs_wgsl[gate..];
    let trunc_pos = after.find("trunc(")?;
    let life = last_attr_w_read(&after[..trunc_pos])?;
    Some((birth, life))
}

/// Last `let _eK = in_attrN_1;` in `s` whose captured value's `.w` is read on a
/// following line.
fn last_attr_w_read(s: &str) -> Option<u32> {
    let mut result = None;
    let mut prev: Option<(u32, String)> = None;
    for line in s.lines() {
        let t = line.trim();
        if let Some((n, e)) = prev.take() {
            if t.contains(&format!("{e}.w")) {
                result = Some(n);
            }
        }
        if let Some(rest) = t.strip_prefix("let ") {
            if let Some((e_id, rhs)) = rest.split_once(" = ") {
                if let Some(attr) = rhs
                    .strip_prefix("in_attr")
                    .and_then(|r| r.strip_suffix("_1;"))
                    .and_then(|n| n.parse::<u32>().ok())
                {
                    prev = Some((attr, e_id.trim().to_string()));
                }
            }
        }
    }
    result
}

/// True when decoded VS WGSL runs an NVN colour Hermite / combiner chain in `main_1()`.
pub fn vs_has_native_color_chain(wgsl: &str) -> bool {
    wgsl.contains("main_1();")
        && (wgsl.contains("cbuf_9_1_._m0_[60]")
            || wgsl.contains("cbuf_9_1_._m0_[61]")
            || wgsl.contains("cbuf_9_1_._m0_[62]")
            || wgsl.contains("cbuf_8_1_._m0_[6]")
            || wgsl.contains("cbuf_8_1_._m0_[7]")
            || (wgsl.contains("cbuf_16_1_") && wgsl.contains("out_attr0_")))
}

/// True when decoded VS WGSL writes atlas UV varyings in `main_1()` (not CPU passthrough only).
fn vs_has_native_uv_chain(wgsl: &str) -> bool {
    wgsl.contains("main_1();")
        && wgsl.contains("out_attr2_")
        && (wgsl.contains("cbuf_8_1_") || wgsl.contains("cbuf_9_1_") || wgsl.contains("cbuf_10_1_"))
        && wgsl.contains("out_attr2_.")
}

/// Skip CPU attr0/1/2 passthrough when the decoded VS already computes colour/UV varyings.
fn preserve_native_vs_varyings(vs: &str) -> bool {
    if !crate::fx_env::fx_native_vs_pos_enabled() {
        return false;
    }
    vs_has_native_color_chain(vs) || should_preserve_native_uv_varyings(vs)
}

/// True when decoded FS WGSL runs an NVN colour combiner / Hermite table chain in `main_1()`.
pub fn fs_has_native_color_chain(wgsl: &str) -> bool {
    wgsl.contains("cbuf_9_1_._m0_[60]")
        || wgsl.contains("cbuf_9_1_._m0_[68]")
        || wgsl.contains("cbuf_8_1_._m0_[6]")
        || wgsl.contains("cbuf_8_1_._m0_[7]")
        || (wgsl.contains("cbuf_16_1_")
            && (wgsl.contains("out_attr0_") || wgsl.contains("frag_color0_")))
        || (wgsl.contains("cbuf_16_1_._m0_[0]")
            && wgsl.contains("cbuf_16_1_._m0_[2]")
            && wgsl.contains("cbuf_16_1_._m0_[4]"))
}

/// Infer native colour input from wired/clamped fragment WGSL alone.
pub fn infer_native_color_from_fs_wgsl(wgsl: &str) -> crate::shader_registry::NativeColorInput {
    use crate::shader_registry::NativeColorInput;
    if fs_has_native_color_chain(wgsl) {
        return NativeColorInput::FsChain;
    }
    if wgsl.contains("in_attr1_1")
        || wgsl.contains("@location(1) in_attr1_")
        || wgsl.contains("in_attr1_:")
    {
        return NativeColorInput::VertexAttr;
    }
    NativeColorInput::FsChain
}

fn resolve_native_color_in_override(
    wgsl: &str,
    hint: crate::shader_registry::NativeColorInput,
) -> Option<&'static str> {
    use crate::shader_registry::NativeColorInput;
    let has_attr1 = wgsl.contains("in_attr1_1")
        || wgsl.contains("@location(1) in_attr1_")
        || wgsl.contains("in_attr1_:");
    let effective = match hint {
        NativeColorInput::Auto => infer_native_color_from_fs_wgsl(wgsl),
        other => other,
    };
    match effective {
        NativeColorInput::VertexAttr => {
            if has_attr1 {
                Some("in_attr1_1")
            } else {
                None
            }
        }
        NativeColorInput::FsChain | NativeColorInput::Auto => None,
    }
}

/// Ensure the fragment shader declares attr11 when the VS or cbuf chain needs tex3–5 UV offsets.
/// When the VS forwards attr11 (CPU-sim path) but decoded FS omits cbuf_9[101]/cbuf_10[12],
/// tex5 is force-injected alongside tex3/4 (same rule as slots 0–1).
pub fn wire_extra_tex_fragment_input(fs_wgsl: &str, vs_wgsl: &str) -> String {
    let mut needed = native_fs_extra_tex_slots_needed(fs_wgsl);
    let vs_forwards_attr11 = vs_wgsl.contains("in_attr11_1")
        || vs_wgsl.contains("out_attr11_")
        || vs_wgsl.contains("@location(11) in_attr11_");
    let vs_forwards_attr12 = vs_wgsl.contains("in_attr12_1")
        || vs_wgsl.contains("out_attr12_")
        || vs_wgsl.contains("@location(12) in_attr12_");
    if vs_forwards_attr11 {
        needed[0] = true;
        needed[1] = true;
        needed[2] = true;
    }
    if vs_forwards_attr12 {
        needed[2] = true;
    }
    if !needed.iter().any(|&b| b) {
        return fs_wgsl.to_string();
    }
    let mut result = fs_wgsl.to_string();
    if !result.contains("in_attr11_1")
        && (vs_wgsl.contains("in_attr11_1") || needed[0] || needed[1])
    {
        result = inject_fragment_location_input(&result, 11, "in_attr11_", "vec4<f32>", "in_attr11_1");
    }
    if !result.contains("in_attr12_1") && (vs_forwards_attr12 || needed[2]) {
        result = inject_fragment_location_input(&result, 12, "in_attr12_", "vec4<f32>", "in_attr12_1");
    }
    result
}

/// Atlas UV for slot-0 flipbook: unit quad corner (attr2) × tile scale + per-particle origin (attr5).
/// attr6 carries billboard half-extents for position — never use it as a texture corner.
pub(crate) fn primary_atlas_uv_expr(wgsl: &str) -> String {
    let corner = if wgsl.contains("in_attr2_1") {
        "in_attr2_1.xy".to_string()
    } else {
        "vec2<f32>(0.5, 0.5)".to_string()
    };
    let corner = if wgsl.contains("in_attr10_1") {
        format!(
            "((({corner}) - vec2<f32>(0.5, 0.5)) * mat2x2<f32>(cos(in_attr10_1.w), -sin(in_attr10_1.w), sin(in_attr10_1.w), cos(in_attr10_1.w)) + vec2<f32>(0.5, 0.5))"
        )
    } else {
        corner
    };
    let scaled = if wgsl.contains("cbuf_9_1_") && wgsl.contains("in_attr2_1") {
        format!("({corner} * max(abs(cbuf_9_1_._m0_[127].xy), vec2<f32>(0.001, 0.001)))")
    } else if wgsl.contains("cbuf_10_1_") && wgsl.contains("_m0_[4]") && wgsl.contains("_m0_[5]") {
        format!(
            "({corner} * vec2<f32>(cbuf_10_1_._m0_[4].x, cbuf_10_1_._m0_[5].y))"
        )
    } else {
        corner
    };
    if wgsl.contains("in_attr5_1") {
        format!("({scaled} + in_attr5_1.xy)")
    } else {
        scaled
    }
}

fn extra_tex_uv_expr(wgsl: &str, base_uv: &str, slot: u32) -> String {
    let atlas_base = if slot == 0 {
        primary_atlas_uv_expr(wgsl)
    } else {
        base_uv.to_string()
    };
    let mut expr = atlas_base;
    match slot {
        0 => {
            if wgsl.contains("in_attr11_1") {
                expr = format!("({expr} + in_attr11_1.xy)");
            }
            if wgsl.contains("_m0_[100]") {
                expr = format!("({expr} + cbuf_9_1_._m0_[100].xy)");
            }
        }
        1 => {
            if wgsl.contains("in_attr11_1") {
                expr = format!("({expr} + in_attr11_1.zw)");
            }
            if wgsl.contains("_m0_[100]") {
                expr = format!("({expr} + cbuf_9_1_._m0_[100].zw)");
            }
        }
        2 => {
            if wgsl.contains("in_attr12_1") {
                expr = format!("({expr} + in_attr12_1.xy)");
            } else if wgsl.contains("cbuf_10_1_") && wgsl.contains("_m0_[12]") {
                expr = format!("({expr} + cbuf_10_1_._m0_[12].xy)");
            }
            if wgsl.contains("_m0_[101]") {
                expr = format!("({expr} + cbuf_9_1_._m0_[101].xy)");
            }
        }
        _ => {}
    }
    expr
}

/// Full `@group(1)` emitter texture layout: primary, alpha, indirect (distortion map),
/// per-draw [`FxIndirectParams`] (binding 6, dynamic offset on CPU), slot-2 tertiary.
/// Remaining after native FS pass: `is_distortion_by_camera_distance`; soft-particle depth (Agent 1).
fn emitter_tex_group1_decls() -> &'static str {
    "@group(1) @binding(0) var color_tex: texture_2d<f32>;\n\
     @group(1) @binding(1) var color_sampler: sampler;\n\
     @group(1) @binding(2) var alpha_tex: texture_2d<f32>;\n\
     @group(1) @binding(3) var alpha_sampler: sampler;\n\
     @group(1) @binding(4) var indirect_tex: texture_2d<f32>;\n\
     @group(1) @binding(5) var indirect_sampler: sampler;\n\
     struct FxIndirectParams {\n\
         is_indirect: u32,\n\
         distortion_strength: f32,\n\
         indirect_scroll_u: f32,\n\
         indirect_scroll_v: f32,\n\
         indirect_scale_u: f32,\n\
         indirect_scale_v: f32,\n\
         indirect_offset_u: f32,\n\
         indirect_offset_v: f32,\n\
         distortion_by_cam_dist: u32,\n\
         enable_cam_dist_near: u32,\n\
         enable_cam_dist_far: u32,\n\
         _pad0: u32,\n\
         cam_dist_near: f32,\n\
         cam_dist_far: f32,\n\
         _pad1: f32,\n\
         _pad2: f32,\n\
         cam_pos: vec3<f32>,\n\
         _pad3: f32,\n\
     }\n\
     @group(1) @binding(6) var<uniform> _fx_indirect: FxIndirectParams;\n\
     @group(1) @binding(7) var slot2_tex: texture_2d<f32>;\n\
     @group(1) @binding(8) var slot2_sampler: sampler;\n"
}

fn fx_distortion_uv_helpers(wgsl: &str) -> String {
    let world_pos = particle_alpha_mod_world_pos_expr(wgsl);
    let frag_depth_fallback = if wgsl.contains("_fx_frag_pos") {
        "if (dist < 1e-5) {\n\
        dist = _fx_frag_pos.z;\n\
    }\n"
    } else {
        ""
    };
    format!(
        "fn _fx_distort_cam_scale(dist: f32) -> f32 {{\n\
    if (_fx_indirect.distortion_by_cam_dist == 0u) {{\n\
        return 1.0;\n\
    }}\n\
    let near_d = max(_fx_indirect.cam_dist_near, 1e-5);\n\
    let far_d = max(_fx_indirect.cam_dist_far, near_d);\n\
    var scale = dist / far_d;\n\
    if (_fx_indirect.enable_cam_dist_near != 0u) {{\n\
        scale *= saturate((dist - near_d) / near_d);\n\
    }}\n\
    if (_fx_indirect.enable_cam_dist_far != 0u) {{\n\
        scale *= saturate((far_d - dist) / far_d);\n\
    }}\n\
    return scale;\n\
}}\n\
fn _fx_distort_uv(base_uv: vec2<f32>) -> vec2<f32> {{\n\
    if (_fx_indirect.is_indirect == 0u) {{\n\
        return base_uv;\n\
    }}\n\
    let ind_uv = base_uv * vec2<f32>(_fx_indirect.indirect_scale_u, _fx_indirect.indirect_scale_v)\n\
        + vec2<f32>(_fx_indirect.indirect_offset_u, _fx_indirect.indirect_offset_v);\n\
    let ind = textureSample(indirect_tex, indirect_sampler, ind_uv);\n\
    var offset = (ind.rg * 2.0 - vec2<f32>(1.0, 1.0)) * _fx_indirect.distortion_strength;\n\
    if (_fx_indirect.distortion_by_cam_dist != 0u) {{\n\
        let world_pos = {world_pos};\n\
        var dist = length(_fx_indirect.cam_pos - world_pos);\n\
        {frag_depth_fallback}\
        offset *= _fx_distort_cam_scale(dist);\n\
    }}\n\
    return base_uv + offset;\n\
}}\n",
        world_pos = world_pos,
        frag_depth_fallback = frag_depth_fallback,
    )
}

/// True when fragment WGSL includes camera-distance-scaled indirect distortion.
pub fn native_fs_camera_distortion_needed(wgsl: &str) -> bool {
    wgsl.contains("distortion_by_cam_dist")
}

fn extra_tex_group2_decls() -> &'static str {
    "@group(2) @binding(0) var extra_tex3: texture_2d<f32>;\n\
     @group(2) @binding(1) var extra_sampler3: sampler;\n\
     @group(2) @binding(2) var extra_tex4: texture_2d<f32>;\n\
     @group(2) @binding(3) var extra_sampler4: sampler;\n\
     @group(2) @binding(4) var extra_tex5: texture_2d<f32>;\n\
     @group(2) @binding(5) var extra_sampler5: sampler;\n"
}

fn extra_tex_blend_uniform_decls() -> &'static str {
    "struct FxTexBlendCoeffs {\n\
    primary: vec4<f32>,\n\
    tex1: vec4<f32>,\n\
    tex2: vec4<f32>,\n\
    tex3: vec4<f32>,\n\
    tex4: vec4<f32>,\n\
    tex5: vec4<f32>,\n\
}\n\
@group(2) @binding(6) var<uniform> _fx_tex_blend: FxTexBlendCoeffs;\n"
}

/// Per-draw fresnel / near-far distance alpha (`@group(2)` binding 7).
fn particle_alpha_mod_group2_decls() -> &'static str {
    "struct FxParticleAlphaMods {\n\
    flags: u32,\n\
    _pad0: u32,\n\
    _pad1: u32,\n\
    _pad2: u32,\n\
    fresnel_p1: f32,\n\
    fresnel_p2: f32,\n\
    near_dist_p1: f32,\n\
    near_dist_p2: f32,\n\
    far_dist_p1: f32,\n\
    far_dist_p2: f32,\n\
    cam_pos: vec3<f32>,\n\
    _pad3: f32,\n\
}\n\
@group(2) @binding(7) var<uniform> _fx_particle_alpha: FxParticleAlphaMods;\n"
}

/// True when fragment WGSL includes fresnel / distance alpha modifiers (`@group(2)` binding 7).
pub fn native_fs_particle_alpha_uniform_needed(wgsl: &str) -> bool {
    wgsl.contains("_fx_particle_alpha")
}

fn particle_alpha_mod_world_pos_expr(wgsl: &str) -> &'static str {
    if wgsl.contains("in_attr2_1")
        && wgsl.contains("in_attr4_1")
        && wgsl.contains("cbuf_9_1_._m0_[46]")
    {
        "{\n\
    let _fx_bb_right = normalize(cbuf_9_1_._m0_[120].xyz);\n\
    let _fx_bb_up = normalize(vec3<f32>(cbuf_9_1_._m0_[121].y, cbuf_9_1_._m0_[121].z, cbuf_9_1_._m0_[121].w));\n\
    let _fx_bb_corner = in_attr2_1.xy - vec2<f32>(0.5, 0.5);\n\
    let _fx_bb_sz = in_attr4_1.x;\n\
    in_attr0_1.xyz + _fx_bb_corner.x * _fx_bb_sz * 2.0 * _fx_bb_right + _fx_bb_corner.y * _fx_bb_sz * 2.0 * _fx_bb_up\n\
}"
    } else if wgsl.contains("in_attr0_1") {
        "in_attr0_1.xyz"
    } else {
        "vec3<f32>(0.0, 0.0, 0.0)"
    }
}

fn particle_alpha_mod_helpers(wgsl: &str) -> String {
    let world_pos = particle_alpha_mod_world_pos_expr(wgsl);
    format!(
        "fn _fx_billboard_normal() -> vec3<f32> {{\n\
    if ({has_cbuf_basis}) {{\n\
        let _fx_right = normalize(cbuf_9_1_._m0_[120].xyz);\n\
        let _fx_up = normalize(vec3<f32>(cbuf_9_1_._m0_[121].y, cbuf_9_1_._m0_[121].z, cbuf_9_1_._m0_[121].w));\n\
        return normalize(cross(_fx_right, _fx_up));\n\
    }}\n\
    return vec3<f32>(0.0, 0.0, 1.0);\n\
}}\n\
fn _fx_apply_particle_alpha_modifiers(col: vec4<f32>) -> vec4<f32> {{\n\
    let flags = _fx_particle_alpha.flags;\n\
    if (flags == 0u) {{\n\
        return col;\n\
    }}\n\
    var a = col.a;\n\
    let straight_rgb = select(col.rgb / max(a, 1e-5), col.rgb, a < 1e-5);\n\
    let world_pos = {world_pos};\n\
    let to_cam = _fx_particle_alpha.cam_pos - world_pos;\n\
    let dist = length(to_cam);\n\
    let view_dir = select(normalize(to_cam), vec3<f32>(0.0, 0.0, 1.0), dist < 1e-5);\n\
    if ((flags & 1u) != 0u) {{\n\
        let n_dot_v = saturate(dot(_fx_billboard_normal(), view_dir));\n\
        let fresnel = pow(1.0 - n_dot_v, max(_fx_particle_alpha.fresnel_p1, 0.001));\n\
        a *= fresnel * max(_fx_particle_alpha.fresnel_p2, 0.0);\n\
    }}\n\
    if ((flags & 2u) != 0u) {{\n\
        let near_range = max(_fx_particle_alpha.near_dist_p2, 1e-5);\n\
        a *= saturate((dist - _fx_particle_alpha.near_dist_p1) / near_range);\n\
    }}\n\
    if ((flags & 4u) != 0u) {{\n\
        let far_range = max(_fx_particle_alpha.far_dist_p2, 1e-5);\n\
        a *= saturate((_fx_particle_alpha.far_dist_p1 + far_range - dist) / far_range);\n\
    }}\n\
    return vec4(straight_rgb * a, a);\n\
}}\n",
        has_cbuf_basis = if wgsl.contains("cbuf_9_1_._m0_[46]") {
            "true"
        } else {
            "false"
        },
        world_pos = world_pos,
    )
}

/// UV for `@group(1)` alpha_tex (physical slot 1) — shares primary atlas corner math.
fn group1_tex1_uv_expr(wgsl: &str) -> String {
    primary_atlas_uv_expr(wgsl)
}

/// UV for `@group(1)` slot2_tex (physical slot 2) with optional cbuf scroll/offset.
fn group1_tex2_uv_expr(wgsl: &str) -> String {
    let mut expr = primary_atlas_uv_expr(wgsl);
    if wgsl.contains("cbuf_10_1_") && wgsl.contains("_m0_[9]") {
        expr = format!("({expr} + cbuf_10_1_._m0_[9].zw)");
    }
    if wgsl.contains("cbuf_9_1_") && wgsl.contains("_m0_[92]") {
        expr = format!("({expr} + cbuf_9_1_._m0_[92].zw)");
    }
    expr
}

fn group1_combiner_tex_sample_prelude(wgsl: &str, indent: &str, _base_uv: &str) -> String {
    let tex1_uv = group1_tex1_uv_expr(wgsl);
    let tex2_uv = group1_tex2_uv_expr(wgsl);
    format!(
        "{indent}let _fx_uv_alpha = _fx_distort_uv({tex1_uv});\n\
         {indent}let _fx_uv_slot2 = _fx_distort_uv({tex2_uv});\n\
         {indent}let _fx_ts_alpha = textureSample(alpha_tex, alpha_sampler, _fx_uv_alpha);\n\
         {indent}let _fx_ts_slot2 = textureSample(slot2_tex, slot2_sampler, _fx_uv_slot2);\n"
    )
}

fn blend_group1_combiner_tex(indent: &str, tex_var: &str, phys_slot: u32, wgsl: &str) -> String {
    let (helper, cbuf_slot, coeff) = match phys_slot {
        1 => ("_fx_cbuf16_blend_ch12", 2, "_fx_tex_blend.tex1"),
        _ => ("_fx_cbuf16_blend_ch3", 3, "_fx_tex_blend.tex2"),
    };
    // Bank 16 now carries GAME constants (e.g. [2] = (1,0,1,-1)), which are NOT the
    // editor blend selectors these helpers expect — the (1,0,1,-1) value tripped the
    // subtract path and blacked whole draws. Selectors come only from the
    // FxTexBlendCoeffs uniform; without it, modulate.
    let _ = cbuf_slot;
    if wgsl.contains("_fx_tex_blend") {
        format!(
            "{indent}_fx_native_col = {helper}(_fx_native_col, {tex_var}, {coeff});\n"
        )
    } else {
        format!(
            "{indent}_fx_native_col = _fx_modulate_particle_tex(_fx_native_col, {tex_var});\n"
        )
    }
}

fn modulate_native_col_with_group1_combiner_tex(indent: &str, wgsl: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{indent}if (_fx_indirect.is_indirect == 0u) {{\n\
         {indent}    {}\n\
         {indent}}}\n",
        blend_group1_combiner_tex(indent, "_fx_ts_alpha", 1, wgsl).trim_end()
    ));
    out.push_str(&blend_group1_combiner_tex(indent, "_fx_ts_slot2", 2, wgsl));
    out
}

fn extra_tex_sample_prelude(wgsl: &str, indent: &str, base_uv: &str, slots: [bool; 3]) -> String {
    let mut out = String::new();
    if slots[0] {
        let uv = extra_tex_uv_expr(wgsl, base_uv, 0);
        out.push_str(&format!(
            "{indent}let _fx_uv3 = {uv};\n\
             {indent}let _fx_ts3 = textureSample(extra_tex3, extra_sampler3, _fx_uv3);\n"
        ));
    }
    if slots[1] {
        let uv = extra_tex_uv_expr(wgsl, base_uv, 1);
        out.push_str(&format!(
            "{indent}let _fx_uv4 = {uv};\n\
             {indent}let _fx_ts4 = textureSample(extra_tex4, extra_sampler4, _fx_uv4);\n"
        ));
    }
    if slots[2] {
        let uv = extra_tex_uv_expr(wgsl, base_uv, 2);
        out.push_str(&format!(
            "{indent}let _fx_uv5 = {uv};\n\
             {indent}let _fx_ts5 = textureSample(extra_tex5, extra_sampler5, _fx_uv5);\n"
        ));
    }
    out
}

fn fx_modulate_particle_tex_helpers() -> &'static str {
    "fn _fx_modulate_particle_tex(base: vec4<f32>, tex: vec4<f32>) -> vec4<f32> {\n\
    var straight_rgb = base.rgb * tex.rgb;\n\
    var a = base.a * tex.a;\n\
    if (dot(base.rgb, vec3<f32>(1.0)) < 0.001) {\n\
        straight_rgb = tex.rgb;\n\
    } else if (tex.r > 0.98 && tex.g > 0.98 && tex.b > 0.98) {\n\
        straight_rgb = base.rgb;\n\
    }\n\
    return vec4(straight_rgb * a, a);\n\
}\n"
}

fn extra_tex_cbuf16_blend_helpers(wgsl: &str, blend_uniform: bool) -> Option<&'static str> {
    if !blend_uniform && !wgsl.contains("cbuf_16_1_") {
        return None;
    }
    Some(
        "fn _fx_cbuf16_blend_ch12(base: vec4<f32>, tex: vec4<f32>, c: vec4<f32>) -> vec4<f32> {\n\
    if (c.y > 0.5) {\n\
        return vec4<f32>(clamp(base.rgb + tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a + tex.a, 0.0, 1.0));\n\
    } else if (c.w < -0.5) {\n\
        return vec4<f32>(clamp(base.rgb - tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a - tex.a, 0.0, 1.0));\n\
    }\n\
    return _fx_modulate_particle_tex(base, tex);\n\
}\n\
fn _fx_cbuf16_blend_ch3(base: vec4<f32>, tex: vec4<f32>, c: vec4<f32>) -> vec4<f32> {\n\
    if (c.z > 0.5) {\n\
        return vec4<f32>(clamp(base.rgb + tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a + tex.a, 0.0, 1.0));\n\
    } else if (c.z < -0.5) {\n\
        return vec4<f32>(clamp(base.rgb - tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a - tex.a, 0.0, 1.0));\n\
    }\n\
    return _fx_modulate_particle_tex(base, tex);\n\
}\n",
    )
}

fn blend_extra_tex_into_col(indent: &str, tex_var: &str, extra_idx: usize, wgsl: &str) -> String {
    let helper = if extra_idx == 2 {
        "_fx_cbuf16_blend_ch3"
    } else {
        "_fx_cbuf16_blend_ch12"
    };
    let coeff = match extra_idx {
        0 => "_fx_tex_blend.tex3",
        1 => "_fx_tex_blend.tex4",
        _ => "_fx_tex_blend.tex5",
    };
    if wgsl.contains("_fx_tex_blend") {
        format!(
            "{indent}_fx_native_col = {helper}(_fx_native_col, {tex_var}, {coeff});\n"
        )
    } else {
        format!(
            "{indent}_fx_native_col = _fx_modulate_particle_tex(_fx_native_col, {tex_var});\n"
        )
    }
}

fn blend_primary_color_tex(indent: &str, wgsl: &str) -> String {
    if wgsl.contains("_fx_tex_blend") {
        format!(
            "{indent}let _fx_native_col_base = _fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary);\n"
        )
    } else {
        format!(
            "{indent}let _fx_native_col_base = _fx_modulate_particle_tex(_fx_native_in, _fx_ts);\n"
        )
    }
}

fn modulate_native_col_with_extra_tex(indent: &str, slots: [bool; 3], wgsl: &str) -> String {
    let mut out = String::new();
    if slots[0] {
        out.push_str(&blend_extra_tex_into_col(indent, "_fx_ts3", 0, wgsl));
    }
    if slots[1] {
        out.push_str(&blend_extra_tex_into_col(indent, "_fx_ts4", 1, wgsl));
    }
    if slots[2] {
        out.push_str(&blend_extra_tex_into_col(indent, "_fx_ts5", 2, wgsl));
    }
    out
}

/// Prefer the `in_attrN_1` private copy over the `@location` param when wiring passthroughs.
fn vertex_input_source_expr(vs: &str, param_name: &str) -> String {
    let private = format!("{param_name}1");
    if vs.contains(&format!("var<private> {private}:")) {
        private
    } else {
        param_name.to_string()
    }
}

fn wire_one_vertex_passthrough(vs: &mut String, location: u32, in_name: &str, out_name: &str) {
    if !vs.contains(in_name) {
        return;
    }
    let ty = "vec4<f32>";
    if !vs.contains(&format!("@location({location}) {out_name}")) {
        let missing = [(location, out_name.to_string(), ty.to_string())];
        *vs = rebuild_vertex_output_struct(vs, &missing);
        append_private_var(vs, out_name, ty);
        if !vertex_output_is_wired(vs, out_name) {
            add_assignment_before_return(vs, out_name, in_name);
            if !return_vertex_output_contains(vs, out_name) {
                *vs = extend_vertex_return(vs, &[out_name.to_string()]);
            }
        }
    } else if !vertex_output_is_wired(vs, out_name) {
        add_assignment_before_return(vs, out_name, in_name);
    }
}

/// Declare a CPU-simulated vertex input + passthrough varying when spirv-cross omitted it.
pub(crate) fn wire_native_attr_passthrough(
    vs: &mut String,
    location: u32,
    param_name: &str,
    private_in: &str,
    out_name: &str,
) {
    if !vs.contains(private_in) {
        append_private_var(vs, private_in, "vec4<f32>");
    }
    ensure_vertex_entry_locations(vs, &[(location, param_name, private_in)]);
    wire_one_vertex_passthrough(vs, location, private_in, out_name);
}

fn insert_after_main1_varying_forwards(vs: &str) -> String {
    let marker = "main_1();";
    let Some(pos) = vs.find(marker) else {
        return vs.to_string();
    };
    let insert_at = pos + marker.len();
    let mut block = String::from("\n    {");
    if vs.contains("out_attr10_") && vs.contains("in_attr10_1") {
        block.push_str("\n        out_attr10_ = in_attr10_1;");
    }
    if vs.contains("out_attr11_") && vs.contains("in_attr11_1") {
        block.push_str("\n        out_attr11_ = in_attr11_1;");
    }
    if vs.contains("out_attr12_") && vs.contains("in_attr12_1") {
        block.push_str("\n        out_attr12_ = in_attr12_1;");
    }
    if block.len() <= 6 {
        return vs.to_string();
    }
    block.push_str("\n    }\n");
    let mut result = vs.to_string();
    result.insert_str(insert_at, &block);
    ensure_vertex_entry_locations(&mut result, &[
        (10, "in_attr10_", "in_attr10_1"),
        (11, "in_attr11_", "in_attr11_1"),
        (12, "in_attr12_", "in_attr12_1"),
    ]);
    result
}

fn ensure_vertex_entry_locations(vs: &mut String, locations: &[(u32, &str, &str)]) {
    for &(location, param_name, private_name) in locations {
        if !vs.contains(private_name) {
            continue;
        }
        if vs.contains(&format!("@location({location}) {param_name}:")) {
            continue;
        }
        append_private_var(vs, private_name, "vec4<f32>");
        let entry = "@vertex";
        let Some(frag_pos) = vs.find(entry) else {
            continue;
        };
        let fn_pos = vs[frag_pos..].find("fn ").map(|i| frag_pos + i);
        let Some(fn_start) = fn_pos else {
            continue;
        };
        let open = vs[fn_start..].find('(').map(|i| fn_start + i);
        let Some(open_paren) = open else {
            continue;
        };
        let param = format!("@location({location}) {param_name}: vec4<f32>, ");
        vs.insert_str(open_paren + 1, &param);
        let assign = format!("\n    {private_name} = {param_name};");
        if let Some(body) = vs[fn_start..].find('{') {
            let insert = fn_start + body + 1;
            if !vs[fn_start..insert].contains(&format!("{private_name} = {param_name}")) {
                vs.insert_str(insert, &assign);
            }
        }
    }
}

fn inject_fragment_location_input(
    fs: &str,
    location: u32,
    param_name: &str,
    ty: &str,
    private_name: &str,
) -> String {
    if fs.contains(private_name) {
        return fs.to_string();
    }
    let mut result = fs.to_string();
    append_private_var(&mut result, private_name, ty);
    let entry = "@fragment";
    let Some(frag_pos) = result.find(entry) else {
        return result;
    };
    let fn_pos = result[frag_pos..].find("fn ").map(|i| frag_pos + i);
    let Some(fn_start) = fn_pos else {
        return result;
    };
    let open = result[fn_start..].find('(').map(|i| fn_start + i);
    let Some(open_paren) = open else {
        return result;
    };
    let param = format!("@location({location}) {param_name}: {ty}, ");
    result.insert_str(open_paren + 1, &param);
    let assign = format!("\n    {private_name} = {param_name};");
    if let Some(body) = result[fn_start..].find('{') {
        let insert = fn_start + body + 1;
        result.insert_str(insert, &assign);
    }
    result
}

/// Convert pre‑patched SPIR-V bytes to WGSL via spirv‑cross → Vulkan GLSL → naga.
///
/// `spirv_bytes` should already have NVN execution‑mode patches and binding
/// remapping applied.
/// Deterministic SPIR-V→WGSL conversion with on-disk memoization.
///
/// naga's GLSL→WGSL stage is nondeterministic across process launches (std `HashMap` seed),
/// which makes the generated WGSL — and therefore rendered pixels — vary run-to-run. Since
/// `spirv_bytes` is deterministic (the C++ decoder + our SPIR-V patches are), we memoize the
/// result keyed by a content hash so every process produces identical WGSL. Set
/// `HITBOX_WGSL_CACHE=0` to bypass (e.g. when debugging the decode).
pub fn spirv_to_wgsl(
    spirv_bytes: &[u8],
    stage: naga::ShaderStage,
    shader_name: &str,
) -> Result<(String, Vec<DescriptorInfo>)> {
    let cache_key = wgsl_cache_key(spirv_bytes, stage, shader_name);
    if let Some(hit) = wgsl_cache_get(&cache_key) {
        return Ok(hit);
    }
    let result = spirv_to_wgsl_uncached(spirv_bytes, stage, shader_name)?;
    wgsl_cache_put(&cache_key, &result);
    Ok(result)
}

fn wgsl_cache_enabled() -> bool {
    !matches!(std::env::var("HITBOX_SHADER_CACHE").as_deref(), Ok("0"))
}

fn wgsl_cache_key(spirv_bytes: &[u8], stage: naga::ShaderStage, shader_name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(spirv_bytes);
    h.update(format!("{stage:?}").as_bytes());
    h.update(shader_name.as_bytes());
    format!("{:x}", h.finalize())
}

fn wgsl_cache_path(key: &str) -> std::path::PathBuf {
    crate::scratch_dirs::wgsl_cache_root().join(format!("{key}.wgslc"))
}

fn wgsl_cache_get(key: &str) -> Option<(String, Vec<DescriptorInfo>)> {
    if !wgsl_cache_enabled() {
        return None;
    }
    let data = std::fs::read(wgsl_cache_path(key)).ok()?;
    bincode::deserialize::<(String, Vec<DescriptorInfo>)>(&data).ok()
}

fn wgsl_cache_put(key: &str, value: &(String, Vec<DescriptorInfo>)) {
    if !wgsl_cache_enabled() {
        return;
    }
    let dir = crate::scratch_dirs::wgsl_cache_root();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(bytes) = bincode::serialize(value) {
        let _ = std::fs::write(wgsl_cache_path(key), bytes);
    }
}

/// `spirv_bytes` should already have NVN execution-mode patches and binding remapping applied.
fn spirv_to_wgsl_uncached(
    spirv_bytes: &[u8],
    stage: naga::ShaderStage,
    shader_name: &str,
) -> Result<(String, Vec<DescriptorInfo>)> {
    let temp_dir = crate::scratch_dirs::app_scratch_dir("spirv-")
        .map_err(|e| anyhow!("Failed to create temp directory: {}", e))?;
    let temp_dir_path = temp_dir.path().to_path_buf();

    let spirv_path = temp_dir_path.join("shader.spv");
    let glsl_path = temp_dir_path.join("shader.glsl");
    std::fs::write(&spirv_path, spirv_bytes)?;

    let cli: String = option_env!("SPIRV_CROSS_CLI")
        .filter(|p| std::path::Path::new(p).exists())
        .unwrap_or("spirv-cross")
        .to_owned();

    eprintln!("[SPIRV→WGSL] {}: spirv-cross {} --vulkan-semantics",
        shader_name, spirv_path.display());

    let output = Command::new(&cli)
        .arg("--vulkan-semantics")
        .arg(&spirv_path)
        .arg("--output").arg(&glsl_path)
        .output()
        .map_err(|e| anyhow!("spirv-cross execution failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("spirv-cross failed: {}", stderr));
    }

    let glsl_source = std::fs::read_to_string(&glsl_path)?;
    eprintln!("[SPIRV→WGSL] spirv-cross produced {} lines GLSL ({})",
        glsl_source.lines().count(), shader_name);

    // temp_dir is dropped here, automatically cleaning up

    // GLSL→WGSL compatibility fixes:
    // NVN fragment shaders may emit gl_Position which should be gl_FragCoord in Vulkan.
    // All other NVN→Vulkan patches are now handled at the SPIR-V level
    // (nvn_to_vulkan_patch in spirv_patch.rs).

    // Safety net: if spirv-cross emitted gl_BaseVertexARB/gl_BaseInstanceARB
    // from BuiltIn(24)/(25), strip declarations and replace usages with 0
    // (we always draw with base_vertex=0 / base_instance=0).
    let glsl_source: String = glsl_source
        .lines()
        .filter(|line| {
            // Filter out declaration lines for these built-ins
            !(line.contains("gl_BaseVertexARB") && (line.contains("in ") || line.contains("int ") || line.contains("require")))
            && !(line.contains("gl_BaseInstanceARB") && (line.contains("in ") || line.contains("int ") || line.contains("require")))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("gl_BaseVertexARB", "0 /* base_vertex */")
        .replace("gl_BaseInstanceARB", "0 /* base_instance */");

    // Safety net: intBitsToFloat takes int but naga may resolve built-in arguments
    // (e.g. gl_VertexIndex → uint) differently.  Both intBitsToFloat and
    // uintBitsToFloat do the same bitcast-to-float operation, so redirect
    // intBitsToFloat → uintBitsToFloat(uint(x)) for scalar args.
    // This is a #define so the preprocessor rewrites calls before the parser.
    let first_nl = glsl_source.find('\n').map(|p| p + 1).unwrap_or(0);
    let glsl_source = format!(
        "{}#define intBitsToFloat(x) uintBitsToFloat(uint(x))\n{}",
        &glsl_source[..first_nl],
        &glsl_source[first_nl..]
    );

    // naga's GLSL frontend does not implement textureQueryLod/Levels; stub calls before parse.
    let glsl_source = stub_glsl_texture_query_calls(&glsl_source);

    // Safety net: strip `precise` qualifier (GLSL keyword naga doesn't parse).
    // Appears as `precise float`, `precise out vec4`, etc. from SPIR-V decoration 12.
    // The SPIR-V patch (nvn_strip_problematic_decorations) should prevent this, but
    // handle it at GLSL level as a fallback.  Since `precise` is a GLSL keyword used
    // only as a qualifier, replace `precise ` (word + space) anywhere it appears.
    // The risk of matching `imprecise ` is negligible in GLSL output.
    let glsl_source = glsl_source.replace("precise ", "");
    // spirv-cross may emit separate texture2D + sampler; naga requires combined sampler2D(...).
    let glsl_source = combine_glsl_split_textures(&glsl_source);

    let glsl_clean: String = glsl_source.lines()
        .map(|line| -> String {
            let trimmed = line.trim();
            // gl_PointSize: strip declarations (out float gl_PointSize) and assignments
            if line.contains("gl_PointSize") {
                if line.contains("gl_PointSize =") {
                    format!("// gl_PointSize assignment removed: {}", trimmed)
                } else if line.contains("gl_PointSize") && (line.contains("out ") || line.contains("in ")) {
                    String::new() // remove declaration line
                } else {
                    // reads of gl_PointSize (unlikely but handle)
                    line.replace("gl_PointSize", "1.0")
                }
            } else if stage == naga::ShaderStage::Fragment && line.contains("gl_Position") {
                let gl_pos = line.find("gl_Position").unwrap();
                if let Some(eq_pos) = line.find('=') {
                    if eq_pos > gl_pos {
                        // gl_Position is on LHS: gl_Position = ... or gl_Position.w = ...
                        format!("// gl_Position removed: {}", trimmed)
                    } else {
                        line.replace("gl_Position", "gl_FragCoord")
                    }
                } else {
                    line.replace("gl_Position", "gl_FragCoord")
                }
            } else if stage == naga::ShaderStage::Fragment && line.contains("gl_VertexIndex") {
                // VertexIndex is not valid in fragment stage; replace with 0
                line.replace("gl_VertexIndex", "0")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Debug: show any lines with `precise` that might cause parsing issues
    for (idx, line) in glsl_clean.lines().enumerate() {
        if line.contains("precise") {
            eprintln!("[SPIRV→WGSL] precise line {}: {}", idx + 1, line.trim());
        }
    }

    // Debug: show lines with intBitsToFloat / uintBitsToFloat
    for (idx, line) in glsl_clean.lines().enumerate() {
        if line.contains("intBitsToFloat") || line.contains("uintBitsToFloat") || line.contains("floatBitsToInt") || line.contains("floatBitsToUint") {
            eprintln!("[SPIRV→WGSL] bits cast line {}: {}", idx + 1, line.trim());
        }
    }
    // Show the first few lines (version, extensions)
    for (idx, line) in glsl_clean.lines().take(5).enumerate() {
        eprintln!("[SPIRV→WGSL] header line {}: {}", idx + 1, line.trim());
    }

    // Parse GLSL with naga GLSL frontend
    let mut frontend = glsl::Frontend::default();
    let options = glsl::Options::from(stage);
    let module = frontend
        .parse(&options, &glsl_clean)
        .map_err(|e| anyhow!("naga GLSL parse ({}): {:?}", shader_name, e))?;

    let descriptors = extract_descriptors_from_module(&module);
    eprintln!("[SPIRV→WGSL] GLSL→naga parsed ({}): {} global vars, {} descriptors",
        shader_name, module.global_variables.len(), descriptors.len());

    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| anyhow!("naga validation ({}): {:?}", shader_name, e))?;

    let mut wgsl = String::new();
    let mut writer = wgsl::Writer::new(&mut wgsl, wgsl::WriterFlags::empty());
    writer
        .write(&module, &info)
        .map_err(|e| anyhow!("naga WGSL write ({}): {:?}", shader_name, e))?;

    eprintln!("[SPIRV→WGSL] ✓ {} lines of WGSL generated ({})",
        wgsl.lines().count(), shader_name);
    // Write WGSL to a file for offline analysis (clear old first)
    {
        use std::io::Write;
        let dump_name = format!("hitbox_{}.wgsl", shader_name.replace('/', "_"));
        let debug_path = crate::scratch_dirs::workshop_tmp_path(&dump_name);
        let _ = std::fs::remove_file(&debug_path);
        if let Ok(mut f) = std::fs::File::create(&debug_path) {
            let _ = writeln!(f, "// {} — {} bytes WGSL", shader_name, wgsl.len());
            let _ = write!(f, "{}", wgsl);
            eprintln!("[DBG] Wrote WGSL to {}", debug_path.display());
        }
    }
    // For vertex shaders, show buffer access and position computation
    if shader_name.contains("vs") || shader_name.contains("vertex") {
        for (idx, line) in wgsl.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.contains("_m0[") {
                eprintln!("[SPIRV→WGSL] VS _m0 line {}: {}", idx + 1, trimmed);
            }
            if trimmed.contains("gl_Position") && (trimmed.contains("=") || trimmed.contains("let")) {
                eprintln!("[SPIRV→WGSL] VS pos line {}: {}", idx + 1, trimmed);
            }
        }
    }

    Ok((wgsl, descriptors))
}

/// Maximum fragment color-output `@location` index allowed by WebGPU.
///
/// WebGPU rejects any fragment color output whose location is `>= max_color_attachments`.
/// 8 is the spec default (and the value reported on native Vulkan/Metal/DX adapters).
pub const MAX_COLOR_ATTACHMENT_LOCATIONS: u32 = 8;

/// MRT locations kept when compositing particles to a single RGBA target.
///
/// NVN deferred FS shaders write G-buffer data to `@location(1+)`, but the visible particle
/// colour always lives at `@location(0)` (`out_attr0_`). The offscreen pass and blit composite
/// only bind that one attachment — no merge of secondary MRT outputs is required.
pub const PARTICLE_COMPOSITE_MRT_LOCATIONS: u32 = 1;

/// Replace GLSL `textureQueryLod(...)` / `textureQueryLevels(...)` with safe stubs.
///
/// naga's GLSL frontend lacks these intrinsics; NVN particle FS only uses them for LOD hints.
fn stub_glsl_texture_query_calls(glsl: &str) -> String {
    fn replace_calls(glsl: &str, name: &str, stub: &str) -> String {
        let mut out = String::with_capacity(glsl.len());
        let mut i = 0;
        while let Some(rel) = glsl[i..].find(name) {
            let start = i + rel;
            out.push_str(&glsl[i..start]);
            let after_name = start + name.len();
            if glsl.as_bytes().get(after_name) == Some(&b'(') {
                if let Some(close) = matching_close_paren(glsl, after_name) {
                    out.push_str(stub);
                    i = close + 1;
                    continue;
                }
            }
            out.push_str(name);
            i = after_name;
        }
        out.push_str(&glsl[i..]);
        out
    }
    let glsl = replace_calls(glsl, "textureQueryLod", "vec2(0.0)");
    replace_calls(&glsl, "textureQueryLevels", "1")
}

/// Pair split `texture2D` / `sampler` declarations and rewrite sampling builtins.
fn combine_glsl_split_textures(glsl: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in glsl.lines() {
        let trimmed = line.trim();
        let Some(idx) = trimmed.find("uniform texture2D ") else {
            continue;
        };
        let rest = &trimmed[idx + "uniform texture2D ".len()..];
        let tex = rest.trim_end_matches(';').trim();
        if let Some(suffix) = tex.strip_prefix("texture_") {
            pairs.push((tex.to_string(), format!("sampler_{suffix}")));
        }
    }
    if pairs.is_empty() {
        return glsl.to_string();
    }
    let funcs = [
        "textureLodOffset",
        "textureGradOffset",
        "textureLod",
        "textureGrad",
        "textureOffset",
        "texelFetch",
        "texture",
    ];
    let mut out = glsl.to_string();
    for (tex, samp) in pairs {
        for func in funcs {
            let from = format!("{func}({tex},");
            let to = format!("{func}(sampler2D({tex}, {samp}),");
            out = out.replace(&from, &to);
        }
    }
    out
}

/// Find the index of the `)` that closes the `(` at `open_paren_idx` (paren depth only).
fn matching_close_paren(s: &str, open_paren_idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if open_paren_idx >= bytes.len() || bytes[open_paren_idx] != b'(' {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open_paren_idx;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Trim a fragment shader's `FragmentOutput` (multiple-render-target) struct to color
/// `@location`s that WebGPU accepts and/or the particle composite path binds.
///
/// NVN deferred/G-buffer shaders frequently declare up to ~10 MRT color outputs, but
/// particles composite into a single color attachment at `@location(0)`. Secondary outputs
/// (normals, material IDs, etc.) are not read by the blit path. WebGPU permits a fragment
/// shader to write to color locations that have no bound target *provided the location index
/// is in range* (`< max_color_attachments`), but spirv-cross faithfully reproduces all ~10
/// outputs so `@location(8)`/`@location(9)` make pipeline creation fail with
/// `ColorAttachmentLocationTooLarge`.
///
/// This removes out-of-range fields from the `FragmentOutput` struct and the matching
/// positional `return FragmentOutput(...)` constructor, preserving `@location(0)` (the visible
/// colour) and the relative order of every kept field. Pass [`PARTICLE_COMPOSITE_MRT_LOCATIONS`]
/// to drop deferred G-buffer outputs 1+; pass [`MAX_COLOR_ATTACHMENT_LOCATIONS`] to keep all
/// in-range locations. `@builtin(...)` outputs (e.g. `frag_depth`) are always kept. Returns the
/// input unchanged if there is nothing to trim or the shape is unexpected.
pub fn clamp_fragment_output_locations(wgsl: &str, max_locations: u32) -> String {
    let start_marker = "struct FragmentOutput {";
    let Some(start) = wgsl.find(start_marker) else {
        return wgsl.to_string();
    };
    let body_start = start + start_marker.len();
    let Some(close_rel) = wgsl[body_start..].find('}') else {
        return wgsl.to_string();
    };
    let body = &wgsl[body_start..body_start + close_rel];

    // Parse struct fields in source order. The order matches the positional
    // `return FragmentOutput(arg0, arg1, ...)` constructor emitted by spirv-cross.
    struct OutField {
        line: String,
        keep: bool,
    }
    let mut fields: Vec<OutField> = Vec::new();
    let mut saw_unknown = false;
    for raw in body.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let field_text = trimmed.trim_end_matches(',').to_string();
        if trimmed.starts_with("@builtin") {
            fields.push(OutField { line: field_text, keep: true });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("@location(") {
            if let Some(paren) = rest.find(')') {
                if let Ok(loc) = rest[..paren].parse::<u32>() {
                    fields.push(OutField { line: field_text, keep: loc < max_locations });
                    continue;
                }
            }
        }
        // Unrecognized member: bail out rather than risk misaligning the constructor.
        saw_unknown = true;
        break;
    }

    if saw_unknown || fields.is_empty() || fields.iter().all(|f| f.keep) {
        return wgsl.to_string();
    }

    let mut new_struct = String::from("struct FragmentOutput {\n");
    for f in fields.iter().filter(|f| f.keep) {
        new_struct.push_str("    ");
        new_struct.push_str(&f.line);
        new_struct.push_str(",\n");
    }
    new_struct.push('}');

    let struct_end = body_start + close_rel + 1; // include the closing '}'
    let mut result = format!("{}{}{}", &wgsl[..start], new_struct, &wgsl[struct_end..]);

    // Rebuild the positional `return FragmentOutput(...)` constructor to match kept fields.
    let ctor_marker = "return FragmentOutput(";
    if let Some(ret_start) = result.find(ctor_marker) {
        let open_idx = ret_start + ctor_marker.len() - 1; // index of '('
        if let Some(close_idx) = matching_close_paren(&result, open_idx) {
            let args = split_top_level_commas(&result[open_idx + 1..close_idx]);
            if args.len() == fields.len() {
                let kept: Vec<String> = args
                    .into_iter()
                    .zip(fields.iter())
                    .filter(|(_, f)| f.keep)
                    .map(|(a, _)| a)
                    .collect();
                let new_ret = format!("return FragmentOutput({})", kept.join(", "));
                result.replace_range(ret_start..close_idx + 1, &new_ret);
            }
        }
    }

    result
}

/// After native `main_1()` varyings, overwrite `gl_Position` with VP×billboard world position
/// without clobbering NVN-computed `out_attr*`.
pub fn finalize_native_vs_clip_position(wgsl: &str) -> String {
    insert_billboard_clip_position(wgsl, BillboardClipMode::PositionOnly)
}

/// Replace the NVN vertex shader's clip-space position with a clean billboard transform.
///
/// The decoded NVN position chain depends on a web of `cbuf_8` transform matrices
/// (`[0..3]`, `[8..11]`, `[12..14]`) whose exact NintendoWare semantics are not reliably
/// reproducible from the game data we have, so the native chain emits degenerate/off-screen
/// clip coordinates. Since the fragment shader consumes only the `out_attr*` varyings (color,
/// UV, life) — never the clip position — we let `main_1()` compute all of those natively, then
/// overwrite only `gl_Position` with a correct camera-facing billboard:
///
///   world = center + corner.x·size·right + corner.y·size·up
///   clip  = VP · world
///
/// where `center = in_attr0_`, `corner = in_attr6_.xy`, `size = in_attr4_.y`. Most particle BNSH
/// shaders store the VP matrix in `cbuf_8[8..11]` (Family A); older variants use `cbuf_9[0..3]`
/// (Family B). The camera basis is `cbuf_9[46]` (right) / `cbuf_9[47].yzw` (up). Referencing
/// those slots makes the data-driven NVN evaluator fill them (the slot-usage scan runs on the
/// patched WGSL). Returns the input unchanged when the shader is not a billboard particle VS
/// (missing center/size inputs), so mesh/primitive shaders keep their native transform.
pub fn override_billboard_position(wgsl: &str) -> String {
    insert_billboard_clip_position(wgsl, BillboardClipMode::OverrideAll)
}

enum BillboardClipMode {
    /// Replace gl_Position and forward CPU-simulated colour/UV varyings.
    OverrideAll,
    /// Replace gl_Position only; native main_1() outputs are preserved.
    PositionOnly,
}

fn insert_billboard_clip_position(wgsl: &str, mode: BillboardClipMode) -> String {
    let marker = "main_1();";
    let Some(pos) = wgsl.find(marker) else {
        return wgsl.to_string();
    };
    let uses_cbuf8 = uses_cbuf8_vp(wgsl);
    let vp_buf = if uses_cbuf8 { "cbuf_8_1_" } else { "cbuf_9_1_" };
    let vp_base: u32 = if uses_cbuf8 { 8 } else { 0 };
    let base_needed = ["in_attr0_1", "in_attr4_1", vp_buf, "gl_Position"];
    if base_needed.iter().any(|id| !wgsl.contains(id)) {
        return wgsl.to_string();
    }
    if !wgsl.contains("cbuf_9_1_") {
        return wgsl.to_string();
    }
    let partial_family_b = family_b_vp(wgsl) && !uses_cbuf9_camera_basis(wgsl);
    // Family-A (cbuf_8 VP) stores a different camera-basis encoding in cbuf_9[46]/[47] than
    // finalize expects; derive right/up from the VP block like OverrideAll so Samus bomb-style
    // billboards rasterize on-screen while native colour varyings stay from main_1().
    let use_cbuf_basis = matches!(mode, BillboardClipMode::PositionOnly)
        && !partial_family_b
        && !uses_cbuf8
        && wgsl.contains("cbuf_9_1_._m0_[46]");
    // Corner offsets: CPU uploads attr6 (±0.5 half-extents). When attr6 is absent, attr2 quad
    // UV corners (0..1) are remapped to ±0.5 offsets for legacy billboard VS without half-extents.
    let corner_expr = if wgsl.contains("in_attr6_1") {
        // CPU stores rotated corner + pivot in .xy and pivot alone in .zw for the native GPR chain.
        "(in_attr6_1.xy - in_attr6_1.zw)".to_string()
    } else if wgsl.contains("in_attr2_1") {
        "(in_attr2_1.xy - vec2<f32>(0.5, 0.5))".to_string()
    } else {
        return wgsl.to_string();
    };
    let has_attr7 = wgsl.contains("in_attr7_1");
    let has_attr9 = wgsl.contains("in_attr9_1");
    let tilt_block = if has_attr9 {
        "\x20       if (i32(in_attr9_1.w) != 0) {\n\
        \x20           let _rx = in_attr9_1.x;\n\
        \x20           let _ry = in_attr9_1.y;\n\
        \x20           if (abs(_rx) > 0.001) {\n\
        \x20               let _cx = cos(_rx);\n\
        \x20               let _sx = sin(_rx);\n\
        \x20               _right = vec3<f32>(_right.x, _right.y * _cx - _right.z * _sx, _right.y * _sx + _right.z * _cx);\n\
        \x20               _up = vec3<f32>(_up.x, _up.y * _cx - _up.z * _sx, _up.y * _sx + _up.z * _cx);\n\
        \x20           }\n\
        \x20           if (abs(_ry) > 0.001) {\n\
        \x20               let _cy = cos(_ry);\n\
        \x20               let _sy = sin(_ry);\n\
        \x20               let _nr = _right;\n\
        \x20               _right = vec3<f32>(_nr.x * _cy + _nr.z * _sy, _nr.y, -_nr.x * _sy + _nr.z * _cy);\n\
        \x20               let _nu = _up;\n\
        \x20               _up = vec3<f32>(_nu.x * _cy + _nu.z * _sy, _nu.y, -_nu.x * _sy + _nu.z * _cy);\n\
        \x20           }\n\
        \x20       }\n"
    } else {
        ""
    };
    let vp_fwd = "\x20       let _fwd = normalize(vec3<f32>(_vp0.z, _vp1.z, _vp2.z));\n";
    let vp_derived_basis = format!(
        "{vp_fwd}\
        \x20       var _right = normalize(cross(vec3<f32>(0.0, 1.0, 0.0), _fwd));\n\
        \x20       if (length(_right) < 0.001) {{ _right = vec3<f32>(1.0, 0.0, 0.0); }}\n\
        \x20       var _up = normalize(cross(_fwd, _right));\n\
        \x20       if (length(_up) < 0.001) {{ _up = vec3<f32>(0.0, 1.0, 0.0); }}\n",
        vp_fwd = vp_fwd,
    );
    let bb_type_overrides = "\x20       let _vel = in_attr3_1.xyz;\n\
        \x20       if (_bb_type == 8) {\n\
        \x20           _up = vec3<f32>(0.0, 1.0, 0.0);\n\
        \x20           let _fwd = normalize(vec3<f32>(_vp0.z, _vp1.z, _vp2.z));\n\
        \x20           _right = normalize(cross(_up, _fwd));\n\
        \x20       } else if (_bb_type == 1) {\n\
        \x20           _right = vec3<f32>(1.0, 0.0, 0.0);\n\
        \x20           _up = vec3<f32>(0.0, 1.0, 0.0);\n\
        \x20       } else if (_bb_type == 2) {\n\
        \x20           _right = vec3<f32>(1.0, 0.0, 0.0);\n\
        \x20           _up = vec3<f32>(0.0, 0.0, 1.0);\n\
        \x20       } else if (_bb_type == 3) {\n\
        \x20           let _fwd = normalize(_vel);\n\
        \x20           if (length(_fwd) > 0.001) {\n\
        \x20               _up = vec3<f32>(0.0, 1.0, 0.0);\n\
        \x20               _right = normalize(cross(_up, _fwd));\n\
        \x20           }\n\
        \x20       } else if (_bb_type == 4) {\n\
        \x20           let _fwd = normalize(_vel);\n\
        \x20           if (length(_fwd) > 0.001) {\n\
        \x20               _right = normalize(cross(_up, _fwd));\n\
        \x20               _up = normalize(cross(_fwd, _right));\n\
        \x20           }\n\
        \x20       } else if (_bb_type == 5 || _bb_type == 6) {\n\
        \x20           let _along = normalize(_vel);\n\
        \x20           if (length(_along) > 0.001) {\n\
        \x20               _right = normalize(cross(_up, _along));\n\
        \x20               _up = _along;\n\
        \x20           }\n\
        \x20       } else if (_bb_type == 7) {\n\
        \x20           let _mesh_up = vec3<f32>(cbuf_9_1_._m0_[121].y, cbuf_9_1_._m0_[121].z, cbuf_9_1_._m0_[121].w);\n\
        \x20           if (length(_mesh_up) > 0.001) { _up = normalize(_mesh_up); }\n\
        \x20       }\n";
    let basis_block = if has_attr7 {
        let mut s = String::from("\x20       let _bb_type = i32(in_attr7_1.w);\n");
        if use_cbuf_basis {
            s.push_str("\x20       var _right = cbuf_9_1_._m0_[120].xyz;\n");
            match mode {
                BillboardClipMode::OverrideAll => {
                    s.push_str(
                        "\x20       var _up = vec3<f32>(cbuf_9_1_._m0_[121].y, cbuf_9_1_._m0_[121].z, cbuf_9_1_._m0_[121].w);\n",
                    );
                }
                BillboardClipMode::PositionOnly => {
                    s.push_str(
                        "\x20       let _fwd = normalize(vec3<f32>(_vp0.z, _vp1.z, _vp2.z));\n\
        \x20       var _up = normalize(cross(_fwd, _right));\n\
        \x20       if (length(_up) < 0.001) { _up = vec3<f32>(0.0, 1.0, 0.0); }\n",
                    );
                }
            }
        } else {
            s.push_str(&vp_derived_basis);
        }
        s.push_str(bb_type_overrides);
        s.push_str(tilt_block);
        s
    } else if use_cbuf_basis {
        match mode {
            BillboardClipMode::OverrideAll => {
                "\x20       let _right = cbuf_9_1_._m0_[120].xyz;\n\
        \x20       let _up = vec3<f32>(cbuf_9_1_._m0_[121].y, cbuf_9_1_._m0_[121].z, cbuf_9_1_._m0_[121].w);\n"
                    .to_string()
            }
            BillboardClipMode::PositionOnly => {
                "\x20       let _right = cbuf_9_1_._m0_[120].xyz;\n\
        \x20       let _fwd = normalize(vec3<f32>(_vp0.z, _vp1.z, _vp2.z));\n\
        \x20       var _up = normalize(cross(_fwd, _right));\n\
        \x20       if (length(_up) < 0.001) { _up = vec3<f32>(0.0, 1.0, 0.0); }\n"
                    .to_string()
            }
        }
    } else {
        vp_derived_basis
    };
    let insert_at = pos + marker.len();
    let mut override_code = format!(
        "\n    {{\n\
        \x20       let _vp0 = {vp_buf}._m0_[{vp0}];\n\
        \x20       let _vp1 = {vp_buf}._m0_[{vp1}];\n\
        \x20       let _vp2 = {vp_buf}._m0_[{vp2}];\n\
        \x20       let _vp3 = {vp_buf}._m0_[{vp3}];\n",
        vp_buf = vp_buf,
        vp0 = vp_base,
        vp1 = vp_base + 1,
        vp2 = vp_base + 2,
        vp3 = vp_base + 3,
    );
    override_code.push_str(&basis_block);
    let world_block = if std::env::var("FX_NATIVE_CLIP").is_ok() {
        // Keep main_1()'s own clip position (the game's corner/size/stretch math) —
        // diagnostic for aligning our CPU billboard expansion with the native chain.
        String::from("        // FX_NATIVE_CLIP: native gl_Position kept\n")
    } else if std::env::var("FX_DEBUG_CLIP_CENTER").is_ok() {
        // Spread corners so the quad has non-zero area (identical clip coords rasterize nothing).
        format!(
            "\x20       let _corner = {corner_expr};\n\
            \x20       gl_Position = vec4<f32>(_corner.x * 0.45, _corner.y * 0.45, 0.5, 1.0);\n",
            corner_expr = corner_expr,
        )
    } else if has_attr7 {
        format!(
            "\x20       let _sz = in_attr4_1.y;\n\
            \x20       let _aspect = in_attr4_1.z;\n\
            \x20       let _corner = {corner_expr};\n\
            \x20       let _width_scale = select(_aspect, 1.0, _bb_type == 5 || _bb_type == 6);\n\
            \x20       let _world = in_attr0_1.xyz + _corner.x * _sz * _width_scale * _right + _corner.y * _sz * _up;\n\
            \x20       gl_Position = _vp0 * _world.x + _vp1 * _world.y + _vp2 * _world.z + _vp3;\n",
            corner_expr = corner_expr,
        )
    } else {
        format!(
            "\x20       let _sz = in_attr4_1.y;\n\
            \x20       let _aspect = in_attr4_1.z;\n\
            \x20       let _corner = {corner_expr};\n\
            \x20       let _world = in_attr0_1.xyz + _corner.x * _sz * _aspect * _right + _corner.y * _sz * _up;\n\
            \x20       gl_Position = _vp0 * _world.x + _vp1 * _world.y + _vp2 * _world.z + _vp3;\n",
            corner_expr = corner_expr,
        )
    };
    override_code.push_str(&world_block);
    if matches!(mode, BillboardClipMode::OverrideAll) {
        // Forward CPU-simulated per-particle colour and quad UV so both native and patched FS
        // paths receive sane inputs at the standard locations — EXCEPT outputs main_1() itself
        // computes: the cbuf_9 keyframe tables feed the native colour chain correct colours,
        // and a CPU overwrite would clobber the chain's output.
        let main1 = vs_main1_body(wgsl);
        if !main1.contains("out_attr0_") && wgsl.contains("out_attr0_") && wgsl.contains("in_attr1_1")
        {
            override_code.push_str("        out_attr0_ = in_attr1_1;\n");
        }
        if !main1.contains("out_attr1_") && wgsl.contains("out_attr1_") && wgsl.contains("in_attr1_1")
        {
            override_code.push_str("        out_attr1_ = in_attr1_1;\n");
        }
        if wgsl.contains("out_attr2_")
            && (wgsl.contains("in_attr2_1")
                || wgsl.contains("@location(2) in_attr2_")
                || wgsl.contains("in_attr2_:"))
        {
            override_code.push_str("        out_attr2_ = in_attr2_1;\n");
        }
        if wgsl.contains("out_attr5_")
            && (wgsl.contains("in_attr5_1")
                || wgsl.contains("@location(5) in_attr5_")
                || wgsl.contains("in_attr5_:"))
        {
            override_code.push_str("        out_attr5_ = in_attr5_1;\n");
        }
        if wgsl.contains("out_attr10_") && wgsl.contains("in_attr10_1") {
            override_code.push_str("        out_attr10_ = in_attr10_1;\n");
        }
        if wgsl.contains("out_attr11_") && wgsl.contains("in_attr11_1") {
            override_code.push_str("        out_attr11_ = in_attr11_1;\n");
        }
        if wgsl.contains("out_attr12_") && wgsl.contains("in_attr12_1") {
            override_code.push_str("        out_attr12_ = in_attr12_1;\n");
        }
    }
    // Forward CPU attr3/attr4 only when main_1() does not itself compute the varying, so
    // clip overrides never leave zero varyings that discard every fragment. When the native
    // chain (frame-clock fed) DOES write them, keep its outputs — out_attr3 carries the
    // colour evaluated from the cbuf_9 keyframe tables, and the CPU overwrite rendered all
    // fire/flare emitters grayscale. Legacy OverrideAll distrusts main_1 wholesale and
    // keeps the unconditional forward.
    let force_cpu_attr34 = matches!(mode, BillboardClipMode::OverrideAll);
    let main1_for_attr34 = vs_main1_body(wgsl);
    if (force_cpu_attr34 || !main1_writes_varying(&main1_for_attr34, "out_attr3_"))
        && wgsl.contains("out_attr3_")
        && wgsl.contains("in_attr3_1")
    {
        override_code.push_str("        out_attr3_ = in_attr3_1;\n");
    }
    if (force_cpu_attr34 || !main1_writes_varying(&main1_for_attr34, "out_attr4_"))
        && wgsl.contains("out_attr4_")
        && wgsl.contains("in_attr4_1")
    {
        override_code.push_str("        out_attr4_ = in_attr4_1;\n");
    }
    // Family-A bomb VS computes atlas UV in main_1() without reading CPU attr2; forward quad UV.
    if !vs_main1_reads_in_attr2(wgsl)
        && wgsl.contains("out_attr2_")
        && wgsl.contains("in_attr2_1")
    {
        override_code.push_str("        out_attr2_ = in_attr2_1;\n");
    }
    // FX_DEBUG_VS_OUT="<wgsl scalar exprs>": overwrite out_attr0 with arbitrary VS values
    // (gprs, cbuf reads) after main_1(), so the FS ATTR0 debug can visualize any VS
    // register. Example: FX_DEBUG_VS_OUT="gpr_10_, gpr_0_ * 0.05, gpr_1_ * 0.05, 1.0".
    if let Ok(expr) = std::env::var("FX_DEBUG_VS_OUT") {
        if wgsl.contains("out_attr0_") && !expr.is_empty() {
            override_code.push_str(&format!(
                "        out_attr0_ = vec4<f32>({expr});\n"
            ));
        }
    }
    override_code.push_str("    }\n");
    let mut result = wgsl.to_string();
    result.insert_str(insert_at, &override_code);
    result
}

/// True when `main_1()` assigns the varying beyond naga's `_NNN_init` zero-initializers
/// (whole-vector assignment from a computed value, or any per-component write).
fn main1_writes_varying(main1: &str, name: &str) -> bool {
    let assign = format!("{name} =");
    let component = format!("{name}.");
    main1.lines().any(|l| {
        let t = l.trim_start();
        (t.starts_with(&assign) && !t.contains("_init;")) || t.starts_with(&component)
    })
}

/// Insert an early return at the start of `@fragment fn main` (after input copies).
fn inject_fragment_main_early_return(wgsl: &str, return_stmt: &str) -> Option<String> {
    let entry = "@fragment";
    let frag_pos = wgsl.find(entry)?;
    let fn_pos = wgsl[frag_pos..].find("fn main")? + frag_pos;
    let body = wgsl[fn_pos..].find('{')? + fn_pos + 1;
    let main_1 = wgsl[body..].find("main_1();")?;
    let insert_at = body + main_1;
    let mut result = wgsl.to_string();
    result.insert_str(insert_at, &format!("\n    {return_stmt}\n"));
    Some(result)
}

/// Select the debug fragment-output expression from the active `FX_DEBUG_*_FS` env var.
/// Used by both the early-return bypass and the final-output override in
/// [`debug_solid_fragment_wgsl`] so the two agree. Each variant is a WGSL `vec4<f32>` expr
/// evaluated with the live varyings/cbufs; default is opaque magenta.
fn debug_fs_output_expr(wgsl: &str) -> String {
    if std::env::var("FX_DEBUG_CULL_FS").is_ok()
        && wgsl.contains("in_attr5_1")
        && wgsl.contains("cbuf_10_1_")
    {
        // White when the fragment-stage life gate would NOT cull (in_attr5_1.w <= cbuf_10[2].x).
        "select(vec4<f32>(0.0, 0.0, 0.0, 1.0), vec4<f32>(1.0, 1.0, 1.0, 1.0), in_attr5_1.w <= cbuf_10_1_._m0_[2].x)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF10_FS").is_ok() && wgsl.contains("cbuf_10_1_") {
        "vec4<f32>(cbuf_10_1_._m0_[0].x, cbuf_10_1_._m0_[0].y, cbuf_10_1_._m0_[0].z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_NATIVE_RGB_FS").is_ok() && wgsl.contains("out_attr0_") {
        "vec4<f32>(out_attr0_.x, out_attr0_.y, out_attr0_.z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF59_FS").is_ok() && wgsl.contains("cbuf_9_1_") {
        "vec4<f32>(cbuf_9_1_._m0_[59].x, cbuf_9_1_._m0_[59].x, cbuf_9_1_._m0_[59].x, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF60_FS").is_ok() && wgsl.contains("cbuf_9_1_") {
        "vec4<f32>(cbuf_9_1_._m0_[60].x, cbuf_9_1_._m0_[60].y, cbuf_9_1_._m0_[60].z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF84_FS").is_ok() && wgsl.contains("cbuf_9_1_") {
        "vec4<f32>(cbuf_9_1_._m0_[84].x, cbuf_9_1_._m0_[84].y, cbuf_9_1_._m0_[84].z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_ATTR0_FS").is_ok() && wgsl.contains("in_attr0_1") {
        // Native VS colour-chain varying as received by the FS.
        "vec4<f32>(in_attr0_1.rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_ATTR1_FS").is_ok() && wgsl.contains("in_attr1_1") {
        "vec4<f32>(in_attr1_1.rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_VCOLOR_FS").is_ok() && wgsl.contains("in_attr1_1") {
        "vec4<f32>(in_attr1_1.rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_UV2ZW_FS").is_ok() && wgsl.contains("in_attr2_1") {
        // Live colour UV (fract'd so out-of-[0,1] still shows a gradient, not a flat clamp).
        "vec4<f32>(fract(in_attr2_1.z), fract(in_attr2_1.w), 0.0, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_UV2ZW_RAW_FS").is_ok() && wgsl.contains("in_attr2_1") {
        // Raw (unfract'd) colour UV: if it clamps to yellow/white the range exceeds [0,1] (wraps).
        "vec4<f32>(in_attr2_1.z, in_attr2_1.w, 0.0, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_UV2ZW16_FS").is_ok() && wgsl.contains("in_attr2_1") {
        // Colour UV / 16: if this shows a 0..1 gradient, the raw UV spans 0..16 (16x wrap).
        "vec4<f32>(in_attr2_1.z * 0.0625, in_attr2_1.w * 0.0625, 0.0, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_UV2XY_FS").is_ok() && wgsl.contains("in_attr2_1") {
        // Live indirect/base UV.
        "vec4<f32>(fract(in_attr2_1.x), fract(in_attr2_1.y), 0.0, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_UV3XY_FS").is_ok() && wgsl.contains("in_attr3_1") {
        "vec4<f32>(fract(in_attr3_1.x), fract(in_attr3_1.y), 0.0, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_TEX1_FS").is_ok()
        && wgsl.contains("texture_1_")
        && wgsl.contains("in_attr2_1")
    {
        // Sample the colour texture (native FS's texture_1) at the colour UV, alpha opaque —
        // reveals whether the decoded texture itself is striped (BC5 decode) vs a geometry/alpha
        // pattern.
        "vec4<f32>(textureSample(texture_1_, sampler_1_, vec2<f32>(in_attr2_1.z, in_attr2_1.w)).rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_TEX1A_FS").is_ok()
        && wgsl.contains("texture_1_")
        && wgsl.contains("in_attr2_1")
    {
        // The colour texture's ALPHA channel on all RGB (opaque) — is the alpha striped?
        "vec4<f32>(vec3<f32>(textureSample(texture_1_, sampler_1_, vec2<f32>(in_attr2_1.z, in_attr2_1.w)).w), 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_TEX0_FS").is_ok()
        && wgsl.contains("texture_0_")
        && wgsl.contains("in_attr2_1")
    {
        "vec4<f32>(textureSample(texture_0_, sampler_0_, vec2<f32>(in_attr2_1.x, in_attr2_1.y)).rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_TEX0A_FS").is_ok()
        && wgsl.contains("texture_0_")
        && wgsl.contains("in_attr2_1")
    {
        "vec4<f32>(vec3<f32>(textureSample(texture_0_, sampler_0_, vec2<f32>(in_attr2_1.x, in_attr2_1.y)).w), 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_PROBE").is_ok() && wgsl.contains("_dbg_probe") {
        "_dbg_probe".to_string()
    } else {
        "vec4<f32>(1.0, 0.0, 1.0, 1.0)".to_string()
    }
}

/// Debug: force the fragment shader's primary color output (location 0) to a constant
/// opaque magenta, so billboard geometry coverage can be verified independently of the
/// NVN color chain / texture binding. Gated by the `FX_DEBUG_SOLID_FS` env var.
pub fn debug_solid_fragment_wgsl(wgsl: &str) -> String {
    // Optional empirical probe: snapshot a native-FS intermediate into a capture global at a
    // chosen point, then output it. Lets us read exact runtime values (e.g. the per-particle
    // time `gpr_10_` feeding the colour spline) instead of hand-tracing register reuse.
    let wgsl_probed;
    let wgsl: &str = if let Ok(probe) = std::env::var("FX_DEBUG_PROBE") {
        wgsl_probed = inject_fs_probe(wgsl, &probe);
        &wgsl_probed
    } else {
        wgsl
    };

    // Bypass native main_1() discard/alpha gates — they run before the patched final return.
    // The `@fragment fn main` copies its inputs into the `in_attr*_1` privates before calling
    // `main_1()`, so the debug expression (live varyings/cbufs) is valid at the early return.
    if let Some(bypass) = inject_fragment_main_early_return(
        wgsl,
        &format!("return FragmentOutput({});", debug_fs_output_expr(wgsl)),
    ) {
        return bypass;
    }

    let ctor = "return FragmentOutput(";
    let Some(ret) = wgsl.rfind(ctor) else {
        return wgsl.to_string();
    };
    let open = ret + ctor.len() - 1;
    let Some(close) = matching_close_paren(wgsl, open) else {
        return wgsl.to_string();
    };
    let mut args = split_top_level_commas(&wgsl[open + 1..close]);
    if args.is_empty() {
        return wgsl.to_string();
    }
    args[0] = debug_fs_output_expr(wgsl);
    let mut result = wgsl.to_string();
    result.replace_range(ret..close + 1, &format!("{ctor}{})", args.join(", ")));
    result
}

/// Inject an empirical capture probe into a native FS. Adds a module-scope `_dbg_probe`
/// global and snapshots a chosen intermediate into it at the right program point. The probe
/// name selects what to capture:
///   - "time": the per-particle time `gpr_10_` just before the colour spline reads cbuf_9[60]
///   - "kf60w": keyframe-A time = cbuf_9[60].w (verifies the value reaching the FS)
///   - "branch": flow_var_4_ (1=flipbook-frame time path taken, 0=lifetime path)
/// `debug_solid_fragment_wgsl` then outputs `_dbg_probe` when FX_DEBUG_PROBE is set.
fn inject_fs_probe(wgsl: &str, probe: &str) -> String {
    let mut result = wgsl.to_string();

    // 1. Declare the capture global after out_attr0_.
    let decl_anchor = "var<private> out_attr0_: vec4<f32>;";
    if let Some(pos) = result.find(decl_anchor) {
        let insert_at = pos + decl_anchor.len();
        result.insert_str(
            insert_at,
            "\nvar<private> _dbg_probe: vec4<f32> = vec4<f32>(0.0, 0.0, 0.0, 1.0);",
        );
    } else {
        return wgsl.to_string();
    }

    // 2. Capture at the appropriate point. `after` selects whether to inject after the anchor's
    //    line (bisection beacons) or before it (so register globals hold pre-anchor values).
    let (capture_expr, anchor, after): (String, &str, bool) = match probe {
        "time" => (
            "vec4<f32>(gpr_10_, gpr_10_, gpr_10_, 1.0)".to_string(),
            "cbuf_9_1_._m0_[60]",
            false,
        ),
        "kf60w" => (
            "vec4<f32>(cbuf_9_1_._m0_[60].w, cbuf_9_1_._m0_[60].w, cbuf_9_1_._m0_[60].w, 1.0)"
                .to_string(),
            "cbuf_9_1_._m0_[60]",
            false,
        ),
        "branch" => (
            "select(vec4<f32>(0.0, 0.0, 0.0, 1.0), vec4<f32>(1.0, 1.0, 1.0, 1.0), flow_var_4_)"
                .to_string(),
            "cbuf_9_1_._m0_[60]",
            false,
        ),
        "alive" => (
            "vec4<f32>(1.0, 1.0, 1.0, 1.0)".to_string(),
            "cbuf_9_1_._m0_[60]",
            false,
        ),
        // Bisection beacons: set _dbg_probe=white once execution passes the anchor.
        "b_top" => ("vec4<f32>(1.0, 1.0, 1.0, 1.0)".to_string(), "fn main_1() {", true),
        "b_mid" => ("vec4<f32>(1.0, 1.0, 1.0, 1.0)".to_string(), "flow_var_4_ = false;", true),
        _ => return result,
    };
    if let Some(rel) = result.find(anchor) {
        if after {
            let line_end = result[rel..].find('\n').map(|i| rel + i + 1).unwrap_or(result.len());
            result.insert_str(line_end, &format!("    _dbg_probe = {capture_expr};\n"));
        } else {
            // Back up to the start of the statement (line) containing the anchor.
            let line_start = result[..rel].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let indent: String = result[line_start..]
                .chars()
                .take_while(|c| *c == ' ')
                .collect();
            result.insert_str(line_start, &format!("{indent}_dbg_probe = {capture_expr};\n"));
        }
    }
    crate::scratch_dirs::write_workshop_wgsl_dump("hitbox_probed_fs.wgsl", &result);
    result
}

/// Per-pixel alpha-test threshold for opaque-core depth-write passes (within-path occlusion).
pub const OPAQUE_CORE_DEPTH_ALPHA_TEST: f32 = 0.5;

/// True when fragment WGSL includes editor soft-particle depth fade (`@group(3)`).
pub fn native_fs_soft_particle_needed(wgsl: &str) -> bool {
    wgsl.contains("_fx_apply_soft_particle")
}

fn soft_particle_group3_decls() -> &'static str {
    "struct FxSoftParticle {\n\
    enabled: u32,\n\
    volume: f32,\n\
    edge1: f32,\n\
    edge2: f32,\n\
    dist: f32,\n\
    _pad0: f32,\n\
    _pad1: vec2<f32>,\n\
}\n\
@group(3) @binding(0) var scene_depth: texture_2d<f32>;\n\
@group(3) @binding(1) var<uniform> _fx_soft: FxSoftParticle;\n"
}

fn soft_particle_helpers() -> &'static str {
    "fn _fx_apply_soft_particle(col: vec4<f32>, frag_pos: vec4<f32>) -> vec4<f32> {\n\
    if (_fx_soft.enabled == 0u) {\n\
        return col;\n\
    }\n\
    let scene_z = textureLoad(scene_depth, vec2<i32>(frag_pos.xy), 0).x;\n\
    let depth_diff = scene_z - frag_pos.z;\n\
    if (depth_diff <= 0.0) {\n\
        return col;\n\
    }\n\
    let fade_dist = max(_fx_soft.dist, 1e-5);\n\
    var fade = clamp(depth_diff / fade_dist, 0.0, 1.0) * max(_fx_soft.volume, 0.0);\n\
    if (_fx_soft.edge2 > _fx_soft.edge1) {\n\
        fade = smoothstep(_fx_soft.edge1, _fx_soft.edge2, fade);\n\
    }\n\
    return vec4(col.rgb * fade, col.a * fade);\n\
}\n"
}

fn fragment_entry_param_list<'a>(wgsl: &'a str) -> Option<&'a str> {
    let frag = wgsl.find("@fragment")?;
    let fn_kw = wgsl[frag..].find("fn ")? + frag;
    let open_paren = wgsl[fn_kw..].find('(')? + fn_kw;
    let close_paren = matching_close_paren(wgsl, open_paren)?;
    Some(&wgsl[open_paren + 1..close_paren])
}

fn fragment_position_builtin_ident(wgsl: &str) -> Option<String> {
    let params = fragment_entry_param_list(wgsl)?;
    for segment in params.split(',') {
        if !segment.contains("@builtin(position)") {
            continue;
        }
        let after = segment.split("@builtin(position)").nth(1)?.trim();
        let name = after.split(':').next()?.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

fn fragment_entry_body_open(wgsl: &str) -> Option<usize> {
    let frag = wgsl.find("@fragment")?;
    let fn_kw = wgsl[frag..].find("fn ")? + frag;
    wgsl[fn_kw..].find('{').map(|i| fn_kw + i)
}

fn ensure_fragment_position_builtin(wgsl: &str) -> String {
    if wgsl.contains("_fx_frag_pos") {
        return wgsl.to_string();
    }
    let mut result = wgsl.to_string();
    append_private_var(&mut result, "_fx_frag_pos", "vec4<f32>");
    let Some(fn_start) = result.find("@fragment").and_then(|_| fragment_entry_body_open(&result)) else {
        return result;
    };
    if let Some(existing) = fragment_position_builtin_ident(&result) {
        result.insert_str(fn_start + 1, &format!("\n    _fx_frag_pos = {existing};"));
        return result;
    }
    let frag = result.find("@fragment").unwrap();
    let fn_kw = result[frag..].find("fn ").unwrap() + frag;
    let open_paren = result[fn_kw..].find('(').unwrap() + fn_kw;
    let param = "@builtin(position) _fx_frag_pos_in: vec4<f32>, ";
    result.insert_str(open_paren + 1, &param);
    result.insert_str(fn_start + 1, "\n    _fx_frag_pos = _fx_frag_pos_in;");
    result
}

/// Inject `@group(3)` mesh-depth soft-particle fade on the primary `FragmentOutput` colour.
///
/// Gated at runtime via [`FxSoftParticle::enabled`] uniform (see `particle_renderer` group 3 bind).
/// Does not use `@group(2)` so Agent 3 can keep extra texture bindings there.
pub fn inject_soft_particle_fs(wgsl: &str) -> String {
    // Soft-particle depth fade is opt-in (FX_SOFT_PARTICLE=1) until its distance/compare
    // math is capture-validated: with the live viewport's real scene depth bound it faded
    // most of the bomb smoke to a dithered remnant (harness never binds scene depth, so
    // tests could not catch it).
    if !crate::fx_env::fx_soft_particle_enabled() {
        return wgsl.to_string();
    }
    if wgsl.contains("_fx_apply_soft_particle") {
        return wgsl.to_string();
    }
    let ctor = "return FragmentOutput(";
    if !wgsl.contains(ctor) {
        return wgsl.to_string();
    }
    let mut result = ensure_fragment_position_builtin(wgsl);
    let decl_block = format!(
        "{}{}",
        soft_particle_group3_decls(),
        soft_particle_helpers()
    );
    if let Some(priv_pos) = result.find("var<private>") {
        result.insert_str(priv_pos, &decl_block);
    } else if let Some(entry) = result.find("@fragment") {
        result.insert_str(entry, &decl_block);
    } else {
        return wgsl.to_string();
    }
    let Some(ret) = result.rfind(ctor) else {
        return result;
    };
    let open = ret + ctor.len() - 1;
    let Some(close) = matching_close_paren(&result, open) else {
        return result;
    };
    let mut args = split_top_level_commas(&result[open + 1..close]);
    if args.is_empty() {
        return result;
    }
    let color = args[0].trim();
    args[0] = format!("_fx_apply_soft_particle({color}, _fx_frag_pos)");
    let new_return = format!("{ctor}{})", args.join(", "));
    result.replace_range(ret..close + 1, &new_return);
    result
}

/// Insert `discard` before the fragment return when the primary colour alpha is below `threshold`.
///
/// Used only for depth-write pipelines so billboard quads do not write depth for cutout pixels.
/// Does not affect transparent (depth-read-only) particle passes.
pub fn inject_opaque_core_alpha_test(wgsl: &str, threshold: f32) -> String {
    let ctor = "return FragmentOutput(";
    let Some(ret) = wgsl.rfind(ctor) else {
        return wgsl.to_string();
    };
    let open = ret + ctor.len() - 1;
    let Some(close) = matching_close_paren(wgsl, open) else {
        return wgsl.to_string();
    };
    let args = split_top_level_commas(&wgsl[open + 1..close]);
    if args.is_empty() {
        return wgsl.to_string();
    }
    let color_expr = args[0].trim();
    let line_start = wgsl[..ret].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = wgsl[line_start..ret]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect::<String>();
    let test = format!(
        "{indent}if ({color_expr}.a < {threshold}) {{\n{indent}    discard;\n{indent}}}\n"
    );
    let mut result = wgsl.to_string();
    result.insert_str(line_start, &test);
    result
}

/// Disable the NVN FS life gate (`pred_* = gpr_k <= cbuf_9[94].z; discard`).
///
/// Off by default: [`crate::nvn_chain::force_hybrid_billboard_cbuf_defaults`] fills slot 94
/// with a large negative `.z` so the gate stays open. Opt in with
/// `FX_NEUTRALIZE_FS_LIFE_DISCARD=1` when a shader still discards despite CPU fill.
pub fn neutralize_fs_cbuf9_life_discard(wgsl: &str) -> String {
    if !crate::fx_env::fx_neutralize_fs_life_discard_enabled()
        || !wgsl.contains("cbuf_9_1_._m0_[94]")
    {
        return wgsl.to_string();
    }
    let mut out = String::with_capacity(wgsl.len());
    let mut neutralize_next_pred = false;
    for line in wgsl.lines() {
        let trimmed = line.trim();
        if trimmed.contains("cbuf_9_1_._m0_[94]") {
            out.push_str(line);
            out.push('\n');
            neutralize_next_pred = true;
            continue;
        }
        if neutralize_next_pred
            && trimmed.starts_with("pred_")
            && trimmed.contains("<=")
        {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let pred = trimmed.split('=').next().unwrap_or("pred_0_").trim();
            out.push_str(&indent);
            out.push_str(pred);
            out.push_str(" = false;\n");
            neutralize_next_pred = false;
            continue;
        }
        neutralize_next_pred = false;
        out.push_str(line);
        out.push('\n');
    }
    if wgsl.ends_with('\n') {
        out
    } else {
        out.trim_end_matches('\n').to_string()
    }
}

/// Patch fragment WGSL to use native WGSL texture sampling with the vertex
/// colour attribute for the output colour.
///
/// Adds `@group(1)` texture+sampler, optional `@group(2)` extra textures and
/// [`FxTexBlendCoeffs`], and replaces the first `FragmentOutput` argument with a
/// cbuf_16-style blend of `in_attr1_1` (CPU-simulated per-particle colour) and
/// the emitter atlas sample — same combiner path as [`enhance_native_fragment_wgsl`].
pub fn patch_fragment_wgsl(wgsl: &str) -> String {
    inject_fragment_texture_blend(wgsl, Some("in_attr1_1"), true)
}

/// Post-process native fragment WGSL: modulate the NVN chain's primary colour output
/// (first `FragmentOutput` argument) by the emitter texture. Decoded BNSH particle FS has no
/// `textureSample`; we blend the native RGBA by a `@group(1)` sample (same layout as
/// [`patch_fragment_wgsl`]).
pub fn enhance_native_fragment_wgsl(wgsl: &str) -> String {
    enhance_native_fragment_wgsl_with_hint(wgsl, crate::shader_registry::NativeColorInput::Auto)
}

pub fn enhance_native_fragment_wgsl_with_hint(
    wgsl: &str,
    hint: crate::shader_registry::NativeColorInput,
) -> String {
    let wgsl = skip_native_fs_main1_for_vs_colour_feed(wgsl);
    let native_in = resolve_native_color_in_override(&wgsl, hint);
    inject_fragment_texture_blend(&wgsl, native_in, false)
}

/// When the VS already ran the NVN Hermite colour chain, skip the FS `main_1()` life-gate
/// path (it discards before the enhance texture pass). Requires `in_attr1_1` for colour feed.
fn skip_native_fs_main1_for_vs_colour_feed(wgsl: &str) -> String {
    if !fs_has_native_color_chain(wgsl) || !wgsl.contains("in_attr1_1") {
        return wgsl.to_string();
    }
    const MARKER: &str = "gl_FragCoord_1 = gl_FragCoord;\n    main_1();";
    if !wgsl.contains(MARKER) {
        return wgsl.to_string();
    }
    wgsl.replace(
        MARKER,
        "gl_FragCoord_1 = gl_FragCoord;\n    // VS native colour via in_attr0; skip FS main_1 discard gate\n",
    )
}

fn inject_fragment_texture_blend(
    wgsl: &str,
    native_in_override: Option<&str>,
    force_blend_uniform: bool,
) -> String {
    let mut result = wgsl.to_string();
    let extra_tex_slots = native_fs_extra_tex_slots_needed(&result);
    let blend_uniform = force_blend_uniform || native_fs_tex_blend_uniform_needed(&result);
    let needs_group2 = extra_tex_slots.iter().any(|&b| b)
        || blend_uniform
        || !result.contains("_fx_particle_alpha");
    let attr_wgsl = wgsl.to_string();

    fn append_group2_decls(
        out: &mut String,
        extra_tex_slots: [bool; 3],
        blend_uniform: bool,
        wgsl: &str,
    ) {
        if extra_tex_slots.iter().any(|&b| b) {
            out.push_str(extra_tex_group2_decls());
        }
        if blend_uniform {
            out.push_str(extra_tex_blend_uniform_decls());
        }
        if !out.contains("_fx_particle_alpha") {
            out.push_str(particle_alpha_mod_group2_decls());
        }
        if let Some(helpers) = extra_tex_cbuf16_blend_helpers(wgsl, blend_uniform) {
            out.push_str(fx_modulate_particle_tex_helpers());
            out.push_str(helpers);
        }
    }

    fn append_particle_alpha_helpers(result: &mut String, wgsl: &str) {
        if result.contains("_fx_apply_particle_alpha_modifiers") {
            return;
        }
        let mut block = String::new();
        if !result.contains("_fx_modulate_particle_tex") {
            block.push_str(fx_modulate_particle_tex_helpers());
        }
        block.push_str(&particle_alpha_mod_helpers(wgsl));
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &block);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &block);
        }
    }

    if !result.contains("_fx_distort_uv") {
        let helpers = fx_distortion_uv_helpers(&result);
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &helpers);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &helpers);
        }
    }

    if !result.contains("@group(1)") {
        let mut tex_decls = String::from("\n");
        tex_decls.push_str(emitter_tex_group1_decls());
        if needs_group2 && !result.contains("@group(2)") {
            append_group2_decls(&mut tex_decls, extra_tex_slots, blend_uniform, &result);
        }
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &tex_decls);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &tex_decls);
        }
    } else if needs_group2 && !result.contains("@group(2)") {
        let mut decls = String::new();
        append_group2_decls(&mut decls, extra_tex_slots, blend_uniform, &result);
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &decls);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &decls);
        }
    } else if needs_group2 {
        if let Some(helpers) = extra_tex_cbuf16_blend_helpers(&result, blend_uniform) {
            if !result.contains("_fx_cbuf16_blend_ch12") {
                let mut helper_block = String::new();
                if !result.contains("_fx_modulate_particle_tex") {
                    helper_block.push_str(fx_modulate_particle_tex_helpers());
                }
                helper_block.push_str(helpers);
                if let Some(priv_pos) = result.find("var<private>") {
                    result.insert_str(priv_pos, &helper_block);
                } else if let Some(entry) = result.find("@fragment") {
                    result.insert_str(entry, &helper_block);
                }
            }
        }
        if blend_uniform && !result.contains("_fx_tex_blend") {
            if let Some(priv_pos) = result.find("var<private>") {
                result.insert_str(priv_pos, extra_tex_blend_uniform_decls());
            } else if let Some(entry) = result.find("@fragment") {
                result.insert_str(entry, extra_tex_blend_uniform_decls());
            }
        }
        if !result.contains("_fx_particle_alpha") {
            if let Some(priv_pos) = result.find("var<private>") {
                result.insert_str(priv_pos, particle_alpha_mod_group2_decls());
            } else if let Some(entry) = result.find("@fragment") {
                result.insert_str(entry, particle_alpha_mod_group2_decls());
            }
        }
    }

    append_particle_alpha_helpers(&mut result, &attr_wgsl);

    if !result.contains("_fx_modulate_particle_tex") {
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, fx_modulate_particle_tex_helpers());
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, fx_modulate_particle_tex_helpers());
        }
    }

    let uv_expr = primary_atlas_uv_expr(&result);
    let crossfade_expr = if result.contains("in_attr10_1") {
        Some((
            "in_attr10_1.x".to_string(),
            format!("({uv_expr} + in_attr10_1.yz)"),
        ))
    } else if result.contains("cbuf_9_1_") && result.contains("_m0_[9]") {
        // Next-frame UV for a sequential grid flipbook: advance one column (su =
        // cbuf_9[97].x), wrapping to the next row (sv = cbuf_9[97].y) at the atlas edge.
        // Sparse / non-sequential pattern tables use the CPU attr10 path above instead.
        Some((
            "cbuf_9_1_._m0_[9].x".to_string(),
            format!(
                "(select({uv_expr} + vec2<f32>(cbuf_9_1_._m0_[125].x, 0.0), \
                 vec2<f32>(({uv_expr}).x + cbuf_9_1_._m0_[125].x - 1.0, ({uv_expr}).y + cbuf_9_1_._m0_[125].y), \
                 ({uv_expr}).x + cbuf_9_1_._m0_[125].x >= 1.0))"
            ),
        ))
    } else {
        None
    };

    let ctor = "return FragmentOutput(";
    let Some(ret) = result.rfind(ctor) else {
        return result;
    };
    let open = ret + ctor.len() - 1;
    let Some(close) = matching_close_paren(&result, open) else {
        return result;
    };
    let mut args = split_top_level_commas(&result[open + 1..close]);
    if args.is_empty() {
        return result;
    }
    let line_start = result[..ret].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = result[line_start..ret]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect::<String>();
    let native_in = native_in_override
        .map(str::to_string)
        .unwrap_or_else(|| args[0].trim().to_string());
    let extra_prelude = extra_tex_sample_prelude(&result, &indent, "_fx_uv0", extra_tex_slots);
    let group1_prelude = group1_combiner_tex_sample_prelude(&result, &indent, "_fx_uv0");
    let group1_modulate = modulate_native_col_with_group1_combiner_tex(&indent, &result);
    let extra_modulate = if extra_tex_slots.iter().any(|&b| b) {
        modulate_native_col_with_extra_tex(&indent, extra_tex_slots, &result)
    } else {
        String::new()
    };
    let fs_chain_modulate = native_in_override.is_none() && fs_has_native_color_chain(&result);
    let native_in_decl = if native_in_override == Some("in_attr1_1") {
        format!("{indent}let _fx_native_in = in_attr1_1;\n")
    } else if fs_chain_modulate {
        // The game's FS computes its final per-particle colour into its return value
        // (e.g. `frag_color0_`) via the cbuf colour chain. Modulate the re-added texture
        // by THAT computed colour — not the raw `in_attr1_1` varying, which is only the
        // chain's input and is often left white, washing the effect out. (Game-accurate:
        // the game outputs the chain result, so the texture multiplies against it.)
        format!("{indent}let _fx_native_in = {native_in};\n")
    } else if result.contains("in_attr1_1") {
        format!(
            "{indent}var _fx_native_in = in_attr1_1;\n\
             {indent}let _fx_native_chain = {native_in};\n\
             {indent}if (dot(_fx_native_chain.rgb, vec3<f32>(1.0)) > 0.001 && _fx_native_chain.a > 0.001) {{\n\
             {indent}    _fx_native_in = _fx_native_chain;\n\
             {indent}}}\n"
        )
    } else {
        format!("{indent}let _fx_native_in = {native_in};\n")
    };
    let primary_blend = blend_primary_color_tex(&indent, &result);
    let texture_sample = if let Some((blend, uv_next)) = &crossfade_expr {
        format!(
            "{native_in_decl}\
             {indent}let _fx_blend = {blend};\n\
             {indent}let _fx_uv0 = _fx_distort_uv({uv_expr});\n\
             {indent}let _fx_uv1 = _fx_distort_uv({uv_next});\n\
             {indent}let _fx_ts0 = textureSample(color_tex, color_sampler, _fx_uv0);\n\
             {indent}let _fx_ts1 = textureSample(color_tex, color_sampler, _fx_uv1);\n\
             {indent}let _fx_ts = mix(_fx_ts0, _fx_ts1, _fx_blend);\n\
             {primary_blend}{indent}var _fx_native_col = _fx_native_col_base;\n\
             {group1_prelude}{group1_modulate}{extra_prelude}{extra_modulate}{indent}_fx_native_col = _fx_apply_particle_alpha_modifiers(_fx_native_col);\n"
        )
    } else {
        format!(
            "{native_in_decl}\
             {indent}let _fx_uv0 = _fx_distort_uv({uv_expr});\n\
             {indent}let _fx_ts = textureSample(color_tex, color_sampler, _fx_uv0);\n\
             {primary_blend}{indent}var _fx_native_col = _fx_native_col_base;\n\
             {group1_prelude}{group1_modulate}{extra_prelude}{extra_modulate}{indent}_fx_native_col = _fx_apply_particle_alpha_modifiers(_fx_native_col);\n"
        )
    };
    let prelude = texture_sample;
    args[0] = "_fx_native_col".to_string();
    let new_return = format!("{ctor}{})", args.join(", "));
    result.insert_str(line_start, &prelude);
    let ret2 = result.rfind(ctor).expect("return FragmentOutput vanished after prelude insert");
    let open2 = ret2 + ctor.len() - 1;
    let close2 = matching_close_paren(&result, open2).expect("FragmentOutput paren");
    result.replace_range(ret2..close2 + 1, &new_return);
    result
}

#[cfg(test)]
pub(crate) fn with_test_env<F: FnOnce()>(key: &str, value: &str, f: F) {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Mutex;
    static FX_TEST_ENV_LOCK: Mutex<()> = Mutex::new(());
    let _lock = FX_TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let prev = std::env::var(key).ok();
    std::env::set_var(key, value);
    let result = catch_unwind(AssertUnwindSafe(f));
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;
    use super::with_test_env;

    #[test]
    fn merge_stage_pipeline_descriptors_keeps_vs_storage_over_fs_storage() {
        let vs = vec![
            DescriptorInfo {
                set: 0,
                binding: 0,
                name: "cbuf_1".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 1,
                name: "cbuf_8".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let fs = vec![
            DescriptorInfo {
                set: 0,
                binding: 0,
                name: "cbuf_9".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 1,
                name: "cbuf_16".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let merged = merge_stage_pipeline_descriptors(&vs, &fs);
        assert!(merged.iter().any(|d| d.name == "cbuf_8" && d.binding == 1));
        assert!(!merged.iter().any(|d| d.name == "cbuf_16" && d.binding == 1));
    }

    #[test]
    fn remap_fs_storage_bindings_moves_conflicts_to_free_slots() {
        let vs = vec![
            DescriptorInfo {
                set: 0,
                binding: 0,
                name: "cbuf_1".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 1,
                name: "cbuf_8".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let fs = vec![
            DescriptorInfo {
                set: 0,
                binding: 0,
                name: "cbuf_9".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 1,
                name: "cbuf_16".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let fs_wgsl = "\
@group(0) @binding(0)
var<storage> cbuf_9_1_: cbuf_9_;
@group(0) @binding(1)
var<storage> cbuf_16_1_: cbuf_16_;
fn main_1() {
    let _ = cbuf_9_1_._m0_[0];
}
";
        let (out, remapped) = remap_fs_storage_bindings_for_vs(fs_wgsl, &vs, &fs);
        assert!(out.contains("@binding(5)") || out.contains("@binding(2)"));
        assert!(remapped.iter().all(|d| d.binding >= 2));
        let merged = merge_stage_pipeline_descriptors(&vs, &remapped);
        assert!(merged.iter().any(|d| d.name == "cbuf_8" && d.binding == 1));
        assert!(merged.iter().any(|d| d.name == "cbuf_9"));
        assert!(merged.iter().any(|d| d.name == "cbuf_16"));
    }

    #[test]
    fn merge_stage_pipeline_descriptors_prefers_vs_storage_over_fs_texture() {
        let vs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "cbuf_9".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "cbuf_10".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let fs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "texture_0_".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "sampler_0_".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let merged = merge_stage_pipeline_descriptors(&vs, &fs);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(|d| d.class == BindingClass::Storage));
    }

    #[test]
    fn strip_fs_wgsl_removes_texture_not_storage_at_vs_storage_slots() {
        let fs = "\
@group(0) @binding(0)
var<storage> cbuf_9_1_: cbuf_9_;
@group(0) @binding(2)
var texture_0_: texture_2d<f32>;
@group(0) @binding(3)
var sampler_0_: sampler;
fn main() {
    let _e62 = textureSample(texture_0_, sampler_0_, vec2<f32>(0.0, 0.0));
}
";
        let vs_descs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "cbuf_9".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "cbuf_10".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        let fs_descs = vec![
            DescriptorInfo {
                set: 0,
                binding: 0,
                name: "cbuf_9_1_".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "texture_0_".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "sampler_0_".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let out = strip_fs_wgsl_conflicting_with_vs(fs, &vs_descs, &fs_descs);
        assert!(out.contains("cbuf_9_1_"));
        assert!(!out.contains("texture_0_"));
        assert!(!out.contains("sampler_0_"));
        assert!(out.contains("textureSample(color_tex, color_sampler,"));
    }

    #[test]
    fn strip_fs_wgsl_handles_split_attribute_lines() {
        let fs = "\
@group(0) @binding(2)
var texture_0_: texture_2d<f32>;
";
        let vs_descs = vec![DescriptorInfo {
            set: 0,
            binding: 2,
            name: "cbuf_9".into(),
            ty_str: "Struct".into(),
            class: BindingClass::Storage,
        }];
        let fs_descs = vec![DescriptorInfo {
            set: 0,
            binding: 2,
            name: "texture_0_".into(),
            ty_str: "Image".into(),
            class: BindingClass::Texture,
        }];
        let out = strip_fs_wgsl_conflicting_with_vs(fs, &vs_descs, &fs_descs);
        assert!(!out.contains("texture_0_"));
    }

    #[test]
    fn strip_redirects_wgsl_var_names_when_fs_desc_names_differ() {
        let fs = "\
@group(0) @binding(2)
var texture_0_: texture_2d<f32>;
@group(0) @binding(3)
var sampler_0_: sampler;
@group(1) @binding(0) var color_tex: texture_2d<f32>;
fn main() {
    let _e62 = textureSample(texture_0_, sampler_0_, vec2<f32>(0.0, 0.0));
}
";
        let vs_descs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "cbuf_9".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "cbuf_10".into(),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            },
        ];
        // Reflection name does not match spirv-cross WGSL identifier.
        let fs_descs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "var_0_2".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "var_0_3".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let out = strip_fs_wgsl_conflicting_with_vs(fs, &vs_descs, &fs_descs);
        assert!(!out.contains("texture_0_"));
        assert!(out.contains("textureSample(color_tex, color_sampler,"));
    }

    #[test]
    fn native_enhance_strip_and_validate_bomb_like_fs() {
        let fs = "\
struct cbuf_9_ { _m0_: array<vec4<f32>, 4096>, }
struct cbuf_16_ { _m0_: array<vec4<f32>, 4096>, }
struct FragmentOutput { @location(0) frag_color0_: vec4<f32>, }
@group(0) @binding(0)
var<storage> cbuf_9_1_: cbuf_9_;
@group(0) @binding(1)
var<storage> cbuf_16_1_: cbuf_16_;
@group(0) @binding(2)
var texture_0_: texture_2d<f32>;
@group(0) @binding(3)
var sampler_0_: sampler;
var<private> in_attr2_1: vec4<f32>;
var<private> gl_FragCoord_1: vec4<f32>;
var<private> gpr_0_: f32;
var<private> gpr_1_: f32;
var<private> gpr_2_: f32;
var<private> gpr_256_: f32;
var<private> gpr_257_: f32;
var<private> gpr_258_: f32;
var<private> frag_color0_: vec4<f32>;
fn main_1() {
    let _e62 = textureSample(texture_0_, sampler_0_, vec2<f32>(gpr_0_, gpr_1_));
    gpr_256_ = _e62.x;
    frag_color0_ = vec4<f32>(gpr_256_, gpr_257_, gpr_258_, 1.0);
}
@fragment
fn main(@location(2) in_attr2_: vec4<f32>, @builtin(position) gl_FragCoord: vec4<f32>) -> FragmentOutput {
    in_attr2_1 = in_attr2_;
    gl_FragCoord_1 = gl_FragCoord;
    main_1();
    return FragmentOutput(frag_color0_);
}
";
        let vs_descs: Vec<DescriptorInfo> = (0..5)
            .map(|b| DescriptorInfo {
                set: 0,
                binding: b,
                name: format!("cbuf_{b}"),
                ty_str: "Struct".into(),
                class: BindingClass::Storage,
            })
            .collect();
        let fs_descs = vec![
            DescriptorInfo {
                set: 0,
                binding: 2,
                name: "var_0_2".into(),
                ty_str: "Image".into(),
                class: BindingClass::Texture,
            },
            DescriptorInfo {
                set: 0,
                binding: 3,
                name: "var_0_3".into(),
                ty_str: "Sampler".into(),
                class: BindingClass::Sampler,
            },
        ];
        let enhanced = enhance_native_fragment_wgsl(fs);
        let stripped = strip_fs_wgsl_conflicting_with_vs(&enhanced, &vs_descs, &fs_descs);
        validate_wgsl_shader(&stripped, "bomb_like_fs").expect("stripped native FS must parse");
        assert!(!stripped.contains("texture_0_"));
        assert!(stripped.contains("color_tex"));
    }

    #[test]
    fn samus_export_default_fs_validates_after_native_strip() {
        let Some(eff_path) = crate::scratch_dirs::resolve_fighter_eff("samus") else {
            eprintln!("skip samus_export_default_fs_validates_after_native_strip: eff missing");
            return;
        };
        let Ok(eff) = crate::effects::EffIndex::from_file(&eff_path) else {
            return;
        };
        let Ok(ptcl) = crate::effects::PtclFile::parse(&eff.ptcl_data) else {
            return;
        };
        let legacy = crate::bnsh_shader_integration::decode_legacy_stage_pair(&ptcl);
        let (mut pairs, _) = crate::bnsh_shader_integration::decode_effect_export_shaders("samus");
        crate::bnsh_shader_integration::finalize_shader_pairs(&mut pairs, &legacy);
        let key = 0xaea4749ba63852u64;
        let Some(pair) = pairs.get(&key) else {
            eprintln!("skip: registry key {key:#x} missing from export");
            return;
        };
        let vs_info = pair.vertex.as_ref().expect("vs");
        let fs_info = pair.fragment.as_ref().expect("fs");
        let mut vs_w = crate::spirv_to_wgsl::bytes_to_words(&vs_info.spirv).unwrap();
        let mut fs_w = crate::spirv_to_wgsl::bytes_to_words(&fs_info.spirv).unwrap();
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
        let _ = crate::spirv_patch::nvn_remap_vertex_input_locations(&mut vs_w);
        let to_bytes = |w: &[u32]| w.iter().flat_map(|&x| x.to_le_bytes()).collect::<Vec<u8>>();
        let (vs_wgsl, vs_descs) = crate::spirv_to_wgsl::spirv_to_wgsl(
            &to_bytes(&vs_w),
            naga::ShaderStage::Vertex,
            "test_samus_vs",
        )
        .unwrap();
        let (fs_wgsl, fs_descs) = crate::spirv_to_wgsl::spirv_to_wgsl(
            &to_bytes(&fs_w),
            naga::ShaderStage::Fragment,
            "test_samus_fs",
        )
        .unwrap();
        let prepared = crate::particle_renderer_bnsh::prepare_bnsh_wgsl(
            &vs_wgsl,
            &fs_wgsl,
            None,
            Some(&to_bytes(&vs_w)),
            Some(&to_bytes(&fs_w)),
            crate::shader_registry::NativeColorInput::Auto,
        );
        let stripped = strip_fs_wgsl_conflicting_with_vs(
            &prepared.fs_wgsl,
            &vs_descs,
            &fs_descs,
        );
        validate_wgsl_shader(&stripped, "samus_default_fs").expect("samus default FS");
        assert!(
            !stripped.contains("texture_0_"),
            "stripped FS must not reference set-0 texture globals"
        );
    }

    /// Minimal spirv-cross-style bomb VS/FS pair: FS declares `@location(6/7)` varyings
    /// that the decoded VS omits from `VertexOutput` and `return VertexOutput(...)`.
    const BOMB_LINK_VS: &str = "\
struct VertexOutput {
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
    @location(2) out_attr2_: vec4<f32>,
    @location(3) out_attr3_: vec4<f32>,
    @location(4) out_attr4_: vec4<f32>,
    @location(5) out_attr5_: vec4<f32>,
    @builtin(position) gl_Position: vec4<f32>,
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr1_1: vec4<f32>;
var<private> in_attr3_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr5_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> in_attr7_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr1_: vec4<f32>;
var<private> out_attr2_: vec4<f32>;
var<private> out_attr3_: vec4<f32>;
var<private> out_attr4_: vec4<f32>;
var<private> out_attr5_: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {}
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(1) in_attr1_: vec4<f32>, \
@location(3) in_attr3_: vec4<f32>, @location(4) in_attr4_: vec4<f32>, \
@location(5) in_attr5_: vec4<f32>, @location(6) in_attr6_: vec4<f32>, \
@location(7) in_attr7_: vec4<f32>) -> VertexOutput {
    in_attr0_1 = in_attr0_;
    in_attr1_1 = in_attr1_;
    in_attr3_1 = in_attr3_;
    in_attr4_1 = in_attr4_;
    in_attr5_1 = in_attr5_;
    in_attr6_1 = in_attr6_;
    in_attr7_1 = in_attr7_;
    main_1();
    let _e239 = out_attr0_;
    let _e241 = out_attr1_;
    let _e243 = out_attr2_;
    let _e245 = out_attr3_;
    let _e247 = out_attr4_;
    let _e249 = out_attr5_;
    let _e251 = gl_Position;
    return VertexOutput(_e239, _e241, _e243, _e245, _e247, _e249, _e251);
}
";

    const BOMB_FS: &str = "\
struct FragmentOutput {
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
    @location(2) out_attr2_: vec4<f32>,
    @location(3) out_attr3_: vec4<f32>,
    @location(4) out_attr4_: vec4<f32>,
    @location(5) out_attr5_: vec4<f32>,
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr1_1: vec4<f32>;
var<private> in_attr3_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr5_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> in_attr7_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr1_: vec4<f32>;
var<private> out_attr2_: vec4<f32>;
var<private> out_attr3_: vec4<f32>;
var<private> out_attr4_: vec4<f32>;
var<private> out_attr5_: vec4<f32>;
fn main_1() {}
@fragment
fn main(@location(0) in_attr0_: vec4<f32>, @location(1) in_attr1_: vec4<f32>, \
@location(3) in_attr3_: vec4<f32>, @location(4) in_attr4_: vec4<f32>, \
@location(5) in_attr5_: vec4<f32>, @location(6) in_attr6_: vec4<f32>, \
@location(7) in_attr7_: vec4<f32>) -> FragmentOutput {
    in_attr0_1 = in_attr0_;
    in_attr1_1 = in_attr1_;
    in_attr3_1 = in_attr3_;
    in_attr4_1 = in_attr4_;
    in_attr5_1 = in_attr5_;
    in_attr6_1 = in_attr6_;
    in_attr7_1 = in_attr7_;
    main_1();
    let _e239 = out_attr0_;
    let _e241 = out_attr1_;
    let _e243 = out_attr2_;
    let _e245 = out_attr3_;
    let _e247 = out_attr4_;
    let _e249 = out_attr5_;
    return FragmentOutput(_e239, _e241, _e243, _e245, _e247, _e249);
}
";

    #[test]
    fn patch_real_bomb_fixture_preserves_native_colour_and_links_attr5() {
        use crate::bnsh_shader_integration::{decode_effect_export_shaders, BOMB_SHADER_KEY};
        let (pairs, _) = decode_effect_export_shaders("samus");
        let Some(pair) = pairs.get(&BOMB_SHADER_KEY) else {
            return;
        };
        let vs = pair.vertex.as_ref().expect("vs");
        let fs = pair.fragment.as_ref().expect("fs");
        let mut vs_w = bytes_to_words(&vs.spirv).unwrap();
        let mut fs_w = bytes_to_words(&fs.spirv).unwrap();
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
        let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
        let _ = crate::spirv_patch::nvn_remap_vertex_input_locations(&mut vs_w);
        let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let (vs_wgsl, _) =
            spirv_to_wgsl(&to_bytes(&vs_w), naga::ShaderStage::Vertex, "bomb_vs").unwrap();
        let (fs_wgsl, _) =
            spirv_to_wgsl(&to_bytes(&fs_w), naga::ShaderStage::Fragment, "bomb_fs").unwrap();
        let patched = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
        let fs_locs: Vec<u32> = fragment_io_fields(&fs_wgsl).iter().map(|f| f.location).collect();
        assert_eq!(fs_locs, vec![0, 1, 2, 3, 4, 5]);
        assert!(
            patched.contains("@location(5) out_attr5_"),
            "bomb VS must output location 5 for bomb FS"
        );
        assert!(vs_has_native_color_chain(&vs_wgsl));
        assert!(!patched.contains("out_attr0_ = in_attr1_1"));
        assert!(!patched.contains("out_attr1_ = in_attr1_1"));
        assert!(
            patched.contains("out_attr2_ = in_attr2_1"),
            "bomb VS must forward CPU quad UV when main_1() never reads in_attr2_1"
        );
        assert!(
            patched.contains("_world = in_attr0_1.xyz"),
            "Family-A bomb VS must finalize clip position while keeping native colour varyings"
        );
    }

    #[test]
    fn vs_has_native_color_chain_detects_hermite_slots() {
        let vs = "main_1(); cbuf_9_1_._m0_[60] cbuf_8_1_._m0_[6] out_attr0_.x out_attr1_.y";
        assert!(vs_has_native_color_chain(vs));
        assert!(!vs_has_native_color_chain("main_1(); in_attr1_1 out_attr0_"));
    }

    #[test]
    fn preserve_native_vs_skips_cpu_attr2_passthrough() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr2_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr2_: vec4<f32>;
fn main_1() {
    let _ = cbuf_9_1_._m0_[60];
    out_attr2_.x = 1.0;
}
@vertex
fn main(@location(2) in_attr2_: vec4<f32>) -> VertexOutput {
    in_attr2_1 = in_attr2_;
    main_1();
    let _e240 = out_attr2_;
    return VertexOutput(out_attr0_, _e240, gl_Position);
}
var<private> gl_Position: vec4<f32>;
";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let out = patch_vertex_wgsl(vs, "@fragment\nfn main(@location(2) in_attr2_: vec4<f32>) {}");
            assert!(
                out.contains("out_attr2_ = in_attr2_1"),
                "native VS that writes out_attr2 in main_1 without reading in_attr2_1 still needs CPU UV"
            );
            assert!(!out.contains("out_attr0_ = in_attr1_1"));
        });
    }

    #[test]
    fn legacy_vs_pos_override_still_forwards_cpu_colour() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr1_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr1_: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {
    let _ = cbuf_9_1_._m0_[60];
    let _ = cbuf_8_1_._m0_[8];
    let _ = cbuf_9_1_._m0_[46];
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(out_attr0_, out_attr1_, gl_Position);
}
fn _ref() { let _ = cbuf_8_1_._m0_[8]; }
";
        with_test_env("FX_NATIVE_VS_POS", "0", || {
            let out = override_billboard_position(vs);
            assert!(out.contains("out_attr0_ = in_attr1_1"));
            assert!(out.contains("out_attr1_ = in_attr1_1"));
        });
    }

    #[test]
    fn native_default_family_a_finalizes_clip_without_colour_clobber() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr1_: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {
    let _ = cbuf_9_1_._m0_[60];
    let _ = cbuf_8_1_._m0_[8];
    let _ = cbuf_9_1_._m0_[46];
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(out_attr0_, out_attr1_, gl_Position);
}
";
        let fs = "@fragment\nfn main(@location(0) in_attr0_: vec4<f32>) -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        let patched = patch_vertex_wgsl(vs, fs);
        assert!(!patched.contains("out_attr0_ = in_attr1_1"));
        assert!(!patched.contains("out_attr1_ = in_attr1_1"));
        assert!(patched.contains("_world = in_attr0_1.xyz"));
    }

    #[test]
    fn wire_vertex_simulation_forwards_only_attr10_11_12() {
        let vs = "\
struct VertexOutput {
    @builtin(position) gl_Position: vec4<f32>,
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
    @location(2) out_attr2_: vec4<f32>,
    @location(10) out_attr10_: vec4<f32>,
    @location(11) out_attr11_: vec4<f32>,
    @location(12) out_attr12_: vec4<f32>,
}
@vertex
fn main(
    @location(0) in_attr0_: vec4<f32>,
    @location(1) in_attr1_: vec4<f32>,
    @location(2) in_attr2_: vec4<f32>,
    @location(10) in_attr10_: vec4<f32>,
    @location(11) in_attr11_: vec4<f32>,
    @location(12) in_attr12_: vec4<f32>,
) -> VertexOutput {
    var in_attr0_1: vec4<f32>;
    var in_attr1_1: vec4<f32>;
    var in_attr2_1: vec4<f32>;
    var in_attr10_1: vec4<f32>;
    var in_attr11_1: vec4<f32>;
    var in_attr12_1: vec4<f32>;
    var out_attr0_: vec4<f32>;
    var out_attr1_: vec4<f32>;
    var out_attr2_: vec4<f32>;
    var out_attr10_: vec4<f32>;
    var out_attr11_: vec4<f32>;
    var out_attr12_: vec4<f32>;
    in_attr0_1 = in_attr0_;
    in_attr1_1 = in_attr1_;
    in_attr2_1 = in_attr2_;
    in_attr10_1 = in_attr10_;
    in_attr11_1 = in_attr11_;
    in_attr12_1 = in_attr12_;
    main_1();
    return VertexOutput(gl_Position, out_attr0_, out_attr1_, out_attr2_, out_attr10_, out_attr11_, out_attr12_);
}
";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let out = wire_vertex_simulation_varyings(vs);
            assert!(out.contains("out_attr10_ = in_attr10_1"));
            assert!(out.contains("out_attr11_ = in_attr11_1"));
            assert!(out.contains("out_attr12_ = in_attr12_1"));
            assert!(!out.contains("out_attr0_ = in_attr0_1"));
            assert!(!out.contains("out_attr1_ = in_attr1_1"));
            assert!(!out.contains("out_attr2_ = in_attr2_1"));
        });
    }

    #[test]
    fn detect_life_attr_roles_bomb_and_impactflash_families() {
        // bomb_base1 family: birth = attr4.w, lifetime = trunc(attr3.w)
        let bomb = "\
    let _e99 = in_attr4_1;
    gpr_0_ = _e99.w;
    let _e101 = gpr_0_;
    let _e105 = cbuf_10_1_._m0_[2];
    pred_1_ = ((_e101 > _e105.x) && true);
    let _e158 = in_attr3_1;
    gpr_2_ = _e158.w;
    let _e160 = gpr_2_;
    gpr_1_ = bitcast<f32>(u32(i32(trunc(_e160))));
";
        assert_eq!(detect_life_attr_roles(bomb), Some((4, 3)));
        // impactflash2 family: birth = attr5.w, lifetime = trunc(attr4.w)
        let flash = "\
    let _e103 = in_attr5_1;
    gpr_0_ = _e103.w;
    let _e109 = cbuf_10_1_._m0_[2];
    pred_1_ = ((_e105 > _e109.x) && true);
    let _e162 = in_attr4_1;
    gpr_2_ = _e162.w;
    let _e164 = gpr_2_;
    gpr_1_ = bitcast<f32>(u32(i32(trunc(_e164))));
";
        assert_eq!(detect_life_attr_roles(flash), Some((5, 4)));
    }

    #[test]
    fn wire_vertex_forwards_attr10_for_native_crossfade() {
        let vs = "\
struct VertexOutput {
    @builtin(position) gl_Position: vec4<f32>,
    @location(10) out_attr10_: vec4<f32>,
}
@vertex
fn main(@location(10) in_attr10_: vec4<f32>) -> VertexOutput {
    var in_attr10_1: vec4<f32>;
    var out_attr10_: vec4<f32>;
    in_attr10_1 = in_attr10_;
    main_1();
    return VertexOutput(gl_Position, out_attr10_);
}
";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let out = wire_vertex_simulation_varyings(vs);
            assert!(out.contains("out_attr10_ = in_attr10_1"));
        });
    }

    #[test]
    fn wire_crossfade_injects_fs_attr10_for_native_path() {
        let vs = "\
@vertex
fn main(@location(10) in_attr10_: vec4<f32>) -> VertexOutput {
    var in_attr10_1: vec4<f32>;
    in_attr10_1 = in_attr10_;
    return VertexOutput(gl_Position);
}
";
        let fs = "\
@fragment
fn main(@location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr5_1: vec4<f32>;
    in_attr5_1 = in_attr5_;
    return FragmentOutput(vec4<f32>(1.0));
}
";
        let vs = wire_vertex_simulation_varyings(vs);
        let fs = wire_crossfade_fragment_input(&fs, &vs);
        assert!(fs.contains("@location(10) in_attr10_"));
        assert!(fs.contains("in_attr10_1 = in_attr10_"));
        let enhanced = enhance_native_fragment_wgsl(&fs);
        assert!(enhanced.contains("mix(_fx_ts0, _fx_ts1, _fx_blend)"));
        assert!(enhanced.contains("in_attr10_1.x"));
    }

    #[test]
    fn enhance_native_fragment_crossfade_via_cbuf9_when_no_attr10() {
        let fs = "\
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
@fragment
fn main(@location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr5_1: vec4<f32>;
    in_attr5_1 = in_attr5_;
    let _blend = cbuf_9_1_._m0_[9].x;
    let _e0 = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    return FragmentOutput(_e0);
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("mix(_fx_ts0, _fx_ts1, _fx_blend)"));
        assert!(out.contains("cbuf_9_1_._m0_[9].x"));
        assert!(!out.contains("in_attr10_1"));
    }

    #[test]
    fn wire_native_attr_passthrough_creates_missing_attr10() {
        let vs = "\
@vertex
fn main(@location(0) in_attr0_: vec4<f32>) -> VertexOutput {
    var in_attr0_1: vec4<f32>;
    in_attr0_1 = in_attr0_;
    return VertexOutput(gl_Position);
}
";
        let mut out = vs.to_string();
        wire_native_attr_passthrough(&mut out, 10, "in_attr10_", "in_attr10_1", "out_attr10_");
        assert!(out.contains("@location(10) in_attr10_"));
        assert!(out.contains("var<private> in_attr10_1:"));
        assert!(out.contains("out_attr10_ = in_attr10_1"));
    }

    #[test]
    fn family_a_billboard_vs_gets_finalize_clip_position_when_native_vs_enabled() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {
    let _vp = cbuf_8_1_._m0_[8];
    let _basis = cbuf_9_1_._m0_[46];
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(gl_Position);
}
";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let patched = patch_vertex_wgsl(vs, fs);
            assert!(patched.contains("_world = in_attr0_1.xyz"));
        });
    }

    #[test]
    fn infer_native_color_prefers_fs_chain_when_cbuf9_tables_and_attr1() {
        let fs = "main_1(); cbuf_9_1_._m0_[60] in_attr1_1 frag_color0_";
        assert_eq!(
            infer_native_color_from_fs_wgsl(fs),
            crate::shader_registry::NativeColorInput::FsChain,
        );
    }

    #[test]
    fn infer_native_color_detects_cbuf16_frag_color_chain() {
        let fs = "cbuf_16_1_._m0_[0] cbuf_16_1_._m0_[2] cbuf_16_1_._m0_[4] frag_color0_ in_attr1_1";
        assert_eq!(
            infer_native_color_from_fs_wgsl(fs),
            crate::shader_registry::NativeColorInput::FsChain,
        );
    }

    #[test]
    fn family_a_billboard_vs_with_cbuf9_tex_dims_slot_is_not_mesh() {
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] cbuf_9_1_._m0_[17] gl_Position";
        assert!(!is_mesh_model_vs(vs, None));
        assert!(billboard_particle_vs(vs));
    }

    #[test]
    fn family_a_billboard_vs_without_cbuf9_basis_slot_is_detected() {
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] gl_Position";
        assert!(billboard_particle_vs(vs));
        assert!(!billboard_particle_vs(
            "main_1(); in_attr0_1 in_attr4_1 cbuf_9_1_ cbuf_9_1_._m0_[0] gl_Position"
        ));
    }

    #[test]
    fn hybrid_mode_overrides_trusted_family_a_billboard_vs() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {
    let _vp = cbuf_8_1_._m0_[8];
    let _basis = cbuf_9_1_._m0_[46];
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(gl_Position);
}
";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        assert!(!trusts_native_position_chain(vs));
        with_test_env("FX_NATIVE_VS_POS", "0", || {
            let patched = patch_vertex_wgsl(vs, fs);
            assert!(patched.contains("_world = in_attr0_1.xyz"));
        });
    }

    #[test]
    fn native_default_finalizes_family_a_clip_position() {
        let vs = "\
var<private> cbuf_8_1_: array<vec4<f32>, 128>;
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {
    let _vp = cbuf_8_1_._m0_[8];
    let _basis = cbuf_9_1_._m0_[46];
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(gl_Position);
}
";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        let patched = patch_vertex_wgsl(vs, fs);
        assert!(patched.contains("_world = in_attr0_1.xyz"));
    }

    #[test]
    fn trusts_native_position_chain_rejects_family_a() {
        let wgsl = "main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] cbuf_9_1_._m0_[46] gl_Position";
        assert!(!trusts_native_position_chain(wgsl));
        assert!(!trusts_native_position_chain("main_1(); in_attr0_1 gl_Position"));
    }

    #[test]
    fn trusts_native_position_chain_detects_family_b() {
        let wgsl = "main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] cbuf_9_1_._m0_[46] gl_Position";
        assert!(trusts_native_position_chain(wgsl));
        assert!(!trusts_native_position_chain(
            "main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] cbuf_9_1_._m0_[0] gl_Position"
        ));
    }

    #[test]
    fn hybrid_finalize_skips_mesh_vs_with_billboard_like_attrs() {
        let vs = "\
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_8_1_: cbuf_8_;
var<storage> cbuf_9_1_: cbuf_9_;
fn main_1() {
    let _ = cbuf_8_1_._m0_[8];
    let _ = cbuf_9_1_._m0_[17].xy;
    let _ = cbuf_8_1_._m0_[17].x;
}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(gl_Position);
}
";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            assert!(!billboard_particle_vs(vs));
            let patched = patch_vertex_wgsl(vs, fs);
            assert!(!patched.contains("_world = in_attr0_1.xyz"));
        });
    }

    #[test]
    fn hybrid_finalize_skips_non_billboard_vs() {
        let vs = "\
var<private> in_attr0_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
fn main_1() {}
@vertex
fn main() -> VertexOutput {
    main_1();
    return VertexOutput(gl_Position);
}
";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let patched = patch_vertex_wgsl(vs, fs);
            assert!(!patched.contains("_world = in_attr0_1.xyz"));
        });
    }

    #[test]
    fn trusts_native_position_chain_detects_partial_family_b() {
        let partial = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] gl_Position";
        assert!(is_partial_family_b_billboard_vs(partial));
        assert!(!trusts_native_position_chain(partial));

        let full = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] cbuf_9_1_._m0_[46] gl_Position";
        assert!(!is_partial_family_b_billboard_vs(full));
        assert!(trusts_native_position_chain(full));
    }

    #[test]
    fn finalize_native_vs_clip_position_uses_cbuf9_vp_for_family_b() {
        let trusted = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] cbuf_9_1_._m0_[46] gl_Position";
        assert!(trusts_native_position_chain(trusted));

        // Partial Family-B billboard VS (VP in cbuf_9, no basis slot) gets hybrid finalize.
        let vs = "\
fn main_1() { gl_Position = vec4(0.0); }
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(4) in_attr4_: vec4<f32>, \
@location(6) in_attr6_: vec4<f32>) -> VertexOutput {
    in_attr0_1 = in_attr0_;
    in_attr4_1 = in_attr4_;
    in_attr6_1 = in_attr6_;
    main_1();
    return VertexOutput(gl_Position);
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_9_1_: cbuf_9_;
fn _ref() { let _ = cbuf_9_1_._m0_[0]; }
";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            let out = finalize_native_vs_clip_position(vs);
            assert!(out.contains("cbuf_9_1_._m0_[0]"));
            assert!(out.contains("_world = in_attr0_1.xyz"));
            assert!(
                !out.contains("cbuf_9_1_._m0_[120].xyz"),
                "partial Family-B finalize must derive basis from VP, not cbuf_9[46]"
            );
        });
    }

    #[test]
    fn hybrid_finalize_skips_mesh_vs_with_registry_hint() {
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] cbuf_9_1_._m0_[46] gl_Position";
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        with_test_env("FX_NATIVE_VS_POS", "1", || {
            assert!(billboard_particle_vs(vs));
            assert!(!billboard_particle_vs_with_hint(
                vs,
                Some(crate::shader_registry::ShaderVsProfile::MeshModel),
            ));
            let patched = patch_vertex_wgsl_with_hint(
                vs,
                fs,
                Some(crate::shader_registry::ShaderVsProfile::MeshModel),
            );
            assert!(!patched.contains("_world = in_attr0_1.xyz"));
        });
    }

    #[test]
    fn should_use_native_fs_when_color_tables_present_even_if_env_off() {
        let fs = "\
@fragment
fn main() -> FragmentOutput {
    var<storage> cbuf_9_1_: cbuf_9_;
    let _e0 = cbuf_9_1_._m0_[60];
    return FragmentOutput(vec4<f32>(1.0));
}
";
        with_test_env("FX_NATIVE_FS", "0", || {
            assert!(should_use_native_fs_fragment(
                fs,
                crate::shader_registry::NativeColorInput::Auto,
            ));
        });
    }

    #[test]
    fn should_use_patched_fs_when_no_tables_and_env_off() {
        let fs = "\
@fragment
fn main(@location(1) in_attr1_: vec4<f32>) -> FragmentOutput {
    var in_attr1_1: vec4<f32>;
    in_attr1_1 = in_attr1_;
    return FragmentOutput(in_attr1_1);
}
";
        with_test_env("FX_NATIVE_FS", "0", || {
            assert!(!should_use_native_fs_fragment(
                fs,
                crate::shader_registry::NativeColorInput::VertexAttr,
            ));
        });
    }

    #[test]
    fn enhance_native_prefers_fs_chain_when_cbuf9_color_tables() {
        let fs = "\
@fragment
fn main(@location(1) in_attr1_: vec4<f32>) -> FragmentOutput {
    var<storage> cbuf_9_1_: cbuf_9_;
    var in_attr1_1: vec4<f32>;
    in_attr1_1 = in_attr1_;
    let _e0 = cbuf_9_1_._m0_[60];
    let _e1 = cbuf_9_1_._m0_[61];
    return FragmentOutput(vec4<f32>(_e0, _e0, _e0, 1.0));
}
";
        let out = enhance_native_fragment_wgsl_with_hint(
            fs,
            crate::shader_registry::NativeColorInput::Auto,
        );
        assert!(out.contains("let _fx_native_in = vec4<f32>(_e0, _e0, _e0, 1.0)"));
        assert!(!out.contains("var _fx_native_in = in_attr1_1"));
        assert!(!out.contains("if (dot(_fx_native_chain.rgb"));
    }

    #[test]
    fn enhance_native_modulates_frag_color0_not_attr1() {
        let fs = "\
var<private> frag_color0_: vec4<f32>;
var<private> in_attr1_1: vec4<f32>;
var<private> in_attr0_1: vec4<f32>;
var<private> cbuf_16_1_: array<vec4<f32>, 128>;
fn main_1() {
    let _scale = cbuf_16_1_._m0_[0].x;
    let _bias = cbuf_16_1_._m0_[2].y;
    let _branch = cbuf_16_1_._m0_[4].y;
    let _ = in_attr0_1;
    let _ = in_attr1_1;
    frag_color0_ = vec4<f32>(1.0, 0.5, 0.25, 1.0);
}
@fragment
fn main() -> FragmentOutput {
    main_1();
    return FragmentOutput(frag_color0_);
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("let _fx_native_in = frag_color0_"));
        assert!(!out.contains("_fx_native_in = in_attr1_1"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
        assert!(out.contains("return FragmentOutput(_fx_native_col"));
    }

    #[test]
    fn neutralize_fs_cbuf9_life_discard_noop_by_default() {
        let fs = "\
    let _life = cbuf_9_1_._m0_[94];
    pred_0 = gpr_5 <= _life.z;
";
        let out = neutralize_fs_cbuf9_life_discard(fs);
        assert!(out.contains("pred_0 = gpr_5"));
        assert!(!out.contains("pred_0 = false"));
    }

    #[test]
    fn neutralize_fs_cbuf9_life_discard_rewrites_pred_when_env_on() {
        let fs = "\
    let _life = cbuf_9_1_._m0_[94];
    pred_0 = gpr_5 <= _life.z;
";
        with_test_env("FX_NEUTRALIZE_FS_LIFE_DISCARD", "1", || {
            let out = neutralize_fs_cbuf9_life_discard(fs);
            assert!(out.contains("pred_0 = false"));
            assert!(!out.contains("pred_0 = gpr_5"));
        });
    }

    #[test]
    fn enhance_native_fragment_crossfade_when_attr10_present() {
        let fs = "\
@fragment
fn main(@location(10) in_attr10_: vec4<f32>, @location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr10_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    in_attr10_1 = in_attr10_;
    in_attr5_1 = in_attr5_;
    let _e0 = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    return FragmentOutput(_e0);
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("mix(_fx_ts0, _fx_ts1, _fx_blend)"));
        assert!(out.contains("in_attr10_1.x"));
    }

    #[test]
    fn enhance_native_fragment_injects_indirect_distortion() {
        let fs = "\
@fragment
fn main(@location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr1_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    in_attr5_1 = in_attr5_;
    return FragmentOutput(vec4<f32>(1.0));
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("_fx_distort_uv"));
        assert!(out.contains("indirect_tex"));
        assert!(out.contains("_fx_indirect.is_indirect"));
        assert!(out.contains("distortion_strength"));
        assert!(out.contains("distortion_by_cam_dist"));
        assert!(out.contains("_fx_distort_cam_scale"));
        assert!(native_fs_camera_distortion_needed(&out));
    }

    #[test]
    fn fx_distort_cam_scale_scales_offset_by_world_distance() {
        let helpers = fx_distortion_uv_helpers(
            "var in_attr0_1: vec4<f32>;\n@fragment\nfn main() {}",
        );
        assert!(helpers.contains("_fx_distort_cam_scale"));
        assert!(helpers.contains("length(_fx_indirect.cam_pos - world_pos)"));
        assert!(helpers.contains("offset *= _fx_distort_cam_scale(dist)"));
    }

    #[test]
    fn enhance_native_fragment_modulates_first_output_with_texture() {
        let fs = "\
@fragment
fn main(@location(6) in_attr6_: vec4<f32>) -> FragmentOutput {
    var in_attr6_1: vec4<f32>;
    in_attr6_1 = in_attr6_;
    let _e0 = vec4<f32>(1.0, 0.5, 0.25, 1.0);
    return FragmentOutput(_e0, _e0);
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("@group(1) @binding(0) var color_tex"));
        assert!(out.contains("textureSample(color_tex, color_sampler"));
        assert!(out.contains("vec2<f32>(0.5, 0.5)"));
        assert!(out.contains("let _fx_native_col_base = _fx_modulate_particle_tex(_fx_native_in, _fx_ts)"));
        assert!(out.contains("var _fx_native_col = _fx_native_col_base"));
        assert!(out.contains("_fx_distort_uv"));
        assert!(out.contains("return FragmentOutput(_fx_native_col, _e0)"));
        assert!(!out.contains("FragmentOutput({"));
    }

    #[test]
    fn enhance_native_fragment_applies_fresnel_alpha_modifiers_after_tex_modulate() {
        let fs = "\
var<private> cbuf_9_1_: array<vec4<f32>, 128>;
@fragment
fn main(@location(0) in_attr0_: vec4<f32>, @location(2) in_attr2_: vec4<f32>, \
@location(4) in_attr4_: vec4<f32>) -> FragmentOutput {
    var in_attr0_1: vec4<f32>;
    var in_attr1_1: vec4<f32>;
    var in_attr2_1: vec4<f32>;
    var in_attr4_1: vec4<f32>;
    in_attr0_1 = in_attr0_;
    in_attr1_1 = in_attr0_;
    in_attr2_1 = in_attr2_;
    in_attr4_1 = in_attr4_;
    return FragmentOutput(vec4<f32>(1.0), vec4<f32>(0.0));
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("_fx_particle_alpha"));
        assert!(out.contains("_fx_apply_particle_alpha_modifiers"));
        assert!(out.contains("_fx_native_col = _fx_apply_particle_alpha_modifiers(_fx_native_col)"));
        assert!(out.contains("pow(1.0 - n_dot_v"));
    }

    #[test]
    fn patch_fragment_wgsl_crossfade_and_extra_tex_match_native_inject() {
        let vs = "\
@vertex
fn main(@location(10) in_attr10_: vec4<f32>, @location(11) in_attr11_: vec4<f32>) -> VertexOutput {
    var in_attr10_1: vec4<f32>;
    var in_attr11_1: vec4<f32>;
    in_attr10_1 = in_attr10_;
    in_attr11_1 = in_attr11_;
    return VertexOutput(gl_Position);
}
";
        let fs = "\
@fragment
fn main(@location(2) in_attr2_: vec4<f32>, @location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr1_1: vec4<f32>;
    var in_attr2_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    in_attr2_1 = in_attr2_;
    in_attr5_1 = in_attr5_;
    return FragmentOutput(vec4<f32>(1.0));
}
";
        let vs = wire_vertex_simulation_varyings(vs);
        let fs = wire_crossfade_fragment_input(&fs, &vs);
        let fs = wire_extra_tex_fragment_input(&fs, &vs);
        with_test_env("FX_PATCHED_FS", "1", || {
            let out = patch_fragment_wgsl(&fs);
            assert!(out.contains("mix(_fx_ts0, _fx_ts1, _fx_blend)"));
            assert!(out.contains("in_attr10_1.x"));
            assert!(out.contains("textureSample(extra_tex3, extra_sampler3"));
            assert!(out.contains("textureSample(extra_tex5, extra_sampler5"));
            assert!(out.contains("in_attr11_1.xy"));
            assert!(out.contains("_fx_tex_blend"));
            assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
            assert!(out.contains("_fx_cbuf16_blend_ch3(_fx_native_col, _fx_ts5, _fx_tex_blend.tex5)"));
            assert!(out.contains("textureSample(alpha_tex, alpha_sampler"));
            assert!(out.contains("textureSample(slot2_tex, slot2_sampler"));
            assert!(out.contains("_fx_tex_blend.tex1"));
            assert!(out.contains("_fx_indirect.is_indirect == 0u"));
        });
    }

    #[test]
    fn native_fs_extra_tex_slots_needed_attr11_enables_tex5_without_cbuf() {
        let fs = "var x = in_attr11_1.xy;";
        let slots = native_fs_extra_tex_slots_needed(fs);
        assert!(slots[0] && slots[1] && slots[2]);

        let fs_no_attr = "return FragmentOutput(vec4(1.0));";
        let slots_none = native_fs_extra_tex_slots_needed(fs_no_attr);
        assert!(!slots_none[2]);
    }

    #[test]
    fn patch_fragment_wgsl_uses_tex_blend_uniform_for_primary() {
        let fs = "\
@fragment
fn main(@location(2) in_attr2_: vec4<f32>, @location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr1_1: vec4<f32>;
    var in_attr2_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    in_attr2_1 = in_attr2_;
    in_attr5_1 = in_attr5_;
    return FragmentOutput(vec4<f32>(1.0));
}
";
        let out = patch_fragment_wgsl(fs);
        assert!(out.contains("@group(1) @binding(0) var color_tex"));
        assert!(out.contains("_fx_tex_blend"));
        assert!(out.contains("_fx_cbuf16_blend_ch12"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
        assert!(out.contains("let _fx_native_in = in_attr1_1"));
        assert!(out.contains("_fx_distort_uv"));
        assert!(out.contains("textureSample(alpha_tex, alpha_sampler"));
        assert!(out.contains("textureSample(slot2_tex, slot2_sampler"));
        assert!(out.contains("_fx_tex_blend.tex1"));
        assert!(!out.contains("in_attr1_1.rgb * _ts.rgb"));
        assert!(native_fs_tex_blend_uniform_needed(&out));
    }

    #[test]
    fn native_fs_extra_tex_slots_needed_detects_cbuf_and_attr() {
        let fs = "var x = cbuf_9_1_._m0_[100].xy + cbuf_10_1_._m0_[11].zw;";
        let slots = native_fs_extra_tex_slots_needed(fs);
        assert!(slots[0] && slots[1]);
        assert!(!slots[2]);

        let fs5 = "var y = cbuf_9_1_._m0_[101].x + cbuf_10_1_._m0_[12].y;";
        let slots5 = native_fs_extra_tex_slots_needed(fs5);
        assert!(!slots5[0] && !slots5[1] && slots5[2]);
    }

    #[test]
    fn enhance_native_fragment_samples_extra_tex345() {
        let fs = "\
@fragment
fn main(@location(11) in_attr11_: vec4<f32>, @location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr11_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    var cbuf_16_1_: array<vec4<f32>, 128>;
    in_attr11_1 = in_attr11_;
    in_attr5_1 = in_attr5_;
    let _scroll = cbuf_9_1_._m0_[100].xy + cbuf_9_1_._m0_[101].xy + cbuf_10_1_._m0_[12].xy;
    let _chain = cbuf_16_1_._m0_[1].z + cbuf_16_1_._m0_[2].y + cbuf_16_1_._m0_[3].z;
    return FragmentOutput(vec4<f32>(1.0, 0.5, 0.25, 1.0));
}
";
        let out = enhance_native_fragment_wgsl(fs);
        assert!(out.contains("@group(2) @binding(0) var extra_tex3"));
        assert!(out.contains("textureSample(extra_tex3, extra_sampler3"));
        assert!(out.contains("textureSample(extra_tex5, extra_sampler5"));
        assert!(out.contains("in_attr11_1.xy"));
        assert!(out.contains("cbuf_9_1_._m0_[100].xy"));
        assert!(out.contains("_fx_ts3"));
        assert!(out.contains("var _fx_native_col = _fx_native_col_base"));
        assert!(out.contains("_fx_distort_uv"));
        assert!(out.contains("indirect_tex"));
        assert!(out.contains("_fx_indirect"));
        assert!(out.contains("_fx_cbuf16_blend_ch12"));
        assert!(out.contains("_fx_cbuf16_blend_ch3"));
        assert!(out.contains("_fx_tex_blend"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_col, _fx_ts3, _fx_tex_blend.tex3)"));
        assert!(out.contains("_fx_cbuf16_blend_ch3(_fx_native_col, _fx_ts5, _fx_tex_blend.tex5)"));
        assert!(out.contains("textureSample(alpha_tex, alpha_sampler"));
        assert!(out.contains("textureSample(slot2_tex, slot2_sampler"));
        assert!(out.contains("_fx_tex_blend.tex2"));
    }

    #[test]
    fn patch_fragment_wgsl_group1_tex2_uv_uses_cbuf_offset() {
        let fs = "\
@fragment
fn main(@location(2) in_attr2_: vec4<f32>, @location(5) in_attr5_: vec4<f32>) -> FragmentOutput {
    var in_attr1_1: vec4<f32>;
    var in_attr2_1: vec4<f32>;
    var in_attr5_1: vec4<f32>;
    var cbuf_10_1_: array<vec4<f32>, 128>;
    var cbuf_9_1_: array<vec4<f32>, 128>;
    in_attr2_1 = in_attr2_;
    in_attr5_1 = in_attr5_;
    let _ = cbuf_10_1_._m0_[9].zw + cbuf_9_1_._m0_[92].zw;
    return FragmentOutput(vec4<f32>(1.0));
}
";
        let out = patch_fragment_wgsl(fs);
        assert!(out.contains("cbuf_10_1_._m0_[9].zw"));
        assert!(out.contains("cbuf_9_1_._m0_[92].zw"));
        assert!(out.contains("_fx_uv_slot2"));
    }

    #[test]
    fn clamp_fragment_outputs_trims_deferred_mrt_to_visible_only() {
        let fs = "\
struct FragmentOutput {
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
    @location(2) out_attr2_: vec4<f32>,
    @location(3) out_attr3_: vec4<f32>,
    @location(4) out_attr4_: vec4<f32>,
    @location(5) out_attr5_: vec4<f32>,
}

@fragment
fn main() -> FragmentOutput {
    return FragmentOutput(_e0, _e1, _e2, _e3, _e4, _e5);
}
";
        let out = clamp_fragment_output_locations(fs, PARTICLE_COMPOSITE_MRT_LOCATIONS);
        assert!(out.contains("@location(0) out_attr0_"));
        assert!(!out.contains("@location(1)"));
        assert!(!out.contains("_e1"));
        assert!(out.contains("return FragmentOutput(_e0)"));
    }

    #[test]
    fn inject_soft_particle_wraps_fragment_output() {
        let fs = "\
struct FragmentOutput { @location(0) frag_color0_: vec4<f32>, }
@fragment
fn main() -> FragmentOutput {
    let _fx_native_col_base = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    return FragmentOutput(_fx_native_col_base);
}
";
        // Injection is opt-in (FX_SOFT_PARTICLE) — force it on for the structural test.
        std::env::set_var("FX_SOFT_PARTICLE", "1");
        let out = inject_soft_particle_fs(fs);
        std::env::remove_var("FX_SOFT_PARTICLE");
        assert!(out.contains("@group(3) @binding(0) var scene_depth"));
        assert!(out.contains("_fx_apply_soft_particle"));
        assert!(out.contains(
            "return FragmentOutput(_fx_apply_soft_particle(_fx_native_col_base, _fx_frag_pos)"
        ));
    }

    #[test]
    fn inject_soft_particle_reuses_existing_fragment_position_builtin() {
        let fs = "\
struct FragmentOutput { @location(0) frag_color0_: vec4<f32>, }
@fragment
fn main(@builtin(position) gl_FragCoord: vec4<f32>) -> FragmentOutput {
    let col = vec4<f32>(1.0);
    return FragmentOutput(col);
}
";
        // Injection is opt-in (FX_SOFT_PARTICLE) — force it on for the structural test.
        std::env::set_var("FX_SOFT_PARTICLE", "1");
        let out = inject_soft_particle_fs(fs);
        std::env::remove_var("FX_SOFT_PARTICLE");
        assert_eq!(out.matches("@builtin(position)").count(), 1);
        assert!(out.contains("_fx_frag_pos = gl_FragCoord"));
        assert!(out.contains("_fx_apply_soft_particle(col, _fx_frag_pos)"));
    }

    #[test]
    fn inject_opaque_core_alpha_test_inserts_discard_before_return() {
        let fs = "\
@fragment
fn main() -> FragmentOutput {
    let _fx_native_col_base = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    return FragmentOutput(_fx_native_col_base, _e0);
}
";
        let out = inject_opaque_core_alpha_test(fs, 0.5);
        assert!(out.contains("if (_fx_native_col_base.a < 0.5)"));
        assert!(out.contains("discard;"));
        assert!(out.find("discard;").unwrap() < out.find("return FragmentOutput").unwrap());
    }

    #[test]
    fn clamp_fragment_outputs_trims_mrt_beyond_limit() {
        let fs = "\
struct FragmentOutput {
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
    @location(8) out_attr8_: vec4<f32>,
    @location(9) out_attr9_: vec4<f32>,
}

@fragment
fn main(@location(0) in_attr0_: vec4<f32>) -> FragmentOutput {
    return FragmentOutput(_e0, _e1, _e8, _e9);
}
";
        let out = clamp_fragment_output_locations(fs, MAX_COLOR_ATTACHMENT_LOCATIONS);
        assert!(out.contains("@location(0) out_attr0_"));
        assert!(out.contains("@location(1) out_attr1_"));
        assert!(!out.contains("@location(8)"));
        assert!(!out.contains("@location(9)"));
        assert!(out.contains("return FragmentOutput(_e0, _e1)"));
        // No dangling references to the dropped constructor temporaries.
        assert!(!out.contains("_e8"));
        assert!(!out.contains("_e9"));
    }

    #[test]
    fn clamp_fragment_outputs_keeps_builtin_and_inrange() {
        let fs = "\
struct FragmentOutput {
    @location(0) out_attr0_: vec4<f32>,
    @builtin(frag_depth) depth: f32,
    @location(8) out_attr8_: vec4<f32>,
}

@fragment
fn main() -> FragmentOutput {
    return FragmentOutput(color, d, extra);
}
";
        let out = clamp_fragment_output_locations(fs, MAX_COLOR_ATTACHMENT_LOCATIONS);
        assert!(out.contains("@builtin(frag_depth) depth: f32"));
        assert!(out.contains("@location(0) out_attr0_"));
        assert!(!out.contains("@location(8)"));
        assert!(out.contains("return FragmentOutput(color, d)"));
    }

    #[test]
    fn clamp_fragment_outputs_noop_when_all_in_range() {
        let fs = "\
struct FragmentOutput {
    @location(0) out_attr0_: vec4<f32>,
    @location(1) out_attr1_: vec4<f32>,
}

@fragment
fn main() -> FragmentOutput {
    return FragmentOutput(a, b);
}
";
        let out = clamp_fragment_output_locations(fs, MAX_COLOR_ATTACHMENT_LOCATIONS);
        assert_eq!(out, fs);
    }

    #[test]
    fn parse_inline_fragment_params_stops_at_commas() {
        let params = "@location(6) in_attr6_: vec4<f32>, @location(7) in_attr7_: vec4<f32>";
        let fields = parse_inline_location_params(params);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].location, 6);
        assert_eq!(fields[0].name, "in_attr6_");
        assert_eq!(fields[0].ty, "vec4<f32>");
        assert_eq!(fields[1].location, 7);
        assert_eq!(fields[1].name, "in_attr7_");
        assert_eq!(fields[1].ty, "vec4<f32>");
    }

    #[test]
    fn override_billboard_uses_cbuf8_vp_when_shader_reads_cbuf8() {
        let wgsl = "\
fn main_1() { gl_Position = vec4(0.0); }
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(4) in_attr4_: vec4<f32>, \
@location(6) in_attr6_: vec4<f32>) -> VertexOutput {
    in_attr0_1 = in_attr0_;
    in_attr4_1 = in_attr4_;
    in_attr6_1 = in_attr6_;
    main_1();
    return VertexOutput(out_attr0_, gl_Position);
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_8_1_: cbuf_8_;
var<storage> cbuf_9_1_: cbuf_9_;
fn _ref() { let _ = cbuf_8_1_._m0_[8]; }
";
        let out = override_billboard_position(wgsl);
        assert!(out.contains("cbuf_8_1_._m0_[8]"));
        assert!(!out.contains("cbuf_9_1_._m0_[0]"));
    }

    #[test]
    fn override_billboard_injects_vp_transform_after_main() {
        let wgsl = "\
fn main_1() { gl_Position = vec4(0.0); }
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(1) in_attr1_: vec4<f32>, \
@location(2) in_attr2_: vec4<f32>, @location(4) in_attr4_: vec4<f32>, \
@location(6) in_attr6_: vec4<f32>) -> VertexOutput {
    in_attr0_1 = in_attr0_;
    in_attr1_1 = in_attr1_;
    in_attr2_1 = in_attr2_;
    in_attr4_1 = in_attr4_;
    in_attr6_1 = in_attr6_;
    main_1();
    return VertexOutput(out_attr0_, out_attr1_, out_attr2_, gl_Position);
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr1_1: vec4<f32>;
var<private> in_attr2_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> out_attr0_: vec4<f32>;
var<private> out_attr1_: vec4<f32>;
var<private> out_attr2_: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_9_1_: cbuf_9_;
";
        let out = override_billboard_position(wgsl);
        assert!(out.contains("cbuf_9_1_._m0_[0]"));
        assert!(out.contains("_aspect = in_attr4_1.z"));
        assert!(out.contains("out_attr1_ = in_attr1_1"));
        assert!(out.contains("out_attr2_ = in_attr2_1"));
        assert!(out.contains("out_attr0_ = in_attr1_1"));
        assert!(out.contains("(in_attr6_1.xy - in_attr6_1.zw)"));
    }

    #[test]
    fn override_billboard_falls_back_to_attr2_corners() {
        let wgsl = "\
fn main_1() {}
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(2) in_attr2_: vec4<f32>, \
@location(4) in_attr4_: vec4<f32>) {
    in_attr0_1 = in_attr0_;
    in_attr2_1 = in_attr2_;
    in_attr4_1 = in_attr4_;
    main_1();
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr2_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_9_1_: cbuf_9_;
";
        let out = override_billboard_position(wgsl);
        assert!(out.contains("in_attr2_1.xy - vec2<f32>(0.5, 0.5)"));
        assert!(!out.contains("in_attr6_1"));
    }

    #[test]
    fn override_billboard_supports_stripe_and_directional_modes() {
        let wgsl = "\
fn main_1() { gl_Position = vec4(0.0); }
@vertex
fn main(@location(0) in_attr0_: vec4<f32>, @location(3) in_attr3_: vec4<f32>, \
@location(4) in_attr4_: vec4<f32>, @location(6) in_attr6_: vec4<f32>, \
@location(7) in_attr7_: vec4<f32>) -> VertexOutput {
    in_attr0_1 = in_attr0_;
    in_attr3_1 = in_attr3_;
    in_attr4_1 = in_attr4_;
    in_attr6_1 = in_attr6_;
    in_attr7_1 = in_attr7_;
    main_1();
    return VertexOutput(gl_Position);
}
var<private> in_attr0_1: vec4<f32>;
var<private> in_attr3_1: vec4<f32>;
var<private> in_attr4_1: vec4<f32>;
var<private> in_attr6_1: vec4<f32>;
var<private> in_attr7_1: vec4<f32>;
var<private> gl_Position: vec4<f32>;
var<storage> cbuf_8_1_: cbuf_8_;
var<storage> cbuf_9_1_: cbuf_9_;
fn _ref() { let _ = cbuf_8_1_._m0_[8]; }
";
        let out = override_billboard_position(wgsl);
        assert!(out.contains("_bb_type == 5"), "stripe mode");
        assert!(out.contains("_bb_type == 3"), "directional Y mode");
        assert!(out.contains("_bb_type == 7"), "primitive mode");
        assert!(out.contains("in_attr7_1.w"), "per-particle billboard type");
        assert!(out.contains("normalize(_vel)"), "velocity-aligned modes");
    }

    #[test]
    fn rebuild_bomb_struct_is_clean() {
        let vs = BOMB_LINK_VS;
        let missing = vec![
            (6, "out_attr6_".into(), "vec4<f32>".into()),
            (7, "out_attr7_".into(), "vec4<f32>".into()),
        ];
        let out = rebuild_vertex_output_struct(&vs, &missing);
        let struct_body = {
            let start = out.find("struct VertexOutput {").unwrap();
            let rest = &out[start..];
            &rest[..rest.find('}').unwrap()]
        };
        assert!(
            !struct_body.contains("in_attr7_"),
            "rebuild must not leak fragment input names into VertexOutput: {struct_body}"
        );
        assert!(struct_body.contains("@location(7) out_attr7_"));
    }

    #[test]
    fn rebuild_from_patch_missing_matches_patch_struct() {
        let vs = BOMB_LINK_VS;
        let fs = BOMB_FS;
        let fs_inputs = fragment_io_fields(&fs);
        let mut vs_outputs = parse_struct_io_fields(&vs, "VertexOutput");
        let mut new_private_vars: Vec<(String, String)> = Vec::new();
        for fs_in in &fs_inputs {
            let out_name = vs_output_name_for_fs_input(&fs_in.name);
            if vs_outputs.iter().any(|o| o.location == fs_in.location) {
                continue;
            }
            new_private_vars.push((out_name.clone(), fs_in.ty.clone()));
            vs_outputs.push(IoField {
                location: fs_in.location,
                name: out_name,
                ty: fs_in.ty.clone(),
            });
        }
        let missing: Vec<(u32, String, String)> = vs_outputs
            .iter()
            .filter(|o| new_private_vars.iter().any(|(n, _)| n == &o.name))
            .map(|o| (o.location, o.name.clone(), o.ty.clone()))
            .collect();
        let rebuilt = rebuild_vertex_output_struct(&vs, &missing);
        let struct_body = {
            let start = rebuilt.find("struct VertexOutput {").unwrap();
            let rest = &rebuilt[start..];
            &rest[..rest.find('}').unwrap()]
        };
        assert!(!struct_body.contains("in_attr7_"));
        assert!(struct_body.contains("@location(7) out_attr7_"));
    }

    #[test]
    fn patch_bomb_does_not_corrupt_private_decls() {
        let vs = BOMB_LINK_VS;
        let fs = BOMB_FS;
        with_test_env("FX_PATCHED_FS", "1", || {
            let out = patch_vertex_wgsl(&vs, &fs);
            assert!(!out.contains("var<private> out_attr6_: vec4<f32>, @location"));
            assert!(!out.contains("out_attr0_ = in_attr0_"));
            assert!(out.contains("let _e239 = out_attr0_"));
            assert!(out.contains("out_attr6_ = in_attr6_1"));
            assert!(out.contains("out_attr10_ = in_attr10_1"));
            assert!(out.contains("out_attr11_ = in_attr11_1"));
            assert!(out.contains("out_attr12_ = in_attr12_1"));
            assert!(
                out.contains("out_attr10_, out_attr11_, out_attr12_")
                    || out.contains("out_attr10_, out_attr11_, _e251")
            );
            let snap = out
                .find("let _e")
                .and_then(|_| {
                    out.lines()
                        .enumerate()
                        .find(|(_, l)| l.trim_start().starts_with("let _e") && l.contains("out_attr2_"))
                        .map(|(i, l)| out.find(l).unwrap_or(i))
                })
                .or_else(|| out.find("let _e240 = out_attr2_"));
            let assign = out.find("out_attr2_ = in_attr2_1");
            if let (Some(snap), Some(assign)) = (snap, assign) {
                assert!(
                    assign < snap,
                    "bomb VS must forward CPU quad UVs before out_attr2 snapshot:\n{out}"
                );
            }
            assert!(
                out.contains("out_attr5_ = in_attr5_1"),
                "bomb VS must forward CPU flipbook tile origin:\n{out}"
            );
            assert!(
                out.contains("@location(5) in_attr5_"),
                "bomb VS must declare CPU attr5 input:\n{out}"
            );
        });
    }

    #[test]
    fn primary_atlas_uv_expr_rotates_and_scales_with_attr5() {
        let wgsl = "struct X { in_attr2_1: vec4<f32>, in_attr5_1: vec4<f32>, in_attr10_1: vec4<f32> } var<uniform> cbuf_9_1_: cbuf_9;";
        let uv = primary_atlas_uv_expr(wgsl);
        assert!(uv.contains("in_attr10_1.w"), "expected scroll rotation in {uv}");
        assert!(uv.contains("cbuf_9_1_._m0_[127].xy"), "expected tile scale in {uv}");
        assert!(uv.contains("in_attr5_1.xy"), "expected tile origin in {uv}");
        assert!(!uv.contains("in_attr6_1"), "must not use billboard half-extents as UV");
    }

    #[test]
    fn ensure_cpu_quad_uv_passthrough_before_out_attr2_snapshot() {
        let vs = "\
@vertex
fn main(@location(2) in_attr2_: vec4<f32>) -> VertexOutput {
    in_attr2_1 = in_attr2_;
    main_1();
    {
        out_attr1_ = in_attr1_1;
    }
    let _e240 = out_attr2_;
    return VertexOutput(_e240);
}";
        let out = ensure_cpu_quad_uv_passthrough(vs, vs);
        let snap = out.find("let _e240 = out_attr2_").expect("snapshot");
        let assign = out.find("out_attr2_ = in_attr2_1").expect("assign");
        assert!(
            assign < snap,
            "out_attr2 forward must precede let snapshot:\n{out}"
        );
    }

    #[test]
    fn ensure_cpu_attr5_passthrough_after_attr2_in_billboard_block() {
        let vs = "\
@vertex
fn main(@location(5) in_attr5_: vec4<f32>) -> VertexOutput {
    in_attr5_1 = in_attr5_;
    main_1();
    {
        out_attr1_ = in_attr1_1;
        out_attr2_ = in_attr2_1;
    }
    return VertexOutput(out_attr5_);
}";
        let out = ensure_cpu_attr5_passthrough(vs);
        let assign = out.find("out_attr5_ = in_attr5_1").expect("assign");
        let block = out.find("out_attr2_ = in_attr2_1").expect("attr2");
        assert!(
            assign > block,
            "attr5 forward should follow attr2 in billboard block:\n{out}"
        );
    }

    #[test]
    fn ensure_cpu_quad_uv_relocates_misplaced_assign() {
        let vs = "\
@vertex
fn main(@location(2) in_attr2_: vec4<f32>) -> VertexOutput {
    in_attr2_1 = in_attr2_;
    main_1();
    {
        out_attr1_ = in_attr1_1;
    }
    let _e240 = out_attr2_;
    out_attr2_ = in_attr2_1;
    return VertexOutput(_e240);
}";
        let out = ensure_cpu_quad_uv_passthrough(vs, vs);
        let snap = out.find("let _e240 = out_attr2_").expect("snapshot");
        let assign = out.find("out_attr2_ = in_attr2_1").expect("assign");
        assert!(assign < snap, "must relocate assign before snapshot:\n{out}");
        assert_eq!(out.matches("out_attr2_ = in_attr2_1").count(), 1);
    }

    #[test]
    fn wire_quad_uv_fragment_input_adds_attr2_and_attr5() {
        let vs = "struct VertexOutput { @location(2) out_attr2_: vec4<f32> } @vertex fn main() {}";
        let fs = "var<private> in_attr6_1: vec4<f32>; @fragment fn main() {}";
        let out = wire_quad_uv_fragment_input(fs, vs);
        assert!(out.contains("@location(2) in_attr2_"));
        assert!(out.contains("in_attr2_1"));
        assert!(out.contains("@location(5) in_attr5_"));
        assert!(out.contains("in_attr5_1"));
    }
}
