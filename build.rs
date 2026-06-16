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
    process_shader("src/trail.wgsl",    "src/trail_shader.rs");

    // Build the bnsh-decoder CLI tool from git submodule
    build_bnsh_decoder_cli();
    
    // Build the spirv-cross library from git submodule
    build_spirv_cross_library();

    // Build the EffectConverter CLI from git submodule
    build_effect_converter();
}

fn cmake_build_command(build_dir: &std::path::Path) -> std::process::Command {
    let tmp_dir = build_dir.join("compiler-tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let mut cmd = std::process::Command::new("cmake");
    // gcc/clang write large .s files to TMPDIR; /tmp is often a small tmpfs.
    cmd.env("TMPDIR", &tmp_dir);
    cmd.env("TEMP", &tmp_dir);
    cmd.env("TMP", &tmp_dir);
    cmd
}

fn build_effect_converter() {
    use std::path::PathBuf;
    use std::process::Command;

    let effect_lib_dir = PathBuf::from("extern/effect-library");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("effect-converter-build");

    println!("cargo:rerun-if-env-changed=EFFECT_CONVERTER_CLI");
    println!("cargo:rerun-if-env-changed=EFFECT_CONVERTER_FORCE_REBUILD");

    let cli_path = if cfg!(windows) {
        build_dir.join("EffectConverter.exe")
    } else {
        build_dir.join("EffectConverter")
    };
    let stable_cli = PathBuf::from("target/effect-converter").join(effect_converter_exe_name());
    let vendored_dir = vendored_effect_converter_dir();

    // Explicit override (system install or copied binary).
    if let Ok(env_cli) = std::env::var("EFFECT_CONVERTER_CLI") {
        let p = PathBuf::from(&env_cli);
        if p.exists() {
            println!("cargo:rustc-env=EFFECT_CONVERTER_CLI={}", p.display());
            println!(
                "cargo:warning=Using EffectConverter from EFFECT_CONVERTER_CLI={}",
                p.display()
            );
            return;
        }
        println!(
            "cargo:warning=EFFECT_CONVERTER_CLI set but missing: {}",
            p.display()
        );
    }

    // Reuse a previous successful publish in OUT_DIR (avoids flaky dotnet/csc crashes).
    if cli_path.exists() && std::env::var("EFFECT_CONVERTER_FORCE_REBUILD").is_err() {
        if !effect_converter_sources_changed(&cli_path, &effect_lib_dir) {
            emit_effect_converter_cli(&cli_path);
            return;
        }
        println!("cargo:warning=EffectConverter sources changed — rebuilding");
    }

    // OUT_DIR changes when crate metadata changes; fall back to a stable project cache.
    if !cli_path.exists()
        && std::env::var("EFFECT_CONVERTER_FORCE_REBUILD").is_err()
        && restore_effect_converter_from_dir(
            stable_cli.parent(),
            &build_dir,
            &cli_path,
            &effect_lib_dir,
            "stable cache",
        )
    {
        return;
    }

    // Vendored publish output (survives `cargo clean` / tmp cache wipes).
    if !cli_path.exists()
        && std::env::var("EFFECT_CONVERTER_FORCE_REBUILD").is_err()
        && restore_effect_converter_from_dir(
            vendored_dir.as_deref(),
            &build_dir,
            &cli_path,
            &effect_lib_dir,
            "tools/effect-converter",
        )
    {
        return;
    }

    println!("cargo:warning=Building EffectConverter CLI from {}", effect_lib_dir.display());
    println!("cargo:warning=Build output directory: {}", build_dir.display());

    if !effect_lib_dir.exists() {
        println!("cargo:warning=ERROR: EffectLibrary source not found at {}", effect_lib_dir.display());
        println!("cargo:warning=Did you forget to run: git submodule update --init --recursive");
        std::process::exit(1);
    }

    match Command::new("dotnet").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!("cargo:warning=Using .NET: {}", version.lines().next().unwrap_or("unknown"));
        }
        _ => {
            if try_use_existing_effect_converter(
                &cli_path,
                &stable_cli,
                vendored_dir.as_deref(),
                &build_dir,
                "dotnet not found in PATH",
            ) {
                return;
            }
            println!("cargo:warning=ERROR: dotnet not found in PATH");
            println!("cargo:warning=Install .NET 6.0+ SDK to build EffectConverter");
            std::process::exit(1);
        }
    }

    let csproj = effect_lib_dir.join("EffectConverter").join("EffectConverter.csproj");
    if !csproj.exists() {
        println!("cargo:warning=ERROR: EffectConverter.csproj not found at {}", csproj.display());
        std::process::exit(1);
    }

    std::fs::create_dir_all(&build_dir).expect("Failed to create effect-converter build directory");
    let dotnet_tmp = build_dir.join("dotnet-tmp");
    let nuget_packages = build_dir.join("nuget-packages");
    let _ = std::fs::create_dir_all(&dotnet_tmp);
    let _ = std::fs::create_dir_all(&nuget_packages);

    println!("cargo:warning=Publishing EffectConverter...");
    let publish_output = Command::new("dotnet")
        .arg("publish")
        .arg(&csproj)
        .arg("-c").arg("Release")
        .arg("-o").arg(&build_dir)
        .arg("--self-contained").arg("false")
        .arg("-maxcpucount:1")
        .arg("-p:UseSharedCompilation=false")
        .env("TMPDIR", &dotnet_tmp)
        .env("TEMP", &dotnet_tmp)
        .env("TMP", &dotnet_tmp)
        .env("NUGET_PACKAGES", &nuget_packages)
        .env("MSBUILDDISABLENODEREUSE", "1")
        .output()
        .expect("Failed to publish EffectConverter");

    if !publish_output.status.success() {
        let stderr = String::from_utf8_lossy(&publish_output.stderr);
        let stdout = String::from_utf8_lossy(&publish_output.stdout);
        if !stdout.trim().is_empty() {
            println!("cargo:warning=EffectConverter publish stdout:\n{stdout}");
        }
        if !stderr.trim().is_empty() {
            println!("cargo:warning=EffectConverter publish stderr:\n{stderr}");
        }
        if try_use_existing_effect_converter(
            &cli_path,
            &stable_cli,
            vendored_dir.as_deref(),
            &build_dir,
            "dotnet publish failed",
        ) {
            return;
        }
        println!("cargo:warning=ERROR: dotnet publish failed for EffectConverter");
        println!("cargo:warning=Bundled copy expected at tools/effect-converter/{}/", vendored_platform_dir());
        println!("cargo:warning=Or set EFFECT_CONVERTER_CLI to a working EffectConverter binary.");
        std::process::exit(1);
    }

    if cli_path.exists() {
        emit_effect_converter_cli(&cli_path);
        if let Some(stable_dir) = stable_cli.parent() {
            if let Err(e) = sync_effect_converter_dir(&build_dir, stable_dir) {
                println!("cargo:warning=Failed to update stable EffectConverter cache: {e}");
            }
        }
    } else {
        if try_use_existing_effect_converter(
            &cli_path,
            &stable_cli,
            vendored_dir.as_deref(),
            &build_dir,
            "publish succeeded but binary missing",
        ) {
            return;
        }
        println!("cargo:warning=ERROR: EffectConverter CLI binary not found at {}", cli_path.display());
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=extern/effect-library");
    println!("cargo:rerun-if-changed=tools/effect-converter");
}

