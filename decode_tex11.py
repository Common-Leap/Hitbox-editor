#!/usr/bin/env python3
"""Decode texture 11 (BC5 flash01) and check if R channel has transparent edges."""
import struct, subprocess, sys, os

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]

# Find BNTX and scan for BRTI[11]
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

print(f"Found {len(brti_list)} BRTI, BRTD at {brtd_off:#x}")

# Get texture 11 info
brti = brti_list[11]
fmt_raw = r32(brti + 0x1C)
width = r32(brti + 0x24)
height = r32(brti + 0x28)
data_size = r32(brti + 0x50)
comp_sel = r32(brti + 0x58)
fmt_type = (fmt_raw >> 8) & 0xFF

print(f"Texture 11: {width}x{height} fmt={fmt_type:#04x} data_size={data_size}")

# Find the pixel data using mip0_ptr
pts_addr_lo = r32(brti + 0x70)
pts_addr_hi = r32(brti + 0x74)
pts_addr = (pts_addr_hi << 32) | pts_addr_lo
pts_addr_abs = bntx_off + pts_addr
mip0_lo = r32(pts_addr_abs)
mip0_hi = r32(pts_addr_abs + 4)
mip0_ptr = bntx_off + ((mip0_hi << 32) | mip0_lo)
print(f"mip0_ptr={mip0_ptr:#x}")

# BC5 block: 16 bytes per 4x4 block
# First block covers pixels (0,0)-(3,3) = top-left corner
# Last block covers pixels (252,124)-(255,127) = bottom-right corner
# Middle block covers center

bc5_data_start = mip0_ptr
blocks_x = (width + 3) // 4
blocks_y = (height + 3) // 4

def decode_bc4_block(block_bytes):
    """Decode a BC4 block (8 bytes) to 4x4 R values."""
    r0, r1 = block_bytes[0], block_bytes[1]
    indices = int.from_bytes(block_bytes[2:8], 'little')
    
    if r0 > r1:
        palette = [r0, r1,
                   (6*r0 + 1*r1) // 7, (5*r0 + 2*r1) // 7,
                   (4*r0 + 3*r1) // 7, (3*r0 + 4*r1) // 7,
                   (2*r0 + 5*r1) // 7, (1*r0 + 6*r1) // 7]
    else:
        palette = [r0, r1,
                   (4*r0 + 1*r1) // 5, (3*r0 + 2*r1) // 5,
                   (2*r0 + 3*r1) // 5, (1*r0 + 4*r1) // 5,
                   0, 255]
    
    pixels = []
    for i in range(16):
        idx = (indices >> (i * 3)) & 0x7
        pixels.append(palette[idx])
    return pixels

def get_block_r_values(block_x, block_y):
    """Get R channel values for a 4x4 block."""
    block_idx = block_y * blocks_x + block_x
    block_off = bc5_data_start + block_idx * 16
    if block_off + 16 > len(data):
        return [0] * 16
    block = data[block_off:block_off+16]
    # BC5 = two BC4 blocks: first 8 bytes = R channel, next 8 bytes = G channel
    r_pixels = decode_bc4_block(block[:8])
    return r_pixels

# Check corners and center
print("\nR channel values (0=transparent, 255=opaque):")
print("Top-left corner (block 0,0):", get_block_r_values(0, 0))
print("Top-right corner:", get_block_r_values(blocks_x-1, 0))
print("Bottom-left corner:", get_block_r_values(0, blocks_y-1))
print("Bottom-right corner:", get_block_r_values(blocks_x-1, blocks_y-1))
print("Center:", get_block_r_values(blocks_x//2, blocks_y//2))

# Check if edges are transparent
corner_r = get_block_r_values(0, 0)
center_r = get_block_r_values(blocks_x//2, blocks_y//2)
print(f"\nCorner avg R: {sum(corner_r)/len(corner_r):.1f}")
print(f"Center avg R: {sum(center_r)/len(center_r):.1f}")
if sum(corner_r)/len(corner_r) < 50 and sum(center_r)/len(center_r) > 100:
    print("→ Texture has transparent edges (radial gradient) — shape should work!")
elif sum(corner_r)/len(corner_r) > 200:
    print("→ Texture is fully opaque at corners — will appear as solid square")
else:
    print("→ Mixed — partial transparency at edges")
