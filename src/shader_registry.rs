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

/// Classify VS profile from shader stage input names when available.
/// (eff-editor branch: takes the raw name list — bnsh_reflection lives on the render branch.)
pub fn vs_profile_from_input_names(input_names: &[String]) -> ShaderVsProfile {
    if input_names.is_empty() {
        return ShaderVsProfile::Unknown;
    }
    let lower: Vec<String> = input_names
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

impl CombinerState {
    /// True when indirect UV distortion strength scales with camera distance.
    pub fn distortion_by_camera_distance(&self) -> bool {
        self.is_distortion_by_camera_distance != 0
    }
}

/// Camera-distance scale references from `ParticleScale` (ScaleMin/Max + enable flags).
#[derive(Debug, Clone, Default)]
pub struct ParticleScaleState {
    pub enable_scaling_by_camera_dist_near: u32,
    pub enable_scaling_by_camera_dist_far: u32,
    pub scale_min: f32,
    pub scale_max: f32,
}

impl ParticleScaleState {
    pub fn from_fields(
        enable_near: u32,
        enable_far: u32,
        scale_min: f32,
        scale_max: f32,
    ) -> Self {
        Self {
            enable_scaling_by_camera_dist_near: enable_near,
            enable_scaling_by_camera_dist_far: enable_far,
            scale_min,
            scale_max,
        }
    }
}

/// Per-draw `@group(1)` binding-6 uniform (`FxIndirectParams` in WGSL).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct IndirectParams {
    pub is_indirect: u32,
    pub distortion_strength: f32,
    pub indirect_scroll_u: f32,
    pub indirect_scroll_v: f32,
    pub indirect_scale_u: f32,
    pub indirect_scale_v: f32,
    pub indirect_offset_u: f32,
    pub indirect_offset_v: f32,
    pub distortion_by_cam_dist: u32,
    pub enable_cam_dist_near: u32,
    pub enable_cam_dist_far: u32,
    pub _pad0: u32,
    pub cam_dist_near: f32,
    pub cam_dist_far: f32,
    pub _pad1: f32,
    pub _pad2: f32,
    pub cam_pos: [f32; 3],
    pub _pad3: f32,
}

pub const INDIRECT_PARAMS_UNIFORM_SIZE: usize = std::mem::size_of::<IndirectParams>();

/// Pack per-draw indirect distortion + camera-distance scale params for native FS.
pub fn indirect_params_from_emitter(
    emitter: &crate::effects::EmitterDef,
    cam_pos: glam::Vec3,
    indirect_scroll_u: f32,
    indirect_scroll_v: f32,
) -> IndirectParams {
    IndirectParams {
        is_indirect: u32::from(emitter.is_indirect_slot1),
        distortion_strength: emitter.distortion_strength,
        indirect_scroll_u,
        indirect_scroll_v,
        indirect_scale_u: emitter.indirect_tex_scale_uv[0],
        indirect_scale_v: emitter.indirect_tex_scale_uv[1],
        indirect_offset_u: indirect_scroll_u,
        indirect_offset_v: indirect_scroll_v,
        distortion_by_cam_dist: u32::from(emitter.combiner.distortion_by_camera_distance()),
        enable_cam_dist_near: emitter.particle_scale.enable_scaling_by_camera_dist_near,
        enable_cam_dist_far: emitter.particle_scale.enable_scaling_by_camera_dist_far,
        _pad0: 0,
        cam_dist_near: emitter.particle_scale.scale_min,
        cam_dist_far: emitter.particle_scale.scale_max.max(emitter.particle_scale.scale_min),
        _pad1: 0.0,
        _pad2: 0.0,
        cam_pos: cam_pos.to_array(),
        _pad3: 0.0,
    }
}

