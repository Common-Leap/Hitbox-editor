#!/usr/bin/env python3
"""Dump scale and scale_random for all emitters to diagnose size issue."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
try:
    with open(EFF_PATH, 'rb') as f: raw = f.read()
except FileNotFoundError:
    print(f"File not found: {EFF_PATH}")
    sys.exit(1)

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def rstr(d, off, maxlen=64):
    if off == 0 or off >= len(d): return ''
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
vfx_version = r16(data, 0x0A)
block_offset = r16(data, 0x16)
print(f"VFXB version={vfx_version}, block_offset={block_offset:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

def get_scale_random(base, version):
    """Walk to ParticleScale block and return scale_x, scale_y, scale_random."""
    off = base
    off += 16  # Flags
    off += 24  # NumKeys
    off += 8   # Unknown1/2
    if version > 50: off += 16
    off += 40  # LoopRates
    off += 8   # Unknown3/4
    off += 16  # gravity (4 floats)
    off += 4   # AirRes
    off += 12  # val_0x74
    off += 16  # CenterXY
    off += 32  # Amplitude
    off += 16  # Coefficient

    tex_pat_count = 5 if version > 40 else 3
    off += tex_pat_count * 144  # TexPatAnim
    off += (5 if version > 40 else 3) * 80  # TexScrollAnim
    off += 16        # ColorScale
    off += 128 * 4   # Color tables
    off += 32        # SoftEdge
    off += 16        # Decal
    off += 16        # AddVelToScale
    off += 128       # ScaleAnim
    off += 128       # ParamAnim
    if version > 50: off += 512
    if version > 40: off += 64
    off += 64        # RotateInit/Rand/Add/Regist
    off += 16        # ScaleLimitDist
    if version > 40: off += 64

    # EmitterInfo
    off += 16  # IsParticleDraw
    off += 16  # RandomSeed
    off += 12  # Trans
    off += 12  # TransRand
    off += 12  # Rotate
    off += 12  # RotateRand
    off += 12  # Scale (emitter scale)
    off += 32  # Color0+Color1 RGBA
    off += 12  # EmissionRange
    off += 16 + (8 if version > 40 else 0) + 8  # EmitterInheritance

    # Emission
    off += 72

    # EmitterShapeInfo
    off += 8 + 48 + 28 + (8 if version < 40 else 0)

    # EmitterRenderState
    off += 16

    # ParticleData
    off += 16 + 8 + 24 + 12 + (20 if version < 50 else 10)

    # EmitterCombiner
    if version < 36: off += 24
    elif version == 36: off += 8
    elif version < 50: off += 24
    else: off += 28

    # ShaderRefInfo
    off += 4 + 20
    if version < 50: off += 16
    if version < 22: off += 8
    off += 8
    if version > 50: off += 8
    off += 32

    # ActionInfo
    off += 4 + (20 if version > 40 else 0)
    if version > 40: off += 16 + 52

    # ParticleVelocityInfo
    off += 48
    if version >= 36: off += 16

    # ParticleColor (44 bytes) then ParticleScale
    off += 44
    scale_x = rf32(data, off)
    scale_y = rf32(data, off + 4)
    scale_random = rf32(data, off + 8)

    # Direct reads
    scale_x_direct = rf32(data, base + 0x2E0) if version > 22 else 0.0
    scale_y_direct = rf32(data, base + 0x2E4) if version > 22 else 0.0

    if scale_x_direct > 0: final_scale = scale_x_direct
    elif scale_y_direct > 0: final_scale = scale_y_direct
    else: final_scale = max(scale_x, scale_y)

    return scale_x, scale_y, scale_random, scale_x_direct, scale_y_direct, final_scale

# Walk sections
sec = block_offset
count = 0
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        esta_child_cnt = sec_child_cnt(sec)
        eset_base = sec + sec_child_off(sec)
        for _ in range(esta_child_cnt):
            if eset_base + 4 > len(data): break
            if sec_magic(eset_base) != b'ESET': break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = rstr(data, eset_bin + 16)

            emtr_base = eset_base + sec_child_off(eset_base)
            eset_child_cnt = sec_child_cnt(eset_base)
            for ei in range(eset_child_cnt):
                if emtr_base + 4 > len(data): break
                if sec_magic(emtr_base) != b'EMTR': break
                emtr_bin = emtr_base + sec_bin_off(emtr_base)
                emtr_static = emtr_bin + 80
                emtr_name = rstr(data, emtr_bin + 16)
                sx, sy, sr, sxd, syd, fs = get_scale_random(emtr_static, vfx_version)
                if sr > 0.01 or count < 10:
                    print(f"[{count:3d}] '{set_name}'/'{emtr_name}': scale={fs:.3f} (walk_x={sx:.3f} walk_y={sy:.3f} direct_x={sxd:.3f}) scale_random={sr:.4f}")
                count += 1
                nxt = sec_next_off(emtr_base)
                if nxt == NULL: break
                emtr_base += nxt

            nxt = sec_next_off(eset_base)
            if nxt == NULL: break
            eset_base += nxt
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec += nxt

print(f"\nTotal emitters scanned: {count}")
