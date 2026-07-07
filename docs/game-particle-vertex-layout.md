# SSBU particle draw architecture (decoded from Ryujinx vertex captures)

Source: `~/Workshop/hitbox-captures/session2-bomb-vertex` (Hook 2c capture, one samus bomb).
Reference emitter: vs=497a93c3 (bomb smoke, ef_common set 105 family), one particle alive at
a time, respawning every ~18–24 frames over frames 2463–2575.

## The big picture — stateless GPU sim

**Every vertex attribute is constant for a particle's entire life.** The only per-frame
change is `cbuf_10[2].x`, which is the **emitter's elapsed time in frames** (observed
counting 0,1,2,3 … 109 across the capture, continuing across particle respawns).
`cbuf_10[2].y` varies slowly (1..3 — emission wave index?).

So the game does NOT re-upload per-frame particle state. It uploads **spawn state** once
per particle and the VS/FS reconstruct position/colour/size at the current time from
(spawn params, emitter clock, cbuf curves). Draws are one particle each: 4-vertex
non-indexed strip, all 9 attribute buffers are single 16-byte vec4s (constant attributes),
corners expanded in-shader from the vertex index.

Contrast with the editor: we run a CPU sim and upload evaluated per-frame values into a
6-vertex quad stream with our own attr semantics. Both can produce identical pixels; the
game's *attr semantics* cannot be compared 1:1 with ours (only cbuf contents can — see the
cbuf golden tier).

## Decoded per-particle attribute layout (this shader family)

| attr | observed | meaning (confidence) |
|---|---|---|
| 0 | (-0.036, -0.052, 0.0, huge-bits) | spawn position, local/world; .w = packed uint bits (flags?) (high) |
| 1 | (±300..±700, ±3..32, ±0.7..5, 4.0) | per-particle motion params — velocity/rotation-speed scale? .w = 4.0 constant (low) |
| 2 | (0,0,0,0) | zero here — velocity for this emitter type? (low) |
| 3 | (s, s, s, 1.0), s ≈ 0.99..1.77 | spawn scale (uniform × random) (high) |
| 4 | (r1, r2, r3, r4) all 0..1 | per-particle RANDOM SEEDS — fresh every respawn (high) |
| 5 | (0, 0, -0.8727, 0) | initial rotation (z = −50°) (med) |
| 6 | (0.9945, −0.1045, 0, −20) | rotation-matrix row 0 (cos6°, −sin6°) + coefficient −20 (med) |
| 7 | (0.1045, 0.9945, 0, 200) | rotation-matrix row 1 (sin6°, cos6°) + coefficient 200 (med) |
| 8 | (0, 0, 1, 0) | axis/normal (med) |

Maxwell attrib words: attrs 0–7 map 1:1 to vertex buffers 0–7 at offset 0
(`0x38200000+N` = 32-bit float RGBA from buffer N); attrs 8+ are constant-flagged
(`0x…40`).

## Particle age derivation (RESOLVED — impactflash2 VS trace + capture)

The impactflash2 VS microcode (fixture `impactflash2_0x379a93eac9f8e935.bnsh`, decoded
WGSL) computes, right at the top of `main_1()`:

```
gpr_0 = in_attr5.w                       // BIRTH time in emitter frames
pred  = (birth > cbuf_10[2].x)           // born in the future → degenerate clip (cull)
gpr_0 = cbuf_10[2].x - birth             // AGE in frames
lifetime = float(int(trunc(in_attr4.w))) // LIFETIME in frames
pred  = (age >= lifetime)                // expired → cull
spline_t = age / lifetime                // the keyframe-table evaluation time
```

Capture confirmation (497a93c3 group): the shader's `in_attr5.w` maps to capture buffer 2
(`buf2.w` = 0 / 18 / 42 / … = birth frames), `in_attr4.w` to buffer 1 (`buf1.w` = 4.0 —
matching the observed 4-frame draw bursts). Note the decoded-WGSL `in_attrN` numbering ≠
capture buffer index (offset differs per shader family; the values identify the roles).

**Editor implementation** (`particle_renderer.rs` + `nvn_chain.rs`): we feed a fixed
clock origin `EMITTER_CLOCK_FRAMES` in `cbuf_10[2].x`, `attr5.w = clock − p.age`
(birth), and `attr4.w = p.lifetime` (frames) — the shader-computed age is exact for any
shared origin. This replaced the old normalized-life feed and, combined with the
forward-axis raw tables, fixed a real bug: particles were being drawn with death-state
colour/alpha at spawn (the old samus f0 golden's dark smoke streak was this bug — the
authored smoke alpha starts at 0.05 and ramps to 0.81 at death).

## Open questions (task #15)

- **attr1 xyz semantics** (large values ±700; .w = lifetime).
- Where ColorScale enters (game keeps cbuf_10[0] = 1.0; colour tables are raw).
- Alpha table entry layout (yz components / prepended keys).

## Implications for the editor

- Our CPU-sim + evaluated-attr architecture stays: it renders correct pixels without
  reproducing the game's stateless evaluation. The native VS chains that expect the
  game's spawn-state inputs are exactly why `override_billboard_position` /
  `finalize_native_vs_clip_position` exist.
- cbuf comparisons remain the valid ground-truth channel (colour tables verified
  bit-exact); attr-level comparisons need this table, not our attr numbering.
- `cbuf_10[2].x` should be understood as *emitter frame clock* everywhere in our RE notes
  (we feed 1.0 with normalized-life attrs — internally consistent with our overrides).
