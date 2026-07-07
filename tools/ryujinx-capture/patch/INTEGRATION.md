# Integrating EffectCapture.cs into Ryujinx (Ryubing fork)

`EffectCapture.cs` is deliberately dumb: it takes plain data and writes files. All the
Ryujinx-API work happens at three small call sites you add by hand. Exact type/member names
drift between Ryujinx versions, so each hook below gives the concept, the likely location in
the tree, and a snippet template — expect to adjust identifiers, not structure.

Copy `EffectCapture.cs` into `src/Ryujinx.Graphics.Gpu/` (any subfolder; it declares
namespace `Ryujinx.Graphics.Gpu`). It compiles with no extra references.

Build as usual, e.g. `dotnet build -c Release src/Ryujinx`.

---

## Hook 1 — frame counter (required)

The dumper needs to know when a frame ends. Hook the swapchain present.

**Where:** `src/Ryujinx.Graphics.Gpu/Window.cs` — the method that dequeues and presents a
texture (named `Present` in every version I know of).

```csharp
// At the top of Window.Present(...), before presenting:
long hitboxFrame = EffectCapture.OnPresent();
```

That's it — `OnPresent()` is a no-op counter when capture is disabled.

## Hook 2 — draw dump (the important one)

**Where:** `src/Ryujinx.Graphics.Gpu/Engine/Threed/DrawManager.cs`, at the end of the method
that actually issues the draw (`DrawEnd` / `PerformDraw` depending on version — the one that
calls `_context.Renderer.Pipeline.Draw(...)` / `DrawIndexed(...)`).

You need four things at that point. All of them are reachable from the `ThreedClass`/
`DrawManager` fields (`_state`, `_channel`, `_context`):

1. **Constant buffer bytes.** `_channel.BufferManager.GetGraphicsUniformBufferAddress(stageIndex, bank)`
   returns the GPU VA of Maxwell constant bank `bank` for that stage (0 if unbound). Read
   the bytes with `_channel.MemoryManager.Physical.GetSpan(address, size)` (or
   `_channel.MemoryManager.GetSpan(gpuVa, size)` if the address returned is virtual in your
   version — check the other callers of `GetGraphicsUniformBufferAddress` and mirror them).
   Stage indices: 0 = Vertex … 4 = Fragment (Ryujinx's 5 graphics stages; confirm against
   the `SetGraphicsUniformBuffer` callers in your tree).

2. **Shader identity.** The bound program is set by the state updater; the cheapest robust
   identity is an FNV-1a hash of the guest shader code, computed where the program is bound
   (see Hook 2b) and stashed in a static. If your tree exposes the cached program on the
   draw path directly, hash `program.Shaders[stage].Code` inline instead.

3. **Blend state.** `_state.State.BlendStateCommon` / `BlendState[0]` — just `ToString()`
   or format the enable/src/dst factors; the Rust side treats it as an opaque note.

4. **Texture identity (optional but useful for correlating emitters).** From
   `_channel.TextureManager`, the bound graphics textures; use each texture's size +
   format + a hash of the first KB, or skip and pass an empty list — correlation can be
   done by frame number alone.

Snippet template (adjust names to your tree):

```csharp
private static byte[] HitboxReadCbuf(GpuChannel channel, int stageIndex, int bank, int size = 4096)
{
    ulong addr = channel.BufferManager.GetGraphicsUniformBufferAddress(stageIndex, bank);
    if (addr == 0) return null;
    return channel.MemoryManager.Physical.GetSpan(addr, size).ToArray();
}

// ... at the end of the draw method, after the real draw is issued:
if (EffectCapture.Enabled)
{
    const int VtxStage = 0, FragStage = 4;

    var vsCbufs = new Dictionary<int, byte[]>();
    var fsCbufs = new Dictionary<int, byte[]>();
    foreach (int bank in new[] { 1, 8, 9, 10 })
    {
        var b = HitboxReadCbuf(_channel, VtxStage, bank);
        if (b != null) vsCbufs[bank] = b;
    }
    foreach (int bank in new[] { 9, 16 })
    {
        var b = HitboxReadCbuf(_channel, FragStage, bank);
        if (b != null) fsCbufs[bank] = b;
    }

    // Particle-draw filter for SSBU effects: the effect FS always binds bank 16,
    // and the VS binds banks 8/9/10. Everything else (UI, models) is skipped.
    if (fsCbufs.ContainsKey(16) && vsCbufs.ContainsKey(9))
    {
        EffectCapture.DumpDraw(
            HitboxLastVsHash, HitboxLastFsHash,
            _state.State.BlendState[0].ToString(),
            Array.Empty<string>(),
            vsCbufs, fsCbufs);
    }
}
```

