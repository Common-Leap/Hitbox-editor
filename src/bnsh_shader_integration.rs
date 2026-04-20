// Integrates BNSH shader decoding with particle_renderer
// Provides functions to load BNSH shaders from effect files into wgpu pipelines
//
// The C++ BNSH decoder (https://github.com/maierfelix/bnsh-decoder) outputs SPIR-V,
// which we convert to WGSL using naga for immediate wgpu use without overhead.

use anyhow::Result;
use crate::bnsh_ffi::BnshDecoder;
use crate::effects::PtclFile;

/// A decoded shader ready for wgpu pipeline creation
/// 
/// Contains both SPIR-V bytes and WGSL source code
#[derive(Debug, Clone)]
pub struct DecodedShader {
    pub spirv: Vec<u8>,        // SPIR-V bytes for direct wgpu use
    pub wgsl_source: String,   // WGSL shader code (fallback)
    pub entry_point: String,   // e.g., "main" or "vs_main"
    pub sampler_count: u32,
    pub uniform_buffer_count: u32,
}

impl DecodedShader {
    /// Get shader source as bytes for validation or debugging
    pub fn source_bytes(&self) -> Vec<u8> {
        self.wgsl_source.as_bytes().to_vec()
    }

    /// Get a summary of the shader (line count, byte count)
    pub fn summary(&self) -> String {
        format!(
            "WGSL shader: {} lines, {} bytes, entry_point={}",
            self.wgsl_source.lines().count(),
            self.wgsl_source.len(),
            self.entry_point
        )
    }
}

/// A pair of vertex/fragment shaders extracted from an effect file
#[derive(Debug, Clone)]
pub struct EffectShaderPair {
    pub vertex: Option<DecodedShader>,
    pub fragment: Option<DecodedShader>,
    pub compute: Option<DecodedShader>,
}

/// Extract and decode shaders from a PTCL effect file
/// 
/// # Arguments
/// * `ptcl` - Parsed effect file containing shader binaries
/// 
/// # Returns
/// Decoded shaders ready for GPU pipeline creation
/// 
/// # Notes
/// - Uses the real bnsh-decoder from https://github.com/maierfelix/bnsh-decoder
/// - Converts BNSH → SPIR-V → WGSL automatically via naga
/// - shader_binary_1 (GRSN section) is typically vertex/geometry
/// - shader_binary_2 (GRSC section) is typically fragment/compute
pub fn decode_effect_shaders(ptcl: &PtclFile) -> Result<EffectShaderPair> {
    let mut pair = EffectShaderPair {
        vertex: None,
        fragment: None,
        compute: None,
    };

    eprintln!("[BNSH] === decode_effect_shaders START ===");
    eprintln!("[BNSH] shader_binary_1 size: {} bytes", ptcl.shader_binary_1.len());
    eprintln!("[BNSH] shader_binary_2 size: {} bytes", ptcl.shader_binary_2.len());

    // Decode first shader binary (typically vertex)
    if !ptcl.shader_binary_1.is_empty() {
        eprintln!("[BNSH] Attempting to decode shader_binary_1...");
        match BnshDecoder::decode_wgsl_with_index(&ptcl.shader_binary_1, 1) {
            Ok(wgsl_result) => {
                let shader = DecodedShader {
                    spirv: wgsl_result.spirv,
                    wgsl_source: wgsl_result.wgsl,
                    entry_point: wgsl_result.entry_point.clone(),
                    sampler_count: wgsl_result.sampler_count,
                    uniform_buffer_count: wgsl_result.uniform_buffer_count,
                };
                
                eprintln!("[BNSH] ✓ Decoded shader 1 via bnsh-decoder: {}", shader.summary());
                
                // Assign based on stage information from decode result
                if wgsl_result.is_fragment {
                    pair.fragment = Some(shader);
                } else {
                    pair.vertex = Some(shader);
                }
            }
            Err(e) => {
                eprintln!("[BNSH] ✗ Failed to decode shader binary 1: {}", e);
                // Continue with other shaders
            }
        }
    } else {
        eprintln!("[BNSH] shader_binary_1 is empty, skipping");
    }

    // Decode second shader binary (typically fragment)
    if !ptcl.shader_binary_2.is_empty() {
        eprintln!("[BNSH] Attempting to decode shader_binary_2...");
        match BnshDecoder::decode_wgsl_with_index(&ptcl.shader_binary_2, 2) {
            Ok(wgsl_result) => {
                let shader = DecodedShader {
                    spirv: wgsl_result.spirv,
                    wgsl_source: wgsl_result.wgsl,
                    entry_point: wgsl_result.entry_point.clone(),
                    sampler_count: wgsl_result.sampler_count,
                    uniform_buffer_count: wgsl_result.uniform_buffer_count,
                };
                
                eprintln!("[BNSH] ✓ Decoded shader 2 via bnsh-decoder: {}", shader.summary());
                
                // Assign based on stage information from decode result
                if wgsl_result.is_fragment {
                    pair.fragment = Some(shader);
                } else {
                    pair.vertex = Some(shader);
                }
            }
            Err(e) => {
                eprintln!("[BNSH] ✗ Failed to decode shader binary 2: {}", e);
                // Continue with what we have
            }
        }
    } else {
        eprintln!("[BNSH] shader_binary_2 is empty, skipping");
    }

    // Validate we have at least vertex and fragment
    if pair.vertex.is_none() || pair.fragment.is_none() {
        eprintln!("[BNSH] Warning: Effect file missing vertex or fragment shader");
        if pair.vertex.is_none() {
            eprintln!("  - No vertex shader decoded");
        }
        if pair.fragment.is_none() {
            eprintln!("  - No fragment shader decoded");
        }
    }

    Ok(pair)
}

