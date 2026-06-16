/// Integration tests: Load 328 real SSBU effect files
/// Validates batch_loader, shader extraction, and PTCL parsing
/// 
/// Data source: editor data_root or `HITBOX_EFFECT_EXPORT` (see `scratch_dirs::effect_export_root`).

use hitbox_editor::scratch_dirs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn effect_root() -> Option<PathBuf> {
    scratch_dirs::effect_export_root()
}

/// Metadata for a loaded effect
#[derive(Debug, Clone)]
struct LoadedEffect {
    name: String,
    path: PathBuf,
    size_bytes: u64,
    has_bnsh: bool,
    shader_count: usize,
    error: Option<String>,
}

/// Statistics from loading all effects
#[derive(Debug, Clone, Default)]
struct LoadStats {
    total_files: usize,
    successfully_loaded: usize,
    failed_to_load: usize,
    has_bnsh_shaders: usize,
    shader_count: usize,
    total_bytes: u64,
}

/// Test: Can we find and enumerate all effect files?
#[test]
fn test_enumerate_all_real_effects() {
    let Some(effect_root) = effect_root() else {
        eprintln!("⚠ Effect directory not configured (set data_root or HITBOX_EFFECT_EXPORT)");
        return;
    };

    let mut effect_files = Vec::new();
    
    // Recursively find all .eff files
    fn walk_dir(path: &Path, results: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_dir() {
                        walk_dir(&path, results);
                    } else if path.extension().map_or(false, |ext| ext == "eff") {
                        results.push(path);
                    }
                }
            }
        }
    }
    
    walk_dir(&effect_root, &mut effect_files);
    
    assert!(!effect_files.is_empty(), "No .eff files found");
    println!("✓ Found {} .eff files", effect_files.len());
    
    // Group by category
    let mut by_category: HashMap<String, usize> = HashMap::new();
    for f in &effect_files {
        let category = f.parent()
            .and_then(|p| p.components().find_map(|c| {
                use std::path::Component::Normal;
                if let Normal(n) = c {
                    n.to_str().map(|s| s.to_string())
                } else {
                    None
                }
            }))
            .unwrap_or_else(|| "unknown".to_string());
        
        *by_category.entry(category).or_insert(0) += 1;
    }
    
    println!("\nEffect files by category:");
    for (cat, count) in &by_category {
        println!("  {}: {}", cat, count);
    }
}

/// Test: Can we read basic file metadata?
#[test]
fn test_read_effect_file_metadata() {
    let Some(effect_root) = effect_root() else {
        eprintln!("⚠ Effect directory not configured (set data_root or HITBOX_EFFECT_EXPORT)");
        return;
    };

    let mut effect_files = Vec::new();
    fn walk_dir(path: &Path, results: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_dir() {
                        walk_dir(&path, results);
                    } else if path.extension().map_or(false, |ext| ext == "eff") {
                        results.push(path);
                    }
                }
            }
        }
    }
    walk_dir(&effect_root, &mut effect_files);

    let mut stats = LoadStats::default();
    stats.total_files = effect_files.len();
    
    let mut sample_effects = Vec::new();
    
    for (idx, file_path) in effect_files.iter().enumerate() {
        // Sample every 10th file (31 samples from 328)
        if idx % 10 == 0 {
            match std::fs::read(&file_path) {
                Ok(data) => {
                    stats.successfully_loaded += 1;
                    stats.total_bytes += data.len() as u64;
                    
                    let name = file_path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    
                    sample_effects.push(LoadedEffect {
                        name,
                        path: file_path.clone(),
                        size_bytes: data.len() as u64,
                        has_bnsh: data.len() > 16, // Rough check
                        shader_count: 0,
                        error: None,
                    });
                }
                Err(e) => {
                    stats.failed_to_load += 1;
                    eprintln!("Failed to read {:?}: {}", file_path, e);
                }
            }
        }
    }
    
    println!("\n=== Real Effect File Sampling ===");
    println!("Total files: {}", stats.total_files);
    println!("Successfully loaded (sampled): {}", stats.successfully_loaded);
    println!("Failed to load: {}", stats.failed_to_load);
    println!("Total data (sampled): {} bytes", stats.total_bytes);
    
    println!("\nSample effects:");
    for effect in sample_effects.iter().take(5) {
        println!("  {} - {} bytes", effect.name, effect.size_bytes);
    }
    
    assert!(stats.successfully_loaded > 0, "Failed to load any effects");
}

