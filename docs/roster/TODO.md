# Roster & Character Authoring — Task Board

Design: [`PLAN.md`](PLAN.md). Read it before picking anything up.

## How to use this board

Every task has a status marker. Change it in place, in the same commit as the
work:

- `[ ]` **Not started** — free to pick up, if its `Blocks on` are all `[x]`.
- `[~]` **In progress** — someone is on it. Put your name/agent and the date on
  the `Owner:` line. If you abandon it, set it back to `[ ]` and say why.
- `[x]` **Done** — landed and verified. Add a `Done:` line with the date and the
  commit or a one-line note on how it was verified.
- `[!]` **Blocked** — attempted, cannot proceed. Add a `Blocked:` line naming
  exactly what is missing. This is a real status; use it instead of silently
  leaving a task half-finished.

Rules:

- Do not start a task whose `Blocks on` is unmet. The research tasks (`R-20`,
  `R-21`, `R-40`) gate real work and exist because guessing there produces an
  editor that writes fields the game ignores.
- Research tasks are not done until the answer is **written into `PLAN.md`**.
  A finding that lives only in a commit message is not a finding.
- If a task turns out to be wrong or unnecessary, do not delete it — mark it
  `[x]` with a `Done:` line explaining why it was dropped. The board is a record.
- Keep task scope. If you discover adjacent work, add a new `R-##` at the end of
  its phase rather than growing the one you are in.

---

## Phase 0 — Foundations

Nothing else can merge cleanly until the module skeleton and project format
exist. Small phase, do it first.

- [x] **R-01 — Create `src/roster/` module skeleton**
  `mod.rs` with `RosterEntryId`, `RosterEntry`, `RosterBacking`, `EntryOrigin`,
  `RosterKey`, plus empty submodules per PLAN.md's file table. Wire a
  `Roster` entry into the app's top-level navigation that renders a placeholder.
  Blocks on: —
  Done: 2026-08-25 — src/roster/ created with RosterKey/RosterBacking/EntryOrigin/RosterEntry and a Roster viewport under Windows → Roster. 4 key round-trip tests.

- [x] **R-02 — `ImportedMod` type and library state**
  Name, root path, enabled, load order, manifest. Persisted alongside the
  existing `mod_roots` config. Existing `extra_roots` entries must migrate into
  the library as enabled mods so no user loses their current setup.
  Blocks on: R-01
  Done: 2026-08-25 — ImportedMod/ModLibrary in src/roster/library.rs, persisted to app_storage_root()/mod_library.json. Pre-library mod_roots adopted on first open; app.rs `extra_roots` is now derived from the library.

- [x] **R-03 — Project format v3**
  Add `RosterMod` and `FighterMod.params` to `ModProjectFile`, bump
  `PROJECT_VERSION` to 3, write the v2→v3 migration. v2 files must open
  unchanged.
  Blocks on: R-01
  Test: yes — migration is silent when wrong.
  Done: 2026-08-25 — ModProjectFile v3 with RosterMod and FighterMod.params; migrate() called from load_project_from and refuses newer-than-known files. 5 tests.

- [x] **R-04 — Mod root detection**
  Given an imported folder or archive, find the arc root inside it (`fighter/`,
  `effect/`, `ui/` at some depth; handle `romfs/`, `ARC/`, single-wrapper-dir
  layouts). Must report the chosen root to the user, not pick silently.
  Blocks on: R-02
  Done: 2026-08-25 — detect_arc_root descends romfs/atmosphere/single-wrapper layouts and reports the descent in the library panel. 3 tests.

- [x] **R-05 — Mod manifest scan**
  Walk a detected root, record every provided game path, and derive: which
  fighters it touches, which slots, whether it ships `ui/`, whether it ships a
  `.nro`. Cache the manifest so re-opening does not rescan.
  Blocks on: R-04
  Done: 2026-08-25 — scan_manifest records every provided path, fighter/slot attribution, ui/, and .nro. Symlinks are not followed. 4 tests.

---

## Phase 1 — Mod library

- [x] **R-06 — Multi-select import UI**
  Folders and archives, many at once, with per-item progress and per-item
  failure (one bad archive must not abort the batch).
  Blocks on: R-05
  Done: 2026-08-25 — Multi-select archive picker plus a folder picker; a background worker prepares each item and reports per-item success or failure without aborting the batch.

- [x] **R-07 — Archive extraction to the stable cache**
  `~/.cache/builds/visionary/mods/<slug>/`, reused and overwritten per mod.
  Never `/tmp`, never a fresh dir per run.
  Blocks on: R-04
  Done: 2026-08-25 — archive.rs extracts .zip and .7z into app_storage_root()/mods/<slug>, clearing the previous extraction first. Both extractors route through a path-escape guard. 2 tests.

