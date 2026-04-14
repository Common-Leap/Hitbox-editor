#!/usr/bin/env python3
"""
Dump emitter data using the EXACT same sequential walk as the Rust code.
This mirrors parse_vfxb_emitter() in src/effects.rs precisely.
"""
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
print(f"VFXB at {vfxb_off:#x}, version={vfx_version}, block_offset={block_offset:#x}")

NULL = 0xFFFFFFFF
def sec_magic(base): return bytes(data[base:base+4]) if base+4<=len(data) else b'\x00\x00\x00\x00'
def sec_child_off(base): return r32(data, base+8)
def sec_next_off(base): return r32(data, base+0xC)
def sec_bin_off(base): return r32(data, base+0x14)
def sec_child_cnt(base): return r16(data, base+0x1C)

def parse_emitter(base, version):
    """Mirror of parse_vfxb_emitter() in effects.rs"""
    name = rstr(data, base - 64, 64)  # name is at base-64

    num_color0_keys = r32(data, base + 16)
    num_alpha0_keys = r32(data, base + 20)
    num_color1_keys = r32(data, base + 24)
    num_alpha1_keys = r32(data, base + 28)
    num_scale_keys  = r32(data, base + 32)

    # Direct reads (v>22 only)
    scale_x_direct = rf32(data, base + 0x2E0) if version > 22 else 0.0
    scale_y_direct = rf32(data, base + 0x2E4) if version > 22 else 0.0
    particle_life_direct = rf32(data, base + 0x2B0) if version > 22 else 0.0
    emission_rate_direct = rf32(data, base + 0x1C4) if version > 22 else 0.0

    # Sequential walk — mirrors Rust exactly
    off = base
    off += 16  # Flags (4x u32)
    off += 24  # NumColor0Keys..NumParamKeys (6x u32)
    off += 8   # Unknown1, Unknown2
    if version > 50: off += 16
    off += 40  # LoopRates
    off += 8   # Unknown3, Unknown4
    gravity_x = rf32(data, off); off += 4
    gravity_y = rf32(data, off); off += 4
    gravity_z = rf32(data, off); off += 4
    gravity_scale = rf32(data, off); off += 4
    off += 4   # AirRes
    off += 12  # val_0x74..val_0x82
    off += 16  # CenterX/Y, Offset, Padding
    off += 32  # Amplitude, Cycle, PhaseRnd, PhaseInit
    off += 16  # Coefficient0/1, val_0xB8/BC

    tex_pat_count = 5 if version > 40 else 3
    tex_scale_u = rf32(data, off + 0x10)
    tex_scale_v = rf32(data, off + 0x14)
    tex_offset_u = rf32(data, off + 0x18)
    tex_offset_v = rf32(data, off + 0x1C)
    off += tex_pat_count * 144  # TexPatAnim

    scroll_u = rf32(data, off + 0)
    scroll_v = rf32(data, off + 4)
    off += (5 if version > 40 else 3) * 80  # TexScrollAnim

    off += 16        # ColorScale + 3 floats
    off += 128 * 4   # Color0/Alpha0/Color1/Alpha1 tables
    off += 32        # SoftEdge..FarDistAlpha
    off += 16        # Decal + AlphaThreshold + Padding
    off += 16        # AddVelToScale..Padding3
    off += 128       # ScaleAnim
    off += 128       # ParamAnim
    if version > 50: off += 512
    if version > 40: off += 64
    rotation_speed = rf32(data, off + 8)
    off += 64        # RotateInit/Rand/Add/Regist
    off += 16        # ScaleLimitDist
    if version > 40: off += 64

    # EmitterInfo
    off += 16  # IsParticleDraw..padding3
    off += 16  # RandomSeed, DrawPath, AlphaFadeTime, FadeInTime
    emitter_trans_x = rf32(data, off); off += 4
    emitter_trans_y = rf32(data, off); off += 4
    emitter_trans_z = rf32(data, off); off += 4
    off += 12  # TransRand
    emitter_rot_x = rf32(data, off); off += 4
    emitter_rot_y = rf32(data, off); off += 4
    emitter_rot_z = rf32(data, off); off += 4
    off += 12  # RotateRand
    emitter_scale_x = rf32(data, off); off += 4
    emitter_scale_y = rf32(data, off); off += 4
    emitter_scale_z = rf32(data, off); off += 4
    off += 12  # Scale done
    emitter_color0_r = rf32(data, off); off += 4
    emitter_color0_g = rf32(data, off); off += 4
    emitter_color0_b = rf32(data, off); off += 4
    off += 4   # color0 alpha
    emitter_color1_r = rf32(data, off); off += 4
    emitter_color1_g = rf32(data, off); off += 4
    emitter_color1_b = rf32(data, off); off += 4
    off += 4   # color1 alpha
    off += 12  # EmissionRangeNear/Far/Ratio
    off += 16 + (8 if version > 40 else 0) + 8  # EmitterInheritance

    # Emission — timing/duration are f32 in VFXB v22
    emission_base = off
    is_one_time = r8(data, emission_base) != 0
    emission_timing   = rf32(data, emission_base + 8)
    emission_duration = rf32(data, emission_base + 12)
    emission_rate     = rf32(data, emission_base + 16)
    emission_rate_random = rf32(data, emission_base + 20)
    off += 72

    # EmitterShapeInfo
    emit_type = r8(data, off)
    off += 8 + 48 + 28 + (8 if version < 40 else 0)

    # EmitterRenderState
    mesh_type    = r32(data, off)
    primitive_index = r32(data, off + 4)
    blend_type   = r8(data, off + 6)
    display_side = r8(data, off + 7)
    off += 16

    # ParticleData — life fields are f32
    particle_base = off
    infinite_life = r8(data, particle_base) != 0
    particle_life = rf32(data, particle_base + 16)
    particle_life_random = rf32(data, particle_base + 20)
    off += 16 + 8 + 24 + 12 + (20 if version < 50 else 10)

    # EmitterCombiner
    if version < 36: off += 24
    elif version == 36: off += 8
    elif version < 50: off += 24
    else: off += 28

    # ShaderRefInfo
    off += 4 + 20
    if version < 50: off += 16
    if version < 22: off += 8
    off += 8
    if version > 50: off += 8
    off += 32

    # ActionInfo + DepthMode + PassInfo
    off += 4 + (20 if version > 40 else 0)
    if version > 40: off += 16 + 52

    # ParticleVelocityInfo
    vel_base = off
    all_direction_speed = rf32(data, vel_base)
    vel_random = rf32(data, vel_base + 44)
    off += 48
    if version >= 36: off += 16

    # ParticleColor (44 bytes) then ParticleScale
    off += 44
    scale_x = rf32(data, off)
    scale_y = rf32(data, off + 4)
    scale_random = rf32(data, off + 8)

    # Sampler info
    sampler_base = base + (2472 if version >= 37 else 2464 if version > 21 else 2472)
    tex_ids = []
    for slot in range(3):
        soff = sampler_base + slot * 32
        if soff + 8 > len(data): break
        lo = r32(data, soff)
        hi = r32(data, soff + 4)
        tex_id = (hi << 32) | lo
        if tex_id != 0 and lo != 0xffffffff:
            tex_ids.append(f"{tex_id:#018x}")

    # Determine final scale
    if scale_x_direct > 0: raw_scale = scale_x_direct
    elif scale_y_direct > 0: raw_scale = scale_y_direct
    else: raw_scale = max(scale_x, scale_y)

    # Determine final lifetime
    if particle_life_direct > 0: lifetime = particle_life_direct
    elif infinite_life: lifetime = emission_duration
    elif particle_life > 0: lifetime = particle_life
    elif particle_life_random > 0: lifetime = particle_life_random
    elif emission_duration > 0: lifetime = emission_duration
    else: lifetime = 20.0

    return {
        'name': name,
        'is_one_time': is_one_time,
        'emission_timing': emission_timing,
        'emission_duration': emission_duration,
        'emission_rate': emission_rate,
        'emission_rate_direct': emission_rate_direct,
        'particle_life': particle_life,
        'particle_life_direct': particle_life_direct,
        'lifetime': lifetime,
        'scale_x': scale_x, 'scale_y': scale_y,
        'scale_x_direct': scale_x_direct, 'scale_y_direct': scale_y_direct,
        'raw_scale': raw_scale,
        'speed': all_direction_speed,
        'vel_random': vel_random,
        'mesh_type': mesh_type, 'primitive_index': primitive_index,
        'blend_type': blend_type, 'display_side': display_side,
        'emit_type': emit_type, 'infinite_life': infinite_life,
        'color0_r': emitter_color0_r, 'color0_g': emitter_color0_g, 'color0_b': emitter_color0_b,
        'gravity': (gravity_x * gravity_scale, gravity_y * gravity_scale, gravity_z * gravity_scale),
        'tex_ids': tex_ids,
        'emitter_trans': (emitter_trans_x, emitter_trans_y, emitter_trans_z),
        'emitter_rot': (emitter_rot_x, emitter_rot_y, emitter_rot_z),
        'emitter_scale': (emitter_scale_x, emitter_scale_y, emitter_scale_z),
        'rotation_speed': rotation_speed,
        'num_color0_keys': num_color0_keys,
        'num_alpha0_keys': num_alpha0_keys,
    }

