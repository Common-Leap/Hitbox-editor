// Portions of this file are ported from Switch Toolbox
// (KillzXGaming/Switch-Toolbox, MIT License)
// https://github.com/KillzXGaming/Switch-Toolbox
//
// MIT License
// Copyright (c) 2018 KillzXGaming
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.

/// Effect system: .eff index parsing, .ptcl particle data parsing,
/// CPU particle simulation, and GPU billboard rendering.

use std::collections::HashMap;
use std::path::Path;
use glam::{Mat4, Vec3, Vec4};

// ── EFF index ─────────────────────────────────────────────────────────────────

/// Maps effect handle names (e.g. "sys_smash_flash") to emitter set indices
/// inside the embedded .ptcl resource.
#[derive(Debug, Default, Clone)]
pub struct EffIndex {
    /// effect_handle_name -> emitter_set_handle (index into ptcl emitter sets)
    pub handles: HashMap<String, i32>,
    /// The raw .ptcl bytes embedded in the .eff file
    pub ptcl_data: Vec<u8>,
}

impl EffIndex {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let eff = eff_lib::EffFile::from_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to parse .eff: {e}"))?;

        let mut handles = HashMap::new();
        for (handle, name) in eff.effect_handles.iter().zip(eff.effect_handle_names.iter()) {
            let name_str = name.to_string()?;
            // emitter_set_handle is 1-based in the eff file; convert to 0-based index
            let set_idx = handle.emitter_set_handle - 1;
            // Store both original and lowercase versions for case-insensitive lookup
            handles.insert(name_str.to_lowercase(), set_idx);
            handles.insert(name_str, set_idx);
        }

        let ptcl_data = eff.resource_data.unwrap_or_default();
        Ok(Self { handles, ptcl_data })
    }

    /// Merge handles AND particle data from another eff file into this index.
    /// The emitter sets from the other file are appended to `ptcl`, and handles
    /// are registered with the correct (appended) set indices.
    /// Existing handles are not overwritten.
    pub fn merge_from_file_with_ptcl(&mut self, path: &Path, ptcl: &mut crate::effects::PtclFile) -> anyhow::Result<()> {
        let eff = eff_lib::EffFile::from_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to parse .eff: {e}"))?;

        let ptcl_data = eff.resource_data.unwrap_or_default();
        if ptcl_data.is_empty() {
            return Ok(());
        }

        // Parse the other file's VFXB
        let other_ptcl = crate::effects::PtclFile::parse(&ptcl_data)
            .unwrap_or_else(|_| {
                let max_idx = eff.effect_handles.iter()
                    .map(|h| (h.emitter_set_handle - 1).max(0))
                    .max().unwrap_or(0) as usize;
                crate::effects::PtclFile::synthetic(max_idx)
            });

        // The base index for the appended sets
        let base_idx = ptcl.emitter_sets.len() as i32;

        // Register handles pointing into the appended sets
        for (handle, name) in eff.effect_handles.iter().zip(eff.effect_handle_names.iter()) {
            if let Ok(name_str) = name.to_string() {
                // emitter_set_handle is 1-based; convert to 0-based then offset by base_idx
                let set_idx = base_idx + (handle.emitter_set_handle - 1);
                self.handles.entry(name_str.to_lowercase()).or_insert(set_idx);
                self.handles.entry(name_str).or_insert(set_idx);
            }
        }

        // Merge embedded BNSH shaders (ef_common carries the shared particle shader library).
        let merged_shaders = other_ptcl.shader_registry.len();
        ptcl.shader_registry.merge_from(&other_ptcl.shader_registry);
        if !other_ptcl.shader_binary_1.is_empty() {
            ptcl.shader_registry.register(other_ptcl.shader_binary_1.clone());
        }
        if !other_ptcl.shader_binary_2.is_empty() {
            ptcl.shader_registry.register(other_ptcl.shader_binary_2.clone());
        }
        let (b1, b2) = ptcl.shader_registry.legacy_pair();
        ptcl.shader_binary_1 = b1;
        ptcl.shader_binary_2 = b2;

        // Append the emitter sets, offsetting texture indices by the current bntx count
        let bntx_base_idx = ptcl.bntx_textures.len() as u32;
        let merged_count = other_ptcl.emitter_sets.len();
        // Offset texture indices in merged emitter sets to point into the combined bntx_textures
        let mut merged_sets = other_ptcl.emitter_sets;
        for set in &mut merged_sets {
            for emitter in &mut set.emitters {
                if emitter.texture_index != u32::MAX {
                    emitter.texture_index += bntx_base_idx;
                }
                for tex in &mut emitter.textures {
                    tex.ftx_data_offset += ptcl.texture_section.len() as u32;
                }
            }
        }
        ptcl.emitter_sets.extend(merged_sets);
        // Merge BNTX textures and texture section
        let tex_section_base = ptcl.texture_section.len() as u32;
        for mut tex in other_ptcl.bntx_textures {
            tex.ftx_data_offset += tex_section_base;
            tex.original_data_offset += tex_section_base;
            ptcl.bntx_textures.push(tex);
        }
        ptcl.texture_section.extend_from_slice(&other_ptcl.texture_section);
        eprintln!(
            "[EFF] merged {} emitter sets from {:?}, total now {} sets, {} bntx textures, +{} shaders ({} total)",
            merged_count,
            path.file_name().unwrap_or_default(),
            ptcl.emitter_sets.len(),
            ptcl.bntx_textures.len(),
            merged_shaders,
            ptcl.shader_registry.len(),
        );
        Ok(())
    }

    /// Merge handles from another eff file (e.g. ef_sys.eff) into this index.
    /// Existing handles are not overwritten.
    #[allow(dead_code)]
    pub fn merge_from_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let eff = eff_lib::EffFile::from_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to parse .eff: {e}"))?;
        // Offset sys handles by a large number to avoid colliding with fighter set indices
        let offset = 10000i32;
        for (handle, name) in eff.effect_handles.iter().zip(eff.effect_handle_names.iter()) {
            let name_str = name.to_string()?;
            let idx = handle.emitter_set_handle + offset;
            self.handles.entry(name_str.to_lowercase()).or_insert(idx);
            self.handles.entry(name_str).or_insert(idx);
        }
        Ok(())
    }
}

// ── PTCL parser ───────────────────────────────────────────────────────────────

/// A parsed emitter set from the .ptcl file.
/// One emitter set = one "effect" that can be spawned by name.
#[derive(Debug, Clone)]
pub struct EmitterSet {
    pub name: String,
    pub emitters: Vec<EmitterDef>,
}

/// Per-slot TextureAnim flags from Emitter.cs (`PatternAnimType`, `IsScroll`, etc.).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TextureAnimFlags {
    pub pattern_anim_type: u8,
    pub is_scroll: bool,
    pub is_rotate: bool,
    pub is_scale: bool,
    pub inv_rand_u: bool,
    pub inv_rand_v: bool,
    pub pat_loop_random: bool,
    pub crossfade: bool,
    pub scroll_rotation: f32,
    pub scroll_rotation_add: f32,
}

/// PatternAnimType values — mirrors `EffectLibrary.Enums.TexturePatternType`
/// (`Emitter.cs` `TextureAnim.PatternAnimType`).
pub mod pattern_anim_type {
    /// No pattern playback mode (`TexturePatternType.None`).
    pub const NONE: u8 = 0;
    /// Advance once over particle lifetime (`TexturePatternType.FitLifespan`).
    pub const FIT_LIFESPAN: u8 = 1;
    /// Forward playback, hold last frame (`TexturePatternType.Clamp`).
    pub const CLAMP: u8 = 2;
    /// Loop pattern over lifetime (`TexturePatternType.Loop`).
    pub const LOOP: u8 = 3;
    /// One random frame chosen at particle birth (`TexturePatternType.Random`).
    pub const RANDOM: u8 = 4;
}

/// TexPatAnim / TexScrollAnim metadata for TextureAnim3–5 sampler slots.
#[derive(Debug, Clone, Default)]
pub struct TexExtraSlotDef {
    pub scale_uv: [f32; 2],
    pub offset_uv: [f32; 2],
    pub scroll_uv: [f32; 2],
    pub pat_frame_count: usize,
    pub pat_frame_table: Vec<usize>,
    pub pat_frequency: f32,
    /// Sampler wrap U (0=Repeat, 1=MirrorRepeat, 2=ClampToEdge) from TextureSampler3–5.
    pub wrap_u: u8,
    /// Sampler wrap V
    pub wrap_v: u8,
}

/// A single keyframe value (x, y, z, time) for emitter-level animations.
#[derive(Debug, Clone, Default)]
pub struct AnimKeyframe {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub time: f32,
}

/// Emitter-level animation track loaded from EA*.json sidecar files.
#[derive(Debug, Clone)]
pub struct EmitterAnimDef {
    pub enable: bool,
    pub loop_: bool,
    pub randomize_start_frame: bool,
    pub loop_count: u32,
    pub key_frames: Vec<AnimKeyframe>,
}

/// A single emitter definition parsed from the .ptcl emitter data block.
#[derive(Debug, Clone)]
pub struct EmitterDef {
    pub name: String,
    pub emit_type: EmitType,
    pub blend_type: BlendType,
    pub display_side: DisplaySide,
    /// Base emission rate (particles per frame)
    pub emission_rate: f32,
    pub emission_rate_random: f32,
    /// Initial particle speed
    pub initial_speed: f32,
    pub speed_random: f32,
    /// Gravity / acceleration
    pub accel: Vec3,
    /// Per-frame velocity damping (nw::eft `EmitterStatic.AirRes`). 1.0 = no drag;
    /// values < 1.0 decay velocity geometrically each frame (`v *= air_res`).
    pub air_res: f32,
    /// Particle lifetime in frames
    pub lifetime: f32,
    pub lifetime_random: f32,
    /// Base particle scale
    pub scale: f32,
    pub scale_random: f32,
    /// Rotation speed (radians/frame)
    pub rotation_speed: f32,
    pub rotation_init: f32,
    pub rotation_init_random: f32,
    /// Color table 0 (up to 8 RGBA entries, each 8 bytes: frame u32 + rgba u8x4)
    pub color0: Vec<ColorKey>,
    /// Color table 1
    pub color1: Vec<ColorKey>,
    /// Alpha animation (3v4k approximation — use alpha0_keys for full fidelity)
    pub alpha0: AnimKey3v4k,
    pub alpha1: AnimKey3v4k,
    /// Full alpha key tables for accurate multi-key interpolation
    pub alpha0_keys: Vec<ColorKey>,
    pub alpha1_keys: Vec<ColorKey>,
    /// Scale animation (3v4k approximation — fallback when [`scale_keys`] is empty)
    pub scale_anim: AnimKey3v4k,
    /// Full scale key table (up to 8 keys); value stored in each key's channels.
    /// Sampled with [`sample_alpha`] for accurate multi-key scale curves.
    pub scale_keys: Vec<ColorKey>,
    /// Textures (up to 3)
    pub textures: Vec<TextureRes>,
    /// Mesh type: 0=billboard quad, 1=primitive mesh
    pub mesh_type: u32,
    /// Primitive index (if mesh_type == 1)
    pub primitive_index: u32,
    /// Texture index into the BNTX texture array (for VFXB)
    pub texture_index: u32,
    /// UV scale for texture sampling (from TexPatAnim[0], default [1.0, 1.0])
    pub tex_scale_uv: [f32; 2],
    /// UV offset for texture sampling (from TexPatAnim[0], default [0.0, 0.0])
    pub tex_offset_uv: [f32; 2],
    /// UV scroll speed (from TexScrollAnim[0], default [0.0, 0.0])
    pub tex_scroll_uv: [f32; 2],
    /// Atlas grid division from TexScrollAnim UVDivX/Y (0 = unknown).
    pub tex_uv_div: [u32; 2],
    /// Number of animation frames in the sprite sheet (from TexPatAnim PatternCount)
    pub tex_pat_frame_count: usize,
    /// Per-frame sprite-sheet indices from TexPatAnim[0].Table.
    pub tex_pat_frame_table: Vec<usize>,
    /// Pattern playback rate over particle lifetime (TexPatAnim[0].Frequency).
    pub tex_pat_frequency: f32,
    /// TextureAnim0.PatternAnimType.
    pub tex_pattern_anim_type: u8,
    /// TextureAnim0.IsScroll — UV scroll path when no pattern frames.
    pub tex_is_scroll: bool,
    /// TextureAnim0.IsRotate — apply TexScrollAnim rotation during scroll.
    pub tex_is_rotate: bool,
    /// TextureAnim0.IsScale — scroll/scale path uses animated UV scale.
    pub tex_is_scale: bool,
    /// TexScrollAnim0.Rotation (initial rotation contribution).
    pub tex_scroll_rotation: f32,
    /// TexScrollAnim0.RotationAdd (rotation speed, radians/frame).
    pub tex_scroll_rotation_add: f32,
    /// TextureAnim0.InvRandU — flip U at spawn (per-particle seed).
    pub tex_inv_rand_u: bool,
    /// TextureAnim0.InvRandV — flip V at spawn.
    pub tex_inv_rand_v: bool,
    /// TextureAnim0.IsPatAnimLoopRandom — randomize pattern phase at spawn.
    pub tex_pat_loop_random: bool,
    /// TextureAnim0.IsCrossfade — blend between consecutive pattern frames.
    pub tex_crossfade: bool,
    /// TextureAnim1 flags + slot-1 pattern timing (alpha / indirect texture).
    pub indirect_anim: TextureAnimFlags,
    pub indirect_pat_frame_count: usize,
    pub indirect_pat_frame_table: Vec<usize>,
    pub indirect_pat_frequency: f32,
    /// TextureAnim2 flags + slot-2 pattern timing.
    pub tex2_anim: TextureAnimFlags,
    pub tex2_pat_frequency: f32,
    /// TextureAnim3–5 when the combiner references those sampler slots.
    pub tex_anims_extra: [TextureAnimFlags; 3],
    /// TexPatAnim3–5 / TexScrollAnim3–5 metadata paired with [`tex_anims_extra`].
    pub tex_extra_slots: [TexExtraSlotDef; 3],
    /// Emitter local position offset (Trans from EmitterInfo)
    pub emitter_offset: Vec3,
    /// Emitter local rotation (Euler angles XYZ in radians, from EmitterInfo Rotate)
    pub emitter_rotation: Vec3,
    /// Emitter local scale (per-axis, from EmitterInfo Scale)
    pub emitter_scale: Vec3,
    /// Per-axis spawn translation jitter (EmitterInfo TransRand*)
    pub trans_rand: Vec3,
    /// Spherical spawn offset radius (Emission.PositionRandom)
    pub position_random: f32,
    /// Bone attachment mode (EmitterInfo FollowType)
    pub follow_type: FollowType,
    /// When true, particle base transform tracks the emitter each frame (EmitterInfo).
    pub is_update_matrix_by_emit: bool,
    /// Vertex transform mode (ParticleData.BillboardType / VertexTransformMode).
    pub billboard_type: BillboardType,
    /// Particle rotation mode (ParticleData.RotType; non-zero enables spin).
    pub rot_type: u32,
    /// Per-axis rotation flags (ParticleData IsRotateX/Y/Z).
    pub rot_axis_x: bool,
    pub rot_axis_y: bool,
    pub rot_axis_z: bool,
    /// Corner pivot / offset mode (ParticleData.OffsetType).
    pub offset_type: u32,
    /// Render pass id (EmitterInfo.DrawPath). On NVN each path may target a separate RT;
    /// the editor composites paths via sequential wgpu passes into one offscreen texture.
    pub draw_path: u32,
    /// EmitterStatic.Flags1–4 from VFXB export (NVN render-flag mask components).
    pub flags1: u32,
    pub flags2: u32,
    pub flags3: u32,
    pub flags4: u32,
    /// ParticleData.ColorScale multiplier.
    pub color_scale: f32,
    /// Emitter volume radii (ShapeInfo VolumeRadius*)
    pub volume_radius: Vec3,
    /// Emitter volume form scale (ShapeInfo VolumeFormScale*)
    pub volume_form_scale: Vec3,
    /// Line emitter length / center (ShapeInfo LineLength / LineCenter)
    pub line_length: f32,
    pub line_center: f32,
    /// Surface position randomization (ShapeInfo VolumeSurfacePosRand)
    pub volume_surface_pos_rand: f32,
    /// Arc sweep width in radians (ShapeInfo SweepLongitude / volumeSweepParam).
    pub sweep_longitude: f32,
    /// Minimum spawn latitude in radians (ShapeInfo SweepLatitude / volumeLatitude).
    pub sweep_latitude: f32,
    /// Arc start angle in radians (ShapeInfo SweepStart / volumeSweepStart).
    pub sweep_start: f32,
    /// Randomize arc start per particle (ShapeInfo SweepStartRandom).
    pub sweep_start_random: bool,
    /// Arc emission mode (ShapeInfo ArcType).
    pub arc_type: ArcType,
    /// Fixed circle divide count override (ShapeInfo NumDivideCircle, 0 = use emit count).
    pub num_divide_circle: u32,
    /// Randomize circle divide index (ShapeInfo NumDivideCircleRandom).
    pub num_divide_circle_random: u32,
    /// Fixed line divide count override (ShapeInfo NumDivideLine).
    pub num_divide_line: u32,
    /// Randomize line divide index (ShapeInfo NumDivideLineRandom).
    pub num_divide_line_random: u32,
    /// Use latitude-limited sphere emission (ShapeInfo IsVolumeLatitudeEnabled).
    pub is_volume_latitude_enabled: bool,
    /// Index into same-divide sphere tables (ShapeInfo VolumeTblIndex).
    pub volume_tbl_index: u8,
    /// Index into 64-point sphere tables (ShapeInfo VolumeTblIndex64).
    pub volume_tbl_index64: u8,
    /// Latitude basis axis selector (ShapeInfo VolumeLatitudeDir).
    pub volume_latitude_dir: u8,
    /// Inner-radius ratio for fill-circle (ShapeInfo CaliberRatio / volumeCaliber).
    pub caliber_ratio: f32,
    /// Primitive emit mode: 0=Vertex, 1=Random, 2=EmissionRate (ShapeInfo PrimEmitType).
    pub prim_emit_type: u32,
    /// Shape primitive index (ShapeInfo PrimitiveIndex).
    pub shape_primitive_index: u64,
    /// Particle primitive id (ParticleData PrimitiveID).
    pub particle_primitive_id: u64,
    /// Per-axis spawn rotation randomizer (EmitterInfo RotateRand*).
    pub rotate_rand: Vec3,
    /// Distance-based emission along emitter motion (Emission.IsEmitDistEnabled).
    pub is_emit_dist_enabled: bool,
    pub emitter_dist_unit: f32,
    pub emitter_dist_min: f32,
    pub emitter_dist_max: f32,
    pub emitter_dist_marg: f32,
    pub emitter_dist_particles_max: u32,
    /// Fixed emission direction when not omnidirectional (ParticleVelocity DesignatedDir*)
    pub designated_dir: Vec3,
    /// When false, velocity uses [`designated_dir`] instead of volume spread.
    pub use_omnidirectional: bool,
    /// Velocity direction is world-space (Emission.IsWorldOrientedVelocity).
    pub is_world_oriented_velocity: bool,
    /// Cone half-angle in degrees around emit direction (ParticleVelocity.DiffusionDirAngle).
    pub diffusion_dir_angle: f32,
    /// Per-axis direction jitter (ParticleVelocity.DiffusionX/Y/Z).
    pub diffusion_axis: Vec3,
    /// Add normalized spawn XZ * this to velocity direction (ParticleVelocity.XZDiffusion).
    pub xz_diffusion: f32,
    /// Scale for inheriting emitter motion into particle velocity (ParticleVelocity.EmVelInherit).
    pub em_vel_inherit: f32,
    /// Child-emitter linkage and inheritance from parent particles (ChildInheritance + Action).
    pub child_inheritance: ChildInheritanceDef,
    /// Whether this emitter fires a one-shot burst (from VFXB Emission.isOneTime)
    pub is_one_time: bool,
    /// Emission window start in effect-local frames (VFXB Emission.Start)
    pub emission_start: u32,
    /// One-shot burst frame, or legacy timing field (VFXB Emission.Timing)
    pub emission_timing: u32,
    /// Emission window length in frames (VFXB Emission.Duration)
    pub emission_duration: u32,
    /// true when textures[1].tex_name contains "indirect" (case-insensitive)
    pub is_indirect_slot1: bool,
    /// UV distortion scale for indirect textures; parsed from VFXB TexScrollAnim[1]+8, clamped [0,1]
    pub distortion_strength: f32,
    /// UV scroll speed for the indirect texture (from TexScrollAnim[1], default [0.0, 0.0])
    pub indirect_scroll_uv: [f32; 2],
    /// UV scale for the indirect texture (from TexPatAnim[1], default [1.0, 1.0])
    pub indirect_tex_scale_uv: [f32; 2],
    /// UV offset for the indirect texture (from TexPatAnim[1], default [0.0, 0.0])
    pub indirect_tex_offset_uv: [f32; 2],
    /// Slot-2 UV scale (from TexPatAnim[2], default [1.0, 1.0])
    pub tex2_scale_uv: [f32; 2],
    /// Slot-2 UV offset (from TexPatAnim[2], default [0.0, 0.0])
    pub tex2_offset_uv: [f32; 2],
    /// Slot-2 UV scroll speed (from TexScrollAnim[2], default [0.0, 0.0])
    pub tex2_scroll_uv: [f32; 2],
    /// Slot-2 texture sampler wrap mode U
    pub tex2_wrap_u: u8,
    /// Slot-2 texture sampler wrap mode V
    pub tex2_wrap_v: u8,
    /// Slot-2 sprite-sheet frame count (from TexPatAnim[2].num)
    pub tex2_pat_frame_count: usize,
    /// Slot-2 per-frame sprite-sheet indices (from TexPatAnim[2].Table)
    pub tex2_pat_frame_table: Vec<usize>,
    /// Emitter-level translation animation (from EAET.json)
    pub anim_translate: Option<EmitterAnimDef>,
    /// Emitter-level rotation animation (from EAER.json)
    pub anim_rotation: Option<EmitterAnimDef>,
    /// Emitter-level scale animation (from EAES.json)
    pub anim_emit_scale: Option<EmitterAnimDef>,
    /// Texture scale animation (from EASL.json)
    pub anim_tex_scale: Option<EmitterAnimDef>,
    /// Color 0 animation (from EAC0.json)
    pub anim_color0: Option<EmitterAnimDef>,
    /// Color 1 animation (from EAC1.json)
    pub anim_color1: Option<EmitterAnimDef>,
    /// Alpha animation (from EAA0.json)
    pub anim_alpha: Option<EmitterAnimDef>,
    /// Texture sampler wrap mode U (0=Repeat, 1=MirrorRepeat, 2=ClampToEdge)
    pub tex_wrap_u: u8,
    /// Texture sampler wrap mode V
    pub tex_wrap_v: u8,
    /// BNSH shader index (from ShaderReferences.shader_index, -1 = none/default)
    pub shader_index: i32,
    /// BNSH custom shader index (from ShaderReferences.custom_shader_index)
    pub custom_shader_index: u32,
    /// User-defined shader indices (from ShaderReferences.user_shader_index1/2)
    pub user_shader_indices: [i32; 2],
    /// Content hash of this emitter's embedded Shader.bnsh (0 = use registry default).
    pub shader_key: crate::shader_registry::ShaderKey,
    /// Combiner configuration from EmitterData.json.
    pub combiner: crate::shader_registry::CombinerState,
    /// Soft-particle / fresnel / decal flags from EmitterData.json.
    pub particle_color: crate::shader_registry::ParticleColorState,
    /// Camera-distance scale references from `ParticleScale` (distortion + future scale).
    pub particle_scale: crate::shader_registry::ParticleScaleState,
}

/// Child emitter inheritance flags from EFT2 `EmitterInheritance` + `Action.ActionIndex`.
#[derive(Debug, Clone)]
pub struct ChildInheritanceDef {
    pub inherit_velocity: bool,
    pub inherit_scale: bool,
    pub inherit_rotate: bool,
    pub inherit_color0: bool,
    pub inherit_color1: bool,
    pub inherit_alpha0: bool,
    pub inherit_alpha1: bool,
    pub inherit_color_scale: bool,
    pub inherit_draw_path: bool,
    pub inherit_pre_draw: bool,
    pub inherit_alpha0_each_frame: bool,
    pub inherit_alpha1_each_frame: bool,
    pub velocity_rate: f32,
    pub scale_rate: f32,
    /// When true, particles spawn only when a parent emitter's particle dies.
    pub spawn_from_parent_particle: bool,
    /// Parent emitter index within the same emitter set (`Action.ActionIndex`).
    pub parent_emitter_idx: u32,
}

impl Default for ChildInheritanceDef {
    fn default() -> Self {
        Self {
            inherit_velocity: false,
            inherit_scale: false,
            inherit_rotate: false,
            inherit_color0: false,
            inherit_color1: false,
            inherit_alpha0: false,
            inherit_alpha1: false,
            inherit_color_scale: false,
            inherit_draw_path: false,
            inherit_pre_draw: false,
            inherit_alpha0_each_frame: false,
            inherit_alpha1_each_frame: false,
            velocity_rate: 1.0,
            scale_rate: 1.0,
            spawn_from_parent_particle: false,
            parent_emitter_idx: 0,
        }
    }
}

/// Per-channel inheritance multipliers applied before the combiner.
#[derive(Debug, Clone)]
pub struct ParticleInheritState {
    pub color0_mul: [f32; 3],
    pub color1_mul: [f32; 3],
    pub alpha0_mul: f32,
    pub alpha1_mul: f32,
    pub color_scale: f32,
    pub alpha0_each_frame: bool,
    pub alpha1_each_frame: bool,
    pub parent_seed: u64,
    pub parent_set_idx: usize,
    pub parent_emitter_idx: usize,
    pub draw_path: Option<u32>,
    pub pre_draw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EmitType {
    Point,
    Circle,
    CircleSameDivide,
    FillCircle,
    Sphere,
    SphereSameDivide,
    SphereSameDivide64,
    FillSphere,
    Cylinder,
    FillCylinder,
    Box,
    FillBox,
    Line,
    LineSameDivide,
    Rectangle,
    Primitive,
    Unknown(u32),
}

impl From<u32> for EmitType {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::Point, 1 => Self::Circle, 2 => Self::CircleSameDivide,
            3 => Self::FillCircle, 4 => Self::Sphere, 5 => Self::SphereSameDivide,
            6 => Self::SphereSameDivide64, 7 => Self::FillSphere, 8 => Self::Cylinder,
            9 => Self::FillCylinder, 10 => Self::Box, 11 => Self::FillBox,
            12 => Self::Line, 13 => Self::LineSameDivide, 14 => Self::Rectangle,
            15 => Self::Primitive, v => Self::Unknown(v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendType { Normal, Add, Sub, Screen, Multiply, Unknown(u32) }
impl From<u32> for BlendType {
    fn from(v: u32) -> Self {
        // NintendoWare VFXB blend type enum (verified from file data):
        // 0=Normal, 1=Sub, 2=Screen, 3=Add, 4=Multiply
        match v { 0 => Self::Normal, 1 => Self::Sub, 2 => Self::Screen,
                  3 => Self::Add, 4 => Self::Multiply, v => Self::Unknown(v) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySide { Both, Front, Back, Unknown(u32) }
impl From<u32> for DisplaySide {
    fn from(v: u32) -> Self {
        match v { 0 => Self::Both, 1 => Self::Front, 2 => Self::Back, v => Self::Unknown(v) }
    }
}

/// Bone follow mode from EmitterInfo.FollowType (PtclFollowType in NW).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowType {
    #[default]
    Srt = 0,
    None = 1,
    Translate = 2,
}

impl From<u32> for FollowType {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::Srt,
            1 => Self::None,
            2 => Self::Translate,
            _ => Self::Srt,
        }
    }
}

/// Arc sweep mode for circle/cylinder/sphere volumes (ShapeInfo `ArcType`, Switch EFT2).
///
/// Switch EFT2 [`EmitterShapeInfo::ArcType`](extern/effect-library/EffectLibrary/FileData/EFT2/EmitterStructs/Emitter.cs)
/// and nw4f Cafe `eft_EmitterVolume.cpp` only define 0–2. Values beyond that are preserved as
/// [`ArcType::Unknown`] and treated like Random for non-`*SameDivide` sweep sampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArcType {
    /// Random angle within [`EmitterDef::sweep_longitude`] starting at [`EmitterDef::sweep_start`].
    #[default]
    Random,
    /// Equally-spaced stepping for `*SameDivide` emitters (uses NumDivide* when set).
    EquallyDivided,
    /// Fixed [`EmitterDef::sweep_start`] only (no spread within arc width).
    Fixed,
    /// Unrecognized on-disk value; behaves like Random for sweep spread.
    Unknown(u8),
}

impl From<u8> for ArcType {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Random,
            1 => Self::EquallyDivided,
            2 => Self::Fixed,
            v => Self::Unknown(v),
        }
    }
}

impl ArcType {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Random => 0,
            Self::EquallyDivided => 1,
            Self::Fixed => 2,
            Self::Unknown(v) => v,
        }
    }
}

/// Vertex transform / billboard mode (ParticleData.BillboardType).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum BillboardType {
    #[default]
    Billboard = 0,
    PlateXy = 1,
    PlateXz = 2,
    DirectionalY = 3,
    DirectionalPolygon = 4,
    Stripe = 5,
    ComplexStripe = 6,
    Primitive = 7,
    YBillboard = 8,
    Unknown(u32),
}

impl From<u32> for BillboardType {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::Billboard,
            1 => Self::PlateXy,
            2 => Self::PlateXz,
            3 => Self::DirectionalY,
            4 => Self::DirectionalPolygon,
            5 => Self::Stripe,
            6 => Self::ComplexStripe,
            7 => Self::Primitive,
            8 => Self::YBillboard,
            v => Self::Unknown(v),
        }
    }
}

impl BillboardType {
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Billboard => 0,
            Self::PlateXy => 1,
            Self::PlateXz => 2,
            Self::DirectionalY => 3,
            Self::DirectionalPolygon => 4,
            Self::Stripe => 5,
            Self::ComplexStripe => 6,
            Self::Primitive => 7,
            Self::YBillboard => 8,
            Self::Unknown(v) => v,
        }
    }
}

/// Cache key for render pipeline variants: one pipeline per (blend, cull, geometry) combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    pub blend_type:   BlendType,
    pub display_side: DisplaySide,
    pub is_mesh:      bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ColorKey {
    pub frame: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// 3-value 4-key animation (as documented in PTCL spec).
/// Encodes: value1, value2 = value1+start_diff, value3 = value2+end_diff
/// at times: 0, time2, time3, 1.0 (normalized)
#[derive(Debug, Clone, Copy)]
pub struct AnimKey3v4k {
    pub start_value: f32,
    pub start_diff: f32,
    pub end_diff: f32,
    pub time2: f32,
    pub time3: f32,
}

impl AnimKey3v4k {
    pub fn sample(&self, t: f32) -> f32 {
        let v1 = self.start_value;
        let v2 = v1 + self.start_diff;
        let v3 = v2 + self.end_diff;
        if self.time2 <= 0.0 && self.time3 <= 0.0 { return v1; }
        if t <= 0.0 { return v1; }
        if t >= 1.0 { return v3; }
        if t < self.time2 {
            let s = t / self.time2.max(0.0001);
            v1 + (v2 - v1) * s
        } else if t < self.time3 {
            v2
        } else {
            let s = (t - self.time3) / (1.0 - self.time3).max(0.0001);
            v2 + (v3 - v2) * s
        }
    }
}

impl Default for AnimKey3v4k {
    fn default() -> Self { Self { start_value: 1.0, start_diff: 0.0, end_diff: -1.0, time2: 0.5, time3: 0.8 } }
}

impl Default for EmitterDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            emit_type: EmitType::Point,
            blend_type: BlendType::Add,
            display_side: DisplaySide::Both,
            emission_rate: 8.0,
            emission_rate_random: 0.0,
            initial_speed: 0.3,
            speed_random: 0.3,
            accel: Vec3::ZERO,
            air_res: 1.0,
            lifetime: 30.0,
            lifetime_random: 0.0,
            scale: 1.0,
            scale_random: 0.0,
            rotation_speed: 0.0,
            rotation_init: 0.0,
            rotation_init_random: 0.0,
            color0: Vec::new(),
            color1: Vec::new(),
            alpha0: AnimKey3v4k::default(),
            alpha1: AnimKey3v4k::default(),
            alpha0_keys: vec![],
            alpha1_keys: vec![],
            scale_anim: AnimKey3v4k::default(),
            scale_keys: vec![],
            textures: Vec::new(),
            mesh_type: 0,
            primitive_index: 0,
            texture_index: u32::MAX,
            tex_scale_uv: [1.0, 1.0],
            tex_offset_uv: [0.0, 0.0],
            tex_scroll_uv: [0.0, 0.0],
            tex_uv_div: [0, 0],
            tex_pat_frame_count: 1,
            tex_pat_frame_table: Vec::new(),
            tex_pat_frequency: 1.0,
            tex_pattern_anim_type: 0,
            tex_is_scroll: false,
            tex_is_rotate: false,
            tex_is_scale: false,
            tex_scroll_rotation: 0.0,
            tex_scroll_rotation_add: 0.0,
            tex_inv_rand_u: false,
            tex_inv_rand_v: false,
            tex_pat_loop_random: false,
            tex_crossfade: false,
            indirect_anim: TextureAnimFlags::default(),
            indirect_pat_frame_count: 1,
            indirect_pat_frame_table: Vec::new(),
            indirect_pat_frequency: 1.0,
            tex2_anim: TextureAnimFlags::default(),
            tex2_pat_frequency: 1.0,
            tex_anims_extra: [TextureAnimFlags::default(); 3],
            tex_extra_slots: std::array::from_fn(|_| TexExtraSlotDef::default()),
            emitter_offset: Vec3::ZERO,
            emitter_rotation: Vec3::ZERO,
            emitter_scale: Vec3::ONE,
            trans_rand: Vec3::ZERO,
            position_random: 0.0,
            follow_type: FollowType::Srt,
            is_update_matrix_by_emit: false,
            billboard_type: BillboardType::Billboard,
            rot_type: 0,
            rot_axis_x: false,
            rot_axis_y: false,
            rot_axis_z: false,
            offset_type: 0,
            draw_path: 0,
            flags1: 0,
            flags2: 0,
            flags3: 0,
            flags4: 0,
            color_scale: 1.0,
            volume_radius: Vec3::ONE,
            volume_form_scale: Vec3::ONE,
            line_length: 1.0,
            line_center: 0.0,
            volume_surface_pos_rand: 0.0,
            sweep_longitude: 0.0,
            sweep_latitude: 0.0,
            sweep_start: 0.0,
            sweep_start_random: false,
            arc_type: ArcType::Random,
            num_divide_circle: 0,
            num_divide_circle_random: 0,
            num_divide_line: 0,
            num_divide_line_random: 0,
            is_volume_latitude_enabled: false,
            volume_tbl_index: 0,
            volume_tbl_index64: 0,
            volume_latitude_dir: 0,
            caliber_ratio: 0.0,
            prim_emit_type: 0,
            shape_primitive_index: 0,
            particle_primitive_id: 0,
            rotate_rand: Vec3::ZERO,
            is_emit_dist_enabled: false,
            emitter_dist_unit: 1.0,
            emitter_dist_min: 0.0,
            emitter_dist_max: 0.0,
            emitter_dist_marg: 0.0,
            emitter_dist_particles_max: 0,
            designated_dir: Vec3::Z,
            use_omnidirectional: true,
            is_world_oriented_velocity: false,
            diffusion_dir_angle: 0.0,
            diffusion_axis: Vec3::ZERO,
            xz_diffusion: 0.0,
            em_vel_inherit: 0.0,
            child_inheritance: ChildInheritanceDef::default(),
            is_one_time: false,
            emission_start: 0,
            emission_timing: 0,
            emission_duration: 9999,
            is_indirect_slot1: false,
            distortion_strength: 0.0,
            indirect_scroll_uv: [0.0, 0.0],
            indirect_tex_scale_uv: [1.0, 1.0],
            indirect_tex_offset_uv: [0.0, 0.0],
            tex2_scale_uv: [1.0, 1.0],
            tex2_offset_uv: [0.0, 0.0],
            tex2_scroll_uv: [0.0, 0.0],
            tex_wrap_u: 2,
            tex_wrap_v: 2,
            tex2_wrap_u: 2,
            tex2_wrap_v: 2,
            tex2_pat_frame_count: 1,
            tex2_pat_frame_table: Vec::new(),
            anim_translate: None,
            anim_rotation: None,
            anim_emit_scale: None,
            anim_tex_scale: None,
            anim_color0: None,
            anim_color1: None,
            anim_alpha: None,
            shader_index: -1,
            custom_shader_index: 0,
            user_shader_indices: [-1, -1],
            shader_key: 0,
            combiner: crate::shader_registry::CombinerState::default(),
            particle_color: crate::shader_registry::ParticleColorState::default(),
            particle_scale: crate::shader_registry::ParticleScaleState::default(),
        }
    }
}

