#!/usr/bin/env python3
"""Check all BNTX texture names and find matches with GTNT names."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

# Find BNTX
bntx_base = next((i for i in range(len(data)-4) if data[i:i+4] == b'BNTX'), None)
if not bntx_base:
    print("No BNTX found")
    exit()

# Get all texture names from _STR
str_names = []
str_pos = bntx_base
while str_pos + 4 <= len(data):
    if data[str_pos:str_pos+4] == b'_STR':
        str_count = r32(data, str_pos + 16)
        soff = str_pos + 20
        for _ in range(min(str_count, 512)):
            if soff + 2 > len(data): break
            slen = r16(data, soff)
            soff += 2
            if soff + slen > len(data): break
            s = data[soff:soff+slen].decode('utf-8', errors='replace')
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 1

print(f"BNTX has {len(str_names)} textures")

# GTNT names we need to find
gtnt_names = [
    'ef_cmn_grade00', 'ef_cmn_bomb_indirect00', 'ef_cmn_smoke11',
    'ef_cmn_smoke14', 'ef_cmn_smoke15', 'ef_cmn_impact00',
    'ef_cmn_fireimpact02', 'ef_cmn_fire00', 'ef_cmn_debris00',
    'ef_samus_impact08', 'ef_item_impact14', 'ef_cmn_impact04',
    'ef_cmn_spark04', 'ef_cmn_flare03', 'ef_cmn_parts01',
    'ef_samus_impactring04', 'ef_cmn_ring00', 'ef_samus_impactring05',
]

print("\nGTNT names in BNTX:")
for name in gtnt_names:
    if name in str_names:
        idx = str_names.index(name)
        print(f"  FOUND: '{name}' at index {idx}")
    else:
        print(f"  MISSING: '{name}'")

print("\nAll BNTX texture names:")
for i, name in enumerate(str_names):
    print(f"  [{i:3d}] {name}")
