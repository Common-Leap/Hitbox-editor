#!/usr/bin/env python3
"""Check texture indices used by P_SamusAttackBomb emitters and their formats."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def rstr(d, off, maxlen=64):
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

# Parse BNTX to get texture names and formats
bntx_base = next((i for i in range(len(data)-4) if data[i:i+4] == b'BNTX'), None)
if bntx_base is None:
    print("No BNTX found")
else:
    nx = bntx_base + 0x20
    tex_count = r32(data, nx + 0x04)
    data_blk_abs = nx + 0x10 + r32(data, nx + 0x10)
    scan_end = data_blk_abs
    
    brti_offsets = []
    pos = bntx_base
    while pos + 4 <= scan_end:
        if data[pos:pos+4] == b'BRTI':
            brti_offsets.append(pos)
            brti_len = r32(data, pos + 4)
            pos += max(brti_len, 0x90)
        else:
            pos += 8
    
    # Get texture names
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
    
    print(f"Total BNTX textures: {len(brti_offsets)}")
    print(f"Total texture names: {len(str_names)}")
    
    # Show textures at specific indices used by P_SamusAttackBomb
    indices_of_interest = [0, 30, 73, 74, 75]
    for idx in indices_of_interest:
        if idx < len(brti_offsets):
            brti = brti_offsets[idx]
            fmt_raw = r32(data, brti + 0x1C)
            w = r32(data, brti + 0x24)
            h = r32(data, brti + 0x28)
            fmt_type = (fmt_raw >> 8) & 0xFF
            fmt_variant = fmt_raw & 0xFF
            name = str_names[idx] if idx < len(str_names) else f"tex_{idx}"
            print(f"  [{idx}] '{name}': {w}x{h} fmt={fmt_raw:#06x} (type={fmt_type:#04x} variant={fmt_variant:#04x})")
        else:
            print(f"  [{idx}] OUT OF RANGE")
    
    # Also show what the CADP fallback would assign
    # The CADP fallback uses last_tex_idx which propagates from previous emitters
    # For P_SamusAttackBomb, the first emitter with CADP is 'smokeBomb' -> cadp_idx=None -> last_tex_idx=Some(73)
    # Let's find what index 73 actually is
    print(f"\nAll texture names (first 10 and around index 73):")
    for i, name in enumerate(str_names[:10]):
        print(f"  [{i}] {name}")
    print("  ...")
    for i in range(max(0, 70), min(len(str_names), 80)):
        print(f"  [{i}] {str_names[i]}")
