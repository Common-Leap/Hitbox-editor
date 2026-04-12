#!/usr/bin/env python3
"""Find the actual GTNT section and parse it correctly."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

# Find GTNT magic
gtnt_pos = data.find(b"GTNT")
print(f"GTNT magic at {gtnt_pos:#x}")
if gtnt_pos < 0:
    print("Not found!")
    exit(1)

print(f"Bytes around GTNT: {data[gtnt_pos-8:gtnt_pos+64].hex()}")

def r32(off): return struct.unpack_from("<I", data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from("<H", data, off)[0] if off+2<=len(data) else 0

# Section header at gtnt_pos
sec_size = r32(gtnt_pos + 4)
sec_child_off = r32(gtnt_pos + 8)
sec_next_off = r32(gtnt_pos + 0xC)
sec_bin_off_val = r32(gtnt_pos + 0x14)
sec_child_cnt = r16(gtnt_pos + 0x1C)
print(f"size={sec_size:#x} child_off={sec_child_off:#x} next={sec_next_off:#x} bin_off={sec_bin_off_val:#x} child_cnt={sec_child_cnt}")

bin_start = gtnt_pos + sec_bin_off_val
print(f"Binary data at {bin_start:#x}")
print(f"First 128 bytes: {data[bin_start:bin_start+128].hex()}")

# Parse entries: from the context dump we know the format is:
# [u32 TextureID][u32 zero][u32 total_entry_size][u32 name_len][name_bytes...][padding]
# e.g.: 044658ad 00000000 28000000 12000000 "ef_samus_burner00" 000000
# total_entry_size=0x28=40, name_len=0x12=18, name="ef_samus_burner00" (18 chars)
# 40 = 16 (header) + 18 (name) + 6 (padding to align to 8)

off = bin_start
entries = {}
print("\nParsing GTNT entries:")
for i in range(200):
    if off + 16 > len(data): break
    tex_id = r32(off)
    zero = r32(off + 4)
    entry_size = r32(off + 8)
    name_len = r32(off + 12)
    
    if tex_id == 0 and zero == 0 and entry_size == 0: break
    if entry_size == 0 or entry_size > 0x200: break
    
    if name_len > 0 and name_len < 128 and off + 16 + name_len <= len(data):
        name = data[off+16:off+16+name_len].rstrip(b'\x00').decode('utf-8', errors='replace')
    else:
        name = ""
    
    print(f"  [{i}] {tex_id:#010x} -> '{name}' (entry_size={entry_size}, name_len={name_len})")
    entries[tex_id] = name
    off += entry_size

print(f"\nTotal: {len(entries)} entries")
