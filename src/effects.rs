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
            // Store both original and lowercase versions for case-insensitive lookup
            handles.insert(name_str.to_lowercase(), handle.emitter_set_handle);
            handles.insert(name_str, handle.emitter_set_handle);
        }

        let ptcl_data = eff.resource_data.unwrap_or_default();
        Ok(Self { handles, ptcl_data })
    }

    /// Merge handles from another eff file (e.g. ef_sys.eff) into this index.
    /// Existing handles are not overwritten.
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
    /// Alpha animation (3v4k)
    pub alpha0: AnimKey3v4k,
    pub alpha1: AnimKey3v4k,
    /// Scale animation (3v4k)
    pub scale_anim: AnimKey3v4k,
    /// Textures (up to 3)
    pub textures: Vec<TextureRes>,
    /// Mesh type: 0=billboard quad, 1=primitive mesh
    pub mesh_type: u32,
    /// Primitive index (if mesh_type == 1)
    pub primitive_index: u32,
    /// Whether this emitter fires a one-shot burst (from VFXB Emission.isOneTime)
    pub is_one_time: bool,
    /// Emission timing offset in frames (from VFXB Emission.Timing)
    pub emission_timing: u32,
    /// Emission duration in frames
    pub emission_duration: u32,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendType { Normal, Add, Sub, Screen, Multiply, Unknown(u32) }
