#!/usr/bin/env python3
"""
Probe the actual binary data to find correct offsets for key fields.
We know:
- smokeBomb emit_type should be 1 (Circle)
- smokeBomb blend_type should be 3 (Screen) 
- smokeBomb scale should be ~10
- The emitter_trans for P_SamusAttackBomb[0] (flare1) should be (0,0,0) or small values

Strategy: scan around the expected offsets to find where valid values appear.
"""
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

# Find P_SamusAttackBomb ESET and get first two emitters (flare1, smokeBomb)
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
                print(f"Found ESET '{set_name}'")
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(min(3, sec_child_cnt(eset_base))):
                    if emtr_base + 4 > len(data): break
                    if sec_magic(emtr_base) != b'EMTR': break
                    emtr_bin = emtr_base + sec_bin_off(emtr_base)
                    emtr_static = emtr_bin + 80
                    name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
                    base = emtr_static
                    
                    print(f"\n  EMTR[{ei}] '{name}' base={base:#x}")
                    
                    # Scan for emit_type values (0-15) in range base+1900..base+2200
                    print(f"  Scanning for emit_type (0-15) in range base+1900..base+2200:")
                    for off in range(1900, 2200):
                        v = r8(data, base + off)
                        if 0 <= v <= 15:
                            # Check if next byte is also small (blend_type 0-4)
                            blend = r8(data, base + off + 6)
                            if 0 <= blend <= 4:
                                print(f"    off={off:#x} ({off}): emit_type={v} blend_at+6={blend}")
                    
                    # Scan for blend_type=3 (Screen) in range base+1900..base+2200
                    print(f"  Scanning for blend_type=3 (Screen) at base+off+6:")
                    for off in range(1900, 2200):
                        v = r8(data, base + off + 6)
                        if v == 3:
                            emit = r8(data, base + off)
                            if 0 <= emit <= 15:
                                print(f"    off={off:#x} ({off}): emit_type={emit} blend=3")
                    
                    # Also dump raw bytes around expected EmitterShapeInfo location
                    expected_shape = 2048  # from our calculation
                    print(f"  Raw bytes at base+{expected_shape-20}..base+{expected_shape+20}:")
                    for i in range(-20, 21):
                        v = r8(data, base + expected_shape + i)
                        print(f"    base+{expected_shape+i} ({expected_shape+i:#x}): {v:#04x} ({v})", end="")
                        if v <= 15: print(" <-- possible emit_type", end="")
                        if v <= 4: print(" <-- possible blend_type", end="")
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
