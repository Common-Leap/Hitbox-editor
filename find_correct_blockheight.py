#!/usr/bin/env python3
"""Try different block heights for texture 11 to find which one produces correct output."""
import struct, sys

try:
    import tegra_swizzle
    HAS_TEGRA = True
except ImportError:
    HAS_TEGRA = False
    print("tegra_swizzle not available, using manual decode")

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

print(f"Texture 11: {width}x{height} data_size={data_size} block_height_log2={block_height_log2}")

# Get raw swizzled data
pts_addr_lo = r32(brti + 0x70)
pts_addr_hi = r32(brti + 0x74)
pts_addr = (pts_addr_hi << 32) | pts_addr_lo
pts_addr_abs = bntx_off + pts_addr
mip0_lo = r32(pts_addr_abs)
mip0_hi = r32(pts_addr_abs + 4)
mip0_ptr = bntx_off + ((mip0_hi << 32) | mip0_lo)
raw_data = bytes(data[mip0_ptr:mip0_ptr + data_size])
print(f"Raw data: {len(raw_data)} bytes, first 8: {raw_data[:8].hex()}")

def decode_bc4_block(block_bytes):
    r0, r1 = block_bytes[0], block_bytes[1]
    indices = int.from_bytes(block_bytes[2:8], 'little')
    if r0 > r1:
        palette = [r0, r1, (6*r0+1*r1)//7, (5*r0+2*r1)//7, (4*r0+3*r1)//7, (3*r0+4*r1)//7, (2*r0+5*r1)//7, (1*r0+6*r1)//7]
    else:
        palette = [r0, r1, (4*r0+1*r1)//5, (3*r0+2*r1)//5, (2*r0+3*r1)//5, (1*r0+4*r1)//5, 0, 255]
    return [palette[(indices >> (i*3)) & 7] for i in range(16)]

def decode_bc5_to_r(swizzled, width, height, block_height):
    """Deswizzle and decode BC5 to R channel values."""
    # Tegra block-linear deswizzle
    # GOB = 64 bytes = 8 rows × 8 bytes (for BC5: 8 bytes per block row)
    # Block height = number of GOBs per tile column
    
    blocks_x = (width + 3) // 4
    blocks_y = (height + 3) // 4
    
    # Tegra swizzle: blocks are arranged in tiles of (block_height × 8) block rows
    # Within a GOB: 8 rows × 8 bytes = 64 bytes
    # BC5 block = 16 bytes, so each GOB row holds 8/16 = 0.5 blocks... 
    # Actually for BC5 (16 bytes/block), bytes_per_block_row = blocks_x * 16
    
    # Simple linear decode (no deswizzle) to check if data is already linear
    r_values = [[0]*width for _ in range(height)]
    for by in range(blocks_y):
        for bx in range(blocks_x):
            block_idx = by * blocks_x + bx
            block_off = block_idx * 16
            if block_off + 16 > len(swizzled):
                continue
            block = swizzled[block_off:block_off+16]
            r_pixels = decode_bc4_block(block[:8])
            for py in range(4):
                for px in range(4):
                    x = bx*4 + px
                    y = by*4 + py
                    if x < width and y < height:
                        r_values[y][x] = r_pixels[py*4+px]
    return r_values

# Try linear decode (no deswizzle) first
print("\n--- Linear decode (no deswizzle) ---")
r = decode_bc5_to_r(raw_data, width, height, 1)
print(f"Top-left 4x4: {[r[y][x] for y in range(4) for x in range(4)]}")
print(f"Center 4x4: {[r[height//2+y][width//2+x] for y in range(4) for x in range(4)]}")
print(f"Corner avg: {sum(r[0][x] for x in range(4))/4:.1f}")
print(f"Center avg: {sum(r[height//2][width//2+x] for x in range(4))/4:.1f}")

# The Rust code uses tegra_swizzle with block_height = 1 << block_height_log2
# Let's check what the Rust code actually does by examining the deswizzle output
# We need to understand the GOB layout for BC5

# For BC5 on Tegra X1:
# - Each block is 16 bytes (4x4 pixels)
# - A GOB is 512 bytes = 8 rows × 64 bytes
# - For BC5 with blocks_x=64 (256/4): bytes_per_block_row = 64*16 = 1024 bytes
# - A GOB holds 512/1024 = 0.5 block rows... that's less than 1
# - So the GOB structure is different for wide textures

# Actually tegra_swizzle handles this correctly. Let me check what block_height
# the Rust code computes vs what's stored in the BRTI.

# The Rust code: block_height = tegra_swizzle::BlockHeight::new(1 << block_height_log2)
# For block_height_log2=2: block_height = 4
# But maybe the correct value is different

# Let's check: for 256x128 BC5, what should block_height be?
# height_in_gobs = ceil(blocks_y / 8) = ceil(32/8) = 4
# block_height_mip0 = min(16, next_pow2(height_in_gobs)) = min(16, 4) = 4
# So block_height=4 seems correct...

# But wait - the BRTI sizeRange field might not be block_height_log2 directly
# It might be the log2 of the block height in GOBs
# Let's try block_height = 1 (no tiling) to see if that gives correct output
print("\n--- Checking if data might be linear (tile_mode=1) ---")
# tile_mode for tex 11
tile_mode = struct.unpack_from('<H', data, brti + 0x12)[0]
print(f"tile_mode = {tile_mode} (0=block-linear, 1=linear)")
if tile_mode == 1:
    print("Texture is LINEAR - no deswizzle needed!")
    print("But Rust code deswizzles it anyway if is_bc=True...")
