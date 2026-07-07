//! Convert Ryujinx EffectCapture dumps (see tools/ryujinx-capture/) into harness goldens.
//!
//! Three modes:
//!
//! ```text
//! # Summarize a capture session (find the particle draws worth converting):
//! cargo +nightly-2026-02-14 run --example ryujinx_to_goldens -- list <capture_dir>
//!
//! # Turn one captured draw into a cbuf golden for tests/cbuf_golden.rs:
//! cargo +nightly-2026-02-14 run --example ryujinx_to_goldens -- cbuf <draw.json> \
//!     --fighter samus --emitter-set 0 --emitter 0 [--life-t 0.5] [--tolerance 0.002] \
//!     [--exclude cbuf_9:44,cbuf_9:45] [--out <name>] [--full]
//!
//! # Convert raw .rgba frame dumps to PNGs (visual-tier goldens / eyeballing):
//! cargo +nightly-2026-02-14 run --example ryujinx_to_goldens -- frames <capture_dir> [--bgra] [--out <dir>]
//! ```
//!
//! `cbuf` mode only asserts slots that BOTH the capture and our local `nvn_chain` builders
//! populate — captured slots we don't model yet are reference material, not assertions
//! (`--full` writes them all to `tests/goldens/cbuf/ref/` for reverse-engineering; that
//! subdirectory is not scanned by the test). The golden is written with identity view_proj
//! and a default camera: capture with the training-stage default camera, or hand-edit the
//! `view_proj`/`camera` fields afterwards, and `--exclude` any camera-dependent slots that
//! can't be reproduced headlessly.
//!
//! Correlating a draw with an (emitter_set, emitter) is manual in v1: use `list`, the frame
//! window you captured, and the fact that one emitter = one draw with a stable vs/fs hash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};

const STAGE_FOR_CBUF: &[(&str, &str)] = &[
    ("cbuf_8", "vs_cbufs"),
    ("cbuf_9", "vs_cbufs"),
    ("cbuf_10", "vs_cbufs"),
    ("cbuf_16", "fs_cbufs"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[1..];
    let code = match mode {
        "list" => mode_list(rest),
        "cbuf" => mode_cbuf(rest),
        "frames" => mode_frames(rest),
        _ => {
            eprintln!("usage: ryujinx_to_goldens <list|cbuf|frames> ... (see module docs)");
            2
        }
    };
    std::process::exit(code);
}

fn flag_value<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1))
        .map(String::as_str)
}

