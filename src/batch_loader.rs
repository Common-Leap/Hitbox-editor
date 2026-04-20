// Batch loader: pre-scans and caches all 500+ SSBU effects for efficient access
// Lazy-loads PTCL files on first use; provides error recovery (one bad effect doesn't crash)
// Target: ~100-200ms full scan + lazy loading on first effect access

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;
use crate::effects::{EffIndex, PtclFile};

#[derive(Debug, Clone)]
pub struct EffectMetadata {
    pub name: String,
    pub path: PathBuf,
    pub category: String, // "fighter", "pokemon", "stage", "boss", "assist", etc.
    pub loaded: bool,
}

#[derive(Debug, Clone)]
pub struct CachedEffect {
    pub metadata: EffectMetadata,
    pub ptcl: Option<PtclFile>,
    pub error: Option<String>,
}

/// Batch loader for SSBU effects
/// Manages ~500+ .eff files from the dumped game data
pub struct BatchEffectLoader {
    effects: HashMap<String, CachedEffect>,
    base_dir: PathBuf,
}

impl BatchEffectLoader {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            effects: HashMap::new(),
            base_dir,
        }
    }

    /// Scan directory and build metadata index (no loading yet)
    /// Returns count of .eff files discovered
    pub fn scan(&mut self) -> Result<usize> {
        self.effects.clear();
        let count = self.scan_recursive(&self.base_dir.clone(), "root")?;
        eprintln!("[BatchLoader] scanned {} effect files from {:?}", count, self.base_dir);
        Ok(count)
    }

    fn scan_recursive(&mut self, dir: &Path, category: &str) -> Result<usize> {
        let mut count = 0;
        
        if !dir.is_dir() {
            return Ok(0);
        }

        let entries = std::fs::read_dir(dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            
            if path.is_dir() {
                // Recurse into subdirectory
                let subcat = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                count += self.scan_recursive(&path, &subcat)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("eff") {
                let name = path.file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unnamed")
                    .to_string();
                
                self.effects.insert(name.clone(), CachedEffect {
                    metadata: EffectMetadata {
                        name,
                        path: path.clone(),
                        category: category.to_string(),
                        loaded: false,
                    },
                    ptcl: None,
                    error: None,
                });
                count += 1;
            }
        }
        
        Ok(count)
    }

    /// (Testing only) Insert an effect into the loader
    #[cfg(test)]
    pub fn insert_effect(&mut self, name: String, effect: CachedEffect) {
        self.effects.insert(name, effect);
    }

    /// Get effect metadata (always available; fast)
    pub fn get_metadata(&self, name: &str) -> Option<EffectMetadata> {
        self.effects.get(name).map(|e| e.metadata.clone())
    }

    /// List all available effect names
    pub fn list_all(&self) -> Vec<String> {
        self.effects.keys().cloned().collect()
    }

    /// Count effects by category
    pub fn count_by_category(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for effect in self.effects.values() {
            *counts.entry(effect.metadata.category.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Lazy-load and cache a PTCL file
    /// Returns (success, loaded_from_cache)
    pub fn load_effect(&mut self, name: &str) -> (bool, bool) {
        // Check if already loaded or has error
        if let Some(cached) = self.effects.get(name) {
            if cached.ptcl.is_some() {
                return (true, true);
            }
            if cached.error.is_some() {
                return (false, true);
            }
        } else {
            return (false, false);
        }

        // Extract path to drop the borrow before calling load_eff_file
        let path = self.effects.get(name).map(|e| e.metadata.path.clone());
        if let Some(path) = path {
            match self.load_eff_file(&path) {
                Ok(ptcl) => {
                    if let Some(cached) = self.effects.get_mut(name) {
                        cached.ptcl = Some(ptcl);
                        cached.metadata.loaded = true;
                    }
                    (true, false)
                }
                Err(e) => {
                    if let Some(cached) = self.effects.get_mut(name) {
                        cached.error = Some(e.to_string());
                    }
                    (false, false)
                }
            }
        } else {
            (false, false)
        }
    }

    /// Get loaded PTCL file (None if not loaded or error)
    pub fn get_ptcl(&self, name: &str) -> Option<PtclFile> {
        self.effects.get(name)
            .and_then(|e| e.ptcl.clone())
    }

    /// Get error message if load failed
    pub fn get_error(&self, name: &str) -> Option<String> {
        self.effects.get(name)
            .and_then(|e| e.error.clone())
    }

    /// Unload all cached PTCL files to free memory
    pub fn clear_cache(&mut self) {
        for effect in self.effects.values_mut() {
            effect.ptcl = None;
            effect.metadata.loaded = false;
        }
        eprintln!("[BatchLoader] cleared all cached PTCL data");
    }

    /// Statistics
    pub fn stats(&self) -> BatchLoaderStats {
        let total = self.effects.len();
        let loaded = self.effects.values()
            .filter(|e| e.ptcl.is_some())
            .count();
        let failed = self.effects.values()
            .filter(|e| e.error.is_some())
            .count();
        let pending = total - loaded - failed;

        let total_emitters = self.effects.values()
            .filter_map(|e| e.ptcl.as_ref())
            .map(|p| p.emitter_sets.iter().map(|s| s.emitters.len()).sum::<usize>())
            .sum::<usize>();

        BatchLoaderStats {
            total_effects: total,
            loaded_effects: loaded,
            failed_effects: failed,
            pending_effects: pending,
            total_emitters,
        }
    }

    fn load_eff_file(&self, path: &Path) -> Result<PtclFile> {
        let eff_index = EffIndex::from_file(path)?;
        if eff_index.ptcl_data.is_empty() {
            anyhow::bail!("No PTCL data in EFF file");
        }
        PtclFile::parse(&eff_index.ptcl_data)
    }
}

#[derive(Debug, Clone)]
pub struct BatchLoaderStats {
    pub total_effects: usize,
    pub loaded_effects: usize,
    pub failed_effects: usize,
    pub pending_effects: usize,
    pub total_emitters: usize,
}

impl BatchLoaderStats {
    pub fn print_summary(&self) {
        eprintln!(
            "[BatchLoader] total={} loaded={} failed={} pending={} emitters={}",
            self.total_effects,
            self.loaded_effects,
            self.failed_effects,
            self.pending_effects,
            self.total_emitters
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_loader_creation() {
        let loader = BatchEffectLoader::new(PathBuf::from("/tmp"));
        assert_eq!(loader.effects.len(), 0);
    }

    #[test]
    fn test_stats_empty() {
        let loader = BatchEffectLoader::new(PathBuf::from("/tmp"));
        let stats = loader.stats();
        assert_eq!(stats.total_effects, 0);
        assert_eq!(stats.loaded_effects, 0);
    }
}
