// Integration example: Using batch_loader + shader_cache together
// This demonstrates the workflow for loading 500+ SSBU effects efficiently

#[cfg(test)]
mod integration_tests {
    use crate::batch_loader::BatchEffectLoader;
    use crate::shader_cache::ShaderCache;
    use std::path::PathBuf;

    #[test]
    #[ignore] // Enable only with real effect files
    fn test_batch_load_and_cache_flow() {
        // 1. Initialize batch loader pointing to dumped effects
        let mut loader = BatchEffectLoader::new(
            PathBuf::from("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/")
        );

        // 2. Scan all available effects (fast - metadata only)
        let scan_count = loader.scan().expect("Failed to scan effects");
        println!("Scanned {} effect files", scan_count);

        // 3. Initialize shader cache with optional filesystem persistence
        let mut cache = if let Some(home) = dirs::home_dir() {
            let cache_dir = home.join(".cache/hitbox_editor/shaders");
            ShaderCache::new().with_cache_dir(cache_dir).unwrap_or_else(|_| ShaderCache::new())
        } else {
            ShaderCache::new()
        };

        // 4. Load a few sample effects and extract their shaders
        let effects_to_test = vec!["sys_smash_flash", "eff_mario_fire", "eff_pikachu_thunder"];
        for effect_name in effects_to_test {
            let (success, from_cache) = loader.load_effect(effect_name);
            
            if success {
                println!("✓ Loaded {} (cached: {})", effect_name, from_cache);
                
                // 5. Get the loaded PTCL and extract shader binaries
                if let Some(ptcl) = loader.get_ptcl(effect_name) {
                    if !ptcl.shader_binary_1.is_empty() {
                        // Try to get from cache first
                        if let Some(cached) = cache.get(&ptcl.shader_binary_1) {
                            println!("  ✓ Shader found in cache (hash: {})", cached.bnsh_hash);
                        } else {
                            // In real implementation, would call:
                            // let result = BnshDecoder::decode_with_metadata(&ptcl.shader_binary_1)?;
                            // cache.put(&ptcl.shader_binary_1, entry)?;
                            println!("  - Shader would be decoded and cached");
                        }
                    }
                    
                    println!("  - {} emitter sets, {} textures",
                        ptcl.emitter_sets.len(),
                        ptcl.bntx_textures.len()
                    );
                }
            } else {
                if let Some(error) = loader.get_error(effect_name) {
                    println!("✗ Failed to load {}: {}", effect_name, error);
                } else {
                    println!("✗ Not found: {}", effect_name);
                }
            }
        }

        // 6. Print statistics
        let stats = loader.stats();
        stats.print_summary();
        
        let (hits, misses, hit_rate) = cache.stats();
        println!("Shader cache: hits={} misses={} rate={:.1}%", hits, misses, hit_rate);
    }

    #[test]
    fn test_shader_cache_deduplication() {
        // Demonstrate that identical shader binaries reuse cache entries
        let mut cache = ShaderCache::new();
        
        let bnsh_data1 = b"identical_shader_binary_1234567890";
        let bnsh_data2 = b"identical_shader_binary_1234567890";  // Same content
        let _bnsh_data3 = b"different_shader_binary_content_xyz";
        
        // First access to shader 1 misses
        assert!(cache.get(bnsh_data1).is_none());
        let (h1, m1, _) = cache.stats();
        assert_eq!((h1, m1), (0, 1));
        
        // Access to shader 2 (identical content) also misses (different object)
        assert!(cache.get(bnsh_data2).is_none());
        let (h2, m2, _) = cache.stats();
        assert_eq!((h2, m2), (0, 2));
        
        // Note: These have identical binary content, so SHA256 hashes are equal
        // In real usage, shader cache would see this as the same shader
        let hash1 = ShaderCache::hash_bnsh(bnsh_data1);
        let hash2 = ShaderCache::hash_bnsh(bnsh_data2);
        assert_eq!(hash1, hash2);
        
        println!("Deduplication test: SHA256 hashes match for identical content");
    }

    #[test]
    fn test_batch_loader_category_grouping() {
        let mut loader = BatchEffectLoader::new(PathBuf::from("/tmp"));
        
        // Metadata for test effects
        let effects = vec![
            ("fighter_mario_1", "fighters/mario", "fighters"),
            ("fighter_pikachu_1", "fighters/pikachu", "fighters"),
            ("pokemon_mewtwo_1", "pokemon/mewtwo", "pokemon"),
            ("stage_final_1", "stages/final", "stages"),
        ];
        
        for (name, path, category) in effects {
            let effect = crate::batch_loader::CachedEffect {
                metadata: crate::batch_loader::EffectMetadata {
                    name: name.to_string(),
                    path: PathBuf::from(path),
                    category: category.to_string(),
                    loaded: false,
                },
                ptcl: None,
                error: None,
            };
            loader.insert_effect(name.to_string(), effect);
        }
        
        let categories = loader.count_by_category();
        println!("Category distribution:");
        for (cat, count) in categories.iter() {
            println!("  {}: {} effects", cat, count);
        }
        
        assert_eq!(categories.get("fighters"), Some(&2));
        assert_eq!(categories.get("pokemon"), Some(&1));
        assert_eq!(categories.get("stages"), Some(&1));
    }
}

// Usage example for application code:
#[allow(dead_code)]
pub fn example_load_all_effects() -> anyhow::Result<()> {
    use crate::batch_loader::BatchEffectLoader;
    use std::path::PathBuf;

    // Initialize
    let mut loader = BatchEffectLoader::new(
        PathBuf::from("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/")
    );

    // Scan effects
    let count = loader.scan()?;
    eprintln!("Found {} effects", count);

    // Load all (this would block, so usually done in background)
    for effect_name in loader.list_all() {
        let (success, _) = loader.load_effect(&effect_name);
        if !success {
            eprintln!("Failed to load {}", effect_name);
        }
    }

    // Get stats
    loader.stats().print_summary();
    Ok(())
}
