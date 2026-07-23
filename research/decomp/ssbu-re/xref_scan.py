import struct
from capstone import *
from capstone.arm64 import *
from dump_paths import dump_file
img=dump_file('main_decompressed.bin').read_bytes()
TEXT_END=0x2ee2000  # ro starts here; text is [0, TEXT_END)
targets={
 0x035832b6:"EmitterCalc",0x035832d8:"EmitterConstBuf",0x035832fa:"EmitterSetSort",
 0x0358331c:"ParticleSort",0x03583511:"ParticleSortFail",
}
md=Cs(CS_ARCH_ARM64,CS_MODE_LITTLE_ENDIAN)
md.detail=True
regpage={}
hits={}
code=img[:TEXT_END]
for insn in md.disasm(code,0):
    if insn.id==ARM64_INS_ADRP:
        ops=insn.operands
        regpage[ops[0].reg]=ops[1].imm
    elif insn.id==ARM64_INS_ADD and len(insn.operands)==3:
        ops=insn.operands
        if ops[1].type==ARM64_OP_REG and ops[2].type==ARM64_OP_IMM:
            base=regpage.get(ops[1].reg)
            if base is not None:
                tgt=base+ops[2].imm
                if tgt in targets:
                    hits.setdefault(tgt,[]).append(insn.address)
        # dest reg clobbered
        regpage.pop(ops[0].reg,None)
for t,name in targets.items():
    addrs=hits.get(t,[])
    print(f"{name} @ {t:#x}: {len(addrs)} xref(s): "+", ".join(hex(a) for a in addrs[:8]))
