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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingClass {
    Uniform,
    Storage,
    Texture,
    Sampler,
}

/// A descriptor binding extracted from the naga IR module.
#[derive(Debug, Clone)]
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
    before_return.contains(&format!("= {out_name};"))
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
    // Native VS: run the decoded NVN register chain in main_1() for colour/UV varyings.
    // When the Family-A position chain is fully wired (cbuf_8 VP + world rows + cbuf_9 basis
    // + attr6 corners), trust main_1() gl_Position and skip the VP×billboard finalize.
    // Otherwise replace only gl_Position with VP×billboard (colour/UV varyings preserved).
    // Non-native: full billboard override including varying forwards.
    let vs_wgsl_owned;
    let vs_wgsl: &str = if crate::fx_env::fx_native_vs_pos_enabled() {
        if trusts_native_position_chain(vs_wgsl) {
            vs_wgsl
        } else if billboard_particle_vs_with_hint(vs_wgsl, vs_hint) {
            vs_wgsl_owned = finalize_native_vs_clip_position(vs_wgsl);
            &vs_wgsl_owned
        } else {
            // Mesh / model VS: no center×corner billboard inputs — keep native gl_Position.
            vs_wgsl
        }
    } else {
        vs_wgsl_owned = override_billboard_position(vs_wgsl);
        &vs_wgsl_owned
    };

    let fs_inputs = fragment_io_fields(fs_wgsl);
    if fs_inputs.is_empty() {
        return wire_vertex_simulation_varyings(vs_wgsl);
    }

    let mut result = vs_wgsl.to_string();
    let mut vs_outputs = parse_struct_io_fields(&result, "VertexOutput");
    if vs_outputs.is_empty() {
        return wire_vertex_simulation_varyings(&result);
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
        return wire_vertex_simulation_varyings(&result);
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
    wire_vertex_simulation_varyings(&result)
}

fn uses_cbuf8_vp(wgsl: &str) -> bool {
    wgsl.contains("cbuf_8_1_._m0_[8]")
        || wgsl.contains("cbuf_8_1_._m0_[9]")
        || wgsl.contains("cbuf_8_1_._m0_[10]")
        || wgsl.contains("cbuf_8_1_._m0_[11]")
}

fn uses_cbuf9_vp(wgsl: &str) -> bool {
    wgsl.contains("cbuf_9_1_._m0_[0]")
        || wgsl.contains("cbuf_9_1_._m0_[1]")
        || wgsl.contains("cbuf_9_1_._m0_[2]")
        || wgsl.contains("cbuf_9_1_._m0_[3]")
}

/// Model/mesh VS read texture dimensions from cbuf_9[17]; particle billboards do not.
fn uses_mesh_tex_dims_slot(wgsl: &str) -> bool {
    wgsl.contains("cbuf_9_1_._m0_[17]")
}