fn effect_converter_exe_name() -> &'static str {
    if cfg!(windows) {
        "EffectConverter.exe"
    } else {
        "EffectConverter"
    }
}

fn vendored_platform_dir() -> &'static str {
    if cfg!(windows) {
        "win-x64"
    } else {
        "linux-x64"
    }
}

fn vendored_effect_converter_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let dir = PathBuf::from("tools/effect-converter").join(vendored_platform_dir());
    let cli = dir.join(effect_converter_exe_name());
    cli.exists().then_some(dir)
}

fn restore_effect_converter_from_dir(
    src_dir: Option<&std::path::Path>,
    build_dir: &std::path::Path,
    cli_path: &std::path::Path,
    effect_lib_dir: &std::path::Path,
    label: &str,
) -> bool {
    let Some(src_dir) = src_dir else {
        return false;
    };
    let src_cli = src_dir.join(effect_converter_exe_name());
    if !src_cli.exists() {
        return false;
    }
    if effect_converter_sources_changed(&src_cli, effect_lib_dir) {
        return false;
    }
    std::fs::create_dir_all(build_dir).expect("Failed to create effect-converter build directory");
    if let Err(e) = sync_effect_converter_dir(src_dir, build_dir) {
        println!("cargo:warning=Failed to copy EffectConverter from {label}: {e}");
        return false;
    }
    if cli_path.exists() {
        emit_effect_converter_cli(cli_path);
        println!(
            "cargo:warning=✓ EffectConverter restored from {label}: {}",
            src_cli.display()
        );
        true
    } else {
        false
    }
}

