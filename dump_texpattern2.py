#!/usr/bin/env python3
"""Dump TexPatAnim data from the first emitter to understand UV layout.
Uses the same offset logic as the Rust parser."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
try:
    with open(EFF_PATH, 'rb') as f:
        raw = f.read()
except FileNotFoundError:
    print(f"File not found: {EFF_PATH}")
    sys.exit(1)

print(f"File size: {len(raw):#x}")
vfxb_off = raw.find(b'VFXB')
if vfxb_off < 0:
    print("No VFXB found!")
    sys.exit(1)
print(f"VFXB at {vfxb_off:#x}")
data = raw[vfxb_off:]

def r32(off): return struct.unpack_from('<I', data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from('<H', data, off)[0] if off+2<=len(data) else 0
def rf32(off): return struct.unpack_from('<f', data, off)[0] if off+4<=len(data) else 0.0
def r8(off): return data[off] if off<len(data) else 0

NULL = 0xFFFFFFFF
def sec_magic(b): return bytes(data[b:b+4]) if b+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(b): return r32(b+4)
def sec_child_off(b): return r32(b+8)
def sec_next_off(b): return r32(b+0xC)
def sec_bin_off(b): return r32(b+0x14)
def sec_child_cnt(b): return r16(b+0x1C)

block_offset = r16(0x16)
sec = block_offset

# Find ESTA
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA': break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec = sec + nxt

print(f"ESTA at {sec:#x}")
eset = sec + sec_child_off(sec)
print(f"ESET at {eset:#x}")
emtr = eset + sec_child_off(eset)
print(f"EMTR at {emtr:#x}")

emtr_bin = emtr + sec_bin_off(emtr)
name_bytes = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0]
print(f"Emitter name: '{name_bytes.decode()}'")

emtr_static = emtr_bin + 80
base = emtr_static
version = r32(base)
print(f"version = {version}")

# Walk offsets exactly as Rust does
off = base
off += 4   # version
off += 4   # flags
off += 8   # RandomSeed, UserData
off += 8   # EmitterSet name hash + padding
off += 8   # Emitter name hash + padding
off += 8   # GroupID + padding
off += 16  # BoundingBox
off += 16  # CullingRadius + padding
off += 16 + (8 if version > 40 else 0) + 8  # EmitterInheritance

# Emission (72 bytes)
off += 72

# EmitterShapeInfo
off += 8 + 48 + 28 + (8 if version < 40 else 0)

# EmitterRenderState (16 bytes)
mesh_type = r32(off)
primitive_index = r32(off+4)
blend_type = r8(off+6)
print(f"\nmesh_type={mesh_type} primitive_index={primitive_index} blend={blend_type}")
off += 16

# ParticleData
off += 16 + 8 + 24 + 12 + (20 if version < 50 else 10)

# EmitterCombiner (8 bytes)
off += 8

# Fluctuation (40 bytes)
off += 40
off += 8   # Unknown3, Unknown4
# gravity (4 floats)
off += 4*4
off += 4   # AirRes
off += 12  # val_0x74..val_0x82
off += 16  # CenterX/Y, Offset, Padding
off += 32  # Amplitude, Cycle, PhaseRnd, PhaseInit
off += 16  # Coefficient0/1, val_0xB8/BC

tex_pat_count = 5 if version > 40 else 3
tex_pat_size = 144
print(f"\nTexPatAnim: {tex_pat_count} x {tex_pat_size} bytes at offset {off-base} from base ({off:#x})")

for i in range(tex_pat_count):
    pat_off = off + i * tex_pat_size
    print(f"\n  TexPatAnim[{i}] at {pat_off:#x}:")
    raw_bytes = data[pat_off:pat_off+tex_pat_size]
    print(f"    hex: {raw_bytes.hex()}")
    floats = [rf32(pat_off + j*4) for j in range(36)]
    u32s = [r32(pat_off + j*4) for j in range(36)]
    print(f"    f32[0..4]:  {[f'{v:.6f}' for v in floats[0:4]]}")
    print(f"    f32[4..8]:  {[f'{v:.6f}' for v in floats[4:8]]}")
    print(f"    f32[8..12]: {[f'{v:.6f}' for v in floats[8:12]]}")
    print(f"    f32[12..16]:{[f'{v:.6f}' for v in floats[12:16]]}")
    print(f"    u32[0..8]:  {[f'{v:#010x}' for v in u32s[0:8]]}")
    print(f"    u32[8..16]: {[f'{v:#010x}' for v in u32s[8:16]]}")
    # Look for values that look like UV scale (0.25, 0.333, 0.5, etc.)
    interesting = [(j, floats[j]) for j in range(36) 
                   if 0.01 < abs(floats[j]) < 10.0 and abs(floats[j]) != 1.0]
    print(f"    interesting floats: {interesting}")

off += tex_pat_count * tex_pat_size

tex_scroll_count = 5 if version > 40 else 3
tex_scroll_size = 80
print(f"\nTexScrollAnim: {tex_scroll_count} x {tex_scroll_size} bytes at offset {off-base} from base ({off:#x})")
for i in range(tex_scroll_count):
    sc_off = off + i * tex_scroll_size
    floats = [rf32(sc_off + j*4) for j in range(20)]
    interesting = [(j, floats[j]) for j in range(20) 
                   if 0.001 < abs(floats[j]) < 100.0]
    if interesting:
        print(f"  TexScrollAnim[{i}]: {interesting}")