impl Default for TextureRes {
    fn default() -> Self {
        Self {
            tex_name: String::new(),
            width: 1,
            height: 1,
            ftx_format: 0x0B01,
            ftx_data_offset: 0,
            ftx_data_size: 0,
            original_format: 0x0B01,
            original_data_offset: 0,
            original_data_size: 0,
            wrap_mode: 0,
            filter_mode: 1,
            mipmap_count: 1,
            channel_swizzle: 0,
        }
    }
}

/// Build a rotation matrix from XYZ-named Euler angles composed in ZYX order.
fn mat_from_euler_zyx(angles: Vec3) -> Mat4 {
    Mat4::from_euler(glam::EulerRot::ZYX, angles.z, angles.y, angles.x)
}

/// Build the emitter's local TRS matrix: T * R * S.
/// Returns `Mat4::IDENTITY` (and logs to stderr) if the resulting matrix is degenerate
/// (determinant < 1e-6), per Requirement 7.3.
pub fn build_emitter_trs(emitter: &EmitterDef) -> Mat4 {
    build_emitter_trs_at(emitter, 0.0)
}

/// Build emitter TRS at normalized effect time `effect_t` (0..1), applying EA translate /
/// rotate / emit-scale animation tracks when enabled.
pub fn build_emitter_trs_at(emitter: &EmitterDef, effect_t: f32) -> Mat4 {
    let mut trans = emitter.emitter_offset;
    let mut rot = emitter.emitter_rotation;
    let mut scale = emitter.emitter_scale;

    if let Some(anim) = &emitter.anim_translate {
        if anim.enable && !anim.key_frames.is_empty() {
            let v = sample_emitter_anim_track(anim, effect_t);
            trans += Vec3::new(v[0], v[1], v[2]);
        }
    }
    if let Some(anim) = &emitter.anim_rotation {
        if anim.enable && !anim.key_frames.is_empty() {
            let v = sample_emitter_anim_track(anim, effect_t);
            rot += Vec3::new(v[0], v[1], v[2]);
        }
    }
    if let Some(anim) = &emitter.anim_emit_scale {
        if anim.enable && !anim.key_frames.is_empty() {
            let v = sample_emitter_anim_track(anim, effect_t);
            scale *= Vec3::new(v[0].max(0.001), v[1].max(0.001), v[2].max(0.001));
        }
    }

    let t = Mat4::from_translation(trans);
    let r = mat_from_euler_zyx(rot);
    let s = Mat4::from_scale(scale);
    let trs = t * r * s;
    let det = trs.determinant();
    if det.abs() < 1e-6 {
        eprintln!(
            "[TRS] degenerate emitter transform (det={det:.2e}) for '{}', using IDENTITY",
            emitter.name
        );
        return Mat4::IDENTITY;
    }
    trs
}

/// Sample an EA* emitter animation track at normalized time `t` (0..1).
pub fn sample_emitter_anim_track(anim: &EmitterAnimDef, t: f32) -> [f32; 3] {
    if !anim.enable || anim.key_frames.is_empty() {
        return [0.0, 0.0, 0.0];
    }
    let keys = &anim.key_frames;
    let t = t.clamp(0.0, 1.0);
    if t <= keys[0].time {
        return [keys[0].x, keys[0].y, keys[0].z];
    }
    let last = &keys[keys.len() - 1];
    if t >= last.time {
        return [last.x, last.y, last.z];
    }
    for i in 0..keys.len() - 1 {
        let a = &keys[i];
        let b = &keys[i + 1];
        if t >= a.time && t <= b.time {
            let range = (b.time - a.time).max(0.0001);
            let s = (t - a.time) / range;
            return [
                a.x + (b.x - a.x) * s,
                a.y + (b.y - a.y) * s,
                a.z + (b.z - a.z) * s,
            ];
        }
    }
    [0.0, 0.0, 0.0]
}

/// Apply FollowType semantics to a bone world matrix.
pub fn follow_bone_matrix(bone_mat: Mat4, follow_type: FollowType) -> Mat4 {
    match follow_type {
        FollowType::Srt => bone_mat,
        FollowType::None => Mat4::IDENTITY,
        FollowType::Translate => Mat4::from_translation(bone_mat.w_axis.truncate()),
    }
}

/// Full world matrix for an emitter instance at effect time `effect_t`.
pub fn compute_emitter_world_mat(
    emitter: &EmitterDef,
    inst: &EmitterInstance,
    bone_mat: Mat4,
    effect_t: f32,
) -> Mat4 {
    let parent = follow_bone_matrix(bone_mat, emitter.follow_type);
    let inst_mat =
        mat_from_euler_zyx(inst.rotation()) * Mat4::from_translation(inst.offset());
    let emitter_mat = build_emitter_trs_at(emitter, effect_t);
    parent * inst_mat * emitter_mat
}

/// Only [`EmitterDef::is_update_matrix_by_emit`] re-parents particles each frame.
/// [`FollowType`] affects emitter spawn origin via [`compute_emitter_world_mat`], not
/// per-particle motion after spawn (explosions/sparks integrate velocity in world space).
///
/// Stationary particles on SRT-following emitters also re-parent so attached auras
/// (fair aerial slashes, glows) stay on the moving bone without `IsUpdateMatrixByEmit`.
pub fn particle_follows_emitter(emitter: &EmitterDef) -> bool {
    if emitter.is_update_matrix_by_emit {
        return true;
    }
    emitter.follow_type == FollowType::Srt && emitter.initial_speed.abs() < 1e-3
}

/// Pack a `Mat4` as the three row vectors written to NVN `cbuf_8[12..14]`.
pub fn mat4_to_cbuf_rows_3x4(m: Mat4) -> [[f32; 4]; 3] {
    [
        [m.x_axis.x, m.y_axis.x, m.z_axis.x, m.w_axis.x],
        [m.x_axis.y, m.y_axis.y, m.z_axis.y, m.w_axis.y],
        [m.x_axis.z, m.y_axis.z, m.z_axis.z, m.w_axis.z],
    ]
}

/// Which Euler axes participate in billboard corner rotation.
#[derive(Debug, Clone, Copy, Default)]
pub struct RotAxisMask {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

impl RotAxisMask {
    pub fn from_emitter(emitter: &EmitterDef) -> Self {
        if emitter.rot_type == 0 {
            return Self::default();
        }
        let any = emitter.rot_axis_x || emitter.rot_axis_y || emitter.rot_axis_z;
        Self {
            x: emitter.rot_axis_x,
            y: emitter.rot_axis_y,
            z: emitter.rot_axis_z || !any,
        }
    }

    pub fn any(self) -> bool {
        self.x || self.y || self.z
    }
}

/// Live + spawn Euler rotation for one particle (radians).
pub fn particle_rotation_euler(p: &Particle, emitter: &EmitterDef) -> Vec3 {
    let axes = RotAxisMask::from_emitter(emitter);
    if emitter.rot_type == 0 || !axes.any() {
        return Vec3::ZERO;
    }
    let mut e = p.rotation_rand;
    if axes.x {
        e.x += p.rotation;
    }
    if axes.y {
        e.y += p.rotation;
    }
    if axes.z {
        e.z += p.rotation;
    }
    e
}

/// Rotate billboard corner half-extents around Z in the quad plane.
pub fn rotate_billboard_corner(corner: [f32; 2], z_angle: f32, rot_type: u32, axes: RotAxisMask) -> [f32; 2] {
    if rot_type == 0 || !axes.z || z_angle.abs() < 1e-6 {
        return corner;
    }
    let (c, s) = (z_angle.cos(), z_angle.sin());
    [corner[0] * c - corner[1] * s, corner[0] * s + corner[1] * c]
}

/// Tilt camera-facing basis by RotType X/Y axes (spawn + live euler components).
pub fn tilt_billboard_basis(
    right: Vec3,
    up: Vec3,
    euler: Vec3,
    axes: RotAxisMask,
) -> (Vec3, Vec3) {
    let mut r = right;
    let mut u = up;
    if axes.x && euler.x.abs() > 1e-6 {
        let m = glam::Mat3::from_rotation_x(euler.x);
        r = m * r;
        u = m * u;
    }
    if axes.y && euler.y.abs() > 1e-6 {
        let m = glam::Mat3::from_rotation_y(euler.y);
        r = m * r;
        u = m * u;
    }
    (r, u)
}

/// Pivot bias for attr6.zw from OffsetType (0 = centered quad).
pub fn billboard_pivot_bias(offset_type: u32) -> [f32; 2] {
    match offset_type {
        1 => [0.0, -0.5],
        2 => [-0.5, 0.0],
        3 => [0.0, 0.5],
        4 => [0.5, 0.0],
        5 => [-0.5, -0.5],
        6 => [0.5, -0.5],
        7 => [-0.5, 0.5],
        8 => [0.5, 0.5],
        _ => [0.0, 0.0],
    }
}

/// cbuf_9[47] layout for native VS pivot chain (.y = Y offset, .z = X/init).
pub fn billboard_pivot_cbuf47(offset_type: u32) -> [f32; 4] {
    let pivot = billboard_pivot_bias(offset_type);
    [0.0, pivot[1], pivot[0], 0.0]
}

/// Camera-facing basis (right, up) for a billboard mode.
pub fn billboard_basis(
    bb_type: BillboardType,
    cam_right: Vec3,
    cam_up: Vec3,
    view_dir: Vec3,
    velocity: Vec3,
) -> (Vec3, Vec3) {
    let fallback = || (cam_right, cam_up);
    match bb_type {
        BillboardType::Billboard => fallback(),
        BillboardType::PlateXy => (Vec3::X, Vec3::Y),
        BillboardType::PlateXz => (Vec3::X, Vec3::Z),
        BillboardType::DirectionalY => {
            let fwd = velocity.normalize_or_zero();
            if fwd.length_squared() < 1e-6 {
                fallback()
            } else {
                let up = Vec3::Y;
                (up.cross(fwd).normalize_or_zero(), up)
            }
        }
        BillboardType::DirectionalPolygon => {
            let fwd = velocity.normalize_or_zero();
            if fwd.length_squared() < 1e-6 {
                fallback()
            } else {
                let right = cam_up.cross(fwd).normalize_or_zero();
                let up = fwd.cross(right).normalize_or_zero();
                (right, up)
            }
        }
        BillboardType::Stripe | BillboardType::ComplexStripe => {
            let along = velocity.normalize_or_zero();
            if along.length_squared() < 1e-6 {
                fallback()
            } else {
                let right = cam_up.cross(along).normalize_or_zero();
                (right, along)
            }
        }
        BillboardType::Primitive => (Vec3::X, Vec3::Y),
        BillboardType::YBillboard => {
            let fwd = view_dir.normalize_or_zero();
            if fwd.length_squared() < 1e-6 {
                fallback()
            } else {
                let up = Vec3::Y;
                (up.cross(fwd).normalize_or_zero(), up)
            }
        }
        BillboardType::Unknown(_) => fallback(),
    }
}

/// Which PRMA/BFRES slot an emitter references (game ShapeInfo vs ParticleData).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrmaMeshRole {
    /// ShapeInfo.PrimitiveIndex — volume / surface spawn mesh.
    Spawn,
    /// ParticleData.PrimitiveID — draw / primitive billboard mesh.
    Draw,
}

/// Raw PRMA id from emitter metadata for the given role.
pub fn emitter_prma_id(emitter: &EmitterDef, role: PrmaMeshRole) -> u64 {
    match role {
        PrmaMeshRole::Spawn => {
            if emitter.shape_primitive_index > 0 {
                emitter.shape_primitive_index
            } else {
                emitter.particle_primitive_id
            }
        }
        PrmaMeshRole::Draw => {
            if emitter.particle_primitive_id > 0 {
                emitter.particle_primitive_id
            } else if emitter.shape_primitive_index > 0 {
                emitter.shape_primitive_index
            } else {
                emitter.primitive_index as u64
            }
        }
    }
}

/// Map a PRMA descriptor id (or small sequential index) to `primitives` vec slot.
pub fn resolve_prma_slot(primitives: &[PrimitiveData], raw_id: u64) -> usize {
    if primitives.is_empty() {
        return 0;
    }
    if raw_id > 0 {
        if let Some(idx) = primitives.iter().position(|p| p.id == raw_id) {
            return idx;
        }
        let as_idx = raw_id as usize;
        if as_idx < primitives.len() {
            return as_idx;
        }
    }
    0
}

/// Resolve PRMA vec index for draw/billboard mesh lookup.
pub fn emitter_primitive_index(emitter: &EmitterDef, primitives: &[PrimitiveData]) -> usize {
    resolve_prma_slot(primitives, emitter_prma_id(emitter, PrmaMeshRole::Draw))
}

/// PRMA mesh for primitive billboard mode, if loaded.
pub fn emitter_primitive<'a>(
    emitter: &EmitterDef,
    primitives: &'a [PrimitiveData],
) -> Option<&'a PrimitiveData> {
    let idx = emitter_primitive_index(emitter, primitives);
    primitives.get(idx).filter(|p| !p.vertices.is_empty())
}

fn basis_from_normal(normal: Vec3) -> (Vec3, Vec3) {
    let mut right = Vec3::Y.cross(normal).normalize_or_zero();
    if right.length_squared() < 1e-8 {
        right = Vec3::X;
    }
    let up = normal.cross(right).normalize_or_zero();
    (right, up)
}

/// Surface-aligned tangent basis from mesh geometry (area-weighted triangle normals).
pub fn mesh_basis(vertices: &[MeshVertex], indices: &[u16]) -> (Vec3, Vec3) {
    let tris = mesh_triangles(vertices, indices);
    if !tris.is_empty() {
        let mut avg_normal = Vec3::ZERO;
        for tri in &tris {
            avg_normal += (tri[1] - tri[0]).cross(tri[2] - tri[0]);
        }
        let normal = avg_normal.normalize_or_zero();
        if normal.length_squared() > 1e-8 {
            return basis_from_normal(normal);
        }
    }
    let mut avg = Vec3::ZERO;
    let mut count = 0usize;
    for v in vertices {
        let n = Vec3::from_array(v.normal);
        if n.length_squared() > 1e-8 {
            avg += n;
            count += 1;
        }
    }
    if count > 0 {
        let normal = (avg / count as f32).normalize_or_zero();
        if normal.length_squared() > 1e-8 {
            return basis_from_normal(normal);
        }
    }
    let tri = |i: usize| -> Option<Vec3> {
        let vi = *indices.get(i)? as usize;
        vertices.get(vi).map(|v| Vec3::from_array(v.position))
    };
    let (Some(v0), Some(v1), Some(v2)) = (tri(0), tri(1), tri(2)) else {
        return (Vec3::X, Vec3::Y);
    };
    let normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
    if normal.length_squared() < 1e-8 {
        (Vec3::X, Vec3::Y)
    } else {
        basis_from_normal(normal)
    }
}

/// Local XY basis for primitive billboard mode from mesh triangles / vertex normals.
pub fn primitive_mesh_basis(prim: &PrimitiveData) -> (Vec3, Vec3) {
    mesh_basis(&prim.vertices, &prim.indices)
}

/// Surface-aligned basis from the emitter draw mesh (BFRES preferred, PRMA fallback).
pub fn draw_mesh_basis(ctx: &SpawnMeshContext<'_>, emitter: &EmitterDef) -> Option<(Vec3, Vec3)> {
    emitter_draw_mesh(ctx, emitter).map(|(verts, idx)| mesh_basis(verts, idx))
}

fn convex_hull_2d(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if points.len() <= 1 {
        return points.to_vec();
    }
    let mut pts: Vec<[f32; 2]> = points.to_vec();
    pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
        a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal)
    }));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6);
    if pts.len() <= 2 {
        return pts;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut lower = Vec::new();
    for p in &pts {
        while lower.len() >= 2
            && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0
        {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2
            && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0
        {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Minimum half-thickness on each billboard axis so degenerate line meshes still rasterize.
const MESH_CORNER_MIN_HALF_THICKNESS: f32 = 0.25;

fn principal_axis_2d(points: &[[f32; 2]]) -> Option<[f32; 2]> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f32;
    let cx = points.iter().map(|p| p[0]).sum::<f32>() / n;
    let cy = points.iter().map(|p| p[1]).sum::<f32>() / n;
    let mut cxx = 0.0f32;
    let mut cyy = 0.0f32;
    let mut cxy = 0.0f32;
    for p in points {
        let dx = p[0] - cx;
        let dy = p[1] - cy;
        cxx += dx * dx;
        cyy += dy * dy;
        cxy += dx * dy;
    }
    cxx /= n;
    cyy /= n;
    cxy /= n;
    let tr = cxx + cyy;
    let det = cxx * cyy - cxy * cxy;
    let lambda1 = tr * 0.5 + (tr * tr * 0.25 - det).max(0.0).sqrt();
    let mut vx = if cxy.abs() > 1e-6 {
        lambda1 - cyy
    } else if cxx >= cyy {
        1.0
    } else {
        0.0
    };
    let mut vy = if cxy.abs() > 1e-6 { cxy } else if cxx >= cyy { 0.0 } else { 1.0 };
    let len = (vx * vx + vy * vy).sqrt();
    if len < 1e-6 {
        return None;
    }
    vx /= len;
    vy /= len;
    Some([vx, vy])
}

fn mesh_tangent_edge_axes(
    vertices: &[MeshVertex],
    indices: &[u16],
    right: Vec3,
    up: Vec3,
) -> Vec<[f32; 2]> {
    let mut axes = Vec::new();
    let mut i = 0;
    while i + 2 < indices.len() {
        let tri = [
            indices[i] as usize,
            indices[i + 1] as usize,
            indices[i + 2] as usize,
        ];
        for j in 0..3 {
            let a = tri[j];
            let b = tri[(j + 1) % 3];
            if a >= vertices.len() || b >= vertices.len() {
                continue;
            }
            let pa = Vec3::from_array(vertices[a].position);
            let pb = Vec3::from_array(vertices[b].position);
            let edge = [pb.dot(right) - pa.dot(right), pb.dot(up) - pa.dot(up)];
            let len = (edge[0] * edge[0] + edge[1] * edge[1]).sqrt();
            if len > 1e-6 {
                axes.push([edge[0] / len, edge[1] / len]);
            }
        }
        i += 3;
    }
    axes
}

fn ensure_min_corner_thickness(
    min_corner: [f32; 2],
    max_corner: [f32; 2],
    min_half: f32,
) -> ([f32; 2], [f32; 2]) {
    let mut min_c = min_corner;
    let mut max_c = max_corner;
    for axis in 0..2 {
        let span = max_c[axis] - min_c[axis];
        if span < 2.0 * min_half {
            let center = (min_c[axis] + max_c[axis]) * 0.5;
            min_c[axis] = center - min_half;
            max_c[axis] = center + min_half;
        }
    }
    (min_c, max_c)
}

/// Minimum-area enclosing rectangle for a 2D point set.
///
/// Tries convex-hull edges, mesh tangent edges, and the PCA principal axis so elongated and
/// non-convex silhouettes align with their dominant in-plane direction.
fn min_area_rect_for_points(
    points: &[[f32; 2]],
    extra_axes: &[[f32; 2]],
) -> ([f32; 2], [f32; 2], [f32; 2], [f32; 2]) {
    if points.is_empty() {
        return ([1.0, 0.0], [0.0, 1.0], [-0.5, -0.5], [0.5, 0.5]);
    }
    if points.len() == 1 {
        return ([1.0, 0.0], [0.0, 1.0], [-0.5, -0.5], [0.5, 0.5]);
    }
    let hull = convex_hull_2d(points);
    let mut candidates: Vec<[f32; 2]> = Vec::new();
    let edge_source = if hull.len() >= 2 { &hull } else { points };
    for i in 0..edge_source.len() {
        let a = edge_source[i];
        let b = edge_source[(i + 1) % edge_source.len()];
        let edge = [b[0] - a[0], b[1] - a[1]];
        let len = (edge[0] * edge[0] + edge[1] * edge[1]).sqrt();
        if len > 1e-6 {
            candidates.push([edge[0] / len, edge[1] / len]);
        }
    }
    candidates.extend_from_slice(extra_axes);
    if let Some(pca) = principal_axis_2d(points) {
        candidates.push(pca);
    }
    if candidates.is_empty() {
        candidates.push([1.0, 0.0]);
    }
    let mut best_area = f32::MAX;
    let mut best = ([1.0, 0.0], [0.0, 1.0], [-0.5, -0.5], [0.5, 0.5]);
    for axis0 in candidates {
        let axis1 = [-axis0[1], axis0[0]];
        let mut min0 = f32::MAX;
        let mut max0 = f32::MIN;
        let mut min1 = f32::MAX;
        let mut max1 = f32::MIN;
        for p in points {
            let s0 = p[0] * axis0[0] + p[1] * axis0[1];
            let s1 = p[0] * axis1[0] + p[1] * axis1[1];
            min0 = min0.min(s0);
            max0 = max0.max(s0);
            min1 = min1.min(s1);
            max1 = max1.max(s1);
        }
        let area = (max0 - min0).max(1e-4) * (max1 - min1).max(1e-4);
        if area < best_area {
            best_area = area;
            best = (axis0, axis1, [min0, min1], [max0, max1]);
        }
    }
    let (axis0, axis1, min_s, max_s) = best;
    let center_s = [(min_s[0] + max_s[0]) * 0.5, (min_s[1] + max_s[1]) * 0.5];
    let min_corner = [min_s[0] - center_s[0], min_s[1] - center_s[1]];
    let max_corner = [max_s[0] - center_s[0], max_s[1] - center_s[1]];
    (axis0, axis1, min_corner, max_corner)
}

fn polygon_area_2d(poly: &[[f32; 2]]) -> f32 {
    if poly.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0f32;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        area += a[0] * b[1] - b[0] * a[1];
    }
    (area * 0.5).abs()
}

fn point_in_rect_2d(p: [f32; 2], min_c: [f32; 2], max_c: [f32; 2]) -> bool {
    p[0] >= min_c[0] - 1e-5
        && p[0] <= max_c[0] + 1e-5
        && p[1] >= min_c[1] - 1e-5
        && p[1] <= max_c[1] + 1e-5
}

fn rect_area_2d(min_c: [f32; 2], max_c: [f32; 2]) -> f32 {
    (max_c[0] - min_c[0]).max(0.0) * (max_c[1] - min_c[1]).max(0.0)
}

fn covers_all_points(points: &[[f32; 2]], rects: &[([f32; 2], [f32; 2])]) -> bool {
    points.iter().all(|p| rects.iter().any(|(min_c, max_c)| point_in_rect_2d(*p, *min_c, *max_c)))
}

fn mesh_boundary_polygon_2d(points: &[[f32; 2]], indices: &[u16]) -> Vec<[f32; 2]> {
    use std::collections::HashMap;
    let mut edge_count: HashMap<(u16, u16), u32> = HashMap::new();
    let mut i = 0;
    while i + 2 < indices.len() {
        let tri = [indices[i], indices[i + 1], indices[i + 2]];
        for j in 0..3 {
            let a = tri[j];
            let b = tri[(j + 1) % 3];
            let key = if a <= b { (a, b) } else { (b, a) };
            *edge_count.entry(key).or_insert(0) += 1;
        }
        i += 3;
    }
    let boundary: Vec<(u16, u16)> = edge_count
        .into_iter()
        .filter(|(_, count)| *count == 1)
        .map(|(e, _)| e)
        .collect();
    if boundary.is_empty() {
        return convex_hull_2d(points);
    }
    let boundary_len = boundary.len();
    let mut adj: HashMap<u16, Vec<u16>> = HashMap::new();
    for (a, b) in boundary {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    for nbrs in adj.values_mut() {
        nbrs.sort_unstable();
    }
    let start = *adj.keys().min().unwrap_or(&0);
    let mut poly = Vec::new();
    let mut current = start;
    let mut prev = start;
    loop {
        let p = points.get(current as usize).copied().unwrap_or([0.0, 0.0]);
        poly.push(p);
        let Some(neighbors) = adj.get(&current) else {
            break;
        };
        let next = if neighbors.len() == 1 {
            neighbors[0]
        } else {
            neighbors
                .iter()
                .copied()
                .find(|n| *n != prev)
                .unwrap_or(neighbors[0])
        };
        if next == start && poly.len() > 2 {
            break;
        }
        if next == prev {
            break;
        }
        prev = current;
        current = next;
        if poly.len() > boundary_len + 2 {
            break;
        }
    }
    if poly.len() >= 3 && !polygon_is_convex(&poly) {
        poly
    } else if poly.len() >= 3 {
        poly
    } else {
        // Last resort: vertex loop in index order (fan meshes) before convex hull fill.
        let mut loop_pts: Vec<[f32; 2]> = indices
            .iter()
            .map(|&idx| points.get(idx as usize).copied().unwrap_or([0.0, 0.0]))
            .collect();
        loop_pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6);
        if loop_pts.len() >= 3 && !polygon_is_convex(&loop_pts) {
            loop_pts
        } else {
            convex_hull_2d(points)
        }
    }
}

fn polygon_is_convex(poly: &[[f32; 2]]) -> bool {
    if poly.len() < 3 {
        return true;
    }
    let mut sign = 0i32;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let c = poly[(i + 2) % n];
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cross.abs() < 1e-6 {
            continue;
        }
        let s = if cross > 0.0 { 1 } else { -1 };
        if sign == 0 {
            sign = s;
        } else if sign != s {
            return false;
        }
    }
    true
}

fn reflex_vertices_ccw(poly: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if poly.len() < 4 {
        return Vec::new();
    }
    let area = polygon_area_2d(poly);
    let ccw = area >= 0.0;
    let n = poly.len();
    let mut out = Vec::new();
    for i in 0..n {
        let a = poly[(i + n - 1) % n];
        let b = poly[i];
        let c = poly[(i + 1) % n];
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        let is_reflex = if ccw { cross < -1e-6 } else { cross > 1e-6 };
        if is_reflex {
            out.push(b);
        }
    }
    out
}

fn try_notch_split_quads(
    points: &[[f32; 2]],
    reflex: [f32; 2],
) -> Option<Vec<([f32; 2], [f32; 2])>> {
    let min_x = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
    let min_y = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    let candidates = [
        vec![
            ([min_x, min_y], [max_x, reflex[1]]),
            ([min_x, reflex[1]], [reflex[0], max_y]),
        ],
        vec![
            ([min_x, min_y], [reflex[0], max_y]),
            ([reflex[0], min_y], [max_x, max_y]),
        ],
        vec![
            ([min_x, reflex[1]], [max_x, max_y]),
            ([min_x, min_y], [reflex[0], reflex[1]]),
        ],
        vec![
            ([reflex[0], min_y], [max_x, max_y]),
            ([min_x, min_y], [reflex[0], max_y]),
        ],
    ];
    let mut best: Option<(f32, Vec<([f32; 2], [f32; 2])>)> = None;
    for rects in candidates {
        if !covers_all_points(points, &rects) {
            continue;
        }
        let area: f32 = rects.iter().map(|(a, b)| rect_area_2d(*a, *b)).sum();
        if best.as_ref().map_or(true, |b| area < b.0) {
            best = Some((area, rects));
        }
    }
    best.map(|(_, rects)| rects)
}

const MESH_SILHOUETTE_MAX_QUADS: usize = 4;
const MESH_SILHOUETTE_SPLIT_RATIO: f32 = 1.15;

/// Best-effort 1–4 billboard quads covering a mesh silhouette in its tangent plane.
///
/// Convex outlines use one minimum-area rectangle. Non-convex outlines try notch splits at
/// reflex vertices (L/T/U shapes) before falling back to the single enclosing rect.
pub fn mesh_silhouette_quads(
    vertices: &[MeshVertex],
    indices: &[u16],
) -> (Vec<([f32; 2], [f32; 2])>, (Vec3, Vec3)) {
    let (min_c, max_c, basis) = mesh_corner_half_extents_single(vertices, indices);
    if vertices.is_empty() {
        return (vec![(min_c, max_c)], basis);
    }
    let (right, up) = basis;
    let points: Vec<[f32; 2]> = vertices
        .iter()
        .map(|v| {
            let p = Vec3::from_array(v.position);
            [p.dot(right), p.dot(up)]
        })
        .collect();
    let poly = mesh_boundary_polygon_2d(&points, indices);
    let mesh_area = polygon_area_2d(&poly).max(polygon_area_2d(&points));
    let single_area = rect_area_2d(min_c, max_c);
    if mesh_area < 1e-6
        || polygon_is_convex(&poly)
        || single_area <= mesh_area * MESH_SILHOUETTE_SPLIT_RATIO
    {
        return (vec![(min_c, max_c)], basis);
    }
    let reflex = reflex_vertices_ccw(&poly);
    let mut best_split: Option<(f32, Vec<([f32; 2], [f32; 2])>)> = None;
    for rv in reflex {
        if let Some(mut rects) = try_notch_split_quads(&points, rv) {
            if rects.len() > MESH_SILHOUETTE_MAX_QUADS {
                continue;
            }
            rects.truncate(MESH_SILHOUETTE_MAX_QUADS);
            let area: f32 = rects.iter().map(|(a, b)| rect_area_2d(*a, *b)).sum();
            if best_split.as_ref().map_or(true, |b| area < b.0) {
                best_split = Some((area, rects));
            }
        }
    }
    if let Some((split_area, mut rects)) = best_split {
        if split_area < single_area * 0.98 {
            for (min_corner, max_corner) in &mut rects {
                let (mc, mx) = ensure_min_corner_thickness(
                    *min_corner,
                    *max_corner,
                    MESH_CORNER_MIN_HALF_THICKNESS,
                );
                *min_corner = mc;
                *max_corner = mx;
            }
            return (rects, basis);
        }
    }
    (vec![(min_c, max_c)], basis)
}

fn mesh_corner_half_extents_single(
    vertices: &[MeshVertex],
    indices: &[u16],
) -> ([f32; 2], [f32; 2], (Vec3, Vec3)) {
    let basis = mesh_basis(vertices, indices);
    let (right, up) = basis;
    if vertices.is_empty() {
        return ([-0.5, -0.5], [0.5, 0.5], basis);
    }
    let points: Vec<[f32; 2]> = vertices
        .iter()
        .map(|v| {
            let p = Vec3::from_array(v.position);
            [p.dot(right), p.dot(up)]
        })
        .collect();
    let edge_axes = mesh_tangent_edge_axes(vertices, indices, right, up);
    let (axis0, axis1, min_corner, max_corner) = min_area_rect_for_points(&points, &edge_axes);
    let (min_corner, max_corner) = ensure_min_corner_thickness(
        min_corner,
        max_corner,
        MESH_CORNER_MIN_HALF_THICKNESS,
    );
    let new_right = (right * axis0[0] + up * axis0[1]).normalize_or_zero();
    let new_up = (right * axis1[0] + up * axis1[1]).normalize_or_zero();
    let final_right = if new_right.length_squared() > 1e-8 {
        new_right
    } else {
        right
    };
    let final_up = if new_up.length_squared() > 1e-8 {
        new_up
    } else {
        up
    };
    (min_corner, max_corner, (final_right, final_up))
}

/// Project mesh vertices onto a surface-aligned basis; returns mesh-local corner half-extents.
///
/// Primitive billboard mode (`BillboardType::Primitive`) maps each particle to **one axis-aligned
/// quad** in the mesh tangent plane. Corners are stretched along the minimum-area rectangle axes
/// (convex-hull edges, mesh tangent edges, and the in-plane PCA principal axis) so elongated
/// meshes fill the quad along their dominant direction.
///
/// Returns the minimum-area enclosing rectangle (first quad of [`mesh_silhouette_quads`] when
/// only one quad is needed).
pub fn mesh_corner_half_extents(
    vertices: &[MeshVertex],
    indices: &[u16],
) -> ([f32; 2], [f32; 2], (Vec3, Vec3)) {
    mesh_corner_half_extents_single(vertices, indices)
}

/// Project PRMA vertices onto a mesh-local basis; returns mesh-faithful corner seeds.
///
/// See [`mesh_silhouette_quads`] for multi-quad concave silhouettes (up to four quads).
pub fn primitive_corner_half_extents(prim: &PrimitiveData) -> ([f32; 2], [f32; 2], (Vec3, Vec3)) {
    mesh_corner_half_extents(&prim.vertices, &prim.indices)
}

/// Multi-quad silhouette for primitive data (see [`mesh_silhouette_quads`]).
pub fn primitive_silhouette_quads(prim: &PrimitiveData) -> (Vec<([f32; 2], [f32; 2])>, (Vec3, Vec3)) {
    mesh_silhouette_quads(&prim.vertices, &prim.indices)
}

/// One axis-aligned billboard quad per mesh triangle (primitive mode).
///
/// Each quad covers the triangle's projected AABB in the mesh tangent plane. More accurate than
/// [`mesh_silhouette_quads`] for sparse meshes; can emit many quads on dense geometry.
pub fn mesh_per_triangle_quads(
    vertices: &[MeshVertex],
    indices: &[u16],
) -> (Vec<([f32; 2], [f32; 2])>, (Vec3, Vec3)) {
    let basis = mesh_basis(vertices, indices);
    let (right, up) = basis;
    if vertices.is_empty() || indices.len() < 3 {
        return (vec![([-0.5, -0.5], [0.5, 0.5])], basis);
    }
    let mut quads = Vec::new();
    for chunk in indices.chunks(3) {
        if chunk.len() < 3 {
            break;
        }
        let mut min_c = [f32::MAX; 2];
        let mut max_c = [f32::MIN; 2];
        for &idx in chunk {
            let v = &vertices[idx as usize];
            let p = Vec3::from_array(v.position);
            let u = p.dot(right);
            let v2 = p.dot(up);
            min_c[0] = min_c[0].min(u);
            min_c[1] = min_c[1].min(v2);
            max_c[0] = max_c[0].max(u);
            max_c[1] = max_c[1].max(v2);
        }
        let (min_c, max_c) = ensure_min_corner_thickness(min_c, max_c, MESH_CORNER_MIN_HALF_THICKNESS);
        quads.push((min_c, max_c));
    }
    if quads.is_empty() {
        quads.push(([-0.5, -0.5], [0.5, 0.5]));
    }
    (quads, basis)
}

pub fn primitive_per_triangle_quads(prim: &PrimitiveData) -> (Vec<([f32; 2], [f32; 2])>, (Vec3, Vec3)) {
    mesh_per_triangle_quads(&prim.vertices, &prim.indices)
}

/// Quad set for primitive billboard particles (silhouette or per-triangle).
pub fn primitive_billboard_quads(
    vertices: &[MeshVertex],
    indices: &[u16],
    per_triangle: bool,
) -> (Vec<([f32; 2], [f32; 2])>, (Vec3, Vec3)) {
    if per_triangle {
        mesh_per_triangle_quads(vertices, indices)
    } else {
        mesh_silhouette_quads(vertices, indices)
    }
}

/// Union AABB of silhouette sub-quads in mesh-local corner space.
pub fn silhouette_envelope(rects: &[([f32; 2], [f32; 2])]) -> ([f32; 2], [f32; 2]) {
    let mut env_min = [f32::INFINITY; 2];
    let mut env_max = [f32::NEG_INFINITY; 2];
    for (min_c, max_c) in rects {
        for axis in 0..2 {
            env_min[axis] = env_min[axis].min(min_c[axis]);
            env_max[axis] = env_max[axis].max(max_c[axis]);
        }
    }
    if !env_min[0].is_finite() {
        return ([-0.5, -0.5], [0.5, 0.5]);
    }
    (env_min, env_max)
}

/// Map a unit-quad UV corner into the atlas sub-rect occupied by one silhouette quad.
pub fn silhouette_atlas_uv(
    unit_uv: [f32; 2],
    sub_rect: ([f32; 2], [f32; 2]),
    envelope: ([f32; 2], [f32; 2]),
) -> [f32; 2] {
    let (env_min, env_max) = envelope;
    let (sub_min, sub_max) = sub_rect;
    let ew = (env_max[0] - env_min[0]).max(1e-6);
    let eh = (env_max[1] - env_min[1]).max(1e-6);
    let cx = sub_min[0] + (sub_max[0] - sub_min[0]) * unit_uv[0];
    let cy = sub_min[1] + (sub_max[1] - sub_min[1]) * unit_uv[1];
    [(cx - env_min[0]) / ew, (cy - env_min[1]) / eh]
}

/// Map a mesh-tangent silhouette rect to billboard attr4 size/aspect and center offset.
///
/// Corners uploaded to attr6 stay at ±0.5; mesh half-extents fold into `size` and `aspect`.
pub fn silhouette_billboard_metrics(
    min_c: [f32; 2],
    max_c: [f32; 2],
    particle_size: f32,
    tex_aspect: f32,
    bb: BillboardType,
) -> ([f32; 2], f32, f32) {
    let half_w = (max_c[0] - min_c[0]) * 0.5;
    let half_h = (max_c[1] - min_c[1]) * 0.5;
    let center = [(min_c[0] + max_c[0]) * 0.5, (min_c[1] + max_c[1]) * 0.5];
    let mesh_aspect = if half_h > 1e-6 { half_w / half_h } else { 1.0 };
    let size = particle_size * 2.0 * half_h.max(1e-6);
    let aspect = match bb {
        BillboardType::Stripe | BillboardType::ComplexStripe => 1.0,
        _ => tex_aspect * mesh_aspect,
    };
    (center, size, aspect)
}

/// Velocity-aligned ribbon corner scaling for Stripe / ComplexStripe billboard modes.
///
/// Width (corner X) receives texture aspect; length (corner Y) stays full size. ComplexStripe
/// additionally biases the trailing edge backward when the particle is moving.
pub fn stripe_corner_half_extents(
    bb_type: BillboardType,
    corner: [f32; 2],
    aspect: f32,
    velocity: Vec3,
) -> [f32; 2] {
    match bb_type {
        BillboardType::Stripe => {
            let w = if aspect > 0.0 { 1.0 / aspect } else { 1.0 };
            [corner[0] * w, corner[1]]
        }
        BillboardType::ComplexStripe => {
            let w = if aspect > 0.0 { 1.0 / aspect } else { 1.0 };
            let trail = (velocity.length() * 0.015).clamp(0.0, 0.45);
            let y = if corner[1] < 0.0 {
                corner[1] - trail
            } else {
                corner[1]
            };
            [corner[0] * w, y]
        }
        _ => corner,
    }
}

/// Camera-facing basis for an emitter, using draw-mesh orientation for primitive mode.
pub fn billboard_basis_for_emitter(
    emitter: &EmitterDef,
    cam_right: Vec3,
    cam_up: Vec3,
    view_dir: Vec3,
    batch_velocity: Vec3,
    mesh_ctx: Option<&SpawnMeshContext<'_>>,
    primitives: &[PrimitiveData],
) -> (Vec3, Vec3) {
    if emitter.billboard_type == BillboardType::Primitive {
        if let Some(ctx) = mesh_ctx {
            if let Some(basis) = draw_mesh_basis(ctx, emitter) {
                return basis;
            }
        }
        if let Some(prim) = emitter_primitive(emitter, primitives) {
            return primitive_mesh_basis(prim);
        }
    }
    let velocity = match emitter.billboard_type {
        BillboardType::Stripe | BillboardType::ComplexStripe => {
            if batch_velocity.length_squared() > 1e-6 {
                batch_velocity
            } else {
                emitter.designated_dir
            }
        }
        _ => batch_velocity,
    };
    billboard_basis(
        emitter.billboard_type,
        cam_right,
        cam_up,
        view_dir,
        velocity,
    )
}

fn volume_axes(emitter: &EmitterDef) -> Vec3 {
    emitter.volume_radius * emitter.volume_form_scale
}

/// Context for primitive/BFRES mesh surface spawn sampling.
pub struct SpawnMeshContext<'a> {
    pub primitives: &'a [PrimitiveData],
    pub bfres_models: &'a [BfresModel],
}

