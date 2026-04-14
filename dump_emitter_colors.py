#!/usr/bin/env python3
"""Dump the actual color values being read by the Rust sequential walk for P_SamusAttackBomb emitters."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]
def rf32(off):
    if off + 4 > len(data): return 0.0
    return struct.unpack_from('<f', data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from('<H', data, off)[0]
def r8(off):
    if off >= len(data): return 0
    return data[off]

def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_next_off(base): return r32(base + 0xC)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_off(base): return r32(base + 0x08)
def sec_child_cnt(base): return r16(base + 0x1C)

block_offset = r16(0x16)
version = r16(0x0A)
print(f"VFXB version={version:#x}")

sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        break
    nxt = sec_next_off(sec)
    if nxt == 0xFFFFFFFF: break
    sec = sec + nxt

eset_base = sec + sec_child_off(sec)
while eset_base + 4 <= len(data):
    eset_bin = eset_base + sec_bin_off(eset_base)
    eset_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
    if eset_name == 'P_SamusAttackBomb':
        break
    nxt = sec_next_off(eset_base)
    if nxt == 0xFFFFFFFF: break
    eset_base = eset_base + nxt

print(f"Found P_SamusAttackBomb at {eset_base:#x}")

emtr_base = eset_base + sec_child_off(eset_base)
while emtr_base + 4 <= len(data):
    emtr_bin = emtr_base + sec_bin_off(emtr_base)
    emtr_name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
    emtr_static = emtr_bin + 80

    print(f"\n=== EMTR '{emtr_name}' (static={emtr_static:#x}) ===")

    # Simulate the sequential walk to find where emitter_color0 lands
    off = emtr_static
    off += 16  # Flags
    off += 24  # NumColor0Keys..NumParamKeys
    off += 8   # Unknown1, Unknown2
    # version=22, not > 50
    off += 40  # LoopRates
    off += 8   # Unknown3, Unknown4
    off += 4   # gravity_x
    off += 4   # gravity_y
    off += 4   # gravity_z
    off += 4   # gravity_scale
    off += 4   # AirRes
    off += 12  # val_0x74..val_0x82
    off += 16  # CenterX/Y, Offset, Padding
    off += 32  # Amplitude, Cycle, PhaseRnd, PhaseInit
    off += 16  # Coefficient0/1, val_0xB8/BC

    # TexPatAnim: version=22, not > 40, so tex_pat_count=3
    tex_pat_count = 3
    print(f"  TexPatAnim[0] at {off:#x}: scaleU={rf32(off):.4f} scaleV={rf32(off+4):.4f} offsetU={rf32(off+8):.4f} offsetV={rf32(off+12):.4f}")
    off += tex_pat_count * 144

    # TexScrollAnim: 3 * 80
    print(f"  TexScrollAnim[0] at {off:#x}: scrollU={rf32(off):.4f} scrollV={rf32(off+4):.4f}")
    off += 3 * 80

    off += 16       # ColorScale + 3 floats
    off += 128 * 4  # Color0/Alpha0/Color1/Alpha1 tables
    off += 32       # SoftEdge..FarDistAlpha
    off += 16       # Decal + AlphaThreshold + Padding
    off += 16       # AddVelToScale..Padding3
    off += 128      # ScaleAnim
    off += 128      # ParamAnim
    # version=22, not > 50, not > 40
    off += 64       # RotateInit/Rand/Add/Regist
    off += 16       # ScaleLimitDist
    # not > 40

    # EmitterInfo
    off += 16  # IsParticleDraw..padding3
    off += 16  # RandomSeed, DrawPath, AlphaFadeTime, FadeInTime
    # Trans
    print(f"  EmitterInfo Trans at {off:#x}: x={rf32(off):.4f} y={rf32(off+4):.4f} z={rf32(off+8):.4f}")
    off += 12  # Trans
    off += 12  # TransRand
    # Rotate
    off += 12  # Rotate
    off += 12  # RotateRand
    # Scale
    off += 12  # Scale
    # Color0 RGBA
    color0_walk_off = off
    print(f"  EmitterInfo Color0 (walk) at {color0_walk_off:#x}: r={rf32(off):.4f} g={rf32(off+4):.4f} b={rf32(off+8):.4f} a={rf32(off+12):.4f}")
    # Color1 RGBA
    print(f"  EmitterInfo Color1 (walk) at {off+16:#x}: r={rf32(off+16):.4f} g={rf32(off+20):.4f} b={rf32(off+24):.4f} a={rf32(off+28):.4f}")

    # Compare with absolute offsets
    abs_2384 = emtr_static + 2384
    abs_2392 = emtr_static + 2392
    print(f"  EmitterInfo Color0 (abs 2384): r={rf32(abs_2384):.4f} g={rf32(abs_2384+4):.4f} b={rf32(abs_2384+8):.4f} a={rf32(abs_2384+12):.4f}")
    print(f"  EmitterInfo Color0 (abs 2392): r={rf32(abs_2392):.4f} g={rf32(abs_2392+4):.4f} b={rf32(abs_2392+8):.4f} a={rf32(abs_2392+12):.4f}")
    print(f"  Walk offset vs abs 2384: diff={color0_walk_off - emtr_static} (expected 2384 for v22)")

    # Also check color0 animation keys
    num_c0 = r32(emtr_static + 16)
    c0_off = emtr_static + 880
    print(f"  num_color0_keys={num_c0}")
    if num_c0 > 0:
        print(f"  color0[0] (R,G,B,time): r={rf32(c0_off):.4f} g={rf32(c0_off+4):.4f} b={rf32(c0_off+8):.4f} t={rf32(c0_off+12):.4f}")

    nxt = sec_next_off(emtr_base)
    if nxt == 0xFFFFFFFF: break
    emtr_base = emtr_base + nxt
