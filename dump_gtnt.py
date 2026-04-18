#!/usr/bin/env python3
"""Check GTNT map and what slot 1 TextureIDs resolve to."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
raw = open(EFF_PATH,'rb').read()

def r8(d,o): return d[o] if o<len(d) else 0
def r16(d,o): return struct.unpack_from('<H',d,o)[0] if o+2<=len(d) else 0
def r32(d,o): return struct.unpack_from('<I',d,o)[0] if o+4<=len(d) else 0
def rstr(d,o,n=64):
    if o==0 or o>=len(d): return ''
    e=d.find(b'\x00',o,o+n); return d[o:(e if e>=0 else o+n)].decode('utf-8',errors='replace')

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
NULL = 0xFFFFFFFF

def sec_next(b): return r32(data,b+0xC)
def sec_bin(b): return r32(data,b+0x14)
def sec_magic(b): return bytes(data[b:b+4]) if b+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(b): return r32(data,b+8)
def sec_child_cnt(b): return r16(data,b+0x1C)

block_offset = r16(data, 0x16)

# Build GTNT map
gtnt_map = {}
sec = block_offset
while sec+4<=len(data):
    m = sec_magic(sec)
    if m==b'GRTF':
        # Check for GTNT child
        child_cnt = sec_child_cnt(sec)
        child_off = sec_child_off(sec)
        if child_cnt>0 and child_off!=NULL:
            child = sec + child_off
            if child+4<=len(data) and sec_magic(child)==b'GTNT':
                bin_off = sec_bin(child)
                bin_start = child + bin_off
                bin_len = r32(data,child+4) - bin_off
                # Parse GTNT
                off = bin_start
                while off+16<=bin_start+bin_len and off+16<=len(data):
                    lo = r32(data,off); hi = r32(data,off+4)
                    tex_id = (hi<<32)|lo
                    entry_size = r32(data,off+8)
                    name_len = r32(data,off+12)
                    if tex_id==0 and entry_size==0: break
                    if name_len>0 and off+16+name_len<=len(data):
                        name = data[off+16:off+16+name_len].decode('utf-8',errors='replace').rstrip('\x00')
                        if name: gtnt_map[tex_id] = name
                    if entry_size==0: break
                    off += entry_size
                print(f"GTNT: {len(gtnt_map)} entries")
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt

# Also build from BNTX names (hash40+crc32)
import hashlib

def crc32_of(b):
    crc = 0xFFFFFFFF
    for byte in b:
        crc ^= byte
        for _ in range(8):
            if crc&1: crc = (crc>>1)^0xEDB88320
            else: crc>>=1
    return (~crc)&0xFFFFFFFF

# Get BNTX names
bntx_names = []
sec = block_offset
while sec+4<=len(data):
    if sec_magic(sec)==b'GRTF':
        bin_off = sec_bin(sec)
        bin_start = sec + bin_off
        bntx_off = data.find(b'BNTX', bin_start)
        if bntx_off>=0:
            str_pos = bntx_off
            while str_pos+4<=len(data):
                if bytes(data[str_pos:str_pos+4])==b'_STR':
                    str_count = r32(data,str_pos+16)
                    soff = str_pos+20
                    for _ in range(min(str_count,512)):
                        if soff+2>len(data): break
                        slen = r16(data,soff); soff+=2
                        if soff+slen>len(data): break
                        s = data[soff:soff+slen].decode('utf-8',errors='replace')
                        soff+=slen+1
                        if soff%2!=0: soff+=1
                        if s: bntx_names.append(s)
                    break
                str_pos+=1
        break
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt

print(f"BNTX names: {len(bntx_names)}")

# Build hash40+crc32 map from BNTX names
hash_map = {}
for name in bntx_names:
    crc = crc32_of(name.encode())
    hash_map[crc] = name
    # Also try without ef_ prefix
    if name.startswith('ef_'):
        stripped = name[3:]
        crc2 = crc32_of(stripped.encode())
        hash_map[crc2] = name

# Check the specific TextureIDs from smokeBomb
test_ids = [
    0x00000000c6a6335c,  # flare1 slot 0
    0x0000000081d59f24,  # smokeBomb slot 0
    0x00000000f23f06f8,  # smokeBomb slot 1
    0x00000000da3564c9,  # smokeLoop slot 0
    0x000000007cdcd964,  # smokeLoop slot 1
]

print("\nTextureID resolution:")
for tex_id in test_ids:
    lo = tex_id & 0xFFFFFFFF
    # Check GTNT map
    name_gtnt = gtnt_map.get(tex_id) or gtnt_map.get(lo)
    # Check hash map
    name_hash = hash_map.get(lo)
    print(f"  {tex_id:#018x}: GTNT={name_gtnt} hash={name_hash}")
    if name_gtnt:
        idx = bntx_names.index(name_gtnt) if name_gtnt in bntx_names else -1
        print(f"    -> bntx_idx={idx}")
    elif name_hash:
        idx = bntx_names.index(name_hash) if name_hash in bntx_names else -1
        print(f"    -> bntx_idx={idx}")

# Show what format the resolved textures are
print("\nBNTX texture formats for resolved names:")
sec = block_offset
while sec+4<=len(data):
    if sec_magic(sec)==b'GRTF':
        bin_off = sec_bin(sec)
        bin_start = sec + bin_off
        bntx_off = data.find(b'BNTX', bin_start)
        if bntx_off>=0:
            bntx_base = bntx_off
            nx_off = bntx_base+0x20
            brtd_rel = r32(data, nx_off+0x10)
            data_blk = nx_off+0x10+brtd_rel
            brti_offsets = []
            pos = bntx_base
            while pos+4<=data_blk and pos+4<=len(data):
                if bytes(data[pos:pos+4])==b'BRTI':
                    brti_offsets.append(pos)
                    blen = r32(data,pos+4)
                    pos += max(blen,0x90)
                else:
                    pos += 8
            for idx in [0,1,2,3,4,5,6,7,8,9,10]:
                if idx < len(brti_offsets) and idx < len(bntx_names):
                    brti = brti_offsets[idx]
                    fmt_raw = r32(data,brti+0x1C)
                    w = r32(data,brti+0x24); h = r32(data,brti+0x28)
                    cs = r32(data,brti+0x58)
                    fmt_type = (fmt_raw>>8)&0xFF
                    ch_a = (cs>>24)&0xFF
                    src = {0:'zero',1:'one',2:'R',3:'G',4:'B',5:'A'}
                    fmt_names = {0x1A:'BC1',0x1B:'BC2',0x1C:'BC3',0x1D:'BC4',0x1E:'BC5',0x1F:'BC6H',0x20:'BC7',0x0B:'RGBA8',0x0C:'BGRA8'}
                    print(f"  [{idx}] '{bntx_names[idx]}': {w}x{h} {fmt_names.get(fmt_type,'?')} A_src={src.get(ch_a,ch_a)}")
        break
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt
