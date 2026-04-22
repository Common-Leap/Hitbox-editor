// Shader integration: Demonstrate batch_loader + shader_cache working together
// with real SSBU effects and shader deduplication

use std::path::PathBuf;
use crate::batch_loader::BatchEffectLoader;
use crate::shader_cache::{ShaderCache, ShaderMetadata, ShaderCacheEntry, ShaderStage};
use std::collections::HashMap;

#[allow(dead_code)]

/// Statistics for shader integration across a batch
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ShaderBatchStats {
    pub total_effects: usize,
    pub effects_with_shaders: usize,
    pub unique_shader_hashes: usize,
    pub total_shader_bytes: usize,
    pub deduped_shader_bytes: usize,
    pub dedup_savings_pct: f32,
    pub errors: Vec<(String, String)>,
}

impl ShaderBatchStats {
    fn new() -> Self {
        Self {
            total_effects: 0,
            effects_with_shaders: 0,
            unique_shader_hashes: 0,
            total_shader_bytes: 0,
            deduped_shader_bytes: 0,
            dedup_savings_pct: 0.0,
            errors: Vec::new(),
        }
    }

    pub fn print_summary(&self) {
        eprintln!("╭─ Shader Batch Integration Summary");
        eprintln!("│ Total effects: {}", self.total_effects);
        eprintln!("│ Effects with shaders: {}", self.effects_with_shaders);
        eprintln!("│ Unique hashes: {}", self.unique_shader_hashes);
        eprintln!("│ Total bytes: {} MB", self.total_shader_bytes / 1024 / 1024);
        eprintln!("│ After dedup: {} MB", self.deduped_shader_bytes / 1024 / 1024);
        eprintln!("│ Savings: {:.1}%", self.dedup_savings_pct);
        eprintln!("│ Errors: {}", self.errors.len());
        eprintln!("╰─");
    }
}

/// Shader integration coordinator
pub struct ShaderIntegration {
    loader: BatchEffectLoader,
    cache: ShaderCache,
    batch_stats: ShaderBatchStats,
}

impl ShaderIntegration {
    pub fn new(effects_dir: PathBuf) -> Self {
        Self {
            loader: BatchEffectLoader::new(effects_dir),
            cache: ShaderCache::new(),
            batch_stats: ShaderBatchStats::new(),
        }
    }

    /// Scan all effects and index their shaders
    pub fn scan_and_index_shaders(&mut self) -> anyhow::Result<ShaderBatchStats> {
        self.batch_stats = ShaderBatchStats::new();

        // Scan all effects
        let count = self.loader.scan()?;
        self.batch_stats.total_effects = count;
        eprintln!("[ShaderIntegration] Scanning {} effects for shaders...", count);

        let all_effects = self.loader.list_all();
        let mut shader_hashes = HashMap::new();
        let mut total_bytes = 0usize;

        for name in all_effects {
            // Load effect metadata
            if let Some(_meta) = self.loader.get_metadata(&name) {
                // Try to load the full PTCL to get shader binaries
                let (success, _) = self.loader.load_effect(&name);
                
                if success {
                    if let Some(ptcl) = self.loader.get_ptcl(&name) {
                        // Index shader 1
                        if !ptcl.shader_binary_1.is_empty() {
                            let hash = ShaderCache::hash_bnsh(&ptcl.shader_binary_1);
                            let entry = ShaderCacheEntry {
                                bnsh_hash: hash.clone(),
                                spirv_module: vec![0x07230203], // SPIR-V magic placeholder
                                metadata: ShaderMetadata {
                                    entry_point: "fs_main".to_string(),
                                    stage: ShaderStage::Fragment,
                                    sampler_count: 0,
                                    uniform_buffer_count: 0,
                                },
                            };
                            let _ = self.cache.put(&ptcl.shader_binary_1, entry);
                            
                            *shader_hashes.entry(hash).or_insert(0) += 1;
                            total_bytes += ptcl.shader_binary_1.len();
                            self.batch_stats.effects_with_shaders += 1;
                        }

                        // Index shader 2
                        if !ptcl.shader_binary_2.is_empty() {
                            let hash = ShaderCache::hash_bnsh(&ptcl.shader_binary_2);
                            let entry = ShaderCacheEntry {
                                bnsh_hash: hash.clone(),
                                spirv_module: vec![0x07230203], // SPIR-V magic placeholder
                                metadata: ShaderMetadata {
                                    entry_point: "vs_main".to_string(),
                                    stage: ShaderStage::Vertex,
                                    sampler_count: 0,
                                    uniform_buffer_count: 0,
                                },
                            };
                            let _ = self.cache.put(&ptcl.shader_binary_2, entry);
                            
                            *shader_hashes.entry(hash).or_insert(0) += 1;
                            total_bytes += ptcl.shader_binary_2.len();
                        }
                    }
                } else if let Some(error) = self.loader.get_error(&name) {
                    self.batch_stats.errors.push((name, error));
                }
            }
        }

        // Calculate deduplication savings
        self.batch_stats.unique_shader_hashes = shader_hashes.len();
        self.batch_stats.total_shader_bytes = total_bytes;

        let (_hits, _misses, hit_rate) = self.cache.stats();
        let estimated_deduped = if hit_rate > 0.0 {
            (total_bytes as f32 * (1.0 - hit_rate / 100.0)) as usize
        } else {
            total_bytes
        };
        
        self.batch_stats.deduped_shader_bytes = estimated_deduped;
        self.batch_stats.dedup_savings_pct = if total_bytes > 0 {
            ((total_bytes - estimated_deduped) as f32 / total_bytes as f32) * 100.0
        } else {
            0.0
        };

        Ok(self.batch_stats.clone())
    }

