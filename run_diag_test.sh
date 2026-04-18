#!/bin/bash
cargo test test_bc_decode_pipeline_real_data -- --nocapture 2>&1 | tee diag_test_out.txt
echo "EXIT: $?"
