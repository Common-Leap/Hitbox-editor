#!/usr/bin/env python3
"""Dump ALL emitter set names from the VFXB to find the forward aerial effect."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from('<H', data, off)[0]

def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_next_off(base): return r32(base + 0xC)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_off(base): return r32(base + 0x08)
def sec_child_cnt(base): return r16(base + 0x1C)

block_offset = r16(0x16)
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        break
    nxt = sec_next_off(sec)
    if nxt == 0xFFFFFFFF: break
    sec = sec + nxt

eset_base = sec + sec_child_off(sec)
idx = 0
print("All emitter sets:")
while eset_base + 4 <= len(data):
    eset_bin = eset_base + sec_bin_off(eset_base)
    eset_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
    print(f"  [{idx:3d}] '{eset_name}'")
    idx += 1
    nxt = sec_next_off(eset_base)
    if nxt == 0xFFFFFFFF: break
    eset_base = eset_base + nxt

print(f"\nTotal: {idx} emitter sets")
