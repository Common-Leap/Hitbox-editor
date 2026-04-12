#!/usr/bin/env python3
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

def r32(off): return struct.unpack_from("<I", data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from("<H", data, off)[0] if off+2<=len(data) else 0

block_offset = r16(0x16)
NULL = 0xFFFFFFFF

def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(base+4)
def sec_child_off(base): return r32(base+8)
def sec_next_off(base): return r32(base+0xC)
def sec_attr_off(base): return r32(base+0x10)
def sec_bin_off(base): return r32(base+0x14)
def sec_child_cnt(base): return r16(base+0x1C)

# Walk top-level sections
sec = block_offset
while sec + 4 <= len(data):
    magic = sec_magic(sec)
    size = sec_size(sec)
    child_off = sec_child_off(sec)
    next_off = sec_next_off(sec)
    attr_off = sec_attr_off(sec)
    bin_off = sec_bin_off(sec)
    child_cnt = sec_child_cnt(sec)
    print(f"{sec:#x}: '{magic.decode('ascii','replace')}' size={size:#x} child_off={child_off:#x} next={next_off:#x} attr={attr_off:#x} bin={bin_off:#x} child_cnt={child_cnt}")
    
    if magic == b"GRTF":
        # Check children
        if child_off != NULL and child_off != 0:
            child_abs = sec + child_off
            print(f"  GRTF child at {child_abs:#x}: '{sec_magic(child_abs).decode('ascii','replace')}'")
            print(f"  child header: {data[child_abs:child_abs+32].hex()}")
        # Check attr
        if attr_off != NULL and attr_off != 0:
            attr_abs = sec + attr_off
            print(f"  GRTF attr at {attr_abs:#x}: '{sec_magic(attr_abs).decode('ascii','replace')}'")
    
    if next_off == NULL: break
    sec = sec + next_off
