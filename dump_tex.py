#!/usr/bin/env python3
"""Dump texture info and save a few textures as raw data for inspection."""
import struct, os

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

def r32(d, off): return struct.unpack_from("<I", d, off)[0] if off+4<=len(d) else 0
def r16(d, off): return struct.unpack_from("<H", d, off)[0] if off+2<=len(d) else 0

block_offset = r16(data, 0x16)
NULL = 0xFFFFFFFF
def sec_next_off(base): return r32(data, base+0x0C)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'

sec = block_offset
while sec+4 <= len(data):
    if sec_magic(sec) == b"GRTF": break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

bntx_start = sec + sec_bin_off(sec)
bd = data[bntx_start:]
bntx_base = bd.find(b"BNTX")
bd = bd[bntx_base:]

nx_off = 0x20
data_blk_rel = r32(bd, nx_off+0x10)
data_blk_abs = nx_off + 0x10 + data_blk_rel
brtd_data_start = data_blk_abs + 0x10

# Find BRTI structs
brti_offsets = []
pos = 0
while pos+4 <= data_blk_abs:
    if bd[pos:pos+4] == b"BRTI":
        brti_offsets.append(pos)
        brti_len = r32(bd, pos+4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

# Find _STR names
str_names = []
str_pos = 0
while str_pos+4 <= data_blk_abs:
    if bd[str_pos:str_pos+4] == b"_STR":
        str_count = r32(bd, str_pos+16)
        soff = str_pos+20
        for _ in range(min(str_count, 300)):
            if soff+2 > len(bd): break
            slen = r16(bd, soff); soff += 2
            if soff+slen > len(bd): break
            s = bd[soff:soff+slen].decode("utf-8", errors="replace")
            soff += slen+1
            if soff%2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 8

fmt_names = {
    0x0201: "R8_UNORM", 0x0701: "B5G6R5_UNORM", 0x0901: "RG8_UNORM",
    0x0B01: "RGBA8_UNORM", 0x0B06: "RGBA8_SRGB", 0x0C01: "BGRA8_UNORM", 0x0C06: "BGRA8_SRGB",
    0x1A01: "BC1_UNORM", 0x1A06: "BC1_SRGB",
    0x1B01: "BC2_UNORM", 0x1B06: "BC2_SRGB",
    0x1C01: "BC3_UNORM", 0x1C06: "BC3_SRGB",
    0x1D01: "BC4_UNORM", 0x1E01: "BC5_UNORM",
    0x1F01: "BC6H_UF16", 0x2001: "BC7_UNORM", 0x2006: "BC7_SRGB",
}

# Print info for key textures
key_indices = [8, 19, 24, 30, 73, 15, 17, 66, 67]
print("Key texture info:")
for i in key_indices:
    if i >= len(brti_offsets): continue
    brti = brti_offsets[i]
    name = str_names[i] if i < len(str_names) else f"tex_{i}"
    tile_mode = bd[brti+0x10]
    fmt_raw = r32(bd, brti+0x1C)
    width = r32(bd, brti+0x24)
    height = r32(bd, brti+0x28)
    data_size = r32(bd, brti+0x50)
    fmt_name = fmt_names.get(fmt_raw, f"UNK_{fmt_raw:#06x}")
    print(f"  [{i}] '{name}': {width}x{height} fmt={fmt_name} tile={tile_mode} size={data_size:#x}")

# Save tex[73] (ef_cmn_flare01) raw deswizzled data to inspect
print("\nSaving tex[73] raw pixel data...")
try:
    import tegra_swizzle
    print("  tegra_swizzle available")
except ImportError:
    print("  tegra_swizzle not available, saving raw swizzled data")

# Just dump the raw bytes for tex[73] so we can check if it looks like valid BC data
i = 73
brti = brti_offsets[i]
name = str_names[i]
fmt_raw = r32(bd, brti+0x1C)
width = r32(bd, brti+0x24)
height = r32(bd, brti+0x28)
data_size = r32(bd, brti+0x50)
mip0_ptr_lo = r32(bd, brti+0x290)
mip0_ptr_hi = r32(bd, brti+0x294)
mip0_ptr = (mip0_ptr_hi << 32) | mip0_ptr_lo

pixel_start = mip0_ptr if mip0_ptr > 0 and mip0_ptr < len(bd) else brtd_data_start
print(f"  tex[{i}] '{name}': {width}x{height} fmt={fmt_names.get(fmt_raw, hex(fmt_raw))} mip0_ptr={mip0_ptr:#x} pixel_start={pixel_start:#x} size={data_size:#x}")
print(f"  First 32 bytes: {bd[pixel_start:pixel_start+32].hex()}")

# Check if it looks like BC data (BC1: 8 bytes per 4x4 block)
fmt_type = (fmt_raw >> 8) & 0xFF
is_bc1 = fmt_type == 0x1A
blocks_x = (width + 3) // 4
blocks_y = (height + 3) // 4
expected_bc1_size = blocks_x * blocks_y * 8
expected_bc3_size = blocks_x * blocks_y * 16
print(f"  fmt_type={fmt_type:#04x} expected_bc1_size={expected_bc1_size:#x} expected_bc3_size={expected_bc3_size:#x} actual={data_size:#x}")
