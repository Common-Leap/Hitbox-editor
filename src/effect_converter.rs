/// Effect loader — parses VFXB/EFTF `.eff`/`.ptcl` files into the Rust types in
/// `effects.rs`.
///
/// Parsing is done in-process by the `effect_library` crate (a Rust port of the
/// C# EffectLibrary). The crate dumps an `.eff`/`.ptcl` into a folder of JSON +
/// binary assets, and [`load_dump`] re-assembles that tree. This replaces the old
/// `dotnet`-built EffectConverter CLI subprocess.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::effects::{
    AnimKey3v4k, BlendType, BfresModel, ColorKey, DisplaySide, EmitType, EmitterDef, EmitterSet,
    PrimitiveData, PtclFile, TexExtraSlotDef, TextureAnimFlags, TextureRes,
};

fn default_color_scale() -> f32 {
    1.0
}
use crate::shader_registry::{
    CombinerState, ParticleColorState, ParticleScaleState, ShaderRegistry, audit_ptcl,
};

// ── Temp / cache (avoid /tmp tmpfs quota exhaustion) ─────────────────────────

pub use crate::scratch_dirs::app_storage_root as effect_storage_root;

fn effect_scratch_dir() -> anyhow::Result<tempfile::TempDir> {
    crate::scratch_dirs::app_scratch_dir("ec-")
}

fn effect_dump_cache_root() -> PathBuf {
    crate::scratch_dirs::effect_dump_cache_root()
}

/// Bump when dump parsing or post-load emitter fixes change (invalidates stale caches).
pub const EFFECT_DUMP_CACHE_VERSION: u32 = 3;

fn cache_key_for_bytes(data: &[u8]) -> String {
    let hash = format!("{:x}", Sha256::digest(data));
    format!("{hash}-v{EFFECT_DUMP_CACHE_VERSION}")
}

/// Re-apply [`crate::effects::fix_tex_scale_uv`] and flipbook UV scale on every emitter after loading a dump.
fn fix_all_emitter_tex_scales(ptcl: &mut PtclFile) {
    for set in &mut ptcl.emitter_sets {
        for emitter in &mut set.emitters {
            crate::effects::fix_tex_scale_uv(emitter, &ptcl.bntx_textures);
            if emitter.tex_pat_frame_count <= 1 {
                continue;
            }
            if emitter.tex_scale_uv != [1.0, 1.0] {
                continue;
            }
            if emitter.tex_uv_div[0] > 1 || emitter.tex_uv_div[1] > 1 {
                if crate::fx_env::fx_viewport_log_enabled() {
                    eprintln!(
                        "[ATLAS-UV] emitter '{}' fc={} still tex_scale_uv=[1,1] despite uv_div={:?}",
                        emitter.name,
                        emitter.tex_pat_frame_count,
                        emitter.tex_uv_div,
                    );
                }
                continue;
            }
            let u = 1.0 / emitter.tex_pat_frame_count as f32;
            emitter.tex_scale_uv = [u, 1.0];
            if crate::fx_env::fx_viewport_log_enabled() {
                eprintln!(
                    "[ATLAS-UV] emitter '{}' fc={}: vertical-strip fallback tex_scale_uv=[{u}, 1]",
                    emitter.name,
                    emitter.tex_pat_frame_count,
                );
            }
        }
    }
}

fn dump_dir_is_usable(dump_dir: &Path) -> bool {
    if !dump_dir.is_dir() {
        return false;
    }
    let eset_info_path = dump_dir.join("EmitterSetInfo.txt");
    if !eset_info_path.exists() {
        return false;
    }
    let text = match std::fs::read_to_string(&eset_info_path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let info: EmitterSetInfo = serde_json::from_str(&text).unwrap_or_default();
    !info.order.is_empty()
}

fn discover_emitter_set_dirs(dump_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(rd) = std::fs::read_dir(dump_dir) else {
        return names;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".ptcl") || name.starts_with('.') {
            continue;
        }
        if path.join("EmitterOrder.txt").exists() {
            names.push(name);
        }
    }
    names.sort();
    names
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

fn install_dump_cache(src_input: &Path, cache_input: &Path) -> std::io::Result<()> {
    if let Some(parent) = cache_input.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if cache_input.exists() {
        std::fs::remove_dir_all(cache_input)?;
    }
    copy_dir_recursive(src_input, cache_input)
}

fn byte_unit_str(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} MB", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1} KB", n as f64 / 1_000.0)
    } else {
        format!("{n} B")
    }
}

/// Dump an `.eff` (EFFN) byte buffer to `dump_dir` using the in-process
/// `effect_library` crate. Produces the same JSON + asset tree the old C#
/// EffectConverter CLI emitted, so [`load_dump`] consumes it unchanged.
fn dump_namco_in_process(data: &[u8], dump_dir: &Path) -> anyhow::Result<()> {
    eprint!("  >> Converting effect (in-process)");
    let namco = effect_library::NamcoEffectFile::load(data)
        .map_err(|e| anyhow::anyhow!("effect_library failed to parse .eff: {e}"))?;
    let out = dump_dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("dump path is not valid UTF-8: {dump_dir:?}"))?;
    effect_library::Dumper::dump_namco(&namco, out)
        .map_err(|e| anyhow::anyhow!("effect_library dump_namco failed: {e}"))?;
    eprintln!("\r  >> Converted effect                          ");
    Ok(())
}

/// Dump a standalone `.ptcl` byte buffer (e.g. the resource embedded in an `.eff`)
/// to `dump_dir` using the in-process `effect_library` crate.
fn dump_ptcl_in_process(data: &[u8], dump_dir: &Path) -> anyhow::Result<()> {
    eprint!("  >> Converting effect (in-process)");
    let ptcl = effect_library::PtclFile::load(data)
        .map_err(|e| anyhow::anyhow!("effect_library failed to parse .ptcl: {e}"))?;
    let out = dump_dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("dump path is not valid UTF-8: {dump_dir:?}"))?;
    effect_library::Dumper::dump_ptcl(&ptcl, out)
        .map_err(|e| anyhow::anyhow!("effect_library dump_ptcl failed: {e}"))?;
    eprintln!("\r  >> Converted effect                          ");
    Ok(())
}

/// Parse embedded PTCL bytes from an `.eff` via EffectConverter, with disk cache.
pub fn parse_embedded_ptcl(data: &[u8]) -> anyhow::Result<PtclFile> {
    eprintln!(
        ">>> Loading effect ({}) [storage: {}]",
        byte_unit_str(data.len() as u64),
        effect_storage_root().display()
    );

    let cache_key = cache_key_for_bytes(data);
    let cached_input = effect_dump_cache_root().join(&cache_key).join("input");
    if dump_dir_is_usable(&cached_input) {
        eprintln!(
            "[EC] Using cached dump ({}) at {}",
            &cache_key[..16.min(cache_key.len())],
            cached_input.display()
        );
        return load_dump(&cached_input);
    }

    let scratch = effect_scratch_dir()?;
    let dump_dir = scratch.path().join("input");

    dump_ptcl_in_process(data, &dump_dir)?;

    if !dump_dir_is_usable(&dump_dir) {
        anyhow::bail!("effect_library did not produce a usable dump directory at {:?}", dump_dir);
    }
    eprintln!("[EC] Dump dir: {:?}", dump_dir);

    let ptcl = load_dump(&dump_dir)?;

    match install_dump_cache(&dump_dir, &cached_input) {
        Ok(()) => eprintln!("[EC] Cached dump at {}", cached_input.display()),
        Err(e) => eprintln!("[EC] Warning: could not cache effect dump: {e}"),
    }

    Ok(ptcl)
}

pub fn is_effect_io_error(err: &anyhow::Error) -> bool {
    crate::scratch_dirs::is_disk_quota_error(err.as_ref())
}

// ── Top-level entry point ─────────────────────────────────────────────────────

/// Load a `.eff` file by dumping it via EffectConverter and reading back the dump.
pub fn load_ptcl_from_eff(path: &Path) -> anyhow::Result<PtclFile> {
    let data = std::fs::read(path)?;

    let scratch = effect_scratch_dir()?;
    let dump_dir = scratch.path().join("input");

    dump_namco_in_process(&data, &dump_dir)?;
    if !dump_dir.is_dir() {
        anyhow::bail!("effect_library did not produce a dump directory at {:?}", dump_dir);
    }

    let ptcl = load_dump(&dump_dir)?;
    Ok(ptcl)
}

/// Load a standalone `.ptcl` file by dumping it via EffectConverter.
pub fn load_ptcl_from_ptcl(path: &Path) -> anyhow::Result<PtclFile> {
    let data = std::fs::read(path)?;

    let scratch = effect_scratch_dir()?;
    let dump_dir = scratch.path().join("input");

    dump_ptcl_in_process(&data, &dump_dir)?;

    if !dump_dir.is_dir() {
        anyhow::bail!("effect_library did not produce a dump directory at {:?}", dump_dir);
    }

    let ptcl = load_dump(&dump_dir)?;
    Ok(ptcl)
}

// ── Dump directory reader ─────────────────────────────────────────────────────