**Bank numbering note:** these are *Maxwell constant banks*, the `c[N]` the shader
microcode reads — identical to the editor's `cbuf_N` naming and to the slot index Ryujinx
uses in `GetGraphicsUniformBufferAddress`. No translation needed.

## Hook 2b — shader hash stash (goes with Hook 2)

**Where:** `src/Ryujinx.Graphics.Gpu/Engine/Threed/StateUpdater.cs`, in `UpdateShaderState`
(the method that fetches `CachedShaderProgram` from the shader cache and binds it).

```csharp
public static ulong HitboxLastVsHash, HitboxLastFsHash;   // put on DrawManager or a shared static

private static ulong HitboxFnv(ReadOnlySpan<byte> data)
{
    ulong h = 0xcbf29ce484222325;
    foreach (byte b in data) { h ^= b; h *= 0x100000001b3; }
    return h;
}

// after `gs` (the CachedShaderProgram) is resolved:
if (EffectCapture.Enabled)
{
    var vs = gs.Shaders[1];  // 0 = VertexA (rare), 1 = VertexB; adjust if your tree differs
    var fs = gs.Shaders[5];
    DrawManager.HitboxLastVsHash = vs != null ? HitboxFnv(vs.Code) : 0;
    DrawManager.HitboxLastFsHash = fs != null ? HitboxFnv(fs.Code) : 0;
}
```

These hashes won't match the editor's BNSH hashes (different container); they're only for
grouping draws that use the same shader within a capture session.

## Hook 3 — frame image dump (IMPLEMENTED in the mirrored Window.cs)

Only needed for the *visual* golden tier; the cbuf tier works without it.

**Where:** same `Window.Present` as Hook 1, right before
`_context.Renderer.Window.Present(texture.HostTexture, ...)` so the texture is fully
synchronized. The exact call site is in the mirrored `Window.cs` in this directory.

```csharp
if (EffectCapture.FrameDumpDue())   // cheap gate: stride + frame window, skips the readback
{
    try
    {
        using var data = texture.HostTexture.GetData();   // PinnedSpan<byte>, host format
        int w = (int)MathF.Ceiling(texture.Info.Width * texture.ScaleFactor);
        int h = (int)MathF.Ceiling(texture.Info.Height * texture.ScaleFactor);
        EffectCapture.DumpFrame(EffectCapture.CurrentFrame, w, h,
            texture.Format.ToString(), data.Get().ToArray());
    }
    catch (Exception e) { System.Console.Error.WriteLine($"[EffectCapture] present readback failed: {e.Message}"); }
}
```

Output: `frames/frame_%06d_%dx%d_%s.rgba.gz` — gzipped tightly-packed 4bpp pixels, with
the host texture format name (`R8G8B8A8Unorm`, `B8G8R8A8Unorm`, ...) in the filename so
the Rust converter picks the channel order. Compression + IO run on the thread pool.
`HITBOX_CAPTURE_FRAME_EVERY` controls the stride (default 2, i.e. every other presented
frame ≈ 30 dumps/sec; 0 disables frame dumps). Use `HITBOX_CAPTURE_FRAMES=start-end` to
bound disk usage on long sessions.

---

## Sanity check

1. `HITBOX_CAPTURE_DIR=/tmp/cap HITBOX_CAPTURE_FRAMES=300-360 ./Ryujinx <ssbu.nsp>`
2. Get into training mode as Samus, drop a bomb around frame 300+.
3. Expect `~/tmp/cap/draws/frame_0003xx_draw_*.json` files whose `vs_cbufs` has keys
   "8"/"9"/"10" and `fs_cbufs` has "16". Zero files means the particle filter never
   matched — dump unconditionally once to see what banks are actually bound.

---

## Hook 2c — vertex buffer dump (added for eval-chain / per-particle attr RE)

`EffectCapture.DumpDraw` takes an optional `DrawGeometry` payload. In `DrawManager`, pass
the draw parameters from `DrawImpl` into the capture helper and snapshot the drawn window
of every enabled vertex buffer plus the raw `VertexAttribState` words (see the applied
implementation in `HitboxCaptureGeometry` — mirrors `StateUpdater.UpdateVertexBufferState`:
address = `vb.Address.Pack()`, size = `VertexBufferEndAddress - address + 1`, drawn window
= `firstVertex*stride .. +count*stride` for non-indexed draws, read via
`_channel.MemoryManager.GetSpan`, 256 KB/buffer cap).

Dump JSON gains:
```json
"draw_params":   { "vertex_count": N, "first_vertex": F, "indexed": false },
"vertex_attribs": [u32, ...],
"vertex_buffers": { "0": { "stride": 208, "data": "<hex>" } }
```
This is what resolves which per-vertex values the game feeds in attr4.w / attr5.w
(life/eval-time chain) and the billboard position inputs.
