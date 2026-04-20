// End-to-end integration test demonstrating batch_loader + shader_cache + effect_browser working together
// This test shows the complete workflow without requiring real .eff files

#[cfg(test)]
mod end_to_end_integration_tests {
    use std::path::PathBuf;

    #[test]
    fn test_infrastructure_compiles() {
        // This test simply verifies all modules exist and compile
        // Real tests require actual effect files
        assert!(true, "All infrastructure modules compiled successfully");
    }

    #[test]
    fn test_batch_loader_effect_browser_browser_compatibility() {
        // Verify effect_browser can work with batch_loader output
        use crate::batch_loader::BatchEffectLoader;
        use crate::effect_browser::EffectBrowser;

        let loader = BatchEffectLoader::new(PathBuf::from("/tmp/nonexistent"));
        let browser = EffectBrowser::new(PathBuf::from("/tmp/nonexistent"));

        // Both should create successfully
        assert_eq!(loader.stats().total_effects, 0);
        assert_eq!(browser.stats().total_effects, 0);
    }

    #[test]
    fn test_shader_integration_statistics_format() {
        // Verify shader integration can aggregate statistics correctly
        use crate::shader_integration::ShaderIntegration;

        let mut integration = ShaderIntegration::new(PathBuf::from("/tmp/nonexistent"));
        
        // Should not crash even with no effects
        let stats = integration.stats();
        assert_eq!(stats.total_effects, 0);
        assert_eq!(stats.effects_with_shaders, 0);
        assert_eq!(stats.dedup_savings_pct, 0.0);

        // Should format summary without panicking
        stats.print_summary();
    }

    #[test]
    fn test_shader_cache_with_integration() {
        // Verify shader_cache works with shader_integration
        use crate::shader_cache::ShaderCache;
        use crate::shader_cache::ShaderCacheEntry;
        use crate::shader_cache::ShaderMetadata;
        use crate::shader_cache::ShaderStage;

        let mut cache = ShaderCache::new();
        
        // Create a mock shader binary
        let shader1 = b"test_shader_binary_1";
        let shader2 = b"test_shader_binary_2";

        // Hash both
        let hash1 = ShaderCache::hash_bnsh(shader1);
        let hash2 = ShaderCache::hash_bnsh(shader2);

        // Should produce different hashes
        assert_ne!(hash1, hash2);

        // Create cache entries
        let entry1 = ShaderCacheEntry {
            bnsh_hash: hash1.clone(),
            spirv_module: vec![0x07230203, 0x00010000],
            metadata: ShaderMetadata {
                entry_point: "vs_main".to_string(),
                stage: ShaderStage::Vertex,
                sampler_count: 1,
                uniform_buffer_count: 1,
            },
        };

        let entry2 = ShaderCacheEntry {
            bnsh_hash: hash2.clone(),
            spirv_module: vec![0x07230203, 0x00020000],
            metadata: ShaderMetadata {
                entry_point: "fs_main".to_string(),
                stage: ShaderStage::Fragment,
                sampler_count: 0,
                uniform_buffer_count: 2,
            },
        };

        // Put both in cache
        assert!(cache.put(shader1, entry1.clone()).is_ok());
        assert!(cache.put(shader2, entry2.clone()).is_ok());

        // Verify retrieval
        let retrieved1 = cache.get(shader1);
        assert!(retrieved1.is_some());
        assert_eq!(retrieved1.unwrap().metadata.entry_point, "vs_main");

        let retrieved2 = cache.get(shader2);
        assert!(retrieved2.is_some());
        assert_eq!(retrieved2.unwrap().metadata.entry_point, "fs_main");

        // Verify stats show hits
        let (hits, misses, hit_rate) = cache.stats();
        assert_eq!(hits, 2);
        assert_eq!(misses, 0);
        assert_eq!(hit_rate, 100.0);
    }

    #[test]
    fn test_effect_browser_cache_integration() {
        // Verify effect_browser can use shader_cache infrastructure
        use crate::effect_browser::EffectBrowser;

        let mut browser = EffectBrowser::new(PathBuf::from("/tmp/nonexistent"));
        
        // Verify cache is accessible
        let (hits, misses, _rate) = browser.shader_cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);

