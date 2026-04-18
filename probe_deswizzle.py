#!/usr/bin/env python3
"""Check what the deswizzled BC5 data looks like for ef_cmn_grade00."""
import struct, subprocess, sys, os

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]

bntx_base = next((i for i in range(len(data)-4) if data[i:i+4] == b'BNTX'), None)
nx = bntx_base + 0x20
data_blk_abs = nx + 0x10 + r32(data, nx + 0x10)
scan_end = data_blk_abs

brti_offsets = []
pos = bntx_base
while pos + 4 <= scan_end:
    if data[pos:pos+4] == b'BRTI':
        brti_offsets.append(pos)
        brti_len = r32(data, pos + 4)
        pos += max(brti_len, 0x90)
    else:
        pos += 8

str_names = []
str_pos = bntx_base
while str_pos + 4 <= len(data):
    if data[str_pos:str_pos+4] == b'_STR':
        str_count = r32(data, str_pos + 16)
        soff = str_pos + 20
        for _ in range(min(str_count, 512)):
            if soff + 2 > len(data): break
            slen = r16(data, soff); soff += 2
            if soff + slen > len(data): break
            s = data[soff:soff+slen].decode('utf-8', errors='replace')
            soff += slen + 1
            if soff % 2 != 0: soff += 1
            if s: str_names.append(s)
        break
    str_pos += 1

# Find ef_cmn_grade00 (index 11)
idx = str_names.index('ef_cmn_grade00')
brti = brti_offsets[idx]

w = r32(data, brti + 0x24)
h = r32(data, brti + 0x28)
data_size = r32(data, brti + 0x50)
block_height_log2 = r32(data, brti + 0x34)
tile_mode = r16(data, brti + 0x12)

pts_addr_lo = r32(data, brti + 0x70)
pts_addr_hi = r32(data, brti + 0x74)
pts_addr = (pts_addr_hi << 32 | pts_addr_lo)
pts_addr_abs = bntx_base + pts_addr
mip0_lo = r32(data, pts_addr_abs)
mip0_hi = r32(data, pts_addr_abs + 4)
mip0_rel = (mip0_hi << 32 | mip0_lo)
pixel_start = bntx_base + mip0_rel

mip0_size = ((w+3)//4) * 16 * ((h+3)//4)
raw_data = data[pixel_start:pixel_start+mip0_size]

print(f"ef_cmn_grade00: {w}x{h} tile_mode={tile_mode} block_height_log2={block_height_log2}")
print(f"mip0_size={mip0_size} raw_data_len={len(raw_data)}")
print(f"Raw first 32 bytes: {raw_data[:32].hex()}")

# Write a Rust test to deswizzle and check the output
test_code = f'''
#[test]
fn test_deswizzle_grade00() {{
    let eff_path = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff";
    let raw = match std::fs::read(eff_path) {{
        Ok(d) => d,
        Err(_) => {{ eprintln!("[SKIP] ef_samus.eff not found"); return; }}
    }};
    let vfxb_off = raw.windows(4).position(|w| w == b"VFXB").expect("no VFXB");
    let data = &raw[vfxb_off..];
    let ptcl = crate::effects::PtclFile::parse(data).expect("parse failed");
    
    // Find ef_cmn_grade00 (index 11)
    let tex = &ptcl.bntx_textures[11];
    eprintln!("[GRADE00] {{}}x{{}} fmt={{:#06x}} data_offset={{}} data_size={{}}", 
        tex.width, tex.height, tex.ftx_format, tex.ftx_data_offset, tex.ftx_data_size);
    
    let off = tex.ftx_data_offset as usize;
    let sz = tex.ftx_data_size as usize;
    if off + sz > ptcl.texture_section.len() {{
        eprintln!("[GRADE00] OOB: off={{}} sz={{}} section={{}}", off, sz, ptcl.texture_section.len());
        return;
    }}
    let raw_bc5 = &ptcl.texture_section[off..off+sz];
    eprintln!("[GRADE00] first 32 bytes of deswizzled: {{:?}}", &raw_bc5[..32.min(raw_bc5.len())]);
    
    // Check first BC5 block
    if raw_bc5.len() >= 16 {{
        let r0 = raw_bc5[0];
        let r1 = raw_bc5[1];
        let g0 = raw_bc5[8];
        let g1 = raw_bc5[9];
        eprintln!("[GRADE00] first block: R0={{}} R1={{}} G0={{}} G1={{}}", r0, r1, g0, g1);
    }}
    
    // Decode BC5 using image_dds
    let surface = image_dds::Surface {{
        width: tex.width as u32,
        height: tex.height as u32,
        depth: 1, layers: 1, mipmaps: 1,
        image_format: image_dds::ImageFormat::BC5RgUnorm,
        data: raw_bc5[..((tex.width as usize+3)/4*16*(tex.height as usize+3)/4)].to_vec(),
    }};
    match surface.decode_rgba8() {{
        Ok(s) => {{
            eprintln!("[GRADE00] decoded rgba8 len={{}}", s.data.len());
            eprintln!("[GRADE00] first pixel: R={{}} G={{}} B={{}} A={{}}", s.data[0], s.data[1], s.data[2], s.data[3]);
            eprintln!("[GRADE00] pixel at center: R={{}} G={{}} B={{}} A={{}}", 
                s.data[(128*128+128)*4], s.data[(128*128+128)*4+1], 
                s.data[(128*128+128)*4+2], s.data[(128*128+128)*4+3]);
        }}
        Err(e) => eprintln!("[GRADE00] decode error: {{e}}"),
    }}
}}
'''

# Check if test already exists
with open("src/effects.rs", "r") as f:
    content = f.read()

if "test_deswizzle_grade00" not in content:
    insert_pos = content.rfind("fn test_print_samus_handles()")
    if insert_pos > 0:
        new_content = content[:insert_pos] + test_code.strip() + "\n\n    " + content[insert_pos:]
        with open("src/effects.rs", "w") as f:
            f.write(new_content)
        print("Added test")
    else:
        print("Could not find insertion point")
else:
    print("Test already exists")

os.chdir("/home/leap/Workshop/Hitbox editor")
result = subprocess.run(
    ["cargo", "test", "test_deswizzle_grade00", "--", "--nocapture"],
    capture_output=True, text=True, timeout=120
)
with open("probe_deswizzle_out.txt", "w") as f:
    f.write(result.stdout[-8000:] if len(result.stdout) > 8000 else result.stdout)
    f.write(result.stderr[-4000:] if len(result.stderr) > 4000 else result.stderr)
print("done")