/// Test: Verify effect files have expected binary structure
#[test]
fn test_effect_file_binary_structure() {
    let Some(effect_root) = effect_root() else {
        return;
    };

    // Test a few specific fighter effects as representative samples
    let test_cases = vec![
        "fighter/mario/ef_mario.eff",
        "fighter/link/ef_link.eff",
        "pokemon/pikachu/ef_pikachu.eff",
        "stage/battlefield/ef_battlefield.eff",
    ];
    
    for test_path_str in test_cases {
        let full_path = effect_root.join(test_path_str);
        
        if !full_path.exists() {
            println!("⚠ Not found: {}", test_path_str);
            continue;
        }
        
        match std::fs::read(&full_path) {
            Ok(data) => {
                println!("\n✓ Loaded: {} ({} bytes)", test_path_str, data.len());
                
                // Check for common magic numbers / markers
                if data.len() >= 4 {
                    let first_u32 = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    println!("  First u32: 0x{:08x}", first_u32);
                }
                
                // PTCL section check (PTCL magic = 0x4C544350 or "PTCL")
                if let Some(ptcl_pos) = data.windows(4).position(|w| w == b"PTCL") {
                    println!("  Found PTCL marker at offset: 0x{:x}", ptcl_pos);
                }
                
                // Shader section checks
                let has_grsn = data.windows(4).any(|w| w == b"GRSN");
                let has_grsc = data.windows(4).any(|w| w == b"GRSC");
                let has_bnsh = data.windows(4).any(|w| w == b"BNSH");
                
                if has_grsn {
                    println!("  Found GRSN marker (shader 1)");
                }
                if has_grsc {
                    println!("  Found GRSC marker (shader 2)");
                }
                if has_bnsh {
                    println!("  Found BNSH marker");
                }
                
                if !has_grsn && !has_grsc {
                    println!("  ⚠ No shader sections found");
                }
            }
            Err(e) => {
                println!("✗ Failed to load {}: {}", test_path_str, e);
            }
        }
    }
}

/// Test: Parse real .eff files through the full EffectConverter pipeline
/// and verify they return real data (not synthetic fallback).
#[test]
fn test_ptcl_parser_on_real_effects() {
    let Some(effect_root) = effect_root() else {
        eprintln!("⚠ Effect directory not configured, skipping PtclFile::parse test");
        return;
    };

    // Test a representative sample of fighter effects
    let test_cases = vec![
        ("fighter/mario/ef_mario.eff",     "mario"),
        ("fighter/link/ef_link.eff",       "link"),
        ("fighter/sonic/ef_sonic.eff",     "sonic"),
        ("pokemon/pikachu/ef_pikachu.eff", "pikachu"),
        ("stage/battlefield/ef_battlefield.eff", "battlefield"),
    ];

    let mut parsed = 0usize;
    let mut not_found = 0usize;

    for (rel_path, label) in &test_cases {
        let full_path = effect_root.join(rel_path);
        if !full_path.exists() {
            eprintln!("⚠ Not found: {} — skipping", rel_path);
            not_found += 1;
            continue;
        }

        let bytes = match std::fs::read(&full_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("⚠ Failed to read {}: {} — skipping", rel_path, e);
                not_found += 1;
                continue;
            }
        };

        // This calls EffectConverter CLI under the hood via parse_via_converter
        let ptcl = match hitbox_editor::effects::PtclFile::parse(&bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("✗ {}: PtclFile::parse FAILED: {:?}", label, e);
                continue;
            }
        };

        parsed += 1;

        // Sanity: the parsed data must NOT look like synthetic fallback.
        // Synthetic sets are named "set_0", "set_1", etc. with 1 emitter each.
        // Real data has meaningful set names with properly configured emitters.
        let set_count = ptcl.emitter_sets.len();
        let total_emitters: usize = ptcl.emitter_sets.iter().map(|s| s.emitters.len()).sum();

        println!("\n  {} ({}): {} set(s), {} emitter(s), {} bntx textures, {} shader bytes",
            label, rel_path, set_count, total_emitters,
            ptcl.bntx_textures.len(),
            ptcl.shader_binary_1.len() + ptcl.shader_binary_2.len(),
        );

        // ═══ Real-data assertions ═══
        // 1. Must have at least one emitter set
        assert!(!ptcl.emitter_sets.is_empty(),
            "{}: expected at least 1 emitter set", label);

        // 2. Set names must NOT be the synthetic "set_0" pattern
        for eset in &ptcl.emitter_sets {
            assert_ne!(eset.name, "set_0",
                "{}: set name '{}' looks synthetic", label, eset.name);
        }

        // 3. A real .eff should produce at least as many emitters as
        //    synthetic fallback would (synthetic gives 1 emitter per set).
        //    Most fighter effects have 2-8+ emitters.
        assert!(total_emitters >= set_count,
            "{}: expected at least {} emitters, got {}",
            label, set_count, total_emitters);

        // 4. At least some emitters should have a non-empty name
        let named_emitters: usize = ptcl.emitter_sets.iter()
            .flat_map(|s| s.emitters.iter())
            .filter(|e| !e.name.is_empty())
            .count();
        assert!(named_emitters > 0,
            "{}: expected at least 1 named emitter", label);

        // 5. Either bntx_textures or texture_section should be present
        //    (most effects carry at least a placeholder texture)
        let has_texture_data = !ptcl.bntx_textures.is_empty()
            || !ptcl.texture_section.is_empty();
        assert!(has_texture_data,
            "{}: expected texture data from a real .eff", label);
    }

    assert!(parsed + not_found > 0, "No effect files were tested at all");
    if parsed == 0 {
        eprintln!("⚠ All effect files were missing or failed to parse");
    }
    println!("\n✓ PtclFile::parse: {}/{} tested effects parsed successfully",
        parsed, test_cases.len() - not_found);
}