# Walk sections
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
            eset_child_cnt = sec_child_cnt(eset_base)

            # Filter to interesting effects
            if any(k in set_name.lower() for k in ['bomb', 'attack', 'screw', 'burner', 'charge']):
                print(f"\nESET '{set_name}' ({eset_child_cnt} emitters)")
                emtr_base = eset_base + sec_child_off(eset_base)
                for ei in range(eset_child_cnt):
                    if emtr_base + 4 > len(data): break
                    if sec_magic(emtr_base) != b'EMTR': break
                    emtr_bin = emtr_base + sec_bin_off(emtr_base)
                    emtr_static = emtr_bin + 80
                    e = parse_emitter(emtr_static, vfx_version)
                    print(f"  [{ei}] '{e['name']}':")
                    print(f"    one_time={e['is_one_time']} timing={e['emission_timing']:.1f} dur={e['emission_duration']:.1f} rate={e['emission_rate']:.2f}")
                    print(f"    life={e['lifetime']:.2f} (walk={e['particle_life']:.2f} direct={e['particle_life_direct']:.2f})")
                    print(f"    scale={e['raw_scale']:.3f} (walk_x={e['scale_x']:.3f} walk_y={e['scale_y']:.3f} direct_x={e['scale_x_direct']:.3f})")
                    print(f"    speed={e['speed']:.3f} blend={e['blend_type']} emit_type={e['emit_type']} mesh={e['mesh_type']}")
                    print(f"    color0=({e['color0_r']:.3f},{e['color0_g']:.3f},{e['color0_b']:.3f}) gravity={e['gravity']}")
                    print(f"    trans={e['emitter_trans']} rot={e['emitter_rot']} scale={e['emitter_scale']}")
                    print(f"    c0keys={e['num_color0_keys']} a0keys={e['num_alpha0_keys']} tex_ids={e['tex_ids']}")
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
