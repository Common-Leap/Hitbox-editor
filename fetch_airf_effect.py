#!/usr/bin/env python3
"""Fetch Samus forward aerial effect_ ACMD script."""
import urllib.request

# Try effect_ prefix
for name in ["effect_attackairf", "effect_attack_air_f", "AttackAirF"]:
    url = f"https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_samus/samus/{name}.txt"
    try:
        with urllib.request.urlopen(url, timeout=10) as r:
            content = r.read().decode('utf-8')
        print(f"=== Found: {name} ===")
        # Show only lines with EFFECT
        for line in content.split('\n'):
            if 'EFFECT' in line or 'effect' in line.lower():
                print(line)
        print()
    except Exception as e:
        print(f"Not found: {name} ({e})")
