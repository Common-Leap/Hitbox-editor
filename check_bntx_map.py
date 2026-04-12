#!/usr/bin/env python3
"""
Check if 'ef_samus_burner00' is in the bntx_map as built by parse_bntx_named.
Also check the _STR block parsing.
"""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off): return struct.unpack_from('<I', data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from('<H', data, off)[0] if off+2<=len(data) else 0

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(base + 4)
def sec_child_off(base): return r32(base + 8)
def sec_next_off(base): return r32(base + 0xC)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_cnt(base): return r16(base + 0x1C)

block_offset = r16(0x16)
sec = block_offset

# Find GRTF
while sec + 4 <= len(data):
    if sec_magic(sec) == b'GRTF':
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec = sec + nxt

print(f"GRTF at {sec:#x}")
bin_off_rel = sec_bin_off(sec)
bin_start = sec + bin_off_rel
bin_len = sec_size(sec) - bin_off_rel
print(f"GRTF bin_start={bin_start:#x} bin_len={bin_len:#x}")

# Find BNTX inside GRTF binary
bntx_pos = data.find(b'BNTX', bin_start, bin_start + min(bin_len, 0x100000))
print(f"BNTX at {bntx_pos:#x}")

# Parse _STR block (same logic as Rust parse_bntx_named)
str_names = []
str_pos = bntx_pos
scan_end = min(bin_start + bin_len, len(data))
while str_pos + 4 < scan_end:
    if data[str_pos:str_pos+4] == b'_STR':
        str_count = r32(str_pos + 16)
        soff = str_pos + 20
        print(f"_STR at {str_pos:#x}: str_count={str_count}")
        for _ in range(min(str_count, 300)):
            if soff + 2 > len(data): break
            slen = r16(soff); soff += 2
            if soff + slen > len(data): break
            s = data[soff:soff+slen].decode('utf-8', errors='replace')
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 8
    if str_pos > scan_end + 0x1000: break

print(f"_STR names: {len(str_names)}")
for i, n in enumerate(str_names[:15]):
    print(f"  [{i}] '{n}'")

# Check if 'ef_samus_burner00' is in the list
target = 'ef_samus_burner00'
if target in str_names:
    idx = str_names.index(target)
    print(f"\n'{target}' found at index {idx}")
else:
    print(f"\n'{target}' NOT FOUND in _STR names!")
    # Try to find it
    for i, n in enumerate(str_names):
        if 'burner' in n.lower() or 'samus' in n.lower():
            print(f"  Similar: [{i}] '{n}'")

# Now parse BRTI structs to see what textures are there
# Find BRTD to know where pixel data starts
brtd_pos = data.find(b'BRTD', bntx_pos, scan_end)
print(f"\nBRTD at {brtd_pos:#x}")

# NX section
nx = bntx_pos + 0x20
print(f"NX at {nx:#x}: magic={data[nx:nx+4]}")
tex_count = r32(nx + 4)
data_blk_rel = r32(nx + 0x10)
data_blk_abs = nx + 0x10 + data_blk_rel
brtd_data_start = data_blk_abs + 0x10
print(f"tex_count={tex_count} data_blk_abs={data_blk_abs:#x} brtd_data_start={brtd_data_start:#x}")

# Scan for BRTI
brti_offsets = []
pos = bntx_pos
while pos + 4 <= scan_end:
    if data[pos:pos+4] == b'BRTI':
        brti_offsets.append(pos)
        brti_len = r32(pos + 4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

print(f"Found {len(brti_offsets)} BRTI structs")

# Check BRTI at index 8 (ef_samus_burner00)
for i in [0, 8]:
    if i >= len(brti_offsets): continue
    brti = brti_offsets[i]
    width = r32(brti + 0x24)
    height = r32(brti + 0x28)
    fmt_raw = r32(brti + 0x1C)
    data_size = r32(brti + 0x50)
    tile_mode = data[brti + 0x10]
    name = str_names[i] if i < len(str_names) else '?'
    print(f"  BRTI[{i}] '{name}': {width}x{height} fmt={fmt_raw:#06x} size={data_size} tile={tile_mode}")

# The key question: does bntx_map contain 'ef_samus_burner00'?
# bntx_map is built from str_names (from _STR block)
# If str_names[8] == 'ef_samus_burner00', then bntx_map['ef_samus_burner00'] exists
print(f"\nstr_names[8] = '{str_names[8] if len(str_names) > 8 else 'N/A'}'")
print(f"Expected: 'ef_samus_burner00'")

# Also check: the Rust code uses parse_bntx_named which scans for _STR
# but the scan goes: str_pos = bntx_base; while str_pos + 4 <= data.len()
# It scans the ENTIRE data slice, not just the GRTF binary section
# This means it might find a different _STR block!
print(f"\n=== Checking if there are multiple _STR blocks ===")
pos = 0
count = 0
while pos + 4 <= len(data):
    if data[pos:pos+4] == b'_STR':
        str_count = r32(pos + 16)
        print(f"  _STR at {pos:#x}: str_count={str_count}")
        count += 1
        if count > 5: break
    pos += 8
