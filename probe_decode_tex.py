#!/usr/bin/env python3
"""Decode ef_cmn_grade00 texture and check if it's black."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

bntx_base = next((i for i in range(len(data)-4) if data[i:i+4] == b'BNTX'), None)
nx = bntx_base + 0x20
data_blk_abs = nx + 0x10 + r32(data, nx + 0x10)
scan_end = data_blk_abs

brti_offsets = []
pos = bntx_base
while pos + 4 <= scan_end:
    if data[pos:pos+4] == b'BRTI':
        brti_offsets.append(pos)
        brti_len = r32(data, pos + 4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

str_names = []
str_pos = bntx_base
while str_pos + 4 <= len(data):
    if data[str_pos:str_pos+4] == b'_STR':
        str_count = r32(data, str_pos + 16)
        soff = str_pos + 20
        for _ in range(min(str_count, 512)):
            if soff + 2 > len(data): break
            slen = r16(data, soff); soff += 2
            if soff + slen > len(data): break
            s = data[soff:soff+slen].decode('utf-8', errors='replace')
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 1

# Find ef_cmn_grade00 (index 11)
target = 'ef_cmn_grade00'
idx = str_names.index(target)
brti = brti_offsets[idx]

fmt_raw = r32(data, brti + 0x1C)
w = r32(data, brti + 0x24)
h = r32(data, brti + 0x28)
data_size = r32(data, brti + 0x50)
block_height_log2 = r32(data, brti + 0x34)
tile_mode = r16(data, brti + 0x12)
comp_sel = r32(data, brti + 0x58)

print(f"Texture: {target} [{idx}]")
print(f"  Size: {w}x{h}")
print(f"  Format: {fmt_raw:#06x} (BC5 unorm)")
print(f"  tile_mode: {tile_mode} (0=block-linear, 1=linear)")
print(f"  block_height_log2: {block_height_log2}")
print(f"  comp_sel: {comp_sel:#010x}")
print(f"  data_size: {data_size}")

# Get pixel data
pts_addr_lo = r32(data, brti + 0x70)
pts_addr_hi = r32(data, brti + 0x74)
pts_addr = (pts_addr_hi << 32 | pts_addr_lo)
pts_addr_abs = bntx_base + pts_addr
mip0_lo = r32(data, pts_addr_abs)
mip0_hi = r32(data, pts_addr_abs + 4)
mip0_rel = (mip0_hi << 32 | mip0_lo)
pixel_start = bntx_base + mip0_rel

print(f"  pixel_start: {pixel_start:#x}")
print(f"  First 32 bytes (raw swizzled): {data[pixel_start:pixel_start+32].hex()}")

# Try to deswizzle using tegra_swizzle via Python
# We'll use the same logic as the Rust code
# For BC5: block_dim = 4x4, bpp = 16 bytes/block
# block_height = 1 << block_height_log2

# Check if tegra_swizzle Python binding is available
try:
    import tegra_swizzle
    print("tegra_swizzle available")
except ImportError:
    print("tegra_swizzle not available, checking raw data")

# Check the raw BC5 blocks
# BC5 block: 8 bytes R channel + 8 bytes G channel
# R block: R0 (1 byte), R1 (1 byte), 6 bytes indices
# If R0=0, R1=0, all pixels have R=0

mip0_size = ((w+3)//4) * 16 * ((h+3)//4)
raw_data = data[pixel_start:pixel_start+mip0_size]
print(f"\nMip0 size: {mip0_size} bytes")
print(f"Raw data length: {len(raw_data)}")

# Check first few BC5 blocks (after deswizzle, but we can check raw)
# For block-linear, the first GOB (512 bytes) contains the first 8 rows of blocks
# Each GOB row is 64 bytes = 4 BC5 blocks
# The first block is at offset 0 in the first GOB

# Check if the raw data has non-zero R values
non_zero_r = sum(1 for i in range(0, min(len(raw_data), 1024), 16) if raw_data[i] != 0)
print(f"Non-zero R0 values in first 64 blocks: {non_zero_r}/64")

# Print first 10 BC5 blocks
print("\nFirst 10 BC5 blocks (raw, may be swizzled):")
for i in range(min(10, len(raw_data)//16)):
    block = raw_data[i*16:(i+1)*16]
    r0, r1 = block[0], block[1]
    g0, g1 = block[8], block[9]
    print(f"  Block {i}: R0={r0} R1={r1} G0={g0} G1={g1}")

# The key question: after deswizzle, what are the R values?
# For block-linear with block_height_log2=4, the GOB layout is complex
# But we can check if the raw data has ANY non-zero values
non_zero_bytes = sum(1 for b in raw_data[:1024] if b != 0)
print(f"\nNon-zero bytes in first 1024 bytes: {non_zero_bytes}/1024")
print(f"First 64 bytes: {raw_data[:64].hex()}")
