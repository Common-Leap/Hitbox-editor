import struct, lz4.block, sys
from dump_paths import dump_file
d=dump_file('exefs/main').read_bytes()
def u32(o): return struct.unpack_from('<I',d,o)[0]
assert d[:4]==b'NSO0'
flags=u32(0xC)
segs={}
for i,name in enumerate(['text','ro','data']):
    base=0x10+i*0x10
    fo,mo,ds=u32(base),u32(base+4),u32(base+8)
    csz=u32(0x60+i*4)
    raw=d[fo:fo+csz]
    if flags&(1<<i):
        dec=lz4.block.decompress(raw, uncompressed_size=ds)
    else:
        dec=raw[:ds]
    segs[name]=(mo,dec)
ro=segs['ro'][1]
dynstr_off,dynstr_sz=u32(0x90),u32(0x94)
dynsym_off,dynsym_sz=u32(0x98),u32(0x9C)
dynstr=ro[dynstr_off:dynstr_off+dynstr_sz]
def cstr(o):
    e=dynstr.find(b'\0',o); return dynstr[o:e].decode('utf-8','replace')
nsyms=dynsym_sz//24
print(f"# segments: text@{hex(segs['text'][0])} ro@{hex(segs['ro'][0])} data@{hex(segs['data'][0])}", file=sys.stderr)
print(f"# dynsym entries: {nsyms}", file=sys.stderr)
out=[]
for i in range(nsyms):
    st_name,st_info,st_other,st_shndx,st_value,st_size=struct.unpack_from('<IBBHQQ',ro,dynsym_off+i*24)
    nm=cstr(st_name)
    out.append((st_value,st_size,nm))
# save full list
with open('dynsym.txt','w') as f:
    for v,s,nm in out:
        f.write(f"{v:#x}\t{s}\t{nm}\n")
print(f"# wrote dynsym.txt ({len(out)} syms)", file=sys.stderr)
# search
import re
pat=re.compile(r'eft|Eft|EFT|[Pp]article|[Ee]mitter|[Vv]fx|VFX')
hits=[(v,s,nm) for v,s,nm in out if pat.search(nm)]
print(f"# matching syms: {len(hits)}", file=sys.stderr)
for v,s,nm in hits[:60]:
    print(f"{v:#011x} {s:6d} {nm}")
