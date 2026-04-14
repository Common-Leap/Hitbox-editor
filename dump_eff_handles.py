#!/usr/bin/env python3
"""Dump all effect handle names from ef_samus.eff to see what names map to what indices."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

print(f"File size: {len(raw)}, magic: {raw[:4]}")

# The .eff file format (EFFN):
# Header, then effect handle table
# Each handle: emitter_set_handle (i32) + name (hash40 string)
# Let's look at the eff_lib crate's format by examining the binary

# Find the handle table - look for known effect names
# The eff file has a string table with effect names like "samus_atk_bomb"
# Let's scan for null-terminated strings that look like effect names

print("\nSearching for 'samus' strings in eff file:")
pos = 0
while pos < len(raw) - 4:
    if raw[pos:pos+5] == b'samus':
        # Found a potential name - read until null
        end = raw.index(b'\x00', pos) if b'\x00' in raw[pos:pos+100] else pos+100
        name = raw[pos:end].decode('utf-8', errors='replace')
        print(f"  @ {pos:#x}: '{name}'")
        pos = end + 1
    else:
        pos += 1
