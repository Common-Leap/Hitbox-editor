#!/usr/bin/env python3
"""Check ef_cmn_grade00 texture format and data."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0

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

# Check textures used by P_SamusAttackBomb
target_names = ['ef_cmn_grade00', 'ef_cmn_bomb_indirect00', 'ef_cmn_smoke11',
                'ef_cmn_fireimpact02', 'ef_cmn_fire00', 'ef_cmn_debris00',
                'ef_samus_impact08', 'ef_cmn_impact04', 'ef_cmn_spark04',
                'ef_samus_impactring04', 'ef_cmn_ring00', 'ef_item_impact14',
                'ef_cmn_impact00', 'ef_cmn_flare03', 'ef_cmn_parts01']

brtd_data_start = data_blk_abs + 0x10

for name in target_names:
    if name not in str_names:
        print(f"MISSING: {name}")
        continue
    idx = str_names.index(name)
    if idx >= len(brti_offsets):
        print(f"[{idx:3d}] {name}: NO BRTI")
        continue
    brti = brti_offsets[idx]
    fmt_raw = r32(data, brti + 0x1C)
    w = r32(data, brti + 0x24)
    h = r32(data, brti + 0x28)
    data_size = r32(data, brti + 0x50)
    comp_sel = r32(data, brti + 0x58)
    tile_mode = r16(data, brti + 0x12)
    fmt_type = (fmt_raw >> 8) & 0xFF
    fmt_variant = fmt_raw & 0xFF
    
    # Get pixel data offset
    pts_addr_lo = r32(data, brti + 0x70)
    pts_addr_hi = r32(data, brti + 0x74)
    pts_addr = (pts_addr_hi << 32 | pts_addr_lo)
    pts_addr_abs = bntx_base + pts_addr
    if pts_addr > 0 and pts_addr_abs + 8 <= len(data):
        mip0_lo = r32(data, pts_addr_abs)
        mip0_hi = r32(data, pts_addr_abs + 4)
        mip0_rel = (mip0_hi << 32 | mip0_lo)
        pixel_start = bntx_base + mip0_rel
    else:
        pixel_start = 0
    
    # Sample first few pixels
    pixel_sample = ""
    if pixel_start > 0 and pixel_start + 16 <= len(data):
        sample = data[pixel_start:pixel_start+16]
        pixel_sample = f" first_bytes={sample.hex()}"
    
    print(f"[{idx:3d}] {name}: {w}x{h} fmt={fmt_raw:#06x}(type={fmt_type:#04x},var={fmt_variant:#04x}) tile={tile_mode} swizzle={comp_sel:#010x} size={data_size}{pixel_sample}")
