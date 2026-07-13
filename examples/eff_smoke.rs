// Smoke test: parse a real ef_*.eff the way eff_editor.rs does and print entries + hash40s.
fn main() -> anyhow::Result<()> {
    let path = std::path::Path::new(
        "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/mario/ef_mario.eff",
    );
    let index = hitbox_editor::effects::EffIndex::from_file(path)?;
    let ptcl = hitbox_editor::effects::PtclFile::parse(&index.ptcl_data)?;
    println!("sets: {}", ptcl.emitter_sets.len());
    let mut seen = std::collections::HashSet::new();
    let mut n = 0;
    for (name, set_idx) in &index.handles {
        let lower = name.to_lowercase();
        if !seen.insert(lower.clone()) { continue; }
        if *set_idx < 0 || *set_idx as usize >= ptcl.emitter_sets.len() { continue; }
        n += 1;
        if n <= 8 {
            let set = &ptcl.emitter_sets[*set_idx as usize];
            let em0 = set.emitters.first();
            println!(
                "  {lower}  hash 0x{:010x}  set {}  emitters {}  scale0 {:?}  c0keys {:?}",
                hash40::hash40(&lower).0, set_idx, set.emitters.len(),
                em0.map(|e| e.scale), em0.map(|e| e.color0.len()),
            );
        }
    }
    println!("entries: {n}");
    // Known-good cross-check: sys_flyroll_smoke must hash to what the plugin showed.
    println!("sys_flyroll_smoke = 0x{:010x}", hash40::hash40("sys_flyroll_smoke").0);
    Ok(())
}
