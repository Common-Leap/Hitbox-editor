#!/bin/bash
cargo build 2>&1 | tee /tmp/hitbox_build.txt
echo "Exit: $?"
