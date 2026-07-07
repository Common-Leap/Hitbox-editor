//! Write an effect's embedded BNSH container to a file (for dump_fs_wgsl RE work).
//! Usage: dump_embedded_bnsh <eff> <out.bnsh>

use hitbox_editor::effects::{EffIndex, PtclFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let eff = EffIndex::from_file(args[0].as_ref()).expect("eff");
    let ptcl = PtclFile::parse(&eff.ptcl_data).expect("ptcl");
    std::fs::write(&args[1], &ptcl.shader_binary_1).expect("write");
    println!("wrote {} ({} bytes)", args[1], ptcl.shader_binary_1.len());
}