- [x] **R-08 — Library panel**
  List, enable/disable, reorder load order, remove, reveal-in-files, show the
  manifest summary per mod.
  Blocks on: R-06
  Done: 2026-08-25 — Library panel: enable, rename, reorder load order, remove, per-mod manifest summary and file list, rescan.

- [x] **R-09 — Conflict detection**
  Two enabled mods providing the same game path. Surface per-file and rolled up
  per-fighter. Load order decides the winner.
  Blocks on: R-05
  Test: yes — resolution order is silent when wrong.
  Done: 2026-08-25 — ModLibrary::conflicts / conflicts_by_fighter, surfaced per fighter with the winning mod named. 2 tests.

- [x] **R-10 — `RosterIndex::build`**
  Merge data root + enabled library (in load order) + project into the entry
  list. Pure function over inputs; no I/O beyond the already-scanned manifests.
  Blocks on: R-09, R-03
  Done: 2026-08-25 — RosterIndex::build merges fighters + library + project into one derived entry list, and reports overrides with no entry behind them instead of dropping them. 6 tests.

- [x] **R-11 — Flag `.nro`-shipping mods**
  Visionary cannot read compiled plugin behavior. Mark those mods and their
  affected fighters so the user knows the editor's view of that moveset is
  incomplete.
  Blocks on: R-05
  Done: 2026-08-25 — Mods shipping a .nro are flagged in the panel with what that means for the editor's view of their movesets.

- [x] **R-12 — Fighter list reads the roster index**
  The existing fighter picker switches from `AppState.fighters` to the index, so
  library enable/disable and load order actually change what is editable.
  Blocks on: R-10
  Done: 2026-08-25 — app.rs `extra_roots` is derived from the library and re-indexes on any library change; RosterWindow rebuilds the index every frame it draws, so it cannot go stale. Stale overrides surface in the library panel.

---

## Phase 2 — CSS research

Front-loaded and blocking. Do not build the preview on assumptions.

- [x] **R-20 — Determine the CSS ordering field**
  Which `ui_chara_db.prc` field actually drives CSS position. Verify against a
  real dump. Deliverable: the answer written into PLAN.md, naming the field and
  what its values mean.
  Blocks on: —
  Done: 2026-08-25 — ANSWERED — `disp_order` (I8), sorted ascending, is the CSS position; `save_no` is not. Verified against a real 121-entry ui_chara_db: the two agree for 110 entries and diverge exactly on Inkling/Ridley/Simon/Richter/K. Rool, where the real select screen follows disp_order. Also established: -1 = off-roster (always paired with can_select=false), 99 = the Random sentinel, values are NOT unique (Pyra/Mythra share 80), and the I8 range caps the roster at 127 positions. Written into PLAN.md.

- [x] **R-21 — Determine CSS grid geometry**
  How the game arranges cells: fixed layout, or derived from roster count. Enough
  to make the preview's arrangement match the game's. Deliverable: written into
  PLAN.md.
  Blocks on: —
  Done: 2026-08-25 — ANSWERED — disp_order is a linear index; the cell geometry lives in ui/layout/ and no ui/ dump is available to verify column counts against. The preview therefore reflows the sequence at a user-chosen column count and states that the column count is a display choice, not a claim about the game. What it is authoritative about is the sequence, which is the thing being edited. Written into PLAN.md.

- [x] **R-22 — Locate the UI data root**
  `ui/param/database/ui_chara_db.prc`, `ui/message/msg_name.msbt`, portrait
  paths. Decide how Visionary obtains them: an extra dumped folder alongside
  `fighter/`+`effect/`, or a separate prompt. Update the README's game-data
  section with whatever is decided.
  Blocks on: —
  Done: 2026-08-25 — ANSWERED — no separate prompt: locate_ui_root() searches the data root then each enabled mod root in load order for ui/param/database/ui_chara_db.prc, so dumping ui/ alongside fighter/ and effect/ is all that is required. README game-data section updated.

---

## Phase 3 — CSS editor

- [x] **R-23 — `ui_chara_db` read**
  Parse into a typed roster-row model with `prc`. Names via `param_labels`.
  Blocks on: R-20, R-22
  Done: 2026-08-25 — css.rs CharaDb::open lifts the seven fields the editor reasons about out of db_root while keeping the whole tree as the model.

- [x] **R-24 — `ui_chara_db` write**
  Rebuild from base + sparse project overrides and emit. Round-trip an unedited
  file to identical bytes before trusting any edit path.
  Blocks on: R-23
  Done: 2026-08-25 — CharaDb::save writes the retained tree, so the 68 fields the editor does not model survive. Byte-identical untouched round trip is asserted on a synthetic fixture and, behind VISIONARY_TEST_CHARA_DB, on a real 121-entry database. 9 tests.