impl<'a> SpawnMeshContext<'a> {
    pub fn from_ptcl(ptcl: &'a PtclFile) -> Self {
        Self {
            primitives: &ptcl.primitives,
            bfres_models: &ptcl.bfres_models,
        }
    }
}

fn bfres_model_has_mesh(model: &BfresModel) -> bool {
    model
        .meshes
        .first()
        .map(|m| !m.vertices.is_empty())
        .unwrap_or(false)
}

fn resolve_bfres_index(
    ctx: &SpawnMeshContext<'_>,
    emitter: &EmitterDef,
    role: PrmaMeshRole,
) -> Option<usize> {
    if emitter.mesh_type != 2 {
        return None;
    }
    let direct = emitter.primitive_index as usize;
    if ctx
        .bfres_models
        .get(direct)
        .map(bfres_model_has_mesh)
        .unwrap_or(false)
    {
        return Some(direct);
    }
    let prma_id = emitter_prma_id(emitter, role);
    if prma_id > 0 {
        if let Some(idx) = ctx
            .bfres_models
            .iter()
            .position(|m| m.source_id == prma_id && bfres_model_has_mesh(m))
        {
            return Some(idx);
        }
    }
    None
}

/// Draw mesh for primitive billboard mode (BFRES preferred, PRMA fallback).
pub fn emitter_draw_mesh<'a>(
    ctx: &'a SpawnMeshContext<'a>,
    emitter: &EmitterDef,
) -> Option<(&'a [MeshVertex], &'a [u16])> {
    if let Some(idx) = resolve_bfres_index(ctx, emitter, PrmaMeshRole::Draw) {
        if let Some(mesh) = ctx.bfres_models[idx].meshes.first() {
            if !mesh.vertices.is_empty() {
                return Some((&mesh.vertices, &mesh.indices));
            }
        }
    }
    let idx = resolve_prma_slot(ctx.primitives, emitter_prma_id(emitter, PrmaMeshRole::Draw));
    let prim = ctx.primitives.get(idx)?;
    if prim.vertices.is_empty() {
        return None;
    }
    Some((&prim.vertices, &prim.indices))
}

const SAME_DIVIDE_SPHERE_TABLES: &[&[[f32; 3]]] =
    crate::sphere_volume_tables::SAME_DIVIDE_SPHERE_TABLES;

fn same_divide_sphere_dir(emitter: &EmitterDef, index: usize) -> Option<Vec3> {
    let tbl_idx = emitter.volume_tbl_index as usize;
    let table = SAME_DIVIDE_SPHERE_TABLES.get(tbl_idx)?;
    let entry = table.get(index % table.len())?;
    Some(Vec3::new(entry[0], entry[1], entry[2]))
}

fn same_divide_sphere64_dir(emitter: &EmitterDef, index: usize) -> Vec3 {
    if let Some(table) =
        crate::sphere_volume_tables::same_divide_sphere64_table(emitter.volume_tbl_index64)
    {
        let entry = table.get(index % table.len()).unwrap_or(&table[0]);
        return Vec3::new(entry[0], entry[1], entry[2]);
    }
    // Fallback if index out of nw4f table range.
    let n = (emitter.volume_tbl_index64 as usize + 2).max(2);
    let i = index % n;
    let phi = (1.0 - 2.0 * (i as f32 + 0.5) / n as f32).acos();
    let theta = i as f32 * 2.39996323;
    Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin())
}

fn effective_circle_divide_count(emitter: &EmitterDef, emit_count: usize) -> usize {
    if emitter.num_divide_circle > 0 {
        emitter.num_divide_circle as usize
    } else {
        emit_count.max(1)
    }
}

fn effective_line_divide_count(emitter: &EmitterDef, emit_count: usize) -> usize {
    if emitter.num_divide_line > 0 {
        emitter.num_divide_line as usize
    } else {
        emit_count.max(1)
    }
}

fn circle_divide_index(emitter: &EmitterDef, index: usize, count: usize, seed: usize) -> usize {
    let count = effective_circle_divide_count(emitter, count);
    let mut idx = index % count;
    if emitter.num_divide_circle_random > 0 {
        let rf = rand_factor(seed.wrapping_add(60));
        let jitter = (rf.abs() * emitter.num_divide_circle_random as f32).round() as usize;
        idx = (idx + jitter) % count;
    }
    idx
}

fn line_divide_index(emitter: &EmitterDef, index: usize, count: usize, seed: usize) -> usize {
    let count = effective_line_divide_count(emitter, count);
    let mut idx = index % count;
    if emitter.num_divide_line_random > 0 {
        let rf = rand_factor(seed.wrapping_add(61));
        let jitter = (rf.abs() * emitter.num_divide_line_random as f32).round() as usize;
        idx = (idx + jitter) % count;
    }
    idx
}

fn latitude_inside(emitter: &EmitterDef, dir: Vec3) -> bool {
    if !emitter.is_volume_latitude_enabled || emitter.sweep_latitude <= 0.0 {
        return true;
    }
    let y_cut = emitter.sweep_latitude.cos();
    dir.y > y_cut
}

fn rotate_latitude_basis(emitter: &EmitterDef, dir: Vec3) -> Vec3 {
    if !emitter.is_volume_latitude_enabled || emitter.volume_latitude_dir == 0 {
        return dir;
    }
    // Non-zero VolumeLatitudeDir rotates from default +Y basis (nw::eft::_rotateDirection).
    let basis = match emitter.volume_latitude_dir {
        1 => Vec3::X,
        2 => Vec3::NEG_Y,
        3 => Vec3::NEG_X,
        4 => Vec3::Z,
        5 => Vec3::NEG_Z,
        _ => Vec3::Y,
    };
    if basis == Vec3::Y {
        return dir;
    }
    let q = glam::Quat::from_rotation_arc(Vec3::Y, basis.normalize_or_zero());
    q * dir
}

fn rand_unit(seed: usize) -> f32 {
    (rand_factor(seed) + 1.0) * 0.5
}

fn sweep_sin_cos(emitter: &EmitterDef, ru: impl Fn(usize) -> f32, salt: usize) -> (f32, f32) {
    let theta = if emitter.is_volume_latitude_enabled {
        ru(salt) * std::f32::consts::TAU
    } else {
        match emitter.arc_type {
            ArcType::EquallyDivided | ArcType::Fixed => {
                let start = if emitter.sweep_start_random {
                    ru(salt.wrapping_add(1)) * std::f32::consts::TAU
                } else {
                    emitter.sweep_start
                };
                start
            }
            ArcType::Random | ArcType::Unknown(_) => {
                let width = if emitter.sweep_longitude > 0.0 {
                    emitter.sweep_longitude
                } else {
                    std::f32::consts::TAU
                };
                let start = if emitter.sweep_start_random {
                    ru(salt.wrapping_add(2)) * std::f32::consts::TAU
                } else {
                    emitter.sweep_start
                };
                ru(salt) * width + start
            }
        }
    };
    (theta.sin(), theta.cos())
}

fn sphere_spawn_y(emitter: &EmitterDef, ru: impl Fn(usize) -> f32) -> f32 {
    if emitter.is_volume_latitude_enabled {
        (ru(3) * emitter.sweep_latitude).cos()
    } else {
        ru(3) * 2.0 - 1.0
    }
}

fn circle_same_divide_theta(emitter: &EmitterDef, index: usize, count: usize, seed: usize) -> f32 {
    let count = effective_circle_divide_count(emitter, count);
    let idx = circle_divide_index(emitter, index, count, seed);
    let start = emitter.sweep_start;
    if count <= 1 {
        return start;
    }
    let step = if emitter.sweep_longitude > 0.0 {
        emitter.sweep_longitude / (count - 1) as f32
    } else {
        std::f32::consts::TAU / count as f32
    };
    start + idx as f32 * step
}

fn line_same_divide_t(emitter: &EmitterDef, index: usize, count: usize, seed: usize) -> f32 {
    let count = effective_line_divide_count(emitter, count);
    let idx = line_divide_index(emitter, index, count, seed);
    if count <= 1 {
        0.5
    } else {
        idx as f32 / (count - 1) as f32
    }
}

fn fill_circle_radius(emitter: &EmitterDef, ru: impl Fn(usize) -> f32) -> f32 {
    let inner = 1.0 - emitter.caliber_ratio.clamp(0.0, 1.0);
    let r = ru(2);
    if inner <= 0.0 {
        r.sqrt()
    } else {
        (r + inner * inner * (1.0 - r)).sqrt()
    }
}

/// Collect triangle positions/normals for mesh surface sampling.
fn mesh_triangles(vertices: &[MeshVertex], indices: &[u16]) -> Vec<[Vec3; 3]> {
    let mut tris = Vec::new();
    let push_tri = |tris: &mut Vec<[Vec3; 3]>, a: usize, b: usize, c: usize| {
        if let (Some(va), Some(vb), Some(vc)) = (vertices.get(a), vertices.get(b), vertices.get(c))
        {
            tris.push([
                Vec3::from(va.position),
                Vec3::from(vb.position),
                Vec3::from(vc.position),
            ]);
        }
    };
    let mut i = 0;
    while i + 2 < indices.len() {
        let (a, b, c) = (indices[i] as usize, indices[i + 1] as usize, indices[i + 2] as usize);
        push_tri(&mut tris, a, b, c);
        i += 3;
    }
    tris
}

fn triangle_areas(tris: &[[Vec3; 3]]) -> Vec<f32> {
    tris
        .iter()
        .map(|t| {
            let e1 = t[1] - t[0];
            let e2 = t[2] - t[0];
            e1.cross(e2).length() * 0.5
        })
        .collect()
}

fn sample_triangle_surface(tris: &[[Vec3; 3]], areas: &[f32], seed: usize) -> Vec3 {
    if tris.is_empty() {
        return Vec3::ZERO;
    }
    let total: f32 = areas.iter().sum();
    if total <= 0.0 {
        return tris[0][0];
    }
    let mut pick = rand_factor(seed.wrapping_add(11)).abs() * total;
    let tri_idx = areas
        .iter()
        .position(|&a| {
            if pick <= a {
                true
            } else {
                pick -= a;
                false
            }
        })
        .unwrap_or(tris.len() - 1);
    let tri = tris[tri_idx];
    let r1 = rand_factor(seed.wrapping_add(12)).abs();
    let r2 = rand_factor(seed.wrapping_add(13)).abs();
    let sqrt_r1 = r1.sqrt();
    let u = 1.0 - sqrt_r1;
    let v = sqrt_r1 * (1.0 - r2);
    let w = sqrt_r1 * r2;
    tri[0] * u + tri[1] * v + tri[2] * w
}

fn resolve_spawn_mesh<'a>(
    ctx: &'a SpawnMeshContext<'a>,
    emitter: &EmitterDef,
) -> Option<(&'a [MeshVertex], &'a [u16])> {
    // Prefer BFRES when configured; fall back to PRMA for missing/empty model indices.
    if let Some(idx) = resolve_bfres_index(ctx, emitter, PrmaMeshRole::Spawn) {
        if let Some(mesh) = ctx.bfres_models[idx].meshes.first() {
            if !mesh.vertices.is_empty() {
                return Some((&mesh.vertices, &mesh.indices));
            }
        }
    }
    let prim_idx = resolve_prma_slot(ctx.primitives, emitter_prma_id(emitter, PrmaMeshRole::Spawn));
    let prim = ctx.primitives.get(prim_idx)?;
    if prim.vertices.is_empty() {
        return None;
    }
    Some((&prim.vertices, &prim.indices))
}

/// Sample a point on a primitive/BFRES mesh surface (PrimEmitType semantics).
pub fn sample_primitive_surface_pos(
    ctx: &SpawnMeshContext<'_>,
    emitter: &EmitterDef,
    seed: usize,
    index: usize,
    count: usize,
) -> Vec3 {
    let Some((vertices, indices)) = resolve_spawn_mesh(ctx, emitter) else {
        return Vec3::ZERO;
    };
    match emitter.prim_emit_type {
        0 => {
            let v = &vertices[index % vertices.len()];
            Vec3::from(v.position)
        }
        2 => {
            let v = &vertices[(index + count) % vertices.len()];
            Vec3::from(v.position)
        }
        _ => {
            let tris = mesh_triangles(vertices, indices);
            if tris.is_empty() {
                let v = &vertices[seed % vertices.len()];
                return Vec3::from(v.position);
            }
            let areas = triangle_areas(&tris);
            sample_triangle_surface(&tris, &areas, seed)
        }
    }
}

/// Distance-based emission count and interpolated spawn origins (EmitSameDistance).
pub fn emit_dist_spawn_batch(
    emitter: &EmitterDef,
    inst: &mut EmitterInstance,
    curr_world_pos: Vec3,
) -> Vec<Vec3> {
    if !emitter.is_emit_dist_enabled || emitter.emitter_dist_unit <= 0.0 {
        return Vec::new();
    }
    if !inst.emit_dist_prev_pos_set {
        inst.emit_dist_prev_pos = curr_world_pos;
        inst.emit_dist_prev_pos_set = true;
        return Vec::new();
    }
    let prev = inst.emit_dist_prev_pos;
    let move_len = (prev - curr_world_pos).length();
    let mut virtual_len = move_len;
    if virtual_len < emitter.emitter_dist_marg {
        virtual_len = 0.0;
    }
    if virtual_len == 0.0 {
        virtual_len = emitter.emitter_dist_min;
    } else if virtual_len < emitter.emitter_dist_min {
        virtual_len = emitter.emitter_dist_min;
    } else if emitter.emitter_dist_max > 0.0 && virtual_len > emitter.emitter_dist_max {
        virtual_len = emitter.emitter_dist_max;
    }
    inst.emit_dist_vessel += virtual_len;
    let mut count = (inst.emit_dist_vessel / emitter.emitter_dist_unit).floor() as usize;
    if emitter.emitter_dist_particles_max > 0 {
        count = count.min(emitter.emitter_dist_particles_max as usize);
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        inst.emit_dist_vessel -= emitter.emitter_dist_unit;
        let ratio = if virtual_len > 0.0 {
            (inst.emit_dist_vessel / virtual_len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(curr_world_pos * (1.0 - ratio) + prev * ratio);
    }
    inst.emit_dist_prev_pos = curr_world_pos;
    out
}

/// Per-particle rotation randomizer from EmitterInfo.RotateRand* (range [-1,1] * rand).
pub fn spawn_rotation_rand(emitter: &EmitterDef, seed: usize) -> Vec3 {
    let rf = |salt: usize| rand_factor(seed.wrapping_add(salt));
    Vec3::new(
        rf(51) * emitter.rotate_rand.x,
        rf(52) * emitter.rotate_rand.y,
        rf(53) * emitter.rotate_rand.z,
    )
}

/// Local-space spawn offset inside the emitter volume (before TRS / bone transforms).
pub fn volume_local_spawn_pos(
    emitter: &EmitterDef,
    seed: usize,
    index: usize,
    count: usize,
    mesh_ctx: Option<&SpawnMeshContext<'_>>,
) -> Vec3 {
    let axes = volume_axes(emitter);
    let rf = |salt: usize| rand_factor(seed.wrapping_add(salt));
    let ru = |salt: usize| rand_unit(seed.wrapping_add(salt));
    let count = count.max(1);

    let mut pos = match emitter.emit_type {
        EmitType::Point => Vec3::ZERO,
        EmitType::Circle | EmitType::CircleSameDivide => {
            let theta = if matches!(emitter.emit_type, EmitType::CircleSameDivide) {
                circle_same_divide_theta(emitter, index, count, seed)
            } else {
                let (sin_v, cos_v) = sweep_sin_cos(emitter, ru, 1);
                sin_v.atan2(cos_v)
            };
            Vec3::new(theta.cos() * axes.x, 0.0, theta.sin() * axes.z)
        }
        EmitType::FillCircle => {
            let (sin_v, cos_v) = sweep_sin_cos(emitter, ru, 1);
            let r = fill_circle_radius(emitter, ru);
            Vec3::new(sin_v * axes.x * r, 0.0, cos_v * axes.z * r)
        }
        EmitType::Sphere
        | EmitType::SphereSameDivide
        | EmitType::SphereSameDivide64
        | EmitType::FillSphere => {
            let dir = if matches!(emitter.emit_type, EmitType::SphereSameDivide) {
                same_divide_sphere_dir(emitter, index).unwrap_or_else(|| {
                    let theta = index as f32 * std::f32::consts::TAU / count as f32;
                    Vec3::new(theta.cos(), 0.0, theta.sin())
                })
            } else if matches!(emitter.emit_type, EmitType::SphereSameDivide64) {
                same_divide_sphere64_dir(emitter, index)
            } else {
                let (sin_v, cos_v) = sweep_sin_cos(emitter, ru, 1);
                let y = sphere_spawn_y(emitter, ru);
                let r = (1.0 - y * y).max(0.0).sqrt();
                Vec3::new(r * sin_v, y, r * cos_v)
            };
            let mut dir = if matches!(emitter.emit_type, EmitType::FillSphere) {
                dir * rf(4).abs().cbrt()
            } else {
                dir
            };
            if !latitude_inside(emitter, dir) {
                dir = dir.normalize_or_zero();
            }
            dir = rotate_latitude_basis(emitter, dir);
            dir * axes
        }
        EmitType::Cylinder | EmitType::FillCylinder => {
            let (sin_v, cos_v) = sweep_sin_cos(emitter, ru, 1);
            let y = if matches!(emitter.emit_type, EmitType::FillCylinder) {
                rf(2) * axes.y
            } else {
                axes.y * 0.5
            };
            Vec3::new(sin_v * axes.x, y, cos_v * axes.z)
        }
        EmitType::Box | EmitType::FillBox => {
            if matches!(emitter.emit_type, EmitType::FillBox) {
                Vec3::new(rf(1) * axes.x, rf(2) * axes.y, rf(3) * axes.z)
            } else {
                // Random point on box surface.
                let face = (rf(1).abs() * 6.0).floor() as i32;
                let u = rf(2);
                let v = rf(3);
                match face {
                    0 => Vec3::new(axes.x, u * axes.y, v * axes.z),
                    1 => Vec3::new(-axes.x, u * axes.y, v * axes.z),
                    2 => Vec3::new(u * axes.x, axes.y, v * axes.z),
                    3 => Vec3::new(u * axes.x, -axes.y, v * axes.z),
                    4 => Vec3::new(u * axes.x, v * axes.y, axes.z),
                    _ => Vec3::new(u * axes.x, v * axes.y, -axes.z),
                }
            }
        }
        EmitType::Rectangle => Vec3::new(rf(1) * axes.x, 0.0, rf(2) * axes.z),
        EmitType::Line | EmitType::LineSameDivide => {
            let t = if matches!(emitter.emit_type, EmitType::LineSameDivide) {
                line_same_divide_t(emitter, index, count, seed)
            } else {
                rf(1).abs()
            };
            let half = emitter.line_length * 0.5;
            let z = emitter.line_center + (t - 0.5) * emitter.line_length;
            Vec3::new(0.0, 0.0, z.clamp(emitter.line_center - half, emitter.line_center + half))
        }
        EmitType::Primitive => mesh_ctx
            .map(|ctx| sample_primitive_surface_pos(ctx, emitter, seed, index, count))
            .unwrap_or(Vec3::ZERO),
        EmitType::Unknown(_) => Vec3::ZERO,
    };

    if emitter.volume_surface_pos_rand.abs() > 0.0 {
        pos += Vec3::new(rf(40), rf(41), rf(42)) * emitter.volume_surface_pos_rand;
    }
    pos
}

fn spawn_jitter(emitter: &EmitterDef, seed: usize) -> Vec3 {
    let rf = |salt: usize| rand_factor(seed.wrapping_add(salt));
    let trans_rand = Vec3::new(
        rf(10) * emitter.trans_rand.x,
        rf(11) * emitter.trans_rand.y,
        rf(12) * emitter.trans_rand.z,
    );
    let pos_rand = if emitter.position_random.abs() > 0.0 {
        let dir = Vec3::new(rf(20), rf(21), rf(22)).normalize_or_zero();
        dir * emitter.position_random * rf(23).abs()
    } else {
        Vec3::ZERO
    };
    trans_rand + pos_rand
}

/// World spawn position for one particle.
pub fn compute_particle_spawn_world_pos(
    emitter: &EmitterDef,
    inst: &EmitterInstance,
    bone_mat: Mat4,
    effect_t: f32,
    seed: usize,
    index: usize,
    count: usize,
    mesh_ctx: Option<&SpawnMeshContext<'_>>,
) -> Vec3 {
    let world_mat = compute_emitter_world_mat(emitter, inst, bone_mat, effect_t);
    let local = volume_local_spawn_pos(emitter, seed, index, count, mesh_ctx) + spawn_jitter(emitter, seed);
    world_mat.transform_point3(local)
}

/// Deterministic unit vector on the sphere (Fibonacci-style from seed).
fn rand_unit_vec(seed: usize) -> Vec3 {
    let theta = seed as f32 * 2.39996323;
    let z = 1.0 - 2.0 * ((seed.wrapping_mul(1103515245).wrapping_add(12345) % 10000) as f32 / 10000.0);
    let r = (1.0 - z * z).max(0.0).sqrt();
    Vec3::new(r * theta.cos(), r * theta.sin(), z).normalize_or_zero()
}

/// Sample a direction uniformly within a cone around `axis` (half-angle in radians).
pub fn sample_cone_direction(axis: Vec3, half_angle_rad: f32, seed: usize) -> Vec3 {
    let axis = axis.normalize_or_zero();
    if half_angle_rad <= 0.0 || axis.length_squared() < 1e-8 {
        return axis;
    }
    let u = (rand_factor(seed.wrapping_add(1)) + 1.0) * 0.5;
    let cos_max = half_angle_rad.cos();
    let cos_theta = cos_max + u * (1.0 - cos_max);
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let phi = rand_factor(seed.wrapping_add(2)) * std::f32::consts::TAU;
    let up = if axis.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
    let tangent = axis.cross(up).normalize_or_zero();
    let bitangent = tangent.cross(axis);
    (axis * cos_theta
        + tangent * (sin_theta * phi.cos())
        + bitangent * (sin_theta * phi.sin()))
    .normalize_or_zero()
}

/// Apply cone and per-axis diffusion around a base emit direction.
pub fn apply_velocity_diffusion(
    base_dir: Vec3,
    seed: usize,
    dir_angle_deg: f32,
    axis_spread: Vec3,
) -> Vec3 {
    let mut dir = if dir_angle_deg.abs() > 0.001 {
        sample_cone_direction(base_dir, dir_angle_deg.to_radians(), seed)
    } else {
        base_dir.normalize_or_zero()
    };
    if axis_spread.length_squared() > 0.0 {
        let r = rand_unit_vec(seed.wrapping_add(99));
        dir = (
            dir + Vec3::new(
                r.x * axis_spread.x,
                r.y * axis_spread.y,
                r.z * axis_spread.z,
            )
        )
        .normalize_or_zero();
    }
    dir
}

/// Add spawn XZ contribution to the velocity direction (ParticleVelocity.XZDiffusion).
pub fn apply_xz_diffusion(dir: Vec3, local_spawn: Vec3, xz_diffusion: f32) -> Vec3 {
    if xz_diffusion.abs() <= 0.001 {
        return dir;
    }
    let xz = Vec3::new(local_spawn.x, 0.0, local_spawn.z);
    if xz.length_squared() < 1e-8 {
        return dir;
    }
    (dir + xz.normalize() * xz_diffusion).normalize_or_zero()
}

fn emit_velocity_base_direction(
    emitter: &EmitterDef,
    seed: usize,
    index: usize,
    count: usize,
) -> Vec3 {
    if !emitter.use_omnidirectional {
        let dir = emitter.designated_dir.normalize_or_zero();
        if dir.length_squared() > 0.0 {
            return dir;
        }
    }
    let count = count.max(1);
    match emitter.emit_type {
        EmitType::Sphere
        | EmitType::SphereSameDivide
        | EmitType::SphereSameDivide64
        | EmitType::FillSphere => {
            let theta = seed as f32 * 2.399;
            let phi = (1.0 - 2.0 * ((seed as f32 + 0.5) / count as f32)).acos();
            Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
        }
        EmitType::Point => {
            let theta = seed as f32 * 2.399;
            let phi = (1.0 - ((index as f32 + 0.5) / count as f32)).acos();
            Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
        }
        EmitType::Circle | EmitType::CircleSameDivide | EmitType::FillCircle => {
            let theta = index as f32 * std::f32::consts::TAU / count as f32;
            Vec3::new(theta.cos(), 0.0, theta.sin())
        }
        EmitType::Cylinder | EmitType::FillCylinder => {
            let theta = index as f32 * std::f32::consts::TAU / count as f32;
            let y = (seed as f32 * 0.37).sin() * 0.5;
            Vec3::new(theta.cos(), y, theta.sin()).normalize()
        }
        EmitType::Box | EmitType::FillBox => {
            let rx = (seed as f32 * 0.13).sin() * 2.0 - 1.0;
            let ry = (seed as f32 * 0.17).sin() * 2.0 - 1.0;
            let rz = (seed as f32 * 0.19).sin() * 2.0 - 1.0;
            Vec3::new(rx, ry, rz).normalize_or_zero()
        }
        EmitType::Rectangle => {
            let rx = (seed as f32 * 0.13).sin() * 2.0 - 1.0;
            let rz = (seed as f32 * 0.19).sin() * 2.0 - 1.0;
            Vec3::new(rx, 0.0, rz).normalize_or_zero()
        }
        EmitType::Line | EmitType::LineSameDivide => Vec3::new(0.0, 0.0, 1.0),
        EmitType::Primitive => {
            Vec3::new(
                (seed as f32 * 0.11).sin() * 0.5,
                (seed as f32 * 0.13).sin() * 0.5,
                1.0,
            )
            .normalize_or_zero()
        }
        EmitType::Unknown(_) => {
            let theta = seed as f32 * 2.399;
            let phi = (1.0 - 2.0 * ((seed as f32 + 0.5) / count as f32)).acos();
            Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
        }
    }
}

/// Full emit direction including diffusion; transformed by emitter rotation unless world-oriented.
pub fn emit_velocity_direction(
    emitter: &EmitterDef,
    seed: usize,
    index: usize,
    count: usize,
    emitter_rot_mat: Mat4,
    local_spawn: Vec3,
) -> Vec3 {
    let base = emit_velocity_base_direction(emitter, seed, index, count);
    let mut dir = apply_velocity_diffusion(
        base,
        seed,
        emitter.diffusion_dir_angle,
        emitter.diffusion_axis,
    );
    dir = apply_xz_diffusion(dir, local_spawn, emitter.xz_diffusion);
    if emitter.is_world_oriented_velocity {
        dir
    } else {
        emitter_rot_mat.transform_vector3(dir)
    }
}

/// Compute final particle velocity vector at spawn.
pub fn compute_particle_velocity(
    emitter: &EmitterDef,
    seed: usize,
    index: usize,
    count: usize,
    emitter_rot_mat: Mat4,
    local_spawn: Vec3,
    emitter_motion_velocity: Vec3,
) -> Vec3 {
    let dir = emit_velocity_direction(emitter, seed, index, count, emitter_rot_mat, local_spawn);
    let speed = emitter.initial_speed
        * (1.0 + (seed as f32 * 0.37).sin() * emitter.speed_random.min(0.5));
    let mut velocity = dir * speed;
    if emitter.em_vel_inherit.abs() > 0.0 {
        velocity += emitter_motion_velocity * emitter.em_vel_inherit;
    }
    velocity
}

/// Apply child inheritance flags to spawn size / rotation / velocity / channel multipliers.
pub fn apply_child_inheritance(
    inh: &ChildInheritanceDef,
    parent: &Particle,
    mut size: f32,
    mut rotation: f32,
    mut velocity: Vec3,
) -> (f32, f32, Vec3, Option<ParticleInheritState>) {
    if inh.inherit_velocity {
        velocity += parent.velocity * inh.velocity_rate;
    }
    if inh.inherit_scale {
        size = (parent.size * inh.scale_rate).max(0.01);
    }
    if inh.inherit_rotate {
        rotation = parent.rotation;
    }

    let has_channel_inherit = inh.inherit_color0
        || inh.inherit_color1
        || inh.inherit_alpha0
        || inh.inherit_alpha1
        || inh.inherit_color_scale
        || inh.inherit_alpha0_each_frame
        || inh.inherit_alpha1_each_frame
        || inh.inherit_draw_path
        || inh.inherit_pre_draw;

    let inherit = if has_channel_inherit {
        Some(ParticleInheritState {
            color0_mul: if inh.inherit_color0 {
                parent.color0_rgb
            } else {
                [1.0, 1.0, 1.0]
            },
            color1_mul: if inh.inherit_color1 {
                parent.color1_rgb
            } else {
                [1.0, 1.0, 1.0]
            },
            alpha0_mul: if inh.inherit_alpha0 {
                parent.alpha0_live
            } else {
                1.0
            },
            alpha1_mul: if inh.inherit_alpha1 {
                parent.alpha1_live
            } else {
                1.0
            },
            color_scale: if inh.inherit_color_scale {
                parent.color_scale_live
            } else {
                1.0
            },
            alpha0_each_frame: inh.inherit_alpha0_each_frame,
            alpha1_each_frame: inh.inherit_alpha1_each_frame,
            parent_seed: parent.seed,
            parent_set_idx: parent.emitter_set_idx,
            parent_emitter_idx: parent.emitter_idx,
            draw_path: if inh.inherit_draw_path {
                Some(parent.draw_path)
            } else {
                None
            },
            pre_draw: inh.inherit_pre_draw,
        })
    } else {
        None
    };

    (size, rotation, velocity, inherit)
}

/// Within one `draw_path` + emitter set, order pre_draw child batches immediately before their parent.
pub fn particle_hierarchy_order_key(p: &Particle) -> (usize, u8, usize) {
    let idx = p.emitter_idx;
    if p.pre_draw {
        let parent = p
            .parent_emitter_idx
            .or_else(|| p.inherit.as_ref().map(|i| i.parent_emitter_idx));
        if let Some(parent) = parent {
            (parent, 0, idx)
        } else {
            // Ungrouped pre_draw: before all emitters in this set.
            (0, 0, idx)
        }
    } else {
        (idx, 1, idx)
    }
}

/// GPU draw / batch ordering: lower `draw_path` first, then hierarchy order within each set.
pub fn particle_draw_sort_key(p: &Particle) -> (u32, usize, usize, u8, usize) {
    let (anchor, tier, idx) = particle_hierarchy_order_key(p);
    (p.draw_path, p.emitter_set_idx, anchor, tier, idx)
}

/// Batch grouping key for particles sharing the same emitter shader/textures.
pub fn particle_batch_key(p: &Particle) -> (u32, bool, usize, usize) {
    (
        p.draw_path,
        p.pre_draw,
        p.emitter_set_idx,
        p.emitter_idx,
    )
}

/// Ascending distinct `draw_path` ids present in `particles`.
pub fn distinct_particle_draw_paths(particles: &[Particle]) -> Vec<u32> {
    let mut paths: Vec<u32> = particles.iter().map(|p| p.draw_path).collect();
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Ascending draw_path ids for multi-pass compositing (particles + sword trails).
pub fn distinct_draw_paths(particles: &[Particle], trails: &[SwordTrail]) -> Vec<u32> {
    let mut paths = distinct_particle_draw_paths(particles);
    for trail in trails {
        if !paths.contains(&trail.draw_path) {
            paths.push(trail.draw_path);
        }
    }
    paths.sort_unstable();
    paths
}

/// Clip-space depth proxy for transparent particle ordering within one batch.
pub fn particle_clip_depth(view_proj: Mat4, p: &Particle) -> f32 {
    let clip = view_proj * p.position.extend(1.0);
    clip.z / clip.w.max(1e-6)
}

/// Ordered batch keys matching [`crate::particle_renderer::ParticleRenderer::prepare_particle_frame`].
pub fn ordered_particle_batch_keys(particles: &[Particle]) -> Vec<(u32, bool, usize, usize)> {
    ordered_particle_batch_keys_filtered(particles, None)
}

/// Batch keys for one draw path (or all paths when `draw_path` is `None`).
pub fn ordered_particle_batch_keys_filtered(
    particles: &[Particle],
    draw_path: Option<u32>,
) -> Vec<(u32, bool, usize, usize)> {
    let mut sorted: Vec<&Particle> = particles
        .iter()
        .filter(|p| draw_path.map_or(true, |d| p.draw_path == d))
        .collect();
    sorted.sort_by_key(|p| particle_draw_sort_key(p));
    let mut keys = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let key = particle_batch_key(sorted[i]);
        while i < sorted.len() && particle_batch_key(sorted[i]) == key {
            i += 1;
        }
        keys.push(key);
    }
    keys
}

/// Per-path batch key lists in ascending draw_path order (for multi-pass rendering tests).
pub fn ordered_particle_batch_keys_by_draw_path(
    particles: &[Particle],
) -> Vec<(u32, Vec<(u32, bool, usize, usize)>)> {
    distinct_particle_draw_paths(particles)
        .into_iter()
        .map(|path| {
            (
                path,
                ordered_particle_batch_keys_filtered(particles, Some(path)),
            )
        })
        .collect()
}

/// Apply per-channel inheritance multipliers before combiner evaluation.
///
/// When `use_live_parent_alpha` is false, each-frame alpha inheritance is deferred (multiplier 1.0)
/// so a second pass can apply the parent's same-frame alpha after integration.
pub fn apply_inherit_channels(
    mut c0: [f32; 4],
    mut c1: [f32; 4],
    mut a0: f32,
    mut a1: f32,
    inherit: Option<&ParticleInheritState>,
    parent_alphas: Option<&HashMap<(u64, usize, usize), (f32, f32)>>,
    use_live_parent_alpha: bool,
) -> ([f32; 4], [f32; 4], f32, f32) {
    let Some(inh) = inherit else {
        return (c0, c1, a0, a1);
    };
    for i in 0..3 {
        c0[i] *= inh.color0_mul[i];
        c1[i] *= inh.color1_mul[i];
    }
    if inh.color_scale != 1.0 {
        for i in 0..3 {
            c0[i] *= inh.color_scale;
            c1[i] *= inh.color_scale;
        }
    }
    let parent_key = (inh.parent_seed, inh.parent_set_idx, inh.parent_emitter_idx);
    let parent_live = parent_alphas.and_then(|m| m.get(&parent_key).copied());
    let a0_mul = if inh.alpha0_each_frame {
        if use_live_parent_alpha {
            parent_live.map(|(a0, _)| a0).unwrap_or(1.0)
        } else {
            1.0
        }
    } else {
        inh.alpha0_mul
    };
    let a1_mul = if inh.alpha1_each_frame {
        if use_live_parent_alpha {
            parent_live.map(|(_, a1)| a1).unwrap_or(1.0)
        } else {
            1.0
        }
    } else {
        inh.alpha1_mul
    };
    a0 *= a0_mul;
    a1 *= a1_mul;
    (c0, c1, a0, a1)
}

/// Re-sample emitter colour/alpha tracks and apply inheritance for one particle.
pub fn update_particle_color_channels(
    p: &mut Particle,
    emitter: &EmitterDef,
    parent_alpha_lookup: Option<&HashMap<(u64, usize, usize), (f32, f32)>>,
    use_live_parent_alpha: bool,
) {
    let t = (p.age / emitter.lifetime).clamp(0.0, 1.0);
    let c0 = sample_color_or_white(&emitter.color0, t);
    let c1 = if !emitter.color1.is_empty() {
        sample_color_or_white(&emitter.color1, t)
    } else {
        Vec4::ONE
    };
    let a0 = if !emitter.alpha0_keys.is_empty() {
        sample_alpha(&emitter.alpha0_keys, t)
    } else {
        emitter.alpha0.sample(t)
    };
    let a1 = if !emitter.alpha1_keys.is_empty() {
        sample_alpha(&emitter.alpha1_keys, t)
    } else {
        emitter.alpha1.sample(t)
    };
    let (c0_arr, c1_arr, a0_live, a1_live) = apply_inherit_channels(
        [c0.x, c0.y, c0.z, c0.w],
        [c1.x, c1.y, c1.z, c1.w],
        a0,
        a1,
        p.inherit.as_ref(),
        parent_alpha_lookup,
        use_live_parent_alpha,
    );
    p.color0_rgb = [c0_arr[0], c0_arr[1], c0_arr[2]];
    p.color1_rgb = [c1_arr[0], c1_arr[1], c1_arr[2]];
    p.alpha0_live = a0_live;
    p.alpha1_live = a1_live;
    p.color_scale_live = emitter.color_scale;
    p.color = combine_particle_channels(c0_arr, c1_arr, a0_live, a1_live, &emitter.combiner);
}

/// Combine sampled combiner channels into final particle RGBA.
pub fn combine_particle_channels(
    c0: [f32; 4],
    c1: [f32; 4],
    a0: f32,
    a1: f32,
    combiner: &crate::shader_registry::CombinerState,
) -> Vec4 {
    let combined = crate::combiner::combine_particle_rgba(c0, c1, a0, a1, combiner);
    Vec4::new(combined[0], combined[1], combined[2], combined[3])
}

/// Child emitters in `set` that spawn when `parent_emitter_idx` particles die.
pub fn child_emitters_for_parent<'a>(
    set: &'a EmitterSet,
    parent_emitter_idx: usize,
) -> impl Iterator<Item = (usize, &'a EmitterDef)> {
    set.emitters.iter().enumerate().filter(move |(_, e)| {
        e.child_inheritance.spawn_from_parent_particle
            && e.child_inheritance.parent_emitter_idx as usize == parent_emitter_idx
    })
}

/// Normalized effect-local time for emitter animation tracks.
pub fn emitter_effect_t(emitter: &EmitterDef, local_frame: f32) -> f32 {
    let dur = emitter.emission_duration.max(1) as f32;
    let t0 = emitter.emission_start as f32;
    ((local_frame - t0) / dur).clamp(0.0, 1.0)
}

/// Texture resource parsed from the emitter data block.
#[derive(Debug, Clone)]
pub struct TextureRes {
    /// BNTX texture name from the _STR block (e.g. "ef_cmn_bomb_indirect00").
    /// Empty string if no name was available during parsing.
    pub tex_name: String,
    pub width: u16,
    pub height: u16,
    pub ftx_format: u32,
    pub ftx_data_offset: u32,
    pub ftx_data_size: u32,
    pub original_format: u32,
    pub original_data_offset: u32,
    pub original_data_size: u32,
    pub wrap_mode: u8,
    pub filter_mode: u8,
    pub mipmap_count: u32,
    /// BNTX compSel packed u32: each byte is a channel source (2=R,3=G,4=B,5=A).
    /// Used to detect BGRA channel ordering. 0 = not set / unknown.
    pub channel_swizzle: u32,
}

/// A single vertex in a primitive mesh.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub normal: [f32; 3],
}

/// Primitive mesh geometry data parsed from the VFXB file.
#[derive(Debug, Clone)]
pub struct PrimitiveData {
    /// PRMA descriptor id (from PRIM binary / descriptor table).
    pub id: u64,
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u16>,
}

/// One sub-mesh extracted from a G3PR BFRES model.
#[derive(Debug, Clone, Default)]
pub struct BfresMesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u16>,
    /// Index into PtclFile::bntx_textures for the color (_col) texture.
    /// u32::MAX means "not found / use emitter fallback".
    pub texture_index: u32,
    /// Index into PtclFile::bntx_textures for the emissive (_emi) texture.
    /// u32::MAX means absent.
    pub emissive_tex_index: u32,
    /// Index into PtclFile::bntx_textures for the PBR params (_prm) texture.
    /// u32::MAX means absent.
    pub prm_tex_index: u32,
}

/// Parsed G3PR BFRES model — one entry per FMDL in the embedded BFRES file.
#[derive(Debug, Clone, Default)]
pub struct BfresModel {
    pub name: String,
    /// PRMA id from dump `{id}.bfres` filename (0 when unknown).
    pub source_id: u64,
    pub meshes: Vec<BfresMesh>,
}

/// Parsed .ptcl file.
#[derive(Debug, Default, Clone)]
pub struct PtclFile {
    pub emitter_sets: Vec<EmitterSet>,
    /// Raw texture section bytes (for GPU upload)
    pub texture_section: Vec<u8>,
    #[allow(dead_code)]
    pub texture_section_offset: usize,
    /// BNTX textures extracted from the VFXB file
    pub bntx_textures: Vec<TextureRes>,
    /// Primitive mesh geometry data (from PRMA section)
    pub primitives: Vec<PrimitiveData>,
    /// G3PR BFRES models (one per FMDL in the embedded BFRES)
    pub bfres_models: Vec<BfresModel>,
    /// All unique embedded Shader.bnsh binaries from the dump.
    pub shader_registry: crate::shader_registry::ShaderRegistry,
    /// Legacy: first unique BNSH (compat — prefer shader_registry).
    pub shader_binary_1: Vec<u8>,
    /// Legacy: second unique BNSH (compat — prefer shader_registry).
    pub shader_binary_2: Vec<u8>,
}

impl PtclFile {
    /// True when embedded BFRES meshes carry material textures (_col/_emi/_prm).
    /// Particle billboards use emitter BNTX slots instead; skip the mesh material pass otherwise.
    pub fn needs_mesh_material_pass(&self) -> bool {
        self.bfres_models.iter().any(|model| {
            model.meshes.iter().any(|mesh| {
                mesh.texture_index != u32::MAX
                    || mesh.emissive_tex_index != u32::MAX
                    || mesh.prm_tex_index != u32::MAX
            })
        })
    }
}

/// Returns (r, g, b, blend_type, scale, lifetime) defaults based on effect name keywords.
/// Used to give synthetic/fallback emitters visually appropriate colors.
/// Scale values are in world units where a typical character is ~25 units tall.
pub fn name_hint_defaults(name: &str) -> (f32, f32, f32, BlendType, f32, f32) {
    let n = name.to_lowercase();
    if n.contains("fire") || n.contains("flame") || n.contains("burn") || n.contains("heat") {
        (1.0, 0.4, 0.05, BlendType::Add, 15.0, 15.0)
    } else if n.contains("electric") || n.contains("thunder") || n.contains("spark")
           || n.contains("elec") || n.contains("volt") || n.contains("lightning") {
        (1.0, 1.0, 0.3, BlendType::Add, 10.0, 8.0)
    } else if n.contains("ice") || n.contains("freeze") || n.contains("frost") || n.contains("cold") {
        (0.4, 0.8, 1.0, BlendType::Normal, 12.0, 20.0)
    } else if n.contains("smoke") || n.contains("dust") || n.contains("cloud") {
        (0.6, 0.6, 0.6, BlendType::Normal, 20.0, 25.0)
    } else {
        (1.0, 1.0, 1.0, BlendType::Add, 10.0, 12.0)
    }
}

// ── BNTX parsing ──────────────────────────────────────────────────────────────
// Hand-rolled parser for embedded BNTX (the bntx crate expects standalone files;
// embedded BNTX inside VFXB/GRTF sections have absolute pointer offsets that
// don't survive slicing). We use tegra_swizzle directly for deswizzle.

#[allow(dead_code)]
fn parse_bntx(data: &[u8]) -> (Vec<TextureRes>, Vec<u8>) {
    let (map, section, ordered) = parse_bntx_named(data);
    let _ = map;
    (ordered, section)
}

/// Parse BNTX and return a name-keyed map, combined texture section, and ordered list.
pub(crate) fn parse_bntx_named(data: &[u8]) -> (HashMap<String, (TextureRes, Vec<u8>)>, Vec<u8>, Vec<TextureRes>) {
    let r16 = |off: usize| -> u16 {
        if off + 2 > data.len() { return 0; }
        u16::from_le_bytes(data[off..off+2].try_into().unwrap_or([0;2]))
    };
    let r32 = |off: usize| -> u32 {
        if off + 4 > data.len() { return 0; }
        u32::from_le_bytes(data[off..off+4].try_into().unwrap_or([0;4]))
    };
    let _r64 = |off: usize| -> u64 {
        if off + 8 > data.len() { return 0; }
        u64::from_le_bytes(data[off..off+8].try_into().unwrap_or([0;8]))
    };

    // Scan for BNTX magic — may be embedded at a non-zero offset.
    let bntx_base = match data.windows(4).position(|w| w == b"BNTX") {
        Some(p) => p,
        None => return (HashMap::new(), vec![], vec![]),
    };

    // NX section immediately follows BNTX header at bntx_base + 0x20
    let nx = bntx_base + 0x20;
    if nx + 0x24 > data.len() || &data[nx..nx+4] != b"NX  " {
        return (HashMap::new(), vec![], vec![]);
    }

    let tex_count = r32(nx + 0x04) as usize;
    // BRTD offset: self-relative u32 at NX+0x10
    let data_blk_abs = nx + 0x10 + r32(nx + 0x10) as usize;
    // BRTD block: "BRTD" magic + u64 size header (16 bytes), then pixel data
    let brtd_data_start = data_blk_abs + 0x10;

    // Scan for BRTI magic between bntx_base and the BRTD block
    let scan_end = data_blk_abs.min(data.len());
    let mut brti_offsets: Vec<usize> = Vec::new();
    let mut pos = bntx_base;
    while pos + 4 <= scan_end {
        if &data[pos..pos+4] == b"BRTI" {
            brti_offsets.push(pos);
            let brti_len = r32(pos + 4) as usize;
            pos += brti_len.max(0x90);
        } else {
            pos += 8;
        }
    }
    eprintln!("[BNTX] found {} BRTI structs, {} textures", brti_offsets.len(), tex_count);

    // Scan for _STR block to get texture names in order.
    // Fix 1.2: advance by 1 byte instead of 8 so _STR is found regardless of
    // its alignment relative to bntx_base. The old stride-8 scan skipped _STR
    // when it was at a non-8-byte-aligned offset (e.g. bntx_base + 0x14).
    // Fix 1.3: use data.len() as the scan ceiling instead of scan_end (data_blk_abs).
    // When BNTX is embedded in a GRTF sub-slice, data_blk_abs is computed from a
    // self-relative pointer inside the sub-slice and may land before _STR.
    let mut str_names: Vec<String> = Vec::new();
    let mut str_pos = bntx_base;
    while str_pos + 4 <= data.len() {
        if &data[str_pos..str_pos+4] == b"_STR" {
            let str_count = r32(str_pos + 16) as usize;
            // BNTX _STR block format (Switch/NX):
            //   +0x00: "_STR" magic
            //   +0x04: u32 total_block_size
            //   +0x08: u32 block_size_2 (often same)
            //   +0x0C: u32 padding
            //   +0x10: u32 string_count
            //   +0x14: u32 offsets[string_count]  (each offset is relative to BNTX base)
            //   ... : string data: each = u16 length_prefix + content + padding
            for i in 0..str_count.min(512) {
                let off_pos = str_pos + 20 + i * 4;
                if off_pos + 4 > data.len() { break; }
                let str_off = r32(off_pos) as usize;
                if str_off + 2 > data.len() { break; }
                let slen = r16(str_off) as usize;
                if str_off + 2 + slen > data.len() { break; }
                let s = String::from_utf8_lossy(&data[str_off + 2..str_off + 2 + slen]).to_string();
                if !s.is_empty() {
                    str_names.push(s);
                }
            }
            break;
        }
        str_pos += 1;
        if str_pos > data.len() { break; }
    }
    eprintln!("[BNTX] _STR names: {:?}", &str_names[..str_names.len().min(5)]);

    let mut bntx_map: HashMap<String, (TextureRes, Vec<u8>)> = HashMap::new();
    let mut bntx_ordered: Vec<TextureRes> = Vec::new();
    let mut texture_section: Vec<u8> = Vec::new();
    let mut brtd_cursor: usize = 0;

    for (brti_idx, &brti) in brti_offsets.iter().enumerate() {
        if brti + 0x78 > data.len() { continue; }

        // BRTI field offsets (verified against ScanMountGoat/bntx and aboood40091/BNTX-Extractor):
        // +0x10: flags (u8)
        // +0x11: texture_dimension (u8)
        // +0x12: tile_mode (u16) — 0=block-linear, 1=pitch/linear
        // +0x14: swizzle (u16)
        // +0x16: mipmap_count (u16)
        // +0x18: multi_sample_count (u32)
        // +0x1C: image_format (u32)
        // +0x24: width (u32)
        // +0x28: height (u32)
        // +0x34: block_height_log2 / sizeRange (u32)
        // +0x50: image_size (u32)
        // +0x54: align (u32)
        // +0x58: comp_sel (u32)
        // +0x70: ptrsAddr (u64) — pointer to mipmap offset array
        let tile_mode         = r16(brti + 0x12) as u8; // u16 at +0x12, not u8 at +0x10
        let mip_count         = r16(brti + 0x16) as u32;
        let fmt_raw           = r32(brti + 0x1C);
        let width             = r32(brti + 0x24);
        let height            = r32(brti + 0x28);
        let block_height_log2 = r32(brti + 0x34);
        let data_size         = r32(brti + 0x50);
        let comp_sel          = r32(brti + 0x58);

        // mip0_ptr: ptrsAddr is at BRTI+0x70 (u64 pointer to mipmap offset array).
        // The pointer is self-relative within the BNTX slice. We convert to an absolute
        // offset in `data` by adding bntx_base, then dereference to get the pixel data
        // offset (also relative to bntx_base).
        let pts_addr = {
            let lo = r32(brti + 0x70) as u64;
            let hi = r32(brti + 0x74) as u64;
            (hi << 32 | lo) as usize
        };
        let pts_addr_abs = bntx_base.saturating_add(pts_addr);
        let mip0_ptr = if pts_addr > 0 && pts_addr_abs + 8 <= data.len() {
            let lo = r32(pts_addr_abs) as u64;
            let hi = r32(pts_addr_abs + 4) as u64;
            let rel = (hi << 32 | lo) as usize;
            bntx_base.saturating_add(rel)
        } else {
            0
        };

        let pixel_start = if mip0_ptr > 0 && mip0_ptr < data.len() {
            mip0_ptr
        } else {
            // Fallback: sequential cursor into BRTD pixel data block
            brtd_data_start + brtd_cursor
        };
        let raw_end = pixel_start + (data_size as usize).min(data.len().saturating_sub(pixel_start));
        let pixel_end = pixel_start + data_size as usize;
        // Always advance cursor regardless of whether this texture is valid,
        // so subsequent textures land at the correct offset.
        brtd_cursor = (brtd_cursor + data_size as usize + 0x1FF) & !0x1FF;

        if width == 0 || height == 0 || data_size == 0 || pixel_end > data.len() { continue; }

        let tex_name = str_names.get(brti_idx)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("tex_{}", bntx_map.len()));

        let format_id = (fmt_raw & 0xFFFF) as u32;
        let fmt_type  = (format_id >> 8) as u8;

        // Deswizzle using tegra_swizzle (replaces the old hand-rolled gob_addr loop).
        let raw = &data[pixel_start..raw_end];
        let is_bc = matches!(fmt_type, 0x1A | 0x1B | 0x1C | 0x1D | 0x1E | 0x1F | 0x20);
        let (blk_w, blk_h) = if is_bc { (4u32, 4u32) } else { (1u32, 1u32) };
        let bpp: u32 = match fmt_type {
            0x1A | 0x1D => 8,
            0x1B | 0x1C | 0x1E | 0x1F | 0x20 => 16,
            0x02 => 1,
            0x09 | 0x0A => 2,
            _ => 4,
        };
        let block_dim = tegra_swizzle::surface::BlockDim {
            width:  std::num::NonZeroU32::new(blk_w).unwrap(),
            height: std::num::NonZeroU32::new(blk_h).unwrap(),
            depth:  std::num::NonZeroU32::new(1).unwrap(),
        };
        // tile_mode==1 means linear/pitch (no swizzle). tile_mode==0 means block-linear (deswizzle required for all formats including BC).
        let pixel_bytes = if tile_mode == 1 {
            raw.to_vec()
        } else {
            // Use block_height_log2 from BRTI header (sizeRange field).
            // The field stores log2 of the block height in GOBs, so actual = 1 << sizeRange.
            // BlockHeight::new() takes the actual value (1, 2, 4, 8, 16, or 32).
            let block_height = tegra_swizzle::BlockHeight::new(1u32 << block_height_log2.min(5))
                .unwrap_or_else(|| tegra_swizzle::block_height_mip0(
                    tegra_swizzle::div_round_up((height + blk_h - 1) / blk_h, 8),
                ));
            // Compute the expected surface size for the deswizzler.
            // If raw data is smaller, pad with zeros so deswizzle succeeds
            // (this handles textures whose data_size is smaller than the
            // uncompressed surface, e.g. NX compressed or misreported sizes).
            let surface_size = (width * height * bpp / (blk_w * blk_h)) as usize;
            let padded = if raw.len() < surface_size {
                let mut v = raw.to_vec();
                v.resize(surface_size, 0);
                v
            } else {
                raw.to_vec()
            };
            tegra_swizzle::surface::deswizzle_surface(
                width, height, 1,
                &padded,
                block_dim,
                Some(block_height),
                bpp,
                1, 1,
            ).unwrap_or_else(|e| {
                if crate::fx_debug_enabled() {
                    eprintln!("[BNTX] deswizzle error tex {brti_idx} ({}x{} fmt={:#04x} bpp={}): {e}", width, height, fmt_type, bpp);
                }
                raw.to_vec()
            })
        };

        let ftx_data_offset = texture_section.len() as u32;
        let pixel_len = pixel_bytes.len() as u32;
        if crate::fx_debug_enabled() {
            eprintln!("[TEX_INFO] {} fmt_type={:#04x} name='{}'", tex_name, fmt_type, tex_name);
        }
        texture_section.extend_from_slice(&pixel_bytes);

        let tex_res = TextureRes {
            tex_name: tex_name.clone(),
            width: width as u16,
            height: height as u16,
            ftx_format: format_id,
            ftx_data_offset,
            ftx_data_size: pixel_len,
            original_format: format_id,
            original_data_offset: ftx_data_offset,
            original_data_size: pixel_len,
            wrap_mode: tile_mode,
            filter_mode: block_height_log2 as u8,
            mipmap_count: mip_count,
            channel_swizzle: comp_sel,
        };
        bntx_ordered.push(tex_res.clone());
        bntx_map.insert(tex_name, (tex_res, pixel_bytes));
    }

    eprintln!("[BNTX] parsed {} textures, {} section bytes", bntx_ordered.len(), texture_section.len());
    (bntx_map, texture_section, bntx_ordered)
}

/// Convert a bntx::SurfaceFormat to the 16-bit format ID used by TextureRes.
#[allow(dead_code)]
fn bntx_surface_format_to_id(fmt: bntx::SurfaceFormat) -> u32 {
    match fmt {
        bntx::SurfaceFormat::R8Unorm        => 0x0201,
        bntx::SurfaceFormat::R8G8B8A8Unorm  => 0x0B01,
        bntx::SurfaceFormat::R8G8B8A8Srgb   => 0x0B06,
        bntx::SurfaceFormat::B8G8R8A8Unorm  => 0x0C01,
        bntx::SurfaceFormat::B8G8R8A8Srgb   => 0x0C06,
        bntx::SurfaceFormat::BC1Unorm       => 0x1A01,
        bntx::SurfaceFormat::BC1Srgb        => 0x1A06,
        bntx::SurfaceFormat::BC2Unorm       => 0x1B01,
        bntx::SurfaceFormat::BC2Srgb        => 0x1B06,
        bntx::SurfaceFormat::BC3Unorm       => 0x1C01,
        bntx::SurfaceFormat::BC3Srgb        => 0x1C06,
        bntx::SurfaceFormat::BC4Unorm       => 0x1D01,
        bntx::SurfaceFormat::BC4Snorm       => 0x1D02,
        bntx::SurfaceFormat::BC5Unorm       => 0x1E01,
        bntx::SurfaceFormat::BC5Snorm       => 0x1E02,
        bntx::SurfaceFormat::BC7Unorm       => 0x2001,
        bntx::SurfaceFormat::BC7Srgb        => 0x2006,
        bntx::SurfaceFormat::R11G11B10      => 0x0F05,
        bntx::SurfaceFormat::BC6Sfloat      => 0x1F05,
        bntx::SurfaceFormat::BC6Ufloat      => 0x1F0A,
        bntx::SurfaceFormat::Unk1           => 0x0A05,
    }
}

#[allow(dead_code)]
fn bntx_block_dim(fmt: bntx::SurfaceFormat) -> tegra_swizzle::surface::BlockDim {
    use tegra_swizzle::surface::BlockDim;
    match fmt {
        bntx::SurfaceFormat::BC1Unorm | bntx::SurfaceFormat::BC1Srgb
        | bntx::SurfaceFormat::BC2Unorm | bntx::SurfaceFormat::BC2Srgb
        | bntx::SurfaceFormat::BC3Unorm | bntx::SurfaceFormat::BC3Srgb
        | bntx::SurfaceFormat::BC4Unorm | bntx::SurfaceFormat::BC4Snorm
        | bntx::SurfaceFormat::BC5Unorm | bntx::SurfaceFormat::BC5Snorm
        | bntx::SurfaceFormat::BC6Sfloat | bntx::SurfaceFormat::BC6Ufloat
        | bntx::SurfaceFormat::BC7Unorm | bntx::SurfaceFormat::BC7Srgb => BlockDim::block_4x4(),
        _ => BlockDim::uncompressed(),
    }
}

#[allow(dead_code)]
fn bntx_bytes_per_pixel(fmt: bntx::SurfaceFormat) -> u32 {
    match fmt {
        bntx::SurfaceFormat::R8Unorm => 1,
        bntx::SurfaceFormat::R8G8B8A8Unorm | bntx::SurfaceFormat::R8G8B8A8Srgb
        | bntx::SurfaceFormat::B8G8R8A8Unorm | bntx::SurfaceFormat::B8G8R8A8Srgb
        | bntx::SurfaceFormat::R11G11B10 => 4,
        bntx::SurfaceFormat::BC1Unorm | bntx::SurfaceFormat::BC1Srgb
        | bntx::SurfaceFormat::BC4Unorm | bntx::SurfaceFormat::BC4Snorm => 8,
        bntx::SurfaceFormat::BC2Unorm | bntx::SurfaceFormat::BC2Srgb
        | bntx::SurfaceFormat::BC3Unorm | bntx::SurfaceFormat::BC3Srgb
        | bntx::SurfaceFormat::BC5Unorm | bntx::SurfaceFormat::BC5Snorm
        | bntx::SurfaceFormat::BC6Sfloat | bntx::SurfaceFormat::BC6Ufloat
        | bntx::SurfaceFormat::BC7Unorm | bntx::SurfaceFormat::BC7Srgb => 16,
        bntx::SurfaceFormat::Unk1 => 4,
    }
}