/// Model shaders read both cbuf_8[17] and [18]; particle VS only reads [18].
fn uses_model_position_param_slot(wgsl: &str) -> bool {
    wgsl.contains("cbuf_8_1_._m0_[17]")
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
    if !wgsl.contains("main_1();") || !wgsl.contains("gl_Position") {
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
    let partial_family_b = family_b_vp(wgsl) && !uses_cbuf9_camera_basis(wgsl);
    if !partial_family_b && !wgsl.contains("cbuf_9_1_._m0_[46]") {
        return false;
    }
    wgsl.contains("in_attr6_1")
        || wgsl.contains("in_attr2_1")
}

/// Family-A particle VS reads VP (cbuf_8[8..11]), world rows (cbuf_8[12..14]), camera basis
/// (cbuf_9[46..47]), and corner seeds (attr6). Full Family-B reads VP from cbuf_9[0..3] with
/// the same camera basis slot. Partial Family-B omits cbuf_9[46] — see
/// [`is_partial_family_b_billboard_vs`]. When the matching family is fully wired our NVN
/// evaluator fills the slots from PTCL data, so `main_1()` clip position is trustworthy without
/// VP×billboard finalize.
pub fn trusts_native_position_chain(wgsl: &str) -> bool {
    if !native_particle_vs_base(wgsl) {
        return false;
    }
    let family_a = wgsl.contains("cbuf_8_1_") && uses_cbuf8_vp(wgsl);
    let family_b = family_b_vp(wgsl);
    family_a || family_b
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

/// Atlas UV for slot-0 flipbook: corner UV × tile scale + per-particle tile origin (attr5).
/// Matches NVN `cbuf_9[99]` scale + `attr5.xy` offset wired in [`crate::nvn_chain`].
pub(crate) fn primary_atlas_uv_expr(wgsl: &str) -> String {
    let corner = if wgsl.contains("in_attr2_1") {
        "in_attr2_1.xy".to_string()
    } else if wgsl.contains("in_attr6_1") {
        "(in_attr6_1.xy + vec2<f32>(0.5, 0.5))".to_string()
    } else {
        "vec2<f32>(0.5, 0.5)".to_string()
    };
    let scaled = if wgsl.contains("cbuf_9_1_") && wgsl.contains("_m0_[99]") {
        format!("({corner} * cbuf_9_1_._m0_[99].xy)")
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
    tex3: vec4<f32>,\n\
    tex4: vec4<f32>,\n\
    tex5: vec4<f32>,\n\
}\n\
@group(2) @binding(6) var<uniform> _fx_tex_blend: FxTexBlendCoeffs;\n"
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
    return vec4<f32>(base.rgb * tex.rgb, base.a * tex.a);\n\
}\n\
fn _fx_cbuf16_blend_ch3(base: vec4<f32>, tex: vec4<f32>, c: vec4<f32>) -> vec4<f32> {\n\
    if (c.z > 0.5) {\n\
        return vec4<f32>(clamp(base.rgb + tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a + tex.a, 0.0, 1.0));\n\
    } else if (c.z < -0.5) {\n\
        return vec4<f32>(clamp(base.rgb - tex.rgb, vec3(0.0), vec3(1.0)), clamp(base.a - tex.a, 0.0, 1.0));\n\
    }\n\
    return vec4<f32>(base.rgb * tex.rgb, base.a * tex.a);\n\
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
    } else if wgsl.contains("cbuf_16_1_") {
        let cbuf_slot = extra_idx as u32 + 1;
        if wgsl.contains(&format!("_m0_[{cbuf_slot}]")) {
            format!(
                "{indent}_fx_native_col = {helper}(_fx_native_col, {tex_var}, cbuf_16_1_._m0_[{cbuf_slot}]);\n"
            )
        } else {
            format!(
                "{indent}_fx_native_col = vec4<f32>(_fx_native_col.rgb * {tex_var}.rgb, _fx_native_col.a * {tex_var}.a);\n"
            )
        }
    } else {
        format!(
            "{indent}_fx_native_col = vec4<f32>(_fx_native_col.rgb * {tex_var}.rgb, _fx_native_col.a * {tex_var}.a);\n"
        )
    }
}

fn blend_primary_color_tex(indent: &str, wgsl: &str) -> String {
    if wgsl.contains("_fx_tex_blend") {
        format!(
            "{indent}let _fx_native_col_base = _fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary);\n"
        )
    } else if wgsl.contains("cbuf_16_1_") && wgsl.contains("_m0_[1]") {
        format!(
            "{indent}let _fx_native_col_base = _fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, cbuf_16_1_._m0_[1]);\n"
        )
    } else {
        format!(
            "{indent}let _fx_native_col_base = vec4<f32>(_fx_native_in.rgb * _fx_ts.rgb, _fx_native_in.a * _fx_ts.a);\n"
        )
    }
}

fn modulate_native_col_with_extra_tex(indent: &str, slots: [bool; 3], wgsl: &str) -> String {
    let mut out = format!("{indent}var _fx_native_col = _fx_native_col_base;\n");
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

/// Declare missing `@location` vertex inputs and copy them into the spirv-cross private vars.
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
pub fn spirv_to_wgsl(
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

    // Safety net: strip `precise` qualifier (GLSL keyword naga doesn't parse).
    // Appears as `precise float`, `precise out vec4`, etc. from SPIR-V decoration 12.
    // The SPIR-V patch (nvn_strip_problematic_decorations) should prevent this, but
    // handle it at GLSL level as a fallback.  Since `precise` is a GLSL keyword used
    // only as a qualifier, replace `precise ` (word + space) anywhere it appears.
    // The risk of matching `imprecise ` is negligible in GLSL output.
    let glsl_source = glsl_source.replace("precise ", "");

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
        let debug_path = format!("/tmp/hitbox_{}.wgsl", shader_name.replace('/', "_"));
        let _ = std::fs::remove_file(&debug_path);
        if let Ok(mut f) = std::fs::File::create(&debug_path) {
            let _ = writeln!(f, "// {} — {} bytes WGSL", shader_name, wgsl.len());
            let _ = write!(f, "{}", wgsl);
            eprintln!("[DBG] Wrote WGSL to {}", debug_path);
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
    let use_cbuf_basis = !partial_family_b && wgsl.contains("cbuf_9_1_._m0_[46]");
    // Corner offsets: prefer attr6 (±0.5 half-extents written by the renderer), else derive
    // from attr2 quad UVs (0..1 → -0.5..0.5) for shader variants that omit attr6.
    let corner_expr = if wgsl.contains("in_attr6_1") {
        "in_attr6_1.xy".to_string()
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
        \x20           let _mesh_up = vec3<f32>(cbuf_9_1_._m0_[47].y, cbuf_9_1_._m0_[47].z, cbuf_9_1_._m0_[47].w);\n\
        \x20           if (length(_mesh_up) > 0.001) { _up = normalize(_mesh_up); }\n\
        \x20       }\n";
    let basis_block = if has_attr7 {
        let mut s = String::from("\x20       let _bb_type = i32(in_attr7_1.w);\n");
        if use_cbuf_basis {
            s.push_str("\x20       var _right = cbuf_9_1_._m0_[46].xyz;\n");
            match mode {
                BillboardClipMode::OverrideAll => {
                    s.push_str(
                        "\x20       var _up = vec3<f32>(cbuf_9_1_._m0_[47].y, cbuf_9_1_._m0_[47].z, cbuf_9_1_._m0_[47].w);\n",
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
                "\x20       let _right = cbuf_9_1_._m0_[46].xyz;\n\
        \x20       let _up = vec3<f32>(cbuf_9_1_._m0_[47].y, cbuf_9_1_._m0_[47].z, cbuf_9_1_._m0_[47].w);\n"
                    .to_string()
            }
            BillboardClipMode::PositionOnly => {
                "\x20       let _right = cbuf_9_1_._m0_[46].xyz;\n\
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
    let world_block = if has_attr7 {
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
        // The native NVN colour/UV varying chains are as unreliable as the position chain; forward
        // CPU-simulated per-particle colour and quad UV so both native and patched FS paths receive
        // sane inputs at the standard locations.
        if wgsl.contains("out_attr1_") && wgsl.contains("in_attr1_1") {
            override_code.push_str("        out_attr1_ = in_attr1_1;\n");
        }
        if wgsl.contains("out_attr2_") && wgsl.contains("in_attr2_1") {
            override_code.push_str("        out_attr2_ = in_attr2_1;\n");
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
    override_code.push_str("    }\n");
    let mut result = wgsl.to_string();
    result.insert_str(insert_at, &override_code);
    result
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
    // Debug overrides to localize the black-colour bug:
    //  - FX_DEBUG_VCOLOR_FS: output the interpolated vertex-colour varying (in_attr1_1).
    //  - FX_DEBUG_CBUF60_FS: output cbuf_9[60].rgb directly (tests whether the FS's cbuf_9
    //    binding is actually populated, independent of the colour chain).
    //  - otherwise: constant opaque magenta.
    args[0] = if std::env::var("FX_DEBUG_CULL_FS").is_ok()
        && wgsl.contains("in_attr5_1")
        && wgsl.contains("cbuf_10_1_")
    {
        // White when the fragment-stage life gate would NOT cull (in_attr5_1.w <= cbuf_10[2].x),
        // black when it would. Localises whether the native FS early-returns before colouring.
        "select(vec4<f32>(0.0, 0.0, 0.0, 1.0), vec4<f32>(1.0, 1.0, 1.0, 1.0), in_attr5_1.w <= cbuf_10_1_._m0_[2].x)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF10_FS").is_ok() && wgsl.contains("cbuf_10_1_") {
        // Output cbuf_10[0].rgb directly: tests whether the FS's cbuf_10 binding is populated
        // (the final colour multiply is out.rgb *= cbuf_10[0].xyz, so a zero here blacks everything).
        "vec4<f32>(cbuf_10_1_._m0_[0].x, cbuf_10_1_._m0_[0].y, cbuf_10_1_._m0_[0].z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_NATIVE_RGB_FS").is_ok() && wgsl.contains("out_attr0_") {
        // Output the natively-computed colour but force alpha opaque, to tell whether the native
        // RGB chain is zero or only the alpha channel is the problem.
        "vec4<f32>(out_attr0_.x, out_attr0_.y, out_attr0_.z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF59_FS").is_ok() && wgsl.contains("cbuf_9_1_") {
        // Output cbuf_9[59].x (the global colour multiplier) on all channels.
        "vec4<f32>(cbuf_9_1_._m0_[59].x, cbuf_9_1_._m0_[59].x, cbuf_9_1_._m0_[59].x, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_CBUF60_FS").is_ok() && wgsl.contains("cbuf_9_1_") {
        "vec4<f32>(cbuf_9_1_._m0_[60].x, cbuf_9_1_._m0_[60].y, cbuf_9_1_._m0_[60].z, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_VCOLOR_FS").is_ok() && wgsl.contains("in_attr1_1") {
        "vec4<f32>(in_attr1_1.rgb, 1.0)".to_string()
    } else if std::env::var("FX_DEBUG_PROBE").is_ok() && wgsl.contains("_dbg_probe") {
        "_dbg_probe".to_string()
    } else {
        "vec4<f32>(1.0, 0.0, 1.0, 1.0)".to_string()
    };
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
    let _ = std::fs::write("/tmp/hitbox_probed_fs.wgsl", &result);
    result
}

/// Per-pixel alpha-test threshold for opaque-core depth-write passes (within-path occlusion).
pub const OPAQUE_CORE_DEPTH_ALPHA_TEST: f32 = 0.5;

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
    inject_fragment_texture_blend(wgsl, None, false)
}

fn inject_fragment_texture_blend(
    wgsl: &str,
    native_in_override: Option<&str>,
    force_blend_uniform: bool,
) -> String {
    let mut result = wgsl.to_string();
    let extra_tex_slots = native_fs_extra_tex_slots_needed(&result);
    let blend_uniform = force_blend_uniform || native_fs_tex_blend_uniform_needed(&result);

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
        if let Some(helpers) = extra_tex_cbuf16_blend_helpers(wgsl, blend_uniform) {
            out.push_str(helpers);
        }
    }

    if !result.contains("@group(1)") {
        let mut tex_decls = String::from(
            "\n@group(1) @binding(0) var color_tex: texture_2d<f32>;\n\
             @group(1) @binding(1) var color_sampler: sampler;\n",
        );
        if (extra_tex_slots.iter().any(|&b| b) || blend_uniform) && !result.contains("@group(2)") {
            append_group2_decls(&mut tex_decls, extra_tex_slots, blend_uniform, &result);
        }
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &tex_decls);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &tex_decls);
        }
    } else if (extra_tex_slots.iter().any(|&b| b) || blend_uniform) && !result.contains("@group(2)") {
        let mut decls = String::new();
        append_group2_decls(&mut decls, extra_tex_slots, blend_uniform, &result);
        if let Some(priv_pos) = result.find("var<private>") {
            result.insert_str(priv_pos, &decls);
        } else if let Some(entry) = result.find("@fragment") {
            result.insert_str(entry, &decls);
        }
    } else if extra_tex_slots.iter().any(|&b| b) || blend_uniform {
        if let Some(helpers) = extra_tex_cbuf16_blend_helpers(&result, blend_uniform) {
            if !result.contains("_fx_cbuf16_blend_ch12") {
                if let Some(priv_pos) = result.find("var<private>") {
                    result.insert_str(priv_pos, helpers);
                } else if let Some(entry) = result.find("@fragment") {
                    result.insert_str(entry, helpers);
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
    }

    let uv_expr = primary_atlas_uv_expr(&result);
    let crossfade_expr = if result.contains("in_attr10_1") {
        Some((
            "in_attr10_1.x".to_string(),
            format!("({uv_expr} + in_attr10_1.yz)"),
        ))
    } else if result.contains("cbuf_9_1_") && result.contains("_m0_[9]") {
        Some((
            "cbuf_9_1_._m0_[9].x".to_string(),
            format!("({uv_expr} + cbuf_9_1_._m0_[96].w * vec2<f32>(1.0, 0.0))"),
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
    let extra_prelude = extra_tex_sample_prelude(&result, &indent, &uv_expr, extra_tex_slots);
    let extra_modulate = if extra_tex_slots.iter().any(|&b| b) {
        modulate_native_col_with_extra_tex(&indent, extra_tex_slots, &result)
    } else {
        String::new()
    };
    let primary_blend = blend_primary_color_tex(&indent, &result);
    let texture_sample = if let Some((blend, uv_next)) = &crossfade_expr {
        format!(
            "{indent}let _fx_native_in = {native_in};\n\
             {indent}let _fx_blend = {blend};\n\
             {indent}let _fx_uv0 = {uv_expr};\n\
             {indent}let _fx_ts0 = textureSample(color_tex, color_sampler, _fx_uv0);\n\
             {indent}let _fx_ts1 = textureSample(color_tex, color_sampler, {uv_next});\n\
             {indent}let _fx_ts = mix(_fx_ts0, _fx_ts1, _fx_blend);\n\
             {primary_blend}{extra_prelude}{extra_modulate}"
        )
    } else {
        format!(
            "{indent}let _fx_native_in = {native_in};\n\
             {indent}let _fx_ts = textureSample(color_tex, color_sampler, {uv_expr});\n\
             {primary_blend}{extra_prelude}{extra_modulate}"
        )
    };
    let prelude = texture_sample;
    args[0] = if extra_tex_slots.iter().any(|&b| b) {
        "_fx_native_col".to_string()
    } else {
        "_fx_native_col_base".to_string()
    };
    let new_return = format!("{ctor}{})", args.join(", "));
    result.insert_str(line_start, &prelude);
    let ret2 = result.rfind(ctor).expect("return FragmentOutput vanished after prelude insert");
    let open2 = ret2 + ctor.len() - 1;
    let close2 = matching_close_paren(&result, open2).expect("FragmentOutput paren");
    result.replace_range(ret2..close2 + 1, &new_return);
    result
}

#[cfg(test)]
mod patch_tests {
    use super::*;

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
        std::env::set_var("FX_NATIVE_VS_POS", "1");
        let out = wire_vertex_simulation_varyings(vs);
        std::env::remove_var("FX_NATIVE_VS_POS");
        assert!(out.contains("out_attr10_ = in_attr10_1"));
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
    fn trusts_native_position_chain_skips_billboard_finalize() {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
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
        assert!(!patched.contains("_world = in_attr0_1.xyz"));
        std::env::remove_var("FX_NATIVE_VS_POS");
    }

    #[test]
    fn trusts_native_position_chain_detects_family_a() {
        let wgsl = "main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_8_1_ cbuf_9_1_ cbuf_8_1_._m0_[8] cbuf_9_1_._m0_[46] gl_Position";
        assert!(trusts_native_position_chain(wgsl));
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
        std::env::set_var("FX_NATIVE_VS_POS", "1");
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
        assert!(!billboard_particle_vs(vs));
        let patched = patch_vertex_wgsl(vs, fs);
        assert!(!patched.contains("_world = in_attr0_1.xyz"));
        std::env::remove_var("FX_NATIVE_VS_POS");
    }

    #[test]
    fn hybrid_finalize_skips_non_billboard_vs() {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
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
        let patched = patch_vertex_wgsl(vs, fs);
        assert!(!patched.contains("_world = in_attr0_1.xyz"));
        std::env::remove_var("FX_NATIVE_VS_POS");
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
        std::env::set_var("FX_NATIVE_VS_POS", "1");
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
        let out = finalize_native_vs_clip_position(vs);
        assert!(out.contains("cbuf_9_1_._m0_[0]"));
        assert!(out.contains("_world = in_attr0_1.xyz"));
        assert!(
            !out.contains("cbuf_9_1_._m0_[46].xyz"),
            "partial Family-B finalize must derive basis from VP, not cbuf_9[46]"
        );
        std::env::remove_var("FX_NATIVE_VS_POS");
    }

    #[test]
    fn hybrid_finalize_skips_mesh_vs_with_registry_hint() {
        std::env::set_var("FX_NATIVE_VS_POS", "1");
        let vs = "\
main_1(); in_attr0_1 in_attr4_1 in_attr6_1 cbuf_9_1_ cbuf_9_1_._m0_[0] cbuf_9_1_._m0_[46] gl_Position";
        assert!(billboard_particle_vs(vs));
        assert!(!billboard_particle_vs_with_hint(
            vs,
            Some(crate::shader_registry::ShaderVsProfile::MeshModel),
        ));
        let fs = "@fragment\nfn main() -> FragmentOutput { return FragmentOutput(vec4(1.0)); }";
        let patched = patch_vertex_wgsl_with_hint(
            vs,
            fs,
            Some(crate::shader_registry::ShaderVsProfile::MeshModel),
        );
        assert!(!patched.contains("_world = in_attr0_1.xyz"));
        std::env::remove_var("FX_NATIVE_VS_POS");
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
        assert!(out.contains("in_attr6_1.xy + vec2<f32>(0.5, 0.5)"));
        assert!(out.contains("let _fx_native_col_base = vec4<f32>(_fx_native_in.rgb * _fx_ts.rgb"));
        assert!(out.contains("return FragmentOutput(_fx_native_col_base, _e0)"));
        assert!(!out.contains("FragmentOutput({"));
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
        std::env::set_var("FX_PATCHED_FS", "1");
        let out = patch_fragment_wgsl(&fs);
        std::env::remove_var("FX_PATCHED_FS");
        assert!(out.contains("mix(_fx_ts0, _fx_ts1, _fx_blend)"));
        assert!(out.contains("in_attr10_1.x"));
        assert!(out.contains("textureSample(extra_tex3, extra_sampler3"));
        assert!(out.contains("textureSample(extra_tex5, extra_sampler5"));
        assert!(out.contains("in_attr11_1.xy"));
        assert!(out.contains("_fx_tex_blend"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
        assert!(out.contains("_fx_cbuf16_blend_ch3(_fx_native_col, _fx_ts5, _fx_tex_blend.tex5)"));
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
        assert!(out.contains("_fx_cbuf16_blend_ch12"));
        assert!(out.contains("_fx_cbuf16_blend_ch3"));
        assert!(out.contains("_fx_tex_blend"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_in, _fx_ts, _fx_tex_blend.primary)"));
        assert!(out.contains("_fx_cbuf16_blend_ch12(_fx_native_col, _fx_ts3, _fx_tex_blend.tex3)"));
        assert!(out.contains("_fx_cbuf16_blend_ch3(_fx_native_col, _fx_ts5, _fx_tex_blend.tex5)"));
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
        assert!(out.contains("in_attr6_1.xy"));
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
        std::env::set_var("FX_PATCHED_FS", "1");
        let out = patch_vertex_wgsl(&vs, &fs);
        std::env::remove_var("FX_PATCHED_FS");
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
    }
}