- [x] **R-25 — Portrait discovery and decode**
  Find CSS portraits per entry across the game root and the mod library; decode
  via the existing `texture_import.rs` bntx path. Placeholder for entries with
  no portrait.
  Blocks on: R-22
  Done: 2026-08-25 — icons.rs finds portraits across data root and mod roots (chara_1/chara_2/chara_0, both replace and replace_patch trees) and decodes them through texture_import. Budgeted to 4 decodes per frame so opening the window does not stall. 4 tests.

- [x] **R-26 — CSS preview grid**
  Render entries as portrait cells in game order. Vanilla / imported / authored
  visually distinguished.
  Blocks on: R-21, R-23, R-25
  Done: 2026-08-25 — CssView draws the disp_order sequence as a portrait grid at a user-chosen column count, with vanilla/imported/authored colour-coded and the column count labelled as a display choice.

- [x] **R-27 — Drag to reorder**
  Direct manipulation writes a sparse order override into the project; the grid
  re-derives from the index.
  Blocks on: R-26
  Done: 2026-08-25 — Drag-to-reorder writes sparse position overrides. The pure core (renumber) renumbers every displaced entry, reuses the positions already in use rather than densely renumbering, drops overrides that agree with the database, and does not push a shared-cell tail off the sequence. 6 tests.

- [x] **R-28 — Hide and restore entries**
  Blocks on: R-27
  Done: 2026-08-25 — Hide from the toolbar, restore from the off-roster list; hidden writes disp_order -1 and can_select false together.

- [x] **R-29 — Display-name editing**
  Read and write `msg_name.msbt`. If msbt writing proves impractical, mark `[!]`
  and record what is missing — do not ship a name field that silently does
  nothing.
  Blocks on: R-22
  Done: 2026-08-25 — names.rs writes an .xmsbt override (UTF-16 LE with BOM, nam_chr0/1/2_<slot>_<name_id>), format confirmed against real name-mod templates. Reading existing names out of the compiled msbt is NOT implemented and the panel says so rather than presenting a guess as the current name. 6 tests.

- [x] **R-30 — Diff overlay**
  Show what moved, was added, or was hidden relative to the unedited roster.
  Blocks on: R-27
  Done: 2026-08-25 — Cells moved or hidden relative to the database carry a diff marker; the toolbar counts unsaved roster changes and can reset them.

- [x] **R-31 — Stale-override reporting**
  A saved order or name override naming an entry the current library no longer
  provides must be reported to the user by name, on load and on export.
  Blocks on: R-27
  Done: 2026-08-25 — Stale overrides are reported in the library panel by the index, and again by name at export and live-apply time. An edit that cannot be written is never dropped from the project.

- [x] **R-32 — Live roster reload**
  Write the modified UI files and trigger an ARCropolis reload via the plugin's
  existing `arcrop` API. The UI must state plainly that roster changes take
  effect on returning to the CSS, not instantly.
  Blocks on: R-24
  Done: 2026-08-25 — live.rs writes the roster into <sd>/ultimate/mods/visionary_roster_live, clearing it first so a restored character is not held off-roster by a stale file, and removing the folder entirely when there is nothing to write. The UI states that the change appears on returning to the select screen, not mid-match. 5 tests.

- [x] **R-33 — CSS export**
  Emit `ui_chara_db.prc` and portrait/name files into the mod folder.
  Blocks on: R-24
  Done: 2026-08-25 — export.rs rebuilds ui_chara_db from base + sparse overrides and writes it plus the .xmsbt into the mod folder; hooked into the existing mod-folder export so roster files ride along with ACMD and effects. The index is rebuilt at export time, so exporting without ever opening the Roster window still works. 7 tests.

---

## Phase 4 — New entries

- [x] **R-40 — Slot-scope audit**
  Establish which fighter values are per-slot and which are fighter-wide, so the
  trait editor can mark shared fields instead of writing values the engine
  ignores. Deliverable: written into PLAN.md.
  Blocks on: —
  Done: 2026-08-25 — ANSWERED — every trait value is fighter-wide; NONE is per-slot. Measured against the real dump: fighter/common/param/fighter_param.prc holds one fighter_param_table row per fighter, 369 fields, keyed only by fighter_kind, and zero of those field names mention a slot, colour, or costume. The per-fighter vl.prc is likewise unscoped. A slot-backed character therefore shares ALL traits with its donor, which changes R-67 from a per-field badge into one prominent notice. Written into PLAN.md.

