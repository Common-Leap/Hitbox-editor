//! Phase 0 numerical (cbuf) golden tier — the higher-fidelity half of the capture-diff harness
//! (see `src/regression.rs` and the plan). It validates that `nvn_chain` reproduces the game's
//! NVN constant-buffer values slot-for-slot.
//!
//! Ground truth is **captured cbuf snapshots** (from hardware / GPU traces) dropped into
//! `tests/goldens/cbuf/*.json` (gitignored, user-supplied). Without captures the test skips
//! cleanly. This tier is what makes the Phase-3 deep items (cbuf_9[46/47] position roles,
//! combiner 1:1, colour-table fill) verifiable — it turns "looks right" into a slot diff.
//!
//! Capture format (one JSON object per file):
//! ```json
//! {
//!   "fighter": "samus",
//!   "emitter_set": 0,
//!   "emitter": 0,
//!   "particle_life_t": 0.5,
//!   "camera": { "right": [1.0, 0.0, 0.0], "up": [0.0, 1.0, 0.0], "aspect": 1.0 },
//!   "view_proj": [[..],[..],[..],[..]],   // column-major 4x4; optional (identity if absent)
//!   "tolerance": 0.002,                    // optional (default 2e-3)
//!   "cbufs": {
//!     "cbuf_9":  { "60": [1.0, 0.0, 0.0, 1.0] },
//!     "cbuf_16": { "1":  [1.0, 0.0, 1.0, 1.0] }
//!   }
//! }
//! ```
//! Supported cbuf names: `cbuf_8`, `cbuf_9`, `cbuf_10`, `cbuf_16`.
//!
//! Run: `cargo test --test cbuf_golden -- --test-threads=1`.

use std::collections::HashMap;

use glam::{Mat4, Vec3};
use serde::Deserialize;

#[derive(Deserialize, Default)]
struct CameraJson {
    right: Option<[f32; 3]>,
    up: Option<[f32; 3]>,
    aspect: Option<f32>,
}

#[derive(Deserialize)]
struct CbufCapture {
    fighter: String,
    #[serde(default)]
    emitter_set: usize,
    #[serde(default)]
    emitter: usize,
    #[serde(default)]
    particle_life_t: f32,
    #[serde(default)]
    camera: CameraJson,
    /// Column-major 4x4 view-projection used at capture time (identity if absent).
    view_proj: Option<[[f32; 4]; 4]>,
    tolerance: Option<f32>,
    /// cbuf name → (slot index string → expected float4).
    cbufs: HashMap<String, HashMap<String, [f32; 4]>>,
}

fn cbuf_golden_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/cbuf")
}

fn build_cbuf(
    name: &str,
    emitter: &hitbox_editor::effects::EmitterDef,
    cap: &CbufCapture,
    view_proj: &Mat4,
) -> Option<hitbox_editor::nvn_chain::NvnBufferData> {
    use hitbox_editor::nvn_chain::*;
    let right = Vec3::from(cap.camera.right.unwrap_or([1.0, 0.0, 0.0]));
    let up = Vec3::from(cap.camera.up.unwrap_or([0.0, 1.0, 0.0]));
    let aspect = cap.camera.aspect.unwrap_or(1.0);
    Some(match name {
        "cbuf_8" => build_cbuf_8(emitter, cap.particle_life_t, view_proj),
        "cbuf_9" => build_cbuf_9(emitter, view_proj, None, right, up, aspect),
        "cbuf_10" => build_cbuf_10(emitter),
        "cbuf_16" => build_cbuf_16(emitter, cap.particle_life_t),
        _ => return None,
    })
}

#[test]
fn cbuf_golden_matches_captures() {
    let dir = cbuf_golden_dir();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();

    if entries.is_empty() {
        eprintln!(
            "[cbuf-golden] no captures in {} — skipping (drop hardware cbuf snapshots there)",
            dir.display()
        );
        return;
    }

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for path in entries {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{}: read failed: {e}", path.display()));
                continue;
            }
        };
        let cap: CbufCapture = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{}: parse failed: {e}", path.display()));
                continue;
            }
        };

        let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff(&cap.fighter) else {
            eprintln!("[cbuf-golden] {}: no export for '{}' — skipping", path.display(), cap.fighter);
            continue;
        };
        let Ok(eff) = hitbox_editor::effects::EffIndex::from_file(&eff_path) else { continue };
        let Ok(ptcl) = hitbox_editor::effects::PtclFile::parse(&eff.ptcl_data) else { continue };
        let Some(emitter) = ptcl
            .emitter_sets
            .get(cap.emitter_set)
            .and_then(|s| s.emitters.get(cap.emitter))
        else {
            failures.push(format!(
                "{}: emitter ({},{}) out of range",
                path.display(),
                cap.emitter_set,
                cap.emitter
            ));
            continue;
        };

        let view_proj = cap
            .view_proj
            .map(|m| Mat4::from_cols_array_2d(&m))
            .unwrap_or(Mat4::IDENTITY);
        let tol = cap.tolerance.unwrap_or(2e-3);

        for (cbuf_name, slots) in &cap.cbufs {
            let Some(data) = build_cbuf(cbuf_name, emitter, &cap, &view_proj) else {
                failures.push(format!("{}: unsupported cbuf '{cbuf_name}'", path.display()));
                continue;
            };
            for (slot_str, expected) in slots {
                let Ok(slot) = slot_str.parse::<u64>() else {
                    failures.push(format!("{}: bad slot '{slot_str}'", path.display()));
                    continue;
                };
                let actual = data.slot_data.get(&slot).copied().unwrap_or([f32::NAN; 4]);
                checked += 1;
                let ok = (0..4).all(|i| (actual[i] - expected[i]).abs() <= tol);
                if !ok {
                    failures.push(format!(
                        "{} {cbuf_name}[{slot}]: expected {expected:?} got {actual:?}",
                        cap.fighter
                    ));
                }
            }
        }
    }

    if checked == 0 {
        eprintln!("[cbuf-golden] no slots asserted (no matching game data) — skipping");
        return;
    }
    eprintln!("[cbuf-golden] checked {checked} slot(s)");
    assert!(failures.is_empty(), "cbuf golden mismatches:\n{}", failures.join("\n"));
}