impl From<u32> for BlendType {
    fn from(v: u32) -> Self {
        match v { 0 => Self::Normal, 1 => Self::Add, 2 => Self::Sub,
                  3 => Self::Screen, 4 => Self::Multiply, v => Self::Unknown(v) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplaySide { Both, Front, Back, Unknown(u32) }
impl From<u32> for DisplaySide {
    fn from(v: u32) -> Self {
        match v { 0 => Self::Both, 1 => Self::Front, 2 => Self::Back, v => Self::Unknown(v) }
    }
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

/// Texture resource parsed from the emitter data block.
#[derive(Debug, Clone)]
pub struct TextureRes {
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
}

/// Parsed .ptcl file.
#[derive(Debug, Default, Clone)]
pub struct PtclFile {
    pub emitter_sets: Vec<EmitterSet>,
    /// Raw texture section bytes (for GPU upload)
    pub texture_section: Vec<u8>,
    pub texture_section_offset: usize,
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
                scale: 0.15,
                scale_random: 0.0,
                rotation_speed: 0.05,
                color0: Vec::new(),
                color1: Vec::new(),
                alpha0: AnimKey3v4k::default(),
                alpha1: AnimKey3v4k::default(),
                scale_anim: AnimKey3v4k::default(),
                textures: Vec::new(),
                mesh_type: 0,
                primitive_index: 0,
                is_one_time: false,
                emission_timing: 0,
                emission_duration: 9999,
            }],
        }).collect();
        Self { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0 }
    }

    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 8 {
            anyhow::bail!("PTCL data too short: {} bytes", data.len());
        }

        // Check magic
        if &data[0..4] == b"EFTF" {
            return Self::parse_eftf(data);
        }
        if &data[0..4] == b"VFXB" {
            return Self::parse_vfxb(data);
        }
        anyhow::bail!("Invalid PTCL magic: {:?}", &data[0..4]);
    }

    /// Parse the Switch VFXB (NintendoWare Effect Binary) format.
    /// Structure: BinaryHeader → sections (ESTA→ESET→EMTR)
    fn parse_vfxb(data: &[u8]) -> anyhow::Result<Self> {
        let r8  = |off: usize| -> u8  { if off < data.len() { data[off] } else { 0 } };
        let r16 = |off: usize| -> u16 {
            if off + 2 > data.len() { return 0; }
            u16::from_le_bytes(data[off..off+2].try_into().unwrap_or([0;2]))
        };
        let r32 = |off: usize| -> u32 {
            if off + 4 > data.len() { return 0; }
            u32::from_le_bytes(data[off..off+4].try_into().unwrap_or([0;4]))
        };
        let rf32 = |off: usize| -> f32 { f32::from_bits(r32(off)) };
        let read_str_fixed = |off: usize, len: usize| -> String {
            if off + len > data.len() { return String::new(); }
            let bytes = &data[off..off+len];
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(len);
            String::from_utf8_lossy(&bytes[..end]).to_string()
        };

        // BinaryHeader (32 bytes):
        // 0x00: magic VFXB (8 bytes with padding)
        // 0x08: GraphicsAPIVersion (u16)
        // 0x0A: VFXVersion (u16)
        // 0x0C: ByteOrder (u16)
        // 0x0E: Alignment (u8)
        // 0x0F: TargetAddressSize (u8)
        // 0x10: NameOffset (u32)
        // 0x14: Flag (u16)
        // 0x16: BlockOffset (u16)  ← first section offset
        // 0x18: RelocationTableOffset (u32)
        // 0x1C: FileSize (u32)
        let vfx_version = r16(0x0A) as u32;
        let block_offset = r16(0x16) as usize;

        eprintln!("[VFXB] version={:#x} block_offset={:#x}", vfx_version, block_offset);

        // SectionHeader (32 bytes):
        // +0x00: magic (4 bytes)
        // +0x04: size (u32)
        // +0x08: childrenOffset (u32)
        // +0x0C: nextSectionOffset (u32)
        // +0x10: attrOffset (u32)
        // +0x14: binaryOffset (u32)
        // +0x18: padding (u32)
        // +0x1C: childrenCount (u16)
        // +0x1E: unknown (u16)
        let section_magic = |base: usize| -> [u8;4] {
            if base + 4 > data.len() { return [0;4]; }
            data[base..base+4].try_into().unwrap_or([0;4])
        };
        let section_children_offset = |base: usize| -> usize { r32(base + 0x08) as usize };
        let section_next_offset     = |base: usize| -> u32   { r32(base + 0x0C) };
        let section_binary_offset   = |base: usize| -> usize { r32(base + 0x14) as usize };
        let section_children_count  = |base: usize| -> usize { r16(base + 0x1C) as usize };

        let mut emitter_sets: Vec<EmitterSet> = Vec::new();

        // Find ESTA section
        let esta_base = block_offset;
        if esta_base + 4 > data.len() || &section_magic(esta_base) != b"ESTA" {
            anyhow::bail!("VFXB: expected ESTA at {:#x}, got {:?}", esta_base, section_magic(esta_base));
        }

        let esta_children_off = section_children_offset(esta_base);
        let esta_children_count = section_children_count(esta_base);
        eprintln!("[VFXB] ESTA: {} emitter sets, children at +{:#x}", esta_children_count, esta_children_off);

        // Walk ESET sections
        let mut eset_base = esta_base + esta_children_off;
        for _set_idx in 0..esta_children_count {
            if eset_base + 4 > data.len() { break; }
            if &section_magic(eset_base) != b"ESET" {
                eprintln!("[VFXB] expected ESET at {:#x}, got {:?}", eset_base, section_magic(eset_base));
                break;
            }

            // Read ESET binary: 16 bytes padding + 64 bytes name
            let eset_bin_off = eset_base + section_binary_offset(eset_base);
            let set_name = read_str_fixed(eset_bin_off + 16, 64);
            let eset_children_count = section_children_count(eset_base);
            let eset_children_off = section_children_offset(eset_base);

            eprintln!("[VFXB] ESET '{}': {} emitters", set_name, eset_children_count);

            let mut emitters: Vec<EmitterDef> = Vec::new();

            // Walk EMTR sections
            let mut emtr_base = eset_base + eset_children_off;
            for _emtr_idx in 0..eset_children_count {
                if emtr_base + 4 > data.len() { break; }
                if &section_magic(emtr_base) != b"EMTR" {
                    eprintln!("[VFXB] expected EMTR at {:#x}, got {:?}", emtr_base, section_magic(emtr_base));
                    break;
                }

                let emtr_bin_off = emtr_base + section_binary_offset(emtr_base);
                if let Some(emitter) = Self::parse_vfxb_emitter(data, emtr_bin_off, vfx_version, &read_str_fixed, &rf32, &r32, &r16, &r8) {
                    emitters.push(emitter);
                }

                // Advance to next EMTR
                let next = section_next_offset(emtr_base);
                if next == u32::MAX { break; }
                emtr_base = emtr_base + next as usize;
            }

            emitter_sets.push(EmitterSet { name: set_name, emitters });

            // Advance to next ESET
            let next = section_next_offset(eset_base);
            if next == u32::MAX { break; }
            eset_base = eset_base + next as usize;
        }

        eprintln!("[VFXB] parsed {} emitter sets", emitter_sets.len());
        Ok(PtclFile { emitter_sets, texture_section: Vec::new(), texture_section_offset: 0 })
    }

    fn parse_vfxb_emitter(
        data: &[u8],
        base: usize,
        version: u32,
        read_str_fixed: &impl Fn(usize, usize) -> String,
        rf32: &impl Fn(usize) -> f32,
        r32: &impl Fn(usize) -> u32,
        r16: &impl Fn(usize) -> u16,
        r8: &impl Fn(usize) -> u8,
    ) -> Option<EmitterDef> {
        if base + 4 > data.len() { return None; }

        // EmitterData layout (version-dependent, targeting Smash Ultimate ~v22-36):
        // +0x00: Flag (u32)
        // +0x04: RandomSeed (u32)
        // +0x08: Padding1 (u32)
        // +0x0C: Padding2 (u32)
        // +0x10: Name (64 bytes if version < 40, else 96 bytes)
        let name_size = if version >= 40 { 96usize } else { 64usize };
        let name = read_str_fixed(base + 0x10, name_size);
        let mut off = base + 0x10 + name_size;

        // EmitterStatic starts here
        // Key fields we need (offsets relative to EmitterStatic start):
        // We'll read sequentially through the struct

        // EmitterStatic:
        // Flags1-4 (4x u32 = 16 bytes)
        // NumColor0Keys..NumParamKeys (6x u32 = 24 bytes)
        // Unknown1, Unknown2 (2x u32 = 8 bytes)
        // [if version > 50: 4x u32 = 16 bytes]
        // Color0LoopRate..ScaleLoopRandom (10x f32 = 40 bytes)
        // Unknown3, Unknown4 (2x f32 = 8 bytes)
        // GravityDirX/Y/Z, GravityScale, AirRes (5x f32 = 20 bytes)
        // val_0x74..val_0x82 (3x f32 = 12 bytes)
        // CenterX, CenterY (2x f32 = 8 bytes)
        // Offset, Padding (2x f32 = 8 bytes)
        // AmplitudeX/Y, CycleX/Y, PhaseRndX/Y, PhaseInitX/Y (8x f32 = 32 bytes)
        // Coefficient0/1, val_0xB8/BC (4x f32 = 16 bytes)
        // TexPatAnim x3 (each = 4+4+4+4 + 32*4 = 144 bytes → 3*144 = 432 bytes)
        // [if version > 40: +2 more TexPatAnim = 288 bytes]
        // TexScrollAnim x3 (each = 20x f32 = 80 bytes → 3*240 = 240 bytes)
        // [if version > 40: +2 more = 160 bytes]
        // ColorScale + 3x f32 (4x f32 = 16 bytes)
        // AnimationKeyTable Color0 (8 * AnimationKey(16 bytes) = 128 bytes)
        // AnimationKeyTable Alpha0 (128 bytes)
        // AnimationKeyTable Color1 (128 bytes)
        // AnimationKeyTable Alpha1 (128 bytes)
        // SoftEdge..FarDistAlpha (8x f32 = 32 bytes)
        // Decal (2x f32), AlphaThreshold+Padding (2x f32) = 16 bytes
        // AddVelToScale..Padding3 (4x f32 = 16 bytes)
        // AnimationKeyTable ScaleAnim (128 bytes)
        // AnimationKeyTable ParamAnim (128 bytes)
        // [if version > 50: 4 more AnimationKeyTables = 512 bytes]
        // [if version > 40: 16x f32 = 64 bytes]
        // RotateInit/Rand/Add/Regist (16x f32 = 64 bytes)
        // ScaleLimitDist (4x f32 = 16 bytes)
        // [if version > 40: 16x f32 = 64 bytes]

        let static_base = off;

        // Skip Flags (4x u32)
        off += 16;
        // NumColor0Keys..NumParamKeys (6x u32)
        let num_color0_keys = r32(off) as usize; off += 4;
        let num_alpha0_keys = r32(off) as usize; off += 4;
        let num_color1_keys = r32(off) as usize; off += 4;
        let num_alpha1_keys = r32(off) as usize; off += 4;
        let num_scale_keys  = r32(off) as usize; off += 4;
        let _num_param_keys = r32(off) as usize; off += 4;
        // Unknown1, Unknown2
        off += 8;
        // version > 50: 4 more u32
        if version > 50 { off += 16; }
        // LoopRates (10x f32)
        off += 40;
        // Unknown3, Unknown4
        off += 8;
        // Gravity + AirRes (5x f32)
        let gravity_x = rf32(off); off += 4;
        let gravity_y = rf32(off); off += 4;
        let gravity_z = rf32(off); off += 4;
        let gravity_scale = rf32(off); off += 4;
        let _air_res = rf32(off); off += 4;
        // val_0x74..val_0x82 (3x f32)
        off += 12;
        // CenterX/Y, Offset, Padding (4x f32)
        off += 16;
        // Amplitude, Cycle, PhaseRnd, PhaseInit (8x f32)
        off += 32;
        // Coefficient0/1, val_0xB8/BC (4x f32)
        off += 16;

        // TexPatAnim: each = 4 floats + 32 ints = 16 + 128 = 144 bytes
        let tex_pat_count = if version > 40 { 5 } else { 3 };
        off += tex_pat_count * 144;

        // TexScrollAnim: each = 20 floats = 80 bytes
        let tex_scroll_count = if version > 40 { 5 } else { 3 };
        off += tex_scroll_count * 80;

        // ColorScale + 3 floats
        off += 16;

        // AnimationKeyTable Color0: 8 keys × 16 bytes = 128 bytes
        // Each AnimationKey: X(f32), Y(f32), Z(f32), Time(f32)
        let color0_table_off = off;
        off += 128; // Color0
        let alpha0_table_off = off;
        off += 128; // Alpha0
        let color1_table_off = off;
        off += 128; // Color1
        let alpha1_table_off = off;
        off += 128; // Alpha1

        // Read color0 keys (X=R, Y=G, Z=B, Time=normalized time)
        let mut color0 = Vec::new();
        for k in 0..num_color0_keys.min(8) {
            let ko = color0_table_off + k * 16;
            if ko + 16 > data.len() { break; }
            color0.push(ColorKey {
                frame: rf32(ko + 12), // Time
                r: rf32(ko + 0),
                g: rf32(ko + 4),
                b: rf32(ko + 8),
                a: 1.0,
            });
        }

        // Read alpha0 keys — these are scalar (X=value, Time=time)
        let alpha0_anim = if num_alpha0_keys > 0 {
            let k0 = alpha0_table_off;
            let k_last = alpha0_table_off + (num_alpha0_keys.min(8) - 1) * 16;
            AnimKey3v4k {
                start_value: rf32(k0),
                start_diff: if num_alpha0_keys > 1 { rf32(k0 + 16) - rf32(k0) } else { 0.0 },
                end_diff: if num_alpha0_keys > 2 { rf32(k_last) - rf32(k_last - 16) } else { -rf32(k0) },
                time2: if num_alpha0_keys > 1 { rf32(k0 + 16 + 12) } else { 0.5 },
                time3: if num_alpha0_keys > 2 { rf32(k_last + 12) } else { 0.8 },
            }
        } else {
            AnimKey3v4k::default()
        };

        // SoftEdge..FarDistAlpha (8x f32 = 32 bytes)
        off += 32;
        // Decal + AlphaThreshold + Padding (4x f32 = 16 bytes)
        off += 16;
        // AddVelToScale..Padding3 (4x f32 = 16 bytes)
        off += 16;
        // ScaleAnim (128 bytes)
        let scale_anim_off = off;
        off += 128;
        // ParamAnim (128 bytes)
        off += 128;
        // version > 50: 4 more AnimationKeyTables
        if version > 50 { off += 512; }
        // version > 40: 16x f32
        if version > 40 { off += 64; }

        // RotateInit (4x f32), RotateInitRand (4x f32), RotateAdd (4x f32 + regist), RotateAddRand (4x f32)
        off += 64;
        // ScaleLimitDist (4x f32)
        off += 16;
        // version > 40: 16x f32
        if version > 40 { off += 64; }

        // EmitterInfo starts here
        let emitter_info_base = off;
        // IsParticleDraw..padding3 (16 bytes)
        off += 16;
        // RandomSeed (u32), DrawPath (u32), AlphaFadeTime (i32), FadeInTime (i32)
        off += 16;
        // Trans (3x f32), TransRand (3x f32), Rotate (3x f32), RotateRand (3x f32), Scale (3x f32)
        off += 60;
        // Color0 RGBA (4x f32), Color1 RGBA (4x f32)
        let emitter_color0_r = rf32(emitter_info_base + 16 + 16 + 60);
        let emitter_color0_g = rf32(emitter_info_base + 16 + 16 + 64);
        let emitter_color0_b = rf32(emitter_info_base + 16 + 16 + 68);
        let emitter_color0_a = rf32(emitter_info_base + 16 + 16 + 72);
        off += 32;
        // EmissionRangeNear/Far/Ratio (3x f32)
        off += 12;

        // EmitterInheritance
        // 16 bytes flags + [if version > 40: 8 bytes] + VelocityRate + ScaleRate (2x f32)
        let inherit_size = 16 + (if version > 40 { 8 } else { 0 }) + 8;
        off += inherit_size;

        // Emission struct (fixed 0x10 alignment, ~21 fields)
        // isOneTime(bool), IsWorldGravity(bool), IsEmitDistEnabled(bool), IsWorldOrientedVelocity(bool) = 4 bytes
        // Start(u32), Timing(u32), Duration(u32) = 12 bytes
        // Rate(f32), RateRandom(f32) = 8 bytes
        // Interval(i32), IntervalRandom(f32) = 8 bytes
        // PositionRandom(f32), GravityScale(f32), GravityDirX/Y/Z(3x f32) = 20 bytes
        // EmitterDistUnit/Min/Max/Marg(4x f32), EmitterDistParticlesMax(i32) = 20 bytes
        let emission_base = off;
        let is_one_time = r8(emission_base) != 0;
        let emission_start  = r32(emission_base + 4);
        let emission_timing = r32(emission_base + 8);
        let emission_duration = r32(emission_base + 12);
        let emission_rate = rf32(emission_base + 16);
        off += 72; // total Emission size

        // EmitterShapeInfo
        // VolumeType..IsGpuEmitter (8 bytes)
        // SweepLongitude..VolumeFormScaleZ (12x f32 = 48 bytes)
        // PrimEmitType(i32), PrimitiveIndex(u64), NumDivideCircle(i32), NumDivideCircleRandom(i32)
        // NumDivideLine(i32), NumDivideLineRandom(i32) = 4+8+4+4+4+4 = 28 bytes
        // [if version < 40: 8 bytes padding]
        let shape_base = off;
        let volume_type = r8(shape_base);
        let emit_type = EmitType::from(volume_type as u32);
        let shape_size = 8 + 48 + 28 + (if version < 40 { 8 } else { 0 });
        off += shape_size;

        // EmitterRenderState
        // IsBlendEnable(bool), IsDepthTest(bool), DepthFunc(u8), IsDepthMask(bool) = 4 bytes
        // IsAlphaTest(bool), AlphaFunc(u8), BlendType(u8), DisplaySide(u8) = 4 bytes
        // AlphaThreshold(f32), padding(u32) = 8 bytes
        let render_base = off;
        let blend_type_raw = r8(render_base + 6);
        let display_side_raw = r8(render_base + 7);
        let blend_type = BlendType::from(blend_type_raw as u32);
        let display_side = DisplaySide::from(display_side_raw as u32);
        off += 16;

        // ParticleData
        // InfiniteLife(bool)..val_0xF(u8) = 16 bytes
        // Life(i32), LifeRandom(i32) = 8 bytes
        let particle_base = off;
        let infinite_life = r8(particle_base) != 0;
        let particle_life = r32(particle_base + 16) as f32; // Life in frames
        let particle_life_random = r32(particle_base + 20) as f32;
        // MomentumRandom(f32)=4, PrimitiveVertexInfoFlags(u32)=4, PrimitiveID(u64)=8, PrimitiveExID(u64)=8 = 24
        // LoopColor0..ScaleLoopRandom (10 bools = 10 bytes), PrimFlag1/2 (2 bytes) = 12
        // Color0LoopRate..ScaleLoopRate: version < 50 → 5x i32 = 20, version >= 50 → 5x i16 = 10
        let particle_data_size = 16 + 8 + 24 + 12 + (if version < 50 { 20 } else { 10 });
        off += particle_data_size;

        // EmitterCombiner (version-dependent, comes BEFORE ParticleVelocityInfo)
        // version < 36:  EmitterCombiner       = 8+8+1+1+1+1+4 = 24 bytes
        // version == 36: EmitterCombinerV36     = 8 bytes
        // version > 40:  EmitterCombinerV40     = 8+8+8+2+4+4 = 34 bytes (v>=50 adds padding)
        let combiner_size = if version < 36 { 24 }
            else if version == 36 { 8 }
            else if version <= 40 { 24 } // same as < 36 for v40
            else if version < 50 { 24 }  // EmitterCombinerV40 without extra padding
            else { 28 };                  // EmitterCombinerV40 with v50 padding
        off += combiner_size;

        // ShaderRefInfo
        // Type(1)+val_0x2(1)+val_0x3(1)+val_0x4(1) = 4
        // ShaderIndex..CustomShaderIndex (5x i32 = 20)
        // CustomShaderFlag(u64) + CustomShaderSwitch(u64) = 16  [version < 50]
        // Unknown1(u64) = 8  [version < 22 only]
        // ExtraShaderIndex2(i32) + val_0x34(i32) = 8
        // Unknown2(u64) = 8  [version > 50]
        // UserShaderDefine1(16) + UserShaderDefine2(16) = 32
        let shader_size = 4 + 20
            + (if version < 50 { 16 } else { 0 })
            + (if version < 22 { 8 } else { 0 })
            + 8
            + (if version > 50 { 8 } else { 0 })
            + 32;
        off += shader_size;

        // ActionInfo: ActionIndex(u32) = 4 bytes, + version > 40: 5x u32 = 20 bytes
        let action_size = 4 + (if version > 40 { 20 } else { 0 });
        off += action_size;

        // DepthMode (version > 40): 16 bytes
        // PassInfo (version > 40): 52 bytes
        if version > 40 { off += 16 + 52; }

        // ParticleVelocityInfo
        // AllDirection(f32), DesignatedDirScale(f32), DesignatedDirX/Y/Z(3x f32)
        // DiffusionDirAngle(f32), XZDiffusion(f32), DiffusionX/Y/Z(3x f32)
        // VelRandom(f32), EmVelInherit(f32) = 12x f32 = 48 bytes
        let vel_base = off;
        let all_direction_speed = rf32(vel_base);
        let vel_random = rf32(vel_base + 44);
        off += 48;

        // UnknownV36 (version >= 36): 4x f32 = 16 bytes
        if version >= 36 { off += 16; }

        // ParticleColor
        // IsSoftParticle..val_0x7 (8 bytes)
        // Color0Type..Alpha1Type (4 bytes)
        // Color0R/G/B, Alpha0, Color1R/G/B, Alpha1 (8x f32 = 32 bytes)
        let pcolor_base = off;
        let base_color_r = rf32(pcolor_base + 12);
        let base_color_g = rf32(pcolor_base + 16);
        let base_color_b = rf32(pcolor_base + 20);
        let base_alpha   = rf32(pcolor_base + 24);
        off += 44;

        // ParticleScale
        // ScaleX/Y/Z (3x f32), ScaleRandomX/Y/Z (3x f32) = 24 bytes
        // 4 bytes flags, ScaleMin/Max (2x f32) = 16 bytes
        let pscale_base = off;
        let scale_x = rf32(pscale_base);
        let scale_y = rf32(pscale_base + 4);

        // Use color0 from animation table if available, else from ParticleColor base
        let final_color0 = if color0.is_empty() {
            vec![ColorKey { frame: 0.0, r: base_color_r, g: base_color_g, b: base_color_b, a: base_alpha }]
        } else {
            color0
        };

        // Lifetime: InfiniteLife → use emission_duration; Life=0 → use LifeRandom or duration
        let lifetime = if infinite_life {
            emission_duration as f32
        } else if particle_life > 0.0 {
            particle_life
        } else if particle_life_random > 0.0 {
            particle_life_random
        } else if emission_duration > 0 && emission_duration < 9999 {
            emission_duration as f32
        } else {
            20.0
        };

        // Scale: use ScaleX from ParticleScale
        let scale = if scale_x > 0.0 { scale_x } else if scale_y > 0.0 { scale_y } else { 0.15 };

        // Skip mesh/primitive emitters (scale=0 means no visible billboard particle)
        if scale <= 0.0 && lifetime <= 0.0 {
            eprintln!("[EMTR] '{}' skipped — zero scale and lifetime (mesh emitter?)", name);
            return None;
        }

        // Speed: AllDirection velocity — clamp subnormals to 0
        let speed = if all_direction_speed.is_normal() && all_direction_speed > 0.0 {
            all_direction_speed
        } else {
            0.0
        };

        // Emission rate
        let rate = if emission_rate > 0.0 { emission_rate } else { 8.0 };

        eprintln!("[EMTR] '{}' one_time={} timing={} duration={} life={} scale={:.3} speed={:.3} rate={:.2} blend={:?}",
            name, is_one_time, emission_timing, emission_duration, lifetime, scale, speed, rate, blend_type);

        Some(EmitterDef {
            name,
            emit_type,
            blend_type,
            display_side,
            emission_rate: rate,
            emission_rate_random: 0.0,
            initial_speed: speed,
            speed_random: vel_random,
            accel: Vec3::new(gravity_x * gravity_scale, gravity_y * gravity_scale, gravity_z * gravity_scale),
            lifetime,
            lifetime_random: particle_life_random,
            scale,
            scale_random: 0.0,
            rotation_speed: 0.0,
            color0: final_color0,
            color1: Vec::new(),
            alpha0: alpha0_anim,
            alpha1: AnimKey3v4k::default(),
            scale_anim: AnimKey3v4k::default(),
            textures: Vec::new(),
            mesh_type: 0,
            primitive_index: 0,
            is_one_time,
            emission_timing,
            emission_duration,
        })
    }

    /// Parse the Wii U EFTF format (legacy, kept for completeness).
    fn parse_eftf(data: &[u8]) -> anyhow::Result<Self> {
        let r32 = |off: usize| -> u32 {
            if off + 4 > data.len() { return 0; }
            u32::from_le_bytes(data[off..off+4].try_into().unwrap_or([0;4]))
        };
        let r16 = |off: usize| -> u16 {
            if off + 2 > data.len() { return 0; }
            u16::from_le_bytes(data[off..off+2].try_into().unwrap_or([0;2]))
        };
        let rf32 = |off: usize| -> f32 { f32::from_bits(r32(off)) };

        let _version = r32(0x4);
        let effect_count = r32(0x8) as usize;
        let string_table_offset = r32(0x10) as usize;
        let texture_section_offset = r32(0x14) as usize;
        let texture_section_size = r32(0x18) as usize;

        let texture_section = if texture_section_offset > 0 && texture_section_offset + texture_section_size <= data.len() {
            data[texture_section_offset..texture_section_offset + texture_section_size].to_vec()
        } else {
            Vec::new()
        };

        // Read string from string table
        let read_str = |offset: u32| -> String {
            let abs = string_table_offset + offset as usize;
            if abs >= data.len() { return String::new(); }
            let end = data[abs..].iter().position(|&b| b == 0).unwrap_or(0);
            String::from_utf8_lossy(&data[abs..abs+end]).to_string()
        };

        // Effects start at 0x48
        let effects_base = 0x48usize;
        let mut emitter_sets = Vec::new();

        for i in 0..effect_count {
            let eff_off = effects_base + i * 0x18;
            if eff_off + 0x18 > data.len() { break; }

            let name_offset = r32(eff_off + 0x8);
            let emitter_count = r32(eff_off + 0x10) as usize;
            let emitter_list_offset = r32(eff_off + 0x14) as usize;

            let set_name = read_str(name_offset);
            let mut emitters = Vec::new();

            for j in 0..emitter_count {
                let emitter_ptr_off = emitter_list_offset + j * 0x10;
                if emitter_ptr_off + 4 > data.len() { break; }
                let emitter_data_offset = r32(emitter_ptr_off) as usize;
                if emitter_data_offset == 0 || emitter_data_offset + 0x8BC > data.len() { continue; }

                let ed = emitter_data_offset;

                let emit_type = EmitType::from(r32(ed + 0x0));
                let blend_type = BlendType::from(r32(ed + 0x324));
                let display_side = DisplaySide::from(r32(ed + 0x320));
                let name_off = r32(ed + 0x38);
                let emitter_name = read_str(name_off);

                let emission_rate = rf32(ed + 0x444).max(1.0); // treat as particles/frame, min 1
                let emission_rate_random = rf32(ed + 0x448);
                let initial_speed = rf32(ed + 0x45C);
                let speed_random = rf32(ed + 0x4C0);
                let accel_x = rf32(ed + 0x4A4);
                let accel_y = rf32(ed + 0x4A8);
                let accel_z = rf32(ed + 0x4AC);
                let mesh_type = r32(ed + 0x4B8);
                let primitive_index = r32(ed + 0x2DC);

                // Alpha animations (3v4k) at 0x784 and 0x798
                let alpha0 = AnimKey3v4k {
                    start_value: rf32(ed + 0x784),
                    start_diff:  rf32(ed + 0x788),
                    end_diff:    rf32(ed + 0x78C),
                    time2:       rf32(ed + 0x790),
                    time3:       rf32(ed + 0x794),
                };
                let alpha1 = AnimKey3v4k {
                    start_value: rf32(ed + 0x798),
                    start_diff:  rf32(ed + 0x79C),
                    end_diff:    rf32(ed + 0x7A0),
                    time2:       rf32(ed + 0x7A4),
                    time3:       rf32(ed + 0x7A8),
                };
                // Scale anim — use alpha0 slot shape but for scale (approximation)
                let scale_anim = AnimKey3v4k::default();

                // Rotation acceleration at 0x828
                let rotation_speed = rf32(ed + 0x828);

                // Color tables at 0x650 (color0) and 0x6D0 (color1), 8 entries each, 16 bytes per entry
                let mut color0 = Vec::new();
                let mut color1 = Vec::new();
                for k in 0..8usize {
                    let c0_off = ed + 0x650 + k * 16;
                    let c1_off = ed + 0x6D0 + k * 16;
                    if c0_off + 16 <= data.len() {
                        color0.push(ColorKey {
                            frame: rf32(c0_off),
                            r: rf32(c0_off + 4),
                            g: rf32(c0_off + 8),
                            b: rf32(c0_off + 12),
                            a: 1.0,
                        });
                    }
                    if c1_off + 16 <= data.len() {
                        color1.push(ColorKey {
                            frame: rf32(c1_off),
                            r: rf32(c1_off + 4),
                            g: rf32(c1_off + 8),
                            b: rf32(c1_off + 12),
                            a: 1.0,
                        });
                    }
                }

                // Texture resources at 0x40 (tex1), 0x118 (tex2), 0x1F0 (tex3), each 0xD8 bytes
                let mut textures = Vec::new();
                for tex_idx in 0..3usize {
                    let tex_off = ed + 0x40 + tex_idx * 0xD8;
                    if tex_off + 0xD8 > data.len() { break; }
                    let width = r16(tex_off + 0x0);
                    let height = r16(tex_off + 0x2);
                    let wrap_mode = if tex_off + 0xC < data.len() { data[tex_off + 0xC] } else { 0 };
                    let filter_mode = if tex_off + 0xD < data.len() { data[tex_off + 0xD] } else { 0 };
                    let mipmap_count = r32(tex_off + 0x10);
                    let original_format = r32(tex_off + 0x20);
                    let original_data_offset = r32(tex_off + 0x24);
                    let original_data_size = r32(tex_off + 0x28);
                    let ftx_format = r32(tex_off + 0x2C);
                    let ftx_data_size = r32(tex_off + 0x30);
                    let ftx_data_offset = r32(tex_off + 0x34);
                    if width == 0 && height == 0 { break; }
                    textures.push(TextureRes {
                        width, height, ftx_format, ftx_data_offset, ftx_data_size,
                        original_format, original_data_offset, original_data_size,
                        wrap_mode, filter_mode, mipmap_count,
                    });
                }

                // Lifetime: not directly in emitter data in this version — default to 60 frames
                let lifetime = 60.0f32;
                let lifetime_random = 0.0f32;
                let scale = 1.0f32;
                let scale_random = 0.0f32;

                emitters.push(EmitterDef {
                    name: emitter_name,
                    emit_type,
                    blend_type,
                    display_side,
                    emission_rate,
                    emission_rate_random,
                    initial_speed,
                    speed_random,
                    accel: Vec3::new(accel_x, accel_y, accel_z),
                    lifetime,
                    lifetime_random,
                    scale,
                    scale_random,
                    rotation_speed,
                    color0,
                    color1,
                    alpha0,
                    alpha1,
                    scale_anim,
                    textures,
                    mesh_type,
                    primitive_index,
                    is_one_time: false,
                    emission_timing: 0,
                    emission_duration: 9999,
                });
            }

            emitter_sets.push(EmitterSet { name: set_name, emitters });
        }

        Ok(PtclFile { emitter_sets, texture_section, texture_section_offset })
    }
}

