#!/bin/bash
python3 dump_fmat.py 2>&1 | tee dump_fmat_out.txt
echo "EXIT: $?"
