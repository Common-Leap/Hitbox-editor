#!/usr/bin/env python3
"""
Scan the VFXB binary for the TextureIDs and show surrounding context.
Also try to find a GTNT-equivalent section by scanning for the IDs near string data.
"""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, "rb") as f:
    raw = f.read()

vfxb_pos = raw.find(b"VFXB")
data = raw[vfxb_pos:]

targets = {0xad584604: "burner1", 0x10edf80b: "burner2", 0xc6a6335c: "flash1",
           0x81d59f24: "smokeBomb_t0", 0xf23f06f8: "smokeBomb_t1",
           0xda3564c9: "smokeLoop_t0", 0x7cdcd964: "smokeLoop_t1"}

print("Scanning VFXB for TextureID values and surrounding context:")
for off in range(0, len(data) - 4, 4):
    val = struct.unpack_from("<I", data, off)[0]
    if val in targets:
        # Show 32 bytes before and after
        start = max(0, off - 32)
        end = min(len(data), off + 36)
        ctx = data[start:end]
        # Try to find any printable strings nearby
        nearby_str = ""
        for i in range(max(0, off-64), min(len(data), off+64)):
            if all(32 <= data[j] < 127 for j in range(i, min(i+4, len(data)))):
                s_end = i
                while s_end < len(data) and 32 <= data[s_end] < 127:
                    s_end += 1
                if s_end - i >= 4:
                    nearby_str = data[i:s_end].decode('ascii', errors='replace')
                    break
        print(f"  {off:#010x}: {val:#010x} ({targets[val]})")
        print(f"    context: {ctx.hex()}")
        if nearby_str:
            print(f"    nearby string: '{nearby_str[:60]}'")

# Also: look for a section that maps IDs to indices
# The GTNT section in newer files has entries like: [u64 hash][u32 index][u32 pad]
# Try scanning for patterns where our target IDs appear followed by small integers
print("\nLooking for ID->index mapping patterns:")
for off in range(0, len(data) - 8, 4):
    val = struct.unpack_from("<I", data, off)[0]
    if val in targets:
        # Check if next 4 bytes is a small integer (texture index 0-200)
        next_val = struct.unpack_from("<I", data, off + 4)[0]
        if 0 < next_val < 200:
            print(f"  {off:#x}: id={val:#010x} ({targets[val]}) -> index={next_val}")
        # Also check 8 bytes ahead
        if off + 8 < len(data):
            next8 = struct.unpack_from("<I", data, off + 8)[0]
            if 0 < next8 < 200:
                print(f"  {off:#x}: id={val:#010x} ({targets[val]}) -> index@+8={next8}")

# Print all unique 4-byte values that appear right after our target IDs
print("\nValues immediately after each target ID:")
for off in range(0, len(data) - 8, 4):
    val = struct.unpack_from("<I", data, off)[0]
    if val in targets:
        after = struct.unpack_from("<I", data, off + 4)[0]
        print(f"  {targets[val]}: after={after:#010x} ({after})")
