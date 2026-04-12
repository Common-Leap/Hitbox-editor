#!/usr/bin/env python3
"""Check if texture names hash to the TextureIDs seen in EMTR sampler slots."""
import binascii
import struct

# TextureIDs seen in the logs that aren't resolving
unknown_ids = [
    0xad584604,
    0x10edf80b,
    0xc6a6335c,
]

# Texture names from GRTF logs (first run had different names, second run had these)
# Combine both sets
tex_names = [
    "ef_item_lightning01", "ef_cmn_parts01", "ef_cmn_impact14", "ef_cmn_stone00",
    "ef_cmn_mask01", "ef_cmn_impact08", "ef_cmn_smoke01", "ef_cmn_fire05",
    "ef_item_impact01", "ef_cmn_stone01", "ef_cmn_impact11", "ef_item_flash05",
    "ef_item_impact11", "ef_samus_color00", "ef_cmn_impactflash00", "ef_cmn_wind00",
    "ef_samus_wind00", "ef_captain_fireimpact00", "ef_cmn_impact00",
    # Also try without ef_ prefix
]

def crc32(s):
    return binascii.crc32(s.encode()) & 0xFFFFFFFF

def hash40(s):
    """hash40 as used in Smash: CRC32 of string XOR'd with length shifted"""
    # Standard hash40: lower 40 bits of a custom hash
    # Simple version: crc32 with length in high bits
    h = crc32(s)
    return h | (len(s) << 32)

print("Checking CRC32 matches for unknown TextureIDs:")
for uid in unknown_ids:
    print(f"\n  Looking for: {uid:#010x}")
    for name in tex_names:
        c = crc32(name)
        if c == uid:
            print(f"    MATCH: '{name}' crc32={c:#010x}")
        # Also try without ef_ prefix
        stripped = name.removeprefix("ef_")
        c2 = crc32(stripped)
        if c2 == uid:
            print(f"    MATCH (stripped): '{stripped}' crc32={c2:#010x}")

print("\n\nAll CRC32 values for known texture names:")
for name in tex_names:
    c = crc32(name)
    stripped = name.removeprefix("ef_")
    c2 = crc32(stripped)
    print(f"  '{name}': {c:#010x}  (stripped '{stripped}': {c2:#010x})")