/// True when any tracked EffectConverter source is newer than the built CLI.
fn effect_converter_sources_changed(cli_path: &std::path::Path, effect_lib_dir: &std::path::Path) -> bool {
    let Ok(cli_mtime) = std::fs::metadata(cli_path).and_then(|m| m.modified()) else {
        return true;
    };
    let tracked = [
        effect_lib_dir.join("EffectConverter/EffectConverter.csproj"),
        effect_lib_dir.join("EffectLibrary/EffectLibrary.csproj"),
    ];
    for path in tracked {
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.modified().ok().is_some_and(|t| t > cli_mtime) {
                return true;
            }
        }
    }
    for subdir in ["EffectConverter", "EffectLibrary"] {
        let root = effect_lib_dir.join(subdir);
        if sources_under_newer_than(&root, cli_mtime) {
            return true;
        }
    }
    false
}

fn sources_under_newer_than(root: &std::path::Path, cutoff: std::time::SystemTime) -> bool {
    let Ok(read) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("cs") {
            if entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| t > cutoff)
            {
                return true;
            }
        } else if path.is_dir() {
            if sources_under_newer_than(&path, cutoff) {
                return true;
            }
        }
    }
    false
}

fn try_use_existing_effect_converter(
    cli_path: &std::path::Path,
    stable_cli: &std::path::Path,
    vendored_dir: Option<&std::path::Path>,
    build_dir: &std::path::Path,
    reason: &str,
) -> bool {
    if cli_path.exists() {
        emit_effect_converter_cli(cli_path);
        println!(
            "cargo:warning=Reusing existing EffectConverter ({reason}): {}",
            cli_path.display()
        );
        return true;
    }
    for (label, dir) in [
        ("stable cache", stable_cli.parent()),
        ("tools/effect-converter", vendored_dir),
    ] {
        let Some(dir) = dir else { continue };
        let src_cli = dir.join(effect_converter_exe_name());
        if !src_cli.exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(build_dir);
        if sync_effect_converter_dir(dir, build_dir).is_ok() && cli_path.exists() {
            emit_effect_converter_cli(cli_path);
            println!(
                "cargo:warning=Reusing {label} EffectConverter ({reason}): {}",
                src_cli.display()
            );
            return true;
        }
    }
    false
}

fn emit_effect_converter_cli(cli_path: &std::path::Path) {
    println!("cargo:rustc-env=EFFECT_CONVERTER_CLI={}", cli_path.display());
    println!(
        "cargo:warning=✓ EffectConverter CLI ready: {}",
        cli_path.display()
    );
    println!("cargo:rerun-if-changed=extern/effect-library");
}

fn sync_effect_converter_dir(src_dir: &std::path::Path, dest_dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let dest = dest_dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            let _ = std::fs::remove_dir_all(&dest);
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
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
    let build_status = cmake_build_command(&build_dir)
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
    let build_status = cmake_build_command(&build_dir)
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
