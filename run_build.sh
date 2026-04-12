#!/bin/bash
cargo build 2>&1 | grep -E "^error" > build_out.txt
echo "exit: $?"
