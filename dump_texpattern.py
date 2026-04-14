#!/usr/bin/env python3
"""Dump TexPatAnim data from the first emitter to understand UV layout."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
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

eset = sec + sec_child_off(sec)
emtr = eset + sec_child_off(eset)

emtr_bin = emtr + sec_bin_off(emtr)
emtr_static = emtr_bin + 80

name_bytes = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0]
print(f"Emitter: '{name_bytes.decode()}'")
print(f"emtr_static = {emtr_static:#x}")

base = emtr_static
version = r32(base)
print(f"version = {version}")

# Walk to TexPatAnim following the same offset logic as Rust
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

# Emission
off += 72

# EmitterShapeInfo
off += 8 + 48 + 28 + (8 if version < 40 else 0)

# EmitterRenderState
mesh_type = r32(off)
primitive_index = r32(off+4)
blend_type = r8(off+6)
display_side = r8(off+7)
print(f"mesh_type={mesh_type} primitive_index={primitive_index} blend={blend_type} display={display_side}")
off += 16

# ParticleData
off += 16 + 8 + 24 + 12 + (20 if version < 50 else 10)

# EmitterCombiner
off += 8

# Fluctuation
off += 40
off += 8
# gravity
off += 4*4
off += 4   # AirRes
off += 12  # val_0x74..val_0x82
off += 16  # CenterX/Y, Offset, Padding
off += 32  # Amplitude, Cycle, PhaseRnd, PhaseInit
off += 16  # Coefficient0/1, val_0xB8/BC

tex_pat_count = 5 if version > 40 else 3
tex_pat_size = 144
print(f"\nTexPatAnim: {tex_pat_count} entries x {tex_pat_size} bytes each")
print(f"TexPatAnim starts at offset {off - base} from base ({off:#x})")

for i in range(tex_pat_count):
    pat_off = off + i * tex_pat_size
    print(f"\n  TexPatAnim[{i}] at {pat_off:#x} (offset {pat_off-base} from base):")
    # Dump raw bytes
    raw_bytes = data[pat_off:pat_off+tex_pat_size]
    print(f"    raw: {raw_bytes[:32].hex()} ...")
    # Try to interpret as floats
    floats = [rf32(pat_off + j*4) for j in range(36)]
    print(f"    f32[0..8]: {[f'{v:.4f}' for v in floats[:8]]}")
    print(f"    f32[8..16]: {[f'{v:.4f}' for v in floats[8:16]]}")
    print(f"    f32[16..24]: {[f'{v:.4f}' for v in floats[16:24]]}")
    # Try to find non-zero values
    nonzero = [(j, floats[j]) for j in range(36) if abs(floats[j]) > 0.001 and abs(floats[j]) < 1000]
    print(f"    non-zero floats: {nonzero[:10]}")
    # Also check u32 values
    u32s = [r32(pat_off + j*4) for j in range(36)]
    nonzero_u32 = [(j, u32s[j]) for j in range(36) if u32s[j] != 0 and u32s[j] < 0x10000]
    print(f"    non-zero u32 (small): {nonzero_u32[:10]}")

off += tex_pat_count * tex_pat_size

tex_scroll_count = 5 if version > 40 else 3
tex_scroll_size = 80
print(f"\nTexScrollAnim starts at offset {off - base} from base ({off:#x})")
for i in range(tex_scroll_count):
    sc_off = off + i * tex_scroll_size
    raw_bytes = data[sc_off:sc_off+tex_scroll_size]
    floats = [rf32(sc_off + j*4) for j in range(20)]
    nonzero = [(j, floats[j]) for j in range(20) if abs(floats[j]) > 0.001 and abs(floats[j]) < 1000]
    if nonzero:
        print(f"  TexScrollAnim[{i}]: non-zero floats: {nonzero}")
