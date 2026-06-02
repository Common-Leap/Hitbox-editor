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
        eprintln!("[EFF] merged {} emitter sets from {:?}, total now {} sets, {} bntx textures", 
            merged_count, path.file_name().unwrap_or_default(), ptcl.emitter_sets.len(), ptcl.bntx_textures.len());
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
    /// Particle lifetime in frames
    pub lifetime: f32,
    pub lifetime_random: f32,
    /// Base particle scale
    pub scale: f32,
    pub scale_random: f32,
    /// Rotation speed (radians/frame)
    pub rotation_speed: f32,
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
    /// Scale animation (3v4k)
    pub scale_anim: AnimKey3v4k,
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
    /// Number of animation frames in the sprite sheet (from TexPatAnim PatternCount)
    pub tex_pat_frame_count: usize,
    /// Emitter local position offset (Trans from EmitterInfo)
    pub emitter_offset: Vec3,
    /// Emitter local rotation (Euler angles XYZ in radians, from EmitterInfo Rotate)
    pub emitter_rotation: Vec3,
    /// Emitter local scale (per-axis, from EmitterInfo Scale)
    pub emitter_scale: Vec3,
    /// Whether this emitter fires a one-shot burst (from VFXB Emission.isOneTime)
    pub is_one_time: bool,
    /// Emission timing offset in frames (from VFXB Emission.Timing)
    pub emission_timing: u32,
    /// Emission duration in frames
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

