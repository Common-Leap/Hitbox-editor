#!/usr/bin/env python3
"""
Investigate the TextureID format used in v22 VFXB EMTR sampler slots.
The IDs 0xad584604, 0x10edf80b, 0xc6a6335c don't match CRC32 of known names.
Try other hash algorithms.
"""
import binascii
import struct
import hashlib

unknown_ids = [0xad584604, 0x10edf80b, 0xc6a6335c]

# All texture names from the GRTF section (both runs combined)
tex_names = [
    "ef_item_lightning01", "ef_cmn_parts01", "ef_cmn_impact14", "ef_cmn_stone00",
    "ef_cmn_mask01", "ef_cmn_impact08", "ef_cmn_smoke01", "ef_cmn_fire05",
    "ef_item_impact01", "ef_cmn_stone01", "ef_cmn_impact11", "ef_item_flash05",
    "ef_item_impact11", "ef_samus_color00", "ef_cmn_impactflash00", "ef_cmn_wind00",
    "ef_samus_wind00", "ef_captain_fireimpact00", "ef_cmn_impact00",
    # Guesses for samus bomb textures
    "ef_samus_bomb00", "ef_samus_bomb01", "ef_samus_bomb_flash00",
    "ef_samus_burner00", "ef_samus_burner01",
    "samus_bomb00", "samus_bomb01", "samus_burner00",
    "burner1_L", "burner2_L", "flash1_L",
]

def crc32(s):
    return binascii.crc32(s.encode()) & 0xFFFFFFFF

def crc32_bytes(b):
    return binascii.crc32(b) & 0xFFFFFFFF

def murmur32(s, seed=0):
    """MurmurHash3 32-bit"""
    data = s.encode('utf-8')
    length = len(data)
    h = seed
    c1, c2 = 0xcc9e2d51, 0x1b873593
    for i in range(0, length - 3, 4):
        k = struct.unpack_from('<I', data, i)[0]
        k = (k * c1) & 0xFFFFFFFF
        k = ((k << 15) | (k >> 17)) & 0xFFFFFFFF
        k = (k * c2) & 0xFFFFFFFF
        h ^= k
        h = ((h << 13) | (h >> 19)) & 0xFFFFFFFF
        h = (h * 5 + 0xe6546b64) & 0xFFFFFFFF
    tail_len = length & 3
    if tail_len:
        tail = data[length - tail_len:]
        k = 0
        for i, b in enumerate(tail):
            k |= b << (i * 8)
        k = (k * c1) & 0xFFFFFFFF
        k = ((k << 15) | (k >> 17)) & 0xFFFFFFFF
        k = (k * c2) & 0xFFFFFFFF
        h ^= k
    h ^= length
    h ^= h >> 16
    h = (h * 0x85ebca6b) & 0xFFFFFFFF
    h ^= h >> 13
    h = (h * 0xc2b2ae35) & 0xFFFFFFFF
    h ^= h >> 16
    return h

def fnv1a32(s):
    h = 0x811c9dc5
    for c in s.encode():
        h ^= c
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h

def djb2(s):
    h = 5381
    for c in s.encode():
        h = ((h << 5) + h + c) & 0xFFFFFFFF
    return h

print("Trying multiple hash algorithms against unknown TextureIDs:")
for uid in unknown_ids:
    print(f"\n  Target: {uid:#010x}")
    for name in tex_names:
        for variant in [name, name.removeprefix("ef_"), name.upper(), name.lower()]:
            for fn, fname in [(crc32, "crc32"), (murmur32, "murmur32"), (fnv1a32, "fnv1a32"), (djb2, "djb2")]:
                h = fn(variant)
                if h == uid:
                    print(f"    MATCH {fname}: '{variant}' -> {h:#010x}")

# Also: maybe the ID is just the lower 32 bits of a hash40
# hash40 = crc32 of string, with length in bits 32-39
print("\n\nChecking if IDs are lower 32 bits of hash40 (crc32 part):")
for uid in unknown_ids:
    print(f"  {uid:#010x}: no match found via crc32 above")

# Print the actual IDs in decimal too
print("\nUnknown IDs in decimal:")
for uid in unknown_ids:
    print(f"  {uid:#010x} = {uid}")
