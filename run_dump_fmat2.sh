#!/bin/bash
python3 dump_fmat2.py 2>&1 | tee dump_fmat2_out.txt
echo "EXIT: $?"
