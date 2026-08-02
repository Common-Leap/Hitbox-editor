fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS")
        .expect("Cargo did not provide the target operating system");

    // `image_dds`'s encoder is `intel_tex_2`, which links prebuilt ISPC/C++ objects. Those
    // objects reference the C++ personality routine, and rustc links a Rust binary against
    // libc only — without this the build dies at `undefined symbol: __gxx_personality_v0`.
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if target_os == "macos" {
        println!("cargo:rustc-link-lib=dylib=c++");
    }

    // A native Windows resource is needed in addition to the runtime window icon. This is
    // what Explorer and shortcuts read before the program has started.
    if target_os == "windows" {
        winresource::WindowsResource::new()
            .set_icon("assets/icons/visionary.ico")
            .compile()
            .expect("failed to compile the Visionary Windows icon resource");
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icons/visionary.ico");
}
