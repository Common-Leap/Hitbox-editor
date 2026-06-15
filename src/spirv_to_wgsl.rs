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
    if let Some(pos) = wgsl.rfind("var<private> out_attr") {
        let line_end = wgsl[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(wgsl.len());
        wgsl.insert_str(line_end, &format!("{decl}\n"));
    } else if let Some(pos) = wgsl.rfind("var<private>") {
        let line_end = wgsl[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(wgsl.len());
        wgsl.insert_str(line_end, &format!("{decl}\n"));
    } else {
        wgsl.insert_str(0, &format!("{decl}\n"));
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
    // Unless explicitly opted out, replace the (unreliable) native NVN clip-space position
    // with a clean billboard transform. Done first so the rest of the patching (and the
    // downstream cbuf slot-usage scan) sees the cbuf_9 references it introduces.
    let vs_wgsl_owned;
    let vs_wgsl: &str = if std::env::var("FX_NATIVE_VS_POS").is_ok() {
        vs_wgsl
    } else {
        vs_wgsl_owned = override_billboard_position(vs_wgsl);
        &vs_wgsl_owned
    };

    let fs_inputs = fragment_io_fields(fs_wgsl);
    if fs_inputs.is_empty() {
        return vs_wgsl.to_string();
    }

    let mut result = vs_wgsl.to_string();
    let mut vs_outputs = parse_struct_io_fields(&result, "VertexOutput");
    if vs_outputs.is_empty() {
        return result;
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
        return result;
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
            new_assignments.push_str(&format!("\n    {out_name} = {};", vin.name));
        } else if let Some(vin) = vs_inputs.iter().find(|v| v.name == fs_in.name) {
            new_assignments.push_str(&format!("\n    {out_name} = {};", vin.name));
        }
    }

    if !new_assignments.is_empty() {
        if let Some(pos) = result.rfind("return VertexOutput(") {
            result.insert_str(pos, &format!("{new_assignments}\n    "));
        }
    }

    let new_names: Vec<String> = new_private_vars.into_iter().map(|(n, _)| n).collect();
    extend_vertex_return(&result, &new_names)
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
    let temp_dir = tempfile::tempdir()
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
/// `@location`s that WebGPU accepts.
///
/// NVN deferred/G-buffer shaders frequently declare up to ~10 MRT color outputs, but
/// particles composite into a single color attachment. WebGPU permits a fragment shader to
/// write to color locations that have no bound target *provided the location index is in
/// range* (`< max_color_attachments`). spirv-cross faithfully reproduces all 10 outputs, so
/// `@location(8)`/`@location(9)` make pipeline creation fail with
/// `ColorAttachmentLocationTooLarge`.
///
/// This removes out-of-range fields from the `FragmentOutput` struct and the matching
/// positional `return FragmentOutput(...)` constructor, preserving `@location(0)` (the color
/// we actually composite) and the relative order of every kept field. `@builtin(...)` outputs
/// (e.g. `frag_depth`) are always kept. Returns the input unchanged if there is nothing to trim
/// or the shape is unexpected (so this is a safe no-op for already-valid shaders).
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
/// where `center = in_attr0_`, `corner = in_attr6_.xy`, `size = in_attr4_.y`, the VP matrix is
/// stored column-major in `cbuf_9[0..3]`, and the camera basis is `cbuf_9[46]` (right) /
/// `cbuf_9[47].yzw` (up). Referencing those slots makes the data-driven NVN evaluator fill them
/// (the slot-usage scan runs on the patched WGSL). Returns the input unchanged when the shader
/// is not a billboard particle VS (missing `in_attr6_`/center inputs), so mesh/primitive
/// shaders keep their native transform.
pub fn override_billboard_position(wgsl: &str) -> String {
    let marker = "main_1();";
    let Some(pos) = wgsl.find(marker) else {
        return wgsl.to_string();
    };
    let base_needed = ["in_attr0_1", "in_attr4_1", "cbuf_9_1_", "gl_Position"];
    if base_needed.iter().any(|id| !wgsl.contains(id)) {
        return wgsl.to_string();
    }
    // Corner offsets: prefer attr6 (±0.5 half-extents written by the renderer), else derive
    // from attr2 quad UVs (0..1 → -0.5..0.5) for shader variants that omit attr6.
    let corner_expr = if wgsl.contains("in_attr6_1") {
        "in_attr6_1.xy".to_string()
    } else if wgsl.contains("in_attr2_1") {
        "(in_attr2_1.xy - vec2<f32>(0.5, 0.5))".to_string()
    } else {
        return wgsl.to_string();
    };
    let insert_at = pos + marker.len();
    let mut override_code = format!(
        "\n    {{\n\
        \x20       let _vp0 = cbuf_9_1_._m0_[0];\n\
        \x20       let _vp1 = cbuf_9_1_._m0_[1];\n\
        \x20       let _vp2 = cbuf_9_1_._m0_[2];\n\
        \x20       let _vp3 = cbuf_9_1_._m0_[3];\n\
        \x20       let _right = cbuf_9_1_._m0_[46].xyz;\n\
        \x20       let _up = vec3<f32>(cbuf_9_1_._m0_[47].y, cbuf_9_1_._m0_[47].z, cbuf_9_1_._m0_[47].w);\n\
        \x20       let _sz = in_attr4_1.y;\n\
        \x20       let _aspect = in_attr4_1.z;\n\
        \x20       let _corner = {corner_expr};\n\
        \x20       let _world = in_attr0_1.xyz + _corner.x * _sz * _aspect * _right + _corner.y * _sz * _up;\n\
        \x20       gl_Position = _vp0 * _world.x + _vp1 * _world.y + _vp2 * _world.z + _vp3;\n",
    );
    // The native NVN colour/UV varying chains are as unreliable as the position chain; forward
    // CPU-simulated per-particle colour and quad UV so both native and patched FS paths receive
    // sane inputs at the standard locations.
    if wgsl.contains("out_attr1_") && wgsl.contains("in_attr1_1") {
        override_code.push_str("        out_attr1_ = in_attr1_1;\n");
    }
    if wgsl.contains("out_attr2_") && wgsl.contains("in_attr2_1") {
        override_code.push_str("        out_attr2_ = in_attr2_1;\n");
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

/// Patch fragment WGSL to use native WGSL texture sampling with the vertex
/// colour attribute for the output colour.
///
/// Adds @group(1) @binding(0/1) for a native texture+sampler, adds
/// @location(8) in_attr1_1 fragment input, and replaces the first return
/// argument with `textureSample(color_tex, sampler, in_attr2_1.xy) * in_attr1_1`
/// so the fragment samples the emitter's actual texture and modulates it with
/// the per‑particle vertex colour.
pub fn patch_fragment_wgsl(wgsl: &str) -> String {
    let mut result = wgsl.to_string();

    // 1. Add native texture+sampler declarations at group(1) if not present
    if !result.contains("@group(1)") {
        // Insert after the last @group(0) ... storage declaration, before var<private>
        if let Some(priv_pos) = result.find("var<private>") {
            let tex_decls = "\n@group(1) @binding(0) var color_tex: texture_2d<f32>;\n\
                            @group(1) @binding(1) var color_sampler: sampler;\n";
            result.insert_str(priv_pos, tex_decls);
        }
    }

    // 2. Inject a native texture sample binding and combine it with the
    //    vertex colour from simulation (in_attr1_1).  The NVN fragment
    //    colour chain produces out_attr0_ from vertex position (in_attr0_)
    //    and colour-table coefficients — but since our vertex buffer
    //    contains pre-expanded billboard corners (not particle centres),
    //    the position-based out_attr0_ varies across the particle, giving
    //    a wrong per-corner tint.  Use in_attr1_1 (CPU-simulated per-vertex
    //    colour, same for all four corners) instead.
    //  * Single-channel textures (R8/BC4) are now expanded to (R,R,R,R)
    //    so the channel value provides greyscale colour + alpha.
    //  * BC3/RGBA8 keep their native RGBA channels.
    //    First arg: vec4<f32>(vertex_colour.rgb * _ts.rgb, _ts.a)
    if let Some(ret_pos) = result.rfind("return FragmentOutput(") {
        let has_uv = result.contains("in_attr2_1: vec4<f32>")
                  || result.contains("in_attr2_1 ");
        let uv_expr = if has_uv { "in_attr2_1.xy" } else { "vec2<f32>(0.5, 0.5)" };
        // Inject let _ts = textureSample(...); just before the return
        let tex_sample_let = format!("\n    let _ts = textureSample(color_tex, color_sampler, {uv_expr});\n");
        result.insert_str(ret_pos, &tex_sample_let);
        let ret_pos_updated = ret_pos + tex_sample_let.len();
        // Use vertex colour from simulation (in_attr1_1) instead of the
        // NVN chain output (out_attr0_, which is position-dependent and
        // wrong for pre-expanded billboard corners).
        // Alpha = vertex_alpha × texture_alpha so that fade-in/fade-out
        // (driven by vertex alpha from simulation) and per-pixel shape
        // (from texture alpha / BC4 luminance) both contribute.
        let first_arg = {
            "vec4<f32>(in_attr1_1.rgb * _ts.rgb, in_attr1_1.a * _ts.a)"
        };
        // Replace the first argument in FragmentOutput(...)
        if let Some(open_pos) = result[ret_pos_updated..].find('(') {
            let open_pos_abs = ret_pos_updated + open_pos + 1;
            let after_open = &result[open_pos_abs..];
            if let Some(comma_pos) = after_open.find(',') {
                result.replace_range(open_pos_abs..open_pos_abs + comma_pos, first_arg);
            } else {
                if let Some(close_paren) = after_open.find(')') {
                    result.replace_range(open_pos_abs..open_pos_abs + close_paren, first_arg);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod patch_tests {
    use super::*;

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
    fn rebuild_bomb_struct_is_clean() {
        let vs = std::fs::read_to_string("/tmp/hitbox_link_vs_0x5740678a2aa5959f.wgsl")
            .expect("run bomb test first to generate link vs dump");
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
        let vs = std::fs::read_to_string("/tmp/hitbox_link_vs_0x5740678a2aa5959f.wgsl").unwrap();
        let fs = std::fs::read_to_string("/tmp/hitbox_bomb_fs.wgsl").unwrap();
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
        let vs = std::fs::read_to_string("/tmp/hitbox_link_vs_0x5740678a2aa5959f.wgsl").unwrap();
        let fs = std::fs::read_to_string("/tmp/hitbox_bomb_fs.wgsl").unwrap();
        let out = patch_vertex_wgsl(&vs, &fs);
        assert!(!out.contains("var<private> out_attr6_: vec4<f32>, @location"));
        assert!(!out.contains("out_attr0_ = in_attr0_"));
        assert!(out.contains("let _e239 = out_attr0_"));
        assert!(out.contains("out_attr6_ = in_attr6_"));
        assert!(out.contains("return VertexOutput(_e239, _e241, _e243, _e245, _e247, _e249, out_attr6_, out_attr7_, _e251)"));
    }
}
