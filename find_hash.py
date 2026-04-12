#!/usr/bin/env python3
"""
Brute-force what hash of BNTX texture names produces the TextureIDs in the EMTR sampler.
IDs are 32-bit: 0xad584604, 0x10edf80b, 0xc6a6335c
"""
import binascii, struct, zlib

targets = {0xad584604, 0x10edf80b, 0xc6a6335c}

# All 103 BNTX texture names — need to get the full list
# Run: cargo run 2>&1 | grep "^\[GRTF\] tex\[" > grtf_names.txt
# For now use what we know plus guesses
import os
names = []
if os.path.exists("grtf_names.txt"):
    for line in open("grtf_names.txt"):
        # format: [GRTF] tex[N] = 'name'
        if "= '" in line:
            name = line.split("= '")[1].rstrip("'\n")
            names.append(name)
    print(f"Loaded {len(names)} names from grtf_names.txt")
else:
    print("grtf_names.txt not found, using partial list")
    names = [
        "ef_item_lightning01","ef_cmn_parts01","ef_cmn_impact14","ef_cmn_stone00",
        "ef_cmn_mask01","ef_cmn_impact08","ef_cmn_smoke01","ef_cmn_fire05",
        "ef_item_impact01","ef_cmn_stone01","ef_cmn_impact11","ef_item_flash05",
        "ef_item_impact11","ef_samus_color00","ef_cmn_impactflash00","ef_cmn_wind00",
        "ef_samus_wind00","ef_captain_fireimpact00","ef_cmn_impact00",
    ]

def crc32(s): return binascii.crc32(s.encode()) & 0xFFFFFFFF
def adler32(s): return zlib.adler32(s.encode()) & 0xFFFFFFFF

def fnv1a32(s):
    h = 0x811c9dc5
    for c in s.encode(): h = ((h ^ c) * 0x01000193) & 0xFFFFFFFF
    return h

def fnv132(s):
    h = 0x811c9dc5
    for c in s.encode(): h = ((h * 0x01000193) ^ c) & 0xFFFFFFFF
    return h

def djb2(s):
    h = 5381
    for c in s.encode(): h = ((h << 5) + h + c) & 0xFFFFFFFF
    return h

def sdbm(s):
    h = 0
    for c in s.encode(): h = (c + (h << 6) + (h << 16) - h) & 0xFFFFFFFF
    return h

# Nintendo uses a specific hash for texture IDs in older VFXB
# Try: hash of just the name without "ef_" prefix, various encodings
def nintendo_hash(s):
    """Common Nintendo string hash used in older NW4F/NW4C tools"""
    h = 0
    for c in s.encode():
        h = (h * 31 + c) & 0xFFFFFFFF
    return h

def nintendo_hash2(s):
    h = 0
    for c in s.encode():
        h = (h * 65599 + c) & 0xFFFFFFFF
    return h

hashfns = [
    ("crc32", crc32),
    ("adler32", adler32),
    ("fnv1a32", fnv1a32),
    ("fnv132", fnv132),
    ("djb2", djb2),
    ("sdbm", sdbm),
    ("nintendo*31", nintendo_hash),
    ("nintendo*65599", nintendo_hash2),
]

found = set()
for name in names:
    variants = [name, name.removeprefix("ef_"), name.upper(), name.lower(),
                name.removeprefix("ef_").upper()]
    for v in variants:
        for fname, fn in hashfns:
            h = fn(v)
            if h in targets:
                print(f"MATCH: {fname}('{v}') = {h:#010x}")
                found.add(h)

if not found:
    print("No matches found with standard hashes.")
    print("\nPrinting all hashes for first 5 names to compare manually:")
    for name in names[:5]:
        print(f"\n  '{name}':")
        for fname, fn in hashfns:
            print(f"    {fname}: {fn(name):#010x}  stripped: {fn(name.removeprefix('ef_')):#010x}")