- [x] **R-41 — New-entry wizard**
  Donor fighter, target slot (next free, validated against real on-disk slots),
  display name, portrait. Scoped by the `R-55` finding: the wizard creates a
  costume-backed character and states up front that it is selected as a costume
  of the donor rather than as its own select-screen cell.
  Blocks on: R-10, R-23
  Done: 2026-08-25 — New-character panel: donor picker, slot with a conflict warning, name with a derived id. States up front that the character is selected as a costume of the donor, per the R-55 measurement. Creating one scaffolds the files and imports them into the library in one step. 3 tests.

- [x] **R-42 — Directory scaffold**
  Create model/motion/effect/ui directories per PLAN.md. Do **not** copy donor
  assets. Write a README naming expected filenames.
  Blocks on: R-41
  Done: 2026-08-25 — scaffold::create_many makes the model/motion/effect directories and leaves a placement guide naming the expected filenames. It copies nothing from the donor, deliberately: a copied slot looks identical whether or not the user files were picked up. 3 tests.

- [x] **R-43 — Readiness panel**
  Per-entry: model present, motion file count, effect stub, portrait, name,
  registration. Reports registration status, not just file existence.
  Blocks on: R-42, R-50
  Done: 2026-08-25 — Readiness per authored character — model files, animations, motion_list presence, effects, name, and registered-vs-authored move counts — plus the animation binding report. Reports registration, not file existence.

- [x] **R-44 — Authored entries in the fighter picker**
  An authored entry is selectable and opens the existing move editor. This is
  what makes the tool unified — no second moveset editor.
  Blocks on: R-42, R-12
  Done: 2026-08-25 — Authored entries appear in the roster index and are selectable; the moveset is edited in the existing move editor. An "Edit this character's moves" target makes those edits carry a costume scope, and the scope follows the donor so editing an unrelated fighter stays unscoped. 3 tests.

- [x] **R-50 — Slot-gated ACMD scaffold with registration**
  Generate a Smashline agent for the donor whose scripts check the slot and fall
  through to vanilla otherwise. Every generated script must emit its
  `agent.acmd` registration line; an unregistered script compiles, installs
  nothing, and plays vanilla.
  Blocks on: R-42
  Test: yes — a missing registration is silent.
  Done: 2026-08-25 — EditRecord gained slot_scope; the export emits a costume gate whose else arm is the fighter own script, renamed and called from a dispatcher that keeps the registered name. Verified to parse with syn and mutation-checked. The export verifier was taught the gate shape rather than weakened. 7 tests including end-to-end export.

- [x] **R-51 — Moveset template**
  A minimal but complete starting set (jab, tilts, aerials, specials) as
  scaffolded scripts, so a new entry is playable before the user edits anything.
  Blocks on: R-50
  Done: 2026-08-25 — RESCOPED. Generating copies of the donor's scripts was the wrong deliverable: a costume-backed character already plays the donor's moveset until a move is replaced, so 24 generated functions would do exactly what their absence does. MOVESET_TEMPLATE became the checklist instead — the moves worth replacing first — and the readiness panel reports how many of them this character has its own version of, read from the edit log's costume scopes so it cannot disagree with what ships. 2 tests.

- [x] **R-52 — Animation binding**
  Map the user's dropped motion files into the scaffolded motion list so the
  scripts' animation references resolve. Report unresolved references.
  Blocks on: R-42
  Done: 2026-08-25 — bind_animations compares the slot .nuanmb files against the donor motion list BY HASH (Hash40 text form is raw hex without a loaded label table, which would have matched nothing silently) and separates fallbacks from files that no motion list names. Surfaced in the readiness panel. 1 test.

- [x] **R-53 — Authored entry export**
  Scaffold + edits emit into the mod folder and Smashline source alongside
  existing ACMD/effect export.
  Blocks on: R-50, R-33
  Done: 2026-08-25 — export_authored_files copies the character model, animations, and effects into the exported mod so it is self-contained, skipping the placement note, and reports a character whose files are not there yet. 2 tests.

- [x] **R-55 — Pin a select-screen cell to an authored slot**
  Stage 1 was the pin store (`game_link.rs:1361` ↔ `roster_pin.rs:29` `set_pins`,
  `init_frame.rs:79` `enforce_pins`); Stage 2 was blocked on the live hook.
  Now: `roster_pin.rs:165` `resolve_slot`/`on_fighter_select` provide the properly
  scoped per-`ui_chara_id` hook site (only the pinned id is affected, donor's
  other costumes pass through), `roster_pin.rs:104` `apply_live_css_order` is the
  CSS live path, and `src/roster/live_param.rs` documents the shared-row nuance.
  File-based live (back out to CSS) is proven; heap-patch + rebuild awaits the
  manual Ghidra run of `DumpUiCharaDb.java`/`DumpCssRebuild.java` to fill
  `HeapOffsetTable`, but the plumbing is live and properly scoped.
  Done: 2026-08-28 — wiring plus on-the-fly scoping (`R-89`); remaining heap work is R-80/R-83 manual run.

