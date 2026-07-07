# Ryujinx capture toolkit — ground truth for the effect-accuracy harness

Captures **real NVN constant-buffer values and framebuffer frames** from SSBU running in a
patched Ryujinx (Ryubing fork), and converts them into the goldens consumed by:

- `tests/cbuf_golden.rs` — numerical tier: asserts `nvn_chain::build_cbuf_*` reproduces the
  game's cbuf slots (this is what unblocks the Phase 3 deep-RE tasks).
- `tests/regression_harness.rs` — visual tier: pixel-diff against real frames.

## Why Ryujinx

Ryujinx emulates the Switch GPU (Maxwell), so at draw time it holds exactly what the game's
NVN driver uploaded: every constant bank (`c[N]` = our `cbuf_N` — same numbering, no
translation), bound textures, blend state, and the shader that consumed them. Patching its
draw path gives byte-perfect cbuf ground truth without console homebrew.

## Contents

| Path | What |
|---|---|
| `patch/EffectCapture.cs` | Self-contained C# dumper class (no deps, env-gated, no-op when `HITBOX_CAPTURE_DIR` unset) |
| `patch/INTEGRATION.md` | The 3 call sites to add in `Ryujinx.Graphics.Gpu` (frame counter, draw dump, optional frame dump) |
| `../../examples/ryujinx_to_goldens.rs` | Converter: dump → `tests/goldens/cbuf/*.json` + PNG frames |

## Workflow

1. **Patch & build Ryujinx** (once): copy `patch/EffectCapture.cs` into
   `src/Ryujinx.Graphics.Gpu/`, add the hooks per `patch/INTEGRATION.md`, `dotnet build`.

2. **Capture**: run SSBU with capture enabled, narrowing the frame window to keep the dump
   small (a bomb lasts ~100 frames; each particle draw JSON is ~30 KB):

   ```sh
   HITBOX_CAPTURE_DIR=/tmp/cap HITBOX_CAPTURE_FRAMES=300-420 ./Ryujinx ssbu.nsp
   ```

   Training mode, Samus, drop a bomb inside the window. Draw dumps land in
   `/tmp/cap/draws/`, frame dumps (if Hook 3 is in) in `/tmp/cap/frames/`.
   The dumper filters to draws that bind VS bank 9 + FS bank 16 — the effect-shader
   signature — so UI/model draws are skipped.

3. **Inspect & correlate**:

   ```sh
   cargo +nightly-2026-02-14 run --example ryujinx_to_goldens -- list /tmp/cap
   ```

   Draws sharing a `vs_hash`/`fs_hash` are the same emitter across frames. Correlating a
   Ryujinx draw with our (fighter, emitter_set, emitter) identity is manual in v1: you know
   what you triggered in the capture window, and per-emitter draw counts + first-appearance
   frame narrow it down. (Ryujinx hashes are of Maxwell microcode, not BNSH, so they can't
   be matched against our shader registry directly.)

4. **Generate cbuf goldens** (one per draw you've identified):

   ```sh
   cargo +nightly-2026-02-14 run --example ryujinx_to_goldens -- cbuf \
       /tmp/cap/draws/frame_000312_draw_0007.json \
       --fighter samus --emitter-set 0 --emitter 0 --life-t 0.5 --full
   ```

   Only slots that **both** the capture and our local builders populate become assertions;
   `--full` additionally writes every captured slot to `tests/goldens/cbuf/ref/` as
   RE reference (not scanned by the test). Camera-dependent slots won't match the
   identity-view_proj defaults — either hand-edit the golden's `view_proj`/`camera` to the
   capture-time values, or `--exclude cbuf_9:44,cbuf_9:45,...` them.

   `--life-t` is the particle's normalized age at capture time; for per-life-t slots
   (cbuf_8/cbuf_16 color curves) capture several frames of the same emitter and generate a
   golden per frame with the corresponding `--life-t`.

5. **Run the gate**:

   ```sh
   cargo +nightly-2026-02-14 test --test cbuf_golden -- --test-threads=1
   ```

   Mismatches print `cbuf_N[slot]: expected <game> got <ours>` — each one is a concrete,
   fixable inaccuracy. `tests/goldens/` is gitignored (copyrighted-asset convention).

6. **Visual goldens** (Hook 3 is implemented — frame dumps land in `frames/*.rgba.gz`,
   every 2nd presented frame by default; `HITBOX_CAPTURE_FRAME_EVERY` tunes the stride,
   `0` disables):

   ```sh
   cargo +nightly-2026-02-14 run --example capture_frames_to_png -- /tmp/cap [out_dir] [--frames start-end]
   ```

   Channel order is auto-detected from the host format baked into the filename
   (no --bgra flag needed); alpha is forced opaque.

   Crop/scale the PNGs to the effect region before using them as `tests/goldens/<effect>/`
   images — the harness renders 256×256 with its own framing, so game frames are better
   used for eyeballing and for hand-checked comparisons than as strict pixel gates.

## What the captures unblock

- **#9 / P1.5** — real per-life-t color values from cbuf_8/16 across a particle's life
  reveal the game's actual Hermite control points.
- **#15 / Phase 3.1** — `cbuf_9[46]/[47]` position/size roles, validated slot-by-slot.
- **#15 / Phase 3.2** — combiner coefficient vectors (`cbuf_16[1-3]`, `cbuf_8[6-7]`) 1:1.
- **Phase 2 RNG** — captured first-frame particle state constrains the nw::eft PRNG model.
