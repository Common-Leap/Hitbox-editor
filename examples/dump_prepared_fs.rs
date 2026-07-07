//! Dump the fully-prepared (NVN-injected) VS/FS WGSL for one emitter.
//! Usage: dump_prepared_fs <eff> <emitter_name> [out_prefix]

use hitbox_editor::effects::{EffIndex, PtclFile};
use hitbox_editor::particle_renderer_bnsh::{prepare_bnsh_wgsl, BnshShaderSet};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let want = args[1].as_str();
    let prefix = args.get(2).cloned().unwrap_or_else(|| "/tmp/prep".into());
    let bnsh = BnshShaderSet::from_ptcl_file(&ptcl, "ef.eff").expect("bnsh");

    for set in &ptcl.emitter_sets {
        for e in &set.emitters {
            if e.name != want {
                continue;
            }
            let pair = bnsh.pair_for_emitter(e);
            match &pair.fragment {
                Some(fsd) => match &fsd.reflection {
                    Some(r) => eprintln!(
                        "FS reflection: samplers={:?} textures={:?}",
                        r.sampler_names, r.texture_names
                    ),
                    None => eprintln!("FS reflection: NONE"),
                },
                None => eprintln!("FS: NONE"),
            }
            let vs = pair.vertex.as_ref().map(|v| v.wgsl_source.clone()).unwrap_or_default();
            let fs = pair.fragment.as_ref().map(|f| f.wgsl_source.clone()).unwrap_or_default();
            let prepared = prepare_bnsh_wgsl(
                &vs,
                &fs,
                None,
                None,
                None,
                hitbox_editor::shader_registry::NativeColorInput::Auto,
            );
            std::fs::write(format!("{prefix}_vs.wgsl"), &prepared.vs_wgsl).unwrap();
            std::fs::write(format!("{prefix}_fs.wgsl"), &prepared.fs_wgsl).unwrap();
            std::fs::write(format!("{prefix}_fs_raw.wgsl"), &fs).unwrap();
            std::fs::write(format!("{prefix}_vs_raw.wgsl"), &vs).unwrap();
            eprintln!("raw fs {} bytes, raw vs {} bytes", fs.len(), vs.len());
            if let Some(vsd) = &pair.vertex {
                if let Ok((w, _)) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
                    &vsd.spirv, naga::ShaderStage::Vertex, "fire_vs") {
                    std::fs::write(format!("{prefix}_vs_spv.wgsl"), &w).unwrap();
                    eprintln!("decoded vs from spirv: {} bytes -> {prefix}_vs_spv.wgsl", w.len());
                    // Run the real injection (as prepare_bnsh_wgsl does) on the SPIR-V-decoded VS.
                    let fsw = pair.fragment.as_ref().and_then(|f| hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(&f.spirv, naga::ShaderStage::Fragment, "fs").ok()).map(|(x,_)| x).unwrap_or_default();
                    let prep = hitbox_editor::particle_renderer_bnsh::prepare_bnsh_wgsl(&w, &fsw, None, Some(&vsd.spirv), pair.fragment.as_ref().map(|f| f.spirv.as_slice()), hitbox_editor::shader_registry::NativeColorInput::Auto);
                    std::fs::write(format!("{prefix}_vs_injected.wgsl"), &prep.vs_wgsl).unwrap();
                    eprintln!("injected vs: {} bytes -> {prefix}_vs_injected.wgsl", prep.vs_wgsl.len());
                }
            }
            if let Some(fsd) = &pair.fragment {
                if let Ok((w, _)) = hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(
                    &fsd.spirv,
                    naga::ShaderStage::Fragment,
                    "fire_fs",
                ) {
                    std::fs::write(format!("{prefix}_fs_spv.wgsl"), &w).unwrap();
                    eprintln!("decoded fs from spirv: {} bytes -> {prefix}_fs_spv.wgsl", w.len());
                }
            }
            println!(
                "emitter '{}' shader_index={} uses_native_fs={} -> {prefix}_vs.wgsl / {prefix}_fs.wgsl ({} vs bytes, {} fs bytes)",
                e.name, e.shader_index, prepared.uses_native_fs_fragment,
                prepared.vs_wgsl.len(), prepared.fs_wgsl.len()
            );
            return;
        }
    }
    eprintln!("emitter '{want}' not found");
}
