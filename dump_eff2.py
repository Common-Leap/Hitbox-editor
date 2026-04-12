#!/usr/bin/env python3
"""Find the VFXB inside the EFFN container and dump texture info."""
import struct, binascii

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"

with open(EFF_PATH, "rb") as f:
    raw = f.read()

print(f"File size: {len(raw)}, magic: {raw[:4]}")

# Find VFXB inside the file
vfxb_pos = raw.find(b"VFXB")
if vfxb_pos < 0:
    print("No VFXB found!")
    exit(1)
print(f"VFXB found at offset {vfxb_pos:#x}")
data = raw[vfxb_pos:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from("<I", data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from("<H", data, off)[0]

vfx_version = r16(0x0A)
block_offset = r16(0x16)
print(f"VFXB version={vfx_version}, block_offset={block_offset:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base):
    if base + 4 > len(data): return b'\x00\x00\x00\x00'
    return bytes(data[base:base+4])
def sec_size(base): return r32(base + 0x04)
def sec_next_off(base): return r32(base + 0x0C)
def sec_bin_off(base): return r32(base + 0x14)
def sec_child_off(base): return r32(base + 0x08)
def sec_child_cnt(base): return r16(base + 0x1C)

# Collect all BNTX texture names in order
all_bntx_names = []

sec = block_offset
iters = 0
while sec + 4 <= len(data) and iters < 512:
    iters += 1
    magic = sec_magic(sec)

    if magic == b"GTNT":
        bin_off = sec + sec_bin_off(sec)
        bin_len = sec_size(sec) - sec_bin_off(sec)
        print(f"\nGTNT at {sec:#x}: bin_off={bin_off:#x} len={bin_len}")
        print(f"  raw[0:32]: {data[bin_off:bin_off+32].hex()}")

    if magic == b"GRTF":
        bin_off_rel = sec_bin_off(sec)
        bin_start = sec + bin_off_rel
        bin_len = sec_size(sec) - bin_off_rel
        bntx_pos = data.find(b"BNTX", bin_start, bin_start + min(bin_len, 0x100000))
        if bntx_pos >= 0:
            # Find _STR
            str_pos = bntx_pos
            end = min(bin_start + bin_len, len(data))
            while str_pos + 4 < end:
                if data[str_pos:str_pos+4] == b"_STR":
                    str_count = r32(str_pos + 16)
                    soff = str_pos + 20
                    names = []
                    for _ in range(min(str_count, 300)):
                        if soff + 2 > len(data): break
                        slen = r16(soff); soff += 2
                        if soff + slen > len(data): break
                        s = data[soff:soff+slen].decode("utf-8", errors="replace")
                        soff += slen + 1
                        if soff % 2 != 0: soff += 1
                        if s: names.append(s)
                    print(f"\nGRTF at {sec:#x}: {len(names)} BNTX names")
                    for i, n in enumerate(names[:5]):
                        print(f"  [{i}] '{n}'")
                    if len(names) > 5:
                        print(f"  ... ({len(names)} total)")
                    all_bntx_names.extend(names)
                    break
                str_pos += 8

    next_off = sec_next_off(sec)
    if next_off == NULL: break
    sec = sec + next_off

print(f"\nTotal BNTX names: {len(all_bntx_names)}")

# Now find the first EMTR and dump its sampler slots
targets = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1"}

sec = block_offset
iters = 0
while sec + 4 <= len(data) and iters < 512:
    iters += 1
    if sec_magic(sec) == b"ESTA":
        child_cnt = sec_child_cnt(sec)
        child_off = sec_child_off(sec)
        eset_base = sec + child_off
        for _ in range(min(child_cnt, 5)):
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

                # Try both sampler base offsets
                for samp_label, samp_base in [("v22_2464", emtr_static + 2464), ("v22_2472", emtr_static + 2472)]:
                    slot0_id = struct.unpack_from("<I", data, samp_base)[0] if samp_base + 4 <= len(data) else 0
                    print(f"\n  ESET='{set_name}' EMTR[{j}]='{emtr_name}' static={emtr_static:#x}")
                    print(f"    {samp_label}: slot0_id={slot0_id:#010x} {'<-- MATCH: '+targets[slot0_id] if slot0_id in targets else ''}")
                    for slot in range(3):
                        soff = samp_base + slot * 32
                        if soff + 8 > len(data): break
                        tid = struct.unpack_from("<I", data, soff)[0]
                        print(f"      slot[{slot}] @ {soff:#x}: id={tid:#010x} {'MATCH:'+targets.get(tid,'') if tid in targets else ''}")

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

# Try to match TextureIDs against BNTX names
print(f"\n\nTrying to match TextureIDs against {len(all_bntx_names)} BNTX names:")
def crc32(s): return binascii.crc32(s.encode()) & 0xFFFFFFFF
def nw4f(s):
    h = 0
    for c in s.encode():
        h = (((h << 5) | (h >> 27)) ^ c) & 0xFFFFFFFF
    return h
def fnv1a(s):
    h = 0x811c9dc5
    for c in s.encode(): h = ((h ^ c) * 0x01000193) & 0xFFFFFFFF
    return h

for i, name in enumerate(all_bntx_names):
    for v in [name, name.removeprefix("ef_")]:
        for fn, fname in [(crc32,"crc32"),(nw4f,"nw4f"),(fnv1a,"fnv1a")]:
            h = fn(v)
            if h in targets:
                print(f"  MATCH: {fname}('{v}') = {h:#010x} -> tex[{i}]='{name}' (emitter: {targets[h]})")