/// Sample a color from a color key table at normalized time `t` (0..1).
/// Falls back to white if the table is empty.
fn sample_color(keys: &[ColorKey], t: f32) -> Vec4 {
    if keys.is_empty() {
        return Vec4::ONE;
    }
    if keys.len() == 1 || t <= 0.0 {
        let k = &keys[0];
        return Vec4::new(k.r, k.g, k.b, k.a);
    }
    // Find the two surrounding keys by frame (keys store frame as 0..1 normalized or raw frame)
    // Treat key.frame as normalized 0..1
    let last = &keys[keys.len() - 1];
    if t >= last.frame {
        return Vec4::new(last.r, last.g, last.b, last.a);
    }
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
    pub texture_idx: usize,
    pub blend_type: BlendType,
}

impl Particle {
    pub fn life_t(&self) -> f32 {
        if self.lifetime <= 0.0 { 1.0 } else { (self.age / self.lifetime).clamp(0.0, 1.0) }
    }
    pub fn is_dead(&self) -> bool { self.age >= self.lifetime }
}

/// Tracks fractional emission accumulator per active emitter instance.
#[derive(Debug, Clone)]
pub struct EmitterInstance {
    emitter_set_idx: usize,
    emitter_idx: usize,
    bone_name: String,
    start_frame: f32,
    end_frame: f32,
    emit_accum: f32,
}

