#!/usr/bin/env python3
"""Check the actual decoded alpha values for ef_cmn_grade00 (bntx_idx=11)."""
import struct, sys

try:
    import image_dds
    HAS_IMAGE_DDS = True
except ImportError:
    HAS_IMAGE_DDS = False
    print("image_dds not available, using manual BC5 decode")

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
raw = open(EFF_PATH,'rb').read()

def r8(d,o): return d[o] if o<len(d) else 0
def r16(d,o): return struct.unpack_from('<H',d,o)[0] if o+2<=len(d) else 0
def r32(d,o): return struct.unpack_from('<I',d,o)[0] if o+4<=len(d) else 0

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
NULL = 0xFFFFFFFF

def sec_next(b): return r32(data,b+0xC)
def sec_bin(b): return r32(data,b+0x14)
def sec_magic(b): return bytes(data[b:b+4]) if b+4<=len(data) else b'\x00\x00\x00\x00'

block_offset = r16(data, 0x16)

# Find GRTF and get texture section
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
            data_blk_abs = nx_off+0x10+brtd_rel
            brtd_data_start = data_blk_abs+0x10

            # Find BRTI structs
            brti_offsets = []
            pos = bntx_base
            while pos+4<=data_blk_abs and pos+4<=len(data):
                if bytes(data[pos:pos+4])==b'BRTI':
                    brti_offsets.append(pos)
                    blen = r32(data,pos+4)
                    pos += max(blen,0x90)
                else:
                    pos += 8

            # Get BRTI[11] (ef_cmn_grade00)
            if 11 < len(brti_offsets):
                brti = brti_offsets[11]
                fmt_raw = r32(data,brti+0x1C)
                w = r32(data,brti+0x24); h = r32(data,brti+0x28)
                data_size = r32(data,brti+0x50)
                comp_sel = r32(data,brti+0x58)
                tile_mode = r16(data,brti+0x12)
                block_h_log2 = r32(data,brti+0x34)
                fmt_type = (fmt_raw>>8)&0xFF

                # Get pixel data offset
                pts_addr_lo = r32(data,brti+0x70); pts_addr_hi = r32(data,brti+0x74)
                pts_addr = (pts_addr_hi<<32)|pts_addr_lo
                pts_addr_abs = bntx_base + pts_addr
                if pts_addr>0 and pts_addr_abs+8<=len(data):
                    mip0_lo = r32(data,pts_addr_abs); mip0_hi = r32(data,pts_addr_abs+4)
                    mip0_rel = (mip0_hi<<32)|mip0_lo
                    pixel_start = bntx_base + mip0_rel
                else:
                    pixel_start = 0

                print(f"BRTI[11]: {w}x{h} fmt={fmt_raw:#06x} fmt_type={fmt_type:#04x} tile_mode={tile_mode} block_h_log2={block_h_log2}")
                print(f"  data_size={data_size} pixel_start={pixel_start:#x} comp_sel={comp_sel:#010x}")
                ch_a = (comp_sel>>24)&0xFF
                src = {0:'zero',1:'one',2:'R',3:'G',4:'B',5:'A'}
                print(f"  A_src={src.get(ch_a,ch_a)} ({ch_a})")

                if pixel_start>0 and pixel_start+data_size<=len(data):
                    raw_pixels = bytes(data[pixel_start:pixel_start+data_size])
                    print(f"  First 32 bytes of raw pixel data: {list(raw_pixels[:32])}")

                    # Manual BC5 decode of first block to check values
                    # BC5 block: 8 bytes for R channel + 8 bytes for G channel
                    if len(raw_pixels)>=16:
                        r0,r1 = raw_pixels[0],raw_pixels[1]
                        g0,g1 = raw_pixels[8],raw_pixels[9]
                        print(f"  First BC5 block: R0={r0} R1={r1} G0={g0} G1={g1}")
                        # The G channel (index 1 in decoded RGBA) is what A_src=G picks
                        # After swizzle: ch_r=1(one=255), ch_g=1(one=255), ch_b=1(one=255), ch_a=3(G)
                        # So alpha = G channel of decoded BC5
                        # G channel range: [g0, g1] interpolated
                        print(f"  G channel range: [{g0}, {g1}] -> alpha range [{g0}, {g1}]")
                        print(f"  If G0=G1=0, alpha is 0 everywhere (transparent)")
                        print(f"  If G0=G1=255, alpha is 255 everywhere (opaque)")
                else:
                    print(f"  pixel_start OOB or zero")
        break
    nxt = sec_next(sec)
    if nxt==NULL: break
    sec+=nxt