/// Parse a G3PR section's embedded BFRES binary into a list of BfresModel entries.
/// Applies the NX BFRES relocation table to resolve all pointer fields, then
/// walks FMDL → FVTX → FSHP to extract vertex and index buffers.
/// `bntx_str_names` is the ordered list of BNTX texture names (from the _STR block)
/// used to resolve FMAT sampler names to texture indices.
fn parse_g3pr(data: &[u8], bfres_start: usize, bfres_len: usize, bntx_str_names: &[String]) -> Vec<BfresModel> {
    let end = (bfres_start + bfres_len).min(data.len());
    if bfres_start >= data.len() || bfres_len < 0x60 || end <= bfres_start {
        return vec![];
    }
    let raw = &data[bfres_start..end];

    if raw.len() < 4 || &raw[0..4] != b"FRES" {
        eprintln!("[G3PR] BFRES magic mismatch at offset {:#x}", bfres_start);
        return vec![];
    }

    let r16 = |buf: &[u8], off: usize| -> u16 {
        if off + 2 > buf.len() { return 0; }
        u16::from_le_bytes(buf[off..off+2].try_into().unwrap_or([0;2]))
    };
    let r32 = |buf: &[u8], off: usize| -> u32 {
        if off + 4 > buf.len() { return 0; }
        u32::from_le_bytes(buf[off..off+4].try_into().unwrap_or([0;4]))
    };
    let r64 = |buf: &[u8], off: usize| -> u64 {
        if off + 8 > buf.len() { return 0; }
        u64::from_le_bytes(buf[off..off+8].try_into().unwrap_or([0;8]))
    };
    let rf32 = |buf: &[u8], off: usize| -> f32 { f32::from_bits(r32(buf, off)) };

    // Binary file header:
    // +0x16: first_block_offset (u16)
    // +0x18: relocation_table_offset (u32) — absolute file offset
    let rlt_offset = r32(raw, 0x18) as usize;

    // Make a mutable copy and apply the relocation table
    let mut bfres = raw.to_vec();

    if rlt_offset + 16 <= bfres.len() && &bfres[rlt_offset..rlt_offset+4] == b"_RLT" {
        let num_sections = r32(&bfres, rlt_offset + 8) as usize;

        // Compute memory base from the first section header:
        // section.memory_address - section.file_offset = memory_base
        let mut memory_base: Option<u64> = None;
        let sec_hdr_start = rlt_offset + 16;
        for si in 0..num_sections.min(64) {
            let sh = sec_hdr_start + si * 24;
            if sh + 24 > bfres.len() { break; }
            let mem_addr  = r64(&bfres, sh);
            let file_off  = r32(&bfres, sh + 8) as u64;
            if mem_addr != 0 && mem_addr > file_off {
                memory_base = Some(mem_addr - file_off);
                break;
            }
        }

        // Relocation entries start after the section headers
        // Each section header is 24 bytes: mem_addr(u64) + file_off(u32) + file_size(u32) + first_reloc(u32) + num_relocs(u32)
        let reloc_entries_start = rlt_offset + 16 + num_sections * 24;

        if let Some(base) = memory_base {
            let mut entry_ptr = reloc_entries_start;
            while entry_ptr + 8 <= bfres.len() {
                let field_off  = r32(&bfres, entry_ptr) as usize;
                let num_chunks = r16(&bfres, entry_ptr + 4) as usize;
                let rel_words  = bfres.get(entry_ptr + 6).copied().unwrap_or(0) as usize;
                let skip_words = bfres.get(entry_ptr + 7).copied().unwrap_or(0) as usize;
                entry_ptr += 8;

                let mut cur_off = field_off;
                for _ in 0..num_chunks.min(256) {
                    for _ in 0..rel_words.min(8) {
                        if cur_off + 8 > bfres.len() { break; }
                        let stored = r64(&bfres, cur_off);
                        let file_off = if stored == 0 { 0u64 } else if stored >= base { stored - base } else { 0u64 };
                        bfres[cur_off..cur_off+8].copy_from_slice(&file_off.to_le_bytes());
                        cur_off += 8;
                    }
                    cur_off += skip_words * 8;
                }
            }
        } else {
        }
    } else {
    }

    let read_str = |buf: &[u8], off: usize| -> String {
        if off == 0 || off >= buf.len() { return String::new(); }
        let end = buf[off..].iter().position(|&b| b == 0).unwrap_or(0);
        String::from_utf8_lossy(&buf[off..off+end]).to_string()
    };

    // FRES-specific header (NX BFRES, from binary analysis of ef_samus.eff):
    // +0x20: name_offset (u64) — NOT model_arr
    // +0x22: num_models (u16) — packed inside the name_offset field (little-endian)
    // +0x28: model_arr (u64) — direct pointer to first FMDL (not a pointer array)
    //
    // Note: the NX BFRES in SSBU effect files uses direct FMDL pointers, not
    // an indirection array. model_arr points directly to the first FMDL block.
    let model_arr  = r64(&bfres, 0x28) as usize;
    let num_models = r16(&bfres, 0x22) as usize;
    eprintln!("[G3PR] BFRES len={} num_models={} model_arr={:#x}", bfres.len(), num_models, model_arr);

    if num_models == 0 || model_arr == 0 || model_arr >= bfres.len() {
        return vec![];
    }

    let mut models = Vec::new();

    // model_arr is a direct pointer to the first FMDL block (not an array of pointers).
    // SSBU effect BFRES files always have exactly 1 model.
    for mi in 0..num_models.min(256) {
        let fmdl = if mi == 0 { model_arr } else { break };
        if fmdl == 0 || fmdl + 0x70 > bfres.len() { continue; }
        if &bfres[fmdl..fmdl+4] != b"FMDL" { continue; }

        // NX BFRES FMDL layout (from binary analysis of ef_samus.eff):
        // +0x20: fvtx_ptr (u64) — direct pointer to first FVTX
        // +0x28: fshp_ptr (u64) — direct pointer to first FSHP
        // +0x38: fmat_ptr (u64) — direct pointer to first FMAT
        // +0x68: num_vbufs (u16)
        // +0x6a: num_shapes (u16)
        // +0x6c: num_mats (u16)
        let num_vbufs  = r16(&bfres, fmdl + 0x68) as usize;
        let num_shapes = r16(&bfres, fmdl + 0x6a) as usize;
        let num_mats   = r16(&bfres, fmdl + 0x6c) as usize;
        let fvtx_ptr   = r64(&bfres, fmdl + 0x20) as usize;
        let fshp_ptr   = r64(&bfres, fmdl + 0x28) as usize;
        let fmat_ptr   = r64(&bfres, fmdl + 0x38) as usize;

        eprintln!("[G3PR] FMDL[{}]: num_vbufs={} num_shapes={} num_mats={} fvtx={:#x} fshp={:#x} fmat={:#x}",
            mi, num_vbufs, num_shapes, num_mats, fvtx_ptr, fshp_ptr, fmat_ptr);

        if num_vbufs == 0 || num_shapes == 0 { continue; }
        if fvtx_ptr == 0 || fvtx_ptr >= bfres.len() { continue; }
        if fshp_ptr == 0 || fshp_ptr >= bfres.len() { continue; }

        struct FvtxData { positions: Vec<[f32;3]>, uvs: Vec<[f32;2]>, normals: Vec<[f32;3]> }
        let mut fvtx_data: Vec<FvtxData> = Vec::new();

        // fvtx_ptr is a direct pointer to the first FVTX block
        for vi in 0..num_vbufs.min(64) {
            let fvtx = if vi == 0 { fvtx_ptr } else { break };
            if fvtx == 0 || fvtx + 0x50 > bfres.len() || &bfres[fvtx..fvtx+4] != b"FVTX" {
                fvtx_data.push(FvtxData { positions: vec![], uvs: vec![], normals: vec![] });
                continue;
            }

            // NX BFRES FVTX layout (from binary analysis):
            // +0x08: attrib_arr (u64) — array of attrib entries (0x10 bytes each)
            // +0x30: buf_arr (u64) — array of buffer entries (0x18 bytes each)
            // +0x4a: num_vertices (u16)
            // +0x4c: num_attribs (byte)
            // +0x4d: num_buffers (byte)
            // Attrib entry (0x10 bytes): name_ptr(u64) + buf_idx(u8) + pad(u8) + attr_off(u16) + format(u32)
            // Buffer entry (0x18 bytes): data_off(u64) + [8 bytes pad] + stride(u64)
            let num_attribs  = bfres.get(fvtx + 0x4c).copied().unwrap_or(0) as usize;
            let num_buffers  = bfres.get(fvtx + 0x4d).copied().unwrap_or(0) as usize;
            let num_vertices = r16(&bfres, fvtx + 0x4a) as usize;
            let attrib_arr   = r64(&bfres, fvtx + 0x08) as usize;
            let buf_arr      = r64(&bfres, fvtx + 0x30) as usize;

            eprintln!("[G3PR] FVTX[{}]: num_attribs={} num_buffers={} num_vertices={}", vi, num_attribs, num_buffers, num_vertices);

            if num_vertices == 0 || num_vertices > 1_000_000 {
                fvtx_data.push(FvtxData { positions: vec![], uvs: vec![], normals: vec![] });
                continue;
            }

            struct AttribInfo { name: String, buf_idx: usize, offset: usize, format: u32 }
            let mut attribs: Vec<AttribInfo> = Vec::new();
            if attrib_arr != 0 && attrib_arr < bfres.len() {
                for ai in 0..num_attribs.min(32) {
                    let a = attrib_arr + ai * 0x10;
                    if a + 0x10 > bfres.len() { break; }
                    let name_off = r64(&bfres, a) as usize;
                    let name     = read_str(&bfres, name_off);
                    let buf_idx  = bfres[a + 0x08] as usize;
                    let attr_off = r16(&bfres, a + 0x0A) as usize;
                    let format   = r32(&bfres, a + 0x0C);
                    eprintln!("[G3PR]   attrib[{}]: '{}' buf={} off={:#x} fmt={:#06x}", ai, name, buf_idx, attr_off, format);
                    attribs.push(AttribInfo { name, buf_idx, offset: attr_off, format });
                }
            }

            struct BufInfo { data_off: usize, stride: usize }
            let mut buffers: Vec<BufInfo> = Vec::new();
            if buf_arr != 0 && buf_arr < bfres.len() {
                for bi in 0..num_buffers.min(16) {
                    let b = buf_arr + bi * 0x18;
                    if b + 0x18 > bfres.len() { break; }
                    let data_off = r64(&bfres, b) as usize;
                    let stride   = r64(&bfres, b + 0x10) as usize;
                    eprintln!("[G3PR]   buf[{}]: data_off={:#x} stride={}", bi, data_off, stride);
                    buffers.push(BufInfo { data_off, stride });
                }
            }

            let mut positions: Vec<[f32;3]> = vec![[0.0;3]; num_vertices];
            let mut uvs:       Vec<[f32;2]> = vec![[0.0;2]; num_vertices];
            let mut normals:   Vec<[f32;3]> = vec![[0.0;3]; num_vertices];

            for attr in &attribs {
                let is_pos = attr.name == "_p0";
                let is_uv  = attr.name == "_u0";
                let is_nrm = attr.name == "_n0";
                if !is_pos && !is_uv && !is_nrm { continue; }
                let buf = match buffers.get(attr.buf_idx) { Some(b) => b, None => continue };
                if buf.data_off == 0 || buf.stride == 0 || buf.data_off >= bfres.len() { continue; }
                for v in 0..num_vertices {
                    let voff = buf.data_off + v * buf.stride + attr.offset;
                    if is_pos && attr.format == 0x0306 && voff + 12 <= bfres.len() {
                        positions[v] = [rf32(&bfres, voff), rf32(&bfres, voff+4), rf32(&bfres, voff+8)];
                    } else if is_uv {
                        if attr.format == 0x0206 && voff + 8 <= bfres.len() {
                            // f32x2
                            uvs[v] = [rf32(&bfres, voff), rf32(&bfres, voff+4)];
                        } else if attr.format == 0x0204 && voff + 4 <= bfres.len() {
                            // f16x2 (half-float)
                            uvs[v] = [half_to_f32(r16(&bfres, voff)), half_to_f32(r16(&bfres, voff+2))];
                        } else if attr.format == 0x020A && voff + 4 <= bfres.len() {
                            // SNorm16x2: divide by 32767.0 → [-1, 1]
                            let u = i16::from_le_bytes([bfres[voff], bfres[voff+1]]) as f32 / 32767.0;
                            let v2 = i16::from_le_bytes([bfres[voff+2], bfres[voff+3]]) as f32 / 32767.0;
                            uvs[v] = [u, v2];
                        } else if attr.format == 0x0209 && voff + 4 <= bfres.len() {
                            // UNorm16x2: divide by 65535.0 → [0, 1]
                            let u = u16::from_le_bytes([bfres[voff], bfres[voff+1]]) as f32 / 65535.0;
                            let v2 = u16::from_le_bytes([bfres[voff+2], bfres[voff+3]]) as f32 / 65535.0;
                            uvs[v] = [u, v2];
                        }
                        // If no UV attribute matched, uvs[v] stays [0.0, 0.0] (initialized above)
                    } else if is_nrm {
                        if attr.format == 0x0306 && voff + 12 <= bfres.len() {
                            normals[v] = [rf32(&bfres, voff), rf32(&bfres, voff+4), rf32(&bfres, voff+8)];
                        } else if attr.format == 0x020B && voff + 4 <= bfres.len() {
                            normals[v] = unpack_10_10_10_2_snorm(r32(&bfres, voff));
                        }
                    }
                }
            }
            fvtx_data.push(FvtxData { positions, uvs, normals });
        }

        let mut meshes: Vec<BfresMesh> = Vec::new();

        // ── FMAT: build per-material texture index table ──────────────────
        // NX BFRES FMAT layout (from binary analysis):
        // fmat_ptr is a direct pointer to the first FMAT block.
        // +0x28: TextureNameArray ptr (u64) — array of u64 string ptrs to texture names
        // +0x4A: numTextureRef (byte)
        // Texture slot assignment by name suffix:
        //   _col (or no suffix) → color slot
        //   _emi                → emissive slot
        //   _prm                → PBR params slot
        // Packed as (color, emissive, prm) per material.
        let mut mat_tex_indices: Vec<(u32, u32, u32)> = Vec::new();
        if fmat_ptr != 0 && fmat_ptr < bfres.len() && num_mats > 0 {
            for mat_idx in 0..num_mats.min(64) {
                let fmat = if mat_idx == 0 { fmat_ptr } else { break };
                if fmat == 0 || fmat + 0x50 > bfres.len() || &bfres[fmat..fmat+4] != b"FMAT" {
                    mat_tex_indices.push((u32::MAX, u32::MAX, u32::MAX)); continue;
                }
                let tex_name_arr = r64(&bfres, fmat + 0x28) as usize;
                let num_tex_refs = bfres.get(fmat + 0x4A).copied().unwrap_or(0) as usize;
                eprintln!("[G3PR] FMAT[{}]: tex_name_arr={:#x} num_tex_refs={}", mat_idx, tex_name_arr, num_tex_refs);
                if num_tex_refs == 0 || tex_name_arr == 0 || tex_name_arr >= bfres.len() {
                    mat_tex_indices.push((u32::MAX, u32::MAX, u32::MAX)); continue;
                }
                let mut col_idx = u32::MAX;
                let mut emi_idx = u32::MAX;
                let mut prm_idx = u32::MAX;
                for ti in 0..num_tex_refs.min(16) {
                    let name_ptr = r64(&bfres, tex_name_arr + ti * 8) as usize;
                    let tex_name = read_str(&bfres, name_ptr);
                    let idx = bntx_str_names.iter().position(|n| n == &tex_name)
                        .map(|i| i as u32)
                        .unwrap_or(u32::MAX);
                    eprintln!("[G3PR] FMAT[{}] tex[{}] '{}' -> bntx_idx={:?}", mat_idx, ti, tex_name, idx);
                    let lower = tex_name.to_lowercase();
                    if lower.ends_with("_emi") {
                        if emi_idx == u32::MAX { emi_idx = idx; }
                    } else if lower.ends_with("_prm") {
                        if prm_idx == u32::MAX { prm_idx = idx; }
                    } else {
                        // _col or unsuffixed → color slot (first one wins)
                        if col_idx == u32::MAX { col_idx = idx; }
                    }
                }
                mat_tex_indices.push((col_idx, emi_idx, prm_idx));
            }
        }

        // ── FSHP: parse shapes ────────────────────────────────────────────
        // NX BFRES FSHP layout (from binary analysis):
        // fshp_ptr is a direct pointer to the first FSHP block.
        // +0x18: mesh_arr (u64) — pointer to first mesh entry
        // Mesh entry layout:
        //   +0x00: ibuf_off (u64) — index buffer offset
        //   +0x20: index_count (u32)
        //   +0x24: index_fmt (u32): 0=u8, 1=u16, 2=u32
        // fvtx_idx and mat_idx are both 0 for single-vbuf/single-mat models.
        for si in 0..num_shapes.min(64) {
            let fshp = if si == 0 { fshp_ptr } else { break };
            if fshp == 0 || fshp + 0x60 > bfres.len() || &bfres[fshp..fshp+4] != b"FSHP" { continue; }

            let fvtx_idx = 0usize; // single FVTX
            let mat_idx  = 0usize; // single FMAT
            let mesh_arr = r64(&bfres, fshp + 0x18) as usize;
            if mesh_arr == 0 || mesh_arr >= bfres.len() { continue; }

            let mesh_off = mesh_arr;
            if mesh_off + 0x28 > bfres.len() { continue; }
            let ibuf_off    = r64(&bfres, mesh_off) as usize;
            let index_count = r32(&bfres, mesh_off + 0x20) as usize;
            let index_fmt   = r32(&bfres, mesh_off + 0x24);

            eprintln!("[G3PR] FSHP[{}]: mesh_arr={:#x} ibuf_off={:#x} index_count={} index_fmt={}",
                si, mesh_arr, ibuf_off, index_count, index_fmt);

            if ibuf_off == 0 || ibuf_off >= bfres.len() || index_count == 0 { continue; }
            let icount_aligned = (index_count / 3) * 3;
            let mut indices: Vec<u16> = Vec::with_capacity(icount_aligned);
            match index_fmt {
                0 => { for i in 0..icount_aligned { let o = ibuf_off+i; if o >= bfres.len() { break; } indices.push(bfres[o] as u16); } }
                1 => { for i in 0..icount_aligned { let o = ibuf_off+i*2; if o+2 > bfres.len() { break; } indices.push(r16(&bfres, o)); } }
                2 => { for i in 0..icount_aligned { let o = ibuf_off+i*4; if o+4 > bfres.len() { break; } indices.push(r32(&bfres, o).min(u16::MAX as u32) as u16); } }
                _ => continue,
            }
            if indices.is_empty() { continue; }

            let (positions, uvs, normals) = match fvtx_data.get(fvtx_idx) {
                Some(d) => (&d.positions, &d.uvs, &d.normals),
                None => continue,
            };
            if positions.is_empty() { continue; }

            let vertices: Vec<MeshVertex> = (0..positions.len()).map(|v| MeshVertex {
                position: positions[v], uv: uvs[v], normal: normals[v],
            }).collect();
            let tex_idx = mat_tex_indices.get(mat_idx).copied().unwrap_or((u32::MAX, u32::MAX, u32::MAX));
            meshes.push(BfresMesh { vertices, indices, texture_index: tex_idx.0, emissive_tex_index: tex_idx.1, prm_tex_index: tex_idx.2 });
        }

        let name_off = r64(&bfres, fmdl + 0x08) as usize;
        let name = read_str(&bfres, name_off);
        eprintln!("[G3PR] parsed model '{}': {} meshes", name, meshes.len());
        models.push(BfresModel {
            name,
            source_id: 0,
            meshes,
        });
    }

    models
}

/// Parse a standalone `.bfres` file (not embedded in a G3PR section).
/// Delegates to `parse_g3pr` with start=0 and no texture-name hint list.
pub(crate) fn parse_bfres(data: &[u8]) -> Vec<BfresModel> {
    parse_g3pr(data, 0, data.len(), &[])
}

/// Convert a 16-bit half-float to f32.
fn half_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exp  = ((bits >> 10) & 0x1F) as u32;
    let mant = (bits & 0x3FF) as u32;
    let (exp32, mant32) = if exp == 0 {
        if mant == 0 { (0, 0) } else {
            let mut e = 127 - 14;
            let mut m = mant;
            while m & 0x400 == 0 { m <<= 1; e -= 1; }
            (e, (m & 0x3FF) << 13)
        }
    } else if exp == 31 {
        (255, mant << 13)
    } else {
        (exp + 127 - 15, mant << 13)
    };
    f32::from_bits(sign | (exp32 << 23) | mant32)
}

/// Unpack a 10_10_10_2 SNorm packed u32 into [x, y, z] f32 normals.
fn unpack_10_10_10_2_snorm(packed: u32) -> [f32; 3] {
    let x_raw = (packed & 0x3FF) as i32;
    let y_raw = ((packed >> 10) & 0x3FF) as i32;
    let z_raw = ((packed >> 20) & 0x3FF) as i32;
    let snorm10 = |v: i32| -> f32 {
        let s = if v >= 512 { v - 1024 } else { v };
        (s as f32 / 511.0).clamp(-1.0, 1.0)
    };
    [snorm10(x_raw), snorm10(y_raw), snorm10(z_raw)]
}

impl PtclFile {
    /// Build a synthetic PtclFile with placeholder emitter sets for each handle index.
    /// Used when the embedded PTCL uses an unsupported format (e.g. VFXB on Switch).
    pub fn synthetic(max_set_idx: usize) -> Self {
        let emitter_sets = (0..=max_set_idx).map(|i| EmitterSet {
            name: format!("set_{}", i),
            emitters: vec![EmitterDef {
                emission_rate: 8.0,
                initial_speed: 0.3,
                speed_random: 0.3,
                accel: Vec3::new(0.0, 0.05, 0.0),
                lifetime: 12.0,
                ..Default::default()
            }],
        }).collect();
        Self { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0, bntx_textures: Vec::new(), primitives: Vec::new(), bfres_models: Vec::new(), shader_registry: Default::default(), shader_binary_1: Vec::new(), shader_binary_2: Vec::new() }
    }

    /// Build a synthetic PtclFile where each set is named and colored based on the effect name.
    /// `names` maps set_index → effect_handle_name for color hinting.
    pub fn synthetic_named(max_set_idx: usize, names: &std::collections::HashMap<i32, String>) -> Self {
        let emitter_sets = (0..=max_set_idx).map(|i| {
            let hint_name = names.get(&(i as i32)).map(|s| s.as_str()).unwrap_or("");
            let (r, g, b, blend, scale, lifetime) = name_hint_defaults(hint_name);
            EmitterSet {
                name: if hint_name.is_empty() { format!("set_{}", i) } else { hint_name.to_string() },
                emitters: vec![EmitterDef {
                    name: hint_name.to_string(),
                    emit_type: EmitType::Sphere,
                    blend_type: blend,
                    initial_speed: 0.2,
                    speed_random: 0.3,
                    lifetime,
                    scale,
                    color0: vec![ColorKey { frame: 0.0, r, g, b, a: 1.0 }],
                    is_one_time: true,
                    emission_duration: lifetime as u32,
                    ..Default::default()
                }],
            }
        }).collect();
        Self { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0, bntx_textures: Vec::new(), primitives: Vec::new(), bfres_models: Vec::new(), shader_registry: Default::default(), shader_binary_1: Vec::new(), shader_binary_2: Vec::new() }
    }

    /// Scan a directory (non-recursively) for `.nutexb` files and merge their textures
    /// into this `PtclFile`'s texture pool.  The texture name (base filename without
    /// extension) is stored in `TextureRes::tex_name` so that sampler lookups by name
    /// can find them.  Returns the number of textures successfully merged.
    pub fn merge_external_nutexb_dir(&mut self, dir: &std::path::Path) -> usize {
        self.merge_external_nutexb_dir_recursive(dir, false)
    }

    /// Scan a directory **recursively** for `.nutexb` files and merge their textures.
    pub fn merge_external_nutexb_dir_recursive(&mut self, dir: &std::path::Path, recursive: bool) -> usize {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let mut count = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && recursive {
                count += self.merge_external_nutexb_dir_recursive(&path, true);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("nutexb") {
                continue;
            }
            let stem = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            // Skip if we already have a texture with this name
            if self.bntx_textures.iter().any(|t| t.tex_name == stem) {
                continue;
            }
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => { eprintln!("[EXT_TEX] failed to read {:?}: {e}", path); continue; }
            };
            // .nutexb files are raw BNTX containers
            let (_name_map, section_bytes, ordered) = parse_bntx_named(&data);
            if ordered.is_empty() {
                eprintln!("[EXT_TEX] {:?}: no textures found in BNTX", path);
                continue;
            }
            let tex_section_base = self.texture_section.len() as u32;
            // Use the first texture from the BNTX; override its name with the file stem
            // so that sampler lookups by filename work correctly.
            let mut tex = ordered.into_iter().next().unwrap();
            if tex.tex_name.is_empty() {
                tex.tex_name = stem.clone();
            }
            tex.ftx_data_offset += tex_section_base;
            tex.original_data_offset += tex_section_base;
            self.bntx_textures.push(tex);
            self.texture_section.extend_from_slice(&section_bytes);
            eprintln!("[EXT_TEX] merged '{}' from {:?}", stem, path);
            count += 1;
        }
        count
    }
}

/// Infer the best grid layout (cols × rows) for a sprite-sheet texture with
/// `frame_count` frames.  Uses the texture dimensions to pick the factorization
/// that makes each frame closest to square.
pub(crate) fn infer_grid_layout(width: u16, height: u16, frame_count: usize) -> (usize, usize) {
    let tw = width as f64;
    let th = height as f64;
    if tw <= 0.0 || th <= 0.0 { return (1, frame_count); }
    let fc = frame_count;
    let mut best_score = f64::MAX;
    let mut best_balance = usize::MAX;
    let mut best_cols = 1;
    let mut best_rows = fc;
    for cols in 1..=fc {
        if fc % cols != 0 { continue; }
        let rows = fc / cols;
        let aspect = (tw * rows as f64) / (th * cols as f64);
        let score = aspect.log10().abs();
        let balance = (cols as isize - rows as isize).unsigned_abs();
        if score < best_score || (score == best_score && balance < best_balance) {
            best_score = score;
            best_balance = balance;
            best_cols = cols;
            best_rows = rows;
        }
    }
    (best_cols, best_rows)
}

/// True when a texture slot uses flipbook/pattern animation (not pure scroll).
pub fn slot_uses_tex_pattern(
    anim: &TextureAnimFlags,
    pat_frame_count: usize,
    pat_frame_table: &[usize],
) -> bool {
    if pat_frame_count > 1 {
        return true;
    }
    if !pat_frame_table.is_empty() {
        return true;
    }
    anim.pattern_anim_type > 0
}

/// True when slot-0 texture uses flipbook/pattern animation (not pure scroll).
pub fn emitter_uses_tex_pattern(emitter: &EmitterDef) -> bool {
    slot_uses_tex_pattern(
        &texture_anim_flags_slot0(emitter),
        emitter.tex_pat_frame_count,
        &emitter.tex_pat_frame_table,
    )
}

fn texture_anim_flags_slot0(emitter: &EmitterDef) -> TextureAnimFlags {
    TextureAnimFlags {
        pattern_anim_type: emitter.tex_pattern_anim_type,
        is_scroll: emitter.tex_is_scroll,
        is_rotate: emitter.tex_is_rotate,
        is_scale: emitter.tex_is_scale,
        inv_rand_u: emitter.tex_inv_rand_u,
        inv_rand_v: emitter.tex_inv_rand_v,
        pat_loop_random: emitter.tex_pat_loop_random,
        crossfade: emitter.tex_crossfade,
        scroll_rotation: emitter.tex_scroll_rotation,
        scroll_rotation_add: emitter.tex_scroll_rotation_add,
    }
}

/// True when a slot uses UV scroll (`IsScroll` or non-zero scroll speed).
pub fn slot_uses_tex_scroll(anim: &TextureAnimFlags, scroll_uv: [f32; 2]) -> bool {
    if anim.is_scroll {
        return true;
    }
    scroll_uv[0].abs() + scroll_uv[1].abs() > 1e-6
}

/// True when slot-0 texture uses UV scroll (TextureAnim0.IsScroll or non-zero scroll speed).
pub fn emitter_uses_tex_scroll(emitter: &EmitterDef) -> bool {
    slot_uses_tex_scroll(&texture_anim_flags_slot0(emitter), emitter.tex_scroll_uv)
}

/// Sample EASL / animated UV scale for a slot when `IsScale` is set.
pub fn effective_tex_scale_uv(
    base_scale: [f32; 2],
    anim: &TextureAnimFlags,
    anim_tex_scale: Option<&EmitterAnimDef>,
    life_t: f32,
) -> [f32; 2] {
    let mut scale = base_scale;
    if anim.is_scale {
        if let Some(track) = anim_tex_scale {
            if track.enable {
                let v = sample_emitter_anim_track(track, life_t);
                scale[0] *= v[0].max(0.001);
                scale[1] *= v[1].max(0.001);
            }
        }
    }
    scale
}

/// Normalized pattern phase (0..1) over particle lifetime, scaled by TexPatAnim.Frequency.
pub fn pattern_anim_phase(life_t: f32, frequency: f32, phase_offset: f32) -> f32 {
    let freq = if frequency > 0.0 { frequency } else { 1.0 };
    (life_t.clamp(0.0, 1.0) * freq + phase_offset).fract()
}

fn pattern_table_len(pat_frame_count: usize, pat_frame_table: &[usize]) -> usize {
    if pat_frame_table.is_empty() {
        pat_frame_count.max(1)
    } else {
        pat_frame_table.len()
    }
}

fn pattern_table_index(
    anim: &TextureAnimFlags,
    life_t: f32,
    frequency: f32,
    phase_offset: f32,
    table_len: usize,
) -> (usize, f32) {
    let life = life_t.clamp(0.0, 1.0);
    let freq = if frequency > 0.0 { frequency } else { 1.0 };
    let phase = life * freq + phase_offset;
    match anim.pattern_anim_type {
        pattern_anim_type::FIT_LIFESPAN => {
            let denom = table_len.saturating_sub(1).max(1) as f32;
            let raw = (phase.min(1.0) * denom).clamp(0.0, denom);
            (raw.floor() as usize, raw.fract())
        }
        pattern_anim_type::CLAMP => {
            let raw = phase * table_len as f32;
            let idx = raw.floor() as usize;
            (idx.min(table_len.saturating_sub(1)), raw.fract())
        }
        pattern_anim_type::LOOP | pattern_anim_type::NONE => {
            let raw = phase.fract() * table_len as f32;
            (raw.floor() as usize % table_len.max(1), raw.fract())
        }
        _ => {
            let raw = phase.fract() * table_len as f32;
            (raw.floor() as usize % table_len.max(1), raw.fract())
        }
    }
}

/// Resolve flipbook frame index + crossfade blend at normalized life.
pub fn pattern_frame_at_life(
    anim: &TextureAnimFlags,
    pat_frame_count: usize,
    pat_frame_table: &[usize],
    pat_frequency: f32,
    life_t: f32,
    phase_offset: f32,
    fixed_frame: Option<usize>,
) -> (usize, f32) {
    if anim.pattern_anim_type == pattern_anim_type::RANDOM {
        let frame = fixed_frame.unwrap_or(0);
        return (frame.min(pat_frame_count.saturating_sub(1)), 0.0);
    }

    let table_len = pattern_table_len(pat_frame_count, pat_frame_table);
    let (table_idx, frac) = pattern_table_index(anim, life_t, pat_frequency, phase_offset, table_len);

    let frame = if pat_frame_table.is_empty() {
        table_idx
    } else {
        pat_frame_table[table_idx.min(pat_frame_table.len().saturating_sub(1))]
    };
    let frame = frame.min(pat_frame_count.saturating_sub(1));
    let blend = if anim.crossfade { frac } else { 0.0 };
    (frame, blend)
}

/// Frame index, next frame for crossfade, and blend fraction at normalized life.
pub fn pattern_frame_with_crossfade(
    anim: &TextureAnimFlags,
    pat_frame_count: usize,
    pat_frame_table: &[usize],
    pat_frequency: f32,
    life_t: f32,
    phase_offset: f32,
    fixed_frame: Option<usize>,
) -> (usize, usize, f32) {
    let (frame, blend) = pattern_frame_at_life(
        anim,
        pat_frame_count,
        pat_frame_table,
        pat_frequency,
        life_t,
        phase_offset,
        fixed_frame,
    );
    if blend <= 0.0 || anim.pattern_anim_type == pattern_anim_type::RANDOM {
        return (frame, frame, blend);
    }
    let table_len = pattern_table_len(pat_frame_count, pat_frame_table);
    let (table_idx, _) = pattern_table_index(anim, life_t, pat_frequency, phase_offset, table_len);
    let next_table_idx = (table_idx + 1) % table_len.max(1);
    let next_frame = if pat_frame_table.is_empty() {
        next_table_idx
    } else {
        pat_frame_table[next_table_idx.min(pat_frame_table.len().saturating_sub(1))]
    };
    (
        frame,
        next_frame.min(pat_frame_count.saturating_sub(1)),
        blend,
    )
}

/// UV delta from current flipbook cell to the next cell (for crossfade sampling).
pub fn pattern_crossfade_uv_delta(
    frame: usize,
    next_frame: usize,
    tex_scale_uv: [f32; 2],
    tex_offset_uv: [f32; 2],
) -> [f32; 2] {
    let cur = frame_uv_offset(frame, tex_scale_uv, tex_offset_uv);
    let nxt = frame_uv_offset(next_frame, tex_scale_uv, tex_offset_uv);
    [nxt[0] - cur[0], nxt[1] - cur[1]]
}

/// True when TextureAnim3–5 slot `idx` (0..2) should be simulated for this emitter.
pub fn extra_tex_slot_active(emitter: &EmitterDef, idx: usize) -> bool {
    let Some(anim) = emitter.tex_anims_extra.get(idx) else {
        return false;
    };
    let slot = &emitter.tex_extra_slots[idx];
    slot_uses_tex_pattern(anim, slot.pat_frame_count, &slot.pat_frame_table)
        || slot_uses_tex_scroll(anim, slot.scroll_uv)
        || emitter.textures.len() > idx + 3
}

/// Resolve flipbook frame index at normalized life using frequency + optional frame table.
pub fn pattern_frame_index(emitter: &EmitterDef, life_t: f32) -> usize {
    pattern_frame_at_life(
        &texture_anim_flags_slot0(emitter),
        emitter.tex_pat_frame_count,
        &emitter.tex_pat_frame_table,
        emitter.tex_pat_frequency,
        life_t,
        0.0,
        None,
    )
    .0
}

/// UV scroll rotation angle (radians) at normalized life — used for cbuf UV matrix, not billboard spin.
pub fn scroll_uv_angle_at_life(anim: &TextureAnimFlags, life_t: f32, lifetime: f32) -> f32 {
    if !anim.is_rotate {
        return 0.0;
    }
    anim.scroll_rotation + anim.scroll_rotation_add * life_t * lifetime.max(0.0)
}

/// Apply InvRandU/V at spawn: mirror scale when the per-particle seed selects flip.
pub fn apply_inv_rand_uv(
    mut scale: [f32; 2],
    mut offset: [f32; 2],
    anim: &TextureAnimFlags,
    seed: u64,
) -> ([f32; 2], [f32; 2]) {
    if anim.inv_rand_u && (seed & 1) == 1 {
        scale[0] = -scale[0].abs();
        offset[0] = 1.0 - offset[0];
    }
    if anim.inv_rand_v && (seed & 2) == 2 {
        scale[1] = -scale[1].abs();
        offset[1] = 1.0 - offset[1];
    }
    (scale, offset)
}

/// Initialize per-particle UV state at spawn for slot 0.
pub fn init_particle_uv_at_spawn(p: &mut Particle, emitter: &EmitterDef) {
    let anim = texture_anim_flags_slot0(emitter);
    let (scale, offset) = apply_inv_rand_uv(
        emitter.tex_scale_uv,
        emitter.tex_offset_uv,
        &anim,
        p.seed,
    );
    p.tex_scale_live = effective_tex_scale_uv(
        scale,
        &anim,
        emitter.anim_tex_scale.as_ref(),
        0.0,
    );
    p.tex_offset = if slot_uses_tex_pattern(
        &anim,
        emitter.tex_pat_frame_count,
        &emitter.tex_pat_frame_table,
    ) {
        let (frame, _) = pattern_frame_at_life(
            &anim,
            emitter.tex_pat_frame_count,
            &emitter.tex_pat_frame_table,
            emitter.tex_pat_frequency,
            0.0,
            p.pat_phase_offset,
            p.pat_fixed_frame,
        );
        frame_uv_offset(
            frame,
            [
                emitter.tex_scale_uv[0].abs().max(0.001),
                emitter.tex_scale_uv[1].abs().max(0.001),
            ],
            offset,
        )
    } else {
        offset
    };
    p.pat_phase_offset = if anim.pat_loop_random {
        ((p.seed % 997) as f32 / 997.0)
    } else {
        0.0
    };
    p.pat_fixed_frame = if anim.pattern_anim_type == pattern_anim_type::RANDOM {
        let fc = emitter.tex_pat_frame_count.max(1);
        Some((p.seed as usize) % fc)
    } else {
        None
    };
    p.tex_scroll_angle = if anim.is_rotate { anim.scroll_rotation } else { 0.0 };

    let (ind_scale, ind_offset) = apply_inv_rand_uv(
        emitter.indirect_tex_scale_uv,
        emitter.indirect_tex_offset_uv,
        &emitter.indirect_anim,
        p.seed.wrapping_add(3),
    );
    p.indirect_tex_offset = ind_offset;
    let _ = ind_scale;

    let (t2_scale, t2_offset) = apply_inv_rand_uv(
        emitter.tex2_scale_uv,
        emitter.tex2_offset_uv,
        &emitter.tex2_anim,
        p.seed.wrapping_add(5),
    );
    p.tex2_tex_offset = t2_offset;
    let _ = t2_scale;

    for i in 0..3 {
        if !extra_tex_slot_active(emitter, i) {
            continue;
        }
        let slot = &emitter.tex_extra_slots[i];
        let (scale, offset) = apply_inv_rand_uv(
            slot.scale_uv,
            slot.offset_uv,
            &emitter.tex_anims_extra[i],
            p.seed.wrapping_add(7 + i as u64),
        );
        p.tex_extra_offsets[i] = offset;
        let _ = scale;
    }
}

/// Advance one texture slot's UV offset for the current simulation step.
pub fn advance_uv_slot(
    offset: &mut [f32; 2],
    scale_live: &mut [f32; 2],
    scroll_angle: &mut f32,
    anim: &TextureAnimFlags,
    scale_uv: [f32; 2],
    scroll_uv: [f32; 2],
    pat_frame_count: usize,
    pat_frame_table: &[usize],
    pat_frequency: f32,
    base_offset_uv: [f32; 2],
    anim_tex_scale: Option<&EmitterAnimDef>,
    life_t: f32,
    dt: f32,
    phase_offset: f32,
    fixed_frame: Option<usize>,
) {
    if slot_uses_tex_pattern(anim, pat_frame_count, pat_frame_table) {
        let (frame, _blend) = pattern_frame_at_life(
            anim,
            pat_frame_count,
            pat_frame_table,
            pat_frequency,
            life_t,
            phase_offset,
            fixed_frame,
        );
        *offset = frame_uv_offset(frame, scale_uv, base_offset_uv);
        *scale_live = effective_tex_scale_uv(scale_uv, anim, anim_tex_scale, life_t);
    } else if slot_uses_tex_scroll(anim, scroll_uv) {
        *scale_live = effective_tex_scale_uv(scale_uv, anim, anim_tex_scale, life_t);
        let tile_u = (1.0 / scale_live[0].abs().max(0.001)).min(1.0);
        let tile_v = (1.0 / scale_live[1].abs().max(0.001)).min(1.0);
        offset[0] = (offset[0] + scroll_uv[0] * dt).rem_euclid(tile_u);
        offset[1] = (offset[1] + scroll_uv[1] * dt).rem_euclid(tile_v);
        if anim.is_rotate {
            *scroll_angle += anim.scroll_rotation_add * dt;
        }
    }
}

/// Convert a sprite-sheet slot index to UV offset given scale and base offset.
pub fn frame_uv_offset(
    frame: usize,
    tex_scale_uv: [f32; 2],
    tex_offset_uv: [f32; 2],
) -> [f32; 2] {
    let su = tex_scale_uv[0].abs().max(0.001);
    let sv = tex_scale_uv[1].abs().max(0.001);
    let cols = (1.0 / su).round() as usize;
    let rows = (1.0 / sv).round() as usize;
    let total_slots = (cols * rows).max(1);
    let slot = frame % total_slots;
    let col = slot % cols.max(1);
    let row = slot / cols.max(1);
    [
        tex_offset_uv[0] + col as f32 * tex_scale_uv[0],
        tex_offset_uv[1] + row as f32 * tex_scale_uv[1],
    ]
}