pub(crate) fn load_dump(dump_dir: &Path) -> anyhow::Result<PtclFile> {
    let eset_info_path = dump_dir.join("EmitterSetInfo.txt");
    let mut eset_info: EmitterSetInfo = if eset_info_path.exists() {
        let text = std::fs::read_to_string(&eset_info_path)?;
        serde_json::from_str(&text).unwrap_or_default()
    } else {
        EmitterSetInfo { order: vec![] }
    };
    if eset_info.order.is_empty() {
        eset_info.order = discover_emitter_set_dirs(dump_dir);
        if !eset_info.order.is_empty() {
            eprintln!(
                "[EC] EmitterSetInfo missing/empty; discovered {} emitter set dirs",
                eset_info.order.len()
            );
        }
    }

    let mut emitter_sets: Vec<EmitterSet> = Vec::new();
    let mut bntx_textures: Vec<TextureRes> = Vec::new();
    let mut texture_section: Vec<u8> = Vec::new();
    let mut primitives: Vec<PrimitiveData> = Vec::new();
    let mut bfres_models: Vec<BfresModel> = Vec::new();
    let mut shader_registry = ShaderRegistry::default();

    for (set_idx, set_name) in eset_info.order.iter().enumerate() {
        let set_dir = dump_dir.join(set_name);
        if !set_dir.is_dir() {
            eprintln!("[EC] emitter set dir not found: {:?}", set_dir);
            continue;
        }

        let order_path = set_dir.join("EmitterOrder.txt");
        let emitter_order: EmitterOrder = if order_path.exists() {
            match std::fs::read_to_string(&order_path) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
                Err(_) => EmitterOrder::default(),
            }
        } else {
            EmitterOrder::default()
        };
        let mut emitters: Vec<EmitterDef> = Vec::new();

        for (emtr_idx, emtr_name) in emitter_order.order.iter().enumerate() {
            let emtr_dir = set_dir.join(emtr_name);
            if !emtr_dir.is_dir() {
                continue;
            }

            let data_path = emtr_dir.join("EmitterData.json");
            let emitter_data: EmitterDataJson = if data_path.exists() {
                let text = std::fs::read_to_string(&data_path).unwrap_or_default();
                match serde_json::from_str::<EmitterDataJson>(&text) {
                    Ok(ed) => ed,
                    Err(e) => {
                        eprintln!("[EC_WARN] parse error {set_name}/{emtr_name}: {e}");
                        EmitterDataJson::default()
                    }
                }
            } else {
                EmitterDataJson::default()
            };

            // Read EA*.json sidecar animation files (EASL, EAC0/1, EAET, EAER, EAES, EAA0)
            let ea_anims: Vec<(String, EmitterAnimJson)> = {
                let mut anims = Vec::new();
                for name in &["EASL", "EAC0", "EAC1", "EAET", "EAER", "EAES", "EAA0"] {
                    let path = emtr_dir.join(format!("{name}.json"));
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if let Ok(anim) = serde_json::from_str::<EmitterAnimJson>(&text) {
                            anims.push((name.to_string(), anim));
                        }
                    }
                }
                anims
            };
            if !ea_anims.is_empty() {
                let names: Vec<&str> = ea_anims.iter().map(|(n, _)| n.as_str()).collect();
                eprintln!("[EA] {set_name}/{emtr_name}: {}", names.join(", "));
            }

            let mut emitter_shader_key = 0u64;
            let shader_path = emtr_dir.join("Shader.bnsh");
            if shader_path.exists() {
                let bytes = std::fs::read(&shader_path).unwrap_or_default();
                if !bytes.is_empty() {
                    emitter_shader_key = shader_registry.register(bytes.clone());
                    shader_registry.set_vs_profile(
                        emitter_shader_key,
                        crate::bnsh_shader_integration::vs_profile_from_bnsh_bytes(&bytes),
                    );
                }
            }

            // Read textures (<texture_id>.bntx) in this emitter directory and order them
            // by the emitter's sampler slots (Sampler0..5 TextureID). Directory order is
            // filesystem-dependent and REVERSED the slots for some emitters (ring3), and
            // the per-slot entries used to carry offsets from their own discarded BNTX
            // section (usually 0) — every alpha/indirect slot decoded the first texture's
            // bytes as noise. Entries now point into the shared texture_section.
            let mut by_id: Vec<(u64, TextureRes, Vec<u8>)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&emtr_dir) {
                let mut paths: Vec<std::path::PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("bntx"))
                    .collect();
                paths.sort();
                for p in paths {
                    let Ok(bntx_bytes) = std::fs::read(&p) else { continue };
                    let (mut tex_map, _section, ordered) =
                        crate::effects::parse_bntx_named(&bntx_bytes);
                    let Some(first) = ordered.first() else { continue };
                    let Some((tex, pixels)) = tex_map.remove(&first.tex_name) else {
                        continue;
                    };
                    let id = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(u64::MAX);
                    by_id.push((id, tex, pixels));
                }
            }
            let sampler_ids: Vec<u64> = [
                &emitter_data.sampler0,
                &emitter_data.sampler1,
                &emitter_data.sampler2,
                &emitter_data.sampler3,
                &emitter_data.sampler4,
                &emitter_data.sampler5,
            ]
            .iter()
            .filter_map(|s| s.as_ref())
            .map(|s| s.texture_id)
            .filter(|&id| id != u64::MAX)
            .collect();
            let mut slot_texs: Vec<(TextureRes, Vec<u8>)> = Vec::new();
            for id in &sampler_ids {
                if let Some(pos) = by_id.iter().position(|(i, ..)| i == id) {
                    let (_, tex, pixels) = by_id.remove(pos);
                    slot_texs.push((tex, pixels));
                }
            }
            // Textures without a matching sampler id keep sorted-filename order.
            for (_, tex, pixels) in by_id {
                slot_texs.push((tex, pixels));
            }

            let mut tex_list: Vec<TextureRes> = Vec::new();
            for (tex, pixels) in slot_texs {
                let mut tx = tex;
                tx.ftx_data_offset = texture_section.len() as u32;
                tx.ftx_data_size = pixels.len() as u32;
                tx.original_data_offset = tx.ftx_data_offset;
                tx.original_data_size = tx.ftx_data_size;
                texture_section.extend_from_slice(&pixels);
                bntx_textures.push(tx.clone());
                tex_list.push(tx);
            }

            // Read primitive models (.bfres files) — track where this emitter's models start
            let model_start_idx = bfres_models.len();
            if let Ok(entries) = std::fs::read_dir(&emtr_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("bfres") {
                        let bfres_bytes = std::fs::read(&p).unwrap_or_default();
                        if !bfres_bytes.is_empty() {
                            let source_id = p
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);
                            let models = crate::effects::parse_bfres(&bfres_bytes);
                            for mut model in models {
                                model.source_id = source_id;
                                bfres_models.push(model);
                            }
                        }
                    }
                }
            }

            let mut emitter = convert_emitter_data(&emitter_data, emtr_name, &bntx_textures, &tex_list);
            emitter.shader_key = emitter_shader_key;
            if emitter.shader_index >= 0 {
                shader_registry.register_library_index(emitter.shader_index, emitter_shader_key);
            }
            if emitter_shader_key != 0 {
                shader_registry.note_emitter_native_color(
                    emitter_shader_key,
                    &emitter.combiner,
                    &emitter.particle_color,
                );
            }

            // Attach EA*.json animation data
            let anim_map: std::collections::HashMap<&str, &EmitterAnimJson> = ea_anims.iter().map(|(n, a)| (n.as_str(), a)).collect();
            if let Some(a) = anim_map.get("EASL") { emitter.anim_tex_scale = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAC0") { emitter.anim_color0 = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAC1") { emitter.anim_color1 = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAET") { emitter.anim_translate = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAER") { emitter.anim_rotation = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAES") { emitter.anim_emit_scale = Some(emit_anim_from_json(a)); }
            if let Some(a) = anim_map.get("EAA0") { emitter.anim_alpha = Some(emit_anim_from_json(a)); }

            // If this emitter has BFRES models, use mesh_type=2 (BFRES model rendering)
            // The converter always exports models as .bfres files, not as raw PRMA primitives.
            let model_end_idx = bfres_models.len();
            if model_end_idx > model_start_idx {
                emitter.mesh_type = 2;
                emitter.primitive_index = model_start_idx as u32;
            }

            emitters.push(emitter);
        }

        emitter_sets.push(EmitterSet {
            name: set_name.clone(),
            emitters,
        });
    }

    // Base.ptcl holds PRMA mesh data from the original binary. Always parse when
    // present so BFRES-backed emitters can fall back to PRMA for missing indices.
    let base_ptcl = dump_dir.join("Base.ptcl");
    if base_ptcl.is_file() {
        if let Ok(bytes) = std::fs::read(&base_ptcl) {
            let parsed = parse_prma_from_ptcl_bytes(&bytes);
            if !parsed.is_empty() {
                eprintln!(
                    "[EC] PRMA: loaded {} primitives from Base.ptcl (bfres_models={})",
                    parsed.len(),
                    bfres_models.len()
                );
                primitives = parsed;
            }
        }
    }

    let (shader_binary_1, shader_binary_2) = shader_registry.legacy_pair();

    let mut ptcl = PtclFile {
        emitter_sets,
        texture_section,
        texture_section_offset: 0,
        bntx_textures,
        primitives,
        bfres_models,
        shader_registry,
        shader_binary_1,
        shader_binary_2,
    };
    fix_all_emitter_tex_scales(&mut ptcl);
    audit_ptcl(&ptcl);
    Ok(ptcl)
}

