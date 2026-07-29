fn main() {
    // `image_dds`'s encoder is `intel_tex_2`, which links prebuilt ISPC/C++ objects. Those
    // objects reference the C++ personality routine, and rustc links a Rust binary against
    // libc only — without this the build dies at `undefined symbol: __gxx_personality_v0`.
    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
