#!/usr/bin/env python3
"""Trace the sequential walk for the first P_SamusAttackBomb emitter and find the EmitterInfo color."""
import struct

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
with open(EFF_PATH, 'rb') as f: raw = f.read()

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def rstr(d, off, maxlen=64):
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
vfx_version = r16(data, 0x0A)
block_offset = r16(data, 0x16)
print(f"version={vfx_version}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

# Find P_SamusAttackBomb first emitter
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        eset_base = sec + sec_child_off(sec)
        for _ in range(sec_child_cnt(sec)):
            if sec_magic(eset_base) != b'ESET': break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = rstr(data, eset_bin + 16)
            if 'AttackBomb' in set_name:
                emtr_base = eset_base + sec_child_off(eset_base)
                emtr_bin = emtr_base + sec_bin_off(emtr_base)
                base = emtr_bin + 80
                print(f"base = {base:#x}")
                
                version = vfx_version
                
                # Trace the walk
                off = base
                print(f"start: off={off-base:#x}")
                off += 16; print(f"after Flags: off={off-base:#x}")
                off += 24; print(f"after NumKeys: off={off-base:#x}")
                off += 8;  print(f"after Unknown1/2: off={off-base:#x}")
                if version > 50: off += 16
                off += 40; print(f"after LoopRates: off={off-base:#x}")
                off += 8;  print(f"after Unknown3/4: off={off-base:#x}")
                off += 4*4; print(f"after gravity+scale: off={off-base:#x}")
                off += 4;  print(f"after AirRes: off={off-base:#x}")
                off += 12; print(f"after val_0x74: off={off-base:#x}")
                off += 16; print(f"after CenterXY: off={off-base:#x}")
                off += 32; print(f"after Amplitude: off={off-base:#x}")
                off += 16; print(f"after Coeff: off={off-base:#x}")
                
                tex_pat_count = 5 if version > 40 else 3
                print(f"TexPatAnim: tex_pat_count={tex_pat_count}, stride={tex_pat_count*144}")
                off += tex_pat_count * 144; print(f"after TexPatAnim: off={off-base:#x}")
                
                scroll_count = 5 if version > 40 else 3
                print(f"TexScrollAnim: count={scroll_count}, stride={scroll_count*80}")
                off += scroll_count * 80; print(f"after TexScrollAnim: off={off-base:#x}")
                
                off += 16;    print(f"after ColorScale: off={off-base:#x}")
                off += 128*4; print(f"after Color/Alpha tables: off={off-base:#x}")
                off += 32;    print(f"after SoftEdge: off={off-base:#x}")
                off += 16;    print(f"after Decal: off={off-base:#x}")
                off += 16;    print(f"after AddVelToScale: off={off-base:#x}")
                off += 128;   print(f"after ScaleAnim: off={off-base:#x}")
                off += 128;   print(f"after ParamAnim: off={off-base:#x}")
                if version > 50: off += 512
                if version > 40: off += 64
                print(f"rotation_speed at: off+8={off+8-base:#x}, val={rf32(data, off+8):.4f}")
                off += 64;    print(f"after RotateInit: off={off-base:#x}")
                off += 16;    print(f"after ScaleLimitDist: off={off-base:#x}")
                if version > 40: off += 64
                
                # EmitterInfo
                print(f"\n=== EmitterInfo starts at off={off-base:#x} ===")
                off += 16; print(f"after IsParticleDraw: off={off-base:#x}")
                off += 16; print(f"after RandomSeed: off={off-base:#x}")
                
                trans_x = rf32(data, off)
                trans_y = rf32(data, off+4)
                trans_z = rf32(data, off+8)
                print(f"Trans at off={off-base:#x}: ({trans_x:.3f}, {trans_y:.3f}, {trans_z:.3f})")
                off += 12
                off += 12; print(f"after TransRand: off={off-base:#x}")
                
                rot_x = rf32(data, off)
                rot_y = rf32(data, off+4)
                rot_z = rf32(data, off+8)
                print(f"Rotate at off={off-base:#x}: ({rot_x:.3f}, {rot_y:.3f}, {rot_z:.3f})")
                off += 12
                off += 12; print(f"after RotateRand: off={off-base:#x}")
                
                sc_x = rf32(data, off)
                sc_y = rf32(data, off+4)
                sc_z = rf32(data, off+8)
                print(f"Scale at off={off-base:#x}: ({sc_x:.3f}, {sc_y:.3f}, {sc_z:.3f})")
                off += 12
                
                c0r = rf32(data, off)
                c0g = rf32(data, off+4)
                c0b = rf32(data, off+8)
                c0a = rf32(data, off+12)
                print(f"Color0 RGBA at off={off-base:#x}: ({c0r:.4f}, {c0g:.4f}, {c0b:.4f}, {c0a:.4f})")
                print(f"  raw bytes: {data[off:off+16].hex()}")
                
                c1r = rf32(data, off+16)
                c1g = rf32(data, off+20)
                c1b = rf32(data, off+24)
                print(f"Color1 RGB at off+16={off+16-base:#x}: ({c1r:.4f}, {c1g:.4f}, {c1b:.4f})")
                
                off += 32
                print(f"after Color0/1: off={off-base:#x}")
                off += 12; print(f"after EmissionRange: off={off-base:#x}")
                
                inherit_size = 16 + (8 if version > 40 else 0) + 8
                print(f"EmitterInheritance size={inherit_size}")
                off += inherit_size; print(f"after EmitterInheritance: off={off-base:#x}")
                
                # Emission
                print(f"\n=== Emission at off={off-base:#x} ===")
                is_one_time = r8(data, off) != 0
                timing = r32(data, off+8)
                duration = r32(data, off+12)
                rate = rf32(data, off+16)
                print(f"is_one_time={is_one_time} timing={timing} duration={duration} rate={rate:.3f}")
                off += 72
                
                # EmitterShapeInfo
                print(f"\n=== EmitterShapeInfo at off={off-base:#x} ===")
                emit_type = r8(data, off)
                print(f"emit_type={emit_type}")
                shape_size = 8 + 48 + 28 + (8 if version < 40 else 0)
                print(f"shape_size={shape_size}")
                off += shape_size
                
                # EmitterRenderState
                print(f"\n=== EmitterRenderState at off={off-base:#x} ===")
                mesh_type = r32(data, off)
                blend_type = r8(data, off+6)
                display_side = r8(data, off+7)
                print(f"mesh_type={mesh_type} blend_type={blend_type} display_side={display_side}")
                off += 16
                
                # ParticleData
                print(f"\n=== ParticleData at off={off-base:#x} ===")
                infinite_life = r8(data, off) != 0
                p_life = r32(data, off+16)
                p_life_r = r32(data, off+20)
                print(f"infinite_life={infinite_life} particle_life={p_life} particle_life_random={p_life_r}")
                pd_size = 16 + 8 + 24 + 12 + (20 if version < 50 else 10)
                print(f"ParticleData size={pd_size}")
                off += pd_size
                
                # EmitterCombiner
                if version < 36: ec_size = 24
                elif version == 36: ec_size = 8
                elif version < 50: ec_size = 24
                else: ec_size = 28
                print(f"\nEmitterCombiner size={ec_size} at off={off-base:#x}")
                off += ec_size
                
                # ShaderRefInfo
                sri_size = 4 + 20 + (16 if version < 50 else 0) + (8 if version < 22 else 0) + 8 + (8 if version > 50 else 0) + 32
                print(f"ShaderRefInfo size={sri_size} at off={off-base:#x}")
                off += sri_size
                
                # ActionInfo + DepthMode + PassInfo
                ai_size = 4 + (20 if version > 40 else 0)
                print(f"ActionInfo size={ai_size} at off={off-base:#x}")
                off += ai_size
                if version > 40: off += 16 + 52
                
                # ParticleVelocityInfo
                print(f"\n=== ParticleVelocityInfo at off={off-base:#x} ===")
                speed = rf32(data, off)
                vel_random = rf32(data, off+44)
                print(f"speed={speed:.4f} vel_random={vel_random:.4f}")
                off += 48
                if version >= 36: off += 16
                
                # ParticleColor (44 bytes) then ParticleScale
                print(f"\n=== ParticleColor at off={off-base:#x} ===")
                # Show 44 bytes of ParticleColor
                for i in range(0, 44, 4):
                    v = rf32(data, off+i)
                    print(f"  [{i:#x}] = {v:.4f}  ({r32(data,off+i):#010x})")
                off += 44
                
                print(f"\n=== ParticleScale at off={off-base:#x} ===")
                sx = rf32(data, off)
                sy = rf32(data, off+4)
                sr = rf32(data, off+8)
                print(f"scale_x={sx:.4f} scale_y={sy:.4f} scale_random={sr:.4f}")
                
                print(f"\n=== Sampler base = base+{2464 if version > 21 else 2472:#x} = {base+2464:#x} ===")
                print(f"Current off = {off-base:#x} (relative to base)")
                print(f"Sampler base relative = {2464:#x}")
                print(f"Difference = {2464 - (off-base):#x} bytes")
                
                break
            nxt = sec_next_off(eset_base)
            if nxt == NULL: break
            eset_base += nxt
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec += nxt