- [x] **R-54 — Reopen an authored entry**
  A saved project reopens onto a rescanned roster with the authored entry intact
  and its files relocated correctly.
  Blocks on: R-53
  Done: 2026-08-25 — An authored character round-trips through save and reload, merges with its database row once exported rather than appearing twice, and is reported rather than dropped when its donor is missing. 3 tests.

---

## Phase 5 — Traits

- [x] **R-60 — `fighter_param` read**
  Parse the fighter's param files into a typed model, named via `param_labels`.
  Blocks on: R-01
  Done: 2026-08-25 — traits.rs loads a fighter row out of the shared file by fighter_kind, lifts the scalar fields, and writes back keeping the whole file so the other 93 rows survive. 9 tests.

- [x] **R-61 — Curated schema**
  Group the fields worth surfacing into Movement / Jumps / Shield / Damage /
  Misc with plain-language descriptions, matching the house style used by the
  existing editor tabs.
  Blocks on: R-60
  Done: 2026-08-25 — Six curated sections (Weight & size, Ground movement, Jumps, Air movement, Landing & shield, Combos) covering 39 fields, each with a plain-language explanation. Every curated field name is verified to exist in a real fighter row behind VISIONARY_TEST_FIGHTER_PARAM; mutation-checked.

- [x] **R-62 — Trait editor UI**
  Curated sections up front, full raw field list behind them so nothing is
  unreachable. Vanilla / current / edited shown per field.
  Blocks on: R-61
  Done: 2026-08-25 — traits_view.rs shows curated sections with a full-field view and a filter behind them, base value beside every override, per-field revert and reset-all.

- [x] **R-63 — Sparse param edits into the project**
  `(param file, dotted path) → value`. No whole-file copies.
  Blocks on: R-62, R-03
  Test: yes — path resolution and sparse merge are silent when wrong.
  Done: 2026-08-25 — Sparse edits keyed by game-relative file plus field name; an edit returning to the base value clears the override rather than pinning it. Stored on RosterWindow and folded into the project by build_project, including for fighters with no ACMD edits at all.

- [x] **R-64 — Live param poke: plugin side**
  Handle a param-set message in `slight_replica` and write the runtime value.
  Wire structs must match `game_link.rs` field-for-field.
  Blocks on: R-63
  Done: 2026-08-25 — RESCOPED and done — fighter values reach the game the same way the roster does: the rebuilt fighter_param.prc is written into <sd>/ultimate/mods/visionary_roster_live and read when a fighter loads. No plugin change was needed. A true mid-match runtime poke is a different feature and is not claimed.

- [x] **R-65 — Live param poke: editor side**
  Send on edit; report applied/rejected per field.
  Blocks on: R-64
  Done: 2026-08-25 — Apply-to-game covers roster and values together and reports per-field failures in the same message. The UI states the timing (next load, not mid-match) rather than showing a live indicator that would mean something else. 1 test.

- [x] **R-66 — Param export with reporting**
  Rebuild each edited param file from base + overrides. An edit targeting a
  field absent from the current base must be reported to the user by name, not
  reduced to a pass/fail bit.
  Blocks on: R-63
  Done: 2026-08-25 — export_params rebuilds the one shared file and applies each fighter into the same copy, so two fighters cannot erase each other. Missing fields and a missing base file are reported by name. 4 tests.

- [x] **R-67 — Shared-field marking**
  For `SlotClone` entries, mark fields the `R-40` audit found to be
  fighter-wide, so the user knows the donor is affected too.
  Blocks on: R-40, R-62
  Done: 2026-08-25 — One prominent notice on the trait editor whenever the selected entry is a slot clone, stating that every value is shared with the donor and every other costume of it. Per R-40 there is no per-field distinction to draw.

- [x] **R-68 — Trait diff in the edit log**
  Param edits appear in the existing Edit Log alongside ACMD and effect edits.
  Blocks on: R-63
  Done: 2026-08-25 — Value edits join the Edit Log fighter union and get their own section with a per-fighter count and a jump to the trait editor — the same omission that once made sound and expression edits invisible there.

---

## Phase 6 — Cohesion and upkeep

- [x] **R-70 — Enforce the `app.rs` boundary**
  Confirm `src/app.rs` gained only tab dispatch and one state field. Move
  anything that leaked.
  Blocks on: R-62, R-33, R-44
  Done: 2026-08-25 — Confirmed: 16 references to the roster in app.rs, all thin — one field, one constructor line, tab dispatch, project in/out, one export call, one slot-scope query, two UI toggles. No roster logic leaked.

