import struct, sys
from dump_paths import dump_file
img=dump_file('main_decompressed.bin').read_bytes()
TEXT_END=0x2ee2000
mv=memoryview(img)
def bl_callers(target):
    out=[]
    n=TEXT_END//4
    for i in range(n):
        w=struct.unpack_from('<I',mv,i*4)[0]
        if (w & 0xFC000000)==0x94000000:  # BL
            imm=w & 0x03FFFFFF
            if imm & (1<<25): imm-=(1<<26)
            tgt=i*4 + imm*4
            if tgt==target: out.append(i*4)
    return out
for seed,name in [(0x1eeb0,"worksize_logger"),(0x20adc,"particlesort_assert_fn")]:
    c=bl_callers(seed)
    print(f"{name} @ {seed:#x}: {len(c)} caller(s): "+", ".join(hex(x) for x in c[:12]))
