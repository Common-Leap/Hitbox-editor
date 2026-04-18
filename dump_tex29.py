#!/usr/bin/env python3
"""Check what texture index 29 is in the BNTX."""
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

# Find GRTF section and parse BNTX _STR names
block_offset = r16(data, 0x16)
sec = block_offset
while sec+4<=len(data):
    if sec_magic(sec)==b'GRTF':
        bin_off = sec_bin(sec)
        bin_start = sec + bin_off
        # Find BNTX in the GRTF payload
        bntx_off = data.find(b'BNTX', bin_start)
        if bntx_off >= 0:
            bntx_base = bntx_off
            # Find _STR block
            str_pos = bntx_base
            while str_pos+4<=len(data):
                if bytes(data[str_pos:str_pos+4])==b'_STR':
                    str_count = r32(data, str_pos+16)
                    soff = str_pos+20
                    names = []
                    for _ in range(min(str_count,512)):
                        if soff+2>len(data): break
                        slen = r16(data,soff); soff+=2
                        if soff+slen>len(data): break
                        s = data[soff:soff+slen].decode('utf-8',errors='replace')
                        soff+=slen+1
                        if soff%2!=0: soff+=1
                        if s: names.append(s)
                    print(f"Found {len(names)} BNTX textures")
                    # Show textures around index 29
                    for i,n in enumerate(names):
                        if abs(i-29)<=5:
                            print(f"  [{i}] '{n}'")
                    # Also show the BRTI for index 29 to get format
                    # Find BRTI structs
                    brti_offsets = []
                    pos = bntx_base
                    nx = r32(data, bntx_base+0x20+0x10)  # NX section BRTD offset
                    data_blk = bntx_base+0x20+0x10+nx
                    while pos+4<=data_blk and pos+4<=len(data):
                        if bytes(data[pos:pos+4])==b'BRTI':
                            brti_offsets.append(pos)
                            blen = r32(data,pos+4)
                            pos += max(blen,0x90)
                        else:
                            pos += 8
                    if 29 < len(brti_offsets):
                        brti = brti_offsets[29]
                        fmt_raw = r32(data, brti+0x1C)
                        w = r32(data, brti+0x24)
                        h = r32(data, brti+0x28)
                        comp_sel = r32(data, brti+0x58)
                        fmt_type = (fmt_raw>>8)&0xFF
                        print(f"\nBRTI[29]: {w}x{h} fmt={fmt_raw:#06x} fmt_type={fmt_type:#04x} comp_sel={comp_sel:#010x}")
                        # Decode comp_sel
                        ch_r = (comp_sel>>0)&0xFF
                        ch_g = (comp_sel>>8)&0xFF
                        ch_b = (comp_sel>>16)&0xFF
                        ch_a = (comp_sel>>24)&0xFF
                        src = {0:'zero',1:'one',2:'R',3:'G',4:'B',5:'A'}
                        print(f"  comp_sel: R={src.get(ch_r,ch_r)} G={src.get(ch_g,ch_g)} B={src.get(ch_b,ch_b)} A={src.get(ch_a,ch_a)}")
                        fmt_names = {0x1A:'BC1',0x1B:'BC2',0x1C:'BC3',0x1D:'BC4',0x1E:'BC5',0x1F:'BC6H',0x20:'BC7',0x0B:'RGBA8',0x0C:'BGRA8'}
                        print(f"  format: {fmt_names.get(fmt_type, f'unknown({fmt_type:#04x})')}")
                    break
                str_pos+=1
        break
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt

# Also check what textures the first few explosion emitters resolve to via GTNT
# by looking at their SamplerInfo TextureIDs
print("\n--- Checking SamplerInfo for P_SamusAttackBomb emitters ---")
def sec_child_off(b): return r32(data,b+8)
def sec_child_cnt(b): return r16(data,b+0x1C)

sec = block_offset
while sec+4<=len(data):
    if sec_magic(sec)==b'ESTA':
        eset_base = sec + sec_child_off(sec)
        for _ in range(sec_child_cnt(sec)):
            if eset_base+4>len(data): break
            if sec_magic(eset_base)!=b'ESET': break
            eset_bin = eset_base + sec_bin(eset_base)
            set_name = rstr(data, eset_bin+16)
            if 'attackbomb' in set_name.lower():
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(min(sec_child_cnt(eset_base),3)):
                    if emtr_base+4>len(data): break
                    if sec_magic(emtr_base)!=b'EMTR': break
                    emtr_bin = emtr_base + sec_bin(emtr_base)
                    emtr_name = rstr(data, emtr_bin+16)
                    base = emtr_bin+80
                    vfx_version = r16(data,0x0A)
                    sampler_base = base + (2472 if vfx_version>=37 else 2464 if vfx_version>21 else 2472)
                    print(f"\n  EMTR '{emtr_name}' sampler_base={sampler_base:#x}:")
                    for slot in range(3):
                        soff = sampler_base + slot*32
                        if soff+8>len(data): break
                        lo = r32(data,soff); hi = r32(data,soff+4)
                        tex_id = (hi<<32)|lo
                        if tex_id!=0 and lo!=0xffffffff:
                            print(f"    slot {slot}: TextureID={tex_id:#018x}")
                    nxt = sec_next(emtr_base)
                    if nxt==NULL: break
                    emtr_base+=nxt
            nxt = sec_next(eset_base)
            if nxt==NULL: break
            eset_base+=nxt
        break
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt
