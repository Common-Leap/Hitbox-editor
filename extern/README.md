# Third-party dependencies

This directory contains **git submodules only** — no vendored copies of upstream source
in the Hitbox Editor tree. Clone with:

```bash
git submodule update --init --recursive
```

| Path | Upstream | Notes |
|------|----------|-------|
| `bnsh-decoder/` | [maierfelix/bnsh-decoder](https://github.com/maierfelix/bnsh-decoder) | Unmodified submodule; built by `build.rs` |
| `spirv-cross/` | [KhronosGroup/SPIRV-Cross](https://github.com/KhronosGroup/SPIRV-Cross) | Unmodified submodule; built by `build.rs` |
| `effect-library/` | [joobert/EffectLibrary](https://github.com/joobert/EffectLibrary) | Unmodified submodule; EffectConverter built by `build.rs` |
| `ssbh_wgpu/` | [Common-Leap/ssbh_wgpu](https://github.com/Common-Leap/ssbh_wgpu) (`hitbox-editor`) | Fork of [ScanMountGoat/ssbh_wgpu](https://github.com/ScanMountGoat/ssbh_wgpu) with pass-ordered 1× mesh depth for particle occlusion |

Rust crates (`ssbh_wgpu`, `nutexb_wgpu`) are loaded from the submodule via path dependencies in `Cargo.toml`.
