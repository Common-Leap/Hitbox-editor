#!/usr/bin/env python3
"""Dump the actual effect handle table from ef_samus.eff using the EFFN format."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

print(f"File size: {len(raw)}, magic: {raw[:4]}")

# EFFN format: look for the handle table
# The eff_lib crate reads: effect_handles (i32 each) and effect_handle_names (strings)
# Let's find the handle table by looking at the structure

# EFFN header is typically:
# 0x00: magic "EFFN"
# 0x04: version
# 0x08: num_handles
# 0x0C: handle_table_offset
# 0x10: name_table_offset

def r32(off):
    if off + 4 > len(raw): return 0
    return struct.unpack_from('<I', raw, off)[0]

def r32s(off):
    if off + 4 > len(raw): return 0
    return struct.unpack_from('<i', raw, off)[0]

print(f"Header bytes: {raw[:32].hex()}")
print(f"  [0x00] magic: {raw[0:4]}")
print(f"  [0x04] u32: {r32(4):#010x}")
print(f"  [0x08] u32: {r32(8):#010x}")
print(f"  [0x0C] u32: {r32(0xC):#010x}")
print(f"  [0x10] u32: {r32(0x10):#010x}")
print(f"  [0x14] u32: {r32(0x14):#010x}")
print(f"  [0x18] u32: {r32(0x18):#010x}")
print(f"  [0x1C] u32: {r32(0x1C):#010x}")

# Try to find handle names by scanning for known patterns
# The handle names are stored as null-terminated strings
# Let's look at the region around 0xac0000 where we found names
print("\nHandle names region (0xac0e00 - 0xac2000):")
pos = 0xac0e00
while pos < min(0xac2000, len(raw)):
    if raw[pos] != 0:
        end = raw.index(b'\x00', pos) if b'\x00' in raw[pos:pos+200] else pos+200
        name = raw[pos:end].decode('utf-8', errors='replace')
        if len(name) > 3 and name.isprintable():
            # Look for the handle index before this string
            # Typically stored as i32 before the string
            handle_idx = r32s(pos - 4) if pos >= 4 else -1
            print(f"  @ {pos:#x} (handle={handle_idx}): '{name}'")
        pos = end + 1
    else:
        pos += 1

# Also try to find the handle table by looking for sequential i32 values
print("\nLooking for handle index table...")
# The handles are likely stored as an array of i32 values
# Let's look for a sequence like 0, 1, 2, 3, ... near the start
for start in range(0, min(0x1000, len(raw)-4), 4):
    if r32s(start) == 0 and r32s(start+4) == 1 and r32s(start+8) == 2:
        print(f"  Found sequential i32 at {start:#x}: {[r32s(start+i*4) for i in range(10)]}")
        break
