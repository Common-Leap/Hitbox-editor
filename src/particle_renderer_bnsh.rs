// Helper module for loading BNSH shaders into particle renderer
// Bridges the gap between effect files and GPU shader pipeline

use anyhow::Result;
use crate::effects::PtclFile;
use crate::bnsh_shader_integration::{decode_effect_shaders, EffectShaderPair, ShaderStats};

/// Metadata about loaded BNSH shaders for rendering
#[derive(Debug, Clone)]
pub struct BnshShaderSet {
    pub shader_pair: EffectShaderPair,
    pub stats: ShaderStats,
    pub source_name: String,  // e.g., "mario.eff"
}

impl BnshShaderSet {
    /// Load and decode BNSH shaders from an effect file
    pub fn from_ptcl_file(ptcl: &PtclFile, source_name: &str) -> Result<Self> {
        eprintln!("[BNSH Shader] Loading shaders from {}", source_name);
        
        let shader_pair = decode_effect_shaders(ptcl)?;
        let stats = crate::bnsh_shader_integration::get_shader_stats(&shader_pair);
        
        eprintln!("[BNSH Shader] Loaded: {} bytes, {} samplers",
            stats.total_bytes(), stats.total_samplers());
        
        Ok(BnshShaderSet {
            shader_pair,
            stats,
            source_name: source_name.to_string(),
        })
    }

    /// Check if we have both required shaders (vertex + fragment)
    pub fn is_complete(&self) -> bool {
        self.shader_pair.vertex.is_some() && self.shader_pair.fragment.is_some()
    }

    /// Get a summary of what shaders we have
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        
        if let Some(vs) = &self.shader_pair.vertex {
            parts.push(format!("vertex({} lines)", vs.wgsl_source.lines().count()));
        }
        if let Some(fs) = &self.shader_pair.fragment {
            parts.push(format!("fragment({} lines)", fs.wgsl_source.lines().count()));
        }
        if let Some(cs) = &self.shader_pair.compute {
            parts.push(format!("compute({} lines)", cs.wgsl_source.lines().count()));
        }
        
        if parts.is_empty() {
            "no shaders".to_string()
        } else {
            parts.join(" + ")
        }
    }
}

/// Load BNSH shaders from multiple effect files
/// Useful for batch loading or testing
pub fn load_shaders_from_files(effect_files: &[(&str, &PtclFile)]) -> Vec<(String, Result<BnshShaderSet>)> {
    let mut results = Vec::new();
    
    for (name, ptcl) in effect_files {
        let result = BnshShaderSet::from_ptcl_file(ptcl, name);
        results.push((name.to_string(), result));
    }
    
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bnsh_shader_set_creation() {
        // This test will be skipped if no effect files available
        // It's here for future integration testing
    }

    #[test]
    fn test_shader_summary_generation() {
        let pair = crate::bnsh_shader_integration::EffectShaderPair {
            vertex: None,
            fragment: None,
            compute: None,
        };
        
        let set = BnshShaderSet {
            shader_pair: pair,
            stats: crate::bnsh_shader_integration::ShaderStats::default(),
            source_name: "test.eff".to_string(),
        };
        
        assert_eq!(set.summary(), "no shaders");
        assert!(!set.is_complete());
    }
}