/// Particle color / soft-particle flags from EmitterData.json.
#[derive(Debug, Clone, Default)]
pub struct ParticleColorState {
    pub is_soft_particle: bool,
    /// Fade range scale from `EmitterStatic.SoftParticleVolume`.
    pub soft_particle_volume: f32,
    /// Soft depth edge from `EmitterStatic.SoftEdgeParam1/2`.
    pub soft_edge_param1: f32,
    pub soft_edge_param2: f32,
    /// Distance scale from `EmitterStatic.SoftPartcileDist` (JSON typo preserved).
    pub soft_particle_dist: f32,
    pub is_fresnel_alpha: bool,
    /// Rim exponent from `EmitterStatic.FresnelAlphaParam1` (NW `FresnelAlphaParam1`).
    pub fresnel_alpha_param1: f32,
    /// Rim intensity scale from `EmitterStatic.FresnelAlphaParam2` (NW `FresnelAlphaParam2`).
    pub fresnel_alpha_param2: f32,
    pub is_near_dist_alpha: bool,
    /// Near fade start distance from `EmitterStatic.NearDistAlphaParam1`.
    pub near_dist_alpha_param1: f32,
    /// Near fade range from `EmitterStatic.NearDistAlphaParam2`.
    pub near_dist_alpha_param2: f32,
    pub is_far_dist_alpha: bool,
    /// Far fade start distance from `EmitterStatic.FarDistAlphaParam1`.
    pub far_dist_alpha_param1: f32,
    /// Far fade range from `EmitterStatic.FarDistAlphaParam2`.
    pub far_dist_alpha_param2: f32,
    pub is_decal: bool,
}

impl ParticleColorState {
    /// True when fresnel or camera-distance alpha modifiers must be uploaded / applied.
    pub fn alpha_modifiers_needed(&self) -> bool {
        self.is_fresnel_alpha || self.is_near_dist_alpha || self.is_far_dist_alpha
    }
}

/// `@group(2)` binding 7 uniform size (std140, 64 bytes).
pub const PARTICLE_ALPHA_MOD_UNIFORM_SIZE: u64 = 64;

/// Pack per-draw fresnel / distance alpha params for native FS `_fx_particle_alpha`.
pub fn particle_alpha_mods_uniform(pc: &ParticleColorState, cam_pos: glam::Vec3) -> [u8; 64] {
    let flags = (pc.is_fresnel_alpha as u32)
        | ((pc.is_near_dist_alpha as u32) << 1)
        | ((pc.is_far_dist_alpha as u32) << 2);
    let mut out = [0u8; 64];
    out[0..4].copy_from_slice(&flags.to_le_bytes());
    out[16..20].copy_from_slice(&pc.fresnel_alpha_param1.to_le_bytes());
    out[20..24].copy_from_slice(&pc.fresnel_alpha_param2.to_le_bytes());
    out[24..28].copy_from_slice(&pc.near_dist_alpha_param1.to_le_bytes());
    out[28..32].copy_from_slice(&pc.near_dist_alpha_param2.to_le_bytes());
    out[32..36].copy_from_slice(&pc.far_dist_alpha_param1.to_le_bytes());
    out[36..40].copy_from_slice(&pc.far_dist_alpha_param2.to_le_bytes());
    out[48..52].copy_from_slice(&cam_pos.x.to_le_bytes());
    out[52..56].copy_from_slice(&cam_pos.y.to_le_bytes());
    out[56..60].copy_from_slice(&cam_pos.z.to_le_bytes());
    out
}

/// Which colour feeds the native FS texture-modulate inject (`enhance_native_fragment_wgsl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeColorInput {
    /// Infer from FS WGSL + registry hints at shader compile time.
    #[default]
    Auto,
    /// Trust `main_1()` / NVN FS chain output (first `FragmentOutput` argument).
    FsChain,
    /// Force CPU-simulated per-particle colour (`in_attr1_1`).
    VertexAttr,
}

impl NativeColorInput {
    /// Merge emitter-side hints: CPU vertex colour wins over native chain.
    pub fn merge(self, other: Self) -> Self {
        if self == NativeColorInput::VertexAttr || other == NativeColorInput::VertexAttr {
            NativeColorInput::VertexAttr
        } else if self == NativeColorInput::FsChain || other == NativeColorInput::FsChain {
            NativeColorInput::FsChain
        } else {
            NativeColorInput::Auto
        }
    }
}

/// Emitter JSON combiner / particle flags → native colour input hint (no FS WGSL).
pub fn infer_native_color_from_emitter(
    combiner: &CombinerState,
    particle_color: &ParticleColorState,
) -> NativeColorInput {
    if combiner.color_combiner_process >= 2 {
        return NativeColorInput::VertexAttr;
    }
    if combiner.primitive_color_input_type != 0 {
        return NativeColorInput::VertexAttr;
    }
    if particle_color.is_decal {
        return NativeColorInput::FsChain;
    }
    NativeColorInput::Auto
}