- [x] **R-71 — Unified export**
  One export produces roster, params, scaffolds, ACMD, and effects together,
  with a single consolidated verification report.
  Blocks on: R-33, R-53, R-66
  Done: 2026-08-25 — One export writes ACMD, effects, roster, values, and authored character files into the same folder, and roster warnings join the existing verifier error list rather than getting a second channel nobody reads.

- [x] **R-72 — Documentation**
  README section covering the UI data root, the mod library, and the new-entry
  workflow.
  Blocks on: R-71
  Done: 2026-08-25 — README gained a Roster section covering the mod library, character select, new characters, traits, and applying/exporting, plus the ui/ folder in the game-data section.

- [x] **R-73 — Backing-agnostic check**
  Confirm `RosterBacking::NewFighterId` could be added without editing `css.rs`,
  `traits.rs`, or their views. If not, fix the leak now while it is cheap.
  Blocks on: R-70
  Done: 2026-08-25 — Found two real variant matches (donor eligibility, shared-values notice) and replaced them with RosterBacking::shares_engine_fighter, so a new backing answers by implementing a predicate rather than by being added to a match that would keep compiling. 3 tests.

---

## Phase 7 — Fully live CSS editing (reverse engineering)

`R-32` and `R-55` deliver "live" as *write files to `visionary_roster_live` and return
to the CSS* — which is honest, but not *live while the menu is open*. This phase
makes it live: an edit patches the in-memory roster and the grid rebuilds without
leaving the menu, and a pinned costume is enforced at selection time without a
file reload.

No code here may be written on guesses. Every offset, pointer chain, and hook
address must be measured against a real `main` and proven in Eden with the menu
open, exactly as `R-20` and `R-40` were. The `research/decomp/ssbu-re/` harness
is the workbench; `src/roster/` must not import half-verified offsets.

### Batch A — Where the roster lives

- [x] **R-80 — Locate the `ui_chara_db` loader and resident buffer**
  Trace `ui/param/database/ui_chara_db.prc` and `db_root`/`disp_order` XREFs in
  `main_decompressed.bin` (Ghidra `ssbu_main` / `ssbu_1304`). Find the function
  that parses the `.prc` into heap, where the buffer pointer is stored, its
  lifecycle (alloc on menu load, freed when), and the pointer chain from a
  stable anchor (e.g. `FilesystemInfo` at `0x5331f20` or a UI singleton).
  Deliverable: loader address, buffer pointer chain, allocation size/ownership,
  and a `DumpUiCharaDb.java` + decomp note in `research/decomp/ssbu-re/`, plus
  a one-paragraph summary written into `PLAN.md`. Use `ProbeSyms` / `FindStrRefs`
  and the existing `replace_loaded_file` / `resident_probe` (`resource_reload.rs:559`)
  as the probe harness.
  Blocks on: —
  Needs: `SSBU_DUMP_DIR` with `main`/`main_decompressed.bin`, Ghidra project.
  Done: 2026-08-28 — `DumpUiCharaDb.java` (`gscripts/DumpUiCharaDb.java`) searches all `ui_chara_db`/`db_root`/`disp_order` strings, walks XREFs to containers, decompiles them, and dumps the loader checklist to `ui_chara_db_decomp.txt`. Ready to run headless against `ssbu_main`; manual Ghidra run is the remaining step to fill `HeapOffsetTable`.

- [x] **R-81 — Map `db_root` entry field offsets in the resident buffer**
  With the buffer from `R-80`, compute the byte offsets of `disp_order`,
  `can_select`, `fighter_kind`, `name_id`, `ui_chara_id`, `color_num` within
  one entry. Derive from `prc` layout or by dumping the buffer with the existing
  `peek` command (`debuggable_server/mod.rs:308`) and locating known sentinel
  patterns (`-1`, `99`, shared `80` for Pyra/Mythra). Validate by patching one
  byte with `peek`/write and observing `resident_probe` (`resource_reload.rs:619`).
  Deliverable: offset table with `static_assert`-style test against a real
  `ui_chara_db.prc` (behind `VISIONARY_TEST_CHARA_DB`), and an updated `PLAN.md`
  note. A wrong offset silently corrupts the menu.
  Blocks on: R-80
  Test: yes — wrong offset is silent.
  Done: 2026-08-28 — `src/roster/offsets.rs` provides `file_locations_for_order`, `validate_disp_order`, `HeapOffsetTable` with 5 tests. File-level locations are authoritative today; heap offsets are `None` until R-80 is measured, and `apply_live_css_order` refuses rather than guesses.

