# Visionary TO DO

A standing backlog of ACMD coverage gaps and known rough edges. Each entry is a complete
work order: any model can cold-start into this file, take the top unblocked task, and finish
it without prior conversation.

## How to work this list

1. Read **Working agreement** and **Definition of done** below. They apply to every task.
2. Take the first task whose status is `[ ]` and whose **Blocked by** is satisfied.
3. Set it to `[~]` with your date before starting, `[x]` when it meets the definition of done.
4. Commit the code and the status flip together, on a branch, not on `main`.
5. If a task turns out to be wrong or impossible as written, do not silently drop it —
   rewrite the entry with what you learned and leave it `[ ]`.

Status key: `[ ]` open · `[~]` in progress · `[x]` done · `[!]` blocked (say why inline).

## Working agreement

**The rule this whole file exists to enforce: an edit that reaches one surface and not the
others is a bug, not a partial feature.** A value the user can drag in a panel but that never
reaches the game is a lie; a value that reaches the game but not the export is a mod that
doesn't ship what was previewed.

Every ACMD capability lives on **five surfaces**. A task is not done until it is coherent
across all five, or until the entry says explicitly which surface is out of scope and why.

| # | Surface | Where | What it means |
|---|---|---|---|
| 1 | **Parse + IR** | [acmd.rs](src/acmd.rs), [data.rs](src/data.rs) | The call becomes a typed variant instead of falling into `Raw`. Unknown tails are kept verbatim so an export can reproduce the caller's own macro. |
| 2 | **Panel** | [app.rs](src/app.rs) | The user can see and change it. Timeline placement if it has a frame range. |
| 3 | **Live** | [game_link.rs](src/game_link.rs) + [hitbox_viewer/mod.rs](plugins/slight_replica/src/slight/hitbox_viewer/mod.rs) / [acmd_hooks.rs](plugins/slight_replica/src/slight/effect_viewer/acmd_hooks.rs) | The plugin hooks the primitive, captures it, and applies suppress/override/inject rules for it. |
| 4 | **Export** | `emit_*` in [acmd.rs](src/acmd.rs) | Regenerating the function reproduces the call, including anything added, deleted, or retimed. |
| 5 | **Write-back** | [acmd_src.rs](src/acmd_src.rs) | Editing a value rewrites that argument in the user's own source, touching nothing else. Structural change is *reported*, never guessed. |

Two export paths, different rules — do not conflate them:

- **Export to `acmd_source/`** regenerates the whole function from the IR. Structure is free:
  add, delete, retime.
- **Write-back into the user's linked project** rewrites argument *values* only. Their macros,
  comments, and formatting survive verbatim. Anything structural goes in the skip report with
  a reason naming the macro. Never write a guess.

### Traps that have already cost real time

- **Argument slots are per family.** An id that means one thing in `ATTACK` means something
  else in `CATCH` or `AREA_WIND`. Never reuse a slot table across families — a cross-family
  write silently corrupts a different call. Give each new family its own table.
- **Two sources of truth for arity, and they disagree. You need both.**
  `/home/leap/.cargo/git/checkouts/smash-script-*/*/src/macros.rs` tells you what *compiles*,
  which is what an export must emit. The vanilla archive tells you what you must *parse* — and
  it omits optional arguments that smash-script declares. A2 found `ATTACK_IGNORE_THROW`
  written with 33 arguments against a 36-parameter signature. Count the corpus per family
  before writing a slot table:

  ```bash
  # counts | arity, excluding the leading `agent` — matches the tables in this file
  grep -rho 'macros::YOUR_MACRO([^;]*' ~/.cache/visionary/script-cache/ | \
    awk -F',' '{print NF-1}' | sort | uniq -c
  ```

  If a family has more than one arity, the discriminator must be the argument *shape*, not the
  count — a length test breaks the moment a call is short for a different reason.
- **Symbolic constants need both directions.** The editor holds names, the wire wants numbers.
  A new constant needs a `ConstTable` in [param_labels.rs](src/param_labels.rs) plus
  `decode_const`/`encode_const` coverage, or the live path sends `None` and the game keeps its
  own value while the panel claims otherwise.
