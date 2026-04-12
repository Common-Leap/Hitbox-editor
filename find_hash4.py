#!/usr/bin/env python3
"""
Extract all 104 BNTX texture names from the .eff file and try every hash.
Also dump the BNTX BRTI structs to find the name hash field directly.
"""
import struct, binascii

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

def r32(d, off):
    if off + 4 > len(d): return 0
    return struct.unpack_from("<I", d, off)[0]
def r16(d, off):
    if off + 2 > len(d): return 0
    return struct.unpack_from("<H", d, off)[0]
def r64(d, off):
    if off + 8 > len(d): return 0
    return struct.unpack_from("<Q", d, off)[0]

# Find GRTF section
block_offset = r16(data, 0x16)
NULL = 0xFFFFFFFF

def sec_next_off(base): return r32(data, base + 0x0C)
def sec_bin_off(base): return r32(data, base + 0x14)
def sec_size(base): return r32(data, base + 0x04)

sec = block_offset
bntx_start = None
while sec + 4 <= len(data):
    magic = bytes(data[sec:sec+4])
    if magic == b"GRTF":
        bin_off_rel = sec_bin_off(sec)
        bntx_start = sec + bin_off_rel
        break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

if bntx_start is None:
    print("GRTF not found"); exit(1)

bntx_data = data[bntx_start:]
bntx_base = bntx_data.find(b"BNTX")
if bntx_base < 0:
    print("BNTX not found"); exit(1)

bd = bntx_data[bntx_base:]

# Parse BNTX header
nx_off = 0x20
tex_count = r32(bd, nx_off + 0x04)
data_blk_rel = r32(bd, nx_off + 0x10)
data_blk_abs = nx_off + 0x10 + data_blk_rel
brtd_data_start = data_blk_abs + 0x10

print(f"BNTX: tex_count={tex_count}, data_blk_abs={data_blk_abs:#x}")

# Find all BRTI structs
brti_offsets = []
pos = bntx_base  # relative to bntx_data
while pos + 4 <= data_blk_abs + bntx_base:
    if bd[pos:pos+4] == b"BRTI":
        brti_offsets.append(pos)
        brti_len = r32(bd, pos + 4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

print(f"Found {len(brti_offsets)} BRTI structs")

# Find _STR block for names
str_names = []
str_pos = 0
while str_pos + 4 <= data_blk_abs:
    if bd[str_pos:str_pos+4] == b"_STR":
        str_count = r32(bd, str_pos + 16)
        soff = str_pos + 20
        for _ in range(min(str_count, 300)):
            if soff + 2 > len(bd): break
            slen = r16(bd, soff); soff += 2
            if soff + slen > len(bd): break
            s = bd[soff:soff+slen].decode("utf-8", errors="replace")
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 8

print(f"_STR names: {len(str_names)}")

# For each BRTI, dump the name hash field
# BRTI layout: +0x20 = name_rel (u64 self-relative pointer to name string)
# But more importantly, check if there's a hash field
targets = {0xad584604, 0x10edf80b, 0xc6a6335c, 0x81d59f24, 0xf23f06f8, 0xda3564c9, 0x7cdcd964}

print("\nBRTI name hash fields (checking offsets 0x00-0x60 for target values):")
for i, brti in enumerate(brti_offsets[:10]):
    name = str_names[i] if i < len(str_names) else f"tex_{i}"
    # Scan the first 96 bytes of BRTI for any of our target values
    for off in range(0, 96, 4):
        val = r32(bd, brti + off)
        if val in targets:
            print(f"  BRTI[{i}] '{name}' offset +{off:#x}: {val:#010x} <-- TARGET")
    # Also print the first 32 bytes
    raw_brti = bd[brti:brti+32].hex()
    print(f"  BRTI[{i}] '{name}': {raw_brti}")

# Now try all hash functions on all names
print(f"\nTrying hash functions on all {len(str_names)} names:")
def crc32(s): return binascii.crc32(s.encode()) & 0xFFFFFFFF
def nw4f(s):
    h = 0
    for c in s.encode():
        h = (((h << 5) | (h >> 27)) ^ c) & 0xFFFFFFFF
    return h
def fnv1a(s):
    h = 0x811c9dc5
    for c in s.encode(): h = ((h ^ c) * 0x01000193) & 0xFFFFFFFF
    return h
def murmur_simple(s):
    # Simple murmur-like
    h = 0
    for c in s.encode():
        h = (h * 0x9e3779b9 + c) & 0xFFFFFFFF
    return h

all_targets = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1",
               0x81d59f24: "smokeBomb_tex0", 0xf23f06f8: "smokeBomb_tex1"}

found = False
for i, name in enumerate(str_names):
    for v in [name, name.removeprefix("ef_"), name.lower(), name.upper()]:
        for fn, fname in [(crc32,"crc32"),(nw4f,"nw4f"),(fnv1a,"fnv1a"),(murmur_simple,"murmur")]:
            h = fn(v)
            if h in all_targets:
                print(f"  MATCH: {fname}('{v}') = {h:#010x} -> tex[{i}]='{name}' ({all_targets[h]})")
                found = True

if not found:
    print("  No matches. Printing hashes for first 5 names:")
    for name in str_names[:5]:
        print(f"  '{name}':")
        for v in [name, name.removeprefix("ef_")]:
            for fn, fname in [(crc32,"crc32"),(nw4f,"nw4f"),(fnv1a,"fnv1a")]:
                print(f"    {fname}('{v}') = {fn(v):#010x}")
