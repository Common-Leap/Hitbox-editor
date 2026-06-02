/// EffectConverter CLI wrapper — replaces the hand-rolled VFXB/EFTF binary parser.
///
/// At build time, Cargo's build.rs compiles the C# EffectConverter tool from
/// extern/effect-library and exposes its path via `EFFECT_CONVERTER_CLI`.
/// This module invokes the CLI to dump an `.eff` or `.ptcl` file to a folder
/// of JSON + binary files, then re-assembles them into the existing Rust types
/// defined in `effects.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::effects::{
    AnimKey3v4k, BlendType, BfresModel, ColorKey, DisplaySide, EmitType, EmitterDef, EmitterSet,
    PrimitiveData, PtclFile, TextureRes,
};

// ── CLI discovery ─────────────────────────────────────────────────────────────

fn get_cli_path() -> anyhow::Result<PathBuf> {
    if let Ok(cli) = std::env::var("EFFECT_CONVERTER_CLI") {
        let p = PathBuf::from(&cli);
        if p.exists() {
            return Ok(p);
        }
        eprintln!("[EC] EFFECT_CONVERTER_CLI set but not found: {}", cli);
    }
    // Fallback: search PATH
    if let Ok(output) = Command::new("EffectConverter").arg("--help").output() {
        if output.status.success() {
            return Ok(PathBuf::from("EffectConverter"));
        }
    }
    anyhow::bail!(
        "EffectConverter CLI not found. Rebuild with .NET 6.0+ SDK available."
    );
}

// ── Top-level entry point ─────────────────────────────────────────────────────

/// Load a `.eff` file by dumping it via EffectConverter and reading back the dump.
pub fn load_ptcl_from_eff(path: &Path) -> anyhow::Result<PtclFile> {
    let cli = get_cli_path()?;

    // Create a temp directory and copy the .eff file there so the CLI's
    // sibling-dump output doesn't pollute the game-data directory.
    let tmp = tempfile::tempdir()?;
    let tmp_input = tmp.path().join("input.eff");
    std::fs::copy(path, &tmp_input)?;

    let status = Command::new(&cli)
        .arg(&tmp_input)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run EffectConverter: {e}"))?;

    if !status.success() {
        anyhow::bail!("EffectConverter CLI exited with non-zero status");
    }

    // The CLI creates {input}_dump/ next to the input file
    let dump_dir = tmp.path().join("input_dump");
    if !dump_dir.is_dir() {
        anyhow::bail!("EffectConverter did not produce dump directory at {:?}", dump_dir);
    }

    let ptcl = load_dump(&dump_dir)?;
    Ok(ptcl)
}

/// Load a standalone `.ptcl` file by dumping it via EffectConverter.
pub fn load_ptcl_from_ptcl(path: &Path) -> anyhow::Result<PtclFile> {
    let cli = get_cli_path()?;

    let tmp = tempfile::tempdir()?;
    let tmp_input = tmp.path().join("input.ptcl");
    std::fs::copy(path, &tmp_input)?;

    let status = Command::new(&cli)
        .arg(&tmp_input)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run EffectConverter: {e}"))?;

    if !status.success() {
        anyhow::bail!("EffectConverter CLI exited with non-zero status");
    }

    let dump_dir = tmp.path().join("input_dump");
    if !dump_dir.is_dir() {
        anyhow::bail!("EffectConverter did not produce dump directory at {:?}", dump_dir);
    }

    let ptcl = load_dump(&dump_dir)?;
    Ok(ptcl)
}

// ── Dump directory reader ─────────────────────────────────────────────────────