- **New collision families must be added to the plugin's `is_collision_func`**
  ([hitbox_viewer/mod.rs:494](plugins/slight_replica/src/slight/hitbox_viewer/mod.rs:494)) or
  the capture stream drops them. It is documented as being kept in step with the editor's
  bucketing — keep it so.
- **No SD I/O on a per-frame path in the plugin.** Free on the dev machine's Linux, a whole
  frame budget on a Windows tester's. Throttled work goes through `slight::sd_poll`.
- **Old plugin builds must keep working.** Every wire field is `Option` +
  `skip_serializing_if`; a plugin that predates a field ignores it and applies the rest.
  Keep that property.

## Definition of done

A task is done when all of these hold. Copy this list into your working notes; don't
paraphrase it from memory.

- [ ] The five surfaces above are coherent, or the entry names the out-of-scope surface.
- [ ] `bash build_check.sh` passes.
- [ ] `cargo test` passes (248 tests green as of this file's writing), including the two
      corpus oracles — run them by name with `cargo test cached_script`:
      `acmd_verify::tests::every_cached_script_survives_its_own_export` and
      `acmd::tests::cached_scripts_round_trip_through_the_emitter`. They run the new code over
      every script the app has ever fetched (currently 461 files under
      `~/.cache/visionary/script-cache`, ~1000 functions), and a clean run there means the
      emitter is a faithful inverse of the parser across real code, not just across the cases
      someone thought to write down.
      **Both return early and pass vacuously if that cache directory is missing.** The first
      then asserts `checked > 100`, so a *thin* cache fails loudly — but an *absent* one is
      silently green. Confirm the directory exists before trusting either.
- [ ] A round-trip test for the new family: parse a real vanilla call → emit → parse again →
      identical IR. Put the real call in the test, not a synthetic one.
- [ ] A write-back test asserting that a value edit rewrites *only* that argument span, and
      that a structural edit lands in the skip report with a reason naming the macro.
- [ ] The plugin builds if it was touched (`bash plugins/slight_replica/scripts/build.sh`),
      and the deployed build stamp in `diag.txt` matches what you just built.
- [ ] [README.md](README.md) updated if user-visible behaviour changed. House style: plain
      imperative button labels, no ellipsis.

---

# Part 1 — ACMD coverage gaps

Ordered so that earlier tasks unblock later ones. Within a section, top is highest value.

## Foundations

### [~] A1 — Wind write-back into the user's own source (started 2026-08-04)

The one gap in an otherwise fully editable family. Windboxes are editable in the panel, live,
and on export, but a wind edit to a *linked project* is refused: `AREA_WIND` is treated as a
flat float list with no layout to retune against, so [acmd_src.rs](src/acmd_src.rs) reports it
instead of writing it. The user drags a wind value, sees it in game, and the file never changes.

The premise is now false. `WindboxData::expected_arity`
([data.rs:132](src/data.rs:132)) already pins each command to a fixed arity —
`AREA_WIND_2ND_RAD` 8, `_RAD_arg9` 9, `AREA_WIND_2ND` 9, `_arg10` 10 — and the panel already
maps slots to named fields (id, strength, falloff, area). That is a layout.

- **Work order:** give each of the four commands its own slot table, in the shape
  `attack_edits`/`catch_edits` already use. Reuse `scan_macro_sites` for spans. Refuse
  cross-command edits: a `_RAD` value must never be written into a rectangular call.
- **Done when:** a wind value edit rewrites exactly that argument; a command change
  (rect ↔ radial) or an add/delete is reported, not written.
- **Trap:** the four commands share a prefix and nothing else. One table per command.

### [x] A2 — `ATTACK_IGNORE_THROW` as a first-class hitbox

Done 2026-08-03. All five surfaces; `cargo test` 254 green, plugin builds.

**It was not the cheap task this entry predicted, and the entry's premise was wrong.** Kept
here in full because the reason is a fact about the corpus that the next family task needs.

The plan said: confirm the arity, and if it matches `ATTACK`, just carry the name. smash-script
declares both with the same 36 parameters, so that check passes. **The archive does not agree
with smash-script.** Measured across the local corpus:

| macro | arguments | occurrences |
|---|---|---|
| `ATTACK` | 36 | 386 |
| `ATTACK_IGNORE_THROW` | **33** | 1 |
| `ATTACK_ABS` | 16 | 32 |
| `CATCH` | 8 or 11 | 6 + 6 |

The archive writes `ATTACK_IGNORE_THROW` with the `x2`/`y2`/`z2` capsule options simply left
out. Read against `ATTACK`'s table it parses, and parses *wrong*: hitlag lands in the capsule,
`*ATTACK_LR_CHECK_POS` in hitlag, every property after that off by three. It would have
silently corrupted kirby/ThrowHi — the one real usage — and looked fine doing it.

So the capsule triple is now detected from the **arguments** (all three spelled `None`/`Some(..)`),
not from the macro name or the argument count, and everything past the transform shifts when it
is absent. Both the parser (`capsule_slots_present`) and the write-back slot table
(`shift_past_absent_capsule`) do this; they are separate code paths and both were wrong.

Exports always write the long form, since that is the signature smash-script declares and
therefore the only one that builds.

- **Landed:** `func` on `AttackCall`/`Hitbox` (serde-defaulted, so old projects load), family
  match in `parse_excute_block`, name in `emit_attack`, name-scoped site matching in
  `rewrite_hitboxes`, `func` carried off the live capture, `inject_command` on the wire, plugin
  dispatch in the inject arm, and a **Macro** dropdown in the hitbox properties.
- **Left alone deliberately:** the live *override* path needed no change — the plugin hooks
  both macros through the same `attack_hook!` and rules are keyed by category, id, and frame.
  Only *injection* (added or retimed hitboxes) had to learn the name.

**For whoever takes B1/B2:** do not trust a signature in `macros.rs` to tell you what the
archive wrote. Count the arguments in the corpus first — the one-liner that produced the table
above is worth rerunning per family.

### [ ] A3 — `LAST_EFFECT_SET_RATE` as an editable field

Blocked by: nothing. Already parsed into the IR
([acmd.rs:437](src/acmd.rs:437)) and re-emitted. It just has no UI, so the value round-trips
untouched and can't be changed. Surfaces 1 and 4 are already done; this is 2, 3, 5.

- **Work order:** expose rate on the owning effect call in the Effects panel. It applies to the
  *last spawned* effect, so it belongs to the spawn above it — bind it there, not as a free row.
- **Trap:** "last spawned" is positional. Reordering or disabling the spawn above it changes
  what it modifies. Decide and document what happens then; a silent reattachment is worse than
  a refusal.

## Hitbox families

### [ ] B1 — `ATTACK_ABS` (absolute-position hitboxes)

Preserved verbatim today. Positions are world-absolute rather than bone-relative, so the
viewport gizmo and the bone dropdown do not apply as written.

**Measured, not assumed:** 16 arguments, 32 occurrences in the local corpus (A2's table).
That is less than half of `ATTACK`'s 36 — it is a genuinely different call, and none of
`AttackCall`'s hit-property block can be reused positionally.

- **Work order:** own family, own slot table, own struct. Reuse `AttackCall`'s hit-property
  block only if the arities genuinely match — they do not, so expect a real struct.
- **Done when:** it draws in the viewport at the right place and survives the corpus oracles.
- **Trap:** the shared-transform assumption behind the bone dropdown is wrong here. Suppress
  the bone control rather than showing one that does nothing.

### [ ] B2 — `ATTACK_FP` (fighter-position hitboxes)

Same shape as B1. Do it immediately after, while the family-splitting pattern is fresh.

### [ ] B3 — Post-hoc hitbox tuning

`ATK_POWER`, `ATK_LERP_RATIO`, `ATK_HIT_ABS`, `WHOLE_HIT`,
`ATK_SET_SHIELD_SETOFF_MUL` + its `_arg3`/`_arg4`/`_arg5` variants. These modify hitboxes
*already out*, so they are edits to an existing collision rather than new collisions.

- **Work order:** model them as modifiers attached to the hitbox id they target, shown on the
  timeline at their own frame. Do not fold their values into the parent `ATTACK` — the export
  must re-emit them as separate calls at their own frames.
- **Trap:** the `_argN` suffixes are different arities of the same idea, exactly like the wind
  family. One table each.

### [ ] B4 — Hurtbox control

`HIT_NODE`, `HIT_NO`, `HIT_RESET_ALL`, `COL_NORMAL`, `COL_PRI`. Intangibility and hurtbox
state per bone — the thing every competitive-facing mod actually wants to tune, and currently
invisible.

- **Work order:** these are per-bone state over a frame range, not collisions. They want their
  own panel and their own timeline lane, not a row in Hitboxes.
- **Trap:** `HIT_NODE` takes a bone and a state constant. That constant needs a `ConstTable`
  with both directions before the live path can carry it.

### [ ] B5 — `SEARCH` / detection boxes

`SEARCH`, `SET_SEARCH_SIZE_EXIST`, `ENABLE_AREA`, `UNABLE_AREA`. Grab-range and detection
volumes. Geometrically these are close to grab boxes, so the panel work is mostly reuse.

- **Trap:** new collision family → it must be added to the plugin's `is_collision_func` and
  given a category id on the wire, alongside 0 attack / 1 grab / 2 wind.

## Effects

### [ ] C1 — The remaining `LAST_EFFECT_SET_*` modifiers

Blocked by: A3 (settles how a "modifies the previous spawn" call is attached).

`LAST_EFFECT_SET_COLOR`, `_ALPHA`, `_SCALE_W`, `_OFFSET_TO_CAMERA_FLAT`, and
`LAST_PARTICLE_SET_COLOR`. Same attachment rule as A3; reuse whatever that task settled on.

- **Trap:** kind-level colour/speed overrides already exist on the live wire and apply to
  *every* spawn of an effect. These are per-spawn. Read the comment above `LiveOverride`
  ([game_link.rs:374](src/game_link.rs:374)) before wiring anything — conflating per-kind with
  per-emitter recoloured whole effects once already.

### [ ] C2 — Sword trail joints

`AFTER_IMAGE4_ON` / `_arg29` / `AFTER_IMAGE_ON`. Read-only today: the trail shows on the
timeline with its graphic and joint, and write-back explicitly refuses it
([acmd_src.rs:785](src/acmd_src.rs:785)) because a trail has no transform.

That refusal is correct and must stay. But a trail *is* placed — by the joints it names,
arguments 4 onward. Those are editable; the transform never will be.

- **Work order:** expose the joint pair and the trail parameters. Keep refusing transform
  writes, with the existing message.
- **Done when:** changing a trail's joints rewrites those arguments and nothing else, and
  dragging a position still reports the existing "no position arguments" skip.

### [ ] C3 — Screen and body colour effects

`FLASH`, `FLASH_FRM`, `BURN_COLOR`, `BURN_COLOR_FRAME`, `BURN_COLOR_NORMAL`,
`START_INFO_FLASH_EYE`. Colour + duration; the panel work is small and mostly a colour picker.

### [ ] C4 — Effect lifetime control

`EFFECT_DETACH_KIND`, `EFFECT_DETACH_KIND_WORK`, `SET_PLAY_INHIVIT`. Detach and inhibit
interact with the follow/off-kind lifetime the editor already models for spawns.

- **Trap:** end frames are currently derived from `EFFECT_OFF_KIND`. A detach ends a spawn's
  attachment without ending the spawn. Do not fold it into the same end-frame field.

## New script categories

### [ ] D1 — `sound_` scripts

Blocked by: nothing, but it is the largest task here — treat it as its own project.

Not supported at all today. The project indexer recognises the `sound_` prefix
([acmd_src.rs:345](src/acmd_src.rs:345)) so it knows what the function is, but `script_body`
([acmd_src.rs:305](src/acmd_src.rs:305)) only ever loads `game_` and `effect_`. So `PLAY_SE`,
`PLAY_SE_NO_3D`, `PLAY_SEQUENCE`, `PLAY_STEP`, `PLAY_STEP_FLIPPABLE`, `PLAY_LANDING_SE`,
`PLAY_STATUS`, `PLAY_FLY_VOICE`, `PLAY_DOWN_SE`, `PLAY_SE_REMAIN`, `STOP_SE` are invisible.

Nothing is at risk right now — files holding them are never rewritten. That safety property
is what makes this task optional-until-scheduled rather than urgent.

- **Work order, in this order, each landing on its own:**
  1. Load `sound_` in `script_body` and parse it to `Raw` only. Round-trip proven byte-identical
     over the corpus. **Ship this before touching anything else** — it is where the risk is.
  2. Type the `PLAY_SE` family; sound timeline lane.
  3. Plugin hooks for the sound primitives, capture, then live.
  4. Export + write-back.
- **Trap:** the moment step 1 lands, a file that was previously never rewritten becomes
  rewritable. The byte-identical round-trip over the whole corpus is the gate for that, not a
  spot check.

### [ ] D2 — `expression_` scripts

Blocked by: D1 (reuse its loading and round-trip machinery wholesale).

`RUMBLE_HIT`, `QUAKE`, `FT_ATTACK_ABS_CAMERA_QUAKE`, and the screen-fill calls. Same staged
approach, same round-trip gate.

## Gameplay

### [ ] E1 — Movement and kinetics

`sv_kinetic_energy`, `SET_SPEED_EX`, `ADD_SPEED_NO_LIMIT`, `CORRECT`, `REVERSE_LR`. Preserved
verbatim today. High mod value — this is how a move's momentum is authored — and the editor
already knows the frame each call lands on.

- **Trap:** these change where the fighter *is*, so previewing them means moving the model,
  not drawing a box. Scope the first pass to editing values with no viewport preview, and say
  so in the entry when you take it.

### [ ] E2 — Model `FT_MOTION_RATE`

Highest-leverage task in Part 1, and the one most likely to break things.

`FT_MOTION_RATE`, `FT_MOTION_RATE_RANGE`, `FT_DESIRED_RATE` are preserved verbatim, and their
presence deliberately **disables the export timing checks**
([acmd_verify.rs:667](src/acmd_verify.rs:667)) — the editor does not model animation rate, and
a timing warning that guesses is worse than none. Modelling rate would re-enable those checks
for a large slice of the corpus.

- **Work order:** rate scales the frame advance for everything after it. The timeline must show
  real frames, not script frames, once one is in play.
- **Done when:** the timing checks run on rate-carrying scripts and are *correct* on the
  corpus. Re-enabling them while wrong is strictly worse than today.
- **Trap:** branches (`if(WorkModule::is_flag(…)){`) are excluded from timing checks for the
  same reason and are **not** in scope here. Leave that exclusion alone.

### [ ] E3 — Camera and zoom

`CAM_ZOOM_IN_arg5`/`_arg6`, `CAM_ZOOM_IN_FINAL_arg13`, `CAM_ZOOM_OUT`, `CAM_ZOOM_OUT_FINAL`,
`REQ_MOTION_CAMERA`, `FT_START_CUTIN`, `FILL_SCREEN_MODEL_COLOR`, `CANCEL_FILL_SCREEN`.
Lowest value of the gameplay set — mostly final-smash staging. Values and timing only; no
viewport preview.

---

# Part 2 — Known rough edges

Not ACMD coverage. Real, already-diagnosed, and each blocked on something specific.

### [!] R1 — Instant carrier retire (blocked: needs `ITEM_MANAGER_OFFSET`)

Carrier swaps cost ~300 frames (5s) about half the time. Not a hang — the game's deferred
destruction of a dead item. `remove_auto_carrier`
([effect_reload.rs](plugins/slight_replica/src/slight/effect_viewer/effect_reload.rs)) has two
paths: a held item is removed outright and is instant; a *loose* item only gets
`retire_auto_carrier_id` (clear IMMORTAL, lifetime 0, offstage, force `ITEM_STATUS_KIND_DEAD`)
and then waits. It goes loose because the holder got hit.

The fix is `ItemManager::remove_item_from_id(manager, id)` — it exists in the skyline-smash
bindings and takes a battle-object id, exactly the shape a loose item needs. Wire it into the
`else` branch, guarded by `carrier_boma_for_id`, falling back to today's DEAD path when the
manager pointer is null, so worst case is current behaviour.

**Blocker:** it needs an `*mut ItemManager` singleton and the plugin has no
`ITEM_MANAGER_OFFSET`. Every manager here is a hardcoded build-specific text offset.
**Do not guess one — a wrong deref is a boot crash.**

To unblock: set `SSBU_DUMP_DIR` to a dump containing `exefs/main`, `pip install lz4`, then
`research/decomp/ssbu-re/nso_syms.py` parses the NSO dynsym (which still carries the mangled
`_ZN3app...` names, unlike the stripped Ghidra projects here). Resolve
`_ZN3app12item_manager22get_num_of_active_itemENS_8ItemKindE`, disassemble its prologue with
capstone using the ADRP+ADD/LDR technique `xref_scan.py` already uses, and read the global it
loads. **Validate the method by re-deriving the known `EFFECT_MANAGER_OFFSET = 0x5333920`
first — if it does not reproduce that number, do not trust the new one.**

### [ ] R2 — Windows path and frame-path audit

The dev machine runs Eden on Linux; a chunk of the tester base is on Windows. Two bug classes
are structurally invisible here and have shipped before:

1. **Per-frame filesystem work in the plugin.** Linux serves repeated *failed* lookups from the
   negative dentry cache at ~1 µs. Windows has no negative-lookup cache: each miss is a full
   path parse through the emulator's sdmc VFS with Defender hooking the open, 20–200 µs. Work
   that is free here costs the whole 16.6 ms frame budget there.
2. **Host paths built from `dirs::home_dir()`.** `home_dir()/.local/share/...` is Linux-only
   and silently resolves to a nonexistent Windows location — and `create_dir_all` then
   *creates* it, turning a wrong path into a successful write.

- **Work order:** sweep for `home_dir()` in host code and for `exists`/`metadata`/`read_dir`/
  `remove_file` on plugin frame paths. Move the latter onto `slight::sd_poll`. Use
  `dirs::data_dir()`/`dirs::config_dir()`, and validate a probed directory **by its contents**,
  never by `is_dir()` alone.
- **Done when:** a grep-level sweep is clean and the findings are written down here, since this
  cannot be verified locally.

### [ ] R3 — Robust Skyline 13.0.4 hook

`plugins/slight_replica/src/slight/systems/skyline_hook.rs:66` carries the only TODO left in
the source: the current hook is a workaround to keep the core effect viewer usable. Wants
either a proper 13.0.4 hook or a run on real hardware.

### [ ] R4 — Guard against the double-plugin footgun

Skyline loads **every file** in `romfs:/skyline/plugins/` as a plugin regardless of extension.
A `.bak` beside the real `.nro` runs two full copies: double ACMD hooks, double per-frame
ticks, two servers contending for port 7878, both overwriting the same `sd:/slight/` diag
files. Symptom is a hard 60→30 fps drop on entering training mode. It cost ~6 rounds of
misdiagnosis once, because it invalidates every A/B test and makes `diag.txt` show the old
build's header.

- **Work order:** have the plugin detect that a second instance of itself is already live
  (the port bind already fails — make that failure loud in `diag.txt` instead of silent) and
  make the deploy script refuse to leave strays behind.
- **Done when:** deploying over a stray `.bak` produces a visible warning rather than a
  mysterious halved framerate.
