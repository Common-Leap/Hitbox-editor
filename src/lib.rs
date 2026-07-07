// Re-export all modules as a library so integration tests can import them
// This is a thin wrapper around the main binary module structure

pub mod effects;
pub mod particle_renderer;
pub mod particle_renderer_bnsh;
pub mod bnsh_shader_integration;
pub mod bnsh_reflection;
pub mod batch_loader;
pub mod shader_cache;
pub mod bnsh_ffi;
pub mod effect_converter;
pub mod spirv_to_wgsl;
pub mod spirv_patch;
pub mod nvn_chain;
pub mod combiner;
pub mod shader_registry;
pub mod scratch_dirs;
pub mod sphere_volume_tables;
pub mod fx_env;
pub mod trail_shader;
pub mod blit_shader;
pub mod regression;

pub use fx_env::{fx_debug_enabled, fx_native_fs_enabled, fx_native_vs_pos_enabled, fx_prim_per_triangle_enabled};
pub use particle_renderer_bnsh::wgpu_device_limits;

#[cfg(test)]
mod test_eff_pipeline {
    use std::path::PathBuf;

    fn local_eff_path(fighter: &str) -> PathBuf {
        crate::scratch_dirs::resolve_fighter_eff(fighter).unwrap_or_else(|| {
            PathBuf::from("/nonexistent").join(format!("ef_{fighter}.eff"))
        })
    }

    /// Test the grid-layout inference for sprite-sheet textures.
    #[test]
    fn test_infer_grid_layout() {
        // Square texture, 4 frames → 2×2 grid
        let (c, r) = crate::effects::infer_grid_layout(256, 256, 4);
        assert_eq!((c, r), (2, 2), "256×256 fc=4 should be 2×2");
        // Tall texture, 4 frames → 1×4 vertical strip
        let (c, r) = crate::effects::infer_grid_layout(64, 256, 4);
        assert_eq!((c, r), (1, 4), "64×256 fc=4 should be 1×4");
        // Wide texture, 4 frames → 4×1 horizontal strip
        let (c, r) = crate::effects::infer_grid_layout(256, 64, 4);
        assert_eq!((c, r), (4, 1), "256×64 fc=4 should be 4×1");
        // Square texture, 1 frame → 1×1
        let (c, r) = crate::effects::infer_grid_layout(256, 256, 1);
        assert_eq!((c, r), (1, 1), "256×256 fc=1 should be 1×1");
        // Square texture, 8 frames → expect balanced grid (2×4)
        let (c, r) = crate::effects::infer_grid_layout(256, 256, 8);
        assert!(
            (c == 2 && r == 4) || (c == 4 && r == 2),
            "256×256 fc=8 should be a balanced grid, got {}×{}", c, r
        );
        // Wide texture, 6 frames → 3×2 (closest to square)
        let (c, r) = crate::effects::infer_grid_layout(300, 200, 6);
        assert_eq!((c, r), (3, 2), "3:2 texture fc=6 should be 3×2");
        // Square texture, prime frame count 7 → 1×7 (vertical strip, first divisor)
        let (c, r) = crate::effects::infer_grid_layout(256, 256, 7);
        assert_eq!((c, r), (1, 7), "256×256 fc=7 should be 1×7");
        // 16:9 wide texture, 12 frames → best is 4×3
        let (c, r) = crate::effects::infer_grid_layout(1600, 900, 12);
        assert!(
            (c == 4 && r == 3) || (c == 3 && r == 4) || (c == 6 && r == 2) || (c == 2 && r == 6),
            "1600×900 fc=12 should pick a balanced layout, got {}×{}", c, r
        );
    }

    #[test]
    fn test_eff_pipeline_mario() {
        let eff_path = local_eff_path("mario");
        if !eff_path.exists() {
            eprintln!("Mario effect not found — skipping");
            return;
        }

        let eff = crate::effects::EffIndex::from_file(&eff_path)
            .expect("EffIndex::from_file failed");
        
        println!("EffIndex OK: {} handles, ptcl_data={} bytes", eff.handles.len(), eff.ptcl_data.len());
        assert!(!eff.ptcl_data.is_empty(), "ptcl_data should not be empty");
        assert!(eff.handles.len() > 0, "handles should not be empty");
        
        let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data)
            .expect("PtclFile::parse failed");
        
        println!("PtclFile::parse OK: {} emitter sets, {} bntx textures, {} bfres models", 
            ptcl.emitter_sets.len(), ptcl.bntx_textures.len(), ptcl.bfres_models.len());
        
