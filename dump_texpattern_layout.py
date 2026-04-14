#!/usr/bin/env python3
"""Dump the TexPatAnim block layout for P_SamusAttackBomb flare1 to understand the actual field layout."""
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

# flare1 emtr_static = 0x8e50
emtr_static = 0x8e50

# The sequential walk reaches TexPatAnim at offset 192 from emtr_static
tex_pat_off = emtr_static + 192
print(f"TexPatAnim[0] starts at {tex_pat_off:#x}")
print(f"First 144 bytes (one TexPatAnim block):")
block = data[tex_pat_off:tex_pat_off+144]
print(f"  hex: {block.hex()}")
print()

# Print as f32 values
print("As f32 values (first 36 floats = 144 bytes):")
for i in range(36):
    val = rf32(tex_pat_off + i*4)
    print(f"  [{i:2d}] offset={tex_pat_off + i*4:#x}: {val:.6f} (raw={r32(tex_pat_off + i*4):#010x})")

print()
# The Switch Toolbox PTCL.cs TexPatAnim layout:
# According to NintendoWare documentation, TexPatAnim has:
# - PatternTable: array of u8 frame indices
# - AnimKeyTable: array of AnimKey entries
# The first fields are likely NOT scaleU/scaleV
# Let's check what makes sense:
# If the first 4 bytes are u32 (not f32):
print("As u32 values (first 8 u32s):")
for i in range(8):
    val = r32(tex_pat_off + i*4)
    print(f"  [{i}] {val:#010x} = {val}")

print()
# Check if the actual UV scale/offset is elsewhere in the TexPatAnim block
# NintendoWare TexPatAnim (144 bytes) layout from Switch Toolbox:
# +0x00: u32 PatternCount
# +0x04: u32 PatternTableOffset (relative)
# +0x08: u32 AnimKeyCount  
# +0x0C: u32 AnimKeyTableOffset (relative)
# +0x10: f32 ScaleU
# +0x14: f32 ScaleV
# +0x18: f32 OffsetU
# +0x1C: f32 OffsetV
# ... rest is pattern/key data
print("Checking if UV scale/offset is at +0x10 in TexPatAnim:")
print(f"  +0x10 (ScaleU?): {rf32(tex_pat_off + 0x10):.6f}")
print(f"  +0x14 (ScaleV?): {rf32(tex_pat_off + 0x14):.6f}")
print(f"  +0x18 (OffsetU?): {rf32(tex_pat_off + 0x18):.6f}")
print(f"  +0x1C (OffsetV?): {rf32(tex_pat_off + 0x1C):.6f}")

print()
# Also check the absolute offset approach - what's at emtr_static + known offsets?
# From Switch Toolbox PTCL.cs, the TexPatAnim table starts at a fixed offset
# Let's check what's at various offsets that might be UV scale
print("Checking various offsets for UV scale values (looking for 1.0 or reasonable UV values):")
for off_delta in range(0, 300, 4):
    val = rf32(emtr_static + off_delta)
    if 0.01 < abs(val) < 10.0 and val != 1.0:
        print(f"  emtr_static+{off_delta:#x} ({off_delta}): {val:.6f}")
