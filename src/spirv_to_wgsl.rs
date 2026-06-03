// SPIR-V → WGSL shader conversion using naga's SPIR-V frontend directly.
// Single-step: SPIR-V → naga IR → WGSL (no spirv-cross, no GLSL, no CLI).
//
// Also applies NVN→Vulkan compatibility patches for Switch shaders.

use naga::front::spv;
use naga::back::wgsl;

use anyhow::{Result, anyhow};

/// A descriptor binding extracted from the naga IR module.
#[derive(Debug, Clone)]
pub struct DescriptorInfo {
    pub set: u32,
    pub binding: u32,
    pub name: String,
    pub ty_str: String,
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
                            ty.name.clone().unwrap_or_else(|| "Uniform".to_string())
                        }
                        _ => format!("{:?}", ty.inner).chars().take(24).collect(),
                    }
                })
                .unwrap_or_else(|| "Unknown".to_string());

            descriptors.push(DescriptorInfo {
                set: binding.group,
                binding: binding.binding,
                name: var.name.clone().unwrap_or_else(|| format!("var_{}_{}", binding.group, binding.binding)),
                ty_str,
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

/// Convert SPIR-V words to WGSL via naga's SPIR-V frontend directly.
/// The words should already be NVN-patched (execution modes) and binding-remapped.
/// Returns (WGSL_source, descriptor_bindings).
pub fn spirv_words_to_wgsl(
    spirv_words: &[u32],
    shader_name: &str,
) -> Result<(String, Vec<DescriptorInfo>)> {
    let options = spv::Options {
        adjust_coordinate_space: true,
        strict_capabilities: false,
        block_ctx_dump_prefix: None,
    };

    let module = spv::Frontend::new(spirv_words.iter().copied(), &options)
        .parse()
        .map_err(|e| anyhow!("naga SPIR-V parse ({}): {:?}", shader_name, e))?;

    let descriptors = extract_descriptors_from_module(&module);

    eprintln!("[SPIRV→WGSL] Parsed module ({}): {} global vars, {} descriptors",
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
    Ok((wgsl, descriptors))
}

/// Convert SPIR-V bytes to WGSL via naga's SPIR-V frontend directly.
/// Also patches NVN-specific SPIR-V to be Vulkan-compatible.
/// Returns (WGSL_source, descriptor_bindings). No spirv-cross needed.
pub fn spirv_to_wgsl(spirv_bytes: &[u8], shader_name: &str) -> Result<(String, Vec<DescriptorInfo>)> {
    if spirv_bytes.len() < 4 {
        return Err(anyhow!("SPIR-V data too small"));
    }
    let magic = u32::from_le_bytes([spirv_bytes[0], spirv_bytes[1], spirv_bytes[2], spirv_bytes[3]]);
    if magic != 0x07230203 {
        return Err(anyhow!("Invalid SPIR-V magic: {:#x}", magic));
    }
    eprintln!("[SPIRV→WGSL] {}: {} bytes (naga direct)", shader_name, spirv_bytes.len());

    // Convert to words for NVN patching
    let mut spirv_words = bytes_to_words(spirv_bytes)?;

    // Apply NVN→Vulkan compatibility patches
    let patches = crate::spirv_patch::nvn_to_vulkan_patch(&mut spirv_words);
    if !patches.is_empty() {
        eprintln!("[SPIRV→WGSL] NVN patches applied: {}", patches.join(", "));
    }

    spirv_words_to_wgsl(&spirv_words, shader_name)
}

/// Create a wgpu ShaderModule from SPIR-V via naga direct conversion.
pub fn create_shader_module_from_spirv(
    device: &wgpu::Device,
    spirv_bytes: &[u8],
    shader_name: &str,
) -> Result<wgpu::ShaderModule> {
    let (wgsl, descs) = spirv_to_wgsl(spirv_bytes, shader_name)?;
    for d in &descs {
        eprintln!("[SPIRV→WGSL]   binding: set={} binding={} name={} type={}",
            d.set, d.binding, d.name, d.ty_str);
    }
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader_name),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    Ok(module)
}

/// Create a wgpu ShaderModule from pre-patched SPIR-V words.
/// The words should already have NVN→Vulkan patches and binding remapping applied.
pub fn create_shader_module_from_spirv_words(
    device: &wgpu::Device,
    spirv_words: &[u32],
    shader_name: &str,
) -> Result<wgpu::ShaderModule> {
    let (wgsl, descs) = spirv_words_to_wgsl(spirv_words, shader_name)?;
    for d in &descs {
        eprintln!("[SPIRV→WGSL]   binding: set={} binding={} name={} type={}",
            d.set, d.binding, d.name, d.ty_str);
    }
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader_name),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    Ok(module)
}
