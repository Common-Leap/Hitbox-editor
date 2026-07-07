//! Per-frame, per-emitter particle trace for one effect handle (static bone).
//! Usage: probe_bomb <eff> <handle> [max_frame]

use glam::{Mat4, Vec3};
use hitbox_editor::effects::{acmd_spawn_window, EffIndex, ParticleSystem, PtclFile};
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    // Register set names as handles (common sets like P_CmnBombMain1 have no eff handle).
    for (i, set) in ptcl.emitter_sets.iter().enumerate() {
        eff.handles.entry(set.name.clone()).or_insert(i as i32);
        eff.handles.entry(set.name.to_lowercase()).or_insert(i as i32);
    }
    let handle = args[1].clone();
    let max_frame: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);

    let (start, end) = acmd_spawn_window(&handle, 4, 4, &eff, &ptcl);
    eprintln!("spawn window: {start}..{end}");
    let mut sys = ParticleSystem::default();
    sys.spawn_effect(&handle, "Trans", Vec3::ZERO, Vec3::ZERO, start, end, &eff, &ptcl);

    let set_idx = *eff.handles.get(&handle).unwrap() as usize;
    let names: Vec<String> = ptcl.emitter_sets[set_idx]
        .emitters
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();
    for f in 0..=max_frame {
        sys.step(f as f32, &bone, &ptcl);
        sys.particles.retain(|p| !p.is_dead());
        // per-emitter counts
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for p in &sys.particles {
            *counts.entry(p.emitter_idx).or_default() += 1;
        }
        if sys.particles.is_empty() {
            println!("f{f:2}: 0 particles");
            continue;
        }
        let mut line = format!("f{f:2}: {} particles | ", sys.particles.len());
        let mut keys: Vec<usize> = counts.keys().copied().collect();
        keys.sort();
        for k in keys {
            line += &format!("{}={} ", names.get(k).map(|s| s.as_str()).unwrap_or("?"), counts[&k]);
        }
        println!("{line}");
    }

    // Detail dump at a representative mid-explosion frame.
    println!("\n--- sample particles at f{max_frame} ---");
    for p in sys.particles.iter().take(20) {
        println!(
            "  em[{:2}]{:<16} pos=({:6.2},{:6.2},{:6.2}) size={:7.3} col=({:.2},{:.2},{:.2},{:.2}) age={:.1}/{:.1}",
            p.emitter_idx,
            names.get(p.emitter_idx).map(|s| s.as_str()).unwrap_or("?"),
            p.position.x, p.position.y, p.position.z, p.size,
            p.color.x, p.color.y, p.color.z, p.color.w,
            p.age, p.lifetime
        );
    }
}
