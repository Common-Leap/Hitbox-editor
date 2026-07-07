//! Print effect handles → emitter-set index + emitter names (for capture correlation).
//! Usage: list_sets <eff> [filter]

use hitbox_editor::effects::{EffIndex, PtclFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let filter = args.get(1).cloned().unwrap_or_default();
    let mut handles: Vec<(&String, &i32)> = eff.handles.iter().collect();
    handles.sort_by_key(|(_, idx)| **idx);
    for (handle, &set_idx) in handles {
        let set_idx = set_idx as usize;
        if !filter.is_empty() && !handle.contains(&filter) {
            continue;
        }
        let n = ptcl.emitter_sets.get(set_idx).map(|s| s.emitters.len()).unwrap_or(0);
        println!("set={set_idx:3} emitters={n} handle={handle}");
        if let Some(set) = ptcl.emitter_sets.get(set_idx) {
            for (i, e) in set.emitters.iter().enumerate() {
                println!("    [{i}] name='{}' shader_index={}", e.name, e.shader_index);
            }
        }
    }
}