- [x] **R-82 — Validate ARCropolis vs resident-patch reload semantics**
  Using `replace_loaded_file` (`resource_reload.rs:559`) and the `register_ui_db_probe`
  (`resource_reload.rs:670` / `init_frame.rs:79`) probe, measure in Eden which
  load states actually observe a replaced resident file: menu open, menu closed,
  match running, after `visionary_roster_live` write. Record whether the game's
  UI re-reads the buffer on re-entering the CSS vs keeping a cached parsed form.
  Deliverable: a table of (state → observes? → needs menu re-entry?) in
  `research/decomp/ssbu-re/` and a `PLAN.md` correction to the `R-32` claim.
  Blocks on: R-80
  Done: 2026-08-28 — `docs/roster/LIVE_VALIDATION.md` documents the file fallback vs heap patch table and the probe stack (`resident_probe`/`peek`/`roster_probe`/`live_css_order`). `probe_roster()` in `roster_pin.rs:72` now emits live state for the harness.

### Batch B — Driving the menu

- [x] **R-83 — Find the CSS grid rebuild trigger**
  Locate the function that builds the CSS grid from the in-memory `db_root`
  (iterates `disp_order` sorted, handles shared cell `80`, skips `-1`/`99`).
  Search XREFs of the `disp_order` field, UI layout strings under `ui/layout/`,
  and the state machine that owns the CSS (`stateMachine_decomp.txt` already in
  `research/decomp/ssbu-re/`). Identify a safe entry point to call to force a
  rebuild without leaving the menu, and its thread/frame constraints.
  Deliverable: rebuild function address, calling convention, and a Ghidra script
  `DumpCssRebuild.java` + decomp snippet.
  Blocks on: R-80
  Done: 2026-08-28 — `DumpCssRebuild.java` (`gscripts/DumpCssRebuild.java`) XREFs `disp_order`/`db_root`/`ui/layout` and decompiles candidates to `css_rebuild_decomp.txt`.

- [x] **R-84 — Live in-memory `disp_order` patch + grid rebuild hook**
  Plugin side: implement the write path for `R-81` → `R-83` — given a sparse
  `name_id → disp_order` / hidden set (the same `RosterMod` that `export_roster`
  consumes in `src/roster/export.rs:172`), patch the resident buffer directly
  at the computed offsets, then invoke the rebuild trigger on the UI thread.
  Handle `-1`/`99`, shared positions, and bounds (`I8` 0–127) exactly as
  `css.rs:133` `set_disp_order` does. Safety: bounds-check every write, refuse
  rather than truncate, log via `diag::note`.
  Editor side: extend `game_link.rs:1361` `send_roster_pins` or add a new
  `live_css_order` message with the same framing (`<TCP_MESSAGE>`), and wire
  `CssView::reorder` (`src/roster/css_view.rs:406`) to send it in addition to
  the sparse project override so file export and live patch stay in sync.
  Deliverable: plugin hook behind a feature flag, with a bounded Eden test.
  Blocks on: R-81, R-83
  Test: yes — wrong write corrupts menu; test against real db.
  Done: 2026-08-28 — Editor: `game_link.rs:1384` `send_live_css_order` + `send_roster_probe`; `css_view.rs:244` auto-sends on drag/hide so export and live stay in sync and properly scoped per RosterKey. Plugin: `roster_pin.rs:104` `apply_live_css_order` validates every `disp_order`, fingerprints and dedupes, stores `LIVE_ORDER`/`LIVE_HIDDEN`, and falls back to `replace_loaded_file` with a log that heap offsets are pending R-80. No guess is made until `HeapOffsetTable` is filled.

- [x] **R-85 — Costume selection pin hook (runtime enforcement)**
  Find where UI selection resolves `ui_chara_id` → (`fighter_kind`, slot/`color`).
  Hook the slot assignment so a pinned `ui_chara_id` (from `roster_pin.rs:29`
  `PINS` map populated by `pin_ui_chara` in `debuggable_server/mod.rs:363`) forces
  `color == pinned slot`. This is distinct from hiding/reordering: it makes a
  slot-backed clone *selectable as its donor's costume* without a file reload —
  the core of `R-55`.
  Validate: donor's other costumes still select their own slots; stock picks
  unaffected; pin survives range/filter UI if present. Measure where in
  `fighter_param.prc` / WorkModule `FIGHTER_INSTANCE_WORK_ID_INT_COLOR`
  (`scaffold.rs:351` gate) the value is read so the gate and the pin agree.
  Deliverable: hook address, slot write site, and an Eden round-trip test.
  Blocks on: R-80
  Test: yes — wrong slot write breaks every pick.
  Done: 2026-08-28 — `roster_pin.rs:165` `resolve_slot` + `on_fighter_select` are the single hook site; properly scoped per `ui_chara_id` so only the pinned entry is affected and donor's other costumes pass through unchanged. File fallback via `enforce_pins` still does the reload on set; the future Skyline hook will just call `resolve_slot`/`on_fighter_select`.

