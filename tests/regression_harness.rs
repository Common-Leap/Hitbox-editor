//! Phase 0 visual regression tier (see `src/regression.rs`).
//!
//! For each (fighter, handle, frame) with a golden PNG under
//! `tests/goldens/<handle>/frame_NN.png`, render the effect headlessly and diff it. Semantics:
//!   * No GPU / no game data / no matching handle → skip cleanly (never fails CI).
//!   * Golden missing → skip that frame, unless `UPDATE_GOLDENS=1` (then write it as a baseline).
//!   * Golden present → assert the render is within thresholds; on failure dump artifacts to the
//!     workshop tmp dir (`regression/<handle>/frame_NN/`).
//!
//! Goldens are gitignored and user-supplied — either a previously-approved editor render
//! (pure regression) or a framing-matched real game frame (accuracy).
//!
//! Run serialized: `cargo +nightly-2026-02-14 test --test regression_harness -- --test-threads=1`.

use hitbox_editor::regression::{
    create_headless_device, diff_images, golden_path, load_png_rgba, save_png,
    write_diff_artifacts, Camera, EffectHarness,
};

/// (fighter, candidate handles in priority order, content-ful frames spanning birth→mid→death).
///
/// Rendering is deterministic across process launches after the shader-registry ordering fix
/// (see the `renderer-nondeterminism` note), so spawn-instant (f0) and heavy-smoke frames are
/// safe to include. Empty/background-only frames are skipped (they can't catch particle changes).
const CASES: &[(&str, &[&str], &[u32])] = &[
    ("samus", &["samus_atk_bomb", "samus_cshot_bomb"], &[0, 8, 24, 48]),
    ("mario", &["mario_pump_hit", "mario_fb_bullet_l"], &[0, 65]),
];

fn env_u8(key: &str, default: u8) -> u8 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Rendering the same frame twice on one reused harness must be byte-identical. Catches
/// readback races / reused-state nondeterminism (single-process, so no HashMap-seed variance).
#[test]
fn render_is_deterministic_across_calls() {
    let Some((device, queue)) = create_headless_device() else {
        eprintln!("[regression] no GPU adapter — skipping");
        return;
    };
    let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        eprintln!("[regression] no samus export — skipping");
        return;
    };
    let Some(mut harness) = EffectHarness::load(&device, &queue, &eff_path) else {
        eprintln!("[regression] failed to load samus — skipping");
        return;
    };
    let handle = ["samus_atk_bomb", "samus_cshot_bomb"]
        .into_iter()
        .find(|h| harness.handles().any(|k| k.as_str() == *h));
    let Some(handle) = handle else {
        eprintln!("[regression] no bomb handle — skipping");
        return;
    };
    let cam = Camera::default();
    let a = harness.render_frame(handle, 8, cam);
    let b = harness.render_frame(handle, 8, cam);
    let c = harness.render_frame(handle, 24, cam);
    let d = harness.render_frame(handle, 8, cam);
    assert_eq!(a, b, "same frame rendered twice in a row differs (readback race?)");
    assert_eq!(a, d, "same frame differs after an intervening different frame (state bleed)");
    let _ = c;
}

