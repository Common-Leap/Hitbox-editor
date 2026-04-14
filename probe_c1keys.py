#!/usr/bin/env python3
"""Check color1 keys for P_SamusAttackBomb emitters."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r8(d, off): return d[off] if off < len(d) else 0
def rstr(d, off, maxlen=64):
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

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

sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        eset_base = sec + sec_child_off(sec)
        for _ in range(sec_child_cnt(sec)):
            if sec_magic(eset_base) != b'ESET': break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = rstr(data, eset_bin + 16)
            if 'AttackBomb' in set_name:
                eset_child_cnt = sec_child_cnt(eset_base)
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(eset_child_cnt):
                    if sec_magic(emtr_base) != b'EMTR': break
                    emtr_bin = emtr_base + sec_bin_off(emtr_base)
                    base = emtr_bin + 80

                    name = rstr(data, base - 64, 64)
                    num_c0 = r32(data, base + 16)
                    num_a0 = r32(data, base + 20)
                    num_c1 = r32(data, base + 24)
                    num_a1 = r32(data, base + 28)

                    # Color1 keys at base + 880 + 128 + 128 = base + 1136
                    color1_off = base + 880 + 128 + 128
                    print(f"[{ei}] '{name}' c0={num_c0} a0={num_a0} c1={num_c1} a1={num_a1}")
                    if num_c1 > 0:
                        for k in range(min(num_c1, 4)):
                            ko = color1_off + k * 16
                            r = rf32(data, ko + 0)
                            g = rf32(data, ko + 4)
                            b = rf32(data, ko + 8)
                            t = rf32(data, ko + 12)
                            print(f"  c1[{k}]: R={r:.4f} G={g:.4f} B={b:.4f} T={t:.4f}")

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
