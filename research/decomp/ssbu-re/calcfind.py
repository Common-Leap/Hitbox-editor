from unicorn import *; from unicorn.arm64_const import *
import struct, json, bisect
from emu2 import IMG, size, STACK, STACK_SZ, RET
# fixed addresses for our crafted state (high, won't collide with image/stack)
E=0x10000000   # emitter
P=0x10010000   # particle buffer
R=0x10020000   # resource
def build(mu):
    for base in (E,P,R):
        mu.mem_map(base, 0x10000)
    # emitter fields
    mu.mem_write(E+0x28, struct.pack('<i', 1))         # particle count = 1
    mu.mem_write(E+0x44, struct.pack('<f', 10.0))      # current time
    mu.mem_write(E+0xb0, struct.pack('<Q', P))         # particle buffer ptr
    mu.mem_write(E+0xc0, struct.pack('<Q', P))         # alt pos buffer ptr
    mu.mem_write(E+0x1d0, struct.pack('<Q', P))
    mu.mem_write(E+0x238, struct.pack('<Q', R))        # resource ptr
    mu.mem_write(R+0x10, struct.pack('<Q', R+0x100))   # resource->data
    # particle: birth=0, life=60, and tag the rest with distinct floats per offset
    for off in range(0, 0x60, 4):
        mu.mem_write(P+off, struct.pack('<f', 1000.0+off))
    mu.mem_write(P+0x8, struct.pack('<f', 0.0))        # birth
    mu.mem_write(P+0xc, struct.pack('<f', 60.0))       # life
    mu.mem_write(P+0x0, struct.pack('<i', 1))          # valid flag
    # resource: tag with floats incl air_res-like 0.9 scattered
    for off in range(0, 0x400, 4):
        mu.mem_write(R+0x100+off, struct.pack('<f', 0.9))
cg=json.load(open('callgraph.json'))
starts=sorted(set(int(x,16) for x in cg['callers'])|set(int(x,16) for x in cg['callees']))
cands=[f for f in starts if 0x10000<=f<0x80000]
print(f"scanning {len(cands)} cluster functions for particle-buffer writers...")
hits=[]
for f in cands:
    mu=Uc(UC_ARCH_ARM64, UC_MODE_LITTLE_ENDIAN)
    mu.mem_map(0,size); mu.mem_write(0,IMG); mu.mem_map(STACK,STACK_SZ)
    faulted=set()
    def flt(mu,a,addr,sz,v,u,faulted=faulted):
        pg=addr&~0xfff
        if pg not in faulted and not (E<=pg<E+0x10000 or P<=pg<P+0x10000 or R<=pg<R+0x10000):
            try:mu.mem_map(pg,0x1000)
            except:pass
            faulted.add(pg)
        return True
    pw=[]
    def w(mu,a,addr,sz,v,u,pw=pw):
        if P<=addr<P+0x100: pw.append((addr-P,sz,v))
    mu.hook_add(UC_HOOK_MEM_READ_UNMAPPED|UC_HOOK_MEM_WRITE_UNMAPPED|UC_HOOK_MEM_FETCH_UNMAPPED, flt)
    mu.hook_add(UC_HOOK_MEM_WRITE, w)
    build(mu)
    # try emitter as x0
    for i in range(8): mu.reg_write(UC_ARM64_REG_X0+i, 0)
    mu.reg_write(UC_ARM64_REG_X0, E)
    mu.reg_write(UC_ARM64_REG_SP, STACK+STACK_SZ-0x4000); mu.reg_write(UC_ARM64_REG_LR, RET)
    try: mu.emu_start(f, RET, count=500000)
    except UcError: pass
    if pw: hits.append((f, pw))
print(f"functions that wrote into the particle buffer: {len(hits)}")
for f,pw in hits[:40]:
    offs=sorted(set(o for o,_,_ in pw))
    print(f"  {f:#x}: wrote offsets {[hex(o) for o in offs]}")
