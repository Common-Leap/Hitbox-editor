//! NintendoWare emitter combiner — maps `CombinerState` to CPU particle color
//! and GPU NVN chain coefficient slots (cbuf_8[6-7], cbuf_16[1-3]).
//!
//! Blend / process values follow PTCL / Eft conventions (see NSMBU PTCL wiki):
//! - ColorCombinerProcess: 0=Color0, 1=Color0*Tex, 2=Color0*Tex+Color1*(1-Tex), 3=Color0*Tex+Color1
//! - Texture/Primitive *Blend: 0=modulate, 1=add, 2=subtract

use crate::shader_registry::CombinerState;

/// GPU coefficient vectors written into NVN constant buffers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CombinerGpuCoeffs {
    pub cbuf_8_slot_6: [f32; 4],
    pub cbuf_8_slot_7: [f32; 4],
    pub cbuf_16_slot_1: [f32; 4],
    pub cbuf_16_slot_2: [f32; 4],
    pub cbuf_16_slot_3: [f32; 4],
}

impl Default for CombinerGpuCoeffs {
    fn default() -> Self {
        Self::identity()
    }
}

impl CombinerGpuCoeffs {
    pub fn identity() -> Self {
        Self {
            cbuf_8_slot_6: [1.0, 1.0, 1.0, 1.0],
            cbuf_8_slot_7: [1.0, 1.0, 1.0, 1.0],
            cbuf_16_slot_1: fs_cbuf_16_slot_1(CombinerBlendOp::Modulate),
            cbuf_16_slot_2: fs_cbuf_16_slot_2(CombinerBlendOp::Modulate, 1.0),
            cbuf_16_slot_3: fs_cbuf_16_slot_3(CombinerBlendOp::Modulate),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinerBlendOp {
    Modulate,
    Add,
    Subtract,
}

pub fn blend_op_from_raw(mode: u32) -> CombinerBlendOp {
    match mode {
        1 => CombinerBlendOp::Add,
        2 => CombinerBlendOp::Subtract,
        _ => CombinerBlendOp::Modulate,
    }
}

/// Native FS cbuf_16[1]: chain registers are multiplied by `.z` (texture1 colour blend).
fn fs_cbuf_16_slot_1(op: CombinerBlendOp) -> [f32; 4] {
    match op {
        CombinerBlendOp::Modulate => [1.0, 0.0, 1.0, 1.0],
        CombinerBlendOp::Add => [1.0, 1.0, 1.0, 0.0],
        CombinerBlendOp::Subtract => [1.0, 0.0, 1.0, -1.0],
    }
}

/// Native FS cbuf_16[2]: `.y` is added to chain registers; `.z` is the colour-table lerp
/// threshold compared against `gpr_23_` (~1.0) to select lerp/add vs pass-through.
fn fs_cbuf_16_slot_2(op: CombinerBlendOp, table_threshold: f32) -> [f32; 4] {
    let y = match op {
        CombinerBlendOp::Modulate => 0.0,
        CombinerBlendOp::Add => 1.0,
        CombinerBlendOp::Subtract => -1.0,
    };
    [1.0, y, table_threshold, 0.0]
}

/// Native FS cbuf_16[3]: `.z` is the fma bias on the primitive-colour branch.
fn fs_cbuf_16_slot_3(op: CombinerBlendOp) -> [f32; 4] {
    match op {
        CombinerBlendOp::Modulate => [1.0, 0.0, 0.0, 1.0],
        CombinerBlendOp::Add => [1.0, 0.0, 1.0, 0.0],
        CombinerBlendOp::Subtract => [1.0, 0.0, -1.0, -1.0],
    }
}

/// Threshold written to cbuf_16[2].z for ColorCombinerProcess 2/3 table lerp/add.
fn color_table_chain_threshold(process: u32) -> f32 {
    match process {
        // Color0*Tex + Color1*(1-Tex): enable lerp branch at tex weight 0.5.
        2 => 0.5,
        // Color0*Tex + Color1: enable additive second-table branch.
        3 => 0.0,
        // Color0-only / Color0*Tex: keep branch disabled (gpr_23_=1 is not > 1).
        _ => 1.0,
    }
}

/// Color / alpha combiner process weights for cbuf_8 slots 6 (color) and 7 (alpha).
fn color_process_coeffs(process: u32) -> ([f32; 4], [f32; 4]) {
    match process {
        // Color0 only — disable color1 table contribution.
        0 => ([1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
        // Color0 * Texture (texture handled in FS; both tables active).
        1 => ([1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]),
        // Color0 * Texture + Color1 * (1 - Texture) — lerp between tables.
        2 => ([1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 0.5]),
        // Color0 * Texture + Color1 — additive second table.
        3 => ([1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 0.0]),
        _ => ([1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]),
    }
}

fn alpha_process_coeffs(process: u32) -> ([f32; 4], [f32; 4]) {
    // Alpha combiner uses the same process encoding as color in most emitters.
    color_process_coeffs(process)
}

/// Build GPU coefficient vectors from emitter combiner state.
pub fn eval_combiner_gpu_coeffs(combiner: &CombinerState) -> CombinerGpuCoeffs {
    if !combiner_is_configured(combiner) {
        return CombinerGpuCoeffs::identity();
    }
    let (c6, c7) = color_process_coeffs(combiner.color_combiner_process);
    let (a6, a7) = alpha_process_coeffs(combiner.alpha_combiner_process);

    // Merge color + alpha process into slots 6/7 (color in .xyz, alpha weight in .w).
    let slot_6 = [
        c6[0],
        c6[1],
        c6[2],
        a6[3],
    ];
    let slot_7 = [
        c7[0],
        c7[1],
        c7[2],
        a7[3],
    ];

    let table_threshold = color_table_chain_threshold(combiner.color_combiner_process)
        .min(color_table_chain_threshold(combiner.alpha_combiner_process));

    CombinerGpuCoeffs {
        cbuf_8_slot_6: slot_6,
        cbuf_8_slot_7: slot_7,
        cbuf_16_slot_1: fs_cbuf_16_slot_1(blend_op_from_raw(combiner.texture1_color_blend)),
        cbuf_16_slot_2: fs_cbuf_16_slot_2(
            blend_op_from_raw(combiner.texture2_color_blend),
            table_threshold,
        ),
        cbuf_16_slot_3: fs_cbuf_16_slot_3(blend_op_from_raw(combiner.primitive_color_blend)),
    }
}

/// Apply combiner to sampled color/alpha keyframes (CPU particle sim).
pub fn combine_particle_rgba(
    c0: [f32; 4],
    c1: [f32; 4],
    a0: f32,
    a1: f32,
    combiner: &CombinerState,
) -> [f32; 4] {
    if !combiner_is_configured(combiner) {
        return [
            (c0[0] * c1[0]).clamp(0.0, 1.0),
            (c0[1] * c1[1]).clamp(0.0, 1.0),
            (c0[2] * c1[2]).clamp(0.0, 1.0),
            (a0 * a1).clamp(0.0, 1.0),
        ];
    }
    let rgb = combine_color_rgb(c0, c1, combiner.color_combiner_process);
    let alpha = combine_alpha(a0, a1, combiner.alpha_combiner_process);
    [rgb[0], rgb[1], rgb[2], alpha]
}

fn combine_color_rgb(c0: [f32; 4], c1: [f32; 4], process: u32) -> [f32; 3] {
    match process {
        0 => [c0[0], c0[1], c0[2]],
        1 => [
            c0[0].clamp(0.0, 1.0),
            c0[1].clamp(0.0, 1.0),
            c0[2].clamp(0.0, 1.0),
        ],
        2 | 3 => {
            let t = 0.5;
            let r = c0[0] * (1.0 - t) + c1[0] * t;
            let g = c0[1] * (1.0 - t) + c1[1] * t;
            let b = c0[2] * (1.0 - t) + c1[2] * t;
            [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
        }
        _ => [
            (c0[0] * c1[0]).clamp(0.0, 1.0),
            (c0[1] * c1[1]).clamp(0.0, 1.0),
            (c0[2] * c1[2]).clamp(0.0, 1.0),
        ],
    }
}

fn combine_alpha(a0: f32, a1: f32, process: u32) -> f32 {
    match process {
        0 => a0,
        1 => a0,
        2 | 3 => (a0 * 0.5 + a1 * 0.5).clamp(0.0, 1.0),
        _ => (a0 * a1).clamp(0.0, 1.0),
    }
}

pub fn apply_blend_op(a: f32, b: f32, op: CombinerBlendOp) -> f32 {
    match op {
        CombinerBlendOp::Modulate => a * b,
        CombinerBlendOp::Add => (a + b).clamp(0.0, 1.0),
        CombinerBlendOp::Subtract => (a - b).clamp(0.0, 1.0),
    }
}

/// True when emitter JSON carries an explicit combiner configuration.
pub fn combiner_is_configured(c: &CombinerState) -> bool {
    c.color_combiner_process != 0
        || c.alpha_combiner_process != 0
        || c.texture1_color_blend != 0
        || c.texture2_color_blend != 0
        || c.primitive_color_blend != 0
        || c.texture1_alpha_blend != 0
        || c.texture2_alpha_blend != 0
        || c.primitive_alpha_blend != 0
        || c.tex_color0_input_type != 0
        || c.tex_color1_input_type != 0
        || c.shader_type != 0
        || c.apply_alpha != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_combiner_is_identity() {
        let coeffs = eval_combiner_gpu_coeffs(&CombinerState::default());
        assert_eq!(coeffs.cbuf_8_slot_6, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(coeffs.cbuf_16_slot_1, [1.0, 0.0, 1.0, 1.0]);
        assert_eq!(coeffs.cbuf_16_slot_2[1], 0.0, "modulate must not add bias via .y");
        assert_eq!(coeffs.cbuf_16_slot_2[2], 1.0, "process 0/1 skips lerp branch");
    }

    #[test]
    fn test_color0_only_process() {
        let mut c = CombinerState::default();
        c.color_combiner_process = 0;
        c.shader_type = 1; // mark configured so process 0 = Color0 only
        let coeffs = eval_combiner_gpu_coeffs(&c);
        assert_eq!(coeffs.cbuf_8_slot_7[0], 0.0);
        let rgba = combine_particle_rgba(
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            0.8,
            0.2,
            &c,
        );
        assert!((rgba[0] - 1.0).abs() < 0.001);
        assert!((rgba[3] - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_modulate_process() {
        let rgba = combine_particle_rgba(
            [1.0, 0.5, 0.25, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            0.8,
            0.5,
            &CombinerState::default(),
        );
        assert!((rgba[0] - 0.5).abs() < 0.001);
        assert!((rgba[3] - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_configured_modulate_process() {
        let mut c = CombinerState::default();
        c.color_combiner_process = 1;
        c.alpha_combiner_process = 1;
        let rgba = combine_particle_rgba(
            [1.0, 0.5, 0.25, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            0.8,
            0.5,
            &c,
        );
        assert!((rgba[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_add_blend_op_coeff() {
        let c = fs_cbuf_16_slot_1(CombinerBlendOp::Add);
        assert_eq!(c[3], 0.0);
        assert!((apply_blend_op(0.3, 0.4, CombinerBlendOp::Add) - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_process2_sets_cbuf_16_lerp_threshold() {
        let mut c = CombinerState::default();
        c.color_combiner_process = 2;
        c.shader_type = 1;
        let coeffs = eval_combiner_gpu_coeffs(&c);
        assert!((coeffs.cbuf_16_slot_2[2] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_process3_enables_additive_table_branch() {
        let mut c = CombinerState::default();
        c.color_combiner_process = 3;
        c.shader_type = 1;
        let coeffs = eval_combiner_gpu_coeffs(&c);
        assert_eq!(coeffs.cbuf_16_slot_2[2], 0.0);
    }

    #[test]
    fn test_process1_color0_times_texture_cpu_path() {
        let mut c = CombinerState::default();
        c.color_combiner_process = 1;
        let rgba = combine_particle_rgba(
            [1.0, 0.5, 0.25, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            0.8,
            0.3,
            &c,
        );
        assert!((rgba[0] - 1.0).abs() < 0.001, "process 1 uses colour0 only on CPU");
        assert!((rgba[3] - 0.8).abs() < 0.001);
    }
}
