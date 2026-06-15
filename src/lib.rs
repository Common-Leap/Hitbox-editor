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

use std::sync::OnceLock;

pub fn fx_debug_enabled() -> bool {
    static FX_DEBUG: OnceLock<bool> = OnceLock::new();
    *FX_DEBUG.get_or_init(|| std::env::var("FX_DEBUG").is_ok())
}

/// Native NVN fragment colour chain is the default.
/// Opt out with `FX_PATCHED_FS=1` or `FX_NATIVE_FS=0`.
pub fn fx_native_fs_enabled() -> bool {
    static FX_NATIVE_FS: OnceLock<bool> = OnceLock::new();
    *FX_NATIVE_FS.get_or_init(|| match std::env::var("FX_NATIVE_FS").as_deref() {
        Ok("0") | Ok("false") | Ok("no") => false,
        Ok(_) => true,
        Err(_) => !matches!(
            std::env::var("FX_PATCHED_FS").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        ),
    })
}

#[cfg(test)]
mod test_eff_pipeline {
    use std::path::Path;

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
        let eff_path = Path::new("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/mario/ef_mario.eff");
        
        let eff = crate::effects::EffIndex::from_file(eff_path)
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
        let eff_path = Path::new("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff");
        let eff = crate::effects::EffIndex::from_file(eff_path)
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
                // If frame_count > 1, the total slots should equal or exceed frame_count
                if emitter.tex_pat_frame_count > 1 {
                    assert!(slots >= emitter.tex_pat_frame_count,
                        "tex_scale_uv={:?} gives only {} slots but fc={} (insufficient)",
                        emitter.tex_scale_uv, slots, emitter.tex_pat_frame_count);
                }
            }
        }
    }

    #[test]
    fn test_samus_headless_simulation() {
        let eff_path = Path::new("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff");
        let eff = crate::effects::EffIndex::from_file(eff_path)
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

    /// Every embedded Shader.bnsh in Samus must produce a linked VS→FS WGSL interface.
    #[test]
    fn test_samus_all_registry_shaders_link_stages() {
        use crate::particle_renderer_bnsh::BnshShaderSet;
        use crate::spirv_to_wgsl::{fragment_input_locations, patch_vertex_wgsl, vertex_return_wires_fs_inputs};

        let eff_path = Path::new(
            "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff",
        );
        if !eff_path.exists() {
            eprintln!("Samus effect not found — skipping");
            return;
        }
        let eff = crate::effects::EffIndex::from_file(eff_path).expect("eff");
        let ptcl = crate::effects::PtclFile::parse(&eff.ptcl_data).expect("ptcl");
        let set = BnshShaderSet::from_ptcl_file(&ptcl, "ef_samus.eff").expect("bnsh set");

        let mut failures = Vec::new();
        for (&key, pair) in &set.all_shaders {
            if pair.vertex.is_none() || pair.fragment.is_none() {
                failures.push(format!("{key:#x}: incomplete decode"));
                continue;
            }
            let label = format!("{key:#x}");
            let vs = pair.vertex.as_ref().unwrap();
            let fs = pair.fragment.as_ref().unwrap();
            let mut vs_w = crate::spirv_to_wgsl::bytes_to_words(&vs.spirv).unwrap();
            let mut fs_w = crate::spirv_to_wgsl::bytes_to_words(&fs.spirv).unwrap();
            let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut vs_w);
            let _ = crate::spirv_patch::nvn_to_vulkan_patch(&mut fs_w);
            let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let (vs_wgsl, _) = crate::spirv_to_wgsl::spirv_to_wgsl(
                &to_bytes(&vs_w),
                naga::ShaderStage::Vertex,
                &format!("link_vs_{label}"),
            )
            .unwrap();
            let (fs_wgsl, _) = crate::spirv_to_wgsl::spirv_to_wgsl(
                &to_bytes(&fs_w),
                naga::ShaderStage::Fragment,
                &format!("link_fs_{label}"),
            )
            .unwrap();
            let patched = patch_vertex_wgsl(&vs_wgsl, &fs_wgsl);
            for loc in fragment_input_locations(&fs_wgsl) {
                let needle = format!("@location({loc})");
                if !patched.contains(&needle) {
                    failures.push(format!("{key:#x}: VS missing output {needle}"));
                }
            }
            if !vertex_return_wires_fs_inputs(&patched, &fs_wgsl) {
                failures.push(format!("{key:#x}: return VertexOutput missing FS varyings"));
            }
        }

        if !failures.is_empty() {
            for f in failures.iter().take(20) {
                eprintln!("[SHADER-LINK] {f}");
            }
            panic!(
                "{} Samus shader(s) failed stage linking (showing up to 20)",
                failures.len()
            );
        }
    }
}
