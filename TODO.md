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
  count — a length test breaks the moment a call is short for a different reason. When every
  argument of a family is a bare number there *is* no shape, and then the command name is the
  layout: refuse a call whose length disagrees with its name rather than reinterpreting it.
- **The plugin can hook a primitive the export cannot write.** `sv_animcmd` has every
  primitive; `smash_script::macros` wraps only some. A1 found `macros::AREA_WIND_2ND` missing
  while the plugin hooked `sv_animcmd::AREA_WIND_2ND` happily — so a capture came back live,
  drew correctly, and exported a project that did not build. Before adding a family, grep
  `macros.rs` for **every** member by name, and make the verifier block the ones that are not
  there. Do not assume a hooked primitive is an emittable one.

  **Grep the whole prefix, not each name.** Declarations are not spelled consistently — some
  are `fn NAME<A: ToF32>`, others `fn NAME <A: ToF32>` with a space — so a per-name
  `fn NAME[<(]` pattern reports a wrapper missing when it exists. That false negative would
  block an exportable macro, which is the same class of bug in the other direction. List them
  all and compare against the list:

  ```bash
  grep -rhoE 'pub unsafe fn [A-Z_0-9]+' ~/.cargo/git/checkouts/smash-script-*/*/src/macros.rs \
    | sed 's/pub unsafe fn //' | sort -u
  ```

  Confirmed missing so far: `AREA_WIND_2ND` (A1), `LAST_EFFECT_SET_WORK_INT` (found while
  auditing C1).
- **"Parsed into the IR" is not the same as "reaches the export."** The effect export is
  generated from `EffectCall`s, not from `EffectScript` statements, so a macro can have its own
  `EffectMacro` variant, parse perfectly, and still be dropped by `eval_effect_stmts` on the way
  to `EffectCall` — after which the export never sees it. A3 found `LAST_EFFECT_SET_RATE` in
  exactly that state, described in this file as already done. Before believing a surface is
  covered, follow the value all the way to the emitted text; and note that
  `cached_scripts_round_trip_through_the_emitter` only compares `(spawn_func, effect_name)`
  pairs, so it will not catch a dropped field for you.
- **A macro that names no target binds to the line above it, and nowhere else.**
  `LAST_EFFECT_SET_*` modifies whatever spawned last, so there is nothing in the call to match
  on. Bind it to the immediately preceding recognised spawn and refuse otherwise — reaching
  further back silently retunes an effect the user never touched. The rule must be implemented
  identically in the script parser, the live-capture reconstruction, and the write-back
  scanner; A3 has all three, and they are the pattern to copy.
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
- [ ] `cargo test` passes (**268 green after A3**), including the two corpus oracles — run them
      by name with `cargo test cached_script`:
      `acmd_verify::tests::every_cached_script_survives_its_own_export` and
      `acmd::tests::cached_scripts_round_trip_through_the_emitter`. They run the new code over
      every script the app has ever fetched (currently 461 files under
      `~/.cache/visionary/script-cache`, ~1000 functions).
      **Both return early and pass vacuously if that cache directory is missing.** The first
      then asserts `checked > 100`, so a *thin* cache fails loudly — but an *absent* one is
      silently green. Confirm the directory exists before trusting either. `ls` shows only 3
      entries; the files are nested per fighter, so count with
      `find ~/.cache/visionary/script-cache -type f | wc -l`.
      **Neither oracle is a full-fidelity check, so do not read a green run as one.** The
      effect one compares only `(spawn_func, effect_name)` pairs — A3's dropped rate passed it
      for as long as the bug existed. The per-field comparison lives in
      `check_effect_fidelity` / `check_hitbox_fidelity`; a new field must be added *there* or
      nothing is checking it.
- [ ] A round-trip test for the new family: parse a real vanilla call → emit → parse again →
      identical IR. Put the real call in the test, not a synthetic one.
- [ ] A write-back test asserting that a value edit rewrites *only* that argument span, and
      that a structural edit lands in the skip report with a reason naming the macro.
- [ ] The plugin builds if it was touched: `bash plugins/slight_replica/scripts/build.sh`.
      **That is the whole of what can be verified without a Switch or a running emulator.** The
      script only copies the `.nro` to `target/output/`; it does not deploy, so the `diag.txt`
      build stamp cannot be checked from a build alone. If you have a running game, check it;
      if you do not, say so in the entry rather than implying the live surface was exercised.
