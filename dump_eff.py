#!/usr/bin/env python3
"""
Dump the GTNT section and BNTX texture names from an .eff file,
then try to match the TextureIDs found in EMTR sampler slots.
"""
import struct, binascii, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"

with open(EFF_PATH, "rb") as f:
    data = f.read()

print(f"File size: {len(data)} bytes")
print(f"Magic: {data[:4]}")

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from("<I", data, off)[0]

def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from("<H", data, off)[0]

def r8(off):
    if off >= len(data): return 0
    return data[off]

# Find VFXB block offset
vfx_version = r16(0x0A)
block_offset = r16(0x16)
print(f"VFX version: {vfx_version}, block_offset: {block_offset:#x}")

NULL = 0xFFFFFFFF

def sec_magic(base):
    if base + 4 > len(data): return b'\x00\x00\x00\x00'
    return bytes(data[base:base+4])

def sec_size(base): return r32(base + 0x04)
def sec_child_off(base): return r32(base + 0x08)
def sec_next_off(base): return r32(base + 0x0C)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_cnt(base): return r16(base + 0x1C)

# Walk sections
sec = block_offset
iters = 0
gtnt_found = False
bntx_names = []

while sec + 4 <= len(data) and iters < 512:
    iters += 1
    magic = sec_magic(sec)
    print(f"  Section at {sec:#x}: {magic}")

    if magic == b"GTNT":
        gtnt_found = True
        bin_off = sec + sec_bin_off(sec)
        bin_len = sec_size(sec) - sec_bin_off(sec)
        print(f"    GTNT found! bin_off={bin_off:#x} bin_len={bin_len}")
        # Dump first 64 bytes of GTNT binary
        print(f"    GTNT raw: {data[bin_off:bin_off+64].hex()}")
        # Try to parse as array of (hash64, name_offset) pairs
        off = bin_off
        for i in range(min(20, bin_len // 16)):
            h = struct.unpack_from("<Q", data, off)[0]
            name_off = struct.unpack_from("<I", data, off+8)[0]
            print(f"    entry[{i}]: hash={h:#018x} name_off={name_off:#x}")
            off += 16

    if magic == b"GRTF":
        bin_off_rel = sec_bin_off(sec)
        bin_start = sec + bin_off_rel
        bin_len = sec_size(sec) - bin_off_rel
        # Find BNTX inside
        bntx_pos = data.find(b"BNTX", bin_start, bin_start + bin_len)
        if bntx_pos >= 0:
            # Find _STR block
            str_pos = bntx_pos
            while str_pos + 4 < bin_start + bin_len:
                if data[str_pos:str_pos+4] == b"_STR":
                    str_count = r32(str_pos + 16)
                    soff = str_pos + 20
                    names = []
                    for _ in range(min(str_count, 200)):
                        if soff + 2 > len(data): break
                        slen = r16(soff); soff += 2
                        if soff + slen > len(data): break
                        s = data[soff:soff+slen].decode("utf-8", errors="replace")
                        soff += slen + 1
                        if soff % 2 != 0: soff += 1
                        if s: names.append(s)
                    print(f"    GRTF BNTX at {bntx_pos:#x}: {len(names)} texture names")
                    for i, n in enumerate(names[:10]):
                        print(f"      [{i}] '{n}'")
                    bntx_names.extend(names)
                    break
                str_pos += 8

    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

if not gtnt_found:
    print("\nNo GTNT section found!")

# Now try to match the TextureIDs
targets = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1"}
print(f"\nTotal BNTX names collected: {len(bntx_names)}")
print("\nTrying to match TextureIDs against BNTX names:")

def crc32(s): return binascii.crc32(s.encode()) & 0xFFFFFFFF
def nw4f(s):
    h = 0
    for c in s.encode():
        h = (((h << 5) | (h >> 27)) ^ c) & 0xFFFFFFFF
    return h

for i, name in enumerate(bntx_names):
    for v in [name, name.removeprefix("ef_")]:
        for fn, fname in [(crc32, "crc32"), (nw4f, "nw4f")]:
            h = fn(v)
            if h in targets:
                print(f"  MATCH: {fname}('{v}') = {h:#010x} -> tex[{i}]='{name}'")

# Also dump the EMTR sampler area for the first emitter to understand the ID format
print("\nLooking for ESTA/ESET/EMTR sections to dump sampler data...")
sec = block_offset
iters = 0
while sec + 4 <= len(data) and iters < 512:
    iters += 1
    magic = sec_magic(sec)
    if magic == b"ESTA":
        child_cnt = sec_child_cnt(sec)
        child_off = sec_child_off(sec)
        eset_base = sec + child_off
        for _ in range(min(child_cnt, 3)):
            if sec_magic(eset_base) != b"ESET": break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = data[eset_bin+16:eset_bin+80].split(b'\x00')[0].decode("utf-8", errors="replace")
            emtr_cnt = sec_child_cnt(eset_base)
            emtr_off = sec_child_off(eset_base)
            emtr_base = eset_base + emtr_off
            for j in range(min(emtr_cnt, 3)):
                if sec_magic(emtr_base) != b"EMTR": break
                emtr_bin = emtr_base + sec_bin_off(emtr_base)
                emtr_name = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0].decode("utf-8", errors="replace")
                emtr_static = emtr_bin + 80
                # Sampler base for v22: emtr_static + 2464
                samp_base = emtr_static + 2464
                print(f"\n  ESET='{set_name}' EMTR='{emtr_name}' static={emtr_static:#x} samp_base={samp_base:#x}")
                for slot in range(3):
                    soff = samp_base + slot * 32
                    if soff + 16 > len(data): break
                    raw = data[soff:soff+16]
                    tex_id = struct.unpack_from("<I", raw, 0)[0]
                    print(f"    slot[{slot}]: raw={raw.hex()} tex_id={tex_id:#010x}")
                next_off = sec_next_off(emtr_base)
                if next_off == NULL: break
                emtr_base = emtr_base + next_off
            next_off = sec_next_off(eset_base)
            if next_off == NULL: break
            eset_base = eset_base + next_off
        break
    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off
