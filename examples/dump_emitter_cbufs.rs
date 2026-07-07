//! Dump our per-emitter static cbuf slots as JSON for capture correlation.
//! Usage: dump_emitter_cbufs <eff> [set_idx]   (omit set_idx to dump every set)

use glam::{Mat4, Vec3};
use hitbox_editor::effects::{EffIndex, PtclFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let only_set: Option<usize> = args.get(1).and_then(|s| s.parse().ok());
    for (set_idx, set) in ptcl.emitter_sets.iter().enumerate() {
        if only_set.is_some_and(|s| s != set_idx) {
            continue;
        }
        dump_set(set_idx, set);
    }
}

fn dump_set(set_idx: usize, set: &hitbox_editor::effects::EmitterSet) {
    for (i, emitter) in set.emitters.iter().enumerate() {
        let c16 = hitbox_editor::nvn_chain::build_cbuf_16(emitter, 0.0);
        let c10 = hitbox_editor::nvn_chain::build_cbuf_10(emitter);
        let c9 = hitbox_editor::nvn_chain::build_cbuf_9(
            emitter,
            &Mat4::IDENTITY,
            None,
            Vec3::X,
            Vec3::Y,
            1.0,
        );
        let grab = |d: &hitbox_editor::nvn_chain::NvnBufferData, slots: &[u64]| -> Vec<serde_json::Value> {
            slots
                .iter()
                .map(|s| match d.slot_data.get(s) {
                    Some(v) => serde_json::json!({ "slot": s, "v": v }),
                    None => serde_json::json!({ "slot": s, "v": null }),
                })
                .collect()
        };
        let raw_c0: Vec<[f32; 4]> = emitter.color0.iter().map(|k| [k.frame, k.r, k.g, k.b]).collect();
        let raw_a0: Vec<[f32; 2]> = emitter.alpha0_keys.iter().map(|k| [k.frame, k.a]).collect();
        let raw_a1: Vec<[f32; 2]> = emitter.alpha1_keys.iter().map(|k| [k.frame, k.a]).collect();
        let doc = serde_json::json!({
            "set": set_idx,
            "emitter": i,
            "name": emitter.name,
            "cbuf_16": grab(&c16, &[0, 1, 2, 3, 4]),
            "cbuf_10": grab(&c10, &[0, 1, 2, 3]),
            "cbuf_9_tables": grab(&c9, &[60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75]),
            "cbuf_9_uv": grab(&c9, &[8, 10, 48, 53, 96, 97, 98, 99]),
            "scale_xy": [emitter.scale, emitter.scale * emitter.scale_aspect_y],
            "emitter_scale": emitter.emitter_scale.to_array(),
            "particle_scale_state": [
                emitter.particle_scale.enable_scaling_by_camera_dist_near as f32,
                emitter.particle_scale.enable_scaling_by_camera_dist_far as f32,
                emitter.particle_scale.scale_min,
                emitter.particle_scale.scale_max,
            ],
            "rotation_init": [emitter.rotation_init],
            "raw_color0": raw_c0,
            "raw_alpha0": raw_a0,
            "alpha0_3v4k": [emitter.alpha0.start_value, emitter.alpha0.start_diff, emitter.alpha0.end_diff, emitter.alpha0.time2, emitter.alpha0.time3],
            "raw_alpha1": raw_a1,
            "alpha1_3v4k": [emitter.alpha1.start_value, emitter.alpha1.start_diff, emitter.alpha1.end_diff, emitter.alpha1.time2, emitter.alpha1.time3],
        });
        println!("{doc}");
    }
}
