#!/usr/bin/env python3
"""Dump the color key bytes from the P_SamusAttackBomb 'flare1' emitter to verify channel order."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]
def rf32(off):
    if off + 4 > len(data): return 0.0
    return struct.unpack_from('<f', data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from('<H', data, off)[0]
def r8(off):
    if off >= len(data): return 0
    return data[off]

def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(base + 4)
def sec_child_off(base): return r32(base + 8)
def sec_next_off(base): return r32(base + 0xC)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_cnt(base): return r16(base + 0x1C)

block_offset = r16(0x16)
sec = block_offset

# Find ESTA
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        break
    nxt = sec_next_off(sec)
    if nxt == 0xFFFFFFFF: break
    sec = sec + nxt

print(f"ESTA at {sec:#x}")
eset_child_rel = sec_child_off(sec)
eset = sec + eset_child_rel

# Walk ESETs to find P_SamusAttackBomb
while eset + 4 <= len(data):
    eset_bin = eset + sec_bin_off(eset)
    eset_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
    if eset_name == 'P_SamusAttackBomb':
        print(f"Found P_SamusAttackBomb at {eset:#x}")
        break
    nxt = sec_next_off(eset)
    if nxt == 0xFFFFFFFF: break
    eset = eset + nxt
else:
    print("P_SamusAttackBomb not found")
    sys.exit(1)

# Walk EMTRs in this ESET
emtr_child_rel = sec_child_off(eset)
emtr = eset + emtr_child_rel

while emtr + 4 <= len(data):
    emtr_bin = emtr + sec_bin_off(emtr)
    emtr_name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode('utf-8', errors='replace')
    emtr_static = emtr_bin + 80

    print(f"\n=== EMTR '{emtr_name}' ===")
    print(f"  emtr_bin={emtr_bin:#x} emtr_static={emtr_static:#x}")

    num_c0 = r32(emtr_static + 16)
    num_a0 = r32(emtr_static + 20)
    num_c1 = r32(emtr_static + 24)
    num_a1 = r32(emtr_static + 28)
    num_sc = r32(emtr_static + 32)
    print(f"  num_color0={num_c0} num_alpha0={num_a0} num_color1={num_c1} num_alpha1={num_a1} num_scale={num_sc}")

    # Color0 table at base+880
    c0_off = emtr_static + 880
    print(f"  Color0 table at {c0_off:#x}:")
    for k in range(min(num_c0, 8)):
        ko = c0_off + k * 16
        raw_bytes = data[ko:ko+16].hex()
        f0 = rf32(ko+0)
        f1 = rf32(ko+4)
        f2 = rf32(ko+8)
        f3 = rf32(ko+12)
        print(f"    key[{k}]: bytes={raw_bytes}")
        print(f"           f[0]={f0:.4f} f[1]={f1:.4f} f[2]={f2:.4f} f[3]={f3:.4f}")
        print(f"           (if time,R,G,B): t={f0:.4f} r={f1:.4f} g={f2:.4f} b={f3:.4f}")
        print(f"           (if R,G,B,time): r={f0:.4f} g={f1:.4f} b={f2:.4f} t={f3:.4f}")

    # EmitterInfo color at sequential walk position
    # We need to walk to find it — let's use the known offset from Switch Toolbox
    # EmitterInfo base color: version>=37 at base+2392, version>21 at base+2384, else base+2392
    # For v22: base+2384
    version = 22
    color_off = emtr_static + (2392 if version >= 37 or version <= 21 else 2384)
    # Actually for v22 (which is > 21 but < 37): base+2384
    color_off = emtr_static + 2384
    print(f"  EmitterInfo Color0 at {color_off:#x} (base+2384 for v22):")
    raw_bytes = data[color_off:color_off+16].hex()
    print(f"    bytes: {raw_bytes}")
    for i in range(4):
        print(f"    f[{i}]={rf32(color_off + i*4):.4f}")
    print(f"    (if R,G,B,A): r={rf32(color_off):.4f} g={rf32(color_off+4):.4f} b={rf32(color_off+8):.4f} a={rf32(color_off+12):.4f}")
    print(f"    (if B,G,R,A): b={rf32(color_off):.4f} g={rf32(color_off+4):.4f} r={rf32(color_off+8):.4f} a={rf32(color_off+12):.4f}")

    # Also check base+2392
    color_off2 = emtr_static + 2392
    print(f"  EmitterInfo Color0 at {color_off2:#x} (base+2392):")
    raw_bytes2 = data[color_off2:color_off2+16].hex()
    print(f"    bytes: {raw_bytes2}")
    print(f"    (if R,G,B,A): r={rf32(color_off2):.4f} g={rf32(color_off2+4):.4f} b={rf32(color_off2+8):.4f} a={rf32(color_off2+12):.4f}")
    print(f"    (if B,G,R,A): b={rf32(color_off2):.4f} g={rf32(color_off2+4):.4f} r={rf32(color_off2+8):.4f} a={rf32(color_off2+12):.4f}")

    nxt = sec_next_off(emtr)
    if nxt == 0xFFFFFFFF: break
    emtr = emtr + nxt
