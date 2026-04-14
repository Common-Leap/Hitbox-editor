#!/usr/bin/env python3
"""Decode and inspect the flash01 texture (index 11) to see if it has transparent edges."""
import struct, sys
from builtins import max

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

# The BNTX textures are parsed by the Rust code. Let's look at what image_dds
# would produce for the BC5 texture at index 11.
# First, let's find the texture data by looking at the BNTX section.

# From the test output, texture 11 is 256x128 BC5 (fmt=0x1e01)
# Let's find it in the BNTX section

# Find BNTX
bntx_off = data.find(b'BNTX')
print(f"BNTX at {bntx_off:#x}")

# Find GRTF section which contains the BNTX
grtf_off = data.find(b'GRTF')
print(f"GRTF at {grtf_off:#x}")

# The BNTX is embedded in the GRTF binary section
# GRTF section header: magic(4) + size(4) + child_off(4) + next_off(4) + attr_off(4) + bin_off(4) + ...
def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]

grtf_bin_off = r32(grtf_off + 0x14)
grtf_bin_start = grtf_off + grtf_bin_off
print(f"GRTF bin at {grtf_bin_start:#x}")

# The BNTX is at grtf_bin_start
bntx_in_grtf = data.find(b'BNTX', grtf_bin_start)
print(f"BNTX in GRTF at {bntx_in_grtf:#x}")

# Try to use image_dds to decode texture 11
# First let's find the _STR block to get texture names
str_off = data.find(b'_STR', bntx_in_grtf)
print(f"_STR at {str_off:#x}")

# Count BRTI blocks to find texture 11
brti_count = 0
pos = bntx_in_grtf
brtd_off = None
while pos < len(data):
    if data[pos:pos+4] == b'BRTD':
        brtd_off = pos
        break
    if data[pos:pos+4] == b'BRTI':
        brti_count += 1
        brti_size = r32(pos + 4)
        if brti_count == 12:  # texture index 11 (0-based)
            print(f"\nBRTI[11] at {pos:#x}:")
            fmt_raw = r32(pos + 0x1C)
            width = r32(pos + 0x24)
            height = r32(pos + 0x28)
            data_size = r32(pos + 0x50)
            comp_sel = r32(pos + 0x58)
            print(f"  fmt={fmt_raw:#010x} ({(fmt_raw>>8):#04x} type, {(fmt_raw&0xFF):#04x} variant)")
            print(f"  size={width}x{height} data_size={data_size}")
            print(f"  comp_sel={comp_sel:#010x}")
            # comp_sel bytes: [A_src, B_src, G_src, R_src] in little-endian
            print(f"  swizzle: R_out={((comp_sel>>24)&0xFF):#04x} G_out={((comp_sel>>16)&0xFF):#04x} B_out={((comp_sel>>8)&0xFF):#04x} A_out={((comp_sel>>0)&0xFF):#04x}")
            print(f"  (2=R,3=G,4=B,5=A,0=zero,1=one)")
        pos += max(brti_size, 0x90)
    else:
        pos += 8

print(f"\nTotal BRTI blocks found: {brti_count}")
print(f"BRTD at {brtd_off:#x}" if brtd_off else "BRTD not found")

# The key question: does the flash texture have transparent edges?
# BC5 decodes to RG8 (no alpha). The R channel is the intensity.
# If R=0 at the edges, then after swizzle (1,1,1,R), alpha=0 at edges → transparent.
# If R=255 everywhere, it's a solid square.
# Let's check the raw BC5 data for texture 11.
print("\nTo check if the texture has transparent edges, we need to decode the BC5 data.")
print("BC5 block format: 16 bytes per 4x4 block")
print("First block of texture 11 would tell us the corner pixel values.")
