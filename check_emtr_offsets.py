#!/usr/bin/env python3
"""Check the EMTR section offsets to understand why emtr_static is wrong in Rust."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f:
    raw = f.read()

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

def r32(off):
    if off + 4 > len(data): return 0
    return struct.unpack_from('<I', data, off)[0]
def r16(off):
    if off + 2 > len(data): return 0
    return struct.unpack_from('<H', data, off)[0]

NULL = 0xFFFFFFFF
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
    if nxt == NULL: break
    sec = sec + nxt

print(f"ESTA at {sec:#x}")
eset_child_rel = sec_child_off(sec)
eset = sec + eset_child_rel
print(f"First ESET at {eset:#x}")

emtr_child_rel = sec_child_off(eset)
emtr = eset + emtr_child_rel
print(f"First EMTR at {emtr:#x}")

# Dump EMTR section header
print(f"EMTR header (32 bytes): {data[emtr:emtr+32].hex()}")
print(f"  magic:      {sec_magic(emtr)}")
print(f"  size:       {sec_size(emtr):#x}")
print(f"  child_off:  {sec_child_off(emtr):#x}")
print(f"  next_off:   {sec_next_off(emtr):#x}")
print(f"  attr_off:   {r32(emtr+0x10):#x}")
print(f"  bin_off:    {sec_bin_off(emtr):#x}")
print(f"  child_cnt:  {sec_child_cnt(emtr)}")

emtr_bin_rel = sec_bin_off(emtr)
emtr_bin = emtr + emtr_bin_rel
emtr_static = emtr_bin + 80
print(f"\nemtr_bin = {emtr:#x} + {emtr_bin_rel:#x} = {emtr_bin:#x}")
print(f"emtr_static = emtr_bin + 80 = {emtr_static:#x}")
name_bytes = data[emtr_bin+16:emtr_bin+80].split(b'\x00')[0]
print(f"emtr_name (at emtr_bin+16): '{name_bytes.decode()}'")

# Check sampler slots
sampler_base_2464 = emtr_static + 2464
sampler_base_2472 = emtr_static + 2472
print(f"\nsampler_base (2464) = {sampler_base_2464:#x}")
print(f"sampler_base (2472) = {sampler_base_2472:#x}")

for label, sb in [("2464", sampler_base_2464), ("2472", sampler_base_2472)]:
    print(f"\n  Slots at offset {label}:")
    for slot in range(3):
        soff = sb + slot * 32
        if soff + 8 > len(data): break
        id_lo = r32(soff)
        id_hi = r32(soff + 4)
        print(f"    slot[{slot}] @ {soff:#x}: id_lo={id_lo:#010x} id_hi={id_hi:#010x}")

# Now check what the Rust code actually computes
# The Rust code uses: emtr_base = emtr section offset (relative to data start)
# emtr_bin = emtr_base + sec_bin_off(emtr_base)
# emtr_static_off = emtr_bin + 80
# Then passes emtr_static_off to parse_vfxb_emitter as `base`
# Inside parse_vfxb_emitter: sampler_base = base + 2464

# But wait — the Rust code also has:
# let emtr_bin = emtr_base + sec_bin_off(emtr_base);
# let emtr_name = read_str_fixed(emtr_bin + 16, 64);
# let emtr_static_off = emtr_bin + 80;
# deferred_emtrs.push(DeferredEmtr { emtr_static_off, emtr_base, ... });
# Then: Self::parse_vfxb_emitter(data, de.emtr_static_off, ...)
# Inside parse_vfxb_emitter: base = emtr_static_off
# sampler_base = base + 2464

print(f"\n=== Rust code simulation ===")
print(f"emtr_base (EMTR section) = {emtr:#x}")
print(f"sec_bin_off(emtr_base) = {sec_bin_off(emtr):#x}")
print(f"emtr_bin = {emtr_bin:#x}")
print(f"emtr_static_off (base for parse_vfxb_emitter) = {emtr_static:#x}")
print(f"sampler_base = base + 2464 = {emtr_static + 2464:#x}")
print(f"slot[0] id_lo = {r32(emtr_static + 2464):#010x}")

# Also check: what does the Rust code read at base+16 for num_color0_keys?
print(f"\nbase+16 (num_color0_keys) = {r32(emtr_static + 16)}")
print(f"base+20 (num_alpha0_keys) = {r32(emtr_static + 20)}")
print(f"base+24 (num_color1_keys) = {r32(emtr_static + 24)}")
print(f"base+28 (num_alpha1_keys) = {r32(emtr_static + 28)}")
print(f"base+32 (num_scale_keys)  = {r32(emtr_static + 32)}")

# Dump first 64 bytes of emtr_static
print(f"\nFirst 64 bytes at emtr_static ({emtr_static:#x}):")
print(data[emtr_static:emtr_static+64].hex())