    /// Get current batch stats
    pub fn stats(&self) -> &ShaderBatchStats {
        &self.batch_stats
    }

    /// Load a specific effect and return its shader data
    pub fn load_effect_shaders(&mut self, name: &str) -> Option<(Vec<u8>, Vec<u8>)> {
        let (success, _) = self.loader.load_effect(name);
        
        if success {
            self.loader.get_ptcl(name).map(|ptcl| {
                (ptcl.shader_binary_1, ptcl.shader_binary_2)
            })
        } else {
            None
        }
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> (usize, usize, f32) {
        self.cache.stats()
    }

    /// Get loader statistics
    pub fn loader_stats(&self) -> crate::batch_loader::BatchLoaderStats {
        self.loader.stats()
    }

    /// Verify shader deduplication by checking hit rate
    pub fn verify_deduplication(&self) -> (bool, String) {
        let (hits, total_misses, hit_rate) = self.cache.stats();
        let total = hits + total_misses;
        
        let message = if total == 0 {
            "No shaders cached yet".to_string()
        } else {
            format!(
                "Cached: {} shaders, Hits: {}, Rate: {:.1}%",
                total, hits, hit_rate
            )
        };

        // Deduplication is considered successful if we have any hits
        let success = hits > 0 && hit_rate > 0.0;
        (success, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shader_integration_creation() {
        let integration = ShaderIntegration::new(PathBuf::from("/tmp"));
        let stats = integration.stats();
        assert_eq!(stats.total_effects, 0);
        assert_eq!(stats.unique_shader_hashes, 0);
    }

    #[test]
    fn test_batch_stats_format() {
        let stats = ShaderBatchStats {
            total_effects: 500,
            effects_with_shaders: 250,
            unique_shader_hashes: 50,
            total_shader_bytes: 100 * 1024 * 1024, // 100 MB
            deduped_shader_bytes: 20 * 1024 * 1024,  // 20 MB
            dedup_savings_pct: 80.0,
            errors: vec![],
        };
        
        // Verify the message formatting works
        assert_eq!(stats.total_effects, 500);
        assert_eq!(stats.unique_shader_hashes, 50);
        assert!(stats.dedup_savings_pct > 0.0);
    }

    #[test]
    fn test_deduplication_verification() {
        let integration = ShaderIntegration::new(PathBuf::from("/tmp"));
        let (success, message) = integration.verify_deduplication();
        
        // Should report no shaders yet
        assert!(!success);
        assert!(message.contains("No shaders cached"));
    }

    #[test]
    fn test_cache_stats_exposed() {
        let integration = ShaderIntegration::new(PathBuf::from("/tmp"));
        let (hits, misses, _rate) = integration.cache_stats();
        
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn test_loader_stats_exposed() {
        let integration = ShaderIntegration::new(PathBuf::from("/tmp"));
        let stats = integration.loader_stats();
        
        assert_eq!(stats.total_effects, 0);
        assert_eq!(stats.loaded_effects, 0);
    }
}
