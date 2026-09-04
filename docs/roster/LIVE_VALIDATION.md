# Live CSS validation harness (R-82, R-86)

This is the R-82 / R-86 deliverable: how to prove whether a roster live
change landed in the game's memory.

## What is being validated

Two live paths share the same sparse input (`RosterMod.order` / `hidden`):

* **File fallback** (R-32, today): `src/roster/live.rs:60` `apply_live` writes
  `ui_chara_db.prc` into `sd:/ultimate/mods/visionary_roster_live/` and calls
  `resource_reload::replace_loaded_file` for the resident buffer. The menu must
  be re-entered to see the change. This is honest and already ships.

* **Heap patch** (R-84, behind the flag): `plugins/slight_replica/src/slight/roster_pin.rs:104`
  `apply_live_css_order` writes `disp_order` / `can_select` directly into the
  heap copy of `db_root` at offsets from `src/roster/offsets.rs:102`
  `HeapOffsetTable` (once measured in R-80/R-81) and then invokes the rebuild
  trigger from R-83. This is in-menu live and requires the loader address.

The harness proves which one you are on.

## Probe infrastructure already in the binary

* `plugins/slight_replica/src/slight/effect_viewer/resource_reload.rs:619`
  `resident_probe(hash, needle)` — size + first 8 bytes of the resident buffer.
* `plugins/slight_replica/src/slight/roster_pin.rs:72` `probe_roster` — called
  on `roster_probe` command and after every live change.
* `src/game_link.rs:1399` `send_roster_probe` — editor asks for a probe.
* `plugins/slight_replica/src/rust_extender/debugging/debuggable_server/mod.rs:308`
  `peek` — raw hex dump of any address, for dumping heap entries once offsets
  are known.
* `src/game_link.rs:1369` `send_live_css_order` — the in-mem mirror of the
  sparse project write.

## Manual steps in Eden (no harness)

1. Boot Eden, load a save at the CSS.
2. In Visionary, drag a character (e.g. Inkling `disp_order 64` onto Richter `67`).
   `live_css_order` is sent automatically (`src/roster/css_view.rs:244`).
3. In the plugin log (`sd:/` or `diag::note`), look for:
   `live_css_order: X order overrides` and `roster_probe: size=... head=...`.
4. If heap offsets are pending R-80, the log will say
   `heap_offsets pending R-80 — fallback replace_loaded_file` and the change
   will appear after backing out to the CSS (file path). If heap patch is
   active, the grid rebuilds without leaving the menu.

## Automated harness (R-86)

`research/decomp/ssbu-re/tools/live_harness.py` drives this over TCP:

```bash
export SSBU_DUMP_DIR=/path/to/external/ssbu-dumps
python3 research/decomp/ssbu-re/tools/live_harness.py \
  --eden-sd ~/.config/yuzu/sdmc \
  --move mario:5 --hide ridley --probe
```

It does:

1. Take a `roster_probe` pre-snapshot (peek + resident_probe).
2. Send `live_css_order` with the requested reorder/hide.
3. Wait for `live_css_order` ack in the log.
4. Take a post-snapshot and diff the two `db_root` dumps by name_id.
5. Assert that the moved entries' `disp_order` changed and that
   `visible()` order (`src/roster/index.rs:263`) matches the requested order.

See `research/decomp/ssbu-re/tools/live_harness.py` for the exact TCP framing
(`<TCP_MESSAGE>…</TCP_MESSAGE>`) and for how to capture a screenshot for
manual comparison.

## Expected result table (R-82)

| Game state       | File fallback observes? | Heap patch observes? | Needs menu re-entry? |
|------------------|-------------------------|----------------------|----------------------|
| CSS open         | no (until re-enter)     | yes (with rebuild)   | fallback: yes, heap: no |
| CSS closed       | yes (next enter)        | N/A                  | no                   |
| In match         | no (until next CSS)     | no (UI not resident) | —                    |

Until the heap table is filled, every row in the table says "file fallback"
and the harm is only that the preview claims "live" while the game says
"back out to CSS" — which is why `live.rs:41` `describe` still says that
until `HeapOffsetTable::is_ready()` is true.
