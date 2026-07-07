//! Verify the ef_common merge keeps texture bytes addressable: for every emitter texture
//! in a common set, the merged texture_section slice must equal the unmerged one.
//! Usage: merge_tex_check <fighter_eff> <common_eff> <common_set_idx>

use hitbox_editor::effects::{EffIndex, PtclFile};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let fighter_eff = &a[0];
    let common_eff = &a[1];
    let set_idx: usize = a[2].parse().unwrap();

    let mut idx = EffIndex::from_file(fighter_eff.as_ref()).expect("fighter eff");
    let mut merged = PtclFile::parse(&idx.ptcl_data).expect("fighter ptcl");
    let base_sets = merged.emitter_sets.len();
    idx.merge_from_file_with_ptcl(common_eff.as_ref(), &mut merged)
        .expect("merge");

    let common_idx = EffIndex::from_file(common_eff.as_ref()).expect("common eff");
    let common = PtclFile::parse(&common_idx.ptcl_data).expect("common ptcl");

    let cset = &common.emitter_sets[set_idx];
    let mset = &merged.emitter_sets[base_sets + set_idx];
    assert_eq!(cset.name, mset.name, "set alignment");
    println!("set '{}' ({} emitters)", cset.name, cset.emitters.len());

    let mut bad = 0;
    for (ce, me) in cset.emitters.iter().zip(mset.emitters.iter()) {
        for (ct, mt) in ce.textures.iter().zip(me.textures.iter()) {
            let co = ct.ftx_data_offset as usize;
            let cs = ct.ftx_data_size as usize;
            let mo = mt.ftx_data_offset as usize;
            let ms = mt.ftx_data_size as usize;
            let cok = co + cs <= common.texture_section.len();
            let mok = mo + ms <= merged.texture_section.len();
            let equal = cok
                && mok
                && cs == ms
                && common.texture_section[co..co + cs] == merged.texture_section[mo..mo + ms];
            if !equal {
                bad += 1;
            }
            println!(
                "  {:20} tex '{}' {}x{} fmt={:#06x} un(off={} sz={} ok={}) merged(off={} sz={} ok={}) bytes_equal={}",
                ce.name, ct.tex_name, ct.width, ct.height, ct.ftx_format, co, cs, cok, mo, ms, mok, equal
            );
        }
        // bntx_textures route (texture_index)
        if ce.texture_index != u32::MAX {
            let cbt = common.bntx_textures.get(ce.texture_index as usize);
            let mbt = merged.bntx_textures.get(me.texture_index as usize);
            match (cbt, mbt) {
                (Some(cb), Some(mb)) => {
                    let co = cb.ftx_data_offset as usize;
                    let cs = cb.ftx_data_size as usize;
                    let mo = mb.ftx_data_offset as usize;
                    let ms = mb.ftx_data_size as usize;
                    let equal = cs == ms
                        && co + cs <= common.texture_section.len()
                        && mo + ms <= merged.texture_section.len()
                        && common.texture_section[co..co + cs]
                            == merged.texture_section[mo..mo + ms];
                    if !equal {
                        bad += 1;
                    }
                    println!(
                        "  {:20} bntx[{} -> {}] '{}' vs '{}' bytes_equal={}",
                        ce.name, ce.texture_index, me.texture_index, cb.tex_name, mb.tex_name, equal
                    );
                }
                _ => {
                    bad += 1;
                    println!(
                        "  {:20} bntx index OOB: common {} (have {}), merged {} (have {})",
                        ce.name,
                        ce.texture_index,
                        common.bntx_textures.len(),
                        me.texture_index,
                        merged.bntx_textures.len()
                    );
                }
            }
        }
    }
    println!("mismatches: {bad}");
}