fn has_flag(rest: &[String], name: &str) -> bool {
    rest.iter().any(|a| a == name)
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Raw cbuf bytes → slot index → float4 (16 bytes per slot, little-endian f32).
fn bytes_to_slots(bytes: &[u8]) -> BTreeMap<u64, [f32; 4]> {
    let mut slots = BTreeMap::new();
    for (i, chunk) in bytes.chunks_exact(16).enumerate() {
        let mut v = [0f32; 4];
        for (j, b) in chunk.chunks_exact(4).enumerate() {
            v[j] = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        slots.insert(i as u64, v);
    }
    slots
}

/// Extract our four named cbufs from a draw JSON, preferring the stage the shader
/// actually reads them from and falling back to the other stage.
fn extract_cbufs(draw: &serde_json::Value) -> BTreeMap<String, BTreeMap<u64, [f32; 4]>> {
    let mut out = BTreeMap::new();
    for (name, primary_stage) in STAGE_FOR_CBUF {
        let bank = name.trim_start_matches("cbuf_");
        let other = if *primary_stage == "vs_cbufs" { "fs_cbufs" } else { "vs_cbufs" };
        let hex = draw
            .get(primary_stage)
            .and_then(|m| m.get(bank))
            .or_else(|| draw.get(other).and_then(|m| m.get(bank)))
            .and_then(|v| v.as_str());
        if let Some(bytes) = hex.and_then(hex_to_bytes) {
            out.insert(name.to_string(), bytes_to_slots(&bytes));
        }
    }
    out
}

fn draw_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("draws"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    files.sort();
    files
}

fn mode_list(rest: &[String]) -> i32 {
    let Some(dir) = rest.first().map(PathBuf::from) else {
        eprintln!("usage: ryujinx_to_goldens list <capture_dir>");
        return 2;
    };
    let files = draw_files(&dir);
    if files.is_empty() {
        eprintln!("no draw dumps under {}/draws", dir.display());
        return 1;
    }
    println!("{:<8} {:<6} {:<18} {:<18} banks", "frame", "draw", "vs_hash", "fs_hash");
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let cbufs = extract_cbufs(&v);
        let banks: Vec<&str> = cbufs.keys().map(String::as_str).collect();
        println!(
            "{:<8} {:<6} {:<18} {:<18} {}",
            v.get("frame").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("draw").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("vs_hash").and_then(|x| x.as_str()).unwrap_or("?"),
            v.get("fs_hash").and_then(|x| x.as_str()).unwrap_or("?"),
            banks.join(",")
        );
    }
    println!("{} draw dump(s)", files.len());
    0
}

fn mode_cbuf(rest: &[String]) -> i32 {
    let Some(draw_path) = rest.first().map(PathBuf::from) else {
        eprintln!("usage: ryujinx_to_goldens cbuf <draw.json> --fighter <name> [flags]");
        return 2;
    };
    let fighter = flag_value(rest, "--fighter").unwrap_or("samus").to_string();
    let emitter_set: usize = flag_value(rest, "--emitter-set").and_then(|s| s.parse().ok()).unwrap_or(0);
    let emitter_idx: usize = flag_value(rest, "--emitter").and_then(|s| s.parse().ok()).unwrap_or(0);
    let life_t: f32 = flag_value(rest, "--life-t").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let tolerance: f32 = flag_value(rest, "--tolerance").and_then(|s| s.parse().ok()).unwrap_or(2e-3);
    let full = has_flag(rest, "--full");

    // --exclude cbuf_9:44,cbuf_9:45,cbuf_8:6
    let mut excluded: Vec<(String, u64)> = Vec::new();
    if let Some(list) = flag_value(rest, "--exclude") {
        for item in list.split(',') {
            if let Some((name, slot)) = item.split_once(':') {
                if let Ok(slot) = slot.parse() {
                    excluded.push((name.to_string(), slot));
                    continue;
                }
            }
            eprintln!("bad --exclude entry '{item}' (want cbuf_N:slot)");
            return 2;
        }
    }

    let Ok(text) = std::fs::read_to_string(&draw_path) else {
        eprintln!("cannot read {}", draw_path.display());
        return 1;
    };
    let Ok(draw) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("{} is not valid JSON", draw_path.display());
        return 1;
    };
    let captured = extract_cbufs(&draw);
    if captured.is_empty() {
        eprintln!("no vs_cbufs/fs_cbufs banks 8/9/10/16 in {}", draw_path.display());
        return 1;
    }

    // Load the emitter and build our local cbufs; only their slot set is assertable.
    let Some(eff_path) = hitbox_editor::scratch_dirs::resolve_fighter_eff(&fighter) else {
        eprintln!("no effect export for fighter '{fighter}' (set data_root or HITBOX_EFFECT_EXPORT)");
        return 1;
    };
    let Ok(eff) = hitbox_editor::effects::EffIndex::from_file(&eff_path) else {
        eprintln!("failed to parse {}", eff_path.display());
        return 1;
    };
    let Ok(ptcl) = hitbox_editor::effects::PtclFile::parse(&eff.ptcl_data) else {
        eprintln!("failed to parse PTCL for {fighter}");
        return 1;
    };
    let Some(emitter) = ptcl
        .emitter_sets
        .get(emitter_set)
        .and_then(|s| s.emitters.get(emitter_idx))
    else {
        eprintln!("emitter ({emitter_set},{emitter_idx}) out of range for {fighter}");
        return 1;
    };

    // Same defaults tests/cbuf_golden.rs uses when the golden omits camera/view_proj.
    let view_proj = Mat4::IDENTITY;
    let (right, up, aspect) = (Vec3::X, Vec3::Y, 1.0);
    let local: BTreeMap<&str, hitbox_editor::nvn_chain::NvnBufferData> = {
        use hitbox_editor::nvn_chain::*;
        BTreeMap::from([
            ("cbuf_8", build_cbuf_8(emitter, life_t, &view_proj)),
            ("cbuf_9", build_cbuf_9(emitter, &view_proj, None, right, up, aspect)),
            ("cbuf_10", build_cbuf_10(emitter)),
            ("cbuf_16", build_cbuf_16(emitter, life_t)),
        ])
    };

    let mut golden_cbufs: BTreeMap<String, BTreeMap<String, [f32; 4]>> = BTreeMap::new();
    let mut asserted = 0usize;
    let mut skipped_unmodeled = 0usize;
    for (name, cap_slots) in &captured {
        let Some(local_data) = local.get(name.as_str()) else { continue };
        let mut slots = BTreeMap::new();
        for (slot, value) in cap_slots {
            if excluded.iter().any(|(n, s)| n == name && s == slot) {
                continue;
            }
            if local_data.slot_data.contains_key(slot) {
                slots.insert(slot.to_string(), *value);
                asserted += 1;
            } else {
                skipped_unmodeled += 1;
            }
        }
        if !slots.is_empty() {
            golden_cbufs.insert(name.clone(), slots);
        }
    }

    if golden_cbufs.is_empty() {
        eprintln!("no overlap between captured slots and locally-built slots — nothing to assert");
        eprintln!("(use --full to still write the reference dump)");
        if !full {
            return 1;
        }
    }

    let stem = flag_value(rest, "--out").map(String::from).unwrap_or_else(|| {
        let frame = draw.get("frame").and_then(|x| x.as_u64()).unwrap_or(0);
        let d = draw.get("draw").and_then(|x| x.as_u64()).unwrap_or(0);
        format!("{fighter}_s{emitter_set}e{emitter_idx}_f{frame:06}d{d:04}")
    });
    let golden_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/cbuf");

    if !golden_cbufs.is_empty() {
        let golden = serde_json::json!({
            "fighter": fighter,
            "emitter_set": emitter_set,
            "emitter": emitter_idx,
            "particle_life_t": life_t,
            "camera": { "right": [1.0, 0.0, 0.0], "up": [0.0, 1.0, 0.0], "aspect": 1.0 },
            "tolerance": tolerance,
            "cbufs": golden_cbufs,
        });
        std::fs::create_dir_all(&golden_dir).ok();
        let out = golden_dir.join(format!("{stem}.json"));
        if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&golden).unwrap()) {
            eprintln!("write failed: {e}");
            return 1;
        }
        println!(
            "wrote {} ({asserted} asserted slot(s), {skipped_unmodeled} captured-but-unmodeled skipped)",
            out.display()
        );
        println!("run: cargo +nightly-2026-02-14 test --test cbuf_golden -- --test-threads=1");
    }

    if full {
        let ref_dir = golden_dir.join("ref");
        std::fs::create_dir_all(&ref_dir).ok();
        let all: BTreeMap<&String, BTreeMap<String, [f32; 4]>> = captured
            .iter()
            .map(|(n, s)| (n, s.iter().map(|(k, v)| (k.to_string(), *v)).collect()))
            .collect();
        let out = ref_dir.join(format!("{stem}.json"));
        if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&all).unwrap()) {
            eprintln!("ref write failed: {e}");
            return 1;
        }
        println!("wrote {} (full slot dump, not asserted)", out.display());
    }
    0
}

