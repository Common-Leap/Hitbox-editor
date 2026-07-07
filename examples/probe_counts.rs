//! Scratch probe: per-frame particle state for one effect handle.
//! Usage: probe_counts <eff> <handle> [max_frame]

use glam::{Mat4, Vec3};
use hitbox_editor::effects::{acmd_spawn_window, EffIndex, ParticleSystem, PtclFile};
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let handle = args[1].clone();
    let max_frame: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let active_start = 4u32;
    let (start, end) = acmd_spawn_window(&handle, active_start, active_start, &eff, &ptcl);
    let mut sys = ParticleSystem::default();
    sys.spawn_effect(&handle, "Trans", Vec3::ZERO, Vec3::ZERO, start, end, &eff, &ptcl);
    for f in 0..=max_frame {
        // Moving bone: stripe/arc emitters only produce geometry under motion.
        let t = f as f32 * 0.3;
        let m = Mat4::from_translation(glam::Vec3::new(t.cos() * 3.0, t.sin() * 3.0, 0.0));
        let bone: HashMap<String, Mat4> = [("Trans".to_string(), m)].into();
        sys.step(f as f32, &bone, &ptcl);
        sys.particles.retain(|p| !p.is_dead());
    }
    println!("f{max_frame}: {} particles, {} active emitter instances", sys.particles.len(), sys.active_emitters.len());
    for (i, p) in sys.particles.iter().enumerate().take(12) {
        println!(
            "  p{i}: em=({},{}) pos=({:.2},{:.2},{:.2}) size={:.3} color=({:.2},{:.2},{:.2},{:.3}) age={:.0}/{:.0}",
            p.emitter_set_idx, p.emitter_idx,
            p.position.x, p.position.y, p.position.z, p.size,
            p.color.x, p.color.y, p.color.z, p.color.w,
            p.age, p.lifetime
        );
    }
}
