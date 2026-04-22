/// SPIR-V to WGSL shader conversion using spirv-cross
/// Provides infrastructure for loading BNSH-decoded SPIR-V shaders into wgpu pipelines
///
/// Uses the spirv-cross CLI tool (https://github.com/KhronosGroup/SPIRV-Cross)
/// to convert SPIR-V binary format to WGSL source code.
///
/// The spirv-cross binary must be available in the system PATH.

use anyhow::{Result, anyhow};

/// Convert SPIR-V bytes to WGSL shader source using spirv-cross CLI
/// 
/// Requires spirv-cross to be installed and available in PATH.
/// 
/// # Arguments
/// * `spirv_bytes` - Binary SPIR-V shader data
/// * `shader_name` - Name for logging/error messages
/// 
/// # Returns
/// WGSL shader source code as a string
/// 
/// # Installation
/// spirv-cross must be installed to convert BNSH shaders:
/// - Ubuntu/Debian: `sudo apt install spirv-cross`
/// - macOS: `brew install spirv-cross`
/// - Windows: Download from https://github.com/KhronosGroup/SPIRV-Cross/releases
/// - Or build from source: https://github.com/KhronosGroup/SPIRV-Cross
#[allow(dead_code)]
pub fn spirv_to_wgsl(spirv_bytes: &[u8], shader_name: &str) -> Result<String> {
    use std::process::Command;
    
    // Validate magic number
    if spirv_bytes.len() >= 4 {
        let magic = u32::from_le_bytes([spirv_bytes[0], spirv_bytes[1], spirv_bytes[2], spirv_bytes[3]]);
        if magic == 0x07230203 {
            eprintln!("[SPIRV] SPIR-V shader {} received ({} bytes)", shader_name, spirv_bytes.len());
        } else {
            return Err(anyhow!("[SPIRV] Invalid SPIR-V magic number: {:#x}", magic));
        }
    } else {
        return Err(anyhow!("[SPIRV] SPIR-V data too small: {} bytes", spirv_bytes.len()));
    }
    
    // Get spirv-cross CLI path (embedded binary from build or system PATH)
    let cli_path = get_spirv_cross_cli_path()?;
    eprintln!("[SPIRV] Using spirv-cross CLI: {}", cli_path);
    
    // Create temporary directory for spirv-cross I/O
    let temp_dir = std::env::temp_dir().join(format!("spirv-cross-{}", 
        std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    
    let spirv_path = temp_dir.join("shader.spv");
    let wgsl_path = temp_dir.join("shader.wgsl");
    
    // Write SPIR-V bytes to temporary file
    std::fs::write(&spirv_path, spirv_bytes)?;
    eprintln!("[SPIRV] Wrote {} bytes to {}", spirv_bytes.len(), spirv_path.display());
    
    // Run spirv-cross to convert SPIR-V to WGSL
    eprintln!("[SPIRV] Running: {} --language wgsl {} --output {}", 
        cli_path, spirv_path.display(), wgsl_path.display());
    
    let output = Command::new(&cli_path)
        .arg("--language")
        .arg("wgsl")
        .arg(&spirv_path)
        .arg("--output")
        .arg(&wgsl_path)
        .output()?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("[SPIRV] ✗ spirv-cross conversion failed");
        eprintln!("[SPIRV] stderr: {}", stderr);
        eprintln!("[SPIRV] stdout: {}", stdout);
        return Err(anyhow!("spirv-cross failed: {}", stderr));
    }
    
    // Read WGSL output
    let wgsl_source = std::fs::read_to_string(&wgsl_path)?;
    eprintln!("[SPIRV] ✓ spirv-cross produced {} lines of WGSL ({} bytes)", 
        wgsl_source.lines().count(), wgsl_source.len());
    
    // Clean up temp files
    let _ = std::fs::remove_file(&spirv_path);
    let _ = std::fs::remove_file(&wgsl_path);
    let _ = std::fs::remove_dir(&temp_dir);
    
    Ok(wgsl_source)
}

/// Find spirv-cross CLI binary (embedded in build or system PATH)
fn get_spirv_cross_cli_path() -> Result<String> {
    eprintln!("[SPIRV] Searching for spirv-cross CLI tool...");
    
    // First, check the environment variable set by build.rs (embedded binary)
    if let Ok(cli_path) = std::env::var("SPIRV_CROSS_CLI") {
        eprintln!("[SPIRV] ✓ Found SPIRV_CROSS_CLI from build: {}", cli_path);
        if std::path::Path::new(&cli_path).exists() {
            eprintln!("[SPIRV] ✓ Embedded spirv-cross CLI ready: {}", cli_path);
            return Ok(cli_path);
        } else {
            eprintln!("[SPIRV] ✗ SPIRV_CROSS_CLI path does not exist: {}", cli_path);
        }
    }
    
    // Fallback: Try to find spirv-cross in PATH
    eprintln!("[SPIRV] Fallback: Searching for spirv-cross in PATH...");
    let candidates = if cfg!(windows) {
        vec!["spirv-cross.exe", "spirv-cross"]
    } else {
        vec!["spirv-cross", "./spirv-cross"]
    };
    
    for candidate in candidates {
        eprintln!("[SPIRV] Trying: {}", candidate);
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok()
        {
            eprintln!("[SPIRV] ✓ Found spirv-cross in PATH: {}", candidate);
            return Ok(candidate.to_string());
        }
    }
    
    eprintln!("[SPIRV] ✗ spirv-cross CLI tool not found anywhere");
    eprintln!("[SPIRV]   Embedded binary should have been built if CMake was available");
    eprintln!("[SPIRV]   Fallback install options:");
    eprintln!("[SPIRV]     - Ubuntu/Debian: apt install spirv-cross");
    eprintln!("[SPIRV]     - macOS: brew install spirv-cross");
    eprintln!("[SPIRV]     - Windows: Download from https://github.com/KhronosGroup/SPIRV-Cross/releases");
    Err(anyhow!("spirv-cross CLI not found. Rebuild the project with CMake available."))
}

/// Create a wgpu shader module from SPIR-V bytes
/// 
/// This is the main public interface for loading BNSH-decoded SPIR-V into wgpu
#[allow(dead_code)]
pub fn create_shader_module_from_spirv(
    device: &wgpu::Device,
    spirv_bytes: &[u8],
    shader_name: &str,
) -> Result<wgpu::ShaderModule> {
    // Convert SPIR-V to WGSL
    let wgsl_source = spirv_to_wgsl(spirv_bytes, shader_name)?;

    // Create shader module from WGSL
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader_name),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spirv_conversion_not_implemented() {
        // All SPIR-V conversion returns an error (not yet implemented)
        let result = spirv_to_wgsl(&[], "test");
        assert!(result.is_err());
        
        // Even valid magic number fails (feature not yet implemented)
        let mut valid_header = [0x03u8, 0x02, 0x23, 0x07]; // SPIR-V magic
        let result = spirv_to_wgsl(&valid_header, "test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }
}
