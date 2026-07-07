//! Interactive driver for the regression harness: render one effect/frame to a PNG for
//! eyeballing during iteration.
//!
//! Usage:
//!   cargo +nightly-2026-02-14 run --example regression_shot -- <fighter> <handle> <frame> [out.png]
//! Or via env (reusing the HITBOX_AUTOLOAD_* convention):
//!   HITBOX_AUTOLOAD_FIGHTER=samus HITBOX_AUTOLOAD_EFFECT=samus_atk_bomb REGRESSION_FRAME=24 \
//!     cargo +nightly-2026-02-14 run --example regression_shot
//!
//! With no out path, writes to the workshop tmp dir as shot_<handle>_fNN.png.

use hitbox_editor::regression::{create_headless_device, save_png, visible_pixels, Camera, EffectHarness};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fighter = args
        .first()
        .cloned()
        .or_else(|| std::env::var("HITBOX_AUTOLOAD_FIGHTER").ok())
        .unwrap_or_else(|| "samus".into());
    let handle = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("HITBOX_AUTOLOAD_EFFECT").ok())
        .unwrap_or_else(|| "samus_atk_bomb".into());
    let frame: u32 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("REGRESSION_FRAME").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(24);
    let out = args.get(3).cloned().unwrap_or_else(|| {
        hitbox_editor::scratch_dirs::workshop_tmp_path(&format!("shot_{handle}_f{frame:02}.png"))
            .to_string_lossy()
            .into_owned()
    });

    let Some((device, queue)) = create_headless_device() else {
        eprintln!("no GPU adapter available");
        std::process::exit(1);
    };
    let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff(&fighter) else {
        eprintln!("no effect export for fighter '{fighter}' (set data_root or HITBOX_EFFECT_EXPORT)");
        std::process::exit(1);
    };
    let Some(mut harness) = EffectHarness::load(&device, &queue, &eff_path) else {
        eprintln!("failed to load {}", eff_path.display());
        std::process::exit(1);
    };

    let pixels = harness.render_frame(&handle, frame, Camera::default());
    let visible = visible_pixels(&pixels);
    let out_path = std::path::PathBuf::from(&out);
    match save_png(&out_path, &pixels) {
        Ok(()) => println!("wrote {out} ({visible} visible px, frame {frame}, handle {handle})"),
        Err(e) => {
            eprintln!("save failed: {e}");
            std::process::exit(1);
        }
    }
}
