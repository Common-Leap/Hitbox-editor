//! Shader asset registry: every embedded `Shader.bnsh` from an effect dump,
//! keyed by content hash. Emitters reference shaders by hash instead of sharing
//! a global binary_1/binary_2 pair.

use std::collections::HashMap;
use sha2::{Digest, Sha256};

/// Content hash of a BNSH binary (first 8 bytes of SHA-256).
pub type ShaderKey = u64;

pub fn hash_bnsh_key(bnsh: &[u8]) -> ShaderKey {
    let digest = Sha256::digest(bnsh);
    u64::from_le_bytes(digest[0..8].try_into().unwrap())
}

/// Decoded vertex-shader role used to avoid mesh/model false positives in hybrid finalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShaderVsProfile {
    #[default]
    Unknown,
    ParticleBillboard,
    MeshModel,
}

impl ShaderVsProfile {
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (ShaderVsProfile::MeshModel, _) | (_, ShaderVsProfile::MeshModel) => {
                ShaderVsProfile::MeshModel
            }
            (ShaderVsProfile::ParticleBillboard, ShaderVsProfile::ParticleBillboard) => {
                ShaderVsProfile::ParticleBillboard
            }
            (ShaderVsProfile::ParticleBillboard, ShaderVsProfile::Unknown)
            | (ShaderVsProfile::Unknown, ShaderVsProfile::ParticleBillboard) => {
                ShaderVsProfile::ParticleBillboard
            }
            _ => ShaderVsProfile::Unknown,
        }
    }
}

/// Classify VS profile from BNSH stage reflection input names when available.
pub fn vs_profile_from_reflection(
    reflection: &crate::bnsh_reflection::ShaderStageReflection,
) -> ShaderVsProfile {
    if reflection.input_names.is_empty() {
        return ShaderVsProfile::Unknown;
    }
    let lower: Vec<String> = reflection
        .input_names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let particle_markers = lower.iter().any(|n| {
        n.contains("attr4")
            || n.contains("_a4")
            || n.contains("attr6")
            || n.contains("_a6")
            || n.contains("life")
    });
    let mesh_markers = lower.iter().any(|n| {
        n.contains("normal") || n.contains("tangent") || n.contains("binormal")
    });
    if mesh_markers && !particle_markers {
        ShaderVsProfile::MeshModel
    } else if particle_markers {
        ShaderVsProfile::ParticleBillboard
    } else {
        ShaderVsProfile::Unknown
    }
}

/// Combiner / blend configuration from EmitterData.json (drives render state).
#[derive(Debug, Clone, Default)]
pub struct CombinerState {
    pub color_combiner_process: u32,
    pub alpha_combiner_process: u32,
    pub texture1_color_blend: u32,
    pub texture2_color_blend: u32,
    pub primitive_color_blend: u32,
    pub texture1_alpha_blend: u32,
    pub texture2_alpha_blend: u32,
    pub primitive_alpha_blend: u32,
    pub tex_color0_input_type: u32,
    pub tex_color1_input_type: u32,
    pub tex_color2_input_type: u32,
    pub tex_alpha0_input_type: u32,
    pub tex_alpha1_input_type: u32,
    pub tex_alpha2_input_type: u32,
    pub primitive_color_input_type: u32,
    pub primitive_alpha_input_type: u32,
    pub shader_type: u32,
    pub apply_alpha: u32,
    pub is_distortion_by_camera_distance: u32,
    /// v50+ EmitterCombinerV40 padding: dedicated tex3 colour/alpha blend (0=modulate).
    pub texture3_color_blend: u32,
    pub texture3_alpha_blend: u32,
    pub texture4_color_blend: u32,
    pub texture4_alpha_blend: u32,
    pub texture5_color_blend: u32,
    pub texture5_alpha_blend: u32,
    /// True when EmitterData.json carried v50 padding blend fields.
    pub has_v50_extra_tex_blend: bool,
}

/// Particle color / soft-particle flags from EmitterData.json.
#[derive(Debug, Clone, Default)]
pub struct ParticleColorState {
    pub is_soft_particle: bool,
    pub is_fresnel_alpha: bool,
    pub is_near_dist_alpha: bool,
    pub is_far_dist_alpha: bool,
    pub is_decal: bool,
}

/// All unique BNSH binaries extracted from an effect dump.
#[derive(Debug, Clone, Default)]
pub struct ShaderRegistry {
    binaries: HashMap<ShaderKey, Vec<u8>>,
    vs_profiles: HashMap<ShaderKey, ShaderVsProfile>,
    /// First registered key — used when an emitter has no embedded shader.
    first_key: ShaderKey,
    /// Emitters whose `shader_index` != -1 (for future library lookup).
    library_indices_seen: HashMap<i32, ShaderKey>,
}

