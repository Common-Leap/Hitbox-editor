#!/usr/bin/env python3
"""
Quick diagnostic to see if we can identify texture loading issues.
Compiles and runs a single test to check PTCL parsing.
"""
import subprocess
import sys

# Run the VFXB parsing test
result = subprocess.run(
    ["cargo", "test", "--release", "test_mario_effect", "--", "--nocapture"],
    cwd="/home/leap/Workshop/Hitbox editor",
    capture_output=True,
    text=True,
    timeout=60
)

print("STDOUT:")
print(result.stdout)
print("\nSTDERR:")
print(result.stderr)

# Filter for texture diagnostics
lines = (result.stdout + result.stderr).split('\n')
tex_lines = [l for l in lines if any(x in l for x in ['[TEX]', '[EMTR]', '[GRTF]', '[GTNT]', '[CADP]', 'bntx_textures', 'texture_index'])]

print(f"\n=== TEXTURE DIAGNOSTICS ({len(tex_lines)} lines) ===")
for line in tex_lines[:50]:  # First 50
    print(line)

sys.exit(result.returncode)
