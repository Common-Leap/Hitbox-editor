#!/usr/bin/env python3
"""Dump the forward aerial explosion effect emitters to diagnose mesh_type and primitive issues."""
import struct, sys

EFF_PATH = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff"
try:
    with open(EFF_PATH, 'rb') as f: raw = f.read()
except FileNotFoundError:
    print(f"File not found: {EFF_PATH}")
    sys.exit(1)

def r8(d, off): return d[off] if off < len(d) else 0
def r16(d, off): return struct.unpack_from('<H', d, off)[0] if off+2<=len(d) else 0
def r32(d, off): return struct.unpack_from('<I', d, off)[0] if off+4<=len(d) else 0
def rf32(d, off): return struct.unpack_from('<f', d, off)[0] if off+4<=len(d) else 0.0
def rstr(d, off, maxlen=64):
    if off == 0 or off >= len(d): return ''
    end = d.find(b'\x00', off, off+maxlen)
    if end < 0: end = off+maxlen
    return d[off:end].decode('utf-8', errors='replace')

vfxb_off = raw.find(b'VFXB')
data = raw[vfxb_off:]
vfx_version = r16(data, 0x0A)
block_offset = r16(data, 0x16)
print(f"VFXB version={vfx_version} block_offset={block_offset:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

# Walk ESTA to find explosion-related emitters — show speed/scale/lifetime
sec = block_offset
while sec + 4 <= len(data):
    if sec_magic(sec) == b'ESTA':
        esta_child_cnt = sec_child_cnt(sec)
        eset_base = sec + sec_child_off(sec)
        for _ in range(esta_child_cnt):
            if eset_base + 4 > len(data): break
            if sec_magic(eset_base) != b'ESET': break
            eset_bin = eset_base + sec_bin_off(eset_base)
            set_name = rstr(data, eset_bin + 16)

            if any(k in set_name.lower() for k in ['airf', 'attackbomb', 'chargeshot']):
                eset_child_cnt = sec_child_cnt(eset_base)
                print(f"\nESET '{set_name}' ({eset_child_cnt} emitters)")
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(eset_child_cnt):
                    if emtr_base + 4 > len(data): break
                    if sec_magic(emtr_base) != b'EMTR': break
                    emtr_bin = emtr_base + sec_bin_off(emtr_base)
                    emtr_name = rstr(data, emtr_bin + 16)
                    base = emtr_bin + 80

                    # Sequential walk to ParticleVelocityInfo
                    off = base
                    off += 16  # Flags
                    off += 24  # NumKeys
                    off += 8   # Unknown
                    if vfx_version > 50: off += 16
                    off += 40  # LoopRates
                    off += 8   # Unknown
                    off += 16  # gravity
                    off += 4   # AirRes
                    off += 12  # val_0x74
                    off += 16  # CenterXY
                    off += 32  # Amplitude
                    off += 16  # Coefficient

                    tex_pat_count = 5 if vfx_version > 40 else 3
                    off += tex_pat_count * 144
                    off += (5 if vfx_version > 40 else 3) * 80
                    off += 16        # ColorScale
                    off += 128 * 4   # Color tables
                    off += 32        # SoftEdge
                    off += 16        # Decal
                    off += 16        # AddVelToScale
                    off += 128       # ScaleAnim
                    off += 128       # ParamAnim
                    if vfx_version > 50: off += 512
                    if vfx_version > 40: off += 64
                    off += 64        # Rotate
                    off += 16        # ScaleLimitDist
                    if vfx_version > 40: off += 64

                    # EmitterInfo
                    off += 16  # IsParticleDraw
                    off += 16  # RandomSeed
                    emitter_trans_x = rf32(data, off); off += 4
                    emitter_trans_y = rf32(data, off); off += 4
                    emitter_trans_z = rf32(data, off); off += 4
                    off += 12  # TransRand
                    off += 12  # Rotate
                    off += 12  # RotateRand
                    emitter_scale_x = rf32(data, off); off += 4
                    emitter_scale_y = rf32(data, off); off += 4
                    emitter_scale_z = rf32(data, off); off += 4
                    off += 32  # Color0+Color1
                    off += 12  # EmissionRange
                    off += 16 + (8 if vfx_version > 40 else 0) + 8

                    # Emission
                    is_one_time = r8(data, off) != 0
                    emission_timing = r32(data, off + 8)
                    emission_duration = r32(data, off + 12)
                    emission_rate = rf32(data, off + 16)
                    off += 72

                    # EmitterShapeInfo
                    emit_type = r8(data, off)
                    off += 8 + 48 + 28 + (8 if vfx_version < 40 else 0)

                    # EmitterRenderState
                    blend_type = r8(data, off + 6)
                    off += 16

                    # ParticleData
                    particle_life = r32(data, off + 16)
                    off += 16 + 8 + 24 + 12 + (20 if vfx_version < 50 else 10)

                    # EmitterCombiner
                    if vfx_version < 36: off += 24
                    elif vfx_version == 36: off += 8
                    elif vfx_version < 50: off += 24
                    else: off += 28

                    # ShaderRefInfo
                    off += 4 + 20
                    if vfx_version < 50: off += 16
                    if vfx_version < 22: off += 8
                    off += 8
                    if vfx_version > 50: off += 8
                    off += 32

                    # ActionInfo
                    off += 4 + (20 if vfx_version > 40 else 0)
                    if vfx_version > 40: off += 16 + 52

                    # ParticleVelocityInfo
                    all_direction_speed = rf32(data, off)
                    vel_random = rf32(data, off + 44)
                    off += 48
                    if vfx_version >= 36: off += 16

                    # ParticleColor (44 bytes) then ParticleScale
                    off += 44
                    scale_x = rf32(data, off)
                    scale_y = rf32(data, off + 4)
                    scale_random = rf32(data, off + 8)

                    # Direct reads
                    scale_x_direct = rf32(data, base + 0x2E0)
                    scale_y_direct = rf32(data, base + 0x2E4)
                    particle_life_direct = r32(data, base + 0x2B0)

                    final_scale = scale_x_direct if scale_x_direct > 0 else (scale_y_direct if scale_y_direct > 0 else max(scale_x, scale_y))
                    final_life = particle_life_direct if particle_life_direct > 0 else particle_life

                    print(f"  [{ei}] '{emtr_name}': one_time={is_one_time} timing={emission_timing} dur={emission_duration} rate={emission_rate:.1f}")
                    print(f"       emit_type={emit_type} blend={blend_type} speed={all_direction_speed:.3f} vel_random={vel_random:.3f}")
                    print(f"       scale={final_scale:.3f}(direct_x={scale_x_direct:.3f},walk_x={scale_x:.3f}) life={final_life}")
                    print(f"       emitter_trans=({emitter_trans_x:.2f},{emitter_trans_y:.2f},{emitter_trans_z:.2f}) emitter_scale=({emitter_scale_x:.2f},{emitter_scale_y:.2f},{emitter_scale_z:.2f})")

                    nxt = sec_next_off(emtr_base)
                    if nxt == NULL: break
                    emtr_base += nxt

            nxt = sec_next_off(eset_base)
            if nxt == NULL: break
            eset_base += nxt
        break
    nxt = sec_next_off(sec)
    if nxt == NULL: break
    sec += nxt
