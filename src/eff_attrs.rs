//! Every editable attribute of one emitter, as one table.
//!
//! An `.eff` emitter is ~2.7 KB of named fields (`EmitterData`), and the editor used to expose
//! nine of them. This module names the rest: each entry pairs a stable id with a getter and a
//! setter over `effect_library::EmitterData`, plus the label, grouping and description the UI
//! draws. Layout, types and descriptions come from LilyLavender's EffectResearch
//! (`documentation/EmitterData.md`), which documents the section this crate parses.
//!
//! The id is the Rust field path (`emission.rate`, `sampler0.wrap_u`). It is what a project file
//! records, so it must stay stable — renaming one silently drops that edit from every project
//! that carries it.
//!
//! Fields the documentation marks as padding or reserved are deliberately absent: the game reads
//! nothing from them, so an editor row for one is a control that does nothing.
//!
//! Attributes living in an `Option` sub-struct (the samplers, the texture animations, the
//! fluctuation block, the combiner) return `None` from `get` when the emitter has no such block.
//! That is not the same as zero, and the UI hides the row rather than offering an edit that
//! `set` would drop on the floor.

use effect_library::{ColorType, EmitterData, WrapMode};
use serde::{Deserialize, Serialize};

/// One attribute's value, in the signed, unsigned and floating-point shapes the format stores.
///
/// Untagged on purpose: a project file reads `{"emission.rate": 12.5}` rather than
/// `{"emission.rate": {"float": 12.5}}`. The ambiguity is harmless because the SETTER knows the
/// field's real type and converts — an integer attribute read back as `Float` still lands as an
/// integer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttrValue {
    Int(i64),
    UInt(u64),
    Float(f32),
}

impl AttrValue {
    pub fn as_f32(self) -> f32 {
        match self {
            AttrValue::Int(v) => v as f32,
            AttrValue::UInt(v) => v as f32,
            AttrValue::Float(v) => v,
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            AttrValue::Int(v) => v,
            AttrValue::UInt(v) => v.min(i64::MAX as u64) as i64,
            AttrValue::Float(v) => v.round() as i64,
        }
    }

    pub fn as_u64(self) -> u64 {
        match self {
            AttrValue::Int(v) => v.max(0) as u64,
            AttrValue::UInt(v) => v,
            AttrValue::Float(v) => v.round().clamp(0.0, u64::MAX as f32) as u64,
        }
    }

    pub fn as_bool(self) -> bool {
        self.as_i64() != 0
    }

    /// Whether two readings of the same attribute are the same value.
    ///
    /// Floats compare with a tolerance because the UI round-trips them through a drag widget;
    /// integers compare exactly, since a one-step difference in a type index or a frame count is
    /// the whole edit.
    pub fn same(self, other: Self) -> bool {
        match (self, other) {
            (AttrValue::Float(a), AttrValue::Float(b)) => (a - b).abs() <= 1e-6,
            (AttrValue::Int(a), AttrValue::Int(b)) => a == b,
            (AttrValue::UInt(a), AttrValue::UInt(b)) => a == b,
            (a, b) => (a.as_f32() - b.as_f32()).abs() <= 1e-6,
        }
    }

    /// Whether this value can be written to a project file. `serde_json` cannot encode NaN or
    /// infinity, so a non-finite value would fail the whole save rather than one field.
    pub fn is_storable(self) -> bool {
        match self {
            AttrValue::Float(v) => v.is_finite(),
            AttrValue::Int(_) | AttrValue::UInt(_) => true,
        }
    }
}

/// How the UI should present an attribute, and how much a drag step is worth.
#[derive(Debug, Clone, Copy)]
pub enum AttrKind {
    Float {
        speed: f32,
    },
    Int,
    UInt,
    /// A byte the format uses as a boolean — `00` or `01`.
    Flag,
    /// A small integer whose values are named. Index into the slice IS the stored value.
    Enum(&'static [&'static str]),
}

pub struct Attr {
    /// Stable id and project-file key: the Rust field path.
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    pub kind: AttrKind,
    pub get: fn(&EmitterData) -> Option<AttrValue>,
    pub set: fn(&mut EmitterData, AttrValue),
}

/// Assign editor-sized integers without Rust's narrowing-cast wraparound. A typed field still
/// defines its own legal range; values outside it land on the nearest representable endpoint.
trait AssignI64 {
    fn assign_i64(&mut self, value: i64);
}

macro_rules! assign_signed {
    ($($ty:ty),+ $(,)?) => {$(
        impl AssignI64 for $ty {
            fn assign_i64(&mut self, value: i64) {
                *self = value.clamp(<$ty>::MIN as i64, <$ty>::MAX as i64) as $ty;
            }
        }
    )+};
}

macro_rules! assign_unsigned {
    ($($ty:ty),+ $(,)?) => {$(
        impl AssignI64 for $ty {
            fn assign_i64(&mut self, value: i64) {
                *self = value.clamp(0, <$ty>::MAX as i64) as $ty;
            }
        }
    )+};
}

assign_signed!(i8, i16, i32, i64);
assign_unsigned!(u8, u16, u32);

trait AssignU64 {
    fn assign_u64(&mut self, value: u64);
}

macro_rules! assign_u64 {
    ($($ty:ty),+ $(,)?) => {$(
        impl AssignU64 for $ty {
            fn assign_u64(&mut self, value: u64) {
                *self = value.min(<$ty>::MAX as u64) as $ty;
            }
        }
    )+};
}

assign_u64!(u8, u16, u32, u64);

// Groups, in the order the UI stacks them. Named after the sections of the emitter binary so a
// row here can be found in the EffectResearch tables without translation.
pub const G_COMMON: &str = "Common";
pub const G_EMISSION: &str = "Emission";
pub const G_SHAPE: &str = "Emitter shape";
pub const G_INFO: &str = "Emitter transform & colour";
pub const G_INFO_FLAGS: &str = "Emitter behaviour";
pub const G_PARTICLE: &str = "Particle";
pub const G_PTCL_VELOCITY: &str = "Particle velocity";
pub const G_PTCL_COLOR: &str = "Particle colour";
pub const G_PTCL_SCALE: &str = "Particle scale";
pub const G_PTCL_ROTATE: &str = "Particle rotation";
pub const G_MOTION: &str = "Gravity & air resistance";
pub const G_FLUCTUATION: &str = "Fluctuation";
pub const G_ANIM_RATES: &str = "Animation keys & loops";
pub const G_COLOR_KEYS: &str = "Colour keyframes";
pub const G_ALPHA_KEYS: &str = "Alpha keyframes";
pub const G_SCALE_KEYS: &str = "Scale keyframes";
pub const G_PARAM_KEYS: &str = "Shader coefficient keyframes";
pub const G_RENDER: &str = "Render state";
pub const G_COMBINER: &str = "Combiner";
pub const G_ALPHA_FX: &str = "Soft / fresnel / distance alpha";
pub const G_INHERIT: &str = "Child inheritance";
pub const G_SHADER: &str = "Shader references";
pub const G_SAMPLER0: &str = "Sampler 0";
pub const G_SAMPLER1: &str = "Sampler 1";
pub const G_SAMPLER2: &str = "Sampler 2";
pub const G_TEXANIM0: &str = "Texture 0 animation";
pub const G_TEXANIM1: &str = "Texture 1 animation";
pub const G_TEXANIM2: &str = "Texture 2 animation";
pub const G_TEXPAT0: &str = "Texture 0 pattern anim";
pub const G_TEXPAT1: &str = "Texture 1 pattern anim";
pub const G_TEXPAT2: &str = "Texture 2 pattern anim";
pub const G_TEXUV0: &str = "Texture 0 UV anim";
pub const G_TEXUV1: &str = "Texture 1 UV anim";
pub const G_TEXUV2: &str = "Texture 2 UV anim";

pub const GROUPS: &[&str] = &[
    G_COMMON,
    G_EMISSION,
    G_SHAPE,
    G_INFO,
    G_INFO_FLAGS,
    G_PARTICLE,
    G_PTCL_VELOCITY,
    G_PTCL_COLOR,
    G_PTCL_SCALE,
    G_PTCL_ROTATE,
    G_MOTION,
    G_FLUCTUATION,
    G_ANIM_RATES,
    G_COLOR_KEYS,
    G_ALPHA_KEYS,
    G_SCALE_KEYS,
    G_PARAM_KEYS,
    G_RENDER,
    G_COMBINER,
    G_ALPHA_FX,
    G_INHERIT,
    G_SHADER,
    G_SAMPLER0,
    G_SAMPLER1,
    G_SAMPLER2,
    G_TEXANIM0,
    G_TEXANIM1,
    G_TEXANIM2,
    G_TEXPAT0,
    G_TEXPAT1,
    G_TEXPAT2,
    G_TEXUV0,
    G_TEXUV1,
    G_TEXUV2,
];

// ── Table constructors ────────────────────────────────────────────────────────
//
// Each macro builds one row. The closures are non-capturing, so they coerce to the `fn` pointers
// the table stores. `as _` in the integer setters resolves to whatever width the field has, which
// is what keeps one macro usable for u8/i16/i32/u32/u64 alike.

macro_rules! fl {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $speed:expr, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Float { speed: $speed },
            get: |d| Some(AttrValue::Float(d.$($f).+)),
            set: |d, v| d.$($f).+ = v.as_f32(),
        }
    };
}

macro_rules! it {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Int,
            get: |d| Some(AttrValue::Int(d.$($f).+ as i64)),
            set: |d, v| AssignI64::assign_i64(&mut d.$($f).+, v.as_i64()),
        }
    };
}

macro_rules! uit {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::UInt,
            get: |d| Some(AttrValue::UInt(d.$($f).+ as u64)),
            set: |d, v| AssignU64::assign_u64(&mut d.$($f).+, v.as_u64()),
        }
    };
}

/// A flag the format stores as a `u8`.
macro_rules! fg {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Flag,
            get: |d| Some(AttrValue::Int(d.$($f).+ as i64)),
            set: |d, v| d.$($f).+ = u8::from(v.as_bool()),
        }
    };
}

/// A flag `effect_library` decodes to a `bool`.
macro_rules! fb {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Flag,
            get: |d| Some(AttrValue::Int(d.$($f).+ as i64)),
            set: |d, v| d.$($f).+ = v.as_bool(),
        }
    };
}

// The four `opt_*` macros mirror the four above, for a field inside a sub-struct the emitter may
// not have at all. `get` yielding None is what tells the UI to leave the row out; `set` on an
// absent block does nothing, because there is nowhere to put the value.

macro_rules! opt_fl {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $speed:expr, $opt:ident, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Float { speed: $speed },
            get: |d| d.$opt.as_ref().map(|s| AttrValue::Float(s.$($f).+)),
            set: |d, v| { if let Some(s) = d.$opt.as_mut() { s.$($f).+ = v.as_f32(); } },
        }
    };
}

macro_rules! opt_it {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $opt:ident, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Int,
            get: |d| d.$opt.as_ref().map(|s| AttrValue::Int(s.$($f).+ as i64)),
            set: |d, v| { if let Some(s) = d.$opt.as_mut() { AssignI64::assign_i64(&mut s.$($f).+, v.as_i64()); } },
        }
    };
}

macro_rules! opt_uit {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $opt:ident, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::UInt,
            get: |d| d.$opt.as_ref().map(|s| AttrValue::UInt(s.$($f).+ as u64)),
            set: |d, v| { if let Some(s) = d.$opt.as_mut() { AssignU64::assign_u64(&mut s.$($f).+, v.as_u64()); } },
        }
    };
}

