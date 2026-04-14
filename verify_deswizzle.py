#!/usr/bin/env python3
"""Verify the deswizzle output for texture 11 by checking if decoded pixels match expected pattern."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]

# Find BNTX and BRTI[11]
bntx_off = data.find(b'BNTX')
brti_list = []
pos = bntx_off
brtd_off = None
while pos + 4 <= len(data):
    if data[pos:pos+4] == b'BRTD':
        brtd_off = pos
        break
    if data[pos:pos+4] == b'BRTI':
        brti_size = r32(pos + 4)
        brti_list.append(pos)
        step = brti_size if brti_size >= 0x90 else 0x90
        pos += step
    else:
        pos += 8

brti = brti_list[11]
width = r32(brti + 0x24)
height = r32(brti + 0x28)
data_size = r32(brti + 0x50)
block_height_log2 = r32(brti + 0x34)
tile_mode = struct.unpack_from('<H', data, brti + 0x12)[0]

print(f"Texture 11: {width}x{height} data_size={data_size}")
print(f"tile_mode={tile_mode} block_height_log2={block_height_log2}")
print(f"block_height = 1 << {block_height_log2} = {1 << block_height_log2}")

# Get mip0 data using mip0_ptr
pts_addr_lo = r32(brti + 0x70)
pts_addr_hi = r32(brti + 0x74)
pts_addr = (pts_addr_hi << 32) | pts_addr_lo
pts_addr_abs = bntx_off + pts_addr
mip0_lo = r32(pts_addr_abs)
mip0_hi = r32(pts_addr_abs + 4)
mip0_ptr = bntx_off + ((mip0_hi << 32) | mip0_lo)
print(f"mip0_ptr={mip0_ptr:#x} (relative to vfxb start)")

# The Rust code uses the GRTF sub-slice, so bntx_off in Rust is relative to GRTF bin start
# GRTF bin start = GRTF section + bin_off
grtf_off = data.find(b'GRTF')
grtf_bin_off = r32(grtf_off + 0x14)
grtf_bin_start = grtf_off + grtf_bin_off
print(f"GRTF bin start = {grtf_bin_start:#x}")
print(f"BNTX in GRTF sub-slice at offset {bntx_off - grtf_bin_start:#x}")

# In Rust, bntx_base = position of BNTX within the GRTF sub-slice
bntx_in_slice = bntx_off - grtf_bin_start
print(f"bntx_base in Rust = {bntx_in_slice:#x}")

# The mip0_ptr in Rust would be: bntx_base + pts_addr_value + mip0_offset_value
# pts_addr is relative to bntx_base in the file
rust_pts_addr_abs = bntx_in_slice + pts_addr
rust_mip0_ptr = bntx_in_slice + ((mip0_hi << 32) | mip0_lo)
print(f"Rust mip0_ptr = {rust_mip0_ptr:#x} (in GRTF sub-slice)")

# Check if this is within the GRTF sub-slice bounds
grtf_size = r32(grtf_off + 4)
print(f"GRTF section size = {grtf_size:#x}")
print(f"mip0_ptr in bounds: {rust_mip0_ptr < grtf_size}")

# The actual pixel data starts at mip0_ptr in the GRTF sub-slice
# Let's check the first few bytes
actual_pixel_start = grtf_bin_start + rust_mip0_ptr
print(f"\nFirst 32 bytes at mip0 (absolute): {data[actual_pixel_start:actual_pixel_start+32].hex()}")

# For comparison, check what's at the BRTD data start
brtd_data_start = brtd_off + 0x10  # BRTD header is 16 bytes
print(f"BRTD data start: {brtd_data_start:#x}")
# Find texture 11's offset in BRTD using sequential cursor
cursor = 0
for i in range(11):
    b = brti_list[i]
    ds = r32(b + 0x50)
    cursor = (cursor + ds + 0x1FF) & ~0x1FF
print(f"Sequential cursor for tex 11: {cursor:#x}")
print(f"BRTD sequential start for tex 11: {brtd_data_start + cursor:#x}")
print(f"First 32 bytes at BRTD sequential: {data[brtd_data_start + cursor:brtd_data_start + cursor + 32].hex()}")
