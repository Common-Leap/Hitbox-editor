#!/usr/bin/env python3
"""Targeted probe: dump raw bytes around the EmitterRenderState region for smokeBomb."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
vfx_version = r16(data, 0x0A)
block_offset = r16(data, 0x16)

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

# Find P_SamusAttackBomb and get smokeBomb (emitter 1)
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        eset_base = sec + sec_child_off(sec)
        for _ in range(sec_child_cnt(sec)):
            if eset_base + 4 > len(data): break
            if sec_magic(eset_base) != b'ESET': break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
            if 'AttackBomb' in set_name:
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(3):
                    if emtr_base + 4 > len(data): break
                    if sec_magic(emtr_base) != b'EMTR': break
                    emtr_bin = emtr_base + sec_bin_off(emtr_base)
                    emtr_static = emtr_bin + 80
                    name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
                    base = emtr_static
                    
                    print(f"\n=== EMTR[{ei}] '{name}' base={base:#x} ===")
                    
                    # Dump bytes 1900-2200 with float interpretation
                    print("off  | hex  | u8  | f32 (if aligned)")
                    for off in range(1900, 2200, 4):
                        b0 = r8(data, base+off)
                        b1 = r8(data, base+off+1)
                        b2 = r8(data, base+off+2)
                        b3 = r8(data, base+off+3)
                        fval = rf32(data, base+off)
                        u32val = r32(data, base+off)
                        print(f"  {off:4d} ({off:#06x}): {b0:02x} {b1:02x} {b2:02x} {b3:02x}  u32={u32val:10d}  f32={fval:12.4f}", end="")
                        # Flag interesting values
                        if b0 <= 15 and b1 == 0 and b2 == 0 and b3 == 0:
                            print(f"  <-- u8={b0} (emit_type?)", end="")
                        if 0.0 < fval < 100.0 and fval == int(fval):
                            print(f"  <-- nice float", end="")
                        print()
                    
                    nxt = sec_next_off(emtr_base)
                    if nxt == NULL: break
                    emtr_base += nxt
                break
            nxt = sec_next_off(eset_base)
            if nxt == NULL: break
            eset_base += nxt
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec += nxt