/// Fix `tex_scale_uv` on emitters where the converter produced values that
/// don't form a valid grid for the sprite-sheet frame count.
///
/// Tests whether the current UV scale corresponds to a valid `cols × rows = fc`
/// grid.  If the scale wraps to a different number of cells than there are
/// frames, the converter almost certainly produced a wrong value (e.g.
/// `[1.0, 1.0]` full-texture fallback, `[1.0, 1/fc]` vertical-strip guess, or
/// `TexScrollAnim.uv_scale` for scrolling mode).  In that case the correct
/// grid is inferred from the texture dimensions via `infer_grid_layout()`.
///
/// Also fixes `tex2_scale_uv` (slot-2 texture) using the same heuristic.
pub fn fix_tex_scale_uv(emitter: &mut EmitterDef, bntx_textures: &[TextureRes]) {
    let apply_uv_div = |scale_uv: &mut [f32; 2], fc: usize| -> bool {
        let div_x = emitter.tex_uv_div[0];
        let div_y = emitter.tex_uv_div[1];
        if div_x <= 1 || div_y <= 1 {
            return false;
        }
        let cols = div_x as usize;
        let rows = div_y as usize;
        if cols * rows < fc.max(1) {
            return false;
        }
        let su = 1.0 / div_x as f32;
        let sv = 1.0 / div_y as f32;
        if (scale_uv[0] - su).abs() > 0.001 || (scale_uv[1] - sv).abs() > 0.001 {
            eprintln!(
                "[FIX_UV] tex_uv_div {}×{} fc={}: tex_scale_uv=[{}, {}] (was [{}, {}])",
                cols, rows, fc, su, sv, scale_uv[0], scale_uv[1]
            );
            *scale_uv = [su, sv];
        }
        true
    };
    let fix_one = |scale_uv: &mut [f32; 2], pat_frame_count: usize| {
        let fc = pat_frame_count;
        if fc <= 1 { return; }
        if apply_uv_div(scale_uv, fc) {
            return;
        }
        let Some(tex) = bntx_textures.get(emitter.texture_index as usize) else { return; };
        let cur_cols = (1.0 / scale_uv[0].max(0.001)).round() as usize;
        let cur_rows = (1.0 / scale_uv[1].max(0.001)).round() as usize;
        if cur_cols * cur_rows == fc && cur_cols > 0 && cur_rows > 0 { return; }
        let (cols, rows) = infer_grid_layout(tex.width, tex.height, fc);
        let su = 1.0 / cols as f32;
        let sv = 1.0 / rows as f32;
        if (scale_uv[0] - su).abs() > 0.001 || (scale_uv[1] - sv).abs() > 0.001 {
            eprintln!("[FIX_UV] tex='{}' {}x{} fc={}: inferred {}×{} grid → tex_scale_uv=[{}, {}] (was [{}, {}], cur_grid={}×{})",
                tex.tex_name, tex.width, tex.height, fc, cols, rows, su, sv,
                scale_uv[0], scale_uv[1], cur_cols, cur_rows);
            *scale_uv = [su, sv];
        }
    };
    fix_one(&mut emitter.tex_scale_uv, emitter.tex_pat_frame_count);
    fix_one(&mut emitter.tex2_scale_uv, emitter.tex2_pat_frame_count);
    if emitter.tex_pat_frame_count > 1
        && (emitter.tex_scale_uv[0] - 1.0).abs() < 0.001
        && (emitter.tex_scale_uv[1] - 1.0).abs() < 0.001
        && crate::fx_env::fx_viewport_log_enabled()
    {
        eprintln!(
            "[ATLAS-UV] emitter '{}' fc={} tex_scale_uv still [1,1] after fix_tex_scale_uv (uv_div={:?})",
            emitter.name,
            emitter.tex_pat_frame_count,
            emitter.tex_uv_div,
        );
    }
}

impl PtclFile {
    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 32 {
            anyhow::bail!("PTCL data too short: {} bytes", data.len());
        }

        crate::effect_converter::parse_embedded_ptcl(data)
    }
}

/// Sample a color from a color key table at normalized time `t` (0..1).
/// Falls back to white if the table is empty.
pub fn sample_color_pub(keys: &[ColorKey], t: f32) -> [f32; 4] {
    let v = sample_color(keys, t);
    [v.x, v.y, v.z, v.w]
}

fn sample_color(keys: &[ColorKey], t: f32) -> Vec4 {
    if keys.is_empty() {
        return Vec4::ONE;
    }
    if keys.len() == 1 {
        let k = &keys[0];
        return Vec4::new(k.r, k.g, k.b, k.a);
    }
    // Find the first and last valid keys by max/min frame.
    // Keys may contain zero-initialized padding entries (frame=0, r=g=b=a=0)
    // at the end of the array, so we use actual extreme frame values instead
    // of assuming positional first/last.
    let first = keys.iter().min_by(|a, b| a.frame.partial_cmp(&b.frame).unwrap()).unwrap();
    let last = keys.iter().max_by(|a, b| a.frame.partial_cmp(&b.frame).unwrap()).unwrap();
    let first_frame = first.frame;
    let last_frame = last.frame;
    // At or before min frame → return the corresponding key's color
    if t <= first_frame {
        return Vec4::new(first.r, first.g, first.b, first.a);
    }
    // At or after max frame → return the corresponding key's color
    if t >= last_frame {
        return Vec4::new(last.r, last.g, last.b, last.a);
    }
    // Sort keys by frame for correct bracketing
    let mut sorted: Vec<&ColorKey> = keys.iter().collect();
    sorted.sort_by(|a, b| a.frame.partial_cmp(&b.frame).unwrap());
    // Find the two bracketing keys and linearly interpolate
    for i in 0..sorted.len() - 1 {
        let a = sorted[i];
        let b = sorted[i + 1];
        if t >= a.frame && t <= b.frame {
            let range = (b.frame - a.frame).max(0.0001);
            let s = if range <= 0.0 { 0.0 } else { (t - a.frame) / range };
            return Vec4::new(
                a.r + (b.r - a.r) * s,
                a.g + (b.g - a.g) * s,
                a.b + (b.b - a.b) * s,
                a.a + (b.a - a.a) * s,
            );
        }
    }
    Vec4::ONE
}
/// Build an AnimKey3v4k from a sorted list of (time, value) pairs.
/// Handles 0, 1, 2, or N keys safely without panicking on NaN/inf.
/// Build an AnimKey3v4k from a sorted list of (time, value) pairs.
/// Handles 0, 1, 2, or N keys safely without panicking on NaN/inf.
fn build_anim_key(akeys: &[(f32, f32)]) -> AnimKey3v4k {
    match akeys.len() {
        0 => AnimKey3v4k::default(),
        1 => AnimKey3v4k {
            start_value: akeys[0].1,
            start_diff: 0.0,
            end_diff: -akeys[0].1,
            time2: 0.5,
            time3: 0.8,
        },
        2 => {
            let t1 = akeys[1].0.max(0.001).min(0.998);
            let t2 = (t1 + 0.001).min(0.999);
            AnimKey3v4k {
                start_value: akeys[0].1,
                start_diff: akeys[1].1 - akeys[0].1,
                end_diff: -akeys[1].1,
                time2: t1,
                time3: t2,
            }
        }
        _ => {
            let mid = akeys.len() / 2;
            let t2 = akeys[mid].0.max(0.001).min(0.997);
            let t3 = akeys[akeys.len() - 2].0.max(t2 + 0.001).min(0.999);
            AnimKey3v4k {
                start_value: akeys[0].1,
                start_diff: akeys[1].1 - akeys[0].1,
                end_diff: akeys[akeys.len()-1].1 - akeys[akeys.len()-2].1,
                time2: t2,
                time3: t3,
            }
        }
    }
}

/// Sample a color key table at normalized time `t`, clamping `t` to [0.0, 1.0]
/// before sampling to prevent NaN propagation (Req 11.1).
/// - Empty table → `Vec4::ONE` (white)
/// - Single-entry table → that entry's color for all t
/// - Multi-entry table → linearly interpolate between bracketing ColorKey entries
pub fn sample_color_or_white(keys: &[ColorKey], t: f32) -> Vec4 {
    let t_clamped = t.clamp(0.0, 1.0);
    let v = sample_color(keys, t_clamped);
    // If the entire key table samples to black, return white so this table
    // acts as a multiplicative identity in the color combiner (c0 × c1).
    // This handles the case where the last valid key has value 0 at t=1,
    // preventing the particle from turning black at end-of-life.
    if v == Vec4::ZERO { Vec4::ONE } else { v }
}

/// Sample a color key table for alpha at normalized time `t`.
/// Same as sample_color_or_white but WITHOUT the zero→white fallback:
/// for alpha, zero is a legitimate value (transparent).
pub fn sample_alpha(keys: &[ColorKey], t: f32) -> f32 {
    let t_clamped = t.clamp(0.0, 1.0);
    sample_color(keys, t_clamped).x
}

/// CPU particle simulation ───────────────────────────────────────────────────

/// A single live particle.
#[derive(Debug, Clone)]
pub struct Particle {
    pub position: Vec3,
    pub velocity: Vec3,
    pub age: f32,
    pub lifetime: f32,
    pub color: Vec4,
    /// Sampled color0 RGB before combiner (for child inheritance).
    pub color0_rgb: [f32; 3],
    /// Sampled color1 RGB before combiner (for child inheritance).
    pub color1_rgb: [f32; 3],
    pub alpha0_live: f32,
    pub alpha1_live: f32,
    pub color_scale_live: f32,
    pub draw_path: u32,
    pub pre_draw: bool,
    /// Parent emitter index within the same set (child inheritance); used for pre_draw sibling ordering.
    pub parent_emitter_idx: Option<usize>,
    pub inst_start_frame: f32,
    pub inherit: Option<ParticleInheritState>,
    pub size: f32,
    pub rotation: f32,
    pub rotation_speed: f32,
    pub emitter_set_idx: usize,
    pub emitter_idx: usize,
    /// Spawn offset in emitter-local space (for bone/emitter re-attachment).
    pub local_offset: Vec3,
    pub bone_name: String,
    pub inst_offset: Vec3,
    pub inst_rotation: Vec3,
    #[allow(dead_code)]
    pub texture_idx: usize,
    #[allow(dead_code)]
    pub blend_type: BlendType,
    /// Per-particle UV offset (slot 0; initialized to emitter.tex_offset_uv)
    pub tex_offset: [f32; 2],
    /// Per-particle UV offset for slot 1 (indirect / alpha).
    pub indirect_tex_offset: [f32; 2],
    /// Per-particle UV offset for slot 2.
    pub tex2_tex_offset: [f32; 2],
    /// Live UV scale for slot 0 (scroll + IsScale / EASL path).
    pub tex_scale_live: [f32; 2],
    /// UV-space scroll rotation angle (radians), separate from billboard rotation.
    pub tex_scroll_angle: f32,
    /// Random pattern phase offset (IsPatAnimLoopRandom).
    pub pat_phase_offset: f32,
    /// Fixed flipbook frame for PatternAnimType::RANDOM (chosen at spawn).
    pub pat_fixed_frame: Option<usize>,
    /// Crossfade blend fraction between flipbook frames (slot 0).
    pub pat_blend: f32,
    /// Atlas UV delta to the next flipbook cell for crossfade sampling.
    pub pat_next_uv_delta: [f32; 2],
    /// Per-particle UV offsets for TextureAnim3–5 slots.
    pub tex_extra_offsets: [[f32; 2]; 3],
    /// Deterministic random seed for reproducible randomness
    pub seed: u64,
    /// Per-axis spawn rotation offset from EmitterInfo.RotateRand* (radians).
    pub rotation_rand: Vec3,
}

impl Particle {
    #[allow(dead_code)]
    pub fn life_t(&self) -> f32 {
        if self.lifetime <= 0.0 { 1.0 } else { (self.age / self.lifetime).clamp(0.0, 1.0) }
    }
    pub fn is_dead(&self) -> bool { self.age >= self.lifetime }
}

/// Tracks fractional emission accumulator per active emitter instance.
#[derive(Debug, Clone)]
pub struct EmitterInstance {
    #[allow(dead_code)]
    emitter_set_idx: usize,
    #[allow(dead_code)]
    emitter_idx: usize,
    bone_name: String,
    /// Local offset from the bone origin (in bone-local space, applied as world translation)
    offset: Vec3,
    /// ACMD-specified rotation (Euler angles in radians, ZYX order) applied at spawn time.
    rotation: Vec3,
    start_frame: f32,
    end_frame: f32,
    emit_accum: f32,
    /// Prevents re-firing one-time burst emitters after the first burst frame.
    pub burst_fired: bool,
    /// Previous emitter world position for distance-based emission.
    emit_dist_prev_pos: Vec3,
    emit_dist_prev_pos_set: bool,
    /// Fractional distance accumulator (emitDistVessel).
    emit_dist_vessel: f32,
    /// Previous emitter world origin for EmVelInherit motion delta.
    prev_world_pos: Vec3,
    prev_world_pos_set: bool,
    /// Death-only child emitters track bone motion but do not emit continuously.
    death_only: bool,
}

impl EmitterInstance {
    pub fn emitter_key(&self) -> (usize, usize) {
        (self.emitter_set_idx, self.emitter_idx)
    }

    pub fn bone_name(&self) -> &str {
        &self.bone_name
    }

    pub fn offset(&self) -> Vec3 {
        self.offset
    }

    pub fn rotation(&self) -> Vec3 {
        self.rotation
    }

    pub fn effect_local_frame(&self, target_frame: f32) -> f32 {
        target_frame - self.start_frame
    }

    pub fn start_frame(&self) -> f32 {
        self.start_frame
    }

    /// Test shim: exposes emit_accum for unit tests.
    #[cfg(test)]
    pub fn emit_accum_test(&self) -> f32 {
        self.emit_accum
    }
}

/// Reconstruct an emitter instance from a dying parent particle when the parent
/// instance was already removed from `active_emitters`.
fn instance_from_particle(dead: &Particle) -> EmitterInstance {
    EmitterInstance {
        emitter_set_idx: dead.emitter_set_idx,
        emitter_idx: dead.emitter_idx,
        bone_name: dead.bone_name.clone(),
        offset: dead.inst_offset,
        rotation: dead.inst_rotation,
        start_frame: dead.inst_start_frame,
        end_frame: f32::MAX,
        emit_accum: 0.0,
        burst_fired: false,
        emit_dist_prev_pos: Vec3::ZERO,
        emit_dist_prev_pos_set: false,
        emit_dist_vessel: 0.0,
        prev_world_pos: Vec3::ZERO,
        prev_world_pos_set: false,
        death_only: false,
    }
}

/// Resolve the parent emitter instance for child death spawning.
fn instance_for_child_spawn<'a>(
    dead: &Particle,
    active: &'a [EmitterInstance],
) -> std::borrow::Cow<'a, EmitterInstance> {
    if let Some(inst) = active.iter().find(|i| {
        i.emitter_set_idx == dead.emitter_set_idx && i.emitter_idx == dead.emitter_idx
    }) {
        return std::borrow::Cow::Borrowed(inst);
    }
    std::borrow::Cow::Owned(instance_from_particle(dead))
}

/// Build one particle at spawn time (shared by continuous emitters and child chains).
fn build_spawned_particle(
    emitter: &EmitterDef,
    inst: &EmitterInstance,
    emitter_set_idx: usize,
    emitter_idx: usize,
    bone_mat: Mat4,
    effect_t: f32,
    seed: usize,
    index: usize,
    count: usize,
    position_override: Option<Vec3>,
    inherit_from: Option<&Particle>,
    mesh_ctx: Option<&SpawnMeshContext<'_>>,
    emitter_motion_velocity: Vec3,
) -> Particle {
    let world_mat = compute_emitter_world_mat(emitter, inst, bone_mat, effect_t);
    let local_spawn =
        volume_local_spawn_pos(emitter, seed, index, count, mesh_ctx) + spawn_jitter(emitter, seed);
    let position = position_override.unwrap_or_else(|| world_mat.transform_point3(local_spawn));
    let local_offset = world_mat.inverse().transform_point3(position);

    let mut velocity = compute_particle_velocity(
        emitter,
        seed,
        index,
        count,
        {
            let emitter_mat = build_emitter_trs_at(emitter, effect_t);
            let (_, emitter_rot_quat, _) = emitter_mat.to_scale_rotation_translation();
            Mat4::from_quat(emitter_rot_quat)
        },
        local_spawn,
        emitter_motion_velocity,
    );

    let c0_spawn = sample_color(&emitter.color0, 0.0);
    let c1_spawn = if !emitter.color1.is_empty() {
        sample_color(&emitter.color1, 0.0)
    } else {
        Vec4::ONE
    };
    let mut a0_spawn = if !emitter.alpha0_keys.is_empty() {
        sample_color_or_white(&emitter.alpha0_keys, 0.0).x
    } else {
        emitter.alpha0.sample(0.0)
    };
    let mut a1_spawn = if !emitter.alpha1_keys.is_empty() {
        sample_color_or_white(&emitter.alpha1_keys, 0.0).x
    } else {
        emitter.alpha1.sample(0.0)
    };
    let mut size = {
        let base_size = emitter.scale * emitter.scale_anim.sample(0.0);
        let rf = rand_factor(seed.wrapping_add(7));
        (base_size * (1.0 + rf * emitter.scale_random)).max(0.01)
    };
    let rot_rand = spawn_rotation_rand(emitter, seed);
    let mut rotation = emitter.rotation_init + seed as f32 * emitter.rotation_init_random;

    let mut inherit = None;
    if let Some(parent) = inherit_from {
        (size, rotation, velocity, inherit) = apply_child_inheritance(
            &emitter.child_inheritance,
            parent,
            size,
            rotation,
            velocity,
        );
    }

    let parent_alpha_lookup = inherit_from.map(|parent| {
        let mut m = HashMap::new();
        m.insert(
            (parent.seed, parent.emitter_set_idx, parent.emitter_idx),
            (parent.alpha0_live, parent.alpha1_live),
        );
        m
    });
    let (c0_arr, c1_arr, a0, a1) = apply_inherit_channels(
        [c0_spawn.x, c0_spawn.y, c0_spawn.z, c0_spawn.w],
        [c1_spawn.x, c1_spawn.y, c1_spawn.z, c1_spawn.w],
        a0_spawn,
        a1_spawn,
        inherit.as_ref(),
        parent_alpha_lookup.as_ref(),
        true,
    );
    let color = combine_particle_channels(c0_arr, c1_arr, a0, a1, &emitter.combiner);

    let draw_path = inherit
        .as_ref()
        .and_then(|i| i.draw_path)
        .unwrap_or(emitter.draw_path);
    let pre_draw = inherit.as_ref().map(|i| i.pre_draw).unwrap_or(false);
    let parent_emitter_idx = inherit.as_ref().map(|i| i.parent_emitter_idx);

    let mut particle = Particle {
        position,
        velocity,
        age: 0.0,
        lifetime: {
            let rf = rand_factor(seed.wrapping_add(1));
            let lf = 1.0 + rf * emitter.lifetime_random;
            emitter.lifetime * lf.max(0.0)
        },
        color,
        color0_rgb: [c0_arr[0], c0_arr[1], c0_arr[2]],
        color1_rgb: [c1_arr[0], c1_arr[1], c1_arr[2]],
        alpha0_live: a0,
        alpha1_live: a1,
        color_scale_live: emitter.color_scale,
        draw_path,
        pre_draw,
        parent_emitter_idx,
        inst_start_frame: inst.start_frame(),
        inherit,
        size,
        rotation,
        rotation_speed: emitter.rotation_speed,
        emitter_set_idx,
        emitter_idx,
        local_offset,
        bone_name: inst.bone_name().to_string(),
        inst_offset: inst.offset(),
        inst_rotation: inst.rotation(),
        texture_idx: 0,
        blend_type: emitter.blend_type,
        tex_offset: emitter.tex_offset_uv,
        indirect_tex_offset: emitter.indirect_tex_offset_uv,
        tex2_tex_offset: emitter.tex2_offset_uv,
        tex_scale_live: emitter.tex_scale_uv,
        tex_scroll_angle: 0.0,
        pat_phase_offset: 0.0,
        pat_fixed_frame: None,
        pat_blend: 0.0,
        pat_next_uv_delta: [0.0, 0.0],
        tex_extra_offsets: [[0.0, 0.0]; 3],
        seed: seed as u64,
        rotation_rand: rot_rand,
    };
    init_particle_uv_at_spawn(&mut particle, emitter);
    particle
}

/// Local effect frame when this emitter first spawns particles.
pub fn emitter_first_burst_local_frame(emitter: &EmitterDef) -> u32 {
    if emitter.is_one_time && emitter.emission_timing > 0 {
        emitter.emission_timing
    } else {
        emitter.emission_start
    }
}

/// True when `local_frame` is inside the emitter's emission window.
pub fn emission_window_contains(emitter: &EmitterDef, local_frame: f32) -> bool {
    let start = emitter.emission_start as f32;
    if local_frame < start {
        return false;
    }
    if emitter.emission_duration == 0 {
        return true;
    }
    local_frame < start + emitter.emission_duration as f32
}

/// Earliest global timeline frame where any root emitter would emit for a spawn call.
pub fn earliest_particle_frame_for_spawn(
    effect_name: &str,
    active_start: u32,
    eff_index: &EffIndex,
    ptcl: &PtclFile,
) -> Option<u32> {
    let name_lower = effect_name.to_lowercase();
    let set_idx = eff_index
        .handles
        .get(effect_name)
        .or_else(|| eff_index.handles.get(&name_lower))
        .copied()
        .filter(|&idx| idx >= 0)? as usize;
    let set = ptcl.emitter_sets.get(set_idx)?;
    let local = set
        .emitters
        .iter()
        .filter(|e| !e.child_inheritance.spawn_from_parent_particle)
        .map(emitter_first_burst_local_frame)
        .min()?;
    Some(active_start.saturating_add(local))
}

/// ACMD spawn/end frames for particle emitters.
///
/// Non-following `Effect()` calls set `active_end == active_start` (the script frame only).
/// PTCL emission continues for `emission_timing + duration + particle lifetime` after that.
pub fn acmd_spawn_window(
    effect_name: &str,
    active_start: u32,
    active_end: u32,
    eff_index: &EffIndex,
    ptcl: &PtclFile,
) -> (f32, f32) {
    let start = active_start as f32;
    let mut end = if active_end >= 9999 {
        9999.0
    } else {
        active_end as f32
    };
    if end <= start {
        let name_lower = effect_name.to_lowercase();
        let runtime = eff_index
            .handles
            .get(effect_name)
            .or_else(|| eff_index.handles.get(&name_lower))
            .copied()
            .filter(|&idx| idx >= 0)
            .and_then(|idx| ptcl.emitter_sets.get(idx as usize))
            .map(|set| {
                set.emitters
                    .iter()
                    .filter(|e| !e.child_inheritance.spawn_from_parent_particle)
                    .map(|e| {
                        let burst = emitter_first_burst_local_frame(e) as f32;
                        burst + e.emission_duration as f32 + e.lifetime + e.lifetime_random
                    })
                    .fold(0.0f32, f32::max)
                    .max(1.0)
            })
            .unwrap_or(9999.0);
        end = start + runtime;
    }
    (start, end)
}

/// The full CPU particle system state.
#[derive(Debug, Default)]
pub struct ParticleSystem {
    pub particles: Vec<Particle>,
    pub active_emitters: Vec<EmitterInstance>,
    last_frame: f32,
}

