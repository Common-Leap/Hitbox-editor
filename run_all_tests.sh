#!/bin/bash
cargo test 2>&1 | tee all_test_out.txt
echo "EXIT: $?"
