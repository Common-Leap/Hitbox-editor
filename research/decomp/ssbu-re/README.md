# SSBU reverse-engineering tools

This directory contains reusable scripts for inspecting SSBU executable data with
Ghidra.

Set `SSBU_DUMP_DIR` to the directory containing the input files:

```bash
export SSBU_DUMP_DIR=/path/to/external/ssbu-dumps
```

Depending on the script, the expected files are `exefs/main`,
`main_decompressed.bin`, and `main_reloc.bin`. Run `nso_image.py` to generate
`main_decompressed.bin` in the selected input directory.

Use `ghidra_proj/` for the local Ghidra project. Analysis state and generated
output remain local to this working directory.

Run the scripts in `gscripts/` with this directory selected as Ghidra's working
directory. Generated text output is written relative to this directory.
