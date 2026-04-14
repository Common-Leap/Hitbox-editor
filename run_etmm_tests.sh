#!/bin/bash
cargo test etmm 2>&1 | tee etmm_test_out.txt
echo "EXIT: $?"