- [ ] [README.md](README.md) updated if user-visible behaviour changed. House style: plain
      imperative button labels, no ellipsis.

---

# Part 1 — ACMD coverage gaps

Ordered so that earlier tasks unblock later ones. **Position within a section is not a
priority ranking** — it was meant to be, but measuring the corpus after A3 showed it is not:
C3 (69 occurrences) beats C1 (65), and B4 (45) beats B3 (23). Each entry now carries its own
measured counts; go by those. Blocking relationships are the only thing the order still
encodes.

Counts are occurrences in the local 461-file corpus, which is what the app has fetched so far
— a proxy for how often real scripts use a macro, not a census of the game.

## Foundations

### [x] A1 — Wind write-back into the user's own source

Done 2026-08-04. All five surfaces; `cargo test` 260 green, both corpus oracles run for real
(461 files present). The plugin was not touched — the live surface already hooked and injected
all four commands by name.

Wind edits now reach a linked project. The entry's premise held: the four commands share slots
0..=7, so one table indexed by argument position covers all of them, with the *names* differing
past slot 7 (`width`/`height` vs `radius`/`lifetime`) so the skip report reads correctly. Sites
are matched on command name **and** id, so a rectangular value can never land in a radial call.

**Two things this turned up that the entry did not predict:**

1. **`macros::AREA_WIND_2ND` does not exist.** `sv_animcmd` has all four commands and the
   plugin hooks all four, but smash-script only wrapped three — there is no
   `AREA_WIND_2ND` in `macros.rs`. The emitter wrote `macros::AREA_WIND_2ND(agent, …)`
   regardless, and **Add Wind Box → Rectangle created exactly that**, so every exported mod
   containing an editor-added rectangular wind failed to build. Add now produces
   `AREA_WIND_2ND_arg10` (and `_RAD_arg9` for radial, for the lifetime), and the verifier
   blocks the wrapper-less command with a message naming the replacement. Parsing still
   accepts it — the archive is allowed to contain it, only the export is refused.
   *Possible follow-up:* auto-upgrade a parsed `AREA_WIND_2ND` to `_arg10` by appending the
   lifetime the export already computes, instead of blocking. Deliberately not done — it is a
   macro substitution, and the house rule is to refuse rather than guess.
2. **The Lifetime slider did nothing.** Export rewrites the lifetime argument out of the
   timeline range (`end - start + 1`), so whatever the slider set was overwritten on the way
   out. The slider now moves `active_end` with it — but only for the commands that *have* a
   lifetime slot. The shorter forms end at an `AreaModule::erase_wind` on another line, and
   for those the end frame is not the call's to write. Getting this wrong reported a retime on
   every single edit, because a command with no lifetime "ends" at `u32::MAX`.

- **Landed:** `WIND_COMMANDS` / `WIND_MACRO_COMMANDS` / `is_wind_command` in
  [data.rs](src/data.rs) (the arity table lived in two places and now lives in one),
  `WindboxData::end_frame` as the single home of the lifetime → end-frame derivation,
  `wind_box_edits` in [acmd_src.rs](src/acmd_src.rs), an `ArgValue::ToF32` that writes `20`
  rather than `20.0` so both export paths put the same text in the file, and the verifier
  blocker.

**For whoever takes B3 or C1:** `ArgValue::ToF32` is the right variant for any slot generic
over `ToF32`, which is most of the non-`ATTACK` families. `ArgValue::Float` always writes a
decimal point, which is correct for `ATTACK`'s typed `f32` slots and wrong everywhere else.

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

### [x] A3 — `LAST_EFFECT_SET_RATE` as an editable field

Done 2026-08-04. All five surfaces; `cargo test` 268 green, both corpus oracles run for real
(461 files present). The plugin was touched and builds; the `diag.txt` stamp could not be
checked, because `scripts/build.sh` only writes to `target/output/` and deploying needs a
running game.

**The entry's premise was wrong, and wrong in the expensive direction.** It said the value
"round-trips untouched" with surfaces 1 and 4 already done. Surface 1 was half done and
surface 4 was not done at all: the rate was parsed into `EffectMacro::LastEffectSetRate` and
then **dropped** by `eval_effect_stmts` on the way to `EffectCall`. The effect export is
generated from `EffectCall`s, so it wrote no rate line — every vanilla effect that plays fast
or slow shipped at normal speed, silently. There are 27 such calls in the local corpus.

