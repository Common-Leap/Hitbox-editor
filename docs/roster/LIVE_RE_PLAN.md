# Live CSS Reload — Reverse-Engineering Plan (Reboot Loader → Live)

Reboot works: `export_roster` → `sd/ultimate/mods/visionary_roster_live/ui/param/database/ui_chara_db.prc` → ARCropolis serves it at next boot and the game parses `db_root` into heap. Live while the game is running does not: the same file on SD is ignored until reboot, and the heap `db_root` stays vanilla. This plan reverse-engineers the reboot path so we can re-invoke it live.

## 0. Terminology

* **Raw file** — `ui/param/database/ui_chara_db.prc` on SD, 121 `db_root` entries, 75 fields each, `disp_order` `I8` + `can_select` `Bool`.
* **Resident raw** — `LoadedData[data_index].data` for that hash, `decomp_size` bytes, what `replace_loaded_file` patches.
* **Parsed heap** — `db_root` array of structs in the UI heap, one per entry, `disp_order` at offset `O`, stride `S`, `can_select` nearby. This is what the CSS grid actually draws.

Reboot reloads both: raw via ARCropolis, parsed via the loader that runs on CSS enter. Live must do the same without leaving the CSS.

## 1. What reboot does (to be measured)

1. **File request** — CSS enter calls a loader `L(path_hash)` that does `arc::get_file_info_from_hash(hash40("ui/param/database/ui_chara_db.prc"))` → `LoadedData` → `prc::read_stream` → heap alloc for `db_root`.
2. **Parse** — `prc::disassemble` builds the `ParamStruct`, extracts `db_root` list, allocates `N * S` bytes for the parsed array, copies `disp_order`/`can_select`/`fighter_kind`/`name_id`/`ui_chara_id`/`color_num` into each struct.
3. **Sort + build** — `BuildCssGrid(parsed_array, N)` sorts by `disp_order` (stable, shared `80` kept adjacent, `-1`/`99` skipped) and builds `Pane`/`Layout` objects.

Live must re-run 1–3 on demand.

## 2. Artifacts to produce (all in `research/decomp/ssbu-re/`)

| Artifact | How | Verifies |
|---|---|---|
| `ui_chara_db_decomp.txt` | `DumpUiCharaDb.java` — XREFs `ui/param/database/ui_chara_db.prc`, `db_root`, `disp_order`, `can_select` | Loader `L` address, its caller, heap alloc site, pointer chain from `FilesystemInfo@0x5331f20` or `UI singleton` |
| `css_rebuild_decomp.txt` | `DumpCssRebuild.java` — XREFs `disp_order`/`db_root`/`SelectScene`/`Pane`/`Layout` | `BuildCssGrid` address, its `parsed_array` arg, `N`, thread (UI vs load) |
| `live_offsets.json` | Manual paste from the two dumps, checked into `sd:/visionary_heap_config.json` | `{"disp_offset":0x.., "stride":0x.., "array_ptr":"0x..", "rebuild":"0x.."}` |
| `LIVE_VALIDATION.md` table update | `live_harness.py` pre/post `peek` + `probe_roster` | Back-out vs instant matrix, with `rebuild` present |

Both Ghidra scripts already exist in `gscripts/` and write those txt files.

## 3. Step-by-step RE

### 3.1 Get a dump

```bash
export SSBU_DUMP_DIR=/path/to/external/ssbu-dumps  # contains exefs/main, main_decompressed.bin
# In Ghidra: open ghidra_proj/ssbu_main, import main_decompressed.bin at imageBase 0x7100000000
```

### 3.2 Find the loader `L`

* Search Text for `ui/param/database/ui_chara_db.prc` bytes, `FindStrRefs`.
* For each XREF, decompile the containing function: it will call `prc::open`/`read_stream` or `arc::get_file_info` + `LoadedData` + `disassemble`.
* Record its entry, its `hash`/`out_ptr`/`out_len` signature, and the heap alloc that holds the parsed array. The parsed array pointer is typically stored in a UI singleton — note its address and how it’s reached from `FilesystemInfo` or a static.

