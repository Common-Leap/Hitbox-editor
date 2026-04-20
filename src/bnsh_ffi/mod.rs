// BNSH FFI wrapper - placeholder for bnsh-decoder integration
// This module will eventually interface with bnsh-decoder to convert BNSH → SPIR-V

pub mod lib;

use anyhow::{Result, anyhow};

/// Metadata about a decoded BNSH shader
#[derive(Debug, Clone)]
pub struct BnshDecodeResult {
    pub spirv: Vec<u32>,        // SPIR-V module as u32 words
    pub entry_point: String,    // e.g., "main"
    pub stage: ShaderStage,      // Vertex, Fragment, Compute
    pub source_format: String,  // e.g., "HLSL", "Glsl"
    pub sampler_count: u32,     // Number of samplers
    pub uniform_buffer_count: u32, // Number of uniform buffers
    pub shader_index: u32,      // 1 for shader_binary_1, 2 for shader_binary_2 (for context)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
    Unknown,
}

impl From<u32> for ShaderStage {
    fn from(stage: u32) -> Self {
        match stage {
            0 => ShaderStage::Fragment,
            1 => ShaderStage::Vertex,
            2..=4 => ShaderStage::Unknown, // Tessellation, Geometry
            5 => ShaderStage::Compute,
            _ => ShaderStage::Unknown,
        }
    }
}

impl ToString for ShaderStage {
    fn to_string(&self) -> String {
        match self {
            ShaderStage::Vertex => "Vertex".to_string(),
            ShaderStage::Fragment => "Fragment".to_string(),
            ShaderStage::Compute => "Compute".to_string(),
            ShaderStage::Unknown => "Unknown".to_string(),
        }
    }
}

/// Result of BNSH decoding - contains both SPIR-V and placeholder WGSL
#[derive(Debug, Clone)]
pub struct WgslDecodeResult {
    pub wgsl: String,               // WGSL shader source (placeholder)
    pub spirv: Vec<u8>,             // SPIR-V bytes for wgpu
    pub entry_point: String,        // Entry point name
    pub is_fragment: bool,          // True if fragment shader
    pub sampler_count: u32,
    pub uniform_buffer_count: u32,
}

/// Internal structure for parsed shader metadata
#[derive(Debug)]
struct ShaderMetadata {
    entry_point: String,
    stage: ShaderStage,
    source_format: String,
    sampler_count: u32,
    uniform_buffer_count: u32,
}

/// BNSH decoder interface
pub struct BnshDecoder;

