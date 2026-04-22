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
    
    // Build the spirv-cross library from git submodule
    build_spirv_cross_library();
}

fn build_bnsh_decoder_cli() {
    use std::path::PathBuf;
    use std::process::Command;
    
    let bnsh_dir = PathBuf::from("extern/bnsh-decoder");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("bnsh-decoder-build");
    
    println!("cargo:warning=Building bnsh-decoder CLI from {}", bnsh_dir.display());
    println!("cargo:warning=Build output directory: {}", build_dir.display());
    
    // Check if bnsh-decoder source exists
    if !bnsh_dir.exists() {
        println!("cargo:warning=ERROR: bnsh-decoder source not found at {}", bnsh_dir.display());
        println!("cargo:warning=Did you forget to run: git submodule update --init --recursive");
        std::process::exit(1);
    }
    
    // Check if CMake is available
    match Command::new("cmake").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!("cargo:warning=Using CMake: {}", version.lines().next().unwrap_or("unknown"));
        }
        _ => {
            println!("cargo:warning=ERROR: CMake not found in PATH");
            println!("cargo:warning=Install CMake to build bnsh-decoder");
            println!("cargo:warning=Ubuntu: sudo apt install cmake");
            println!("cargo:warning=macOS: brew install cmake");
            println!("cargo:warning=Windows: https://cmake.org/download/");
            std::process::exit(1);
        }
    }
    
    // Create build directory
    std::fs::create_dir_all(&build_dir).expect("Failed to create bnsh-decoder build directory");
    
    // Run CMake to configure bnsh-decoder
    println!("cargo:warning=Configuring bnsh-decoder with CMake...");
    let cmake_status = Command::new("cmake")
        .arg("-B").arg(&build_dir)
        .arg("-S").arg(&bnsh_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .status()
        .expect("Failed to run cmake for bnsh-decoder");
    
    if !cmake_status.success() {
        println!("cargo:warning=ERROR: CMake configuration failed for bnsh-decoder");
        println!("cargo:warning=Try running manually: cmake -B {}", build_dir.display());
        std::process::exit(1);
    }
    
    // Build bnsh-decoder CLI
    println!("cargo:warning=Building bnsh-decoder CLI...");
    let build_status = Command::new("cmake")
        .arg("--build").arg(&build_dir)
        .arg("--config").arg("Release")
        .status()
        .expect("Failed to build bnsh-decoder");
    
    if !build_status.success() {
        println!("cargo:warning=ERROR: CMake build failed for bnsh-decoder");
        println!("cargo:warning=Try running manually: cmake --build {}", build_dir.display());
        std::process::exit(1);
    }
    
    // Find the CLI binary (platform-specific)
    let cli_candidates = if cfg!(windows) {
        vec![
            build_dir.join("src/bnsh_cli/Release/CLI.exe"),
            build_dir.join("src/bnsh_cli/Debug/CLI.exe"),
            build_dir.join("src/bnsh_cli/CLI.exe"),
            build_dir.join("Release/CLI.exe"),
        ]
    } else {
        vec![
            build_dir.join("src/bnsh_cli/CLI"),
            build_dir.join("Release/CLI"),
            build_dir.join("CLI"),
        ]
    };
    
    let mut found = false;
    for path in &cli_candidates {
        if path.exists() {
            println!("cargo:rustc-env=BNSH_DECODER_CLI={}", path.display());
            println!("cargo:warning=✓ bnsh-decoder CLI built successfully: {}", path.display());
            found = true;
            break;
        }
    }
    
    if !found {
        println!("cargo:warning=ERROR: bnsh-decoder CLI binary not found after successful build");
        println!("cargo:warning=Searched locations:");
        for path in &cli_candidates {
            println!("cargo:warning=  - {}", path.display());
        }
        std::process::exit(1);
    }
    
    println!("cargo:rerun-if-changed=extern/bnsh-decoder");
}

fn build_spirv_cross_library() {
    use std::path::PathBuf;
    use std::process::Command;
    
    let spirv_cross_dir = PathBuf::from("extern/spirv-cross");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("spirv-cross-build");
    
    println!("cargo:warning=Building spirv-cross CLI from {}", spirv_cross_dir.display());
    println!("cargo:warning=Build output directory: {}", build_dir.display());
    
    // Check if spirv-cross source exists
    if !spirv_cross_dir.exists() {
        println!("cargo:warning=ERROR: spirv-cross source not found at {}", spirv_cross_dir.display());
        println!("cargo:warning=Did you forget to run: git submodule update --init --recursive");
        std::process::exit(1);
    }
    
    // Check if CMake is available
    match Command::new("cmake").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!("cargo:warning=Using CMake: {}", version.lines().next().unwrap_or("unknown"));
        }
        _ => {
            println!("cargo:warning=ERROR: CMake not found in PATH");
            println!("cargo:warning=Install CMake to build spirv-cross");
            println!("cargo:warning=Ubuntu: sudo apt install cmake");
            println!("cargo:warning=macOS: brew install cmake");
            println!("cargo:warning=Windows: https://cmake.org/download/");
            std::process::exit(1);
        }
    }
    
    // Create build directory
    std::fs::create_dir_all(&build_dir).expect("Failed to create spirv-cross build directory");
    
    // Run CMake to configure spirv-cross
    // Note: CLI requires static libraries to be built
    println!("cargo:warning=Configuring spirv-cross with CMake...");
    let cmake_status = Command::new("cmake")
        .arg("-B").arg(&build_dir)
        .arg("-S").arg(&spirv_cross_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DSPIRV_CROSS_STATIC=ON")
        .arg("-DSPIRV_CROSS_SHARED=OFF")
        .arg("-DSPIRV_CROSS_CLI=ON")
        .status()
        .expect("Failed to run cmake for spirv-cross");
    
    if !cmake_status.success() {
        println!("cargo:warning=ERROR: CMake configuration failed for spirv-cross");
        println!("cargo:warning=Try running manually: cmake -B {}", build_dir.display());
        std::process::exit(1);
    }
    
    // Build spirv-cross
    println!("cargo:warning=Building spirv-cross CLI...");
    let build_status = Command::new("cmake")
        .arg("--build").arg(&build_dir)
        .arg("--config").arg("Release")
        .status()
        .expect("Failed to build spirv-cross");
    
    if !build_status.success() {
        println!("cargo:warning=ERROR: CMake build failed for spirv-cross");
        println!("cargo:warning=Try running manually: cmake --build {}", build_dir.display());
        std::process::exit(1);
    }
    
    // Find the spirv-cross CLI binary
    let cli_candidates = if cfg!(windows) {
        vec![
            build_dir.join("Release/spirv-cross.exe"),
            build_dir.join("spirv-cross.exe"),
        ]
    } else {
        vec![
            build_dir.join("spirv-cross"),
            build_dir.join("Release/spirv-cross"),
        ]
    };
    
    let mut found = false;
    for path in &cli_candidates {
        if path.exists() {
            println!("cargo:rustc-env=SPIRV_CROSS_CLI={}", path.display());
            println!("cargo:warning=✓ spirv-cross CLI built successfully: {}", path.display());
            found = true;
            break;
        }
    }
    
    if !found {
        println!("cargo:warning=ERROR: spirv-cross CLI binary not found after successful build");
        println!("cargo:warning=Searched locations:");
        for path in &cli_candidates {
            println!("cargo:warning=  - {}", path.display());
        }
        std::process::exit(1);
    }
    
    println!("cargo:rerun-if-changed=extern/spirv-cross");
}