#[test]
fn regression_effect_frames() {
    let Some((device, queue)) = create_headless_device() else {
        eprintln!("[regression] no GPU adapter — skipping");
        return;
    };
    let update = std::env::var("UPDATE_GOLDENS").is_ok();
    let max_delta = env_u8("REGRESSION_MAX_DELTA", 8);
    let rmse_lim = env_f64("REGRESSION_RMSE", 2.5);
    let cam = Camera::default();

    let mut checked = 0usize;
    let mut wrote = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for &(fighter, handles, frames) in CASES {
        let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff(fighter) else {
            eprintln!("[regression] {fighter}: no effect export — skipping");
            continue;
        };
        let Some(mut harness) = EffectHarness::load(&device, &queue, &eff_path) else {
            eprintln!("[regression] {fighter}: failed to load {} — skipping", eff_path.display());
            continue;
        };
        let handle = handles
            .iter()
            .copied()
            .find(|h| harness.handles().any(|k| k.as_str() == *h));
        let Some(handle) = handle else {
            eprintln!("[regression] {fighter}: none of {handles:?} present — skipping");
            continue;
        };

        for &frame in frames {
            let actual = harness.render_frame(handle, frame, cam);
            let gpath = golden_path(handle, frame);
            match load_png_rgba(&gpath) {
                None => {
                    if update {
                        match save_png(&gpath, &actual) {
                            Ok(()) => {
                                wrote += 1;
                                eprintln!("[regression] wrote baseline {}", gpath.display());
                            }
                            Err(e) => eprintln!("[regression] save {} failed: {e}", gpath.display()),
                        }
                    } else {
                        eprintln!(
                            "[regression] {handle} f{frame}: no golden ({}) — skip (UPDATE_GOLDENS=1 to seed)",
                            gpath.display()
                        );
                    }
                }
                Some(golden) => {
                    let report = diff_images(&actual, &golden);
                    checked += 1;
                    let ok = report.within(max_delta, rmse_lim);
                    eprintln!(
                        "[regression] {handle} f{frame}: {} [{}]",
                        report.summary(),
                        if ok { "OK" } else { "FAIL" }
                    );
                    if !ok {
                        let dir = hitbox_editor::scratch_dirs::workshop_tmp_path(&format!(
                            "regression/{handle}/frame_{frame:02}"
                        ));
                        let _ = write_diff_artifacts(&dir, &actual, &golden, &report);
                        failures.push(format!(
                            "{handle} f{frame}: {} (artifacts: {})",
                            report.summary(),
                            dir.display()
                        ));
                    }
                }
            }
        }
    }

    if checked == 0 && wrote == 0 {
        eprintln!("[regression] no goldens available — nothing asserted (skipped)");
        return;
    }
    assert!(
        failures.is_empty(),
        "regression failures:\n{}",
        failures.join("\n")
    );
}

/// Guards the frame-clock life-feed fix (task #22 / native life chain). The Samus bomb's
/// fire/smoke/flare emitters run the game's native VS/FS age chain
/// (`age = cbuf_10[2].x - attr<birth>.w; cull when age >= trunc(attr<life>.w)`). Feeding
/// the legacy normalized `life_t` as birth with `clock = 1.0` culls every fragment whose
/// `life_t > ~0`, so at a mid-life frame the whole explosion was invisible (0 px). With the
/// frame-clock feed the explosion renders in full. Regressing the feed drops this to ~0.
#[test]
fn frame_clock_life_feed_renders_bomb_midlife() {
    let Some((device, queue)) = create_headless_device() else {
        eprintln!("no GPU — skipping");
        return;
    };
    let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff("samus") else {
        eprintln!("no samus effect export — skipping");
        return;
    };
    let Some(harness) = EffectHarness::load(&device, &queue, &eff_path) else {
        eprintln!("failed to load samus eff — skipping");
        return;
    };
    // Frame 10: the bomb explosion is mid-life (particles well past life_t=0). Under the
    // pre-fix normalized feed this rendered 0 visible pixels; the frame-clock feed restores
    // the full explosion (tens of thousands of visible px on a 256x256 target).
    let pixels = harness.render_frame("samus_atk_bomb", 10, Camera::default());
    let visible = hitbox_editor::regression::visible_pixels(&pixels);
    eprintln!("[frame-clock] samus_atk_bomb f10 visible px = {visible}");
    // Pre-fix (normalized life feed) this rendered 0 visible px. The correct explosion is a
    // few thousand px (soft-alpha smoke/fire after the BC5-swizzle fix, no longer the bogus
    // opaque white smoke band that used to inflate this to ~39k). Threshold well above 0.
    assert!(
        visible > 2_000,
        "mid-life bomb should render the explosion, got only {visible} visible px \
         (frame-clock life feed regressed? see fx_frame_clock_enabled + append_bnsh_particle_vertices)"
    );
}
