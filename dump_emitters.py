#!/usr/bin/env python3
"""
Dump all emitter data from the Samus effect file to understand what
the explosion effects actually contain.
"""
import struct, sys, glob, os

EFF_PATHS = [
    "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff",
]
raw = None
for p in EFF_PATHS:
    try:
        with open(p, 'rb') as f: raw = f.read(); break
    except FileNotFoundError: pass

if raw is None:
    paths = glob.glob("/home/**/*.eff", recursive=True)
    print("Available .eff files:", paths[:10])
    sys.exit(1)

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def r64(d, off): return struct.unpack_from('<Q', d, off)[0] if off+8<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def rstr(d, off, maxlen=64):
    if off == 0 or off >= len(d): return ''
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

# Find VFXB
vfxb_off = raw.find(b'VFXB')
if vfxb_off < 0:
    print("No VFXB found!")
    sys.exit(1)
data = raw[vfxb_off:]
print(f"VFXB at {vfxb_off:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_size(base): return r32(data, base+4)
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

block_offset = r16(data, 0x16)
vfx_version = r16(data, 0x0A)
print(f"VFX version={vfx_version} block_offset={block_offset:#x}")

# Walk sections to find ESTA (emitter sets)
sec = block_offset
while sec + 4 <= len(data):
    magic = sec_magic(sec)
    if magic == b'ESTA':
        print(f"\nESTA at {sec:#x}")
        esta_child_cnt = sec_child_cnt(sec)
        esta_child_off = sec_child_off(sec)
        eset_base = sec + esta_child_off
        
        for eset_i in range(esta_child_cnt):
            if eset_base + 4 > len(data): break
            if sec_magic(eset_base) != b'ESET': break
            
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = rstr(data, eset_bin + 16)
            eset_child_cnt = sec_child_cnt(eset_base)
            eset_child_off = sec_child_off(eset_base)
            
            # Only show bomb-related effects
            if 'bomb' not in set_name.lower() and 'attack' not in set_name.lower() and 'explosion' not in set_name.lower():
                nxt = sec_next_off(eset_base)
                if nxt == NULL: break
                eset_base = eset_base + nxt
                continue
            
            print(f"\n  ESET[{eset_i}] '{set_name}' ({eset_child_cnt} emitters)")
            
            emtr_base = eset_base + eset_child_off
            for emtr_i in range(eset_child_cnt):
                if emtr_base + 4 > len(data): break
                if sec_magic(emtr_base) != b'EMTR': break
                
                emtr_bin = emtr_base + sec_bin_off(emtr_base)
                emtr_name = rstr(data, emtr_bin + 16)
                emtr_static_off = emtr_bin + 80
                
                # Parse key emitter fields
                base = emtr_static_off
                
                # Flags block (16 bytes)
                off = base + 16
                # NumColor0Keys etc (24 bytes)
                num_color0_keys = r32(data, base + 16)
                num_alpha0_keys = r32(data, base + 20)
                num_color1_keys = r32(data, base + 24)
                num_alpha1_keys = r32(data, base + 28)
                num_scale_keys  = r32(data, base + 32)
                off = base + 40 + 8  # skip LoopRates + Unknown
                if vfx_version > 50: off += 16
                
                # Gravity
                gravity_x = rf32(data, off); off += 4
                gravity_y = rf32(data, off); off += 4
                gravity_z = rf32(data, off); off += 4
                gravity_scale = rf32(data, off); off += 4
                
                # EmitterInfo
                emitter_info_base = off
                emitter_trans_x = rf32(data, off); off += 4
                emitter_trans_y = rf32(data, off); off += 4
                emitter_trans_z = rf32(data, off); off += 4
                emitter_rot_x = rf32(data, off); off += 4
                emitter_rot_y = rf32(data, off); off += 4
                emitter_rot_z = rf32(data, off); off += 4
                emitter_scale_x = rf32(data, off); off += 4
                emitter_scale_y = rf32(data, off); off += 4
                emitter_scale_z = rf32(data, off); off += 4
                emitter_color0_r = rf32(data, off); off += 4
                emitter_color0_g = rf32(data, off); off += 4
                emitter_color0_b = rf32(data, off); off += 4
                emitter_color0_a = rf32(data, off); off += 4
                emitter_color1_r = rf32(data, off); off += 4
                emitter_color1_g = rf32(data, off); off += 4
                emitter_color1_b = rf32(data, off); off += 4
                emitter_color1_a = rf32(data, off); off += 4
                off += 8  # unknown
                
                # EmissionInfo
                emission_base = off
                is_one_time = r8(data, off) != 0
                off += 4
                off += 4  # unknown
                emission_timing = r32(data, off); off += 4
                emission_duration = r32(data, off); off += 4
                emission_rate = rf32(data, off); off += 4
                off += 72 - 20  # rest of EmissionInfo
                
                # EmitterShapeInfo
                emit_type = r8(data, off)
                off += 8 + 48 + 28 + (8 if vfx_version < 40 else 0)
                
                # EmitterRenderState
                mesh_type = r32(data, off)
                primitive_index = r32(data, off + 4)
                blend_type = r8(data, off + 6)
                off += 16
                
                # ParticleData
                infinite_life = r8(data, off) != 0
                particle_life = r32(data, off + 16)
                off += 16 + 8 + 24 + 12 + (20 if vfx_version < 50 else 10)
                
                # Direct reads
                scale_x_direct = rf32(data, base + 0x2E0)
                scale_y_direct = rf32(data, base + 0x2E4)
                particle_life_direct = r32(data, base + 0x2B0)
                emission_rate_direct = rf32(data, base + 0x1C4)
                
                # Sampler info
                sampler_base = base + (2472 if vfx_version >= 37 else 2464 if vfx_version > 21 else 2472)
                tex_ids = []
                for slot in range(3):
                    soff = sampler_base + slot * 32
                    if soff + 8 > len(data): break
                    lo = r32(data, soff)
                    hi = r32(data, soff + 4)
                    tex_id = (hi << 32) | lo
                    if tex_id != 0 and lo != 0xffffffff:
                        tex_ids.append(f"{tex_id:#018x}")
                
                print(f"    EMTR[{emtr_i}] '{emtr_name}':")
                print(f"      is_one_time={is_one_time} timing={emission_timing} duration={emission_duration}")
                print(f"      emission_rate(walk)={emission_rate:.2f} emission_rate_direct={emission_rate_direct:.2f}")
                print(f"      particle_life(walk)={particle_life} particle_life_direct={particle_life_direct}")
                print(f"      scale_x_direct={scale_x_direct:.4f} scale_y_direct={scale_y_direct:.4f}")
                print(f"      mesh_type={mesh_type} primitive_index={primitive_index} blend_type={blend_type}")
                print(f"      emit_type={emit_type} infinite_life={infinite_life}")
                print(f"      color0=({emitter_color0_r:.2f},{emitter_color0_g:.2f},{emitter_color0_b:.2f},{emitter_color0_a:.2f})")
                print(f"      tex_ids={tex_ids}")
                print(f"      emitter_trans=({emitter_trans_x:.3f},{emitter_trans_y:.3f},{emitter_trans_z:.3f})")
                print(f"      emitter_scale=({emitter_scale_x:.3f},{emitter_scale_y:.3f},{emitter_scale_z:.3f})")
                
                nxt = sec_next_off(emtr_base)
                if nxt == NULL: break
                emtr_base = emtr_base + nxt
            
            nxt = sec_next_off(eset_base)
            if nxt == NULL: break
            eset_base = eset_base + nxt
        break
    
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec = sec + nxt