fn mode_frames(rest: &[String]) -> i32 {
    let Some(dir) = rest.first().map(PathBuf::from) else {
        eprintln!("usage: ryujinx_to_goldens frames <capture_dir> [--bgra] [--out <dir>]");
        return 2;
    };
    let bgra = has_flag(rest, "--bgra");
    let out_dir = flag_value(rest, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("png"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("frames"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "rgba").unwrap_or(false))
        .collect();
    files.sort();
    if files.is_empty() {
        eprintln!("no .rgba dumps under {}/frames", dir.display());
        return 1;
    }
    std::fs::create_dir_all(&out_dir).ok();

    let mut written = 0usize;
    for path in &files {
        // frame_000123_1280x720.rgba
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let Some(dims) = stem.rsplit('_').next() else { continue };
        let Some((w, h)) = dims.split_once('x') else {
            eprintln!("skip {stem}: no WxH suffix");
            continue;
        };
        let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) else { continue };
        let Ok(mut data) = std::fs::read(path) else { continue };
        if data.len() != (w * h * 4) as usize {
            eprintln!("skip {stem}: {} bytes, expected {}", data.len(), w * h * 4);
            continue;
        }
        if bgra {
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        let out = out_dir.join(format!("{stem}.png"));
        match image::save_buffer(&out, &data, w, h, image::ColorType::Rgba8) {
            Ok(()) => written += 1,
            Err(e) => eprintln!("skip {stem}: png encode failed: {e}"),
        }
    }
    println!("wrote {written} PNG(s) to {}", out_dir.display());
    if written > 0 { 0 } else { 1 }
}
