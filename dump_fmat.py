#!/usr/bin/env python3
"""Dump FVTX attrib/buffer layout and FSHP mesh layout."""
import struct, sys, glob

EFF_PATHS = [
    "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff",
]
raw = None
for p in EFF_PATHS:
    try:
        with open(p, 'rb') as f: raw = f.read(); break
    except FileNotFoundError: pass
if raw is None: sys.exit("No .eff file found")

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def r64(d, off): return struct.unpack_from('<Q', d, off)[0] if off+8<=len(d) else 0
def rstr(d, off):
    if off == 0 or off >= len(d): return ''
    end = d.find(b'\x00', off)
    return d[off:end].decode('utf-8', errors='replace') if end > off else ''

NULL = 0xFFFFFFFF
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_size(base): return r32(data, base+4)
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'

block_offset = r16(data, 0x16)
sec = block_offset
g3pr_bin_start = None
while sec + 4 <= len(data):
    if sec_magic(sec) == b'G3PR':
        bin_off_rel = sec_bin_off(sec)
        g3pr_bin_start = sec + bin_off_rel
        g3pr_bin_len = sec_size(sec) - bin_off_rel
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec = sec + nxt

bfres = data[g3pr_bin_start:g3pr_bin_start + g3pr_bin_len]

# FRES -> FMDL
model_arr = r64(bfres, 0x28)
fmdl = int(model_arr)
print(f"FMDL at {fmdl:#x}")

# FMDL corrected offsets
fvtx_ptr = r64(bfres, fmdl + 0x20)
fshp_ptr = r64(bfres, fmdl + 0x28)
fmat_ptr = r64(bfres, fmdl + 0x38)
num_vbufs = r16(bfres, fmdl + 0x68)
num_shapes = r16(bfres, fmdl + 0x6a)
num_mats = r16(bfres, fmdl + 0x6c)
print(f"fvtx_ptr={fvtx_ptr:#x} fshp_ptr={fshp_ptr:#x} fmat_ptr={fmat_ptr:#x}")
print(f"num_vbufs={num_vbufs} num_shapes={num_shapes} num_mats={num_mats}")

# FVTX
fvtx = int(fvtx_ptr)
print(f"\nFVTX at {fvtx:#x}: magic={bfres[fvtx:fvtx+4]}")
print("FVTX full header +0x00..+0x60:")
for row in range(0, min(0x60, len(bfres)-fvtx), 16):
    vals = [f'{bfres[fvtx+row+i]:02x}' for i in range(min(16, len(bfres)-fvtx-row))]
    u16s = [(f'+{row+i:#04x}={r16(bfres,fvtx+row+i)}') for i in range(0,16,2) if 0 < r16(bfres,fvtx+row+i) <= 256]
    print(f"  +{row:#04x}: {' '.join(vals)}")
    if u16s: print(f"         {', '.join(u16s)}")

print("FVTX pointer scan:")
for off in range(0x08, 0x50, 8):
    val = r64(bfres, fvtx + off)
    if 0 < val < len(bfres):
        magic = bfres[int(val):int(val)+4]
        print(f"  fvtx+{off:#04x}: {val:#x} -> {magic}")

# FSHP
fshp = int(fshp_ptr)
print(f"\nFSHP at {fshp:#x}: magic={bfres[fshp:fshp+4]}")
print("FSHP full header +0x00..+0x60:")
for row in range(0, min(0x60, len(bfres)-fshp), 16):
    vals = [f'{bfres[fshp+row+i]:02x}' for i in range(min(16, len(bfres)-fshp-row))]
    u16s = [(f'+{row+i:#04x}={r16(bfres,fshp+row+i)}') for i in range(0,16,2) if 0 < r16(bfres,fshp+row+i) <= 256]
    print(f"  +{row:#04x}: {' '.join(vals)}")
    if u16s: print(f"         {', '.join(u16s)}")

print("FSHP pointer scan:")
for off in range(0x08, 0x60, 8):
    val = r64(bfres, fshp + off)
    if 0 < val < len(bfres):
        magic = bfres[int(val):int(val)+4]
        print(f"  fshp+{off:#04x}: {val:#x} -> {magic}")

# FMAT
fmat = int(fmat_ptr)
print(f"\nFMAT at {fmat:#x}: magic={bfres[fmat:fmat+4]}")
print("FMAT full header +0x00..+0x60:")
for row in range(0, min(0x60, len(bfres)-fmat), 16):
    vals = [f'{bfres[fmat+row+i]:02x}' for i in range(min(16, len(bfres)-fmat-row))]
    u16s = [(f'+{row+i:#04x}={r16(bfres,fmat+row+i)}') for i in range(0,16,2) if 0 < r16(bfres,fmat+row+i) <= 256]
    print(f"  +{row:#04x}: {' '.join(vals)}")
    if u16s: print(f"         {', '.join(u16s)}")

tex_name_arr = r64(bfres, fmat + 0x28)
num_tex = r8(bfres, fmat + 0x4A)
print(f"\nFMAT tex_name_arr={tex_name_arr:#x} num_tex={num_tex}")
if tex_name_arr < len(bfres) and num_tex > 0:
    name_ptr = r64(bfres, int(tex_name_arr))
    tex_name = rstr(bfres, int(name_ptr)) if name_ptr < len(bfres) else '?'
    print(f"First texture name: '{tex_name}'")
