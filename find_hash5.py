#!/usr/bin/env python3
"""
Scan the full BRTI struct for ef_samus_burner00 (index 8) looking for 0xad584604.
Also scan the GTNT-equivalent section if it exists under a different name.
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

block_offset = r16(data, 0x16)
NULL = 0xFFFFFFFF
def sec_next_off(base): return r32(data, base + 0x0C)
def sec_bin_off(base): return r32(data, base + 0x14)
def sec_size(base): return r32(data, base + 0x04)
def sec_magic(base): return bytes(data[base:base+4])

# Find GRTF and get BNTX
sec = block_offset
bntx_start = None
while sec + 4 <= len(data):
    if sec_magic(sec) == b"GRTF":
        bntx_start = sec + sec_bin_off(sec)
        break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

bd = data[bntx_start:]
bntx_base = bd.find(b"BNTX")
bd = bd[bntx_base:]

nx_off = 0x20
data_blk_rel = r32(bd, nx_off + 0x10)
data_blk_abs = nx_off + 0x10 + data_blk_rel

# Find all BRTI
brti_offsets = []
pos = 0
while pos + 4 <= data_blk_abs:
    if bd[pos:pos+4] == b"BRTI":
        brti_offsets.append(pos)
        brti_len = r32(bd, pos + 4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

# Find _STR names
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

targets = {0xad584604, 0x10edf80b, 0xc6a6335c}
target_names = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1"}

# Scan BRTI[8] (ef_samus_burner00) for target values
print("Scanning BRTI[8] 'ef_samus_burner00' for target IDs:")
brti8 = brti_offsets[8]
brti8_len = r32(bd, brti8 + 4)
print(f"  BRTI at {brti8:#x}, len={brti8_len:#x}")
for off in range(0, min(brti8_len, 0x300), 4):
    val = r32(bd, brti8 + off)
    if val in targets:
        print(f"  FOUND at +{off:#x}: {val:#010x} ({target_names[val]})")

# Also scan ALL BRTI structs for any target value
print("\nScanning ALL BRTI structs for target IDs:")
for i, brti in enumerate(brti_offsets):
    brti_len = r32(bd, brti + 4)
    name = str_names[i] if i < len(str_names) else f"tex_{i}"
    for off in range(0, min(brti_len, 0x300), 4):
        val = r32(bd, brti + off)
        if val in targets:
            print(f"  BRTI[{i}] '{name}' +{off:#x}: {val:#010x} ({target_names[val]})")

# Scan the entire BNTX for target values
print("\nScanning entire BNTX for target IDs:")
for off in range(0, min(len(bd), 0x200000), 4):
    val = r32(bd, off)
    if val in targets:
        print(f"  offset {off:#x}: {val:#010x} ({target_names[val]})")

# Also: scan the VFXB data for a GTNT-like section with a different name
print("\nAll section magics in VFXB:")
sec = block_offset
iters = 0
while sec + 4 <= len(data) and iters < 100:
    iters += 1
    magic = bytes(data[sec:sec+4])
    size = r32(data, sec + 4)
    print(f"  {sec:#x}: '{magic.decode('ascii', errors='?')}' size={size:#x}")
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off
