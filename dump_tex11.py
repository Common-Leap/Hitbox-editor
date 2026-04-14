#!/usr/bin/env python3
"""Check texture 11 (flash01) swizzle and format."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]

# Find BNTX
bntx_off = data.find(b'BNTX')
print(f"BNTX at {bntx_off:#x}")

# Scan for BRTI blocks
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

print(f"Found {len(brti_list)} BRTI blocks")
print(f"BRTD at {brtd_off:#x}" if brtd_off else "BRTD not found")

# Print info for textures 8-15 (around index 11)
for i in range(min(len(brti_list), 20)):
    brti = brti_list[i]
    fmt_raw = r32(brti + 0x1C)
    width = r32(brti + 0x24)
    height = r32(brti + 0x28)
    data_size = r32(brti + 0x50)
    comp_sel = r32(brti + 0x58)
    fmt_type = (fmt_raw >> 8) & 0xFF
    fmt_var = fmt_raw & 0xFF
    r_out = (comp_sel >> 24) & 0xFF
    g_out = (comp_sel >> 16) & 0xFF
    b_out = (comp_sel >> 8) & 0xFF
    a_out = (comp_sel >> 0) & 0xFF
    print(f"  [{i:2d}] {width}x{height} fmt_type={fmt_type:#04x} var={fmt_var:#04x} swizzle=({r_out},{g_out},{b_out},{a_out}) data_size={data_size}")

# For texture 11 specifically, check if it's BC5 and what the swizzle means
if len(brti_list) > 11:
    brti = brti_list[11]
    comp_sel = r32(brti + 0x58)
    fmt_raw = r32(brti + 0x1C)
    fmt_type = (fmt_raw >> 8) & 0xFF
    r_out = (comp_sel >> 24) & 0xFF
    g_out = (comp_sel >> 16) & 0xFF
    b_out = (comp_sel >> 8) & 0xFF
    a_out = (comp_sel >> 0) & 0xFF
    print(f"\nTexture 11 details:")
    print(f"  fmt_type={fmt_type:#04x} ({'BC5' if fmt_type==0x1e else 'BC4' if fmt_type==0x1d else 'other'})")
    print(f"  swizzle: R_out={r_out} G_out={g_out} B_out={b_out} A_out={a_out}")
    print(f"  (0=zero, 1=one, 2=R, 3=G, 4=B, 5=A)")
    print(f"  → output RGBA = ({['0','1','R','G','B','A'][r_out]}, {['0','1','R','G','B','A'][g_out]}, {['0','1','R','G','B','A'][b_out]}, {['0','1','R','G','B','A'][a_out]})")
    
    # BC5 decodes to RG. After swizzle, what does alpha become?
    if a_out == 2:
        print(f"  Alpha = R channel of BC5 → intensity mask (correct for transparency)")
    elif a_out == 5:
        print(f"  Alpha = A channel (BC5 has no A, so A=255 always → solid square!)")
    elif a_out == 1:
        print(f"  Alpha = 1 (always opaque → solid square!)")
    elif a_out == 0:
        print(f"  Alpha = 0 (always transparent → invisible!)")