### 3.3 Measure the parsed struct

* In the decompiled `L`, note the `malloc(N*S)` size and the field writes: `*(array + i*S + O) = disp_order`. `O` is `disp_offset`, `S` is `stride`.
* Confirm `can_select` offset (`O±8`) and that hidden writes `disp=-1` + `can_select=0`.
* Cross-check by dumping the raw file’s `disp_order` sequence (from `CharaDb::open` in `src/roster/css.rs`) and comparing to a `peek` of the heap array after `L` returns.

### 3.4 Find the rebuild `BuildCssGrid`

* Search for `disp_order`/`can_select` field hashes being read in a sort comparator, or for `Pane`/`Layout` strings near a loop over `db_root`.
* `BuildCssGrid` takes `(parsed_array, N)` and rebuilds the grid. Note whether it must run on the UI thread and whether it frees the old panes.

### 3.5 Wire the live path

* At `install()` register a disk callback for the live file (already removed for heap-only, but keep as fallback for back-out if instant is not yet wired) — the callback reads `sd:/ultimate/mods/visionary_roster_live/...` and serves it, so `L` can re-read without an ARCropolis rescan.
* `roster_heap::patch_parsed_heap` — instead of the current string/hash heuristic, use `disp_offset`/`stride`/`array_ptr` from `live_offsets.json` to patch `*(array + i*S + O)` directly for the keys in `LIVE_ORDER`/`LIVE_HIDDEN`. Validate `disp` ∈ `{-1,0..127}`∖`{99}` and `can_select` parity.
* `trigger_rebuild` — `transmute(rebuild_addr)` and call on the UI thread after the patch. If `array_ptr` is known, patch there; otherwise scan as before.

### 3.6 Validate in Eden (no reboot)

1. Boot, enter CSS, `probe_roster` pre.
2. Drag via Visionary (or `Force Live Update`), `live_harness.py --move mario:5 --hide ridley` sends `live_css_order`.
3. `peek` the parsed array post-patch, `probe_roster` post, and screenshot the CSS. Assert `visible()` `index.rs:252` order matches the screen.

## 4. Deliverables for “live while running”

* `src/roster/roster_heap.rs` — heap-only `patch_parsed_heap` using `disp_offset`/`stride`/`array_ptr` from config, plus `trigger_rebuild` via `rebuild` addr. No file fallback.
* `plugins/slight_replica/src/slight/roster_heap.rs` — same, plus `ensure_live_callback` removed (heap-only) and `is_readable` via `svcQueryMemory` for safe scanning.
* `docs/roster/PLAN.md` — one paragraph per measured address, with `rebuild` calling convention.
* Tests: `offsets.rs` `HeapOffsetTable::is_ready` + `live_harness.py` assertions.

## 5. How to work it

* No guessed offsets in `src/roster/` — `offsets.rs` stays `None` until `live_offsets.json` is pasted.
* Every heap write is behind `validate_disp_order` and `can_select` parity, deduped by `order_fingerprint`, and logged via `diag::note`.
* Keep `R-55` `[!]` until `DumpCssRebuild`’s `rebuild` has been called live in Eden; reboot alone does not close it.

## 6. Quick start for the next session

```bash
# 1. Dump
python3 research/decomp/ssbu-re/tools/live_harness.py --eden-sd ~/.local/share/eden/sdmc --probe
# 2. Ghidra headless (example)
#   analyzeHeadless ghidra_proj ssbu_main -import main_decompressed.bin -processor AARCH64:LE:64:v8A -scriptPath gscripts -postScript DumpUiCharaDb.java
# 3. Paste the two addresses into sd:/visionary_heap_config.json and restart the game
# 4. Drag in Visionary with CSS open — check sd:/slight/diag.txt for `heap_patch: patched … via hash at …` and `rebuild at …`
```

