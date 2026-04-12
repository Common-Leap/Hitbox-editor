#!/usr/bin/env python3
"""Dump the sub-sections of each EMTR to find CADP and understand texture index storage."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

def r32(off): 
    if off + 4 > len(data): return 0
    return struct.unpack_from("<I", data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from("<H", data, off)[0]

block_offset = r16(0x16)
NULL = 0xFFFFFFFF

def sec_magic(base): return bytes(data[base:base+4]) if base + 4 <= len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(base + 0x04)
def sec_next_off(base): return r32(base + 0x0C)
def sec_attr_off(base): return r32(base + 0x10)  # attribute/child offset
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_off(base): return r32(base + 0x08)
def sec_child_cnt(base): return r16(base + 0x1C)

# Walk to ESTA
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b"ESTA":
        break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

# Walk first ESET
eset_base = sec + sec_child_off(sec)
print(f"ESTA at {sec:#x}, first ESET at {eset_base:#x}")

# Walk first 3 EMTRs of first ESET
emtr_base = eset_base + sec_child_off(eset_base)
for j in range(3):
    if sec_magic(emtr_base) != b"EMTR": break
    emtr_bin = emtr_base + sec_bin_off(emtr_base)
    emtr_name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode("utf-8", errors="replace")
    
    # The attr_off field (offset 0x10) points to sub-sections
    attr_raw = r32(emtr_base + 0x10)
    print(f"\nEMTR[{j}] '{emtr_name}' base={emtr_base:#x} attr_raw={attr_raw:#x}")
    print(f"  Header: {data[emtr_base:emtr_base+32].hex()}")
    
    if attr_raw != NULL and attr_raw != 0:
        sub = emtr_base + attr_raw
        print(f"  Sub-sections starting at {sub:#x}:")
        for k in range(16):
            if sub + 4 > len(data): break
            sub_magic = sec_magic(sub)
            sub_size = sec_size(sub)
            sub_next = sec_next_off(sub)
            sub_bin = sub + sec_bin_off(sub)
            print(f"    [{k}] '{sub_magic.decode('ascii', errors='?')}' size={sub_size:#x} next={sub_next:#x} bin={sub_bin:#x}")
            if sub_magic == b"CADP":
                # Dump CADP binary
                print(f"      CADP bin[0:16]: {data[sub_bin:sub_bin+16].hex()}")
                tex_idx = r32(sub_bin)
                print(f"      tex_idx={tex_idx}")
            if sub_next == NULL or sub_next == 0: break
            sub = sub + sub_next
    else:
        print(f"  No sub-sections (attr_raw={attr_raw:#x})")
    
    next_off = sec_next_off(emtr_base)
    if next_off == NULL: break
    emtr_base = emtr_base + next_off

# Also check the second ESET (P_SamusAttackBomb)
print("\n\n=== P_SamusAttackBomb ===")
eset_base2 = eset_base
for _ in range(10):
    next_off = sec_next_off(eset_base2)
    if next_off == NULL: break
    eset_base2 = eset_base2 + next_off
    eset_bin = eset_base2 + sec_bin_off(eset_base2)
    set_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode("utf-8", errors="replace")
    if "Bomb" in set_name or "bomb" in set_name:
        print(f"Found ESET '{set_name}' at {eset_base2:#x}")
        emtr_base = eset_base2 + sec_child_off(eset_base2)
        for j in range(5):
            if sec_magic(emtr_base) != b"EMTR": break
            emtr_bin = emtr_base + sec_bin_off(emtr_base)
            emtr_name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode("utf-8", errors="replace")
            attr_raw = r32(emtr_base + 0x10)
            print(f"\n  EMTR[{j}] '{emtr_name}' attr_raw={attr_raw:#x}")
            if attr_raw != NULL and attr_raw != 0:
                sub = emtr_base + attr_raw
                for k in range(16):
                    if sub + 4 > len(data): break
                    sub_magic = sec_magic(sub)
                    sub_size = sec_size(sub)
                    sub_next = sec_next_off(sub)
                    sub_bin = sub + sec_bin_off(sub)
                    print(f"    [{k}] '{sub_magic.decode('ascii', errors='?')}' size={sub_size:#x}")
                    if sub_magic == b"CADP":
                        tex_idx = r32(sub_bin)
                        print(f"      CADP tex_idx={tex_idx} bin={data[sub_bin:sub_bin+8].hex()}")
                    if sub_next == NULL or sub_next == 0: break
                    sub = sub + sub_next
            next_off = sec_next_off(emtr_base)
            if next_off == NULL: break
            emtr_base = emtr_base + next_off
        break