        assert!(ptcl.emitter_sets.len() > 0, "should have at least 1 emitter set");
    }

    #[test]
    fn test_emitter_uv_data() {
        let eff_path = local_eff_path("samus");
        if !eff_path.exists() {
            eprintln!("Samus effect not found — skipping");
            return;
        }
        let eff = crate::effects::EffIndex::from_file(&eff_path)
            .expect("EffIndex::from_file failed");
        let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data)
            .expect("PtclFile::parse failed");
        println!("=== UV DATA: {} emitter sets, {} textures ===",
            ptcl.emitter_sets.len(), ptcl.bntx_textures.len());
        for (si, set) in ptcl.emitter_sets.iter().enumerate() {
            for (ei, emitter) in set.emitters.iter().enumerate() {
                let tex_name = ptcl.bntx_textures
                    .get(emitter.texture_index as usize)
                    .map(|t| t.tex_name.as_str())
                    .unwrap_or("(none)");
                let tex_dims = ptcl.bntx_textures
                    .get(emitter.texture_index as usize)
                    .map(|t| format!("{}×{}", t.width, t.height))
                    .unwrap_or_default();
                let slots = if emitter.tex_scale_uv[0] > 0.0 && emitter.tex_scale_uv[1] > 0.0 {
                    let cols = (1.0 / emitter.tex_scale_uv[0].max(0.001)).round() as usize;
                    let rows = (1.0 / emitter.tex_scale_uv[1].max(0.001)).round() as usize;
                    cols * rows
                } else { 0 };
                println!("  [set={si} emtr={ei}] tex='{tex_name}' {tex_dims} fc={} scale=[{:.4},{:.4}] offset=[{:.4},{:.4}] scroll=[{:.4},{:.4}] slots={}",
                    emitter.tex_pat_frame_count,
                    emitter.tex_scale_uv[0], emitter.tex_scale_uv[1],
                    emitter.tex_offset_uv[0], emitter.tex_offset_uv[1],
                    emitter.tex_scroll_uv[0], emitter.tex_scroll_uv[1],
                    slots,
                );
                // The pattern frame TABLE indexes atlas tiles and may revisit them, and the
                // flipbook grid comes from the explicit divisor (`tex_uv_div`) when set —
                // `1/tex_scale_uv` is only a fallback approximation of the grid. Validate
                // that every referenced tile fits whichever grid the emitter declares.
                if emitter.tex_pat_frame_count > 1 {
                    let grid = if emitter.tex_uv_div[0] > 0 && emitter.tex_uv_div[1] > 0 {
                        (emitter.tex_uv_div[0] * emitter.tex_uv_div[1]) as usize
                    } else {
                        slots
                    };
                    let max_tile = emitter
                        .tex_pat_frame_table
                        .iter()
                        .take(emitter.tex_pat_frame_count)
                        .copied()
                        .max()
                        .unwrap_or(0);
                    if grid > 0 && max_tile >= grid {
                        eprintln!(
                            "  [WARN] frame table references tile {max_tile} beyond {grid}-slot grid \
                             (uv_div={:?} scale={:?} fc={})",
                            emitter.tex_uv_div, emitter.tex_scale_uv, emitter.tex_pat_frame_count
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_samus_headless_simulation() {
        let eff_path = local_eff_path("samus");
        if !eff_path.exists() {
            eprintln!("Samus effect not found — skipping");
            return;
        }
        let eff = crate::effects::EffIndex::from_file(&eff_path)
            .expect("EffIndex::from_file failed");
        let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data)
            .expect("PtclFile::parse failed");
        println!("=== HEADLESS SIMULATION: {} emitter sets, {} handles ===",
            ptcl.emitter_sets.len(), eff.handles.len());

        // Find a good test effect: something likely to emit visible particles early
        let test_effects = [
            "samus_cshot_ground",
            "samus_cshot_bullet_sub",
            "samus_gbeam",
            "samus_appeal_s",
            "samus_bomb_jump",
        ];

        for name in test_effects {
            let mut ps = crate::effects::ParticleSystem::default();
            ps.reset();

            let bone_matrices = std::collections::HashMap::new(); // empty = identity fallback

            // Spawn the effect
            ps.spawn_effect(
                name, "Trans",
                glam::Vec3::ZERO, glam::Vec3::ZERO,
                0.0, 9999.0,
                &eff, &ptcl,
            );

            println!("--- {}: active_emitters after spawn: {} ---", name, ps.active_emitters.len());

            // Step through 300 frames
            let max_frames = 300;
            for f in 0..=max_frames {
                ps.step(f as f32, &bone_matrices, &ptcl);
                if !ps.particles.is_empty() || !ps.active_emitters.is_empty() {
                    // Log first time we get particles
                    if ps.particles.len() > 0 {
                        let p = &ps.particles[0];
                        println!("[{}] frame={} active_emitters={} particles={} first_pos=({:.2},{:.2},{:.2}) first_sz={:.3} first_rot={:.3}",
                            name, f, ps.active_emitters.len(), ps.particles.len(),
                            p.position.x, p.position.y, p.position.z,
                            p.size, p.rotation);
                    }
                }
            }
            println!("--- {} FINAL: active_emitters={} particles={} ---",
                name, ps.active_emitters.len(), ps.particles.len());
        }
    }

    /// Every embedded Shader.bnsh must produce a linked VS→FS WGSL interface.
    /// Requires a local effect export (editor data root or HITBOX_EFFECT_EXPORT).
    #[test]
    fn test_samus_all_registry_shaders_link_stages() {
        use crate::bnsh_shader_integration::{
            decode_cached_dump_shaders, decode_shaders_from_fighter_eff,
            shader_link_coverage_report, BOMB_SHADER_KEY, ShaderLinkSource,
        };

        let export_pairs = decode_shaders_from_fighter_eff("samus").unwrap_or_default();
        let export_complete: std::collections::HashMap<_, _> = export_pairs
            .iter()
            .filter(|(_, p)| p.vertex.is_some() && p.fragment.is_some())
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        if export_complete.is_empty() {
            eprintln!("Skipping: no effect export (set data_root or HITBOX_EFFECT_EXPORT)");
            return;
        }

        let mut export_labels = std::collections::HashMap::new();
        if let Some(eff_path) = crate::scratch_dirs::resolve_fighter_eff("samus") {
            for key in export_complete.keys().copied() {
                export_labels.insert(key, eff_path.display().to_string());
            }
        }

        assert!(
            export_complete.contains_key(&BOMB_SHADER_KEY),
            "bomb flare shader ({:#x}) must be present in Samus export",
            BOMB_SHADER_KEY
        );

        let (cache_pairs, cache_labels) = decode_cached_dump_shaders();
        let report = shader_link_coverage_report(
            &export_complete,
            &export_labels,
            &cache_pairs,
            &cache_labels,
        );
        eprintln!("{}", report.summary_line());
        for r in report
            .results
            .iter()
            .filter(|r| r.source == ShaderLinkSource::Export)
        {
            eprintln!(
                "[SHADER-LINK] {} {:#x} {}",
                if r.ok { "PASS" } else { "FAIL" },
                r.key,
                r.fixture.as_deref().unwrap_or("?")
            );
        }
        if report.cache_extension_pairs > 0 {
            eprintln!(
                "[SHADER-LINK] +{} cached shader(s) beyond Samus export",
                report.cache_extension_pairs
            );
        }

        report.assert_all_passed();
    }

    /// Samus export should expose a large shader registry; fixtures auto-sync locally (not committed).
    #[test]
    fn test_samus_shader_fixture_coverage() {
        use crate::bnsh_shader_integration::{
            count_shader_fixtures_on_disk, ensure_shader_fixtures, registry_fixture_coverage,
            shader_fixtures_dir, shader_registry_entry_count,
        };

        let synced = ensure_shader_fixtures("samus");
        if synced > 0 {
            eprintln!("[FIXTURE] synced {synced} new Samus shader(s)");
        }

        let registry_count = shader_registry_entry_count("samus").unwrap_or(0);
        let (registry_total, fixtures_for_registry) =
            registry_fixture_coverage("samus").unwrap_or((0, 0));
        let fixture_files = count_shader_fixtures_on_disk();

        if registry_count == 0 && fixture_files == 0 {
            eprintln!("Skipping: no Samus shaders (set data_root or HITBOX_EFFECT_EXPORT)");
            return;
        }

        eprintln!(
            "[FIXTURE] registry={registry_count} synced={fixtures_for_registry}/{registry_total} \
             files={fixture_files} dir={}",
            shader_fixtures_dir().display()
        );
        assert!(
            registry_count >= 128 || fixture_files >= 128,
            "expected broad Samus shader coverage (registry={registry_count}, files={fixture_files})"
        );
        if registry_count > 0 {
            assert_eq!(
                fixtures_for_registry, registry_total,
                "every registry shader should have a synced fixture ({fixtures_for_registry}/{registry_total})"
            );
        }
    }
}
