import struct, bisect, sys, json
from dump_paths import dump_file
img=dump_file('main_decompressed.bin').read_bytes()
TEXT_END=0x2ee2000; mv=memoryview(img); n=TEXT_END//4
edges=[]  # (pc, target)
bl_targets=set()
for i in range(n):
    w=struct.unpack_from('<I',mv,i*4)[0]
    if (w&0xFC000000)==0x94000000:  # BL
        imm=w&0x03FFFFFF
        if imm&(1<<25): imm-=(1<<26)
        t=i*4+imm*4
        if 0<=t<TEXT_END:
            edges.append((i*4,t)); bl_targets.add(t)
starts=sorted(bl_targets)
def func_of(pc):
    j=bisect.bisect_right(starts,pc)-1
    return starts[j] if j>=0 else None
callers={}; callees={}
for pc,t in edges:
    f=func_of(pc)
    if f is None: continue
    callees.setdefault(f,set()).add(t)
    callers.setdefault(t,set()).add(f)
json.dump({"starts":starts,
           "callers":{hex(k):[hex(x) for x in v] for k,v in callers.items()},
           "callees":{hex(k):[hex(x) for x in v] for k,v in callees.items()}},
          open('callgraph.json','w'))
print(f"functions(BL targets): {len(starts)}, edges: {len(edges)}")
# query helper
def show(f):
    print(f"\n{f:#x}: callers={[hex(x) for x in sorted(callers.get(f,[]))][:12]}")
    print(f"        callees={[hex(x) for x in sorted(callees.get(f,[]))][:16]}")
for q in [0x1a730,0x1a6b8 and func_of(0x1a6b8)]:
    show(q)