/// Get summary stats about decoded shaders
pub fn get_shader_stats(pair: &EffectShaderPair) -> ShaderStats {
    let mut stats = ShaderStats::default();
    
    if let Some(shader) = &pair.vertex {
        stats.has_vertex = true;
        stats.vertex_lines = shader.wgsl_source.lines().count();
        stats.vertex_bytes = shader.wgsl_source.len();
        stats.vertex_samplers = shader.sampler_count;
        stats.vertex_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.fragment {
        stats.has_fragment = true;
        stats.fragment_lines = shader.wgsl_source.lines().count();
        stats.fragment_bytes = shader.wgsl_source.len();
        stats.fragment_samplers = shader.sampler_count;
        stats.fragment_buffers = shader.uniform_buffer_count;
    }
    
    if let Some(shader) = &pair.compute {
        stats.has_compute = true;
        stats.compute_lines = shader.wgsl_source.lines().count();
        stats.compute_bytes = shader.wgsl_source.len();
        stats.compute_samplers = shader.sampler_count;
        stats.compute_buffers = shader.uniform_buffer_count;
    }
    
    stats
}

/// Statistics about decoded shaders
#[derive(Debug, Clone, Default)]
pub struct ShaderStats {
    pub has_vertex: bool,
    pub has_fragment: bool,
    pub has_compute: bool,
    pub vertex_lines: usize,
    pub fragment_lines: usize,
    pub compute_lines: usize,
    pub vertex_bytes: usize,
    pub fragment_bytes: usize,
    pub compute_bytes: usize,
    pub vertex_samplers: u32,
    pub fragment_samplers: u32,
    pub compute_samplers: u32,
    pub vertex_buffers: u32,
    pub fragment_buffers: u32,
    pub compute_buffers: u32,
}

impl ShaderStats {
    pub fn total_lines(&self) -> usize {
        self.vertex_lines + self.fragment_lines + self.compute_lines
    }
    
    pub fn total_bytes(&self) -> usize {
        self.vertex_bytes + self.fragment_bytes + self.compute_bytes
    }
    
    pub fn total_samplers(&self) -> u32 {
        self.vertex_samplers + self.fragment_samplers + self.compute_samplers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shader_pair_creation() {
        let pair = EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };
        
        assert!(pair.vertex.is_none());
        assert!(pair.fragment.is_none());
        assert!(pair.compute.is_none());
    }

    #[test]
    fn test_shader_stats() {
        let pair = EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };
        
        let stats = get_shader_stats(&pair);
        assert!(!stats.has_vertex);
        assert!(!stats.has_fragment);
        assert!(!stats.has_compute);
        assert_eq!(stats.total_lines(), 0);
    }

    #[test]
    fn test_decoded_shader_summary() {
        let shader = DecodedShader {
            spirv: vec![],  // Empty for test
            wgsl_source: "fn main() { }\n".to_string(),
            entry_point: "main".to_string(),
            sampler_count: 1,
            uniform_buffer_count: 2,
        };
        
        let summary = shader.summary();
        assert!(summary.contains("WGSL shader"));
        assert!(summary.contains("lines"));
        assert!(summary.contains("main"));
    }
}
