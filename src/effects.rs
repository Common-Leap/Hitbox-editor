//! Lean `.eff` model used by Visionary's authored-effect editor.
//!
//! Particle simulation and rendering live in the game through slight_replica. This module only
//! exposes the fields the desktop editor needs and converts them from EffectLibraryRust.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Default, Clone)]
pub struct EffIndex {
    /// Effect entry name to zero-based emitter-set index. Original and lowercase keys are kept.
    pub handles: HashMap<String, i32>,
}

impl EffIndex {
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(load_effect(path)?.index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorKey {
    pub frame: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Debug, Clone)]
pub struct EmitterDef {
    pub name: String,
    pub emission_rate: f32,
    pub lifetime: f32,
    pub scale: f32,
    pub color_scale: f32,
    pub emitter_scale: glam::Vec3,
    pub color0: Vec<ColorKey>,
    pub color1: Vec<ColorKey>,
    /// Alpha values use `r`, matching `AuthoredEdit`'s compact `[value, frame]` form.
    pub alpha0_keys: Vec<ColorKey>,
    pub texture_index: u32,
}

#[derive(Debug, Clone)]
pub struct EmitterSet {
    pub name: String,
    /// Parent-first flattened emitter tree. This order matches the export edit path.
    pub emitters: Vec<EmitterDef>,
}

#[derive(Debug, Clone)]
pub struct TextureInfo {
    pub tex_name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PtclFile {
    pub emitter_sets: Vec<EmitterSet>,
    pub bntx_textures: Vec<TextureInfo>,
}

pub struct LoadedEffect {
    pub index: EffIndex,
    pub ptcl: PtclFile,
}

pub fn load_effect(path: &Path) -> Result<LoadedEffect> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let file = effect_library::NamcoEffectFile::load(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let mut handles = HashMap::new();
    for (name, entry) in file.entry_names.iter().zip(&file.entries) {
        let set_idx = entry
            .emitter_set_id
            .checked_sub(1)
            .map(|index| index as i32)
            .unwrap_or(-1);
        handles.insert(name.to_lowercase(), set_idx);
        handles.insert(name.clone(), set_idx);
    }

    let ptcl = file
        .ptcl_file
        .as_ref()
        .map(convert_ptcl)
        .unwrap_or_default();
    Ok(LoadedEffect {
        index: EffIndex { handles },
        ptcl,
    })
}

fn convert_ptcl(source: &effect_library::PtclFile) -> PtclFile {
    let descriptors = source
        .texture_info
        .as_ref()
        .map(|info| info.descriptors.as_slice())
        .unwrap_or_default();
    let bntx_textures = descriptors
        .iter()
        .map(|texture| TextureInfo {
            tex_name: texture.name.clone(),
            // EffectLibrary intentionally keeps texture payloads opaque here; dimensions are
            // irrelevant to authored edits and are no longer decoded by the desktop app.
            width: 0,
            height: 0,
        })
        .collect();

    let emitter_sets = source
        .emitter_list
        .emitter_sets
        .iter()
        .map(|set| {
            let mut emitters = Vec::new();
            flatten_emitters(&set.emitters, descriptors, &mut emitters);
            EmitterSet {
                name: set.name.clone(),
                emitters,
            }
        })
        .collect();

    PtclFile {
        emitter_sets,
        bntx_textures,
    }
}

fn flatten_emitters(
    source: &[effect_library::Emitter],
    textures: &[effect_library::ptcl_file::TextureDescriptor],
    output: &mut Vec<EmitterDef>,
) {
    for emitter in source {
        output.push(convert_emitter(&emitter.data, textures));
        flatten_emitters(&emitter.children, textures, output);
    }
}

fn convert_emitter(
    data: &effect_library::EmitterData,
    textures: &[effect_library::ptcl_file::TextureDescriptor],
) -> EmitterDef {
    let texture_index = data
        .sampler0
        .as_ref()
        .and_then(|sampler| {
            textures
                .iter()
                .position(|texture| texture.id == sampler.texture_id)
        })
        .unwrap_or(0) as u32;

    EmitterDef {
        name: data.display_name(),
        emission_rate: data.emission.rate,
        lifetime: data.particle_data.life as f32,
        scale: data.particle_scale.scale_x,
        color_scale: data.emitter_static.color_scale,
        emitter_scale: glam::Vec3::new(
            data.emitter_info.scale_x,
            data.emitter_info.scale_y,
            data.emitter_info.scale_z,
        ),
        color0: color_keys(data, 0),
        color1: color_keys(data, 1),
        alpha0_keys: alpha_keys(data),
        texture_index,
    }
}

fn key(frame: f32, r: f32, g: f32, b: f32, a: f32) -> ColorKey {
    ColorKey { frame, r, g, b, a }
}

fn color_keys(data: &effect_library::EmitterData, channel: usize) -> Vec<ColorKey> {
    let particle = &data.particle_color;
    let stat = &data.emitter_static;
    let (kind, count, table, constant, fallback) = if channel == 0 {
        (
            particle.color0_type,
            stat.num_color0_keys,
            &stat.color0.keys,
            [particle.color0_r, particle.color0_g, particle.color0_b],
            [
                data.emitter_info.color0_r,
                data.emitter_info.color0_g,
                data.emitter_info.color0_b,
            ],
        )
    } else {
        (
            particle.color1_type,
            stat.num_color1_keys,
            &stat.color1.keys,
            [particle.color1_r, particle.color1_g, particle.color1_b],
            [
                data.emitter_info.color1_r,
                data.emitter_info.color1_g,
                data.emitter_info.color1_b,
            ],
        )
    };

    if matches!(kind, effect_library::ColorType::Constant) {
        return vec![key(0.0, constant[0], constant[1], constant[2], 1.0)];
    }
    if count > 0 {
        return table
            .iter()
            .take((count as usize).min(table.len()))
            .map(|value| key(value.time, value.x, value.y, value.z, 1.0))
            .collect();
    }
    vec![key(0.0, fallback[0], fallback[1], fallback[2], 1.0)]
}

fn alpha_keys(data: &effect_library::EmitterData) -> Vec<ColorKey> {
    let count = data.emitter_static.num_alpha0_keys as usize;
    if count > 0 {
        return data
            .emitter_static
            .alpha0
            .keys
            .iter()
            .take(count.min(8))
            .map(|value| key(value.time, value.x, value.x, value.x, value.x))
            .collect();
    }
    let alpha = if matches!(
        data.particle_color.alpha0_type,
        effect_library::ColorType::Constant
    ) {
        data.particle_color.alpha0
    } else {
        data.emitter_info.color0_a
    };
    vec![key(0.0, alpha, alpha, alpha, alpha)]
}