pub(crate) fn load_dump(dump_dir: &Path) -> anyhow::Result<PtclFile> {
    // Read EmitterSetInfo.txt for the ordered list of emitter set folder names
    let eset_info_path = dump_dir.join("EmitterSetInfo.txt");
    let eset_info: EmitterSetInfo = if eset_info_path.exists() {
        let text = std::fs::read_to_string(&eset_info_path)?;
        serde_json::from_str(&text)?
    } else {
        EmitterSetInfo { order: vec![] }
    };

    let mut emitter_sets: Vec<EmitterSet> = Vec::new();
    let mut bntx_textures: Vec<TextureRes> = Vec::new();
    let mut texture_section: Vec<u8> = Vec::new();
    let mut primitives: Vec<PrimitiveData> = Vec::new();
    let mut bfres_models: Vec<BfresModel> = Vec::new();
    let mut shader_binary_1: Vec<u8> = Vec::new();
    let mut shader_binary_2: Vec<u8> = Vec::new();

    for set_name in &eset_info.order {
        let set_dir = dump_dir.join(set_name);
        if !set_dir.is_dir() {
            eprintln!("[EC] emitter set dir not found: {:?}", set_dir);
            continue;
        }

        // Read emitter order
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

        for emtr_name in &emitter_order.order {
            let emtr_dir = set_dir.join(emtr_name);
            if !emtr_dir.is_dir() {
                eprintln!("[EC] emitter dir not found: {:?}", emtr_dir);
                continue;
            }

            // Read EmitterData.json
            let data_path = emtr_dir.join("EmitterData.json");
            let emitter_data: EmitterDataJson = if data_path.exists() {
                match std::fs::read_to_string(&data_path) {
                    Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
                    Err(e) => {
                        eprintln!("[EC] failed to read {}: {e}", data_path.display());
                        EmitterDataJson::default()
                    }
                }
            } else {
                EmitterDataJson::default()
            };

            // Read shader binaries
            let shader_path = emtr_dir.join("Shader.bnsh");
            if shader_path.exists() {
                let bytes = std::fs::read(&shader_path).unwrap_or_default();
                if !bytes.is_empty() {
                    if shader_binary_1.is_empty() {
                        shader_binary_1 = bytes;
                    } else if shader_binary_2.is_empty() {
                        shader_binary_2 = bytes;
                    }
                }
            }

            // Read textures (.bntx files) in this emitter directory
            let mut tex_list: Vec<TextureRes> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&emtr_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("bntx") {
                        if let Ok(bntx_bytes) = std::fs::read(&p) {
                            let (tex_map, _section, _ordered) =
                                crate::effects::parse_bntx_named(&bntx_bytes);
                            if let Some((_, (tex, _pixels))) = tex_map.into_iter().next() {
                                tex_list.push(tex);
                            }
                        }
                    }
                }
            }

            // Build texture_section / bntx_textures from the collected textures
            for tex in &tex_list {
                let tex_clone = tex.clone();
                // Read texture pixels from the original bntx file
                if let Ok(entries) = std::fs::read_dir(&emtr_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("bntx") {
                            if let Ok(bntx_bytes) = std::fs::read(&p) {
                                let (tex_map2, section2, _ordered2) =
                                    crate::effects::parse_bntx_named(&bntx_bytes);
                                for (name, (_tex_res, pixels)) in &tex_map2 {
                                    if *name == tex_clone.tex_name {
                                        let offset = texture_section.len() as u32;
                                        texture_section.extend_from_slice(pixels);
                                        let mut tx = tex_clone.clone();
                                        tx.ftx_data_offset = offset;
                                        tx.ftx_data_size = pixels.len() as u32;
                                        tx.original_data_offset = offset;
                                        tx.original_data_size = pixels.len() as u32;
                                        bntx_textures.push(tx);
                                        break;
                                    }
                                }
                                let _ = section2;
                            }
                        }
                    }
                }
            }

            // Read primitive models (.bfres files)
            if let Ok(entries) = std::fs::read_dir(&emtr_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("bfres") {
                        let bfres_bytes = std::fs::read(&p).unwrap_or_default();
                        if !bfres_bytes.is_empty() {
                            let models = crate::effects::parse_bfres(&bfres_bytes);
                            bfres_models.extend(models);
                        }
                    }
                }
            }

            let emitter = convert_emitter_data(&emitter_data, emtr_name, &bntx_textures, &tex_list);

            emitters.push(emitter);
        }

        emitter_sets.push(EmitterSet {
            name: set_name.clone(),
            emitters,
        });
    }

    // Also read Base.ptcl for texture section / shaders if available
    let base_ptcl = dump_dir.join("Base.ptcl");
    if base_ptcl.exists() {
        if let Ok(base_bytes) = std::fs::read(&base_ptcl) {
            // Use our existing parser for the base file to get additional data
            if let Ok(base_ptcl_file) = crate::effects::PtclFile::parse(&base_bytes) {
                if texture_section.is_empty() {
                    texture_section = base_ptcl_file.texture_section;
                }
                if bntx_textures.is_empty() {
                    bntx_textures = base_ptcl_file.bntx_textures;
                }
                if primitives.is_empty() {
                    primitives = base_ptcl_file.primitives;
                }
                if bfres_models.is_empty() {
                    bfres_models = base_ptcl_file.bfres_models;
                }
                if shader_binary_1.is_empty() {
                    shader_binary_1 = base_ptcl_file.shader_binary_1;
                }
                if shader_binary_2.is_empty() {
                    shader_binary_2 = base_ptcl_file.shader_binary_2;
                }
            }
        }
    }

    Ok(PtclFile {
        emitter_sets,
        texture_section,
        texture_section_offset: 0,
        bntx_textures,
        primitives,
        bfres_models,
        shader_binary_1,
        shader_binary_2,
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

/// Minimal deserialization of EmitterData.json — only the fields needed to
/// populate our own `EmitterDef`.  Extra fields get silently ignored.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterDataJson {
    flag: u32,
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
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmitterStaticJson {
    num_color0_keys: u32,
    num_alpha0_keys: u32,
    num_color1_keys: u32,
    num_alpha1_keys: u32,
    num_scale_keys: u32,
    color0: Option<AnimKeyTableJson>,
    alpha0: Option<AnimKeyTableJson>,
    color1: Option<AnimKeyTableJson>,
    alpha1: Option<AnimKeyTableJson>,
    scale_anim: Option<AnimKeyTableJson>,
    tex_pattern_anim0: Option<TexPatAnimJson>,
    tex_pattern_anim1: Option<TexPatAnimJson>,
    tex_pattern_anim2: Option<TexPatAnimJson>,
    tex_scroll_anim0: Option<TexScrollAnimJson>,
    tex_scroll_anim1: Option<TexScrollAnimJson>,
    tex_scroll_anim2: Option<TexScrollAnimJson>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct AnimKeyTableJson {
    keys: Vec<AnimKeyJson>,
}

#[derive(serde::Deserialize, Default)]
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
    trans_x: f32,
    trans_y: f32,
    trans_z: f32,
    rotate_x: f32,
    rotate_y: f32,
    rotate_z: f32,
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
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EmissionJson {
    is_one_time: bool,
    start: u32,
    timing: u32,
    duration: u32,
    rate: f32,
    rate_random: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ShapeInfoJson {
    volume_type: u32,
    primitive_index: u64,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RenderStateJson {
    blend_type: u32,
    display_side: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleDataJson {
    life: u32,
    life_random: u32,
    #[serde(rename = "PrimitiveID")]
    primitive_id: u64,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleVelocityJson {
    all_direction: f32,
    vel_random: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct ParticleScaleJson {
    scale_x: f32,
    scale_random_x: f32,
    scale_y: f32,
    scale_z: f32,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct TexPatAnimJson {
    num: f32,
    frequency: f32,
    num_random: f32,
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
    let emission_timing = json.emission.as_ref().map(|e| e.timing).unwrap_or(0);
    let emission_duration = json.emission.as_ref().map(|e| e.duration).unwrap_or(9999);

    // ── Particle lifetime ─────────────────────────────────────────────────
    let lifetime = json
        .particle_data
        .as_ref()
        .map(|p| p.life as f32)
        .unwrap_or(60.0);
    let lifetime_random = json
        .particle_data
        .as_ref()
        .map(|p| p.life_random as f32)
        .unwrap_or(0.0);

    // ── Particle velocity ──────────────────────────────────────────────────
    let initial_speed = json
        .particle_velocity
        .as_ref()
        .map(|v| v.all_direction)
        .unwrap_or(0.3);
    let speed_random = json
        .particle_velocity
        .as_ref()
        .map(|v| v.vel_random)
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
        .map(|s| s.scale_random_x)
        .unwrap_or(0.0);

    // ── Rotation speed (use RotateAddX from EmitterStatic if available) ────
    // In our JSON schema we don't extract RotateAdd directly yet.
    let rotation_speed = 0.05;

    // ── Accel (from gravity direction + scale in EmitterStatic) ────────────
    // Use a default for now since gravity fields aren't in our minimal schema.
    let accel = glam::Vec3::new(0.0, 0.05, 0.0);

    // ── Color / alpha / scale animation keys ───────────────────────────────
    let (color0, _color0_keys) = extract_color_keys(json.emitter_static.as_ref().and_then(|s| s.color0.as_ref()));
    let (color1, _color1_keys) = extract_color_keys(json.emitter_static.as_ref().and_then(|s| s.color1.as_ref()));
    let (alpha0_anim, alpha0_keys) = extract_alpha_keys(json.emitter_static.as_ref().and_then(|s| s.alpha0.as_ref()));
    let (alpha1_anim, alpha1_keys) = extract_alpha_keys(json.emitter_static.as_ref().and_then(|s| s.alpha1.as_ref()));
    let scale_anim = extract_scale_anim(json.emitter_static.as_ref().and_then(|s| s.scale_anim.as_ref()));

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

    // ── Texture UV data ────────────────────────────────────────────────────
    let tex_scale_uv = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim0.as_ref())
        .map(|t| [t.scale_x, t.scale_y])
        .unwrap_or([1.0, 1.0]);
    let tex_offset_uv = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim0.as_ref())
        .map(|t| [t.scroll_x, t.scroll_y])
        .unwrap_or([0.0, 0.0]);

    // tex_pat_frame_count: from tex_pattern_anim0.num as frame count
    let tex_pat_frame_count = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_pattern_anim0.as_ref())
        .map(|t| t.num as usize)
        .unwrap_or(1);

    let tex_scroll_uv = json
        .emitter_static
        .as_ref()
        .and_then(|s| s.tex_scroll_anim0.as_ref())
        .map(|t| [t.scroll_add_x, t.scroll_add_y])
        .unwrap_or([0.0, 0.0]);

    // ── Textures ───────────────────────────────────────────────────────────
    let textures: Vec<TextureRes> = tex_list.to_vec();

    // ── mesh_type & primitive_index from shape_info ────────────────────────
    let mesh_type = json
        .shape_info
        .as_ref()
        .map(|s| {
            if s.volume_type == 15 { 1 } else { 0 } // Primitive = 15
        })
        .unwrap_or(0);
    let primitive_index = json
        .shape_info
        .as_ref()
        .map(|s| s.primitive_index as u32)
        .unwrap_or(0);

    // ── Indirect texture fields (from sampler1 / texture_anim1) ────────────
    let is_indirect_slot1 = tex_list
        .get(1)
        .map(|t| t.tex_name.to_lowercase().contains("indirect"))
        .unwrap_or(false);
    let distortion_strength = 0.0; // TODO: extract from data
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

    // ── texture_index ──────────────────────────────────────────────────────
    let texture_index = if !bntx_textures.is_empty() && !tex_list.is_empty() {
        // Find the first matching entry in bntx_textures
        bntx_textures
            .iter()
            .position(|t| t.tex_name == tex_list[0].tex_name)
            .unwrap_or(0) as u32
    } else {
        0
    };

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
        lifetime,
        lifetime_random,
        scale,
        scale_random,
        rotation_speed,
        color0,
        color1,
        alpha0: alpha0_anim,
        alpha1: alpha1_anim,
        alpha0_keys,
        alpha1_keys,
        scale_anim,
        textures,
        mesh_type,
        primitive_index,
        texture_index,
        tex_scale_uv,
        tex_offset_uv,
        tex_scroll_uv,
        tex_pat_frame_count,
        emitter_offset,
        emitter_rotation,
        emitter_scale,
        is_one_time,
        emission_timing,
        emission_duration,
        is_indirect_slot1,
        distortion_strength,
        indirect_scroll_uv,
        indirect_tex_scale_uv,
        indirect_tex_offset_uv,
    }
}

// ── Helper extraction functions ───────────────────────────────────────────────

fn extract_color_keys(table: Option<&AnimKeyTableJson>) -> (Vec<ColorKey>, Vec<ColorKey>) {
    let Some(table) = table else { return (vec![], vec![]) };
    let keys: Vec<ColorKey> = table
        .keys
        .iter()
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

fn extract_alpha_keys(table: Option<&AnimKeyTableJson>) -> (AnimKey3v4k, Vec<ColorKey>) {
    let Some(table) = table else {
        return (AnimKey3v4k::default(), vec![]);
    };
    let pairs: Vec<(f32, f32)> = table
        .keys
        .iter()
        .map(|k| (k.time, k.x))
        .collect();
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

fn extract_scale_anim(table: Option<&AnimKeyTableJson>) -> AnimKey3v4k {
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
    let k0 = &table.keys[0];
    let start_value = k0.x;
    let (start_diff, time2) = if table.keys.len() > 1 {
        (
            table.keys[1].x - start_value,
            table.keys[1].time,
        )
    } else {
        (0.0, 0.5)
    };
    let (end_diff, time3) = if table.keys.len() > 2 {
        let last = table.keys.last().unwrap();
        let prev = &table.keys[table.keys.len() - 2];
        (last.x - prev.x, last.time)
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

fn raw_scroll(anim: Option<&TexScrollAnimJson>) -> [f32; 2] {
    anim.map(|t| [t.scroll_add_x, t.scroll_add_y])
        .unwrap_or([0.0, 0.0])
}

fn raw_uv_scale(anim: Option<&TexPatAnimJson>) -> [f32; 2] {
    anim.map(|t| [t.scale_x, t.scale_y])
        .unwrap_or([1.0, 1.0])
}

fn raw_uv_offset(anim: Option<&TexPatAnimJson>) -> [f32; 2] {
    anim.map(|t| [t.scroll_x, t.scroll_y])
        .unwrap_or([0.0, 0.0])
}
