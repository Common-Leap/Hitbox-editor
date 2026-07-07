//! RE helper: decode a BNSH fixture and print its WGSL (for inspecting native NVN chains).
//!
//! Usage: cargo run --example dump_fs_wgsl -- <path.bnsh> [shader_index] [--stage fs|vs]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: dump_fs_wgsl <path.bnsh> [shader_index]");
        std::process::exit(2);
    };
    let index: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let data = std::fs::read(path).expect("read bnsh");
    let result = hitbox_editor::bnsh_ffi::BnshDecoder::decode_with_metadata_and_index(&data, index)
        .expect("decode");
    let stage = match result.stage {
        hitbox_editor::bnsh_ffi::ShaderStage::Vertex => naga::ShaderStage::Vertex,
        hitbox_editor::bnsh_ffi::ShaderStage::Fragment => naga::ShaderStage::Fragment,
        _ => naga::ShaderStage::Compute,
    };
    let spirv_bytes: Vec<u8> = result.spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
    eprintln!("stage: {:?}, spirv {} bytes", result.stage, spirv_bytes.len());
    let (wgsl, _desc) =
        hitbox_editor::spirv_to_wgsl::spirv_to_wgsl(&spirv_bytes, stage, "dump").expect("to wgsl");
    println!("{wgsl}");
}
