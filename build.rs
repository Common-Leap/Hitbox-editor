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

    // Build the bnsh-decoder CLI tool from git submodule
    build_bnsh_decoder_cli();
}

fn build_bnsh_decoder_cli() {
    use std::path::PathBuf;
    use std::process::Command;
    
    let bnsh_dir = PathBuf::from("extern/bnsh-decoder");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("bnsh-decoder-build");
    
    // Create build directory
    std::fs::create_dir_all(&build_dir).expect("Failed to create bnsh-decoder build directory");
    
    // Run CMake to configure bnsh-decoder
    let cmake_status = Command::new("cmake")
        .arg("-B").arg(&build_dir)
        .arg("-S").arg(&bnsh_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.24")
        .status()
        .expect("Failed to run cmake for bnsh-decoder");
    
    if !cmake_status.success() {
        eprintln!("cargo:warning=CMake configuration failed for bnsh-decoder");
        std::process::exit(1);
    }
    
    // Build bnsh-decoder CLI
    let build_status = Command::new("cmake")
        .arg("--build").arg(&build_dir)
        .arg("--config").arg("Release")
        .status()
        .expect("Failed to build bnsh-decoder");
    
    if !build_status.success() {
        eprintln!("cargo:warning=CMake build failed for bnsh-decoder");
        std::process::exit(1);
    }
    
    // Find and export the CLI binary path
    let cli_binary_paths = vec![
        build_dir.join("src/bnsh_cli/CLI"),
        build_dir.join("src/bnsh_cli/Release/CLI.exe"),
        build_dir.join("src/bnsh_cli/Debug/CLI.exe"),
        build_dir.join("src/bnsh_cli/CLI.exe"),
    ];
    
    let mut found = false;
    for path in &cli_binary_paths {
        if path.exists() {
            println!("cargo:rustc-env=BNSH_DECODER_CLI={}", path.display());
            eprintln!("cargo:warning=bnsh-decoder CLI built at: {}", path.display());
            found = true;
            break;
        }
    }
    
    if !found {
        eprintln!("cargo:warning=Warning: bnsh-decoder CLI binary not found, will use placeholder");
        // Still set a path in case it's found at runtime
        println!("cargo:rustc-env=BNSH_DECODER_CLI_SEARCHED=true");
    }
    
    println!("cargo:rerun-if-changed=extern/bnsh-decoder");
}
