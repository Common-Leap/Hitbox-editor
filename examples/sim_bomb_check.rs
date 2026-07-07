use hitbox_editor::effects::{EffIndex, ParticleSystem, PtclFile, acmd_spawn_window};
use glam::{Mat4, Vec3};
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: sim_bomb_check <eff>");
    let eff = EffIndex::from_file(path.as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let spawn = eff
        .handles
        .keys()
        .find(|k| k.contains("bomb") || k.contains("Bomb"))
        .cloned()
        .unwrap_or_else(|| "samus_atk_bomb".to_string());
    let active_start = 4u32;
    let (start, end) = acmd_spawn_window(&spawn, active_start, active_start, &eff, &ptcl);
    eprintln!(
        "handle={spawn} emitters={} spawn_window={start}..{end}",
        ptcl.emitter_sets.len()
    );

    let mut sys = ParticleSystem::default();
    sys.spawn_effect(
        &spawn,
        "Trans",
        Vec3::ZERO,
        Vec3::ZERO,
        start,
        end,
        &eff,
        &ptcl,
    );
    eprintln!("active_emitters={}", sys.active_emitters.len());

    let bone: HashMap<String, Mat4> = [("Trans".to_string(), Mat4::IDENTITY)].into();
    for f in 0..=64u32 {
        sys.step(f as f32, &bone, &ptcl);
        sys.particles.retain(|p| !p.is_dead());
        if f <= 10 || f == 30 || f == 64 {
            eprintln!(
                "frame={f} particles={} emitters={}",
                sys.particles.len(),
                sys.active_emitters.len()
            );
        }
    }
}