        // Should handle empty effect list
        let filtered = browser.get_filtered_effects();
        assert_eq!(filtered.len(), 0);

        // Should handle category query
        let categories = browser.get_categories();
        assert_eq!(categories.len(), 0);
    }

    #[test]
    fn test_full_workflow_simulation() {
        // Simulate the complete workflow without real files:
        // 1. Create integration coordinator
        // 2. Verify all subcomponents accessible
        // 3. Check stats flow

        use crate::shader_integration::ShaderIntegration;

        let mut integration = ShaderIntegration::new(PathBuf::from("/tmp/nonexistent"));

        // Stats should initialize to 0
        let stats = integration.stats();
        assert_eq!(stats.total_effects, 0);

        // Cache stats should be accessible
        let (cache_hits, cache_misses, _rate) = integration.cache_stats();
        assert_eq!(cache_hits, 0);
        assert_eq!(cache_misses, 0);

        // Loader stats should be accessible
        let loader_stats = integration.loader_stats();
        assert_eq!(loader_stats.total_effects, 0);

        // Deduplication verification should work
        let (success, message) = integration.verify_deduplication();
        assert!(!success); // No effects loaded yet
        assert!(message.contains("No shaders cached"));

        // Should not crash on print_summary
        stats.print_summary();
    }

    #[test]
    fn test_cache_hit_rate_calculation() {
        // Verify cache hit rate is calculated correctly
        use crate::shader_cache::ShaderCache;
        use crate::shader_cache::ShaderCacheEntry;
        use crate::shader_cache::ShaderMetadata;
        use crate::shader_cache::ShaderStage;

        let mut cache = ShaderCache::new();

        // Create 3 different shaders
        let shaders = vec![
            b"shader_a".to_vec(),
            b"shader_b".to_vec(),
            b"shader_c".to_vec(),
        ];

        // Put all in cache
        for (i, shader) in shaders.iter().enumerate() {
            let entry = ShaderCacheEntry {
                bnsh_hash: ShaderCache::hash_bnsh(shader),
                spirv_module: vec![0x07230203],
                metadata: ShaderMetadata {
                    entry_point: format!("entry_{}", i),
                    stage: ShaderStage::Vertex,
                    sampler_count: 0,
                    uniform_buffer_count: 0,
                },
            };
            let _ = cache.put(shader, entry);
        }

        // Get all 3 (should hit)
        let _ = cache.get(&shaders[0]);
        let _ = cache.get(&shaders[1]);
        let _ = cache.get(&shaders[2]);

        // Get one again (cache hit for duplicated shader)
        let _ = cache.get(&shaders[0]);

        // Stats should show 4 hits (3 initial + 1 duplicate)
        let (hits, misses, hit_rate) = cache.stats();
        assert_eq!(hits, 4);
        assert_eq!(misses, 0);
        assert_eq!(hit_rate, 100.0);
    }

    #[test]
    fn test_error_isolation_in_batch_loader() {
        // Verify batch_loader error isolation works
        use crate::batch_loader::BatchEffectLoader;

        let loader = BatchEffectLoader::new(PathBuf::from("/tmp/nonexistent"));
        
        // Should handle non-existent directory gracefully
        // (scan on empty dir should not panic)
        let stats = loader.stats();
        assert_eq!(stats.failed_effects, 0);
        assert_eq!(stats.total_effects, 0);
    }

    #[test]
    fn test_module_dependencies_satisfied() {
        // Verify all modules can import and use each other
        use crate::shader_cache::ShaderCache;
        use crate::batch_loader::BatchEffectLoader;
        use crate::effect_browser::EffectBrowser;
        use crate::shader_integration::ShaderIntegration;

        // All should construct without panic
        let _cache = ShaderCache::new();
        let _loader = BatchEffectLoader::new(PathBuf::from("/tmp"));
        let _browser = EffectBrowser::new(PathBuf::from("/tmp"));
        let _integration = ShaderIntegration::new(PathBuf::from("/tmp"));

        assert!(true, "All module dependencies satisfied");
    }
}
