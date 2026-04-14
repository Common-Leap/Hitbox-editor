#!/usr/bin/env python3
"""Fetch Samus forward aerial ACMD to see what effect names are used."""
import urllib.request

url = "https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_samus/samus/AttackAirF.txt"
try:
    with urllib.request.urlopen(url, timeout=10) as r:
        content = r.read().decode('utf-8')
    print(content[:3000])
except Exception as e:
    print(f"Error: {e}")
    # Try alternate URL format
    url2 = "https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_samus/samus/attack_air_f.txt"
    try:
        with urllib.request.urlopen(url2, timeout=10) as r:
            content = r.read().decode('utf-8')
        print(content[:3000])
    except Exception as e2:
        print(f"Error2: {e2}")
