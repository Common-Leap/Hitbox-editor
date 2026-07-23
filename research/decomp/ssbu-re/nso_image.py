import struct, lz4.block
from dump_paths import dump_file
d=dump_file('exefs/main').read_bytes()
def u32(o): return struct.unpack_from('<I',d,o)[0]
flags=u32(0xC); segs=[]
for i in range(3):
    base=0x10+i*0x10
    fo,mo,ds=u32(base),u32(base+4),u32(base+8); csz=u32(0x60+i*4)
    raw=d[fo:fo+csz]
    dec=lz4.block.decompress(raw, uncompressed_size=ds) if flags&(1<<i) else raw[:ds]
    segs.append((mo,ds,dec))
end=max(mo+ds for mo,ds,_ in segs)
img=bytearray(end)
for mo,ds,dec in segs: img[mo:mo+len(dec)]=dec
dump_file('main_decompressed.bin').write_bytes(img)
print("image size", hex(len(img)), "text@",hex(segs[0][0]),"ro@",hex(segs[1][0]),"data@",hex(segs[2][0]))