/// The full CPU particle system state.
#[derive(Debug, Default)]
pub struct ParticleSystem {
    pub particles: Vec<Particle>,
    pub active_emitters: Vec<EmitterInstance>,
    last_frame: f32,
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
        start_frame: f32,
        end_frame: f32,
        eff_index: &EffIndex,
        ptcl: &PtclFile,
    ) {
        // Try exact match first, then lowercase (eff names are often uppercase)
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
                start_frame,
                end_frame,
                emit_accum: 0.0,
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

        // Emit new particles from active emitters
        for inst in &mut self.active_emitters {
            let is_one_shot = inst.emitter_set_idx < ptcl.emitter_sets.len()
                && ptcl.emitter_sets[inst.emitter_set_idx].emitters.get(inst.emitter_idx)
                    .map(|e| e.is_one_time)
                    .unwrap_or(false)
                || (inst.end_frame - inst.start_frame).abs() < 1.0;

            // One-shot effects: emit a single burst on the spawn frame only.
            // Following effects: emit continuously while active.
            if is_one_shot {
                // Only emit on the exact spawn frame (first step where target >= start)
                if inst.emit_accum < 0.0 { continue; } // already fired
                if target_frame < inst.start_frame { continue; }
            } else {
                if target_frame < inst.start_frame || target_frame > inst.end_frame { continue; }
            }

            let Some(set) = ptcl.emitter_sets.get(inst.emitter_set_idx) else { continue };
            let Some(emitter) = set.emitters.get(inst.emitter_idx) else { continue };

            // Get bone world position for spawn origin
            let bone_mat = bone_matrices.get(&inst.bone_name)
                .or_else(|| bone_matrices.get(&inst.bone_name.to_lowercase()))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let origin = bone_mat.col(3).truncate();

            let to_emit = if is_one_shot {
                // Burst: clamp to a reasonable count (smash effects are typically 6–16 particles)
                inst.emit_accum = -1.0; // mark as fired
                (emitter.emission_rate.round() as usize).clamp(4, 16)
            } else {
                inst.emit_accum += emitter.emission_rate * dt;
                let n = inst.emit_accum.floor() as usize;
                inst.emit_accum -= n as f32;
                n.min(32)
            };

            // Sample base color from color0 table at t=0
            let base_color = sample_color(&emitter.color0, 0.0);

            for i in 0..to_emit {
                // Spherical spread using golden-angle fibonacci distribution
                let seed = (self.particles.len() + i) as f32;
                let theta = seed * 2.399; // golden angle in radians
                let phi = (1.0 - 2.0 * ((seed + 0.5) / to_emit.max(1) as f32)).acos();
                let dir = Vec3::new(
                    phi.sin() * theta.cos(),
                    phi.sin() * theta.sin(),
                    phi.cos(),
                );
                let speed = emitter.initial_speed.max(0.5)
                    * (1.0 + (seed * 0.37).sin() * emitter.speed_random.min(0.5));
                let velocity = dir * speed;

                self.particles.push(Particle {
                    position: origin,
                    velocity,
                    age: 0.0,
                    lifetime: emitter.lifetime.clamp(8.0, 30.0),
                    color: base_color,
                    size: emitter.scale.clamp(0.05, 0.5),
                    rotation: seed * 0.5,
                    rotation_speed: emitter.rotation_speed,
                    emitter_set_idx: inst.emitter_set_idx,
                    emitter_idx: inst.emitter_idx,
                    texture_idx: 0,
                    blend_type: emitter.blend_type,
                });
            }
        }

        // Integrate existing particles
        for p in &mut self.particles {
            let Some(set) = ptcl.emitter_sets.get(p.emitter_set_idx) else { p.age = p.lifetime; continue };
            let Some(emitter) = set.emitters.get(p.emitter_idx) else { p.age = p.lifetime; continue };

            p.age += dt;
            p.velocity += emitter.accel * dt;
            p.position += p.velocity * dt;
            p.rotation += p.rotation_speed * dt;

            let t = p.life_t();
            let alpha = emitter.alpha0.sample(t);
            let rgb = sample_color(&emitter.color0, t);
            p.color = Vec4::new(rgb.x, rgb.y, rgb.z, alpha * rgb.w);
            p.size = emitter.scale.clamp(0.05, 0.5) * emitter.scale_anim.sample(t).max(0.01);
        }

        // Remove dead particles
        self.particles.retain(|p| !p.is_dead());
        eprintln!("[STEP_END] frame={target_frame} particles_after_retain={}", self.particles.len());
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
}

impl SwordTrail {
    pub fn new(effect_name: &str, tip_bone: &str, base_bone: &str) -> Self {
        Self {
            effect_name: effect_name.to_string(),
            tip_bone: tip_bone.to_string(),
            base_bone: base_bone.to_string(),
            samples: Vec::new(),
            max_samples: 16,
            active: true,
            blend_type: BlendType::Add,
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

    pub fn start_trail(&mut self, effect_name: &str, tip_bone: &str, base_bone: &str) {
        // Remove any existing trail for this effect
        self.trails.retain(|t| t.effect_name != effect_name);
        self.trails.push(SwordTrail::new(effect_name, tip_bone, base_bone));
    }

    pub fn stop_trail(&mut self, effect_name: &str) {
        for t in &mut self.trails { if t.effect_name == effect_name { t.stop(); } }
    }

    pub fn step(&mut self, bone_matrices: &HashMap<String, Mat4>) {
        for trail in &mut self.trails { trail.record(bone_matrices); }
        self.trails.retain(|t| t.active || !t.samples.is_empty());
    }
}