/// Deterministic scalar in [-1.0, 1.0] derived from an integer seed.
/// Uses a cheap integer hash (xorshift-style) mapped to f32.
fn rand_factor(seed: usize) -> f32 {
    let h = seed.wrapping_mul(2654435761).wrapping_add(0x9e3779b9);
    let h = h ^ (h >> 16);
    let h = h.wrapping_mul(0x45d9f3b);
    let h = h ^ (h >> 16);
    (h as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
}

impl ParticleSystem {
    /// True if the system is in its initial reset state (no frames simulated yet).
    pub fn is_reset(&self) -> bool {
        self.last_frame < 0.0
    }

    /// The last frame the system was stepped to.
    pub fn last_frame(&self) -> f32 {
        self.last_frame
    }

    pub fn reset(&mut self) {
        self.particles.clear();
        self.active_emitters.clear();
        self.last_frame = -1.0;
    }

    /// Spawn an emitter set for a given effect call.
    pub fn spawn_effect(
        &mut self,
        effect_name: &str,
        bone_name: &str,
        offset: Vec3,
        rotation: Vec3,
        start_frame: f32,
        end_frame: f32,
        eff_index: &EffIndex,
        ptcl: &PtclFile,
    ) {
        let set_handle = eff_index.handles.get(effect_name)
            .or_else(|| eff_index.handles.get(&effect_name.to_lowercase()))
            .copied();
        let Some(set_handle) = set_handle else {
            eprintln!("[SPAWN] MISS '{effect_name}' — handles: {:?}", eff_index.handles.keys().take(5).collect::<Vec<_>>());
            return
        };
        if set_handle < 0 {
            eprintln!("[SPAWN] handle={set_handle} < 0 for '{effect_name}'");
            return;
        }
        let set_idx = set_handle as usize;
        if ptcl.emitter_sets.is_empty() || set_idx >= ptcl.emitter_sets.len() {
            eprintln!("[SPAWN] SKIP '{effect_name}' set_idx={set_idx} out of range (have {})", ptcl.emitter_sets.len());
            return;
        }
        eprintln!("[SPAWN] OK '{effect_name}' -> set_idx={set_idx} emitters={}", ptcl.emitter_sets[set_idx].emitters.len());
        let set = &ptcl.emitter_sets[set_idx];
        for (emitter_idx, emitter) in set.emitters.iter().enumerate() {
            let death_only = emitter.child_inheritance.spawn_from_parent_particle;
            self.active_emitters.push(EmitterInstance {
                emitter_set_idx: set_idx,
                emitter_idx,
                bone_name: bone_name.to_string(),
                offset,
                rotation,
                start_frame,
                end_frame,
                emit_accum: 0.0,
                burst_fired: false,
                emit_dist_prev_pos: Vec3::ZERO,
                emit_dist_prev_pos_set: false,
                emit_dist_vessel: 0.0,
                prev_world_pos: Vec3::ZERO,
                prev_world_pos_set: false,
                death_only,
            });
        }
    }

    /// Advance simulation to `target_frame`, stepping from `last_frame`.
    /// `bone_matrices` provides world transforms for bone attachment.
    pub fn step(
        &mut self,
        target_frame: f32,
        bone_matrices: &HashMap<String, Mat4>,
        ptcl: &PtclFile,
    ) {
        // Scrub rewind: caller resets/re-spawns before stepping backwards. Ignore tiny
        // float drift (wall clock vs integer catch-up) so we don't wipe live particles.
        if target_frame + 0.5 < self.last_frame {
            self.particles.clear();
        }

        let dt = if self.last_frame < 0.0 {
            // First step — treat as a single frame advance
            1.0f32
        } else {
            (target_frame - self.last_frame).max(0.0)
        };
        self.last_frame = target_frame;

        if crate::fx_debug_enabled() && !self.active_emitters.is_empty() {
            let first_p = self.particles.first().map(|p| (p.position, p.velocity, p.rotation));
            eprintln!("[STEP] frame={target_frame} dt={dt:.3} active={} particles={} {:?}",
                self.active_emitters.len(), self.particles.len(), first_p);
        }

        // Skip emission when dt=0 (paused or duplicate step) — only integrate existing particles.
        // This prevents continuous emitters from over-firing when the simulation is stalled.
        // Still emit when emitters are active but no particles exist yet (initial burst after spawn).
        let needs_initial_emit = self.particles.is_empty()
            && self.active_emitters.iter().any(|i| !i.death_only);
        let skip_emission = dt <= 0.0 && !needs_initial_emit;

        // Integrate existing particles first, so newly spawned particles this frame
        // start at age=0 and survive until the next frame (fixes lifetime=1 particles
        // being born and killed in the same step).
        let mut died_particles: Vec<Particle> = Vec::new();
        for (pi, p) in self.particles.iter_mut().enumerate() {
            let Some(set) = ptcl.emitter_sets.get(p.emitter_set_idx) else { p.age = p.lifetime; continue };
            let Some(emitter) = set.emitters.get(p.emitter_idx) else { p.age = p.lifetime; continue };

            p.age += dt;
            let safe_accel = if emitter.accel.is_finite() && emitter.accel.length() < 1000.0 {
                emitter.accel
            } else {
                Vec3::ZERO
            };
            p.velocity += safe_accel * dt;
            // Air resistance (nw::eft): geometric per-frame velocity damping applied
            // after gravity. dt is normally 1.0 (fixed 60 Hz step); powf keeps it correct
            // for any non-unit step. air_res == 1.0 is a no-op.
            if emitter.air_res.is_finite()
                && emitter.air_res > 0.0
                && (emitter.air_res - 1.0).abs() > 1e-5
            {
                p.velocity *= emitter.air_res.powf(dt);
            }
            if !emitter.is_update_matrix_by_emit && p.velocity.is_finite() {
                p.position += p.velocity * dt;
            }
            if particle_follows_emitter(emitter) {
                let inst = self.active_emitters.iter().find(|inst| {
                    inst.emitter_key() == (p.emitter_set_idx, p.emitter_idx)
                        && inst.bone_name() == p.bone_name
                });
                if let Some(inst) = inst {
                    let bone_mat = bone_matrices
                        .get(&p.bone_name)
                        .or_else(|| bone_matrices.get(&p.bone_name.to_lowercase()))
                        .or_else(|| bone_matrices.get("top"))
                        .or_else(|| bone_matrices.get("Trans"))
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    let f = inst.effect_local_frame(target_frame);
                    let effect_t = emitter_effect_t(emitter, f);
                    let world_mat = compute_emitter_world_mat(emitter, inst, bone_mat, effect_t);
                    p.position = world_mat.transform_point3(p.local_offset);
                }
            }
            p.rotation += p.rotation_speed * dt;

            let t = (p.age / emitter.lifetime).clamp(0.0, 1.0);

            update_particle_color_channels(p, emitter, None, false);
            let scale_rand =
                1.0 + rand_factor(p.seed.wrapping_add(7) as usize) * emitter.scale_random;
            // Prefer the full 8-key scale table; fall back to the 3v4k approximation.
            let scale_curve = if !emitter.scale_keys.is_empty() {
                sample_alpha(&emitter.scale_keys, t)
            } else {
                emitter.scale_anim.sample(t)
            };
            p.size = (emitter.scale * scale_curve * scale_rand).max(0.01);
            if pi < 2 && crate::fx_debug_enabled() {
                let raw = emitter.scale * emitter.scale_anim.sample(t);
                eprintln!("[SIM] pi={} a={:.1}/{:.0} t={:.4} p=({:.2},{:.2},{:.2}) vel=({:.2},{:.2},{:.2}) sz={:.4} e.sc={:.4} raw={:.4}",
                    pi, p.age, emitter.lifetime, t,
                    p.position.x, p.position.y, p.position.z,
                    p.velocity.x, p.velocity.y, p.velocity.z,
                    p.size, emitter.scale, raw);
            }
            // Texture animation: pattern flipbook vs UV scroll for slots 0–2.
            let slot0 = texture_anim_flags_slot0(emitter);
            advance_uv_slot(
                &mut p.tex_offset,
                &mut p.tex_scale_live,
                &mut p.tex_scroll_angle,
                &slot0,
                emitter.tex_scale_uv,
                emitter.tex_scroll_uv,
                emitter.tex_pat_frame_count,
                &emitter.tex_pat_frame_table,
                emitter.tex_pat_frequency,
                emitter.tex_offset_uv,
                emitter.anim_tex_scale.as_ref(),
                t,
                dt,
                p.pat_phase_offset,
                p.pat_fixed_frame,
            );
            let (cur_frame, next_frame, blend) = pattern_frame_with_crossfade(
                &slot0,
                emitter.tex_pat_frame_count,
                &emitter.tex_pat_frame_table,
                emitter.tex_pat_frequency,
                t,
                p.pat_phase_offset,
                p.pat_fixed_frame,
            );
            p.pat_blend = blend;
            p.pat_next_uv_delta = if blend > 0.0 {
                pattern_crossfade_uv_delta(
                    cur_frame,
                    next_frame,
                    emitter.tex_scale_uv,
                    emitter.tex_offset_uv,
                )
            } else {
                [0.0, 0.0]
            };
            let mut _ind_scale = [1.0, 1.0];
            let mut _ind_angle = 0.0f32;
            advance_uv_slot(
                &mut p.indirect_tex_offset,
                &mut _ind_scale,
                &mut _ind_angle,
                &emitter.indirect_anim,
                emitter.indirect_tex_scale_uv,
                emitter.indirect_scroll_uv,
                emitter.indirect_pat_frame_count,
                &emitter.indirect_pat_frame_table,
                emitter.indirect_pat_frequency,
                emitter.indirect_tex_offset_uv,
                None,
                t,
                dt,
                p.pat_phase_offset,
                None,
            );
            let mut _t2_scale = [1.0, 1.0];
            let mut _t2_angle = 0.0f32;
            advance_uv_slot(
                &mut p.tex2_tex_offset,
                &mut _t2_scale,
                &mut _t2_angle,
                &emitter.tex2_anim,
                emitter.tex2_scale_uv,
                emitter.tex2_scroll_uv,
                emitter.tex2_pat_frame_count,
                &emitter.tex2_pat_frame_table,
                emitter.tex2_pat_frequency,
                emitter.tex2_offset_uv,
                None,
                t,
                dt,
                p.pat_phase_offset,
                None,
            );
            for i in 0..3 {
                if !extra_tex_slot_active(emitter, i) {
                    continue;
                }
                let slot = &emitter.tex_extra_slots[i];
                let mut _scale = [1.0, 1.0];
                let mut _angle = 0.0f32;
                advance_uv_slot(
                    &mut p.tex_extra_offsets[i],
                    &mut _scale,
                    &mut _angle,
                    &emitter.tex_anims_extra[i],
                    slot.scale_uv,
                    slot.scroll_uv,
                    slot.pat_frame_count,
                    &slot.pat_frame_table,
                    slot.pat_frequency,
                    slot.offset_uv,
                    None,
                    t,
                    dt,
                    p.pat_phase_offset,
                    None,
                );
            }
            if p.is_dead() {
                died_particles.push(p.clone());
            }
        }

        // Second pass: each-frame alpha inheritance uses parent alphas updated this frame.
        let parent_alpha_lookup: HashMap<(u64, usize, usize), (f32, f32)> = self
            .particles
            .iter()
            .map(|p| ((p.seed, p.emitter_set_idx, p.emitter_idx), (p.alpha0_live, p.alpha1_live)))
            .collect();
        for p in &mut self.particles {
            let needs_refresh = p
                .inherit
                .as_ref()
                .is_some_and(|inh| inh.alpha0_each_frame || inh.alpha1_each_frame);
            if !needs_refresh {
                continue;
            }
            let Some(set) = ptcl.emitter_sets.get(p.emitter_set_idx) else { continue };
            let Some(emitter) = set.emitters.get(p.emitter_idx) else { continue };
            update_particle_color_channels(p, emitter, Some(&parent_alpha_lookup), true);
        }

        // Spawn child-emitter particles from parent deaths before removing corpses.
        if !skip_emission && !died_particles.is_empty() {
            for dead in &died_particles {
                let Some(set) = ptcl.emitter_sets.get(dead.emitter_set_idx) else { continue };
                let parent_idx = dead.emitter_idx;
                let children: Vec<(usize, EmitterDef)> = child_emitters_for_parent(set, parent_idx)
                    .map(|(idx, e)| (idx, e.clone()))
                    .collect();
                if children.is_empty() {
                    continue;
                }
                let inst = instance_for_child_spawn(dead, &self.active_emitters);
                let bone_mat = bone_matrices
                    .get(inst.bone_name())
                    .or_else(|| bone_matrices.get(&inst.bone_name().to_lowercase()))
                    .or_else(|| bone_matrices.get("top"))
                    .or_else(|| bone_matrices.get("Trans"))
                    .copied()
                    .unwrap_or(Mat4::IDENTITY);
                let f = target_frame - inst.start_frame();
                for (child_idx, child_emitter) in children {
                    let effect_t = emitter_effect_t(&child_emitter, f);
                    let seed = self.particles.len().wrapping_add(child_idx).wrapping_add(dead.seed as usize);
                    self.particles.push(build_spawned_particle(
                        &child_emitter,
                        &inst,
                        dead.emitter_set_idx,
                        child_idx,
                        bone_mat,
                        effect_t,
                        seed,
                        0,
                        1,
                        Some(dead.position),
                        Some(dead),
                        Some(&SpawnMeshContext {
                            primitives: &ptcl.primitives,
                            bfres_models: &ptcl.bfres_models,
                        }),
                        Vec3::ZERO,
                    ));
                }
            }
        }

        // Remove particles that died during integration
        self.particles.retain(|p| !p.is_dead());

        // Now emit new particles — they start at age=0 and live until next frame
        if !skip_emission { for inst in &mut self.active_emitters {
            if inst.death_only { continue; }
            if target_frame < inst.start_frame || target_frame > inst.end_frame { continue; }

            let Some(set) = ptcl.emitter_sets.get(inst.emitter_set_idx) else { continue };
            let Some(emitter) = set.emitters.get(inst.emitter_idx) else { continue };

            // Local frame within the effect (relative to when this emitter was spawned)
            let f = target_frame - inst.start_frame;

            // Emission window gating (Req 6.1–6.5)
            let in_window = emission_window_contains(emitter, f);

            // Get bone world transform for spawn origin
            let bone_mat = bone_matrices.get(&inst.bone_name)
                .or_else(|| bone_matrices.get(&inst.bone_name.to_lowercase()))
                // Common fallbacks when the exact bone isn't in the skeleton
                .or_else(|| bone_matrices.get("top"))
                .or_else(|| bone_matrices.get("Trans"))
                .copied()
                .unwrap_or(Mat4::IDENTITY);

            let effect_t = emitter_effect_t(emitter, f);
            let (_, emitter_rot_quat, _) =
                build_emitter_trs_at(emitter, effect_t).to_scale_rotation_translation();
            let _emitter_rot_mat = Mat4::from_quat(emitter_rot_quat);

            let mesh_ctx = SpawnMeshContext {
                primitives: &ptcl.primitives,
                bfres_models: &ptcl.bfres_models,
            };
            let curr_world_pos = compute_emitter_world_mat(emitter, inst, bone_mat, effect_t)
                .transform_point3(Vec3::ZERO);

            let emitter_motion_velocity = if inst.prev_world_pos_set && dt > 0.0 {
                (curr_world_pos - inst.prev_world_pos) / dt
            } else {
                Vec3::ZERO
            };
            inst.prev_world_pos = curr_world_pos;
            inst.prev_world_pos_set = true;

            if crate::fx_debug_enabled() {
                let spawn_origin = compute_particle_spawn_world_pos(
                    emitter, inst, bone_mat, effect_t, 0, 0, 1, Some(&mesh_ctx),
                );
                let bone_pos = bone_mat.col(3).truncate();
                let is_fallback = !bone_matrices.contains_key(&inst.bone_name)
                    && !bone_matrices.contains_key(&inst.bone_name.to_lowercase());
                eprintln!("[EMIT] bone='{}' (fallback={}) bone_pos=({:.2},{:.2},{:.2}) follow={:?} effect_t={:.3} origin=({:.2},{:.2},{:.2})",
                    inst.bone_name, is_fallback,
                    bone_pos.x, bone_pos.y, bone_pos.z,
                    emitter.follow_type, effect_t,
                    spawn_origin.x, spawn_origin.y, spawn_origin.z);
            }

            let to_emit = if emitter.is_emit_dist_enabled {
                0
            } else if emitter.is_one_time {
                // One-time burst: fire exactly once on the burst frame (Req 7.1–7.4)
                let burst_at = emitter_first_burst_local_frame(emitter) as f32;
                if f >= burst_at && !inst.burst_fired {
                    inst.burst_fired = true;
                    // Treat emission_rate <= 0.0 as 1.0 (Req 11.3 / 7.4)
                    let rate = if emitter.emission_rate <= 0.0 { 1.0 } else { emitter.emission_rate };
                    let n = rate.floor().max(1.0) as usize;
                    if crate::fx_debug_enabled() {
                        eprintln!("[EMIT] one_time burst: f={f} burst_at={burst_at} rate={rate} spawning={n}");
                    }
                    n
                } else {
                    0
                }
            } else if in_window {
                // Normal accumulator-based emission (Req 6.1–6.5)
                // Treat emission_rate <= 0.0 as 1.0 (Req 11.3)
                let base_rate = if emitter.emission_rate <= 0.0 { 1.0 } else { emitter.emission_rate };
                let rate_rf = rand_factor(inst.emit_accum.to_bits() as usize ^ (target_frame.to_bits() as usize));
                let rate = base_rate * (1.0 + rate_rf * emitter.emission_rate_random);
                inst.emit_accum += rate.max(0.0);
                let n = inst.emit_accum.floor() as usize;
                inst.emit_accum -= n as f32;
                let n = n.min(256);
                if n > 0 && crate::fx_debug_enabled() { eprintln!("[EMIT] continuous: f={f} timing={} dur={} rate={rate} spawning={n}", emitter.emission_timing, emitter.emission_duration); }
                n
            } else {
                0
            };

            if emitter.is_emit_dist_enabled && in_window {
                let dist_positions = emit_dist_spawn_batch(emitter, inst, curr_world_pos);
                for (i, world_pos) in dist_positions.into_iter().enumerate() {
                    let seed = self.particles.len() + i;
                    let (set_idx, em_idx) = inst.emitter_key();
                    self.particles.push(build_spawned_particle(
                        emitter,
                        inst,
                        set_idx,
                        em_idx,
                        bone_mat,
                        effect_t,
                        seed,
                        i,
                        1,
                        Some(world_pos),
                        None,
                        Some(&mesh_ctx),
                        emitter_motion_velocity,
                    ));
                }
            }

            for i in 0..to_emit {
                let seed = self.particles.len() + i;
                let (set_idx, em_idx) = inst.emitter_key();
                self.particles.push(build_spawned_particle(
                    emitter,
                    inst,
                    set_idx,
                    em_idx,
                    bone_mat,
                    effect_t,
                    seed,
                    i,
                    to_emit,
                    None,
                    None,
                    Some(&mesh_ctx),
                    emitter_motion_velocity,
                ));
            }
        } } // end skip_emission guard

        // Remove emitters that have passed their full lifecycle (emission window + max particle lifetime).
        // Keep instances while they still have live particles so child death chains can resolve context.
        self.active_emitters.retain(|inst| {
            let f = target_frame - inst.start_frame;
            let has_live_particles = self.particles.iter().any(|p| {
                p.emitter_set_idx == inst.emitter_set_idx && p.emitter_idx == inst.emitter_idx
            });
            if has_live_particles {
                return true;
            }
            let Some(set) = ptcl.emitter_sets.get(inst.emitter_set_idx) else { return false };
            let Some(emitter) = set.emitters.get(inst.emitter_idx) else { return false };
            let emit_end = emitter.emission_start as f32
                + (emitter.emission_duration as f32).max(1.0);
            let burst_end = emitter_first_burst_local_frame(emitter) as f32 + 1.0;
            let full_end = emit_end.max(burst_end) + emitter.lifetime + emitter.lifetime_random;
            f < full_end
        });

        if crate::fx_debug_enabled() {
            eprintln!("[STEP_END] frame={target_frame} particles_after_retain={} active_emitters={}", self.particles.len(), self.active_emitters.len());
        }
    }
}

// ── Sword trail simulation ────────────────────────────────────────────────────

/// One recorded position sample for a sword trail.
#[derive(Debug, Clone, Copy)]
pub struct TrailSample {
    pub tip: Vec3,
    pub base: Vec3,
    pub age: f32,
}

/// Sword trail state for one active AFTER_IMAGE effect.
#[derive(Debug, Clone)]
pub struct SwordTrail {
    pub effect_name: String,
    pub tip_bone: String,
    pub base_bone: String,
    /// NVN `EmitterInfo.DrawPath` for this trail (composited with matching particle path).
    pub draw_path: u32,
    pub samples: Vec<TrailSample>,
    pub max_samples: usize,
    pub active: bool,
    pub blend_type: BlendType,
    /// RGBA color sampled from the emitter's color table
    pub color: [f32; 4],
}

impl SwordTrail {
    pub fn new(
        effect_name: &str,
        tip_bone: &str,
        base_bone: &str,
        draw_path: u32,
        color: [f32; 4],
        blend_type: BlendType,
    ) -> Self {
        Self {
            effect_name: effect_name.to_string(),
            tip_bone: tip_bone.to_string(),
            base_bone: base_bone.to_string(),
            draw_path,
            samples: Vec::new(),
            max_samples: 20,
            active: true,
            blend_type,
            color,
        }
    }

    pub fn record(&mut self, bone_matrices: &HashMap<String, Mat4>) {
        if !self.active { return; }
        let tip_mat = bone_matrices.get(&self.tip_bone)
            .or_else(|| bone_matrices.get(&self.tip_bone.to_lowercase()))
            .copied().unwrap_or(Mat4::IDENTITY);
        let base_mat = bone_matrices.get(&self.base_bone)
            .or_else(|| bone_matrices.get(&self.base_bone.to_lowercase()))
            .copied().unwrap_or(Mat4::IDENTITY);

        // Age existing samples
        for s in &mut self.samples { s.age += 1.0; }
        // Remove old samples
        self.samples.retain(|s| s.age < self.max_samples as f32);

        self.samples.insert(0, TrailSample {
            tip: tip_mat.col(3).truncate(),
            base: base_mat.col(3).truncate(),
            age: 0.0,
        });
    }

    pub fn stop(&mut self) { self.active = false; }
}

/// All active sword trails.
#[derive(Debug, Default)]
pub struct TrailSystem {
    pub trails: Vec<SwordTrail>,
}

impl TrailSystem {
    pub fn reset(&mut self) { self.trails.clear(); }

    pub fn start_trail(
        &mut self,
        effect_name: &str,
        tip_bone: &str,
        base_bone: &str,
        draw_path: u32,
        color: [f32; 4],
        blend_type: BlendType,
    ) {
        // Remove any existing trail for this effect
        self.trails.retain(|t| t.effect_name != effect_name);
        self.trails.push(SwordTrail::new(
            effect_name,
            tip_bone,
            base_bone,
            draw_path,
            color,
            blend_type,
        ));
    }

    pub fn stop_trail(&mut self, effect_name: &str) {
        for t in &mut self.trails { if t.effect_name == effect_name { t.stop(); } }
    }

    pub fn step(&mut self, bone_matrices: &HashMap<String, Mat4>) {
        for trail in &mut self.trails { trail.record(bone_matrices); }
        self.trails.retain(|t| t.active || !t.samples.is_empty());
    }
}

#[cfg(test)]
mod uv_tests {
    use super::*;

    #[test]
    fn fix_tex_scale_uv_infers_grid_from_texture_dims() {
        let mut emitter = EmitterDef {
            texture_index: 0,
            tex_scale_uv: [1.0, 1.0],
            tex_pat_frame_count: 4,
            ..Default::default()
        };
        let textures = vec![TextureRes {
            tex_name: "sheet".into(),
            width: 256,
            height: 64,
            ..Default::default()
        }];
        fix_tex_scale_uv(&mut emitter, &textures);
        assert_eq!(emitter.tex_scale_uv, [0.25, 1.0]);
    }

    #[test]
    fn fix_tex_scale_uv_prefers_uv_div_from_export() {
        let mut emitter = EmitterDef {
            texture_index: 0,
            tex_scale_uv: [1.0, 1.0],
            tex_uv_div: [4, 4],
            tex_pat_frame_count: 8,
            ..Default::default()
        };
        let textures = vec![TextureRes {
            tex_name: "sheet".into(),
            width: 256,
            height: 256,
            ..Default::default()
        }];
        fix_tex_scale_uv(&mut emitter, &textures);
        assert_eq!(emitter.tex_scale_uv, [0.25, 0.25]);
    }

    #[test]
    fn fix_tex_scale_uv_keeps_valid_uv_div_scale() {
        let mut emitter = EmitterDef {
            texture_index: 0,
            tex_scale_uv: [0.25, 0.25],
            tex_uv_div: [4, 4],
            tex_pat_frame_count: 16,
            ..Default::default()
        };
        fix_tex_scale_uv(&mut emitter, &[]);
        assert_eq!(emitter.tex_scale_uv, [0.25, 0.25]);
    }

    #[test]
    fn pattern_frame_index_uses_frequency_and_table() {
        let emitter = EmitterDef {
            tex_pat_frame_count: 3,
            tex_pat_frame_table: vec![2, 0, 1],
            tex_pat_frequency: 2.0,
            ..Default::default()
        };
        assert_eq!(pattern_frame_index(&emitter, 0.0), 2);
        assert_eq!(pattern_frame_index(&emitter, 0.25), 0);
        assert_eq!(pattern_frame_index(&emitter, 0.5), 2);
    }

    #[test]
    fn emitter_uses_tex_pattern_vs_scroll() {
        let pat = EmitterDef {
            tex_pat_frame_count: 3,
            ..Default::default()
        };
        assert!(emitter_uses_tex_pattern(&pat));
        assert!(!emitter_uses_tex_scroll(&pat));

        let scroll = EmitterDef {
            tex_is_scroll: true,
            tex_scroll_uv: [0.1, 0.0],
            ..Default::default()
        };
        assert!(!emitter_uses_tex_pattern(&scroll));
        assert!(emitter_uses_tex_scroll(&scroll));
    }

    #[test]
    fn pattern_anim_type_random_picks_fixed_frame() {
        let anim = TextureAnimFlags {
            pattern_anim_type: pattern_anim_type::RANDOM,
            ..Default::default()
        };
        let (frame, blend) = pattern_frame_at_life(&anim, 4, &[], 1.0, 0.5, 0.0, Some(2));
        assert_eq!(frame, 2);
        assert_eq!(blend, 0.0);
    }

    #[test]
    fn pattern_anim_type_loop_wraps() {
        let anim = TextureAnimFlags {
            pattern_anim_type: pattern_anim_type::LOOP,
            ..Default::default()
        };
        let (frame, _) = pattern_frame_at_life(&anim, 4, &[], 1.0, 0.99, 0.0, None);
        assert_eq!(frame, 3);
    }

    #[test]
    fn pattern_anim_type_clamp_holds_last_frame() {
        let anim = TextureAnimFlags {
            pattern_anim_type: pattern_anim_type::CLAMP,
            ..Default::default()
        };
        let (frame, _) = pattern_frame_at_life(&anim, 4, &[], 1.0, 1.0, 0.0, None);
        assert_eq!(frame, 3);
    }

    #[test]
    fn pattern_crossfade_blend_is_fractional() {
        let anim = TextureAnimFlags {
            pattern_anim_type: pattern_anim_type::LOOP,
            crossfade: true,
            ..Default::default()
        };
        let (_, _, blend) = pattern_frame_with_crossfade(&anim, 4, &[], 1.0, 0.125, 0.0, None);
        assert!(blend > 0.0 && blend < 1.0);
    }

    #[test]
    fn extra_tex_slot_active_when_texture_present() {
        let mut emitter = EmitterDef::default();
        emitter.textures = vec![
            TextureRes::default(),
            TextureRes::default(),
            TextureRes::default(),
            TextureRes::default(),
        ];
        assert!(extra_tex_slot_active(&emitter, 0));
        assert!(!extra_tex_slot_active(&emitter, 1));
    }

    #[test]
    fn effective_tex_scale_respects_is_scale_and_easl() {
        let anim = TextureAnimFlags {
            is_scale: true,
            ..Default::default()
        };
        let track = EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                AnimKeyframe { x: 2.0, y: 0.5, z: 0.0, time: 0.0 },
                AnimKeyframe { x: 2.0, y: 0.5, z: 0.0, time: 1.0 },
            ],
        };
        let scale = effective_tex_scale_uv([0.25, 0.5], &anim, Some(&track), 0.0);
        assert!((scale[0] - 0.5).abs() < 0.001);
        assert!((scale[1] - 0.25).abs() < 0.001);
    }

    #[test]
    fn scroll_uv_angle_not_particle_rotation() {
        let anim = TextureAnimFlags {
            is_rotate: true,
            scroll_rotation: 0.5,
            scroll_rotation_add: 0.1,
            ..Default::default()
        };
        let angle = scroll_uv_angle_at_life(&anim, 0.5, 10.0);
        assert!((angle - 1.0).abs() < 0.001);
    }

    #[test]
    fn inv_rand_u_flips_offset_for_odd_seed() {
        let anim = TextureAnimFlags {
            inv_rand_u: true,
            ..Default::default()
        };
        let (scale, offset) = apply_inv_rand_uv([0.25, 1.0], [0.1, 0.2], &anim, 1);
        assert!(scale[0] < 0.0);
        assert!((offset[0] - 0.9).abs() < 0.001);
    }

    #[test]
    fn build_emitter_trs_applies_ea_translate_at_time() {
        let mut emitter = EmitterDef::default();
        emitter.emitter_offset = Vec3::new(1.0, 0.0, 0.0);
        emitter.anim_translate = Some(EmitterAnimDef {
            enable: true,
            loop_: false,
            randomize_start_frame: false,
            loop_count: 0,
            key_frames: vec![
                AnimKeyframe { x: 0.0, y: 0.0, z: 0.0, time: 0.0 },
                AnimKeyframe { x: 2.0, y: 0.0, z: 0.0, time: 1.0 },
            ],
        });
        let trs = build_emitter_trs_at(&emitter, 0.5);
        let pos = trs.transform_point3(Vec3::ZERO);
        assert!((pos.x - 2.0).abs() < 0.01, "expected x≈2, got {pos:?}");
    }

    #[test]
    fn spawn_world_pos_includes_emitter_offset_and_inst_offset() {
        let mut emitter = EmitterDef::default();
        emitter.emitter_offset = Vec3::new(3.0, 0.0, 0.0);
        let inst = EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::new(0.0, 2.0, 0.0),
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        };
        let pos = compute_particle_spawn_world_pos(
            &emitter, &inst, Mat4::IDENTITY, 0.0, 0, 0, 1, None,
        );
        assert!((pos.x - 3.0).abs() < 0.01 && (pos.y - 2.0).abs() < 0.01,
            "expected (3,2,0), got {pos:?}");
    }

    #[test]
    fn spawn_applies_acmd_instance_rotation() {
        let emitter = EmitterDef::default();
        let inst = EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::new(5.0, 0.0, 0.0),
            rotation: Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2),
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        };
        let pos = compute_particle_spawn_world_pos(
            &emitter, &inst, Mat4::IDENTITY, 0.0, 0, 0, 1, None,
        );
        let expected = (mat_from_euler_zyx(inst.rotation()) * Mat4::from_translation(inst.offset()))
            .transform_point3(Vec3::ZERO);
        assert!(
            (pos - expected).length() < 0.01,
            "spawn pos {pos:?} should match inst R*T {expected:?}"
        );
        assert!(
            expected.y.abs() > 0.01,
            "sanity: Z rotation should move +X offset off the X axis, got {expected:?}"
        );
    }

    #[test]
    fn fill_box_volume_spawns_inside_axes() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::FillBox;
        emitter.volume_radius = Vec3::new(10.0, 2.0, 3.0);
        for seed in 0..32 {
            let p = volume_local_spawn_pos(&emitter, seed, 0, 1, None);
            assert!(p.x.abs() <= 10.0 + 0.001);
            assert!(p.y.abs() <= 2.0 + 0.001);
            assert!(p.z.abs() <= 3.0 + 0.001);
        }
    }

    #[test]
    fn designated_dir_used_when_not_omnidirectional() {
        let mut emitter = EmitterDef::default();
        emitter.use_omnidirectional = false;
        emitter.designated_dir = Vec3::new(0.0, 1.0, 0.0);
        let dir = emit_velocity_direction(&emitter, 0, 0, 1, Mat4::IDENTITY, Vec3::ZERO);
        assert!((dir.y - 1.0).abs() < 0.001, "expected +Y, got {dir:?}");
    }

    #[test]
    fn diffusion_dir_angle_zero_preserves_axis() {
        let mut emitter = EmitterDef::default();
        emitter.use_omnidirectional = false;
        emitter.designated_dir = Vec3::new(0.0, 0.0, 1.0);
        emitter.diffusion_dir_angle = 0.0;
        let dir = emit_velocity_direction(&emitter, 5, 0, 1, Mat4::IDENTITY, Vec3::ZERO);
        assert!((dir.z - 1.0).abs() < 0.001, "expected +Z, got {dir:?}");
    }

    #[test]
    fn diffusion_cone_spreads_off_axis() {
        let axis = Vec3::Z;
        let spread = sample_cone_direction(axis, 90.0_f32.to_radians(), 42);
        assert!(spread.z < 0.99, "90-deg cone should deviate from +Z, got {spread:?}");
        assert!(spread.length() > 0.99);
    }

    #[test]
    fn world_oriented_velocity_ignores_emitter_rotation() {
        let mut emitter = EmitterDef::default();
        emitter.use_omnidirectional = false;
        emitter.designated_dir = Vec3::new(1.0, 0.0, 0.0);
        emitter.is_world_oriented_velocity = true;
        let rot = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let dir = emit_velocity_direction(&emitter, 0, 0, 1, rot, Vec3::ZERO);
        assert!((dir.x - 1.0).abs() < 0.001, "world-oriented should stay +X, got {dir:?}");
    }

    #[test]
    fn xz_diffusion_biases_toward_spawn_quadrant() {
        let base = Vec3::Z;
        let biased = apply_xz_diffusion(base, Vec3::new(1.0, 0.0, 0.0), 2.0);
        assert!(biased.x > 0.5, "XZ diffusion should add +X component, got {biased:?}");
    }

    #[test]
    fn child_inheritance_applies_velocity_scale_and_color() {
        let parent = Particle {
            position: Vec3::new(1.0, 2.0, 3.0),
            velocity: Vec3::new(0.0, 4.0, 0.0),
            age: 10.0,
            lifetime: 20.0,
            color: Vec4::new(0.5, 0.5, 0.5, 0.5),
            color0_rgb: [0.5, 0.5, 0.5],
            color1_rgb: [1.0, 1.0, 1.0],
            alpha0_live: 0.5,
            alpha1_live: 1.0,
            color_scale_live: 1.0,
            draw_path: 2,
            pre_draw: false,
            parent_emitter_idx: None,
            inst_start_frame: 0.0,
            inherit: None,
            size: 2.0,
            rotation: 1.5,
            rotation_speed: 0.0,
            emitter_set_idx: 0,
            emitter_idx: 0,
            local_offset: Vec3::ZERO,
            bone_name: "Trans".to_string(),
            inst_offset: Vec3::ZERO,
            inst_rotation: Vec3::ZERO,
            texture_idx: 0,
            blend_type: BlendType::Add,
            tex_offset: [0.0, 0.0],
            indirect_tex_offset: [0.0, 0.0],
            tex2_tex_offset: [0.0, 0.0],
            tex_scale_live: [1.0, 1.0],
            tex_scroll_angle: 0.0,
            pat_phase_offset: 0.0,
            pat_fixed_frame: None,
            pat_blend: 0.0,
            pat_next_uv_delta: [0.0, 0.0],
            tex_extra_offsets: [[0.0, 0.0]; 3],
            seed: 42,
            rotation_rand: Vec3::ZERO,
        };
        let inh = ChildInheritanceDef {
            inherit_velocity: true,
            inherit_scale: true,
            inherit_rotate: true,
            inherit_color0: true,
            inherit_alpha0: true,
            velocity_rate: 0.5,
            scale_rate: 2.0,
            ..Default::default()
        };
        let (size, rotation, velocity, inherit) = apply_child_inheritance(
            &inh,
            &parent,
            1.0,
            0.0,
            Vec3::ZERO,
        );
        assert!((velocity.y - 2.0).abs() < 0.001);
        assert!((size - 4.0).abs() < 0.001);
        assert!((rotation - 1.5).abs() < 0.001);
        let (c0, c1, a0, a1) = apply_inherit_channels(
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            1.0,
            1.0,
            inherit.as_ref(),
            None,
            true,
        );
        assert!((c0[0] - 0.5).abs() < 0.001 && (a0 - 0.5).abs() < 0.001);
        assert!((c1[0] - 1.0).abs() < 0.001, "color1 channel untouched");
    }

    #[test]
    fn child_inheritance_color1_channel_separate_from_color0() {
        let parent = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            lifetime: 10.0,
            color: Vec4::ONE,
            color0_rgb: [1.0, 0.0, 0.0],
            color1_rgb: [0.0, 0.0, 1.0],
            alpha0_live: 1.0,
            alpha1_live: 1.0,
            color_scale_live: 1.0,
            draw_path: 0,
            pre_draw: false,
            parent_emitter_idx: None,
            inst_start_frame: 0.0,
            inherit: None,
            size: 1.0,
            rotation: 0.0,
            rotation_speed: 0.0,
            emitter_set_idx: 0,
            emitter_idx: 0,
            local_offset: Vec3::ZERO,
            bone_name: "Trans".to_string(),
            inst_offset: Vec3::ZERO,
            inst_rotation: Vec3::ZERO,
            texture_idx: 0,
            blend_type: BlendType::Add,
            tex_offset: [0.0, 0.0],
            indirect_tex_offset: [0.0, 0.0],
            tex2_tex_offset: [0.0, 0.0],
            tex_scale_live: [1.0, 1.0],
            tex_scroll_angle: 0.0,
            pat_phase_offset: 0.0,
            pat_fixed_frame: None,
            pat_blend: 0.0,
            pat_next_uv_delta: [0.0, 0.0],
            tex_extra_offsets: [[0.0, 0.0]; 3],
            seed: 1,
            rotation_rand: Vec3::ZERO,
        };
        let inh = ChildInheritanceDef {
            inherit_color1: true,
            ..Default::default()
        };
        let (_, _, _, inherit) = apply_child_inheritance(&inh, &parent, 1.0, 0.0, Vec3::ZERO);
        let (c0, c1, _, _) = apply_inherit_channels(
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            1.0,
            1.0,
            inherit.as_ref(),
            None,
            true,
        );
        assert!((c0[2] - 1.0).abs() < 0.001, "color0 should stay white");
        assert!((c1[2] - 1.0).abs() < 0.001, "color1 B should inherit parent blue");
        assert!((c1[0] - 0.0).abs() < 0.001);
    }

    #[test]
    fn em_vel_inherit_adds_emitter_motion_to_spawn_velocity() {
        let mut emitter = EmitterDef::default();
        emitter.use_omnidirectional = false;
        emitter.designated_dir = Vec3::Z;
        emitter.initial_speed = 0.0;
        emitter.em_vel_inherit = 1.0;
        let vel = compute_particle_velocity(
            &emitter,
            0,
            0,
            1,
            Mat4::IDENTITY,
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 10.0),
        );
        assert!(
            (vel.z - 10.0).abs() < 0.001,
            "EmVelInherit should add emitter motion, got {vel:?}"
        );

        let mut sys = ParticleSystem::default();
        emitter.emission_rate = 1.0;
        emitter.lifetime = 30.0;
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![emitter],
            }],
            ..Default::default()
        };
        sys.active_emitters.push(EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: true,
            death_only: false,
        });
        let mut bone_mats = HashMap::new();
        bone_mats.insert("Trans".to_string(), Mat4::from_translation(Vec3::new(0.0, 0.0, 10.0)));
        sys.step(1.0, &bone_mats, &ptcl);
        assert!(
            sys.particles.iter().any(|p| p.velocity.z > 5.0),
            "step path should inherit bone motion, got {:?}",
            sys.particles.iter().map(|p| p.velocity).collect::<Vec<_>>()
        );
    }

    #[test]
    fn birth_spawn_child_emitter_emits_at_effect_start() {
        let parent = EmitterDef {
            emission_rate: 0.0,
            lifetime: 30.0,
            ..Default::default()
        };
        let child = EmitterDef {
            emission_rate: 1.0,
            lifetime: 30.0,
            child_inheritance: ChildInheritanceDef {
                parent_emitter_idx: 0,
                spawn_from_parent_particle: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![parent, child],
            }],
            ..Default::default()
        };
        let mut sys = ParticleSystem::default();
        sys.spawn_effect("fx", "Trans", Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &EffIndex {
            handles: [("fx".to_string(), 0i32)].into_iter().collect(),
            ..Default::default()
        }, &ptcl);
        assert_eq!(sys.active_emitters.len(), 2, "birth child should register at effect start");
        let mut bone_mats = HashMap::new();
        bone_mats.insert("Trans".to_string(), Mat4::IDENTITY);
        sys.step(1.0, &bone_mats, &ptcl);
        assert!(
            sys.particles.iter().any(|p| p.emitter_idx == 1),
            "child emitter should spawn particles at effect start"
        );
    }

    #[test]
    fn child_death_spawn_works_after_parent_instance_removed() {
        let parent = EmitterDef {
            emission_rate: 1.0,
            lifetime: 20.0,
            ..Default::default()
        };
        let child = EmitterDef {
            lifetime: 10.0,
            child_inheritance: ChildInheritanceDef {
                spawn_from_parent_particle: true,
                parent_emitter_idx: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![parent, child],
            }],
            ..Default::default()
        };
        let mut sys = ParticleSystem::default();
        sys.spawn_effect("fx", "Trans", Vec3::ZERO, Vec3::ZERO, 0.0, 9999.0, &EffIndex {
            handles: [("fx".to_string(), 0i32)].into_iter().collect(),
            ..Default::default()
        }, &ptcl);
        let mut bone_mats = HashMap::new();
        bone_mats.insert("Trans".to_string(), Mat4::IDENTITY);
        sys.step(1.0, &bone_mats, &ptcl);
        assert!(
            sys.particles.iter().any(|p| p.emitter_idx == 0),
            "parent particle should spawn"
        );
        sys.active_emitters.retain(|i| i.emitter_idx != 0);
        sys.step(25.0, &bone_mats, &ptcl);
        assert!(
            sys.particles.iter().any(|p| p.emitter_idx == 1),
            "child particle should spawn from dead parent without parent instance"
        );
    }

    #[test]
    fn samus_bomb_ptcl_emits_before_explosion_frame() {
        let Some(path) = crate::scratch_dirs::resolve_fighter_eff("samus") else {
            return;
        };
        let eff = EffIndex::from_file(&path).expect("eff");
        let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
        let spawn = ["samus_cshot_bomb", "samus_atk_bomb"]
            .iter()
            .find(|name| eff.handles.contains_key(**name))
            .map(|s| (*s).to_string())
            .or_else(|| {
                eff.handles
                    .keys()
                    .find(|k| k.contains("bomb") || k.contains("Bomb"))
                    .cloned()
            })
            .unwrap_or_else(|| "samus_atk_bomb".to_string());
        let active_start = 4u32;
        let (start, end) = acmd_spawn_window(&spawn, active_start, active_start, &eff, &ptcl);
        let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();

        let mut sys = ParticleSystem::default();
        sys.spawn_effect(
            &spawn,
            "Trans",
            Vec3::ZERO,
            Vec3::ZERO,
            start,
            end,
            &eff,
            &ptcl,
        );
        for f in 0..=10u32 {
            sys.step(f as f32, &bone, &ptcl);
        }
        let early = sys.particles.len();
        assert!(
            early > 0,
            "samus bomb should emit throw/smoke particles by local frame 6 (global ~10), got 0"
        );

        sys.step(64.0, &bone, &ptcl);
        sys.particles.retain(|p| !p.is_dead());
        assert!(
            !sys.particles.is_empty(),
            "samus bomb should still have particles at explosion frame 64"
        );
    }

    #[test]
    fn bomb_early_emitters_use_emission_start_not_timing_as_window() {
        let mut impact = EmitterDef::default();
        impact.is_one_time = true;
        impact.emission_start = 0;
        impact.emission_timing = 0;
        impact.emission_duration = 2;
        impact.emission_rate = 2.0;

        let mut ring = EmitterDef::default();
        ring.is_one_time = true;
        ring.emission_start = 1;
        ring.emission_timing = 60;
        ring.emission_duration = 1;

        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "bomb".into(),
                emitters: vec![impact, ring],
            }],
            ..Default::default()
        };
        let mut eff = EffIndex::default();
        eff.handles.insert("samus_atk_bomb".into(), 0);
        let (start, end) = acmd_spawn_window("samus_atk_bomb", 4, 4, &eff, &ptcl);
        let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();

        let mut sys = ParticleSystem::default();
        sys.spawn_effect(
            "samus_atk_bomb",
            "Trans",
            Vec3::ZERO,
            Vec3::ZERO,
            start,
            end,
            &eff,
            &ptcl,
        );
        for f in 0..=10u32 {
            sys.step(f as f32, &bone, &ptcl);
        }
        assert!(
            !sys.particles.is_empty(),
            "impact/smoke (Start=0 Timing=0) should emit near effect spawn, not wait for Timing=60"
        );
        assert_eq!(
            earliest_particle_frame_for_spawn("samus_atk_bomb", 4, &eff, &ptcl),
            Some(4),
            "first visible burst should be at effect spawn (active_start + Start=0), not frame 64"
        );
    }

    #[test]
    fn acmd_one_shot_spawn_window_covers_late_emission_timing() {
        let mut emitter = EmitterDef::default();
        emitter.is_one_time = true;
        emitter.emission_timing = 60;
        emitter.emission_duration = 1;
        emitter.lifetime = 20.0;
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "bomb".into(),
                emitters: vec![emitter],
            }],
            ..Default::default()
        };
        let mut eff = EffIndex::default();
        eff.handles.insert("samus_atk_bomb".into(), 0);

        let (start, end) = acmd_spawn_window("samus_atk_bomb", 4, 4, &eff, &ptcl);
        assert!(
            end > start + 60.0,
            "one-shot ACMD window must extend past emission_timing=60 (got {start}..{end})"
        );

        let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();
        let mut sys = ParticleSystem::default();
        sys.spawn_effect(
            "samus_atk_bomb",
            "Trans",
            Vec3::ZERO,
            Vec3::ZERO,
            start,
            end,
            &eff,
            &ptcl,
        );
        for f in 0..=64u32 {
            sys.step(f as f32, &bone, &ptcl);
        }
        assert!(
            !sys.particles.is_empty(),
            "burst at local frame 60 (global 64) should spawn particles with extended window"
        );

        let mut closed = ParticleSystem::default();
        closed.spawn_effect(
            "samus_atk_bomb",
            "Trans",
            Vec3::ZERO,
            Vec3::ZERO,
            4.0,
            4.0,
            &eff,
            &ptcl,
        );
        for f in 0..=64u32 {
            closed.step(f as f32, &bone, &ptcl);
        }
        assert!(
            closed.particles.is_empty(),
            "raw ACMD one-frame window (4..4) must not emit at global frame 64"
        );
    }

    #[test]
    fn emitter_start_frame_aligns_emission_window_at_spawn_frame() {
        let mut emitter = EmitterDef::default();
        emitter.emission_rate = 4.0;
        emitter.emission_duration = 5;
        emitter.lifetime = 30.0;
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![emitter],
            }],
            ..Default::default()
        };
        let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();

        let mut late = ParticleSystem::default();
        late.active_emitters.push(EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 45.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        });
        late.step(45.0, &bone, &ptcl);
        assert!(
            !late.particles.is_empty(),
            "start_frame=45 at target=45 should emit (local f=0 inside window)"
        );

        let mut wrong = ParticleSystem::default();
        wrong.active_emitters.push(EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        });
        wrong.step(45.0, &bone, &ptcl);
        assert!(
            wrong.particles.is_empty(),
            "start_frame=0 at target=45 should miss emission window (local f=45)"
        );
    }

    #[test]
    fn bone_follow_reattaches_when_emitter_moves() {
        let mut emitter = EmitterDef::default();
        emitter.is_update_matrix_by_emit = true;
        emitter.emission_rate = 1.0;
        emitter.lifetime = 30.0;
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![emitter],
            }],
            ..Default::default()
        };
        let inst = EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        };
        let mut sys = ParticleSystem::default();
        sys.active_emitters.push(inst);
        let mut bone_mats = HashMap::new();
        bone_mats.insert("Trans".to_string(), Mat4::IDENTITY);
        sys.step(0.0, &bone_mats, &ptcl);
        sys.step(1.0, &bone_mats, &ptcl);
        assert!(
            !sys.particles.is_empty(),
            "expected at least one spawned particle"
        );
        let pos_a = sys.particles[0].position;
        bone_mats.insert("Trans".to_string(), Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)));
        sys.step(2.0, &bone_mats, &ptcl);
        let pos_b = sys.particles[0].position;
        assert!(
            (pos_b.x - pos_a.x - 10.0).abs() < 0.1,
            "expected +10 X follow, {pos_a:?} -> {pos_b:?}"
        );
    }

    #[test]
    fn follow_type_srt_reparents_stationary_particles_without_update_matrix_by_emit() {
        let mut emitter = EmitterDef::default();
        emitter.follow_type = FollowType::Srt;
        emitter.is_update_matrix_by_emit = false;
        emitter.emission_rate = 1.0;
        emitter.lifetime = 60.0;
        emitter.initial_speed = 0.0;
        emitter.speed_random = 0.0;
        let ptcl = PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "fx".into(),
                emitters: vec![emitter],
            }],
            ..Default::default()
        };
        let inst = EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: false,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        };
        let mut sys = ParticleSystem::default();
        sys.active_emitters.push(inst);
        let mut bone_mats = HashMap::new();
        bone_mats.insert("Trans".to_string(), Mat4::IDENTITY);
        sys.step(0.0, &bone_mats, &ptcl);
        sys.step(1.0, &bone_mats, &ptcl);
        let pos_a = sys.particles[0].position;
        bone_mats.insert("Trans".to_string(), Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)));
        sys.step(2.0, &bone_mats, &ptcl);
        let pos_b = sys.particles[0].position;
        assert!(
            (pos_b.x - pos_a.x - 10.0).abs() < 0.1,
            "stationary SRT-following particles should re-parent with the bone: {pos_a:?} -> {pos_b:?}"
        );
    }

    #[test]
    fn rotate_billboard_corner_applies_z_spin() {
        let axes = RotAxisMask { x: false, y: false, z: true };
        let c = rotate_billboard_corner(
            [1.0, 0.0],
            std::f32::consts::FRAC_PI_2,
            1,
            axes,
        );
        assert!((c[0]).abs() < 0.001 && (c[1] - 1.0).abs() < 0.001);
    }

    #[test]
    fn tilt_billboard_basis_applies_x_axis() {
        let axes = RotAxisMask { x: true, y: false, z: false };
        let (right, up) = tilt_billboard_basis(
            Vec3::X,
            Vec3::Y,
            Vec3::new(std::f32::consts::FRAC_PI_2, 0.0, 0.0),
            axes,
        );
        assert!((right.x - 1.0).abs() < 0.001);
        assert!((up.y).abs() < 0.001 && (up.z - 1.0).abs() < 0.001);
    }

    #[test]
    fn billboard_pivot_bias_covers_offset_types() {
        assert_eq!(billboard_pivot_bias(0), [0.0, 0.0]);
        assert_eq!(billboard_pivot_bias(1), [0.0, -0.5]);
        assert_eq!(billboard_pivot_bias(3), [0.0, 0.5]);
        assert_eq!(billboard_pivot_bias(8), [0.5, 0.5]);
    }

    #[test]
    fn billboard_pivot_cbuf47_maps_xy_to_yz() {
        let v = billboard_pivot_cbuf47(1);
        assert_eq!(v[0], 0.0);
        assert_eq!(v[1], -0.5);
        assert_eq!(v[2], 0.0);
    }

    #[test]
    fn billboard_basis_stripe_uses_velocity() {
        let vel = Vec3::new(0.0, 0.0, 10.0);
        let (right, up) = billboard_basis(
            BillboardType::Stripe,
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            vel,
        );
        assert!((up.z - 1.0).abs() < 0.001);
        assert!(right.length() > 0.5);
    }

    #[test]
    fn stripe_corner_half_extents_scales_width_only() {
        let c = stripe_corner_half_extents(
            BillboardType::Stripe,
            [0.5, 0.5],
            2.0,
            Vec3::new(0.0, 0.0, 10.0),
        );
        assert!((c[0] - 0.25).abs() < 0.001, "width scaled by 1/aspect");
        assert!((c[1] - 0.5).abs() < 0.001, "length unchanged");
    }

    #[test]
    fn stripe_corner_complex_stretches_trailing_edge() {
        let c = stripe_corner_half_extents(
            BillboardType::ComplexStripe,
            [0.0, -0.5],
            1.0,
            Vec3::new(0.0, 0.0, 100.0),
        );
        assert!(c[1] < -0.5, "trailing Y corner extends backward, got {}", c[1]);
    }

    #[test]
    fn primitive_mesh_basis_from_triangle() {
        let prim = PrimitiveData {
            id: 0,
            vertices: vec![
                MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                MeshVertex { position: [0.0, 0.0, 1.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
            ],
            indices: vec![0, 1, 2],
        };
        let (right, up) = primitive_mesh_basis(&prim);
        assert!(right.length() > 0.5);
        assert!(up.length() > 0.5);
        assert!(right.dot(up).abs() < 0.01);
    }

    #[test]
    fn primitive_corner_half_extents_match_bbox() {
        let prim = PrimitiveData {
            id: 0,
            vertices: vec![
                MeshVertex { position: [-1.0, -2.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
                MeshVertex { position: [1.0, 2.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            ],
            indices: vec![0, 1, 0],
        };
        let (min_c, max_c, _) = primitive_corner_half_extents(&prim);
        assert!((min_c[0] + max_c[0]).abs() < 0.01);
        assert!((min_c[1] + max_c[1]).abs() < 0.01);
        let span = (max_c[0] - min_c[0]).hypot(max_c[1] - min_c[1]);
        assert!(
            (span - 20.0_f32.sqrt()).abs() < 0.05,
            "min-area rect spans diagonal segment, got span={span}"
        );
        assert!(
            (max_c[1] - min_c[1]).abs() >= 0.4,
            "degenerate line gets minimum quad thickness"
        );
    }

    #[test]
    fn billboard_basis_primitive_uses_mesh_when_loaded() {
        let prim = PrimitiveData {
            id: 0,
            vertices: vec![
                MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
                MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
                MeshVertex { position: [0.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            ],
            indices: vec![0, 1, 2],
        };
        let mut emitter = EmitterDef::default();
        emitter.billboard_type = BillboardType::Primitive;
        emitter.particle_primitive_id = 0;
        let (right, up) = billboard_basis_for_emitter(
            &emitter,
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::ZERO,
            None,
            &[prim],
        );
        assert!(right.x.abs() > 0.9);
        assert!(up.y.abs() > 0.9);
    }

    #[test]
    fn billboard_basis_primitive_prefers_bfres_draw_mesh() {
        let prma = PrimitiveData {
            id: 1,
            vertices: vec![
                MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
                MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
                MeshVertex { position: [0.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            ],
            indices: vec![0, 1, 2],
        };
        let prims = [prma];
        let bfres = BfresModel {
            source_id: 99,
            meshes: vec![BfresMesh {
                vertices: vec![
                    MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                    MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                    MeshVertex { position: [0.0, 0.0, 1.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                ],
                indices: vec![0, 1, 2],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ctx = SpawnMeshContext {
            primitives: &prims,
            bfres_models: &[bfres],
        };
        let mut emitter = EmitterDef::default();
        emitter.billboard_type = BillboardType::Primitive;
        emitter.mesh_type = 2;
        emitter.particle_primitive_id = 99;
        emitter.primitive_index = 0;
        let (right, up) = draw_mesh_basis(&ctx, &emitter).expect("bfres draw mesh");
        assert!(right.x.abs() > 0.9, "BFRES XZ triangle right along X, got {right:?}");
        assert!(up.z.abs() > 0.9, "BFRES XZ triangle up along Z, got {up:?}");
        let (br, bu) = billboard_basis_for_emitter(
            &emitter,
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::ZERO,
            Some(&ctx),
            &prims,
        );
        assert!((br - right).length() < 0.01);
        assert!((bu - up).length() < 0.01);
    }

    #[test]
    fn mesh_corner_half_extents_centers_on_mesh_centroid() {
        let verts = vec![
            MeshVertex { position: [1.0, 2.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [3.0, 2.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 4.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
        ];
        let (min_c, max_c, _) = mesh_corner_half_extents(&verts, &[0, 1, 2]);
        assert!((min_c[0] + max_c[0]).abs() < 0.01, "corners centered on mesh X");
        assert!((min_c[1] + max_c[1]).abs() < 0.01, "corners centered on mesh Y");
        assert!((max_c[0] - min_c[0]).abs() > 0.5);
        assert!((max_c[1] - min_c[1]).abs() > 0.5);
    }

    #[test]
    fn mesh_corner_min_area_rect_tighter_than_fixed_aabb() {
        let verts = vec![
            MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [4.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [4.5, 1.5, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [0.5, 0.5, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let (min_c, max_c, _) = mesh_corner_half_extents(&verts, &indices);
        let area = (max_c[0] - min_c[0]) * (max_c[1] - min_c[1]);
        let fixed_aabb_area = 4.5 * 1.5;
        assert!(
            area < fixed_aabb_area * 0.85,
            "min-area rect should beat world-axis AABB for rotated quad, area={area}"
        );
    }

    #[test]
    fn mesh_per_triangle_quads_one_quad_per_face() {
        let verts = vec![
            MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [0.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
        ];
        let indices = vec![0u16, 1, 2];
        let (quads, _) = mesh_per_triangle_quads(&verts, &indices);
        assert_eq!(quads.len(), 1);
    }

    #[test]
    fn mesh_silhouette_quads_splits_concave_l_shape() {
        let verts = vec![
            MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [3.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [3.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 3.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [0.0, 3.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
        ];
        let indices = vec![0, 1, 2, 0, 2, 3, 0, 3, 4, 0, 4, 5];
        let (quads, _) = mesh_silhouette_quads(&verts, &indices);
        assert!(
            quads.len() >= 2,
            "concave L should split into multiple quads, got {}",
            quads.len()
        );
        let split_area: f32 = quads
            .iter()
            .map(|(a, b)| (b[0] - a[0]) * (b[1] - a[1]))
            .sum();
        let (min_c, max_c, _) = mesh_corner_half_extents(&verts, &indices);
        let single_area = (max_c[0] - min_c[0]) * (max_c[1] - min_c[1]);
        assert!(
            split_area < single_area * 0.85,
            "split quads should beat single rect (split={split_area} single={single_area})"
        );
    }

    #[test]
    fn mesh_corner_non_convex_looser_than_true_silhouette() {
        // L-shaped non-convex quad: one enclosing rectangle must cover the concavity.
        let verts = vec![
            MeshVertex { position: [0.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [3.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [3.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 1.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [1.0, 3.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
            MeshVertex { position: [0.0, 3.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 0.0, 1.0] },
        ];
        let indices = vec![0, 1, 2, 0, 2, 3, 0, 3, 4, 0, 4, 5];
        let (min_c, max_c, _) = mesh_corner_half_extents(&verts, &indices);
        let area = (max_c[0] - min_c[0]) * (max_c[1] - min_c[1]);
        let mesh_area = 3.0 + 2.0;
        assert!(
            area > mesh_area * 1.05,
            "single quad must over-fill concave L (area={area} vs mesh={mesh_area})"
        );
        assert!((min_c[0] + max_c[0]).abs() < 0.01);
        assert!((min_c[1] + max_c[1]).abs() < 0.01);
    }

    #[test]
    fn silhouette_atlas_uv_maps_sub_quads_into_envelope() {
        let quads = vec![([0.0, 0.0], [3.0, 1.0]), ([0.0, 1.0], [1.0, 3.0])];
        let env = silhouette_envelope(&quads);
        assert_eq!(env, ([0.0, 0.0], [3.0, 3.0]));
        let uv00 = silhouette_atlas_uv([0.0, 0.0], quads[0], env);
        let uv11 = silhouette_atlas_uv([1.0, 1.0], quads[1], env);
        assert!((uv00[0] - 0.0).abs() < 1e-5 && (uv00[1] - 0.0).abs() < 1e-5);
        assert!((uv11[0] - 1.0 / 3.0).abs() < 1e-4 && (uv11[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn billboard_basis_primitive_is_world_xy() {
        let (right, up) = billboard_basis(
            BillboardType::Primitive,
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::ZERO,
        );
        assert_eq!(right, Vec3::X);
        assert_eq!(up, Vec3::Y);
    }

    #[test]
    fn same_divide_sphere_uses_volume_table() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::SphereSameDivide;
        emitter.volume_tbl_index = 0;
        emitter.volume_radius = Vec3::ONE;
        let p0 = volume_local_spawn_pos(&emitter, 0, 0, 2, None);
        let p1 = volume_local_spawn_pos(&emitter, 1, 1, 2, None);
        assert!((p0.y - 1.0).abs() < 0.001, "table[0] should be +Y, got {p0:?}");
        assert!((p1.y + 1.0).abs() < 0.001, "table[1] should be -Y, got {p1:?}");
    }

    #[test]
    fn sweep_arc_limits_circle_spawn() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::Circle;
        emitter.volume_radius = Vec3::new(1.0, 1.0, 1.0);
        emitter.sweep_start = 0.0;
        emitter.sweep_longitude = std::f32::consts::FRAC_PI_2;
        emitter.sweep_start_random = false;
        for seed in 0..16 {
            let p = volume_local_spawn_pos(&emitter, seed, 0, 1, None);
            let theta = p.z.atan2(p.x);
            assert!(theta >= -0.01 && theta <= std::f32::consts::FRAC_PI_2 + 0.01,
                "theta {theta} out of arc range for pos {p:?}");
        }
    }

    #[test]
    fn primitive_vertex_emit_picks_mesh_vertices() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::Primitive;
        emitter.prim_emit_type = 0;
        let prim = PrimitiveData {
            id: 0,
            vertices: vec![
                MeshVertex { position: [1.0, 0.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
                MeshVertex { position: [0.0, 2.0, 0.0], uv: [0.0, 0.0], normal: [0.0, 1.0, 0.0] },
            ],
            indices: vec![0, 1, 0],
        };
        let ctx = SpawnMeshContext {
            primitives: &[prim],
            bfres_models: &[],
        };
        emitter.shape_primitive_index = 0;
        let p0 = volume_local_spawn_pos(&emitter, 0, 0, 2, Some(&ctx));
        let p1 = volume_local_spawn_pos(&emitter, 1, 1, 2, Some(&ctx));
        assert!((p0.x - 1.0).abs() < 0.001 && (p0.y - 0.0).abs() < 0.001);
        assert!((p1.y - 2.0).abs() < 0.001);
    }

    #[test]
    fn same_divide_sphere64_uses_nw4f_table() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::SphereSameDivide64;
        emitter.volume_tbl_index64 = 0; // 2-point table
        emitter.volume_radius = Vec3::ONE;
        let p0 = volume_local_spawn_pos(&emitter, 0, 0, 2, None);
        let p1 = volume_local_spawn_pos(&emitter, 1, 1, 2, None);
        let n0 = p0.normalize_or_zero();
        let n1 = p1.normalize_or_zero();
        assert!((n0.x - 0.975795).abs() < 0.001, "unexpected p0 {p0:?}");
        assert!((n1.x - 0.550744).abs() < 0.001, "unexpected p1 {p1:?}");
    }

    #[test]
    fn arc_type_fixed_pins_circle_angle() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::Circle;
        emitter.arc_type = ArcType::Fixed;
        emitter.sweep_start = std::f32::consts::FRAC_PI_4;
        emitter.sweep_longitude = std::f32::consts::FRAC_PI_2;
        for seed in 0..8 {
            let p = volume_local_spawn_pos(&emitter, seed, 0, 1, None);
            let theta = p.z.atan2(p.x);
            assert!(
                (theta - std::f32::consts::FRAC_PI_4).abs() < 0.01,
                "fixed arc should pin theta, seed {seed} got {theta}"
            );
        }
    }

    #[test]
    fn arc_type_from_u8_maps_known_and_unknown() {
        assert_eq!(ArcType::from(0), ArcType::Random);
        assert_eq!(ArcType::from(1), ArcType::EquallyDivided);
        assert_eq!(ArcType::from(2), ArcType::Fixed);
        assert_eq!(ArcType::from(7), ArcType::Unknown(7));
        assert_eq!(ArcType::Unknown(7).as_u8(), 7);
    }

    #[test]
    fn arc_type_unknown_spreads_like_random() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::Circle;
        emitter.arc_type = ArcType::Unknown(5);
        emitter.sweep_start = 0.0;
        emitter.sweep_longitude = std::f32::consts::PI;
        let mut thetas = Vec::new();
        for seed in 0..16 {
            let p = volume_local_spawn_pos(&emitter, seed, 0, 1, None);
            thetas.push(p.z.atan2(p.x));
        }
        let first = thetas[0];
        assert!(
            thetas.iter().any(|t| (t - first).abs() > 0.05),
            "unknown arc type should spread within sweep, got {thetas:?}"
        );
    }

    fn test_mesh_vertex(x: f32, y: f32, z: f32) -> MeshVertex {
        MeshVertex {
            position: [x, y, z],
            uv: [0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
        }
    }

    #[test]
    fn resolve_prma_slot_maps_descriptor_id() {
        let prims = vec![
            PrimitiveData {
                id: 100,
                vertices: vec![test_mesh_vertex(1.0, 0.0, 0.0)],
                indices: vec![0, 0, 0],
            },
            PrimitiveData {
                id: 200,
                vertices: vec![test_mesh_vertex(2.0, 0.0, 0.0)],
                indices: vec![0, 0, 0],
            },
        ];
        assert_eq!(resolve_prma_slot(&prims, 200), 1);
        assert_eq!(resolve_prma_slot(&prims, 100), 0);
    }

    #[test]
    fn resolve_spawn_mesh_bfres_by_source_id_when_index_mismatch() {
        let prma = PrimitiveData {
            id: 42,
            vertices: vec![test_mesh_vertex(1.0, 0.0, 0.0)],
            indices: vec![0, 0, 0],
        };
        let bfres = BfresModel {
            source_id: 42,
            meshes: vec![BfresMesh {
                vertices: vec![test_mesh_vertex(77.0, 0.0, 0.0)],
                indices: vec![0, 0, 0],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ctx = SpawnMeshContext {
            primitives: &[prma],
            bfres_models: &[bfres],
        };
        let mut emitter = EmitterDef::default();
        emitter.mesh_type = 2;
        emitter.primitive_index = 99;
        emitter.shape_primitive_index = 42;
        emitter.prim_emit_type = 0;
        let p = sample_primitive_surface_pos(&ctx, &emitter, 0, 0, 1);
        assert!(
            (p.x - 77.0).abs() < 0.001,
            "expected BFRES matched by source_id, got {p:?}"
        );
    }

    #[test]
    fn mesh_basis_prefers_vertex_normals_when_triangles_degenerate() {
        let verts = vec![
            MeshVertex {
                position: [0.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                normal: [1.0, 0.0, 0.0],
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                normal: [1.0, 0.0, 0.0],
            },
            MeshVertex {
                position: [2.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                normal: [1.0, 0.0, 0.0],
            },
        ];
        let (right, up) = mesh_basis(&verts, &[0, 1, 2]);
        assert!(
            right.x.abs() < 0.01,
            "right should be perpendicular to +X normal, got {right:?}"
        );
        assert!(up.length() > 0.5);
    }

    #[test]
    fn resolve_spawn_mesh_prefers_bfres_over_prma() {
        let prma = PrimitiveData {
            id: 0,
            vertices: vec![test_mesh_vertex(1.0, 0.0, 0.0)],
            indices: vec![0, 0, 0],
        };
        let bfres = BfresModel {
            meshes: vec![BfresMesh {
                vertices: vec![test_mesh_vertex(99.0, 0.0, 0.0)],
                indices: vec![0, 0, 0],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ctx = SpawnMeshContext {
            primitives: &[prma],
            bfres_models: &[bfres],
        };
        let mut emitter = EmitterDef::default();
        emitter.mesh_type = 2;
        emitter.primitive_index = 0;
        emitter.shape_primitive_index = 0;
        emitter.prim_emit_type = 0;
        let p = sample_primitive_surface_pos(&ctx, &emitter, 0, 0, 1);
        assert!((p.x - 99.0).abs() < 0.001, "expected BFRES mesh, got {p:?}");
    }

    #[test]
    fn resolve_spawn_mesh_falls_back_to_prma_when_bfres_missing() {
        let prma = PrimitiveData {
            id: 0,
            vertices: vec![test_mesh_vertex(2.0, 0.0, 0.0)],
            indices: vec![0, 0, 0],
        };
        let ctx = SpawnMeshContext {
            primitives: &[prma],
            bfres_models: &[],
        };
        let mut emitter = EmitterDef::default();
        emitter.mesh_type = 2;
        emitter.primitive_index = 0;
        emitter.shape_primitive_index = 0;
        emitter.prim_emit_type = 0;
        let p = sample_primitive_surface_pos(&ctx, &emitter, 0, 0, 1);
        assert!((p.x - 2.0).abs() < 0.001, "expected PRMA fallback, got {p:?}");
    }

    #[test]
    fn resolve_spawn_mesh_falls_back_to_prma_when_bfres_empty() {
        let prma = PrimitiveData {
            id: 0,
            vertices: vec![test_mesh_vertex(3.0, 0.0, 0.0)],
            indices: vec![0, 0, 0],
        };
        let bfres = BfresModel {
            meshes: vec![BfresMesh::default()],
            ..Default::default()
        };
        let ctx = SpawnMeshContext {
            primitives: &[prma],
            bfres_models: &[bfres],
        };
        let mut emitter = EmitterDef::default();
        emitter.mesh_type = 2;
        emitter.primitive_index = 0;
        emitter.shape_primitive_index = 0;
        emitter.prim_emit_type = 0;
        let p = sample_primitive_surface_pos(&ctx, &emitter, 0, 0, 1);
        assert!((p.x - 3.0).abs() < 0.001, "expected PRMA fallback for empty BFRES, got {p:?}");
    }

    #[test]
    fn num_divide_circle_overrides_same_divide_stepping() {
        let mut emitter = EmitterDef::default();
        emitter.emit_type = EmitType::CircleSameDivide;
        emitter.num_divide_circle = 4;
        emitter.sweep_start = 0.0;
        let mut thetas = Vec::new();
        for i in 0..4 {
            let p = volume_local_spawn_pos(&emitter, 0, i, 99, None);
            thetas.push(p.z.atan2(p.x));
        }
        assert!((thetas[1] - std::f32::consts::FRAC_PI_2).abs() < 0.05);
        assert!((thetas[2] - std::f32::consts::PI).abs() < 0.05
            || (thetas[2] + std::f32::consts::PI).abs() < 0.05);
        assert!((thetas[3] + std::f32::consts::FRAC_PI_2).abs() < 0.05);
    }

    #[test]
    fn particle_rotation_euler_applies_xyz_rand() {
        let mut emitter = EmitterDef::default();
        emitter.rot_type = 4;
        emitter.rot_axis_x = true;
        emitter.rot_axis_y = true;
        emitter.rot_axis_z = true;
        let particle = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            lifetime: 1.0,
            color: Vec4::ONE,
            color0_rgb: [1.0, 1.0, 1.0],
            color1_rgb: [1.0, 1.0, 1.0],
            alpha0_live: 1.0,
            alpha1_live: 1.0,
            color_scale_live: 1.0,
            draw_path: 0,
            pre_draw: false,
            parent_emitter_idx: None,
            inst_start_frame: 0.0,
            inherit: None,
            size: 1.0,
            rotation: 0.5,
            rotation_speed: 0.0,
            emitter_set_idx: 0,
            emitter_idx: 0,
            local_offset: Vec3::ZERO,
            bone_name: String::new(),
            inst_offset: Vec3::ZERO,
            inst_rotation: Vec3::ZERO,
            texture_idx: 0,
            blend_type: BlendType::Normal,
            tex_offset: [0.0, 0.0],
            indirect_tex_offset: [0.0, 0.0],
            tex2_tex_offset: [0.0, 0.0],
            tex_scale_live: [1.0, 1.0],
            tex_scroll_angle: 0.0,
            pat_phase_offset: 0.0,
            pat_fixed_frame: None,
            pat_blend: 0.0,
            pat_next_uv_delta: [0.0, 0.0],
            tex_extra_offsets: [[0.0, 0.0]; 3],
            seed: 0,
            rotation_rand: Vec3::new(0.1, 0.2, 0.3),
        };
        let e = particle_rotation_euler(&particle, &emitter);
        assert!((e.x - 0.6).abs() < 0.001);
        assert!((e.y - 0.7).abs() < 0.001);
        assert!((e.z - 0.8).abs() < 0.001);
    }

    #[test]
    fn spawn_rotation_rand_scales_by_emitter_fields() {
        let mut emitter = EmitterDef::default();
        emitter.rotate_rand = Vec3::new(1.0, 1.0, 1.0);
        let r = spawn_rotation_rand(&emitter, 42);
        assert!(r.x.abs() <= 1.0 && r.x.abs() > 0.0);
        assert!(r.y.abs() <= 1.0 && r.y.abs() > 0.0);
        assert!(r.z.abs() <= 1.0 && r.z.abs() > 0.0);
    }

    #[test]
    fn emit_dist_spawns_along_motion_path() {
        let mut emitter = EmitterDef::default();
        emitter.is_emit_dist_enabled = true;
        emitter.emitter_dist_unit = 1.0;
        emitter.emitter_dist_min = 1.0;
        let mut inst = EmitterInstance {
            emitter_set_idx: 0,
            emitter_idx: 0,
            bone_name: "Trans".to_string(),
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            start_frame: 0.0,
            end_frame: 9999.0,
            emit_accum: 0.0,
            burst_fired: false,
            emit_dist_prev_pos: Vec3::ZERO,
            emit_dist_prev_pos_set: true,
            emit_dist_vessel: 0.0,
            prev_world_pos: Vec3::ZERO,
            prev_world_pos_set: false,
            death_only: false,
        };
        let batch = emit_dist_spawn_batch(&emitter, &mut inst, Vec3::new(3.0, 0.0, 0.0));
        assert!(!batch.is_empty(), "3-unit move with unit=1 should spawn particles");
        assert!(batch[0].x >= 0.0 && batch[0].x <= 3.0);
    }

    #[test]
    fn mat4_to_cbuf_rows_extracts_translation() {
        let rows = mat4_to_cbuf_rows_3x4(Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(rows[0][3], 1.0);
        assert_eq!(rows[1][3], 2.0);
        assert_eq!(rows[2][3], 3.0);
    }

    fn stub_particle(draw_path: u32, pre_draw: bool, set: usize, idx: usize) -> Particle {
        stub_particle_with_parent(draw_path, pre_draw, set, idx, None)
    }

    fn stub_particle_with_parent(
        draw_path: u32,
        pre_draw: bool,
        set: usize,
        idx: usize,
        parent_emitter_idx: Option<usize>,
    ) -> Particle {
        Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            lifetime: 1.0,
            color: Vec4::ONE,
            color0_rgb: [1.0, 1.0, 1.0],
            color1_rgb: [1.0, 1.0, 1.0],
            alpha0_live: 1.0,
            alpha1_live: 1.0,
            color_scale_live: 1.0,
            draw_path,
            pre_draw,
            parent_emitter_idx,
            inst_start_frame: 0.0,
            inherit: None,
            size: 1.0,
            rotation: 0.0,
            rotation_speed: 0.0,
            emitter_set_idx: set,
            emitter_idx: idx,
            local_offset: Vec3::ZERO,
            bone_name: String::new(),
            inst_offset: Vec3::ZERO,
            inst_rotation: Vec3::ZERO,
            texture_idx: 0,
            blend_type: BlendType::Normal,
            tex_offset: [0.0, 0.0],
            indirect_tex_offset: [0.0, 0.0],
            tex2_tex_offset: [0.0, 0.0],
            tex_scale_live: [1.0, 1.0],
            tex_scroll_angle: 0.0,
            pat_phase_offset: 0.0,
            pat_fixed_frame: None,
            pat_blend: 0.0,
            pat_next_uv_delta: [0.0, 0.0],
            tex_extra_offsets: [[0.0, 0.0]; 3],
            seed: 0,
            rotation_rand: Vec3::ZERO,
        }
    }

    #[test]
    fn particle_draw_sort_orders_draw_path_pre_draw_emitter() {
        let particles = vec![
            stub_particle(2, false, 0, 1),
            stub_particle(1, true, 0, 0),
            stub_particle(1, false, 0, 0),
            stub_particle(1, false, 1, 0),
        ];
        let keys = ordered_particle_batch_keys(&particles);
        assert_eq!(
            keys,
            vec![
                (1, true, 0, 0),
                (1, false, 0, 0),
                (1, false, 1, 0),
                (2, false, 0, 1),
            ]
        );
    }

    #[test]
    fn pre_draw_child_sorts_before_parent_not_globally_first() {
        let particles = vec![
            stub_particle_with_parent(0, true, 0, 5, Some(2)),
            stub_particle(0, false, 0, 0),
            stub_particle(0, false, 0, 1),
            stub_particle(0, false, 0, 2),
            stub_particle(0, false, 0, 3),
        ];
        let keys = ordered_particle_batch_keys(&particles);
        assert_eq!(
            keys,
            vec![
                (0, false, 0, 0),
                (0, false, 0, 1),
                (0, true, 0, 5),
                (0, false, 0, 2),
                (0, false, 0, 3),
            ],
            "pre_draw child of emitter 2 should draw after 0/1 and before parent 2"
        );
    }

    #[test]
    fn distinct_draw_paths_sorted_ascending() {
        let particles = vec![
            stub_particle(2, false, 0, 0),
            stub_particle(0, false, 0, 1),
            stub_particle(1, false, 0, 2),
            stub_particle(2, false, 0, 3),
        ];
        assert_eq!(distinct_particle_draw_paths(&particles), vec![0, 1, 2]);
    }

    #[test]
    fn ordered_batches_grouped_by_draw_path() {
        let particles = vec![
            stub_particle(1, false, 0, 0),
            stub_particle(0, true, 0, 1),
            stub_particle(0, false, 0, 0),
            stub_particle(1, false, 0, 1),
        ];
        assert_eq!(
            ordered_particle_batch_keys_by_draw_path(&particles),
            vec![
                (0, vec![(0, true, 0, 1), (0, false, 0, 0)]),
                (1, vec![(1, false, 0, 0), (1, false, 0, 1)]),
            ]
        );
    }

    #[test]
    fn multi_path_compositing_order_matches_distinct_draw_paths() {
        use crate::particle_renderer::{editor_composite_steps, EditorCompositeStep};

        let particles = vec![
            stub_particle(2, false, 0, 0),
            stub_particle(0, false, 0, 1),
            stub_particle(1, false, 0, 2),
        ];
        let paths = distinct_particle_draw_paths(&particles);
        assert_eq!(paths, vec![0, 1, 2]);
        assert_eq!(
            editor_composite_steps(&paths),
            vec![
                EditorCompositeStep::BlitDrawPath(0),
                EditorCompositeStep::SubDrawPath(0),
                EditorCompositeStep::BlitDrawPath(1),
                EditorCompositeStep::SubDrawPath(1),
                EditorCompositeStep::BlitDrawPath(2),
                EditorCompositeStep::SubDrawPath(2),
            ]
        );
    }

    #[test]
    fn distinct_draw_paths_includes_trail_paths() {
        let particles = vec![stub_particle(1, false, 0, 0)];
        let trails = vec![SwordTrail::new("t", "tip", "base", 2, [1.0; 4], BlendType::Add)];
        assert_eq!(distinct_draw_paths(&particles, &trails), vec![1, 2]);
    }

    #[test]
    fn particle_clip_depth_orders_farther_particles_first_within_batch() {
        use glam::{Mat4, Vec3};
        let view_proj = Mat4::IDENTITY;
        let mut near = stub_particle(0, false, 0, 0);
        near.position = Vec3::new(0.0, 0.0, -1.0);
        let mut far = stub_particle(0, false, 0, 0);
        far.position = Vec3::new(0.0, 0.0, -5.0);
        assert!(
            particle_clip_depth(view_proj, &far) < particle_clip_depth(view_proj, &near),
            "farther particles have smaller clip depth in this setup"
        );
        let mut particles = vec![near, far];
        particles.sort_by(|a, b| {
            particle_draw_sort_key(a)
                .cmp(&particle_draw_sort_key(b))
                .then_with(|| {
                    particle_clip_depth(view_proj, a)
                        .partial_cmp(&particle_clip_depth(view_proj, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        assert!(
            particle_clip_depth(view_proj, &particles[0])
                < particle_clip_depth(view_proj, &particles[1]),
            "farther particle should draw first within the same batch"
        );
    }

    #[test]
    fn alpha_each_frame_two_pass_uses_fresh_parent_alpha() {
        let parent_key = (42u64, 0, 0);
        let stale_lookup = HashMap::from([(parent_key, (0.25, 1.0))]);
        let fresh_lookup = HashMap::from([(parent_key, (0.75, 1.0))]);
        let inherit = ParticleInheritState {
            color0_mul: [1.0, 1.0, 1.0],
            color1_mul: [1.0, 1.0, 1.0],
            alpha0_mul: 1.0,
            alpha1_mul: 1.0,
            color_scale: 1.0,
            alpha0_each_frame: true,
            alpha1_each_frame: false,
            parent_seed: 42,
            parent_set_idx: 0,
            parent_emitter_idx: 0,
            draw_path: None,
            pre_draw: false,
        };

        let (_, _, a0_pass1, _) = apply_inherit_channels(
            [1.0; 4],
            [1.0; 4],
            1.0,
            1.0,
            Some(&inherit),
            Some(&stale_lookup),
            false,
        );
        assert!(
            (a0_pass1 - 1.0).abs() < 0.001,
            "first pass defers each-frame parent alpha"
        );

        let (_, _, a0_stale, _) = apply_inherit_channels(
            [1.0; 4],
            [1.0; 4],
            1.0,
            1.0,
            Some(&inherit),
            Some(&stale_lookup),
            true,
        );
        assert!((a0_stale - 0.25).abs() < 0.001, "stale parent alpha for comparison");

        let (_, _, a0_fresh, _) = apply_inherit_channels(
            [1.0; 4],
            [1.0; 4],
            1.0,
            1.0,
            Some(&inherit),
            Some(&fresh_lookup),
            true,
        );
        assert!(
            (a0_fresh - 0.75).abs() < 0.001,
            "second pass should use same-frame parent alpha"
        );
    }

    #[test]
    fn step_alpha_each_frame_refresh_tracks_parent_after_integration() {
        let mut emitter = EmitterDef::default();
        emitter.lifetime = 10.0;
        emitter.alpha0_keys = vec![
            ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            ColorKey { frame: 1.0, r: 1.0, g: 1.0, b: 1.0, a: 0.2 },
        ];

        let mut parent = stub_particle(0, false, 0, 0);
        parent.seed = 7;
        parent.age = 5.0;
        parent.alpha0_live = 0.99;

        let mut child = stub_particle(0, false, 0, 1);
        child.age = 2.0;
        child.inherit = Some(ParticleInheritState {
            color0_mul: [1.0, 1.0, 1.0],
            color1_mul: [1.0, 1.0, 1.0],
            alpha0_mul: 1.0,
            alpha1_mul: 1.0,
            color_scale: 1.0,
            alpha0_each_frame: true,
            alpha1_each_frame: false,
            parent_seed: 7,
            parent_set_idx: 0,
            parent_emitter_idx: 0,
            draw_path: None,
            pre_draw: false,
        });

        update_particle_color_channels(&mut parent, &emitter, None, false);
        update_particle_color_channels(&mut child, &emitter, None, false);

        let lookup: HashMap<(u64, usize, usize), (f32, f32)> = HashMap::from([(
            (parent.seed, parent.emitter_set_idx, parent.emitter_idx),
            (parent.alpha0_live, parent.alpha1_live),
        )]);
        update_particle_color_channels(&mut child, &emitter, Some(&lookup), true);

        assert!(
            (child.alpha0_live - parent.alpha0_live).abs() < 0.001,
            "child alpha0 {:?} should match parent {:?}",
            child.alpha0_live,
            parent.alpha0_live
        );
    }
}


