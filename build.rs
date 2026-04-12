use wgsl_to_wgpu::{create_shader_module, MatrixVectorTypes, WriteOptions};

fn process_shader(src_path: &str, out_path: &str) {
    let src = match std::fs::read_to_string(src_path) {
        Ok(s) => s,
        Err(e) => { eprintln!("cargo:warning=wgsl_to_wgpu: cannot read {src_path}: {e}"); return; }
    };
    let opts = WriteOptions {
        derive_bytemuck_vertex: true,
        derive_encase_host_shareable: true,
        matrix_vector_types: MatrixVectorTypes::Glam,
        ..Default::default()
    };
    match create_shader_module(&src, src_path, opts) {
        Ok(text) => {
            if let Err(e) = std::fs::write(out_path, text.as_bytes()) {
                eprintln!("cargo:warning=wgsl_to_wgpu: cannot write {out_path}: {e}");
            }
        }
        Err(e) => {
            // Non-fatal — hand-written structs remain valid.
            eprintln!("cargo:warning=wgsl_to_wgpu: {src_path}: {e}");
        }
    }
    println!("cargo:rerun-if-changed={src_path}");
}

fn main() {
    process_shader("src/particle.wgsl", "src/particle_shader.rs");
    process_shader("src/trail.wgsl",    "src/trail_shader.rs");
    process_shader("src/mesh.wgsl",     "src/mesh_shader.rs");
}
