# Soft particle depth fade — agent coordination

## CPU flags (`ParticleColorState`)

Parsed in `effect_converter.rs` → `EmitterDef.particle_color`:

| Field | JSON source |
|-------|-------------|
| `is_soft_particle` | `ParticleColor.IsSoftParticle` |
| `soft_particle_volume` | `EmitterStatic.SoftParticleVolume` |
| `soft_edge_param1/2` | `EmitterStatic.SoftEdgeParam1/2` |
| `soft_particle_dist` | `EmitterStatic.SoftPartcileDist` (typo in dump) |

Per-draw uniform upload: `particle_renderer::upload_soft_particle_uniform` (pool `@group(3)` binding 1).

## GPU — fragment shader (`spirv_to_wgsl.rs`)

- `inject_soft_particle_fs()` runs in `prepare_bnsh_wgsl` after native/patch FS wiring.
- `@group(3) @binding(0)` — `texture_2d<f32>` mesh/path depth (`Depth32Float`), sampled with `textureLoad` at `@builtin(position)` pixel coords.
- `@group(3) @binding(1)` — `FxSoftParticle` uniform (`enabled`, `volume`, `edge1`, `edge2`, `dist`).
- Fade: `fade = clamp((scene_z - frag_z) / dist, 0, 1) * volume`, optional `smoothstep(edge1, edge2, fade)`; premultiplied RGBA multiply.

Detection helper: `native_fs_soft_particle_needed(wgsl)`.

**Agent 2 (fresnel / distance alpha):** reuse `_fx_frag_pos` / inject after `_fx_apply_soft_particle` by extending the same helper hook — do not move `@group(3)` bindings.

**Agent 3 (native FS / distortion / combiner):** `@group(2)` unchanged (tex3–5 + blend uniform). Soft fade only uses `@group(3)`.

**Agent 4 (native colour chain):** keep `enhance_native_fragment_wgsl*` output as input to `inject_soft_particle_fs`; do not replace wholesale.

## Draw path / depth source

- Editor: `renderer.rs` `finish_prepare`:
  1. Copy `SsbhRenderer::copy_mesh_depth_resolved` → dedicated `scene_mesh_depth` (1×) for soft-particle `@group(3)` sampling (mesh depth only, unaffected by particle depth writes).
  2. `ParticleRenderer::set_scene_depth_view` + second `prepare_particle_frame` so soft bind groups reference live mesh depth.
  3. Before **each** offscreen pass (ExcludeSub color + SubOnly per draw_path): copy mesh depth → that path's `ParticlePathTarget.depth`, attach `PARTICLE_DEPTH_FORMAT` with `Load`, then draw:
     - ExcludeSub: `DepthDrawConfig::OPAQUE_CORE` (depth write) then `TRANSPARENT` (depth test, no write).
     - SubOnly: `DepthDrawConfig::TRANSPARENT`.
- Fallback: path depth attachment `Clear(1.0)` + `DepthDrawConfig::NONE` when no mesh; soft particles use 1×1 fallback depth texture cleared to `1.0`.

**Agent 5 (draw-path compositing / depth attachments):** done in `renderer.rs` + `particle_path_depth_attachment` in `particle_renderer_bnsh.rs`.

- Hardware depth test on offscreen passes is separate from soft fade (texture read); both need consistent depth content.
- Bind order at draw: BNSH sets → `@group(1)` emitter tex → `@group(2)` extra tex (dummy OK) → `@group(3)` soft particle.

## Pipeline layout

`BnshPipelineState::new(..., extra_tex345_bg_layout, soft_particle_bg_layout, ...)` adds group 2 layout when extra tex **or** soft particles need it (group 3 requires group 2 slot in layout even if only white dummy binds).