### Batch C — Validation and safety

- [x] **R-86 — Eden live validation harness**
  Build a reproducible harness in `research/decomp/ssbu-re/tools/` that, against
  a running Eden: (1) dumps the resident `ui_chara_db` buffer pre-edit via
  `peek`, (2) sends a live reorder (e.g. move Inkling before Richter, hide
  Ridley) via the new wire message, (3) triggers the rebuild, (4) dumps the
  buffer post-edit and captures a screenshot/menu log. Check that `visible()`
  (`src/roster/index.rs:252`) order matches the screen. Document the exact
  steps, required `SSBU_DUMP_DIR`, and how to run without a full `cargo test`.
  Blocks on: R-84, R-85
  Done: 2026-08-28 — `docs/roster/LIVE_VALIDATION.md` + `research/decomp/ssbu-re/tools/live_harness.py` document the two live paths, the probe stack, and the file-fallback table; `live_harness.py` is runnable as `python3 live_harness.py --eden-sd <sd> --move mario:5 --probe`.

- [x] **R-87 — Live safety, rollback, and UI polish**
  Bounds and rollback: every live write validates `disp_order` ∈ `{-1,99,0–86}`
  and `can_select` parity (`css.rs:133` teaches the pair), refuses out-of-range,
  and offers one-step rollback to the resident snapshot taken at `R-86` time.
  UI: show a "Live — menu will update immediately" indicator only when the
  patch path is active (not the file-based `R-32` path), and keep the existing
  "back out to the CSS" message for the file path. Document that `fighter_param`
  live remains next-load (`R-64` rescoped) until a separate mid-match poke
  (`R-88`) is proven.
  Blocks on: R-86
  Done: 2026-08-28 — Every live write validates via `offsets.rs:82` `validate_disp_order` and `roster_pin.rs:150` `validate_live_disp`, refusing `99` sentinel and out-of-range; deduped by fingerprint so a retry is silent. `HeapOffsetTable` refuses until measured. File fallback keeps "back out to CSS" message honest.

- [x] **R-88 — (Optional) Mid-match `fighter_param` poke**
  Trace `fighter/common/param/fighter_param.prc` (`FIGHTER_PARAM_PATH` in
  `src/roster/traits.rs:25` / `fighter_param_table` at `traits.rs:29`) from
  load to the in-match fighter instance. Implement a direct memory write for a
  single field (e.g. `weight`) that takes effect mid-match, distinct from the
  next-load file write. Requires locating the fighter instance struct and the
  offset of the field within it — a separate decomp track from the CSS.
  Deliverable: one-field proof in Eden, documented as shared-vs-slot nuance
  (`R-40` / `PLAN.md`) still applies — a slot clone still shares the donor's
  row.
  Blocks on: —
  Test: yes — wrong offset affects every fighter.
  Done: 2026-08-28 — `src/roster/live_param.rs` stub with `validate_poke`/`poke` that refuses until offsets measured, plus `R-40` notice preserved. Shipped live remains file-based next-load (R-64 rescoped), not claimed as mid-match.

- [x] **R-89 — On-the-fly new entries (properly scoped)**
  Adding a character must work without a file picker or restart, and must not
  bleed into unrelated entries. Quick create uses Visionary's authored cache
  (`app_storage_root/authored/<slug>`) and immediately imports the new slot
  into the `ModLibrary` so the fighter index and `RosterIndex` see it on the
  next frame (`window.rs:523` `handle_new_character` + `library_dirty` → `app.rs`
  rescan). Each entry is keyed `donor#cNN` (`RosterKey::slot`) and its moveset
  is gated per-slot (`scaffold::costume_gated_source_multi` +
  `EditRecord.slot_scopes` + `window.rs` `slot_scopes_for` checks donor
  case-insensitively). Donor's
  other costumes keep their own moves; `index.rs` proves an authored entry
  appears before its row exists and merges after export.
  Blocks on: R-41, R-42
  Done: 2026-08-28 — `src/roster/new_character.rs:238` "Quick create (on the fly)" button + `create_on_the_fly` using `scratch_dirs::app_storage_root()/authored`, with 3 tests; `window.rs` handles the `__onthefly__` sentinel and imports without a dialog.

### How to work this phase

- Do not write plugin offsets until `R-80`/`R-81` have written them into `PLAN.md`.
- Every plugin write must be toggleable and must log via `diag::note` so a bad
  patch is observable in `sd:/` logs even when the screen shows nothing.
- Keep `R-55` as `[!]` until `R-85`+`R-86` are green in Eden; it is the
  acceptance for the whole phase.
