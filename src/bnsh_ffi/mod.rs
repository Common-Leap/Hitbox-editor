// BNSH FFI wrapper - placeholder for bnsh-decoder integration
// This module will eventually interface with bnsh-decoder to convert BNSH → SPIR-V

pub mod lib;

use anyhow::{Result, anyhow};

// ── Deterministic decode cache ─────────────────────────────────────────────────────────────
// The external bnsh-decoder CLI produces different SPIR-V across process launches for identical
// input, which makes rendering non-reproducible. Memoize the decoded result by BNSH content hash
// so the first decode wins and every later run (this process or another) is identical.
// `HITBOX_SHADER_CACHE=0` bypasses the cache (also disables the WGSL cache).

fn decode_cache_enabled() -> bool {
    !matches!(std::env::var("HITBOX_SHADER_CACHE").as_deref(), Ok("0"))
}

fn decode_cache_key(bnsh_data: &[u8], shader_index: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bnsh_data);
    h.update(shader_index.to_le_bytes());
    format!("{:x}", h.finalize())
}

fn decode_cache_path(key: &str) -> std::path::PathBuf {
    crate::scratch_dirs::bnsh_decode_cache_root().join(format!("{key}.bnshc"))
}

fn decode_cache_get(bnsh_data: &[u8], shader_index: u32) -> Option<BnshDecodeResult> {
    if !decode_cache_enabled() {
        return None;
    }
    let data = std::fs::read(decode_cache_path(&decode_cache_key(bnsh_data, shader_index))).ok()?;
    bincode::deserialize::<BnshDecodeResult>(&data).ok()
}

fn decode_cache_put(bnsh_data: &[u8], shader_index: u32, value: &BnshDecodeResult) {
    if !decode_cache_enabled() {
        return;
    }
    let dir = crate::scratch_dirs::bnsh_decode_cache_root();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(bytes) = bincode::serialize(value) {
        let _ = std::fs::write(
            decode_cache_path(&decode_cache_key(bnsh_data, shader_index)),
            bytes,
        );
    }
}