/// All unique BNSH binaries extracted from an effect dump.
#[derive(Debug, Clone, Default)]
pub struct ShaderRegistry {
    binaries: HashMap<ShaderKey, Vec<u8>>,
    vs_profiles: HashMap<ShaderKey, ShaderVsProfile>,
    native_color_inputs: HashMap<ShaderKey, NativeColorInput>,
    /// First registered key — used when an emitter has no embedded shader.
    first_key: ShaderKey,
    /// Emitters whose `shader_index` != -1 (for future library lookup).
    library_indices_seen: HashMap<i32, ShaderKey>,
    /// Legacy VS/FS pair keys frozen before the first merge. The legacy pair supplies the
    /// shared vertex stage for every FS-only registry entry; letting a merged library
    /// (ef_common) re-win the sorted-key pick swapped the effect's own particle VS for an
    /// unrelated common one and collapsed all live-viewport billboards.
    legacy_pair_keys: Option<(ShaderKey, ShaderKey)>,
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

    /// Record emitter combiner / particle-colour hints for a shader key.
    pub fn note_emitter_native_color(
        &mut self,
        key: ShaderKey,
        combiner: &CombinerState,
        particle_color: &ParticleColorState,
    ) {
        if key == 0 {
            return;
        }
        let inferred = infer_native_color_from_emitter(combiner, particle_color);
        if inferred == NativeColorInput::Auto {
            return;
        }
        self.native_color_inputs
            .entry(key)
            .and_modify(|existing| *existing = existing.merge(inferred))
            .or_insert(inferred);
    }

    pub fn native_color_input(&self, key: ShaderKey) -> NativeColorInput {
        self.native_color_inputs
            .get(&key)
            .copied()
            .unwrap_or(NativeColorInput::Auto)
    }

    pub fn native_color_inputs(&self) -> &HashMap<ShaderKey, NativeColorInput> {
        &self.native_color_inputs
    }

    pub fn vs_profiles(&self) -> &HashMap<ShaderKey, ShaderVsProfile> {
        &self.vs_profiles
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
        // Sorted by key so iteration order is deterministic across process launches. HashMap
        // iteration order is per-process randomized; leaking it here made shader selection —
        // and therefore the whole decode→WGSL→render pipeline — non-reproducible.
        let mut keys: Vec<ShaderKey> = self.binaries.keys().copied().collect();
        keys.sort_unstable();
        keys.into_iter().map(move |k| (k, self.binaries[&k].as_slice()))
    }

    /// Legacy compat: first two unique binaries (old shader_binary_1/2 slots).
    pub fn legacy_pair(&self) -> (Vec<u8>, Vec<u8>) {
        let (k1, k2) = self
            .legacy_pair_keys
            .unwrap_or_else(|| self.sorted_leading_keys());
        let get = |k: ShaderKey| self.binaries.get(&k).cloned().unwrap_or_default();
        (get(k1), get(k2))
    }

    /// First two keys in sorted order (stable across process launches — HashMap key
    /// order is per-process random; otherwise the VS/FS pair varies every run).
    fn sorted_leading_keys(&self) -> (ShaderKey, ShaderKey) {
        let mut keys: Vec<ShaderKey> = self.binaries.keys().copied().collect();
        keys.sort_unstable();
        (
            keys.first().copied().unwrap_or(0),
            keys.get(1).copied().unwrap_or(0),
        )
    }

