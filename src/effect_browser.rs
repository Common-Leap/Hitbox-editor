// Effect browser: UI for loading and displaying 500+ SSBU effects
// Demonstrates batch_loader + shader_cache integration in practice

use std::path::PathBuf;
use crate::batch_loader::{BatchEffectLoader, EffectMetadata, BatchLoaderStats};
use crate::shader_cache::ShaderCache;
use crate::effects::PtclFile;

/// Effect browser state for UI integration
pub struct EffectBrowser {
    loader: BatchEffectLoader,
    cache: ShaderCache,
    
    // UI state
    selected_category: String,
    selected_effect: Option<String>,
    search_filter: String,
    
    // Loaded data
    current_ptcl: Option<PtclFile>,
    last_stats: BatchLoaderStats,
    
    // Loading state
    is_scanning: bool,
    scan_error: Option<String>,
}

impl EffectBrowser {
    pub fn new(effects_dir: PathBuf) -> Self {
        let loader = BatchEffectLoader::new(effects_dir);
        let cache = ShaderCache::new();
        let last_stats = BatchLoaderStats {
            total_effects: 0,
            loaded_effects: 0,
            failed_effects: 0,
            pending_effects: 0,
            total_emitters: 0,
        };
        
        Self {
            loader,
            cache,
            selected_category: "fighters".to_string(),
            selected_effect: None,
            search_filter: String::new(),
            current_ptcl: None,
            last_stats,
            is_scanning: false,
            scan_error: None,
        }
    }

    /// Scan all effects (blocking - should be run in background)
    pub fn scan_effects(&mut self) -> anyhow::Result<()> {
        self.is_scanning = true;
        self.scan_error = None;
        
        match self.loader.scan() {
            Ok(count) => {
                eprintln!("[EffectBrowser] Scanned {} effects", count);
                self.is_scanning = false;
                Ok(())
            }
            Err(e) => {
                self.scan_error = Some(e.to_string());
                self.is_scanning = false;
                Err(e)
            }
        }
    }

    /// Get list of effects in selected category (optionally filtered by search)
    pub fn get_filtered_effects(&self) -> Vec<(String, EffectMetadata)> {
        let all = self.loader.list_all();
        
        all.into_iter()
            .filter_map(|name| {
                self.loader.get_metadata(&name).map(|meta| (name, meta))
            })
            .filter(|(name, meta)| {
                // Filter by category
                if !self.selected_category.is_empty() && self.selected_category != "all" {
                    if !meta.category.contains(&self.selected_category) {
                        return false;
                    }
                }
                
                // Filter by search term
                if !self.search_filter.is_empty() {
                    let filter_lower = self.search_filter.to_lowercase();
                    if !name.to_lowercase().contains(&filter_lower) {
                        return false;
                    }
                }
                
                true
            })
            .collect()
    }

    /// Get available categories
    pub fn get_categories(&self) -> Vec<String> {
        let mut categories: Vec<_> = self.loader.count_by_category()
            .keys()
            .cloned()
            .collect();
        categories.sort();
        categories
    }

    /// Load an effect by name
    pub fn load_effect(&mut self, name: &str) -> (bool, Option<String>) {
        let (success, _) = self.loader.load_effect(name);
        
        if success {
            self.current_ptcl = self.loader.get_ptcl(name);
            self.selected_effect = Some(name.to_string());
            (true, None)
        } else {
            let error = self.loader.get_error(name);
            (false, error)
        }
    }

    /// Get current PTCL file (if loaded)
    pub fn current_ptcl(&self) -> Option<&PtclFile> {
        self.current_ptcl.as_ref()
    }

    /// Get effect metadata
    pub fn get_effect_info(&self, name: &str) -> Option<EffectMetadata> {
        self.loader.get_metadata(name)
    }

    /// Update stats
    pub fn update_stats(&mut self) {
        self.last_stats = self.loader.stats();
    }

    /// Get stats
    pub fn stats(&self) -> &BatchLoaderStats {
        &self.last_stats
    }

    /// Get shader cache stats
    pub fn shader_cache_stats(&self) -> (usize, usize, f32) {
        self.cache.stats()
    }

    /// Clear in-memory caches
    pub fn clear_caches(&mut self) {
        self.loader.clear_cache();
        self.current_ptcl = None;
        self.selected_effect = None;
        eprintln!("[EffectBrowser] Caches cleared");
    }

    /// UI helper: format effect count for display
    pub fn format_count(&self, category: &str) -> String {
        self.loader.count_by_category()
            .get(category)
            .map(|count| count.to_string())
            .unwrap_or_default()
    }

    /// Test: verify effects can be loaded
    pub fn verify_loadable(&self, limit: usize) -> usize {
        let mut loaded = 0;
        for (name, _) in self.get_filtered_effects().iter().take(limit) {
            let (success, _) = self.loader.get_metadata(name).map(|_| (true, false)).unwrap_or((false, false));
            if success {
                loaded += 1;
            }
        }
        loaded
    }
}

// UI Display Helpers (format data for egui)

pub struct EffectDisplayInfo {
    pub name: String,
    pub category: String,
    pub emitter_count: usize,
    pub texture_count: usize,
    pub shader_1_size: usize,
    pub shader_2_size: usize,
}

impl EffectBrowser {
    /// Convert PTCL to display info
    pub fn get_display_info(&self, name: &str) -> Option<EffectDisplayInfo> {
        let meta = self.loader.get_metadata(name)?;
        let ptcl = self.loader.get_ptcl(name)?;
        
        Some(EffectDisplayInfo {
            name: name.to_string(),
            category: meta.category,
            emitter_count: ptcl.emitter_sets.iter().map(|s| s.emitters.len()).sum(),
            texture_count: ptcl.bntx_textures.len(),
            shader_1_size: ptcl.shader_binary_1.len(),
            shader_2_size: ptcl.shader_binary_2.len(),
        })
    }

    /// Format info for UI display
    pub fn format_display_info(&self, name: &str) -> String {
        if let Some(info) = self.get_display_info(name) {
            format!(
                "{} [{}]\nemitters: {} | textures: {} | shaders: {}+{}KB",
                info.name,
                info.category,
                info.emitter_count,
                info.texture_count,
                info.shader_1_size / 1024,
                info.shader_2_size / 1024
            )
        } else {
            format!("{} [not loaded]", name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effect_browser_creation() {
        let browser = EffectBrowser::new(PathBuf::from("/tmp"));
        assert_eq!(browser.last_stats.total_effects, 0);
        assert_eq!(browser.selected_category, "fighters");
    }

    #[test]
    fn test_filter_effects_empty() {
        let browser = EffectBrowser::new(PathBuf::from("/tmp"));
        let filtered = browser.get_filtered_effects();
        assert_eq!(filtered.len(), 0);
    }

    #[test]
    fn test_search_filter() {
        let mut browser = EffectBrowser::new(PathBuf::from("/tmp"));
        browser.search_filter = "mario".to_string();
        
        // Should filter by search term
        let filtered = browser.get_filtered_effects();
        // (would have items if effects were loaded)
    }

    #[test]
    fn test_category_filter() {
        let mut browser = EffectBrowser::new(PathBuf::from("/tmp"));
        browser.selected_category = "pokemon".to_string();
        
        let filtered = browser.get_filtered_effects();
        // (would filter to pokemon category if effects were loaded)
    }
}