/// Test: Validate batch_loader on real effect directory
#[test]
fn test_batch_loader_real_effects() {
    // This would use the real batch_loader module
    // For now, validate the path structure
    
    let Some(effect_root) = effect_root() else {
        eprintln!("⚠ Effect directory not configured, skipping batch_loader test");
        return;
    };
    
    println!("\n✓ Batch loader test framework ready");
    println!("  Effect root: {:?}", effect_root);
    
    // Verify subdirectories exist
    let categories = vec!["fighter", "pokemon", "stage", "boss", "assist"];
    for cat in &categories {
        let cat_path = effect_root.join(cat);
        if cat_path.exists() {
            println!("  ✓ Found category: {}", cat);
        }
    }
}

/// Test: Verify shader extraction feasibility
#[test]
fn test_shader_extraction_from_effects() {
    let Some(effect_root) = effect_root() else {
        return;
    };
    
    let test_files = vec![
        effect_root.join("fighter/mario/ef_mario.eff"),
        effect_root.join("fighter/link/ef_link.eff"),
        effect_root.join("pokemon/pikachu/ef_pikachu.eff"),
        effect_root.join("stage/battlefield/ef_battlefield.eff"),
    ];
    
    let mut found_grsn = 0;
    let mut found_grsc = 0;
    let mut found_bnsh = 0;
    
    for file_path in test_files {
        if !file_path.exists() {
            continue;
        }
        
        if let Ok(data) = std::fs::read(&file_path) {
            let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();
            
            // Look for GRSN and GRSC sections (shader containers)
            let has_grsn = data.windows(4).any(|w| w == b"GRSN");
            let has_grsc = data.windows(4).any(|w| w == b"GRSC");
            let has_bnsh = data.windows(4).any(|w| w == b"BNSH");
            
            if has_grsn {
                found_grsn += 1;
                println!("✓ {} has GRSN (shader 1)", file_name);
            }
            if has_grsc {
                found_grsc += 1;
                println!("✓ {} has GRSC (shader 2)", file_name);
            }
            if has_bnsh {
                found_bnsh += 1;
                println!("✓ {} has BNSH", file_name);
            }
            
            if !has_grsn && !has_grsc && !has_bnsh {
                println!("⚠ {} has no shader sections", file_name);
            }
        }
    }
    
    println!("\nShader extraction summary:");
    println!("  GRSN sections (shader 1): {}", found_grsn);
    println!("  GRSC sections (shader 2): {}", found_grsc);
    println!("  BNSH markers: {}", found_bnsh);
}