impl BnshDecoder {
    /// Decode a BNSH binary to SPIR-V using the bnsh-decoder CLI tool
    pub fn decode_to_spirv(bnsh_data: &[u8]) -> Result<Vec<u32>> {
        if bnsh_data.len() < 16 {
            return Err(anyhow!("BNSH data too short: {} bytes", bnsh_data.len()));
        }

        // Get the CLI tool path
        let cli_path = Self::get_cli_path()?;
        
        // Create temporary directory for I/O
        let temp_dir = std::env::temp_dir().join(format!("bnsh-decoder-{}", 
            std::process::id()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| anyhow!("Failed to create temp directory: {}", e))?;
        
        let input_path = temp_dir.join("shader.bnsh");
        let output_spirv = temp_dir.join("shader.spv");
        let output_json = temp_dir.join("shader.json");
        
        // Write BNSH data to temporary file
        std::fs::write(&input_path, bnsh_data)
            .map_err(|e| anyhow!("Failed to write BNSH temp file: {}", e))?;
        
        // Run bnsh-decoder CLI
        eprintln!("[BNSH] Decoding {} bytes with bnsh-decoder CLI...", bnsh_data.len());
        
        let output = std::process::Command::new(&cli_path)
            .arg("--input").arg(&input_path)
            .arg("--output-spirv").arg(&output_spirv)
            .arg("--output-json").arg(&output_json)
            .output()
            .map_err(|e| anyhow!("Failed to execute bnsh-decoder CLI: {}", e))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            eprintln!("[BNSH] CLI stderr: {}", stderr);
            eprintln!("[BNSH] CLI stdout: {}", stdout);
            return Err(anyhow!("bnsh-decoder CLI failed: {}", stderr));
        }
        
        // Read SPIR-V output
        let spirv_bytes = std::fs::read(&output_spirv)
            .map_err(|e| anyhow!("Failed to read SPIR-V output: {}", e))?;
        
        // Convert bytes to u32 words (SPIR-V is little-endian)
        if spirv_bytes.len() % 4 != 0 {
            return Err(anyhow!("SPIR-V output size {} is not a multiple of 4", spirv_bytes.len()));
        }
        
        let mut spirv_words = vec![0u32; spirv_bytes.len() / 4];
        for (i, chunk) in spirv_bytes.chunks_exact(4).enumerate() {
            spirv_words[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        
        // Verify SPIR-V magic number
        if spirv_words.is_empty() || spirv_words[0] != 0x07230203 {
            return Err(anyhow!("Invalid SPIR-V output: missing or invalid magic number"));
        }
        
        eprintln!("[BNSH] Decoded {} SPIR-V words", spirv_words.len());
        
        // Clean up temp files
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_spirv);
        let _ = std::fs::remove_file(&output_json);
        let _ = std::fs::remove_dir(&temp_dir);
        
        Ok(spirv_words)
    }

    /// Decode a BNSH binary with full metadata extraction
    pub fn decode_with_metadata(bnsh_data: &[u8]) -> Result<BnshDecodeResult> {
        Self::decode_with_metadata_and_index(bnsh_data, 0)
    }
    
    pub fn decode_with_metadata_and_index(bnsh_data: &[u8], shader_index: u32) -> Result<BnshDecodeResult> {
        if bnsh_data.len() < 16 {
            return Err(anyhow!("BNSH data too short: {} bytes", bnsh_data.len()));
        }

        let cli_path = Self::get_cli_path()?;
        
        // Create temporary directory for I/O
        let temp_dir = std::env::temp_dir().join(format!("bnsh-decoder-meta-{}", 
            std::process::id()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| anyhow!("Failed to create temp directory: {}", e))?;
        
        let input_path = temp_dir.join("shader.bnsh");
        let output_spirv = temp_dir.join("shader.spv");
        let output_json = temp_dir.join("shader.json");
        
        // Write BNSH data to temporary file
        std::fs::write(&input_path, bnsh_data)
            .map_err(|e| anyhow!("Failed to write BNSH temp file: {}", e))?;
        
        // Run bnsh-decoder CLI
        eprintln!("[BNSH] Decoding {} bytes with metadata extraction...", bnsh_data.len());
        
        let output = std::process::Command::new(&cli_path)
            .arg("--input").arg(&input_path)
            .arg("--output-spirv").arg(&output_spirv)
            .arg("--output-json").arg(&output_json)
            .output()
            .map_err(|e| anyhow!("Failed to execute bnsh-decoder CLI: {}", e))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("bnsh-decoder CLI failed: {}", stderr));
        }
        
        // Read SPIR-V
        let spirv_bytes = std::fs::read(&output_spirv)
            .map_err(|e| anyhow!("Failed to read SPIR-V output: {}", e))?;
        
        let mut spirv_words = vec![0u32; spirv_bytes.len() / 4];
        for (i, chunk) in spirv_bytes.chunks_exact(4).enumerate() {
            spirv_words[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        
        // Read and parse JSON metadata
        let json_text = std::fs::read_to_string(&output_json)
            .unwrap_or_else(|_| "{}".to_string());
        
        let metadata = Self::parse_shader_metadata(&json_text)?;
        
        // Clean up temp files
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_spirv);
        let _ = std::fs::remove_file(&output_json);
        let _ = std::fs::remove_dir(&temp_dir);
        
        Ok(BnshDecodeResult {
            spirv: spirv_words,
            entry_point: metadata.entry_point,
            stage: metadata.stage,
            source_format: metadata.source_format,
            sampler_count: metadata.sampler_count,
            uniform_buffer_count: metadata.uniform_buffer_count,
            shader_index,
        })
    }

    /// Get the path to the bnsh-decoder CLI tool
    fn get_cli_path() -> Result<String> {
        eprintln!("[BNSH_FFI] Searching for bnsh-decoder CLI tool...");
        
        // First, check the environment variable set by build.rs
        if let Ok(cli_path) = std::env::var("BNSH_DECODER_CLI") {
            eprintln!("[BNSH_FFI] Found BNSH_DECODER_CLI env var: {}", cli_path);
            if std::path::Path::new(&cli_path).exists() {
                eprintln!("[BNSH_FFI] ✓ BNSH_DECODER_CLI path exists: {}", cli_path);
                return Ok(cli_path);
            }
            eprintln!("[BNSH_FFI] ✗ BNSH_DECODER_CLI path does not exist: {}", cli_path);
        }
        
        // Try common locations
        let candidates = vec![
            "bnsh-decoder",
            "./bnsh-decoder",
            "/usr/local/bin/bnsh-decoder",
            "/usr/bin/bnsh-decoder",
            "bnsh-decoder.exe",
            "./bnsh-decoder.exe",
        ];
        
        eprintln!("[BNSH_FFI] Trying {} candidate paths...", candidates.len());
        
        for candidate in candidates {
            eprintln!("[BNSH_FFI] Trying: {}", candidate);
            if std::process::Command::new(candidate)
                .arg("--help")
                .output()
                .is_ok()
            {
                eprintln!("[BNSH_FFI] ✓ Found bnsh-decoder: {}", candidate);
                return Ok(candidate.to_string());
            }
        }
        
        eprintln!("[BNSH_FFI] ✗ bnsh-decoder CLI tool not found in any location");
        Err(anyhow!("bnsh-decoder CLI tool not found. Please ensure bnsh-decoder is built and in PATH"))
    }
    
    /// Parse shader metadata from JSON
    fn parse_shader_metadata(json_str: &str) -> Result<ShaderMetadata> {
        use serde_json::json;
        
        eprintln!("[BNSH_FFI] JSON metadata: {}", json_str);
        
        let metadata = serde_json::from_str(json_str)
            .unwrap_or_else(|_| json!({}));
        
        // Extract entry point
        let entry_point = metadata
            .get("entryPoint")
            .and_then(|v| v.as_str())
            .unwrap_or("main")
            .to_string();
        
        eprintln!("[BNSH_FFI] Entry point: {}", entry_point);
        
        // Determine stage from entry point or explicit field
        let stage_name = metadata
            .get("stage")
            .and_then(|v| v.as_str())
            .or_else(|| {
                if entry_point.contains("fragment") || entry_point.contains("frag") {
                    Some("fragment")
                } else if entry_point.contains("vertex") || entry_point.contains("vert") {
                    Some("vertex")
                } else if entry_point.contains("compute") {
                    Some("compute")
                } else {
                    None
                }
            })
            .unwrap_or("fragment");
        
        eprintln!("[BNSH_FFI] Detected stage: {}", stage_name);
        
        let stage = if stage_name.contains("vertex") {
            ShaderStage::Vertex
        } else if stage_name.contains("compute") {
            ShaderStage::Compute
        } else {
            ShaderStage::Fragment
        };
        
        // Count samplers and uniform buffers
        let samplers = metadata
            .get("samplers")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0);
        
        let uniform_buffers = metadata
            .get("uniformBuffers")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0);
        