The one `LAST_EFFECT_SET_RATE` the emitter *did* write came from `LiveTweak::speed`, an
unrelated kind-global live control, which is what made the entry look already-done.

**The positional trap, settled.** The rate binds to the spawn *directly above it in the same
block*, and to nothing otherwise — not to "the last spawn seen". Anything in between breaks
the pairing: an `EFFECT_OFF_KIND`, a trail, or any line the parser keeps as `Raw`, because a
`Raw` line could itself be a spawn and then the rate belongs to it. This is stricter than the
game, whose "last effect" persists across frame blocks. It costs nothing: all 27 corpus calls
sit directly beneath a recognised spawn. Reordering and disabling then need no special case at
all — the rate is a field *on* the call, so it moves and disappears with it.

The same rule is implemented three times and the three must agree, or a value is read off one
call and written into another: `eval_effect_stmts` (script), `effect_calls_from_captures`
(live capture), and `spawn_and_rate_sites` (write-back).

**Also fixed on the way:**

- **The panel had no rate control at all**, so the checkbox/value pair is new. Off means no
  line is written, which is not the same as writing 1.0 — that distinction is the whole reason
  `rate` is an `Option` rather than defaulting to 1.0.
- **The live surface had no rate anything.** The plugin now hooks
  `sv_animcmd::LAST_EFFECT_SET_RATE`: it records the line so a captured move comes back with
  its rates, and rewrites the argument when a spawn rule retuned that spawn. Rewriting is
  required rather than calling `set_rate` — the script's own line runs last and would
  overwrite anything applied before it. `SpawnRule.rate` is `Option` + `skip_serializing_if`,
  so older plugin builds ignore it.
- **A tweak and a script rate would have emitted two rate lines.** The override wins and
  exactly one line is written; the verifier warns that a kind-global speed multiplier has
  taken over a value the script set for one spawn.
- The rate is spelled `2`, not `2.0` — `LAST_EFFECT_SET_RATE<F: ToF32>` — so the emitter and
  the write-back put identical text in the file. `check_effect_values` now refuses a
  non-finite rate, which plain `to_string` would spell `NaN` and break the build.

**Known gaps, deliberately left:**

- A rate that could not be attached is dropped by the export, because the effect export
  regenerates from `EffectCall`s and drops every unmodelled line already. Not specific to
  rate; fixing it means teaching the effect export to preserve `Raw`, which is its own task.
- The live capture stream dedupes on `(motion, kind, frame, func, args)`. Two spawns on one
  frame with the *same* rate collapse to one rate line, so the second spawn captures with no
  rate. Conservative — a missing rate, never a wrong one — and it only affects live capture.

**For whoever takes C1:** the attachment rule and its three implementations are the reusable
part; copy them rather than re-deriving. C1 carries the measured table for the rest of the
family, including one member smash-script never wrapped — read it before writing a slot table.

## Hitbox families

### [ ] B1 — `ATTACK_ABS` (absolute-position hitboxes)

Preserved verbatim today. Positions are world-absolute rather than bone-relative, so the
viewport gizmo and the bone dropdown do not apply as written.

**Measured, not assumed:** 16 arguments, 32 occurrences in the local corpus, and
`macros::ATTACK_ABS` is declared. That is less than half of `ATTACK`'s 36 — it is a genuinely
different call, and none of `AttackCall`'s hit-property block can be reused positionally.

Re-measured after A3: **all 32 occurrences carry exactly 16 arguments.** One arity, so unlike
`ATTACK`/`ATTACK_IGNORE_THROW` there is no optional-capsule shape to detect — A2's
`capsule_slots_present` / `shift_past_absent_capsule` dance is not needed here, and copying it
in would be complexity with nothing behind it.

- **Work order:** own family, own slot table, own struct. Reuse `AttackCall`'s hit-property
  block only if the arities genuinely match — they do not, so expect a real struct.
- **Done when:** it draws in the viewport at the right place and survives the corpus oracles.
- **Trap:** the shared-transform assumption behind the bone dropdown is wrong here. Suppress
  the bone control rather than showing one that does nothing.

### [ ] B2 — `ATTACK_FP` (fighter-position hitboxes)

Blocked by: B1 in practice — but read this before scheduling it.