/// Build the emitter's local TRS matrix: T * R * S.
/// Returns `Mat4::IDENTITY` (and logs to stderr) if the resulting matrix is degenerate
/// (determinant < 1e-6), per Requirement 7.3.
pub fn build_emitter_trs(emitter: &EmitterDef) -> Mat4 {
    let t = Mat4::from_translation(emitter.emitter_offset);
    let r = Mat4::from_euler(glam::EulerRot::ZYX,
        emitter.emitter_rotation.x,
        emitter.emitter_rotation.y,
        emitter.emitter_rotation.z,
    );
    let s = Mat4::from_scale(emitter.emitter_scale);
    let trs = t * r * s;
    // Check for degenerate matrix (near-zero determinant)
    let det = trs.determinant();
    if det.abs() < 1e-6 {
        eprintln!("[TRS] degenerate emitter transform (det={det:.2e}) for '{}', using IDENTITY", emitter.name);
        return Mat4::IDENTITY;
    }
    trs
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
    /// Raw shader binary from GRSN section
    pub shader_binary_1: Vec<u8>,
    /// Raw shader binary from GRSC section
    pub shader_binary_2: Vec<u8>,
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
            let mut soff = str_pos + 20;
            for _ in 0..str_count.min(512) {
                if soff + 2 > data.len() { break; }
                let slen = r16(soff) as usize;
                soff += 2;
                if soff + slen > data.len() { break; }
                let s = String::from_utf8_lossy(&data[soff..soff+slen]).to_string();
                soff += slen + 1;
                if soff % 2 != 0 { soff += 1; }
                if !s.is_empty() { str_names.push(s); }
            }
            break;
        }
        str_pos += 1; // was += 8; stride-1 finds _STR at any byte alignment
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

        // mip0_ptr: ptrsAddr is at BRTI+0x70 (u64, self-relative pointer within the BNTX slice).
        // The pointer is relative to bntx_base, not to the start of `data`.
        // We read ptrsAddr, add bntx_base to get the absolute offset, then dereference
        // to get the mip0 data address (also relative to bntx_base).
        let pts_addr = {
            let lo = r32(brti + 0x70) as u64;
            let hi = r32(brti + 0x74) as u64;
            (hi << 32 | lo) as usize
        };
        // pts_addr is relative to bntx_base — convert to absolute offset in data
        let pts_addr_abs = bntx_base.saturating_add(pts_addr);
        let mip0_ptr = if pts_addr > 0 && pts_addr_abs + 8 <= data.len() {
            // Read the first mipmap offset from the pointer array (also relative to bntx_base)
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
        let raw = &data[pixel_start..pixel_end];
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
            tegra_swizzle::surface::deswizzle_surface(
                width, height, 1,
                raw,
                block_dim,
                Some(block_height),
                bpp,
                1, 1,
            ).unwrap_or_else(|e| {
                eprintln!("[BNTX] deswizzle error tex {brti_idx}: {e}");
                raw.to_vec()
            })
        };

        let ftx_data_offset = texture_section.len() as u32;
        let pixel_len = pixel_bytes.len() as u32;
        eprintln!("[BNTX_TEX] '{}': {}x{} fmt={:#06x} offset={} size={} (first_bytes: {:02x} {:02x} {:02x} {:02x})",
            tex_name, width, height, format_id, ftx_data_offset, pixel_len,
            pixel_bytes.get(0).copied().unwrap_or(0),
            pixel_bytes.get(1).copied().unwrap_or(0),
            pixel_bytes.get(2).copied().unwrap_or(0),
            pixel_bytes.get(3).copied().unwrap_or(0));
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
        models.push(BfresModel { name, meshes });
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
                name: String::new(),
                emit_type: EmitType::Point,
                blend_type: BlendType::Add,
                display_side: DisplaySide::Both,
                emission_rate: 8.0,
                emission_rate_random: 0.0,
                initial_speed: 0.3,
                speed_random: 0.3,
                accel: Vec3::new(0.0, 0.05, 0.0),
                lifetime: 12.0,
                lifetime_random: 0.0,
                scale: 1.0,
                scale_random: 0.0,
                rotation_speed: 0.05,
                color0: Vec::new(),
                color1: Vec::new(),
                alpha0: AnimKey3v4k::default(),
                alpha1: AnimKey3v4k::default(),
                alpha0_keys: vec![],
                alpha1_keys: vec![],
                scale_anim: AnimKey3v4k::default(),
                textures: Vec::new(),
                mesh_type: 0,
                primitive_index: 0,
                texture_index: 0,
                tex_scale_uv: [1.0, 1.0],
                tex_offset_uv: [0.0, 0.0],
                tex_scroll_uv: [0.0, 0.0],
                tex_pat_frame_count: 1,
                emitter_offset: Vec3::ZERO,
                emitter_rotation: Vec3::ZERO,
                emitter_scale: Vec3::ONE,
                is_one_time: false,
                emission_timing: 0,
                emission_duration: 9999,
                is_indirect_slot1: false,
                distortion_strength: 0.0,
                indirect_scroll_uv: [0.0, 0.0],
                indirect_tex_scale_uv: [1.0, 1.0],
                indirect_tex_offset_uv: [0.0, 0.0],
            }],
        }).collect();
        Self { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0, bntx_textures: Vec::new(), primitives: Vec::new(), bfres_models: Vec::new(), shader_binary_1: Vec::new(), shader_binary_2: Vec::new() }
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
                    display_side: DisplaySide::Both,
                    emission_rate: 8.0,
                    emission_rate_random: 0.0,
                    initial_speed: 0.2,
                    speed_random: 0.3,
                    accel: Vec3::ZERO,
                    lifetime,
                    lifetime_random: 0.0,
                    scale,
                    scale_random: 0.0,
                    rotation_speed: 0.05,
                    color0: vec![ColorKey { frame: 0.0, r, g, b, a: 1.0 }],
                    color1: Vec::new(),
                    alpha0: AnimKey3v4k::default(),
                    alpha1: AnimKey3v4k::default(),
                    alpha0_keys: vec![],
                    alpha1_keys: vec![],
                    scale_anim: AnimKey3v4k::default(),
                    textures: Vec::new(),
                    mesh_type: 0,
                    primitive_index: 0,
                    texture_index: 0,
                    tex_scale_uv: [1.0, 1.0],
                    tex_offset_uv: [0.0, 0.0],
                    tex_scroll_uv: [0.0, 0.0],
                    tex_pat_frame_count: 1,
                    emitter_offset: Vec3::ZERO,
                    emitter_rotation: Vec3::ZERO,
                    emitter_scale: Vec3::ONE,
                    is_one_time: true,
                    emission_timing: 0,
                    emission_duration: lifetime as u32,
                    is_indirect_slot1: false,
                    distortion_strength: 0.0,
                    indirect_scroll_uv: [0.0, 0.0],
                    indirect_tex_scale_uv: [1.0, 1.0],
                    indirect_tex_offset_uv: [0.0, 0.0],
                }],
            }
        }).collect();
        Self { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0, bntx_textures: Vec::new(), primitives: Vec::new(), bfres_models: Vec::new(), shader_binary_1: Vec::new(), shader_binary_2: Vec::new() }
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

    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 32 {
            anyhow::bail!("PTCL data too short: {} bytes", data.len());
        }

        Self::parse_via_converter(data)
    }

    /// Parse via the EffectConverter CLI tool.  Returns Err if the CLI is not
    /// available or the converter fails for any reason.
    fn parse_via_converter(data: &[u8]) -> anyhow::Result<Self> {
        use std::path::PathBuf;
        use std::process::Command;

        eprintln!("[EC] parse_via_converter: data={} bytes", data.len());

        let cli = match option_env!("EFFECT_CONVERTER_CLI") {
            Some(p) => {
                let p = PathBuf::from(p);
                if p.exists() {
                    eprintln!("[EC] Using embedded CLI path: {}", p.display());
                    p
                } else {
                    anyhow::bail!("EffectConverter CLI not found at built path: {}", p.display());
                }
            }
            None => {
                if Command::new("EffectConverter").arg("--help").output().is_ok() {
                    eprintln!("[EC] Using EffectConverter from PATH");
                    PathBuf::from("EffectConverter")
                } else {
                    anyhow::bail!("EffectConverter CLI not available (rebuild with .NET 6.0+ SDK)");
                }
            }
        };

        let dir = tempfile::tempdir()?;
        let input_path = dir.path().join("input.ptcl");
        std::fs::write(&input_path, data)?;
        eprintln!("[EC] Written {} bytes to {:?}", data.len(), input_path);

        let status = Command::new(&cli)
            .arg(&input_path)
            .current_dir(dir.path())
            .status()
            .map_err(|e| anyhow::anyhow!("EffectConverter execution failed: {e}"))?;
        eprintln!("[EC] Converter exit status: {:?}", status.code());

        if !status.success() {
            anyhow::bail!("EffectConverter CLI exited with status {:?}", status.code());
        }

        // Converter creates ./input/ (just the stem of the filename, relative to CWD)
        let dump_dir = dir.path().join("input");
        if !dump_dir.is_dir() {
            anyhow::bail!("EffectConverter did not produce dump directory at {:?}", dump_dir);
        }
        eprintln!("[EC] Dump dir: {:?}", dump_dir);

        let ptcl = crate::effect_converter::load_dump(&dump_dir)?;
        eprintln!("[EC] Loaded PtclFile with {} emitter sets", ptcl.emitter_sets.len());
        if !ptcl.emitter_sets.is_empty() {
            let set = &ptcl.emitter_sets[0];
            eprintln!("[EC]   first set: {} with {} emitters", set.name, set.emitters.len());
        }
        Ok(ptcl)
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
    // At or before the first key's frame → return first key's color
    let first = &keys[0];
    if t <= first.frame {
        return Vec4::new(first.r, first.g, first.b, first.a);
    }
    // At or after the last key's frame → return last key's color
    let last = &keys[keys.len() - 1];
    if t >= last.frame {
        return Vec4::new(last.r, last.g, last.b, last.a);
    }
    // Find the two bracketing keys and linearly interpolate
    for i in 0..keys.len() - 1 {
        let a = &keys[i];
        let b = &keys[i + 1];
        if t >= a.frame && t <= b.frame {
            let range = (b.frame - a.frame).max(0.0001);
            let s = (t - a.frame) / range;
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
    sample_color(keys, t_clamped)
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
    pub size: f32,
    pub rotation: f32,
    pub rotation_speed: f32,
    pub emitter_set_idx: usize,
    pub emitter_idx: usize,
    #[allow(dead_code)]
    pub texture_idx: usize,
    #[allow(dead_code)]
    pub blend_type: BlendType,
    /// Per-particle UV offset (initialized to emitter.tex_offset_uv, advanced by tex_scroll_uv each frame)
    pub tex_offset: [f32; 2],
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
    #[allow(dead_code)]
    rotation: Vec3,
    start_frame: f32,
    end_frame: f32,
    emit_accum: f32,
    /// Prevents re-firing one-time burst emitters after the first burst frame.
    pub burst_fired: bool,
}

impl EmitterInstance {
    /// Test shim: exposes emit_accum for unit tests.
    #[cfg(test)]
    pub fn emit_accum_test(&self) -> f32 {
        self.emit_accum
    }
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
        for (emitter_idx, _) in set.emitters.iter().enumerate() {
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
        // If scrubbing backwards, we can't easily rewind — just clear and re-simulate
        // from scratch (caller handles re-spawning effects from frame 0).
        if target_frame < self.last_frame {
            self.particles.clear();
        }

        let dt = if self.last_frame < 0.0 {
            // First step — treat as a single frame advance
            1.0f32
        } else {
            (target_frame - self.last_frame).max(0.0)
        };
        self.last_frame = target_frame;

        if !self.active_emitters.is_empty() {
            eprintln!("[STEP] frame={target_frame} dt={dt} active_emitters={} particles={}", self.active_emitters.len(), self.particles.len());
        }

        // Skip emission when dt=0 (paused or duplicate step) — only integrate existing particles.
        // This prevents continuous emitters from over-firing when the simulation is stalled.
        let skip_emission = dt <= 0.0;

        // Integrate existing particles first, so newly spawned particles this frame
        // start at age=0 and survive until the next frame (fixes lifetime=1 particles
        // being born and killed in the same step).
        for p in &mut self.particles {
            let Some(set) = ptcl.emitter_sets.get(p.emitter_set_idx) else { p.age = p.lifetime; continue };
            let Some(emitter) = set.emitters.get(p.emitter_idx) else { p.age = p.lifetime; continue };

            p.age += dt;
            let safe_accel = if emitter.accel.is_finite() && emitter.accel.length() < 1000.0 {
                emitter.accel
            } else {
                Vec3::ZERO
            };
            p.velocity += safe_accel * dt;
            if p.velocity.is_finite() {
                p.position += p.velocity * dt;
            }
            p.rotation += p.rotation_speed * dt;

            let t = (p.age / emitter.lifetime).clamp(0.0, 1.0);
            let c0 = sample_color_or_white(&emitter.color0, t);
            // NintendoWare color combiner: Color0 × Color1 (multiplicative).
            // Color1 modulates Color0 — when absent, use white (multiplicative identity).
            let c1 = if !emitter.color1.is_empty() {
                sample_color_or_white(&emitter.color1, t)
            } else {
                Vec4::ONE
            };
            // Use full key tables for accurate alpha interpolation when available.
            // Alpha combiner: Alpha0 × Alpha1 (multiplicative).
            let a0 = if !emitter.alpha0_keys.is_empty() {
                sample_color_or_white(&emitter.alpha0_keys, t).x
            } else {
                emitter.alpha0.sample(t)
            };
            let a1 = if !emitter.alpha1_keys.is_empty() {
                sample_color_or_white(&emitter.alpha1_keys, t).x
            } else {
                emitter.alpha1.sample(t)
            };
            // NintendoWare combiner: rgb = color0 * color1, alpha = alpha0 * alpha1
            let rgb = Vec3::new(
                (c0.x * c1.x).clamp(0.0, 1.0),
                (c0.y * c1.y).clamp(0.0, 1.0),
                (c0.z * c1.z).clamp(0.0, 1.0),
            );
            let alpha = (a0 * a1).clamp(0.0, 1.0);
            p.color = Vec4::new(rgb.x, rgb.y, rgb.z, alpha);
            p.size = (emitter.scale * emitter.scale_anim.sample(t)).max(0.01);
            // For sprite-sheet animations: cycle through frames based on normalized age.
            // Only scroll tex_offset[0] for non-sprite-sheet emitters.
            if emitter.tex_pat_frame_count > 1 {
                // Sprite sheet: frame index drives tex_offset[1]; tex_offset[0] stays at authored value
                let frame = (t * emitter.tex_pat_frame_count as f32).floor() as usize;
                let frame = frame.min(emitter.tex_pat_frame_count - 1);
                p.tex_offset[0] = emitter.tex_offset_uv[0]; // fixed at authored offset
                p.tex_offset[1] = frame as f32 * emitter.tex_scale_uv[1];
            } else {
                // Scrolling texture: wrap within the tile size so tiled textures scroll correctly
                let tile_u = (1.0 / emitter.tex_scale_uv[0].max(0.001)).min(1.0);
                let tile_v = (1.0 / emitter.tex_scale_uv[1].max(0.001)).min(1.0);
                p.tex_offset[0] = (p.tex_offset[0] + emitter.tex_scroll_uv[0] * dt).rem_euclid(tile_u);
                p.tex_offset[1] = (p.tex_offset[1] + emitter.tex_scroll_uv[1] * dt).rem_euclid(tile_v);
            }
        }

        // Remove particles that died during integration
        self.particles.retain(|p| !p.is_dead());

        // Now emit new particles — they start at age=0 and live until next frame
        if !skip_emission { for inst in &mut self.active_emitters {
            if target_frame < inst.start_frame || target_frame > inst.end_frame { continue; }

            let Some(set) = ptcl.emitter_sets.get(inst.emitter_set_idx) else { continue };
            let Some(emitter) = set.emitters.get(inst.emitter_idx) else { continue };

            // Local frame within the effect (relative to when this emitter was spawned)
            let f = target_frame - inst.start_frame;

            // Emission window gating (Req 6.1–6.5)
            let in_window = f >= emitter.emission_timing as f32
                && (emitter.emission_duration == 0
                    || f < (emitter.emission_timing + emitter.emission_duration) as f32);

            // Get bone world position for spawn origin
            let bone_mat = bone_matrices.get(&inst.bone_name)
                .or_else(|| bone_matrices.get(&inst.bone_name.to_lowercase()))
                // Common fallbacks when the exact bone isn't in the skeleton
                .or_else(|| bone_matrices.get("top"))
                .or_else(|| bone_matrices.get("Trans"))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            // Apply bone-local offset transformed into world space,
            // plus the emitter's own Trans offset (also in bone-local space)
            let origin = bone_mat.transform_point3(emitter.emitter_offset)
                + bone_mat.transform_vector3(inst.offset);
            eprintln!("[EMIT] bone='{}' origin={:?} scale={} lifetime={}", 
                inst.bone_name, origin, emitter.scale, emitter.lifetime);

            let to_emit = if emitter.is_one_time {
                // One-time burst: fire exactly once on the burst frame (Req 7.1–7.4)
                // Use >= instead of == to handle cases where emission_timing > 0
                // and we might skip the exact frame due to frame stepping.
                if f >= emitter.emission_timing as f32 && !inst.burst_fired {
                    inst.burst_fired = true;
                    // Treat emission_rate <= 0.0 as 1.0 (Req 11.3 / 7.4)
                    let rate = if emitter.emission_rate <= 0.0 { 1.0 } else { emitter.emission_rate };
                    let n = rate.floor().max(1.0) as usize;
                    eprintln!("[EMIT] one_time burst: f={f} timing={} rate={rate} spawning={n}", emitter.emission_timing);
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
                if n > 0 { eprintln!("[EMIT] continuous: f={f} timing={} dur={} rate={rate} spawning={n}", emitter.emission_timing, emitter.emission_duration); }
                n
            } else {
                0
            };

            // Sample base color using the NintendoWare color combiner at t=0
            let c0_spawn = sample_color(&emitter.color0, 0.0);
            let c1_spawn = if !emitter.color1.is_empty() { sample_color(&emitter.color1, 0.0) } else { Vec4::ONE };
            let a0_spawn = if !emitter.alpha0_keys.is_empty() { sample_color_or_white(&emitter.alpha0_keys, 0.0).x } else { emitter.alpha0.sample(0.0) };
            let a1_spawn = if !emitter.alpha1_keys.is_empty() { sample_color_or_white(&emitter.alpha1_keys, 0.0).x } else { emitter.alpha1.sample(0.0) };
            // NintendoWare combiner: rgb = color0 * color1, alpha = alpha0 * alpha1
            let base_color = Vec4::new(
                (c0_spawn.x * c1_spawn.x).clamp(0.0, 1.0),
                (c0_spawn.y * c1_spawn.y).clamp(0.0, 1.0),
                (c0_spawn.z * c1_spawn.z).clamp(0.0, 1.0),
                (a0_spawn * a1_spawn).clamp(0.0, 1.0),
            );

            // Extract rotation matrix from emitter TRS for velocity direction rotation (Task 4.2)
            let emitter_rot_mat = Mat4::from_euler(glam::EulerRot::ZYX,
                emitter.emitter_rotation.x,
                emitter.emitter_rotation.y,
                emitter.emitter_rotation.z,
            );

            for i in 0..to_emit {
                // Spherical spread using golden-angle fibonacci distribution
                let seed = (self.particles.len() + i) as f32;
                let dir = match emitter.emit_type {
                    EmitType::Sphere
                    | EmitType::SphereSameDivide
                    | EmitType::SphereSameDivide64
                    | EmitType::FillSphere => {
                        let theta = seed * 2.399;
                        let phi = (1.0 - 2.0 * ((seed + 0.5) / to_emit.max(1) as f32)).acos();
                        Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
                    }
                    EmitType::Point => {
                        // Forward-facing hemisphere: phi in [0, π/2]
                        let theta = seed * 2.399;
                        let phi = (1.0 - ((i as f32 + 0.5) / to_emit.max(1) as f32)).acos();
                        Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
                    }
                    EmitType::Circle | EmitType::CircleSameDivide | EmitType::FillCircle => {
                        let theta = i as f32 * std::f32::consts::TAU / to_emit.max(1) as f32;
                        Vec3::new(theta.cos(), 0.0, theta.sin())
                    }
                    EmitType::Cylinder | EmitType::FillCylinder => {
                        let theta = i as f32 * std::f32::consts::TAU / to_emit.max(1) as f32;
                        let y = (seed * 0.37).sin() * 0.5;
                        Vec3::new(theta.cos(), y, theta.sin()).normalize()
                    }
                    _ => {
                        let theta = seed * 2.399;
                        let phi = (1.0 - 2.0 * ((seed + 0.5) / to_emit.max(1) as f32)).acos();
                        Vec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos())
                    }
                };
                // Rotate velocity direction by emitter rotation (Req 2.2)
                let rotated_dir = emitter_rot_mat.transform_vector3(dir);
                let speed = emitter.initial_speed
                    * (1.0 + (seed * 0.37).sin() * emitter.speed_random.min(0.5));
                let velocity = rotated_dir * speed;

                self.particles.push(Particle {
                    position: origin,
                    velocity,
                    age: 0.0,
                    lifetime: {
                        let rf = rand_factor((self.particles.len() + i).wrapping_add(1));
                        let lf = 1.0 + rf * emitter.lifetime_random;
                        emitter.lifetime * lf.max(0.0)
                    },
                    color: base_color,
                    size: {
                        // Apply scale_anim at t=0 and scale_random at spawn
                        let base_size = emitter.scale * emitter.scale_anim.sample(0.0);
                        let rf = rand_factor((self.particles.len() + i).wrapping_add(7));
                        (base_size * (1.0 + rf * emitter.scale_random)).max(0.01)
                    },
                    rotation: seed * 0.5,
                    rotation_speed: emitter.rotation_speed,
                    emitter_set_idx: inst.emitter_set_idx,
                    emitter_idx: inst.emitter_idx,
                    texture_idx: 0,
                    blend_type: emitter.blend_type,
                    tex_offset: emitter.tex_offset_uv,
                });
            }
        } } // end skip_emission guard

        // Remove emitters that have passed their full lifecycle (emission window + max particle lifetime).
        // This prevents the simulation from running indefinitely after all effects have expired.
        self.active_emitters.retain(|inst| {
            let f = target_frame - inst.start_frame;
            let Some(set) = ptcl.emitter_sets.get(inst.emitter_set_idx) else { return false };
            let Some(emitter) = set.emitters.get(inst.emitter_idx) else { return false };
            let emit_end = emitter.emission_timing as f32 + (emitter.emission_duration as f32).max(1.0);
            let full_end = emit_end + emitter.lifetime + emitter.lifetime_random;
            f < full_end
        });

        eprintln!("[STEP_END] frame={target_frame} particles_after_retain={} active_emitters={}", self.particles.len(), self.active_emitters.len());
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
    pub samples: Vec<TrailSample>,
    pub max_samples: usize,
    pub active: bool,
    pub blend_type: BlendType,
    /// RGBA color sampled from the emitter's color table
    pub color: [f32; 4],
}

impl SwordTrail {
    pub fn new(effect_name: &str, tip_bone: &str, base_bone: &str, color: [f32; 4], blend_type: BlendType) -> Self {
        Self {
            effect_name: effect_name.to_string(),
            tip_bone: tip_bone.to_string(),
            base_bone: base_bone.to_string(),
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

    pub fn start_trail(&mut self, effect_name: &str, tip_bone: &str, base_bone: &str, color: [f32; 4], blend_type: BlendType) {
        // Remove any existing trail for this effect
        self.trails.retain(|t| t.effect_name != effect_name);
        self.trails.push(SwordTrail::new(effect_name, tip_bone, base_bone, color, blend_type));
    }

    pub fn stop_trail(&mut self, effect_name: &str) {
        for t in &mut self.trails { if t.effect_name == effect_name { t.stop(); } }
    }

    pub fn step(&mut self, bone_matrices: &HashMap<String, Mat4>) {
        for trail in &mut self.trails { trail.record(bone_matrices); }
        self.trails.retain(|t| t.active || !t.samples.is_empty());
    }
}

