// EffectCapture.cs — self-contained NVN constant-buffer + framebuffer dumper for Ryujinx.
//
// Part of the Hitbox editor capture-diff harness (see tools/ryujinx-capture/README.md).
// Drop this file anywhere under src/Ryujinx.Graphics.Gpu/ and add the call sites described
// in INTEGRATION.md. It has zero dependencies beyond the base class library (System.Text.Json
// ships with .NET), so it compiles unmodified against any recent Ryujinx / Ryubing tree.
//
// Everything is gated on the HITBOX_CAPTURE_DIR environment variable: unset means every
// entry point is a no-op and retail behaviour is untouched.
//
// Env vars:
//   HITBOX_CAPTURE_DIR     output directory (created if missing). Unset = disabled.
//   HITBOX_CAPTURE_FRAMES  optional "start-end" inclusive frame window, e.g. "300-420".
//                          Outside the window nothing is written. Default: all frames.
//   HITBOX_CAPTURE_MAX_DRAWS  optional cap on draw dumps per frame (default 512).
//   HITBOX_CAPTURE_FRAME_EVERY  dump every Nth presented frame (default 2; 0 disables
//                               frame dumps entirely, draw dumps are unaffected).
//
// Output layout under HITBOX_CAPTURE_DIR:
//   draws/frame_%06d_draw_%04d.json   one JSON per captured draw (cbufs as hex strings)
//   frames/frame_%06d_%dx%d_%s.rgba.gz  gzipped tightly-packed 4bpp color target;
//                                       %s = host texture format (e.g. R8G8B8A8Unorm)
//
// PNG encoding, float decoding and golden generation all happen on the Rust side
// (examples/ryujinx_to_goldens.rs) — this class only moves bytes to disk.

using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Text.Json;
using System.Threading;

namespace Ryujinx.Graphics.Gpu
{
    public static class EffectCapture
    {
        private static readonly string _dir = Environment.GetEnvironmentVariable("HITBOX_CAPTURE_DIR");
        private static readonly int _frameStart;
        private static readonly int _frameEnd;
        private static readonly int _maxDrawsPerFrame;
        private static readonly int _frameEvery;

        private static long _frame;          // advanced by OnPresent
        private static int _drawInFrame;     // reset each frame
        private static readonly object _ioLock = new();

        public static bool Enabled => _dir != null;

        /// <summary>Last-bound shader identities, stashed by StateUpdater (Hook 2b).</summary>
        public static ulong LastVsHash;
        public static ulong LastFsHash;

        public static ulong Fnv1a(byte[] data)
        {
            if (data == null) return 0;
            ulong h = 0xcbf29ce484222325;
            foreach (byte b in data) { h ^= b; h *= 0x100000001b3; }
            return h;
        }

        static EffectCapture()
        {
            _frameStart = 0;
            _frameEnd = int.MaxValue;
            string window = Environment.GetEnvironmentVariable("HITBOX_CAPTURE_FRAMES");
            if (window != null)
            {
                string[] parts = window.Split('-', 2);
                if (parts.Length == 2 &&
                    int.TryParse(parts[0], out int s) &&
                    int.TryParse(parts[1], out int e))
                {
                    _frameStart = s;
                    _frameEnd = e;
                }
            }

            _maxDrawsPerFrame = 512;
            if (int.TryParse(Environment.GetEnvironmentVariable("HITBOX_CAPTURE_MAX_DRAWS"), out int cap) && cap > 0)
            {
                _maxDrawsPerFrame = cap;
            }

            _frameEvery = 2;
            if (int.TryParse(Environment.GetEnvironmentVariable("HITBOX_CAPTURE_FRAME_EVERY"), out int every) && every >= 0)
            {
                _frameEvery = every;
            }

            if (Enabled)
            {
                Directory.CreateDirectory(Path.Combine(_dir, "draws"));
                Directory.CreateDirectory(Path.Combine(_dir, "frames"));
                Console.Error.WriteLine($"[EffectCapture] enabled, writing to {_dir} (frames {_frameStart}-{_frameEnd})");
            }
        }

        private static bool InWindow(long frame) => frame >= _frameStart && frame <= _frameEnd;

        /// <summary>
        /// Call once per presented frame (see INTEGRATION.md). Returns the new frame index
        /// so the caller can pass it to DumpFrame for the same frame.
        /// </summary>
        public static long OnPresent()
        {
            Interlocked.Exchange(ref _drawInFrame, 0);
            return Interlocked.Increment(ref _frame);
        }

        public static long CurrentFrame => Interlocked.Read(ref _frame);