/// Parse PRMA/PRIM sections from a standalone `.ptcl` binary (e.g. dump `Base.ptcl`).
pub(crate) fn parse_prma_from_ptcl_bytes(data: &[u8]) -> Vec<PrimitiveData> {
    let vfx_version = data.get(10..12).map(|s| u16::from_le_bytes(s.try_into().unwrap_or([0, 0])));
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 32 <= data.len() {
        if &data[i..i + 4] != b"PRIM" {
            i += 1;
            continue;
        }
        let sec_size = read_u32_le(data, i + 4).unwrap_or(0) as usize;
        let binary_rel = read_u32_le(data, i + 20).unwrap_or(u32::MAX);
        if binary_rel != u32::MAX {
            let blob_start = i + binary_rel as usize;
            if let Some(blob) = data.get(blob_start..blob_start.saturating_add(sec_size)) {
                if let Some(prim) = parse_prim_binary(blob, vfx_version) {
                    out.push(prim);
                }
            }
        }
        i += 4;
    }
    out
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(off..off + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn read_i32_le(data: &[u8], off: usize) -> Option<i32> {
    read_u32_le(data, off).map(|v| v as i32)
}

fn read_u64_le(data: &[u8], off: usize) -> Option<u64> {
    let bytes: [u8; 8] = data.get(off..off + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn parse_prim_binary(raw: &[u8], vfx_version: Option<u16>) -> Option<PrimitiveData> {
    if raw.len() < 48 {
        return None;
    }
    let primitive_id = read_u64_le(raw, 0).unwrap_or(0);
    let mut o = 8usize;
    let num_positions = read_i32_le(raw, o)?; o += 4;
    let _num_pos_elems = read_i32_le(raw, o)?; o += 4;
    let num_normals = read_i32_le(raw, o)?; o += 4;
    let _num_norm_elems = read_i32_le(raw, o)?; o += 4;
    let _num_tangents = read_i32_le(raw, o)?; o += 4;
    let _num_tan_elems = read_i32_le(raw, o)?; o += 4;
    let _num_colors = read_i32_le(raw, o)?; o += 4;
    let _num_col_elems = read_i32_le(raw, o)?; o += 4;
    let _num_tex0 = read_i32_le(raw, o)?; o += 4;
    let _num_tex0_elems = read_i32_le(raw, o)?; o += 4;
    let _num_tex1 = read_i32_le(raw, o)?; o += 4;
    let _num_tex1_elems = read_i32_le(raw, o)?; o += 4;
    let num_indices = read_i32_le(raw, o)?; o += 4;
    let pos_off = read_u32_le(raw, o)? as usize; o += 4;
    let nrm_off = read_u32_le(raw, o)? as usize; o += 4;
    let _tan_off = read_u32_le(raw, o)?; o += 4;
    let _col_off = read_u32_le(raw, o)?; o += 4;
    let _uv_off = read_u32_le(raw, o)?; o += 4;
    let idx_off = read_u32_le(raw, o)? as usize;
    if vfx_version.unwrap_or(0) >= 21 {
        let _ = read_u32_le(raw, o);
    }

    if num_positions <= 0 || num_indices <= 0 {
        return None;
    }

    let pos_floats = num_positions as usize * 4;
    let pos_start = pos_off;
    let pos_end = pos_start.saturating_add(pos_floats * 4);
    let pos_bytes = raw.get(pos_start..pos_end)?;

    let mut vertices = Vec::with_capacity(num_positions as usize);
    for vi in 0..num_positions as usize {
        let base = vi * 16;
        if base + 12 > pos_bytes.len() {
            break;
        }
        let px = f32::from_le_bytes(pos_bytes[base..base + 4].try_into().ok()?);
        let py = f32::from_le_bytes(pos_bytes[base + 4..base + 8].try_into().ok()?);
        let pz = f32::from_le_bytes(pos_bytes[base + 8..base + 12].try_into().ok()?);
        vertices.push(crate::effects::MeshVertex {
            position: [px, py, pz],
            uv: [0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
        });
    }

    if num_normals > 0 {
        let nrm_end = nrm_off.saturating_add(num_normals as usize * 16);
        if let Some(nrm_bytes) = raw.get(nrm_off..nrm_end) {
            for vi in 0..num_normals.min(num_positions) as usize {
                let base = vi * 16;
                if base + 12 > nrm_bytes.len() || vi >= vertices.len() {
                    break;
                }
                let nx = f32::from_le_bytes(nrm_bytes[base..base + 4].try_into().ok()?);
                let ny = f32::from_le_bytes(nrm_bytes[base + 4..base + 8].try_into().ok()?);
                let nz = f32::from_le_bytes(nrm_bytes[base + 8..base + 12].try_into().ok()?);
                vertices[vi].normal = [nx, ny, nz];
            }
        }
    }

    let idx_start = idx_off;
    let idx_end = idx_start.saturating_add(num_indices as usize * 4);
    let idx_bytes = raw.get(idx_start..idx_end)?;
    let mut indices = Vec::with_capacity(num_indices as usize);
    for ii in 0..num_indices as usize {
        let base = ii * 4;
        if base + 4 > idx_bytes.len() {
            break;
        }
        let idx = read_i32_le(idx_bytes, base).unwrap_or(0).max(0) as u16;
        indices.push(idx);
    }

    if vertices.is_empty() || indices.len() < 3 {
        return None;
    }
    Some(PrimitiveData {
        id: primitive_id,
        vertices,
        indices,
    })
}

// ── JSON schema (subset of EmitterData fields we care about) ──────────────────
//
// The JSON is produced by C# Newtonsoft.Json → PascalCase field names.
// All structs use `#[serde(rename_all = "PascalCase")]` to match.

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterSetInfo {
    order: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterOrder {
    order: Vec<String>,
}

/// Deserialization of EmitterData.json.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterDataJson {
    flag: u32,
    random_seed: u32,
    name: Option<String>,
    namev40: Option<String>,
    emitter_static: Option<EmitterStaticJson>,
    emitter_info: Option<EmitterInfoJson>,
    emission: Option<EmissionJson>,
    shape_info: Option<ShapeInfoJson>,
    render_state: Option<RenderStateJson>,
    particle_data: Option<ParticleDataJson>,
    particle_velocity: Option<ParticleVelocityJson>,
    particle_scale: Option<ParticleScaleJson>,
    child_inheritance: Option<ChildInheritanceJson>,
    combiner: Option<CombinerJson>,
    shader_references: Option<ShaderReferencesJson>,
    action: Option<ActionJson>,
    particle_color: Option<ParticleColorJson>,
    particle_fluctuation: Option<ParticleFluctuationJson>,
    sampler0: Option<SamplerJson>,
    sampler1: Option<SamplerJson>,
    sampler2: Option<SamplerJson>,
    sampler3: Option<SamplerJson>,
    sampler4: Option<SamplerJson>,
    sampler5: Option<SamplerJson>,
    texture_anim0: Option<TextureAnimJson>,
    texture_anim1: Option<TextureAnimJson>,
    texture_anim2: Option<TextureAnimJson>,
    texture_anim3: Option<TextureAnimJson>,
    texture_anim4: Option<TextureAnimJson>,
    texture_anim5: Option<TextureAnimJson>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ChildInheritanceJson {
    velocity: u32,
    scale: u32,
    rotate: u32,
    color_scale: u32,
    color0: u32,
    color1: u32,
    alpha0: u32,
    alpha1: u32,
    draw_path: u32,
    pre_draw: u32,
    alpha0_each_frame: u32,
    alpha1_each_frame: u32,
    enable_emitter_particle: u32,
    unknown_v40: u32,
    velocity_rate: f32,
    scale_rate: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct CombinerJson {
    color_combiner_process: u32,
    alpha_combiner_process: u32,
    texture1_color_blend: u32,
    texture2_color_blend: u32,
    primitive_color_blend: u32,
    texture1_alpha_blend: u32,
    texture2_alpha_blend: u32,
    primitive_alpha_blend: u32,
    tex_color0_input_type: u32,
    tex_color1_input_type: u32,
    tex_color2_input_type: u32,
    tex_alpha0_input_type: u32,
    tex_alpha1_input_type: u32,
    tex_alpha2_input_type: u32,
    primitive_color_input_type: u32,
    primitive_alpha_input_type: u32,
    shader_type: u32,
    apply_alpha: u32,
    is_distortion_by_camera_distance: u32,
    #[serde(default)]
    padding: Option<i16>,
    #[serde(default)]
    padding2: Option<u32>,
    #[serde(default)]
    padding3: Option<u32>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ShaderReferencesJson {
    #[serde(rename = "Type")]
    type_: u32,
    shader_index: i32,
    compute_shader_index: i32,
    user_shader_index1: i32,
    user_shader_index2: i32,
    custom_shader_index: u32,
    custom_shader_flag: u32,
    custom_shader_switch: u32,
    extra_shader_index2: i32,
    user_shader_define1: String,
    user_shader_define2: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ActionJson {
    action_index: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleColorJson {
    is_soft_particle: u32,
    is_fresnel_alpha: u32,
    is_near_dist_alpha: u32,
    is_far_dist_alpha: u32,
    is_decal: u32,
    #[serde(default)]
    color0_type: serde_json::Value,
    #[serde(default)]
    color1_type: serde_json::Value,
    #[serde(default)]
    alpha0_type: serde_json::Value,
    alpha1_type: String,
    color0_r: f32,
    color0_g: f32,
    color0_b: f32,
    alpha0: f32,
    color1_r: f32,
    color1_g: f32,
    color1_b: f32,
    alpha1: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleFluctuationJson {
    is_apply_alpha: u32,
    is_applay_scale: u32,
    is_applay_scale_y: u32,
    is_wave_type: u32,
    is_phase_random_x: u32,
    is_phase_random_y: u32,
}

/// Note: JSON uses `TextureID` (not `TextureId`) — alias handles it.
/// Same for `MaxLOD` / `LODBias`.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct SamplerJson {
    #[serde(alias = "TextureID")]
    texture_id: u64,
    wrap_u: String,
    wrap_v: String,
    filter: u32,
    is_sphere_map: u32,
    #[serde(alias = "MaxLOD")]
    max_lod: f32,
    #[serde(alias = "LODBias")]
    lod_bias: f32,
    mip_level_limit: u32,
    is_density_fixed_u: u32,
    is_density_fixed_v: u32,
    is_square_rgb: u32,
    is_on_another_binary: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct TextureAnimJson {
    pattern_anim_type: u32,
    is_scroll: bool,
    is_rotate: bool,
    is_scale: bool,
    repeat: u32,
    inv_rand_u: u32,
    inv_rand_v: u32,
    is_pat_anim_loop_random: u32,
    uv_channel: u32,
    is_crossfade: u32,
}

/// Animation keyframe data loaded from EA*.json sidecar files (EASL.json,
/// EAC0.json, EAET.json, EAER.json, EAES.json, EAA0.json).
/// Each file contains a single animation track for the emitter.
#[derive(serde::Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
struct EmitterAnimJson {
    enable: bool,
    #[serde(rename = "Loop")]
    loop_: bool,
    #[serde(default)]
    randomize_start_frame: bool,
    #[serde(default)]
    loop_count: u32,
    key_frames: Vec<AnimKeyJson>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterStaticJson {
    // Flags
    flags1: u32,
    flags2: u32,
    flags3: u32,
    flags4: u32,
    // Key counts
    num_color0_keys: u32,
    num_alpha0_keys: u32,
    num_color1_keys: u32,
    num_alpha1_keys: u32,
    num_scale_keys: u32,
    num_param_keys: u32,
    num_anim2_keys: u32,
    num_anim3_keys: u32,
    num_anim4_keys: u32,
    num_anim5_keys: u32,
    // Animation tables
    color0: Option<AnimKeyTableJson>,
    alpha0: Option<AnimKeyTableJson>,
    color1: Option<AnimKeyTableJson>,
    alpha1: Option<AnimKeyTableJson>,
    scale_anim: Option<AnimKeyTableJson>,
    param_anim: Option<AnimKeyTableJson>,
    // Texture pattern/scroll anims
    tex_pattern_anim0: Option<TexPatAnimJson>,
    tex_pattern_anim1: Option<TexPatAnimJson>,
    tex_pattern_anim2: Option<TexPatAnimJson>,
    tex_pattern_anim3: Option<TexPatAnimJson>,
    tex_pattern_anim4: Option<TexPatAnimJson>,
    tex_pattern_anim5: Option<TexPatAnimJson>,
    tex_scroll_anim0: Option<TexScrollAnimJson>,
    tex_scroll_anim1: Option<TexScrollAnimJson>,
    tex_scroll_anim2: Option<TexScrollAnimJson>,
    tex_scroll_anim3: Option<TexScrollAnimJson>,
    tex_scroll_anim4: Option<TexScrollAnimJson>,
    tex_scroll_anim5: Option<TexScrollAnimJson>,
    // Loop rates & random
    color0_loop_rate: f32,
    alpha0_loop_rate: f32,
    color1_loop_rate: f32,
    alpha1_loop_rate: f32,
    scale_loop_rate: f32,
    color0_loop_random: f32,
    alpha0_loop_random: f32,
    color1_loop_random: f32,
    alpha1_loop_random: f32,
    scale_loop_random: f32,
    // Air resistance
    air_res: f32,
    // Center/Offset/Amplitude/Cycle (wave parameters)
    center_x: f32,
    center_y: f32,
    offset: f32,
    amplitude_x: f32,
    amplitude_y: f32,
    cycle_x: f32,
    cycle_y: f32,
    phase_rnd_x: f32,
    phase_rnd_y: f32,
    phase_init_x: f32,
    phase_init_y: f32,
    coefficient0: f32,
    coefficient1: f32,
    // Color scale
    color_scale: f32,
    // Soft/Fresnel/Near/Far/Decal alpha params
    soft_edge_param1: f32,
    soft_edge_param2: f32,
    fresnel_alpha_param1: f32,
    fresnel_alpha_param2: f32,
    near_dist_alpha_param1: f32,
    near_dist_alpha_param2: f32,
    far_dist_alpha_param1: f32,
    far_dist_alpha_param2: f32,
    decal_param1: f32,
    decal_param2: f32,
    alpha_threshold: f32,
    add_vel_to_scale: f32,
    soft_partcile_dist: f32,
    soft_particle_volume: f32,
    // Scale limit
    scale_limit_dist_near: f32,
    scale_limit_dist_far: f32,
    // Rotation
    rotate_regist: f32,
    rotate_add_x: f32,
    rotate_add_y: f32,
    rotate_add_z: f32,
    rotate_add_rand_x: f32,
    rotate_add_rand_y: f32,
    rotate_add_rand_z: f32,
    rotate_init_x: f32,
    rotate_init_y: f32,
    rotate_init_z: f32,
    rotate_init_rand_x: f32,
    rotate_init_rand_y: f32,
    rotate_init_rand_z: f32,
    // Gravity
    gravity_dir_x: f32,
    gravity_dir_y: f32,
    gravity_dir_z: f32,
    gravity_scale: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct AnimKeyTableJson {
    keys: Vec<AnimKeyJson>,
}

#[derive(serde::Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
struct AnimKeyJson {
    x: f32,
    y: f32,
    z: f32,
    time: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterInfoJson {
    is_particle_draw: u32,
    sort_type: u32,
    calc_type: u32,
    follow_type: u32,
    is_fade_emit: u32,
    is_fade_alpha_fade: u32,
    is_scale_fade: u32,
    random_seed_type: u32,
    is_update_matrix_by_emit: u32,
    test_always: u32,
    interpolate_emission_amount: u32,
    is_alpha_fade_in: u32,
    is_scale_fade_in: u32,
    random_seed: u32,
    draw_path: u32,
    alpha_fade_time: u32,
    fade_in_time: u32,
    trans_x: f32,
    trans_y: f32,
    trans_z: f32,
    trans_rand_x: f32,
    trans_rand_y: f32,
    trans_rand_z: f32,
    rotate_x: f32,
    rotate_y: f32,
    rotate_z: f32,
    rotate_rand_x: f32,
    rotate_rand_y: f32,
    rotate_rand_z: f32,
    scale_x: f32,
    scale_y: f32,
    scale_z: f32,
    color0_r: f32,
    color0_g: f32,
    color0_b: f32,
    color0_a: f32,
    color1_r: f32,
    color1_g: f32,
    color1_b: f32,
    color1_a: f32,
    emission_range_near: f32,
    emission_range_far: f32,
    emission_ratio_far: f32,
}

#[derive(serde::Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase")]
struct EmissionJson {
    #[serde(rename = "isOneTime")]
    is_one_time: bool,
    is_world_gravity: bool,
    is_emit_dist_enabled: bool,
    is_world_oriented_velocity: bool,
    start: u32,
    timing: u32,
    duration: u32,
    rate: f32,
    rate_random: f32,
    interval: u32,
    interval_random: f32,
    position_random: f32,
    gravity_scale: f32,
    gravity_dir_x: f32,
    gravity_dir_y: f32,
    gravity_dir_z: f32,
    emitter_dist_unit: f32,
    emitter_dist_min: f32,
    emitter_dist_max: f32,
    emitter_dist_marg: f32,
    emitter_dist_particles_max: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ShapeInfoJson {
    volume_type: u32,
    sweep_start_random: u32,
    arc_type: u32,
    is_volume_latitude_enabled: u32,
    volume_tbl_index: u32,
    volume_tbl_index64: u64,
    volume_latitude_dir: u32,
    is_gpu_emitter: u32,
    sweep_longitude: f32,
    sweep_latitude: f32,
    sweep_start: f32,
    volume_surface_pos_rand: f32,
    caliber_ratio: f32,
    line_center: f32,
    line_length: f32,
    volume_radius_x: f32,
    volume_radius_y: f32,
    volume_radius_z: f32,
    volume_form_scale_x: f32,
    volume_form_scale_y: f32,
    volume_form_scale_z: f32,
    prim_emit_type: u32,
    #[serde(alias = "PrimitiveIndex")]
    primitive_index: u64,
    num_divide_circle: u32,
    num_divide_circle_random: u32,
    num_divide_line: u32,
    num_divide_line_random: u32,
    is_on_another_binary_volume_primitive: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RenderStateJson {
    is_blend_enable: bool,
    is_depth_test: bool,
    depth_func: u32,
    is_depth_mask: bool,
    is_alpha_test: bool,
    alpha_func: u32,
    blend_type: u32,
    display_side: u32,
    alpha_threshold: f32,
}

#[derive(serde::Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase")]
struct ParticleDataJson {
    infinite_life: bool,
    is_triming: bool,
    billboard_type: u32,
    rot_type: u32,
    offset_type: u32,
    #[serde(default = "default_color_scale")]
    color_scale: f32,
    rot_rev_rand_x: bool,
    rot_rev_rand_y: bool,
    rot_rev_rand_z: bool,
    is_rotate_x: bool,
    is_rotate_y: bool,
    is_rotate_z: u32,
    primitive_scale_type: u32,
    is_texture_common_random: u32,
    connect_ptcl_scale_and_z_offset: u32,
    enable_avoid_z_fighting: u32,
    life: u32,
    life_random: u32,
    momentum_random: f32,
    primitive_vertex_info_flags: u32,
    #[serde(rename = "PrimitiveID")]
    primitive_id: u64,
    #[serde(rename = "PrimitiveExID", default)]
    primitive_ex_id: u64,
    loop_color0: bool,
    loop_alpha0: bool,
    loop_color1: bool,
    loop_alpha1: bool,
    scale_loop: bool,
    loop_random_color0: bool,
    loop_random_alpha0: bool,
    loop_random_color1: bool,
    loop_random_alpha1: bool,
    scale_loop_random: bool,
    prim_flag1: u32,
    prim_flag2: u32,
    color0_loop_rate: u32,
    alpha0_loop_rate: u32,
    color1_loop_rate: u32,
    alpha1_loop_rate: u32,
    scale_loop_rate: u32,
    color0_loop_rate16: u32,
    alpha0_loop_rate16: u32,
    color1_loop_rate16: u32,
    alpha1_loop_rate16: u32,
    scale_loop_rate16: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleVelocityJson {
    all_direction: f32,
    designated_dir_scale: f32,
    designated_dir_x: f32,
    designated_dir_y: f32,
    designated_dir_z: f32,
    diffusion_dir_angle: f32,
    #[serde(alias = "XZDiffusion")]
    xz_diffusion: f32,
    diffusion_x: f32,
    diffusion_y: f32,
    diffusion_z: f32,
    vel_random: f32,
    em_vel_inherit: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleScaleJson {
    scale_x: f32,
    scale_y: f32,
    scale_z: f32,
    scale_random_x: f32,
    scale_random_y: f32,
    scale_random_z: f32,
    enable_scaling_by_camera_dist_near: u32,
    enable_scaling_by_camera_dist_far: u32,
    enable_add_scale_y: u32,
    enable_link_fovy_to_scale_value: u32,
    scale_min: f32,
    scale_max: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct TexPatAnimJson {
    num: f32,
    frequency: f32,
    num_random: f32,
    #[serde(default)]
    pad: f32,
    #[serde(default)]
    table: Vec<u32>,
    /// UV scale (X component)
    #[serde(default)]
    scale_x: f32,
    /// UV scale (Y component)
    #[serde(default)]
    scale_y: f32,
    /// UV scroll/offset (X component)
    #[serde(default)]
    scroll_x: f32,
    /// UV scroll/offset (Y component)
    #[serde(default)]
    scroll_y: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct TexScrollAnimJson {
    scroll_add_x: f32,
    scroll_add_y: f32,
    scroll_x: f32,
    scroll_y: f32,
    scale_add_x: f32,
    scale_add_y: f32,
    scale_x: f32,
    scale_y: f32,
    /// UV scale per frame (set when sprite-sheet layout is defined)
    /// Note: JSON uses `UVScaleX` (abbreviation uppercase), not `UvScaleX`.
    #[serde(default, alias = "UVScaleX")]
    uv_scale_x: f32,
    #[serde(default, alias = "UVScaleY")]
    uv_scale_y: f32,
    /// Number of UV divisions (columns/rows in the sprite sheet grid)
    /// Note: JSON uses `UVDivX` (abbreviation uppercase), not `UvDivX`.
    #[serde(default, alias = "UVDivX")]
    uv_div_x: f32,
    #[serde(default, alias = "UVDivY")]
    uv_div_y: f32,
    /// UV scroll-add speed
    #[serde(default)]
    scroll_add_uv_x: f32,
    #[serde(default)]
    scroll_add_uv_y: f32,
    /// UV scroll rotation speed (TexScrollAnim.RotationAdd)
    #[serde(default)]
    rotation_add: f32,
    #[serde(default)]
    rotation: f32,
    #[serde(default)]
    rotation_random: f32,
    #[serde(default)]
    rotation_type: f32,
    /// UV distortion strength (VFXB TexScrollAnim[1]+8, clamped [0,1])
    #[serde(default)]
    distortion_strength: f32,
}

// ── Converter: JSON → EmitterDef ──────────────────────────────────────────────

fn convert_emitter_data(
    json: &EmitterDataJson,
    emtr_name: &str,
    bntx_textures: &[TextureRes],
    tex_list: &[TextureRes],
) -> EmitterDef {
    let name = json
        .name
        .as_deref()
        .or_else(|| json.namev40.as_deref())
        .unwrap_or(emtr_name)
        .to_string();

    // ── EmitType from shape_info.volume_type ──────────────────────────────
    let emit_type = json
        .shape_info
        .as_ref()
        .map(|s| EmitType::from(s.volume_type))
        .unwrap_or(EmitType::Point);

    // ── Blend type & display side ──────────────────────────────────────────
    let blend_type = json
        .render_state
        .as_ref()
        .map(|r| BlendType::from(r.blend_type))
        .unwrap_or(BlendType::Add);
    let display_side = json
        .render_state
        .as_ref()
        .map(|r| DisplaySide::from(r.display_side))
        .unwrap_or(DisplaySide::Both);

    // ── Emission ───────────────────────────────────────────────────────────
    let emission_rate = json.emission.as_ref().map(|e| e.rate).unwrap_or(8.0);
    let emission_rate_random = json.emission.as_ref().map(|e| e.rate_random).unwrap_or(0.0);
    let is_one_time = json.emission.as_ref().map(|e| e.is_one_time).unwrap_or(false);
    let emission_start = json.emission.as_ref().map(|e| e.start).unwrap_or(0);
    // Emission.Timing is authored in 60ths of a frame, not frames: across every dumped
    // emitter it is exactly 60 (= start at frame 1, 85%) or 0 (= frame 0, 15%). Reading
    // it as frames delayed most one-time emitters by 59 frames — effects never appeared
    // during their moves (which is also why arc effects got hijacked to synthetic trails).
    let emission_timing = json.emission.as_ref().map(|e| e.timing / 60).unwrap_or(0);
    let emission_duration = json.emission.as_ref().map(|e| e.duration).unwrap_or(9999);

    // ── Particle lifetime ─────────────────────────────────────────────────
    let lifetime = json
        .particle_data
        .as_ref()
        .map(|p| p.life as f32)
        .unwrap_or(60.0);
    // LifeRandom / VelRandom / ScaleRandom are authored as PERCENTAGES (0..100 — every
    // dumped value is a round ten ≤ 80). Reading them as raw ± multipliers produced
    // 60x size swings (fire particles at size 290 next to 0.01-floor siblings) and
    // 226-frame fires from a 14-frame base.
    let lifetime_random = json
        .particle_data
        .as_ref()
        .map(|p| p.life_random as f32 / 100.0)
        .unwrap_or(0.0);

    // ── Particle velocity ──────────────────────────────────────────────────
    // nw::eft composes AllDirection (figure-direction speed) and DesignatedDirScale
    // (speed along DesignatedDir) additively — both zero means no emit velocity
    // (movement then comes from gravity / emitter matrix only).
    let initial_speed = json
        .particle_velocity
        .as_ref()
        .map(|v| v.all_direction)
        .unwrap_or(0.0);
    let designated_dir_scale = json
        .particle_velocity
        .as_ref()
        .map(|v| v.designated_dir_scale)
        .unwrap_or(0.0);
    let speed_random = json
        .particle_velocity
        .as_ref()
        .map(|v| v.vel_random / 100.0)
        .unwrap_or(0.0);

    // ── Particle scale ─────────────────────────────────────────────────────
    let scale = json
        .particle_scale
        .as_ref()
        .map(|s| s.scale_x)
        .unwrap_or(1.0);
    let scale_random = json
        .particle_scale
        .as_ref()
        .map(|s| s.scale_random_x / 100.0)
        .unwrap_or(0.0);
    // Authored Y/X billboard ratio; 1.0 when uniform, absent, or X is degenerate (zero-width
    // billboards keep whatever ratio — the extent is zero anyway).
    let scale_aspect_y = json
        .particle_scale
        .as_ref()
        .map(|s| {
            let r = s.scale_y / s.scale_x;
            if r.is_finite() { r } else { 1.0 }
        })
        .unwrap_or(1.0);

    // ── Rotation speed and initial rotation ────
    // Billboards rotate in the screen plane → use Z-axis values.
    // (X/Y rotation is for 3D mesh-type particles, which we render as camera-facing quads.)
    let rotation_speed = json.emitter_static.as_ref().map(|s| s.rotate_add_z).unwrap_or(0.0);
    let rotation_init = json.emitter_static.as_ref().map(|s| s.rotate_init_z).unwrap_or(0.0);
    let rotation_init_random = json.emitter_static.as_ref().map(|s| s.rotate_init_rand_z).unwrap_or(0.0);

    // ── Accel (from gravity direction + scale in EmitterStatic) ────────────
    let accel = json.emitter_static.as_ref().map(|s| {
        let dir = glam::Vec3::new(s.gravity_dir_x, s.gravity_dir_y, s.gravity_dir_z);
        if dir.length_squared() > 0.0 {
            dir.normalize() * s.gravity_scale
        } else {
            glam::Vec3::ZERO
        }
    }).unwrap_or(glam::Vec3::ZERO);

    // ── Gravity space (world vs emitter-local) ─────────────────────────────
    // Applied to `accel` per-particle at spawn (effects::build_spawned_particle). Defaults to
    // world gravity (the common case, and the prior behavior) when emission data is absent.
    let is_world_gravity = json.emission.as_ref().map(|e| e.is_world_gravity).unwrap_or(true);

    // ── Air resistance (per-frame velocity damping) ────────────────────────
    // 1.0 = no drag. Guard against a missing/zero field zeroing all velocity.
    let air_res = json
        .emitter_static
        .as_ref()
        .map(|s| s.air_res)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(1.0);

    // ── Color / alpha / scale animation keys ───────────────────────────────
    let (color0, _color0_keys) = extract_color_keys(json.emitter_static.as_ref().and_then(|s| s.color0.as_ref()));
    let (color1, _color1_keys) = extract_color_keys(json.emitter_static.as_ref().and_then(|s| s.color1.as_ref()));
    let (alpha0_anim, alpha0_keys) =
        extract_alpha_keys(json.emitter_static.as_ref().and_then(|s| s.alpha0.as_ref()), lifetime);
    let (alpha1_anim, alpha1_keys) =
        extract_alpha_keys(json.emitter_static.as_ref().and_then(|s| s.alpha1.as_ref()), lifetime);
    let scale_table = json.emitter_static.as_ref().and_then(|s| s.scale_anim.as_ref());
    let scale_anim = extract_scale_anim(scale_table, lifetime);
    let scale_keys = extract_scale_keys(scale_table, lifetime);

    // Use EmitterInfo base colors as fallback
    let color0 = if color0.is_empty() {
        if let Some(info) = &json.emitter_info {
            vec![ColorKey {
                frame: 0.0,
                r: info.color0_r,
                g: info.color0_g,
                b: info.color0_b,
                a: info.color0_a,
            }]
        } else {
            color0
        }
    } else {
        color0
    };

    // ── Random seed (per-emitter PRNG stream identity) ─────────────────────
    let random_seed = json.emitter_info.as_ref().map(|i| i.random_seed).unwrap_or(0);
    let random_seed_type = json.emitter_info.as_ref().map(|i| i.random_seed_type).unwrap_or(0);

    // ── Emitter transform from EmitterInfo ────────────────────────────────
    let emitter_offset = json
        .emitter_info
        .as_ref()
        .map(|i| glam::Vec3::new(i.trans_x, i.trans_y, i.trans_z))
        .unwrap_or(glam::Vec3::ZERO);
    let emitter_rotation = json
        .emitter_info
        .as_ref()
        .map(|i| glam::Vec3::new(i.rotate_x, i.rotate_y, i.rotate_z))
        .unwrap_or(glam::Vec3::ZERO);
    let emitter_scale = json
        .emitter_info
        .as_ref()
        .map(|i| glam::Vec3::new(i.scale_x, i.scale_y, i.scale_z))
        .unwrap_or(glam::Vec3::ONE);
    let trans_rand = json
        .emitter_info
        .as_ref()
        .map(|i| glam::Vec3::new(i.trans_rand_x, i.trans_rand_y, i.trans_rand_z))
        .unwrap_or(glam::Vec3::ZERO);
    let follow_type = json
        .emitter_info
        .as_ref()
        .map(|i| crate::effects::FollowType::from(i.follow_type))
        .unwrap_or(crate::effects::FollowType::Srt);
    let is_update_matrix_by_emit = json
        .emitter_info
        .as_ref()
        .map(|i| i.is_update_matrix_by_emit != 0)
        .unwrap_or(false);
    let draw_path = json
        .emitter_info
        .as_ref()
        .map(|i| i.draw_path)
        .unwrap_or(0);
    let (flags1, flags2, flags3, flags4) = json
        .emitter_static
        .as_ref()
        .map(|s| (s.flags1, s.flags2, s.flags3, s.flags4))
        .unwrap_or((0, 0, 0, 0));
    let color_scale = json
        .emitter_static
        .as_ref()
        .map(|s| s.color_scale)
        .unwrap_or(1.0);
    let (billboard_type, rot_type, offset_type, rot_axis_x, rot_axis_y, rot_axis_z) = json
        .particle_data
        .as_ref()
        .map(|p| {
            (
                crate::effects::BillboardType::from(p.billboard_type),
                p.rot_type,
                p.offset_type,
                p.is_rotate_x,
                p.is_rotate_y,
                p.is_rotate_z != 0,
            )
        })
        .unwrap_or((crate::effects::BillboardType::Billboard, 0, 0, false, false, false));
    let position_random = json.emission.as_ref().map(|e| e.position_random).unwrap_or(0.0);
    let (
        volume_radius,
        volume_form_scale,
        line_length,
        line_center,
        volume_surface_pos_rand,
        sweep_longitude,
        sweep_latitude,
        sweep_start,
        sweep_start_random,
        arc_type,
        num_divide_circle,
        num_divide_circle_random,
        num_divide_line,
        num_divide_line_random,
        is_volume_latitude_enabled,
        volume_tbl_index,
        volume_tbl_index64,
        volume_latitude_dir,
        caliber_ratio,
        prim_emit_type,
        shape_primitive_index,
    ) = json.shape_info.as_ref().map(|s| {
        (
            glam::Vec3::new(s.volume_radius_x, s.volume_radius_y, s.volume_radius_z),
            glam::Vec3::new(
                s.volume_form_scale_x,
                s.volume_form_scale_y,
                s.volume_form_scale_z,
            ),
            s.line_length,
            s.line_center,
            s.volume_surface_pos_rand,
            s.sweep_longitude,
            s.sweep_latitude,
            s.sweep_start,
            s.sweep_start_random != 0,
            crate::effects::ArcType::from(s.arc_type as u8),
            s.num_divide_circle,
            s.num_divide_circle_random,
            s.num_divide_line,
            s.num_divide_line_random,
            s.is_volume_latitude_enabled != 0,
            s.volume_tbl_index as u8,
            s.volume_tbl_index64 as u8,
            s.volume_latitude_dir as u8,
            s.caliber_ratio,
            s.prim_emit_type,
            s.primitive_index,
        )
    })
    .unwrap_or((
        glam::Vec3::ONE,
        glam::Vec3::ONE,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        crate::effects::ArcType::Random,
        0,
        0,
        0,
        0,
        false,
        0,
        0,
        0,
        0.0,
        0,
        0,
    ));
    let particle_primitive_id = json
        .particle_data
        .as_ref()
        .map(|p| p.primitive_id)
        .unwrap_or(0);
    let rotate_rand = json
        .emitter_info
        .as_ref()
        .map(|i| glam::Vec3::new(i.rotate_rand_x, i.rotate_rand_y, i.rotate_rand_z))
        .unwrap_or(glam::Vec3::ZERO);
    let (
        is_emit_dist_enabled,
        emitter_dist_unit,
        emitter_dist_min,
        emitter_dist_max,
        emitter_dist_marg,
        emitter_dist_particles_max,
    ) = json
        .emission
        .as_ref()
        .map(|e| {
            (
                e.is_emit_dist_enabled,
                e.emitter_dist_unit,
                e.emitter_dist_min,
                e.emitter_dist_max,
                e.emitter_dist_marg,
                e.emitter_dist_particles_max,
            )
        })
        .unwrap_or((false, 1.0, 0.0, 0.0, 0.0, 0));
    let (designated_dir, use_omnidirectional) = json
        .particle_velocity
        .as_ref()
        .map(|v| {
            (
                glam::Vec3::new(v.designated_dir_x, v.designated_dir_y, v.designated_dir_z),
                v.all_direction.abs() > 0.001,
            )
        })
        .unwrap_or((glam::Vec3::Z, true));

    // ── Texture UV data ────────────────────────────────────────────────────
    let tex_pat_anim0 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim0.as_ref());
    let scroll0 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_scroll_anim0.as_ref());

    let tex_anim0 = json.texture_anim0.as_ref();
    let tex_flags0 = texture_anim_flags_from_json(tex_anim0, scroll0);
    let tex_pattern_anim_type = tex_flags0.pattern_anim_type;
    let tex_is_scroll = tex_flags0.is_scroll;
    let tex_is_rotate = tex_flags0.is_rotate;
    let tex_is_scale = tex_flags0.is_scale;

    let tex_pat_frame_table: Vec<usize> = tex_pat_anim0
        .map(|t| {
            t.table
                .iter()
                .take(t.num.max(0.0) as usize)
                .map(|&frame| frame as usize)
                .collect()
        })
        .unwrap_or_default();

    let inferred_frame_count = tex_pat_frame_table
        .iter()
        .copied()
        .max()
        .map(|max_frame| max_frame + 1)
        .unwrap_or_else(|| {
            tex_pat_anim0
                .map(|t| t.num.max(1.0) as usize)
                .unwrap_or(1)
        })
        .max(1);

    let has_pattern_data = inferred_frame_count > 1
        || !tex_pat_frame_table.is_empty()
        || tex_pattern_anim_type > 0;

    let tex_uv_div = scroll0
        .map(|s| {
            let x = s.uv_div_x.round().max(0.0) as u32;
            let y = s.uv_div_y.round().max(0.0) as u32;
            [x, y]
        })
        .unwrap_or([0, 0]);
    let tex_scale_uv = {
        // Prefer UVScale/UVDiv from TexScrollAnim (describes frame layout in atlas)
        let (su, sv) = if let Some(s) = scroll0 {
            let u = if s.uv_scale_x > 0.0 { s.uv_scale_x } else if s.uv_div_x > 1.0 { 1.0 / s.uv_div_x } else { 0.0 };
            let v = if s.uv_scale_y > 0.0 { s.uv_scale_y } else if s.uv_div_y > 1.0 { 1.0 / s.uv_div_y } else { 0.0 };
            (u, v)
        } else {
            (0.0, 0.0)
        };
        if su > 0.0 && sv > 0.0 {
            [su, sv]
        } else {
            // Fallback: use scale from TexPatAnim or infer from frame count (vertical strip)
            let s = tex_pat_anim0
                .map(|t| [t.scale_x, t.scale_y])
                .unwrap_or([1.0, 1.0]);
            let scale_x = if s[0] > 0.0 { s[0] } else { 1.0 };
            let scale_y = if s[1] > 0.0 {
                s[1]
            } else if inferred_frame_count > 1 {
                1.0 / inferred_frame_count as f32
            } else {
                1.0
            };
            [scale_x, scale_y]
        }
    };
    eprintln!("[UV] tex_scale_uv={:?} frame_count={} scroll0.uv_scale={},{} uv_div={},{}",
        [tex_scale_uv[0], tex_scale_uv[1]], inferred_frame_count,
        scroll0.map(|s| s.uv_scale_x).unwrap_or(0.0),
        scroll0.map(|s| s.uv_scale_y).unwrap_or(0.0),
        scroll0.map(|s| s.uv_div_x).unwrap_or(0.0),
        scroll0.map(|s| s.uv_div_y).unwrap_or(0.0),
    );
    let tex_offset_uv = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim0.as_ref())
        .map(|t| [t.scroll_x, t.scroll_y])
        .or_else(|| {
            json.emitter_static
                .as_ref()
                .and_then(|s| s.tex_scroll_anim0.as_ref())
                .map(|t| [t.scroll_x, t.scroll_y])
        })
        .unwrap_or([0.0, 0.0]);

    let tex_pat_frame_count = if tex_is_scroll && !has_pattern_data {
        1
    } else {
        inferred_frame_count
    };
    let tex_pat_frequency = tex_pat_anim0
        .map(|t| if t.frequency > 0.0 { t.frequency } else { 1.0 })
        .unwrap_or(1.0);

    let tex_scroll_uv = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_scroll_anim0.as_ref())
        .map(|t| [t.scroll_add_x, t.scroll_add_y])
        .unwrap_or([0.0, 0.0]);

    let (tex_scroll_rotation, tex_scroll_rotation_add) = (
        tex_flags0.scroll_rotation,
        tex_flags0.scroll_rotation_add,
    );

    let scroll1 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_scroll_anim1.as_ref());
    let indirect_anim = texture_anim_flags_from_json(json.texture_anim1.as_ref(), scroll1);
    let indirect_pat_anim1 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim1.as_ref());
    let (indirect_pat_frame_table, indirect_pat_frame_count, indirect_pat_frequency) =
        pat_anim_meta(indirect_pat_anim1);

    let scroll2 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_scroll_anim2.as_ref());
    let tex2_anim = texture_anim_flags_from_json(json.texture_anim2.as_ref(), scroll2);

    // ── Textures ───────────────────────────────────────────────────────────
    let textures: Vec<TextureRes> = tex_list.to_vec();

    // mesh_type & primitive_index are set in load_dump after BFRES model loading.
    // mesh_type 0 = billboard (default), 1 = PRMA primitive (not used with converter),
    // 2 = BFRES model (set in the loading loop when .bfres files are found).
    let mesh_type = 0;
    let primitive_index = 0;

    // ── Indirect texture fields ────────────────────────────────────────────
    // tex_list is in GAME sampler-slot order; the indirect texture can sit at slot 0
    // (smokeBomb: Sampler0 = ef_cmn_bomb_indirect00, Sampler1 = ef_cmn_smoke11). The
    // editor's colour/alpha caches still assume colour-first, so classify by name.
    let indirect_pos = tex_list
        .iter()
        .position(|t| t.tex_name.to_lowercase().contains("indirect"));
    let is_indirect_slot1 = indirect_pos.is_some() && tex_list.len() >= 2;
    let distortion_strength = json.emitter_static.as_ref()
        .and_then(|s| s.tex_scroll_anim1.as_ref())
        .map(|t| t.distortion_strength.clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let indirect_scroll_uv = raw_scroll(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_scroll_anim1.as_ref()),
    );
    let indirect_tex_scale_uv = raw_uv_scale(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_pattern_anim1.as_ref()),
    );
    let indirect_tex_offset_uv = raw_uv_offset(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_pattern_anim1.as_ref()),
    );

    // ── Slot-2 texture UV data (from TexPatternAnim2 / TexScrollAnim2) ────
    let tex2_scale_uv = raw_uv_scale(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_pattern_anim2.as_ref()),
    );
    let tex2_offset_uv = raw_uv_offset(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_pattern_anim2.as_ref()),
    );
    let tex2_scroll_uv = raw_scroll(
        json.emitter_static
            .as_ref()
            .and_then(|s| s.tex_scroll_anim2.as_ref()),
    );
    let tex2_pat_anim2 = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim2.as_ref());
    let tex2_pat_frame_table: Vec<usize> = tex2_pat_anim2
        .map(|t| {
            t.table
                .iter()
                .take(t.num.max(0.0) as usize)
                .map(|&frame| frame as usize)
                .collect()
        })
        .unwrap_or_default();
    let tex2_pat_frame_count = tex2_pat_frame_table
        .iter()
        .copied()
        .max()
        .map(|max_frame| max_frame + 1)
        .unwrap_or_else(|| {
            tex2_pat_anim2
                .map(|t| t.num.max(1.0) as usize)
                .unwrap_or(1)
        })
        .max(1);
    let tex2_pat_frequency = tex2_pat_anim2
        .map(|t| if t.frequency > 0.0 { t.frequency } else { 1.0 })
        .unwrap_or(1.0);

    let combiner_state = json.combiner.as_ref().map(combiner_from_json).unwrap_or_default();
    fn sampler_wrap_to_u8(s: &str) -> u8 {
        match s.to_lowercase().as_str() {
            "repeat" | "wrap" => 0,
            "mirror" | "mirrored_repeat" => 1,
            "clamp" | "clamp_to_edge" | "clamp_to_border" => 2,
            _ => 2,
        }
    }
    let def_wrap = 2u8;
    let tex_anims_extra = [
        texture_anim_flags_from_json(
            json.texture_anim3.as_ref(),
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim3.as_ref()),
        ),
        texture_anim_flags_from_json(
            json.texture_anim4.as_ref(),
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim4.as_ref()),
        ),
        texture_anim_flags_from_json(
            json.texture_anim5.as_ref(),
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim5.as_ref()),
        ),
    ];
    let tex_extra_slots = {
        let pat = [
            json.emitter_static.as_ref().and_then(|s| s.tex_pattern_anim3.as_ref()),
            json.emitter_static.as_ref().and_then(|s| s.tex_pattern_anim4.as_ref()),
            json.emitter_static.as_ref().and_then(|s| s.tex_pattern_anim5.as_ref()),
        ];
        let scroll = [
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim3.as_ref()),
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim4.as_ref()),
            json.emitter_static.as_ref().and_then(|s| s.tex_scroll_anim5.as_ref()),
        ];
        let extra_samplers = [&json.sampler3, &json.sampler4, &json.sampler5];
        [0, 1, 2].map(|i| {
            let (table, count, freq) = pat_anim_meta(pat[i]);
            let (wrap_u, wrap_v) = extra_samplers[i]
                .as_ref()
                .map(|s| {
                    (
                        sampler_wrap_to_u8(&s.wrap_u),
                        sampler_wrap_to_u8(&s.wrap_v),
                    )
                })
                .unwrap_or((def_wrap, def_wrap));
            TexExtraSlotDef {
                scale_uv: raw_uv_scale(pat[i]),
                offset_uv: raw_uv_offset(pat[i]),
                scroll_uv: raw_scroll(scroll[i]),
                pat_frame_count: count,
                pat_frame_table: table,
                pat_frequency: freq,
                wrap_u,
                wrap_v,
            }
        })
    };
    let _ = crate::combiner::combiner_texture_slots_used(&combiner_state);

    // ── texture_index ──────────────────────────────────────────────────────
    // The editor's colour texture (drives aspect / UV-grid inference / slot-0 cache):
    // the first NON-indirect entry — the indirect UV-distortion map can occupy game
    // sampler slot 0 and must not become the colour texture (smokeBomb rendered its
    // smoke through the 128² indirect map's inferred flipbook grid → full-width quads).
    let color_tex_name = tex_list
        .iter()
        .enumerate()
        .find(|(i, _)| Some(*i) != indirect_pos)
        .map(|(_, t)| t.tex_name.clone());
    let texture_index = match (&color_tex_name, bntx_textures.is_empty()) {
        (Some(name), false) => bntx_textures
            .iter()
            .position(|t| &t.tex_name == name)
            .unwrap_or(0) as u32,
        _ => 0,
    };

    // ── Sampler wrap modes (Texture0–1) ────────────────────────────────────
    let tex_wrap_u = json.sampler0.as_ref().map(|s| sampler_wrap_to_u8(&s.wrap_u)).unwrap_or(def_wrap);
    let tex_wrap_v = json.sampler0.as_ref().map(|s| sampler_wrap_to_u8(&s.wrap_v)).unwrap_or(def_wrap);
    let tex_filter = json.sampler0.as_ref().map(|s| s.filter as u8).unwrap_or(0);
    let tex2_wrap_u = json
        .sampler2
        .as_ref()
        .map(|s| sampler_wrap_to_u8(&s.wrap_u))
        .or_else(|| json.sampler1.as_ref().map(|s| sampler_wrap_to_u8(&s.wrap_u)))
        .unwrap_or(def_wrap);
    let tex2_wrap_v = json
        .sampler2
        .as_ref()
        .map(|s| sampler_wrap_to_u8(&s.wrap_v))
        .or_else(|| json.sampler1.as_ref().map(|s| sampler_wrap_to_u8(&s.wrap_v)))
        .unwrap_or(def_wrap);

    EmitterDef {
        name,
        emit_type,
        blend_type,
        display_side,
        emission_rate,
        emission_rate_random,
        initial_speed,
        speed_random,
        accel,
        is_world_gravity,
        air_res,
        lifetime,
        lifetime_random,
        scale,
        scale_random,
        rotation_speed,
        rotation_init,
        rotation_init_random,
        color0,
        color1,
        alpha0: alpha0_anim,
        alpha1: alpha1_anim,
        alpha0_keys,
        alpha1_keys,
        scale_anim,
        scale_keys,
        textures,
        mesh_type,
        primitive_index,
        texture_index,
        tex_scale_uv,
        tex_offset_uv,
        tex_scroll_uv,
        tex_uv_div,
        tex_pat_frame_count,
        tex_pat_frame_table,
        tex_pat_frequency,
        tex_pattern_anim_type,
        tex_is_scroll,
        tex_is_rotate,
        tex_is_scale,
        tex_scroll_rotation,
        tex_scroll_rotation_add,
        tex_inv_rand_u: tex_flags0.inv_rand_u,
        tex_inv_rand_v: tex_flags0.inv_rand_v,
        tex_pat_loop_random: tex_flags0.pat_loop_random,
        tex_crossfade: tex_flags0.crossfade,
        indirect_anim,
        indirect_pat_frame_count,
        indirect_pat_frame_table,
        indirect_pat_frequency,
        tex2_anim,
        tex2_pat_frequency,
        tex_anims_extra,
        tex_extra_slots,
        emitter_offset,
        emitter_rotation,
        emitter_scale,
        trans_rand,
        position_random,
        follow_type,
        is_update_matrix_by_emit,
        billboard_type,
        rot_type,
        rot_axis_x,
        rot_axis_y,
        rot_axis_z,
        offset_type,
        draw_path,
        flags1,
        flags2,
        flags3,
        flags4,
        color_scale,
        volume_radius,
        volume_form_scale,
        line_length,
        line_center,
        volume_surface_pos_rand,
        sweep_longitude,
        sweep_latitude,
        sweep_start,
        sweep_start_random,
        arc_type,
        num_divide_circle,
        num_divide_circle_random,
        num_divide_line,
        num_divide_line_random,
        is_volume_latitude_enabled,
        volume_tbl_index,
        volume_tbl_index64,
        volume_latitude_dir,
        caliber_ratio,
        prim_emit_type,
        shape_primitive_index,
        particle_primitive_id,
        rotate_rand,
        is_emit_dist_enabled,
        emitter_dist_unit,
        emitter_dist_min,
        emitter_dist_max,
        emitter_dist_marg,
        emitter_dist_particles_max,
        designated_dir,
        designated_dir_scale,
        scale_aspect_y,
        random_seed,
        random_seed_type,
        use_omnidirectional,
        is_world_oriented_velocity: json
            .emission
            .as_ref()
            .map(|e| e.is_world_oriented_velocity)
            .unwrap_or(false),
        diffusion_dir_angle: json
            .particle_velocity
            .as_ref()
            .map(|v| v.diffusion_dir_angle)
            .unwrap_or(0.0),
        diffusion_axis: json
            .particle_velocity
            .as_ref()
            .map(|v| glam::Vec3::new(v.diffusion_x, v.diffusion_y, v.diffusion_z))
            .unwrap_or(glam::Vec3::ZERO),
        xz_diffusion: json
            .particle_velocity
            .as_ref()
            .map(|v| v.xz_diffusion)
            .unwrap_or(0.0),
        em_vel_inherit: json
            .particle_velocity
            .as_ref()
            .map(|v| v.em_vel_inherit)
            .unwrap_or(0.0),
        child_inheritance: json
            .child_inheritance
            .as_ref()
            .map(|c| crate::effects::ChildInheritanceDef {
                inherit_velocity: c.velocity != 0,
                inherit_scale: c.scale != 0,
                inherit_rotate: c.rotate != 0,
                inherit_color0: c.color0 != 0,
                inherit_color1: c.color1 != 0,
                inherit_alpha0: c.alpha0 != 0,
                inherit_alpha1: c.alpha1 != 0,
                inherit_color_scale: c.color_scale != 0,
                inherit_draw_path: c.draw_path != 0,
                inherit_pre_draw: c.pre_draw != 0,
                inherit_alpha0_each_frame: c.alpha0_each_frame != 0,
                inherit_alpha1_each_frame: c.alpha1_each_frame != 0,
                velocity_rate: if c.velocity_rate == 0.0 { 1.0 } else { c.velocity_rate },
                scale_rate: if c.scale_rate == 0.0 { 1.0 } else { c.scale_rate },
                spawn_from_parent_particle: c.enable_emitter_particle != 0,
                parent_emitter_idx: json
                    .action
                    .as_ref()
                    .map(|a| a.action_index)
                    .unwrap_or(0),
            })
            .unwrap_or_default(),
        is_one_time,
        emission_start,
        emission_timing,
        emission_duration,
        is_indirect_slot1,
        distortion_strength,
        indirect_scroll_uv,
        indirect_tex_scale_uv,
        indirect_tex_offset_uv,
        tex2_scale_uv,
        tex2_offset_uv,
        tex2_scroll_uv,
        tex2_pat_frame_count,
        tex2_pat_frame_table,
        tex_wrap_u,
        tex_wrap_v,
        tex_filter,
        tex2_wrap_u,
        tex2_wrap_v,
        anim_translate: None,
        anim_rotation: None,
        anim_emit_scale: None,
        anim_tex_scale: None,
        anim_color0: None,
        anim_color1: None,
        anim_alpha: None,
        shader_index: json.shader_references.as_ref().map(|r| r.shader_index).unwrap_or(-1),
        custom_shader_index: json.shader_references.as_ref().map(|r| r.custom_shader_index).unwrap_or(0),
        user_shader_indices: [
            json.shader_references.as_ref().map(|r| r.user_shader_index1).unwrap_or(-1),
            json.shader_references.as_ref().map(|r| r.user_shader_index2).unwrap_or(-1),
        ],
        shader_key: 0,
        combiner: combiner_state,
        particle_color: particle_color_from_emitter_json(json),
        particle_scale: particle_scale_from_json(json.particle_scale.as_ref()),
        ..Default::default()
    }
}

fn combiner_from_json(c: &CombinerJson) -> CombinerState {
    let (texture3_color_blend, texture3_alpha_blend, texture4_color_blend, texture4_alpha_blend, texture5_color_blend, texture5_alpha_blend, has_v50_extra_tex_blend) =
        v50_extra_tex_blends_from_json(c);
    CombinerState {
        color_combiner_process: c.color_combiner_process,
        alpha_combiner_process: c.alpha_combiner_process,
        texture1_color_blend: c.texture1_color_blend,
        texture2_color_blend: c.texture2_color_blend,
        primitive_color_blend: c.primitive_color_blend,
        texture1_alpha_blend: c.texture1_alpha_blend,
        texture2_alpha_blend: c.texture2_alpha_blend,
        primitive_alpha_blend: c.primitive_alpha_blend,
        tex_color0_input_type: c.tex_color0_input_type,
        tex_color1_input_type: c.tex_color1_input_type,
        tex_color2_input_type: c.tex_color2_input_type,
        tex_alpha0_input_type: c.tex_alpha0_input_type,
        tex_alpha1_input_type: c.tex_alpha1_input_type,
        tex_alpha2_input_type: c.tex_alpha2_input_type,
        primitive_color_input_type: c.primitive_color_input_type,
        primitive_alpha_input_type: c.primitive_alpha_input_type,
        shader_type: c.shader_type,
        apply_alpha: c.apply_alpha,
        is_distortion_by_camera_distance: c.is_distortion_by_camera_distance,
        texture3_color_blend,
        texture3_alpha_blend,
        texture4_color_blend,
        texture4_alpha_blend,
        texture5_color_blend,
        texture5_alpha_blend,
        has_v50_extra_tex_blend,
    }
}

/// Decode v50+ EmitterCombinerV40 padding fields into dedicated tex3–5 blend bytes.
///
/// Binary layout (v50+): `Padding` short (tex3 colour/alpha), `Padding2` uint
/// (tex4 colour/alpha), and `Padding3` uint (tex5 colour/alpha). When `Padding3` is
/// absent from JSON, tex5 is read from the high bytes of `Padding2` for older exports.
/// When only the legacy `Padding2`/`Padding3` uint pair is present (no `Padding` short),
/// fall back to the pre-v50 split decode.
fn v50_extra_tex_blends_from_json(
    c: &CombinerJson,
) -> (u32, u32, u32, u32, u32, u32, bool) {
    if let Some(pad) = c.padding {
        let b = pad.to_le_bytes();
        let p2 = c.padding2.unwrap_or(0).to_le_bytes();
        let (tex5_color, tex5_alpha) = if let Some(p3) = c.padding3 {
            let b3 = p3.to_le_bytes();
            (b3[0] as u32, b3[1] as u32)
        } else {
            (p2[2] as u32, p2[3] as u32)
        };
        return (
            b[0] as u32,
            b[1] as u32,
            p2[0] as u32,
            p2[1] as u32,
            tex5_color,
            tex5_alpha,
            true,
        );
    }
    let Some(p2) = c.padding2 else {
        return (0, 0, 0, 0, 0, 0, false);
    };
    let p3 = c.padding3.unwrap_or(0);
    let b2 = p2.to_le_bytes();
    let b3 = p3.to_le_bytes();
    (
        b2[0] as u32,
        b2[1] as u32,
        b2[2] as u32,
        b2[3] as u32,
        b3[0] as u32,
        b3[1] as u32,
        true,
    )
}

fn particle_color_from_emitter_json(json: &EmitterDataJson) -> ParticleColorState {
    let mut state = json
        .particle_color
        .as_ref()
        .map(particle_color_flags_from_json)
        .unwrap_or_default();
    if let Some(s) = json.emitter_static.as_ref() {
        if s.soft_particle_volume != 0.0 {
            state.soft_particle_volume = s.soft_particle_volume;
        }
        if s.soft_edge_param1 != 0.0 || s.soft_edge_param2 != 0.0 {
            state.soft_edge_param1 = s.soft_edge_param1;
            state.soft_edge_param2 = s.soft_edge_param2;
        }
        if s.soft_partcile_dist != 0.0 {
            state.soft_particle_dist = s.soft_partcile_dist;
        }
        if s.fresnel_alpha_param1 != 0.0 {
            state.fresnel_alpha_param1 = s.fresnel_alpha_param1;
        }
        if s.fresnel_alpha_param2 != 0.0 {
            state.fresnel_alpha_param2 = s.fresnel_alpha_param2;
        }
        if s.near_dist_alpha_param1 != 0.0 {
            state.near_dist_alpha_param1 = s.near_dist_alpha_param1;
        }
        if s.near_dist_alpha_param2 != 0.0 {
            state.near_dist_alpha_param2 = s.near_dist_alpha_param2;
        }
        if s.far_dist_alpha_param1 != 0.0 {
            state.far_dist_alpha_param1 = s.far_dist_alpha_param1;
        }
        if s.far_dist_alpha_param2 != 0.0 {
            state.far_dist_alpha_param2 = s.far_dist_alpha_param2;
        }
    }
    state
}

fn particle_scale_from_json(json: Option<&ParticleScaleJson>) -> ParticleScaleState {
    json.map(|s| {
        ParticleScaleState::from_fields(
            s.enable_scaling_by_camera_dist_near,
            s.enable_scaling_by_camera_dist_far,
            s.scale_min,
            s.scale_max,
        )
    })
    .unwrap_or_default()
}

fn particle_color_flags_from_json(p: &ParticleColorJson) -> ParticleColorState {
    ParticleColorState {
        is_soft_particle: p.is_soft_particle != 0,
        is_fresnel_alpha: p.is_fresnel_alpha != 0,
        is_near_dist_alpha: p.is_near_dist_alpha != 0,
        is_far_dist_alpha: p.is_far_dist_alpha != 0,
        is_decal: p.is_decal != 0,
        ..Default::default()
    }
}

// ── Helper extraction functions ───────────────────────────────────────────────

fn extract_color_keys(table: Option<&AnimKeyTableJson>) -> (Vec<ColorKey>, Vec<ColorKey>) {
    let Some(table) = table else { return (vec![], vec![]) };
    let keys: Vec<ColorKey> = table
        .keys
        .iter()
        .filter(|k| k.time != 0.0 || k.x != 0.0 || k.y != 0.0 || k.z != 0.0)
        .map(|k| ColorKey {
            frame: k.time,
            r: k.x,
            g: k.y,
            b: k.z,
            a: 1.0,
        })
        .collect();
    (keys.clone(), keys)
}

fn extract_alpha_keys(table: Option<&AnimKeyTableJson>, lifetime: f32) -> (AnimKey3v4k, Vec<ColorKey>) {
    let Some(table) = table else {
        return (AnimKey3v4k::default(), vec![]);
    };
    let mut pairs: Vec<(f32, f32)> = table
        .keys
        .iter()
        .filter(|k| k.time != 0.0 || k.x != 0.0)
        .map(|k| (k.time, k.x))
        .collect();
    normalize_anim_key_pairs(&mut pairs, lifetime);
    let anim = build_anim_key_3v4k(&pairs);
    let keys: Vec<ColorKey> = pairs
        .iter()
        .map(|&(t, v)| ColorKey {
            frame: t,
            r: v,
            g: v,
            b: v,
            a: v,
        })
        .collect();
    (anim, keys)
}

fn normalize_anim_key_pairs(pairs: &mut [(f32, f32)], lifetime: f32) {
    if pairs.is_empty() {
        return;
    }
    let max_t = pairs.iter().map(|p| p.0).fold(0.0f32, f32::max);
    let denom = if max_t > 1.0 + 1e-3 {
        max_t.max(lifetime.max(1.0))
    } else {
        1.0
    };
    for (t, _) in pairs.iter_mut() {
        *t = (*t / denom).clamp(0.0, 1.0);
    }
}

/// Full scale key table (up to 8 keys), normalized to [0,1] time, value in every
/// channel so [`crate::effects::sample_alpha`] (reads `.x`) works. Mirrors
/// [`extract_alpha_keys`]. Empty when the table is absent so the sim falls back to
/// the 3v4k approximation.
fn extract_scale_keys(table: Option<&AnimKeyTableJson>, lifetime: f32) -> Vec<ColorKey> {
    let Some(table) = table else { return vec![] };
    let mut pairs: Vec<(f32, f32)> = table
        .keys
        .iter()
        .filter(|k| k.time != 0.0 || k.x != 0.0)
        .map(|k| (k.time, k.x))
        .collect();
    if pairs.is_empty() {
        return vec![];
    }
    normalize_anim_key_pairs(&mut pairs, lifetime);
    pairs
        .iter()
        .map(|&(t, v)| ColorKey { frame: t, r: v, g: v, b: v, a: v })
        .collect()
}

fn extract_scale_anim(table: Option<&AnimKeyTableJson>, lifetime: f32) -> AnimKey3v4k {
    let Some(table) = table else {
        return AnimKey3v4k {
            start_value: 1.0,
            start_diff: 0.0,
            end_diff: 0.0,
            time2: 0.5,
            time3: 0.8,
        };
    };
    if table.keys.is_empty() {
        return AnimKey3v4k {
            start_value: 1.0,
            start_diff: 0.0,
            end_diff: 0.0,
            time2: 0.5,
            time3: 0.8,
        };
    }
    let mut pairs: Vec<(f32, f32)> = table.keys.iter().map(|k| (k.time, k.x)).collect();
    normalize_anim_key_pairs(&mut pairs, lifetime);
    build_anim_key_3v4k(&pairs)
}

fn build_anim_key_3v4k(pairs: &[(f32, f32)]) -> AnimKey3v4k {
    if pairs.is_empty() {
        return AnimKey3v4k::default();
    }
    let start_value = pairs[0].1;
    let (start_diff, time2) = if pairs.len() > 1 {
        (pairs[1].1 - start_value, pairs[1].0)
    } else {
        (0.0, 0.5)
    };
    let (end_diff, time3) = if pairs.len() > 2 {
        let last = pairs.last().unwrap();
        let prev = pairs[pairs.len() - 2];
        (last.1 - prev.1, last.0)
    } else {
        (0.0, 0.8)
    };
    AnimKey3v4k {
        start_value,
        start_diff,
        end_diff,
        time2,
        time3,
    }
}

fn texture_anim_flags_from_json(
    anim: Option<&TextureAnimJson>,
    scroll: Option<&TexScrollAnimJson>,
) -> TextureAnimFlags {
    let (scroll_rotation, scroll_rotation_add) = scroll
        .map(|s| (s.rotation, s.rotation_add))
        .unwrap_or((0.0, 0.0));
    anim.map(|t| TextureAnimFlags {
        pattern_anim_type: t.pattern_anim_type as u8,
        is_scroll: t.is_scroll,
        is_rotate: t.is_rotate,
        is_scale: t.is_scale,
        inv_rand_u: t.inv_rand_u != 0,
        inv_rand_v: t.inv_rand_v != 0,
        pat_loop_random: t.is_pat_anim_loop_random != 0,
        crossfade: t.is_crossfade != 0,
        scroll_rotation,
        scroll_rotation_add,
    })
    .unwrap_or(TextureAnimFlags {
        scroll_rotation,
        scroll_rotation_add,
        ..TextureAnimFlags::default()
    })
}

fn pat_anim_meta(pat: Option<&TexPatAnimJson>) -> (Vec<usize>, usize, f32) {
    let table: Vec<usize> = pat
        .map(|t| {
            t.table
                .iter()
                .take(t.num.max(0.0) as usize)
                .map(|&frame| frame as usize)
                .collect()
        })
        .unwrap_or_default();
    let count = table
        .iter()
        .copied()
        .max()
        .map(|max_frame| max_frame + 1)
        .unwrap_or_else(|| pat.map(|t| t.num.max(1.0) as usize).unwrap_or(1))
        .max(1);
    let frequency = pat
        .map(|t| if t.frequency > 0.0 { t.frequency } else { 1.0 })
        .unwrap_or(1.0);
    (table, count, frequency)
}

fn raw_scroll(anim: Option<&TexScrollAnimJson>) -> [f32; 2] {
    anim.map(|t| [t.scroll_add_x, t.scroll_add_y])
        .unwrap_or([0.0, 0.0])
}

fn raw_uv_scale(anim: Option<&TexPatAnimJson>) -> [f32; 2] {
    let s = anim.map(|t| [t.scale_x, t.scale_y])
        .unwrap_or([1.0, 1.0]);
    [if s[0] > 0.0 { s[0] } else { 1.0 }, if s[1] > 0.0 { s[1] } else { 1.0 }]
}

fn raw_uv_offset(anim: Option<&TexPatAnimJson>) -> [f32; 2] {
    anim.map(|t| [t.scroll_x, t.scroll_y])
        .unwrap_or([0.0, 0.0])
}

fn emit_anim_from_json(src: &EmitterAnimJson) -> crate::effects::EmitterAnimDef {
    crate::effects::EmitterAnimDef {
        enable: src.enable,
        loop_: src.loop_,
        randomize_start_frame: src.randomize_start_frame,
        loop_count: src.loop_count,
        key_frames: src.key_frames.iter().map(|k| crate::effects::AnimKeyframe {
            x: k.x,
            y: k.y,
            z: k.z,
            time: k.time,
        }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sampler2_wrap_for_texture_slot2() {
        let json = EmitterDataJson {
            name: Some("slot2_wrap".to_string()),
            sampler2: Some(SamplerJson {
                wrap_u: "mirror".to_string(),
                wrap_v: "repeat".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "slot2_wrap", &[], &[]);
        assert_eq!(emitter.tex2_wrap_u, 1);
        assert_eq!(emitter.tex2_wrap_v, 0);
    }

    #[test]
    fn maps_v50_combiner_extra_tex_blend_padding() {
        let json = EmitterDataJson {
            name: Some("v50_blend".to_string()),
            combiner: Some(CombinerJson {
                padding2: Some(0x0001_0203),
                padding3: Some(0x0000_0100),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "v50_blend", &[], &[]);
        assert!(emitter.combiner.has_v50_extra_tex_blend);
        assert_eq!(emitter.combiner.texture3_color_blend, 3);
        assert_eq!(emitter.combiner.texture3_alpha_blend, 2);
        assert_eq!(emitter.combiner.texture4_color_blend, 1);
        assert_eq!(emitter.combiner.texture4_alpha_blend, 0);
        assert_eq!(emitter.combiner.texture5_color_blend, 0);
        assert_eq!(emitter.combiner.texture5_alpha_blend, 1);
    }

    #[test]
    fn maps_v50_combiner_padding_short_and_padding2() {
        let json = EmitterDataJson {
            name: Some("v50_blend_short".to_string()),
            combiner: Some(CombinerJson {
                padding: Some(0x0203),
                padding2: Some(0x0100_0001),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "v50_blend_short", &[], &[]);
        assert!(emitter.combiner.has_v50_extra_tex_blend);
        assert_eq!(emitter.combiner.texture3_color_blend, 3);
        assert_eq!(emitter.combiner.texture3_alpha_blend, 2);
        assert_eq!(emitter.combiner.texture4_color_blend, 1);
        assert_eq!(emitter.combiner.texture4_alpha_blend, 0);
        assert_eq!(emitter.combiner.texture5_color_blend, 0);
        assert_eq!(emitter.combiner.texture5_alpha_blend, 1);
    }

    #[test]
    fn maps_v50_combiner_padding3_tex5_when_padding_short_present() {
        let json = EmitterDataJson {
            name: Some("v50_blend_padding3".to_string()),
            combiner: Some(CombinerJson {
                padding: Some(0x0203),
                padding2: Some(0x0000_0100),
                padding3: Some(0x0000_0405),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "v50_blend_padding3", &[], &[]);
        assert!(emitter.combiner.has_v50_extra_tex_blend);
        assert_eq!(emitter.combiner.texture3_color_blend, 3);
        assert_eq!(emitter.combiner.texture3_alpha_blend, 2);
        assert_eq!(emitter.combiner.texture4_color_blend, 0);
        assert_eq!(emitter.combiner.texture4_alpha_blend, 1);
        assert_eq!(emitter.combiner.texture5_color_blend, 5);
        assert_eq!(emitter.combiner.texture5_alpha_blend, 4);
    }

    #[test]
    fn maps_extra_tex_sampler_wrap_from_json() {
        let json = EmitterDataJson {
            name: Some("extra_wrap".to_string()),
            sampler3: Some(SamplerJson {
                wrap_u: "repeat".to_string(),
                wrap_v: "mirror".to_string(),
                ..Default::default()
            }),
            sampler5: Some(SamplerJson {
                wrap_u: "clamp".to_string(),
                wrap_v: "wrap".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "extra_wrap", &[], &[]);
        assert_eq!(emitter.tex_extra_slots[0].wrap_u, 0);
        assert_eq!(emitter.tex_extra_slots[0].wrap_v, 1);
        assert_eq!(emitter.tex_extra_slots[1].wrap_u, 2);
        assert_eq!(emitter.tex_extra_slots[2].wrap_u, 2);
        assert_eq!(emitter.tex_extra_slots[2].wrap_v, 0);
    }

    #[test]
    fn maps_emitter_static_flags_to_emitter_def() {
        let json = EmitterDataJson {
            name: Some("flags".to_string()),
            emitter_static: Some(EmitterStaticJson {
                flags1: 0x40,
                flags2: 0x80,
                flags3: 0x01,
                flags4: 0x02,
                ..Default::default()
            }),
            emitter_info: Some(EmitterInfoJson {
                draw_path: 3,
                ..Default::default()
            }),
            render_state: Some(RenderStateJson {
                display_side: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "flags", &[], &[]);
        assert_eq!(emitter.flags1, 0x40);
        assert_eq!(emitter.flags2, 0x80);
        assert_eq!(emitter.flags3, 0x01);
        assert_eq!(emitter.flags4, 0x02);
        assert_eq!(emitter.draw_path, 3);
        assert_eq!(emitter.display_side, DisplaySide::Front);
    }

    #[test]
    fn maps_texture_anim_flags_and_indirect_pattern() {
        let json = EmitterDataJson {
            name: Some("scroll".to_string()),
            texture_anim0: Some(TextureAnimJson {
                pattern_anim_type: 0,
                is_scroll: true,
                is_rotate: true,
                is_scale: true,
                inv_rand_u: 1,
                is_pat_anim_loop_random: 1,
                is_crossfade: 1,
                ..Default::default()
            }),
            texture_anim1: Some(TextureAnimJson {
                is_scroll: true,
                ..Default::default()
            }),
            emitter_static: Some(EmitterStaticJson {
                tex_pattern_anim0: Some(TexPatAnimJson {
                    num: 1.0,
                    frequency: 0.0,
                    ..Default::default()
                }),
                tex_pattern_anim1: Some(TexPatAnimJson {
                    num: 3.0,
                    frequency: 2.0,
                    table: vec![0, 2, 1],
                    ..Default::default()
                }),
                tex_scroll_anim0: Some(TexScrollAnimJson {
                    rotation: 0.5,
                    rotation_add: 0.1,
                    ..Default::default()
                }),
                tex_scroll_anim1: Some(TexScrollAnimJson {
                    scroll_add_x: 0.2,
                    scroll_add_y: 0.0,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "scroll", &[], &[]);
        assert!(emitter.tex_is_scroll);
        assert!(emitter.tex_is_rotate);
        assert!(emitter.tex_is_scale);
        assert!(emitter.tex_inv_rand_u);
        assert!(emitter.tex_pat_loop_random);
        assert!(emitter.tex_crossfade);
        assert_eq!(emitter.indirect_pat_frame_count, 3);
        assert_eq!(emitter.indirect_pat_frame_table, vec![0, 2, 1]);
        assert!(emitter.indirect_anim.is_scroll);
    }

    #[test]
    fn maps_texture_anim_flags_and_frequency() {
        let json = EmitterDataJson {
            name: Some("scroll".to_string()),
            texture_anim0: Some(TextureAnimJson {
                pattern_anim_type: 0,
                is_scroll: true,
                is_rotate: true,
                is_scale: false,
                ..Default::default()
            }),
            emitter_static: Some(EmitterStaticJson {
                tex_pattern_anim0: Some(TexPatAnimJson {
                    num: 1.0,
                    frequency: 0.0,
                    ..Default::default()
                }),
                tex_scroll_anim0: Some(TexScrollAnimJson {
                    rotation: 0.5,
                    rotation_add: 0.1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "scroll", &[], &[]);
        assert!(emitter.tex_is_scroll);
        assert!(emitter.tex_is_rotate);
        assert_eq!(emitter.tex_pat_frame_count, 1);
        assert_eq!(emitter.tex_pat_frequency, 1.0);
        assert!((emitter.tex_scroll_rotation - 0.5).abs() < 1e-5);
        assert!((emitter.tex_scroll_rotation_add - 0.1).abs() < 1e-5);
    }

    #[test]
    fn converts_tex_pattern_table_to_sprite_atlas_uvs() {
        let json = EmitterDataJson {
            name: Some("atlas".to_string()),
            emitter_static: Some(EmitterStaticJson {
                tex_pattern_anim0: Some(TexPatAnimJson {
                    num: 5.0,
                    frequency: 1.0,
                    num_random: 0.0,
                    table: vec![1, 0, 1, 2, 2, 0, 0, 0],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let tex = TextureRes {
            tex_name: "atlas_tex".to_string(),
            width: 64,
            height: 320,
            ftx_format: 0x0b01,
            ftx_data_offset: 0,
            ftx_data_size: 64 * 320 * 4,
            original_format: 0x0b01,
            original_data_offset: 0,
            original_data_size: 64 * 320 * 4,
            wrap_mode: 1,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0,
        };

        let emitter = convert_emitter_data(&json, "atlas", &[tex.clone()], &[tex]);

        assert_eq!(emitter.tex_pat_frame_table, vec![1, 0, 1, 2, 2]);
        assert_eq!(emitter.tex_pat_frame_count, 3);
        assert_eq!(emitter.tex_scale_uv, [1.0, 1.0 / 3.0]);
    }

    #[test]
    #[ignore = "requires local fox_test dump fixture"]
    fn diagnostic_load_dump_textures() {
        // Load the fox_test dump directory and inspect texture data
        let dump_dir = std::path::Path::new(
            &std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string())
        ).join("fox_test");

        eprintln!("\n[DIAG] fox_test dir: {:?}", dump_dir);
        assert!(dump_dir.is_dir(), "fox_test/ dir not found");

        let ptcl = match load_dump(&dump_dir) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[DIAG] load_dump failed: {e}");
                panic!("load_dump failed: {e}");
            }
        };

        eprintln!("[DIAG] emitter_sets: {}", ptcl.emitter_sets.len());
        eprintln!("[DIAG] bntx_textures: {}", ptcl.bntx_textures.len());
        eprintln!("[DIAG] texture_section: {} bytes", ptcl.texture_section.len());
        eprintln!("[DIAG] bfres_models: {}", ptcl.bfres_models.len());
        eprintln!("[DIAG] shader_binary_1: {} bytes", ptcl.shader_binary_1.len());

        // Dump each emitter's texture_index and the corresponding bntx_textures entry
        for (set_idx, set) in ptcl.emitter_sets.iter().enumerate() {
            for (emtr_idx, emitter) in set.emitters.iter().enumerate() {
                let idx = emitter.texture_index as usize;
                let tex_info = ptcl.bntx_textures.get(idx).map(|t| {
                    format!("{}x{} name='{}' offset={} size={} fmt={:#06x}",
                        t.width, t.height, t.tex_name, t.ftx_data_offset, t.ftx_data_size, t.ftx_format)
                }).unwrap_or_else(|| "NOT FOUND".to_string());
                eprintln!("[DIAG]   set[{}]/emtr[{}] '{}' tex_idx={} mesh_type={} -> {}",
                    set_idx, emtr_idx, emitter.name, idx, emitter.mesh_type, tex_info);
            }
        }

        // Verify at least some textures exist
        assert!(!ptcl.bntx_textures.is_empty(),
            "bntx_textures should not be empty");
        assert!(ptcl.texture_section.len() > 0,
            "texture_section should have data");

        // Count how many emitters have valid texture_index
        let valid = ptcl.emitter_sets.iter().flat_map(|s| &s.emitters)
            .filter(|e| (e.texture_index as usize) < ptcl.bntx_textures.len())
            .count();
        let total: usize = ptcl.emitter_sets.iter().map(|s| s.emitters.len()).sum();
        eprintln!("[DIAG] valid texture_index: {valid}/{total}");
        assert!(valid > 0, "at least some emitters must have valid texture_index");
    }

    #[test]
    fn cache_key_includes_dump_version() {
        let key = super::cache_key_for_bytes(b"test");
        assert!(key.ends_with(&format!("-v{}", super::EFFECT_DUMP_CACHE_VERSION)));
    }

    #[test]
    fn fix_all_emitter_tex_scales_corrects_multi_frame_pat() {
        let tex = TextureRes {
            tex_name: "sheet".to_string(),
            width: 256,
            height: 64,
            ftx_format: 0x0b01,
            ftx_data_offset: 0,
            ftx_data_size: 256 * 64 * 4,
            original_format: 0x0b01,
            original_data_offset: 0,
            original_data_size: 256 * 64 * 4,
            wrap_mode: 1,
            filter_mode: 0,
            mipmap_count: 1,
            channel_swizzle: 0,
        };
        let mut ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "set".to_string(),
                emitters: vec![EmitterDef {
                    name: "e".to_string(),
                    texture_index: 0,
                    tex_scale_uv: [1.0, 1.0],
                    tex_pat_frame_count: 4,
                    ..Default::default()
                }],
            }],
            texture_section: vec![],
            texture_section_offset: 0,
            bntx_textures: vec![tex],
            primitives: vec![],
            bfres_models: vec![],
            shader_registry: ShaderRegistry::default(),
            shader_binary_1: vec![],
            shader_binary_2: vec![],
        };
        super::fix_all_emitter_tex_scales(&mut ptcl);
        assert_eq!(ptcl.emitter_sets[0].emitters[0].tex_scale_uv, [0.25, 1.0]);
    }

    #[test]
    fn parse_prim_binary_reads_positions_and_indices() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0u64.to_le_bytes()); // PrimitiveID
        raw.extend_from_slice(&2i32.to_le_bytes()); // num_positions
        for _ in 0..11 {
            raw.extend_from_slice(&3i32.to_le_bytes());
        }
        raw.extend_from_slice(&3i32.to_le_bytes()); // num_indices
        let pos_off = 84u32;
        let idx_off = 116u32;
        raw.extend_from_slice(&pos_off.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes()); // normal offset
        raw.extend_from_slice(&0u32.to_le_bytes()); // tangent offset
        raw.extend_from_slice(&0u32.to_le_bytes()); // color offset
        raw.extend_from_slice(&0u32.to_le_bytes()); // uv offset
        raw.extend_from_slice(&idx_off.to_le_bytes());
        while raw.len() < pos_off as usize {
            raw.push(0);
        }
        raw.extend_from_slice(&1.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        raw.extend_from_slice(&2.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        raw.extend_from_slice(&0.0f32.to_le_bytes());
        while raw.len() < idx_off as usize {
            raw.push(0);
        }
        raw.extend_from_slice(&0i32.to_le_bytes());
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(&0i32.to_le_bytes());
        let prim = parse_prim_binary(&raw, Some(40)).expect("prim");
        assert_eq!(prim.vertices.len(), 2);
        assert_eq!(prim.vertices[0].position, [1.0, 0.0, 0.0]);
        assert_eq!(prim.vertices[1].position[1], 2.0);
        assert_eq!(prim.indices, vec![0, 1, 0]);
    }

    #[test]
    fn maps_particle_scale_and_distortion_by_camera_distance() {
        let json = EmitterDataJson {
            name: Some("cam_dist_distort".to_string()),
            combiner: Some(CombinerJson {
                is_distortion_by_camera_distance: 1,
                ..Default::default()
            }),
            particle_scale: Some(ParticleScaleJson {
                scale_min: 50.0,
                scale_max: 120.0,
                enable_scaling_by_camera_dist_near: 1,
                enable_scaling_by_camera_dist_far: 0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let emitter = convert_emitter_data(&json, "cam_dist_distort", &[], &[]);
        assert_eq!(emitter.combiner.is_distortion_by_camera_distance, 1);
        assert_eq!(emitter.particle_scale.enable_scaling_by_camera_dist_near, 1);
        assert!((emitter.particle_scale.scale_min - 50.0).abs() < f32::EPSILON);
        assert!((emitter.particle_scale.scale_max - 120.0).abs() < f32::EPSILON);
    }
}
