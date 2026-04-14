#!/usr/bin/env python3
"""Run dump_emitters_v2 and write output to a file in the workspace."""
import subprocess, sys, os

os.chdir("/home/leap/Workshop/Hitbox editor")
result = subprocess.run(
    [sys.executable, "dump_emitters_v2.py"],
    capture_output=True, text=True, timeout=30
)
with open("probe_out.txt", "w") as f:
    f.write("STDOUT:\n")
    f.write(result.stdout)
    f.write("\nSTDERR:\n")
    f.write(result.stderr)
    f.write(f"\nEXIT: {result.returncode}\n")
print("done")
