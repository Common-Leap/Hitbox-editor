// Shader cache: deduplicates BNSH binaries using SHA256 hashing
// Stores BNSH → SPIR-V conversions to avoid redundant decoding
// Expected hit rate: 80%+ across 500+ effects (many reuse the same shaders)

use std::collections::HashMap;
use std::path::PathBuf;
use sha2::{Sha256, Digest};
use anyhow::Result;

#[allow(dead_code)]

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub struct ShaderCacheEntry {
    pub bnsh_hash: String,
    pub spirv_module: Vec<u32>, // SPIR-V as u32 words
    pub metadata: ShaderMetadata,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShaderMetadata {
    pub entry_point: String,
    pub stage: ShaderStage,
    pub sampler_count: u32,
    pub uniform_buffer_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
    Unknown,
}

impl Default for ShaderStage {
    fn default() -> Self { Self::Unknown }
}

#[allow(dead_code)]
pub struct ShaderCache {
    /// SHA256(BNSH binary) -> ShaderCacheEntry
    cache: HashMap<String, ShaderCacheEntry>,
    /// Filesystem cache directory (~/.cache/hitbox_editor/shaders)
    cache_dir: Option<PathBuf>,
    /// Statistics
    hits: usize,
    misses: usize,
}

impl ShaderCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            cache_dir: None,
            hits: 0,
            misses: 0,
        }
    }

    pub fn with_cache_dir(mut self, dir: PathBuf) -> Result<Self> {
        // Ensure directory exists
        std::fs::create_dir_all(&dir)?;
        self.cache_dir = Some(dir);
        Ok(self)
    }

    /// Compute SHA256 hash of BNSH binary
    pub fn hash_bnsh(bnsh_binary: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bnsh_binary);
        format!("{:x}", hasher.finalize())
    }

    /// Try to retrieve cached SPIR-V for a BNSH binary
    pub fn get(&mut self, bnsh_binary: &[u8]) -> Option<ShaderCacheEntry> {
        let hash = Self::hash_bnsh(bnsh_binary);
        
        if let Some(entry) = self.cache.get(&hash) {
            self.hits += 1;
            return Some(entry.clone());
        }

        // Try filesystem cache if configured
        let cache_file_path = self.cache_dir.as_ref().map(|dir| dir.join(format!("{}.spirv", hash)));
        if let Some(cache_file) = cache_file_path {
            if let Ok(data) = std::fs::read(&cache_file) {
                if let Ok(entry) = bincode::deserialize::<ShaderCacheEntry>(&data) {
                    self.cache.insert(hash.clone(), entry.clone());
                    self.hits += 1;
                    return Some(entry);
                }
            }
        }

        self.misses += 1;
        None
    }

    /// Store SPIR-V in cache
    pub fn put(&mut self, bnsh_binary: &[u8], entry: ShaderCacheEntry) -> Result<()> {
        let hash = Self::hash_bnsh(bnsh_binary);
        self.cache.insert(hash.clone(), entry.clone());

        // Persist to filesystem if configured
        if let Some(dir) = &self.cache_dir {
            let cache_file = dir.join(format!("{}.spirv", hash));
            let serialized = bincode::serialize(&entry)?;
            std::fs::write(cache_file, serialized)?;
        }

        Ok(())
    }

    /// Statistics
    pub fn stats(&self) -> (usize, usize, f32) {
        let total = self.hits + self.misses;
        let hit_rate = if total > 0 {
            (self.hits as f32) / (total as f32) * 100.0
        } else {
            0.0
        };
        (self.hits, self.misses, hit_rate)
    }

    pub fn print_stats(&self) {
        let (hits, misses, rate) = self.stats();
        eprintln!("[ShaderCache] hits={} misses={} hit_rate={:.1}%", hits, misses, rate);
    }
}

impl Default for ShaderCache {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shader_hash() {
        let data1 = b"test_shader_1";
        let data2 = b"test_shader_2";
        
        let hash1 = ShaderCache::hash_bnsh(data1);
        let hash2 = ShaderCache::hash_bnsh(data2);
        
        assert_ne!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA256 = 64 hex chars
    }

    #[test]
    fn test_cache_hit() {
        let mut cache = ShaderCache::new();
        let bnsh_data = b"test_bnsh";
        
        // Initially should miss
        assert!(cache.get(bnsh_data).is_none());
        
        let entry = ShaderCacheEntry {
            bnsh_hash: ShaderCache::hash_bnsh(bnsh_data),
            spirv_module: vec![0x07230203, 0x00010000],
            metadata: ShaderMetadata::default(),
        };
        
        let _ = cache.put(bnsh_data, entry.clone());
        
        // Should now hit
        let retrieved = cache.get(bnsh_data);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().spirv_module, vec![0x07230203, 0x00010000]);
    }

    #[test]
    fn test_stats() {
        let mut cache = ShaderCache::new();
        assert_eq!(cache.stats(), (0, 0, 0.0));
        
        let _ = cache.get(b"miss1");
        let _ = cache.get(b"miss2");
        let (_hits, misses, _) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
    }
}
