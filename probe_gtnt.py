#!/usr/bin/env python3
"""Check GTNT map and texture resolution for P_SamusAttackBomb emitters."""
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
vfx_version = r16(data, 0x0A)
block_offset = r16(data, 0x16)
print(f"VFXB version={vfx_version} block_offset={block_offset:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)
def sec_size(base): return r32(data, base+0x04)

# Walk top-level sections
sec = block_offset
gtnt_found = False
grtf_found = False
bntx_names = []

while sec + 4 <= len(data):
    magic = sec_magic(sec)
    print(f"Section at {sec:#x}: {magic}")
    
    if magic == b'GRTF':
        grtf_found = True
        bin_off_rel = sec_bin_off(sec)
        bin_start = sec + bin_off_rel
        bin_len = sec_size(sec) - bin_off_rel
        print(f"  GRTF bin_start={bin_start:#x} bin_len={bin_len}")
        
        # Check for BNTX in GRTF
        bntx_pos = None
        for i in range(bin_start, min(bin_start + bin_len, len(data) - 4)):
            if data[i:i+4] == b'BNTX':
                bntx_pos = i
                break
        if bntx_pos:
            print(f"  BNTX found at {bntx_pos:#x}")
        
        # Check GRTF children for GTNT
        child_cnt = sec_child_cnt(sec)
        child_off_rel = sec_child_off(sec)
        print(f"  GRTF child_cnt={child_cnt} child_off_rel={child_off_rel:#x}")
        if child_cnt > 0 and child_off_rel != NULL:
            child = sec + child_off_rel
            child_magic = sec_magic(child)
            print(f"  GRTF child[0] at {child:#x}: {child_magic}")
            if child_magic == b'GTNT':
                gtnt_found = True
                gtnt_bin_off = sec_bin_off(child)
                gtnt_bin_start = child + gtnt_bin_off
                gtnt_bin_len = sec_size(child) - gtnt_bin_off
                print(f"  GTNT (GRTF child) bin_start={gtnt_bin_start:#x} len={gtnt_bin_len}")
                # Parse GTNT
                off = gtnt_bin_start
                payload_end = gtnt_bin_start + gtnt_bin_len
                count = 0
                while off + 16 <= payload_end and count < 20:
                    tex_id_lo = r32(data, off)
                    tex_id_hi = r32(data, off + 4)
                    tex_id = (tex_id_hi << 32) | tex_id_lo
                    entry_size = r32(data, off + 8)
                    name_len = r32(data, off + 12)
                    if tex_id == 0 and entry_size == 0: break
                    if entry_size > 0x200: break
                    if name_len > 0 and off + 16 + name_len <= payload_end:
                        name = data[off+16:off+16+name_len].decode('utf-8', errors='replace').rstrip('\x00')
                        print(f"    GTNT entry: id={tex_id:#018x} name='{name}'")
                    if entry_size == 0: break
                    off += entry_size
                    count += 1
    
    elif magic == b'GTNT':
        gtnt_found = True
        bin_off_rel = sec_bin_off(sec)
        bin_start = sec + bin_off_rel
        bin_len = sec_size(sec) - bin_off_rel
        print(f"  GTNT (top-level) bin_start={bin_start:#x} len={bin_len}")
    
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    next_abs = sec + nxt
    if next_abs <= sec: break
    sec = next_abs

print(f"\nGTNT found: {gtnt_found}")
print(f"GRTF found: {grtf_found}")

# Check the tex_ids for flare1 against the GTNT map
print(f"\nflare1 tex_id: 0x00000000c6a6335c")
print(f"smokeBomb tex_ids: 0x0000000081d59f24, 0x00000000f23f06f8")

# Check if these IDs are in the BNTX texture names (CRC32 fallback)
import binascii
def crc32_of(s):
    return binascii.crc32(s.encode()) & 0xFFFFFFFF

# Get BNTX texture names
bntx_base = next((i for i in range(len(data)-4) if data[i:i+4] == b'BNTX'), None)
if bntx_base:
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
    
    print(f"\nBNTX has {len(str_names)} textures")
    
    # Check CRC32 of each texture name against the tex_ids
    target_ids = [0xc6a6335c, 0x81d59f24, 0xf23f06f8, 0x6deca699, 0x0e16323b, 0x03b50f63]
    for name in str_names:
        crc = crc32_of(name)
        if crc in target_ids:
            print(f"  CRC32 match: '{name}' -> {crc:#010x}")
        # Also try without ef_ prefix
        if name.startswith('ef_'):
            crc2 = crc32_of(name[3:])
            if crc2 in target_ids:
                print(f"  CRC32 match (no ef_): '{name}' -> {crc2:#010x}")
        # Also try hash40
        # (skip hash40 for now, just check CRC32)
    
    print("\nAll CRC32 values for first 10 textures:")
    for i, name in enumerate(str_names[:10]):
        crc = crc32_of(name)
        print(f"  [{i}] '{name}' crc32={crc:#010x}")