impl ShaderRegistry {
    /// Register BNSH bytes; returns content hash (deduplicates identical shaders).
    pub fn register(&mut self, bnsh: Vec<u8>) -> ShaderKey {
        if bnsh.is_empty() {
            return 0;
        }
        let key = hash_bnsh_key(&bnsh);
        if self.first_key == 0 {
            self.first_key = key;
        }
        self.binaries.entry(key).or_insert(bnsh);
        key
    }

    pub fn set_vs_profile(&mut self, key: ShaderKey, profile: ShaderVsProfile) {
        if key != 0 && profile != ShaderVsProfile::Unknown {
            self.vs_profiles.insert(key, profile);
        }
    }

    pub fn vs_profile(&self, key: ShaderKey) -> ShaderVsProfile {
        self.vs_profiles.get(&key).copied().unwrap_or_default()
    }

    pub fn register_library_index(&mut self, library_index: i32, key: ShaderKey) {
        if library_index >= 0 && key != 0 {
            self.library_indices_seen
                .entry(library_index)
                .or_insert(key);
        }
    }

    pub fn library_indices(&self) -> &HashMap<i32, ShaderKey> {
        &self.library_indices_seen
    }

    pub fn get(&self, key: ShaderKey) -> Option<&[u8]> {
        if key == 0 {
            return None;
        }
        self.binaries.get(&key).map(|v| v.as_slice())
    }

    pub fn resolve(&self, key: ShaderKey, library_index: i32) -> ShaderKey {
        if key != 0 {
            return key;
        }
        if library_index >= 0 {
            if let Some(&k) = self.library_indices_seen.get(&library_index) {
                return k;
            }
        }
        self.first_key
    }

    pub fn default_key(&self) -> ShaderKey {
        self.first_key
    }

    pub fn len(&self) -> usize {
        self.binaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
    }

    pub fn library_index_count(&self) -> usize {
        self.library_indices_seen.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ShaderKey, &[u8])> + '_ {
        self.binaries
            .iter()
            .map(|(&k, v)| (k, v.as_slice()))
    }

    /// Legacy compat: first two unique binaries (old shader_binary_1/2 slots).
    pub fn legacy_pair(&self) -> (Vec<u8>, Vec<u8>) {
        let keys: Vec<ShaderKey> = self.binaries.keys().copied().collect();
        let b1 = keys
            .first()
            .and_then(|k| self.binaries.get(k))
            .cloned()
            .unwrap_or_default();
        let b2 = keys
            .get(1)
            .and_then(|k| self.binaries.get(k))
            .cloned()
            .unwrap_or_default();
        (b1, b2)
    }
}

/// Phase-0 audit summary printed when `FX_DEBUG` is set.
#[derive(Debug, Default)]
pub struct ShaderAuditReport {
    pub unique_shaders: usize,
    pub emitters_with_shader: usize,
    pub emitters_total: usize,
    pub emitters_missing_shader: usize,
    pub distinct_shader_keys: usize,
    pub library_indices: usize,
}

impl ShaderAuditReport {
    pub fn log(&self, label: &str) {
        eprintln!(
            "[SHADER-AUDIT] {label}: {} unique BNSH, {}/{} emitters have shader, {} missing, {} library indices",
            self.unique_shaders,
            self.emitters_with_shader,
            self.emitters_total,
            self.emitters_missing_shader,
            self.library_indices,
        );
    }
}

pub fn audit_ptcl(ptcl: &crate::effects::PtclFile) -> ShaderAuditReport {
    let mut report = ShaderAuditReport {
        unique_shaders: ptcl.shader_registry.len(),
        library_indices: ptcl.shader_registry.library_index_count(),
        ..Default::default()
    };
    let mut keys = std::collections::HashSet::new();
    for set in &ptcl.emitter_sets {
        for em in &set.emitters {
            report.emitters_total += 1;
            if em.shader_key != 0 {
                report.emitters_with_shader += 1;
                keys.insert(em.shader_key);
            } else {
                report.emitters_missing_shader += 1;
            }
        }
    }
    report.distinct_shader_keys = keys.len();
    if crate::fx_debug_enabled() {
        report.log(&ptcl.emitter_sets.first().map(|s| s.name.as_str()).unwrap_or("?"));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vs_profile_from_reflection_detects_mesh_inputs() {
        let mesh = crate::bnsh_reflection::ShaderStageReflection {
            input_names: vec!["Position".into(), "Normal".into(), "TexCoord0".into()],
            ..Default::default()
        };
        assert_eq!(
            vs_profile_from_reflection(&mesh),
            ShaderVsProfile::MeshModel
        );
        let particle = crate::bnsh_reflection::ShaderStageReflection {
            input_names: vec!["ATTR0".into(), "ATTR4".into(), "ATTR6".into()],
            ..Default::default()
        };
        assert_eq!(
            vs_profile_from_reflection(&particle),
            ShaderVsProfile::ParticleBillboard
        );
    }
}
