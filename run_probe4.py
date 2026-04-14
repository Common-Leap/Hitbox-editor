#!/usr/bin/env python3
import subprocess, sys, os
os.chdir("/home/leap/Workshop/Hitbox editor")
result = subprocess.run([sys.executable, "probe_c1keys.py"], capture_output=True, text=True, timeout=30)
with open("probe_c1_out.txt", "w") as f:
    f.write(result.stdout)
    f.write(result.stderr)
print("done")
