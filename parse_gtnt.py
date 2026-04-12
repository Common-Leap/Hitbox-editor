#!/usr/bin/env python3
"""Parse the GTNT section found inside GRTF at 0xc9950."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

# GTNT is at 0xc9950 in the VFXB data
gtnt_off = 0xc9950
print(f"GTNT at {gtnt_off:#x}")
print(f"Magic: {data[gtnt_off:gtnt_off+4]}")

def r32(off): return struct.unpack_from("<I", data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from("<H", data, off)[0] if off+2<=len(data) else 0

# Section header
sec_size = r32(gtnt_off + 4)
sec_child_off = r32(gtnt_off + 8)
sec_next_off = r32(gtnt_off + 0xC)
sec_bin_off = r32(gtnt_off + 0x14)
sec_child_cnt = r16(gtnt_off + 0x1C)
print(f"size={sec_size:#x} child_off={sec_child_off:#x} next={sec_next_off:#x} bin_off={sec_bin_off:#x} child_cnt={sec_child_cnt}")

bin_start = gtnt_off + sec_bin_off
print(f"Binary data at {bin_start:#x}")
print(f"First 64 bytes: {data[bin_start:bin_start+64].hex()}")

# Parse the binary: each entry appears to be:
# [u32 TextureID][u32 zero][u32 name_size][u32 name_len][name_bytes...][padding]
# Based on the context dump:
# 044658ad 00000000 28000000 12000000 65665f73616d75735f6275726e65723030 000000
# = 0xad584604, 0, 0x28(40), 0x12(18), "ef_samus_burner00", pad

off = bin_start
entries = {}
print("\nParsing GTNT entries:")
for i in range(50):
    if off + 16 > len(data): break
    tex_id = r32(off)
    zero = r32(off + 4)
    field1 = r32(off + 8)   # might be total entry size or name offset
    field2 = r32(off + 12)  # might be name length
    
    if tex_id == 0 and zero == 0 and field1 == 0: break
    
    # Try to read name: field2 bytes starting at off+16
    name_len = field2
    if off + 16 + name_len <= len(data) and name_len > 0 and name_len < 100:
        name = data[off+16:off+16+name_len].rstrip(b'\x00').decode('utf-8', errors='replace')
    else:
        name = ""
    
    # Entry size: field1 bytes total (including header)
    entry_size = field1
    
    print(f"  [{i}] id={tex_id:#010x} field1={field1} field2={field2} name='{name}'")
    entries[tex_id] = name
    
    if entry_size > 0 and entry_size < 0x200:
        off += entry_size
    else:
        off += 16 + ((name_len + 3) & ~3)

print(f"\nTotal entries: {len(entries)}")
print("\nTextureID -> Name mapping:")
for tid, name in entries.items():
    print(f"  {tid:#010x} -> '{name}'")