macro_rules! opt_fg {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $opt:ident, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Flag,
            get: |d| d.$opt.as_ref().map(|s| AttrValue::Int(s.$($f).+ as i64)),
            set: |d, v| { if let Some(s) = d.$opt.as_mut() { s.$($f).+ = u8::from(v.as_bool()); } },
        }
    };
}

macro_rules! opt_fb {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $opt:ident, $($f:ident).+) => {
        Attr {
            id: $id, group: $g, label: $label, doc: $doc,
            kind: AttrKind::Flag,
            get: |d| d.$opt.as_ref().map(|s| AttrValue::Int(s.$($f).+ as i64)),
            set: |d, v| { if let Some(s) = d.$opt.as_mut() { s.$($f).+ = v.as_bool(); } },
        }
    };
}

// The combiner comes in three binary layouts, picked by the file's vfx version — Smash's own
// files are version 22, which is the `Legacy` one. The eight blend bytes exist in all three, so
// `cmb`/`cmb_fg` read whichever the emitter has; the rest exist in Legacy and V40 but NOT in the
// version-36 layout, so `cmb_wide`/`cmb_wide_fg` report those as absent there rather than
// guessing at a field that is not in the file.
macro_rules! cmb {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $f:ident) => {
        Attr {
            id: $id,
            group: $g,
            label: $label,
            doc: $doc,
            kind: AttrKind::Int,
            get: |d| {
                use effect_library::EmitterCombinerVariant as V;
                match d.combiner.as_ref()? {
                    V::Legacy(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V36(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V40(c) => Some(AttrValue::Int(c.$f as i64)),
                }
            },
            set: |d, v| {
                use effect_library::EmitterCombinerVariant as V;
                let value = v.as_i64().clamp(0, 255) as u8;
                match d.combiner.as_mut() {
                    Some(V::Legacy(c)) => c.$f = value,
                    Some(V::V36(c)) => c.$f = value,
                    Some(V::V40(c)) => c.$f = value,
                    None => {}
                }
            },
        }
    };
}

macro_rules! cmb_fg {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $f:ident) => {
        Attr {
            id: $id,
            group: $g,
            label: $label,
            doc: $doc,
            kind: AttrKind::Flag,
            get: |d| {
                use effect_library::EmitterCombinerVariant as V;
                match d.combiner.as_ref()? {
                    V::Legacy(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V36(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V40(c) => Some(AttrValue::Int(c.$f as i64)),
                }
            },
            set: |d, v| {
                use effect_library::EmitterCombinerVariant as V;
                let value = u8::from(v.as_bool());
                match d.combiner.as_mut() {
                    Some(V::Legacy(c)) => c.$f = value,
                    Some(V::V36(c)) => c.$f = value,
                    Some(V::V40(c)) => c.$f = value,
                    None => {}
                }
            },
        }
    };
}

macro_rules! cmb_wide {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $f:ident) => {
        Attr {
            id: $id,
            group: $g,
            label: $label,
            doc: $doc,
            kind: AttrKind::Int,
            get: |d| {
                use effect_library::EmitterCombinerVariant as V;
                match d.combiner.as_ref()? {
                    V::Legacy(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V40(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V36(_) => None,
                }
            },
            set: |d, v| {
                use effect_library::EmitterCombinerVariant as V;
                let value = v.as_i64().clamp(0, 255) as u8;
                match d.combiner.as_mut() {
                    Some(V::Legacy(c)) => c.$f = value,
                    Some(V::V40(c)) => c.$f = value,
                    _ => {}
                }
            },
        }
    };
}

macro_rules! cmb_wide_fg {
    ($g:expr, $id:expr, $label:expr, $doc:expr, $f:ident) => {
        Attr {
            id: $id,
            group: $g,
            label: $label,
            doc: $doc,
            kind: AttrKind::Flag,
            get: |d| {
                use effect_library::EmitterCombinerVariant as V;
                match d.combiner.as_ref()? {
                    V::Legacy(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V40(c) => Some(AttrValue::Int(c.$f as i64)),
                    V::V36(_) => None,
                }
            },
            set: |d, v| {
                use effect_library::EmitterCombinerVariant as V;
                let value = u8::from(v.as_bool());
                match d.combiner.as_mut() {
                    Some(V::Legacy(c)) => c.$f = value,
                    Some(V::V40(c)) => c.$f = value,
                    _ => {}
                }
            },
        }
    };
}

pub const WRAP_MODES: &[&str] = &["Mirror", "Repeat", "ClampEdge", "MirrorOnce"];
pub const COLOR_TYPES: &[&str] = &["Constant", "Random", "Animated 8-key"];

fn build() -> Vec<Attr> {
    let mut v = Vec::with_capacity(320);
    v.extend(common());
    v.extend(emission());
    v.extend(shape());
    v.extend(emitter_info());
    v.extend(particle());
    v.extend(velocity_color_scale());
    v.extend(rotation_and_motion());
    v.extend(render_and_combiner());
    v.extend(inheritance_and_shader());
    v.extend(samplers());
    v.extend(texture_anims());
    v.extend(array_attributes());
    v
}

fn common() -> Vec<Attr> {
    vec![
        uit!(G_COMMON, "flag", "Flag", "Emitter flag word", flag),
        uit!(
            G_COMMON,
            "random_seed",
            "Random seed",
            "Random seed for the emitter",
            random_seed
        ),
        uit!(
            G_COMMON,
            "emitter_static.flags1",
            "Static flags 1",
            "Emitter static flag word 1",
            emitter_static.flags1
        ),
        uit!(
            G_COMMON,
            "emitter_static.flags2",
            "Static flags 2",
            "Emitter static flag word 2",
            emitter_static.flags2
        ),
        uit!(
            G_COMMON,
            "emitter_static.flags3",
            "Static flags 3",
            "Emitter static flag word 3",
            emitter_static.flags3
        ),
        uit!(
            G_COMMON,
            "emitter_static.flags4",
            "Static flags 4",
            "Emitter static flag word 4",
            emitter_static.flags4
        ),
    ]
}

fn emission() -> Vec<Attr> {
    vec![
        fb!(
            G_EMISSION,
            "emission.is_one_time",
            "One time",
            "Whether to only play once",
            emission.is_one_time
        ),
        fb!(
            G_EMISSION,
            "emission.is_world_gravity",
            "World gravity",
            "Whether to apply gravity in world coordinates",
            emission.is_world_gravity
        ),
        fb!(
            G_EMISSION,
            "emission.is_emit_dist_enabled",
            "Distance emission",
            "Whether to use distance emission",
            emission.is_emit_dist_enabled
        ),
        fb!(
            G_EMISSION,
            "emission.is_world_oriented_velocity",
            "World oriented velocity",
            "Whether to apply the specified oriented initial velocity in world coordinates",
            emission.is_world_oriented_velocity
        ),
        it!(
            G_EMISSION,
            "emission.start",
            "Start",
            "Emission start frame",
            emission.start
        ),
        it!(
            G_EMISSION,
            "emission.timing",
            "Timing",
            "Emission start timing",
            emission.timing
        ),
        it!(
            G_EMISSION,
            "emission.duration",
            "Duration",
            "Emission time",
            emission.duration
        ),
        fl!(
            G_EMISSION,
            "emission.rate",
            "Rate",
            "Emission rate",
            0.05,
            emission.rate
        ),
        fl!(
            G_EMISSION,
            "emission.rate_random",
            "Rate random",
            "Discharge rate random",
            0.05,
            emission.rate_random
        ),
        it!(
            G_EMISSION,
            "emission.interval",
            "Interval",
            "Emission interval",
            emission.interval
        ),
        fl!(
            G_EMISSION,
            "emission.interval_random",
            "Interval random",
            "Emission interval random",
            0.05,
            emission.interval_random
        ),
        fl!(
            G_EMISSION,
            "emission.position_random",
            "Position random",
            "Initial position randomness",
            0.01,
            emission.position_random
        ),
        fl!(
            G_EMISSION,
            "emission.gravity_scale",
            "Gravity scale",
            "Gravity scale",
            0.01,
            emission.gravity_scale
        ),
        fl!(
            G_EMISSION,
            "emission.gravity_dir_x",
            "Gravity dir X",
            "Gravity direction X",
            0.01,
            emission.gravity_dir_x
        ),
        fl!(
            G_EMISSION,
            "emission.gravity_dir_y",
            "Gravity dir Y",
            "Gravity direction Y",
            0.01,
            emission.gravity_dir_y
        ),
        fl!(
            G_EMISSION,
            "emission.gravity_dir_z",
            "Gravity dir Z",
            "Gravity direction Z",
            0.01,
            emission.gravity_dir_z
        ),
        fl!(
            G_EMISSION,
            "emission.emitter_dist_unit",
            "Distance unit",
            "Emission interval (distance)",
            0.01,
            emission.emitter_dist_unit
        ),
        fl!(
            G_EMISSION,
            "emission.emitter_dist_min",
            "Distance min",
            "Minimum translation distance allowed per frame",
            0.01,
            emission.emitter_dist_min
        ),
        fl!(
            G_EMISSION,
            "emission.emitter_dist_max",
            "Distance max",
            "Maximum translation distance allowed per frame",
            0.01,
            emission.emitter_dist_max
        ),
        fl!(
            G_EMISSION,
            "emission.emitter_dist_marg",
            "Distance margin",
            "Threshold for traverse distance truncation",
            0.01,
            emission.emitter_dist_marg
        ),
        it!(
            G_EMISSION,
            "emission.emitter_dist_particles_max",
            "Distance particles max",
            "Maximum particle emissions when using distance emission",
            emission.emitter_dist_particles_max
        ),
    ]
}

fn shape() -> Vec<Attr> {
    vec![
        it!(
            G_SHAPE,
            "shape_info.volume_type",
            "Volume type",
            "Emitter volume type",
            shape_info.volume_type
        ),
        it!(
            G_SHAPE,
            "shape_info.sweep_start_random",
            "Sweep start random",
            "Arc width randomness",
            shape_info.sweep_start_random
        ),
        it!(
            G_SHAPE,
            "shape_info.arc_type",
            "Arc type",
            "Arc type",
            shape_info.arc_type
        ),
        fg!(
            G_SHAPE,
            "shape_info.is_volume_latitude_enabled",
            "Volume latitude",
            "Unused",
            shape_info.is_volume_latitude_enabled
        ),
        it!(
            G_SHAPE,
            "shape_info.volume_tbl_index",
            "Volume table index",
            "Sphere volume table index",
            shape_info.volume_tbl_index
        ),
        it!(
            G_SHAPE,
            "shape_info.volume_tbl_index64",
            "Volume table index 64",
            "Sphere 64 volume table index",
            shape_info.volume_tbl_index64
        ),
        it!(
            G_SHAPE,
            "shape_info.volume_latitude_dir",
            "Latitude direction",
            "Sphere latitude direction",
            shape_info.volume_latitude_dir
        ),
        fg!(
            G_SHAPE,
            "shape_info.is_gpu_emitter",
            "GPU emitter",
            "Whether to enable the GPU emitter",
            shape_info.is_gpu_emitter
        ),
        fl!(
            G_SHAPE,
            "shape_info.sweep_longitude",
            "Sweep longitude",
            "Value to use for calculating arc",
            0.01,
            shape_info.sweep_longitude
        ),
        fl!(
            G_SHAPE,
            "shape_info.sweep_latitude",
            "Sweep latitude",
            "Latitude to use for calculating arc",
            0.01,
            shape_info.sweep_latitude
        ),
        fl!(
            G_SHAPE,
            "shape_info.sweep_start",
            "Sweep start",
            "Arc width (start)",
            0.01,
            shape_info.sweep_start
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_surface_pos_rand",
            "Surface position random",
            "Random position on emitter shape surface",
            0.01,
            shape_info.volume_surface_pos_rand
        ),
        fl!(
            G_SHAPE,
            "shape_info.caliber_ratio",
            "Caliber ratio",
            "Caliber ratio",
            0.01,
            shape_info.caliber_ratio
        ),
        fl!(
            G_SHAPE,
            "shape_info.line_center",
            "Line center",
            "Line center",
            0.01,
            shape_info.line_center
        ),
        fl!(
            G_SHAPE,
            "shape_info.line_length",
            "Line length",
            "Line length",
            0.01,
            shape_info.line_length
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_radius_x",
            "Volume radius X",
            "Volume radius",
            0.01,
            shape_info.volume_radius_x
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_radius_y",
            "Volume radius Y",
            "Volume radius",
            0.01,
            shape_info.volume_radius_y
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_radius_z",
            "Volume radius Z",
            "Volume radius",
            0.01,
            shape_info.volume_radius_z
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_form_scale_x",
            "Form scale X",
            "Emitter scale",
            0.01,
            shape_info.volume_form_scale_x
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_form_scale_y",
            "Form scale Y",
            "Emitter scale",
            0.01,
            shape_info.volume_form_scale_y
        ),
        fl!(
            G_SHAPE,
            "shape_info.volume_form_scale_z",
            "Form scale Z",
            "Emitter scale",
            0.01,
            shape_info.volume_form_scale_z
        ),
        it!(
            G_SHAPE,
            "shape_info.prim_emit_type",
            "Primitive emit type",
            "Emitter type when a primitive was specified",
            shape_info.prim_emit_type
        ),
        uit!(
            G_SHAPE,
            "shape_info.primitive_index",
            "Primitive index",
            "Primitive index — only meaningful when this eff holds that primitive",
            shape_info.primitive_index
        ),
        it!(
            G_SHAPE,
            "shape_info.num_divide_circle",
            "Divide circle",
            "Number of equilateral circular segments",
            shape_info.num_divide_circle
        ),
        it!(
            G_SHAPE,
            "shape_info.num_divide_circle_random",
            "Divide circle random",
            "Random number of equilateral circular segments",
            shape_info.num_divide_circle_random
        ),
        it!(
            G_SHAPE,
            "shape_info.num_divide_line",
            "Divide line",
            "Number of equal length line segment divisions",
            shape_info.num_divide_line
        ),
        it!(
            G_SHAPE,
            "shape_info.num_divide_line_random",
            "Divide line random",
            "Random number of equal length line segment divisions",
            shape_info.num_divide_line_random
        ),
        Attr {
            id: "shape_info.is_on_another_binary_volume_primitive",
            group: G_SHAPE,
            label: "External volume primitive",
            doc: "Whether the emitter shape primitive is stored in another binary",
            kind: AttrKind::Flag,
            get: |d| {
                d.shape_info
                    .is_on_another_binary_volume_primitive
                    .map(|v| AttrValue::Int(v as i64))
            },
            set: |d, v| {
                if let Some(slot) = d.shape_info.is_on_another_binary_volume_primitive.as_mut() {
                    *slot = u8::from(v.as_bool());
                }
            },
        },
    ]
}

fn emitter_info() -> Vec<Attr> {
    vec![
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_particle_draw",
            "Draw particles",
            "Draw particles",
            emitter_info.is_particle_draw
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.sort_type",
            "Sort type",
            "Particle sort type",
            emitter_info.sort_type
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.calc_type",
            "Calc type",
            "Behavior calculation type",
            emitter_info.calc_type
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.follow_type",
            "Follow type",
            "Emitter follow type",
            emitter_info.follow_type
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_fade_emit",
            "Fade emit",
            "Whether to stop emitting in the finalization process",
            emitter_info.is_fade_emit
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_fade_alpha_fade",
            "Fade alpha",
            "Whether to apply alpha fade in the finalization process",
            emitter_info.is_fade_alpha_fade
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_scale_fade",
            "Scale fade",
            "Whether to enable scale-fade",
            emitter_info.is_scale_fade
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.random_seed_type",
            "Random seed type",
            "Random seed type",
            emitter_info.random_seed_type
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_update_matrix_by_emit",
            "Update matrix by emit",
            "Updates the matrix at each emission",
            emitter_info.is_update_matrix_by_emit
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.test_always",
            "Test always",
            "Whether to always test",
            emitter_info.test_always
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.interpolate_emission_amount",
            "Interpolate emission",
            "Whether to interpolate the emission amount",
            emitter_info.interpolate_emission_amount
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_alpha_fade_in",
            "Alpha fade in",
            "Whether to apply alpha fade-in",
            emitter_info.is_alpha_fade_in
        ),
        fg!(
            G_INFO_FLAGS,
            "emitter_info.is_scale_fade_in",
            "Scale fade in",
            "Whether to enable scale fade-in",
            emitter_info.is_scale_fade_in
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.random_seed",
            "Random seed",
            "Random number seed",
            emitter_info.random_seed
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.draw_path",
            "Draw path",
            "Rendering pass",
            emitter_info.draw_path
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.alpha_fade_time",
            "Alpha fade time",
            "Alpha fadeout duration",
            emitter_info.alpha_fade_time
        ),
        it!(
            G_INFO_FLAGS,
            "emitter_info.fade_in_time",
            "Fade in time",
            "Fade-in duration",
            emitter_info.fade_in_time
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_x",
            "Position X",
            "Emitter position X",
            0.01,
            emitter_info.trans_x
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_y",
            "Position Y",
            "Emitter position Y",
            0.01,
            emitter_info.trans_y
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_z",
            "Position Z",
            "Emitter position Z",
            0.01,
            emitter_info.trans_z
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_rand_x",
            "Position random X",
            "Matrix translation X randomness",
            0.01,
            emitter_info.trans_rand_x
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_rand_y",
            "Position random Y",
            "Matrix translation Y randomness",
            0.01,
            emitter_info.trans_rand_y
        ),
        fl!(
            G_INFO,
            "emitter_info.trans_rand_z",
            "Position random Z",
            "Matrix translation Z randomness",
            0.01,
            emitter_info.trans_rand_z
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_x",
            "Rotation X",
            "Emitter rotation X",
            0.01,
            emitter_info.rotate_x
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_y",
            "Rotation Y",
            "Emitter rotation Y",
            0.01,
            emitter_info.rotate_y
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_z",
            "Rotation Z",
            "Emitter rotation Z",
            0.01,
            emitter_info.rotate_z
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_rand_x",
            "Rotation random X",
            "Matrix rotation X randomness",
            0.01,
            emitter_info.rotate_rand_x
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_rand_y",
            "Rotation random Y",
            "Matrix rotation Y randomness",
            0.01,
            emitter_info.rotate_rand_y
        ),
        fl!(
            G_INFO,
            "emitter_info.rotate_rand_z",
            "Rotation random Z",
            "Matrix rotation Z randomness",
            0.01,
            emitter_info.rotate_rand_z
        ),
        fl!(
            G_INFO,
            "emitter_info.scale_x",
            "Scale X",
            "Emitter scale X",
            0.01,
            emitter_info.scale_x
        ),
        fl!(
            G_INFO,
            "emitter_info.scale_y",
            "Scale Y",
            "Emitter scale Y",
            0.01,
            emitter_info.scale_y
        ),
        fl!(
            G_INFO,
            "emitter_info.scale_z",
            "Scale Z",
            "Emitter scale Z",
            0.01,
            emitter_info.scale_z
        ),
        fl!(
            G_INFO,
            "emitter_info.color0_r",
            "Colour 0 R",
            "Emitter color 0",
            0.01,
            emitter_info.color0_r
        ),
        fl!(
            G_INFO,
            "emitter_info.color0_g",
            "Colour 0 G",
            "Emitter color 0",
            0.01,
            emitter_info.color0_g
        ),
        fl!(
            G_INFO,
            "emitter_info.color0_b",
            "Colour 0 B",
            "Emitter color 0",
            0.01,
            emitter_info.color0_b
        ),
        fl!(
            G_INFO,
            "emitter_info.color0_a",
            "Colour 0 A",
            "Emitter color 0",
            0.01,
            emitter_info.color0_a
        ),
        fl!(
            G_INFO,
            "emitter_info.color1_r",
            "Colour 1 R",
            "Emitter color 1",
            0.01,
            emitter_info.color1_r
        ),
        fl!(
            G_INFO,
            "emitter_info.color1_g",
            "Colour 1 G",
            "Emitter color 1",
            0.01,
            emitter_info.color1_g
        ),
        fl!(
            G_INFO,
            "emitter_info.color1_b",
            "Colour 1 B",
            "Emitter color 1",
            0.01,
            emitter_info.color1_b
        ),
        fl!(
            G_INFO,
            "emitter_info.color1_a",
            "Colour 1 A",
            "Emitter color 1",
            0.01,
            emitter_info.color1_a
        ),
        fl!(
            G_INFO,
            "emitter_info.emission_range_near",
            "Emission range near",
            "Emission range near distance",
            0.5,
            emitter_info.emission_range_near
        ),
        fl!(
            G_INFO,
            "emitter_info.emission_range_far",
            "Emission range far",
            "Emission range far distance",
            0.5,
            emitter_info.emission_range_far
        ),
        fl!(
            G_INFO,
            "emitter_info.emission_ratio_far",
            "Emission ratio far",
            "Emission ratio at far distance",
            0.5,
            emitter_info.emission_ratio_far
        ),
    ]
}

fn particle() -> Vec<Attr> {
    vec![
        fb!(
            G_PARTICLE,
            "particle_data.infinite_life",
            "Infinite life",
            "Infinite lifespan",
            particle_data.infinite_life
        ),
        fb!(
            G_PARTICLE,
            "particle_data.is_triming",
            "Trimming",
            "Trimming",
            particle_data.is_triming
        ),
        it!(
            G_PARTICLE,
            "particle_data.billboard_type",
            "Billboard type",
            "Billboard type",
            particle_data.billboard_type
        ),
        it!(
            G_PARTICLE,
            "particle_data.rot_type",
            "Rotation type",
            "Rotation type",
            particle_data.rot_type
        ),
        it!(
            G_PARTICLE,
            "particle_data.offset_type",
            "Offset type",
            "Camera depth offset type",
            particle_data.offset_type
        ),
        fb!(
            G_PARTICLE,
            "particle_data.rot_rev_rand_x",
            "Rotation reverse random X",
            "Random X for rotation direction",
            particle_data.rot_rev_rand_x
        ),
        fb!(
            G_PARTICLE,
            "particle_data.rot_rev_rand_y",
            "Rotation reverse random Y",
            "Random Y for rotation direction",
            particle_data.rot_rev_rand_y
        ),
        fb!(
            G_PARTICLE,
            "particle_data.rot_rev_rand_z",
            "Rotation reverse random Z",
            "Random Z for rotation direction",
            particle_data.rot_rev_rand_z
        ),
        fb!(
            G_PARTICLE,
            "particle_data.is_rotate_x",
            "Rotate X",
            "Whether to use rotate X",
            particle_data.is_rotate_x
        ),
        fb!(
            G_PARTICLE,
            "particle_data.is_rotate_y",
            "Rotate Y",
            "Whether to use rotate Y",
            particle_data.is_rotate_y
        ),
        fg!(
            G_PARTICLE,
            "particle_data.is_rotate_z",
            "Rotate Z",
            "Whether to use rotate Z",
            particle_data.is_rotate_z
        ),
        it!(
            G_PARTICLE,
            "particle_data.primitive_scale_type",
            "Primitive scale type",
            "Primitive scale application type",
            particle_data.primitive_scale_type
        ),
        it!(
            G_PARTICLE,
            "particle_data.is_texture_common_random",
            "Common texture random",
            "Common texture randomization",
            particle_data.is_texture_common_random
        ),
        it!(
            G_PARTICLE,
            "particle_data.connect_ptcl_scale_and_z_offset",
            "Link scale to Z offset",
            "Relate particle scale and Z offset",
            particle_data.connect_ptcl_scale_and_z_offset
        ),
        fg!(
            G_PARTICLE,
            "particle_data.enable_avoid_z_fighting",
            "Avoid Z-fighting",
            "Whether to enter an offset to avoid z-fighting",
            particle_data.enable_avoid_z_fighting
        ),
        it!(
            G_PARTICLE,
            "particle_data.life",
            "Life",
            "Lifetime, in frames",
            particle_data.life
        ),
        it!(
            G_PARTICLE,
            "particle_data.life_random",
            "Life random",
            "Life randomness",
            particle_data.life_random
        ),
        fl!(
            G_PARTICLE,
            "particle_data.momentum_random",
            "Momentum random",
            "Momentum random",
            0.01,
            particle_data.momentum_random
        ),
        it!(
            G_PARTICLE,
            "particle_data.primitive_vertex_info_flags",
            "Primitive vertex flags",
            "Bit flag for the data held by the primitive's vertices",
            particle_data.primitive_vertex_info_flags
        ),
        uit!(
            G_PARTICLE,
            "particle_data.primitive_id",
            "Primitive ID",
            "Index of the primitive to use — only meaningful when this eff holds it",
            particle_data.primitive_id
        ),
        uit!(
            G_PARTICLE,
            "particle_data.primitive_ex_id",
            "Trim primitive ID",
            "Index of the trimming primitive to use",
            particle_data.primitive_ex_id
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_color0",
            "Loop colour 0",
            "Color 0 animation loop",
            particle_data.loop_color0
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_alpha0",
            "Loop alpha 0",
            "Alpha animation loop",
            particle_data.loop_alpha0
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_color1",
            "Loop colour 1",
            "Color 1 animation loop",
            particle_data.loop_color1
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_alpha1",
            "Loop alpha 1",
            "Alpha 1 animation loop",
            particle_data.loop_alpha1
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.scale_loop",
            "Loop scale",
            "Scale animation loop",
            particle_data.scale_loop
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_random_color0",
            "Loop random colour 0",
            "Initial position randomness of the colour 0 animation",
            particle_data.loop_random_color0
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_random_alpha0",
            "Loop random alpha 0",
            "Initial position randomness of the alpha 0 animation",
            particle_data.loop_random_alpha0
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_random_color1",
            "Loop random colour 1",
            "Initial position randomness of the colour 1 animation",
            particle_data.loop_random_color1
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.loop_random_alpha1",
            "Loop random alpha 1",
            "Initial position randomness of the alpha 1 animation",
            particle_data.loop_random_alpha1
        ),
        fb!(
            G_ANIM_RATES,
            "particle_data.scale_loop_random",
            "Loop random scale",
            "Initial position randomness of the scale animation",
            particle_data.scale_loop_random
        ),
        fg!(
            G_PARTICLE,
            "particle_data.prim_flag1",
            "External primitive",
            "Whether the primitive is stored in another binary",
            particle_data.prim_flag1
        ),
        fg!(
            G_PARTICLE,
            "particle_data.prim_flag2",
            "External trim primitive",
            "Whether the trimming primitive is stored in another binary",
            particle_data.prim_flag2
        ),
        it!(
            G_ANIM_RATES,
            "particle_data.color0_loop_rate",
            "Colour 0 loop rate",
            "Colour 0 loop frame rate, as a percent of one cycle's lifespan",
            particle_data.color0_loop_rate
        ),
        it!(
            G_ANIM_RATES,
            "particle_data.alpha0_loop_rate",
            "Alpha 0 loop rate",
            "Alpha 0 loop frame rate, as a percent of one cycle's lifespan",
            particle_data.alpha0_loop_rate
        ),
        it!(
            G_ANIM_RATES,
            "particle_data.color1_loop_rate",
            "Colour 1 loop rate",
            "Colour 1 loop frame rate, as a percent of one cycle's lifespan",
            particle_data.color1_loop_rate
        ),
        it!(
            G_ANIM_RATES,
            "particle_data.alpha1_loop_rate",
            "Alpha 1 loop rate",
            "Alpha 1 loop frame rate, as a percent of one cycle's lifespan",
            particle_data.alpha1_loop_rate
        ),
        it!(
            G_ANIM_RATES,
            "particle_data.scale_loop_rate",
            "Scale loop rate",
            "Scale loop frame rate, as a percent of one cycle's lifespan",
            particle_data.scale_loop_rate
        ),
    ]
}

fn velocity_color_scale() -> Vec<Attr> {
    vec![
        fl!(G_PTCL_VELOCITY, "particle_velocity.all_direction", "All direction", "All-direction initial velocity", 0.01, particle_velocity.all_direction),
        fl!(G_PTCL_VELOCITY, "particle_velocity.designated_dir_scale", "Designated dir scale", "Specified direction velocity", 0.01, particle_velocity.designated_dir_scale),
        fl!(G_PTCL_VELOCITY, "particle_velocity.designated_dir_x", "Designated dir X", "Specified direction X", 0.01, particle_velocity.designated_dir_x),
        fl!(G_PTCL_VELOCITY, "particle_velocity.designated_dir_y", "Designated dir Y", "Specified direction Y", 0.01, particle_velocity.designated_dir_y),
        fl!(G_PTCL_VELOCITY, "particle_velocity.designated_dir_z", "Designated dir Z", "Specified direction Z", 0.01, particle_velocity.designated_dir_z),
        fl!(G_PTCL_VELOCITY, "particle_velocity.diffusion_dir_angle", "Diffusion angle", "Specified direction dispersion angle", 0.5, particle_velocity.diffusion_dir_angle),
        fl!(G_PTCL_VELOCITY, "particle_velocity.xz_diffusion", "XZ diffusion", "Y axis diffusion speed", 0.01, particle_velocity.xz_diffusion),
        fl!(G_PTCL_VELOCITY, "particle_velocity.diffusion_x", "Diffusion X", "Diffusion initial velocity X", 0.01, particle_velocity.diffusion_x),
        fl!(G_PTCL_VELOCITY, "particle_velocity.diffusion_y", "Diffusion Y", "Diffusion initial velocity Y", 0.01, particle_velocity.diffusion_y),
        fl!(G_PTCL_VELOCITY, "particle_velocity.diffusion_z", "Diffusion Z", "Diffusion initial velocity Z", 0.01, particle_velocity.diffusion_z),
        fl!(G_PTCL_VELOCITY, "particle_velocity.vel_random", "Velocity random", "Velocity randomness", 0.01, particle_velocity.vel_random),
        fl!(G_PTCL_VELOCITY, "particle_velocity.em_vel_inherit", "Emitter velocity inherit", "Inherited emitter velocity ratio", 0.01, particle_velocity.em_vel_inherit),
        fg!(G_PTCL_COLOR, "particle_color.is_soft_particle", "Soft particle", "Soft particles", particle_color.is_soft_particle),
        fg!(G_PTCL_COLOR, "particle_color.is_fresnel_alpha", "Fresnel alpha", "Fresnel alpha", particle_color.is_fresnel_alpha),
        fg!(G_PTCL_COLOR, "particle_color.is_near_dist_alpha", "Near distance alpha", "Near distance alpha", particle_color.is_near_dist_alpha),
        fg!(G_PTCL_COLOR, "particle_color.is_far_dist_alpha", "Far distance alpha", "Far distance alpha", particle_color.is_far_dist_alpha),
        fg!(G_PTCL_COLOR, "particle_color.is_decal", "Decal", "Decals", particle_color.is_decal),
        Attr {
            id: "particle_color.color0_type", group: G_PTCL_COLOR, label: "Colour 0 type",
            doc: "Colour 0 behaviour type. This decides WHERE the colour is read from, so changing it \
                  changes which of the colour controls has any effect.",
            kind: AttrKind::Enum(COLOR_TYPES),
            get: |d| Some(AttrValue::Int(d.particle_color.color0_type.as_u8() as i64)),
            set: |d, v| d.particle_color.color0_type = ColorType::from_u8(v.as_i64().clamp(0, 255) as u8),
        },
        Attr {
            id: "particle_color.color1_type", group: G_PTCL_COLOR, label: "Colour 1 type",
            doc: "Colour 1 behaviour type",
            kind: AttrKind::Enum(COLOR_TYPES),
            get: |d| Some(AttrValue::Int(d.particle_color.color1_type.as_u8() as i64)),
            set: |d, v| d.particle_color.color1_type = ColorType::from_u8(v.as_i64().clamp(0, 255) as u8),
        },
        Attr {
            id: "particle_color.alpha0_type", group: G_PTCL_COLOR, label: "Alpha 0 type",
            doc: "Alpha 0 behaviour type",
            kind: AttrKind::Enum(COLOR_TYPES),
            get: |d| Some(AttrValue::Int(d.particle_color.alpha0_type.as_u8() as i64)),
            set: |d, v| d.particle_color.alpha0_type = ColorType::from_u8(v.as_i64().clamp(0, 255) as u8),
        },
        Attr {
            id: "particle_color.alpha1_type", group: G_PTCL_COLOR, label: "Alpha 1 type",
            doc: "Alpha 1 behaviour type",
            kind: AttrKind::Enum(COLOR_TYPES),
            get: |d| Some(AttrValue::Int(d.particle_color.alpha1_type.as_u8() as i64)),
            set: |d, v| d.particle_color.alpha1_type = ColorType::from_u8(v.as_i64().clamp(0, 255) as u8),
        },
        fl!(G_PTCL_COLOR, "particle_color.color0_r", "Colour 0 R", "Colour 0 red component", 0.01, particle_color.color0_r),
        fl!(G_PTCL_COLOR, "particle_color.color0_g", "Colour 0 G", "Colour 0 green component", 0.01, particle_color.color0_g),
        fl!(G_PTCL_COLOR, "particle_color.color0_b", "Colour 0 B", "Colour 0 blue component", 0.01, particle_color.color0_b),
        fl!(G_PTCL_COLOR, "particle_color.alpha0", "Alpha 0", "Alpha 0", 0.01, particle_color.alpha0),
        fl!(G_PTCL_COLOR, "particle_color.color1_r", "Colour 1 R", "Colour 1 red component", 0.01, particle_color.color1_r),
        fl!(G_PTCL_COLOR, "particle_color.color1_g", "Colour 1 G", "Colour 1 green component", 0.01, particle_color.color1_g),
        fl!(G_PTCL_COLOR, "particle_color.color1_b", "Colour 1 B", "Colour 1 blue component", 0.01, particle_color.color1_b),
        fl!(G_PTCL_COLOR, "particle_color.alpha1", "Alpha 1", "Alpha 1", 0.01, particle_color.alpha1),
        fl!(G_PTCL_SCALE, "particle_scale.scale_x", "Scale X", "Base scale", 0.01, particle_scale.scale_x),
        fl!(G_PTCL_SCALE, "particle_scale.scale_y", "Scale Y", "Base scale", 0.01, particle_scale.scale_y),
        fl!(G_PTCL_SCALE, "particle_scale.scale_z", "Scale Z", "Base scale", 0.01, particle_scale.scale_z),
        fl!(G_PTCL_SCALE, "particle_scale.scale_random_x", "Scale random X", "Base scale randomness", 0.01, particle_scale.scale_random_x),
        fl!(G_PTCL_SCALE, "particle_scale.scale_random_y", "Scale random Y", "Base scale randomness", 0.01, particle_scale.scale_random_y),
        fl!(G_PTCL_SCALE, "particle_scale.scale_random_z", "Scale random Z", "Base scale randomness", 0.01, particle_scale.scale_random_z),
        fg!(G_PTCL_SCALE, "particle_scale.enable_scaling_by_camera_dist_near", "Scale by camera (near)", "Whether to enable near camera distance scaling", particle_scale.enable_scaling_by_camera_dist_near),
        fg!(G_PTCL_SCALE, "particle_scale.enable_scaling_by_camera_dist_far", "Scale by camera (far)", "Whether to enable far camera distance scaling", particle_scale.enable_scaling_by_camera_dist_far),
        fg!(G_PTCL_SCALE, "particle_scale.enable_add_scale_y", "Add scale Y", "Y velocity scaling", particle_scale.enable_add_scale_y),
        fg!(G_PTCL_SCALE, "particle_scale.enable_link_fovy_to_scale_value", "Link FOV to scale", "Relate angle of view to scale restrictions", particle_scale.enable_link_fovy_to_scale_value),
        fl!(G_PTCL_SCALE, "particle_scale.scale_min", "Scale limit near", "Particle scaling limit distance (near)", 0.01, particle_scale.scale_min),
        fl!(G_PTCL_SCALE, "particle_scale.scale_max", "Scale limit far", "Particle scaling limit distance (far)", 0.01, particle_scale.scale_max),
    ]
}

fn rotation_and_motion() -> Vec<Attr> {
    vec![
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_x",
            "Initial rotation X",
            "Initial rotation value X",
            0.01,
            emitter_static.rotate_init_x
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_y",
            "Initial rotation Y",
            "Initial rotation value Y",
            0.01,
            emitter_static.rotate_init_y
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_z",
            "Initial rotation Z",
            "Initial rotation value Z",
            0.01,
            emitter_static.rotate_init_z
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_rand_x",
            "Initial rotation random X",
            "Initial random rotation X",
            0.01,
            emitter_static.rotate_init_rand_x
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_rand_y",
            "Initial rotation random Y",
            "Initial random rotation Y",
            0.01,
            emitter_static.rotate_init_rand_y
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_init_rand_z",
            "Initial rotation random Z",
            "Initial random rotation Z",
            0.01,
            emitter_static.rotate_init_rand_z
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_x",
            "Rotation velocity X",
            "Rotation velocity X",
            0.01,
            emitter_static.rotate_add_x
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_y",
            "Rotation velocity Y",
            "Rotation velocity Y",
            0.01,
            emitter_static.rotate_add_y
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_z",
            "Rotation velocity Z",
            "Rotation velocity Z",
            0.01,
            emitter_static.rotate_add_z
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_regist",
            "Rotation attenuation",
            "Rotation attenuation rate",
            0.01,
            emitter_static.rotate_regist
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_rand_x",
            "Rotation velocity random X",
            "Rotation velocity randomness X",
            0.01,
            emitter_static.rotate_add_rand_x
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_rand_y",
            "Rotation velocity random Y",
            "Rotation velocity randomness Y",
            0.01,
            emitter_static.rotate_add_rand_y
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.rotate_add_rand_z",
            "Rotation velocity random Z",
            "Rotation velocity randomness Z",
            0.01,
            emitter_static.rotate_add_rand_z
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.scale_limit_dist_near",
            "Scale limit distance near",
            "Scale limit distance in front of the camera (near)",
            0.5,
            emitter_static.scale_limit_dist_near
        ),
        fl!(
            G_PTCL_ROTATE,
            "emitter_static.scale_limit_dist_far",
            "Scale limit distance far",
            "Scale limit distance in front of the camera (far)",
            0.5,
            emitter_static.scale_limit_dist_far
        ),
        fl!(
            G_MOTION,
            "emitter_static.gravity_dir_x",
            "Gravity X",
            "Gravity X",
            0.01,
            emitter_static.gravity_dir_x
        ),
        fl!(
            G_MOTION,
            "emitter_static.gravity_dir_y",
            "Gravity Y",
            "Gravity Y",
            0.01,
            emitter_static.gravity_dir_y
        ),
        fl!(
            G_MOTION,
            "emitter_static.gravity_dir_z",
            "Gravity Z",
            "Gravity Z",
            0.01,
            emitter_static.gravity_dir_z
        ),
        fl!(
            G_MOTION,
            "emitter_static.gravity_scale",
            "Gravity scale",
            "Gravity scale",
            0.01,
            emitter_static.gravity_scale
        ),
        fl!(
            G_MOTION,
            "emitter_static.air_res",
            "Air resistance",
            "Air resistance",
            0.01,
            emitter_static.air_res
        ),
        fl!(
            G_MOTION,
            "emitter_static.center_x",
            "Centre X",
            "Particle centre",
            0.01,
            emitter_static.center_x
        ),
        fl!(
            G_MOTION,
            "emitter_static.center_y",
            "Centre Y",
            "Particle centre",
            0.01,
            emitter_static.center_y
        ),
        fl!(
            G_MOTION,
            "emitter_static.offset",
            "Offset",
            "Particle offset",
            0.01,
            emitter_static.offset
        ),
        fl!(
            G_MOTION,
            "emitter_static.add_vel_to_scale",
            "Add velocity to scale",
            "Add velocity to scale",
            0.01,
            emitter_static.add_vel_to_scale
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.amplitude_x",
            "Amplitude X",
            "Fluctuation amplitude X",
            0.01,
            emitter_static.amplitude_x
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.amplitude_y",
            "Amplitude Y",
            "Fluctuation amplitude Y",
            0.01,
            emitter_static.amplitude_y
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.cycle_x",
            "Cycle X",
            "Fluctuation cycle X",
            0.01,
            emitter_static.cycle_x
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.cycle_y",
            "Cycle Y",
            "Fluctuation cycle Y",
            0.01,
            emitter_static.cycle_y
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.phase_rnd_x",
            "Random phase X",
            "Fluctuation random phase X",
            0.01,
            emitter_static.phase_rnd_x
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.phase_rnd_y",
            "Random phase Y",
            "Fluctuation random phase Y",
            0.01,
            emitter_static.phase_rnd_y
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.phase_init_x",
            "Initial phase X",
            "Fluctuation initial phase X",
            0.01,
            emitter_static.phase_init_x
        ),
        fl!(
            G_FLUCTUATION,
            "emitter_static.phase_init_y",
            "Initial phase Y",
            "Fluctuation initial phase Y",
            0.01,
            emitter_static.phase_init_y
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_apply_alpha",
            "Apply to alpha",
            "Whether to apply the fluctuation to alpha",
            particle_fluctuation,
            is_apply_alpha
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_applay_scale",
            "Apply to scale",
            "Whether to apply the fluctuation to scaling",
            particle_fluctuation,
            is_applay_scale
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_applay_scale_y",
            "Apply to scale Y",
            "Set the y axis individually",
            particle_fluctuation,
            is_applay_scale_y
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_wave_type",
            "Wave type",
            "Fluctuation waveform type",
            particle_fluctuation,
            is_wave_type
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_phase_random_x",
            "Phase random X",
            "Dependent randomness X",
            particle_fluctuation,
            is_phase_random_x
        ),
        opt_fg!(
            G_FLUCTUATION,
            "particle_fluctuation.is_phase_random_y",
            "Phase random Y",
            "Fluctuation randomness Y",
            particle_fluctuation,
            is_phase_random_y
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.color_scale",
            "Colour scale",
            "Colour scaling",
            0.01,
            emitter_static.color_scale
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_color0_keys",
            "Colour 0 key count",
            "How many of the eight colour 0 keyframes are live",
            emitter_static.num_color0_keys
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_alpha0_keys",
            "Alpha 0 key count",
            "How many of the eight alpha 0 keyframes are live",
            emitter_static.num_alpha0_keys
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_color1_keys",
            "Colour 1 key count",
            "How many of the eight colour 1 keyframes are live",
            emitter_static.num_color1_keys
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_alpha1_keys",
            "Alpha 1 key count",
            "How many of the eight alpha 1 keyframes are live",
            emitter_static.num_alpha1_keys
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_scale_keys",
            "Scale key count",
            "How many of the eight scale keyframes are live",
            emitter_static.num_scale_keys
        ),
        it!(
            G_ANIM_RATES,
            "emitter_static.num_param_keys",
            "Shader coefficient key count",
            "How many of the eight shader-coefficient keyframes are live",
            emitter_static.num_param_keys
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.color0_loop_rate",
            "Colour 0 loop rate",
            "Colour 0 loop frame rate, as a percent of one cycle's lifespan",
            0.01,
            emitter_static.color0_loop_rate
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.alpha0_loop_rate",
            "Alpha 0 loop rate",
            "Alpha 0 loop frame rate, as a percent of one cycle's lifespan",
            0.01,
            emitter_static.alpha0_loop_rate
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.color1_loop_rate",
            "Colour 1 loop rate",
            "Colour 1 loop frame rate, as a percent of one cycle's lifespan",
            0.01,
            emitter_static.color1_loop_rate
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.alpha1_loop_rate",
            "Alpha 1 loop rate",
            "Alpha 1 loop frame rate, as a percent of one cycle's lifespan",
            0.01,
            emitter_static.alpha1_loop_rate
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.scale_loop_rate",
            "Scale loop rate",
            "Scale loop frame rate, as a percent of one cycle's lifespan",
            0.01,
            emitter_static.scale_loop_rate
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.color0_loop_random",
            "Colour 0 loop random",
            "Initial position randomness of the colour 0 animation",
            0.01,
            emitter_static.color0_loop_random
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.alpha0_loop_random",
            "Alpha 0 loop random",
            "Initial position randomness of the alpha 0 animation",
            0.01,
            emitter_static.alpha0_loop_random
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.color1_loop_random",
            "Colour 1 loop random",
            "Initial position randomness of the colour 1 animation",
            0.01,
            emitter_static.color1_loop_random
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.alpha1_loop_random",
            "Alpha 1 loop random",
            "Initial position randomness of the alpha 1 animation",
            0.01,
            emitter_static.alpha1_loop_random
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.scale_loop_random",
            "Scale loop random",
            "Initial position randomness of the scale animation",
            0.01,
            emitter_static.scale_loop_random
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.coefficient0",
            "Shader coefficient 0",
            "Shader coefficient 0",
            0.01,
            emitter_static.coefficient0
        ),
        fl!(
            G_ANIM_RATES,
            "emitter_static.coefficient1",
            "Shader coefficient 1",
            "Shader coefficient 1",
            0.01,
            emitter_static.coefficient1
        ),
    ]
}

fn render_and_combiner() -> Vec<Attr> {
    vec![
        fb!(
            G_RENDER,
            "render_state.is_blend_enable",
            "Blend",
            "Blend",
            render_state.is_blend_enable
        ),
        fb!(
            G_RENDER,
            "render_state.is_depth_test",
            "Depth test",
            "Depth test",
            render_state.is_depth_test
        ),
        it!(
            G_RENDER,
            "render_state.depth_func",
            "Depth function",
            "Depth test pass conditions",
            render_state.depth_func
        ),
        fb!(
            G_RENDER,
            "render_state.is_depth_mask",
            "Depth mask",
            "Depth mask",
            render_state.is_depth_mask
        ),
        fb!(
            G_RENDER,
            "render_state.is_alpha_test",
            "Alpha test",
            "Alpha test",
            render_state.is_alpha_test
        ),
        it!(
            G_RENDER,
            "render_state.alpha_func",
            "Alpha function",
            "Alpha test pass conditions",
            render_state.alpha_func
        ),
        it!(
            G_RENDER,
            "render_state.blend_type",
            "Blend type",
            "Blending type for blending with the framebuffer",
            render_state.blend_type
        ),
        it!(
            G_RENDER,
            "render_state.display_side",
            "Display side",
            "Which face is drawn",
            render_state.display_side
        ),
        fl!(
            G_RENDER,
            "render_state.alpha_threshold",
            "Alpha threshold",
            "Alpha threshold",
            0.01,
            render_state.alpha_threshold
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.soft_edge_param1",
            "Soft edge 1",
            "Soft particles",
            0.01,
            emitter_static.soft_edge_param1
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.soft_edge_param2",
            "Soft edge 2",
            "Soft particles",
            0.01,
            emitter_static.soft_edge_param2
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.fresnel_alpha_param1",
            "Fresnel alpha 1",
            "Fresnel alpha",
            0.01,
            emitter_static.fresnel_alpha_param1
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.fresnel_alpha_param2",
            "Fresnel alpha 2",
            "Fresnel alpha",
            0.01,
            emitter_static.fresnel_alpha_param2
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.near_dist_alpha_param1",
            "Near distance alpha 1",
            "Near distance alpha",
            0.01,
            emitter_static.near_dist_alpha_param1
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.near_dist_alpha_param2",
            "Near distance alpha 2",
            "Near distance alpha",
            0.01,
            emitter_static.near_dist_alpha_param2
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.far_dist_alpha_param1",
            "Far distance alpha 1",
            "Far distance alpha",
            0.01,
            emitter_static.far_dist_alpha_param1
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.far_dist_alpha_param2",
            "Far distance alpha 2",
            "Far distance alpha",
            0.01,
            emitter_static.far_dist_alpha_param2
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.decal_param1",
            "Decal 1",
            "Decals",
            0.01,
            emitter_static.decal_param1
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.decal_param2",
            "Decal 2",
            "Decals",
            0.01,
            emitter_static.decal_param2
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.alpha_threshold",
            "Alpha test threshold",
            "Threshold value for the alpha test",
            0.01,
            emitter_static.alpha_threshold
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.soft_partcile_dist",
            "Soft particle distance",
            "Start-to-fade distance",
            0.01,
            emitter_static.soft_partcile_dist
        ),
        fl!(
            G_ALPHA_FX,
            "emitter_static.soft_particle_volume",
            "Soft particle volume",
            "Soft particle volume value",
            0.01,
            emitter_static.soft_particle_volume
        ),
        cmb!(
            G_COMBINER,
            "combiner.color_combiner_process",
            "Colour process",
            "Colour calculation formula type",
            color_combiner_process
        ),
        cmb!(
            G_COMBINER,
            "combiner.alpha_combiner_process",
            "Alpha process",
            "Alpha calculation formula type",
            alpha_combiner_process
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.texture1_color_blend",
            "Blend texture 1 colour",
            "Combines the texture1 colour with the colour in the top row",
            texture1_color_blend
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.texture2_color_blend",
            "Blend texture 2 colour",
            "Combines the texture2 colour with the colour in the top row",
            texture2_color_blend
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.primitive_color_blend",
            "Blend primitive colour",
            "Combines the primitive colour with the colour in the top row",
            primitive_color_blend
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.texture1_alpha_blend",
            "Blend texture 1 alpha",
            "Combines the texture1 alpha with the alpha in the top row",
            texture1_alpha_blend
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.texture2_alpha_blend",
            "Blend texture 2 alpha",
            "Combines the texture2 alpha with the alpha in the top row",
            texture2_alpha_blend
        ),
        cmb_fg!(
            G_COMBINER,
            "combiner.primitive_alpha_blend",
            "Blend primitive alpha",
            "Combines the primitive alpha with the alpha in the top row",
            primitive_alpha_blend
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_color0_input_type",
            "Texture colour 0 input",
            "Texture colour 0 input type",
            tex_color0_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_color1_input_type",
            "Texture colour 1 input",
            "Texture colour 1 input type",
            tex_color1_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_color2_input_type",
            "Texture colour 2 input",
            "Texture colour 2 input type",
            tex_color2_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_alpha0_input_type",
            "Texture alpha 0 input",
            "Texture alpha 0 input type",
            tex_alpha0_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_alpha1_input_type",
            "Texture alpha 1 input",
            "Texture alpha 1 input type",
            tex_alpha1_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.tex_alpha2_input_type",
            "Texture alpha 2 input",
            "Texture alpha 2 input type",
            tex_alpha2_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.primitive_color_input_type",
            "Primitive colour input",
            "Primitive colour input type",
            primitive_color_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.primitive_alpha_input_type",
            "Primitive alpha input",
            "Primitive alpha input type",
            primitive_alpha_input_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.shader_type",
            "Shader type",
            "Shader type",
            shader_type
        ),
        cmb_wide!(
            G_COMBINER,
            "combiner.apply_alpha",
            "Apply alpha",
            "Refraction shader, or apply the alpha value",
            apply_alpha
        ),
        cmb_wide_fg!(
            G_COMBINER,
            "combiner.is_distortion_by_camera_distance",
            "Distortion by camera distance",
            "Whether to enhance the distortion with the camera's distance",
            is_distortion_by_camera_distance
        ),
    ]
}

fn inheritance_and_shader() -> Vec<Attr> {
    vec![
        fg!(G_INHERIT, "child_inheritance.velocity", "Velocity", "Inherited speed", child_inheritance.velocity),
        fg!(G_INHERIT, "child_inheritance.scale", "Scale", "Inherited scale", child_inheritance.scale),
        fg!(G_INHERIT, "child_inheritance.rotate", "Rotation", "Inherited rotation", child_inheritance.rotate),
        fg!(G_INHERIT, "child_inheritance.color_scale", "Colour scale", "Inherited colour scale", child_inheritance.color_scale),
        fg!(G_INHERIT, "child_inheritance.color0", "Colour 0", "Inherited colour 0", child_inheritance.color0),
        fg!(G_INHERIT, "child_inheritance.color1", "Colour 1", "Inherited colour 1", child_inheritance.color1),
        fg!(G_INHERIT, "child_inheritance.alpha0", "Alpha 0", "Inherited alpha 0", child_inheritance.alpha0),
        fg!(G_INHERIT, "child_inheritance.alpha1", "Alpha 1", "Inherited alpha 1", child_inheritance.alpha1),
        fg!(G_INHERIT, "child_inheritance.draw_path", "Draw path", "Inherited draw path", child_inheritance.draw_path),
        fg!(G_INHERIT, "child_inheritance.pre_draw", "Pre-draw", "Draw before the inherited class", child_inheritance.pre_draw),
        fg!(G_INHERIT, "child_inheritance.alpha0_each_frame", "Alpha 0 each frame", "Whether to inherit alpha0 every frame", child_inheritance.alpha0_each_frame),
        fg!(G_INHERIT, "child_inheritance.alpha1_each_frame", "Alpha 1 each frame", "Whether to inherit alpha1 every frame", child_inheritance.alpha1_each_frame),
        fg!(G_INHERIT, "child_inheritance.enable_emitter_particle", "Emitter per particle", "Whether to generate an emitter for each particle", child_inheritance.enable_emitter_particle),
        fl!(G_INHERIT, "child_inheritance.velocity_rate", "Velocity rate", "Inherited velocity ratio", 0.01, child_inheritance.velocity_rate),
        fl!(G_INHERIT, "child_inheritance.scale_rate", "Scale rate", "Inherited scale ratio", 0.01, child_inheritance.scale_rate),
        it!(G_SHADER, "action.action_index", "Custom action index", "Selected custom action index", action.action_index),
        it!(G_SHADER, "shader_references.type_", "Shader type", "Shader type", shader_references.type_),
        it!(G_SHADER, "shader_references.shader_index", "Shader index", "Shader index to use. Pointing this outside the variations this eff ships is what makes a file fail to load.", shader_references.shader_index),
        it!(G_SHADER, "shader_references.compute_shader_index", "Compute shader index", "Compute shader index to use", shader_references.compute_shader_index),
        it!(G_SHADER, "shader_references.user_shader_index1", "User shader index 1", "User shader index 1", shader_references.user_shader_index1),
        it!(G_SHADER, "shader_references.user_shader_index2", "User shader index 2", "User shader index 2", shader_references.user_shader_index2),
        it!(G_SHADER, "shader_references.custom_shader_index", "Custom shader index", "Custom shader index", shader_references.custom_shader_index),
        Attr {
            id: "shader_references.custom_shader_flag", group: G_SHADER,
            label: "Custom shader flag", doc: "Custom shader option flag bits",
            kind: AttrKind::UInt,
            get: |d| d.shader_references.custom_shader_flag.map(AttrValue::UInt),
            set: |d, v| { if let Some(slot) = d.shader_references.custom_shader_flag.as_mut() { *slot = v.as_u64(); } },
        },
        Attr {
            id: "shader_references.custom_shader_switch", group: G_SHADER,
            label: "Custom shader switch", doc: "Custom shader option switch-selection bits",
            kind: AttrKind::UInt,
            get: |d| d.shader_references.custom_shader_switch.map(AttrValue::UInt),
            set: |d, v| { if let Some(slot) = d.shader_references.custom_shader_switch.as_mut() { *slot = v.as_u64(); } },
        },
        it!(G_SHADER, "shader_references.extra_shader_index2", "Extra shader index", "Index of the shader generated by the effect combiner", shader_references.extra_shader_index2),
    ]
}

fn samplers() -> Vec<Attr> {
    let mut v = Vec::new();
    macro_rules! bank {
        ($g:expr, $opt:ident, $($id:literal),+ $(,)?) => {
            let ids: [&'static str; 12] = [$($id),+];
            v.extend([
                Attr {
                    id: ids[0], group: $g, label: "Wrap U",
                    doc: "U wrap mode", kind: AttrKind::Enum(WRAP_MODES),
                    get: |d| d.$opt.as_ref().map(|s| AttrValue::Int(s.wrap_u.as_u8() as i64)),
                    set: |d, v| { if let Some(s) = d.$opt.as_mut() { s.wrap_u = WrapMode::from_u8(v.as_i64().clamp(0, 255) as u8); } },
                },
                Attr {
                    id: ids[1], group: $g, label: "Wrap V",
                    doc: "V wrap mode", kind: AttrKind::Enum(WRAP_MODES),
                    get: |d| d.$opt.as_ref().map(|s| AttrValue::Int(s.wrap_v.as_u8() as i64)),
                    set: |d, v| { if let Some(s) = d.$opt.as_mut() { s.wrap_v = WrapMode::from_u8(v.as_i64().clamp(0, 255) as u8); } },
                },
                opt_it!($g, ids[2], "Filter", "Filter mode", $opt, filter),
                opt_fg!($g, ids[3], "Sphere map", "Whether a sphere map is used", $opt, is_sphere_map),
                opt_fl!($g, ids[4], "Max LOD", "Effective mip level (0.0 to 15.99)", 0.1, $opt, max_lod),
                opt_fl!($g, ids[5], "LOD bias", "Mip level bias", 0.1, $opt, lod_bias),
                opt_it!($g, ids[6], "Mip level limit", "Restrict the mipmap level", $opt, mip_level_limit),
                opt_fg!($g, ids[7], "Fix density U", "Fix texture density option U", $opt, is_density_fixed_u),
                opt_fg!($g, ids[8], "Fix density V", "Fix texture density option V", $opt, is_density_fixed_v),
                opt_fg!($g, ids[9], "Square RGB", "Whether to square the texture's RGB values when reading them", $opt, is_square_rgb),
                opt_uit!($g, ids[10], "Texture GUID", "GUID of the texture this sampler reads. The Texture panel is the safe way to change this — a GUID the pool has no descriptor for samples nothing.", $opt, texture_id),
                Attr {
                    id: ids[11], group: $g, label: "External texture",
                    doc: "Whether the texture is stored in another binary", kind: AttrKind::Flag,
                    get: |d| d.$opt.as_ref().and_then(|s| s.is_on_another_binary).map(|v| AttrValue::Int(v as i64)),
                    set: |d, v| { if let Some(slot) = d.$opt.as_mut().and_then(|s| s.is_on_another_binary.as_mut()) { *slot = u8::from(v.as_bool()); } },
                },
            ]);
        };
    }
    bank!(
        G_SAMPLER0,
        sampler0,
        "sampler0.wrap_u",
        "sampler0.wrap_v",
        "sampler0.filter",
        "sampler0.is_sphere_map",
        "sampler0.max_lod",
        "sampler0.lod_bias",
        "sampler0.mip_level_limit",
        "sampler0.is_density_fixed_u",
        "sampler0.is_density_fixed_v",
        "sampler0.is_square_rgb",
        "sampler0.texture_id",
        "sampler0.is_on_another_binary",
    );
    bank!(
        G_SAMPLER1,
        sampler1,
        "sampler1.wrap_u",
        "sampler1.wrap_v",
        "sampler1.filter",
        "sampler1.is_sphere_map",
        "sampler1.max_lod",
        "sampler1.lod_bias",
        "sampler1.mip_level_limit",
        "sampler1.is_density_fixed_u",
        "sampler1.is_density_fixed_v",
        "sampler1.is_square_rgb",
        "sampler1.texture_id",
        "sampler1.is_on_another_binary",
    );
    bank!(
        G_SAMPLER2,
        sampler2,
        "sampler2.wrap_u",
        "sampler2.wrap_v",
        "sampler2.filter",
        "sampler2.is_sphere_map",
        "sampler2.max_lod",
        "sampler2.lod_bias",
        "sampler2.mip_level_limit",
        "sampler2.is_density_fixed_u",
        "sampler2.is_density_fixed_v",
        "sampler2.is_square_rgb",
        "sampler2.texture_id",
        "sampler2.is_on_another_binary",
    );
    v
}

fn texture_anims() -> Vec<Attr> {
    let mut v = Vec::new();
    macro_rules! anim {
        ($g:expr, $opt:ident, $($id:literal),+ $(,)?) => {
            let ids: [&'static str; 10] = [$($id),+];
            v.extend([
                opt_it!($g, ids[0], "Pattern anim type", "Pattern animation type", $opt, pattern_anim_type),
                opt_fb!($g, ids[1], "Scroll", "Enable or disable UV scrolling animation", $opt, is_scroll),
                opt_fb!($g, ids[2], "Rotate", "Enable or disable UV rotation animation", $opt, is_rotate),
                opt_fb!($g, ids[3], "Scale", "Enable or disable UV scaling animation", $opt, is_scale),
                opt_it!($g, ids[4], "Repeat", "Repetition count", $opt, repeat),
                opt_it!($g, ids[5], "Invert random U", "U invert randomness", $opt, inv_rand_u),
                opt_it!($g, ids[6], "Invert random V", "V invert randomness", $opt, inv_rand_v),
                opt_fg!($g, ids[7], "Pattern loop random", "Texture pattern animation loop start randomness", $opt, is_pat_anim_loop_random),
                opt_it!($g, ids[8], "UV channel", "Primitive UV channel", $opt, uv_channel),
                opt_fg!($g, ids[9], "Crossfade", "Enable or disable crossfade", $opt, is_crossfade),
            ]);
        };
    }
    anim!(
        G_TEXANIM0,
        texture_anim0,
        "texture_anim0.pattern_anim_type",
        "texture_anim0.is_scroll",
        "texture_anim0.is_rotate",
        "texture_anim0.is_scale",
        "texture_anim0.repeat",
        "texture_anim0.inv_rand_u",
        "texture_anim0.inv_rand_v",
        "texture_anim0.is_pat_anim_loop_random",
        "texture_anim0.uv_channel",
        "texture_anim0.is_crossfade",
    );
    anim!(
        G_TEXANIM1,
        texture_anim1,
        "texture_anim1.pattern_anim_type",
        "texture_anim1.is_scroll",
        "texture_anim1.is_rotate",
        "texture_anim1.is_scale",
        "texture_anim1.repeat",
        "texture_anim1.inv_rand_u",
        "texture_anim1.inv_rand_v",
        "texture_anim1.is_pat_anim_loop_random",
        "texture_anim1.uv_channel",
        "texture_anim1.is_crossfade",
    );
    anim!(
        G_TEXANIM2,
        texture_anim2,
        "texture_anim2.pattern_anim_type",
        "texture_anim2.is_scroll",
        "texture_anim2.is_rotate",
        "texture_anim2.is_scale",
        "texture_anim2.repeat",
        "texture_anim2.inv_rand_u",
        "texture_anim2.inv_rand_v",
        "texture_anim2.is_pat_anim_loop_random",
        "texture_anim2.uv_channel",
        "texture_anim2.is_crossfade",
    );

    macro_rules! pat {
        ($g:expr, $anim:ident, $($id:literal),+ $(,)?) => {
            let ids: [&'static str; 4] = [$($id),+];
            v.extend([
                fl!($g, ids[0], "Pattern table count", "Number of pattern tables", 1.0, emitter_static.$anim.num),
                fl!($g, ids[1], "Pattern frequency", "Pattern frequency", 1.0, emitter_static.$anim.frequency),
                fl!($g, ids[2], "Random pattern count", "Number of patterns for random patterns", 1.0, emitter_static.$anim.num_random),
                fl!($g, ids[3], "Loop count", "Number of surface animation loops", 1.0, emitter_static.$anim.pad),
            ]);
        };
    }
    pat!(
        G_TEXPAT0,
        tex_pattern_anim0,
        "emitter_static.tex_pattern_anim0.num",
        "emitter_static.tex_pattern_anim0.frequency",
        "emitter_static.tex_pattern_anim0.num_random",
        "emitter_static.tex_pattern_anim0.pad"
    );
    pat!(
        G_TEXPAT1,
        tex_pattern_anim1,
        "emitter_static.tex_pattern_anim1.num",
        "emitter_static.tex_pattern_anim1.frequency",
        "emitter_static.tex_pattern_anim1.num_random",
        "emitter_static.tex_pattern_anim1.pad"
    );
    pat!(
        G_TEXPAT2,
        tex_pattern_anim2,
        "emitter_static.tex_pattern_anim2.num",
        "emitter_static.tex_pattern_anim2.frequency",
        "emitter_static.tex_pattern_anim2.num_random",
        "emitter_static.tex_pattern_anim2.pad"
    );

    macro_rules! uv {
        ($g:expr, $anim:ident, $prefix:literal) => {
            v.extend([
                fl!(
                    $g,
                    concat!($prefix, ".scroll_add_x"),
                    "Scroll add X",
                    "X value to add when scrolling",
                    0.001,
                    emitter_static.$anim.scroll_add_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scroll_add_y"),
                    "Scroll add Y",
                    "Y value to add when scrolling",
                    0.001,
                    emitter_static.$anim.scroll_add_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scroll_x"),
                    "Scroll X",
                    "X initial scroll value",
                    0.01,
                    emitter_static.$anim.scroll_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scroll_y"),
                    "Scroll Y",
                    "Y initial scroll value",
                    0.01,
                    emitter_static.$anim.scroll_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scroll_random_x"),
                    "Scroll random X",
                    "X random initial scroll value",
                    0.01,
                    emitter_static.$anim.scroll_random_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scroll_random_y"),
                    "Scroll random Y",
                    "Y random initial scroll value",
                    0.01,
                    emitter_static.$anim.scroll_random_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_add_x"),
                    "Scale add X",
                    "X value to add when scaling",
                    0.001,
                    emitter_static.$anim.scale_add_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_add_y"),
                    "Scale add Y",
                    "Y value to add when scaling",
                    0.001,
                    emitter_static.$anim.scale_add_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_x"),
                    "Scale X",
                    "X initial scale value",
                    0.01,
                    emitter_static.$anim.scale_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_y"),
                    "Scale Y",
                    "Y initial scale value",
                    0.01,
                    emitter_static.$anim.scale_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_random_x"),
                    "Scale random X",
                    "X random initial scale",
                    0.01,
                    emitter_static.$anim.scale_random_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".scale_random_y"),
                    "Scale random Y",
                    "Y random initial scale",
                    0.01,
                    emitter_static.$anim.scale_random_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".rotation_add"),
                    "Rotation add",
                    "Value to add when rotating",
                    0.001,
                    emitter_static.$anim.rotation_add
                ),
                fl!(
                    $g,
                    concat!($prefix, ".rotation"),
                    "Rotation",
                    "Initial rotation value",
                    0.01,
                    emitter_static.$anim.rotation
                ),
                fl!(
                    $g,
                    concat!($prefix, ".rotation_random"),
                    "Rotation random",
                    "Random initial rotation value",
                    0.01,
                    emitter_static.$anim.rotation_random
                ),
                fl!(
                    $g,
                    concat!($prefix, ".rotation_type"),
                    "Rotation type",
                    "Random rotation type",
                    1.0,
                    emitter_static.$anim.rotation_type
                ),
                fl!(
                    $g,
                    concat!($prefix, ".uv_scale_x"),
                    "UV scale X",
                    "X UV scale value",
                    0.01,
                    emitter_static.$anim.uv_scale_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".uv_scale_y"),
                    "UV scale Y",
                    "Y UV scale value",
                    0.01,
                    emitter_static.$anim.uv_scale_y
                ),
                fl!(
                    $g,
                    concat!($prefix, ".uv_div_x"),
                    "UV divisions X",
                    "Number of horizontal divisions",
                    1.0,
                    emitter_static.$anim.uv_div_x
                ),
                fl!(
                    $g,
                    concat!($prefix, ".uv_div_y"),
                    "UV divisions Y",
                    "Number of vertical divisions",
                    1.0,
                    emitter_static.$anim.uv_div_y
                ),
            ]);
        };
    }
    uv!(
        G_TEXUV0,
        tex_scroll_anim0,
        "emitter_static.tex_scroll_anim0"
    );
    uv!(
        G_TEXUV1,
        tex_scroll_anim1,
        "emitter_static.tex_scroll_anim1"
    );
    uv!(
        G_TEXUV2,
        tex_scroll_anim2,
        "emitter_static.tex_scroll_anim2"
    );
    v
}

/// Fixed-size arrays inside `EmitterData` are attributes too. They are expanded into stable,
/// individually addressable rows so every pattern slot and every component of every keyframe can
/// be edited without replacing an opaque blob.
fn array_attributes() -> Vec<Attr> {
    let mut v = Vec::new();

    macro_rules! pattern_table {
        ($group:expr, $field:ident, $prefix:literal; $($i:tt),+ $(,)?) => {
            $(v.push(Attr {
                id: concat!($prefix, ".table[", stringify!($i), "]"),
                group: $group,
                label: concat!("Pattern slot ", stringify!($i)),
                doc: "Texture index stored in this pattern-animation slot",
                kind: AttrKind::Int,
                get: |d| Some(AttrValue::Int(d.emitter_static.$field.table[$i] as i64)),
                set: |d, value| {
                    d.emitter_static.$field.table[$i] =
                        value.as_i64().clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                },
            });)+
        };
    }
    pattern_table!(G_TEXPAT0, tex_pattern_anim0, "emitter_static.tex_pattern_anim0";
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
    pattern_table!(G_TEXPAT1, tex_pattern_anim1, "emitter_static.tex_pattern_anim1";
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
    pattern_table!(G_TEXPAT2, tex_pattern_anim2, "emitter_static.tex_pattern_anim2";
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);

    macro_rules! key_table {
        ($group:expr, $field:ident, $prefix:literal; $($i:tt),+ $(,)?) => {
            $(v.extend([
                Attr {
                    id: concat!($prefix, ".keys[", stringify!($i), "].x"), group: $group,
                    label: concat!("Key ", stringify!($i), " X"), doc: "Keyframe X component",
                    kind: AttrKind::Float { speed: 0.01 },
                    get: |d| Some(AttrValue::Float(d.emitter_static.$field.keys[$i].x)),
                    set: |d, value| d.emitter_static.$field.keys[$i].x = value.as_f32(),
                },
                Attr {
                    id: concat!($prefix, ".keys[", stringify!($i), "].y"), group: $group,
                    label: concat!("Key ", stringify!($i), " Y"), doc: "Keyframe Y component",
                    kind: AttrKind::Float { speed: 0.01 },
                    get: |d| Some(AttrValue::Float(d.emitter_static.$field.keys[$i].y)),
                    set: |d, value| d.emitter_static.$field.keys[$i].y = value.as_f32(),
                },
                Attr {
                    id: concat!($prefix, ".keys[", stringify!($i), "].z"), group: $group,
                    label: concat!("Key ", stringify!($i), " Z"), doc: "Keyframe Z component",
                    kind: AttrKind::Float { speed: 0.01 },
                    get: |d| Some(AttrValue::Float(d.emitter_static.$field.keys[$i].z)),
                    set: |d, value| d.emitter_static.$field.keys[$i].z = value.as_f32(),
                },
                Attr {
                    id: concat!($prefix, ".keys[", stringify!($i), "].time"), group: $group,
                    label: concat!("Key ", stringify!($i), " frame"), doc: "Keyframe time in frames",
                    kind: AttrKind::Float { speed: 0.1 },
                    get: |d| Some(AttrValue::Float(d.emitter_static.$field.keys[$i].time)),
                    set: |d, value| d.emitter_static.$field.keys[$i].time = value.as_f32(),
                },
            ]);)+
        };
    }
    key_table!(G_COLOR_KEYS, color0, "emitter_static.color0"; 0, 1, 2, 3, 4, 5, 6, 7);
    key_table!(G_COLOR_KEYS, color1, "emitter_static.color1"; 0, 1, 2, 3, 4, 5, 6, 7);
    key_table!(G_ALPHA_KEYS, alpha0, "emitter_static.alpha0"; 0, 1, 2, 3, 4, 5, 6, 7);
    key_table!(G_ALPHA_KEYS, alpha1, "emitter_static.alpha1"; 0, 1, 2, 3, 4, 5, 6, 7);
    key_table!(G_SCALE_KEYS, scale_anim, "emitter_static.scale_anim"; 0, 1, 2, 3, 4, 5, 6, 7);
    key_table!(G_PARAM_KEYS, param_anim, "emitter_static.param_anim"; 0, 1, 2, 3, 4, 5, 6, 7);

    macro_rules! shader_bytes {
        ($field:ident, $prefix:literal, $label:literal; $($i:tt),+ $(,)?) => {
            $(v.push(Attr {
                id: concat!($prefix, "[", stringify!($i), "]"), group: G_SHADER,
                label: concat!($label, " byte ", stringify!($i)),
                doc: "Raw byte in the fixed-size user shader definition",
                kind: AttrKind::Int,
                get: |d| d.shader_references.$field.get($i).map(|value| AttrValue::Int(*value as i64)),
                set: |d, value| {
                    if let Some(slot) = d.shader_references.$field.get_mut($i) {
                        *slot = value.as_i64().clamp(0, 255) as u8;
                    }
                },
            });)+
        };
    }
    shader_bytes!(user_shader_define1, "shader_references.user_shader_define1", "User define 1";
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
    shader_bytes!(user_shader_define2, "shader_references.user_shader_define2", "User define 2";
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);

    v
}

// ── Lookup ────────────────────────────────────────────────────────────────────

static TABLE: std::sync::OnceLock<Vec<Attr>> = std::sync::OnceLock::new();
static BY_ID: std::sync::OnceLock<std::collections::HashMap<&'static str, usize>> =
    std::sync::OnceLock::new();

/// Every editable attribute, in a fixed order. An index into this slice is what the editor's
/// per-emitter value vectors are keyed by, so it must stay stable for the life of the process —
/// which it is, being built once from a constant table.
pub fn table() -> &'static [Attr] {
    TABLE.get_or_init(build)
}

pub fn index_of(id: &str) -> Option<usize> {
    BY_ID
        .get_or_init(|| table().iter().enumerate().map(|(i, a)| (a.id, i)).collect())
        .get(id)
        .copied()
}

/// Read every attribute off one emitter, aligned to [`table`].
pub fn read_all(data: &EmitterData) -> Vec<Option<AttrValue>> {
    table().iter().map(|a| (a.get)(data)).collect()
}

/// Write one attribute by id. Unknown ids are reported rather than ignored: an id that no longer
/// resolves means a project is carrying an edit that will silently not ship.
pub fn write(data: &mut EmitterData, id: &str, value: AttrValue) -> bool {
    match index_of(id) {
        Some(i) => {
            (table()[i].set)(data, value);
            true
        }
        None => false,
    }
}

/// The vfx version Smash's own `.eff` files carry. Every version-gated field in the format is
/// resolved against it — the combiner layout, the name field's width, which sampler and texture
/// animation blocks exist — so a fixture has to be built at this version to hold what a real
/// emitter holds.
#[cfg(test)]
pub const SSBU_VFX_VERSION: u16 = 22;

/// An all-zero emitter at a given version: every block that version defines, no values. Lets a
/// test have an `EmitterData` without a `.eff` to read one out of.
#[cfg(test)]
pub fn blank_emitter_data(version: u16) -> EmitterData {
    let zeros = vec![0u8; 16 * 1024];
    EmitterData::read(&mut std::io::Cursor::new(zeros), version)
        .expect("a zeroed buffer is a readable emitter")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are the project-file keys and the lookup map's keys — a duplicate would make one of
    /// the two rows unreachable and silently drop its edits.
    #[test]
    fn ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for attr in table() {
            assert!(seen.insert(attr.id), "duplicate attribute id '{}'", attr.id);
        }
        assert!(table().len() >= 758, "only {} attributes", table().len());
    }

    /// Fixed tables and opaque bitfields are easy to miss because they are arrays rather than
    /// standalone members in the parser. These are representative endpoints for every family
    /// documented by EffectResearch.
    #[test]
    fn documented_fixed_tables_and_flags_are_editable() {
        for id in [
            "emitter_static.tex_pattern_anim0.table[31]",
            "emitter_static.tex_pattern_anim1.table[31]",
            "emitter_static.tex_pattern_anim2.table[31]",
            "emitter_static.color0.keys[7].time",
            "emitter_static.alpha1.keys[7].z",
            "emitter_static.scale_anim.keys[7].x",
            "emitter_static.param_anim.keys[7].y",
            "emitter_static.flags4",
            "particle_data.prim_flag2",
            "shader_references.custom_shader_switch",
            "shader_references.user_shader_define2[15]",
            "sampler2.texture_id",
        ] {
            assert!(
                index_of(id).is_some(),
                "documented attribute '{id}' is missing"
            );
        }
    }

    #[test]
    fn narrow_integer_edits_clamp_instead_of_wrapping() {
        let mut data = blank_emitter_data(SSBU_VFX_VERSION);
        assert!(write(
            &mut data,
            "emitter_info.sort_type",
            AttrValue::Int(999)
        ));
        let read = table()[index_of("emitter_info.sort_type").unwrap()].get;
        assert_eq!(read(&data), Some(AttrValue::Int(255)));
        assert!(write(
            &mut data,
            "emitter_info.sort_type",
            AttrValue::Int(-20)
        ));
        assert_eq!(read(&data), Some(AttrValue::Int(0)));
    }

    /// Every attribute must belong to a group the UI actually stacks, or its rows never draw.
    #[test]
    fn groups_are_declared() {
        for attr in table() {
            assert!(
                GROUPS.contains(&attr.group),
                "attribute '{}' is in undeclared group '{}'",
                attr.id,
                attr.group
            );
        }
    }

    /// Writing an attribute must change THAT attribute and nothing else.
    ///
    /// The table is a few hundred hand-written getter/setter pairs over a struct with repeated
    /// field names (`scale_x` exists on three different blocks, `alpha_threshold` on two), so a
    /// row pointing at its neighbour's field is the mistake to expect. It would be invisible in
    /// the UI — the row would move, just not the one the user is looking at — which is why this
    /// checks every attribute rather than a sample.
    #[test]
    fn each_attribute_writes_only_itself() {
        let base = blank_emitter_data(SSBU_VFX_VERSION);
        let before = read_all(&base);
        for (i, attr) in table().iter().enumerate() {
            let Some(value) = before[i] else {
                continue; // not a block this version carries
            };
            // A value the field cannot be holding already, within the range every kind accepts.
            let poke = match attr.kind {
                AttrKind::Float { .. } => {
                    AttrValue::Float(if value.as_f32() == 3.0 { 5.0 } else { 3.0 })
                }
                AttrKind::Int | AttrKind::Enum(_) => {
                    AttrValue::Int(if value.as_i64() == 3 { 5 } else { 3 })
                }
                AttrKind::UInt => AttrValue::UInt(if value.as_u64() == 3 { 5 } else { 3 }),
                AttrKind::Flag => AttrValue::Int(i64::from(!value.as_bool())),
            };
            let mut data = base.clone();
            assert!(
                write(&mut data, attr.id, poke),
                "'{}' did not resolve",
                attr.id
            );
            let after = read_all(&data);

            assert!(
                after[i].is_some_and(|v| v.same(poke)),
                "writing '{}' did not take: wrote {poke:?}, read back {:?}",
                attr.id,
                after[i]
            );
            let spilled: Vec<&str> = table()
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter(|(j, _)| match (before[*j], after[*j]) {
                    (Some(a), Some(b)) => !a.same(b),
                    (a, b) => a.is_some() != b.is_some(),
                })
                .map(|(_, a)| a.id)
                .collect();
            assert!(
                spilled.is_empty(),
                "writing '{}' also changed {spilled:?}",
                attr.id
            );
        }
    }

    /// A real game emitter has to expose the whole table, or the editor's rows are a list of
    /// things that are always absent. Skipped without the corpus.
    #[test]
    fn a_real_emitter_carries_most_attributes() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let path = root.join("effect/fighter/mario/ef_mario.eff");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skipped: {} unreadable", path.display());
            return;
        };
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("mario's eff parses");
        let ptcl = file.ptcl_file.expect("mario's eff has a PTCL");
        let emitter = ptcl
            .emitter_list
            .emitter_sets
            .iter()
            .flat_map(|s| s.emitters.iter())
            .next()
            .expect("at least one emitter");

        let values = read_all(&emitter.data);
        let present = values.iter().filter(|v| v.is_some()).count();
        let absent: Vec<&str> = table()
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|(a, _)| a.id)
            .collect();
        eprintln!(
            "{present}/{} attributes present on '{}'; absent: {absent:?}",
            table().len(),
            emitter.data.display_name()
        );
        assert_eq!(
            present,
            table().len(),
            "a real SSBU emitter is missing documented rows: {absent:?}"
        );
    }
}