        /// <summary>
        /// Dump one draw call's shader identity, render state and constant buffers.
        /// All arguments are plain data so the call site owns every Ryujinx-API interaction.
        /// vsCbufs/fsCbufs: Maxwell constant-bank index → raw buffer bytes (whatever length
        /// the caller reads; the Rust converter chunks it into float4 slots).
        /// </summary>
        /// <summary>Per-draw vertex geometry payload (Hook 2, optional).</summary>
        public sealed class DrawGeometry
        {
            public int VertexCount;
            public int FirstVertex;
            public bool Indexed;
            public uint[] VertexAttribs;                       // raw Maxwell attrib words
            public Dictionary<int, (int Stride, byte[] Data)> Buffers;  // vb index -> drawn window
        }

        public static void DumpDraw(
            ulong vsHash,
            ulong fsHash,
            string blend,
            IReadOnlyList<string> textures,
            IReadOnlyDictionary<int, byte[]> vsCbufs,
            IReadOnlyDictionary<int, byte[]> fsCbufs,
            DrawGeometry geometry = null)
        {
            if (!Enabled) return;
            long frame = CurrentFrame;
            if (!InWindow(frame)) return;

            int drawIndex = Interlocked.Increment(ref _drawInFrame) - 1;
            if (drawIndex >= _maxDrawsPerFrame) return;

            var doc = new Dictionary<string, object>
            {
                ["frame"] = frame,
                ["draw"] = drawIndex,
                ["vs_hash"] = vsHash.ToString("x16"),
                ["fs_hash"] = fsHash.ToString("x16"),
                ["blend"] = blend ?? "",
                ["textures"] = textures ?? Array.Empty<string>(),
                ["vs_cbufs"] = HexMap(vsCbufs),
                ["fs_cbufs"] = HexMap(fsCbufs),
            };

            if (geometry != null)
            {
                doc["draw_params"] = new Dictionary<string, object>
                {
                    ["vertex_count"] = geometry.VertexCount,
                    ["first_vertex"] = geometry.FirstVertex,
                    ["indexed"] = geometry.Indexed,
                };
                if (geometry.VertexAttribs != null)
                {
                    doc["vertex_attribs"] = geometry.VertexAttribs;
                }
                if (geometry.Buffers != null)
                {
                    var vbs = new Dictionary<string, object>();
                    foreach (var (index, (stride, data)) in geometry.Buffers)
                    {
                        if (data == null || data.Length == 0) continue;
                        vbs[index.ToString()] = new Dictionary<string, object>
                        {
                            ["stride"] = stride,
                            ["data"] = Convert.ToHexString(data).ToLowerInvariant(),
                        };
                    }
                    doc["vertex_buffers"] = vbs;
                }
            }

            string path = Path.Combine(_dir, "draws", $"frame_{frame:D6}_draw_{drawIndex:D4}.json");
            byte[] json = JsonSerializer.SerializeToUtf8Bytes(doc, new JsonSerializerOptions { WriteIndented = true });
            lock (_ioLock)
            {
                File.WriteAllBytes(path, json);
            }
        }

        /// <summary>
        /// Cheap pre-check for the Present hook: true when the current frame should be
        /// dumped, so the caller can skip the (expensive) host texture readback entirely
        /// on the frames in between.
        /// </summary>
        public static bool FrameDumpDue()
        {
            long f = CurrentFrame;
            return Enabled && _frameEvery > 0 && InWindow(f) && (f % _frameEvery == 0);
        }

        /// <summary>
        /// Dump the presented color target as gzipped tightly-packed 4bpp pixels
        /// (stride = width*4). <paramref name="format"/> is the host texture format name
        /// (e.g. "R8G8B8A8Unorm", "B8G8R8A8Unorm") and is baked into the filename so the
        /// Rust converter can pick the right channel order. Compression and file IO run
        /// on the thread pool to keep the GPU thread moving.
        /// </summary>
        public static void DumpFrame(long frame, int width, int height, string format, byte[] pixels)
        {
            if (!Enabled || !InWindow(frame) || pixels == null || pixels.Length == 0) return;

            string path = Path.Combine(_dir, "frames", $"frame_{frame:D6}_{width}x{height}_{format}.rgba.gz");
            ThreadPool.QueueUserWorkItem(static state =>
            {
                var (p, bytes) = ((string, byte[]))state;
                try
                {
                    using FileStream fs = File.Create(p);
                    using GZipStream gz = new(fs, CompressionLevel.Fastest);
                    gz.Write(bytes, 0, bytes.Length);
                }
                catch (Exception e)
                {
                    Console.Error.WriteLine($"[EffectCapture] frame dump failed: {e.Message}");
                }
            }, (path, pixels));
        }

        private static Dictionary<string, string> HexMap(IReadOnlyDictionary<int, byte[]> cbufs)
        {
            var map = new Dictionary<string, string>();
            if (cbufs == null) return map;
            foreach (var (slot, bytes) in cbufs)
            {
                if (bytes != null && bytes.Length > 0)
                {
                    map[slot.ToString()] = Convert.ToHexString(bytes).ToLowerInvariant();
                }
            }
            return map;
        }
    }
}
