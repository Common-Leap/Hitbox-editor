import struct
EFF_PATH = '/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff'
with open(EFF_PATH, 'rb') as f: raw = f.read()
vfxb_pos = raw.find(b'VFXB')
data = raw[vfxb_pos:]
def r32(off): return struct.unpack_from('<I', data, off)[0] if off+4<=len(data) else 0
def r16(off): return struct.unpack_from('<H', data, off)[0] if off+2<=len(data) else 0
def rf32(off): return struct.unpack_from('<f', data, off)[0] if off+4<=len(data) else 0.0

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(base+4)
def sec_next_off(base): return r32(base+0xC)
def sec_bin_off(base): return r32(base+0x14)
def sec_child_off(base): return r32(base+8)

block_offset = r16(0x16)
sec = block_offset
lines = []
while sec + 4 <= len(data):
    if sec_magic(sec) == b'GRTF':
        bin_off_rel = sec_bin_off(sec)
        bin_start = sec + bin_off_rel
        bin_len = sec_size(sec) - bin_off_rel
        bntx_pos = None
        for i in range(bin_start, min(bin_start + bin_len, len(data) - 4)):
            if data[i:i+4] == b'BNTX':
                bntx_pos = i
                break
        if bntx_pos is None:
            lines.append('No BNTX found'); break
        
        nx = bntx_pos + 0x20
        data_blk_rel = r32(nx + 0x10)
        data_blk_abs = nx + 0x10 + data_blk_rel
        brtd_data_start = data_blk_abs + 0x10
        
        # Find BRTI structs
        scan_end = data_blk_abs
        brti_offsets = []
        pos = bntx_pos
        while pos + 4 <= scan_end:
            if data[pos:pos+4] == b'BRTI':
                brti_offsets.append(pos)
                brti_len = r32(pos + 4)
                pos += max(brti_len, 0x90)
            else:
                pos += 8
        
        # Find _STR names
        str_names = []
        str_pos = bntx_pos
        while str_pos + 4 <= len(data):
            if data[str_pos:str_pos+4] == b'_STR':
                str_count = r32(str_pos + 16)
                soff = str_pos + 20
                for _ in range(min(str_count, 300)):
                    if soff + 2 > len(data): break
                    slen = r16(soff); soff += 2
                    if soff + slen > len(data): break
                    s = data[soff:soff+slen].decode('utf-8', errors='replace')
                    soff += slen + 1
                    if soff % 2 != 0: soff += 1
                    if s: str_names.append(s)
                break
            str_pos += 1
        
        lines.append(f'bntx_base={bntx_pos:#x} brtd_data_start={brtd_data_start:#x}')
        
        # Check textures 8 and 19 with the FIXED mip0_ptr calculation
        brtd_cursor = 0
        for idx in range(min(len(brti_offsets), 25)):
            brti = brti_offsets[idx]
            name = str_names[idx] if idx < len(str_names) else f'tex_{idx}'
            tile_mode = r16(brti + 0x12)
            fmt_raw = r32(brti + 0x1C)
            width = r32(brti + 0x24)
            height = r32(brti + 0x28)
            data_size = r32(brti + 0x50)
            
            # FIXED: pts_addr is relative to bntx_base
            pts_addr_lo = r32(brti + 0x70)
            pts_addr_hi = r32(brti + 0x74)
            pts_addr = (pts_addr_hi << 32) | pts_addr_lo
            pts_addr_abs = bntx_pos + pts_addr
            
            mip0_ptr = 0
            if pts_addr > 0 and pts_addr_abs + 8 <= len(data):
                mip0_lo = r32(pts_addr_abs)
                mip0_hi = r32(pts_addr_abs + 4)
                mip0_rel = (mip0_hi << 32) | mip0_lo
                mip0_ptr = bntx_pos + mip0_rel
            
            if mip0_ptr > 0 and mip0_ptr < len(data):
                pixel_start = mip0_ptr
            else:
                pixel_start = brtd_data_start + brtd_cursor
            
            pixel_end = pixel_start + data_size
            brtd_cursor = (brtd_cursor + data_size + 0x1FF) & ~0x1FF
            
            if idx in [8, 19] or (width > 0 and height > 0 and idx < 5):
                lines.append(f'BRTI[{idx}] "{name}": {width}x{height} tile={tile_mode} pts_addr={pts_addr:#x} pts_addr_abs={pts_addr_abs:#x} mip0_ptr={mip0_ptr:#x} pixel_start={pixel_start:#x}')
                if pixel_end <= len(data) and data_size > 0:
                    lines.append(f'  First 16 bytes: {data[pixel_start:pixel_start+16].hex()}')
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec = sec + nxt

with open('prma_out.txt', 'w') as f:
    f.write('\n'.join(lines) + '\n')