/// Metadata about a decoded BNSH shader
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BnshDecodeResult {
    pub spirv: Vec<u32>,        // SPIR-V module as u32 words
    pub entry_point: String,    // e.g., "main"
    pub stage: ShaderStage,      // Vertex, Fragment, Compute
    pub source_format: String,  // e.g., "HLSL", "Glsl"
    pub sampler_count: u32,     // Number of samplers
    pub uniform_buffer_count: u32, // Number of uniform buffers
    pub shader_index: u32,      // 1 for shader_binary_1, 2 for shader_binary_2 (for context)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    spirv_length: Option<u32>,
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
        
        // Create unique temporary directory for I/O (on disk, not /tmp tmpfs)
        let temp_dir = crate::scratch_dirs::app_scratch_dir("bnsh-")
            .map_err(|e| anyhow!("Failed to create temp directory: {}", e))?;
        let temp_dir_path = temp_dir.path().to_path_buf();
        
        let input_path = temp_dir_path.join("shader.bnsh");
        let output_spirv = temp_dir_path.join("shader.spv");
        
        // Write BNSH data to temporary file
        std::fs::write(&input_path, bnsh_data)
            .map_err(|e| anyhow!("Failed to write BNSH temp file: {}", e))?;
        
        // Run bnsh-decoder CLI
        if crate::fx_debug_enabled() {
            eprintln!("[BNSH] Decoding {} bytes with bnsh-decoder CLI...", bnsh_data.len());
        }
        
        let output = std::process::Command::new(&cli_path)
            .arg("--input").arg(&input_path)
            .arg("--output-spirv").arg(&output_spirv)
            .output()
            .map_err(|e| anyhow!("Failed to execute bnsh-decoder CLI: {}", e))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let code = output.status.code().unwrap_or(-1);
            eprintln!("[BNSH] CLI stderr: {}", stderr);
            eprintln!("[BNSH] CLI stdout: {}", stdout);
            if stderr.trim().is_empty() && stdout.trim().is_empty() {
                return Err(anyhow!(
                    "bnsh-decoder CLI failed (exit {code}, no output; input {} bytes)",
                    bnsh_data.len()
                ));
            }
            if !stderr.trim().is_empty() {
                return Err(anyhow!("bnsh-decoder CLI failed: {stderr}"));
            }
            return Err(anyhow!("bnsh-decoder CLI failed (exit {code}): {stdout}"));
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
        
        // temp_dir is dropped here, automatically cleaning up
        
        Ok(spirv_words)
    }

    /// Decode a BNSH binary with full metadata extraction
    pub fn decode_with_metadata(bnsh_data: &[u8]) -> Result<BnshDecodeResult> {
        Self::decode_with_metadata_and_index(bnsh_data, 0)
    }
    
    pub fn decode_with_metadata_and_index(bnsh_data: &[u8], shader_index: u32) -> Result<BnshDecodeResult> {
        if let Some(hit) = decode_cache_get(bnsh_data, shader_index) {
            return Ok(hit);
        }
        let result = Self::decode_with_metadata_and_index_uncached(bnsh_data, shader_index)?;
        decode_cache_put(bnsh_data, shader_index, &result);
        Ok(result)
    }

    fn decode_with_metadata_and_index_uncached(bnsh_data: &[u8], shader_index: u32) -> Result<BnshDecodeResult> {
        if bnsh_data.len() < 16 {
            return Err(anyhow!("BNSH data too short: {} bytes", bnsh_data.len()));
        }

        let cli_path = Self::get_cli_path()?;
        
        // Create unique temporary directory for I/O (on disk, not /tmp tmpfs)
        let temp_dir = crate::scratch_dirs::app_scratch_dir("bnsh-")
            .map_err(|e| anyhow!("Failed to create temp directory: {}", e))?;
        let temp_dir_path = temp_dir.path().to_path_buf();
        
        let input_path = temp_dir_path.join("shader.bnsh");
        let output_spirv = temp_dir_path.join("shader.spv");
        let output_json = temp_dir_path.join("shader.json");
        
        // Write BNSH data to temporary file
        std::fs::write(&input_path, bnsh_data)
            .map_err(|e| anyhow!("Failed to write BNSH temp file: {}", e))?;
        
        // Run bnsh-decoder CLI
        if crate::fx_debug_enabled() {
            eprintln!("[BNSH] Decoding {} bytes with metadata extraction...", bnsh_data.len());
        }
        
        let output = std::process::Command::new(&cli_path)
            .arg("--input").arg(&input_path)
            .arg("--output-spirv").arg(&output_spirv)
            .arg("--output-json").arg(&output_json)
            .output()
            .map_err(|e| anyhow!("Failed to execute bnsh-decoder CLI: {}", e))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let code = output.status.code().unwrap_or(-1);
            if stderr.trim().is_empty() && stdout.trim().is_empty() {
                return Err(anyhow!(
                    "bnsh-decoder CLI failed (exit {code}, no output; input {} bytes)",
                    bnsh_data.len()
                ));
            }
            if !stderr.trim().is_empty() {
                return Err(anyhow!("bnsh-decoder CLI failed: {stderr}"));
            }
            return Err(anyhow!("bnsh-decoder CLI failed (exit {code}): {stdout}"));
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
        
        let metadata = Self::parse_shader_metadata(&json_text, shader_index)?;

        if let Some(expected) = metadata.spirv_length {
            if expected as usize != spirv_words.len() {
                eprintln!(
                    "[BNSH_FFI] spirvLength mismatch: json={expected} actual={}",
                    spirv_words.len()
                );
            }
        }
        
        // temp_dir is dropped here, automatically cleaning up
        
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
        use std::sync::OnceLock;
        static CLI: OnceLock<Result<String, String>> = OnceLock::new();
        CLI.get_or_init(|| Self::discover_cli_path().map_err(|e| e.to_string()))
            .clone()
            .map_err(|e| anyhow!(e))
    }

    fn discover_cli_path() -> Result<String> {
        eprintln!("[BNSH_FFI] Searching for bnsh-decoder CLI tool...");
        
        // First, check the compile-time embedded path from build.rs (set via cargo:rustc-env).
        // NOTE: must use option_env!(), NOT std::env::var() — build.rs uses cargo:rustc-env
        // which is compile-time only and invisible to the runtime process environment.
        if let Some(cli_path) = option_env!("BNSH_DECODER_CLI") {
            eprintln!("[BNSH_FFI] ✓ Found BNSH_DECODER_CLI from build: {}", cli_path);
            if std::path::Path::new(cli_path).exists() {
                eprintln!("[BNSH_FFI] ✓ Embedded bnsh-decoder CLI ready: {}", cli_path);
                return Ok(cli_path.to_string());
            } else {
                eprintln!("[BNSH_FFI] ✗ BNSH_DECODER_CLI path does not exist: {}", cli_path);
                eprintln!("[BNSH_FFI]   (This may indicate a build issue - was CMake available?)");
            }
        }
        
        // Fallback: Try to find bnsh-decoder in common PATH locations
        eprintln!("[BNSH_FFI] Fallback: Searching for bnsh-decoder in PATH...");
        let candidates = if cfg!(windows) {
            vec!["bnsh-decoder.exe", "bnsh-decoder", "CLI.exe", "CLI"]
        } else {
            vec!["bnsh-decoder", "./bnsh-decoder", "CLI"]
        };
        
        for candidate in candidates {
            eprintln!("[BNSH_FFI] Trying: {}", candidate);
            if std::process::Command::new(candidate)
                .arg("--help")
                .output()
                .is_ok()
            {
                eprintln!("[BNSH_FFI] ✓ Found bnsh-decoder in PATH: {}", candidate);
                return Ok(candidate.to_string());
            }
        }
        
        eprintln!("[BNSH_FFI] ✗ bnsh-decoder CLI tool not found anywhere");
        eprintln!("[BNSH_FFI]   Embedded binary should have been built and linked");
        eprintln!("[BNSH_FFI]   If building from source, ensure CMake is installed:");
        eprintln!("[BNSH_FFI]     - Ubuntu: apt install cmake");
        eprintln!("[BNSH_FFI]     - macOS: brew install cmake");
        eprintln!("[BNSH_FFI]     - Windows: Download from https://cmake.org/download/");
        Err(anyhow!("bnsh-decoder CLI not found. Rebuild the project with CMake available."))
    }
    
    /// Parse shader metadata from bnsh-decoder JSON (`GenerateJSON` in cli.cpp).
    ///
    /// Schema: spirvLength, constantBuffers[{index,maxOffset,size}], samplers[{index,offset,isShadow}],
    /// inputAttributes[], outputAttributes[]. Entry point and stage are not emitted; stage is inferred
    /// from attribute usage, with shader_index as a legacy fallback (1=vertex, 2=fragment).
    fn parse_shader_metadata(json_str: &str, shader_index: u32) -> Result<ShaderMetadata> {
        use serde_json::json;

        let metadata = serde_json::from_str(json_str).unwrap_or_else(|_| json!({}));

        // bnsh-decoder does not emit entry point names; SPIR-V uses "main".
        let entry_point = "main".to_string();

        let input_attributes = json_u64_array(metadata.get("inputAttributes"));
        let output_attributes = json_u64_array(metadata.get("outputAttributes"));

        let mut stage = infer_stage_from_attributes(&input_attributes, &output_attributes)
            .unwrap_or(ShaderStage::Unknown);
        stage = apply_shader_index_fallback(stage, shader_index);

        if crate::fx_debug_enabled() {
            eprintln!("[BNSH_FFI] Detected stage: {}", stage.to_string());
        }

        let sampler_count = metadata
            .get("samplers")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0);

        let uniform_buffer_count = metadata
            .get("constantBuffers")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0);

        let spirv_length = metadata
            .get("spirvLength")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);

        Ok(ShaderMetadata {
            entry_point,
            stage,
            source_format: "HLSL".to_string(),
            sampler_count,
            uniform_buffer_count,
            spirv_length,
        })
    }

    /// Decode a BNSH binary with optional shader index context
    pub fn decode_wgsl_with_index(bnsh_data: &[u8], shader_index: u32) -> Result<WgslDecodeResult> {
        let decode_result = Self::decode_with_metadata_and_index(bnsh_data, shader_index)?;

        // Note: naga's SPIR-V frontend handles capabilities with strict_capabilities: false.
        // We do NOT strip OpCapability instructions here — replacing them with OpNop would
        // cause naga 29.0.1 to error with UnsupportedInstruction(Empty, Nop) since it
        // doesn't implement Op::Nop.

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

fn json_u64_array(value: Option<&serde_json::Value>) -> Vec<u64> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// Infer shader stage from Tegra attribute indices in bnsh-decoder JSON.
///
/// Vertex shaders read vertex attributes (8–39) and write Position (7).
/// Fragment shaders read varyings such as PointCoord (46) and FrontFacing (63).
fn infer_stage_from_attributes(input: &[u64], output: &[u64]) -> Option<ShaderStage> {
    const POSITION: u64 = 7;
    const ATTRIBUTE_0: u64 = 8;
    const ATTRIBUTE_31: u64 = 39;
    const POINT_COORD: u64 = 46;
    const FRONT_FACING: u64 = 63;

    let has_vertex_attr_input = input
        .iter()
        .any(|&a| (ATTRIBUTE_0..=ATTRIBUTE_31).contains(&a));
    let has_position_output = output.contains(&POSITION);
    let has_fragment_varying_input = input.iter().any(|&a| a == POINT_COORD || a == FRONT_FACING);
    let has_position_input = input.contains(&POSITION);

    if has_vertex_attr_input || has_position_output {
        return Some(ShaderStage::Vertex);
    }
    if has_fragment_varying_input || has_position_input {
        return Some(ShaderStage::Fragment);
    }
    None
}

/// Legacy particle-effect convention when attribute inference is inconclusive.
fn apply_shader_index_fallback(stage: ShaderStage, shader_index: u32) -> ShaderStage {
    if stage != ShaderStage::Unknown {
        return stage;
    }
    match shader_index {
        1 => ShaderStage::Vertex,
        2 => ShaderStage::Fragment,
        _ => ShaderStage::Fragment,
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

    #[test]
    fn test_parse_shader_metadata_real_schema_vertex() {
        let json = r#"{
            "spirvLength": 1234,
            "constantBuffers": [
                {"index": 0, "maxOffset": 256, "size": 512},
                {"index": 1, "maxOffset": 64, "size": 128}
            ],
            "samplers": [
                {"index": 0, "offset": 16, "isShadow": 0},
                {"index": 1, "offset": 32, "isShadow": 1}
            ],
            "inputAttributes": [8, 9, 10],
            "outputAttributes": [7, 48]
        }"#;

        let meta = BnshDecoder::parse_shader_metadata(json, 0).unwrap();
        assert_eq!(meta.entry_point, "main");
        assert_eq!(meta.stage, ShaderStage::Vertex);
        assert_eq!(meta.sampler_count, 2);
        assert_eq!(meta.uniform_buffer_count, 2);
        assert_eq!(meta.spirv_length, Some(1234));
    }

    #[test]
    fn test_parse_shader_metadata_real_schema_fragment() {
        let json = r#"{
            "spirvLength": 500,
            "constantBuffers": [],
            "samplers": [{"index": 0, "offset": 0, "isShadow": 0}],
            "inputAttributes": [7, 48, 63],
            "outputAttributes": [40]
        }"#;

        let meta = BnshDecoder::parse_shader_metadata(json, 0).unwrap();
        assert_eq!(meta.stage, ShaderStage::Fragment);
        assert_eq!(meta.sampler_count, 1);
        assert_eq!(meta.uniform_buffer_count, 0);
    }

    #[test]
    fn test_parse_shader_metadata_shader_index_fallback() {
        let json = r#"{
            "spirvLength": 100,
            "constantBuffers": [],
            "samplers": [],
            "inputAttributes": [],
            "outputAttributes": []
        }"#;

        let vs = BnshDecoder::parse_shader_metadata(json, 1).unwrap();
        assert_eq!(vs.stage, ShaderStage::Vertex);

        let fs = BnshDecoder::parse_shader_metadata(json, 2).unwrap();
        assert_eq!(fs.stage, ShaderStage::Fragment);
    }

    #[test]
    fn test_infer_stage_from_attributes() {
        assert_eq!(
            infer_stage_from_attributes(&[8, 9], &[7]),
            Some(ShaderStage::Vertex)
        );
        assert_eq!(
            infer_stage_from_attributes(&[7, 63], &[40]),
            Some(ShaderStage::Fragment)
        );
        assert_eq!(infer_stage_from_attributes(&[], &[]), None);
    }
}