    /// Merge another registry (e.g. from ef_common.eff) into this one; identical BNSH deduplicates by hash.
    pub fn merge_from(&mut self, other: &ShaderRegistry) {
        // Freeze the base file's legacy pair so merged libraries can't change which VS
        // pairs this effect's FS-only entries.
        if self.legacy_pair_keys.is_none() && !self.binaries.is_empty() {
            self.legacy_pair_keys = Some(self.sorted_leading_keys());
        }
        if self.first_key == 0 {
            self.first_key = other.first_key;
        }
        for (&key, bytes) in &other.binaries {
            self.binaries.entry(key).or_insert_with(|| bytes.clone());
        }
        for (&key, &profile) in &other.vs_profiles {
            if profile != ShaderVsProfile::Unknown {
                self.vs_profiles
                    .entry(key)
                    .and_modify(|p| *p = p.merge(profile))
                    .or_insert(profile);
            }
        }
        for (&key, &input) in &other.native_color_inputs {
            self.native_color_inputs
                .entry(key)
                .and_modify(|existing| *existing = existing.merge(input))
                .or_insert(input);
        }
        for (&idx, &key) in &other.library_indices_seen {
            self.library_indices_seen.entry(idx).or_insert(key);
        }
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
    fn legacy_pair_is_frozen_across_merge() {
        // The legacy pair supplies the shared VS for every FS-only registry entry.
        // Merging ef_common must never change it, no matter how the merged keys sort —
        // regression guard for the live-viewport billboard collapse.
        let mut base = ShaderRegistry::default();
        let base_b1 = vec![1u8; 64];
        let base_b2 = vec![2u8; 64];
        base.register(base_b1.clone());
        base.register(base_b2.clone());
        let before = base.legacy_pair();

        // Merge many binaries to make it overwhelmingly likely some key sorts below the
        // base keys (the pre-fix pick was "two smallest hash keys of the merged set").
        let mut common = ShaderRegistry::default();
        for i in 0u16..64 {
            common.register(i.to_le_bytes().repeat(16));
        }
        base.merge_from(&common);

        assert_eq!(base.legacy_pair(), before, "merge changed the legacy VS/FS pair");
        // Late registrations (merged shader_binary_1/2) must not change it either.
        base.register(vec![3u8; 64]);
        assert_eq!(base.legacy_pair(), before);
    }

    #[test]
    fn native_color_input_merge_prefers_vertex_attr() {
        assert_eq!(
            NativeColorInput::FsChain.merge(NativeColorInput::VertexAttr),
            NativeColorInput::VertexAttr
        );
        assert_eq!(
            NativeColorInput::Auto.merge(NativeColorInput::FsChain),
            NativeColorInput::FsChain
        );
    }

    #[test]
    fn infer_native_color_from_emitter_process2_uses_vertex_attr() {
        let mut combiner = CombinerState::default();
        combiner.color_combiner_process = 2;
        assert_eq!(
            infer_native_color_from_emitter(&combiner, &ParticleColorState::default()),
            NativeColorInput::VertexAttr
        );
    }

    #[test]
    fn vs_profile_from_reflection_detects_mesh_inputs() {
        let mesh = vec!["Position".to_string(), "Normal".into(), "TexCoord0".into()];
        assert_eq!(
            vs_profile_from_input_names(&mesh),
            ShaderVsProfile::MeshModel
        );
        let particle = vec!["ATTR0".to_string(), "ATTR4".into(), "ATTR6".into()];
        assert_eq!(
            vs_profile_from_input_names(&particle),
            ShaderVsProfile::ParticleBillboard
        );
    }

    #[test]
    fn indirect_params_uniform_size_is_std140_aligned() {
        assert_eq!(INDIRECT_PARAMS_UNIFORM_SIZE, 80);
        assert_eq!(INDIRECT_PARAMS_UNIFORM_SIZE % 16, 0);
    }

    #[test]
    fn indirect_params_from_emitter_sets_camera_distortion_fields() {
        let mut emitter = crate::effects::EmitterDef::default();
        emitter.is_indirect_slot1 = true;
        emitter.distortion_strength = 0.25;
        emitter.combiner.is_distortion_by_camera_distance = 1;
        emitter.particle_scale = ParticleScaleState::from_fields(0, 0, 50.0, 50.0);
        let params = indirect_params_from_emitter(&emitter, glam::Vec3::new(1.0, 2.0, 3.0), 0.1, 0.2);
        assert_eq!(params.is_indirect, 1);
        assert_eq!(params.distortion_by_cam_dist, 1);
        assert!((params.cam_dist_near - 50.0).abs() < f32::EPSILON);
        assert!((params.cam_pos[0] - 1.0).abs() < f32::EPSILON);
        assert!((params.indirect_offset_u - 0.1).abs() < f32::EPSILON);
    }
}
