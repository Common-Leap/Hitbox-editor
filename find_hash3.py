#!/usr/bin/env python3
"""
We know flash1 -> cadp_idx=19. Find what texture that is.
Then try to figure out what hash 0xad584604 and 0x10edf80b are.
"""
import binascii, struct

targets = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1"}

names = []
with open("grtf_names.txt") as f:
    for line in f:
        if "= '" in line:
            name = line.split("= '")[1].rstrip("'\n")
            names.append(name)

print(f"Total texture names: {len(names)}")
print(f"Texture at index 19: '{names[19] if len(names) > 19 else 'N/A'}'")
print()

# Try every possible 32-bit hash of every name variant
def try_all(name):
    variants = []
    variants.append(name)
    variants.append(name.removeprefix("ef_"))
    # Also try just the base part
    parts = name.split("_")
    for i in range(1, len(parts)):
        variants.append("_".join(parts[i:]))
    return variants

def crc32(s): return binascii.crc32(s.encode()) & 0xFFFFFFFF
def crc32_upper(s): return binascii.crc32(s.upper().encode()) & 0xFFFFFFFF

# Nintendo NW4F uses a specific hash: rotate-left-5 XOR
def nw4f_hash(s):
    h = 0
    for c in s.encode():
        h = (((h << 5) | (h >> 27)) ^ c) & 0xFFFFFFFF
    return h

def nw4f_hash2(s):
    """NW4F string hash variant 2"""
    h = 0
    for c in s.encode():
        h = ((h * 31) + c) & 0xFFFFFFFF
    return h

def nw4f_hash3(s):
    """NW4F string hash variant 3 - used in NW4C/NW4F effect systems"""
    h = 0
    for c in s.encode():
        h = (h * 0x1000193) ^ c
        h &= 0xFFFFFFFF
    return h

def hash_djb2_xor(s):
    h = 5381
    for c in s.encode():
        h = ((h << 5) ^ h ^ c) & 0xFFFFFFFF
    return h

hashfns = [
    ("crc32", crc32),
    ("crc32_upper", crc32_upper),
    ("nw4f_rotl5_xor", nw4f_hash),
    ("nw4f_*31", nw4f_hash2),
    ("fnv1a_xor", nw4f_hash3),
    ("djb2_xor", hash_djb2_xor),
]

print("Searching all 103 names × variants × hash functions:")
found = False
for i, name in enumerate(names):
    for v in try_all(name):
        for fname, fn in hashfns:
            h = fn(v)
            if h in targets:
                print(f"  MATCH: {fname}('{v}') = {h:#010x} -> tex[{i}]='{name}' (emitter: {targets[h]})")
                found = True

if not found:
    print("  No matches found.")
    print()
    print("Hashes of tex[19] with all functions:")
    if len(names) > 19:
        for v in try_all(names[19]):
            for fname, fn in hashfns:
                print(f"  {fname}('{v}') = {fn(v):#010x}")
