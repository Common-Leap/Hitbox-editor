#!/usr/bin/env python3
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

def r32(off): return struct.unpack_from("<I", data, off)[0] if off + 4 <= len(data) else 0
def r16(off): return struct.unpack_from("<H", data, off)[0] if off + 2 <= len(data) else 0

block_offset = r16(0x16)
NULL = 0xFFFFFFFF
def sec_next_off(base): return r32(base + 0x0C)
def sec_bin_off(base): return r32(base + 0x14)
def sec_magic(base): return bytes(data[base:base+4]) if base + 4 <= len(data) else b'\x00\x00\x00\x00'

# Find GRTF
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b"GRTF":
        break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

bntx_start = sec + sec_bin_off(sec)
bd = data[bntx_start:]
bntx_base = bd.find(b"BNTX")
bd = bd[bntx_base:]

# Get _STR names
str_names = []
str_pos = 0
while str_pos + 4 <= len(bd):
    if bd[str_pos:str_pos+4] == b"_STR":
        str_count = r16(str_pos + 16) if str_pos + 18 <= len(bd) else 0
        str_count = struct.unpack_from("<I", bd, str_pos + 16)[0]
        soff = str_pos + 20
        for _ in range(min(str_count, 300)):
            if soff + 2 > len(bd): break
            slen = struct.unpack_from("<H", bd, soff)[0]; soff += 2
            if soff + slen > len(bd): break
            s = bd[soff:soff+slen].decode("utf-8", errors="replace")
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 8

print(f"Total textures: {len(str_names)}")
print(f"tex[73] = '{str_names[73] if len(str_names) > 73 else 'N/A'}'")
print(f"tex[8]  = '{str_names[8] if len(str_names) > 8 else 'N/A'}'")
print(f"tex[19] = '{str_names[19] if len(str_names) > 19 else 'N/A'}'")
print(f"tex[30] = '{str_names[30] if len(str_names) > 30 else 'N/A'}'")

# Print all samus-related textures
print("\nAll samus/bomb/fire/smoke textures:")
for i, n in enumerate(str_names):
    if any(k in n.lower() for k in ["samus", "bomb", "fire", "smoke", "flare", "flash", "burner"]):
        print(f"  [{i}] '{n}'")