**Measured after A3: `ATTACK_FP` appears ZERO times in the local corpus.** smash-script
declares it, so it is emittable, but there is no vanilla call anywhere in the 461 cached
scripts. That breaks two things the old "same shape as B1, do it immediately after" framing
assumed:

- The definition of done requires a round-trip test built from a **real** vanilla call. There
  is none to build it from, so this task cannot meet that bar as written.
- Nothing exercises it in the corpus oracles either, so a wrong slot table here would pass
  every check in the project and only fail on a user's mod.

Do not do this on the strength of "it is next in the list". Either fetch more of the archive
first and re-measure, or take it deliberately as an unverifiable convenience for mod authors
who write `ATTACK_FP` by hand — and say which in the entry, since the second is a real choice
and not a default.

### [ ] B3 — Post-hoc hitbox tuning

These modify hitboxes *already out*, so they are edits to an existing collision rather than
new collisions.

**Measured after A3** — all have `macros.rs` wrappers, so no export trap:
`ATK_SET_SHIELD_SETOFF_MUL` 9, `ATK_HIT_ABS` 6, `WHOLE_HIT` 6, `ATK_POWER` 2,
`ATK_LERP_RATIO` 0. Twenty-three occurrences total, which is the thinnest of the hitbox
tasks — B4 has twice the usage for comparable work. Take that first unless something specific
wants these.

- **Work order:** model them as modifiers attached to the hitbox id they target, shown on the
  timeline at their own frame. Do not fold their values into the parent `ATTACK` — the export
  must re-emit them as separate calls at their own frames.
- **Trap:** the `_argN` suffixes are different arities of the same idea, exactly like the wind
  family. One table each.

### [ ] B4 — Hurtbox control

Intangibility and hurtbox state per bone — the thing every competitive-facing mod actually
wants to tune, and currently invisible.

**Measured after A3**, and the numbers back the framing: `HIT_NODE` 30, `COL_NORMAL` 8,
`HIT_RESET_ALL` 3, `HIT_NO` 2, `COL_PRI` 2 — 45 occurrences, the most-used hitbox task left.
All five have `macros.rs` wrappers.

- **Work order:** these are per-bone state over a frame range, not collisions. They want their
  own panel and their own timeline lane, not a row in Hitboxes.
- **Trap:** `HIT_NODE` takes a bone and a state constant. That constant needs a `ConstTable`
  with both directions before the live path can carry it.

### [ ] B5 — `SEARCH` / detection boxes

Grab-range and detection volumes. Geometrically these are close to grab boxes, so the panel
work is mostly reuse.

**Measured after A3:** `SEARCH` 7; `SET_SEARCH_SIZE_EXIST`, `ENABLE_AREA`, and `UNABLE_AREA`
appear **zero** times in the local corpus, though all four have `macros.rs` wrappers. So this
is really a one-macro task with three untestable extras — there is no vanilla call to build a
round-trip test from for those three, which the definition of done requires. Scope it to
`SEARCH` and say so, rather than shipping three families no corpus can check.

- **Trap:** new collision family → it must be added to the plugin's `is_collision_func` and
  given a category id on the wire, alongside 0 attack / 1 grab / 2 wind.

## Effects

### [ ] C1 — The remaining `LAST_EFFECT_SET_*` modifiers

Unblocked: A3 is done and settled the attachment rule — bind to the immediately preceding
recognised spawn, refuse otherwise, and implement it identically in `eval_effect_stmts`,
`effect_calls_from_captures`, and `spawn_and_rate_sites`. Copy those three, do not re-derive.

**Measured, not assumed.** Corpus occurrences, and whether smash-script declares a wrapper —
both checked, because A1's trap is live in this family:

| macro | args | corpus | `macros.rs` wrapper |
|---|---|---|---|
| `LAST_EFFECT_SET_COLOR` | 3 (`ToF32`) | 65 | yes |
| `LAST_EFFECT_SET_RATE` | 1 (`ToF32`) | 27 | yes — **done, A3** |
| `LAST_EFFECT_SET_ALPHA` | 1 (`ToF32`) | 4 | yes |
| `LAST_PARTICLE_SET_COLOR` | 3 (`ToF32`) | 1 | yes |
| `LAST_EFFECT_SET_WORK_INT` | — | 1 | **NO** |
| `LAST_EFFECT_SET_SCALE_W` | 3 (`ToF32`) | 0 | yes |
| `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` | 1 (`ToF32`) | 0 | yes |

