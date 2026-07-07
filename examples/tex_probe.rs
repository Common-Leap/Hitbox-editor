//! Print each emitter's texture names + blend type for a set.
//! Usage: tex_probe <eff> <set_idx>
use hitbox_editor::effects::{EffIndex, PtclFile};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(a[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    let si: usize = a[1].parse().unwrap();
    for (i, e) in ptcl.emitter_sets[si].emitters.iter().enumerate() {
        let texs: Vec<String> = e
            .textures
            .iter()
            .map(|t| format!("{}x{} '{}'", t.width, t.height, t.tex_name))
            .collect();
        println!(
            "[{i}] {:20} blend={:?} ntex={} {:?}",
            e.name,
            e.blend_type,
            e.textures.len(),
            texs
        );
    }
}
