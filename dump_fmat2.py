#!/usr/bin/env python3
"""Find num_tex, num_attribs, num_buffers, and FSHP mesh layout."""
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

model_arr = r64(bfres, 0x28)
fmdl = int(model_arr)
fvtx = int(r64(bfres, fmdl + 0x20))
fshp = int(r64(bfres, fmdl + 0x28))
fmat = int(r64(bfres, fmdl + 0x38))

# ── FMAT: find num_tex ────────────────────────────────────────────────────
print("=== FMAT ===")
tex_name_arr = r64(bfres, fmat + 0x28)
print(f"tex_name_arr at fmat+0x28: {tex_name_arr:#x}")
print(f"Data at tex_name_arr {tex_name_arr:#x}:")
for row in range(0, min(0x30, len(bfres)-int(tex_name_arr)), 16):
    vals = [f'{bfres[int(tex_name_arr)+row+i]:02x}' for i in range(min(16, len(bfres)-int(tex_name_arr)-row))]
    print(f"  +{row:#04x}: {' '.join(vals)}")

# The tex_name_arr is an array of u64 string pointers
# Try reading the first pointer
name_ptr0 = r64(bfres, int(tex_name_arr))
print(f"tex_name_arr[0] ptr: {name_ptr0:#x}")
if name_ptr0 < len(bfres):
    print(f"  -> '{rstr(bfres, int(name_ptr0))}'")

# Scan FMAT for num_tex (byte values 1-8 near the tex_name_arr offset)
print(f"\nFMAT byte scan for num_tex (values 1-8):")
for off in range(0x40, 0x60):
    val = r8(bfres, fmat + off)
    if 0 < val <= 8:
        print(f"  fmat+{off:#04x}: byte={val}")

# Also check the BfresLibrary layout more carefully
# From MaterialParser.cs:
#   long TextureArrayOffset = loader.ReadInt64();   <- +0x10 (after name at +0x08)
#   long TextureNameArray = loader.ReadInt64();      <- +0x18
#   long SamplerArrayOffset = loader.ReadInt64();    <- +0x20
#   mat.Samplers = loader.LoadDictValues<Sampler>(); <- +0x28 (dict)
#   mat.ShaderParams = ...                           <- +0x30
#   ...
#   byte numTextureRef = loader.ReadByte();          <- somewhere after the pointers
#   byte numSampler = loader.ReadByte();
# The FMAT block starts with "FMAT" magic, then block_size, then the loader reads sequentially
# But the loader uses a ResFileSwitchLoader which reads from a stream position
# The stream starts at the FMAT block start
# After the magic check, LoadHeaderBlock() is called which reads the block header
# Then Name = LoadString() reads a string pointer
# Then the sequential reads follow

# Let's check what's at fmat+0x08 (after magic+block_size)
print(f"\nFMAT sequential read layout:")
print(f"  +0x00: magic = {bfres[fmat:fmat+4]}")
print(f"  +0x04: block_size = {r32(bfres, fmat+4):#x}")
print(f"  +0x08: name_ptr = {r64(bfres, fmat+8):#x} -> '{rstr(bfres, int(r64(bfres, fmat+8)))}'")
# After LoadHeaderBlock (which reads magic+size+next = 12 bytes), Name is read
# But in NX format, the block header is different
# Let's just print all u64 values and identify them
print(f"\nFMAT all u64 values:")
for off in range(0, 0x60, 8):
    val = r64(bfres, fmat + off)
    extra = ''
    if 0 < val < len(bfres):
        magic = bfres[int(val):int(val)+4]
        s = rstr(bfres, int(val))
        extra = f'-> {magic} / "{s[:20]}"'
    print(f"  fmat+{off:#04x}: {val:#x}  {extra}")

# ── FVTX: find num_attribs, num_buffers ──────────────────────────────────
print("\n=== FVTX ===")
attrib_arr = r64(bfres, fvtx + 0x08)
buf_arr = r64(bfres, fvtx + 0x30)
num_vertices = r16(bfres, fvtx + 0x4a)
print(f"attrib_arr={attrib_arr:#x} buf_arr={buf_arr:#x} num_vertices={num_vertices}")

# Scan for num_attribs and num_buffers
print("FVTX byte/u16 scan for counts (1-32):")
for off in range(0x40, 0x60, 1):
    val = r8(bfres, fvtx + off)
    if 0 < val <= 32:
        print(f"  fvtx+{off:#04x}: byte={val}")

# Check what's at attrib_arr
print(f"\nData at attrib_arr {attrib_arr:#x}:")
for row in range(0, min(0x40, len(bfres)-int(attrib_arr)), 16):
    vals = [f'{bfres[int(attrib_arr)+row+i]:02x}' for i in range(min(16, len(bfres)-int(attrib_arr)-row))]
    print(f"  +{row:#04x}: {' '.join(vals)}")

# Check what's at buf_arr
print(f"\nData at buf_arr {buf_arr:#x}:")
for row in range(0, min(0x40, len(bfres)-int(buf_arr)), 16):
    vals = [f'{bfres[int(buf_arr)+row+i]:02x}' for i in range(min(16, len(bfres)-int(buf_arr)-row))]
    print(f"  +{row:#04x}: {' '.join(vals)}")

# ── FSHP: find mat_idx, fvtx_idx, mesh_arr ───────────────────────────────
print("\n=== FSHP ===")
print("FSHP all u64 values:")
for off in range(0, 0x60, 8):
    val = r64(bfres, fshp + off)
    extra = ''
    if 0 < val < len(bfres):
        magic = bfres[int(val):int(val)+4]
        extra = f'-> {magic}'
    print(f"  fshp+{off:#04x}: {val:#x}  {extra}")

print("\nFSHP u16 scan (values 0-256):")
for off in range(0x18, 0x40, 2):
    val = r16(bfres, fshp + off)
    if val <= 256:
        print(f"  fshp+{off:#04x}: u16={val}")

# mesh_arr at fshp+0x18
mesh_arr = r64(bfres, fshp + 0x18)
print(f"\nmesh_arr at fshp+0x18: {mesh_arr:#x}")
print(f"Data at mesh_arr {mesh_arr:#x}:")
for row in range(0, min(0x40, len(bfres)-int(mesh_arr)), 16):
    vals = [f'{bfres[int(mesh_arr)+row+i]:02x}' for i in range(min(16, len(bfres)-int(mesh_arr)-row))]
    print(f"  +{row:#04x}: {' '.join(vals)}")