        Ok(ShaderMetadata {
            entry_point,
            stage,
            source_format: "HLSL".to_string(),
            sampler_count: samplers,
            uniform_buffer_count: uniform_buffers,
        })
    }

    /// Decode a BNSH binary with optional shader index context
    pub fn decode_wgsl_with_index(bnsh_data: &[u8], shader_index: u32) -> Result<WgslDecodeResult> {
        let mut decode_result = Self::decode_with_metadata_and_index(bnsh_data, shader_index)?;
        
        // If stage couldn't be determined from metadata, use index as hint
        // In particle systems: shader_binary_1 is typically vertex, shader_binary_2 is fragment
        if decode_result.stage == ShaderStage::Fragment && shader_index == 1 {
            eprintln!("[BNSH] Correcting stage: shader_binary_1 assumed to be vertex");
            decode_result.stage = ShaderStage::Vertex;
        }
        
        // Convert SPIR-V u32 words to u8 bytes for wgpu (little-endian)
        let spirv_bytes: Vec<u8> = decode_result.spirv.iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        
        eprintln!("[BNSH] ✓ Decoded SPIR-V: {} words = {} bytes", 
            decode_result.spirv.len(), spirv_bytes.len());

        Ok(WgslDecodeResult {
            wgsl: String::new(),  // Empty - we use SPIR-V directly
            spirv: spirv_bytes,
            entry_point: decode_result.entry_point,
            is_fragment: decode_result.stage == ShaderStage::Fragment,
            sampler_count: decode_result.sampler_count,
            uniform_buffer_count: decode_result.uniform_buffer_count,
        })
    }
    
    /// Backwards-compatible method for normal decode (used in tests)
    pub fn decode_wgsl(bnsh_data: &[u8]) -> Result<WgslDecodeResult> {
        Self::decode_wgsl_with_index(bnsh_data, 0)
    }


}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_metadata_extraction() {
        let result = BnshDecodeResult {
            spirv: vec![0x07230203, 0x00010000],
            entry_point: "fs_main".to_string(),
            stage: ShaderStage::Fragment,
            source_format: "HLSL".to_string(),
            sampler_count: 2,
            uniform_buffer_count: 1,
            shader_index: 1,
        };
        
        assert_eq!(result.entry_point, "fs_main");
        assert_eq!(result.stage, ShaderStage::Fragment);
        assert_eq!(result.sampler_count, 2);
        assert_eq!(result.uniform_buffer_count, 1);
    }

    #[test]
    fn test_shader_stage_conversion() {
        assert_eq!(ShaderStage::from(0u32), ShaderStage::Fragment);
        assert_eq!(ShaderStage::from(1u32), ShaderStage::Vertex);
        assert_eq!(ShaderStage::from(5u32), ShaderStage::Compute);
        assert_eq!(ShaderStage::from(99u32), ShaderStage::Unknown);
    }

    #[test]
    fn test_decode_too_short() {
        let short_data = vec![0u8; 8];
        let result = BnshDecoder::decode_to_spirv(&short_data);
        assert!(result.is_err());
    }
}
