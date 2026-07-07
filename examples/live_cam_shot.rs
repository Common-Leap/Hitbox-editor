//! Render an effect with the *live editor* camera (renderer.rs Camera::default:
//! translation (0,-8,-60), rotation (0, PI/2, 0), fov 30deg, aspect 1400/900) to see what
//! the actual viewport shows — vs the harness's framed_origin camera.
//! Usage: live_cam_shot <fighter> <handle> <frame> [out.png]

use glam::{Mat4, Vec3};
use hitbox_editor::regression::{create_headless_device, save_png, visible_pixels, Camera, EffectHarness};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let fighter = a.first().cloned().unwrap_or_else(|| "samus".into());
    let handle = a.get(1).cloned().unwrap_or_else(|| "samus_atk_bomb".into());
    let frame: u32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let out = a.get(3).cloned().unwrap_or_else(|| "/tmp/live_cam.png".into());

    // Live editor camera (renderer.rs).
    let translation = Vec3::new(0.0, -8.0, -60.0);
    let rotation = Mat4::from_euler(glam::EulerRot::XYZ, 0.0, std::f32::consts::FRAC_PI_2, 0.0);
    let model_view = Mat4::from_translation(translation) * rotation;
    let aspect = 1400.0 / 900.0;
    let proj = Mat4::perspective_rh(30f32.to_radians(), aspect, 1.0, 400_000.0);
    // Match the live app's camera_vectors(): basis from the INVERSE model-view (camera world
    // right/up), normalized.
    let mv_inv = model_view.inverse();
    let cam = Camera {
        view_proj: proj * model_view,
        right: mv_inv.col(0).truncate().normalize(),
        up: mv_inv.col(1).truncate().normalize(),
    };

    let Some((device, queue)) = create_headless_device() else {
        eprintln!("no GPU");
        std::process::exit(1);
    };
    // A fighter name resolves via the data root; a path (contains '/') is used directly.
    let eff = if fighter.contains('/') {
        std::path::PathBuf::from(&fighter)
    } else {
        match hitbox_editor::scratch_dirs::resolve_fighter_eff(&fighter) {
            Some(p) => p,
            None => {
                eprintln!("no eff for {fighter}");
                std::process::exit(1);
            }
        }
    };
    let mut idx = hitbox_editor::effects::EffIndex::from_file(&eff).expect("eff parse");
    let mut ptcl = hitbox_editor::effects::PtclFile::parse(&idx.ptcl_data).expect("ptcl parse");
    // FX_MERGE_COMMON=1: merge ef_common.eff like the live app does, to reproduce the
    // live viewport's merged-PTCL state in the harness.
    if std::env::var("FX_MERGE_COMMON").is_ok() {
        let common = eff
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.join("system/common/ef_common.eff"))
            .filter(|p| p.exists())
            .expect("ef_common.eff not found next to fighter effects");
        idx.merge_from_file_with_ptcl(&common, &mut ptcl).expect("merge ef_common");
        eprintln!("[live_cam_shot] merged ef_common: {} sets", ptcl.emitter_sets.len());
    }
    // Register set names as handles so sets without usable eff handles (common sets like
    // P_CmnBombMain1, whose SYS_* handles parse with set idx -1) are spawnable by name.
    for (i, set) in ptcl.emitter_sets.iter().enumerate() {
        idx.handles.entry(set.name.clone()).or_insert(i as i32);
        idx.handles.entry(set.name.to_lowercase()).or_insert(i as i32);
    }
    let source_name = eff.file_name().and_then(|s| s.to_str()).unwrap_or("effect.eff").to_string();
    let Some(harness) = EffectHarness::from_parts(&device, &queue, idx, ptcl, &source_name) else {
        eprintln!("load failed");
        std::process::exit(1);
    };
    {
        let c = cam.view_proj.to_cols_array_2d();
        eprintln!("[HARNESS-VP] c0={:.3?} c1={:.3?} c2={:.3?} c3={:.3?}", c[0], c[1], c[2], c[3]);
    }
    let pixels = harness.render_frame(&handle, frame, cam);
    let vis = visible_pixels(&pixels);
    save_png(std::path::Path::new(&out), &pixels).expect("save");
    println!("wrote {out} ({vis} visible px, live camera)");
}
