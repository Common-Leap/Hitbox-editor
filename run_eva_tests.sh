#!/bin/bash
cd "/home/leap/Workshop/Hitbox editor"
cargo test test_eva 2>&1 > eva_test_out.txt
echo "Exit code: $?" >> eva_test_out.txt
