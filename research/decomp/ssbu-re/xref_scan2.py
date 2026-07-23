import struct
from dump_paths import dump_file
img=dump_file('main_decompressed.bin').read_bytes()
TEXT_END=0x2ee2000
targets={0x035832b6:"EmitterCalc",0x035832d8:"EmitterConstBuf",0x035832fa:"EmitterSetSort",
 0x0358331c:"ParticleSort",0x03583511:"ParticleSortFail"}
tset=set(targets)
regpage=[None]*32
hits={}
n=TEXT_END//4
mv=memoryview(img)
for i in range(n):
    w=struct.unpack_from('<I',mv,i*4)[0]
    pc=i*4
    if (w & 0x9F000000)==0x90000000:  # ADRP
        rd=w&0x1f
        immlo=(w>>29)&3; immhi=(w>>5)&0x7ffff
        imm=((immhi<<2)|immlo)
        if imm & (1<<20): imm-= (1<<21)  # sign extend 21-bit
        page=(pc & ~0xfff) + (imm<<12)
        regpage[rd]=page
    elif (w & 0xFF800000)==0x91000000:  # ADD imm (64-bit, not sub)
        rd=w&0x1f; rn=(w>>5)&0x1f
        imm12=(w>>10)&0xfff; sh=(w>>22)&1
        if sh: imm12<<=12
        base=regpage[rn]
        if base is not None:
            tgt=base+imm12
            if tgt in tset:
                hits.setdefault(tgt,[]).append(pc)
        regpage[rd]=None
    else:
        # crude: many instrs clobber regs; reset dest for common reg-writing forms is hard.
        pass
for t,name in targets.items():
    a=hits.get(t,[])
    print(f"{name} @ {t:#x}: {len(a)} xref(s): "+", ".join(hex(x) for x in a[:10]))