Colour is the whole task by value: 65 occurrences, more than rate's 27. `_SCALE_W` and
`_OFFSET_TO_CAMERA_FLAT` appear **nowhere** in the local corpus — they are exportable but
have no vanilla usage to test against, so do them last or not at all.

**`LAST_EFFECT_SET_WORK_INT` is the `AREA_WIND_2ND` situation again**, verified against the
full wrapper list: `sv_animcmd` has it, the archive uses it once, and `macros.rs` does not
declare it. If it is modelled at all it must be parse-only, with a verifier blocker on export
— the same shape as `WIND_MACRO_COMMANDS` / `has_macro_wrapper` in [data.rs](src/data.rs).
Every argument in the rest of the family is generic over `ToF32`, so use `ArgValue::ToF32` and
plain `to_string`, never `num`.

- **Work order:** `EffectCall` gained one `Option` field for rate. Five more would be five more
  fields; consider a single modifier block instead. Whatever the shape, the `None` vs
  `Some(default)` distinction A3 established has to survive it: "no line" and "a line setting
  the default" are different exports.
- **Do not forget `check_effect_fidelity`.** A new field on `EffectCall` is not verified until
  it is compared there — the corpus oracle will not catch it. That is exactly how A3's rate
  stayed broken.
- **Trap:** kind-level colour/speed overrides already exist on the live wire and apply to
  *every* spawn of an effect. These are per-spawn. Read the comment above `LiveOverride`
  ([game_link.rs:373](src/game_link.rs:373)) before wiring anything — conflating per-kind with
  per-emitter recoloured whole effects once already. A3 hit the export half of this: a live
  speed tweak and a script rate both wanted to write a `LAST_EFFECT_SET_RATE` line. The
  override wins, exactly one line is emitted, and the verifier warns. Colour needs the same
  reconciliation, and `emit_effect_move_fn` already has the shape to copy.

### [ ] C2 — Sword trail joints

`AFTER_IMAGE4_ON` / `_arg29` / `AFTER_IMAGE_ON`. Read-only today: the trail shows on the
timeline with its graphic and joint, and write-back explicitly refuses it
([acmd_src.rs:803](src/acmd_src.rs:803)) because a trail has no transform.

That refusal is correct and must stay. But a trail *is* placed — by the joints it names,
arguments 4 onward. Those are editable; the transform never will be.

- **Work order:** expose the joint pair and the trail parameters. Keep refusing transform
  writes, with the existing message.
- **Done when:** changing a trail's joints rewrites those arguments and nothing else, and
  dragging a position still reports the existing "no position arguments" skip.

### [ ] C3 — Screen and body colour effects

**Measured after A3, and this entry was misfiled as small.** It is the largest effect task in
this section by corpus usage — 69 occurrences against C1's 65 — and every member has a
`macros.rs` wrapper, so there is no export trap here:

| macro | corpus |
|---|---|
| `FLASH` | 41 |
| `BURN_COLOR` | 10 |
| `BURN_COLOR_FRAME` | 10 |
| `BURN_COLOR_NORMAL` | 5 |
| `START_INFO_FLASH_EYE` | 3 |
| `FLASH_FRM` | 0 |

The panel work really is mostly a colour picker, which is what makes the ratio good. `FLASH`
alone is worth more than all of `_ALPHA`, `_SCALE_W`, `_OFFSET_TO_CAMERA_FLAT`, and
`LAST_PARTICLE_SET_COLOR` put together. **Consider taking this before C1**, or at least
before C1's long tail.

- **Trap:** unlike the `LAST_EFFECT_SET_*` family these name their own target, so A3's
  bind-to-the-line-above rule does *not* apply and must not be copied here out of habit.

### [ ] C4 — Effect lifetime control

Detach and inhibit interact with the follow/off-kind lifetime the editor already models for
spawns.

**Measured after A3:** `SET_PLAY_INHIVIT` 10, `EFFECT_DETACH_KIND` 0, `EFFECT_DETACH_KIND_WORK`
0 — all three have `macros.rs` wrappers. So the detach half, which is the part carrying the
trap below, has no vanilla usage to test against; `SET_PLAY_INHIVIT` is the only member with
real corpus backing. Consider scoping to it alone.

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
([acmd_verify.rs:743](src/acmd_verify.rs:743)) — the editor does not model animation rate, and
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
