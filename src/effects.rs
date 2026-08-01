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
    /// Every attribute of this emitter, aligned to [`crate::eff_attrs::table`]. `None` where the
    /// emitter has no such block at all (no sampler 2, no combiner of that layout) — which the
    /// UI shows as an absent row rather than as a zero.
    pub attrs: Vec<Option<crate::eff_attrs::AttrValue>>,
    /// Opaque EMTR child sections. Emitter animations (`EA__`) and documented field/stripe
    /// sections are decoded by the editor, while their original bytes remain the source of
    /// truth so unknown data survives byte-for-byte.
    pub subsections: Vec<EmitterSubsectionDef>,
    /// Depth in the emitter tree: 0 for a root emitter, 1 for its child, and so on. The list is
    /// flattened parent-first, so this is what makes the nesting readable again — and what the
    /// roster editor rebuilds the tree from.
    pub depth: u8,
    /// Where this emitter sat in the set as LOADED. A roster duplicate carries the index of the
    /// emitter it was cloned from, which is how the exporter knows what to copy.
    pub source_idx: usize,
    pub color0: Vec<ColorKey>,
    pub color1: Vec<ColorKey>,
    /// Alpha values use `r`, matching `AuthoredEdit`'s compact `[value, frame]` form.
    pub alpha0_keys: Vec<ColorKey>,
    /// Which pool texture this emitter's `sampler0` reads, or None when it samples nothing
    /// (or samples a GUID the pool has no descriptor for). None is NOT the same as 0: this
    /// used to fall back to 0, which showed every textureless emitter as sampling the pool's
    /// first texture and offered a swap that had nothing to swap.
    pub texture_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmitterSubsectionDef {
    pub magic: String,
    pub data: Vec<u8>,
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
    /// Zero when the pool's BNTX could not be read for this entry.
    pub width: u32,
    pub height: u32,
    /// Surface format as `bntx` names it, e.g. `BC7Srgb`; empty when unreadable.
    pub format: String,
    /// Whether this texture can be exported to a PNG and replaced by one.
    pub convertible: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PtclFile {
    pub emitter_sets: Vec<EmitterSet>,
    pub bntx_textures: Vec<TextureInfo>,
}

/// One effect ENTRY (the kind name the game spawns) and what it brings on screen: an emitter
/// set, any extra parts on their own start frames, and an optional external model.
///
/// This is the spawn side of an effect, and it lives in the eff's header rather than in the PTCL
/// — which is why none of it was reachable from [`PtclFile`].
#[derive(Debug, Clone, Default)]
pub struct EffEntryInfo {
    pub name: String,
    /// The entry's own emitter set, or None when it has none of its own (multi-part entries
    /// normally hang their content off their variants instead).
    pub set_idx: Option<usize>,
    /// Extra parts, each with its own start frame. Empty for a single-part effect.
    pub variants: Vec<EffVariantInfo>,
    /// The model this effect spawns alongside its particles, when it has one.
    pub model: Option<EffModelInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffVariantInfo {
    /// Frames after the effect starts before this part comes in.
    pub start_frame: u16,
    pub set_idx: Option<usize>,
    /// The bone this part attaches to. Empty means the effect's own attachment.
    pub bone: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffModelInfo {
    pub name: String,
    /// The model's spawn condition byte, from the eff's model-flag table.
    pub flag: u8,
}

pub struct LoadedEffect {
    pub index: EffIndex,
    pub ptcl: PtclFile,
    /// Every entry in the file, in entry-table order.
    pub entries: Vec<EffEntryInfo>,
    /// The file's BNTX pool, verbatim. Kept so the editor can decode a texture to PNG for
    /// editing — `PtclFile` only carries what a texture IS, not its pixels.
    pub texture_pool: Option<Vec<u8>>,
}

pub fn load_effect(path: &Path) -> Result<LoadedEffect> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let file = effect_library::NamcoEffectFile::load(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let mut handles = HashMap::new();
    for (name, entry) in file.entry_names.iter().zip(&file.entries) {
        // `emitter_set_id` is a 1-based handle and 0 means "none" — which is what a MULTI-PART
        // entry normally carries, because its content hangs off its variants instead. Resolving
        // only this field mapped every such effect to -1, and an effect with no set is an effect
        // the editor cannot show: 64 of the corpus's 77 multi-part entries have a 0 here, and so
        // does every multi-part transplant (`ef_common`'s SYS_BOMB_* among them). They were
        // recorded in the file, listed in the entry table, and invisible.
        //
        // The editor's model is one set per name, so the first variant is the one to show: it is
        // the part that starts the effect. The later variants are additional parts on their own
        // start frames, and reaching those needs a UI that can express them at all.
        let set_idx = entry
            .emitter_set_id
            .checked_sub(1)
            .or_else(|| {
                (entry.variant_count > 0)
                    .then(|| {
                        let start = (entry.variant_start_idx as usize).checked_sub(1)?;
                        file.effect_variants
                            .get(start)
                            .and_then(|variant| (variant.emitter_set_id as u32).checked_sub(1))
                    })
                    .flatten()
            })
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
    let texture_pool = file
        .ptcl_file
        .as_ref()
        .and_then(|p| p.texture_info.as_ref())
        .and_then(|info| info.binary_data.clone());
    let entries = read_entries(&file);
    Ok(LoadedEffect {
        index: EffIndex { handles },
        ptcl,
        entries,
        texture_pool,
    })
}

/// The entry table, with its variants, bones and model resolved into names and indices.
///
/// Every id in the file is a 1-BASED handle where 0 means "none", so each is decoded through
/// `checked_sub(1)` rather than by subtracting blind — a 0 turned into `usize::MAX` here would
/// index a set that does not exist.
fn read_entries(file: &effect_library::NamcoEffectFile) -> Vec<EffEntryInfo> {
    file.entry_names
        .iter()
        .zip(&file.entries)
        .map(|(name, entry)| {
            let variant_start = (entry.variant_start_idx as usize).saturating_sub(1);
            let variants = (0..entry.variant_count as usize)
                .filter_map(|i| {
                    let variant = file.effect_variants.get(variant_start + i)?;
                    Some(EffVariantInfo {
                        start_frame: variant.start_frame,
                        set_idx: (variant.emitter_set_id as usize).checked_sub(1),
                        bone: file
                            .external_bone_names
                            .get(variant_start + i)
                            .cloned()
                            .unwrap_or_default(),
                    })
                })
                .collect();
            let model = (entry.external_model_idx as usize)
                .checked_sub(1)
                .and_then(|i| {
                    Some(EffModelInfo {
                        name: file.external_model_names.get(i)?.clone(),
                        flag: file.effect_models.get(i).copied().unwrap_or(0),
                    })
                });
            EffEntryInfo {
                name: name.clone(),

                set_idx: (entry.emitter_set_id as usize).checked_sub(1),
                variants,
                model,
            }
        })
        .collect()
}

fn convert_ptcl(source: &effect_library::PtclFile) -> PtclFile {
    let descriptors = source
        .texture_info
        .as_ref()
        .map(|info| info.descriptors.as_slice())
        .unwrap_or_default();
    // Dimensions and format come from the pool's own BRTI headers. `effect_library` keeps the
    // payload opaque, so this reads the container directly — cheap (fixed-offset reads, no
    // decode) and it is what lets the editor label a texture and say whether it can be
    // replaced by a PNG before the user picks one.
    let pool = source
        .texture_info
        .as_ref()
        .and_then(|info| info.binary_data.as_deref());
    let bntx_textures = descriptors
        .iter()
        .enumerate()
        .map(|(index, texture)| {
            let described = pool
                .and_then(|pool| crate::texture_import::describe(pool, index, &texture.name).ok());
            TextureInfo {
                tex_name: texture.name.clone(),
                width: described.as_ref().map(|d| d.width).unwrap_or(0),
                height: described.as_ref().map(|d| d.height).unwrap_or(0),
                format: described
                    .as_ref()
                    .map(|d| d.format.clone())
                    .unwrap_or_default(),
                convertible: described.map(|d| d.convertible).unwrap_or(false),
            }
        })
        .collect();

    let emitter_sets = source
        .emitter_list
        .emitter_sets
        .iter()
        .map(|set| {
            let mut emitters = Vec::new();
            flatten_emitters(&set.emitters, descriptors, 0, &mut emitters);
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
    depth: u8,
    output: &mut Vec<EmitterDef>,
) {
    for emitter in source {
        let index = output.len();
        output.push(convert_emitter(emitter, textures, depth, index));
        flatten_emitters(&emitter.children, textures, depth.saturating_add(1), output);
    }
}

fn convert_emitter(
    emitter: &effect_library::Emitter,
    textures: &[effect_library::ptcl_file::TextureDescriptor],
    depth: u8,
    index: usize,
) -> EmitterDef {
    let data = &emitter.data;
    let texture_index = data
        .sampler0
        .as_ref()
        .and_then(|sampler| {
            textures
                .iter()
                .position(|texture| texture.id == sampler.texture_id)
        })
        .map(|index| index as u32);

    EmitterDef {
        name: data.display_name(),
        attrs: crate::eff_attrs::read_all(data),
        subsections: emitter
            .subsections
            .iter()
            .map(|section| EmitterSubsectionDef {
                magic: section.magic.clone(),
                data: section.data.clone(),
            })
            .collect(),
        depth,
        source_idx: index,
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

#[cfg(test)]
mod tests {
    use super::load_effect;

    fn corpus_root() -> Option<std::path::PathBuf> {
        std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
    }

    /// A multi-part effect has to resolve to a real emitter set, not to -1.
    ///
    /// `emitter_set_id` is 0 for most multi-part entries — their content hangs off their variants
    /// — so reading only that field mapped them to "no set", which is indistinguishable from an
    /// empty effect and drops them out of the editor. This is what made a transplanted
    /// `SYS_BOMB_B` invisible: it was in the entry table, with all three of its sets populated,
    /// and the editor could not resolve it to anything.
    ///
    /// Checked over the whole corpus rather than one file, because the same 0 appears in vanilla
    /// data: the point is that NO entry backed by real emitters resolves to -1.
    #[test]
    fn multi_part_effects_resolve_to_an_emitter_set() {
        let Some(root) = corpus_root() else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(read) = std::fs::read_dir(dir) else {
                return;
            };
            for e in read.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "eff")
                    && !path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with('_'))
                {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&root.join("effect"), &mut files);
        assert!(
            files.len() > 300,
            "only {} corpus files — wrong root?",
            files.len()
        );

        let mut multi_part = 0usize;
        let mut resolved = 0usize;
        let mut unresolved: Vec<String> = Vec::new();
        for path in &files {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Ok(file) = effect_library::NamcoEffectFile::load(&bytes) else {
                continue;
            };
            let Ok(loaded) = load_effect(path) else {
                continue;
            };
            for (name, entry) in file.entry_names.iter().zip(&file.entries) {
                if entry.variant_count == 0 {
                    continue;
                }
                multi_part += 1;
                // Only entries whose first variant actually has a set are expected to resolve.
                let start = (entry.variant_start_idx as usize).saturating_sub(1);
                let backed = file
                    .effect_variants
                    .get(start)
                    .is_some_and(|v| v.emitter_set_id != 0)
                    || entry.emitter_set_id != 0;
                if !backed {
                    continue;
                }
                match loaded.index.handles.get(&name.to_lowercase()) {
                    Some(&idx) if idx >= 0 => resolved += 1,
                    _ => unresolved.push(format!("{}: {name}", path.display())),
                }
            }
        }
        eprintln!(
            "{multi_part} multi-part entries across {} files, {resolved} resolve to a set",
            files.len()
        );
        assert!(
            multi_part > 50,
            "only {multi_part} multi-part entries found — this test is not exercising the case"
        );
        assert!(
            unresolved.is_empty(),
            "{} multi-part effects resolve to no emitter set, so the editor cannot show them:\n{}",
            unresolved.len(),
            unresolved.join("\n")
        );
    }
}
